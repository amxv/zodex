use std::ffi::OsString;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::config::Config;
use crate::invocation::{InvocationContext, ProviderCallMetadata};
use crate::protocol::{CommandStatus, ExecCommandInput, TerminationReason, WriteStdinInput};

use super::{
    OwnedProcess, OwnedProcessEnd, OwnedProcessObserver, ProcessIdentity, SessionManager,
    SessionOrigin, SessionOutputChunk, SessionOutputObserver, SessionRuntimePolicy,
};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use super::{ProcessInspector, SystemProcessInspector};

fn workdir() -> String {
    std::env::current_dir().unwrap().display().to_string()
}

fn local_environment() -> Vec<(OsString, OsString)> {
    [
        ("HOME", "/tmp/zodex-local-home"),
        ("USER", "local-user"),
        ("LOGNAME", "local-logname"),
        ("PATH", "/usr/bin:/bin"),
        ("LOCAL_EXPORTED_VALUE", "captured-value"),
        ("PAGER", "less"),
        ("GIT_PAGER", "less"),
        ("MANPAGER", "less"),
        ("LESS", "-R"),
        ("SYSTEMD_PAGER", "less"),
    ]
    .into_iter()
    .map(|(key, value)| (OsString::from(key), OsString::from(value)))
    .collect()
}

fn local_policy() -> SessionRuntimePolicy {
    SessionRuntimePolicy::local("/bin/sh", local_environment()).unwrap()
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn local_zsh_disables_equals_after_zshenv_without_changing_word_splitting() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(".zshenv"), "setopt EQUALS\n").unwrap();
    let mut environment = local_environment();
    environment.push((OsString::from("ZDOTDIR"), dir.path().as_os_str().to_owned()));
    let manager = SessionManager::with_policy(
        8,
        20_000,
        SessionRuntimePolicy::local("/bin/zsh", environment).unwrap(),
    );
    let output = manager
        .exec_command(
            ExecCommandInput {
                cmd: "print -r -- \"equals=${options[equals]} split=${options[shwordsplit]}\"; echo ====LEDGER====; command -v grep".to_owned(),
                yield_time_ms: Some(2_000),
                workdir: workdir(),
                timeout_ms: None,
            },
            &Config::default(),
            SessionOrigin::direct(),
        )
        .await
        .unwrap();
    assert_eq!(output.status, CommandStatus::Exited);
    assert_eq!(output.exit_code, Some(0));
    assert!(output.output.contains("equals=off split=off"));
    assert!(output.output.contains("====LEDGER===="));
    assert!(output.output.contains("/usr/bin/grep"));
}

#[tokio::test]
async fn local_profile_uses_captured_user_environment_and_overlays_pager_guards() {
    let mgr = SessionManager::with_policy(8, 20_000, local_policy());
    let cfg = Config {
        agent_user: "sprite-user-must-not-leak".to_string(),
        agent_home: "/sprite/home/must-not-leak".to_string(),
        ..Config::default()
    };
    let output = mgr
        .exec_command(
            ExecCommandInput {
                cmd: "printf '%s|%s|%s|%s|%s|%s|%s|%s|%s\\n' \"$HOME\" \"$USER\" \"$LOGNAME\" \"$LOCAL_EXPORTED_VALUE\" \"$PAGER\" \"$GIT_PAGER\" \"$MANPAGER\" \"$LESS\" \"$SYSTEMD_PAGER\"".to_string(),
                yield_time_ms: Some(2_000),
                workdir: workdir(),
                timeout_ms: None,
            },
            &cfg,
            SessionOrigin::direct(),
        )
        .await
        .unwrap();

    assert_eq!(output.status, CommandStatus::Exited);
    assert!(output.output.contains(
        "/tmp/zodex-local-home|local-user|local-logname|captured-value|cat|cat|cat|FRX|cat"
    ));
    assert!(!output.output.contains("sprite-user-must-not-leak"));
    assert!(!output.output.contains("/sprite/home/must-not-leak"));
}

#[derive(Default)]
struct CapturingOutputObserver {
    chunks: Mutex<Vec<SessionOutputChunk>>,
}

