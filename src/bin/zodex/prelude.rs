use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::fs::OpenOptions;
use std::io::{self, Read, Write};
use std::net::IpAddr;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use clap::{Parser, Subcommand};
#[cfg(unix)]
use nix::errno::Errno;
#[cfg(unix)]
use nix::sys::signal::{Signal, kill};
#[cfg(unix)]
use nix::unistd::{Group, Pid, Uid, User, chown, setsid};
use rand::distr::{Alphanumeric, SampleString};
use rcgen::{CertificateParams, DistinguishedName, DnType, KeyPair, SanType};
use reqwest::Url;
use reqwest::header::{ACCEPT, AUTHORIZATION, USER_AGENT};
use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;
use time::format_description::well_known::Rfc3339;
use time::{OffsetDateTime, UtcOffset};
use zodex::config::{Config, DEFAULT_CONFIG_PATH};
use zodex::install_rustls_crypto_provider;
use zodex::publisher::{
    mint_publisher_installation_token_with_metadata, mint_reader_installation_token,
    resolve_repo_installation_id,
};
use zodex::redaction::redact_api_key_query_params;

const SERVICE_NAME: &str = "zodexd.service";
const SYSTEMD_UNIT_PATH: &str = "/etc/systemd/system/zodexd.service";
const SPRITE_MAIN_SERVICE_LABEL: &str = "zodexd";
const STATE_DIR: &str = "/var/lib/zodex";
const TLS_DIR: &str = "/var/lib/zodex/tls";
const LETSENCRYPT_LIVE_DIR: &str = "/etc/letsencrypt/live";
const DEFAULT_LOG_LINES: &str = "200";
const STATUS_HOST_HINT_FALLBACK: &str = "<host>";
const TLS_MODE_LETSENCRYPT_IP: &str = "letsencrypt_ip";
const TLS_MODE_SELF_SIGNED: &str = "self_signed";
const PROCESS_RUNTIME_DIRNAME: &str = "run";
const PROCESS_LOG_DIRNAME: &str = "logs";
const PROCESS_PID_FILENAME: &str = "zodexd.pid";
const PROCESS_LOG_FILENAME: &str = "zodexd.log";
const PUBLISHER_PROCESS_SUBDIR: &str = "publisher";
const PUBLISHER_SERVICE_LABEL: &str = "zodex-prd";
const PUBLISHER_PROCESS_PID_FILENAME: &str = "zodex-prd.pid";
const PUBLISHER_PROCESS_LOG_FILENAME: &str = "zodex-prd.log";
const PROCESS_START_STABILIZE_MS: u64 = 300;
const PROCESS_STOP_TIMEOUT_MS: u64 = 5_000;
const PROCESS_STOP_POLL_MS: u64 = 100;
const SHARED_PROCESS_DIR_MODE: u32 = 0o750;
const SPRITE_SERVICE_RESTART_TIMEOUT_MS: u64 = 20_000;
const SPRITE_SERVICE_RESTART_POLL_MS: u64 = 200;
const PRIMARY_OPERATOR_BINARY: &str = "zodex";
const AGENT_OPERATOR_BINARY: &str = "zodex-agent";
const PRIMARY_DAEMON_BINARY: &str = "zodexd";
const PUSH_GRANTS_DIR: &str = "/var/lib/zodex/push-grants";
const PUSH_GRANT_REMOTE_TMP_PATH: &str = "/tmp/zodex-push-grant.json";
const GITHUB_PUSH_GRANT_DEVICE_CACHE_DIR: &str = ".config/zodex/github-device-flow";
const GITHUB_PUSH_GRANT_CLIENT_ID_ENV: &str = "ZODEX_PUBLISHER_CLIENT_ID";
const DEFAULT_PUSH_GRANT_TTL_SECONDS: u64 = 30 * 60;
const GITHUB_MODE_DIR: &str = "/var/lib/zodex/mode";
const GITHUB_MODE_STATE_PATH: &str = "/var/lib/zodex/mode/state.json";
const GITHUB_MODE_REMOTE_TMP_PATH: &str = "/tmp/zodex-github-mode.json";
const DEFAULT_YOLO_TTL_SECONDS: u64 = 2 * 60 * 60;
const ZODEX_AGENT_USER: &str = "zodex-agent";
const ZODEX_AGENT_HOME: &str = "/home/zodex-agent";
const ZODEX_AGENT_BINARY_PATH: &str = "/usr/local/bin/zodex-agent";
const GITHUB_PUSH_REWRITE_SOURCE: &str = "https://github.com/";
const GITHUB_PUSH_REWRITE_TARGET: &str = "zodex::https://github.com/";
const ZODEX_SPRITE_ENV: &str = "ZODEX_SPRITE";
const OPERATOR_SPRITES_REGISTRY_RELATIVE_PATH: &str = ".config/zodex/sprites.json";
const GITHUB_API_BASE: &str = "https://api.github.com";
const GITHUB_API_VERSION: &str = "2022-11-28";
const GITHUB_OAUTH_DEVICE_CODE_URL: &str = "https://github.com/login/device/code";
const GITHUB_OAUTH_ACCESS_TOKEN_URL: &str = "https://github.com/login/oauth/access_token";
const DEFAULT_GITHUB_USER_AGENT: &str = "zodex/0.1";
const SPRITE_SETUP_REMOTE_SCRIPT_PATH: &str = "/tmp/zodex-sprite-setup.sh";
const SPRITE_UPGRADE_REMOTE_SCRIPT_PATH: &str = "/tmp/zodex-sprite-upgrade.sh";
#[allow(dead_code)]
const SPRITE_REMOTE_INSTALLER_PATH: &str = "/tmp/zodex-install.sh";
#[allow(dead_code)]
const SPRITE_REMOTE_UPLOAD_AGENT_CLI_PATH: &str = "/tmp/zodex-agent";
#[allow(dead_code)]
const SPRITE_REMOTE_UPLOAD_GIT_REMOTE_HELPER_PATH: &str = "/tmp/git-remote-zodex";
#[allow(dead_code)]
const SPRITE_REMOTE_UPLOAD_DAEMON_PATH: &str = "/tmp/zodexd";
#[allow(dead_code)]
const SPRITE_REMOTE_UPLOAD_PUBLISHER_PATH: &str = "/tmp/zodex-prd";
const PROXY_COMPONENT_DIR: &str = "proxy/cloudflare-worker";
const PROXY_COMPONENT_README: &str = "proxy/cloudflare-worker/README.md";
const PROXY_WORKER_ENTRYPOINT: &str = "proxy/cloudflare-worker/src/index.js";
const PROXY_WRANGLER_TEMPLATE_PATH: &str = "proxy/cloudflare-worker/wrangler.jsonc";
const PROXY_SPRITE_ORIGIN_PLACEHOLDER: &str = "__SPRITE_ORIGIN__";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ServiceManager {
    Systemd,
    Process,
}

