use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use rand::distr::{Alphanumeric, SampleString};
use reqwest::header::{ACCEPT, AUTHORIZATION, HeaderMap, HeaderValue, USER_AGENT};
use serde::{Deserialize, Serialize};
use tempfile::tempdir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};

use crate::config::{Config, PublishTarget};

const GITHUB_API_BASE: &str = "https://api.github.com";
const GITHUB_API_VERSION: &str = "2022-11-28";
const SOCKET_DIR_MODE: u32 = 0o750;
const SOCKET_MODE: u32 = 0o660;
const ASKPASS_MODE: u32 = 0o700;
const MAX_SOCKET_REQUEST_BYTES: usize = 16 * 1024 * 1024;
const IMPORTED_REF: &str = "refs/heads/__zodex_imported";
const ASKPASS_SCRIPT_NAME: &str = "git-askpass.sh";
const DEFAULT_USER_AGENT: &str = "zodex-prd/0.1";
const GITHUB_MODE_STATE_PATH: &str = "/var/lib/zodex/mode/state.json";
const DIRECT_PUSH_IMPORTED_REF: &str = "refs/heads/__zodex_direct_push";

fn ensure_publisher_socket_parent_dir(socket_path: &Path) -> Result<()> {
    if let Some(parent) = socket_path.parent() {
        fs::create_dir_all(parent).with_context(|| {
            format!(
                "failed to create publisher socket directory {}",
                parent.display()
            )
        })?;
        fs::set_permissions(parent, fs::Permissions::from_mode(SOCKET_DIR_MODE))
            .with_context(|| format!("failed to chmod {}", parent.display()))?;
    }

    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishPrRequest {
    pub repo_id: String,
    #[serde(default)]
    pub base: Option<String>,
    pub title: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub draft: bool,
    pub bundle_base64: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirectPushRequest {
    pub repo: String,
    pub src: String,
    pub dst: String,
    #[serde(default)]
    pub force: bool,
    #[serde(default)]
    pub bundle_base64: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DirectPushResponse {
    pub repo: String,
    pub dst: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PublishPrResponse {
    pub pr_url: String,
    pub branch: String,
    pub pull_number: u64,
}

#[derive(Debug, Serialize, Deserialize)]
struct PublishPrError {
    error: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum PublisherRequest {
    PublishPr(PublishPrRequest),
    DirectPush(DirectPushRequest),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum PublisherResponse {
    PublishPr(PublishPrResponse),
    DirectPush(DirectPushResponse),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct GithubModeRecord {
    mode: String,
    #[serde(default)]
    all_installed: bool,
    #[serde(default)]
    repos: Vec<String>,
    #[serde(default)]
    expires_at_epoch_seconds: Option<u64>,
}

#[derive(Debug, Serialize)]
struct GithubAppClaims {
    iat: u64,
    exp: u64,
    iss: String,
}

#[derive(Debug, Deserialize)]
struct InstallationTokenResponse {
    token: String,
    #[serde(default)]
    expires_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MintedInstallationToken {
    pub token: String,
    pub expires_at: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CreatePullRequestResponse {
    html_url: String,
    number: u64,
}

#[derive(Debug, Serialize)]
struct CreatePullRequestPayload<'a> {
    title: &'a str,
    body: &'a str,
    head: &'a str,
    base: &'a str,
    draft: bool,
}

pub async fn serve_publisher(config: Config) -> Result<()> {
    validate_publisher_config(&config)?;

    let socket_path = Path::new(&config.publisher_socket_path);
    ensure_publisher_socket_parent_dir(socket_path)?;

    if socket_path.exists() {
        fs::remove_file(socket_path)
            .with_context(|| format!("failed to remove stale socket {}", socket_path.display()))?;
    }

    let listener = UnixListener::bind(socket_path).with_context(|| {
        format!(
            "failed to bind publisher socket at {}",
            socket_path.display()
        )
    })?;
    fs::set_permissions(socket_path, fs::Permissions::from_mode(SOCKET_MODE))
        .with_context(|| format!("failed to chmod {}", socket_path.display()))?;

    loop {
        let (stream, _) = listener
            .accept()
            .await
            .context("failed to accept publisher connection")?;
        let config = config.clone();
        tokio::spawn(async move {
            if let Err(err) = handle_connection(stream, config).await {
                tracing::error!(error = %err, "publisher request failed");
            }
        });
    }
}

pub async fn submit_publish_request(
    socket_path: &Path,
    request: &PublishPrRequest,
) -> Result<PublishPrResponse> {
    let payload = serde_json::to_vec(&PublisherRequest::PublishPr(request.clone()))
        .context("failed to serialize publish request")?;
    let response = submit_publisher_payload(socket_path, &payload).await?;
    match serde_json::from_slice::<PublisherResponse>(&response) {
        Ok(PublisherResponse::PublishPr(response)) => Ok(response),
        Ok(PublisherResponse::DirectPush(_)) => {
            bail!("publisher returned unexpected response type")
        }
        Err(_) => serde_json::from_slice(&response).context("failed to decode publish response"),
    }
}

pub async fn submit_direct_push_request(
    socket_path: &Path,
    request: &DirectPushRequest,
) -> Result<DirectPushResponse> {
    let payload = serde_json::to_vec(&PublisherRequest::DirectPush(request.clone()))
        .context("failed to serialize direct push request")?;
    let response = submit_publisher_payload(socket_path, &payload).await?;
    match serde_json::from_slice::<PublisherResponse>(&response) {
        Ok(PublisherResponse::DirectPush(response)) => Ok(response),
        Ok(PublisherResponse::PublishPr(_)) => bail!("publisher returned unexpected response type"),
        Err(_) => {
            serde_json::from_slice(&response).context("failed to decode direct push response")
        }
    }
}

async fn submit_publisher_payload(socket_path: &Path, payload: &[u8]) -> Result<Vec<u8>> {
    if payload.len() > MAX_SOCKET_REQUEST_BYTES {
        bail!("publisher request exceeds local socket limit");
    }

    let mut stream = UnixStream::connect(socket_path).await.with_context(|| {
        format!(
            "failed to connect to publisher socket {}",
            socket_path.display()
        )
    })?;
    stream
        .write_all(payload)
        .await
        .context("failed to write publish request")?;
    stream
        .shutdown()
        .await
        .context("failed to close publisher request stream")?;

    let mut response_buf = Vec::new();
    stream
        .read_to_end(&mut response_buf)
        .await
        .context("failed to read publisher response")?;
    if response_buf.is_empty() {
        bail!("publisher returned an empty response");
    }

    if let Ok(error) = serde_json::from_slice::<PublishPrError>(&response_buf) {
        bail!(error.error);
    }

    Ok(response_buf)
}

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

fn parse_github_remote_repo(remote_url: &str) -> Option<String> {
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

pub fn validate_publish_request(
    config: &Config,
    request: &PublishPrRequest,
) -> Result<(PublishTarget, Vec<u8>)> {
    let target = config
        .publisher_targets
        .iter()
        .find(|target| target.id == request.repo_id)
        .cloned()
        .ok_or_else(|| anyhow!("repo id not allowed for publishing: {}", request.repo_id))?;

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

async fn handle_connection(mut stream: UnixStream, config: Config) -> Result<()> {
    let mut request_bytes = Vec::new();
    stream
        .read_to_end(&mut request_bytes)
        .await
        .context("failed to read publisher request")?;

    let response = match decode_request(&request_bytes) {
        Ok(PublisherRequest::PublishPr(request)) => {
            match validate_publish_request(&config, &request).map(|validated| (request, validated))
            {
                Ok((request, (target, bundle_bytes))) => {
                    match handle_publish_request(&config, request, &target, &bundle_bytes).await {
                        Ok(response) => serde_json::to_vec(&PublisherResponse::PublishPr(response))
                            .context("failed to encode publish response")?,
                        Err(err) => encode_publisher_error("publish-pr", &err)?,
                    }
                }
                Err(err) => encode_publisher_error("publish-pr validation", &err)?,
            }
        }
        Ok(PublisherRequest::DirectPush(request)) => {
            match handle_direct_push_request(&config, request).await {
                Ok(response) => serde_json::to_vec(&PublisherResponse::DirectPush(response))
                    .context("failed to encode direct push response")?,
                Err(err) => encode_publisher_error("direct push", &err)?,
            }
        }
        Err(err) => encode_publisher_error("publisher request decode", &err)?,
    };

    stream
        .write_all(&response)
        .await
        .context("failed to write publisher response")?;
    stream
        .shutdown()
        .await
        .context("failed to close publisher response stream")?;
    Ok(())
}

fn encode_publisher_error(operation: &str, err: &anyhow::Error) -> Result<Vec<u8>> {
    let error = error_chain_string(err);
    tracing::error!(operation, error = %error, "publisher operation failed");
    serde_json::to_vec(&PublishPrError { error })
        .context("failed to encode publisher error response")
}

fn error_chain_string(err: &anyhow::Error) -> String {
    err.chain()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(": ")
}

fn decode_request(request_bytes: &[u8]) -> Result<PublisherRequest> {
    if request_bytes.is_empty() {
        bail!("publisher request body was empty");
    }
    if request_bytes.len() > MAX_SOCKET_REQUEST_BYTES {
        bail!("publisher request exceeds socket size limit");
    }

    if let Ok(request) = serde_json::from_slice::<PublisherRequest>(request_bytes) {
        return Ok(request);
    }

    let legacy: PublishPrRequest =
        serde_json::from_slice(request_bytes).context("failed to decode publisher request")?;
    Ok(PublisherRequest::PublishPr(legacy))
}

async fn handle_direct_push_request(
    config: &Config,
    request: DirectPushRequest,
) -> Result<DirectPushResponse> {
    let target = validate_direct_push_request(config, &request)?;
    let token = mint_publisher_installation_token(
        config
            .publisher_app_id
            .ok_or_else(|| anyhow!("publisher_app_id is not configured"))?,
        Path::new(&config.publisher_private_key_path),
        target.installation_id,
    )
    .await?;

    let tempdir = tempdir().context("failed to create publisher tempdir")?;
    let askpass_path = write_askpass_script(tempdir.path())?;
    let repo_dir = clone_repo_with_token(tempdir.path(), &token, &askpass_path, &target.repo)?;

    if request.src.is_empty() {
        git_with_token(
            &repo_dir,
            &token,
            &askpass_path,
            &[
                "push",
                &github_repo_https_url(&target.repo),
                &format!(":{}", request.dst),
            ],
        )?;
    } else {
        let bundle_base64 = request
            .bundle_base64
            .as_deref()
            .ok_or_else(|| anyhow!("direct push bundle is required for non-delete pushes"))?;
        let bundle_bytes = BASE64
            .decode(bundle_base64.as_bytes())
            .context("direct push bundle was not valid base64")?;
        if bundle_bytes.is_empty() {
            bail!("direct push bundle cannot be empty");
        }
        if bundle_bytes.len() > config.publisher_max_bundle_bytes {
            bail!(
                "direct push bundle exceeds limit ({} bytes > {} bytes)",
                bundle_bytes.len(),
                config.publisher_max_bundle_bytes
            );
        }
        let bundle_path = tempdir.path().join("direct-push.bundle");
        fs::write(&bundle_path, bundle_bytes)
            .with_context(|| format!("failed to write {}", bundle_path.display()))?;
        git_plain(
            &repo_dir,
            &[
                "fetch",
                bundle_path.to_str().unwrap(),
                &format!("{}:{DIRECT_PUSH_IMPORTED_REF}", request.src),
            ],
        )?;
        let refspec = if request.force {
            format!("+{DIRECT_PUSH_IMPORTED_REF}:{}", request.dst)
        } else {
            format!("{DIRECT_PUSH_IMPORTED_REF}:{}", request.dst)
        };
        git_with_token(
            &repo_dir,
            &token,
            &askpass_path,
            &["push", &github_repo_https_url(&target.repo), &refspec],
        )?;
    }

    Ok(DirectPushResponse {
        repo: target.repo,
        dst: request.dst,
    })
}

async fn handle_publish_request(
    config: &Config,
    request: PublishPrRequest,
    target: &PublishTarget,
    bundle_bytes: &[u8],
) -> Result<PublishPrResponse> {
    let token = mint_publisher_installation_token(
        config
            .publisher_app_id
            .ok_or_else(|| anyhow!("publisher_app_id is not configured"))?,
        Path::new(&config.publisher_private_key_path),
        target.installation_id,
    )
    .await?;

    let tempdir = tempdir().context("failed to create publisher tempdir")?;
    let askpass_path = write_askpass_script(tempdir.path())?;
    let bundle_path = tempdir.path().join("request.bundle");
    fs::write(&bundle_path, bundle_bytes)
        .with_context(|| format!("failed to write {}", bundle_path.display()))?;

    let repo_dir = tempdir.path().join("repo");
    git_with_token(
        tempdir.path(),
        &token,
        &askpass_path,
        &[
            "clone",
            "--quiet",
            &github_repo_https_url(&target.repo),
            repo_dir.to_str().unwrap(),
        ],
    )?;

    git_plain(
        &repo_dir,
        &[
            "fetch",
            bundle_path.to_str().unwrap(),
            &format!("HEAD:{IMPORTED_REF}"),
        ],
    )?;

    let branch = build_publish_branch_name(&config.publisher_branch_prefix);
    git_plain(&repo_dir, &["checkout", "-B", &branch, IMPORTED_REF])?;
    git_with_token(
        &repo_dir,
        &token,
        &askpass_path,
        &["push", "origin", &format!("HEAD:refs/heads/{branch}")],
    )?;

    let pr = create_pull_request(
        &token,
        &target.repo,
        request.base.as_deref().unwrap_or(&target.default_base),
        &branch,
        &request.title,
        &request.body,
        request.draft,
    )
    .await?;

    Ok(pr)
}

fn validate_publisher_config(config: &Config) -> Result<()> {
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

fn validate_direct_push_request(
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

    let mode = load_active_github_yolo_mode(Path::new(GITHUB_MODE_STATE_PATH))?;
    if !github_mode_allows_repo(&mode, &repo) {
        bail!("YOLO mode is not active for repo {repo}");
    }

    resolve_direct_push_target(config, &repo)
        .ok_or_else(|| anyhow!("repo {repo} is not covered by publisher installation config"))
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

fn github_mode_expired(record: &GithubModeRecord, now_epoch_seconds: u64) -> bool {
    matches!(
        record.expires_at_epoch_seconds,
        Some(expires_at_epoch_seconds) if expires_at_epoch_seconds <= now_epoch_seconds
    )
}

fn github_mode_allows_repo(record: &GithubModeRecord, repo: &str) -> bool {
    record.all_installed || record.repos.iter().any(|allowed| allowed == repo)
}

fn resolve_direct_push_target(config: &Config, repo: &str) -> Option<PublishTarget> {
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

fn github_repo_https_url(repo: &str) -> String {
    format!("https://github.com/{repo}.git")
}

fn clone_repo_with_token(
    parent_dir: &Path,
    token: &str,
    askpass_path: &Path,
    repo: &str,
) -> Result<PathBuf> {
    let repo_dir = parent_dir.join("repo");
    git_with_token(
        parent_dir,
        token,
        askpass_path,
        &[
            "clone",
            "--quiet",
            "--no-checkout",
            &github_repo_https_url(repo),
            repo_dir.to_str().unwrap(),
        ],
    )?;
    Ok(repo_dir)
}

fn write_askpass_script(dir: &Path) -> Result<PathBuf> {
    let script_path = dir.join(ASKPASS_SCRIPT_NAME);
    let mut file = fs::File::create(&script_path)
        .with_context(|| format!("failed to create {}", script_path.display()))?;
    file.write_all(
        br#"#!/usr/bin/env bash
case "$1" in
  *Username*) printf '%s\n' "x-access-token" ;;
  *Password*) printf '%s\n' "${GITHUB_APP_TOKEN}" ;;
  *) printf '\n' ;;
esac
"#,
    )
    .with_context(|| format!("failed to write {}", script_path.display()))?;
    fs::set_permissions(&script_path, fs::Permissions::from_mode(ASKPASS_MODE))
        .with_context(|| format!("failed to chmod {}", script_path.display()))?;
    Ok(script_path)
}

fn git_plain(cwd: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .current_dir(cwd)
        .args(args)
        .output()
        .with_context(|| format!("failed to run git {}", args.join(" ")))?;
    check_command_output("git", args, output)
}

fn git_with_token(cwd: &Path, token: &str, askpass_path: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .current_dir(cwd)
        .env("GITHUB_APP_TOKEN", token)
        .env("GIT_ASKPASS", askpass_path)
        .env("GIT_TERMINAL_PROMPT", "0")
        .args(args)
        .output()
        .with_context(|| format!("failed to run authenticated git {}", args.join(" ")))?;
    check_command_output("git", args, output)
}

fn check_command_output(
    program: &str,
    args: &[&str],
    output: std::process::Output,
) -> Result<String> {
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    if output.status.success() {
        return Ok(stdout);
    }

    let status = output.status.code().map_or_else(
        || "terminated by signal".to_string(),
        |code| code.to_string(),
    );
    let details = if stderr.trim().is_empty() {
        stdout.trim()
    } else {
        stderr.trim()
    };
    bail!(
        "{program} {} failed (status: {status})\n{details}",
        args.join(" ")
    )
}

pub async fn mint_reader_installation_token(
    app_id: u64,
    private_key_path: &Path,
    installation_id: u64,
) -> Result<String> {
    Ok(mint_installation_token(
        app_id,
        private_key_path,
        installation_id,
        TokenPermissionProfile::Reader,
    )
    .await?
    .token)
}

pub async fn mint_publisher_installation_token(
    app_id: u64,
    private_key_path: &Path,
    installation_id: u64,
) -> Result<String> {
    Ok(mint_installation_token(
        app_id,
        private_key_path,
        installation_id,
        TokenPermissionProfile::Publisher,
    )
    .await?
    .token)
}

pub async fn mint_publisher_installation_token_with_metadata(
    app_id: u64,
    private_key_path: &Path,
    installation_id: u64,
) -> Result<MintedInstallationToken> {
    mint_installation_token(
        app_id,
        private_key_path,
        installation_id,
        TokenPermissionProfile::Publisher,
    )
    .await
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TokenPermissionProfile {
    Reader,
    Publisher,
}

async fn mint_installation_token(
    app_id: u64,
    private_key_path: &Path,
    installation_id: u64,
    permissions: TokenPermissionProfile,
) -> Result<MintedInstallationToken> {
    let key_pem = fs::read(private_key_path)
        .with_context(|| format!("failed to read {}", private_key_path.display()))?;
    let encoding_key = EncodingKey::from_rsa_pem(&key_pem).with_context(|| {
        format!(
            "failed to parse {} RSA private key",
            permissions.private_key_label()
        )
    })?;

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before UNIX_EPOCH")?
        .as_secs();
    let claims = GithubAppClaims {
        iat: now.saturating_sub(60),
        exp: now + 540,
        iss: app_id.to_string(),
    };
    let jwt = encode(&Header::new(Algorithm::RS256), &claims, &encoding_key)
        .context("failed to encode GitHub App JWT")?;

    let client = reqwest::Client::new();
    let response = client
        .post(format!(
            "{GITHUB_API_BASE}/app/installations/{installation_id}/access_tokens"
        ))
        .header(ACCEPT, "application/vnd.github+json")
        .header(AUTHORIZATION, format!("Bearer {jwt}"))
        .header("X-GitHub-Api-Version", GITHUB_API_VERSION)
        .header(USER_AGENT, DEFAULT_USER_AGENT)
        .json(&serde_json::json!({ "permissions": permissions.github_permissions() }))
        .send()
        .await
        .context("failed to request GitHub installation token")?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        bail!("GitHub installation token request failed ({status}): {body}");
    }

    let payload: InstallationTokenResponse = response
        .json()
        .await
        .context("failed to decode GitHub installation token response")?;
    Ok(MintedInstallationToken {
        token: payload.token,
        expires_at: payload.expires_at,
    })
}

pub async fn resolve_repo_installation_id(
    app_id: u64,
    private_key_path: &Path,
    repo: &str,
) -> Result<u64> {
    #[derive(Debug, Deserialize)]
    struct RepoInstallationResponse {
        id: u64,
    }

    let key_pem = fs::read(private_key_path)
        .with_context(|| format!("failed to read {}", private_key_path.display()))?;
    let encoding_key =
        EncodingKey::from_rsa_pem(&key_pem).context("failed to parse publisher RSA private key")?;

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before UNIX_EPOCH")?
        .as_secs();
    let claims = GithubAppClaims {
        iat: now.saturating_sub(60),
        exp: now + 540,
        iss: app_id.to_string(),
    };
    let jwt = encode(&Header::new(Algorithm::RS256), &claims, &encoding_key)
        .context("failed to encode GitHub App JWT")?;

    let client = reqwest::Client::new();
    let response = client
        .get(format!("{GITHUB_API_BASE}/repos/{repo}/installation"))
        .header(ACCEPT, "application/vnd.github+json")
        .header(AUTHORIZATION, format!("Bearer {jwt}"))
        .header("X-GitHub-Api-Version", GITHUB_API_VERSION)
        .header(USER_AGENT, DEFAULT_USER_AGENT)
        .send()
        .await
        .context("failed to resolve GitHub App installation for repo")?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        bail!("GitHub repo installation lookup failed ({status}): {body}");
    }

    let payload: RepoInstallationResponse = response
        .json()
        .await
        .context("failed to decode GitHub repo installation response")?;
    Ok(payload.id)
}

impl TokenPermissionProfile {
    fn private_key_label(self) -> &'static str {
        match self {
            Self::Reader => "reader",
            Self::Publisher => "publisher",
        }
    }

    fn github_permissions(self) -> serde_json::Value {
        match self {
            Self::Reader => serde_json::json!({
                "contents": "read"
            }),
            Self::Publisher => serde_json::json!({
                "contents": "write",
                "pull_requests": "write"
            }),
        }
    }
}

/// Create a GitHub pull request via the REST API using an already-resolved token.
///
/// This is shared by the controlled publisher flow after it pushes a generated
/// branch. It performs a single `POST /repos/{repo}/pulls` call and never shells
/// out to `gh`.
pub async fn create_pull_request(
    token: &str,
    repo: &str,
    base: &str,
    branch: &str,
    title: &str,
    body: &str,
    draft: bool,
) -> Result<PublishPrResponse> {
    let mut headers = HeaderMap::new();
    headers.insert(
        ACCEPT,
        HeaderValue::from_static("application/vnd.github+json"),
    );
    headers.insert(USER_AGENT, HeaderValue::from_static(DEFAULT_USER_AGENT));
    headers.insert(
        AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {token}"))
            .context("failed to build authorization header")?,
    );
    headers.insert(
        "X-GitHub-Api-Version",
        HeaderValue::from_static(GITHUB_API_VERSION),
    );

    let client = reqwest::Client::new();
    let response = client
        .post(format!("{GITHUB_API_BASE}/repos/{repo}/pulls"))
        .headers(headers)
        .json(&CreatePullRequestPayload {
            title,
            body,
            head: branch,
            base,
            draft,
        })
        .send()
        .await
        .context("failed to create GitHub pull request")?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        bail!("GitHub pull request creation failed ({status}): {body}");
    }

    let payload: CreatePullRequestResponse = response
        .json()
        .await
        .context("failed to decode GitHub pull request response")?;

    Ok(PublishPrResponse {
        pr_url: payload.html_url,
        branch: branch.to_string(),
        pull_number: payload.number,
    })
}

#[cfg(test)]
mod tests {
    use base64::Engine as _;
    use std::os::unix::fs::PermissionsExt;

    use super::{
        GithubModeRecord, PublishPrRequest, SOCKET_DIR_MODE, TokenPermissionProfile,
        build_publish_branch_name, build_publish_request, create_head_bundle,
        ensure_clean_worktree, ensure_publisher_socket_parent_dir, github_mode_allows_repo,
        github_mode_expired, parse_github_remote_repo, resolve_direct_push_target,
        validate_publish_request,
    };
    use crate::config::{Config, PublishTarget, PublisherInstallation};
    use tempfile::tempdir;

    #[test]
    fn branch_name_uses_prefix_namespace_and_never_equals_main() {
        let branch = build_publish_branch_name("main");
        assert!(branch.starts_with("main/"));
        assert_ne!(branch, "main");
    }

    #[test]
    fn validate_publish_request_rejects_unknown_repo_id() {
        let cfg = Config::default();
        let err = validate_publish_request(
            &cfg,
            &PublishPrRequest {
                repo_id: "missing".to_string(),
                base: None,
                title: "title".to_string(),
                body: String::new(),
                draft: false,
                bundle_base64: base64::engine::general_purpose::STANDARD.encode(b"hello"),
            },
        )
        .expect_err("request should be rejected");

        assert!(err.to_string().contains("repo id not allowed"));
    }

    #[test]
    fn github_yolo_mode_scope_checks_repo_allowlist_and_expiry() {
        let record = GithubModeRecord {
            mode: "yolo".to_string(),
            all_installed: false,
            repos: vec!["owner/repo".to_string()],
            expires_at_epoch_seconds: Some(1_000),
        };
        assert!(github_mode_allows_repo(&record, "owner/repo"));
        assert!(!github_mode_allows_repo(&record, "owner/other"));
        assert!(!github_mode_expired(&record, 999));
        assert!(github_mode_expired(&record, 1_000));

        let all_installed = GithubModeRecord {
            all_installed: true,
            repos: Vec::new(),
            ..record
        };
        assert!(github_mode_allows_repo(&all_installed, "owner/other"));
    }

    #[test]
    fn direct_push_target_resolves_exact_target_before_account_installation() {
        let cfg = Config {
            publisher_installations: vec![PublisherInstallation {
                account: "owner".to_string(),
                installation_id: 11,
                default_base: "main".to_string(),
            }],
            publisher_targets: vec![PublishTarget {
                id: "custom".to_string(),
                repo: "owner/repo".to_string(),
                default_base: "trunk".to_string(),
                installation_id: 22,
            }],
            ..Config::default()
        };

        let exact = resolve_direct_push_target(&cfg, "owner/repo").expect("exact target");
        assert_eq!(exact.installation_id, 22);
        assert_eq!(exact.default_base, "trunk");

        let account = resolve_direct_push_target(&cfg, "owner/other").expect("account target");
        assert_eq!(account.repo, "owner/other");
        assert_eq!(account.installation_id, 11);

        assert!(resolve_direct_push_target(&cfg, "other/repo").is_none());
    }

    #[test]
    fn token_permission_profiles_keep_reader_and_publisher_separate() {
        assert_eq!(
            TokenPermissionProfile::Reader.github_permissions(),
            serde_json::json!({ "contents": "read" })
        );
        assert_eq!(
            TokenPermissionProfile::Publisher.github_permissions(),
            serde_json::json!({
                "contents": "write",
                "pull_requests": "write"
            })
        );
    }

    #[test]
    fn validate_publish_request_rejects_oversize_fields() {
        let cfg = Config {
            publisher_max_title_chars: 5,
            publisher_max_body_chars: 5,
            publisher_max_bundle_bytes: 4,
            publisher_targets: vec![PublishTarget {
                id: "repo".to_string(),
                repo: "owner/repo".to_string(),
                default_base: "main".to_string(),
                installation_id: 1,
            }],
            ..Config::default()
        };

        let err = validate_publish_request(
            &cfg,
            &PublishPrRequest {
                repo_id: "repo".to_string(),
                base: None,
                title: "too long".to_string(),
                body: "123456".to_string(),
                draft: false,
                bundle_base64: base64::engine::general_purpose::STANDARD.encode(b"12345"),
            },
        )
        .expect_err("oversize request should fail");

        assert!(err.to_string().contains("PR title exceeds limit"));
    }

    #[test]
    fn create_head_bundle_roundtrips_head_ref() {
        let tempdir = tempdir().expect("tempdir");
        let repo = tempdir.path().join("repo");
        std::fs::create_dir_all(&repo).expect("repo dir");

        let init_status = std::process::Command::new("git")
            .current_dir(&repo)
            .args(["init", "-q"])
            .status()
            .expect("git init");
        assert!(init_status.success(), "git init should succeed");
        std::process::Command::new("git")
            .current_dir(&repo)
            .args(["config", "user.email", "test@example.com"])
            .status()
            .expect("git config email");
        std::process::Command::new("git")
            .current_dir(&repo)
            .args(["config", "user.name", "Test"])
            .status()
            .expect("git config name");
        std::fs::write(repo.join("a.txt"), "hello\n").expect("write file");
        std::process::Command::new("git")
            .current_dir(&repo)
            .args(["add", "a.txt"])
            .status()
            .expect("git add");
        std::process::Command::new("git")
            .current_dir(&repo)
            .args(["commit", "-qm", "init"])
            .status()
            .expect("git commit");

        let bundle = create_head_bundle(&repo).expect("bundle should be created");
        let bundle_path = tempdir.path().join("request.bundle");
        std::fs::write(&bundle_path, bundle).expect("write bundle");

        let output = std::process::Command::new("git")
            .args(["bundle", "list-heads", bundle_path.to_str().unwrap()])
            .output()
            .expect("list bundle heads");
        assert!(output.status.success());
        assert!(String::from_utf8_lossy(&output.stdout).contains("HEAD"));
    }

    #[test]
    fn parse_github_remote_repo_supports_common_remote_shapes() {
        assert_eq!(
            parse_github_remote_repo("https://github.com/amxv/zodex.git"),
            Some("amxv/zodex".to_string())
        );
        assert_eq!(
            parse_github_remote_repo("ssh://git@github.com/amxv/zodex.git"),
            Some("amxv/zodex".to_string())
        );
        assert_eq!(
            parse_github_remote_repo("git@github.com:amxv/zodex.git"),
            Some("amxv/zodex".to_string())
        );
    }

    #[test]
    fn build_publish_request_rejects_checkout_repo_mismatch() {
        let tempdir = tempdir().expect("tempdir");
        let repo = tempdir.path().join("repo");
        std::fs::create_dir_all(&repo).expect("repo dir");

        let init_status = std::process::Command::new("git")
            .current_dir(&repo)
            .args(["init", "-q"])
            .status()
            .expect("git init");
        assert!(init_status.success(), "git init should succeed");
        std::process::Command::new("git")
            .current_dir(&repo)
            .args(["config", "user.email", "test@example.com"])
            .status()
            .expect("git config email");
        std::process::Command::new("git")
            .current_dir(&repo)
            .args(["config", "user.name", "Test"])
            .status()
            .expect("git config name");
        std::process::Command::new("git")
            .current_dir(&repo)
            .args([
                "remote",
                "add",
                "origin",
                "https://github.com/amxv/other.git",
            ])
            .status()
            .expect("git remote add origin");
        std::fs::write(repo.join("a.txt"), "hello\n").expect("write file");
        std::process::Command::new("git")
            .current_dir(&repo)
            .args(["add", "a.txt"])
            .status()
            .expect("git add");
        std::process::Command::new("git")
            .current_dir(&repo)
            .args(["commit", "-qm", "init"])
            .status()
            .expect("git commit");

        let err = build_publish_request(
            &Config::default(),
            "amxv/zodex".to_string(),
            None,
            "Title".to_string(),
            String::new(),
            false,
            &repo,
        )
        .expect_err("mismatched checkout should fail");

        assert!(
            err.to_string()
                .contains("current checkout is for amxv/other")
        );
    }

    #[test]
    fn ensure_clean_worktree_rejects_dirty_repo() {
        let tempdir = tempdir().expect("tempdir");
        let repo = tempdir.path().join("repo");
        std::fs::create_dir_all(&repo).expect("repo dir");
        std::process::Command::new("git")
            .current_dir(&repo)
            .args(["init", "-q"])
            .status()
            .expect("git init");
        std::process::Command::new("git")
            .current_dir(&repo)
            .args(["config", "user.email", "test@example.com"])
            .status()
            .expect("git config email");
        std::process::Command::new("git")
            .current_dir(&repo)
            .args(["config", "user.name", "Test"])
            .status()
            .expect("git config name");
        std::fs::write(repo.join("a.txt"), "hello\n").expect("write file");

        let err = ensure_clean_worktree(&repo).expect_err("dirty repo should fail");
        assert!(
            err.to_string()
                .contains("publish-pr requires a clean worktree")
        );
    }

    #[test]
    fn ensure_publisher_socket_parent_dir_sets_group_traversable_mode() {
        let tempdir = tempdir().expect("tempdir");
        let socket_path = tempdir.path().join("publisher/run/zodex-prd.sock");

        ensure_publisher_socket_parent_dir(&socket_path).expect("socket parent dir");

        let metadata = std::fs::metadata(socket_path.parent().expect("socket parent"))
            .expect("socket parent metadata");
        assert_eq!(metadata.permissions().mode() & 0o777, SOCKET_DIR_MODE);
    }
}