impl SessionOutputObserver for CapturingOutputObserver {
    fn observe_output(&self, chunk: SessionOutputChunk) {
        self.chunks.lock().unwrap().push(chunk);
    }
}

#[derive(Default)]
struct CapturingProcessObserver {
    ended: Mutex<Vec<(u64, OwnedProcessEnd)>>,
    group_members: Mutex<Vec<(u64, Vec<ProcessIdentity>)>>,
}

impl OwnedProcessObserver for CapturingProcessObserver {
    fn process_started(&self, _process: &OwnedProcess) -> anyhow::Result<()> {
        Ok(())
    }

    fn process_group_members_updated(
        &self,
        process: &OwnedProcess,
        members: &[ProcessIdentity],
    ) -> anyhow::Result<()> {
        self.group_members
            .lock()
            .unwrap()
            .push((process.internal_session_id, members.to_vec()));
        Ok(())
    }

    fn process_ended(&self, process: &OwnedProcess, end: &OwnedProcessEnd) -> anyhow::Result<()> {
        self.ended
            .lock()
            .unwrap()
            .push((process.internal_session_id, end.clone()));
        Ok(())
    }
}

impl CapturingProcessObserver {
    fn ended(&self) -> Vec<(u64, OwnedProcessEnd)> {
        self.ended.lock().unwrap().clone()
    }

    fn group_members(&self) -> Vec<(u64, Vec<ProcessIdentity>)> {
        self.group_members.lock().unwrap().clone()
    }
}

#[derive(Default)]
struct RejectingProcessObserver {
    ended: Mutex<Vec<OwnedProcessEnd>>,
}

impl OwnedProcessObserver for RejectingProcessObserver {
    fn process_started(&self, _process: &OwnedProcess) -> anyhow::Result<()> {
        anyhow::bail!("injected process ownership admission failure")
    }

    fn process_ended(&self, _process: &OwnedProcess, end: &OwnedProcessEnd) -> anyhow::Result<()> {
        self.ended.lock().unwrap().push(end.clone());
        Ok(())
    }
}

#[tokio::test]
async fn process_end_observer_is_exactly_once_across_reaper_eviction_and_initial_completion() {
    let observer = Arc::new(CapturingProcessObserver::default());
    let policy = local_policy().with_process_observer(observer.clone());
    let mgr = SessionManager::with_policy(1, 20_000, policy);
    let cfg = Config::default();

    let yielded = mgr
        .exec_command(
            ExecCommandInput {
                cmd: "sleep 0.15; exit 7".to_string(),
                yield_time_ms: Some(20),
                workdir: workdir(),
                timeout_ms: Some(60_000),
            },
            &cfg,
            SessionOrigin::direct(),
        )
        .await
        .unwrap();
    assert_eq!(yielded.status, CommandStatus::Running);

    let deadline = Instant::now() + Duration::from_secs(3);
    while observer.ended().is_empty() && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let first_end = observer.ended();
    assert_eq!(first_end.len(), 1);
    assert_eq!(first_end[0].0, 1);
    assert_eq!(first_end[0].1.exit_code, Some(7));
    assert_eq!(
        first_end[0].1.termination_reason,
        Some(TerminationReason::Exit)
    );
    assert!(first_end[0].1.incomplete_reason.is_none());
    assert!(
        first_end[0]
            .1
            .final_cwd
            .as_deref()
            .is_some_and(|cwd| !cwd.is_empty())
    );

    let immediate = mgr
        .exec_command(
            ExecCommandInput {
                cmd: "exit 0".to_string(),
                yield_time_ms: Some(2_000),
                workdir: workdir(),
                timeout_ms: Some(60_000),
            },
            &cfg,
            SessionOrigin::direct(),
        )
        .await
        .unwrap();
    assert_eq!(immediate.status, CommandStatus::Exited);
    let ended = observer.ended();
    assert_eq!(ended.iter().filter(|(id, _)| *id == 1).count(), 1);
    assert_eq!(ended.iter().filter(|(id, _)| *id == 2).count(), 1);
    let immediate_end = ended.iter().find(|(id, _)| *id == 2).unwrap();
    assert_eq!(immediate_end.1.exit_code, Some(0));
    assert_eq!(
        immediate_end.1.termination_reason,
        Some(TerminationReason::Exit)
    );

    mgr.shutdown_all().await.unwrap();
    let ended = observer.ended();
    assert_eq!(ended.iter().filter(|(id, _)| *id == 1).count(), 1);
    assert_eq!(ended.iter().filter(|(id, _)| *id == 2).count(), 1);
}

