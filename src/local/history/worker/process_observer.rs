use super::LocalHistoryRuntime;
use crate::session::{OwnedProcess, OwnedProcessEnd, OwnedProcessObserver};
use anyhow::{Context, Result};
use std::sync::atomic::Ordering;

impl OwnedProcessObserver for LocalHistoryRuntime {
    fn process_started(&self, process: &OwnedProcess) -> Result<()> {
        let invocation_id = process
            .created_by
            .invocation_id
            .context("active Local process is missing creator invocation ID")?;
        // Reserve terminal capacity before returning from start observation.
        let tracked = self
            .lifecycle
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .start(
                invocation_id,
                process.created_by.agent_id.as_deref().map(str::to_owned),
            );
        let newly_active = self
            .active_process_invocation_ids
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(invocation_id, false)
            .is_none();
        if newly_active {
            self.active_process_count.fetch_add(1, Ordering::AcqRel);
            if let Some(agent) = process.created_by.agent_id.as_deref() {
                let mut counts = self
                    .active_process_counts
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                *counts.entry(agent.to_owned()).or_default() += 1;
            }
        }
        if !tracked {
            self.health.note_evidence_incomplete(invocation_id);
            self.health.degrade_nonblocking(
                "Process lifecycle history unavailable: bounded terminal-state slots are full",
            );
        }
        Ok(())
    }

    fn process_ended(&self, process: &OwnedProcess, end: &OwnedProcessEnd) -> Result<()> {
        let Some(invocation_id) = process.created_by.invocation_id else {
            return Ok(());
        };
        // Mark ending first; persistence can only release the retention guard
        // after the actual terminal observation has been accepted into its slot.
        let was_active = {
            let mut processes = self
                .active_process_invocation_ids
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match processes.get_mut(&invocation_id) {
                Some(ending) if !*ending => {
                    *ending = true;
                    true
                }
                _ => false,
            }
        };
        if was_active {
            self.active_process_count.fetch_sub(1, Ordering::AcqRel);
            if let Some(agent) = process.created_by.agent_id.as_deref() {
                let mut counts = self
                    .active_process_counts
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if let Some(count) = counts.get_mut(agent) {
                    *count = count.saturating_sub(1);
                    if *count == 0 {
                        counts.remove(agent);
                    }
                }
            }
        }
        let tracked = self
            .lifecycle
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .end(invocation_id, end);
        if !tracked {
            self.active_process_invocation_ids
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(&invocation_id);
            if was_active {
                self.health.note_evidence_incomplete(invocation_id);
                self.health.degrade_nonblocking(
                    "Process lifecycle history unavailable: no terminal-state slot was reserved",
                );
            }
        }
        Ok(())
    }
}
