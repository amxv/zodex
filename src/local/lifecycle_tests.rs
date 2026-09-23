use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
#[cfg(target_os = "linux")]
use std::process::Stdio;
#[cfg(target_os = "linux")]
use std::sync::atomic::AtomicU64;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tempfile::tempdir;
use time::OffsetDateTime;

use super::lifecycle::{
    LOCAL_RUNTIME_BOOTSTRAP_SCHEMA_VERSION, LocalRuntimeBootstrap, cleanup_stale_runtime,
    is_expired, prepare_local_launch_at, wait_for_runtime_ready,
};
use super::lifecycle_context::{resolve_developer_shell, start_directory_error};
use super::lifecycle_start::{start_via_launchd, start_via_launchd_with_timeout};
use super::{
    LOCAL_DISCOVERY_SCHEMA_VERSION, LOCAL_RUNTIME_STATE_SCHEMA_VERSION, LaunchdController,
    LocalObservabilityDiscovery, LocalPaths, LocalRuntimeDiscovery, LocalRuntimeHealth,
    LocalRuntimeLifecycle, LocalRuntimeState, load_runtime_state, write_runtime_discovery,
    write_runtime_state,
};
#[cfg(target_os = "linux")]
use super::{
    LocalConfig, LocalProcessRegistryDocument, ManagedTunnelClientRelease, RuntimeKey,
    active_process_record_count, ensure_observability_bearer, paths_from_runtime_bootstrap,
    prepare_local_launch, run_hidden_runtime,
};
#[cfg(target_os = "linux")]
use crate::session::identity_matches;
use crate::session::{ProcessInspector, SystemProcessInspector};

#[test]
fn launch_artifacts_keep_environment_out_of_plist_and_bootstrap() {
    let dir = tempdir().unwrap();
    let paths = test_paths(dir.path());
    let repo = dir.path().join("repo");
    fs::create_dir(&repo).unwrap();
    let environment = vec![
        (OsString::from("HOME"), OsString::from("/Users/example")),
        (
            OsString::from("PATH"),
            OsString::from("/secret/toolchain/path"),
        ),
        (OsString::from("SHELL"), OsString::from("/bin/sh")),
        (OsString::from("MY_SECRET"), OsString::from("top-secret")),
    ];
    let started = OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap();
    let prepared = prepare_local_launch_at(
        &paths,
        Path::new("/usr/local/bin/zodex"),
        &repo,
        Some(4 * 60 * 60),
        &environment,
        started,
        "runtime-fixture".to_string(),
    )
    .unwrap();
    let bootstrap = fs::read_to_string(&prepared.bootstrap_path).unwrap();
    #[cfg(target_os = "macos")]
    {
        let plist = fs::read_to_string(&prepared.plist_path).unwrap();
        assert!(!plist.contains("MY_SECRET"));
        assert!(!plist.contains("top-secret"));
        assert!(!plist.contains("/secret/toolchain/path"));
    }
    #[cfg(not(target_os = "macos"))]
    assert!(!prepared.plist_path.exists());
    assert!(!bootstrap.contains("MY_SECRET"));
    assert!(!bootstrap.contains("top-secret"));
    assert!(paths.environment_handoff_file().exists());
    assert_eq!(prepared.expires_at.as_deref(), Some("2023-11-15T02:13:20Z"));
    let state = load_runtime_state(&paths).unwrap().unwrap();
    assert_eq!(state.runtime_id, "runtime-fixture");
    assert!(state.process.is_none());
}

#[test]
fn bootstrap_schema_and_timestamps_are_stable() {
    let bootstrap = LocalRuntimeBootstrap {
        schema_version: LOCAL_RUNTIME_BOOTSTRAP_SCHEMA_VERSION,
        runtime_id: "runtime-fixture".to_string(),
        config_root: "/tmp/config".into(),
        data_root: "/tmp/data".into(),
        state_root: "/tmp/state".into(),
        start_directory: "/tmp/repo".into(),
        environment_handoff_path: "/tmp/runtime/environment.json".into(),
        started_at: "2026-08-16T00:00:00Z".to_string(),
        expires_at: Some("2026-08-16T04:00:00Z".to_string()),
    };
    let json = serde_json::to_value(bootstrap).unwrap();
    assert_eq!(json["schema_version"], 1);
    assert_eq!(json["expires_at"], "2026-08-16T04:00:00Z");
    assert!(json.get("agent_id").is_none());
}

