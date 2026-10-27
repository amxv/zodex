use std::fs;
use std::path::Path;
use std::sync::Arc;

use rusqlite::{Connection, params};
use sha2::{Digest, Sha256};
use tempfile::tempdir;

use super::clear::clear_local_agent_history;
use super::store::HistoryStore;

fn seed_agent(connection: &Connection, agent: &str, provider: &str, payload: &str) -> i64 {
    connection.execute(
        "INSERT INTO agents(id, provider_kind, provider_session_key, first_seen_at_ms, last_seen_at_ms, last_seen_runtime_id)
         VALUES (?1, 'openai/session', ?2, 1, 1, 'runtime')",
        params![agent, provider],
    ).unwrap();
    connection
        .execute(
            "INSERT INTO invocations(correlation_id, agent_id, provider_kind, provider_session_key,
         tool_name, args_json, started_at_ms, evidence_state, capture_state, result_json)
         VALUES (?1, ?2, 'openai/session', ?3, 'exec_command', ?4, 1, 'complete', 'complete', ?4)",
            params![format!("call-{agent}"), agent, provider, payload],
        )
        .unwrap();
    let invocation = connection.last_insert_rowid();
    connection
        .execute(
            "INSERT INTO invocation_output_chunks(invocation_id, sequence, observed_at_ms, text)
         VALUES (?1, 0, 1, ?2)",
            params![invocation, payload],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO invocation_file_evidence(invocation_id, ordinal, source_kind,
         operation_hint, path_before, path_after, before_state, after_state, after_text)
         VALUES (?1, 0, 'shell_write', 'create', '/tmp/old', '/tmp/new', 'missing', 'text', ?2)",
            params![invocation, payload],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO agent_workdirs(agent_id, normalized_workdir, ordinal, first_seen_at_ms,
         last_seen_at_ms, first_invocation_id, last_invocation_id, retained_invocation_count)
         VALUES (?1, '/tmp', 1, 1, 1, ?2, ?2, 1)",
            params![agent, invocation],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO presentation_materializations(root_invocation_id, presentation_version,
         materialized_at_ms, summary_json, full_json) VALUES (?1, 1, 1, ?2, ?2)",
            params![invocation, payload],
        )
        .unwrap();
    let fingerprint =
        Sha256::digest([b"openai/session".as_slice(), &[0], provider.as_bytes()].concat());
    connection
        .execute(
            "INSERT INTO mcp_context_sessions(provider_fingerprint) VALUES (?1)",
            [fingerprint.as_slice()],
        )
        .unwrap();
    connection.execute(
        "INSERT INTO mcp_context_workdirs(provider_fingerprint, workdir_fingerprint, repo_agents_checked_at_ms)
         VALUES (?1, X'01', 1)",
        [fingerprint.as_slice()],
    ).unwrap();
    connection.execute(
        "INSERT INTO mcp_context_workdir_skills(provider_fingerprint, workdir_fingerprint, repo_skills_checked_at_ms)
         VALUES (?1, X'01', 1)",
        [fingerprint.as_slice()],
    ).unwrap();
    invocation
}

fn count(connection: &Connection, table: &str) -> i64 {
    connection
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
            row.get(0)
        })
        .unwrap()
}

#[test]
fn clear_one_agent_erases_all_owned_evidence_and_preserves_others() {
    let dir = tempdir().unwrap();
    let database = dir.path().join("history.sqlite3");
    let store = HistoryStore::open(database.clone(), Arc::from("runtime")).unwrap();
    let connection = Connection::open(&database).unwrap();
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .unwrap();
    let deleted_invocation = seed_agent(
        &connection,
        "g6rk",
        "deleted-provider",
        "deleted-private-payload-g6rk",
    );
    let survivor = seed_agent(
        &connection,
        "b123",
        "surviving-provider",
        "surviving-private-payload",
    );
    connection.execute(
        "UPDATE invocations SET target_created_by_invocation_id = ?1, target_created_by_agent_id = 'g6rk'
         WHERE id = ?2",
        params![deleted_invocation, survivor],
    ).unwrap();
    connection.execute(
        "UPDATE presentation_materializations SET summary_json = 'cached g6rk reference' WHERE root_invocation_id = ?1",
        [survivor],
    ).unwrap();
    drop(connection);

    // A separate open store simulates the Local service retaining a connection.
    assert_eq!(clear_local_agent_history(&database, "g6rk").unwrap(), 1);

    let connection = Connection::open(&database).unwrap();
    assert_eq!(count(&connection, "agents"), 1);
    assert_eq!(count(&connection, "invocations"), 1);
    assert_eq!(count(&connection, "agent_workdirs"), 1);
    assert_eq!(count(&connection, "invocation_output_chunks"), 1);
    assert_eq!(count(&connection, "invocation_file_evidence"), 1);
    assert_eq!(count(&connection, "presentation_materializations"), 0);
    assert_eq!(count(&connection, "mcp_context_sessions"), 1);
    assert_eq!(count(&connection, "mcp_context_workdirs"), 1);
    assert_eq!(count(&connection, "mcp_context_workdir_skills"), 1);
    assert_eq!(
        connection
            .query_row("SELECT id FROM agents", [], |row| row.get::<_, String>(0),)
            .unwrap(),
        "b123"
    );
    let references: (Option<i64>, Option<String>) = connection
        .query_row(
            "SELECT target_created_by_invocation_id, target_created_by_agent_id
         FROM invocations WHERE id = ?1",
            [survivor],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(references, (None, None));
    drop(connection);
    drop(store);

    // After VACUUM + checkpoint, the SQLite DB and WAL cannot retain this
    // exact marker as a recoverable B-tree/free-page payload.
    for file in [
        database.clone(),
        Path::new(&format!("{}-wal", database.display())).to_path_buf(),
    ] {
        if file.exists() {
            let bytes = fs::read(&file).unwrap();
            assert!(
                !bytes
                    .windows(b"deleted-private-payload-g6rk".len())
                    .any(|window| window == b"deleted-private-payload-g6rk")
            );
        }
    }
    assert_eq!(clear_local_agent_history(&database, "g6rk").unwrap(), 0);
}

#[test]
fn clear_agent_rejects_in_flight_work_without_deleting_it() {
    let dir = tempdir().unwrap();
    let database = dir.path().join("history.sqlite3");
    let store = HistoryStore::open(database.clone(), Arc::from("runtime")).unwrap();
    let connection = Connection::open(&database).unwrap();
    let invocation = seed_agent(&connection, "g6rk", "active-provider", "active-payload");
    connection
        .execute(
            "UPDATE invocations SET process_state = 'running' WHERE id = ?1",
            [invocation],
        )
        .unwrap();
    drop(connection);
    assert!(
        clear_local_agent_history(&database, "g6rk")
            .unwrap_err()
            .to_string()
            .contains("unfinished")
    );
    let connection = Connection::open(&database).unwrap();
    assert_eq!(count(&connection, "agents"), 1);
    assert_eq!(count(&connection, "invocations"), 1);
    drop(connection);
    drop(store);
}

#[test]
fn clear_agent_does_not_create_missing_database_or_accept_invalid_id() {
    let dir = tempdir().unwrap();
    let database = dir.path().join("missing.sqlite3");
    assert!(clear_local_agent_history(&database, "g6rk").is_err());
    assert!(!database.exists());
    assert!(clear_local_agent_history(&database, "g6rk' OR 1=1").is_err());
}
