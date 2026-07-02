use std::env;
use std::fs;
use std::io::{self, BufRead, Read, Write};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use clap::{Parser, Subcommand};
use rcgen::{CertificateParams, DistinguishedName, DnType, KeyPair, SanType};
use reqwest::header::{ACCEPT, AUTHORIZATION, USER_AGENT};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use tracing::warn;
use zodex::config::{Config, DEFAULT_CONFIG_PATH};
use zodex::install_rustls_crypto_provider;
use zodex::publisher::{
    DirectPushRequest, build_publish_request, detect_repo_root, mint_reader_installation_token,
    submit_direct_push_request, submit_publish_request,
};
use zodex::redaction::redact_api_key_query_params;
use zodex::server::run_server;

const PUSH_GRANTS_DIR: &str = "/var/lib/zodex/push-grants";
const GITHUB_PUSH_GRANT_DEVICE_CACHE_DIR: &str = ".config/zodex/github-device-flow";
const GITHUB_PUSH_GRANT_CLIENT_ID_ENV: &str = "ZODEX_PUBLISHER_CLIENT_ID";
const DEFAULT_PUSH_GRANT_TTL_SECONDS: u64 = 30 * 60;
const GITHUB_API_BASE: &str = "https://api.github.com";
const GITHUB_API_VERSION: &str = "2022-11-28";
const GITHUB_OAUTH_DEVICE_CODE_URL: &str = "https://github.com/login/device/code";
const GITHUB_OAUTH_ACCESS_TOKEN_URL: &str = "https://github.com/login/oauth/access_token";
const DEFAULT_GITHUB_USER_AGENT: &str = "zodex/0.1";

#[derive(Debug, Parser)]
#[command(name = "zodexd")]
#[command(about = "Zodex daemon for remote execution")]
struct Args {
    #[arg(long, default_value = DEFAULT_CONFIG_PATH)]
    config: String,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Debug, Subcommand)]
enum Commands {
    #[command(hide = true)]
    GitCredentialHelper { operation: String },
    #[command(hide = true)]
    GitRemoteZodex { remote: String, url: String },
    #[command(hide = true)]
    ShowUrl {
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
    },
    #[command(hide = true)]
    Github {
        #[command(subcommand)]
        command: GithubCommand,
    },
    #[command(hide = true)]
    EnsureTls,
}

