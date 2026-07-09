use std::fs;
use std::io::{self, BufRead, Read, Write};
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use zodex::config::Config;
use zodex::publisher::{
    DirectPushRequest, mint_reader_installation_token, submit_direct_push_request,
};

use super::github::{load_matching_push_grant, normalize_github_repo};
use super::tls::ensure_reader_ready_for_start;

const PUSH_GRANTS_DIR: &str = "/var/lib/zodex/push-grants";

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(super) struct GitCredentialRequest {
    protocol: Option<String>,
    host: Option<String>,
    path: Option<String>,
    url: Option<String>,
    username: Option<String>,
}

pub(super) async fn handle_git_credential_helper(config: &Config, operation: &str) -> Result<()> {
    let request = read_git_credential_request()?;

    if operation != "get" || !git_credential_request_targets_github(&request) {
        return Ok(());
    }

    if let Some(grant) = load_matching_push_grant(&request, Path::new(PUSH_GRANTS_DIR))? {
        println!("username=x-access-token");
        println!("password={}", grant.token);
        println!();
        return Ok(());
    }

    ensure_reader_ready_for_start(config)?;
    let token = mint_reader_installation_token(
        config.reader_app_id.unwrap_or_default(),
        Path::new(&config.reader_private_key_path),
        config.reader_installation_id.unwrap_or_default(),
    )
    .await?;

    println!("username=x-access-token");
    println!("password={token}");
    println!();
    Ok(())
}

pub(super) async fn handle_git_remote_zodex(config: &Config, url: &str) -> Result<()> {
    let repo = git_remote_zodex_repo(url).ok_or_else(|| {
        anyhow!("zodex remote helper only supports GitHub owner/repo URLs: {url}")
    })?;
    let stdin = io::stdin();
    let mut stdout = io::stdout();
    let mut lines = stdin.lock().lines();

    while let Some(line) = lines.next() {
        let line = line.context("failed to read git remote-helper command")?;
        if line.is_empty() {
            break;
        }

        if line == "capabilities" {
            writeln!(stdout, "push")?;
            writeln!(stdout)?;
            stdout.flush()?;
            continue;
        }

        if line == "list for-push" || line == "list" {
            for remote_ref in git_remote_zodex_list_refs(url)? {
                writeln!(stdout, "{remote_ref}")?;
            }
            writeln!(stdout)?;
            stdout.flush()?;
            continue;
        }

        if let Some(first_spec) = line.strip_prefix("push ") {
            let mut specs = vec![first_spec.to_string()];
            for batch_line in lines.by_ref() {
                let batch_line = batch_line.context("failed to read git push batch")?;
                if batch_line.is_empty() {
                    break;
                }
                if let Some(spec) = batch_line.strip_prefix("push ") {
                    specs.push(spec.to_string());
                }
            }

            for spec in specs {
                match handle_git_remote_zodex_push(config, &repo, &spec).await {
                    Ok(dst) => writeln!(stdout, "ok {dst}")?,
                    Err(err) => {
                        let dst = git_remote_zodex_push_dst(&spec)
                            .unwrap_or_else(|| "refs/heads/unknown".to_string());
                        writeln!(stdout, "error {dst} {}", sanitize_remote_helper_error(&err))?;
                    }
                }
            }
            writeln!(stdout)?;
            stdout.flush()?;
            continue;
        }

        bail!("unsupported git remote-helper command: {line}");
    }

    Ok(())
}

pub(super) fn git_remote_zodex_repo(url: &str) -> Option<String> {
    let url = git_remote_zodex_inner_url(url);
    match (credential_url_protocol(url), credential_url_host(url)) {
        (Some(protocol), Some(host))
            if protocol.eq_ignore_ascii_case("https") && credential_host_is_github(host) =>
        {
            credential_url_path(url).and_then(normalize_github_repo)
        }
        (None, None) => normalize_github_repo(url),
        _ => None,
    }
}

fn git_remote_zodex_inner_url(url: &str) -> &str {
    url.strip_prefix("zodex::").unwrap_or(url)
}

fn git_remote_zodex_list_refs(url: &str) -> Result<Vec<String>> {
    let url = git_remote_zodex_inner_url(url);
    let output = Command::new("git")
        .args(["ls-remote", "--heads", "--tags", url])
        .output()
        .context("failed to run git ls-remote for zodex remote helper")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("git ls-remote failed: {}", stderr.trim());
    }

    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let (oid, name) = line.split_once('\t')?;
            if name.ends_with("^{}") {
                None
            } else {
                Some(format!("{oid} {name}"))
            }
        })
        .collect())
}

async fn handle_git_remote_zodex_push(
    config: &Config,
    repo: &str,
    raw_spec: &str,
) -> Result<String> {
    let (force, spec) = raw_spec
        .strip_prefix('+')
        .map_or((false, raw_spec), |stripped| (true, stripped));
    let (src, dst) = spec
        .split_once(':')
        .ok_or_else(|| anyhow!("unsupported push refspec: {raw_spec}"))?;
    if dst.trim().is_empty() {
        bail!("push destination ref cannot be empty");
    }

    let (bundle_base64, src_oid, src_object_type) = if src.is_empty() {
        (None, None, None)
    } else {
        (
            create_direct_push_bundle_base64(src)?,
            Some(resolve_git_object_id(Path::new("."), src)?),
            Some(resolve_git_object_type(Path::new("."), src)?),
        )
    };
    let request = DirectPushRequest {
        repo: repo.to_string(),
        src: src.to_string(),
        dst: dst.to_string(),
        force,
        bundle_base64,
        src_oid,
        src_object_type,
    };
    let response = submit_direct_push_request(Path::new(&config.publisher_socket_path), &request)
        .await
        .with_context(|| format!("zodex direct push failed for {repo} {raw_spec}"))?;
    Ok(response.dst)
}

