use anyhow::{Context, Result};
use rusqlite::Connection;

use super::store::{HistoryStore, now_ms, physical_store_size, recompute_summaries};

pub(super) const SIZE_RETENTION_BATCH_LIMIT: i64 = 1;
const RETENTION_VACUUM_PAGE_LIMIT: i64 = 256;

// Protect the newest root and newest call, as well as all pending/active
// records. Recursion protects both ancestors and descendants, so pruning never
// detaches a protected continuation from the output that created its process.
const PROTECTED_INVOCATIONS: &str = "
WITH RECURSIVE protected(id) AS (
    SELECT id FROM invocations
    WHERE evidence_state = 'pending' OR capture_state = 'pending'
       OR id IN (SELECT invocation_id FROM active_process_invocations)
       OR id = (SELECT id FROM invocations ORDER BY started_at_ms DESC, id DESC LIMIT 1)
       OR id = (SELECT id FROM invocations WHERE target_created_by_invocation_id IS NULL
                  AND (continuation_kind IS NULL OR continuation_kind != 'poll')
                ORDER BY started_at_ms DESC, id DESC LIMIT 1)
    UNION
    SELECT i.id FROM invocations i JOIN protected p ON i.target_created_by_invocation_id = p.id
    UNION
    SELECT i.target_created_by_invocation_id FROM invocations i JOIN protected p ON i.id = p.id
    WHERE i.target_created_by_invocation_id IS NOT NULL
    UNION
    SELECT sibling.id FROM protected p
    JOIN invocations i ON i.id = p.id
    JOIN invocations sibling ON sibling.target_session_handle = i.target_session_handle
    WHERE i.target_created_by_invocation_id IS NULL AND i.continuation_kind = 'poll'
      AND sibling.target_created_by_invocation_id IS NULL AND sibling.continuation_kind = 'poll'
      AND i.tool_name = 'write_stdin' AND sibling.tool_name = 'write_stdin'
)
";
const SIZE_RETENTION_PASS_LIMIT: usize = 256;

impl HistoryStore {
    pub(super) fn run_retention(
        &self,
        max_age_seconds: u64,
        max_size_bytes: u64,
        active_process_invocation_ids: &[i64],
    ) -> Result<()> {
        let cutoff = now_ms()?.saturating_sub(
            i64::try_from(max_age_seconds)
                .unwrap_or(i64::MAX)
                .saturating_mul(1000),
        );
        {
            let mut connection = self.lock_connection();
            let transaction = connection
                .transaction()
                .context("failed to start age-retention transaction")?;
            Self::refresh_active_retention_guards(&transaction, active_process_invocation_ids)?;
            let deleted = transaction
                .execute(
                    &format!(
                        "{PROTECTED_INVOCATIONS}
                     DELETE FROM invocations
                     WHERE id IN (
                         SELECT id FROM invocations
                         WHERE id NOT IN (SELECT id FROM protected)
                           AND COALESCE(completed_at_ms, started_at_ms) < ?1
                         ORDER BY started_at_ms ASC, id ASC
                         LIMIT 256
                     )"
                    ),
                    [cutoff],
                )
                .context("failed to delete age-expired Local invocation units")?;
            // Summaries already reflect every admitted invocation. Rebuilding
            // them without a deletion adds a full history scan and needless
            // writes to every cold start and maintenance pass.
            if deleted > 0 {
                recompute_summaries(&transaction)?;
            }
            transaction
                .commit()
                .context("failed to commit Local age retention")?;
        }

        self.reclaim_pages_bounded()?;
        let mut retained_size = self.retained_store_size()?;
        if retained_size > max_size_bytes && self.discard_presentation_materializations()? > 0 {
            retained_size = self.retained_store_size()?;
        }
        if retained_size > max_size_bytes && self.prune_to_size_budget(max_size_bytes)? > 0 {
            self.reclaim_pages_bounded()?;
        }
        let over_budget = physical_store_size(self.path())? > max_size_bytes;
        self.set_retention_state(over_budget, None)?;
        Ok(())
    }

    /// Make one conservative recovery step after a fresh SQLITE_FULL error.
    /// Retry the failed write after true: cache removal may already suffice.
    /// This runs only on the history worker, never on the tool request path.
    pub(super) fn recover_storage_full(
        &self,
        active_process_invocation_ids: &[i64],
    ) -> Result<bool> {
        let mut connection = self.lock_connection();
        let transaction = connection
            .transaction()
            .context("failed to start Local history storage-full recovery")?;
        Self::refresh_active_retention_guards(&transaction, active_process_invocation_ids)?;
        let cached = transaction
            .execute("DELETE FROM presentation_materializations", [])
            .context("failed to discard Local presentation cache during storage-full recovery")?;
        let deleted = if cached == 0 {
            Self::delete_oldest_complete_invocation(&transaction)?
        } else {
            0
        };
        if deleted > 0 {
            recompute_summaries(&transaction)?;
        }
        transaction
            .commit()
            .context("failed to commit Local history storage-full recovery")?;
        Ok(cached > 0 || deleted > 0)
    }

