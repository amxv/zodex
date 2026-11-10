use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde_json::Value;

pub const HISTORY_SCHEMA_VERSION: u32 = 9;

pub(super) const SCHEMA_V1: &str = r#"
CREATE TABLE agents (
    id TEXT PRIMARY KEY CHECK(length(id) = 4),
    provider_kind TEXT NOT NULL,
    provider_session_key TEXT NOT NULL,
    first_seen_at_ms INTEGER NOT NULL,
    last_seen_at_ms INTEGER NOT NULL,
    last_seen_runtime_id TEXT NOT NULL,
    UNIQUE(provider_kind, provider_session_key)
);

CREATE TABLE invocations (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    correlation_id TEXT NOT NULL UNIQUE,
    agent_id TEXT REFERENCES agents(id) ON DELETE SET NULL,
    provider_kind TEXT,
    provider_session_key TEXT,
    tool_name TEXT NOT NULL,
    args_json TEXT NOT NULL,
    declared_workdir_exact TEXT,
    declared_workdir_normalized TEXT,
    is_new_workdir INTEGER NOT NULL DEFAULT 0 CHECK(is_new_workdir IN (0, 1)),
    started_at_ms INTEGER NOT NULL,
    completed_at_ms INTEGER,
    duration_ms INTEGER,
    outcome_kind TEXT CHECK(outcome_kind IN ('success', 'error')),
    result_json TEXT,
    error_text TEXT,
    evidence_state TEXT NOT NULL CHECK(evidence_state IN ('pending', 'complete', 'incomplete')),
    evidence_reason TEXT,
    capture_state TEXT NOT NULL CHECK(capture_state IN ('not_applicable', 'pending', 'complete', 'incomplete')),
    capture_reason TEXT,
    target_session_handle TEXT,
    target_created_by_agent_id TEXT,
    cross_agent INTEGER CHECK(cross_agent IS NULL OR cross_agent IN (0, 1)),
    result_status TEXT,
    result_cwd TEXT,
    result_session_handle TEXT,
    result_exit_code INTEGER,
    result_termination_reason TEXT
);

CREATE INDEX invocations_started_idx ON invocations(started_at_ms DESC, id DESC);
CREATE INDEX invocations_agent_idx ON invocations(agent_id, started_at_ms DESC, id DESC);
CREATE INDEX invocations_workdir_idx ON invocations(declared_workdir_normalized, started_at_ms DESC, id DESC);
CREATE INDEX invocations_provider_idx ON invocations(provider_kind, provider_session_key, id DESC);

CREATE TABLE invocation_output_chunks (
    invocation_id INTEGER NOT NULL REFERENCES invocations(id) ON DELETE CASCADE,
    sequence INTEGER NOT NULL,
    observed_at_ms INTEGER NOT NULL,
    text TEXT NOT NULL,
    PRIMARY KEY(invocation_id, sequence)
) WITHOUT ROWID;

CREATE TABLE agent_workdirs (
    agent_id TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
    normalized_workdir TEXT NOT NULL,
    ordinal INTEGER NOT NULL,
    first_seen_at_ms INTEGER NOT NULL,
    last_seen_at_ms INTEGER NOT NULL,
    first_invocation_id INTEGER NOT NULL,
    last_invocation_id INTEGER NOT NULL,
    retained_invocation_count INTEGER NOT NULL,
    PRIMARY KEY(agent_id, normalized_workdir),
    UNIQUE(agent_id, ordinal)
);

CREATE TABLE history_state (
    singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
    health_state TEXT NOT NULL,
    health_reason TEXT,
    over_budget INTEGER NOT NULL DEFAULT 0 CHECK(over_budget IN (0, 1)),
    last_retention_error TEXT,
    updated_at_ms INTEGER NOT NULL
);

INSERT INTO history_state(singleton, health_state, updated_at_ms)
VALUES (1, 'healthy', 0);
"#;

