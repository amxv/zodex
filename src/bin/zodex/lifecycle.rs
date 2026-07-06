fn sprite_runtime_detected() -> bool {
    Path::new("/.sprite").exists()
}

fn sprite_services_management_hint(config_path: &Path) -> String {
    format!(
        "Sprite runtime detected; manage lifecycle from a machine with Sprite CLI access. Prefer `zodex sprite upgrade --sprite <sprite> --remote-config {}` for upgrades, or `zodex sprite sync --sprite <sprite> --remote-config {} --force-recreate` for control-plane recovery.",
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
    ensure_push_grants_dir_permissions(&config)?;

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
    if should_run_runtime_upgrade() {
        let config = Config::load(Some(config_path))?;
        let install_args = build_runtime_upgrade_shell_args(version, &config);
        run_shell_script(&install_args)?;
        restart_stack(config_path)?;
        return Ok(());
    }

    let install_args = build_operator_upgrade_shell_args(version);
    run_shell_script(&install_args)?;
    Ok(())
}

fn should_run_runtime_upgrade() -> bool {
    cfg!(target_os = "linux") && effective_uid_is_root()
}

#[cfg(unix)]
fn effective_uid_is_root() -> bool {
    Uid::effective().as_raw() == 0
}

#[cfg(not(unix))]
fn effective_uid_is_root() -> bool {
    false
}

fn build_runtime_upgrade_shell_args(version: &str, config: &Config) -> Vec<String> {
    let mut script = format!(
        "set -euo pipefail
export ZODEX_INSTALL_MODE=runtime
export ZODEX_VERSION={}
",
        shell_escape_single_quotes(version)
    );

    if version != "latest" {
        script.push_str(&format!(
            "export ZODEX_SOURCE_REF={}
",
            shell_escape_single_quotes(version)
        ));
    }

    if let Some(port) = config.http_bind_port {
        script.push_str(&format!(
            "export ZODEX_HTTP_BIND_PORT={port}
"
        ));
    }

    script.push_str("curl -fsSL 'https://zodex.ashray.xyz/install.sh' | bash");
    vec!["-lc".to_string(), script]
}

fn build_operator_upgrade_shell_args(version: &str) -> Vec<String> {
    let script = format!(
        "set -euo pipefail
export ZODEX_INSTALL_MODE=operator
export ZODEX_VERSION={}
curl -fsSL 'https://zodex.ashray.xyz/install.sh' | sh",
        shell_escape_single_quotes(version)
    );
    vec!["-lc".to_string(), script]
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
    fs::create_dir_all(PUSH_GRANTS_DIR)
        .with_context(|| format!("failed to create {PUSH_GRANTS_DIR}"))?;
    Ok(())
}

#[cfg(unix)]
fn ensure_push_grants_dir_permissions(config: &Config) -> Result<()> {
    let grants_dir = Path::new(PUSH_GRANTS_DIR);
    fs::create_dir_all(grants_dir)
        .with_context(|| format!("failed to create {PUSH_GRANTS_DIR}"))?;
    if !current_euid_is_root() {
        return Ok(());
    }

    let agent_user = lookup_user(&config.agent_user)?;
    let service_group = lookup_group(&config.service_group)?;
    chown(grants_dir, Some(agent_user.uid), Some(service_group.gid))
        .with_context(|| format!("failed to chown {}", grants_dir.display()))?;
    set_file_mode(grants_dir, 0o750)?;
    Ok(())
}

#[cfg(not(unix))]
fn ensure_push_grants_dir_permissions(_config: &Config) -> Result<()> {
    fs::create_dir_all(PUSH_GRANTS_DIR)
        .with_context(|| format!("failed to create {PUSH_GRANTS_DIR}"))?;
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
        candidates.push(parent.join("zodexd"));
    }
    candidates.push(PathBuf::from("/usr/local/bin/zodexd"));
    candidates.push(PathBuf::from("/usr/bin/zodexd"));

    candidates
        .into_iter()
        .find(|path| path.exists())
        .ok_or_else(|| anyhow!("failed to locate {PRIMARY_DAEMON_BINARY} binary"))
}

fn resolve_publisher_daemon_binary_path() -> Result<PathBuf> {
    if let Ok(override_path) = std::env::var("ZODEX_PRD_PATH") {
        let path = PathBuf::from(&override_path);
        if path.exists() {
            return Ok(path);
        }
        bail!("ZODEX_PRD_PATH does not exist: {override_path}");
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