#[test]
fn runtime_shell_comes_from_captured_environment() {
    let environment = vec![
        (OsString::from("HOME"), OsString::from("/tmp")),
        (OsString::from("PATH"), OsString::from("/usr/bin:/bin")),
        (OsString::from("SHELL"), OsString::from("/bin/sh")),
    ];
    assert_eq!(
        resolve_developer_shell(&environment).unwrap(),
        PathBuf::from("/bin/sh")
    );
}

#[cfg(target_os = "macos")]
#[test]
fn permission_denial_has_actionable_macos_privacy_hint() {
    let error = start_directory_error(
        Path::new("/Users/example/Documents/project"),
        "read",
        std::io::Error::from(std::io::ErrorKind::PermissionDenied),
    );
    let message = error.to_string();
    assert!(message.contains("Privacy & Security"));
    assert!(message.contains("Files & Folders"));
    assert!(message.contains("Full Disk Access"));
}

#[cfg(not(target_os = "macos"))]
#[test]
fn permission_denial_has_actionable_platform_hint() {
    let error = start_directory_error(
        Path::new("/example/project"),
        "read",
        std::io::Error::from(std::io::ErrorKind::PermissionDenied),
    );
    let message = error.to_string();
    assert!(message.contains("operating system denied permission"));
    assert!(message.contains("Grant the Zodex process access to this workspace"));
    assert!(!message.contains("Privacy & Security"));
}

#[test]
fn absolute_ttl_reconciliation_does_not_extend_with_activity() {
    let expiry = "2026-08-16T04:00:00Z";
    let before = OffsetDateTime::parse(
        "2026-08-16T03:59:59Z",
        &time::format_description::well_known::Rfc3339,
    )
    .unwrap();
    let at = OffsetDateTime::parse(expiry, &time::format_description::well_known::Rfc3339).unwrap();
    assert!(!is_expired(Some(expiry), before).unwrap());
    assert!(is_expired(Some(expiry), at).unwrap());
    assert!(!is_expired(None, at).unwrap());
}

#[tokio::test]
async fn already_ready_start_is_non_mutating_and_reports_existing_runtime() {
    let dir = tempdir().unwrap();
    let paths = test_paths(dir.path());
    let repo = dir.path().join("repo");
    fs::create_dir(&repo).unwrap();
    let inspector = SystemProcessInspector;
    let process = inspector
        .identity(std::process::id() as i32)
        .unwrap()
        .unwrap();
    write_runtime_state(
        &paths,
        &LocalRuntimeState {
            schema_version: LOCAL_RUNTIME_STATE_SCHEMA_VERSION,
            runtime_id: "runtime-existing".to_string(),
            lifecycle: LocalRuntimeLifecycle::Ready,
            process: Some(process),
            start_directory: Some(repo.clone()),
            started_at: Some("2026-08-16T00:00:00Z".to_string()),
            expires_at: Some("2026-08-16T04:00:00Z".to_string()),
            health: fully_ready_health(),
        },
    )
    .unwrap();
    let discovery = LocalRuntimeDiscovery {
        schema_version: LOCAL_DISCOVERY_SCHEMA_VERSION,
        runtime_id: "runtime-existing".to_string(),
        pid: std::process::id(),
        start_directory: repo.clone(),
        started_at: "2026-08-16T00:00:00Z".to_string(),
        expires_at: Some("2026-08-16T04:00:00Z".to_string()),
        observability: LocalObservabilityDiscovery::active(
            "http://127.0.0.1:43123",
            paths.observability_bearer_file(),
        ),
    };
    write_runtime_discovery(&paths, &discovery).unwrap();
    let state_before = fs::read(paths.runtime_state_file()).unwrap();
    let discovery_before = fs::read(paths.discovery_file()).unwrap();
    let launchd = FakeLaunchd::default();
    let outcome = start_via_launchd(
        &paths,
        Path::new("/usr/local/bin/zodex"),
        &repo,
        Some(60),
        &[(OsString::from("SHOULD_NOT"), OsString::from("be-written"))],
        &launchd,
    )
    .await
    .unwrap();

    assert!(outcome.already_running);
    assert_eq!(outcome.discovery, discovery);
    assert_eq!(launchd.bootstrap_calls(), 0);
    assert_eq!(launchd.bootout_calls(), 0);
    assert!(!paths.environment_handoff_file().exists());
    assert_eq!(fs::read(paths.runtime_state_file()).unwrap(), state_before);
    assert_eq!(fs::read(paths.discovery_file()).unwrap(), discovery_before);
}

