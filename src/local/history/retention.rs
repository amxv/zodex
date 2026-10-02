use anyhow::{Context, Result};

use super::store::{HistoryStore, now_ms, physical_store_size, recompute_summaries};

pub(super) const SIZE_RETENTION_BATCH_LIMIT: i64 = 256;
const RETENTION_VACUUM_PAGE_LIMIT: i64 = 256;

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
            transaction
                .execute("DELETE FROM active_process_invocations", [])
                .context("failed to reset Local active-process retention guards")?;
            {
                let mut insert = transaction
                    .prepare("INSERT INTO active_process_invocations(invocation_id) VALUES (?1)")
                    .context("failed to prepare Local active-process retention guards")?;
                for invocation_id in active_process_invocation_ids {
                    insert
                        .execute([invocation_id])
                        .with_context(|| {
                            format!(
                                "failed to protect active Local process invocation {invocation_id} from retention"
                            )
                        })?;
                }
            }
            let deleted = transaction
                .execute(
                    "DELETE FROM invocations
                     WHERE evidence_state != 'pending'
                       AND capture_state != 'pending'
                       AND id NOT IN (SELECT invocation_id FROM active_process_invocations)
                       AND COALESCE(completed_at_ms, started_at_ms) < ?1",
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
            self.checkpoint_wal()?;
            retained_size = self.retained_store_size()?;
        }
        let mut deleted_any = false;
        while retained_size > max_size_bytes {
            if self.delete_oldest_complete_invocation_batch()? == 0 {
                break;
            }
            deleted_any = true;
            self.checkpoint_wal()?;
            retained_size = self.retained_store_size()?;
        }
        if deleted_any {
            self.reclaim_pages_bounded()?;
        }
        let over_budget = physical_store_size(self.path())? > max_size_bytes;
        self.set_retention_state(over_budget, None)?;
        Ok(())
    }

    pub(super) fn delete_oldest_complete_invocation_batch(&self) -> Result<usize> {
        let mut connection = self.lock_connection();
        let eligible_count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM invocations
                 WHERE evidence_state != 'pending' AND capture_state != 'pending'
                   AND id NOT IN (SELECT invocation_id FROM active_process_invocations)",
                [],
                |row| row.get(0),
            )
            .context("failed to count size-retention candidates")?;
        let delete_limit = (eligible_count - 1).clamp(0, SIZE_RETENTION_BATCH_LIMIT);
        if delete_limit == 0 {
            return Ok(0);
        }

        let transaction = connection
            .transaction()
            .context("failed to start size-retention transaction")?;
        let deleted = transaction
            .execute(
                "DELETE FROM invocations
                 WHERE id IN (
                     SELECT id FROM invocations
                     WHERE evidence_state != 'pending' AND capture_state != 'pending'
                       AND id NOT IN (SELECT invocation_id FROM active_process_invocations)
                     ORDER BY COALESCE(completed_at_ms, started_at_ms) ASC, id ASC
                     LIMIT ?1
                 )",
                [delete_limit],
            )
            .context("failed to delete oldest complete invocation batch")?;
        recompute_summaries(&transaction)?;
        transaction
            .commit()
            .context("failed to commit Local size retention")?;
        Ok(deleted)
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
        let physical_size = physical_store_size(self.path())?;
        let connection = self.lock_connection();
        let page_size: i64 = connection
            .pragma_query_value(None, "page_size", |row| row.get(0))
            .context("failed to inspect Local history page size")?;
        let freelist: i64 = connection
            .pragma_query_value(None, "freelist_count", |row| row.get(0))
            .context("failed to inspect Local history freelist")?;
        let reclaimable = u64::try_from(page_size)
            .ok()
            .and_then(|page_size| {
                u64::try_from(freelist)
                    .ok()
                    .and_then(|freelist| page_size.checked_mul(freelist))
            })
            .context("Local history page accounting exceeded supported bounds")?;
        Ok(physical_size.saturating_sub(reclaimable))
    }
}
