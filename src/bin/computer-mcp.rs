use std::collections::BTreeMap;
use std::fs;
use std::fs::OpenOptions;
use std::io::{self, Read};
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
use computer_mcp::config::{Config, DEFAULT_CONFIG_PATH};
use computer_mcp::install_rustls_crypto_provider;
use computer_mcp::publisher::{
    build_publish_request, detect_repo_root, mint_reader_installation_token, submit_publish_request,
};
use computer_mcp::redaction::redact_api_key_query_params;
#[cfg(unix)]
use nix::errno::Errno;
#[cfg(unix)]
use nix::sys::signal::{Signal, kill};
#[cfg(unix)]
use nix::unistd::{Group, Pid, Uid, User, chown, setsid};
use rand::distr::{Alphanumeric, SampleString};
use rcgen::{CertificateParams, DistinguishedName, DnType, KeyPair, SanType};
use serde::Deserialize;

const SERVICE_NAME: &str = "computer-mcpd.service";
const SYSTEMD_UNIT_PATH: &str = "/etc/systemd/system/computer-mcpd.service";
const SPRITE_MAIN_SERVICE_LABEL: &str = "computer-mcpd";
const STATE_DIR: &str = "/var/lib/computer-mcp";
const TLS_DIR: &str = "/var/lib/computer-mcp/tls";
const LETSENCRYPT_LIVE_DIR: &str = "/etc/letsencrypt/live";
const DEFAULT_LOG_LINES: &str = "200";
const STATUS_HOST_HINT_FALLBACK: &str = "<host>";
const TLS_MODE_LETSENCRYPT_IP: &str = "letsencrypt_ip";
const TLS_MODE_SELF_SIGNED: &str = "self_signed";
const PROCESS_RUNTIME_DIRNAME: &str = "run";
const PROCESS_LOG_DIRNAME: &str = "logs";
const PROCESS_PID_FILENAME: &str = "computer-mcpd.pid";
const PROCESS_LOG_FILENAME: &str = "computer-mcpd.log";
const PUBLISHER_PROCESS_SUBDIR: &str = "publisher";
const PUBLISHER_SERVICE_LABEL: &str = "computer-mcp-prd";
const PUBLISHER_PROCESS_PID_FILENAME: &str = "computer-mcp-prd.pid";
const PUBLISHER_PROCESS_LOG_FILENAME: &str = "computer-mcp-prd.log";
const PROCESS_START_STABILIZE_MS: u64 = 300;
const PROCESS_STOP_TIMEOUT_MS: u64 = 5_000;
const PROCESS_STOP_POLL_MS: u64 = 100;
const SHARED_PROCESS_DIR_MODE: u32 = 0o750;
const SPRITE_SERVICE_RESTART_TIMEOUT_MS: u64 = 20_000;
const SPRITE_SERVICE_RESTART_POLL_MS: u64 = 200;
const PRIMARY_OPERATOR_BINARY: &str = "zodex";
const PRIMARY_DAEMON_BINARY: &str = "zodexd";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ServiceManager {
    Systemd,
    Process,
}