pub(super) const SCHEMA_V2: &str = r#"
CREATE TABLE invocation_file_evidence (
    invocation_id INTEGER NOT NULL REFERENCES invocations(id) ON DELETE CASCADE,
    ordinal INTEGER NOT NULL,
    source_kind TEXT NOT NULL CHECK(source_kind IN ('apply_patch', 'shell_write')),
    operation_hint TEXT NOT NULL CHECK(operation_hint IN ('create', 'update', 'delete', 'move', 'overwrite', 'append')),
    path_before TEXT NOT NULL,
    path_after TEXT NOT NULL,
    before_state TEXT NOT NULL CHECK(before_state IN ('missing', 'text', 'unavailable')),
    before_text TEXT,
    before_reason TEXT,
    after_state TEXT NOT NULL CHECK(after_state IN ('pending', 'missing', 'text', 'unavailable')),
    after_text TEXT,
    after_reason TEXT,
    source_after_state TEXT CHECK(source_after_state IS NULL OR source_after_state IN ('missing', 'text', 'unavailable')),
    source_after_text TEXT,
    source_after_reason TEXT,
    PRIMARY KEY(invocation_id, ordinal)
) WITHOUT ROWID;

CREATE INDEX invocation_file_evidence_after_path_idx
ON invocation_file_evidence(path_after, invocation_id);
"#;

pub(super) const SCHEMA_V3: &str = r#"
ALTER TABLE invocation_file_evidence ADD COLUMN destination_before_state TEXT
    CHECK(destination_before_state IS NULL OR destination_before_state IN ('missing', 'text', 'unavailable'));
ALTER TABLE invocation_file_evidence ADD COLUMN destination_before_text TEXT;
ALTER TABLE invocation_file_evidence ADD COLUMN destination_before_reason TEXT;
"#;

pub(super) const SCHEMA_V4: &str = r#"
ALTER TABLE invocations ADD COLUMN target_created_by_invocation_id INTEGER
    REFERENCES invocations(id) ON DELETE SET NULL;
ALTER TABLE invocations ADD COLUMN continuation_kind TEXT
    CHECK(continuation_kind IS NULL OR continuation_kind IN ('poll', 'stdin', 'kill'));
ALTER TABLE invocations ADD COLUMN process_state TEXT
    CHECK(process_state IS NULL OR process_state IN ('running', 'exited', 'incomplete'));
ALTER TABLE invocations ADD COLUMN process_started_at_ms INTEGER;
ALTER TABLE invocations ADD COLUMN process_ended_at_ms INTEGER;
ALTER TABLE invocations ADD COLUMN process_updated_at_ms INTEGER;
ALTER TABLE invocations ADD COLUMN process_exit_code INTEGER;
ALTER TABLE invocations ADD COLUMN process_termination_reason TEXT
    CHECK(process_termination_reason IS NULL OR process_termination_reason IN ('exit', 'timeout', 'killed'));
ALTER TABLE invocations ADD COLUMN process_cwd TEXT;
ALTER TABLE invocations ADD COLUMN process_incomplete_reason TEXT;

CREATE INDEX invocations_continuation_parent_kind_idx
ON invocations(target_created_by_invocation_id, continuation_kind, id)
WHERE target_created_by_invocation_id IS NOT NULL;
CREATE INDEX invocations_completed_changed_idx
ON invocations(completed_at_ms, id)
WHERE completed_at_ms IS NOT NULL;
CREATE INDEX invocations_process_updated_idx
ON invocations(process_updated_at_ms, id)
WHERE process_updated_at_ms IS NOT NULL;
"#;

pub(super) const SCHEMA_V5: &str = r#"
CREATE TABLE presentation_materializations (
    root_invocation_id INTEGER PRIMARY KEY REFERENCES invocations(id) ON DELETE CASCADE,
    presentation_version INTEGER NOT NULL,
    materialized_at_ms INTEGER NOT NULL,
    summary_json TEXT NOT NULL,
    full_json TEXT
);