#[test]
fn two_concurrent_cold_starts_converge_to_one_runtime_and_one_launchd_bootstrap() {
    let dir = tempdir().unwrap();
    let paths = test_paths(dir.path());
    let repo = dir.path().join("repo");
    fs::create_dir(&repo).unwrap();
    let launchd = Arc::new(ReadyOnBootstrapLaunchd::new(paths.clone()));
    let environment = vec![
        (OsString::from("HOME"), OsString::from(dir.path())),
        (OsString::from("PATH"), OsString::from("/usr/bin:/bin")),
        (OsString::from("SHELL"), OsString::from("/bin/sh")),
    ];

    let mut threads = Vec::new();
    for _ in 0..2 {
        let paths = paths.clone();
        let repo = repo.clone();
        let launchd = launchd.clone();
        let environment = environment.clone();
        threads.push(std::thread::spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(start_via_launchd(
                    &paths,
                    Path::new("/usr/local/bin/zodex"),
                    &repo,
                    Some(60),
                    &environment,
                    launchd.as_ref(),
                ))
                .unwrap()
        }));
    }
    let first = threads.remove(0).join().unwrap();
    let second = threads.remove(0).join().unwrap();

    assert_eq!(first.discovery.runtime_id, second.discovery.runtime_id);
    assert_ne!(first.already_running, second.already_running);
    assert_eq!(launchd.bootstrap_calls(), 1);
    assert!(!paths.environment_handoff_file().exists());
    assert!(paths.lifecycle_lock_file().exists());
}

#[tokio::test]
async fn start_retries_once_when_launchd_never_publishes_a_process() {
    let dir = tempdir().unwrap();
    let paths = test_paths(dir.path());
    let repo = dir.path().join("repo");
    fs::create_dir(&repo).unwrap();
    let launchd = ReadyOnBootstrapLaunchd::after_attempts(paths.clone(), 2);
    let outcome = start_via_launchd_with_timeout(
        &paths,
        Path::new("/usr/local/bin/zodex"),
        &repo,
        None,
        &[],
        &launchd,
        Duration::from_millis(20),
    )
    .await
    .unwrap();

    assert!(!outcome.already_running);
    assert_eq!(launchd.bootstrap_calls(), 2);
    assert_eq!(
        outcome.discovery.start_directory,
        fs::canonicalize(repo).unwrap()
    );
    assert!(!paths.environment_handoff_file().exists());
}

