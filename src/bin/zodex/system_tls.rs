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
    run_command_capture_with(program, args, None)
}

fn run_command_capture_with(program: &str, args: &[String], cwd: Option<&Path>) -> Result<String> {
    let mut command = Command::new(program);
    command.args(args);
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }

    let output = command
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
    format!("zodex-{}", ip.to_string().replace(['.', ':'], "-"))
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