CREATE TRIGGER presentation_materializations_invalidate_invocation_insert
AFTER INSERT ON invocations
WHEN NEW.continuation_kind = 'poll'
BEGIN
    DELETE FROM presentation_materializations
    WHERE root_invocation_id = CASE
        WHEN NEW.target_created_by_invocation_id IS NOT NULL
            THEN NEW.target_created_by_invocation_id
        WHEN NEW.target_session_handle IS NOT NULL
            THEN (
                SELECT MIN(i.id) FROM invocations i
                WHERE i.continuation_kind = 'poll'
                  AND i.target_created_by_invocation_id IS NULL
                  AND i.target_session_handle = NEW.target_session_handle
            )
        ELSE NEW.id
    END;
END;

CREATE TRIGGER presentation_materializations_invalidate_invocation_update
AFTER UPDATE ON invocations
BEGIN
    DELETE FROM presentation_materializations
    WHERE root_invocation_id = CASE
        WHEN OLD.continuation_kind = 'poll' AND OLD.target_created_by_invocation_id IS NOT NULL
            THEN OLD.target_created_by_invocation_id
        WHEN OLD.continuation_kind = 'poll' AND OLD.target_session_handle IS NOT NULL
            THEN (
                SELECT MIN(i.id) FROM invocations i
                WHERE i.continuation_kind = 'poll'
                  AND i.target_created_by_invocation_id IS NULL
                  AND i.target_session_handle = OLD.target_session_handle
            )
        ELSE OLD.id
    END;
    DELETE FROM presentation_materializations
    WHERE root_invocation_id = CASE
        WHEN NEW.continuation_kind = 'poll' AND NEW.target_created_by_invocation_id IS NOT NULL
            THEN NEW.target_created_by_invocation_id
        WHEN NEW.continuation_kind = 'poll' AND NEW.target_session_handle IS NOT NULL
            THEN (
                SELECT MIN(i.id) FROM invocations i
                WHERE i.continuation_kind = 'poll'
                  AND i.target_created_by_invocation_id IS NULL
                  AND i.target_session_handle = NEW.target_session_handle
            )
        ELSE NEW.id
    END;
END;

CREATE TRIGGER presentation_materializations_invalidate_invocation_delete
AFTER DELETE ON invocations
BEGIN
    DELETE FROM presentation_materializations
    WHERE root_invocation_id = CASE
        WHEN OLD.continuation_kind = 'poll' AND OLD.target_created_by_invocation_id IS NOT NULL
            THEN OLD.target_created_by_invocation_id
        ELSE OLD.id
    END;
    DELETE FROM presentation_materializations
    WHERE OLD.continuation_kind = 'poll'
      AND OLD.target_created_by_invocation_id IS NULL
      AND OLD.target_session_handle IS NOT NULL
      AND root_invocation_id = (
          SELECT MIN(i.id) FROM invocations i
          WHERE i.continuation_kind = 'poll'
            AND i.target_created_by_invocation_id IS NULL
            AND i.target_session_handle = OLD.target_session_handle
      );
END;

CREATE TRIGGER presentation_materializations_invalidate_file_evidence_insert
AFTER INSERT ON invocation_file_evidence
BEGIN
    DELETE FROM presentation_materializations WHERE root_invocation_id = NEW.invocation_id;
END;

CREATE TRIGGER presentation_materializations_invalidate_file_evidence_update
AFTER UPDATE ON invocation_file_evidence
BEGIN
    DELETE FROM presentation_materializations WHERE root_invocation_id = NEW.invocation_id;
END;

CREATE TRIGGER presentation_materializations_invalidate_file_evidence_delete
AFTER DELETE ON invocation_file_evidence
BEGIN
    DELETE FROM presentation_materializations WHERE root_invocation_id = OLD.invocation_id;
END;
"#;

pub(super) const SCHEMA_V6: &str = r#"
CREATE TABLE mcp_context_sessions (
    provider_fingerprint BLOB PRIMARY KEY,
    global_context_injected_at_ms INTEGER
) WITHOUT ROWID;

