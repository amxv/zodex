use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use rand::distr::{Alphanumeric, SampleString};
use serde::{Deserialize, Serialize};
use serde_json::json;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::config::Config;
use crate::session::{
    ProcessInspector, ProcessSignal, SystemProcessInspector, identity_matches,
    signal_process_if_matching,
};

#[cfg(any(target_os = "macos", target_os = "windows"))]
use super::lifecycle_artifacts::write_private_bytes;
use super::lifecycle_artifacts::{
    append_lifecycle_diagnostic, set_user_only_directory, write_private_json,
};
use super::lifecycle_context::{
    canonicalize_start_directory, resolve_developer_shell, start_directory_error,
    validate_runtime_start_directory,
};
use super::lifecycle_lock::LocalLifecycleLock;

#[cfg(target_os = "macos")]
use super::LocalLaunchdJob;
use super::liveboard::{
    LocalLiveboardDiscovery, remove_liveboard_discovery, write_liveboard_discovery,
};
use super::{
    LOCAL_DISCOVERY_SCHEMA_VERSION, LOCAL_RUNTIME_STATE_SCHEMA_VERSION, LaunchdController,
    LocalConfig, LocalHostRuntime, LocalHostRuntimeOptions, LocalObservabilityDiscovery,
    LocalPaths, LocalRuntimeDiscovery, LocalRuntimeHealth, LocalRuntimeLifecycle,
    LocalRuntimeState, LocalTunnelProfile, ManagedTunnelChild, RuntimeKey, StaleTunnelCleanup,
    cleanup_stale_tunnel_child, consume_environment_handoff, load_runtime_discovery,
    load_runtime_state, probe_tunnel_health, spawn_tunnel_client, start_local_host_runtime,
    terminate_matching_stale_processes, write_environment_handoff, write_mcp_token,
    write_runtime_discovery, write_runtime_state, write_tunnel_profile,
};