pub(super) fn git_remote_zodex_push_dst(raw_spec: &str) -> Option<String> {
    let spec = raw_spec.strip_prefix('+').unwrap_or(raw_spec);
    let (_, dst) = spec.split_once(':')?;
    if dst.is_empty() {
        None
    } else {
        Some(dst.to_string())
    }
}

fn create_direct_push_bundle_base64(src: &str) -> Result<Option<String>> {
    create_direct_push_bundle_base64_from_dir(Path::new("."), src)
}

pub(super) fn create_direct_push_bundle_base64_from_dir(
    repo_dir: &Path,
    src: &str,
) -> Result<Option<String>> {
    let tempdir = tempfile::tempdir().context("failed to create direct push bundle tempdir")?;
    let bundle_path = tempdir.path().join("direct-push.bundle");
    let mut args = vec!["bundle", "create", bundle_path.to_str().unwrap(), src];
    if repository_has_refs(repo_dir, "refs/remotes")? {
        args.extend(["--not", "--remotes"]);
    }
    let output = Command::new("git")
        .current_dir(repo_dir)
        .args(&args)
        .output()
        .context("failed to run git bundle create for direct push")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("Refusing to create empty bundle") {
            return Ok(None);
        }
        bail!("git bundle create failed: {}", stderr.trim());
    }
    let bundle = fs::read(&bundle_path)
        .with_context(|| format!("failed to read {}", bundle_path.display()))?;
    Ok(Some(BASE64.encode(bundle)))
}

pub(super) fn resolve_git_object_id(repo_dir: &Path, src: &str) -> Result<String> {
    git_single_line(
        repo_dir,
        &["rev-parse", "--verify", &format!("{src}^{{object}}")],
    )
    .with_context(|| format!("failed to resolve pushed source object {src}"))
}

pub(super) fn resolve_git_object_type(repo_dir: &Path, src: &str) -> Result<String> {
    git_single_line(repo_dir, &["cat-file", "-t", src])
        .with_context(|| format!("failed to resolve pushed source object type for {src}"))
}

fn git_single_line(repo_dir: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .current_dir(repo_dir)
        .args(args)
        .output()
        .context("failed to run git")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("git {} failed: {}", args.join(" "), stderr.trim());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn repository_has_refs(repo_dir: &Path, ref_namespace: &str) -> Result<bool> {
    let output = Command::new("git")
        .current_dir(repo_dir)
        .args([
            "for-each-ref",
            "--format=%(refname)",
            "--count=1",
            ref_namespace,
        ])
        .output()
        .with_context(|| format!("failed to inspect git refs under {ref_namespace}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("git for-each-ref failed: {}", stderr.trim());
    }
    Ok(!String::from_utf8_lossy(&output.stdout).trim().is_empty())
}

pub(super) fn sanitize_remote_helper_error(err: &anyhow::Error) -> String {
    error_chain_string(err)
        .replace(['\n', '\r', '\t'], " ")
        .trim()
        .to_string()
}

fn error_chain_string(err: &anyhow::Error) -> String {
    err.chain()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(": ")
}

fn read_git_credential_request() -> Result<GitCredentialRequest> {
    let mut raw = String::new();
    io::stdin()
        .read_to_string(&mut raw)
        .context("failed to read git credential request from stdin")?;
    Ok(parse_git_credential_request(&raw))
}

fn parse_git_credential_request(raw: &str) -> GitCredentialRequest {
    let mut request = GitCredentialRequest::default();

    for line in raw.lines().filter(|line| !line.trim().is_empty()) {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };

        match key {
            "protocol" => request.protocol = Some(value.to_string()),
            "host" => request.host = Some(value.to_string()),
            "path" => request.path = Some(value.to_string()),
            "url" => request.url = Some(value.to_string()),
            "username" => request.username = Some(value.to_string()),
            _ => {}
        }
    }

    request
}

fn git_credential_request_targets_github(request: &GitCredentialRequest) -> bool {
    let protocol = request
        .protocol
        .as_deref()
        .or_else(|| request.url.as_deref().and_then(credential_url_protocol));
    let host = request
        .host
        .as_deref()
        .or_else(|| request.url.as_deref().and_then(credential_url_host));

    matches!(protocol, Some(protocol) if protocol.eq_ignore_ascii_case("https"))
        && matches!(host, Some(host) if credential_host_is_github(host))
}

fn credential_url_protocol(url: &str) -> Option<&str> {
    url.split_once("://").map(|(scheme, _)| scheme)
}

fn credential_url_host(url: &str) -> Option<&str> {
    let (_, rest) = url.split_once("://")?;
    let host = rest.split('/').next()?;
    Some(host.split('@').next_back().unwrap_or(host))
}

fn credential_url_path(url: &str) -> Option<&str> {
    let (_, rest) = url.split_once("://")?;
    let (_, path) = rest.split_once('/')?;
    Some(path)
}

fn credential_host_is_github(host: &str) -> bool {
    let normalized = host
        .split(':')
        .next()
        .unwrap_or(host)
        .trim_end_matches('.')
        .to_ascii_lowercase();
    normalized == "github.com" || normalized == "www.github.com"
}

pub(super) fn git_credential_request_repo(request: &GitCredentialRequest) -> Option<String> {
    let path = request
        .path
        .as_deref()
        .or_else(|| request.url.as_deref().and_then(credential_url_path))?;
    normalize_github_repo(path)
}