CREATE TABLE mcp_context_workdirs (
    provider_fingerprint BLOB NOT NULL,
    workdir_fingerprint BLOB NOT NULL,
    repo_agents_checked_at_ms INTEGER NOT NULL,
    PRIMARY KEY(provider_fingerprint, workdir_fingerprint)
) WITHOUT ROWID;
"#;

pub(super) const SCHEMA_V7: &str = r#"
CREATE TABLE mcp_context_workdir_skills (
    provider_fingerprint BLOB NOT NULL,
    workdir_fingerprint BLOB NOT NULL,
    repo_skills_checked_at_ms INTEGER NOT NULL,
    PRIMARY KEY(provider_fingerprint, workdir_fingerprint)
) WITHOUT ROWID;
"#;

pub(super) const SCHEMA_V8: &str = r#"
CREATE INDEX invocations_orphan_poll_handle_started_idx
ON invocations(target_session_handle, started_at_ms DESC, id DESC)
WHERE tool_name = 'write_stdin'
  AND continuation_kind = 'poll'
  AND target_created_by_invocation_id IS NULL
  AND target_session_handle IS NOT NULL;
"#;

// Rowid tables keep ~1 KiB output payloads inline instead of spilling each
// composite-key index record to an overflow page. Preserve the unique lookup
// index and exact output bytes; no compression/decompression is on the hot path.
pub(super) const SCHEMA_V9: &str = r#"
CREATE TABLE invocation_output_chunks_v9 (
    invocation_id INTEGER NOT NULL REFERENCES invocations(id) ON DELETE CASCADE,
    sequence INTEGER NOT NULL,
    observed_at_ms INTEGER NOT NULL,
    text TEXT NOT NULL,
    PRIMARY KEY(invocation_id, sequence)
);
INSERT INTO invocation_output_chunks_v9
SELECT invocation_id, sequence, observed_at_ms, text FROM invocation_output_chunks
ORDER BY invocation_id, sequence;
DROP TABLE invocation_output_chunks;
ALTER TABLE invocation_output_chunks_v9 RENAME TO invocation_output_chunks;
"#;

pub(super) const WRITER_BUSY_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(500);

pub(super) fn configure_writer(connection: &Connection) -> Result<()> {
    connection
        .busy_timeout(WRITER_BUSY_TIMEOUT)
        .context("failed to configure Local history SQLite busy timeout")?;
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .context("failed to enable Local history foreign keys")?;
    Ok(())
}

