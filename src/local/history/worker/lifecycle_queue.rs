//! Reserved lifecycle slots cannot lose a terminal observation to queue pressure.
use super::{
    HistoryWorkerShared, Ordering, PRESENTATION_SCHEMA_VERSION, json, refresh_and_emit_presentation,
};
use crate::session::OwnedProcessEnd;
use std::collections::HashMap;

const MAX_LIFECYCLE_SLOTS: usize = 4096;
const MAX_END_BYTES: usize = 16 * 1024;

#[derive(Clone)]
pub(super) struct LifecycleSlot {
    pub(super) invocation_id: i64,
    agent_id: Option<String>,
    generation: u64,
    dirty: bool,
    end: Option<OwnedProcessEnd>,
}

#[derive(Default)]
pub(super) struct LifecycleQueue {
    slots: HashMap<i64, LifecycleSlot>,
}

impl LifecycleQueue {
    pub(super) fn start(&mut self, invocation_id: i64, agent_id: Option<String>) -> bool {
        if self.slots.contains_key(&invocation_id) {
            return true;
        }
        if self.slots.len() >= MAX_LIFECYCLE_SLOTS {
            return false;
        }
        self.slots.insert(
            invocation_id,
            LifecycleSlot {
                invocation_id,
                agent_id,
                generation: 0,
                dirty: true,
                end: None,
            },
        );
        true
    }

    pub(super) fn end(&mut self, invocation_id: i64, end: &OwnedProcessEnd) -> bool {
        let Some(slot) = self.slots.get_mut(&invocation_id) else {
            return false;
        };
        if slot.end.is_some() {
            return true;
        }
        let bytes = end
            .final_cwd
            .as_ref()
            .map_or(0, String::len)
            .saturating_add(end.incomplete_reason.as_ref().map_or(0, String::len));
        slot.end = Some(if bytes > MAX_END_BYTES {
            OwnedProcessEnd::incomplete(
                "Process terminal metadata exceeded the bounded history budget",
                None,
            )
        } else {
            end.clone()
        });
        slot.generation += 1;
        slot.dirty = true;
        true
    }

    fn batch(&self) -> Vec<LifecycleSlot> {
        self.slots
            .values()
            .filter(|slot| slot.dirty)
            .take(super::OUTPUT_BATCH_LIMIT)
            .cloned()
            .collect()
    }

    fn acknowledge(&mut self, snapshot: &LifecycleSlot) -> bool {
        let Some(current) = self.slots.get_mut(&snapshot.invocation_id) else {
            return false;
        };
        if current.generation != snapshot.generation {
            return false;
        }
        if snapshot.end.is_some() {
            self.slots.remove(&snapshot.invocation_id);
        } else {
            current.dirty = false;
        }
        true
    }

    pub(super) fn has_pending(&self) -> bool {
        self.slots.values().any(|slot| slot.dirty)
    }
}