#[tokio::test]
async fn process_end_observer_matches_kill_timeout_and_shutdown_reasons() {
    let killed_observer = Arc::new(CapturingProcessObserver::default());
    let killed_mgr = SessionManager::with_policy(
        8,
        20_000,
        local_policy().with_process_observer(killed_observer.clone()),
    );
    let cfg = Config::default();
    let running = killed_mgr
        .exec_command(
            ExecCommandInput {
                cmd: "sleep 30".to_string(),
                yield_time_ms: Some(20),
                workdir: workdir(),
                timeout_ms: Some(60_000),
            },
            &cfg,
            SessionOrigin::direct(),
        )
        .await
        .unwrap();
    let killed = killed_mgr
        .write_stdin(
            WriteStdinInput {
                session_handle: running.session_handle.unwrap(),
                chars: None,
                yield_time_ms: Some(6_000),
                kill_process: Some(true),
            },
            &cfg,
        )
        .await
        .unwrap();
    let killed_ends = killed_observer.ended();
    assert_eq!(killed_ends.len(), 1);
    assert_eq!(killed_ends[0].1.exit_code, killed.exit_code);
    assert_eq!(
        killed_ends[0].1.termination_reason,
        Some(TerminationReason::Killed)
    );
    killed_mgr.shutdown_all().await.unwrap();
    assert_eq!(killed_observer.ended().len(), 1);

    let timeout_observer = Arc::new(CapturingProcessObserver::default());
    let timeout_mgr = SessionManager::with_policy(
        8,
        20_000,
        local_policy().with_process_observer(timeout_observer.clone()),
    );
    let timeout_cfg = Config {
        default_exec_timeout_ms: 1_000,
        max_exec_timeout_ms: 1_000,
        ..Config::default()
    };
    let timed_out = timeout_mgr
        .exec_command(
            ExecCommandInput {
                cmd: "sleep 30".to_string(),
                yield_time_ms: Some(2_000),
                workdir: workdir(),
                timeout_ms: Some(1_000),
            },
            &timeout_cfg,
            SessionOrigin::direct(),
        )
        .await
        .unwrap();
    assert_eq!(
        timed_out.termination_reason,
        Some(TerminationReason::Timeout)
    );
    let timeout_ends = timeout_observer.ended();
    assert_eq!(timeout_ends.len(), 1);
    assert_eq!(
        timeout_ends[0].1.termination_reason,
        Some(TerminationReason::Timeout)
    );
    timeout_mgr.shutdown_all().await.unwrap();
    assert_eq!(timeout_observer.ended().len(), 1);

    let shutdown_observer = Arc::new(CapturingProcessObserver::default());
    let shutdown_mgr = SessionManager::with_policy(
        8,
        20_000,
        local_policy()
            .with_process_observer(shutdown_observer.clone())
            .with_shutdown_grace(Duration::from_millis(300)),
    );
    let running = shutdown_mgr
        .exec_command(
            ExecCommandInput {
                cmd: "sleep 30".to_string(),
                yield_time_ms: Some(20),
                workdir: workdir(),
                timeout_ms: Some(60_000),
            },
            &cfg,
            SessionOrigin::direct(),
        )
        .await
        .unwrap();
    assert_eq!(running.status, CommandStatus::Running);
    shutdown_mgr.shutdown_all().await.unwrap();
    let shutdown_ends = shutdown_observer.ended();
    assert_eq!(shutdown_ends.len(), 1);
    assert_eq!(
        shutdown_ends[0].1.termination_reason,
        Some(TerminationReason::Killed)
    );
}