pub(super) fn initialize_or_migrate(connection: &mut Connection) -> Result<()> {
    configure_writer(connection)?;
    let mut version = user_version(connection)?;
    if version > HISTORY_SCHEMA_VERSION {
        bail!(
            "Local history schema version {version} is newer than this Zodex build supports ({HISTORY_SCHEMA_VERSION})"
        );
    }

    if version == 0 {
        let existing_table: Option<String> = connection
            .query_row(
                "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' LIMIT 1",
                [],
                |row| row.get(0),
            )
            .optional()
            .context("failed to inspect unversioned Local history database")?;
        if let Some(table) = existing_table {
            bail!(
                "refusing to initialize unversioned non-empty Local history database; found table `{table}`"
            );
        }

        // auto_vacuum must be selected before the v1 tables are created. WAL
        // is enabled on the same empty database so physical-retention behavior
        // is part of the schema-creation contract rather than a later retrofit.
        connection
            .pragma_update(None, "auto_vacuum", "INCREMENTAL")
            .context("failed to enable incremental auto-vacuum for Local history")?;
        let journal_mode: String = connection
            .query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))
            .context("failed to enable WAL mode for Local history")?;
        if !journal_mode.eq_ignore_ascii_case("wal") {
            bail!("Local history SQLite refused WAL mode (reported `{journal_mode}`)");
        }

        let transaction = connection
            .transaction()
            .context("failed to start Local history schema-v1 transaction")?;
        transaction
            .execute_batch(SCHEMA_V1)
            .context("failed to create Local history schema v1")?;
        transaction
            .pragma_update(None, "user_version", 1)
            .context("failed to publish Local history schema version")?;
        transaction
            .commit()
            .context("failed to commit Local history schema v1")?;
        version = 1;
    }

    if version >= 1 {
        let auto_vacuum: i64 = connection
            .pragma_query_value(None, "auto_vacuum", |row| row.get(0))
            .context("failed to inspect Local history auto-vacuum mode")?;
        if auto_vacuum != 2 {
            bail!(
                "Local history schema v1 requires incremental auto-vacuum; database reports mode {auto_vacuum}"
            );
        }
        let journal_mode: String = connection
            .query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))
            .context("failed to restore WAL mode for Local history")?;
        if !journal_mode.eq_ignore_ascii_case("wal") {
            bail!("Local history SQLite refused WAL mode (reported `{journal_mode}`)");
        }
    }

    if version == 1 {
        let transaction = connection
            .transaction()
            .context("failed to start Local history schema-v2 transaction")?;
        transaction
            .execute_batch(SCHEMA_V2)
            .context("failed to create Local history schema v2")?;
        transaction
            .pragma_update(None, "user_version", 2)
            .context("failed to publish Local history schema version 2")?;
        transaction
            .commit()
            .context("failed to commit Local history schema v2")?;
        version = 2;
    }

    if version == 2 {
        let transaction = connection
            .transaction()
            .context("failed to start Local history schema-v3 transaction")?;
        transaction
            .execute_batch(SCHEMA_V3)
            .context("failed to create Local history schema v3")?;
        transaction
            .pragma_update(None, "user_version", 3)
            .context("failed to publish Local history schema version 3")?;
        transaction
            .commit()
            .context("failed to commit Local history schema v3")?;
        version = 3;
    }

    if version == 3 {
        let transaction = connection
            .transaction()
            .context("failed to start Local history schema-v4 transaction")?;
        transaction
            .execute_batch(SCHEMA_V4)
            .context("failed to create Local history schema v4")?;
        backfill_v4_continuations(&transaction)?;
        transaction
            .pragma_update(None, "user_version", 4)
            .context("failed to publish Local history schema version 4")?;
        transaction
            .commit()
            .context("failed to commit Local history schema v4")?;
        version = 4;
    }

    if version == 4 {
        let transaction = connection
            .transaction()
            .context("failed to start Local history schema-v5 transaction")?;
        transaction
            .execute_batch(SCHEMA_V5)
            .context("failed to create Local history schema v5")?;
        transaction
            .pragma_update(None, "user_version", 5)
            .context("failed to publish Local history schema version 5")?;
        transaction
            .commit()
            .context("failed to commit Local history schema v5")?;
        version = 5;
    }

    if version == 5 {
        let transaction = connection
            .transaction()
            .context("failed to start Local history schema-v6 transaction")?;
        transaction
            .execute_batch(SCHEMA_V6)
            .context("failed to create Local history schema v6")?;
        transaction
            .pragma_update(None, "user_version", 6)
            .context("failed to publish Local history schema version 6")?;
        transaction
            .commit()
            .context("failed to commit Local history schema v6")?;
        version = 6;
    }

    if version == 6 {
        let transaction = connection
            .transaction()
            .context("failed to start Local history schema-v7 transaction")?;
        transaction
            .execute_batch(SCHEMA_V7)
            .context("failed to create Local history schema v7")?;
        transaction
            .pragma_update(None, "user_version", 7)
            .context("failed to publish Local history schema version 7")?;
        transaction
            .commit()
            .context("failed to commit Local history schema v7")?;
        version = 7;
    }

    if version == 7 {
        let transaction = connection
            .transaction()
            .context("failed to start Local history schema-v8 transaction")?;
        transaction
            .execute_batch(SCHEMA_V8)
            .context("failed to create Local history schema v8")?;
        transaction
            .pragma_update(None, "user_version", 8)
            .context("failed to publish Local history schema version 8")?;
        transaction
            .commit()
            .context("failed to commit Local history schema v8")?;
        version = 8;
    }

    if version == 8 {
        let transaction = connection
            .transaction()
            .context("failed to start Local history schema-v9 transaction")?;
        transaction
            .execute_batch(SCHEMA_V9)
            .context("failed to migrate Local history output storage")?;
        transaction.pragma_update(None, "user_version", 9)?;
        transaction
            .commit()
            .context("failed to commit Local history schema v9")?;
    }

    let version = user_version(connection)?;
    if version != HISTORY_SCHEMA_VERSION {
        bail!(
            "Local history schema migration ended at version {version}; expected {HISTORY_SCHEMA_VERSION}"
        );
    }
    Ok(())
}