#[tokio::test]
async fn runtime_ready_wait_requires_every_composite_health_boundary() {
    let dir = tempdir().unwrap();
    let paths = test_paths(dir.path());
    let repo = dir.path().join("repo");
    fs::create_dir(&repo).unwrap();
    let inspector = SystemProcessInspector;
    let process = inspector
        .identity(std::process::id() as i32)
        .unwrap()
        .unwrap();
    let mut state = LocalRuntimeState {
        schema_version: LOCAL_RUNTIME_STATE_SCHEMA_VERSION,
        runtime_id: "runtime-ready-gate".to_string(),
        lifecycle: LocalRuntimeLifecycle::Ready,
        process: Some(process),
        start_directory: Some(repo.clone()),
        started_at: Some("2026-08-16T00:00:00Z".to_string()),
        expires_at: None,
        health: fully_ready_health(),
    };
    state.health.tunnel_control_plane_ready = false;
    state.health.last_error = Some("control plane not ready".to_string());
    write_runtime_state(&paths, &state).unwrap();
    let discovery = LocalRuntimeDiscovery {
        schema_version: LOCAL_DISCOVERY_SCHEMA_VERSION,
        runtime_id: state.runtime_id.clone(),
        pid: std::process::id(),
        start_directory: repo,
        started_at: state.started_at.clone().unwrap(),
        expires_at: None,
        observability: LocalObservabilityDiscovery::active(
            "http://127.0.0.1:43123",
            paths.observability_bearer_file(),
        ),
    };
    write_runtime_discovery(&paths, &discovery).unwrap();
    let error = wait_for_runtime_ready(&paths, &state.runtime_id, Duration::from_millis(40))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("control plane not ready"));

    state.health = fully_ready_health();
    write_runtime_state(&paths, &state).unwrap();
    let ready = wait_for_runtime_ready(&paths, &state.runtime_id, Duration::from_secs(1))
        .await
        .unwrap();
    assert_eq!(ready.runtime_id, state.runtime_id);
}

#[test]
fn stale_cleanup_removes_only_disposable_runtime_artifacts() {
    let dir = tempdir().unwrap();
    let paths = test_paths(dir.path());
    paths.ensure_persistent_dirs().unwrap();
    fs::create_dir_all(paths.runtime_dir()).unwrap();
    fs::write(paths.environment_handoff_file(), "unconsumed").unwrap();
    fs::write(paths.history_dir().join("keep"), "history").unwrap();
    fs::write(paths.observability_bearer_file(), "stable-bearer").unwrap();
    fs::write(
        paths.liveboard_discovery_file(),
        "private-runtime-capability",
    )
    .unwrap();
    fs::write(paths.liveboard_preferences_file(), "durable-preferences").unwrap();
    let launchd = FakeLaunchd::loaded();

    cleanup_stale_runtime(&paths, &launchd).unwrap();

    assert!(!paths.runtime_dir().exists());
    assert_eq!(
        fs::read_to_string(paths.history_dir().join("keep")).unwrap(),
        "history"
    );
    assert_eq!(
        fs::read_to_string(paths.observability_bearer_file()).unwrap(),
        "stable-bearer"
    );
    assert_eq!(
        fs::read_to_string(paths.liveboard_preferences_file()).unwrap(),
        "durable-preferences"
    );
    assert_eq!(launchd.bootout_calls(), 1);
}

