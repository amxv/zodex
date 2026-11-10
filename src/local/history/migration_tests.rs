use std::sync::Arc;

use rusqlite::Connection;
use tempfile::tempdir;

use super::query::{HistoryQuery, LocalHistoryReader};
use super::schema::{
    HISTORY_SCHEMA_VERSION, SCHEMA_V1, SCHEMA_V2, SCHEMA_V3, SCHEMA_V4, SCHEMA_V5, SCHEMA_V6,
};
use super::store::HistoryStore;

#[test]
fn schema_v1_migrates_forward_to_current_file_evidence_schema() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("history.sqlite3");
    let connection = Connection::open(&path).unwrap();
    connection
        .pragma_update(None, "auto_vacuum", "INCREMENTAL")
        .unwrap();
    let mode: String = connection
        .query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))
        .unwrap();
    assert!(mode.eq_ignore_ascii_case("wal"));
    connection.execute_batch(SCHEMA_V1).unwrap();
    connection.pragma_update(None, "user_version", 1).unwrap();
    drop(connection);

    assert!(
        LocalHistoryReader::query(&path, &HistoryQuery::recent(10))
            .unwrap()
            .is_empty(),
        "new binaries must keep stopped schema-v1 history readable before the writer migrates it"
    );

    let store = HistoryStore::open(path.clone(), Arc::from("migration-runtime")).unwrap();
    drop(store);
    let connection = Connection::open(path).unwrap();
    let version: u32 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, HISTORY_SCHEMA_VERSION);
    let table_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'invocation_file_evidence'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(table_count, 1);
    let materialization_table_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'presentation_materializations'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(materialization_table_count, 1);
    let orphan_poll_index_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master
             WHERE type = 'index' AND name = 'invocations_orphan_poll_handle_started_idx'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(orphan_poll_index_count, 1);
}

#[test]
fn schema_v2_migrates_forward_to_destination_before_evidence() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("history.sqlite3");
    let connection = Connection::open(&path).unwrap();
    connection
        .pragma_update(None, "auto_vacuum", "INCREMENTAL")
        .unwrap();
    let _: String = connection
        .query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))
        .unwrap();
    connection.execute_batch(SCHEMA_V1).unwrap();
    connection.execute_batch(SCHEMA_V2).unwrap();
    connection.pragma_update(None, "user_version", 2).unwrap();
    drop(connection);

    assert!(
        LocalHistoryReader::query(&path, &HistoryQuery::recent(10))
            .unwrap()
            .is_empty()
    );
    let store = HistoryStore::open(path.clone(), Arc::from("migration-runtime")).unwrap();
    drop(store);
    let connection = Connection::open(path).unwrap();
    let version: u32 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, HISTORY_SCHEMA_VERSION);
    let destination_column_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('invocation_file_evidence') WHERE name = 'destination_before_state'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(destination_column_count, 1);
}