    fn refresh_active_retention_guards(
        connection: &Connection,
        active_process_invocation_ids: &[i64],
    ) -> Result<()> {
        connection
            .execute("DELETE FROM active_process_invocations", [])
            .context("failed to reset Local active-process retention guards")?;
        let mut insert = connection
            .prepare("INSERT OR IGNORE INTO active_process_invocations(invocation_id) VALUES (?1)")
            .context("failed to prepare Local active-process retention guards")?;
        for invocation_id in active_process_invocation_ids {
            insert.execute([invocation_id]).with_context(|| {
                format!("failed to protect active Local process invocation {invocation_id} from retention")
            })?;
        }
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn delete_oldest_complete_invocation_batch(&self) -> Result<usize> {
        let mut connection = self.lock_connection();
        let transaction = connection
            .transaction()
            .context("failed to start size-retention transaction")?;
        let deleted = Self::delete_oldest_complete_invocation(&transaction)?;
        if deleted > 0 {
            recompute_summaries(&transaction)?;
        }
        transaction
            .commit()
            .context("failed to commit Local size retention")?;
        Ok(deleted)
    }

    fn prune_to_size_budget(&self, max_size_bytes: u64) -> Result<usize> {
        let mut connection = self.lock_connection();
        let transaction = connection
            .transaction()
            .context("failed to start bounded size-retention transaction")?;
        let mut deleted = 0;
        for _ in 0..SIZE_RETENTION_PASS_LIMIT {
            if Self::retained_bytes(&transaction)? <= max_size_bytes {
                break;
            }
            let count = Self::delete_oldest_complete_invocation(&transaction)?;
            if count == 0 {
                break;
            }
            deleted += count;
        }
        // Decide after each unit, but rebuild derived summaries only once.
        if deleted > 0 {
            recompute_summaries(&transaction)?;
        }
        transaction
            .commit()
            .context("failed to commit bounded size retention")?;
        Ok(deleted)
    }

    fn delete_oldest_complete_invocation(connection: &Connection) -> Result<usize> {
        connection
            .execute(
                &format!(
                    "{PROTECTED_INVOCATIONS}
                 DELETE FROM invocations
                 WHERE id IN (
                     SELECT id FROM invocations
                     WHERE id NOT IN (SELECT id FROM protected)
                     ORDER BY started_at_ms ASC, id ASC
                     LIMIT ?1
                 )"
                ),
                [SIZE_RETENTION_BATCH_LIMIT],
            )
            .context("failed to delete oldest complete invocation batch")
    }

    pub(super) fn physical_size(&self) -> Result<u64> {
        physical_store_size(self.path())
    }

    fn checkpoint_wal(&self) -> Result<()> {
        let connection = self.lock_connection();
        let _checkpoint: (i64, i64, i64) = connection
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
            .context("failed to checkpoint Local history WAL")?;
        Ok(())
    }

    fn reclaim_pages_bounded(&self) -> Result<()> {
        self.checkpoint_wal()?;
        let connection = self.lock_connection();
        let freelist: i64 = connection
            .pragma_query_value(None, "freelist_count", |row| row.get(0))
            .context("failed to inspect Local history freelist")?;
        let reclaimed = freelist > 0;
        if freelist > 0 {
            let pages = freelist.min(RETENTION_VACUUM_PAGE_LIMIT);
            let mut statement = connection
                .prepare(&format!("PRAGMA incremental_vacuum({pages})"))
                .context("failed to prepare incremental Local history page reclamation")?;
            let mut rows = statement
                .query([])
                .context("failed to start incremental Local history page reclamation")?;
            while rows
                .next()
                .context("failed during incremental Local history page reclamation")?
                .is_some()
            {}
        }
        drop(connection);
        if reclaimed {
            self.checkpoint_wal()?;
        }
        Ok(())
    }

    fn retained_store_size(&self) -> Result<u64> {
        Self::retained_bytes(&self.lock_connection())
    }

    fn retained_bytes(connection: &Connection) -> Result<u64> {
        // SQLite page_count includes the current logical database in the WAL.
        // Physical WAL bytes may contain many historical copies pinned by a
        // reader; deleting more evidence cannot reclaim those copies.
        let page_size: i64 = connection
            .pragma_query_value(None, "page_size", |row| row.get(0))
            .context("failed to inspect Local history page size")?;
        let freelist: i64 = connection
            .pragma_query_value(None, "freelist_count", |row| row.get(0))
            .context("failed to inspect Local history freelist")?;
        let page_count: i64 = connection
            .pragma_query_value(None, "page_count", |row| row.get(0))
            .context("failed to inspect Local history page count")?;
        let retained = u64::try_from(page_size)
            .ok()
            .and_then(|page_size| {
                u64::try_from(page_count.saturating_sub(freelist))
                    .ok()
                    .and_then(|freelist| page_size.checked_mul(freelist))
            })
            .context("Local history page accounting exceeded supported bounds")?;
        Ok(retained)
    }
}