pub const LOCAL_RUNTIME_BOOTSTRAP_SCHEMA_VERSION: u32 = 1;
const START_READY_POLL: Duration = Duration::from_millis(150);
const STOP_GRACE: Duration = Duration::from_secs(20);
const STOP_POLL: Duration = Duration::from_millis(100);
const TUNNEL_INITIAL_READY_TIMEOUT: Duration = Duration::from_secs(30);
const TUNNEL_HEALTH_POLL: Duration = Duration::from_millis(300);
const TUNNEL_STEADY_HEALTH_INTERVAL: Duration = Duration::from_secs(5);
const TUNNEL_RESTART_BACKOFF: Duration = Duration::from_secs(2);
const TTL_RECONCILE_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LocalRuntimeBootstrap {
    pub schema_version: u32,
    pub runtime_id: String,
    pub config_root: PathBuf,
    pub data_root: PathBuf,
    pub state_root: PathBuf,
    pub start_directory: PathBuf,
    pub environment_handoff_path: PathBuf,
    pub started_at: String,
    pub expires_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedLocalLaunch {
    pub runtime_id: String,
    pub start_directory: PathBuf,
    pub started_at: String,
    pub expires_at: Option<String>,
    pub bootstrap_path: PathBuf,
    pub plist_path: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalStopOutcome {
    AlreadyStopped,
    Graceful,
    Forced,
    StaleCleaned,
}

pub fn prepare_local_launch(
    paths: &LocalPaths,
    executable: &Path,
    requested_start_directory: &Path,
    ttl_seconds: Option<u64>,
    environment: &[(OsString, OsString)],
) -> Result<PreparedLocalLaunch> {
    prepare_local_launch_at(
        paths,
        executable,
        requested_start_directory,
        ttl_seconds,
        environment,
        OffsetDateTime::now_utc(),
        format!("{:032x}", rand::random::<u128>()),
    )
}

pub(super) fn prepare_local_launch_at(
    paths: &LocalPaths,
    executable: &Path,
    requested_start_directory: &Path,
    ttl_seconds: Option<u64>,
    environment: &[(OsString, OsString)],
    started: OffsetDateTime,
    runtime_id: String,
) -> Result<PreparedLocalLaunch> {
    if !executable.is_absolute() {
        bail!("installed Zodex executable path must be absolute");
    }
    let start_directory = canonicalize_start_directory(requested_start_directory)?;
    let started_at = format_timestamp(started)?;
    let expires_at = ttl_seconds
        .map(|seconds| {
            let seconds = i64::try_from(seconds).context("Local TTL is too large")?;
            let expiry = started
                .checked_add(time::Duration::seconds(seconds))
                .context("Local TTL expiry is outside the supported timestamp range")?;
            format_timestamp(expiry)
        })
        .transpose()?;

    paths.ensure_persistent_dirs()?;
    fs::create_dir_all(paths.runtime_dir()).with_context(|| {
        format!(
            "failed to create Local runtime directory {}",
            paths.runtime_dir().display()
        )
    })?;
    set_user_only_directory(&paths.runtime_dir())?;

    let environment_handoff_path = paths.environment_handoff_file();
    write_environment_handoff(&environment_handoff_path, environment)?;
    let bootstrap = LocalRuntimeBootstrap {
        schema_version: LOCAL_RUNTIME_BOOTSTRAP_SCHEMA_VERSION,
        runtime_id: runtime_id.clone(),
        config_root: paths.config_root().to_path_buf(),
        data_root: paths.data_root().to_path_buf(),
        state_root: paths.state_root().to_path_buf(),
        start_directory: start_directory.clone(),
        environment_handoff_path,
        started_at: started_at.clone(),
        expires_at: expires_at.clone(),
    };
    let bootstrap_path = paths.runtime_bootstrap_file();
    write_private_json(&bootstrap_path, &bootstrap)?;
    let plist_path = paths.launchd_plist_file();
    #[cfg(target_os = "macos")]
    write_private_bytes(
        &plist_path,
        LocalLaunchdJob::new(executable, &bootstrap_path)?
            .render_plist()
            .as_bytes(),
    )?;
    write_runtime_state(
        paths,
        &LocalRuntimeState {
            schema_version: LOCAL_RUNTIME_STATE_SCHEMA_VERSION,
            runtime_id: runtime_id.clone(),
            lifecycle: LocalRuntimeLifecycle::Starting,
            process: None,
            start_directory: Some(start_directory.clone()),
            started_at: Some(started_at.clone()),
            expires_at: expires_at.clone(),
            health: LocalRuntimeHealth::default(),
        },
    )?;
    Ok(PreparedLocalLaunch {
        runtime_id,
        start_directory,
        started_at,
        expires_at,
        bootstrap_path,
        plist_path,
    })
}

pub async fn stop_via_launchd(
    paths: &LocalPaths,
    launchd: &dyn LaunchdController,
) -> Result<LocalStopOutcome> {
    let _lifecycle_lock = LocalLifecycleLock::acquire(paths)?;
    let inspector = SystemProcessInspector;
    let state = load_runtime_state(paths)?;
    let mut outcome = LocalStopOutcome::AlreadyStopped;

    if let Some(state) = state.as_ref()
        && let Some(process) = state.process.as_ref()
    {
        if signal_process_if_matching(&inspector, process, ProcessSignal::Terminate)? {
            outcome = LocalStopOutcome::Graceful;
            let deadline = Instant::now() + STOP_GRACE;
            while Instant::now() < deadline {
                if inspector.identity(process.pid)?.is_none()
                    || !identity_matches(&inspector, process)?
                {
                    break;
                }
                tokio::time::sleep(STOP_POLL).await;
            }
            if identity_matches(&inspector, process)? {
                let _ = signal_process_if_matching(&inspector, process, ProcessSignal::Kill)?;
                outcome = LocalStopOutcome::Forced;
            }
        } else if inspector.identity(process.pid)?.is_some() {
            bail!(
                "refusing to signal stale Zodex Local runtime PID {} because its process birth identity no longer matches",
                process.pid
            );
        }
    }

    // If the outer runtime was already gone, safely clean a tunnel child that
    // survived it. A reused PID is never signaled and blocks destructive cleanup.
    match cleanup_stale_tunnel_child(paths, &inspector)? {
        StaleTunnelCleanup::IdentityMismatch(pid) => bail!(
            "refusing to discard stale tunnel state because PID {pid} now has a different process identity"
        ),
        StaleTunnelCleanup::SignaledMatchingChild(_) | StaleTunnelCleanup::AlreadyExited(_) => {
            if outcome == LocalStopOutcome::AlreadyStopped {
                outcome = LocalStopOutcome::StaleCleaned;
            }
        }
        StaleTunnelCleanup::NoRecordedChild => {}
    }
    if paths.owned_process_registry_file().exists() {
        let report = terminate_matching_stale_processes(
            &paths.owned_process_registry_file(),
            state.as_ref().map(|state| state.runtime_id.as_str()),
            &inspector,
        )?;
        if report.identity_mismatch > 0
            || report.unresolved_leaderless_groups > 0
            || report.survivors > 0
        {
            bail!(
                "stale Local command-process cleanup is unresolved (identity mismatches: {}, leaderless groups: {}, survivors: {}); refusing destructive runtime cleanup",
                report.identity_mismatch,
                report.unresolved_leaderless_groups,
                report.survivors,
            );
        }
        if outcome == LocalStopOutcome::AlreadyStopped
            && (report.signaled > 0
                || report.force_killed > 0
                || report.descendants_signaled > 0
                || report.already_gone > 0)
        {
            outcome = LocalStopOutcome::StaleCleaned;
        }
    }
    launchd.bootout()?;
    paths.clear_runtime_state()?;
    Ok(outcome)
}

#[cfg(target_os = "windows")]
pub async fn stop_via_windows_process(paths: &LocalPaths) -> Result<LocalStopOutcome> {
    struct NoopLifecycleController;
    impl LaunchdController for NoopLifecycleController {
        fn is_loaded(&self) -> Result<bool> {
            Ok(false)
        }
        fn bootstrap(&self, _plist: &Path) -> Result<()> {
            Ok(())
        }
        fn bootout(&self) -> Result<()> {
            Ok(())
        }
    }
    let inspector = SystemProcessInspector;
    let mut requested_process = None;
    {
        let _lifecycle_lock = LocalLifecycleLock::acquire(paths)?;
        if let Some(state) = load_runtime_state(paths)?
            && let Some(process) = state.process.as_ref()
        {
            if identity_matches(&inspector, process)? {
                write_private_bytes(
                    paths.stop_request_file().as_path(),
                    state.runtime_id.as_bytes(),
                )?;
                requested_process = Some(process.clone());
            } else if inspector.identity(process.pid)?.is_some() {
                bail!(
                    "refusing to request shutdown from stale Zodex Local runtime PID {} because its process birth identity no longer matches",
                    process.pid
                );
            }
        }
    }

    let mut forced_fallback = false;
    if let Some(process) = requested_process.as_ref() {
        let deadline = Instant::now() + STOP_GRACE;
        while Instant::now() < deadline && identity_matches(&inspector, process)? {
            tokio::time::sleep(STOP_POLL).await;
        }
        forced_fallback = identity_matches(&inspector, process)?;
    }

    let cleanup = stop_via_launchd(paths, &NoopLifecycleController).await?;
    let _ = fs::remove_file(paths.stop_request_file());
    if requested_process.is_some() {
        Ok(if forced_fallback {
            LocalStopOutcome::Forced
        } else {
            LocalStopOutcome::Graceful
        })
    } else {
        Ok(cleanup)
    }
}

#[cfg(target_os = "linux")]
pub async fn stop_via_linux_process(paths: &LocalPaths) -> Result<LocalStopOutcome> {
    struct NoopLifecycleController;
    impl LaunchdController for NoopLifecycleController {
        fn is_loaded(&self) -> Result<bool> {
            Ok(false)
        }
        fn bootstrap(&self, _plist: &Path) -> Result<()> {
            Ok(())
        }
        fn bootout(&self) -> Result<()> {
            Ok(())
        }
    }
    stop_via_launchd(paths, &NoopLifecycleController).await
}

pub fn cleanup_stale_runtime(paths: &LocalPaths, launchd: &dyn LaunchdController) -> Result<()> {
    let inspector = SystemProcessInspector;
    let state = load_runtime_state(paths)?;
    if let Some(state) = state.as_ref()
        && let Some(process) = state.process.as_ref()
        && identity_matches(&inspector, process)?
    {
        bail!(
            "Zodex Local runtime {} is still running (PID {}); use `zodex local stop` before replacing it",
            state.runtime_id,
            process.pid
        );
    }
    match cleanup_stale_tunnel_child(paths, &inspector)? {
        StaleTunnelCleanup::IdentityMismatch(pid) => bail!(
            "a stale tunnel record points at reused PID {pid}; refusing to publish a new Local runtime until it is resolved"
        ),
        StaleTunnelCleanup::NoRecordedChild
        | StaleTunnelCleanup::SignaledMatchingChild(_)
        | StaleTunnelCleanup::AlreadyExited(_) => {}
    }
    if paths.owned_process_registry_file().exists() {
        let report = terminate_matching_stale_processes(
            &paths.owned_process_registry_file(),
            state.as_ref().map(|state| state.runtime_id.as_str()),
            &inspector,
        )?;
        if report.identity_mismatch > 0
            || report.unresolved_leaderless_groups > 0
            || report.survivors > 0
        {
            bail!(
                "stale Local process registry cleanup is unresolved (identity mismatches: {}, leaderless groups: {}, survivors: {}); refusing cleanup",
                report.identity_mismatch,
                report.unresolved_leaderless_groups,
                report.survivors,
            );
        }
    }
    launchd.bootout()?;
    paths.clear_runtime_state()?;
    Ok(())
}

pub fn load_runtime_bootstrap(path: &Path) -> Result<LocalRuntimeBootstrap> {
    let raw = fs::read(path)
        .with_context(|| format!("failed to read Local runtime bootstrap {}", path.display()))?;
    let bootstrap: LocalRuntimeBootstrap = serde_json::from_slice(&raw)
        .with_context(|| format!("failed to parse Local runtime bootstrap {}", path.display()))?;
    if bootstrap.schema_version != LOCAL_RUNTIME_BOOTSTRAP_SCHEMA_VERSION {
        bail!(
            "unsupported Local runtime bootstrap schema version {}; expected {}",
            bootstrap.schema_version,
            LOCAL_RUNTIME_BOOTSTRAP_SCHEMA_VERSION
        );
    }
    Ok(bootstrap)
}

pub async fn run_hidden_runtime(
    paths: LocalPaths,
    bootstrap_path: PathBuf,
    runtime_key: RuntimeKey,
) -> Result<()> {
    let result = run_hidden_runtime_inner(paths.clone(), bootstrap_path.clone(), runtime_key).await;
    if let Err(error) = result.as_ref() {
        // Startup failures can happen before the coordinated steady-state
        // shutdown path owns every component. Fail closed: remove remote
        // discovery, terminate only birth-identity-matching children, and
        // leave a diagnostic state record for the parent `start` poller.
        let _ = fs::remove_file(paths.discovery_file());
        remove_liveboard_discovery(&paths);
        let inspector = SystemProcessInspector;
        let _ = cleanup_stale_tunnel_child(&paths, &inspector);
        let stale_runtime_id = load_runtime_state(&paths)
            .ok()
            .flatten()
            .map(|state| state.runtime_id);
        if paths.owned_process_registry_file().exists() {
            let _ = terminate_matching_stale_processes(
                &paths.owned_process_registry_file(),
                stale_runtime_id.as_deref(),
                &inspector,
            );
        }
        if let Ok(bootstrap) = load_runtime_bootstrap(&bootstrap_path) {
            let process = inspector.identity(std::process::id() as i32).ok().flatten();
            let mut state =
                load_runtime_state(&paths)
                    .ok()
                    .flatten()
                    .unwrap_or(LocalRuntimeState {
                        schema_version: LOCAL_RUNTIME_STATE_SCHEMA_VERSION,
                        runtime_id: bootstrap.runtime_id,
                        lifecycle: LocalRuntimeLifecycle::Starting,
                        process,
                        start_directory: Some(bootstrap.start_directory),
                        started_at: Some(bootstrap.started_at),
                        expires_at: bootstrap.expires_at,
                        health: LocalRuntimeHealth::default(),
                    });
            state.health.last_error = Some(error.to_string());
            if state.process.is_none() {
                state.process = inspector.identity(std::process::id() as i32).ok().flatten();
            }
            let _ = write_runtime_state(&paths, &state);
        }
        let _ = append_lifecycle_diagnostic(
            &paths,
            &format!("Local runtime startup failed: {error:#}"),
        );
    }
    result
}

async fn run_hidden_runtime_inner(
    paths: LocalPaths,
    bootstrap_path: PathBuf,
    runtime_key: RuntimeKey,
) -> Result<()> {
    let bootstrap = load_runtime_bootstrap(&bootstrap_path)?;
    let bootstrap_paths = LocalPaths::from_roots(
        bootstrap.config_root.clone(),
        bootstrap.data_root.clone(),
        bootstrap.state_root.clone(),
    )?;
    if bootstrap_paths != paths {
        bail!("hidden Local runtime path roots do not match the managed bootstrap");
    }
    if bootstrap_path != paths.runtime_bootstrap_file() {
        bail!("hidden Local runtime bootstrap path is outside the managed runtime layout");
    }
    let environment = consume_environment_handoff(&bootstrap.environment_handoff_path)
        .context("failed to consume captured Local developer environment")?;
    let start_directory = validate_runtime_start_directory(&bootstrap.start_directory)?;
    std::env::set_current_dir(&start_directory)
        .map_err(|error| start_directory_error(&start_directory, "enter", error))?;
    let shell = resolve_developer_shell(&environment)?;
    let local_config = LocalConfig::load(&paths.config_file())?;
    if !local_config.is_provider_configured() {
        bail!("Zodex Local is not configured; run `zodex local setup` first");
    }
    let tunnel_id = local_config
        .tunnel
        .id
        .clone()
        .context("Local tunnel id is missing")?;
    let tunnel_binary = local_config
        .tunnel
        .client_path
        .clone()
        .context("managed tunnel-client path is missing")?;
    if !tunnel_binary.is_absolute() {
        bail!("managed tunnel-client path must be absolute");
    }

    let inspector = SystemProcessInspector;
    let pid = std::process::id() as i32;
    let process = inspector
        .identity(pid)?
        .context("could not establish stable identity for hidden Local runtime process")?;
    let mut state = LocalRuntimeState {
        schema_version: LOCAL_RUNTIME_STATE_SCHEMA_VERSION,
        runtime_id: bootstrap.runtime_id.clone(),
        lifecycle: LocalRuntimeLifecycle::Starting,
        process: Some(process),
        start_directory: Some(start_directory.clone()),
        started_at: Some(bootstrap.started_at.clone()),
        expires_at: bootstrap.expires_at.clone(),
        health: LocalRuntimeHealth::default(),
    };
    write_runtime_state(&paths, &state)?;

    let mcp_token = generate_runtime_token();
    write_mcp_token(&paths.mcp_token_file(), &mcp_token)?;
    let shared_runtime_config = Arc::new(Config::load(None)?);
    let host = start_local_host_runtime(LocalHostRuntimeOptions {
        paths: paths.clone(),
        start_directory: start_directory.clone(),
        shell,
        environment: environment.clone(),
        mcp_token: Arc::from(mcp_token.as_str()),
        shared_runtime_config,
        runtime_id: Some(Arc::from(bootstrap.runtime_id.as_str())),
    })
    .await
    .context("failed to start Local MCP/history/observability runtime")?;
    state.health.mcp_ready = false;
    state.health.observability_ready = true;
    write_runtime_state(&paths, &state)?;

    probe_local_mcp(&host.mcp_url(), &mcp_token, &start_directory)
        .await
        .context("Local modern MCP readiness probe failed")?;
    state.health.mcp_ready = true;
    write_runtime_state(&paths, &state)?;

    let profile = LocalTunnelProfile {
        tunnel_id,
        mcp_url: host.mcp_url(),
        mcp_token_path: paths.mcp_token_file(),
        health_url_file: paths.tunnel_health_url_file(),
    };
    write_tunnel_profile(&paths.tunnel_profile_file(), &profile)?;
    let redactions = vec![runtime_key.expose().to_string(), mcp_token.clone()];
    let mut tunnel = spawn_tunnel_client(
        &paths,
        &tunnel_binary,
        &paths.tunnel_profile_file(),
        &runtime_key,
        &bootstrap.runtime_id,
        &environment,
        &redactions,
    )
    .await?;
    state.health.tunnel_process_running = true;
    write_runtime_state(&paths, &state)?;

    wait_for_initial_tunnel_readiness(
        &paths,
        &tunnel_binary,
        &runtime_key,
        &environment,
        &mut tunnel,
        &mut state,
    )
    .await?;

    state.lifecycle = LocalRuntimeLifecycle::Ready;
    state.health.tunnel_control_plane_ready = true;
    state.health.tunnel_ready = true;
    state.health.last_error = None;
    write_runtime_state(&paths, &state)?;
    if let Some(base_url) = host.liveboard_url() {
        match LocalLiveboardDiscovery::new(bootstrap.runtime_id.clone(), base_url)
            .and_then(|discovery| write_liveboard_discovery(&paths, &discovery))
        {
            Ok(()) => {}
            Err(error) => {
                remove_liveboard_discovery(&paths);
                let _ = append_lifecycle_diagnostic(
                    &paths,
                    &format!(
                        "Local Liveboard private discovery could not be published; MCP remains available: {error:#}"
                    ),
                );
            }
        }
    }
    write_runtime_discovery(
        &paths,
        &LocalRuntimeDiscovery {
            schema_version: LOCAL_DISCOVERY_SCHEMA_VERSION,
            runtime_id: bootstrap.runtime_id.clone(),
            pid: std::process::id(),
            start_directory: start_directory.clone(),
            started_at: bootstrap.started_at.clone(),
            expires_at: bootstrap.expires_at.clone(),
            observability: LocalObservabilityDiscovery::active(
                host.observability_url(),
                paths.observability_bearer_file(),
            ),
        },
    )?;

    let supervisor = TunnelSupervisorContext {
        paths: &paths,
        bootstrap: &bootstrap,
        tunnel_binary: &tunnel_binary,
        runtime_key: &runtime_key,
        environment: &environment,
        redactions: &redactions,
    };
    let run_result = supervise_runtime(&supervisor, &host, &mut tunnel, &mut state).await;
    coordinated_hidden_shutdown(paths, host, tunnel, state, run_result).await
}

async fn wait_for_initial_tunnel_readiness(
    paths: &LocalPaths,
    tunnel_binary: &Path,
    runtime_key: &RuntimeKey,
    environment: &[(OsString, OsString)],
    tunnel: &mut ManagedTunnelChild,
    state: &mut LocalRuntimeState,
) -> Result<()> {
    let deadline = Instant::now() + TUNNEL_INITIAL_READY_TIMEOUT;
    let mut last_error = None;
    while Instant::now() < deadline {
        if let Some(status) = tunnel.try_wait()? {
            bail!("managed tunnel-client exited before readiness ({status})");
        }
        match probe_tunnel_health(
            tunnel_binary,
            &paths.tunnel_health_url_file(),
            runtime_key,
            environment,
        )
        .await
        {
            Ok(evidence) if evidence.live && evidence.ready => {
                state.health.tunnel_control_plane_ready = true;
                state.health.tunnel_ready = true;
                state.health.last_error = None;
                write_runtime_state(paths, state)?;
                return Ok(());
            }
            Ok(evidence) => {
                last_error = Some(if evidence.diagnostic.is_empty() {
                    "tunnel-client structured health is not ready".to_string()
                } else {
                    evidence.diagnostic
                });
            }
            Err(error) => last_error = Some(error.to_string()),
        }
        state.health.last_error = last_error.clone();
        let _ = write_runtime_state(paths, state);
        tokio::time::sleep(TUNNEL_HEALTH_POLL).await;
    }
    bail!(
        "managed tunnel-client did not reach structured readiness within {}s: {}",
        TUNNEL_INITIAL_READY_TIMEOUT.as_secs(),
        last_error.unwrap_or_else(|| "no health evidence became available".to_string())
    )
}

struct TunnelSupervisorContext<'a> {
    paths: &'a LocalPaths,
    bootstrap: &'a LocalRuntimeBootstrap,
    tunnel_binary: &'a Path,
    runtime_key: &'a RuntimeKey,
    environment: &'a [(OsString, OsString)],
    redactions: &'a [String],
}

async fn supervise_runtime(
    context: &TunnelSupervisorContext<'_>,
    _host: &LocalHostRuntime,
    tunnel: &mut ManagedTunnelChild,
    state: &mut LocalRuntimeState,
) -> Result<()> {
    let mut ttl_tick = tokio::time::interval(TTL_RECONCILE_INTERVAL);
    ttl_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut health_tick = tokio::time::interval(TUNNEL_STEADY_HEALTH_INTERVAL);
    health_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let stop_signal = shutdown_signal();
    tokio::pin!(stop_signal);
    let mut liveboard_published = _host.liveboard_url().is_some();

    loop {
        tokio::select! {
            _ = ttl_tick.tick() => {
                #[cfg(target_os = "windows")]
                if windows_stop_requested(context.paths, &context.bootstrap.runtime_id)? {
                    return Ok(());
                }
                if liveboard_published && _host.liveboard_is_finished() {
                    liveboard_published = false;
                    #[cfg(target_os = "macos")]
                    _host.disable_liveboard_notifier();
                    remove_liveboard_discovery(context.paths);
                    let _ = append_lifecycle_diagnostic(
                        context.paths,
                        "Local Liveboard sidecar exited unexpectedly; MCP remains available",
                    );
                }
                if is_expired(context.bootstrap.expires_at.as_deref(), OffsetDateTime::now_utc())? {
                    return Ok(());
                }
                if let Some(status) = tunnel.try_wait()? {
                    state.health.tunnel_process_running = false;
                    state.health.tunnel_control_plane_ready = false;
                    state.health.tunnel_ready = false;
                    state.health.last_error = Some(format!("tunnel-client exited unexpectedly ({status}); restarting"));
                    write_runtime_state(context.paths, state)?;
                    // A fixed non-zero backoff prevents a provider crash loop
                    // from turning into a hot respawn loop. A failed respawn
                    // fails the outer runtime closed; a later explicit start
                    // performs identity-safe stale recovery.
                    tokio::time::sleep(TUNNEL_RESTART_BACKOFF).await;
                    *tunnel = spawn_tunnel_client(
                        context.paths,
                        context.tunnel_binary,
                        &context.paths.tunnel_profile_file(),
                        context.runtime_key,
                        &context.bootstrap.runtime_id,
                        context.environment,
                        context.redactions,
                    ).await?;
                    state.health.tunnel_process_running = true;
                    state.health.last_error = Some("tunnel-client restarted; waiting for readiness".to_string());
                    write_runtime_state(context.paths, state)?;
                    wait_for_initial_tunnel_readiness(
                        context.paths,
                        context.tunnel_binary,
                        context.runtime_key,
                        context.environment,
                        tunnel,
                        state,
                    ).await?;
                }
            }
            _ = health_tick.tick() => {
                match probe_tunnel_health(
                    context.tunnel_binary,
                    &context.paths.tunnel_health_url_file(),
                    context.runtime_key,
                    context.environment,
                ).await {
                    Ok(evidence) => {
                        state.health.tunnel_process_running = tunnel.try_wait()?.is_none();
                        state.health.tunnel_control_plane_ready = evidence.live;
                        state.health.tunnel_ready = evidence.ready && state.health.tunnel_process_running;
                        state.health.last_error = (!state.health.tunnel_ready).then_some(evidence.diagnostic);
                    }
                    Err(error) => {
                        state.health.tunnel_process_running = tunnel.try_wait()?.is_none();
                        state.health.tunnel_control_plane_ready = false;
                        state.health.tunnel_ready = false;
                        state.health.last_error = Some(error.to_string());
                    }
                }
                write_runtime_state(context.paths, state)?;
            }
            signal = &mut stop_signal => {
                signal?;
                return Ok(());
            }
        }
    }
}

#[cfg(target_os = "windows")]
fn windows_stop_requested(paths: &LocalPaths, runtime_id: &str) -> Result<bool> {
    let path = paths.stop_request_file();
    let requested = match fs::read_to_string(&path) {
        Ok(value) => value,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "failed to read Local Windows stop request {}",
                    path.display()
                )
            });
        }
    };
    Ok(requested == runtime_id)
}