#[cfg(target_os = "linux")]
#[test]
fn hidden_runtime_child_entry() {
    if std::env::var_os("ZODEX_LIFECYCLE_TEST_CHILD").is_none() {
        return;
    }
    let bootstrap = PathBuf::from(
        std::env::var_os("ZODEX_LIFECYCLE_TEST_BOOTSTRAP").expect("child bootstrap path"),
    );
    let paths = paths_from_runtime_bootstrap(&bootstrap).unwrap();
    let runtime_key = RuntimeKey::new("lifecycle-fixture-runtime-key").unwrap();
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(run_hidden_runtime(paths, bootstrap, runtime_key))
        .unwrap();
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn hidden_runtime_child_reaches_one_runtime_multi_agent_readiness_and_ttl_shutdown() {
    use std::os::unix::fs::PermissionsExt as _;

    // This fixture exercises a real wall-clock TTL in a separate process. The
    // TTL is deliberately longer than the workload itself so default-parallel
    // `cargo test` scheduling cannot consume the access window before the
    // multi-Agent assertions run. The value is fixture pacing, not a shutdown
    // latency allowance: the test still proves the original absolute expiry is
    // unchanged by Agent/process activity and that all work ends at that same
    // runtime-wide deadline.
    const RUNTIME_TTL_SECONDS: u64 = 30;

    let dir = tempdir().unwrap();
    let paths = test_paths(dir.path());
    paths.ensure_persistent_dirs().unwrap();
    ensure_observability_bearer(&paths, false).unwrap();
    let home = dir.path().join("home");
    let repo = dir.path().join("repo");
    let worktree_a = repo.join("worktree-a");
    let worktree_b = repo.join("worktree-b");
    for path in [&home, &repo, &worktree_a, &worktree_b] {
        fs::create_dir_all(path).unwrap();
    }

    let fake_tunnel = dir.path().join("fake-tunnel-client");
    fs::write(
        &fake_tunnel,
        "#!/bin/sh\ncase \"$1\" in\n  run) exec /bin/sleep 60 ;;\n  health) printf '{\"live\":true,\"ready\":true}\\n'; exit 0 ;;\n  *) exit 2 ;;\nesac\n",
    )
    .unwrap();
    fs::set_permissions(&fake_tunnel, fs::Permissions::from_mode(0o700)).unwrap();

    let mut config = LocalConfig::default();
    config
        .set("tunnel.id", "tunnel_0123456789abcdef0123456789abcdef")
        .unwrap();
    config.tunnel.client_path = Some(fake_tunnel);
    config.tunnel.release = Some(ManagedTunnelClientRelease {
        version: "lifecycle-fixture".to_string(),
        asset_name: "fixture".to_string(),
        archive_sha256: "a".repeat(64),
        binary_sha256: "b".repeat(64),
        cloudflared_sha256: "c".repeat(64),
        cloudflared_manifest_sha256: "d".repeat(64),
        source_url: "https://example.invalid/fixture".to_string(),
    });
    config.save(&paths.config_file()).unwrap();

    let environment = vec![
        (OsString::from("HOME"), home.into_os_string()),
        (OsString::from("PATH"), OsString::from("/usr/bin:/bin")),
        (OsString::from("SHELL"), OsString::from("/bin/sh")),
        (OsString::from("USER"), OsString::from("lifecycle-user")),
        (OsString::from("LOGNAME"), OsString::from("lifecycle-user")),
        (
            OsString::from("ZODEX_LIFECYCLE_CAPTURED_EXPORT"),
            OsString::from("captured-value"),
        ),
    ];
    let prepared = prepare_local_launch(
        &paths,
        &std::env::current_exe().unwrap(),
        &repo,
        Some(RUNTIME_TTL_SECONDS),
        &environment,
    )
    .unwrap();
    let original_expiry = prepared.expires_at.clone().expect("fixture TTL expiry");

    let mut child = tokio::process::Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg("local::lifecycle_tests::hidden_runtime_child_entry")
        .arg("--nocapture")
        .env("ZODEX_LIFECYCLE_TEST_CHILD", "1")
        .env("ZODEX_LIFECYCLE_TEST_BOOTSTRAP", &prepared.bootstrap_path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();

    let discovery =
        match wait_for_runtime_ready(&paths, &prepared.runtime_id, Duration::from_secs(5)).await {
            Ok(discovery) => discovery,
            Err(error) => {
                let _ = child.kill().await;
                let output = child.wait_with_output().await.unwrap();
                panic!(
                    "hidden child never became ready: {error:#}\nstdout={}\nstderr={}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
            }
        };
    assert_eq!(discovery.start_directory, fs::canonicalize(&repo).unwrap());
    assert!(!paths.environment_handoff_file().exists());

    let profile = fs::read_to_string(paths.tunnel_profile_file()).unwrap();
    let mcp_url = profile
        .lines()
        .find_map(|line| {
            line.trim()
                .strip_prefix("url: ")
                .map(|value| serde_json::from_str::<String>(value).unwrap())
        })
        .expect("profile MCP URL");
    let token = fs::read_to_string(paths.mcp_token_file()).unwrap();

    let release = dir.path().join("release-slow-agent");
    let slow_started = call_exec(
        &mcp_url,
        &token,
        "lifecycle-session-a",
        &worktree_a,
        &format!(
            "while [ ! -f {} ]; do sleep 0.02; done; printf 'slow:%s:%s\\n' \"$ZODEX_LIFECYCLE_CAPTURED_EXPORT\" \"$PWD\"",
            shell_quote(release.as_os_str())
        ),
        100,
    )
    .await;
    assert!(slow_started.get("error").is_none(), "{slow_started}");
    assert!(
        slow_started.to_string().contains("running"),
        "{slow_started}"
    );
    let slow_handle = slow_started["result"]["structuredContent"]["session_handle"]
        .as_str()
        .expect("slow Agent command should return a running session handle")
        .to_string();

    let fast_result = call_exec(
        &mcp_url,
        &token,
        "lifecycle-session-b",
        &worktree_b,
        &format!(
            "printf 'fast:%s:%s\\n' \"$ZODEX_LIFECYCLE_CAPTURED_EXPORT\" \"$PWD\"; : > {}",
            shell_quote(release.as_os_str())
        ),
        10_000,
    )
    .await;
    assert!(fast_result.get("error").is_none(), "{fast_result}");
    assert!(fast_result.to_string().contains("captured-value"));
    assert!(fast_result.to_string().contains("worktree-b"));

    let slow_result = call_write_stdin(
        &mcp_url,
        &token,
        "lifecycle-session-a",
        &slow_handle,
        10_000,
    )
    .await;
    assert!(slow_result.get("error").is_none(), "{slow_result}");
    assert!(slow_result.to_string().contains("exited"), "{slow_result}");
    assert!(slow_result.to_string().contains("captured-value"));
    assert!(slow_result.to_string().contains("worktree-a"));

    for (session, workdir) in [
        ("lifecycle-session-a", &worktree_a),
        ("lifecycle-session-b", &worktree_b),
    ] {
        let result = call_exec(
            &mcp_url,
            &token,
            session,
            workdir,
            "trap '' TERM; sleep 60",
            100,
        )
        .await;
        assert!(result.get("error").is_none(), "{result}");
        assert!(result.to_string().contains("running"), "{result}");
    }

    let active_deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while active_process_record_count(&paths.owned_process_registry_file()).unwrap() != 2
        && tokio::time::Instant::now() < active_deadline
    {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        active_process_record_count(&paths.owned_process_registry_file()).unwrap(),
        2
    );
    let process_document: LocalProcessRegistryDocument =
        serde_json::from_slice(&fs::read(paths.owned_process_registry_file()).unwrap()).unwrap();
    assert_eq!(process_document.runtime_id, prepared.runtime_id);

    let agent_deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        let status = super::LocalStatusDocument::inspect(&paths).unwrap();
        if status.current_runtime_agent_count == 2 {
            assert_eq!(status.active_process_count, 2);
            assert_eq!(
                status
                    .runtime
                    .as_ref()
                    .and_then(|runtime| runtime.expires_at.as_deref()),
                Some(original_expiry.as_str())
            );
            break;
        }
        assert!(tokio::time::Instant::now() < agent_deadline);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // The coordinated stop closes admission/tunnel first, then allows the
    // configured 5s session TERM grace plus force-reap and the separately
    // bounded history-finalization window. Keep an outer assertion beyond
    // those product bounds so a real wedge still fails deterministically.
    let output = tokio::time::timeout(Duration::from_secs(48), child.wait_with_output())
        .await
        .expect("hidden runtime child must exit at absolute TTL")
        .unwrap();
    assert!(
        output.status.success(),
        "hidden child failed\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let inspector = SystemProcessInspector;
    for process in process_document.processes {
        assert!(
            !identity_matches(&inspector, &process.identity).unwrap(),
            "TTL shutdown left Local-owned process {} alive",
            process.identity.pid
        );
    }
    assert!(!paths.runtime_dir().exists());
    assert!(paths.history_database().exists());
    assert!(paths.observability_bearer_file().exists());
}

#[cfg(target_os = "linux")]
async fn call_exec(
    mcp_url: &str,
    token: &str,
    openai_session: &str,
    workdir: &Path,
    cmd: &str,
    yield_time_ms: u64,
) -> serde_json::Value {
    static NEXT_REQUEST_ID: AtomicU64 = AtomicU64::new(1_000);
    crate::install_rustls_crypto_provider();
    let response = reqwest::Client::new()
        .post(mcp_url)
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .header("MCP-Protocol-Version", "2026-07-28")
        .header("Mcp-Method", "tools/call")
        .header("Mcp-Name", "exec_command")
        .header(crate::server::LOCAL_MCP_TOKEN_HEADER, token)
        .json(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": NEXT_REQUEST_ID.fetch_add(1, Ordering::Relaxed),
            "method": "tools/call",
            "params": {
                "name": "exec_command",
                "arguments": {
                    "cmd": cmd,
                    "yield_time_ms": yield_time_ms,
                    "workdir": workdir,
                },
                "_meta": {
                    "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                    "io.modelcontextprotocol/clientInfo": {
                        "name": "zodex-lifecycle-child-test",
                        "version": "1.0"
                    },
                    "io.modelcontextprotocol/clientCapabilities": {},
                    "openai/session": openai_session
                }
            }
        }))
        .send()
        .await
        .unwrap();
    let status = response.status();
    let body = response.bytes().await.unwrap();
    assert!(
        status.is_success(),
        "exec for {openai_session} failed (cmd={cmd:?}) with HTTP {status}: {}",
        String::from_utf8_lossy(&body),
    );
    serde_json::from_slice(&body).unwrap()
}

#[cfg(target_os = "linux")]
async fn call_write_stdin(
    mcp_url: &str,
    token: &str,
    openai_session: &str,
    session_handle: &str,
    yield_time_ms: u64,
) -> serde_json::Value {
    static NEXT_REQUEST_ID: AtomicU64 = AtomicU64::new(2_000);
    crate::install_rustls_crypto_provider();
    let response = reqwest::Client::new()
        .post(mcp_url)
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .header("MCP-Protocol-Version", "2026-07-28")
        .header("Mcp-Method", "tools/call")
        .header("Mcp-Name", "write_stdin")
        .header(crate::server::LOCAL_MCP_TOKEN_HEADER, token)
        .json(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": NEXT_REQUEST_ID.fetch_add(1, Ordering::Relaxed),
            "method": "tools/call",
            "params": {
                "name": "write_stdin",
                "arguments": {
                    "session_handle": session_handle,
                    "yield_time_ms": yield_time_ms,
                    "kill_process": false,
                },
                "_meta": {
                    "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                    "io.modelcontextprotocol/clientInfo": {
                        "name": "zodex-lifecycle-child-test",
                        "version": "1.0"
                    },
                    "io.modelcontextprotocol/clientCapabilities": {},
                    "openai/session": openai_session
                }
            }
        }))
        .send()
        .await
        .unwrap();
    let status = response.status();
    let body = response.bytes().await.unwrap();
    assert!(
        status.is_success(),
        "write_stdin for {openai_session} session {session_handle} failed with HTTP {status}: {}",
        String::from_utf8_lossy(&body),
    );
    serde_json::from_slice(&body).unwrap()
}