#[derive(Debug, Parser)]
#[command(name = "zodex")]
#[command(about = "Zodex operator CLI")]
#[command(version)]
struct Cli {
    #[arg(long, default_value = DEFAULT_CONFIG_PATH)]
    config: String,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    Install,
    Upgrade {
        #[arg(long, default_value = "latest")]
        version: String,
    },
    Start,
    Stop,
    Restart,
    Status,
    Logs,
    SetKey {
        value: String,
    },
    RotateKey,
    GitCredentialHelper {
        operation: String,
    },
    ShowUrl {
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
    },
    Tls {
        #[command(subcommand)]
        command: TlsCommand,
    },
    #[command(hide = true)]
    Publisher {
        #[command(subcommand)]
        command: PublisherCommand,
    },
    Sprite {
        #[command(subcommand)]
        command: SpriteCommand,
    },
    Proxy {
        #[command(subcommand)]
        command: ProxyCommand,
    },
    Github {
        #[command(subcommand)]
        command: GithubCommand,
    },
}

#[derive(Debug, Subcommand)]
enum TlsCommand {
    Setup,
}

#[derive(Debug, Subcommand)]
enum PublisherCommand {
    Start,
    Stop,
    Status,
    Logs,
}

#[derive(Debug, Subcommand)]
enum SpriteCommand {
    Setup {
        #[arg(long)]
        sprite: String,
        #[arg(long)]
        org: Option<String>,
        #[arg(long)]
        repo: String,
        #[arg(long)]
        reader_app_id: u64,
        #[arg(long)]
        reader_pem: PathBuf,
        #[arg(long)]
        publisher_app_id: u64,
        #[arg(long)]
        publisher_pem: PathBuf,
        #[arg(long, default_value = "main")]
        default_base: String,
        #[arg(long, default_value = "sprite")]
        url_auth: String,
        #[arg(long, default_value = DEFAULT_CONFIG_PATH)]
        remote_config: String,
    },
    Upgrade {
        #[arg(long)]
        sprite: Option<String>,
        #[arg(long)]
        org: Option<String>,
        #[arg(long, default_value = "latest")]
        version: String,
        #[arg(long)]
        repo: Option<String>,
        #[arg(long)]
        url_auth: Option<String>,
        #[arg(long, default_value = DEFAULT_CONFIG_PATH)]
        remote_config: String,
    },
    Sync {
        #[arg(long)]
        sprite: Option<String>,
        #[arg(long)]
        org: Option<String>,
        #[arg(long, default_value = DEFAULT_CONFIG_PATH)]
        remote_config: String,
        #[arg(long, default_value_t = false)]
        force_recreate: bool,
        #[arg(long, default_value_t = false)]
        skip_stop_detached: bool,
    },
    #[command(alias = "services-status")]
    Status {
        #[arg(long)]
        sprite: Option<String>,
        #[arg(long)]
        org: Option<String>,
        #[arg(long, default_value = DEFAULT_CONFIG_PATH)]
        remote_config: String,
    },
    #[command(alias = "service-logs")]
    Logs {
        #[arg(long)]
        sprite: Option<String>,
        #[arg(long)]
        service: String,
        #[arg(long)]
        org: Option<String>,
        #[arg(long)]
        lines: Option<usize>,
        #[arg(long)]
        duration: Option<String>,
    },
    Health {
        #[arg(long)]
        sprite: Option<String>,
        #[arg(long)]
        org: Option<String>,
        #[arg(long)]
        url_auth: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
enum ProxyCommand {
    Inspect {
        #[arg(long)]
        sprite: Option<String>,
        #[arg(long)]
        org: Option<String>,
        #[arg(long)]
        origin: Option<String>,
    },
    #[command(alias = "update")]
    Deploy {
        #[arg(long)]
        sprite: Option<String>,
        #[arg(long)]
        org: Option<String>,
        #[arg(long)]
        origin: Option<String>,
        #[arg(long, default_value_t = false)]
        skip_verify_origin: bool,
    },
    VerifyOrigin {
        #[arg(long)]
        sprite: Option<String>,
        #[arg(long)]
        org: Option<String>,
        #[arg(long)]
        origin: Option<String>,
    },
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
    GrantPush {
        #[arg(long)]
        sprite: Option<String>,
        #[arg(long)]
        repo: String,
        #[arg(long)]
        org: Option<String>,
        #[arg(long)]
        publisher_client_id: Option<String>,
    },
    RevokePush {
        #[arg(long)]
        sprite: Option<String>,
        #[arg(long)]
        repo: String,
        #[arg(long)]
        org: Option<String>,
        #[arg(long, default_value_t = false)]
        forget_local_auth: bool,
    },
    ListGrants {
        #[arg(long)]
        sprite: Option<String>,
        #[arg(long)]
        org: Option<String>,
    },
    Mode {
        #[command(subcommand)]
        command: GithubModeCommand,
    },
}

#[derive(Debug, Subcommand)]
enum GithubModeCommand {
    Yolo {
        #[arg(long)]
        sprite: Option<String>,
        #[arg(long)]
        org: Option<String>,
        #[arg(long = "repo")]
        repos: Vec<String>,
        #[arg(long, default_value = "2h")]
        ttl: String,
        #[arg(long, default_value_t = false)]
        no_ttl: bool,
    },
    Default {
        #[arg(long)]
        sprite: Option<String>,
        #[arg(long)]
        org: Option<String>,
    },
    Status {
        #[arg(long)]
        sprite: Option<String>,
        #[arg(long)]
        org: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct SpriteServiceDefinition {
    cmd: String,
    args: Vec<String>,
    needs: Vec<String>,
    http_port: Option<u16>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
struct SpriteServiceStatus {
    name: String,
    cmd: String,
    args: Vec<String>,
    needs: Vec<String>,
    http_port: Option<u16>,
    state: Option<SpriteServiceState>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
struct SpriteServiceState {
    name: Option<String>,
    pid: Option<u32>,
    started_at: Option<String>,
    status: Option<String>,
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct GithubYoloRepoGrant {
    repo: String,
    created_at: String,
    #[serde(default)]
    expires_at: Option<String>,
    #[serde(default)]
    expires_at_epoch_seconds: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct GithubModeRecord {
    mode: String,
    all_installed: bool,
    repos: Vec<String>,
    #[serde(default)]
    repo_grants: Vec<GithubYoloRepoGrant>,
    created_at: String,
    #[serde(default)]
    expires_at: Option<String>,
    #[serde(default)]
    expires_at_epoch_seconds: Option<u64>,
    enabled_by: String,
    token_source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
struct OperatorSpriteRegistry {
    #[serde(default)]
    sprites: Vec<OperatorSpriteRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct OperatorSpriteRecord {
    name: String,
    #[serde(default)]
    org: Option<String>,
    remote_config: String,
    last_setup_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ResolvedSprite {
    name: String,
    org: Option<String>,
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

#[derive(Debug, Clone)]
struct SpriteSetupOptions<'a> {
    sprite: &'a str,
    org: Option<&'a str>,
    repo: &'a str,
    reader_app_id: u64,
    reader_pem: &'a Path,
    publisher_app_id: u64,
    publisher_pem: &'a Path,
    default_base: &'a str,
    url_auth: &'a str,
    remote_config: &'a Path,
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
struct LocalOperatorBinaries {
    agent_cli: PathBuf,
    git_remote_helper: PathBuf,
    daemon: PathBuf,
    publisher: PathBuf,
}