#[tokio::test]
async fn process_start_failure_rolls_back_with_incomplete_end_evidence() {
    let observer = Arc::new(RejectingProcessObserver::default());
    let mgr = SessionManager::with_policy(
        8,
        20_000,
        local_policy().with_process_observer(observer.clone()),
    );
    let error = mgr
        .exec_command(
            ExecCommandInput {
                cmd: "sleep 30".to_string(),
                yield_time_ms: Some(20),
                workdir: workdir(),
                timeout_ms: Some(60_000),
            },
            &Config::default(),
            SessionOrigin::direct(),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("process ownership"));
    let ended = observer.ended.lock().unwrap();
    assert_eq!(ended.len(), 1);
    assert!(ended[0].exit_code.is_none());
    assert!(ended[0].termination_reason.is_none());
    assert!(
        ended[0]
            .final_cwd
            .as_deref()
            .is_some_and(|cwd| !cwd.is_empty())
    );
    assert!(
        ended[0]
            .incomplete_reason
            .as_deref()
            .unwrap()
            .contains("admission failed")
    );
}

#[tokio::test]
async fn output_observer_receives_full_stream_with_invocation_context_while_tool_output_is_bounded()
{
    let observer = Arc::new(CapturingOutputObserver::default());
    let policy = local_policy().with_output_observer(observer.clone());
    let mgr = SessionManager::with_policy(8, 40, policy);
    let cfg = Config::default();
    let invocation = InvocationContext::default()
        .with_correlation_id("session-output-invocation")
        .with_provider(ProviderCallMetadata::new(
            "openai/session",
            "session-output-provider-session",
        ));
    let output = mgr
        .exec_command_with_context(
            ExecCommandInput {
                cmd: "printf '%0500d\\n' 0 | tr '0' x".to_string(),
                yield_time_ms: Some(2_000),
                workdir: workdir(),
                timeout_ms: None,
            },
            &cfg,
            SessionOrigin::mcp(None),
            invocation.clone(),
        )
        .await
        .unwrap();

    assert!(output.output.contains("full output saved to"));
    let output_file = output
        .output_file
        .as_deref()
        .expect("oversized model output should be spooled to a file");
    assert!(output.output_chars.is_some_and(|chars| chars >= 500));
    assert_eq!(output.output_lines, Some(1));
    let spooled = std::fs::read_to_string(output_file).unwrap();
    assert!(spooled.matches('x').count() >= 500);
    let _ = std::fs::remove_file(output_file);
    let chunks = observer.chunks.lock().unwrap();
    let full = chunks
        .iter()
        .map(|chunk| chunk.text.as_str())
        .collect::<String>();
    assert!(
        full.matches('x').count() >= 500,
        "full output was not observed: {full:?}"
    );
    assert!(!chunks.is_empty());
    assert!(chunks.iter().all(|chunk| chunk.invocation == invocation));
    assert!(
        chunks
            .iter()
            .all(|chunk| chunk.session_handle.len() == super::SESSION_HANDLE_LEN)
    );
}

