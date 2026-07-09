use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use rand::distr::{Alphanumeric, SampleString};

use crate::config::{Config, PublishTarget};

use super::GITHUB_MODE_STATE_PATH;
use super::api::{DirectPushRequest, GithubModeRecord, GithubYoloRepoGrant, PublishPrRequest};

pub fn validate_publish_request(
    config: &Config,
    request: &PublishPrRequest,
) -> Result<(PublishTarget, Vec<u8>)> {
    let target = resolve_publisher_target(config, &request.repo_id).ok_or_else(|| {
        anyhow!(
            "repo is not covered by publisher installation config: {}",
            request.repo_id
        )
    })?;

    if target.repo.trim().is_empty() {
        bail!("publisher target {} has an empty repo value", target.id);
    }
    if target.default_base.trim().is_empty() {
        bail!("publisher target {} has an empty default base", target.id);
    }
    if target.installation_id == 0 {
        bail!("publisher target {} is missing installation_id", target.id);
    }
    if request.title.trim().is_empty() {
        bail!("PR title cannot be empty");
    }
    if request.title.chars().count() > config.publisher_max_title_chars {
        bail!(
            "PR title exceeds limit ({} > {})",
            request.title.chars().count(),
            config.publisher_max_title_chars
        );
    }
    if request.body.chars().count() > config.publisher_max_body_chars {
        bail!(
            "PR body exceeds limit ({} > {})",
            request.body.chars().count(),
            config.publisher_max_body_chars
        );
    }

    let bundle_bytes = BASE64
        .decode(request.bundle_base64.as_bytes())
        .context("bundle_base64 was not valid base64")?;
    if bundle_bytes.is_empty() {
        bail!("publish bundle cannot be empty");
    }
    if bundle_bytes.len() > config.publisher_max_bundle_bytes {
        bail!(
            "publish bundle exceeds limit ({} bytes > {} bytes)",
            bundle_bytes.len(),
            config.publisher_max_bundle_bytes
        );
    }

    if let Some(base) = request.base.as_deref()
        && base.trim().is_empty()
    {
        bail!("base branch cannot be empty when provided");
    }

    Ok((target, bundle_bytes))
}

pub fn build_publish_branch_name(prefix: &str) -> String {
    let sanitized = sanitize_branch_prefix(prefix);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let mut rng = rand::rng();
    let suffix = Alphanumeric.sample_string(&mut rng, 8).to_ascii_lowercase();
    format!("{sanitized}/{now}-{suffix}")
}

fn sanitize_branch_prefix(prefix: &str) -> String {
    let mut cleaned: String = prefix
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '/'))
        .collect();
    cleaned = cleaned.trim_matches('/').to_string();
    if cleaned.is_empty() {
        "agent".to_string()
    } else {
        cleaned
    }
}
pub(super) fn validate_publisher_config(config: &Config) -> Result<()> {
    let app_id = config.publisher_app_id.ok_or_else(|| {
        anyhow!("publisher_app_id must be configured before starting the publisher daemon")
    })?;
    if app_id == 0 {
        bail!("publisher_app_id must be non-zero");
    }
    if config.publisher_private_key_path.trim().is_empty() {
        bail!("publisher_private_key_path must be configured");
    }
    if !Path::new(&config.publisher_private_key_path).exists() {
        bail!(
            "publisher private key file not found: {}",
            config.publisher_private_key_path
        );
    }
    if config.publisher_targets.is_empty() && config.publisher_installations.is_empty() {
        bail!(
            "publisher_targets or publisher_installations must contain at least one allowed repo scope"
        );
    }
    for target in &config.publisher_targets {
        if target.id.trim().is_empty() || target.repo.trim().is_empty() {
            bail!("publisher target entries require both id and repo");
        }
        if target.installation_id == 0 {
            bail!("publisher target {} must define installation_id", target.id);
        }
    }
    for installation in &config.publisher_installations {
        if installation.account.trim().is_empty() {
            bail!("publisher installation entries require account");
        }
        if installation.installation_id == 0 {
            bail!(
                "publisher installation {} must define installation_id",
                installation.account
            );
        }
    }
    Ok(())
}

