use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::serve;
use axum_server::Handle;
use axum_server::tls_rustls::RustlsConfig;
use computer_mcp::config::Config;
use computer_mcp::http_api::build_http_api_router;
use computer_mcp::protocol::{
    ApplyPatchOutput, CommandStatus, ExecCommandInput, TerminationReason, ToolOutput,
    WriteStdinInput,
};
use computer_mcp::service::ComputerService;
use rcgen::generate_simple_self_signed;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tempfile::tempdir;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

fn test_config(api_key: &str) -> Arc<Config> {
    Arc::new(Config {
        api_key: api_key.to_string(),
        ..Config::default()
    })
}

fn assert_running_session_shape(output: &ToolOutput) {
    assert_eq!(output.status, CommandStatus::Running);
    assert!(output.session_id.is_some());
    let handle = output
        .session_handle
        .as_deref()
        .expect("running output should have a session handle");
    assert_eq!(handle.len(), 8);
    assert!(handle.chars().all(|ch| ch.is_ascii_alphanumeric()));
    assert!(output.exit_code.is_none());
    assert!(output.termination_reason.is_none());
}

async fn start_http_api(config: Arc<Config>) -> (SocketAddr, oneshot::Sender<()>, JoinHandle<()>) {
    computer_mcp::install_rustls_crypto_provider();

    let app = build_http_api_router(config.clone(), ComputerService::new(config));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind listener");
    let addr = listener.local_addr().expect("local addr");
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let server = tokio::spawn(async move {
        serve(listener, app.into_make_service())
            .with_graceful_shutdown(async move {
                let _ = shutdown_rx.await;
            })
            .await
            .expect("server should run");
    });

    (addr, shutdown_tx, server)
}

async fn start_https_api(config: Arc<Config>) -> (SocketAddr, oneshot::Sender<()>, JoinHandle<()>) {
    computer_mcp::install_rustls_crypto_provider();

    let app = build_http_api_router(config.clone(), ComputerService::new(config));
    let cert = generate_simple_self_signed(vec!["127.0.0.1".to_string()])
        .expect("self-signed cert should generate");
    let rustls = RustlsConfig::from_pem(
        cert.cert.pem().into_bytes(),
        cert.signing_key.serialize_pem().into_bytes(),
    )
    .await
    .expect("rustls config should build");

    let probe = std::net::TcpListener::bind("127.0.0.1:0").expect("bind probe listener");
    let addr = probe.local_addr().expect("probe addr");
    drop(probe);

    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let handle = Handle::new();
    let shutdown_handle = handle.clone();
    tokio::spawn(async move {
        let _ = shutdown_rx.await;
        shutdown_handle.graceful_shutdown(Some(Duration::from_secs(0)));
    });

    let server = tokio::spawn(async move {
        axum_server::bind_rustls(addr, rustls)
            .handle(handle)
            .serve(app.into_make_service())
            .await
            .expect("https server should run");
    });

    (addr, shutdown_tx, server)
}

