use std::sync::Arc;

use rusqlite::Connection;
use serde_json::json;
use tempfile::tempdir;

use crate::invocation::{
    InvocationContext, InvocationOutcome, InvocationStart, ProviderCallMetadata,
};

use super::query::{HistoryQuery, LocalHistoryReader};
use super::retention::SIZE_RETENTION_BATCH_LIMIT;
use super::store::{HistoryStore, OutputEvent};
use super::worker::{LocalHistoryRuntime, LocalHistoryRuntimeConfig};

#[test]
fn startup_retention_finishes_before_runtime_is_returned() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("history.sqlite3");
    let store = HistoryStore::open(path.clone(), Arc::from("runtime-old")).unwrap();
    let context = store
        .begin(
            InvocationContext::default()
                .with_correlation_id("expired-before-start")
                .with_provider(ProviderCallMetadata::new("openai/session", "provider-old")),
            InvocationStart::new("exec_command", json!({"cmd":"true","workdir":dir.path()})),
        )
        .unwrap();
    store
        .complete(
            &context,
            InvocationOutcome::Success(json!({"status":"exited","exit_code":0})),
        )
        .unwrap();
    drop(store);

    let connection = Connection::open(&path).unwrap();
    connection
        .execute(
            "UPDATE invocations SET started_at_ms = 1, completed_at_ms = 2",
            [],
        )
        .unwrap();
    drop(connection);

    let runtime = LocalHistoryRuntime::open(LocalHistoryRuntimeConfig::new(
        path.clone(),
        "runtime-new",
        1,
        u64::MAX,
    ))
    .unwrap();
    assert!(
        LocalHistoryReader::query(&path, &HistoryQuery::recent(10))
            .unwrap()
            .is_empty(),
        "startup retention must complete before the runtime can admit new work"
    );
    runtime.shutdown_blocking().unwrap();
}

#[test]
fn retention_without_expired_invocations_preserves_existing_summaries() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("history.sqlite3");
    let store = HistoryStore::open(path.clone(), Arc::from("runtime-old")).unwrap();
    let context = store
        .begin(
            InvocationContext::default()
                .with_correlation_id("retained-before-start")
                .with_provider(ProviderCallMetadata::new(
                    "openai/session",
                    "provider-retained",
                )),
            InvocationStart::new(
                "apply_patch",
                json!({"patch":"fixture","workdir":dir.path()}),
            ),
        )
        .unwrap();
    store
        .complete(&context, InvocationOutcome::Success(json!({"ok":true})))
        .unwrap();
    let records = LocalHistoryReader::query(&path, &HistoryQuery::recent(10)).unwrap();
    let summaries_before = LocalHistoryReader::agent_summaries(&path, &records).unwrap();
    assert_eq!(summaries_before.len(), 1);
    assert_eq!(summaries_before[0].workdirs.len(), 1);
    drop(store);

    let connection = Connection::open(&path).unwrap();
    // Make summary churn fail visibly without relying on timing or database
    // size. Neither a cold start nor a no-op maintenance pass needs to rewrite
    // summaries for retained invocations.
    connection
        .execute_batch(
            "CREATE TRIGGER reject_summary_delete BEFORE DELETE ON agent_workdirs
             BEGIN SELECT RAISE(ABORT, 'unexpected summary rewrite'); END;
             CREATE TRIGGER reject_summary_insert BEFORE INSERT ON agent_workdirs
             BEGIN SELECT RAISE(ABORT, 'unexpected summary rewrite'); END;",
        )
        .unwrap();
    drop(connection);

    let runtime = LocalHistoryRuntime::open(LocalHistoryRuntimeConfig::new(
        path.clone(),
        "runtime-new",
        u64::MAX,
        u64::MAX,
    ))
    .unwrap();
    assert!(
        LocalHistoryReader::status(&path)
            .unwrap()
            .last_retention_error
            .is_none()
    );
    runtime.run_retention_now(u64::MAX, u64::MAX).unwrap();
    assert_eq!(
        LocalHistoryReader::agent_summaries(&path, &records).unwrap(),
        summaries_before
    );
    assert_eq!(
        LocalHistoryReader::query(&path, &HistoryQuery::recent(10))
            .unwrap()
            .len(),
        1
    );
    runtime.shutdown_blocking().unwrap();
}

#[test]
fn size_retention_deletes_old_complete_units_in_bounded_batches() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("history.sqlite3");
    let store = HistoryStore::open(path.clone(), Arc::from("runtime-batch")).unwrap();
    let invocation_count = usize::try_from(SIZE_RETENTION_BATCH_LIMIT).unwrap() + 44;
    let mut invocation_ids = Vec::with_capacity(invocation_count);

    for index in 0..invocation_count {
        let marker = format!("retention-batch-{index}");
        let context = store
            .begin(
                InvocationContext::default()
                    .with_correlation_id(marker.clone())
                    .with_provider(ProviderCallMetadata::new(
                        "openai/session",
                        "batch-provider",
                    )),
                InvocationStart::new("exec_command", json!({"cmd":marker,"workdir":dir.path()})),
            )
            .unwrap();
        let invocation_id = context.invocation_id.expect("stored invocation id");
        invocation_ids.push(invocation_id);
        store
            .persist_output_batch(&[OutputEvent::Complete {
                invocation_id,
                agent_id: None,
            }])
            .unwrap();
        store
            .complete(
                &context,
                InvocationOutcome::Success(json!({"status":"exited","exit_code":0})),
            )
            .unwrap();
    }

    let deleted = store.delete_oldest_complete_invocation_batch().unwrap();
    assert_eq!(deleted, SIZE_RETENTION_BATCH_LIMIT as usize);

    let retained =
        LocalHistoryReader::query(&path, &HistoryQuery::recent(invocation_count)).unwrap();
    assert_eq!(retained.len(), invocation_count - deleted);
    assert!(
        retained
            .iter()
            .any(|record| record.id == *invocation_ids.last().unwrap()),
        "the newest complete invocation must survive bounded size retention"
    );
}