#[test]
fn schema_v3_migrates_continuation_identity_and_keeps_ambiguous_parents_nullable() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("history.sqlite3");
    let connection = Connection::open(&path).unwrap();
    connection
        .pragma_update(None, "auto_vacuum", "INCREMENTAL")
        .unwrap();
    let _: String = connection
        .query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))
        .unwrap();
    connection.execute_batch(SCHEMA_V1).unwrap();
    connection.execute_batch(SCHEMA_V2).unwrap();
    connection.execute_batch(SCHEMA_V3).unwrap();
    connection.pragma_update(None, "user_version", 3).unwrap();

    let insert_parent = |correlation: &str, handle: &str, started_at_ms: i64| {
        connection
            .execute(
                "INSERT INTO invocations(
                    correlation_id, tool_name, args_json, started_at_ms, completed_at_ms,
                    outcome_kind, result_json, evidence_state, capture_state,
                    result_status, result_session_handle
                 ) VALUES (?1, 'exec_command', '{}', ?2, ?2, 'success',
                           '{\"status\":\"running\"}', 'complete', 'complete', 'running', ?3)",
                rusqlite::params![correlation, started_at_ms, handle],
            )
            .unwrap();
        connection.last_insert_rowid()
    };
    let insert_continuation =
        |correlation: &str, handle: &str, args_json: &str, started_at_ms: i64| {
            connection
                .execute(
                    "INSERT INTO invocations(
                        correlation_id, tool_name, args_json, started_at_ms, completed_at_ms,
                        outcome_kind, result_json, evidence_state, capture_state,
                        target_session_handle
                     ) VALUES (?1, 'write_stdin', ?2, ?3, ?3, 'success', '{}',
                               'complete', 'not_applicable', ?4)",
                    rusqlite::params![correlation, args_json, started_at_ms, handle],
                )
                .unwrap();
            connection.last_insert_rowid()
        };

    let unique_parent = insert_parent("unique-parent", "unique-handle", 10);
    let poll = insert_continuation(
        "poll",
        "unique-handle",
        r#"{"session_handle":"unique-handle","chars":null,"kill_process":false}"#,
        20,
    );
    let stdin = insert_continuation(
        "stdin",
        "unique-handle",
        r#"{"session_handle":"unique-handle","chars":"y\n","kill_process":false}"#,
        30,
    );
    let kill = insert_continuation(
        "kill",
        "unique-handle",
        r#"{"session_handle":"unique-handle","chars":"ignored","kill_process":true}"#,
        40,
    );
    insert_parent("ambiguous-parent-a", "ambiguous-handle", 50);
    insert_parent("ambiguous-parent-b", "ambiguous-handle", 60);
    let ambiguous = insert_continuation(
        "ambiguous-child",
        "ambiguous-handle",
        r#"{"session_handle":"ambiguous-handle","chars":null,"kill_process":false}"#,
        70,
    );
    drop(connection);

    let stopped_v3 = LocalHistoryReader::query(&path, &HistoryQuery::recent(20)).unwrap();
    let stopped_poll = stopped_v3.iter().find(|record| record.id == poll).unwrap();
    assert!(stopped_poll.target_created_by_invocation_id.is_none());
    assert!(stopped_poll.continuation_kind.is_none());
    assert!(stopped_poll.process_state.is_none());

    let store = HistoryStore::open(path.clone(), Arc::from("migration-runtime")).unwrap();
    drop(store);
    let migrated = LocalHistoryReader::query(&path, &HistoryQuery::recent(20)).unwrap();
    for (id, kind) in [(poll, "poll"), (stdin, "stdin"), (kill, "kill")] {
        let record = migrated.iter().find(|record| record.id == id).unwrap();
        assert_eq!(record.target_created_by_invocation_id, Some(unique_parent));
        assert_eq!(record.continuation_kind.as_deref(), Some(kind));
    }
    let ambiguous = migrated
        .iter()
        .find(|record| record.id == ambiguous)
        .unwrap();
    assert_eq!(ambiguous.continuation_kind.as_deref(), Some("poll"));
    assert!(ambiguous.target_created_by_invocation_id.is_none());

    let connection = Connection::open(&path).unwrap();
    let version: u32 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, HISTORY_SCHEMA_VERSION);
    let index_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master
             WHERE type = 'index' AND name IN (
                'invocations_continuation_parent_kind_idx',
                'invocations_completed_changed_idx',
                'invocations_process_updated_idx'
             )",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(index_count, 3);
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .unwrap();
    connection
        .execute("DELETE FROM invocations WHERE id = ?1", [unique_parent])
        .unwrap();
    let retained_parent: Option<i64> = connection
        .query_row(
            "SELECT target_created_by_invocation_id FROM invocations WHERE id = ?1",
            [poll],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        retained_parent.is_none(),
        "retention/deletion must null continuation parent identity rather than leave a dangling FK"
    );
}