pub(super) fn drain(shared: &HistoryWorkerShared) {
    let snapshots = shared
        .lifecycle
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .batch();
    for snapshot in snapshots {
        if shared
            .admissions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains(snapshot.invocation_id)
        {
            continue;
        }
        // Producer-shared locks are never held across persistence. Replaying the
        // same start/end is idempotent, and an end racing a start ack stays dirty.
        let result = shared
            .store
            .process_started(snapshot.invocation_id)
            .and_then(|()| match snapshot.end.as_ref() {
                Some(end) => shared.store.process_ended(snapshot.invocation_id, end),
                None => Ok(()),
            });
        if let Err(error) = result {
            shared.health.note_storage_error(&error);
            shared
                .health
                .degrade_nonblocking(format!("Process lifecycle queued for retry: {error:#}"));
            continue;
        }
        let acknowledged = shared
            .lifecycle
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .acknowledge(&snapshot);
        if !acknowledged {
            continue;
        }
        if snapshot.end.is_some() {
            shared
                .active_process_invocation_ids
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(&snapshot.invocation_id);
        }
        let state = match snapshot.end.as_ref() {
            None => "running",
            Some(end) if end.is_complete() => "exited",
            Some(_) => "incomplete",
        };
        let active_count = shared.active_process_count.load(Ordering::Acquire);
        let agent_count = snapshot.agent_id.as_ref().map(|agent| {
            shared
                .active_process_counts
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(agent)
                .copied()
                .unwrap_or(0)
        });
        let event_name = if snapshot.end.is_some() {
            "process_ended"
        } else {
            "process_started"
        };
        shared.events.clear_presentation(snapshot.invocation_id);
        let live = shared.events.emit_with(event_name, snapshot.agent_id.as_deref(), Some(snapshot.invocation_id), Some(snapshot.invocation_id), Some(PRESENTATION_SCHEMA_VERSION), || json!({
            "active_process_count": active_count,
            "agent_active_process_count": agent_count,
            "process_state": state,
            "exit_code": snapshot.end.as_ref().and_then(|end| end.exit_code),
            "termination_reason": snapshot.end.as_ref().and_then(|end| end.termination_reason),
            "incomplete_reason": snapshot.end.as_ref().and_then(|end| end.incomplete_reason.as_deref()),
        }));
        refresh_and_emit_presentation(
            &shared.store,
            &shared.events,
            snapshot.invocation_id,
            Some(snapshot.invocation_id),
            snapshot.agent_id.as_deref(),
            event_name,
            live,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::TerminationReason;

    fn end() -> OwnedProcessEnd {
        OwnedProcessEnd::exited(7, TerminationReason::Exit, "/tmp".into())
    }

    #[test]
    fn capacity_reserves_all_terminal_updates_and_recovers_after_ack() {
        let mut queue = LifecycleQueue::default();
        for id in 0..MAX_LIFECYCLE_SLOTS as i64 {
            assert!(queue.start(id, Some("abcd".into())));
        }
        assert!(!queue.start(MAX_LIFECYCLE_SLOTS as i64, None));
        for id in 0..MAX_LIFECYCLE_SLOTS as i64 {
            assert!(queue.end(id, &end()));
        }
        assert_eq!(queue.slots.len(), MAX_LIFECYCLE_SLOTS);
        while queue.has_pending() {
            for slot in queue.batch() {
                assert_eq!(slot.end.as_ref().unwrap().exit_code, Some(7));
                assert!(queue.acknowledge(&slot));
            }
        }
        assert!(queue.start(MAX_LIFECYCLE_SLOTS as i64, None));
    }

    #[test]
    fn terminal_observation_during_start_persistence_cannot_be_acknowledged_away() {
        let mut queue = LifecycleQueue::default();
        assert!(queue.start(1, None));
        let writing_start = queue.batch().pop().unwrap();
        assert!(queue.end(1, &end()));
        assert!(!queue.acknowledge(&writing_start));
        assert!(queue.has_pending());
        let writing_end = queue.batch().pop().unwrap();
        assert_eq!(writing_end.end.unwrap().exit_code, Some(7));
    }

    #[test]
    fn lifecycle_observation_finishes_while_storage_is_blocked() {
        use super::super::{LocalHistoryRuntime, LocalHistoryRuntimeConfig};
        use crate::invocation::{
            InvocationContext, InvocationEvidenceRecorder, InvocationOutcome, InvocationStart,
        };
        use crate::local::history::store::HistoryStore;
        use crate::session::{
            OwnedProcess, OwnedProcessObserver, ProcessBirthIdentity, ProcessIdentity,
            SessionOutputCompletion, SessionOutputObserver,
        };
        use std::sync::{Arc, mpsc};
        use std::time::Duration;
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("history.sqlite3");
        let store =
            Arc::new(HistoryStore::open(path.clone(), Arc::from("lifecycle-slots")).unwrap());
        let runtime = LocalHistoryRuntime::open_with_store(
            LocalHistoryRuntimeConfig::new(
                path.clone(),
                "lifecycle-slots",
                86400,
                1024 * 1024 * 1024,
            ),
            Some(store.clone()),
        )
        .unwrap();
        let storage = store.lock_connection();
        let locker = rusqlite::Connection::open(&path).unwrap();
        locker.execute_batch("BEGIN IMMEDIATE").unwrap();
        let (send, receive) = mpsc::channel();
        let history = runtime.clone();
        let producer = std::thread::spawn(move || {
            let context = history
                .begin(
                    InvocationContext::default(),
                    InvocationStart::new("exec_command", json!({"cmd":"true","workdir":"/tmp"})),
                )
                .unwrap();
            let process = OwnedProcess {
                internal_session_id: 1,
                session_handle: Arc::from("slot-test"),
                identity: ProcessIdentity {
                    pid: 42,
                    birth: ProcessBirthIdentity::LinuxProcStartTicks { ticks: 1 },
                },
                created_by: context.clone(),
            };
            history.process_started(&process).unwrap();
            let active = history.active_process_count();
            history.process_ended(&process, &end()).unwrap();
            let ended = history.active_process_count();
            history
                .complete(
                    &context,
                    InvocationOutcome::Success(json!({"status":"exited"})),
                )
                .unwrap();
            history.observe_output_complete(SessionOutputCompletion {
                internal_session_id: 1,
                session_handle: Arc::from("slot-test"),
                invocation: context.clone(),
            });
            send.send((context.invocation_id.unwrap(), active, ended))
                .unwrap();
        });
        let result = receive.recv_timeout(Duration::from_millis(250));
        locker.execute_batch("ROLLBACK").unwrap();
        drop(storage);
        producer.join().unwrap();
        let (id, active, ended) = result.expect("lifecycle observer waited on storage");
        assert_eq!(active, 1);
        assert_eq!(
            ended, 0,
            "live counts must reflect the observation before storage recovers"
        );
        runtime.flush_for_test().unwrap();
        let row: (String, i64) = store
            .lock_connection()
            .query_row(
                "SELECT process_state, process_exit_code FROM invocations WHERE id = ?1",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(row, ("exited".to_owned(), 7));
        assert!(!runtime.active_process_invocation_ids().contains(&id));
        runtime.shutdown_blocking().unwrap();
    }

    #[test]
    fn oversized_end_metadata_is_explicitly_incomplete_and_bounded() {
        let mut queue = LifecycleQueue::default();
        queue.start(1, None);
        queue.end(
            1,
            &OwnedProcessEnd::incomplete("x".repeat(MAX_END_BYTES + 1), None),
        );
        let slot = queue.batch().pop().unwrap();
        assert!(!slot.end.as_ref().unwrap().is_complete());
        assert!(
            slot.end
                .as_ref()
                .unwrap()
                .incomplete_reason
                .as_ref()
                .unwrap()
                .len()
                < 128
        );
    }
}