#[derive(Debug, Subcommand)]
enum GithubCommand {
    RequestPush {
        #[arg(long)]
        repo: String,
        #[arg(long)]
        publisher_client_id: Option<String>,
        #[arg(long, default_value = "30m")]
        ttl: String,
        #[arg(long, default_value_t = false)]
        no_ttl: bool,
        #[arg(long, default_value_t = false)]
        cache_refresh_token: bool,
    },
    RevokePush {
        #[arg(long)]
        repo: String,
        #[arg(long, default_value_t = false)]
        forget_local_auth: bool,
    },
    ListGrants,
    PublishPr {
        #[arg(long)]
        repo: String,
        #[arg(long)]
        title: String,
        #[arg(long)]
        base: Option<String>,
        #[arg(long, default_value = "")]
        body: String,
        #[arg(long, default_value_t = false)]
        draft: bool,
    },
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct GitCredentialRequest {
    protocol: Option<String>,
    host: Option<String>,
    path: Option<String>,
    url: Option<String>,
    username: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct PushGrantRecord {
    repo: String,
    token: String,
    #[serde(default)]
    expires_at: Option<String>,
    #[serde(default)]
    expires_at_epoch_seconds: Option<u64>,
    #[serde(default)]
    token_source: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct CachedDeviceFlowGrant {
    client_id: String,
    repo: String,
    refresh_token: String,
}

#[derive(Debug, Deserialize)]
struct GitHubDeviceCodeResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    expires_in: u64,
    interval: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct GitHubOAuthTokenResponse {
    access_token: Option<String>,
    expires_in: Option<u64>,
    refresh_token: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
    interval: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct GitHubRepoResponse {
    id: u64,
}

#[derive(Debug)]
struct GitHubUserAccessGrant {
    access_token: String,
    expires_in_seconds: Option<u64>,
    refresh_token: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    install_rustls_crypto_provider();

    let args = Args::parse();
    match args.command {
        None => {
            tracing_subscriber::fmt()
                .with_env_filter(
                    std::env::var("RUST_LOG")
                        .unwrap_or_else(|_| "zodex=info,zodexd=info".to_string()),
                )
                .init();

            let config = Config::load(Some(Path::new(&args.config)))?;
            warn!(
                "zodexd exposes high-privilege remote execution; protect API keys and network access"
            );
            run_server(config).await
        }
        Some(command) => run_hidden_command(Path::new(&args.config), command).await,
    }
}

async fn run_hidden_command(config_path: &Path, command: Commands) -> Result<()> {
    match command {
        Commands::GitCredentialHelper { operation } => {
            let config = Config::load(Some(config_path))?;
            handle_git_credential_helper(&config, &operation).await?;
        }
        Commands::GitRemoteZodex { remote: _, url } => {
            let config = Config::load(Some(config_path))?;
            handle_git_remote_zodex(&config, &url).await?;
        }
        Commands::ShowUrl { host } => {
            let config = Config::load(Some(config_path))?;
            let raw_url = format!("https://{host}/mcp?key={}", config.api_key);
            println!(
                "{} (key redacted in CLI output)",
                redact_api_key_query_params(&raw_url)
            );
        }
        Commands::Github { command } => {
            let config = Config::load(Some(config_path))?;
            match command {
                GithubCommand::RequestPush {
                    repo,
                    publisher_client_id,
                    ttl,
                    no_ttl,
                    cache_refresh_token,
                } => {
                    let ttl = if no_ttl {
                        None
                    } else if ttl == "30m" {
                        Some(Duration::from_secs(DEFAULT_PUSH_GRANT_TTL_SECONDS))
                    } else {
                        Some(parse_push_grant_ttl(&ttl)?)
                    };
                    request_push_access(
                        &config,
                        &repo,
                        publisher_client_id.as_deref(),
                        ttl,
                        cache_refresh_token,
                    )
                    .await?;
                }
                GithubCommand::RevokePush {
                    repo,
                    forget_local_auth,
                } => {
                    revoke_push_access(&repo, forget_local_auth)?;
                }
                GithubCommand::ListGrants => {
                    list_push_grants()?;
                }
                GithubCommand::PublishPr {
                    repo,
                    title,
                    base,
                    body,
                    draft,
                } => {
                    publish_pr(&config, &repo, &title, base.as_deref(), &body, draft).await?;
                }
            }
        }
        Commands::EnsureTls => {
            ensure_tls_artifacts(config_path)?;
        }
    }

    Ok(())
}

async fn handle_git_credential_helper(config: &Config, operation: &str) -> Result<()> {
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

async fn handle_git_remote_zodex(config: &Config, url: &str) -> Result<()> {
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

fn git_remote_zodex_repo(url: &str) -> Option<String> {
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

fn git_remote_zodex_push_dst(raw_spec: &str) -> Option<String> {
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

fn create_direct_push_bundle_base64_from_dir(repo_dir: &Path, src: &str) -> Result<Option<String>> {
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

fn resolve_git_object_id(repo_dir: &Path, src: &str) -> Result<String> {
    git_single_line(
        repo_dir,
        &["rev-parse", "--verify", &format!("{src}^{{object}}")],
    )
    .with_context(|| format!("failed to resolve pushed source object {src}"))
}

fn resolve_git_object_type(repo_dir: &Path, src: &str) -> Result<String> {
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

fn sanitize_remote_helper_error(err: &anyhow::Error) -> String {
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

fn git_credential_request_repo(request: &GitCredentialRequest) -> Option<String> {
    let path = request
        .path
        .as_deref()
        .or_else(|| request.url.as_deref().and_then(credential_url_path))?;
    normalize_github_repo(path)
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

fn push_grant_file_name(repo: &str) -> String {
    format!("{}.json", repo.replace('/', "__"))
}

fn push_grant_path(repo: &str) -> std::path::PathBuf {
    Path::new(PUSH_GRANTS_DIR).join(push_grant_file_name(repo))
}

fn push_grant_expired(grant: &PushGrantRecord, now_epoch_seconds: u64) -> bool {
    matches!(
        grant.expires_at_epoch_seconds,
        Some(expires_at_epoch_seconds) if expires_at_epoch_seconds <= now_epoch_seconds
    )
}

fn current_epoch_seconds() -> Result<u64> {
    Ok(std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .context("system clock is before the Unix epoch")?
        .as_secs())
}

fn format_epoch_seconds_rfc3339(epoch_seconds: u64) -> Result<String> {
    OffsetDateTime::from_unix_timestamp(epoch_seconds as i64)
        .context("failed to build RFC3339 timestamp from epoch seconds")?
        .format(&Rfc3339)
        .context("failed to format RFC3339 timestamp")
}

fn expires_at_from_now(expires_in_seconds: u64) -> Result<(String, u64)> {
    let expires_at_epoch_seconds = current_epoch_seconds()?
        .checked_add(expires_in_seconds)
        .ok_or_else(|| anyhow!("push grant expiration overflowed"))?;
    Ok((
        format_epoch_seconds_rfc3339(expires_at_epoch_seconds)?,
        expires_at_epoch_seconds,
    ))
}

fn parse_push_grant_ttl(raw: &str) -> Result<Duration> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        bail!("push grant TTL must not be empty");
    }
    let unit = trimmed
        .chars()
        .last()
        .ok_or_else(|| anyhow!("push grant TTL must not be empty"))?;
    let (value_part, multiplier_seconds) = if unit.is_ascii_alphabetic() {
        let value = &trimmed[..trimmed.len() - unit.len_utf8()];
        let multiplier = match unit {
            's' | 'S' => 1,
            'm' | 'M' => 60,
            'h' | 'H' => 60 * 60,
            'd' | 'D' => 60 * 60 * 24,
            _ => bail!("unsupported push grant TTL unit `{unit}`; use s, m, h, or d"),
        };
        (value, multiplier)
    } else {
        (trimmed, 1)
    };
    let amount = value_part
        .parse::<u64>()
        .with_context(|| format!("failed to parse push grant TTL `{raw}`"))?;
    if amount == 0 {
        bail!("push grant TTL must be greater than zero");
    }
    let seconds = amount
        .checked_mul(multiplier_seconds)
        .ok_or_else(|| anyhow!("push grant TTL is too large"))?;
    Ok(Duration::from_secs(seconds))
}

fn write_local_push_grant(repo: &str, grant: &PushGrantRecord) -> Result<()> {
    let path = push_grant_path(repo);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let raw = serde_json::to_vec_pretty(grant).context("failed to encode push grant")?;
    fs::write(&path, raw).with_context(|| format!("failed to write {}", path.display()))?;
    #[cfg(unix)]
    {
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640))
            .with_context(|| format!("failed to chmod {}", path.display()))?;
    }
    Ok(())
}

fn load_push_grant_from_dir(repo: &str, grants_dir: &Path) -> Result<Option<PushGrantRecord>> {
    let path = grants_dir.join(push_grant_file_name(repo));
    if !path.exists() {
        return Ok(None);
    }

    let raw =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    let grant: PushGrantRecord = serde_json::from_str(&raw)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    if push_grant_expired(&grant, current_epoch_seconds()?) {
        let _ = fs::remove_file(&path);
        return Ok(None);
    }
    Ok(Some(grant))
}

fn load_matching_push_grant(
    request: &GitCredentialRequest,
    grants_dir: &Path,
) -> Result<Option<PushGrantRecord>> {
    let Some(repo) = git_credential_request_repo(request) else {
        return Ok(None);
    };
    load_push_grant_from_dir(&repo, grants_dir)
}

fn parse_push_grants(raw: &str) -> Result<Vec<PushGrantRecord>> {
    serde_json::Deserializer::from_str(raw)
        .into_iter::<PushGrantRecord>()
        .map(|grant| grant.context("failed to parse push grant"))
        .collect()
}

fn push_grant_cache_path(repo: &str) -> Result<std::path::PathBuf> {
    let home = env::var("HOME").context("HOME must be set to use GitHub App device flow")?;
    let root = Path::new(&home).join(GITHUB_PUSH_GRANT_DEVICE_CACHE_DIR);
    Ok(root.join(push_grant_file_name(repo)))
}

fn save_cached_device_flow_grant(repo: &str, grant: &CachedDeviceFlowGrant) -> Result<()> {
    let path = push_grant_cache_path(repo)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let raw =
        serde_json::to_vec_pretty(grant).context("failed to encode cached device-flow grant")?;
    fs::write(&path, raw).with_context(|| format!("failed to write {}", path.display()))?;
    #[cfg(unix)]
    {
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
            .with_context(|| format!("failed to chmod {}", path.display()))?;
    }
    Ok(())
}

fn load_cached_device_flow_grant(
    repo: &str,
    client_id: &str,
) -> Result<Option<CachedDeviceFlowGrant>> {
    let path = push_grant_cache_path(repo)?;
    if !path.exists() {
        return Ok(None);
    }

    let raw =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    let grant: CachedDeviceFlowGrant = serde_json::from_str(&raw)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    if grant.client_id != client_id {
        return Ok(None);
    }
    Ok(Some(grant))
}

fn remove_cached_device_flow_grant(repo: &str) -> Result<bool> {
    let path = push_grant_cache_path(repo)?;
    if !path.exists() {
        return Ok(false);
    }
    fs::remove_file(&path).with_context(|| format!("failed to remove {}", path.display()))?;
    Ok(true)
}

fn best_effort_open_browser(url: &str) -> bool {
    for (program, args) in browser_open_attempts(url) {
        let status = Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        if matches!(status, Ok(status) if status.success()) {
            return true;
        }
    }
    false
}

fn browser_open_attempts(url: &str) -> Vec<(&'static str, Vec<&str>)> {
    if cfg!(target_os = "macos") {
        return vec![("open", vec![url])];
    }
    if cfg!(target_os = "windows") {
        return vec![("cmd", vec!["/C", "start", "", url])];
    }

    let mut attempts = Vec::new();
    if env::var_os("WSL_DISTRO_NAME").is_some() {
        attempts.push(("wslview", vec![url]));
        attempts.push((
            "powershell.exe",
            vec!["-NoProfile", "-Command", "Start-Process", url],
        ));
    }
    attempts.push(("xdg-open", vec![url]));
    attempts
}

fn best_effort_copy_to_clipboard(text: &str) -> bool {
    for (program, args) in clipboard_copy_attempts() {
        let mut child = match Command::new(program)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(child) => child,
            Err(_) => continue,
        };

        let write_result = child
            .stdin
            .as_mut()
            .map(|stdin| stdin.write_all(text.as_bytes()))
            .transpose();
        let wait_result = child.wait();
        if write_result.is_ok() && matches!(wait_result, Ok(status) if status.success()) {
            return true;
        }
    }
    false
}

fn clipboard_copy_attempts() -> Vec<(&'static str, Vec<&'static str>)> {
    if cfg!(target_os = "macos") {
        return vec![("pbcopy", vec![])];
    }
    if cfg!(target_os = "windows") {
        return vec![("clip.exe", vec![])];
    }

    let mut attempts = Vec::new();
    if env::var_os("WAYLAND_DISPLAY").is_some() {
        attempts.push(("wl-copy", vec![]));
    }
    if env::var_os("DISPLAY").is_some() {
        attempts.push(("xclip", vec!["-selection", "clipboard"]));
        attempts.push(("xsel", vec!["--clipboard", "--input"]));
    }
    if env::var_os("WSL_DISTRO_NAME").is_some() {
        attempts.push(("clip.exe", vec![]));
    }
    attempts
}

async fn github_repo_id(repo: &str, bearer_token: Option<&str>) -> Result<Option<u64>> {
    let client = reqwest::Client::new();
    let mut request = client
        .get(format!("{GITHUB_API_BASE}/repos/{repo}"))
        .header(ACCEPT, "application/vnd.github+json")
        .header("X-GitHub-Api-Version", GITHUB_API_VERSION)
        .header(USER_AGENT, DEFAULT_GITHUB_USER_AGENT);
    if let Some(token) = bearer_token {
        request = request.header(AUTHORIZATION, format!("Bearer {token}"));
    }
    let response = request
        .send()
        .await
        .context("failed to resolve GitHub repository metadata")?;

    if response.status().as_u16() == 404 || response.status().as_u16() == 403 {
        return Ok(None);
    }
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        bail!("GitHub repository lookup failed ({status}): {body}");
    }

    let payload: GitHubRepoResponse = response
        .json()
        .await
        .context("failed to decode GitHub repository metadata")?;
    Ok(Some(payload.id))
}

async fn try_resolve_repo_id_for_device_flow(config: &Config, repo: &str) -> Result<Option<u64>> {
    if let Some(repo_id) = github_repo_id(repo, None).await? {
        return Ok(Some(repo_id));
    }

    if let (Some(app_id), Some(installation_id)) =
        (config.reader_app_id, config.reader_installation_id)
        && Path::new(&config.reader_private_key_path).exists()
    {
        let token = mint_reader_installation_token(
            app_id,
            Path::new(&config.reader_private_key_path),
            installation_id,
        )
        .await?;
        if let Some(repo_id) = github_repo_id(repo, Some(&token)).await? {
            return Ok(Some(repo_id));
        }
    }

    Ok(None)
}

async fn request_device_flow_code(client_id: &str) -> Result<GitHubDeviceCodeResponse> {
    let client = reqwest::Client::new();
    let response = client
        .post(GITHUB_OAUTH_DEVICE_CODE_URL)
        .header(ACCEPT, "application/json")
        .header(USER_AGENT, DEFAULT_GITHUB_USER_AGENT)
        .form(&[("client_id", client_id)])
        .send()
        .await
        .context("failed to request GitHub device code")?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        bail!("GitHub device code request failed ({status}): {body}");
    }

    response
        .json()
        .await
        .context("failed to decode GitHub device code response")
}

async fn poll_device_flow_access_token(
    client_id: &str,
    device_code: &str,
    repository_id: Option<u64>,
) -> Result<GitHubOAuthTokenResponse> {
    let client = reqwest::Client::new();
    let mut params = vec![
        ("client_id", client_id.to_string()),
        ("device_code", device_code.to_string()),
        (
            "grant_type",
            "urn:ietf:params:oauth:grant-type:device_code".to_string(),
        ),
    ];
    if let Some(repository_id) = repository_id {
        params.push(("repository_id", repository_id.to_string()));
    }

    let response = client
        .post(GITHUB_OAUTH_ACCESS_TOKEN_URL)
        .header(ACCEPT, "application/json")
        .header(USER_AGENT, DEFAULT_GITHUB_USER_AGENT)
        .form(&params)
        .send()
        .await
        .context("failed to poll GitHub device flow token")?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        bail!("GitHub device flow token request failed ({status}): {body}");
    }

    response
        .json()
        .await
        .context("failed to decode GitHub device flow token response")
}

async fn refresh_user_access_token(
    client_id: &str,
    refresh_token: &str,
) -> Result<GitHubOAuthTokenResponse> {
    let client = reqwest::Client::new();
    let response = client
        .post(GITHUB_OAUTH_ACCESS_TOKEN_URL)
        .header(ACCEPT, "application/json")
        .header(USER_AGENT, DEFAULT_GITHUB_USER_AGENT)
        .form(&[
            ("client_id", client_id),
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
        ])
        .send()
        .await
        .context("failed to refresh GitHub user access token")?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        bail!("GitHub user access token refresh failed ({status}): {body}");
    }

    response
        .json()
        .await
        .context("failed to decode GitHub user access token refresh response")
}

fn oauth_token_response_error(response: &GitHubOAuthTokenResponse) -> Option<&str> {
    response.error.as_deref()
}

fn grant_expiration_from_expires_in(
    expires_in: Option<u64>,
) -> Result<(Option<String>, Option<u64>)> {
    match expires_in {
        Some(expires_in) => {
            let (formatted, epoch_seconds) = expires_at_from_now(expires_in)?;
            Ok((Some(formatted), Some(epoch_seconds)))
        }
        None => Ok((None, None)),
    }
}

async fn mint_user_access_token_via_device_flow(
    client_id: &str,
    repo: &str,
    repository_id: Option<u64>,
) -> Result<GitHubUserAccessGrant> {
    let code = request_device_flow_code(client_id).await?;
    let opened_browser = best_effort_open_browser(&code.verification_uri);
    let copied_code = best_effort_copy_to_clipboard(&code.user_code);
    println!("github-device-flow: pending");
    println!("repo: {repo}");
    if let Some(repository_id) = repository_id {
        println!("repository-id: {repository_id}");
    } else {
        println!("repository-id: unresolved");
        println!(
            "note: GitHub-side token narrowing could not be confirmed; Sprite delivery remains repo-scoped"
        );
    }
    println!("verification-uri: {}", code.verification_uri);
    println!("user-code: {}", code.user_code);
    println!("expires-in-seconds: {}", code.expires_in);
    println!(
        "verification-uri-opened: {}",
        if opened_browser { "yes" } else { "no" }
    );
    println!(
        "user-code-copied: {}",
        if copied_code { "yes" } else { "no" }
    );
    if !opened_browser {
        println!("note: open the verification URI manually if a browser did not launch");
    }
    if !copied_code {
        println!("note: copy the user code manually if clipboard integration is unavailable");
    }

    let mut interval_seconds = code.interval.unwrap_or(5).max(1);
    loop {
        tokio::time::sleep(Duration::from_secs(interval_seconds)).await;
        let response =
            poll_device_flow_access_token(client_id, &code.device_code, repository_id).await?;
        match oauth_token_response_error(&response) {
            None => {
                let access_token = response.access_token.ok_or_else(|| {
                    anyhow!("GitHub device flow completed without an access token")
                })?;
                return Ok(GitHubUserAccessGrant {
                    access_token,
                    expires_in_seconds: response.expires_in,
                    refresh_token: response.refresh_token,
                });
            }
            Some("authorization_pending") => continue,
            Some("slow_down") => {
                interval_seconds = response
                    .interval
                    .unwrap_or(interval_seconds + 5)
                    .max(interval_seconds + 1);
            }
            Some("expired_token") | Some("token_expired") => {
                bail!("GitHub device flow code expired before authorization completed");
            }
            Some("access_denied") => bail!("GitHub device flow authorization was cancelled"),
            Some("device_flow_disabled") => {
                bail!("GitHub App device flow is disabled; enable device flow in the app settings")
            }
            Some(other) => {
                let details = response
                    .error_description
                    .as_deref()
                    .unwrap_or("no description");
                bail!("GitHub device flow failed with {other}: {details}");
            }
        }
    }
}

async fn mint_device_flow_push_grant(
    config: &Config,
    repo: &str,
    client_id: &str,
    persist_refresh_token: bool,
    active_ttl: Option<Duration>,
) -> Result<PushGrantRecord> {
    if let Some(cached) = load_cached_device_flow_grant(repo, client_id)? {
        let refreshed = refresh_user_access_token(client_id, &cached.refresh_token).await;
        match refreshed {
            Ok(response) if response.error.is_none() => {
                let access_token = response
                    .access_token
                    .ok_or_else(|| anyhow!("GitHub refresh completed without an access token"))?;
                if persist_refresh_token && let Some(refresh_token) = response.refresh_token.clone()
                {
                    save_cached_device_flow_grant(
                        repo,
                        &CachedDeviceFlowGrant {
                            client_id: client_id.to_string(),
                            repo: repo.to_string(),
                            refresh_token,
                        },
                    )?;
                }
                let (expires_at, expires_at_epoch_seconds) = match active_ttl {
                    Some(active_ttl) => {
                        let (formatted, epoch_seconds) = expires_at_from_now(active_ttl.as_secs())?;
                        (Some(formatted), Some(epoch_seconds))
                    }
                    None => grant_expiration_from_expires_in(response.expires_in)?,
                };
                return Ok(PushGrantRecord {
                    repo: repo.to_string(),
                    token: access_token,
                    expires_at,
                    expires_at_epoch_seconds,
                    token_source: Some("github-app-user-token".to_string()),
                });
            }
            Ok(response)
                if matches!(
                    oauth_token_response_error(&response),
                    Some("bad_refresh_token")
                ) =>
            {
                remove_cached_device_flow_grant(repo)?;
            }
            Ok(response) => {
                let error = response
                    .error
                    .unwrap_or_else(|| "unknown_error".to_string());
                let details = response
                    .error_description
                    .unwrap_or_else(|| "no description".to_string());
                bail!("GitHub user access token refresh failed with {error}: {details}");
            }
            Err(err) => {
                let message = err.to_string();
                if message.contains("incorrect_client_credentials")
                    || message.contains("bad_refresh_token")
                {
                    remove_cached_device_flow_grant(repo)?;
                } else {
                    return Err(err);
                }
            }
        }
    }

    let repository_id = try_resolve_repo_id_for_device_flow(config, repo).await?;
    let grant = mint_user_access_token_via_device_flow(client_id, repo, repository_id).await?;
    if persist_refresh_token && let Some(refresh_token) = grant.refresh_token.clone() {
        save_cached_device_flow_grant(
            repo,
            &CachedDeviceFlowGrant {
                client_id: client_id.to_string(),
                repo: repo.to_string(),
                refresh_token,
            },
        )?;
    }
    let (expires_at, expires_at_epoch_seconds) = match active_ttl {
        Some(active_ttl) => {
            let (formatted, epoch_seconds) = expires_at_from_now(active_ttl.as_secs())?;
            (Some(formatted), Some(epoch_seconds))
        }
        None => grant_expiration_from_expires_in(grant.expires_in_seconds)?,
    };

    Ok(PushGrantRecord {
        repo: repo.to_string(),
        token: grant.access_token,
        expires_at,
        expires_at_epoch_seconds,
        token_source: Some("github-app-user-token".to_string()),
    })
}

async fn request_push_access(
    config: &Config,
    repo: &str,
    publisher_client_id: Option<&str>,
    active_ttl: Option<Duration>,
    cache_refresh_token: bool,
) -> Result<()> {
    let repo =
        normalize_github_repo(repo).ok_or_else(|| anyhow!("repo must be in owner/repo form"))?;
    let client_id = resolve_publisher_client_id(config, publisher_client_id).ok_or_else(|| {
        anyhow!(
            "publisher client id is required for device-flow push grants; set `publisher_client_id`, pass `--publisher-client-id`, or export {GITHUB_PUSH_GRANT_CLIENT_ID_ENV}"
        )
    })?;
    let grant =
        mint_device_flow_push_grant(config, &repo, &client_id, cache_refresh_token, active_ttl)
            .await?;
    write_local_push_grant(&repo, &grant)?;

    println!("push-grant: active");
    println!("repo: {repo}");
    println!("grant-location: local");
    println!(
        "ttl: {}",
        if active_ttl.is_some() {
            "enabled"
        } else {
            "disabled"
        }
    );
    println!(
        "refresh-token-cache: {}",
        if cache_refresh_token {
            "enabled"
        } else {
            "disabled"
        }
    );
    println!(
        "token-source: {}",
        grant
            .token_source
            .clone()
            .unwrap_or_else(|| "unknown".to_string())
    );
    if let Some(expires_at) = grant.expires_at.as_deref() {
        println!("expires-at: {expires_at}");
    }
    Ok(())
}

fn revoke_push_access(repo: &str, forget_local_auth: bool) -> Result<()> {
    let repo =
        normalize_github_repo(repo).ok_or_else(|| anyhow!("repo must be in owner/repo form"))?;
    let path = push_grant_path(&repo);
    let removed = if path.exists() {
        fs::remove_file(&path).with_context(|| format!("failed to remove {}", path.display()))?;
        true
    } else {
        false
    };
    println!("grant-location: local");
    println!(
        "push-grant-file: {}",
        if removed { "removed" } else { "not-found" }
    );
    println!("push-grant: revoked");
    println!("repo: {repo}");
    if forget_local_auth {
        let removed_local_state = remove_cached_device_flow_grant(&repo)?;
        println!(
            "local-device-flow-state: {}",
            if removed_local_state {
                "removed"
            } else {
                "not-found"
            }
        );
    } else {
        println!("local-device-flow-state: retained");
        println!("note: pass --forget-local-auth to remove the cached local refresh token too");
    }
    Ok(())
}

fn list_push_grants() -> Result<()> {
    println!("grant-location: local");
    let grants_dir = Path::new(PUSH_GRANTS_DIR);
    let raw = if !grants_dir.is_dir() {
        String::new()
    } else {
        let mut blobs = Vec::new();
        for entry in fs::read_dir(grants_dir)
            .with_context(|| format!("failed to read {}", grants_dir.display()))?
        {
            let entry =
                entry.with_context(|| format!("failed to read {}", grants_dir.display()))?;
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            blobs.push(
                fs::read_to_string(&path)
                    .with_context(|| format!("failed to read {}", path.display()))?,
            );
        }
        blobs.join("\n")
    };

    let mut grants = Vec::new();
    for grant in parse_push_grants(&raw)? {
        if push_grant_expired(&grant, current_epoch_seconds()?) {
            continue;
        }
        grants.push(grant);
    }

    if grants.is_empty() {
        println!("push-grants: none");
        return Ok(());
    }

    for grant in grants {
        println!("repo: {}", grant.repo);
        if let Some(source) = grant.token_source.as_deref() {
            println!("token-source: {source}");
        }
        println!(
            "expires-at: {}",
            grant.expires_at.unwrap_or_else(|| "unknown".to_string())
        );
        println!();
    }
    Ok(())
}

/// Resolve the active push grant for a repo, erroring with agent-friendly guidance
/// when no usable grant exists. Reuses the exact same grant store and expiry
/// semantics as the git credential helper, so a revoked or expired grant yields
/// no usable auth here either.
#[cfg(test)]
fn resolve_active_push_grant(repo: &str, grants_dir: &Path) -> Result<PushGrantRecord> {
    load_push_grant_from_dir(repo, grants_dir)?.ok_or_else(|| {
        anyhow!(
            "no active push grant for {repo}; run `github request-push --repo {repo}` first (a grant may have expired or been revoked)"
        )
    })
}

async fn publish_pr(
    config: &Config,
    repo: &str,
    title: &str,
    base: Option<&str>,
    body: &str,
    draft: bool,
) -> Result<()> {
    let repo =
        normalize_github_repo(repo).ok_or_else(|| anyhow!("repo must be in owner/repo form"))?;
    if title.trim().is_empty() {
        bail!("PR title cannot be empty");
    }
    if base.is_some_and(|value| value.trim().is_empty()) {
        bail!("PR base branch cannot be empty");
    }

    let current_dir = env::current_dir().context("failed to resolve current directory")?;
    let repo_root = detect_repo_root(&current_dir)?;
    let request = build_publish_request(
        config,
        repo.clone(),
        base.map(ToString::to_string),
        title.to_string(),
        body.to_string(),
        draft,
        &repo_root,
    )?;
    let response =
        submit_publish_request(Path::new(&config.publisher_socket_path), &request).await?;

    println!("publish-pr: created");
    println!("repo: {repo}");
    println!("url: {}", response.pr_url);
    println!("number: {}", response.pull_number);
    println!("branch: {}", response.branch);
    println!("base: {}", base.unwrap_or("<default>"));
    println!("draft: {draft}");
    println!("auth-source: publisher-app");
    Ok(())
}

fn resolve_publisher_client_id(
    config: &Config,
    publisher_client_id: Option<&str>,
) -> Option<String> {
    publisher_client_id
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .or_else(|| {
            env::var(GITHUB_PUSH_GRANT_CLIENT_ID_ENV)
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        })
        .or_else(|| config.publisher_client_id.clone())
}

fn ensure_reader_ready_for_start(config: &Config) -> Result<()> {
    let Some(app_id) = config.reader_app_id else {
        bail!("reader_app_id must be configured before start");
    };
    if app_id == 0 {
        bail!("reader_app_id must be non-zero");
    }

    let Some(installation_id) = config.reader_installation_id else {
        bail!("reader_installation_id must be configured before start");
    };
    if installation_id == 0 {
        bail!("reader_installation_id must be non-zero");
    }

    if config.reader_private_key_path.trim().is_empty() {
        bail!("reader_private_key_path must be configured");
    }
    if !Path::new(&config.reader_private_key_path).exists() {
        bail!(
            "reader private key file not found: {}",
            config.reader_private_key_path
        );
    }

    Ok(())
}

fn ensure_tls_artifacts(config_path: &Path) -> Result<()> {
    let mut config = Config::load(Some(config_path))?;
    if tls_artifacts_exist(&config) {
        println!("tls-artifacts: ready");
        println!("tls-mode: {}", config.tls_mode);
        return Ok(());
    }

    let san_ip = select_tls_san_ip(&config.bind_host);
    generate_self_signed_certificate(&config, san_ip)?;
    config.tls_mode = "self_signed".to_string();
    config.save(config_path)?;

    println!("tls-artifacts: created");
    println!("tls-mode: {}", config.tls_mode);
    println!("tls-cert: {}", config.tls_cert_path);
    println!("tls-key: {}", config.tls_key_path);
    Ok(())
}

fn tls_artifacts_exist(config: &Config) -> bool {
    Path::new(&config.tls_cert_path).exists() && Path::new(&config.tls_key_path).exists()
}

fn select_tls_san_ip(bind_host: &str) -> std::net::IpAddr {
    if let Ok(ip) = bind_host.parse::<std::net::IpAddr>()
        && !ip.is_unspecified()
    {
        return ip;
    }
    std::net::IpAddr::from([127, 0, 0, 1])
}

fn generate_self_signed_certificate(config: &Config, ip: std::net::IpAddr) -> Result<()> {
    let mut params = CertificateParams::new(Vec::<String>::new())
        .context("failed to initialize self-signed cert parameters")?;
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, format!("zodex {ip}"));
    params.distinguished_name = dn;
    params.subject_alt_names = vec![SanType::IpAddress(ip)];

    let key_pair = KeyPair::generate().context("failed to generate TLS key pair")?;
    let certificate = params
        .self_signed(&key_pair)
        .context("failed to generate self-signed certificate")?;
    let cert_pem = certificate.pem();
    let key_pem = key_pair.serialize_pem();

    let cert_path = Path::new(&config.tls_cert_path);
    let key_path = Path::new(&config.tls_key_path);
    if let Some(parent) = cert_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    if let Some(parent) = key_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    fs::write(cert_path, cert_pem)
        .with_context(|| format!("failed to write {}", cert_path.display()))?;
    fs::write(key_path, key_pem)
        .with_context(|| format!("failed to write {}", key_path.display()))?;

    #[cfg(unix)]
    {
        fs::set_permissions(cert_path, fs::Permissions::from_mode(0o644))
            .with_context(|| format!("failed to chmod {}", cert_path.display()))?;
        fs::set_permissions(key_path, fs::Permissions::from_mode(0o600))
            .with_context(|| format!("failed to chmod {}", key_path.display()))?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use base64::Engine as _;

    use super::{
        Args, PushGrantRecord, create_direct_push_bundle_base64_from_dir,
        git_remote_zodex_push_dst, git_remote_zodex_repo, parse_push_grants,
        resolve_active_push_grant, resolve_git_object_id, resolve_git_object_type,
        sanitize_remote_helper_error,
    };
    use clap::CommandFactory;
    use std::fs;
    use std::process::Command;
    use tempfile::tempdir;

    fn git_test_status(command: &mut Command) -> bool {
        command
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .expect("run git")
            .success()
    }

    #[test]
    fn clap_help_uses_zodexd_name() {
        let help = Args::command().render_long_help().to_string();
        assert!(help.contains("zodexd"));
        assert!(help.contains("remote execution"));
        assert!(!help.contains("git-credential-helper"));
        assert!(!help.contains("ensure-tls"));
    }

    #[test]
    fn push_grant_resolver_requires_an_active_push_grant() {
        let grants_dir = tempdir().expect("grants dir");
        let err = resolve_active_push_grant("owner/repo", grants_dir.path())
            .expect_err("missing grant should error");
        let message = err.to_string();
        assert!(message.contains("no active push grant"));
        assert!(message.contains("request-push"));
    }

    #[test]
    fn push_grant_resolver_reuses_the_active_push_grant_token() {
        let grants_dir = tempdir().expect("grants dir");
        let grant = PushGrantRecord {
            repo: "owner/repo".to_string(),
            token: "ghu_example_token".to_string(),
            expires_at: None,
            expires_at_epoch_seconds: None,
            token_source: Some("github-app-user-token".to_string()),
        };
        fs::write(
            grants_dir.path().join("owner__repo.json"),
            serde_json::to_vec(&grant).expect("encode grant"),
        )
        .expect("write grant");

        let resolved = resolve_active_push_grant("owner/repo", grants_dir.path())
            .expect("grant should resolve");
        assert_eq!(resolved.token, "ghu_example_token");
    }

    #[test]
    fn zodex_remote_helper_extracts_github_repo_without_credentials() {
        assert_eq!(
            git_remote_zodex_repo("https://github.com/owner/repo.git"),
            Some("owner/repo".to_string())
        );
        assert_eq!(
            git_remote_zodex_repo("https://token@github.com/owner/repo.git"),
            Some("owner/repo".to_string())
        );
        assert_eq!(
            git_remote_zodex_repo("zodex::https://github.com/owner/repo.git"),
            Some("owner/repo".to_string())
        );
    }

    #[test]
    fn zodex_remote_helper_parses_push_destination_and_sanitizes_errors() {
        assert_eq!(
            git_remote_zodex_push_dst("+refs/heads/main:refs/heads/main"),
            Some("refs/heads/main".to_string())
        );
        assert_eq!(
            sanitize_remote_helper_error(&anyhow::anyhow!("first line\nsecond line")),
            "first line second line"
        );
    }

    #[test]
    fn direct_push_bundle_imports_into_clone_with_remote_prerequisites() {
        let tempdir = tempdir().expect("tempdir");
        let origin = tempdir.path().join("origin.git");
        let repo = tempdir.path().join("repo");
        let publisher_clone = tempdir.path().join("publisher-clone");
        let bundle_path = tempdir.path().join("direct-push.bundle");

        assert!(git_test_status(Command::new("git").args([
            "init",
            "-q",
            "--bare",
            origin.to_str().unwrap()
        ])));
        assert!(git_test_status(Command::new("git").args([
            "clone",
            "-q",
            origin.to_str().unwrap(),
            repo.to_str().unwrap()
        ])));
        assert!(git_test_status(
            Command::new("git")
                .current_dir(&repo)
                .args(["config", "user.name", "Test"])
        ));
        assert!(git_test_status(
            Command::new("git").current_dir(&repo).args([
                "config",
                "user.email",
                "test@example.com"
            ])
        ));
        fs::write(repo.join("a.txt"), "base\n").expect("write base");
        assert!(git_test_status(
            Command::new("git")
                .current_dir(&repo)
                .args(["add", "a.txt"])
        ));
        assert!(git_test_status(
            Command::new("git")
                .current_dir(&repo)
                .args(["commit", "-q", "-m", "base"])
        ));
        assert!(git_test_status(
            Command::new("git")
                .current_dir(&repo)
                .args(["push", "-q", "origin", "HEAD:main"])
        ));
        assert!(git_test_status(
            Command::new("git")
                .current_dir(&repo)
                .args(["checkout", "-q", "-b", "smoke"])
        ));
        assert!(git_test_status(
            Command::new("git").current_dir(&repo).args([
                "commit",
                "--allow-empty",
                "-q",
                "-m",
                "smoke"
            ])
        ));

        let bundle_base64 = create_direct_push_bundle_base64_from_dir(&repo, "refs/heads/smoke")
            .expect("create bundle")
            .expect("branch push should have bundle contents");
        fs::write(
            &bundle_path,
            base64::engine::general_purpose::STANDARD
                .decode(bundle_base64)
                .expect("decode bundle"),
        )
        .expect("write bundle");

        assert!(git_test_status(Command::new("git").args([
            "clone",
            "-q",
            origin.to_str().unwrap(),
            publisher_clone.to_str().unwrap()
        ])));
        assert!(git_test_status(
            Command::new("git").current_dir(&publisher_clone).args([
                "fetch",
                bundle_path.to_str().unwrap(),
                "refs/heads/smoke:refs/zodex/direct-push",
            ])
        ));
    }

    #[test]
    fn direct_push_bundle_imports_annotated_tag_without_heads_namespace() {
        let tempdir = tempdir().expect("tempdir");
        let origin = tempdir.path().join("origin.git");
        let repo = tempdir.path().join("repo");
        let publisher_clone = tempdir.path().join("publisher-clone");
        let bundle_path = tempdir.path().join("direct-push.bundle");

        assert!(git_test_status(Command::new("git").args([
            "init",
            "-q",
            "--bare",
            origin.to_str().unwrap()
        ])));
        assert!(git_test_status(Command::new("git").args([
            "clone",
            "-q",
            origin.to_str().unwrap(),
            repo.to_str().unwrap()
        ])));
        assert!(git_test_status(
            Command::new("git")
                .current_dir(&repo)
                .args(["config", "user.name", "Test"])
        ));
        assert!(git_test_status(
            Command::new("git").current_dir(&repo).args([
                "config",
                "user.email",
                "test@example.com"
            ])
        ));
        fs::write(repo.join("a.txt"), "base\n").expect("write base");
        assert!(git_test_status(
            Command::new("git")
                .current_dir(&repo)
                .args(["add", "a.txt"])
        ));
        assert!(git_test_status(
            Command::new("git")
                .current_dir(&repo)
                .args(["commit", "-q", "-m", "base"])
        ));
        assert!(git_test_status(
            Command::new("git")
                .current_dir(&repo)
                .args(["push", "-q", "origin", "HEAD:main"])
        ));
        assert!(git_test_status(
            Command::new("git").current_dir(&repo).args([
                "fetch",
                "-q",
                "origin",
                "main:refs/remotes/origin/main"
            ])
        ));
        assert!(git_test_status(
            Command::new("git")
                .current_dir(&repo)
                .args(["tag", "-a", "v1.0.0", "-m", "v1.0.0"])
        ));

        let bundle_base64 = create_direct_push_bundle_base64_from_dir(&repo, "refs/tags/v1.0.0")
            .expect("create tag bundle")
            .expect(
                "annotated tag object should produce a bundle even when target commit is remote",
            );
        fs::write(
            &bundle_path,
            base64::engine::general_purpose::STANDARD
                .decode(bundle_base64)
                .expect("decode bundle"),
        )
        .expect("write bundle");

        assert!(git_test_status(Command::new("git").args([
            "clone",
            "-q",
            origin.to_str().unwrap(),
            publisher_clone.to_str().unwrap()
        ])));
        assert!(git_test_status(
            Command::new("git").current_dir(&publisher_clone).args([
                "fetch",
                bundle_path.to_str().unwrap(),
                "refs/tags/v1.0.0:refs/zodex/direct-push",
            ])
        ));
        let object_type = resolve_git_object_type(&publisher_clone, "refs/zodex/direct-push")
            .expect("imported ref object type");
        assert_eq!(object_type, "tag");
    }

    #[test]
    fn direct_push_bundle_uses_oid_fallback_for_lightweight_tag_on_remote_commit() {
        let tempdir = tempdir().expect("tempdir");
        let origin = tempdir.path().join("origin.git");
        let repo = tempdir.path().join("repo");

        assert!(git_test_status(Command::new("git").args([
            "init",
            "-q",
            "--bare",
            origin.to_str().unwrap()
        ])));
        assert!(git_test_status(Command::new("git").args([
            "clone",
            "-q",
            origin.to_str().unwrap(),
            repo.to_str().unwrap()
        ])));
        assert!(git_test_status(
            Command::new("git")
                .current_dir(&repo)
                .args(["config", "user.name", "Test"])
        ));
        assert!(git_test_status(
            Command::new("git").current_dir(&repo).args([
                "config",
                "user.email",
                "test@example.com"
            ])
        ));
        fs::write(repo.join("a.txt"), "base\n").expect("write base");
        assert!(git_test_status(
            Command::new("git")
                .current_dir(&repo)
                .args(["add", "a.txt"])
        ));
        assert!(git_test_status(
            Command::new("git")
                .current_dir(&repo)
                .args(["commit", "-q", "-m", "base"])
        ));
        assert!(git_test_status(
            Command::new("git")
                .current_dir(&repo)
                .args(["push", "-q", "origin", "HEAD:main"])
        ));
        assert!(git_test_status(
            Command::new("git").current_dir(&repo).args([
                "fetch",
                "-q",
                "origin",
                "main:refs/remotes/origin/main"
            ])
        ));
        assert!(git_test_status(
            Command::new("git")
                .current_dir(&repo)
                .args(["tag", "v1.0.0"])
        ));

        let bundle_base64 = create_direct_push_bundle_base64_from_dir(&repo, "refs/tags/v1.0.0")
            .expect("lightweight tag bundle attempt should not hard fail");
        assert!(
            bundle_base64.is_none(),
            "lightweight tag on an already-remote commit has no new bundle objects"
        );
        let tag_oid = resolve_git_object_id(&repo, "refs/tags/v1.0.0").expect("tag oid");
        let head_oid = resolve_git_object_id(&repo, "HEAD").expect("head oid");
        assert_eq!(tag_oid, head_oid);
        assert_eq!(
            resolve_git_object_type(&repo, "refs/tags/v1.0.0").expect("tag object type"),
            "commit"
        );
    }

    #[test]
    fn push_grant_resolver_rejects_expired_push_grant() {
        let grants_dir = tempdir().expect("grants dir");
        let grant = PushGrantRecord {
            repo: "owner/repo".to_string(),
            token: "ghu_expired".to_string(),
            expires_at: Some("1970-01-01T00:00:01Z".to_string()),
            expires_at_epoch_seconds: Some(1),
            token_source: Some("github-app-user-token".to_string()),
        };
        let path = grants_dir.path().join("owner__repo.json");
        fs::write(&path, serde_json::to_vec(&grant).expect("encode grant")).expect("write grant");

        let err = resolve_active_push_grant("owner/repo", grants_dir.path())
            .expect_err("expired grant should error");
        assert!(err.to_string().contains("no active push grant"));
        assert!(!path.exists(), "expired grant file should be pruned");
    }

    #[test]
    fn parse_push_grants_accepts_pretty_printed_grant_stream() {
        let first = PushGrantRecord {
            repo: "owner/repo".to_string(),
            token: "ghu_first".to_string(),
            expires_at: Some("2026-06-30T00:00:00Z".to_string()),
            expires_at_epoch_seconds: Some(1_782_777_600),
            token_source: Some("github-app-user-token".to_string()),
        };
        let second = PushGrantRecord {
            repo: "owner/other".to_string(),
            token: "ghu_second".to_string(),
            expires_at: None,
            expires_at_epoch_seconds: None,
            token_source: Some("github-app-user-token".to_string()),
        };
        let raw = format!(
            "{}\n{}\n",
            serde_json::to_string_pretty(&first).expect("encode first grant"),
            serde_json::to_string_pretty(&second).expect("encode second grant")
        );

        let grants = parse_push_grants(&raw).expect("pretty grant stream should parse");

        assert_eq!(grants, vec![first, second]);
    }
}
