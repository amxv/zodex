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
fn startup_retention_preserves_newest_even_when_age_expired() {
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
            .len()
            == 1,
        "startup retention must preserve the newest work even beyond the age budget"
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

fn insert_retention_fixture(connection: &Connection, id: i64, started: i64, completed: i64) {
    connection.execute(
        "INSERT INTO invocations(id,correlation_id,tool_name,args_json,started_at_ms,completed_at_ms,evidence_state,capture_state)
         VALUES(?1,?2,'exec_command','{}',?3,?4,'complete','complete')",
        rusqlite::params![id, format!("fixture-{id}"), started, completed],
    ).unwrap();
}

#[test]
fn size_retention_prunes_one_oldest_start_not_oldest_completion() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("history.sqlite3");
    let store = HistoryStore::open(path.clone(), Arc::from("retention")).unwrap();
    let connection = Connection::open(path).unwrap();
    insert_retention_fixture(&connection, 1, 10, 1000);
    insert_retention_fixture(&connection, 2, 20, 30);
    insert_retention_fixture(&connection, 3, 40, 50);
    assert_eq!(store.delete_oldest_complete_invocation_batch().unwrap(), 1);
    let ids: Vec<i64> = connection
        .prepare("SELECT id FROM invocations ORDER BY id")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(
        ids,
        vec![2, 3],
        "a small overage must not erase a 256-call batch or prune by finish time"
    );
}

#[test]
fn retention_protects_newest_active_and_pending_continuation_chains() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("history.sqlite3");
    let store = HistoryStore::open(path.clone(), Arc::from("retention")).unwrap();
    let connection = Connection::open(path).unwrap();
    for id in 1..=10 {
        insert_retention_fixture(&connection, id, id, id + 100);
    }
    // Old active root 1 plus child 2; pending child 4 protects root 3;
    // newest root 7 and its children 8,9; latest call 10 protects root 5.
    for (child, root) in [(2, 1), (4, 3), (8, 7), (9, 7), (10, 5)] {
        connection.execute("UPDATE invocations SET target_created_by_invocation_id=?2, continuation_kind='poll', tool_name='write_stdin' WHERE id=?1", [child,root]).unwrap();
    }
    connection
        .execute(
            "UPDATE invocations SET evidence_state='pending' WHERE id=4",
            [],
        )
        .unwrap();
    store.run_retention(0, 1, &[1]).unwrap();
    let ids: Vec<i64> = connection
        .prepare("SELECT id FROM invocations ORDER BY id")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(ids, vec![1, 2, 3, 4, 5, 7, 8, 9, 10]);
}

#[test]
fn pinned_wal_copies_do_not_trigger_evidence_overdeletion() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("history.sqlite3");
    let store = HistoryStore::open(path.clone(), Arc::from("retention")).unwrap();
    let writer = Connection::open(&path).unwrap();
    writer
        .execute_batch("PRAGMA wal_autocheckpoint=0;")
        .unwrap();
    for id in 1..=3 {
        insert_retention_fixture(&writer, id, id, id + 1);
        writer
            .execute(
                "INSERT INTO invocation_output_chunks VALUES(?1,0,123,?2)",
                rusqlite::params![id, "x".repeat(16 * 1024)],
            )
            .unwrap();
    }
    writer
        .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
        .unwrap();
    let reader = Connection::open(path).unwrap();
    reader.execute_batch("BEGIN;").unwrap();
    let _: i64 = reader
        .query_row("SELECT COUNT(*) FROM invocations", [], |r| r.get(0))
        .unwrap();
    for n in 0..100 {
        writer
            .execute("UPDATE invocation_output_chunks SET observed_at_ms=?1", [n])
            .unwrap();
    }
    let pages: i64 = writer
        .pragma_query_value(None, "page_count", |r| r.get(0))
        .unwrap();
    let page_size: i64 = writer
        .pragma_query_value(None, "page_size", |r| r.get(0))
        .unwrap();
    let budget = ((pages + 4) * page_size) as u64;
    assert!(store.physical_size().unwrap() > budget * 2);
    store.run_retention(u64::MAX, budget, &[]).unwrap();
    let count: i64 = writer
        .query_row("SELECT COUNT(*) FROM invocations", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        count, 3,
        "pinned WAL versions are not retained logical evidence"
    );
    reader.execute_batch("ROLLBACK;").unwrap();
}

#[test]
fn retention_preserves_newest_orphan_poll_group() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("history.sqlite3");
    let store = HistoryStore::open(path.clone(), Arc::from("retention")).unwrap();
    let connection = Connection::open(path).unwrap();
    for id in 1..=4 {
        insert_retention_fixture(&connection, id, id, id + 1);
    }
    connection.execute("UPDATE invocations SET tool_name='write_stdin',continuation_kind='poll',target_session_handle='orphan' WHERE id IN (2,4)", []).unwrap();
    store.run_retention(0, 1, &[]).unwrap();
    let ids: Vec<i64> = connection
        .prepare("SELECT id FROM invocations ORDER BY id")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(ids, vec![2, 3, 4]);
}

