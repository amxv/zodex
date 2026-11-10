use std::ffi::OsString;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::json;
use tempfile::tempdir;

use crate::config::Config;
use crate::invocation::{
    InvocationContext, InvocationEvidenceRecorder, InvocationOutcome, InvocationStart,
    ProviderCallMetadata,
};
use crate::protocol::{CommandStatus, ExecCommandInput};
use crate::session::{SessionManager, SessionOrigin, SessionRuntimePolicy};

use super::query::{HistoryQuery, LocalHistoryReader};
use super::store::HistoryStore;
use super::worker::{LocalHistoryRuntime, LocalHistoryRuntimeConfig};

fn provider_context(correlation_id: &str) -> InvocationContext {
    InvocationContext::default()
        .with_correlation_id(correlation_id)
        .with_provider(ProviderCallMetadata::new(
            "openai/session",
            "lifecycle-provider-session",
        ))
}

fn exec_start(workdir: &std::path::Path, command: &str) -> InvocationStart {
    InvocationStart::new(
        "exec_command",
        json!({
            "cmd": command,
            "workdir": workdir,
            "yield_time_ms": 20,
        }),
    )
}

fn local_environment(home: &std::path::Path) -> Vec<(OsString, OsString)> {
    vec![
        ("HOME".into(), home.as_os_str().to_os_string()),
        ("PATH".into(), "/usr/bin:/bin".into()),
    ]
}

#[tokio::test]
async fn background_reaper_persists_final_process_truth_without_rewriting_running_result() {
    let dir = tempdir().unwrap();
    let database = dir.path().join("history.sqlite3");
    let history = LocalHistoryRuntime::open(LocalHistoryRuntimeConfig::new(
        database.clone(),
        "runtime-reaper-lifecycle",
        60 * 60,
        64 * 1024 * 1024,
    ))
    .unwrap();
    let command = "sleep 0.15; exit 7";
    let context = history
        .begin(
            provider_context("background-reaper"),
            exec_start(dir.path(), command),
        )
        .unwrap();
    let invocation_id = context.invocation_id.unwrap();
    let policy = SessionRuntimePolicy::local("/bin/sh", local_environment(dir.path()))
        .unwrap()
        .with_process_observer(history.clone())
        .with_output_observer(history.clone());
    let manager = SessionManager::with_policy(8, 20_000, policy);
    let returned = manager
        .exec_command_with_context(
            ExecCommandInput {
                cmd: command.to_string(),
                yield_time_ms: Some(20),
                workdir: dir.path().display().to_string(),
                timeout_ms: Some(60_000),
            },
            &Config::default(),
            SessionOrigin::mcp(None),
            context.clone(),
        )
        .await
        .unwrap();
    assert_eq!(returned.status, CommandStatus::Running);
    assert_eq!(history.active_process_count(), 1);

    history
        .complete(
            &context,
            InvocationOutcome::Success(serde_json::to_value(&returned).unwrap()),
        )
        .unwrap();
    history.flush_for_test().unwrap();

    let deadline = Instant::now() + Duration::from_secs(3);
    while history.active_process_count() != 0 && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        history.active_process_count(),
        0,
        "background reaper never published process end"
    );

    // Live activity changes at observation time; persistence is deliberately async.
    history.flush_for_test().unwrap();
    let record = LocalHistoryReader::query(
        &database,
        &HistoryQuery {
            last: 1,
            invocation_id: Some(invocation_id),
            include_raw: true,
            ..HistoryQuery::default()
        },
    )
    .unwrap()
    .pop()
    .unwrap();
    assert_eq!(record.result_status.as_deref(), Some("running"));
    assert_eq!(record.result.as_ref().unwrap()["status"], "running");
    assert!(record.result_exit_code.is_none());
    assert!(record.result_termination_reason.is_none());
    assert_eq!(record.process_state.as_deref(), Some("exited"));
    assert_eq!(record.process_exit_code, Some(7));
    assert_eq!(record.process_termination_reason.as_deref(), Some("exit"));
    assert!(record.process_started_at_ms.is_some());
    assert!(record.process_ended_at_ms.is_some());
    assert!(record.process_updated_at_ms >= record.process_ended_at_ms);
    assert!(
        record
            .process_cwd
            .as_deref()
            .is_some_and(|cwd| !cwd.is_empty())
    );

    manager.shutdown_all().await.unwrap();
    history.shutdown_blocking().unwrap();
}

#[test]
fn reopening_history_marks_prior_running_process_lifecycle_incomplete_without_fabrication() {
    let dir = tempdir().unwrap();
    let database = dir.path().join("history.sqlite3");
    let store = HistoryStore::open(database.clone(), Arc::from("runtime-a")).unwrap();
    let context = store
        .begin(
            provider_context("interrupted-process"),
            exec_start(dir.path(), "sleep 30"),
        )
        .unwrap();
    let invocation_id = context.invocation_id.unwrap();
    store
        .complete(
            &context,
            InvocationOutcome::Success(json!({
                "summary": "running",
                "output": "",
                "status": "running",
                "cwd": dir.path(),
                "session_handle": "interrupted-handle",
                "exit_code": null,
                "termination_reason": null,
            })),
        )
        .unwrap();
    store.process_started(invocation_id).unwrap();
    drop(store);

    drop(HistoryStore::open(database.clone(), Arc::from("runtime-b")).unwrap());
    let record = LocalHistoryReader::query(
        &database,
        &HistoryQuery {
            last: 1,
            invocation_id: Some(invocation_id),
            ..HistoryQuery::default()
        },
    )
    .unwrap()
    .pop()
    .unwrap();
    assert_eq!(record.result_status.as_deref(), Some("running"));
    assert_eq!(record.process_state.as_deref(), Some("incomplete"));
    assert!(record.process_ended_at_ms.is_none());
    assert!(record.process_exit_code.is_none());
    assert!(record.process_termination_reason.is_none());
    assert!(
        record
            .process_incomplete_reason
            .as_deref()
            .unwrap()
            .contains("previous Local runtime ended")
    );
}
