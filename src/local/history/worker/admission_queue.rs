use std::collections::{HashSet, VecDeque};

use crate::invocation::{InvocationContext, InvocationStart};

const MAX_PENDING_ADMISSIONS: usize = 1024;
const MAX_PENDING_ADMISSION_BYTES: usize = 16 * 1024 * 1024;

pub(super) struct PendingAdmission {
    pub(super) context: InvocationContext,
    pub(super) start: InvocationStart,
    pub(super) started_at_ms: i64,
    bytes: usize,
}

#[derive(Default)]
pub(super) struct AdmissionQueue {
    queue: VecDeque<PendingAdmission>,
    pending: HashSet<i64>,
    bytes: usize,
}

impl AdmissionQueue {
    pub(super) fn push(
        &mut self,
        context: InvocationContext,
        start: InvocationStart,
        started_at_ms: i64,
    ) -> bool {
        let bytes = start.arguments.to_string().len().saturating_add(512);
        if self.pending.len() >= MAX_PENDING_ADMISSIONS
            || self.bytes.saturating_add(bytes) > MAX_PENDING_ADMISSION_BYTES
        {
            return false;
        }
        self.pending
            .insert(context.invocation_id.expect("reserved invocation ID"));
        self.bytes += bytes;
        self.queue.push_back(PendingAdmission {
            context,
            start,
            started_at_ms,
            bytes,
        });
        true
    }

    pub(super) fn pop(&mut self) -> Option<PendingAdmission> {
        self.queue.pop_front()
    }

    pub(super) fn retry(&mut self, admission: PendingAdmission) {
        self.queue.push_back(admission);
    }

    pub(super) fn finish(&mut self, admission: &PendingAdmission) {
        self.pending
            .remove(&admission.context.invocation_id.unwrap());
        self.bytes = self.bytes.saturating_sub(admission.bytes);
    }

    pub(super) fn contains(&self, invocation_id: i64) -> bool {
        self.pending.contains(&invocation_id)
    }

    pub(super) fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }
}