#[tokio::test]
async fn whole_runtime_shutdown_preempts_a_long_yield_write_and_closes_admission() {
    let dir = tempfile::tempdir().unwrap();
    let shell_pid_file = dir.path().join("interactive-shell.pid");
    let policy = local_policy().with_shutdown_grace(Duration::from_millis(250));
    let mgr = Arc::new(SessionManager::with_policy(8, 20_000, policy));
    let cfg = Arc::new(Config::default());
    let started = mgr
        .exec_command(
            ExecCommandInput {
                cmd: "bash --noprofile --norc".to_string(),
                yield_time_ms: Some(50),
                workdir: workdir(),
                timeout_ms: Some(60_000),
            },
            &cfg,
            SessionOrigin::direct(),
        )
        .await
        .unwrap();
    let handle = started.session_handle.unwrap();

    let poll_mgr = mgr.clone();
    let poll_cfg = cfg.clone();
    let shell_pid_file_for_poll = shell_pid_file.clone();
    let poll = tokio::spawn(async move {
        poll_mgr
            .write_stdin(
                WriteStdinInput {
                    session_handle: handle,
                    chars: Some(format!(
                        "printf '%s\\n' \"$$\" > '{}'; sleep 30\n",
                        shell_pid_file_for_poll.display()
                    )),
                    yield_time_ms: Some(60_000),
                    kill_process: Some(false),
                },
                &poll_cfg,
            )
            .await
    });

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let interactive_shell_pid = {
        let deadline = Instant::now() + Duration::from_secs(2);
        while !shell_pid_file.exists() && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let pid = std::fs::read_to_string(&shell_pid_file)
            .expect("interactive shell should publish its pid before shutdown")
            .trim()
            .parse::<i32>()
            .unwrap();
        let inspector = SystemProcessInspector;
        assert!(
            inspector.identity(pid).unwrap().is_some(),
            "interactive shell fixture must be alive before shutdown"
        );
        pid
    };

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    tokio::time::sleep(Duration::from_millis(100)).await;

    let began = Instant::now();
    let shutdown = mgr.shutdown_all().await.unwrap();
    assert!(
        began.elapsed() < Duration::from_secs(2),
        "runtime shutdown waited behind normal write_stdin serialization: {:?}",
        began.elapsed()
    );
    assert_eq!(shutdown.sessions_signaled, 1);
    assert!(!mgr.accepting_new_sessions());

    let terminal = tokio::time::timeout(Duration::from_secs(2), poll)
        .await
        .expect("in-flight write should converge promptly")
        .expect("poll task should join")
        .expect("poll should return truthful terminal output");
    assert_eq!(terminal.status, CommandStatus::Exited);
    assert_eq!(terminal.termination_reason, Some(TerminationReason::Killed));

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        let inspector = SystemProcessInspector;
        let deadline = Instant::now() + Duration::from_secs(1);
        while inspector.identity(interactive_shell_pid).unwrap().is_some()
            && Instant::now() < deadline
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(
            inspector.identity(interactive_shell_pid).unwrap().is_none(),
            "whole-runtime shutdown left interactive shell {interactive_shell_pid} alive after its wrapper exited"
        );
    }

    let error = mgr
        .exec_command(
            ExecCommandInput {
                cmd: "echo must-not-run".to_string(),
                yield_time_ms: Some(100),
                workdir: workdir(),
                timeout_ms: None,
            },
            &cfg,
            SessionOrigin::direct(),
        )
        .await
        .expect_err("shutdown admission must stay closed");
    assert!(error.to_string().contains("stopping"));
}

