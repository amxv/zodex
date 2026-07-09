use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use tempfile::tempdir;

use crate::config::Config;

use super::api::PublishPrRequest;

pub fn build_publish_request(
    config: &Config,
    repo_id: String,
    base: Option<String>,
    title: String,
    body: String,
    draft: bool,
    repo_root: &Path,
) -> Result<PublishPrRequest> {
    ensure_clean_worktree(repo_root)?;
    ensure_repo_root_matches_target(repo_root, &repo_id)?;
    if title.trim().is_empty() {
        bail!("PR title cannot be empty");
    }

    let bundle_bytes = create_head_bundle(repo_root)?;
    if bundle_bytes.len() > config.publisher_max_bundle_bytes {
        bail!(
            "git bundle is too large ({} bytes > {} bytes)",
            bundle_bytes.len(),
            config.publisher_max_bundle_bytes
        );
    }

    Ok(PublishPrRequest {
        repo_id,
        base,
        title,
        body,
        draft,
        bundle_base64: BASE64.encode(bundle_bytes),
    })
}

fn ensure_repo_root_matches_target(repo_root: &Path, repo_id: &str) -> Result<()> {
    let checkout_repo = detect_checkout_github_repo(repo_root)?;
    if checkout_repo != repo_id {
        bail!(
            "current checkout is for {checkout_repo}, but publish-pr targeted {repo_id}; switch to the matching repo before publishing"
        );
    }
    Ok(())
}

pub fn detect_repo_root(start_dir: &Path) -> Result<PathBuf> {
    let output = Command::new("git")
        .arg("rev-parse")
        .arg("--show-toplevel")
        .current_dir(start_dir)
        .output()
        .context("failed to run git rev-parse --show-toplevel")?;

    if !output.status.success() {
        bail!(
            "git rev-parse --show-toplevel failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    let root = String::from_utf8(output.stdout).context("git repo root was not valid UTF-8")?;
    Ok(PathBuf::from(root.trim()))
}

fn detect_checkout_github_repo(repo_root: &Path) -> Result<String> {
    let output = Command::new("git")
        .arg("remote")
        .arg("get-url")
        .arg("origin")
        .current_dir(repo_root)
        .output()
        .context("failed to run git remote get-url origin")?;

    if !output.status.success() {
        bail!(
            "git remote get-url origin failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    let remote_url =
        String::from_utf8(output.stdout).context("git remote origin URL was not valid UTF-8")?;
    parse_github_remote_repo(&remote_url).ok_or_else(|| {
        anyhow!(
            "git remote origin does not point to a supported GitHub repo URL: {}",
            remote_url.trim()
        )
    })
}

pub(super) fn parse_github_remote_repo(remote_url: &str) -> Option<String> {
    let trimmed = remote_url.trim();
    if trimmed.is_empty() {
        return None;
    }

    if let Some((_, rest)) = trimmed.split_once("://") {
        let (authority, path) = rest.split_once('/')?;
        if !github_remote_authority_is_supported(authority) {
            return None;
        }
        return normalize_github_repo_path(path);
    }

    if let Some((authority, path)) = trimmed.split_once(':')
        && authority.contains('@')
    {
        let host = authority
            .rsplit_once('@')
            .map(|(_, host)| host)
            .unwrap_or(authority);
        if !github_remote_host_is_supported(host) {
            return None;
        }
        return normalize_github_repo_path(path);
    }

    let (authority, path) = trimmed.split_once('/')?;
    if !github_remote_authority_is_supported(authority) {
        return None;
    }
    normalize_github_repo_path(path)
}

fn github_remote_authority_is_supported(authority: &str) -> bool {
    let host = authority
        .rsplit_once('@')
        .map(|(_, host)| host)
        .unwrap_or(authority)
        .split(':')
        .next()
        .unwrap_or(authority);
    github_remote_host_is_supported(host)
}

fn github_remote_host_is_supported(host: &str) -> bool {
    let normalized = host.trim_end_matches('.').to_ascii_lowercase();
    normalized == "github.com" || normalized == "www.github.com"
}

fn normalize_github_repo_path(path: &str) -> Option<String> {
    let trimmed = path.trim_matches('/');
    let trimmed = trimmed.strip_suffix(".git").unwrap_or(trimmed);
    let mut parts = trimmed.split('/');
    let owner = parts.next()?;
    let repo = parts.next()?;
    if owner.is_empty() || repo.is_empty() || parts.next().is_some() {
        return None;
    }
    Some(format!("{owner}/{repo}"))
}

pub fn create_head_bundle(repo_root: &Path) -> Result<Vec<u8>> {
    let tempdir = tempdir().context("failed to create temporary directory for git bundle")?;
    let bundle_path = tempdir.path().join("head.bundle");

    let output = Command::new("git")
        .current_dir(repo_root)
        .args(["bundle", "create"])
        .arg(&bundle_path)
        .arg("HEAD")
        .output()
        .context("failed to run git bundle create")?;

    if !output.status.success() {
        bail!(
            "git bundle create failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    fs::read(&bundle_path).with_context(|| format!("failed to read {}", bundle_path.display()))
}

pub fn ensure_clean_worktree(repo_root: &Path) -> Result<()> {
    let output = Command::new("git")
        .current_dir(repo_root)
        .args(["status", "--porcelain"])
        .output()
        .context("failed to run git status --porcelain")?;

    if !output.status.success() {
        bail!(
            "git status --porcelain failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    if !String::from_utf8_lossy(&output.stdout).trim().is_empty() {
        bail!("publish-pr requires a clean worktree; commit or stash changes first");
    }

    Ok(())
}
