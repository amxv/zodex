use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior, params};
use sha2::{Digest, Sha256};

use super::schema::HISTORY_SCHEMA_VERSION;

/// Remove one Agent's durable identity, invocations, outputs, file evidence,
/// presentations, and provider-specific context without touching other Agents.
///
/// This uses a separate SQLite writer connection, so an otherwise healthy
/// running Local service does not need to stop to forget an inactive Agent.
pub fn clear_local_agent_history(path: &Path, agent_id: &str) -> Result<usize> {
    if agent_id.len() != 4
        || !agent_id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
    {
        bail!("Agent ID must be exactly four lowercase ASCII letters or digits");
    }

    let mut connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE)
        .with_context(|| {
            format!(
                "failed to open existing Local history database {}",
                path.display()
            )
        })?;
    connection
        .busy_timeout(Duration::from_secs(20))
        .context("failed to configure Local history deletion lock timeout")?;
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .context("failed to enable Local history deletion foreign keys")?;
    let version: u32 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .context("failed to read Local history schema version")?;
    if version != HISTORY_SCHEMA_VERSION {
        bail!("refusing to delete Agent history with unsupported Local history schema {version}");
    }
    // The normal SQLite FAST setting can leave deleted payload bytes on free
    // pages. Enable full secure deletion *before* removing any Agent rows.
    connection
        .pragma_update(None, "secure_delete", "ON")
        .context("failed to enable secure deletion of Local history records")?;

    let deleted = {
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("failed to acquire Local history deletion transaction")?;
        let provider: Option<(String, String)> = transaction
            .query_row(
                "SELECT provider_kind, provider_session_key FROM agents WHERE id = ?1",
                [agent_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .context("failed to find Agent identity")?;

        let mut removed = 0;
        if let Some((provider_kind, provider_session_key)) = provider {
            // Never erase an active call/process out from under its owner.
            let active: i64 = transaction.query_row(
                "SELECT COUNT(*) FROM invocations
                 WHERE (agent_id = ?1 OR (agent_id IS NULL AND provider_kind = ?2 AND provider_session_key = ?3))
                   AND (evidence_state = 'pending' OR capture_state = 'pending' OR process_state = 'running')",
                params![agent_id, provider_kind, provider_session_key],
                |row| row.get(0),
            )?;
            if active != 0 {
                bail!(
                    "Agent {agent_id} has {active} unfinished invocation(s); stop its work before clearing its history"
                );
            }

            // Materializations are reconstructible caches and may embed the
            // deleted Agent's records even if their root belongs to another.
            let marker = format!("%{agent_id}%");
            transaction.execute(
                "DELETE FROM presentation_materializations WHERE summary_json LIKE ?1 OR full_json LIKE ?1",
                [&marker],
            )?;

            // Preserve cross-Agent invocations, but remove stale provenance
            // pointing to the Agent whose identity is being forgotten.
            transaction.execute(
                "UPDATE invocations SET target_created_by_agent_id = NULL
                 WHERE target_created_by_agent_id = ?1 AND agent_id != ?1",
                [agent_id],
            )?;

            removed = transaction.execute(
                "DELETE FROM invocations
                 WHERE agent_id = ?1 OR (agent_id IS NULL AND provider_kind = ?2 AND provider_session_key = ?3)",
                params![agent_id, provider_kind, provider_session_key],
            )?;

            let fingerprint = Sha256::digest(
                [
                    provider_kind.as_bytes(),
                    &[0],
                    provider_session_key.as_bytes(),
                ]
                .concat(),
            );
            for table in [
                "mcp_context_workdir_skills",
                "mcp_context_workdirs",
                "mcp_context_sessions",
            ] {
                transaction.execute(
                    &format!("DELETE FROM {table} WHERE provider_fingerprint = ?1"),
                    [fingerprint.as_slice()],
                )?;
            }

            transaction.execute("DELETE FROM agents WHERE id = ?1", [agent_id])?;
        }
        transaction
            .commit()
            .context("failed to commit Agent history deletion")?;
        removed
    };

    // Rebuild retained pages so deleted evidence cannot be read back through
    // SQLite's freelist, then truncate the WAL containing pre-delete pages.
    // The final checkpoint is checked explicitly: SQLite can return 'busy'
    // without raising an SQL error when a reader prevents truncation.
    connection.execute_batch("VACUUM").context(
        "Agent records were deleted, but SQLite could not compact history; retry the clear command",
    )?;
    let (busy, _, _): (i64, i64, i64) = connection
        .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .context("Agent records were deleted, but SQLite could not checkpoint history WAL; retry the clear command")?;
    if busy != 0 {
        bail!(
            "Agent records were deleted, but Local history WAL is busy; retry the clear command to finish disk cleanup"
        );
    }

    Ok(deleted)
}