#[cfg(target_os = "linux")]
fn shell_quote(value: &std::ffi::OsStr) -> String {
    let value = value.to_string_lossy();
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn fully_ready_health() -> LocalRuntimeHealth {
    LocalRuntimeHealth {
        mcp_ready: true,
        observability_ready: true,
        tunnel_process_running: true,
        tunnel_control_plane_ready: true,
        tunnel_ready: true,
        last_error: None,
    }
}

#[derive(Default)]
struct FakeLaunchd {
    loaded: Mutex<bool>,
    bootstrap_calls: Mutex<usize>,
    bootout_calls: Mutex<usize>,
}

impl FakeLaunchd {
    fn loaded() -> Self {
        Self {
            loaded: Mutex::new(true),
            ..Self::default()
        }
    }

    fn bootstrap_calls(&self) -> usize {
        *self.bootstrap_calls.lock().unwrap()
    }

    fn bootout_calls(&self) -> usize {
        *self.bootout_calls.lock().unwrap()
    }
}

impl LaunchdController for FakeLaunchd {
    fn is_loaded(&self) -> anyhow::Result<bool> {
        Ok(*self.loaded.lock().unwrap())
    }

    fn bootstrap(&self, _plist: &Path) -> anyhow::Result<()> {
        *self.bootstrap_calls.lock().unwrap() += 1;
        *self.loaded.lock().unwrap() = true;
        Ok(())
    }

    fn bootout(&self) -> anyhow::Result<()> {
        *self.bootout_calls.lock().unwrap() += 1;
        *self.loaded.lock().unwrap() = false;
        Ok(())
    }
}

struct ReadyOnBootstrapLaunchd {
    paths: LocalPaths,
    bootstrap_calls: AtomicUsize,
    ready_after: usize,
}

impl ReadyOnBootstrapLaunchd {
    fn new(paths: LocalPaths) -> Self {
        Self {
            paths,
            bootstrap_calls: AtomicUsize::new(0),
            ready_after: 1,
        }
    }

    fn after_attempts(paths: LocalPaths, ready_after: usize) -> Self {
        Self {
            paths,
            bootstrap_calls: AtomicUsize::new(0),
            ready_after,
        }
    }

    fn bootstrap_calls(&self) -> usize {
        self.bootstrap_calls.load(Ordering::Acquire)
    }
}

impl LaunchdController for ReadyOnBootstrapLaunchd {
    fn is_loaded(&self) -> anyhow::Result<bool> {
        Ok(self.bootstrap_calls() > 0)
    }

    fn bootstrap(&self, _plist: &Path) -> anyhow::Result<()> {
        let attempt = self.bootstrap_calls.fetch_add(1, Ordering::AcqRel) + 1;
        if attempt < self.ready_after {
            return Ok(());
        }
        let bootstrap =
            super::lifecycle::load_runtime_bootstrap(&self.paths.runtime_bootstrap_file())?;
        let inspector = SystemProcessInspector;
        let process = inspector
            .identity(std::process::id() as i32)?
            .expect("test process must have a stable identity");
        write_runtime_state(
            &self.paths,
            &LocalRuntimeState {
                schema_version: LOCAL_RUNTIME_STATE_SCHEMA_VERSION,
                runtime_id: bootstrap.runtime_id.clone(),
                lifecycle: LocalRuntimeLifecycle::Ready,
                process: Some(process),
                start_directory: Some(bootstrap.start_directory.clone()),
                started_at: Some(bootstrap.started_at.clone()),
                expires_at: bootstrap.expires_at.clone(),
                health: fully_ready_health(),
            },
        )?;
        write_runtime_discovery(
            &self.paths,
            &LocalRuntimeDiscovery {
                schema_version: LOCAL_DISCOVERY_SCHEMA_VERSION,
                runtime_id: bootstrap.runtime_id,
                pid: std::process::id(),
                start_directory: bootstrap.start_directory,
                started_at: bootstrap.started_at,
                expires_at: bootstrap.expires_at,
                observability: LocalObservabilityDiscovery::active(
                    "http://127.0.0.1:43123",
                    self.paths.observability_bearer_file(),
                ),
            },
        )?;
        let _ = fs::remove_file(self.paths.environment_handoff_file());
        Ok(())
    }

    fn bootout(&self) -> anyhow::Result<()> {
        Ok(())
    }
}

fn test_paths(root: &Path) -> LocalPaths {
    LocalPaths::from_roots(root.join("config"), root.join("data"), root.join("state")).unwrap()
}