pub(super) fn validate_direct_push_request(
    config: &Config,
    request: &DirectPushRequest,
) -> Result<PublishTarget> {
    let repo = normalize_github_repo(&request.repo)
        .ok_or_else(|| anyhow!("repo must be in owner/repo form"))?;
    if repo != request.repo {
        bail!("direct push repo must be normalized as owner/repo");
    }
    if request.dst.trim().is_empty() || !request.dst.starts_with("refs/") {
        bail!("direct push destination must be a full refs/ name");
    }
    if request.dst.contains("..") || request.dst.contains('\\') || request.dst.ends_with('/') {
        bail!("direct push destination ref is invalid");
    }
    if !request.src.is_empty()
        && (request.src.contains("..") || request.src.contains('\\') || request.src.ends_with('/'))
    {
        bail!("direct push source ref is invalid");
    }
    if let Some(src_oid) = request.src_oid.as_deref() {
        validate_git_object_id(src_oid)?;
    }

    let mode = load_active_github_yolo_mode(Path::new(GITHUB_MODE_STATE_PATH))?;
    if !github_mode_allows_repo(&mode, &repo) {
        bail!("YOLO mode is not active for repo {repo}");
    }

    resolve_publisher_target(config, &repo)
        .ok_or_else(|| anyhow!("repo {repo} is not covered by publisher installation config"))
}

pub(super) fn validate_git_object_id(raw: &str) -> Result<()> {
    let valid_hex_len = raw.len() == 40 || raw.len() == 64;
    if !valid_hex_len || !raw.chars().all(|ch| ch.is_ascii_hexdigit()) {
        bail!("direct push source object id is invalid");
    }
    Ok(())
}

fn load_active_github_yolo_mode(path: &Path) -> Result<GithubModeRecord> {
    let raw = fs::read_to_string(path)
        .with_context(|| format!("failed to read GitHub mode state at {}", path.display()))?;
    let record: GithubModeRecord =
        serde_json::from_str(&raw).context("failed to parse GitHub mode state")?;
    if record.mode != "yolo" {
        bail!("GitHub YOLO mode is not active");
    }
    if github_mode_expired(&record, current_epoch_seconds()?) {
        bail!("GitHub YOLO mode has expired");
    }
    Ok(record)
}

fn github_yolo_all_installed_active(record: &GithubModeRecord, now_epoch_seconds: u64) -> bool {
    if !record.all_installed {
        return false;
    }
    !matches!(
        record.expires_at_epoch_seconds,
        Some(expires_at_epoch_seconds) if expires_at_epoch_seconds <= now_epoch_seconds
    )
}

fn github_yolo_repo_grant_active(grant: &GithubYoloRepoGrant, now_epoch_seconds: u64) -> bool {
    !matches!(
        grant.expires_at_epoch_seconds,
        Some(expires_at_epoch_seconds) if expires_at_epoch_seconds <= now_epoch_seconds
    )
}

fn github_yolo_active_legacy_repos(record: &GithubModeRecord, now_epoch_seconds: u64) -> bool {
    record.repo_grants.is_empty()
        && !record.all_installed
        && !matches!(
            record.expires_at_epoch_seconds,
            Some(expires_at_epoch_seconds) if expires_at_epoch_seconds <= now_epoch_seconds
        )
        && !record.repos.is_empty()
}

pub(super) fn github_mode_expired(record: &GithubModeRecord, now_epoch_seconds: u64) -> bool {
    !github_yolo_all_installed_active(record, now_epoch_seconds)
        && !record
            .repo_grants
            .iter()
            .any(|grant| github_yolo_repo_grant_active(grant, now_epoch_seconds))
        && !github_yolo_active_legacy_repos(record, now_epoch_seconds)
}

pub(super) fn github_mode_allows_repo(record: &GithubModeRecord, repo: &str) -> bool {
    let Ok(now_epoch_seconds) = current_epoch_seconds() else {
        return false;
    };
    github_yolo_all_installed_active(record, now_epoch_seconds)
        || record.repo_grants.iter().any(|grant| {
            grant.repo == repo && github_yolo_repo_grant_active(grant, now_epoch_seconds)
        })
        || (github_yolo_active_legacy_repos(record, now_epoch_seconds)
            && record.repos.iter().any(|allowed| allowed == repo))
}

pub(super) fn resolve_publisher_target(config: &Config, repo: &str) -> Option<PublishTarget> {
    if let Some(target) = config
        .publisher_targets
        .iter()
        .find(|target| target.repo == repo && target.installation_id != 0)
    {
        return Some(target.clone());
    }

    let account = repo.split_once('/')?.0;
    let installation = config.publisher_installations.iter().find(|installation| {
        installation.account == account && installation.installation_id != 0
    })?;
    Some(PublishTarget {
        id: repo.to_string(),
        repo: repo.to_string(),
        default_base: installation.default_base.clone(),
        installation_id: installation.installation_id,
    })
}

fn normalize_github_repo(path: &str) -> Option<String> {
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

fn current_epoch_seconds() -> Result<u64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before the Unix epoch")?
        .as_secs())
}
