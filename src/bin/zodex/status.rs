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
            "hint: register Sprite Services with `zodex sprite sync --sprite {sprite}`"
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
                "hint: re-sync with `zodex sprite sync --sprite {sprite}`"
            ));
        }
    }

    if status != "running" {
        lines.push(format!(
                    "hint: inspect logs with `{PRIMARY_OPERATOR_BINARY} sprite logs --sprite {sprite} --service {service_name}`"
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
                    "zodex-publisher".to_string(),
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
                    "zodex-agent".to_string(),
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
        "service: zodex-reader".to_string(),
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
        format!(
            "allowed-installation-accounts: {}",
            config.publisher_installations.len()
        ),
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
    if config.publisher_targets.is_empty() && config.publisher_installations.is_empty() {
        lines.push(
            "hint: add at least one `publisher_targets` or `publisher_installations` entry to config"
                .to_string(),
        );
    }
    lines.extend(sprite_runtime_note_lines());

    Ok(lines)
}

fn render_systemd_unit(daemon_path: &Path, config_path: &Path) -> String {
    let daemon_arg = quote_unit_arg(daemon_path);
    let config_arg = quote_unit_arg(config_path);

    format!(
        "[Unit]
Description=zodex daemon
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
ExecStart={daemon_arg} --config {config_arg}
Restart=always
RestartSec=2
NoNewPrivileges=true
Environment=RUST_LOG=zodex=info,zodexd=info

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

