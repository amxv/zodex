use serde_json::json;
use tempfile::tempdir;

use crate::invocation::{
    InvocationContext, InvocationEvidenceRecorder, InvocationOutcome, InvocationStart,
};

use super::{HistoryQuery, LocalHistoryReader, LocalHistoryRuntime, LocalHistoryRuntimeConfig};

#[test]
fn shutdown_marks_unfinished_pty_capture_incomplete_instead_of_leaving_pending() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("history.sqlite3");
    let runtime = LocalHistoryRuntime::open(LocalHistoryRuntimeConfig::new(
        path.clone(),
        "shutdown-capture-test",
        365 * 86_400,
        1024 * 1024 * 1024,
    ))
    .unwrap();
    let context = runtime
        .begin(
            InvocationContext::default(),
            InvocationStart::new(
                "exec_command",
                json!({"cmd":"sleep 30", "workdir":dir.path()}),
            ),
        )
        .unwrap();
    runtime
        .complete(
            &context,
            InvocationOutcome::Success(json!({"status":"running"})),
        )
        .unwrap();

    runtime.shutdown_blocking().unwrap();

    let record = LocalHistoryReader::query(&path, &HistoryQuery::recent(1))
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(record.evidence_state, "complete");
    assert_eq!(record.capture_state, "incomplete");
    assert!(
        record
            .capture_reason
            .as_deref()
            .unwrap_or_default()
            .contains("shutdown ended before PTY capture")
    );
}

#[test]
fn shutdown_without_flush_preserves_more_than_one_admission_batch() {
    use std::sync::Arc;

    use super::store::HistoryStore;

    const CALLS: usize = 200;
    let dir = tempdir().unwrap();
    let path = dir.path().join("history.sqlite3");
    let store = Arc::new(HistoryStore::open(path.clone(), Arc::from("shutdown-batch")).unwrap());
    let runtime = LocalHistoryRuntime::open_with_store(
        LocalHistoryRuntimeConfig::new(
            path.clone(),
            "shutdown-batch",
            365 * 86_400,
            1024 * 1024 * 1024,
        ),
        Some(store.clone()),
    )
    .unwrap();
    // Force >128 admissions and completions to be outstanding together. This
    // is deliberately not a flush test: orderly shutdown owns the whole drain.
    let storage = store.lock_connection();
    let mut contexts = Vec::new();
    for index in 0..CALLS {
        let context = runtime
            .begin(
                InvocationContext::default().with_correlation_id(format!("shutdown-{index}")),
                InvocationStart::new("test_tool", json!({"marker":index})),
            )
            .unwrap();
        runtime
            .complete(
                &context,
                InvocationOutcome::Success(
                    json!({"marker":index,"output":format!("result-{index}")}),
                ),
            )
            .unwrap();
        contexts.push(context);
    }
    assert!(runtime.has_pending_admissions());
    drop(storage);
    runtime.shutdown_blocking().unwrap();

    let rows = LocalHistoryReader::query(
        &path,
        &HistoryQuery {
            last: CALLS + 1,
            include_raw: true,
            ..HistoryQuery::default()
        },
    )
    .unwrap();
    assert_eq!(rows.len(), CALLS);
    for (index, context) in contexts.iter().enumerate() {
        let row = rows
            .iter()
            .find(|row| Some(row.id) == context.invocation_id)
            .unwrap();
        assert_eq!(row.arguments, json!({"marker":index}));
        assert_eq!(
            row.result,
            Some(json!({"marker":index,"output":format!("result-{index}")}))
        );
        assert_eq!(row.evidence_state, "complete");
        assert!(row.completed_at_ms.is_some());
    }
}

#[test]
fn shutdown_releases_exclusive_lease_and_denied_live_clear_preserves_data() {
    use super::clear_local_agent_history;
    use crate::invocation::ProviderCallMetadata;

    let dir = tempdir().unwrap();
    let path = dir.path().join("history.sqlite3");
    let config = |runtime| {
        LocalHistoryRuntimeConfig::new(path.clone(), runtime, 365 * 86_400, 1024 * 1024 * 1024)
    };
    let runtime = LocalHistoryRuntime::open(config("lease-first")).unwrap();
    let context = runtime
        .begin(
            InvocationContext::default()
                .with_provider(ProviderCallMetadata::new("test", "lease-provider")),
            InvocationStart::new("test_tool", json!({"marker":"preserve-me"})),
        )
        .unwrap();
    runtime
        .complete(
            &context,
            InvocationOutcome::Success(json!({"output":"preserve-result"})),
        )
        .unwrap();
    runtime.flush_for_test().unwrap();
    let agent = context.agent_id.as_deref().unwrap();
    let query = HistoryQuery {
        last: 10,
        include_raw: true,
        ..HistoryQuery::default()
    };
    let before = serde_json::to_value(LocalHistoryReader::query(&path, &query).unwrap()).unwrap();
    let connection = rusqlite::Connection::open(&path).unwrap();
    let before_sequence: i64 = connection
        .query_row(
            "SELECT seq FROM sqlite_sequence WHERE name = 'invocations'",
            [],
            |row| row.get(0),
        )
        .unwrap();

    let denied_open = LocalHistoryRuntime::open(config("lease-denied"));
    assert!(
        denied_open.is_err(),
        "a second live writer must not acquire the lease"
    );
    assert!(denied_open.err().unwrap().to_string().contains("in use"));
    let denied_clear = clear_local_agent_history(&path, agent).unwrap_err();
    assert!(denied_clear.to_string().contains("in use"));
    assert_eq!(
        serde_json::to_value(LocalHistoryReader::query(&path, &query).unwrap()).unwrap(),
        before
    );
    assert!(
        LocalHistoryReader::agent_record(&path, agent)
            .unwrap()
            .is_some()
    );
    let after_sequence: i64 = connection
        .query_row(
            "SELECT seq FROM sqlite_sequence WHERE name = 'invocations'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        after_sequence, before_sequence,
        "denied opening must not reserve IDs or mutate the database"
    );
    drop(connection);

    runtime.shutdown_blocking().unwrap();
    // Keep the old runtime Arc alive: orderly shutdown, rather than Drop alone,
    // must release the lease for a new runtime and later explicit clear.
    let next = LocalHistoryRuntime::open(config("lease-next")).unwrap();
    assert!(
        clear_local_agent_history(&path, agent)
            .unwrap_err()
            .to_string()
            .contains("in use")
    );
    next.shutdown_blocking().unwrap();
    assert_eq!(clear_local_agent_history(&path, agent).unwrap(), 1);
    assert!(LocalHistoryReader::query(&path, &query).unwrap().is_empty());
    assert!(
        LocalHistoryReader::agent_record(&path, agent)
            .unwrap()
            .is_none()
    );
}