#[derive(Debug, Parser)]
#[command(name = "zodex")]
#[command(about = "Zodex operator CLI (compatible with legacy computer-mcp commands)")]
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
    PublishPr {
        #[arg(long)]
        repo: String,
        #[arg(long)]
        title: String,
        #[arg(long)]
        body: Option<String>,
        #[arg(long)]
        body_file: Option<String>,
        #[arg(long)]
        base: Option<String>,
        #[arg(long, default_value_t = false)]
        draft: bool,
    },
    Publisher {
        #[command(subcommand)]
        command: PublisherCommand,
    },
    Sprite {
        #[command(subcommand)]
        command: SpriteCommand,
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
    ServicesStatus {
        #[arg(long)]
        sprite: String,
        #[arg(long)]
        org: Option<String>,
    },
    ServiceLogs {
        #[arg(long)]
        sprite: String,
        #[arg(long)]
        service: String,
        #[arg(long)]
        org: Option<String>,
        #[arg(long)]
        lines: Option<usize>,
        #[arg(long)]
        duration: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
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

#[tokio::main]
async fn main() -> Result<()> {
    install_rustls_crypto_provider();

    let cli = Cli::parse();
    let config_path = PathBuf::from(&cli.config);

    match cli.command {
        Commands::Install => {
            install(&config_path)?;
        }
        Commands::Upgrade { version } => {
            ensure_linux()?;
            upgrade(&config_path, &version)?;
        }
        Commands::Start => {
            ensure_linux()?;
            start_stack(&config_path)?;
        }
        Commands::Stop => {
            ensure_linux()?;
            let config = Config::load(Some(Path::new(&config_path)))?;
            stop_stack(&config)?;
        }
        Commands::Restart => {
            ensure_linux()?;
            restart_stack(&config_path)?;
        }
        Commands::Status => {
            ensure_linux()?;
            let config = Config::load(Some(Path::new(&config_path)))?;
            print_stack_status_summary(&config)?;
        }
        Commands::Logs => {
            ensure_linux()?;
            let config = Config::load(Some(Path::new(&config_path)))?;
            match detect_service_manager() {
                ServiceManager::Systemd => {
                    let logs = run_journalctl(&build_journalctl_args())?;
                    if logs.is_empty() {
                        println!("no recent logs found for {SERVICE_NAME}");
                    } else {
                        print!("{}", redact_api_key_query_params(&logs));
                    }
                }
                ServiceManager::Process => {
                    let logs =
                        read_process_logs(&config, DEFAULT_LOG_LINES.parse().unwrap_or(200))?;
                    if logs.is_empty() {
                        println!(
                            "no recent logs found for {}",
                            process_log_path(&config).display()
                        );
                    } else {
                        print!("{}", redact_api_key_query_params(&logs));
                    }
                }
            }
        }
        Commands::SetKey { value } => {
            let mut config = Config::load(Some(Path::new(&config_path)))?;
            config.api_key = value;
            config.save(&config_path)?;
            ensure_shared_group_permissions(&config, &config_path)?;
            println!("updated API key in {}", config_path.display());
        }
        Commands::RotateKey => {
            let mut config = Config::load(Some(Path::new(&config_path)))?;
            let mut rng = rand::rng();
            config.api_key = Alphanumeric.sample_string(&mut rng, 48);
            config.save(&config_path)?;
            ensure_shared_group_permissions(&config, &config_path)?;
            println!("rotated API key in {}", config_path.display());
        }
        Commands::GitCredentialHelper { operation } => {
            let config = Config::load(Some(Path::new(&config_path)))?;
            handle_git_credential_helper(&config, &operation).await?;
        }
        Commands::ShowUrl { host } => {
            let config = Config::load(Some(Path::new(&config_path)))?;
            let raw_url = format!("https://{host}/mcp?key={}", config.api_key);
            println!(
                "{} (key redacted in CLI output)",
                redact_api_key_query_params(&raw_url)
            );
        }
        Commands::Tls { command } => match command {
            TlsCommand::Setup => tls_setup(&config_path)?,
        },
        Commands::PublishPr {
            repo,
            title,
            body,
            body_file,
            base,
            draft,
        } => {
            ensure_linux()?;
            let config = Config::load(Some(Path::new(&config_path)))?;
            let body = resolve_pr_body(body, body_file.as_deref())?;
            let cwd = std::env::current_dir().context("failed to resolve current directory")?;
            let repo_root = detect_repo_root(&cwd)?;
            let request =
                build_publish_request(&config, repo, base, title, body, draft, &repo_root)?;
            let response =
                submit_publish_request(Path::new(&config.publisher_socket_path), &request).await?;
            println!("pr-url: {}", response.pr_url);
            println!("branch: {}", response.branch);
            println!("pull-number: {}", response.pull_number);
        }
        Commands::Publisher { command } => {
            ensure_linux()?;
            let config = Config::load(Some(Path::new(&config_path)))?;
            match command {
                PublisherCommand::Start => start_publisher_process_mode(&config, &config_path)?,
                PublisherCommand::Stop => stop_publisher_process_mode(&config)?,
                PublisherCommand::Status => print_publisher_status_summary(&config),
                PublisherCommand::Logs => {
                    let logs =
                        read_publisher_logs(&config, DEFAULT_LOG_LINES.parse().unwrap_or(200))?;
                    if logs.is_empty() {
                        println!(
                            "no recent logs found for {}",
                            publisher_process_log_path(&config).display()
                        );
                    } else {
                        print!("{logs}");
                    }
                }
            }
        }
        Commands::Sprite { command } => {
            let config = Config::load(Some(Path::new(&config_path)))?;
            match command {
                SpriteCommand::ServicesStatus { sprite, org } => {
                    print_sprite_services_status_summary(
                        &config,
                        Path::new(&config_path),
                        &sprite,
                        org.as_deref(),
                    )?;
                }
                SpriteCommand::ServiceLogs {
                    sprite,
                    service,
                    org,
                    lines,
                    duration,
                } => {
                    print_sprite_service_logs(
                        &sprite,
                        org.as_deref(),
                        &service,
                        lines,
                        duration.as_deref(),
                    )?;
                }
            }
        }
    }

    Ok(())
}

fn resolve_pr_body(body: Option<String>, body_file: Option<&str>) -> Result<String> {
    match (body, body_file) {
        (Some(_), Some(_)) => bail!("--body and --body-file are mutually exclusive"),
        (Some(body), None) => Ok(body),
        (None, Some(path)) => {
            fs::read_to_string(path).with_context(|| format!("failed to read PR body file {path}"))
        }
        (None, None) => Ok(String::new()),
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct GitCredentialRequest {
    protocol: Option<String>,
    host: Option<String>,
    path: Option<String>,
    url: Option<String>,
    username: Option<String>,
}

async fn handle_git_credential_helper(config: &Config, operation: &str) -> Result<()> {
    let request = read_git_credential_request()?;

    if operation != "get" || !git_credential_request_targets_github(&request) {
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

fn credential_host_is_github(host: &str) -> bool {
    let normalized = host
        .split(':')
        .next()
        .unwrap_or(host)
        .trim_end_matches('.')
        .to_ascii_lowercase();
    normalized == "github.com" || normalized == "www.github.com"
}

fn sprite_runtime_detected() -> bool {
    Path::new("/.sprite").exists()
}

fn sprite_services_management_hint(config_path: &Path) -> String {
    format!(
        "Sprite runtime detected; manage lifecycle from a machine with Sprite CLI access. Prefer `scripts/upgrade-sprite.sh --sprite <sprite> --config {}` for upgrades, or `scripts/sprite-services.sh sync --sprite <sprite> --config {} --force-recreate` for control-plane recovery.",
        config_path.display(),
        config_path.display()
    )
}

fn sprite_service_supervisor_command_tokens(
    config_path: &Path,
) -> BTreeMap<&'static str, Vec<String>> {
    expected_sprite_service_definitions(config_path)
        .into_iter()
        .map(|(service_name, definition)| {
            let mut tokens = vec![definition.cmd];
            tokens.extend(definition.args);
            (service_name, tokens)
        })
        .collect()
}

fn sprite_service_supervisor_pids(config_path: &Path) -> Result<BTreeMap<&'static str, i32>> {
    let raw = run_command_capture("ps", &["-eo".to_string(), "pid=,args=".to_string()])?;
    Ok(sprite_service_supervisor_pids_from_ps(&raw, config_path))
}

fn sprite_service_supervisor_pids_from_ps(
    raw: &str,
    config_path: &Path,
) -> BTreeMap<&'static str, i32> {
    let expected = sprite_service_supervisor_command_tokens(config_path);
    let mut found = BTreeMap::new();

    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let mut fields = trimmed.split_whitespace();
        let Some(pid_field) = fields.next() else {
            continue;
        };
        let Ok(pid) = pid_field.parse::<i32>() else {
            continue;
        };
        let args: Vec<String> = fields.map(ToString::to_string).collect();

        for (&service_name, expected_tokens) in &expected {
            if args == *expected_tokens {
                found.insert(service_name, pid);
            }
        }
    }

    found
}

fn start_stack(config_path: &Path) -> Result<()> {
    let mut config = Config::load(Some(config_path))?;
    ensure_stack_config_ready(&config)?;

    if !tls_artifacts_exist(&config) {
        println!("TLS artifacts missing; creating them automatically");
        config = provision_tls_artifacts(config_path, false)?;
    }

    if sprite_runtime_detected() {
        let sprite_pids = sprite_service_supervisor_pids(config_path)?;
        if sprite_pids.contains_key(PUBLISHER_SERVICE_LABEL)
            && sprite_pids.contains_key(SPRITE_MAIN_SERVICE_LABEL)
        {
            println!("Sprite runtime detected; lifecycle is managed by Sprite Services");
            print_stack_ready_summary(&config);
            return Ok(());
        }

        bail!("{}", sprite_services_management_hint(config_path));
    }

    start_publisher_process_mode(&config, config_path)?;
    start_main_service(&config, config_path)?;
    print_stack_ready_summary(&config);
    Ok(())
}

fn stop_stack(config: &Config) -> Result<()> {
    stop_main_service(config)?;
    stop_publisher_process_mode(config)?;
    if sprite_runtime_detected() {
        println!(
            "note: Sprite runtime detected; this command only stops detached process-mode daemons"
        );
        println!("hint: Sprite Services remain the lifecycle owner for the running stack");
    }
    Ok(())
}

fn restart_stack(config_path: &Path) -> Result<()> {
    let mut config = Config::load(Some(config_path))?;
    ensure_stack_config_ready(&config)?;

    if !tls_artifacts_exist(&config) {
        println!("TLS artifacts missing; creating them automatically");
        config = provision_tls_artifacts(config_path, false)?;
    }

    if sprite_runtime_detected() {
        restart_sprite_services_in_guest(config_path)?;
        print_stack_ready_summary(&config);
        return Ok(());
    }

    stop_main_service(&config)?;
    stop_publisher_process_mode(&config)?;
    start_publisher_process_mode(&config, config_path)?;
    start_main_service(&config, config_path)?;
    print_stack_ready_summary(&config);
    Ok(())
}

fn start_main_service(config: &Config, config_path: &Path) -> Result<()> {
    match detect_service_manager() {
        ServiceManager::Systemd => {
            run_systemctl(&build_systemctl_args(SystemctlAction::Start))?;
            println!("started {SERVICE_NAME}");
            Ok(())
        }
        ServiceManager::Process => start_process_mode(config, config_path),
    }
}

fn stop_main_service(config: &Config) -> Result<()> {
    match detect_service_manager() {
        ServiceManager::Systemd => {
            run_systemctl(&build_systemctl_args(SystemctlAction::Stop))?;
            println!("stopped {SERVICE_NAME}");
            Ok(())
        }
        ServiceManager::Process => stop_process_mode(config),
    }
}

fn print_stack_ready_summary(config: &Config) {
    let host_hint = status_host_hint(&config.bind_host, detect_public_ip());
    let url_hint =
        redact_api_key_query_params(&format!("https://{host_hint}/mcp?key={}", config.api_key));
    println!("stack-ready: {SERVICE_NAME} + {PUBLISHER_SERVICE_LABEL}");
    println!("url-hint: {url_hint}");
    if let Some(port) = config.http_bind_port {
        println!("http-proxy-listen: {}:{port}", config.bind_host);
    }
}

fn install(config_path: &Path) -> Result<()> {
    ensure_linux()?;
    create_required_dirs(config_path)?;
    ensure_config_exists(config_path)?;
    let config = Config::load(Some(config_path))?;

    match detect_service_manager() {
        ServiceManager::Systemd => {
            let daemon_path = resolve_daemon_binary_path()?;
            let unit_content = render_systemd_unit(&daemon_path, config_path);
            let unit_changed = write_if_changed(Path::new(SYSTEMD_UNIT_PATH), &unit_content)?;
            if unit_changed {
                println!("wrote unit file at {SYSTEMD_UNIT_PATH}");
            } else {
                println!("unit file already up to date at {SYSTEMD_UNIT_PATH}");
            }

            run_systemctl(&build_systemctl_args(SystemctlAction::DaemonReload))?;
            run_systemctl(&build_systemctl_args(SystemctlAction::Enable))?;
            println!("enabled {SERVICE_NAME} for boot persistence");
        }
        ServiceManager::Process => {
            ensure_process_mode_accounts(&config)?;
            ensure_process_mode_dirs(&config)?;
            ensure_publisher_process_dirs(&config)?;
            ensure_agent_workspace_dirs(&config)?;
            prepare_agent_process_ownership(&config)?;
            println!(
                "systemd not detected; configured process mode for container-style environments"
            );
            println!(
                "process mode files: pid={}, log={}",
                process_pid_path(&config).display(),
                process_log_path(&config).display()
            );
            println!(
                "publisher process mode files: pid={}, log={}, socket={}",
                publisher_process_pid_path(&config).display(),
                publisher_process_log_path(&config).display(),
                config.publisher_socket_path
            );
            println!("agent home: {}", config.agent_home);
            println!("default workdir: {}", config.default_workdir);
        }
    }
    Ok(())
}

fn upgrade(config_path: &Path, version: &str) -> Result<()> {
    let config = Config::load(Some(config_path))?;

    let install_args = build_upgrade_shell_args(version, &config);
    run_shell_script(&install_args)?;
    restart_stack(config_path)?;
    Ok(())
}

fn build_upgrade_shell_args(version: &str, config: &Config) -> Vec<String> {
    let mut script = format!(
        "set -euo pipefail\nexport COMPUTER_MCP_VERSION={}\n",
        shell_escape_single_quotes(version)
    );

    if version != "latest" {
        script.push_str(&format!(
            "export COMPUTER_MCP_SOURCE_REF={}\n",
            shell_escape_single_quotes(version)
        ));
    }

    if let Some(port) = config.http_bind_port {
        script.push_str(&format!("export COMPUTER_MCP_HTTP_BIND_PORT={port}\n"));
    }

    let installer_ref = upgrade_installer_ref(version);
    let installer_url = format!(
        "https://raw.githubusercontent.com/amxv/computer-mcp/{installer_ref}/scripts/install.sh"
    );
    script.push_str(&format!(
        "curl -fsSL {} | bash",
        shell_escape_single_quotes(&installer_url)
    ));
    vec!["-lc".to_string(), script]
}

fn upgrade_installer_ref(version: &str) -> &str {
    if version == "latest" { "main" } else { version }
}

fn shell_escape_single_quotes(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn run_shell_script(args: &[String]) -> Result<String> {
    run_command_capture("bash", args)
}

fn tls_setup(config_path: &Path) -> Result<()> {
    provision_tls_artifacts(config_path, true)?;
    Ok(())
}

fn provision_tls_artifacts(config_path: &Path, restart_after: bool) -> Result<Config> {
    ensure_linux()?;
    let mut config = Config::load(Some(config_path))?;
    ensure_tls_dirs_for_config(&config)?;

    let san_ip = select_tls_san_ip(&config.bind_host, detect_public_ip());
    println!("tls setup target IP SAN: {san_ip}");

    match try_setup_letsencrypt_ip(&config, san_ip) {
        Ok(()) => {
            config.tls_mode = TLS_MODE_LETSENCRYPT_IP.to_string();
            println!("acquired Let's Encrypt IP certificate");
        }
        Err(err) => {
            eprintln!(
                "warning: Let's Encrypt IP certificate setup failed, falling back to self-signed: {err}"
            );
            generate_self_signed_certificate(&config, san_ip)?;
            config.tls_mode = TLS_MODE_SELF_SIGNED.to_string();
            println!(
                "generated self-signed certificate fallback at {} and {}",
                config.tls_cert_path, config.tls_key_path
            );
        }
    }

    config.save(config_path)?;
    ensure_shared_group_permissions(&config, config_path)?;
    println!("updated TLS settings in {}", config_path.display());
    if restart_after {
        restart_service_after_tls_setup(&config, config_path);
    }
    Ok(config)
}

fn ensure_linux() -> Result<()> {
    if cfg!(target_os = "linux") {
        Ok(())
    } else {
        bail!("{PRIMARY_OPERATOR_BINARY} service management is Linux-only");
    }
}

fn detect_service_manager() -> ServiceManager {
    if !command_exists("systemctl") {
        return ServiceManager::Process;
    }

    match fs::read_to_string("/proc/1/comm") {
        Ok(pid1_comm) => service_manager_from_pid1(pid1_comm.trim()),
        Err(_) => ServiceManager::Process,
    }
}

fn service_manager_from_pid1(pid1_comm: &str) -> ServiceManager {
    if pid1_comm == "systemd" {
        ServiceManager::Systemd
    } else {
        ServiceManager::Process
    }
}

fn command_exists(program: &str) -> bool {
    Command::new("sh")
        .arg("-c")
        .arg(format!("command -v {program} >/dev/null 2>&1"))
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

#[cfg(unix)]
fn resolve_login_shell() -> &'static str {
    if Path::new("/bin/bash").exists() {
        "/bin/bash"
    } else {
        "/bin/sh"
    }
}

#[cfg(unix)]
fn current_euid_is_root() -> bool {
    Uid::effective().is_root()
}

#[cfg(not(unix))]
fn current_euid_is_root() -> bool {
    false
}

#[cfg(unix)]
fn lookup_user(name: &str) -> Result<User> {
    User::from_name(name)
        .context("failed to query local user database")?
        .ok_or_else(|| anyhow!("local user not found: {name}"))
}

#[cfg(unix)]
fn lookup_group(name: &str) -> Result<Group> {
    Group::from_name(name)
        .context("failed to query local group database")?
        .ok_or_else(|| anyhow!("local group not found: {name}"))
}

#[cfg(unix)]
fn chown_path_to_user(path: &Path, user: &User) -> Result<()> {
    chown(path, Some(user.uid), Some(user.gid))
        .with_context(|| format!("failed to chown {} to {}", path.display(), user.name))
}

#[cfg(unix)]
fn chown_path_to_group(path: &Path, group: &Group) -> Result<()> {
    chown(path, None, Some(group.gid))
        .with_context(|| format!("failed to chgrp {} to {}", path.display(), group.name))
}

#[cfg(unix)]
fn ensure_runuser_available() -> Result<()> {
    if command_exists("runuser") {
        Ok(())
    } else {
        bail!("`runuser` is required to launch daemons under separate users")
    }
}

#[cfg(unix)]
fn ensure_process_mode_accounts(config: &Config) -> Result<()> {
    if !current_euid_is_root() {
        return Ok(());
    }

    if lookup_group(&config.service_group).is_err() {
        run_command_capture(
            "groupadd",
            &["--system".to_string(), config.service_group.clone()],
        )?;
    }

    ensure_process_mode_agent_user(config)?;
    ensure_process_mode_publisher_user(config)?;
    Ok(())
}

#[cfg(not(unix))]
fn ensure_process_mode_accounts(_config: &Config) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn ensure_process_mode_agent_user(config: &Config) -> Result<()> {
    if lookup_user(&config.agent_user).is_ok() {
        run_command_capture(
            "usermod",
            &[
                "--home".to_string(),
                config.agent_home.clone(),
                "--shell".to_string(),
                resolve_login_shell().to_string(),
                config.agent_user.clone(),
            ],
        )?;
        return Ok(());
    }

    run_command_capture(
        "useradd",
        &[
            "--system".to_string(),
            "--create-home".to_string(),
            "--home-dir".to_string(),
            config.agent_home.clone(),
            "--shell".to_string(),
            resolve_login_shell().to_string(),
            "--gid".to_string(),
            config.service_group.clone(),
            config.agent_user.clone(),
        ],
    )?;
    Ok(())
}

#[cfg(unix)]
fn ensure_process_mode_publisher_user(config: &Config) -> Result<()> {
    if lookup_user(&config.publisher_user).is_ok() {
        run_command_capture(
            "usermod",
            &[
                "--home".to_string(),
                "/nonexistent".to_string(),
                "--shell".to_string(),
                "/usr/sbin/nologin".to_string(),
                config.publisher_user.clone(),
            ],
        )?;
        return Ok(());
    }

    run_command_capture(
        "useradd",
        &[
            "--system".to_string(),
            "--no-create-home".to_string(),
            "--home-dir".to_string(),
            "/nonexistent".to_string(),
            "--shell".to_string(),
            "/usr/sbin/nologin".to_string(),
            "--gid".to_string(),
            config.service_group.clone(),
            config.publisher_user.clone(),
        ],
    )?;
    Ok(())
}

fn create_required_dirs(config_path: &Path) -> Result<()> {
    if let Some(parent) = config_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create config directory {}", parent.display()))?;
    }
    fs::create_dir_all(STATE_DIR).with_context(|| format!("failed to create {STATE_DIR}"))?;
    fs::create_dir_all(TLS_DIR).with_context(|| format!("failed to create {TLS_DIR}"))?;
    Ok(())
}

fn ensure_tls_dirs_for_config(config: &Config) -> Result<()> {
    if let Some(parent) = Path::new(&config.tls_cert_path).parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create TLS cert directory {}", parent.display()))?;
    }
    if let Some(parent) = Path::new(&config.tls_key_path).parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create TLS key directory {}", parent.display()))?;
    }
    Ok(())
}