async fn run_computer_cli_json<T: DeserializeOwned>(args: Vec<String>) -> T {
    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_computer"))
        .args(&args)
        .output()
        .await
        .expect("computer CLI should execute");

    assert!(
        output.status.success(),
        "computer CLI failed\nargs: {:?}\nstatus: {:?}\nstdout:\n{}\nstderr:\n{}",
        args,
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    serde_json::from_slice(&output.stdout).expect("computer CLI stdout should be valid json")
}

async fn post_http_json<T: DeserializeOwned>(
    base_url: &str,
    api_key: &str,
    path: &str,
    body: Value,
) -> T {
    let response = reqwest::Client::new()
        .post(format!("{base_url}{path}"))
        .bearer_auth(api_key)
        .json(&body)
        .send()
        .await
        .expect("request should succeed");

    let status = response.status();
    let bytes = response
        .bytes()
        .await
        .expect("response bytes should be readable");
    assert!(
        status.is_success(),
        "http request failed with status {status}: {}",
        String::from_utf8_lossy(&bytes)
    );
    serde_json::from_slice(&bytes).expect("response should be valid json")
}

async fn stop_http_api(shutdown_tx: oneshot::Sender<()>, server: JoinHandle<()>) {
    let _ = shutdown_tx.send(());
    server.await.expect("server join should succeed");
}

#[tokio::test]
async fn phase6_exec_command_cli_handles_self_signed_https_daemon() {
    let api_key = "phase6-https-key";
    let config = test_config(api_key);
    let (addr, shutdown_tx, server) = start_https_api(config).await;
    let base_url = format!("https://{addr}");

    let cli_output: ToolOutput = run_computer_cli_json(vec![
        "--url".to_string(),
        base_url,
        "--key".to_string(),
        api_key.to_string(),
        "exec-command".to_string(),
        "printf 'phase6-https\\n'".to_string(),
        "--yield-time-ms".to_string(),
        "2000".to_string(),
    ])
    .await;

    assert_eq!(cli_output.status, CommandStatus::Exited);
    assert_eq!(cli_output.exit_code, Some(0));
    assert!(cli_output.output.contains("phase6-https"));

    stop_http_api(shutdown_tx, server).await;
}

#[tokio::test]
async fn phase6_exec_command_parity_service_http_and_cli() {
    let api_key = "phase6-exec-key";
    let config = test_config(api_key);
    let direct_service = ComputerService::new(config.clone());
    let (addr, shutdown_tx, server) = start_http_api(config).await;
    let base_url = format!("http://{addr}");
    let cmd = "printf 'phase6-exec\\n'";

    let direct_output = direct_service
        .exec_command(ExecCommandInput {
            cmd: cmd.to_string(),
            yield_time_ms: Some(2_000),
            workdir: None,
            timeout_ms: None,
        })
        .await
        .expect("direct exec should succeed");
    let http_output: ToolOutput = post_http_json(
        &base_url,
        api_key,
        "/v1/exec-command",
        json!({
            "cmd": cmd,
            "yield_time_ms": 2_000
        }),
    )
    .await;
    let cli_output: ToolOutput = run_computer_cli_json(vec![
        "--url".to_string(),
        base_url.clone(),
        "--key".to_string(),
        api_key.to_string(),
        "exec-command".to_string(),
        cmd.to_string(),
        "--yield-time-ms".to_string(),
        "2000".to_string(),
    ])
    .await;

    assert_eq!(direct_output.status, CommandStatus::Exited);
    assert_eq!(http_output.status, direct_output.status);
    assert_eq!(cli_output.status, direct_output.status);
    assert_eq!(http_output.exit_code, direct_output.exit_code);
    assert_eq!(cli_output.exit_code, direct_output.exit_code);
    assert!(direct_output.output.contains("phase6-exec"));
    assert!(http_output.output.contains("phase6-exec"));
    assert!(cli_output.output.contains("phase6-exec"));

    stop_http_api(shutdown_tx, server).await;
}

#[tokio::test]
async fn phase6_write_stdin_parity_service_http_and_cli() {
    let api_key = "phase6-write-key";
    let config = test_config(api_key);
    let direct_service = ComputerService::new(config.clone());
    let (addr, shutdown_tx, server) = start_http_api(config).await;
    let base_url = format!("http://{addr}");
    let start_shell = "bash --noprofile --norc";
    let marker = "phase6-write";

    let direct_started = direct_service
        .exec_command(ExecCommandInput {
            cmd: start_shell.to_string(),
            yield_time_ms: Some(50),
            workdir: None,
            timeout_ms: Some(60_000),
        })
        .await
        .expect("direct shell should start");
    assert_running_session_shape(&direct_started);
    let direct_handle = direct_started
        .session_handle
        .expect("direct session handle");
    let direct_written = direct_service
        .write_stdin(WriteStdinInput {
            session_handle: direct_handle.clone(),
            chars: Some(format!("echo {marker}\n")),
            yield_time_ms: Some(500),
            kill_process: Some(false),
        })
        .await
        .expect("direct write should succeed");
    let direct_done = direct_service
        .write_stdin(WriteStdinInput {
            session_handle: direct_handle,
            chars: Some("exit\n".to_string()),
            yield_time_ms: Some(2_000),
            kill_process: Some(false),
        })
        .await
        .expect("direct exit should succeed");

    let http_started: ToolOutput = post_http_json(
        &base_url,
        api_key,
        "/v1/exec-command",
        json!({
            "cmd": start_shell,
            "yield_time_ms": 50,
            "timeout_ms": 60_000
        }),
    )
    .await;
    assert_running_session_shape(&http_started);
    let http_handle = http_started.session_handle.expect("http session handle");
    let http_written: ToolOutput = post_http_json(
        &base_url,
        api_key,
        "/v1/write-stdin",
        json!({
            "session_handle": http_handle,
            "chars": format!("echo {marker}\n"),
            "yield_time_ms": 500,
            "kill_process": false
        }),
    )
    .await;
    let http_done: ToolOutput = post_http_json(
        &base_url,
        api_key,
        "/v1/write-stdin",
        json!({
            "session_handle": http_handle,
            "chars": "exit\n",
            "yield_time_ms": 2_000,
            "kill_process": false
        }),
    )
    .await;

    let cli_started: ToolOutput = run_computer_cli_json(vec![
        "--url".to_string(),
        base_url.clone(),
        "--key".to_string(),
        api_key.to_string(),
        "exec-command".to_string(),
        start_shell.to_string(),
        "--yield-time-ms".to_string(),
        "50".to_string(),
        "--timeout-ms".to_string(),
        "60000".to_string(),
    ])
    .await;
    assert_running_session_shape(&cli_started);
    let cli_handle = cli_started.session_handle.expect("cli session handle");
    let cli_written: ToolOutput = run_computer_cli_json(vec![
        "--url".to_string(),
        base_url.clone(),
        "--key".to_string(),
        api_key.to_string(),
        "write-stdin".to_string(),
        "--session-handle".to_string(),
        cli_handle.clone(),
        "--chars".to_string(),
        format!("echo {marker}\n"),
        "--yield-time-ms".to_string(),
        "500".to_string(),
    ])
    .await;
    let cli_done: ToolOutput = run_computer_cli_json(vec![
        "--url".to_string(),
        base_url.clone(),
        "--key".to_string(),
        api_key.to_string(),
        "write-stdin".to_string(),
        "--session-handle".to_string(),
        cli_handle,
        "--chars".to_string(),
        "exit\n".to_string(),
        "--yield-time-ms".to_string(),
        "2000".to_string(),
    ])
    .await;

    assert_eq!(direct_written.status, CommandStatus::Running);
    assert_eq!(http_written.status, CommandStatus::Running);
    assert_eq!(cli_written.status, CommandStatus::Running);
    assert!(direct_written.output.contains(marker));
    assert!(http_written.output.contains(marker));
    assert!(cli_written.output.contains(marker));

    assert_eq!(direct_done.status, CommandStatus::Exited);
    assert_eq!(http_done.status, CommandStatus::Exited);
    assert_eq!(cli_done.status, CommandStatus::Exited);
    assert_eq!(http_done.exit_code, direct_done.exit_code);
    assert_eq!(cli_done.exit_code, direct_done.exit_code);
    assert!(http_done.session_handle.is_none());
    assert!(cli_done.session_handle.is_none());

    stop_http_api(shutdown_tx, server).await;
}

#[tokio::test]
async fn phase2_kill_process_parity_service_http_and_cli() {
    let api_key = "phase2-kill-key";
    let config = test_config(api_key);
    let direct_service = ComputerService::new(config.clone());
    let (addr, shutdown_tx, server) = start_http_api(config).await;
    let base_url = format!("http://{addr}");
    let start_cmd = "sleep 30";

    let direct_started = direct_service
        .exec_command(ExecCommandInput {
            cmd: start_cmd.to_string(),
            yield_time_ms: Some(50),
            workdir: None,
            timeout_ms: Some(60_000),
        })
        .await
        .expect("direct sleep should start");
    assert_running_session_shape(&direct_started);
    let direct_handle = direct_started
        .session_handle
        .expect("direct session handle");

    let http_started: ToolOutput = post_http_json(
        &base_url,
        api_key,
        "/v1/exec-command",
        json!({
            "cmd": start_cmd,
            "yield_time_ms": 50,
            "timeout_ms": 60_000
        }),
    )
    .await;
    assert_running_session_shape(&http_started);
    let http_handle = http_started.session_handle.expect("http session handle");

    let cli_started: ToolOutput = run_computer_cli_json(vec![
        "--url".to_string(),
        base_url.clone(),
        "--key".to_string(),
        api_key.to_string(),
        "exec-command".to_string(),
        start_cmd.to_string(),
        "--yield-time-ms".to_string(),
        "50".to_string(),
        "--timeout-ms".to_string(),
        "60000".to_string(),
    ])
    .await;
    assert_running_session_shape(&cli_started);
    let cli_handle = cli_started.session_handle.expect("cli session handle");

    let direct_killed = direct_service
        .write_stdin(WriteStdinInput {
            session_handle: direct_handle,
            chars: Some("echo ignored-direct\n".to_string()),
            yield_time_ms: Some(6_000),
            kill_process: Some(true),
        })
        .await
        .expect("direct kill should succeed");
    let http_killed: ToolOutput = post_http_json(
        &base_url,
        api_key,
        "/v1/write-stdin",
        json!({
            "session_handle": http_handle,
            "chars": "echo ignored-http\n",
            "yield_time_ms": 6_000,
            "kill_process": true
        }),
    )
    .await;
    let cli_killed: ToolOutput = run_computer_cli_json(vec![
        "--url".to_string(),
        base_url.clone(),
        "--key".to_string(),
        api_key.to_string(),
        "write-stdin".to_string(),
        "--session-handle".to_string(),
        cli_handle,
        "--chars".to_string(),
        "echo ignored-cli\n".to_string(),
        "--yield-time-ms".to_string(),
        "6000".to_string(),
        "--kill-process".to_string(),
    ])
    .await;

    for output in [&direct_killed, &http_killed, &cli_killed] {
        assert_eq!(output.status, CommandStatus::Exited);
        assert!(output.session_handle.is_none());
        assert!(output.exit_code.is_some());
        assert_eq!(output.termination_reason, Some(TerminationReason::Killed));
        assert!(output.output.contains("terminated by kill_process"));
    }
    assert!(!direct_killed.output.contains("ignored-direct"));
    assert!(!http_killed.output.contains("ignored-http"));
    assert!(!cli_killed.output.contains("ignored-cli"));

    stop_http_api(shutdown_tx, server).await;
}

#[tokio::test]
async fn phase2_timeout_parity_service_http_and_cli() {
    let api_key = "phase2-timeout-key";
    let config = Arc::new(Config {
        api_key: api_key.to_string(),
        default_exec_timeout_ms: 1_000,
        max_exec_timeout_ms: 1_000,
        ..Config::default()
    });
    let direct_service = ComputerService::new(config.clone());
    let (addr, shutdown_tx, server) = start_http_api(config).await;
    let base_url = format!("http://{addr}");

    let direct_timed_out = direct_service
        .exec_command(ExecCommandInput {
            cmd: "sleep 30".to_string(),
            yield_time_ms: Some(2_500),
            workdir: None,
            timeout_ms: Some(1_000),
        })
        .await
        .expect("direct timeout should complete");
    let http_timed_out: ToolOutput = post_http_json(
        &base_url,
        api_key,
        "/v1/exec-command",
        json!({
            "cmd": "sleep 30",
            "yield_time_ms": 2_500,
            "timeout_ms": 1_000
        }),
    )
    .await;
    let cli_timed_out: ToolOutput = run_computer_cli_json(vec![
        "--url".to_string(),
        base_url.clone(),
        "--key".to_string(),
        api_key.to_string(),
        "exec-command".to_string(),
        "sleep 30".to_string(),
        "--yield-time-ms".to_string(),
        "2500".to_string(),
        "--timeout-ms".to_string(),
        "1000".to_string(),
    ])
    .await;

    for output in [&direct_timed_out, &http_timed_out, &cli_timed_out] {
        assert_eq!(output.status, CommandStatus::Exited);
        assert!(output.session_handle.is_none());
        assert!(output.exit_code.is_some());
        assert_eq!(output.termination_reason, Some(TerminationReason::Timeout));
        assert!(
            output
                .output
                .contains("process timed out and was terminated")
        );
    }

    stop_http_api(shutdown_tx, server).await;
}

#[tokio::test]
async fn phase2_cwd_and_truncation_parity_service_http_and_cli() {
    let api_key = "phase2-cwd-key";
    let config = Arc::new(Config {
        api_key: api_key.to_string(),
        max_output_chars: 80,
        ..Config::default()
    });
    let direct_service = ComputerService::new(config.clone());
    let (addr, shutdown_tx, server) = start_http_api(config).await;
    let base_url = format!("http://{addr}");
    let workdir = tempdir().expect("workdir tempdir");
    let workdir_str = workdir.path().to_string_lossy().to_string();

    let direct_cwd = direct_service
        .exec_command(ExecCommandInput {
            cmd: "pwd".to_string(),
            yield_time_ms: Some(2_000),
            workdir: Some(workdir_str.clone()),
            timeout_ms: None,
        })
        .await
        .expect("direct cwd should succeed");
    let http_cwd: ToolOutput = post_http_json(
        &base_url,
        api_key,
        "/v1/exec-command",
        json!({
            "cmd": "pwd",
            "yield_time_ms": 2_000,
            "workdir": workdir_str.clone()
        }),
    )
    .await;
    let cli_cwd: ToolOutput = run_computer_cli_json(vec![
        "--url".to_string(),
        base_url.clone(),
        "--key".to_string(),
        api_key.to_string(),
        "exec-command".to_string(),
        "pwd".to_string(),
        "--yield-time-ms".to_string(),
        "2000".to_string(),
        "--workdir".to_string(),
        workdir.path().to_string_lossy().to_string(),
    ])
    .await;

    for output in [&direct_cwd, &http_cwd, &cli_cwd] {
        assert_eq!(output.status, CommandStatus::Exited);
        assert_eq!(output.cwd, workdir.path().to_string_lossy().as_ref());
        assert!(
            output
                .output
                .contains(workdir.path().to_string_lossy().as_ref())
        );
    }

    let truncation_cmd = "python3 -c 'print(\"x\" * 200)'";
    let direct_truncated = direct_service
        .exec_command(ExecCommandInput {
            cmd: truncation_cmd.to_string(),
            yield_time_ms: Some(5_000),
            workdir: None,
            timeout_ms: None,
        })
        .await
        .expect("direct truncation should succeed");
    let http_truncated: ToolOutput = post_http_json(
        &base_url,
        api_key,
        "/v1/exec-command",
        json!({
            "cmd": truncation_cmd,
            "yield_time_ms": 5_000
        }),
    )
    .await;
    let cli_truncated: ToolOutput = run_computer_cli_json(vec![
        "--url".to_string(),
        base_url,
        "--key".to_string(),
        api_key.to_string(),
        "exec-command".to_string(),
        truncation_cmd.to_string(),
        "--yield-time-ms".to_string(),
        "5000".to_string(),
    ])
    .await;

    for output in [&direct_truncated, &http_truncated, &cli_truncated] {
        assert_eq!(output.status, CommandStatus::Exited);
        assert!(output.output.contains("bytes truncated"));
        assert!(output.output.contains(&"x".repeat(20)));
    }

    stop_http_api(shutdown_tx, server).await;
}

#[tokio::test]
async fn phase6_apply_patch_parity_service_http_and_cli_relative_paths() {
    let api_key = "phase6-patch-key";
    let config = test_config(api_key);
    let direct_service = ComputerService::new(config.clone());
    let (addr, shutdown_tx, server) = start_http_api(config).await;
    let base_url = format!("http://{addr}");
    let direct_dir = tempdir().expect("direct tempdir");
    let http_dir = tempdir().expect("http tempdir");
    let cli_dir = tempdir().expect("cli tempdir");
    let relative_file = "nested/phase6.txt";
    let patch = "*** Begin Patch\n*** Add File: nested/phase6.txt\n+phase6-patch\n*** End Patch\n";

    let direct_output = direct_service
        .apply_patch(computer_mcp::protocol::ApplyPatchInput {
            patch: patch.to_string(),
            workdir: direct_dir.path().to_string_lossy().to_string(),
        })
        .expect("direct patch should succeed");
    let http_output: ApplyPatchOutput = post_http_json(
        &base_url,
        api_key,
        "/v1/apply-patch",
        json!({
            "patch": patch,
            "workdir": http_dir.path()
        }),
    )
    .await;
    let cli_output: ApplyPatchOutput = run_computer_cli_json(vec![
        "--url".to_string(),
        base_url.clone(),
        "--key".to_string(),
        api_key.to_string(),
        "apply-patch".to_string(),
        "--patch".to_string(),
        patch.to_string(),
        "--workdir".to_string(),
        cli_dir.path().to_string_lossy().to_string(),
    ])
    .await;

    assert!(direct_output.contains("Success. Updated the following files:"));
    assert!(
        http_output
            .output
            .contains("Success. Updated the following files:")
    );
    assert!(
        cli_output
            .output
            .contains("Success. Updated the following files:")
    );

    assert_eq!(
        std::fs::read_to_string(direct_dir.path().join(relative_file))
            .expect("direct patched file should be readable"),
        "phase6-patch\n"
    );
    assert_eq!(
        std::fs::read_to_string(http_dir.path().join(relative_file))
            .expect("http patched file should be readable"),
        "phase6-patch\n"
    );
    assert_eq!(
        std::fs::read_to_string(cli_dir.path().join(relative_file))
            .expect("cli patched file should be readable"),
        "phase6-patch\n"
    );

    stop_http_api(shutdown_tx, server).await;
}