#[test]
fn schema_v5_invalidates_materialized_presentations_when_invocation_truth_changes() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("history.sqlite3");
    let store = HistoryStore::open(path.clone(), Arc::from("materialization-runtime")).unwrap();
    drop(store);
    let connection = Connection::open(path).unwrap();
    connection
        .execute(
            "INSERT INTO invocations(
                correlation_id, tool_name, args_json, started_at_ms,
                evidence_state, capture_state
             ) VALUES ('materialized-root', 'exec_command', '{}', 10, 'complete', 'complete')",
            [],
        )
        .unwrap();
    let root_id = connection.last_insert_rowid();
    connection
        .execute(
            "INSERT INTO presentation_materializations(
                root_invocation_id, presentation_version, materialized_at_ms, summary_json, full_json
             ) VALUES (?1, 3, 20, '{}', NULL)",
            [root_id],
        )
        .unwrap();
    connection
        .execute(
            "UPDATE invocations SET result_status = 'running' WHERE id = ?1",
            [root_id],
        )
        .unwrap();
    let remaining: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM presentation_materializations WHERE root_invocation_id = ?1",
            [root_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(remaining, 0);
}

#[test]
fn schema_v4_migrates_to_materialization_cache_without_eager_backfill() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("history.sqlite3");
    let connection = Connection::open(&path).unwrap();
    connection
        .pragma_update(None, "auto_vacuum", "INCREMENTAL")
        .unwrap();
    let _: String = connection
        .query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))
        .unwrap();
    for schema in [SCHEMA_V1, SCHEMA_V2, SCHEMA_V3, SCHEMA_V4] {
        connection.execute_batch(schema).unwrap();
    }
    connection
        .execute(
            "INSERT INTO invocations(
                correlation_id, tool_name, args_json, started_at_ms, completed_at_ms,
                outcome_kind, result_json, evidence_state, capture_state
             ) VALUES ('legacy-v4', 'test_tool', '{}', 1, 2, 'success', '{}',
                       'complete', 'not_applicable')",
            [],
        )
        .unwrap();
    connection.pragma_update(None, "user_version", 4).unwrap();
    drop(connection);

    drop(HistoryStore::open(path.clone(), Arc::from("migration-runtime")).unwrap());
    let connection = Connection::open(path).unwrap();
    let version: u32 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, HISTORY_SCHEMA_VERSION);
    let materialized: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM presentation_materializations",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        materialized, 0,
        "schema migration must not build historical diffs"
    );
}

#[test]
fn schema_v5_migrates_to_context_delivery_state() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("history.sqlite3");
    let connection = Connection::open(&path).unwrap();
    connection
        .pragma_update(None, "auto_vacuum", "INCREMENTAL")
        .unwrap();
    let _: String = connection
        .query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))
        .unwrap();
    connection.execute_batch(SCHEMA_V1).unwrap();
    connection.execute_batch(SCHEMA_V2).unwrap();
    connection.execute_batch(SCHEMA_V3).unwrap();
    connection.execute_batch(SCHEMA_V4).unwrap();
    connection.execute_batch(SCHEMA_V5).unwrap();
    connection.pragma_update(None, "user_version", 5).unwrap();
    drop(connection);

    let store = HistoryStore::open(path.clone(), Arc::from("migration-runtime")).unwrap();
    drop(store);
    let connection = Connection::open(path).unwrap();
    let version: u32 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, HISTORY_SCHEMA_VERSION);
    let session_table_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master
             WHERE type = 'table' AND name = 'mcp_context_sessions'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(session_table_count, 1);
    let workdir_table_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master
             WHERE type = 'table' AND name = 'mcp_context_workdirs'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(workdir_table_count, 1);
    let repo_skills_table_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master
             WHERE type = 'table' AND name = 'mcp_context_workdir_skills'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(repo_skills_table_count, 1);
}