#[test]
fn size_budget_stops_after_smallest_oldest_prefix() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("history.sqlite3");
    let store = HistoryStore::open(path.clone(), Arc::from("retention")).unwrap();
    let connection = Connection::open(path).unwrap();
    for id in 1..=4 {
        insert_retention_fixture(&connection, id, id, id + 1);
        connection
            .execute(
                "INSERT INTO invocation_output_chunks VALUES(?1,0,123,?2)",
                rusqlite::params![id, "x".repeat(16 * 1024)],
            )
            .unwrap();
    }
    let pages: i64 = connection
        .pragma_query_value(None, "page_count", |r| r.get(0))
        .unwrap();
    let free: i64 = connection
        .pragma_query_value(None, "freelist_count", |r| r.get(0))
        .unwrap();
    let page_size: i64 = connection
        .pragma_query_value(None, "page_size", |r| r.get(0))
        .unwrap();
    store
        .run_retention(u64::MAX, ((pages - free - 1) * page_size) as u64, &[])
        .unwrap();
    let ids: Vec<i64> = connection
        .prepare("SELECT id FROM invocations ORDER BY id")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(ids, vec![2, 3, 4]);
}

#[test]
fn storage_full_recovery_retries_after_cache_before_pruning_evidence() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("history.sqlite3");
    let store = HistoryStore::open(path, Arc::from("storage-full")).unwrap();
    {
        let connection = store.lock_connection();
        for id in 1..=3 {
            insert_retention_fixture(&connection, id, id, id + 1);
        }
        connection
            .execute(
                "INSERT INTO presentation_materializations VALUES(3,1,0,'{}',?1)",
                ["x".repeat(64 * 1024)],
            )
            .unwrap();
        let pages: i64 = connection
            .pragma_query_value(None, "page_count", |r| r.get(0))
            .unwrap();
        connection
            .pragma_update(None, "max_page_count", pages)
            .unwrap();
        let error = connection
            .execute(
                "UPDATE invocations SET args_json=?1 WHERE id=3",
                ["x".repeat(32 * 1024)],
            )
            .unwrap_err();
        assert_eq!(
            error.sqlite_error_code(),
            Some(rusqlite::ErrorCode::DiskFull)
        );
    }
    assert!(store.recover_storage_full(&[]).unwrap());
    let connection = store.lock_connection();
    let count: i64 = connection
        .query_row("SELECT COUNT(*) FROM invocations", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        count, 3,
        "cache reclamation must be retried before deleting evidence"
    );
    connection
        .execute(
            "UPDATE invocations SET args_json=?1 WHERE id=3",
            ["x".repeat(32 * 1024)],
        )
        .unwrap();
}

#[test]
fn storage_full_recovery_refreshes_guards_and_prunes_only_one_oldest_safe_unit() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("history.sqlite3");
    let store = HistoryStore::open(path, Arc::from("storage-full")).unwrap();
    {
        let connection = store.lock_connection();
        for id in 1..=4 {
            insert_retention_fixture(&connection, id, id, id + 1);
            connection
                .execute(
                    "INSERT INTO invocation_output_chunks VALUES(?1,0,123,?2)",
                    rusqlite::params![id, "x".repeat(32 * 1024)],
                )
                .unwrap();
        }
        // Parent 1 is active but capture has already been marked incomplete.
        // Fresh active guards, rather than pending flags, must preserve it.
        connection
            .execute(
                "UPDATE invocations SET capture_state='incomplete' WHERE id=1",
                [],
            )
            .unwrap();
        let pages: i64 = connection
            .pragma_query_value(None, "page_count", |r| r.get(0))
            .unwrap();
        connection
            .pragma_update(None, "max_page_count", pages)
            .unwrap();
        let error = connection
            .execute(
                "UPDATE invocations SET args_json=?1 WHERE id=4",
                ["x".repeat(16 * 1024)],
            )
            .unwrap_err();
        assert_eq!(
            error.sqlite_error_code(),
            Some(rusqlite::ErrorCode::DiskFull)
        );
    }
    assert!(store.recover_storage_full(&[1]).unwrap());
    let connection = store.lock_connection();
    let ids: Vec<i64> = connection
        .prepare("SELECT id FROM invocations ORDER BY id")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(ids, vec![1, 3, 4]);
    connection
        .execute(
            "UPDATE invocations SET args_json=?1 WHERE id=4",
            ["x".repeat(16 * 1024)],
        )
        .unwrap();
    drop(connection);
    assert!(
        !store.recover_storage_full(&[1, 3]).unwrap(),
        "all remaining units are active or newest"
    );
}
