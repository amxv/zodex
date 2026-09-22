use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use tracing::warn;

use super::{
    ProcessIdentity, ProcessInspector, ProcessSignal, RUNTIME_DESCENDANT_DISCOVERY_LIMIT,
    SessionInner, SessionRuntime, TERMINATE_GRACE_PERIOD_MS, process,
};

pub(super) fn reap_exit_code(
    inner: &mut SessionInner,
    inspector: &dyn ProcessInspector,
) -> Result<Option<i32>> {
    if inner.reaped_exit_code.is_none() {
        let mut members =
            inspector.process_group_members(inner.pid, RUNTIME_DESCENDANT_DISCOVERY_LIMIT)?;
        members.retain(|member| member.pid != inner.pid);
        inner.owned_group_members = members;
    } else if !inner.owned_group_members.is_empty() {
        let mut surviving = Vec::with_capacity(inner.owned_group_members.len());
        for member in inner.owned_group_members.drain(..) {
            if process::identity_matches(inspector, &member)? {
                surviving.push(member);
            }
        }
        inner.owned_group_members = surviving;
    }

    if let Some(exit_code) = inner.reaped_exit_code {
        return Ok(Some(exit_code));
    }

    let Some(status) = inner.child.try_wait()? else {
        return Ok(None);
    };

    // A short-lived shell can spawn a background child after the pre-wait
    // group scan and then exit before `try_wait` observes it. Immediately
    // after reaping that owned leader, capture any leaderless members that
    // remain in the same group. If PID==PGID is present with a different birth
    // identity, the numeric group ID was reused by a new leader; reject that
    // snapshot rather than adopting unrelated processes.
    if inner.owned_group_members.is_empty() {
        let mut post_reap_members =
            inspector.process_group_members(inner.pid, RUNTIME_DESCENDANT_DISCOVERY_LIMIT)?;
        let leader_reused = post_reap_members
            .iter()
            .find(|member| member.pid == inner.pid)
            .is_some_and(|member| inner.leader_identity.as_ref() != Some(member));
        if !leader_reused {
            post_reap_members.retain(|member| member.pid != inner.pid);
            inner.owned_group_members = post_reap_members;
        }
    }

    let exit_code = status.code().unwrap_or(-1);
    inner.reaped_exit_code = Some(exit_code);
    Ok(Some(exit_code))
}

pub(super) fn request_termination(inner: &mut SessionInner) {
    if inner.terminate_started_at.is_some() {
        return;
    }

    inner.terminate_started_at = Some(Instant::now());
    #[cfg(any(unix, target_os = "windows"))]
    {
        let _ = process::signal_process_group(inner.pid, ProcessSignal::Terminate);
    }
    #[cfg(not(any(unix, target_os = "windows")))]
    {
        let _ = inner.child.start_kill();
    }
}

pub(super) fn signal_owned_group_members(
    inner: &SessionInner,
    inspector: &dyn ProcessInspector,
    signal: ProcessSignal,
) -> Result<usize> {
    let mut signaled = 0;
    for member in &inner.owned_group_members {
        if process::signal_process_if_matching(inspector, member, signal)? {
            signaled += 1;
        }
    }
    Ok(signaled)
}

pub(super) fn maybe_force_kill(
    inner: &mut SessionInner,
    inspector: &dyn ProcessInspector,
) -> Result<()> {
    let Some(started) = inner.terminate_started_at else {
        return Ok(());
    };
    if inner.force_killed {
        return Ok(());
    }
    if started.elapsed() < Duration::from_millis(TERMINATE_GRACE_PERIOD_MS) {
        return Ok(());
    }

    inner.force_killed = true;
    #[cfg(any(unix, target_os = "windows"))]
    {
        if inner.reaped_exit_code.is_none() {
            let _ = process::signal_process_group(inner.pid, ProcessSignal::Kill);
        }
        let _ = signal_owned_group_members(inner, inspector, ProcessSignal::Kill)?;
    }
    #[cfg(not(any(unix, target_os = "windows")))]
    {
        let _ = inner.child.start_kill();
    }
    Ok(())
}

pub(super) async fn snapshot_descendants_before_termination(
    runtimes: &[Arc<SessionRuntime>],
    limit: usize,
) -> Vec<(Arc<dyn ProcessInspector>, ProcessIdentity)> {
    let mut discovered = Vec::new();
    for runtime in runtimes {
        let inner = runtime.inner.lock().await;
        let pid = inner.pid;
        drop(inner);

        match runtime.process_inspector.descendants(pid, limit) {
            Ok(descendants) => {
                discovered.extend(
                    descendants
                        .into_iter()
                        .map(|descendant| (runtime.process_inspector.clone(), descendant)),
                );
            }
            Err(error) => warn!(
                event = "session_descendant_discovery_failed",
                root_pid = pid,
                error = %error,
            ),
        }
    }
    discovered
}

pub(super) async fn wait_for_session_exits_until(
    runtimes: &[Arc<SessionRuntime>],
    poll_interval: Duration,
    deadline: Instant,
) -> Result<usize> {
    let deadline = tokio::time::Instant::from_std(deadline);
    let first_tick = tokio::time::Instant::now() + poll_interval;
    let mut ticker = tokio::time::interval_at(first_tick, poll_interval);
    // Relative sleep-per-poll loops accumulate scheduler delay and can turn one
    // runtime-wide grace window into several effective windows under load.
    // Keep polling cadence anchored to absolute time and give the shared
    // deadline its own timer so missed ticks never extend shutdown grace.
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        let mut survivors = 0;
        for runtime in runtimes {
            // Shutdown must never queue behind ordinary session work while a
            // wall-clock grace deadline is running. A continuation can hold
            // `inner` briefly while sampling cwd or updating PTY state; under
            // scheduler pressure, awaiting that mutex here can stretch one
            // shared grace window into several effective windows. Treat a
            // contended session as still alive for this poll and retry it on
            // the next absolute tick instead.
            match runtime.inner.try_lock() {
                Ok(mut inner) => {
                    let leader_exited = runtime.reap_and_record_exit(&mut inner)?.is_some();
                    if leader_exited && inner.owned_group_members.is_empty() {
                        let end = super::owned_process_end(&inner)
                            .expect("reaped owned process without final end evidence");
                        runtime.release_process_ownership(end);
                    } else {
                        survivors += 1;
                    }
                }
                Err(_) => survivors += 1,
            }
        }
        if survivors == 0 || tokio::time::Instant::now() >= deadline {
            return Ok(survivors);
        }

        tokio::select! {
            _ = ticker.tick() => {}
            _ = tokio::time::sleep_until(deadline) => return Ok(survivors),
        }
    }
}
