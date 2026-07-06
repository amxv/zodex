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