async fn coordinated_hidden_shutdown(
    paths: LocalPaths,
    host: LocalHostRuntime,
    tunnel: ManagedTunnelChild,
    mut state: LocalRuntimeState,
    run_result: Result<()>,
) -> Result<()> {
    state.lifecycle = LocalRuntimeLifecycle::Stopping;
    let _ = write_runtime_state(&paths, &state);

    // Required ordering: close all mutating tool admission first, then remove
    // remote ingress, then terminate Local-owned command processes/history and
    // finally the listeners.
    host.close_admission().await;
    let _ = fs::remove_file(paths.discovery_file());
    remove_liveboard_discovery(&paths);
    let tunnel_shutdown = tunnel.terminate().await;
    let _ = fs::remove_file(paths.tunnel_process_state_file());
    let host_shutdown = host.shutdown().await;
    let _ = fs::remove_file(paths.mcp_token_file());
    let _ = fs::remove_file(paths.tunnel_profile_file());
    let _ = fs::remove_file(paths.tunnel_health_url_file());

    let mut first_error = run_result.err();
    if let Err(error) = tunnel_shutdown
        && first_error.is_none()
    {
        first_error = Some(error.context("failed to stop managed tunnel-client"));
    }
    if let Err(error) = host_shutdown
        && first_error.is_none()
    {
        first_error = Some(error.context("failed to stop Local host runtime"));
    }
    if let Some(error) = first_error.as_ref() {
        let _ = append_lifecycle_diagnostic(
            &paths,
            &format!("Local runtime shutdown completed with diagnostics: {error:#}"),
        );
    }
    let _ = paths.clear_runtime_state();
    first_error.map_or(Ok(()), Err)
}