fn backfill_v4_continuations(transaction: &Transaction<'_>) -> Result<()> {
    let continuations = {
        let mut statement = transaction
            .prepare(
                "SELECT id, args_json, target_session_handle
                 FROM invocations
                 WHERE tool_name = 'write_stdin'
                 ORDER BY id ASC",
            )
            .context("failed to prepare legacy Local continuation backfill")?;
        statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            })
            .context("failed to query legacy Local continuations")?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("failed to decode legacy Local continuations")?
    };

    for (invocation_id, args_json, target_session_handle) in continuations {
        if let Some(kind) = legacy_continuation_kind(&args_json)? {
            transaction
                .execute(
                    "UPDATE invocations SET continuation_kind = ?2 WHERE id = ?1",
                    params![invocation_id, kind],
                )
                .with_context(|| {
                    format!(
                        "failed to backfill continuation kind for Local invocation {invocation_id}"
                    )
                })?;
        }

        let Some(target_session_handle) = target_session_handle else {
            continue;
        };
        let parent_candidates = {
            let mut statement = transaction
                .prepare(
                    "SELECT id FROM invocations
                     WHERE tool_name = 'exec_command'
                       AND result_session_handle = ?1
                       AND id < ?2
                     ORDER BY id DESC
                     LIMIT 2",
                )
                .context("failed to prepare legacy Local continuation-parent backfill")?;
            statement
                .query_map(params![target_session_handle, invocation_id], |row| {
                    row.get(0)
                })
                .context("failed to query legacy Local continuation parents")?
                .collect::<rusqlite::Result<Vec<i64>>>()
                .context("failed to decode legacy Local continuation parents")?
        };
        if let [parent_id] = parent_candidates.as_slice() {
            transaction
                .execute(
                    "UPDATE invocations SET target_created_by_invocation_id = ?2 WHERE id = ?1",
                    params![invocation_id, parent_id],
                )
                .with_context(|| {
                    format!(
                        "failed to backfill continuation parent for Local invocation {invocation_id}"
                    )
                })?;
        }
    }
    Ok(())
}

fn legacy_continuation_kind(args_json: &str) -> Result<Option<&'static str>> {
    let value: Value = serde_json::from_str(args_json)
        .context("failed to parse legacy Local write_stdin arguments during schema migration")?;
    let Some(object) = value.as_object() else {
        return Ok(None);
    };

    match object.get("kill_process") {
        Some(Value::Bool(true)) => return Ok(Some("kill")),
        Some(Value::Bool(false) | Value::Null) | None => {}
        Some(_) => return Ok(None),
    }
    match object.get("chars") {
        Some(Value::String(chars)) if !chars.is_empty() => Ok(Some("stdin")),
        Some(Value::String(_) | Value::Null) | None => Ok(Some("poll")),
        Some(_) => Ok(None),
    }
}

pub(super) fn verify_readable_schema(connection: &Connection) -> Result<()> {
    readable_schema_version(connection).map(|_| ())
}

pub(super) fn readable_schema_version(connection: &Connection) -> Result<u32> {
    let version = user_version(connection)?;
    if version == 0 || version > HISTORY_SCHEMA_VERSION {
        bail!(
            "unsupported Local history schema version {version}; supported range is 1..={HISTORY_SCHEMA_VERSION}"
        );
    }
    Ok(version)
}

fn user_version(connection: &Connection) -> Result<u32> {
    connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .context("failed to inspect Local history schema version")
}