#[tokio::test]
async fn multi_session_shutdown_uses_one_shared_grace_window() {
    let grace = Duration::from_secs(1);
    let policy = local_policy().with_shutdown_grace(grace);
    let mgr = SessionManager::with_policy(8, 20_000, policy);
    let cfg = Config::default();

    for _ in 0..3 {
        let started = mgr
            .exec_command(
                ExecCommandInput {
                    cmd: "trap '' TERM; while :; do sleep 1; done".to_string(),
                    yield_time_ms: Some(50),
                    workdir: workdir(),
                    timeout_ms: Some(60_000),
                },
                &cfg,
                SessionOrigin::direct(),
            )
            .await
            .unwrap();
        assert_eq!(started.status, CommandStatus::Running);
    }

    let began = Instant::now();
    let shutdown = mgr.shutdown_all().await.unwrap();
    let elapsed = began.elapsed();
    assert_eq!(shutdown.sessions_signaled, 3);
    assert_eq!(shutdown.sessions_force_killed, 3);
    assert!(
        elapsed < Duration::from_millis(2_800),
        "three sessions appear to have consumed serial grace windows: {elapsed:?}"
    );
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[tokio::test]
async fn whole_runtime_shutdown_keeps_background_job_owned_after_shell_exit() {
    let dir = tempfile::tempdir().unwrap();
    let background_pid_file = dir.path().join("background.pid");
    let observer = Arc::new(CapturingProcessObserver::default());
    let policy = local_policy()
        .with_process_observer(observer.clone())
        .with_shutdown_grace(Duration::from_millis(300));
    let mgr = SessionManager::with_policy(8, 20_000, policy);
    let cfg = Config::default();

    let output = mgr
        .exec_command(
            ExecCommandInput {
                cmd: format!(
                    "sleep 60 & printf '%s\\n' \"$!\" > '{}'",
                    background_pid_file.display()
                ),
                yield_time_ms: Some(2_000),
                workdir: dir.path().display().to_string(),
                timeout_ms: Some(60_000),
            },
            &cfg,
            SessionOrigin::direct(),
        )
        .await
        .unwrap();
    assert_eq!(output.status, CommandStatus::Exited);

    let background_pid = std::fs::read_to_string(&background_pid_file)
        .expect("background fixture should publish its pid")
        .trim()
        .parse::<i32>()
        .unwrap();
    let inspector = SystemProcessInspector;
    assert!(
        inspector.identity(background_pid).unwrap().is_some(),
        "background job must still be alive after its shell leader exits"
    );
    let background_pgid = unsafe { nix::libc::getpgid(background_pid) };
    assert!(
        background_pgid > 0,
        "background job should retain a process group"
    );
    let visible_group_members = inspector
        .process_group_members(background_pgid, 32)
        .unwrap();
    assert!(
        visible_group_members
            .iter()
            .any(|member| member.pid == background_pid),
        "process-group inspection must see the live background job"
    );
    let runtime = mgr
        .sessions
        .read()
        .await
        .values()
        .next()
        .expect("retained session runtime")
        .clone();
    assert!(
        runtime
            .inner
            .lock()
            .await
            .owned_group_members
            .iter()
            .any(|member| member.pid == background_pid),
        "session runtime must retain the stable background member identity after leader reap"
    );
    assert!(
        observer
            .group_members()
            .iter()
            .any(|(_, members)| { members.iter().any(|member| member.pid == background_pid) }),
        "process observer must persist the stable background member identity"
    );
    let counts = mgr.session_counts().await.unwrap();
    assert_eq!(
        counts.running, 1,
        "an exited shell with a live owned background process must remain non-evictable"
    );
    assert!(
        observer.ended().is_empty(),
        "process ownership must not end merely because the shell leader exited while an owned group member remains"
    );

    let shutdown = mgr.shutdown_all().await.unwrap();
    assert_eq!(shutdown.sessions_signaled, 1);
    let ended = observer.ended();
    assert_eq!(ended.len(), 1);
    assert_eq!(
        ended[0].1.termination_reason,
        Some(TerminationReason::Killed)
    );

    let deadline = Instant::now() + Duration::from_secs(1);
    while inspector.identity(background_pid).unwrap().is_some() && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        inspector.identity(background_pid).unwrap().is_none(),
        "whole-runtime shutdown left background job {background_pid} alive after its shell leader exited"
    );
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn whole_runtime_shutdown_sweeps_discoverable_descendants_outside_the_process_group() {
    let dir = tempfile::tempdir().unwrap();
    let pid_file = dir.path().join("escaped.pid");
    let policy = local_policy().with_shutdown_grace(Duration::from_millis(300));
    let mgr = SessionManager::with_policy(8, 20_000, policy);
    let cfg = Config::default();
    let started = mgr
        .exec_command(
            ExecCommandInput {
                cmd: format!(
                    "trap '' TERM; setsid sh -c 'trap \"\" TERM; echo $$ > {}; while :; do sleep 1; done' & while :; do sleep 1; done",
                    pid_file.display()
                ),
                yield_time_ms: Some(100),
                workdir: dir.path().display().to_string(),
                timeout_ms: Some(60_000),
            },
            &cfg,
            SessionOrigin::direct(),
        )
        .await
        .unwrap();
    assert_eq!(started.status, CommandStatus::Running);

    let deadline = Instant::now() + Duration::from_secs(2);
    while !pid_file.exists() && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let escaped_pid = std::fs::read_to_string(&pid_file)
        .unwrap()
        .trim()
        .parse::<i32>()
        .unwrap();
    let inspector = SystemProcessInspector;
    assert!(inspector.identity(escaped_pid).unwrap().is_some());

    let shutdown = mgr.shutdown_all().await.unwrap();
    assert!(shutdown.descendants_signaled >= 1);
    assert!(shutdown.descendants_force_killed >= 1);

    let deadline = Instant::now() + Duration::from_secs(2);
    while inspector.identity(escaped_pid).unwrap().is_some() && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        inspector.identity(escaped_pid).unwrap().is_none(),
        "discoverable escaped descendant {escaped_pid} survived whole-runtime shutdown"
    );
}
