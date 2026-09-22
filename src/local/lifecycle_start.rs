use std::ffi::OsString;
use std::path::Path;
#[cfg(target_os = "windows")]
use std::process::{Command, Stdio};
use std::time::Duration;

#[cfg(target_os = "windows")]
use anyhow::Context as _;
use anyhow::Result;

use super::lifecycle::{
    cleanup_partial_start, cleanup_stale_runtime, healthy_existing_discovery, prepare_local_launch,
    wait_for_runtime_ready,
};
use super::lifecycle_artifacts::with_cleanup_error;
use super::lifecycle_lock::LocalLifecycleLock;
use super::{
    LaunchdController, LocalPaths, LocalRuntimeDiscovery, LocalStatusDocument, load_runtime_state,
};

const START_READY_TIMEOUT: Duration = Duration::from_secs(60);
const START_ATTEMPTS: usize = 2;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalStartOutcome {
    pub discovery: LocalRuntimeDiscovery,
    pub already_running: bool,
    pub current_runtime_agent_count: usize,
    pub active_process_count: usize,
}

pub async fn start_via_launchd(
    paths: &LocalPaths,
    executable: &Path,
    requested_start_directory: &Path,
    ttl_seconds: Option<u64>,
    environment: &[(OsString, OsString)],
    launchd: &dyn LaunchdController,
) -> Result<LocalStartOutcome> {
    start_via_launchd_with_timeout(
        paths,
        executable,
        requested_start_directory,
        ttl_seconds,
        environment,
        launchd,
        START_READY_TIMEOUT,
    )
    .await
}

#[cfg(target_os = "windows")]
pub async fn start_via_windows_process(
    paths: &LocalPaths,
    executable: &Path,
    requested_start_directory: &Path,
    ttl_seconds: Option<u64>,
    environment: &[(OsString, OsString)],
) -> Result<LocalStartOutcome> {
    use std::os::windows::process::CommandExt as _;
    use windows_sys::Win32::System::Threading::{CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW};

    let controller = NoopLifecycleController;
    let _lifecycle_lock = LocalLifecycleLock::acquire(paths)?;
    if let Some(discovery) = healthy_existing_discovery(paths)? {
        return outcome(paths, discovery, true);
    }
    cleanup_stale_runtime(paths, &controller)?;

    let mut first_timeout = None;
    for attempt in 0..START_ATTEMPTS {
        let prepared = prepare_local_launch(
            paths,
            executable,
            requested_start_directory,
            ttl_seconds,
            environment,
        )?;
        let spawn = Command::new(executable)
            .args(["local", "__runtime", "--bootstrap"])
            .arg(&prepared.bootstrap_path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW)
            .spawn();
        if let Err(error) = spawn {
            return Err(with_cleanup_error(
                error.context("failed to start detached Zodex Local Windows runtime"),
                cleanup_partial_start(paths, &controller),
            ));
        }

        match wait_for_runtime_ready(paths, &prepared.runtime_id, START_READY_TIMEOUT).await {
            Ok(discovery) => return outcome(paths, discovery, false),
            Err(error) => {
                let retry =
                    attempt == 0 && runtime_never_published_process(paths, &prepared.runtime_id)?;
                if let Err(cleanup_error) = cleanup_partial_start(paths, &controller) {
                    return Err(with_cleanup_error(error, Err(cleanup_error)));
                }
                if retry {
                    first_timeout = Some(error);
                    continue;
                }
                return match first_timeout {
                    Some(first) => Err(error.context(format!(
                        "Local Windows runtime retry also failed after the first process never published readiness: {first:#}"
                    ))),
                    None => Err(error),
                };
            }
        }
    }
    unreachable!("the bounded Local Windows launch attempt loop always returns")
}

#[cfg(target_os = "windows")]
struct NoopLifecycleController;

#[cfg(target_os = "windows")]
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

pub(super) async fn start_via_launchd_with_timeout(
    paths: &LocalPaths,
    executable: &Path,
    requested_start_directory: &Path,
    ttl_seconds: Option<u64>,
    environment: &[(OsString, OsString)],
    launchd: &dyn LaunchdController,
    ready_timeout: Duration,
) -> Result<LocalStartOutcome> {
    let _lifecycle_lock = LocalLifecycleLock::acquire(paths)?;
    if let Some(discovery) = healthy_existing_discovery(paths)? {
        return outcome(paths, discovery, true);
    }
    cleanup_stale_runtime(paths, launchd)?;

    let mut first_timeout = None;
    for attempt in 0..START_ATTEMPTS {
        let prepared = prepare_local_launch(
            paths,
            executable,
            requested_start_directory,
            ttl_seconds,
            environment,
        )?;
        if let Err(error) = launchd.bootstrap(&prepared.plist_path) {
            return Err(with_cleanup_error(
                error.context("failed to bootstrap Zodex Local launchd runtime"),
                cleanup_partial_start(paths, launchd),
            ));
        }
        match wait_for_runtime_ready(paths, &prepared.runtime_id, ready_timeout).await {
            Ok(discovery) => return outcome(paths, discovery, false),
            Err(error) => {
                let retry =
                    attempt == 0 && runtime_never_published_process(paths, &prepared.runtime_id)?;
                if let Err(cleanup_error) = cleanup_partial_start(paths, launchd) {
                    return Err(with_cleanup_error(error, Err(cleanup_error)));
                }
                if retry {
                    first_timeout = Some(error);
                    continue;
                }
                return match first_timeout {
                    Some(first) => Err(error.context(format!(
                        "Local launchd retry also failed after the first job never published a process: {first:#}"
                    ))),
                    None => Err(error),
                };
            }
        }
    }
    unreachable!("the bounded Local launch attempt loop always returns")
}

fn runtime_never_published_process(paths: &LocalPaths, runtime_id: &str) -> Result<bool> {
    Ok(load_runtime_state(paths)?
        .is_some_and(|state| state.runtime_id == runtime_id && state.process.is_none()))
}

fn outcome(
    paths: &LocalPaths,
    discovery: LocalRuntimeDiscovery,
    already_running: bool,
) -> Result<LocalStartOutcome> {
    let status = LocalStatusDocument::inspect(paths)?;
    Ok(LocalStartOutcome {
        discovery,
        already_running,
        current_runtime_agent_count: status.current_runtime_agent_count,
        active_process_count: status.active_process_count,
    })
}