#[test]
fn schema_v6_migrates_to_repo_skill_delivery_state() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("history.sqlite3");
    let connection = Connection::open(&path).unwrap();
    connection
        .pragma_update(None, "auto_vacuum", "INCREMENTAL")
        .unwrap();
    let _: String = connection
        .query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))
        .unwrap();
    connection.execute_batch(SCHEMA_V1).unwrap();
    connection.execute_batch(SCHEMA_V2).unwrap();
    connection.execute_batch(SCHEMA_V3).unwrap();
    connection.execute_batch(SCHEMA_V4).unwrap();
    connection.execute_batch(SCHEMA_V5).unwrap();
    connection.execute_batch(SCHEMA_V6).unwrap();
    connection.pragma_update(None, "user_version", 6).unwrap();
    drop(connection);

    let store = HistoryStore::open(path.clone(), Arc::from("migration-runtime")).unwrap();
    drop(store);
    let connection = Connection::open(path).unwrap();
    let version: u32 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, HISTORY_SCHEMA_VERSION);
    let repo_skills_table_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master
             WHERE type = 'table' AND name = 'mcp_context_workdir_skills'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(repo_skills_table_count, 1);
}

#[test]
fn schema_v9_preserves_output_boundaries_uniqueness_and_cascade() {
    use super::schema::{SCHEMA_V7, SCHEMA_V8};
    let dir = tempdir().unwrap();
    let path = dir.path().join("history.sqlite3");
    let connection = Connection::open(&path).unwrap();
    connection
        .pragma_update(None, "auto_vacuum", "INCREMENTAL")
        .unwrap();
    connection
        .execute_batch("PRAGMA journal_mode=WAL;")
        .unwrap();
    for schema in [
        SCHEMA_V1, SCHEMA_V2, SCHEMA_V3, SCHEMA_V4, SCHEMA_V5, SCHEMA_V6, SCHEMA_V7, SCHEMA_V8,
    ] {
        connection.execute_batch(schema).unwrap();
    }
    connection.pragma_update(None, "user_version", 8).unwrap();
    connection.execute("INSERT INTO invocations(id, correlation_id, tool_name, args_json, started_at_ms, evidence_state, capture_state) VALUES(1, 'migration', 'exec_command', '{}', 123, 'complete', 'complete')", []).unwrap();
    let payloads: Vec<String> = [980, 1000, 1024, 4096]
        .into_iter()
        .map(|n| format!("{}\0終", "x".repeat(n)))
        .collect();
    for (sequence, text) in payloads.iter().enumerate() {
        connection
            .execute(
                "INSERT INTO invocation_output_chunks VALUES(1, ?1, ?2, ?3)",
                rusqlite::params![sequence as i64, 456 + sequence as i64, text],
            )
            .unwrap();
    }
    drop(connection);
    drop(HistoryStore::open(path.clone(), Arc::from("migration")).unwrap());
    let connection = Connection::open(path).unwrap();
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .unwrap();
    let actual: Vec<(i64, i64, String)> = connection.prepare("SELECT sequence, observed_at_ms, text FROM invocation_output_chunks WHERE invocation_id=1 ORDER BY sequence").unwrap().query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap().collect::<rusqlite::Result<_>>().unwrap();
    for (n, (sequence, time, text)) in actual.iter().enumerate() {
        assert_eq!(*sequence, n as i64);
        assert_eq!(*time, 456 + n as i64);
        assert_eq!(text, &payloads[n]);
    }
    assert_eq!(actual.len(), payloads.len());
    connection
        .query_row(
            "SELECT rowid FROM invocation_output_chunks LIMIT 1",
            [],
            |r| r.get::<_, i64>(0),
        )
        .unwrap();
    let plan: String = connection.query_row("EXPLAIN QUERY PLAN SELECT text FROM invocation_output_chunks WHERE invocation_id=1 ORDER BY sequence", [], |r| r.get(3)).unwrap();
    assert!(plan.contains("USING INDEX"), "{plan}");
    assert!(
        connection
            .execute(
                "INSERT INTO invocation_output_chunks VALUES(1, 0, 999, 'duplicate')",
                []
            )
            .is_err()
    );
    assert!(
        connection
            .execute(
                "INSERT INTO invocation_output_chunks VALUES(99, 0, 999, 'missing parent')",
                []
            )
            .is_err()
    );
    connection
        .execute("DELETE FROM invocations WHERE id=1", [])
        .unwrap();
    let count: i64 = connection
        .query_row("SELECT COUNT(*) FROM invocation_output_chunks", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(count, 0);
}

#[test]
fn schema_v9_failed_copy_rolls_back_original_rows_and_version() {
    use super::schema::{SCHEMA_V7, SCHEMA_V8, initialize_or_migrate};
    let dir = tempdir().unwrap();
    let path = dir.path().join("history.sqlite3");
    let mut connection = Connection::open(path).unwrap();
    connection
        .pragma_update(None, "auto_vacuum", "INCREMENTAL")
        .unwrap();
    connection
        .execute_batch("PRAGMA journal_mode=WAL;")
        .unwrap();
    for schema in [
        SCHEMA_V1, SCHEMA_V2, SCHEMA_V3, SCHEMA_V4, SCHEMA_V5, SCHEMA_V6, SCHEMA_V7, SCHEMA_V8,
    ] {
        connection.execute_batch(schema).unwrap();
    }
    connection.pragma_update(None, "user_version", 8).unwrap();
    connection
        .pragma_update(None, "foreign_keys", "OFF")
        .unwrap();
    // Deliberately corrupt foreign-key ancestry with enforcement off: the new
    // table's insert fails after CREATE and must roll back the whole migration.
    connection
        .execute(
            "INSERT INTO invocation_output_chunks VALUES(99, 1, 123, 'preserve me')",
            [],
        )
        .unwrap();
    assert!(initialize_or_migrate(&mut connection).is_err());
    let version: i64 = connection
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap();
    assert_eq!(version, 8);
    let text: String = connection
        .query_row("SELECT text FROM invocation_output_chunks", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(text, "preserve me");
    let temporary: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE name='invocation_output_chunks_v9'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(temporary, 0);
}

#[test]
fn schema_v9_reduces_inline_output_page_usage() {
    use super::schema::{SCHEMA_V7, SCHEMA_V8, initialize_or_migrate};
    for payload_bytes in [980, 1000, 1024] {
        let dir = tempdir().unwrap();
        let mut connection = Connection::open(dir.path().join("history.sqlite3")).unwrap();
        connection
            .pragma_update(None, "auto_vacuum", "INCREMENTAL")
            .unwrap();
        connection
            .execute_batch("PRAGMA journal_mode=WAL;")
            .unwrap();
        for schema in [
            SCHEMA_V1, SCHEMA_V2, SCHEMA_V3, SCHEMA_V4, SCHEMA_V5, SCHEMA_V6, SCHEMA_V7, SCHEMA_V8,
        ] {
            connection.execute_batch(schema).unwrap();
        }
        connection.pragma_update(None, "user_version", 8).unwrap();
        connection.execute("INSERT INTO invocations(id, correlation_id, tool_name, args_json, started_at_ms, evidence_state, capture_state) VALUES(1,'layout','exec_command','{}',123,'complete','complete')", []).unwrap();
        let payload = "x".repeat(payload_bytes);
        for sequence in 0..256 {
            connection
                .execute(
                    "INSERT INTO invocation_output_chunks VALUES(1,?1,1234567890123,?2)",
                    rusqlite::params![sequence, payload],
                )
                .unwrap();
        }
        let used_pages = |c: &Connection| -> i64 {
            let count: i64 = c
                .pragma_query_value(None, "page_count", |r| r.get(0))
                .unwrap();
            let free: i64 = c
                .pragma_query_value(None, "freelist_count", |r| r.get(0))
                .unwrap();
            count - free
        };
        let before = used_pages(&connection);
        initialize_or_migrate(&mut connection).unwrap();
        let after = used_pages(&connection);
        if payload_bytes >= 1000 {
            assert!(
                after * 2 < before,
                "{payload_bytes}-byte output should avoid overflow: {before} -> {after}"
            );
        } else {
            assert!(
                after <= before + 4,
                "980-byte inline rows should not meaningfully regress: {before} -> {after}"
            );
        }
    }
}