pub async fn probe_local_mcp(
    mcp_url: &str,
    token: &str,
    expected_start_directory: &Path,
) -> Result<()> {
    crate::install_rustls_crypto_provider();
    let response = reqwest::Client::new()
        .post(mcp_url)
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .header("MCP-Protocol-Version", "2026-07-28")
        .header("Mcp-Method", "server/discover")
        .header(crate::server::LOCAL_MCP_TOKEN_HEADER, token)
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "server/discover",
            "params": {
                "_meta": {
                    "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                    "io.modelcontextprotocol/clientInfo": {
                        "name": "zodex-local-readiness",
                        "version": env!("CARGO_PKG_VERSION")
                    },
                    "io.modelcontextprotocol/clientCapabilities": {}
                }
            }
        }))
        .send()
        .await
        .context("failed to send modern Local MCP discovery probe")?;
    let status = response.status();
    let body: serde_json::Value = response
        .json()
        .await
        .context("Local MCP discovery probe returned invalid JSON")?;
    if !status.is_success() {
        bail!("Local MCP discovery probe returned HTTP {status}: {body}");
    }
    if body.get("error").is_some() {
        bail!("Local MCP discovery probe returned a JSON-RPC error: {body}");
    }
    let instructions = body["result"]["instructions"]
        .as_str()
        .context("Local MCP discovery result did not contain instructions")?;
    if !instructions.contains(expected_start_directory.to_string_lossy().as_ref()) {
        bail!(
            "Local MCP discovery did not advertise the active start directory {}",
            expected_start_directory.display()
        );
    }
    let supported = body["result"]["supportedVersions"]
        .as_array()
        .context("Local MCP discovery result did not contain supported versions")?;
    if !supported.iter().any(|value| value == "2026-07-28") {
        bail!("Local MCP discovery did not advertise protocol 2026-07-28");
    }
    Ok(())
}