#[cfg(unix)]
fn ensure_shared_group_permissions(config: &Config, config_path: &Path) -> Result<()> {
    if !current_euid_is_root() {
        return Ok(());
    }

    let Ok(group) = lookup_group(&config.service_group) else {
        return Ok(());
    };

    if config_path.exists() {
        chown_path_to_group(config_path, &group)?;
        set_file_mode(config_path, 0o640)?;
    }

    let cert_path = Path::new(&config.tls_cert_path);
    if cert_path.exists() {
        chown_path_to_group(cert_path, &group)?;
        set_file_mode(cert_path, 0o644)?;
    }

    let key_path = Path::new(&config.tls_key_path);
    if key_path.exists() {
        chown_path_to_group(key_path, &group)?;
        set_file_mode(key_path, 0o640)?;
    }

    Ok(())
}

#[cfg(not(unix))]
fn ensure_shared_group_permissions(_config: &Config, _config_path: &Path) -> Result<()> {
    Ok(())
}

fn ensure_config_exists(config_path: &Path) -> Result<()> {
    if config_path.exists() {
        return Ok(());
    }

    let config = Config::default();
    config.save(config_path)?;
    ensure_shared_group_permissions(&config, config_path)?;
    println!("created default config at {}", config_path.display());
    Ok(())
}

fn ensure_stack_config_ready(config: &Config) -> Result<()> {
    ensure_reader_ready_for_start(config)?;
    ensure_publisher_ready_for_start(config)?;
    ensure_http_listener_ready_for_start(config)?;
    Ok(())
}