pub async fn wait_for_runtime_ready(
    paths: &LocalPaths,
    expected_runtime_id: &str,
    timeout: Duration,
) -> Result<LocalRuntimeDiscovery> {
    let deadline = Instant::now() + timeout;
    let mut last_reason = "runtime has not published process state".to_string();
    while Instant::now() < deadline {
        match load_runtime_state(paths) {
            Ok(Some(state)) if state.runtime_id == expected_runtime_id => {
                if let Some(error) = state.health.last_error.as_deref() {
                    last_reason = error.to_string();
                }
                if state.lifecycle == LocalRuntimeLifecycle::Ready
                    && state.health.mcp_ready
                    && state.health.observability_ready
                    && state.health.tunnel_process_running
                    && state.health.tunnel_control_plane_ready
                    && state.health.tunnel_ready
                    && let Some(discovery) = load_runtime_discovery(paths)?
                    && discovery.runtime_id == expected_runtime_id
                {
                    return Ok(discovery);
                }
                if let Some(process) = state.process.as_ref()
                    && !identity_matches(&SystemProcessInspector, process)?
                {
                    bail!(
                        "hidden Local runtime exited before readiness: {}",
                        state.health.last_error.unwrap_or(last_reason)
                    );
                }
            }
            Ok(Some(state)) => {
                bail!(
                    "Local runtime identity changed during startup (expected {expected_runtime_id}, found {})",
                    state.runtime_id
                );
            }
            Ok(None) => {}
            Err(error) => last_reason = error.to_string(),
        }
        tokio::time::sleep(START_READY_POLL).await;
    }
    bail!(
        "Zodex Local did not reach composite readiness within {}s: {last_reason}; inspect `zodex local status` and `zodex local logs`",
        timeout.as_secs()
    )
}

pub(super) fn healthy_existing_discovery(
    paths: &LocalPaths,
) -> Result<Option<LocalRuntimeDiscovery>> {
    let Some(state) = load_runtime_state(paths)? else {
        return Ok(None);
    };
    if state.lifecycle != LocalRuntimeLifecycle::Ready
        || !state.health.mcp_ready
        || !state.health.observability_ready
        || !state.health.tunnel_process_running
        || !state.health.tunnel_control_plane_ready
        || !state.health.tunnel_ready
    {
        return Ok(None);
    }
    let Some(process) = state.process.as_ref() else {
        return Ok(None);
    };
    if !identity_matches(&SystemProcessInspector, process)? {
        return Ok(None);
    }
    let Some(discovery) = load_runtime_discovery(paths)? else {
        return Ok(None);
    };
    if discovery.runtime_id != state.runtime_id {
        return Ok(None);
    }
    Ok(Some(discovery))
}

pub(super) fn cleanup_partial_start(
    paths: &LocalPaths,
    launchd: &dyn LaunchdController,
) -> Result<()> {
    let inspector = SystemProcessInspector;
    let state = load_runtime_state(paths)?;
    if let Some(state) = state.as_ref()
        && let Some(process) = state.process.as_ref()
        && !signal_process_if_matching(&inspector, process, ProcessSignal::Terminate)?
        && inspector.identity(process.pid)?.is_some()
    {
        bail!(
            "partial-start cleanup found reused Local runtime PID {}; ownership state was preserved",
            process.pid
        );
    }
    launchd.bootout()?;
    match cleanup_stale_tunnel_child(paths, &inspector)? {
        StaleTunnelCleanup::IdentityMismatch(pid) => bail!(
            "partial-start cleanup found reused tunnel PID {pid}; ownership state was preserved"
        ),
        StaleTunnelCleanup::NoRecordedChild
        | StaleTunnelCleanup::SignaledMatchingChild(_)
        | StaleTunnelCleanup::AlreadyExited(_) => {}
    }
    if paths.owned_process_registry_file().exists() {
        let report = terminate_matching_stale_processes(
            &paths.owned_process_registry_file(),
            state.as_ref().map(|state| state.runtime_id.as_str()),
            &inspector,
        )?;
        if report.identity_mismatch > 0
            || report.unresolved_leaderless_groups > 0
            || report.survivors > 0
        {
            bail!(
                "partial-start cleanup is unresolved (identity mismatches: {}, leaderless groups: {}, survivors: {}); ownership state was preserved",
                report.identity_mismatch,
                report.unresolved_leaderless_groups,
                report.survivors,
            );
        }
    }
    paths.clear_runtime_state()?;
    Ok(())
}