fn ensure_http_listener_ready_for_start(config: &Config) -> Result<()> {
    if let Some(port) = config.http_bind_port
        && port == config.bind_port
    {
        bail!("http_bind_port must differ from bind_port");
    }

    Ok(())
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

fn ensure_publisher_ready_for_start(config: &Config) -> Result<()> {
    let Some(app_id) = config.publisher_app_id else {
        bail!("publisher_app_id must be configured before start");
    };
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
    if config.publisher_targets.is_empty() {
        bail!("publisher_targets must contain at least one allowed repo target");
    }

    for target in &config.publisher_targets {
        if target.id.trim().is_empty() || target.repo.trim().is_empty() {
            bail!("publisher target entries require both id and repo");
        }
        if target.installation_id == 0 {
            bail!("publisher target {} must define installation_id", target.id);
        }
    }

    Ok(())
}

fn resolve_daemon_binary_path() -> Result<PathBuf> {
    if let Ok(override_path) = std::env::var("COMPUTER_MCPD_PATH") {
        let path = PathBuf::from(&override_path);
        if path.exists() {
            return Ok(path);
        }
        bail!("COMPUTER_MCPD_PATH does not exist: {override_path}");
    }

    let mut candidates: Vec<PathBuf> = Vec::new();
    let current_exe = std::env::current_exe().context("failed to resolve current executable")?;
    if let Some(parent) = current_exe.parent() {
        candidates.push(parent.join("computer-mcpd"));
    }
    candidates.push(PathBuf::from("/usr/local/bin/computer-mcpd"));
    candidates.push(PathBuf::from("/usr/bin/computer-mcpd"));

    candidates
        .into_iter()
        .find(|path| path.exists())
        .ok_or_else(|| anyhow!("failed to locate {PRIMARY_DAEMON_BINARY} binary"))
}

fn resolve_publisher_daemon_binary_path() -> Result<PathBuf> {
    if let Ok(override_path) = std::env::var("COMPUTER_MCP_PRD_PATH") {
        let path = PathBuf::from(&override_path);
        if path.exists() {
            return Ok(path);
        }
        bail!("COMPUTER_MCP_PRD_PATH does not exist: {override_path}");
    }

    let mut candidates: Vec<PathBuf> = Vec::new();
    let current_exe = std::env::current_exe().context("failed to resolve current executable")?;
    if let Some(parent) = current_exe.parent() {
        candidates.push(parent.join(PUBLISHER_SERVICE_LABEL));
    }
    candidates.push(PathBuf::from(format!(
        "/usr/local/bin/{PUBLISHER_SERVICE_LABEL}"
    )));
    candidates.push(PathBuf::from(format!("/usr/bin/{PUBLISHER_SERVICE_LABEL}")));

    candidates
        .into_iter()
        .find(|path| path.exists())
        .ok_or_else(|| anyhow!("failed to locate {PUBLISHER_SERVICE_LABEL} binary"))
}

fn state_root_for_config(config: &Config) -> PathBuf {
    let cert_path = Path::new(&config.tls_cert_path);
    if let Some(parent) = cert_path.parent().and_then(Path::parent) {
        return parent.to_path_buf();
    }
    PathBuf::from(STATE_DIR)
}

fn process_runtime_dir(config: &Config) -> PathBuf {
    state_root_for_config(config).join(PROCESS_RUNTIME_DIRNAME)
}

fn process_log_dir(config: &Config) -> PathBuf {
    state_root_for_config(config).join(PROCESS_LOG_DIRNAME)
}

fn process_pid_path(config: &Config) -> PathBuf {
    process_runtime_dir(config).join(PROCESS_PID_FILENAME)
}

fn process_log_path(config: &Config) -> PathBuf {
    process_log_dir(config).join(PROCESS_LOG_FILENAME)
}

fn agent_home_dir(config: &Config) -> PathBuf {
    PathBuf::from(&config.agent_home)
}

fn default_workdir_path(config: &Config) -> PathBuf {
    PathBuf::from(&config.default_workdir)
}

fn publisher_process_root(config: &Config) -> PathBuf {
    state_root_for_config(config).join(PUBLISHER_PROCESS_SUBDIR)
}

fn publisher_process_runtime_dir(config: &Config) -> PathBuf {
    publisher_process_root(config).join(PROCESS_RUNTIME_DIRNAME)
}

fn publisher_process_log_dir(config: &Config) -> PathBuf {
    publisher_process_root(config).join(PROCESS_LOG_DIRNAME)
}

fn publisher_process_pid_path(config: &Config) -> PathBuf {
    publisher_process_runtime_dir(config).join(PUBLISHER_PROCESS_PID_FILENAME)
}

fn publisher_process_log_path(config: &Config) -> PathBuf {
    publisher_process_log_dir(config).join(PUBLISHER_PROCESS_LOG_FILENAME)
}

fn ensure_process_mode_dirs(config: &Config) -> Result<()> {
    fs::create_dir_all(process_runtime_dir(config))
        .with_context(|| format!("failed to create {}", process_runtime_dir(config).display()))?;
    fs::create_dir_all(process_log_dir(config))
        .with_context(|| format!("failed to create {}", process_log_dir(config).display()))?;
    Ok(())
}

fn ensure_agent_workspace_dirs(config: &Config) -> Result<()> {
    for path in [agent_home_dir(config), default_workdir_path(config)] {
        if path.as_os_str().is_empty() {
            continue;
        }
        fs::create_dir_all(&path)
            .with_context(|| format!("failed to create {}", path.display()))?;
    }
    Ok(())
}

fn ensure_publisher_process_dirs(config: &Config) -> Result<()> {
    fs::create_dir_all(publisher_process_runtime_dir(config)).with_context(|| {
        format!(
            "failed to create {}",
            publisher_process_runtime_dir(config).display()
        )
    })?;
    fs::create_dir_all(publisher_process_log_dir(config)).with_context(|| {
        format!(
            "failed to create {}",
            publisher_process_log_dir(config).display()
        )
    })?;
    if let Some(parent) = Path::new(&config.publisher_socket_path).parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    Ok(())
}

#[cfg(unix)]
fn prepare_agent_process_ownership(config: &Config) -> Result<()> {
    if !current_euid_is_root() {
        return Ok(());
    }

    let user = lookup_user(&config.agent_user)?;
    chown_path_to_user(&process_runtime_dir(config), &user)?;
    chown_path_to_user(&process_log_dir(config), &user)?;
    for path in [agent_home_dir(config), default_workdir_path(config)] {
        if path.as_os_str().is_empty() || !path.exists() {
            continue;
        }
        chown_path_to_user(&path, &user)?;
        set_file_mode(&path, SHARED_PROCESS_DIR_MODE)?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn prepare_agent_process_ownership(_config: &Config) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn prepare_publisher_process_ownership(config: &Config) -> Result<()> {
    if !current_euid_is_root() {
        return Ok(());
    }

    let user = lookup_user(&config.publisher_user)?;
    chown_path_to_user(&publisher_process_root(config), &user)?;
    chown_path_to_user(&publisher_process_runtime_dir(config), &user)?;
    chown_path_to_user(&publisher_process_log_dir(config), &user)?;
    set_file_mode(&publisher_process_root(config), SHARED_PROCESS_DIR_MODE)?;
    set_file_mode(
        &publisher_process_runtime_dir(config),
        SHARED_PROCESS_DIR_MODE,
    )?;
    set_file_mode(&publisher_process_log_dir(config), SHARED_PROCESS_DIR_MODE)?;
    if let Some(parent) = Path::new(&config.publisher_socket_path).parent() {
        chown_path_to_user(parent, &user)?;
        set_file_mode(parent, SHARED_PROCESS_DIR_MODE)?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn prepare_publisher_process_ownership(_config: &Config) -> Result<()> {
    Ok(())
}

fn read_process_pid(config: &Config) -> Result<Option<i32>> {
    let pid_path = process_pid_path(config);
    if !pid_path.exists() {
        return Ok(None);
    }

    let raw = fs::read_to_string(&pid_path)
        .with_context(|| format!("failed to read {}", pid_path.display()))?;
    let pid = raw
        .trim()
        .parse::<i32>()
        .with_context(|| format!("invalid pid in {}", pid_path.display()))?;
    Ok(Some(pid))
}

fn read_publisher_pid(config: &Config) -> Result<Option<i32>> {
    let pid_path = publisher_process_pid_path(config);
    if !pid_path.exists() {
        return Ok(None);
    }

    let raw = fs::read_to_string(&pid_path)
        .with_context(|| format!("failed to read {}", pid_path.display()))?;
    let pid = raw
        .trim()
        .parse::<i32>()
        .with_context(|| format!("invalid pid in {}", pid_path.display()))?;
    Ok(Some(pid))
}

fn pid_is_running(pid: i32) -> bool {
    Path::new(&format!("/proc/{pid}")).exists()
}

fn daemon_launch_command(
    binary_path: &Path,
    config_path: &Path,
    run_user: &str,
) -> Result<Command> {
    #[cfg(unix)]
    if current_euid_is_root() {
        ensure_runuser_available()?;
        let mut command = Command::new("runuser");
        command
            .arg("-u")
            .arg(run_user)
            .arg("--")
            .arg(binary_path)
            .arg("--config")
            .arg(config_path);
        return Ok(command);
    }

    let mut command = Command::new(binary_path);
    command.arg("--config").arg(config_path);
    Ok(command)
}

fn remove_pid_file_if_present(config: &Config) -> Result<()> {
    let pid_path = process_pid_path(config);
    if pid_path.exists() {
        fs::remove_file(&pid_path)
            .with_context(|| format!("failed to remove {}", pid_path.display()))?;
    }
    Ok(())
}

fn remove_publisher_pid_file_if_present(config: &Config) -> Result<()> {
    let pid_path = publisher_process_pid_path(config);
    if pid_path.exists() {
        fs::remove_file(&pid_path)
            .with_context(|| format!("failed to remove {}", pid_path.display()))?;
    }
    Ok(())
}

fn start_process_mode(config: &Config, config_path: &Path) -> Result<()> {
    ensure_process_mode_accounts(config)?;
    ensure_process_mode_dirs(config)?;
    ensure_agent_workspace_dirs(config)?;
    prepare_agent_process_ownership(config)?;

    if let Some(pid) = read_process_pid(config)? {
        if pid_is_running(pid) {
            println!("{SERVICE_NAME} already running in process mode (pid {pid})");
            println!("log file: {}", process_log_path(config).display());
            return Ok(());
        }
        remove_pid_file_if_present(config)?;
    }

    let daemon_path = resolve_daemon_binary_path()?;
    let log_path = process_log_path(config);
    let stdout = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .with_context(|| format!("failed to open {}", log_path.display()))?;
    let stderr = stdout
        .try_clone()
        .with_context(|| format!("failed to clone {}", log_path.display()))?;

    let mut command = daemon_launch_command(&daemon_path, config_path, &config.agent_user)?;
    command
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .current_dir(default_workdir_path(config));

    #[cfg(unix)]
    unsafe {
        command.pre_exec(|| {
            setsid().map_err(|e| io::Error::other(e.to_string()))?;
            Ok(())
        });
    }

    let mut child = command
        .spawn()
        .with_context(|| format!("failed to spawn {}", daemon_path.display()))?;
    let pid = child.id() as i32;

    thread::sleep(Duration::from_millis(PROCESS_START_STABILIZE_MS));
    if let Some(status) = child.try_wait().context("failed to inspect child status")? {
        let recent_logs = read_process_logs(config, 50).unwrap_or_default();
        let details = if recent_logs.trim().is_empty() {
            "no recent process log output".to_string()
        } else {
            format!(
                "recent log output:\n{}",
                redact_api_key_query_params(&recent_logs)
            )
        };
        bail!("{SERVICE_NAME} exited immediately in process mode (status: {status})\n{details}");
    }

    fs::write(process_pid_path(config), format!("{pid}\n"))
        .with_context(|| format!("failed to write {}", process_pid_path(config).display()))?;
    println!("started {SERVICE_NAME} in process mode (pid {pid})");
    println!("log file: {}", log_path.display());
    Ok(())
}

fn stop_process_mode(config: &Config) -> Result<()> {
    let Some(pid) = read_process_pid(config)? else {
        println!("{SERVICE_NAME} is not running in process mode");
        return Ok(());
    };

    if !pid_is_running(pid) {
        remove_pid_file_if_present(config)?;
        println!("removed stale pid file for {SERVICE_NAME} (pid {pid})");
        return Ok(());
    }

    send_signal_if_running(pid, Signal::SIGTERM)?;
    let deadline = Instant::now() + Duration::from_millis(PROCESS_STOP_TIMEOUT_MS);
    while pid_is_running(pid) && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(PROCESS_STOP_POLL_MS));
    }

    if pid_is_running(pid) {
        send_signal_if_running(pid, Signal::SIGKILL)?;
        let kill_deadline = Instant::now() + Duration::from_millis(PROCESS_STOP_TIMEOUT_MS);
        while pid_is_running(pid) && Instant::now() < kill_deadline {
            thread::sleep(Duration::from_millis(PROCESS_STOP_POLL_MS));
        }
    }

    remove_pid_file_if_present(config)?;
    println!("stopped {SERVICE_NAME} in process mode");
    Ok(())
}

fn read_process_logs(config: &Config, max_lines: usize) -> Result<String> {
    let log_path = process_log_path(config);
    if !log_path.exists() {
        return Ok(String::new());
    }

    read_tail_lines(&log_path, max_lines)
}

fn start_publisher_process_mode(config: &Config, config_path: &Path) -> Result<()> {
    ensure_process_mode_accounts(config)?;
    ensure_publisher_process_dirs(config)?;
    prepare_publisher_process_ownership(config)?;

    if let Some(pid) = read_publisher_pid(config)? {
        if pid_is_running(pid) {
            println!("{PUBLISHER_SERVICE_LABEL} already running in process mode (pid {pid})");
            println!("log file: {}", publisher_process_log_path(config).display());
            return Ok(());
        }
        remove_publisher_pid_file_if_present(config)?;
    }

    let daemon_path = resolve_publisher_daemon_binary_path()?;
    let log_path = publisher_process_log_path(config);
    let stdout = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .with_context(|| format!("failed to open {}", log_path.display()))?;
    let stderr = stdout
        .try_clone()
        .with_context(|| format!("failed to clone {}", log_path.display()))?;

    let mut command = daemon_launch_command(&daemon_path, config_path, &config.publisher_user)?;
    command
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .current_dir("/");

    #[cfg(unix)]
    unsafe {
        command.pre_exec(|| {
            setsid().map_err(|e| io::Error::other(e.to_string()))?;
            Ok(())
        });
    }

    let mut child = command
        .spawn()
        .with_context(|| format!("failed to spawn {}", daemon_path.display()))?;
    let pid = child.id() as i32;

    thread::sleep(Duration::from_millis(PROCESS_START_STABILIZE_MS));
    if let Some(status) = child.try_wait().context("failed to inspect child status")? {
        let recent_logs = read_publisher_logs(config, 50).unwrap_or_default();
        let details = if recent_logs.trim().is_empty() {
            "no recent process log output".to_string()
        } else {
            format!("recent log output:\n{}", recent_logs)
        };
        bail!(
            "{PUBLISHER_SERVICE_LABEL} exited immediately in process mode (status: {status})\n{details}"
        );
    }

    fs::write(publisher_process_pid_path(config), format!("{pid}\n")).with_context(|| {
        format!(
            "failed to write {}",
            publisher_process_pid_path(config).display()
        )
    })?;
    println!("started {PUBLISHER_SERVICE_LABEL} in process mode (pid {pid})");
    println!("log file: {}", log_path.display());
    Ok(())
}

fn stop_publisher_process_mode(config: &Config) -> Result<()> {
    let Some(pid) = read_publisher_pid(config)? else {
        println!("{PUBLISHER_SERVICE_LABEL} is not running in process mode");
        return Ok(());
    };

    if !pid_is_running(pid) {
        remove_publisher_pid_file_if_present(config)?;
        println!("removed stale pid file for {PUBLISHER_SERVICE_LABEL} (pid {pid})");
        return Ok(());
    }

    send_signal_if_running(pid, Signal::SIGTERM)?;
    let deadline = Instant::now() + Duration::from_millis(PROCESS_STOP_TIMEOUT_MS);
    while pid_is_running(pid) && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(PROCESS_STOP_POLL_MS));
    }

    if pid_is_running(pid) {
        send_signal_if_running(pid, Signal::SIGKILL)?;
        let kill_deadline = Instant::now() + Duration::from_millis(PROCESS_STOP_TIMEOUT_MS);
        while pid_is_running(pid) && Instant::now() < kill_deadline {
            thread::sleep(Duration::from_millis(PROCESS_STOP_POLL_MS));
        }
    }

    remove_publisher_pid_file_if_present(config)?;
    println!("stopped {PUBLISHER_SERVICE_LABEL} in process mode");
    Ok(())
}

fn restart_sprite_services_in_guest(config_path: &Path) -> Result<()> {
    let initial_pids = sprite_service_supervisor_pids(config_path)?;
    let missing_services: Vec<&str> = [PUBLISHER_SERVICE_LABEL, SPRITE_MAIN_SERVICE_LABEL]
        .into_iter()
        .filter(|service_name| !initial_pids.contains_key(service_name))
        .collect();

    if !missing_services.is_empty() {
        bail!(
            "Sprite runtime detected but the expected Sprite Services are not running inside the guest: {}.\n{}",
            missing_services.join(", "),
            sprite_services_management_hint(config_path)
        );
    }

    for service_name in [SPRITE_MAIN_SERVICE_LABEL, PUBLISHER_SERVICE_LABEL] {
        if let Some(pid) = initial_pids.get(service_name) {
            send_signal_if_running(*pid, Signal::SIGTERM)?;
            println!("recycling Sprite Service {service_name} (supervisor pid {pid})");
        }
    }

    wait_for_sprite_service_supervisor_restarts(config_path, &initial_pids)
}

fn wait_for_sprite_service_supervisor_restarts(
    config_path: &Path,
    initial_pids: &BTreeMap<&'static str, i32>,
) -> Result<()> {
    let deadline = Instant::now() + Duration::from_millis(SPRITE_SERVICE_RESTART_TIMEOUT_MS);
    loop {
        let current_pids = sprite_service_supervisor_pids(config_path)?;
        let all_restarted = [PUBLISHER_SERVICE_LABEL, SPRITE_MAIN_SERVICE_LABEL]
            .into_iter()
            .all(|service_name| {
                let Some(old_pid) = initial_pids.get(service_name) else {
                    return false;
                };
                current_pids
                    .get(service_name)
                    .is_some_and(|current_pid| current_pid != old_pid)
            });

        if all_restarted {
            for service_name in [PUBLISHER_SERVICE_LABEL, SPRITE_MAIN_SERVICE_LABEL] {
                if let Some(pid) = current_pids.get(service_name) {
                    println!("Sprite Service {service_name} restarted with supervisor pid {pid}");
                }
            }
            return Ok(());
        }

        if Instant::now() >= deadline {
            let summary = [PUBLISHER_SERVICE_LABEL, SPRITE_MAIN_SERVICE_LABEL]
                .into_iter()
                .map(|service_name| {
                    let old_pid = initial_pids
                        .get(service_name)
                        .map(|pid| pid.to_string())
                        .unwrap_or_else(|| "<missing>".to_string());
                    let new_pid = current_pids
                        .get(service_name)
                        .map(|pid| pid.to_string())
                        .unwrap_or_else(|| "<missing>".to_string());
                    format!("{service_name}: {old_pid} -> {new_pid}")
                })
                .collect::<Vec<_>>()
                .join(", ");
            bail!("timed out waiting for Sprite Services to restart ({summary})");
        }

        thread::sleep(Duration::from_millis(SPRITE_SERVICE_RESTART_POLL_MS));
    }
}

fn read_publisher_logs(config: &Config, max_lines: usize) -> Result<String> {
    let log_path = publisher_process_log_path(config);
    if !log_path.exists() {
        return Ok(String::new());
    }

    read_tail_lines(&log_path, max_lines)
}

fn read_tail_lines(path: &Path, max_lines: usize) -> Result<String> {
    let content =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let lines: Vec<&str> = content.lines().collect();
    let start = lines.len().saturating_sub(max_lines);
    let mut result = lines[start..].join("\n");
    if content.ends_with('\n') && !result.is_empty() {
        result.push('\n');
    }
    Ok(result)
}

#[cfg(unix)]
fn send_signal_if_running(pid: i32, signal: Signal) -> Result<()> {
    match kill(Pid::from_raw(pid), signal) {
        Ok(_) | Err(Errno::ESRCH) => Ok(()),
        Err(err) => Err(anyhow!("failed to send {signal:?} to pid {pid}: {err}")),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProcessModeState {
    Running(i32),
    Stale(i32),
    Stopped,
}

fn process_mode_state(config: &Config) -> Result<ProcessModeState> {
    match read_process_pid(config)? {
        Some(pid) if pid_is_running(pid) => Ok(ProcessModeState::Running(pid)),
        Some(pid) => Ok(ProcessModeState::Stale(pid)),
        None => Ok(ProcessModeState::Stopped),
    }
}

fn print_stack_status_summary(config: &Config) -> Result<()> {
    let main_lines = build_main_status_lines(config)?;
    for line in main_lines {
        println!("{line}");
    }

    println!();
    for line in build_publisher_status_lines(config, publisher_process_mode_state(config))? {
        println!("{line}");
    }

    println!();
    for line in build_reader_status_lines(config) {
        println!("{line}");
    }

    Ok(())
}

fn build_main_status_lines(config: &Config) -> Result<Vec<String>> {
    match detect_service_manager() {
        ServiceManager::Systemd => {
            let raw = run_systemctl(&build_systemctl_args(SystemctlAction::ShowStatus))?;
            Ok(build_status_summary_lines(&raw, config, detect_public_ip()))
        }
        ServiceManager::Process => {
            build_process_status_lines(config, detect_public_ip(), process_mode_state(config))
        }
    }
}

fn publisher_process_mode_state(config: &Config) -> Result<ProcessModeState> {
    match read_publisher_pid(config)? {
        Some(pid) if pid_is_running(pid) => Ok(ProcessModeState::Running(pid)),
        Some(pid) => Ok(ProcessModeState::Stale(pid)),
        None => Ok(ProcessModeState::Stopped),
    }
}

fn print_publisher_status_summary(config: &Config) {
    match build_publisher_status_lines(config, publisher_process_mode_state(config)) {
        Ok(lines) => {
            for line in lines {
                println!("{line}");
            }
        }
        Err(err) => eprintln!("warning: failed to build publisher status: {err}"),
    }
}

fn print_sprite_services_status_summary(
    config: &Config,
    config_path: &Path,
    sprite: &str,
    org: Option<&str>,
) -> Result<()> {
    let services = fetch_sprite_services(sprite, org)?;
    let lines = build_sprite_services_status_lines(config, config_path, sprite, &services);
    for line in lines {
        println!("{line}");
    }
    Ok(())
}

fn print_sprite_service_logs(
    sprite: &str,
    org: Option<&str>,
    service: &str,
    lines: Option<usize>,
    duration: Option<&str>,
) -> Result<()> {
    let path = sprite_service_logs_api_path(service, lines, duration);
    let raw = run_sprite_api(sprite, org, &path, &["-sS".to_string()])?;

    match serde_json::from_str::<serde_json::Value>(&raw) {
        Ok(parsed) => println!(
            "{}",
            serde_json::to_string_pretty(&parsed)
                .context("failed to format Sprite Service logs")?
        ),
        Err(_) => print!("{raw}"),
    }

    Ok(())
}

fn fetch_sprite_services(sprite: &str, org: Option<&str>) -> Result<Vec<SpriteServiceStatus>> {
    let raw = run_sprite_api(sprite, org, "/services", &["-sS".to_string()])?;
    serde_json::from_str(&raw).context("failed to parse Sprite Services response")
}

fn build_sprite_services_status_lines(
    config: &Config,
    config_path: &Path,
    sprite: &str,
    services: &[SpriteServiceStatus],
) -> Vec<String> {
    let expected = expected_sprite_service_definitions(config_path);
    let service_map: BTreeMap<&str, &SpriteServiceStatus> = services
        .iter()
        .map(|service| (service.name.as_str(), service))
        .collect();

    let mut lines = vec![
        format!("service-mode: sprite-services"),
        format!("sprite: {sprite}"),
        format!("config: {}", config_path.display()),
        format!("agent-home: {}", config.agent_home),
        format!("default-workdir: {}", config.default_workdir),
        format!("source-of-truth: sprite api -s {sprite} /services"),
    ];

    for service_name in [PUBLISHER_SERVICE_LABEL, SPRITE_MAIN_SERVICE_LABEL] {
        lines.push(String::new());
        lines.extend(build_single_sprite_service_status_lines(
            service_name,
            config,
            sprite,
            service_map.get(service_name).copied(),
            expected.get(service_name),
        ));
    }

    lines
}

fn build_single_sprite_service_status_lines(
    service_name: &str,
    config: &Config,
    sprite: &str,
    actual: Option<&SpriteServiceStatus>,
    expected: Option<&SpriteServiceDefinition>,
) -> Vec<String> {
    let mut lines = vec![format!("service: {service_name}")];

    let expected_run_user = if service_name == PUBLISHER_SERVICE_LABEL {
        config.publisher_user.as_str()
    } else {
        config.agent_user.as_str()
    };
    lines.push(format!("expected-run-user: {expected_run_user}"));

    let Some(service) = actual else {
        lines.push("active: missing".to_string());
        lines.push(format!(
            "hint: register Sprite Services with `scripts/sprite-services.sh sync --sprite {sprite}`"
        ));
        return lines;
    };

    let status = service
        .state
        .as_ref()
        .and_then(|state| state.status.as_deref())
        .unwrap_or("unknown");
    let pid = service
        .state
        .as_ref()
        .and_then(|state| state.pid)
        .map(|pid| pid.to_string())
        .unwrap_or_else(|| "unknown".to_string());
    let started_at = service
        .state
        .as_ref()
        .and_then(|state| state.started_at.as_deref())
        .unwrap_or("unknown");

    lines.push(format!("active: {status}"));
    lines.push(format!("pid: {pid}"));
    lines.push(format!("started-at: {started_at}"));
    lines.push(format!(
        "http-port: {}",
        service
            .http_port
            .map(|port| port.to_string())
            .unwrap_or_else(|| "n/a".to_string())
    ));
    lines.push(format!(
        "needs: {}",
        if service.needs.is_empty() {
            "none".to_string()
        } else {
            service.needs.join(", ")
        }
    ));
    lines.push(format!("cmd: {}", service.cmd));
    lines.push(format!("args: {}", service.args.join(" ")));

    if let Some(expected_definition) = expected {
        let matches = sprite_service_matches_definition(service, expected_definition);
        lines.push(format!(
            "definition-match: {}",
            if matches { "yes" } else { "no" }
        ));
        if !matches {
            lines.push(format!(
                "hint: re-sync with `scripts/sprite-services.sh sync --sprite {sprite}`"
            ));
        }
    }

    if status != "running" {
        lines.push(format!(
                    "hint: inspect logs with `{PRIMARY_OPERATOR_BINARY} sprite service-logs --sprite {sprite} --service {service_name}`"
        ));
    }

    lines
}

fn expected_sprite_service_definitions(
    config_path: &Path,
) -> BTreeMap<&'static str, SpriteServiceDefinition> {
    let config_arg = config_path.display().to_string();
    BTreeMap::from([
        (
            PUBLISHER_SERVICE_LABEL,
            SpriteServiceDefinition {
                cmd: "sudo".to_string(),
                args: vec![
                    "-n".to_string(),
                    "-u".to_string(),
                    "computer-mcp-publisher".to_string(),
                    format!("/usr/local/bin/{PUBLISHER_SERVICE_LABEL}"),
                    "--config".to_string(),
                    config_arg.clone(),
                ],
                needs: Vec::new(),
                http_port: None,
            },
        ),
        (
            SPRITE_MAIN_SERVICE_LABEL,
            SpriteServiceDefinition {
                cmd: "sudo".to_string(),
                args: vec![
                    "-n".to_string(),
                    "-u".to_string(),
                    "computer-mcp-agent".to_string(),
                    format!("/usr/local/bin/{SPRITE_MAIN_SERVICE_LABEL}"),
                    "--config".to_string(),
                    config_arg,
                ],
                needs: vec![PUBLISHER_SERVICE_LABEL.to_string()],
                http_port: Some(8080),
            },
        ),
    ])
}

fn sprite_service_matches_definition(
    actual: &SpriteServiceStatus,
    expected: &SpriteServiceDefinition,
) -> bool {
    actual.cmd == expected.cmd
        && actual.args == expected.args
        && actual.needs == expected.needs
        && actual.http_port == expected.http_port
}

fn sprite_service_logs_api_path(
    service: &str,
    lines: Option<usize>,
    duration: Option<&str>,
) -> String {
    let mut query = Vec::new();
    if let Some(lines) = lines {
        query.push(format!("lines={lines}"));
    }
    if let Some(duration) = duration
        && !duration.is_empty()
    {
        query.push(format!("duration={duration}"));
    }

    if query.is_empty() {
        format!("/services/{service}/logs")
    } else {
        format!("/services/{service}/logs?{}", query.join("&"))
    }
}

fn run_sprite_api(
    sprite: &str,
    org: Option<&str>,
    path: &str,
    curl_args: &[String],
) -> Result<String> {
    if !command_exists("sprite") {
        bail!("sprite CLI is required for Sprite service inspection");
    }

    let raw = run_command_capture(
        "sprite",
        &build_sprite_api_args(sprite, org, path, curl_args),
    )?;
    Ok(strip_sprite_api_prelude(&raw))
}

fn build_sprite_api_args(
    sprite: &str,
    org: Option<&str>,
    path: &str,
    curl_args: &[String],
) -> Vec<String> {
    let mut args = vec!["api".to_string()];
    if let Some(org) = org {
        args.push("-o".to_string());
        args.push(org.to_string());
    }
    args.push("-s".to_string());
    args.push(sprite.to_string());
    args.push(path.to_string());
    if !curl_args.is_empty() {
        args.push("--".to_string());
        args.extend(curl_args.iter().cloned());
    }
    args
}

fn strip_sprite_api_prelude(raw: &str) -> String {
    let lines: Vec<&str> = raw.lines().collect();
    if lines.len() >= 2 && lines[0].starts_with("Calling API:") && lines[1].starts_with("URL:") {
        let mut stripped = lines[2..].join("\n");
        if raw.ends_with('\n') && !stripped.ends_with('\n') {
            stripped.push('\n');
        }
        return stripped.trim_start_matches('\n').to_string();
    }

    raw.to_string()
}

fn build_reader_status_lines(config: &Config) -> Vec<String> {
    let app_id = config
        .reader_app_id
        .map(|id| id.to_string())
        .unwrap_or_else(|| "<unset>".to_string());
    let installation_id = config
        .reader_installation_id
        .map(|id| id.to_string())
        .unwrap_or_else(|| "<unset>".to_string());
    let ready = ensure_reader_ready_for_start(config).is_ok();

    let mut lines = vec![
        "service: computer-mcp-reader".to_string(),
        "service-mode: config-only".to_string(),
        format!("active: {}", if ready { "ready" } else { "not-ready" }),
        format!("reader-app-id: {app_id}"),
        format!("reader-installation-id: {installation_id}"),
        format!("reader-key: {}", config.reader_private_key_path),
    ];

    if config.reader_app_id.is_none() {
        lines.push("hint: set `reader_app_id` in config".to_string());
    }
    if config.reader_installation_id.is_none() {
        lines.push("hint: set `reader_installation_id` in config".to_string());
    }
    if !Path::new(&config.reader_private_key_path).exists() {
        lines.push("hint: place the reader private key at the configured path".to_string());
    }

    lines
}

fn sprite_runtime_note_lines() -> Vec<String> {
    if !Path::new("/.sprite").exists() {
        return Vec::new();
    }

    vec![
        "note: Sprite runtime detected; detached pid files are not authoritative across sleep/wake"
            .to_string(),
        "hint: use Sprite Services for lifecycle and inspect them from a machine with Sprite CLI access"
            .to_string(),
    ]
}

fn build_process_status_lines(
    config: &Config,
    public_ip: Option<IpAddr>,
    state: Result<ProcessModeState>,
) -> Result<Vec<String>> {
    let state = state?;
    let host_hint = status_host_hint(&config.bind_host, public_ip);
    let url_hint =
        redact_api_key_query_params(&format!("https://{host_hint}/mcp?key={}", config.api_key));
    let health_hint = format!("https://{host_hint}/health");
    let active = match state {
        ProcessModeState::Running(_) => "active (running)",
        ProcessModeState::Stale(_) => "inactive (stale pid file)",
        ProcessModeState::Stopped => "inactive (dead)",
    };
    let exec_status = match state {
        ProcessModeState::Running(pid) => format!("running pid {pid}"),
        ProcessModeState::Stale(pid) => format!("stale pid file {pid}"),
        ProcessModeState::Stopped => "not running".to_string(),
    };

    let mut lines = vec![
        format!("service: {SERVICE_NAME}"),
        "service-mode: process".to_string(),
        format!("active: {active}"),
        "enabled: n/a (process mode)".to_string(),
        "unit-file: n/a (process mode)".to_string(),
        format!("exec-main-status: {exec_status}"),
        format!("pid-file: {}", process_pid_path(config).display()),
        format!("log-file: {}", process_log_path(config).display()),
        format!("run-user: {}", config.agent_user),
        format!("agent-home: {}", config.agent_home),
        format!("default-workdir: {}", config.default_workdir),
        format!("listen: {}:{}", config.bind_host, config.bind_port),
        format!("tls-mode: {}", config.tls_mode),
        format!("tls-cert: {}", config.tls_cert_path),
        format!("tls-key: {}", config.tls_key_path),
        format!("url-hint: {url_hint}"),
        format!("health-hint: {health_hint}"),
    ];

    if !matches!(state, ProcessModeState::Running(_)) {
        lines.push(format!("hint: run `{PRIMARY_OPERATOR_BINARY} start`"));
    }
    if let Some(port) = config.http_bind_port {
        lines.push(format!("http-proxy-listen: {}:{port}", config.bind_host));
    }
    if !tls_artifacts_exist(config) {
        lines.push(format!(
            "note: `{PRIMARY_OPERATOR_BINARY} start` will create TLS artifacts automatically"
        ));
    }
    if matches!(state, ProcessModeState::Stale(_)) {
        lines.push(
            format!(
                "hint: stale pid file detected; `{PRIMARY_OPERATOR_BINARY} restart` will cleanly recover"
            )
                .to_string(),
        );
    }
    lines.extend(sprite_runtime_note_lines());

    Ok(lines)
}

fn build_publisher_status_lines(
    config: &Config,
    state: Result<ProcessModeState>,
) -> Result<Vec<String>> {
    let state = state?;
    let active = match state {
        ProcessModeState::Running(_) => "active (running)",
        ProcessModeState::Stale(_) => "inactive (stale pid file)",
        ProcessModeState::Stopped => "inactive (dead)",
    };
    let exec_status = match state {
        ProcessModeState::Running(pid) => format!("running pid {pid}"),
        ProcessModeState::Stale(pid) => format!("stale pid file {pid}"),
        ProcessModeState::Stopped => "not running".to_string(),
    };

    let mut lines = vec![
        format!("service: {PUBLISHER_SERVICE_LABEL}"),
        "service-mode: process".to_string(),
        format!("active: {active}"),
        format!("exec-main-status: {exec_status}"),
        format!("pid-file: {}", publisher_process_pid_path(config).display()),
        format!("log-file: {}", publisher_process_log_path(config).display()),
        format!("run-user: {}", config.publisher_user),
        format!("socket: {}", config.publisher_socket_path),
        format!(
            "publisher-app-id: {}",
            config
                .publisher_app_id
                .map(|id| id.to_string())
                .unwrap_or_else(|| "<unset>".to_string())
        ),
        format!("publisher-key: {}", config.publisher_private_key_path),
        format!("allowed-repos: {}", config.publisher_targets.len()),
    ];

    if !matches!(state, ProcessModeState::Running(_)) {
        lines.push(format!("hint: run `{PRIMARY_OPERATOR_BINARY} start`"));
    }
    if config.publisher_app_id.is_none() {
        lines.push("hint: set `publisher_app_id` in config".to_string());
    }
    if !Path::new(&config.publisher_private_key_path).exists() {
        lines.push("hint: place the publisher private key at the configured path".to_string());
    }
    if config.publisher_targets.is_empty() {
        lines.push("hint: add at least one `publisher_targets` entry to config".to_string());
    }
    lines.extend(sprite_runtime_note_lines());

    Ok(lines)
}

fn render_systemd_unit(daemon_path: &Path, config_path: &Path) -> String {
    let daemon_arg = quote_unit_arg(daemon_path);
    let config_arg = quote_unit_arg(config_path);

    format!(
        "[Unit]
Description=zodex daemon (computer-mcp compatible)
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
ExecStart={daemon_arg} --config {config_arg}
Restart=always
RestartSec=2
NoNewPrivileges=true
Environment=RUST_LOG=computer_mcp=info,computer_mcpd=info

[Install]
WantedBy=multi-user.target
"
    )
}

fn quote_unit_arg(path: &Path) -> String {
    let escaped = path
        .display()
        .to_string()
        .replace('\\', r"\\")
        .replace('"', r#"\""#);
    format!("\"{escaped}\"")
}

fn write_if_changed(path: &Path, content: &str) -> Result<bool> {
    if let Ok(existing) = fs::read_to_string(path)
        && existing == content
    {
        return Ok(false);
    }

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create directory {}", parent.display()))?;
    }
    fs::write(path, content).with_context(|| format!("failed to write {}", path.display()))?;
    Ok(true)
}

#[derive(Debug, Clone, Copy)]
enum SystemctlAction {
    DaemonReload,
    Enable,
    Start,
    Stop,
    Restart,
    ShowStatus,
}

fn build_systemctl_args(action: SystemctlAction) -> Vec<String> {
    match action {
        SystemctlAction::DaemonReload => vec!["daemon-reload".to_string()],
        SystemctlAction::Enable => vec!["enable".to_string(), SERVICE_NAME.to_string()],
        SystemctlAction::Start => vec!["start".to_string(), SERVICE_NAME.to_string()],
        SystemctlAction::Stop => vec!["stop".to_string(), SERVICE_NAME.to_string()],
        SystemctlAction::Restart => vec!["restart".to_string(), SERVICE_NAME.to_string()],
        SystemctlAction::ShowStatus => vec![
            "show".to_string(),
            SERVICE_NAME.to_string(),
            "--property=ActiveState,SubState,UnitFileState,FragmentPath,ExecMainStatus".to_string(),
            "--no-pager".to_string(),
        ],
    }
}

fn build_journalctl_args() -> Vec<String> {
    vec![
        "-u".to_string(),
        SERVICE_NAME.to_string(),
        "-n".to_string(),
        DEFAULT_LOG_LINES.to_string(),
        "--no-pager".to_string(),
    ]
}

fn run_systemctl(args: &[String]) -> Result<String> {
    run_command_capture("systemctl", args)
}

fn run_journalctl(args: &[String]) -> Result<String> {
    run_command_capture("journalctl", args)
}

fn run_command_capture(program: &str, args: &[String]) -> Result<String> {
    let output = Command::new(program)
        .args(args)
        .output()
        .with_context(|| format!("failed to run {program}"))?;

    let stdout = redact_api_key_query_params(&String::from_utf8_lossy(&output.stdout));
    let stderr = redact_api_key_query_params(&String::from_utf8_lossy(&output.stderr));

    if output.status.success() {
        return Ok(stdout);
    }

    let status = output.status.code().map_or_else(
        || "terminated by signal".to_string(),
        |code| code.to_string(),
    );
    let details = match (stdout.trim().is_empty(), stderr.trim().is_empty()) {
        (true, true) => "no output".to_string(),
        (false, true) => format!("stdout:\n{}", stdout.trim_end()),
        (true, false) => format!("stderr:\n{}", stderr.trim_end()),
        (false, false) => format!(
            "stdout:\n{}\n\nstderr:\n{}",
            stdout.trim_end(),
            stderr.trim_end()
        ),
    };

    bail!(
        "{program} {} failed (status: {status})\n{details}",
        args.join(" ")
    )
}

fn parse_systemctl_show(raw: &str) -> BTreeMap<String, String> {
    let mut values = BTreeMap::new();

    for line in raw.lines().filter(|line| !line.trim().is_empty()) {
        if let Some((key, value)) = line.split_once('=') {
            values.insert(key.to_string(), value.to_string());
        }
    }

    values
}

fn build_status_summary_lines(
    raw: &str,
    config: &Config,
    public_ip: Option<IpAddr>,
) -> Vec<String> {
    let parsed = parse_systemctl_show(raw);
    let active = parsed
        .get("ActiveState")
        .map(String::as_str)
        .unwrap_or("unknown");
    let sub = parsed
        .get("SubState")
        .map(String::as_str)
        .unwrap_or("unknown");
    let unit_file_state = parsed
        .get("UnitFileState")
        .map(String::as_str)
        .unwrap_or("unknown");
    let fragment = parsed
        .get("FragmentPath")
        .map(String::as_str)
        .unwrap_or("unknown");
    let exec_status = parsed
        .get("ExecMainStatus")
        .map(String::as_str)
        .unwrap_or("unknown");

    let host_hint = status_host_hint(&config.bind_host, public_ip);
    let url_hint =
        redact_api_key_query_params(&format!("https://{host_hint}/mcp?key={}", config.api_key));
    let health_hint = format!("https://{host_hint}/health");

    let mut lines = vec![
        format!("service: {SERVICE_NAME}"),
        format!("active: {active} ({sub})"),
        format!("enabled: {unit_file_state}"),
        format!("unit-file: {fragment}"),
        format!("exec-main-status: {exec_status}"),
        format!("listen: {}:{}", config.bind_host, config.bind_port),
        format!("tls-mode: {}", config.tls_mode),
        format!("tls-cert: {}", config.tls_cert_path),
        format!("tls-key: {}", config.tls_key_path),
        format!("url-hint: {url_hint}"),
        format!("health-hint: {health_hint}"),
    ];

    if active != "active" {
        lines.push(format!("hint: run `{PRIMARY_OPERATOR_BINARY} start`"));
    }
    if let Some(port) = config.http_bind_port {
        lines.push(format!("http-proxy-listen: {}:{port}", config.bind_host));
    }
    if unit_file_state != "enabled" {
        lines.push(format!("hint: run `{PRIMARY_OPERATOR_BINARY} install`"));
    }
    if !tls_artifacts_exist(config) {
        lines.push(format!(
            "note: `{PRIMARY_OPERATOR_BINARY} start` will create TLS artifacts automatically"
        ));
    }
    lines
}

fn tls_artifacts_exist(config: &Config) -> bool {
    Path::new(&config.tls_cert_path).exists() && Path::new(&config.tls_key_path).exists()
}

fn detect_public_ip() -> Option<IpAddr> {
    if !command_exists("curl") {
        return None;
    }

    let output = Command::new("curl")
        .args(["-fsS", "--max-time", "4", "https://api.ipify.org"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }

    let text = String::from_utf8(output.stdout).ok()?;
    text.trim().parse::<IpAddr>().ok()
}

fn status_host_hint(bind_host: &str, public_ip: Option<IpAddr>) -> String {
    if bind_host.is_empty() || bind_host == "0.0.0.0" || bind_host == "::" || bind_host == "[::]" {
        return public_ip
            .map(|ip| ip.to_string())
            .unwrap_or_else(|| STATUS_HOST_HINT_FALLBACK.to_string());
    }

    if let Ok(ip) = bind_host.parse::<IpAddr>()
        && ip.is_unspecified()
    {
        return public_ip
            .map(|candidate| candidate.to_string())
            .unwrap_or_else(|| STATUS_HOST_HINT_FALLBACK.to_string());
    }

    bind_host.to_string()
}

fn select_tls_san_ip(bind_host: &str, public_ip: Option<IpAddr>) -> IpAddr {
    if let Some(ip) = public_ip {
        return ip;
    }

    if let Ok(ip) = bind_host.parse::<IpAddr>()
        && !ip.is_unspecified()
    {
        return ip;
    }

    IpAddr::from([127, 0, 0, 1])
}

fn try_setup_letsencrypt_ip(config: &Config, ip: IpAddr) -> Result<()> {
    if !command_exists("certbot") {
        bail!("certbot is not installed");
    }

    let cert_name = certbot_cert_name(ip);
    run_command_capture("certbot", &build_certbot_args(ip, &cert_name))?;

    let (src_cert, src_key) = letsencrypt_live_paths(&cert_name);
    if !src_cert.exists() || !src_key.exists() {
        bail!(
            "expected certbot output files missing at {} and {}",
            src_cert.display(),
            src_key.display()
        );
    }

    copy_tls_files(
        &src_cert,
        &src_key,
        Path::new(&config.tls_cert_path),
        Path::new(&config.tls_key_path),
    )
}

fn certbot_cert_name(ip: IpAddr) -> String {
    format!("computer-mcp-{}", ip.to_string().replace(['.', ':'], "-"))
}

fn build_certbot_args(ip: IpAddr, cert_name: &str) -> Vec<String> {
    vec![
        "certonly".to_string(),
        "--standalone".to_string(),
        "--non-interactive".to_string(),
        "--agree-tos".to_string(),
        "--register-unsafely-without-email".to_string(),
        "--preferred-challenges".to_string(),
        "http".to_string(),
        "--keep-until-expiring".to_string(),
        "--cert-name".to_string(),
        cert_name.to_string(),
        "-d".to_string(),
        ip.to_string(),
    ]
}

fn letsencrypt_live_paths(cert_name: &str) -> (PathBuf, PathBuf) {
    let base = Path::new(LETSENCRYPT_LIVE_DIR).join(cert_name);
    (base.join("fullchain.pem"), base.join("privkey.pem"))
}

fn copy_tls_files(src_cert: &Path, src_key: &Path, dst_cert: &Path, dst_key: &Path) -> Result<()> {
    if let Some(parent) = dst_cert.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    if let Some(parent) = dst_key.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    fs::copy(src_cert, dst_cert).with_context(|| {
        format!(
            "failed to copy certificate from {} to {}",
            src_cert.display(),
            dst_cert.display()
        )
    })?;
    fs::copy(src_key, dst_key).with_context(|| {
        format!(
            "failed to copy private key from {} to {}",
            src_key.display(),
            dst_key.display()
        )
    })?;

    set_file_mode(dst_cert, 0o644)?;
    set_file_mode(dst_key, 0o600)?;
    Ok(())
}

fn generate_self_signed_certificate(config: &Config, ip: IpAddr) -> Result<()> {
    let mut params = CertificateParams::new(Vec::<String>::new())
        .context("failed to initialize self-signed cert parameters")?;
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, format!("computer-mcp {ip}"));
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
    set_file_mode(cert_path, 0o644)?;
    set_file_mode(key_path, 0o600)?;
    Ok(())
}

#[cfg(unix)]
fn set_file_mode(path: &Path, mode: u32) -> Result<()> {
    let mut perms = fs::metadata(path)
        .with_context(|| format!("failed to read metadata for {}", path.display()))?
        .permissions();
    perms.set_mode(mode);
    fs::set_permissions(path, perms)
        .with_context(|| format!("failed to set permissions for {}", path.display()))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_file_mode(_path: &Path, _mode: u32) -> Result<()> {
    Ok(())
}

fn restart_service_after_tls_setup(config: &Config, config_path: &Path) {
    if sprite_runtime_detected() {
        match restart_sprite_services_in_guest(config_path) {
            Ok(_) => println!("restarted Sprite-managed services to apply TLS changes"),
            Err(err) => eprintln!(
                "warning: TLS artifacts were updated but Sprite Service restart failed.\n{}",
                err
            ),
        }
        return;
    }

    match detect_service_manager() {
        ServiceManager::Systemd => {
            match run_systemctl(&build_systemctl_args(SystemctlAction::Restart)) {
                Ok(_) => println!("restarted {SERVICE_NAME} to apply TLS changes"),
                Err(err) => eprintln!(
                    "warning: TLS artifacts were updated but service restart failed. \
run `{PRIMARY_OPERATOR_BINARY} restart` manually.\n{err}"
                ),
            }
        }
        ServiceManager::Process => match process_mode_state(config) {
            Ok(ProcessModeState::Running(_)) => {
                if let Err(err) =
                    stop_process_mode(config).and_then(|_| start_process_mode(config, config_path))
                {
                    eprintln!(
                        "warning: TLS artifacts were updated but process-mode restart failed. \
run `{PRIMARY_OPERATOR_BINARY} --config \"{}\" restart` manually.\n{}",
                        config_path.display(),
                        err
                    );
                } else {
                    println!("restarted {SERVICE_NAME} in process mode to apply TLS changes");
                }
            }
            Ok(_) => {
                println!(
                    "TLS artifacts are ready. Start the stack with `{PRIMARY_OPERATOR_BINARY} --config \"{}\" start`.",
                    config_path.display(),
                );
            }
            Err(err) => eprintln!(
                "warning: TLS artifacts were updated but process-mode state check failed.\n{}",
                err
            ),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::{
        DEFAULT_LOG_LINES, PUBLISHER_SERVICE_LABEL, ProcessModeState, SERVICE_NAME,
        SPRITE_MAIN_SERVICE_LABEL, ServiceManager, SpriteServiceState, SpriteServiceStatus,
        SystemctlAction, build_certbot_args, build_journalctl_args, build_process_status_lines,
        build_publisher_status_lines, build_reader_status_lines, build_sprite_api_args,
        build_sprite_services_status_lines, build_status_summary_lines, build_systemctl_args,
        build_upgrade_shell_args, certbot_cert_name, credential_host_is_github,
        credential_url_host, credential_url_protocol, ensure_http_listener_ready_for_start,
        expected_sprite_service_definitions, generate_self_signed_certificate,
        git_credential_request_targets_github, parse_git_credential_request, parse_systemctl_show,
        process_log_path, process_pid_path, read_tail_lines, render_systemd_unit,
        select_tls_san_ip, service_manager_from_pid1, shell_escape_single_quotes,
        sprite_service_logs_api_path, sprite_service_supervisor_pids_from_ps,
        state_root_for_config, status_host_hint, strip_sprite_api_prelude, tls_artifacts_exist,
        write_if_changed,
    };
    use crate::Cli;
    use clap::CommandFactory;
    use computer_mcp::config::Config;
    use std::fs;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
    use std::path::{Path, PathBuf};
    use tempfile::tempdir;

    #[test]
    fn clap_help_uses_zodex_name() {
        let help = Cli::command().render_long_help().to_string();
        assert!(help.contains("zodex"));
        assert!(help.contains("legacy computer-mcp commands"));
    }

    #[test]
    fn render_systemd_unit_contains_expected_execstart() {
        let unit = render_systemd_unit(
            Path::new("/usr/local/bin/computer-mcpd"),
            Path::new("/etc/computer-mcp/config.toml"),
        );
        assert!(unit.contains("[Service]"));
        assert!(unit.contains(
            "ExecStart=\"/usr/local/bin/computer-mcpd\" --config \"/etc/computer-mcp/config.toml\""
        ));
        assert!(unit.contains("Restart=always"));
        assert!(unit.contains("[Install]"));
    }

    #[test]
    fn build_systemctl_args_match_expected_shapes() {
        assert_eq!(
            build_systemctl_args(SystemctlAction::DaemonReload),
            vec!["daemon-reload"]
        );
        assert_eq!(
            build_systemctl_args(SystemctlAction::Enable),
            vec!["enable", SERVICE_NAME]
        );
        assert_eq!(
            build_systemctl_args(SystemctlAction::Start),
            vec!["start", SERVICE_NAME]
        );
        assert_eq!(
            build_systemctl_args(SystemctlAction::Stop),
            vec!["stop", SERVICE_NAME]
        );
        assert_eq!(
            build_systemctl_args(SystemctlAction::Restart),
            vec!["restart", SERVICE_NAME]
        );
        assert_eq!(
            build_systemctl_args(SystemctlAction::ShowStatus),
            vec![
                "show",
                SERVICE_NAME,
                "--property=ActiveState,SubState,UnitFileState,FragmentPath,ExecMainStatus",
                "--no-pager",
            ]
        );
    }

    #[test]
    fn build_journalctl_args_match_expected_shape() {
        assert_eq!(
            build_journalctl_args(),
            vec!["-u", SERVICE_NAME, "-n", DEFAULT_LOG_LINES, "--no-pager",]
        );
    }

    #[test]
    fn build_upgrade_shell_args_include_requested_version_and_http_port() {
        let config = Config {
            http_bind_port: Some(8080),
            ..Config::default()
        };

        let args = build_upgrade_shell_args("v0.1.5", &config);
        assert_eq!(args[0], "-lc");
        assert!(args[1].contains("export COMPUTER_MCP_VERSION='v0.1.5'"));
        assert!(args[1].contains("export COMPUTER_MCP_SOURCE_REF='v0.1.5'"));
        assert!(args[1].contains("export COMPUTER_MCP_HTTP_BIND_PORT=8080"));
        assert!(
            args[1].contains(
                "curl -fsSL 'https://raw.githubusercontent.com/amxv/computer-mcp/v0.1.5/scripts/install.sh' | bash"
            )
        );
    }

    #[test]
    fn build_upgrade_shell_args_latest_uses_main_installer_ref() {
        let config = Config::default();

        let args = build_upgrade_shell_args("latest", &config);
        assert!(args[1].contains("export COMPUTER_MCP_VERSION='latest'"));
        assert!(!args[1].contains("COMPUTER_MCP_SOURCE_REF"));
        assert!(
            args[1].contains(
                "curl -fsSL 'https://raw.githubusercontent.com/amxv/computer-mcp/main/scripts/install.sh' | bash"
            )
        );
    }

    #[test]
    fn shell_escape_single_quotes_handles_embedded_quotes() {
        assert_eq!(shell_escape_single_quotes("v0.1.5's"), "'v0.1.5'\"'\"'s'");
    }

    #[test]
    fn parse_systemctl_show_extracts_values() {
        let raw = "ActiveState=active\nSubState=running\nUnitFileState=enabled\nFragmentPath=/etc/systemd/system/computer-mcpd.service\nExecMainStatus=0\n";
        let parsed = parse_systemctl_show(raw);

        assert_eq!(
            parsed.get("ActiveState").map(String::as_str),
            Some("active")
        );
        assert_eq!(parsed.get("SubState").map(String::as_str), Some("running"));
        assert_eq!(
            parsed.get("UnitFileState").map(String::as_str),
            Some("enabled")
        );
        assert_eq!(
            parsed.get("FragmentPath").map(String::as_str),
            Some("/etc/systemd/system/computer-mcpd.service")
        );
        assert_eq!(parsed.get("ExecMainStatus").map(String::as_str), Some("0"));
    }

    #[test]
    fn write_if_changed_is_idempotent() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("computer-mcpd.service");
        let content = "[Unit]\nDescription=test\n";

        let first = write_if_changed(&path, content).expect("first write");
        let second = write_if_changed(&path, content).expect("second write");

        assert!(first);
        assert!(!second);
        assert_eq!(fs::read_to_string(path).expect("read file"), content);
    }

    #[test]
    fn service_manager_from_pid1_detects_systemd() {
        assert_eq!(
            service_manager_from_pid1("systemd"),
            ServiceManager::Systemd
        );
        assert_eq!(
            service_manager_from_pid1("start.sh"),
            ServiceManager::Process
        );
    }

    #[test]
    fn state_root_for_config_uses_tls_parent_directory() {
        let config = Config {
            tls_cert_path: "/custom/state/tls/cert.pem".to_string(),
            ..Config::default()
        };

        assert_eq!(
            state_root_for_config(&config),
            PathBuf::from("/custom/state")
        );
        assert_eq!(
            process_pid_path(&config),
            PathBuf::from("/custom/state/run/computer-mcpd.pid")
        );
        assert_eq!(
            process_log_path(&config),
            PathBuf::from("/custom/state/logs/computer-mcpd.log")
        );
    }

    #[test]
    fn read_tail_lines_returns_only_requested_suffix() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("computer-mcpd.log");
        fs::write(&path, "one\ntwo\nthree\nfour\n").expect("write log");

        let got = read_tail_lines(&path, 2).expect("read tail");
        assert_eq!(got, "three\nfour\n");
    }

    #[test]
    fn certbot_helpers_build_expected_values() {
        let ip: IpAddr = "203.0.113.42".parse().expect("ip parse");
        let cert_name = certbot_cert_name(ip);
        assert_eq!(cert_name, "computer-mcp-203-0-113-42");

        let args = build_certbot_args(ip, &cert_name);
        assert!(args.contains(&"certonly".to_string()));
        assert!(args.contains(&"--standalone".to_string()));
        assert!(args.contains(&"--non-interactive".to_string()));
        assert!(args.contains(&"--cert-name".to_string()));
        assert!(args.contains(&cert_name));
        assert!(args.contains(&ip.to_string()));
    }

    #[test]
    fn select_tls_san_ip_prefers_public_ip() {
        let public = Some(IpAddr::V4(Ipv4Addr::new(198, 51, 100, 7)));
        let selected = select_tls_san_ip("0.0.0.0", public);
        assert_eq!(selected, IpAddr::V4(Ipv4Addr::new(198, 51, 100, 7)));
    }

    #[test]
    fn select_tls_san_ip_falls_back_to_bind_host() {
        let selected = select_tls_san_ip("192.0.2.10", None);
        assert_eq!(selected, IpAddr::V4(Ipv4Addr::new(192, 0, 2, 10)));
    }

    #[test]
    fn select_tls_san_ip_defaults_to_loopback() {
        let selected = select_tls_san_ip("0.0.0.0", None);
        assert_eq!(selected, IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)));
    }

    #[test]
    fn status_host_hint_uses_public_ip_for_wildcard_bind() {
        let hint = status_host_hint("0.0.0.0", Some(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 11))));
        assert_eq!(hint, "203.0.113.11");
    }

    #[test]
    fn status_host_hint_uses_specific_bind_host() {
        let hint = status_host_hint("192.0.2.5", None);
        assert_eq!(hint, "192.0.2.5");
    }

    #[test]
    fn status_host_hint_returns_placeholder_without_public_ip() {
        let hint = status_host_hint("::", None);
        assert_eq!(hint, "<host>");
    }

    #[test]
    fn build_status_summary_lines_includes_network_and_tls_details() {
        let raw = "ActiveState=active\nSubState=running\nUnitFileState=enabled\nExecMainStatus=0\n";
        let config = Config {
            bind_host: "0.0.0.0".to_string(),
            bind_port: 8443,
            http_bind_port: Some(8080),
            api_key: "abc123".to_string(),
            tls_mode: "self_signed".to_string(),
            tls_cert_path: "/var/lib/computer-mcp/tls/cert.pem".to_string(),
            tls_key_path: "/var/lib/computer-mcp/tls/key.pem".to_string(),
            ..Config::default()
        };

        let lines = build_status_summary_lines(
            raw,
            &config,
            Some(IpAddr::V4(Ipv4Addr::new(198, 51, 100, 88))),
        );
        let joined = lines.join("\n");
        assert!(joined.contains("listen: 0.0.0.0:8443"));
        assert!(joined.contains("tls-mode: self_signed"));
        assert!(joined.contains("tls-cert: /var/lib/computer-mcp/tls/cert.pem"));
        assert!(joined.contains("tls-key: /var/lib/computer-mcp/tls/key.pem"));
        assert!(joined.contains("url-hint: https://198.51.100.88/mcp?key=<redacted>"));
        assert!(joined.contains("health-hint: https://198.51.100.88/health"));
        assert!(joined.contains("http-proxy-listen: 0.0.0.0:8080"));
    }

    #[test]
    fn build_process_status_lines_includes_process_mode_details() {
        let config = Config {
            bind_host: "0.0.0.0".to_string(),
            bind_port: 9443,
            http_bind_port: Some(8080),
            api_key: "abc123".to_string(),
            tls_mode: "self_signed".to_string(),
            tls_cert_path: "/var/lib/computer-mcp/tls/cert.pem".to_string(),
            tls_key_path: "/var/lib/computer-mcp/tls/key.pem".to_string(),
            ..Config::default()
        };

        let lines = build_process_status_lines(
            &config,
            Some(IpAddr::V4(Ipv4Addr::new(198, 51, 100, 88))),
            Ok(ProcessModeState::Running(4242)),
        )
        .expect("build process status");
        let joined = lines.join("\n");
        assert!(joined.contains("service-mode: process"));
        assert!(joined.contains("active: active (running)"));
        assert!(joined.contains("exec-main-status: running pid 4242"));
        assert!(joined.contains("agent-home: /home/computer-mcp-agent"));
        assert!(joined.contains("default-workdir: /workspace"));
        assert!(joined.contains("url-hint: https://198.51.100.88/mcp?key=<redacted>"));
        assert!(joined.contains("health-hint: https://198.51.100.88/health"));
        assert!(joined.contains("http-proxy-listen: 0.0.0.0:8080"));
    }

    #[test]
    fn build_process_status_lines_suggests_recovery_for_stale_pid() {
        let config = Config::default();
        let lines = build_process_status_lines(&config, None, Ok(ProcessModeState::Stale(9999)))
            .expect("build process status");
        let joined = lines.join("\n");
        assert!(joined.contains("active: inactive (stale pid file)"));
        assert!(
            joined.contains("hint: stale pid file detected; `zodex restart` will cleanly recover")
        );
    }

    #[test]
    fn build_publisher_status_lines_includes_socket_and_run_user() {
        let config = Config::default();
        let lines = build_publisher_status_lines(&config, Ok(ProcessModeState::Running(5150)))
            .expect("build publisher status");
        let joined = lines.join("\n");
        assert!(joined.contains("service: computer-mcp-prd"));
        assert!(joined.contains("run-user: computer-mcp-publisher"));
        assert!(
            joined.contains("socket: /var/lib/computer-mcp/publisher/run/computer-mcp-prd.sock")
        );
        assert!(joined.contains("allowed-repos: 0"));
        assert!(joined.contains("hint: set `publisher_app_id` in config"));
    }

    #[test]
    fn expected_sprite_service_definitions_use_config_path() {
        let defs = expected_sprite_service_definitions(Path::new("/etc/computer-mcp/custom.toml"));

        assert_eq!(
            defs.get(PUBLISHER_SERVICE_LABEL)
                .expect("publisher definition")
                .args,
            vec![
                "-n".to_string(),
                "-u".to_string(),
                "computer-mcp-publisher".to_string(),
                "/usr/local/bin/computer-mcp-prd".to_string(),
                "--config".to_string(),
                "/etc/computer-mcp/custom.toml".to_string(),
            ]
        );
        assert_eq!(
            defs.get(SPRITE_MAIN_SERVICE_LABEL)
                .expect("main definition")
                .http_port,
            Some(8080)
        );
    }

    #[test]
    fn build_sprite_api_args_include_scope_and_passthrough_curl_flags() {
        let args = build_sprite_api_args(
            "computer",
            Some("amxv"),
            "/services",
            &["-sS".to_string(), "-X".to_string(), "PUT".to_string()],
        );

        assert_eq!(
            args,
            vec![
                "api".to_string(),
                "-o".to_string(),
                "amxv".to_string(),
                "-s".to_string(),
                "computer".to_string(),
                "/services".to_string(),
                "--".to_string(),
                "-sS".to_string(),
                "-X".to_string(),
                "PUT".to_string(),
            ]
        );
    }

    #[test]
    fn strip_sprite_api_prelude_removes_wrapper_lines() {
        let raw = "Calling API: amxv computer\nURL: https://api.sprites.dev/v1/sprites/computer/services\n\n[]\n";
        assert_eq!(strip_sprite_api_prelude(raw), "[]\n");
    }

    #[test]
    fn sprite_service_logs_api_path_adds_optional_query_params() {
        assert_eq!(
            sprite_service_logs_api_path("computer-mcpd", Some(50), Some("5s")),
            "/services/computer-mcpd/logs?lines=50&duration=5s"
        );
        assert_eq!(
            sprite_service_logs_api_path("computer-mcpd", None, None),
            "/services/computer-mcpd/logs"
        );
    }

    #[test]
    fn build_sprite_services_status_lines_report_missing_services() {
        let config = Config::default();
        let lines = build_sprite_services_status_lines(
            &config,
            Path::new("/etc/computer-mcp/config.toml"),
            "computer",
            &[],
        );
        let joined = lines.join("\n");

        assert!(joined.contains("service-mode: sprite-services"));
        assert!(joined.contains("service: computer-mcp-prd"));
        assert!(joined.contains("active: missing"));
        assert!(joined.contains("service: computer-mcpd"));
        assert!(joined.contains(
            "hint: register Sprite Services with `scripts/sprite-services.sh sync --sprite computer`"
        ));
    }

    #[test]
    fn build_sprite_services_status_lines_report_definition_drift() {
        let config = Config::default();
        let services = vec![
            SpriteServiceStatus {
                name: PUBLISHER_SERVICE_LABEL.to_string(),
                cmd: "sudo".to_string(),
                args: vec![
                    "-n".to_string(),
                    "-u".to_string(),
                    "computer-mcp-publisher".to_string(),
                    "/usr/local/bin/computer-mcp-prd".to_string(),
                    "--config".to_string(),
                    "/etc/computer-mcp/config.toml".to_string(),
                ],
                needs: Vec::new(),
                http_port: None,
                state: Some(SpriteServiceState {
                    name: Some(PUBLISHER_SERVICE_LABEL.to_string()),
                    pid: Some(111),
                    started_at: Some("2026-03-21T08:00:00Z".to_string()),
                    status: Some("running".to_string()),
                }),
            },
            SpriteServiceStatus {
                name: SPRITE_MAIN_SERVICE_LABEL.to_string(),
                cmd: "sudo".to_string(),
                args: vec![
                    "-n".to_string(),
                    "-u".to_string(),
                    "computer-mcp-agent".to_string(),
                    "/usr/local/bin/computer-mcpd".to_string(),
                    "--config".to_string(),
                    "/etc/computer-mcp/other.toml".to_string(),
                ],
                needs: vec![PUBLISHER_SERVICE_LABEL.to_string()],
                http_port: Some(8080),
                state: Some(SpriteServiceState {
                    name: Some(SPRITE_MAIN_SERVICE_LABEL.to_string()),
                    pid: Some(222),
                    started_at: Some("2026-03-21T08:01:00Z".to_string()),
                    status: Some("starting".to_string()),
                }),
            },
        ];

        let lines = build_sprite_services_status_lines(
            &config,
            Path::new("/etc/computer-mcp/config.toml"),
            "computer",
            &services,
        );
        let joined = lines.join("\n");

        assert!(joined.contains("service: computer-mcpd"));
        assert!(joined.contains("active: starting"));
        assert!(joined.contains("definition-match: no"));
        assert!(
            joined
                .contains("hint: re-sync with `scripts/sprite-services.sh sync --sprite computer`")
        );
        assert!(joined.contains(
            "hint: inspect logs with `zodex sprite service-logs --sprite computer --service computer-mcpd`"
        ));
    }

    #[test]
    fn sprite_service_supervisor_pids_from_ps_matches_sprite_managed_parents() {
        let raw = "\
11 sudo -n -u computer-mcp-publisher /usr/local/bin/computer-mcp-prd --config /etc/computer-mcp/config.toml
12 sudo -n -u computer-mcp-agent /usr/local/bin/computer-mcpd --config /etc/computer-mcp/config.toml
16 /usr/local/bin/computer-mcp-prd --config /etc/computer-mcp/config.toml
17 /usr/local/bin/computer-mcpd --config /etc/computer-mcp/config.toml
178 runuser -u computer-mcp-publisher -- /usr/local/bin/computer-mcp-prd --config /etc/computer-mcp/config.toml
";
        let pids =
            sprite_service_supervisor_pids_from_ps(raw, Path::new("/etc/computer-mcp/config.toml"));

        assert_eq!(pids.get(PUBLISHER_SERVICE_LABEL), Some(&11));
        assert_eq!(pids.get(SPRITE_MAIN_SERVICE_LABEL), Some(&12));
    }

    #[test]
    fn ensure_http_listener_ready_rejects_same_port_as_https() {
        let config = Config {
            bind_port: 443,
            http_bind_port: Some(443),
            ..Config::default()
        };

        let err = ensure_http_listener_ready_for_start(&config).expect_err("should fail");
        assert!(
            err.to_string()
                .contains("http_bind_port must differ from bind_port")
        );
    }

    #[test]
    fn build_status_summary_lines_notes_start_when_tls_files_missing() {
        let raw = "ActiveState=inactive\nSubState=dead\nUnitFileState=enabled\nExecMainStatus=1\n";
        let dir = tempdir().expect("tempdir");
        let config = Config {
            tls_cert_path: dir.path().join("missing-cert.pem").display().to_string(),
            tls_key_path: dir.path().join("missing-key.pem").display().to_string(),
            ..Config::default()
        };

        let lines = build_status_summary_lines(raw, &config, None);
        let joined = lines.join("\n");

        assert!(joined.contains("note: `zodex start` will create TLS artifacts automatically"));
    }

    #[test]
    fn tls_artifacts_exist_checks_both_files() {
        let dir = tempdir().expect("tempdir");
        let cert = dir.path().join("cert.pem");
        let key = dir.path().join("key.pem");
        fs::write(&cert, "cert").expect("write cert");

        let config = Config {
            tls_cert_path: cert.display().to_string(),
            tls_key_path: key.display().to_string(),
            ..Config::default()
        };
        assert!(!tls_artifacts_exist(&config));

        fs::write(&key, "key").expect("write key");
        assert!(tls_artifacts_exist(&config));
    }

    #[test]
    fn generate_self_signed_certificate_writes_pem_files() {
        let dir = tempdir().expect("tempdir");
        let cert_path = dir.path().join("cert.pem");
        let key_path = dir.path().join("key.pem");

        let config = Config {
            tls_cert_path: cert_path.display().to_string(),
            tls_key_path: key_path.display().to_string(),
            ..Config::default()
        };

        generate_self_signed_certificate(&config, IpAddr::V6(Ipv6Addr::LOCALHOST))
            .expect("generate self signed cert");

        let cert = fs::read_to_string(&cert_path).expect("read cert");
        let key = fs::read_to_string(&key_path).expect("read key");
        assert!(cert.contains("BEGIN CERTIFICATE"));
        assert!(key.contains("BEGIN PRIVATE KEY"));
    }

    #[test]
    fn build_reader_status_lines_include_reader_hints() {
        let config = Config::default();
        let joined = build_reader_status_lines(&config).join("\n");
        assert!(joined.contains("service: computer-mcp-reader"));
        assert!(joined.contains("active: not-ready"));
        assert!(joined.contains("hint: set `reader_app_id` in config"));
        assert!(joined.contains("hint: set `reader_installation_id` in config"));
    }

    #[test]
    fn parse_git_credential_request_extracts_known_fields() {
        let request = parse_git_credential_request(
            "protocol=https\nhost=github.com\npath=amxv/computer-mcp.git\nusername=x-access-token\n\n",
        );

        assert_eq!(request.protocol.as_deref(), Some("https"));
        assert_eq!(request.host.as_deref(), Some("github.com"));
        assert_eq!(request.path.as_deref(), Some("amxv/computer-mcp.git"));
        assert_eq!(request.username.as_deref(), Some("x-access-token"));
    }

    #[test]
    fn git_credential_request_targets_github_for_https_host() {
        let request = parse_git_credential_request("protocol=https\nhost=github.com\n\n");
        assert!(git_credential_request_targets_github(&request));
    }

    #[test]
    fn git_credential_request_targets_github_for_https_url_fallback() {
        let request =
            parse_git_credential_request("url=https://github.com/amxv/computer-mcp.git\n\n");
        assert!(git_credential_request_targets_github(&request));
    }

    #[test]
    fn git_credential_request_rejects_non_github_or_non_https() {
        let ssh_request = parse_git_credential_request("protocol=ssh\nhost=github.com\n\n");
        let other_host_request =
            parse_git_credential_request("protocol=https\nhost=example.com\n\n");

        assert!(!git_credential_request_targets_github(&ssh_request));
        assert!(!git_credential_request_targets_github(&other_host_request));
    }

    #[test]
    fn credential_url_helpers_extract_protocol_and_host() {
        assert_eq!(
            credential_url_protocol("https://github.com/amxv/computer-mcp.git"),
            Some("https")
        );
        assert_eq!(
            credential_url_host("https://token@github.com/amxv/computer-mcp.git"),
            Some("github.com")
        );
        assert!(credential_host_is_github("github.com:443"));
        assert!(credential_host_is_github("www.github.com"));
        assert!(!credential_host_is_github("gitlab.com"));
    }
}