fn generate_runtime_token() -> String {
    Alphanumeric.sample_string(&mut rand::rng(), 64)
}

fn format_timestamp(value: OffsetDateTime) -> Result<String> {
    value
        .format(&Rfc3339)
        .context("failed to format Local runtime timestamp")
}

pub(super) fn is_expired(expires_at: Option<&str>, now: OffsetDateTime) -> Result<bool> {
    let Some(expires_at) = expires_at else {
        return Ok(false);
    };
    let expiry = OffsetDateTime::parse(expires_at, &Rfc3339)
        .context("Local runtime expiry timestamp is invalid")?;
    Ok(now >= expiry)
}

async fn shutdown_signal() -> Result<()> {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut terminate =
            signal(SignalKind::terminate()).context("failed to install Local SIGTERM handler")?;
        let mut interrupt =
            signal(SignalKind::interrupt()).context("failed to install Local SIGINT handler")?;
        tokio::select! {
            _ = terminate.recv() => Ok(()),
            _ = interrupt.recv() => Ok(()),
        }
    }
    #[cfg(target_os = "windows")]
    {
        match tokio::signal::ctrl_c().await {
            Ok(()) => Ok(()),
            Err(_) => std::future::pending::<Result<()>>().await,
        }
    }
    #[cfg(not(any(unix, target_os = "windows")))]
    {
        tokio::signal::ctrl_c()
            .await
            .context("failed to wait for Local shutdown signal")
    }
}
