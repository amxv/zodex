use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::Result;
use serde_json::json;
use tracing::{error, warn};

use crate::invocation::{
    InvocationContext, InvocationEvidenceRecorder, InvocationOutcome, InvocationStart,
    ProviderCallMetadata,
};
use crate::local::file_evidence::{
    CompletedFileEvidence, PendingFileEvidence, complete_file_evidence, prepare_file_evidence,
};
use crate::local::presentation::PRESENTATION_SCHEMA_VERSION;
use crate::session::{
    OwnedProcessEnd, SessionOutputChunk, SessionOutputCompletion, SessionOutputObserver,
};

use super::events::{HistoryEventHub, HistoryLiveEvent};
use super::live_display::LiveDisplayStreams;
use super::materialized::refresh_and_emit_presentation;
use super::store::{
    HistoryStore, OutputEvent, distinct_invocation_ids, mark_retention_error_best_effort,
    normalize_declared_workdir, now_ms,
};

mod output_queue;
mod process_observer;

const DEFAULT_OUTPUT_QUEUE_CAPACITY: usize = 4096;
const OUTPUT_BATCH_LIMIT: usize = 128;
const LIVE_OUTPUT_EVENT_BATCH_MAX_CHUNKS: usize = 64;
const COMPLETION_OVERFLOW_MAX_ITEMS: usize = 256;
const OUTPUT_BATCH_WAIT: Duration = Duration::from_millis(25);
const RETENTION_MIN_INTERVAL: Duration = Duration::from_secs(30);
const MATERIALIZATION_BACKFILL_INTERVAL: Duration = Duration::from_millis(250);
const SHUTDOWN_CAPTURE_DRAIN_TIMEOUT: Duration = Duration::from_secs(10);
const SHUTDOWN_CAPTURE_POLL_INTERVAL: Duration = Duration::from_millis(10);

#[derive(Debug, Clone)]
pub struct LocalHistoryRuntimeConfig {
    pub database_path: PathBuf,
    pub runtime_id: Arc<str>,
    pub max_age_seconds: u64,
    pub max_size_bytes: u64,
    pub output_queue_capacity: usize,
    pub event_capacity: usize,
}

impl LocalHistoryRuntimeConfig {
    pub fn new(
        database_path: PathBuf,
        runtime_id: impl Into<Arc<str>>,
        max_age_seconds: u64,
        max_size_bytes: u64,
    ) -> Self {
        Self {
            database_path,
            runtime_id: runtime_id.into(),
            max_age_seconds,
            max_size_bytes,
            output_queue_capacity: DEFAULT_OUTPUT_QUEUE_CAPACITY,
            event_capacity: HistoryEventHub::default_capacity(),
        }
    }

    pub fn with_output_queue_capacity(mut self, capacity: usize) -> Self {
        self.output_queue_capacity = capacity.max(1);
        self
    }

    pub fn with_event_capacity(mut self, capacity: usize) -> Self {
        self.event_capacity = capacity.max(1);
        self
    }
}

struct HistoryHealth {
    degraded: AtomicBool,
    reason: Mutex<Option<String>>,
    incomplete_evidence_invocations: Mutex<HashSet<i64>>,
    incomplete_capture_invocations: Mutex<HashMap<i64, String>>,
    maintenance_requested: AtomicBool,
}

impl HistoryHealth {
    fn new() -> Self {
        Self {
            degraded: AtomicBool::new(false),
            reason: Mutex::new(None),
            incomplete_evidence_invocations: Mutex::new(HashSet::new()),
            incomplete_capture_invocations: Mutex::new(HashMap::new()),
            maintenance_requested: AtomicBool::new(false),
        }
    }

    fn degrade_nonblocking(&self, reason: impl Into<String>) {
        let reason = reason.into();
        self.degraded.store(true, Ordering::Release);
        let mut guard = self
            .reason
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if guard.is_none() {
            *guard = Some(reason.clone());
        }
        drop(guard);
        error!(event = "local_history_degraded", error = %reason);
    }

    fn degrade_persisting(&self, store: &HistoryStore, reason: impl Into<String>) {
        self.degrade_nonblocking(reason);
        self.persist_degraded_state(store);
    }

    fn persist_degraded_state(&self, store: &HistoryStore) {
        if !self.degraded.load(Ordering::Acquire) {
            return;
        }
        let reason = self
            .reason
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
            .unwrap_or_else(|| "unknown evidence-pipeline failure".to_string());
        if let Err(error) = store.set_health("degraded", Some(&reason)) {
            error!(
                event = "local_history_health_persistence_failed",
                original_error = %reason,
                error = %error,
            );
        }
        let incomplete_evidence = self
            .incomplete_evidence_invocations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        for invocation_id in incomplete_evidence {
            if let Err(error) = store.mark_evidence_incomplete(invocation_id, &reason) {
                error!(
                    event = "local_history_evidence_incomplete_persistence_failed",
                    invocation_id,
                    error = %error,
                );
            }
        }
        self.persist_capture_failures(store);
    }

    fn persist_capture_failures(&self, store: &HistoryStore) {
        let incomplete_capture = self
            .incomplete_capture_invocations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        for (invocation_id, reason) in incomplete_capture {
            if let Err(error) = store.mark_capture_incomplete(invocation_id, &reason) {
                error!(
                    event = "local_history_capture_incomplete_persistence_failed",
                    invocation_id,
                    error = %error,
                );
            } else {
                self.incomplete_capture_invocations
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .remove(&invocation_id);
            }
        }
    }

    fn note_evidence_incomplete(&self, invocation_id: i64) {
        self.incomplete_evidence_invocations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(invocation_id);
    }

    fn note_capture_incomplete(&self, invocation_id: i64, reason: String) {
        self.incomplete_capture_invocations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry(invocation_id)
            .or_insert(reason);
    }
}

enum WorkerMessage {
    Output(OutputEvent),
    Complete {
        context: InvocationContext,
        outcome: InvocationOutcome,
        file_evidence: Vec<CompletedFileEvidence>,
    },
    ProcessStarted {
        invocation_id: i64,
        agent_id: Option<String>,
        active_process_count: u64,
        agent_active_process_count: Option<u64>,
    },
    ProcessEnded {
        invocation_id: i64,
        agent_id: Option<String>,
        end: OwnedProcessEnd,
        was_active: bool,
    },
    #[cfg(test)]
    Barrier(std::sync::mpsc::Sender<()>),
    Shutdown,
}

#[derive(Debug, Clone)]
struct PendingPresentationRefresh {
    invocation_id: Option<i64>,
    fallback_agent_id: Option<String>,
    source: &'static str,
    emit_live: bool,
}

struct HistoryWorkerShared {
    store: Arc<HistoryStore>,
    health: Arc<HistoryHealth>,
    events: Arc<HistoryEventHub>,
    completion_overflow: Arc<Mutex<VecDeque<WorkerMessage>>>,
    pending_presentation_refreshes: Arc<Mutex<HashMap<i64, PendingPresentationRefresh>>>,
    active_process_invocation_ids: Arc<Mutex<HashMap<i64, bool>>>,
    active_process_counts: Arc<Mutex<HashMap<String, u64>>>,
    active_process_count: Arc<AtomicU64>,
    worker_running: Arc<AtomicBool>,
}

pub struct LocalHistoryRuntime {
    store: Arc<HistoryStore>,
    runtime_id: Arc<str>,
    events: Arc<HistoryEventHub>,
    sender: SyncSender<WorkerMessage>,
    completion_overflow: Arc<Mutex<VecDeque<WorkerMessage>>>,
    pending_presentation_refreshes: Arc<Mutex<HashMap<i64, PendingPresentationRefresh>>>,
    worker_running: Arc<AtomicBool>,
    output_overflow: Mutex<output_queue::OutputOverflow>,
    worker: Mutex<Option<JoinHandle<()>>>,
    health: Arc<HistoryHealth>,
    file_evidence: Mutex<HashMap<i64, Vec<PendingFileEvidence>>>,
    // false = running; true = process end observed but not durably committed.
    // Both remain retention-protected until the worker commits the end state.
    active_process_invocation_ids: Arc<Mutex<HashMap<i64, bool>>>,
    active_process_counts: Arc<Mutex<HashMap<String, u64>>>,
    active_process_count: Arc<AtomicU64>,
    agent_first_seen_observer: Mutex<Option<AgentFirstSeenObserver>>,
}

type AgentFirstSeenObserver = Arc<dyn Fn(String) + Send + Sync>;

impl LocalHistoryRuntime {
    pub fn open(config: LocalHistoryRuntimeConfig) -> Result<Arc<Self>> {
        Self::open_with_store(config, None)
    }

    pub(super) fn open_with_store(
        config: LocalHistoryRuntimeConfig,
        store_override: Option<Arc<HistoryStore>>,
    ) -> Result<Arc<Self>> {
        let runtime_id = config.runtime_id.clone();
        let events = HistoryEventHub::new(runtime_id.clone(), config.event_capacity);
        let store = match store_override {
            Some(store) => store,
            None => Arc::new(HistoryStore::open(
                config.database_path,
                runtime_id.clone(),
            )?),
        };
        let max_age_seconds = config.max_age_seconds;
        let max_size_bytes = config.max_size_bytes;
        // Enforce retention before admitting the first invocation. Keeping
        // this pass out of the writer loop avoids delaying queued completions.
        if let Err(error) = store.run_retention(max_age_seconds, max_size_bytes, &[]) {
            mark_retention_error_best_effort(&store, &error.to_string());
        }
        let (sender, receiver) = std::sync::mpsc::sync_channel(config.output_queue_capacity.max(1));
        let completion_overflow = Arc::new(Mutex::new(VecDeque::new()));
        let pending_presentation_refreshes = Arc::new(Mutex::new(HashMap::new()));
        let active_process_invocation_ids = Arc::new(Mutex::new(HashMap::new()));
        let active_process_counts = Arc::new(Mutex::new(HashMap::new()));
        let active_process_count = Arc::new(AtomicU64::new(0));
        let worker_running = Arc::new(AtomicBool::new(true));
        let health = Arc::new(HistoryHealth::new());
        health.maintenance_requested.store(
            store
                .physical_size()
                .is_ok_and(|size| size > max_size_bytes),
            Ordering::Release,
        );
        let worker_shared = HistoryWorkerShared {
            store: store.clone(),
            health: health.clone(),
            events: events.clone(),
            completion_overflow: completion_overflow.clone(),
            pending_presentation_refreshes: pending_presentation_refreshes.clone(),
            active_process_invocation_ids: active_process_invocation_ids.clone(),
            active_process_counts: active_process_counts.clone(),
            active_process_count: active_process_count.clone(),
            worker_running: worker_running.clone(),
        };
        let worker = std::thread::Builder::new()
            .name("zodex-local-history".to_string())
            .spawn(move || run_worker(receiver, worker_shared, max_age_seconds, max_size_bytes))?;
        Ok(Arc::new(Self {
            store,
            runtime_id,
            events,
            sender,
            completion_overflow,
            pending_presentation_refreshes,
            worker_running,
            output_overflow: Mutex::new(output_queue::OutputOverflow::default()),
            worker: Mutex::new(Some(worker)),
            health,
            file_evidence: Mutex::new(HashMap::new()),
            active_process_invocation_ids,
            active_process_counts,
            active_process_count,
            agent_first_seen_observer: Mutex::new(None),
        }))
    }

    #[cfg(any(target_os = "macos", test))]
    pub(crate) fn install_agent_first_seen_observer(
        &self,
        observer: AgentFirstSeenObserver,
    ) -> Result<()> {
        let mut guard = self
            .agent_first_seen_observer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if guard.is_some() {
            anyhow::bail!("Local Agent first-seen observer is already installed")
        }
        *guard = Some(observer);
        Ok(())
    }

    pub fn runtime_id(&self) -> &str {
        &self.runtime_id
    }

    pub(crate) fn claim_global_context_injection(
        &self,
        provider: &ProviderCallMetadata,
    ) -> Result<bool> {
        self.store.claim_global_context_injection(provider)
    }

    pub(crate) fn claim_repo_agents_check(
        &self,
        provider: &ProviderCallMetadata,
        normalized_workdir: &str,
    ) -> Result<bool> {
        self.store
            .claim_repo_agents_check(provider, normalized_workdir)
    }

    pub(crate) fn claim_repo_skills_check(
        &self,
        provider: &ProviderCallMetadata,
        normalized_workdir: &str,
    ) -> Result<bool> {
        self.store
            .claim_repo_skills_check(provider, normalized_workdir)
    }

    pub fn active_process_counts(&self) -> HashMap<String, u64> {
        self.active_process_counts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub fn active_process_count(&self) -> u64 {
        self.active_process_count.load(Ordering::Acquire)
    }

    pub(crate) fn active_process_invocation_ids(&self) -> Vec<i64> {
        self.active_process_invocation_ids
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .keys()
            .copied()
            .collect()
    }

    #[cfg(test)]
    pub(crate) fn subscribe_live_events(
        &self,
    ) -> (u64, tokio::sync::broadcast::Receiver<HistoryLiveEvent>) {
        (self.events.current_sequence(), self.events.subscribe())
    }

    pub(crate) fn subscribe_live_event_channels(
        &self,
    ) -> (
        u64,
        tokio::sync::broadcast::Receiver<HistoryLiveEvent>,
        tokio::sync::broadcast::Receiver<HistoryLiveEvent>,
    ) {
        (
            self.events.current_sequence(),
            self.events.subscribe(),
            self.events.subscribe_output(),
        )
    }

    #[cfg(test)]
    pub(crate) fn subscribe_live_output_events(
        &self,
    ) -> tokio::sync::broadcast::Receiver<HistoryLiveEvent> {
        self.events.subscribe_output()
    }

    pub(crate) fn live_presentation(
        &self,
        root_invocation_id: i64,
        projection: super::HistoryDiffProjection,
    ) -> Option<Arc<crate::local::presentation::PresentationRecord>> {
        self.events.presentation(root_invocation_id, projection)
    }

    fn queue_presentation_refresh(
        &self,
        root_invocation_id: Option<i64>,
        invocation_id: Option<i64>,
        fallback_agent_id: Option<&str>,
        source: &'static str,
        emit_live: bool,
    ) {
        let Some(root_invocation_id) = root_invocation_id else {
            return;
        };
        if !self.worker_running.load(Ordering::Acquire) {
            self.health
                .degrade_nonblocking("Local history presentation writer is unavailable");
            return;
        }
        self.events.clear_presentation(root_invocation_id);
        let mut pending = self
            .pending_presentation_refreshes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        pending
            .entry(root_invocation_id)
            .and_modify(|refresh| {
                refresh.invocation_id = invocation_id.or(refresh.invocation_id);
                if fallback_agent_id.is_some() {
                    refresh.fallback_agent_id = fallback_agent_id.map(str::to_owned);
                }
                refresh.source = source;
                refresh.emit_live |= emit_live;
            })
            .or_insert_with(|| PendingPresentationRefresh {
                invocation_id,
                fallback_agent_id: fallback_agent_id.map(str::to_owned),
                source,
                emit_live,
            });
    }

    #[cfg(test)]
    pub(crate) fn live_event_subscriber_count(&self) -> usize {
        self.events.receiver_count()
    }

    #[cfg(test)]
    pub(crate) fn live_event_sequence(&self) -> u64 {
        self.events.current_sequence()
    }

    pub fn database_path(&self) -> &std::path::Path {
        self.store.path()
    }

    #[cfg(test)]
    pub(crate) fn history_degraded(&self) -> bool {
        self.health.degraded.load(Ordering::Acquire)
    }

    pub fn physical_size_bytes(&self) -> Result<u64> {
        self.store.physical_size()
    }

    pub fn request_retention(&self) {
        self.health
            .maintenance_requested
            .store(true, Ordering::Release);
    }

    pub fn run_retention_now(&self, max_age_seconds: u64, max_size_bytes: u64) -> Result<()> {
        let active_process_invocation_ids = self
            .active_process_invocation_ids
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .keys()
            .copied()
            .collect::<Vec<_>>();
        self.store.run_retention(
            max_age_seconds,
            max_size_bytes,
            &active_process_invocation_ids,
        )
    }

    #[cfg(test)]
    pub(crate) fn flush_for_test(&self) -> Result<()> {
        let (acknowledge, acknowledged) = std::sync::mpsc::channel();
        self.sender
            .send(WorkerMessage::Barrier(acknowledge))
            .map_err(|_| {
                anyhow::anyhow!("Local history writer is unavailable during test flush")
            })?;
        acknowledged.recv().map_err(|_| {
            anyhow::anyhow!("Local history writer stopped before test flush completed")
        })
    }

    pub fn shutdown_blocking(&self) -> Result<()> {
        let handle = self
            .worker
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        let Some(handle) = handle else {
            return Ok(());
        };
        // The command-result path intentionally has a short PTY drain bound so
        // a descendant that keeps the slave open cannot hold the MCP response
        // forever. Runtime shutdown owns a separate bounded evidence-finalize
        // window: keep the history worker alive while any late reader EOF/EIO
        // markers arrive, then mark the remaining captures explicitly
        // incomplete rather than leaving durable `pending` rows behind.
        let capture_deadline = Instant::now() + SHUTDOWN_CAPTURE_DRAIN_TIMEOUT;
        let mut finalize_error = None;
        loop {
            let pending = match self.store.pending_capture_ids() {
                Ok(pending) => pending,
                Err(error) => {
                    warn!(
                        event = "local_history_shutdown_capture_query_failed",
                        error = %error,
                    );
                    finalize_error = Some(
                        error.context("failed while finalizing Local output capture at shutdown"),
                    );
                    break;
                }
            };
            if pending.is_empty() {
                break;
            }
            if Instant::now() >= capture_deadline {
                for invocation_id in pending {
                    if let Err(error) = self.store.mark_capture_incomplete(
                        invocation_id,
                        "Local runtime shutdown ended before PTY capture reached terminal EOF",
                    ) {
                        warn!(
                            event = "local_history_shutdown_capture_finalize_failed",
                            invocation_id,
                            error = %error,
                        );
                        if finalize_error.is_none() {
                            finalize_error = Some(error.context(
                                "failed to finalize incomplete Local output capture at shutdown",
                            ));
                        }
                    }
                }
                break;
            }
            std::thread::sleep(SHUTDOWN_CAPTURE_POLL_INTERVAL);
        }
        // After the finalize window, a blocking send is safe: the worker keeps
        // draining everything already queued before it observes Shutdown. A
        // pathological late producer may still race after the deadline, but
        // that invocation is already durably marked capture-incomplete.
        let _ = self.sender.send(WorkerMessage::Shutdown);
        handle
            .join()
            .map_err(|_| anyhow::anyhow!("Local history writer thread panicked"))?;
        if let Err(error) = self.store.mark_pending_file_evidence_unavailable(
            "Local runtime shutdown ended before after-file evidence completed",
        ) {
            warn!(
                event = "local_history_shutdown_file_evidence_finalize_failed",
                error = %error,
            );
            if finalize_error.is_none() {
                finalize_error = Some(error);
            }
        }
        if let Some(error) = finalize_error {
            Err(error)
        } else {
            Ok(())
        }
    }
}

impl InvocationEvidenceRecorder for LocalHistoryRuntime {
    fn begin(
        &self,
        context: InvocationContext,
        start: InvocationStart,
    ) -> Result<InvocationContext> {
        let pending_file_evidence = prepare_file_evidence(&start);
        let tool_name = start.tool_name.clone();
        let normalized_workdir = start
            .arguments
            .get("workdir")
            .and_then(serde_json::Value::as_str)
            .and_then(normalize_declared_workdir);
        match self.store.begin_with_metadata(context, start) {
            Ok(begin) => {
                let context = begin.context;
                let invocation_id = context.invocation_id;
                let presentation_root_invocation_id = begin.presentation_root_invocation_id;
                let agent_id = context.agent_id.as_deref();
                if begin.agent_first_seen_in_runtime {
                    if let Some(agent_id) = agent_id {
                        let observer = self
                            .agent_first_seen_observer
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .clone();
                        if let Some(observer) = observer {
                            observer(agent_id.to_string());
                        }
                    }
                    self.events.emit_with(
                        "agent_first_seen",
                        agent_id,
                        invocation_id,
                        presentation_root_invocation_id,
                        None,
                        || json!({}),
                    );
                }
                if let Some(new_workdir) = begin.new_workdir.as_deref() {
                    self.events.emit_with(
                        "agent_workdir_added",
                        agent_id,
                        invocation_id,
                        presentation_root_invocation_id,
                        Some(PRESENTATION_SCHEMA_VERSION),
                        || json!({"normalized_workdir": new_workdir}),
                    );
                }
                let emit_live = self.events.emit_with(
                    "invocation_started",
                    agent_id,
                    invocation_id,
                    presentation_root_invocation_id,
                    Some(PRESENTATION_SCHEMA_VERSION),
                    || {
                        json!({
                            "tool_name": tool_name.as_ref(),
                            "normalized_workdir": normalized_workdir,
                        })
                    },
                );
                if let Some(invocation_id) = context.invocation_id
                    && !pending_file_evidence.is_empty()
                {
                    match self
                        .store
                        .persist_file_evidence_start(invocation_id, &pending_file_evidence)
                    {
                        Ok(()) => {
                            self.file_evidence
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner)
                                .insert(invocation_id, pending_file_evidence);
                        }
                        Err(error) => warn!(
                            event = "local_file_evidence_before_persistence_failed",
                            invocation_id,
                            error = %error,
                        ),
                    }
                }
                // Refresh canonical parent cards for attached polls too. The
                // poll invocation is already durable here, so the visible poll
                // count can advance immediately instead of waiting for the
                // potentially long write_stdin call to complete.
                self.queue_presentation_refresh(
                    presentation_root_invocation_id,
                    invocation_id,
                    agent_id,
                    "invocation_started",
                    emit_live,
                );
                Ok(context)
            }
            Err(error) => {
                self.health.degrade_nonblocking(format!(
                    "invocation envelope persistence failed: {error}"
                ));
                Err(error)
            }
        }
    }

    fn complete(&self, context: &InvocationContext, outcome: InvocationOutcome) -> Result<()> {
        let invocation_id = context.invocation_id.ok_or_else(|| {
            anyhow::anyhow!("Local invocation completion is missing durable invocation ID")
        })?;
        let pending_file_evidence = self
            .file_evidence
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&invocation_id)
            .unwrap_or_default();
        let file_evidence = complete_file_evidence(&pending_file_evidence);
        let queued = WorkerMessage::Complete {
            context: context.clone(),
            outcome: outcome.clone(),
            file_evidence: file_evidence.clone(),
        };
        self.enqueue_priority_message(
            queued,
            invocation_id,
            "Local history completion writer is unavailable or saturated",
        );
        Ok(())
    }
}

impl LocalHistoryRuntime {
    fn enqueue_priority_message(
        &self,
        message: WorkerMessage,
        invocation_id: i64,
        failure_reason: &str,
    ) -> bool {
        if !self.worker_running.load(Ordering::Acquire) {
            self.health.note_evidence_incomplete(invocation_id);
            self.health.degrade_nonblocking(failure_reason);
            return false;
        }

        let message = {
            let mut priority = self
                .completion_overflow
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if priority.len() < COMPLETION_OVERFLOW_MAX_ITEMS {
                priority.push_back(message);
                return true;
            }
            message
        };

        match self.sender.try_send(message) {
            Ok(()) => true,
            Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => {
                self.health.note_evidence_incomplete(invocation_id);
                self.health.degrade_nonblocking(failure_reason);
                false
            }
        }
    }
}

impl SessionOutputObserver for LocalHistoryRuntime {
    fn observe_output(&self, chunk: SessionOutputChunk) {
        let Some(invocation_id) = chunk.invocation.invocation_id else {
            return;
        };
        let observed_at_ms = match now_ms() {
            Ok(value) => value,
            Err(error) => {
                let reason = format!("failed to timestamp Local PTY output: {error}");
                self.health
                    .note_capture_incomplete(invocation_id, reason.clone());
                self.health.degrade_nonblocking(reason);
                return;
            }
        };
        self.try_enqueue_output_chunk(
            OutputEvent::Chunk {
                invocation_id,
                agent_id: chunk.invocation.agent_id.as_deref().map(str::to_owned),
                sequence: chunk.sequence,
                observed_at_ms,
                text: chunk.text,
            },
            invocation_id,
        );
    }

    fn observe_output_complete(&self, completion: SessionOutputCompletion) {
        let Some(invocation_id) = completion.invocation.invocation_id else {
            return;
        };
        self.enqueue_output_completion(
            invocation_id,
            completion.invocation.agent_id.as_deref().map(str::to_owned),
        );
    }
}

fn run_worker(
    receiver: Receiver<WorkerMessage>,
    shared: HistoryWorkerShared,
    max_age_seconds: u64,
    max_size_bytes: u64,
) {
    struct WorkerRunningGuard(Arc<AtomicBool>);
    impl Drop for WorkerRunningGuard {
        fn drop(&mut self) {
            self.0.store(false, Ordering::Release);
        }
    }
    let _running_guard = WorkerRunningGuard(shared.worker_running.clone());
    let store = shared.store.as_ref();
    let health = shared.health.as_ref();
    let events = shared.events.as_ref();
    let mut shutdown = false;
    // `open_with_store` completed the startup retention pass before spawning
    // this worker, so future maintenance can honor the normal coalescing
    // interval instead of racing the first evidence messages.
    let mut last_maintenance = Some(Instant::now());
    let mut last_materialization_backfill = None;
    let mut materialization_backfill_complete = false;
    let mut live_display = LiveDisplayStreams::new();
    while !shutdown {
        let mut messages = Vec::with_capacity(OUTPUT_BATCH_LIMIT);
        drain_priority_completions(&shared.completion_overflow, &mut messages);
        if messages.is_empty() {
            match receiver.recv_timeout(OUTPUT_BATCH_WAIT) {
                Ok(
                    message @ (WorkerMessage::Output(_)
                    | WorkerMessage::Complete { .. }
                    | WorkerMessage::ProcessStarted { .. }
                    | WorkerMessage::ProcessEnded { .. }),
                ) => messages.push(message),
                #[cfg(test)]
                Ok(message @ WorkerMessage::Barrier(_)) => messages.push(message),
                Ok(WorkerMessage::Shutdown) => shutdown = true,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        while messages.len() < OUTPUT_BATCH_LIMIT {
            match receiver.try_recv() {
                Ok(
                    message @ (WorkerMessage::Output(_)
                    | WorkerMessage::Complete { .. }
                    | WorkerMessage::ProcessStarted { .. }
                    | WorkerMessage::ProcessEnded { .. }),
                ) => messages.push(message),
                #[cfg(test)]
                Ok(message @ WorkerMessage::Barrier(_)) => messages.push(message),
                Ok(WorkerMessage::Shutdown) => {
                    shutdown = true;
                    break;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    shutdown = true;
                    break;
                }
            }
        }

        // Completions that arrived while the worker was blocked on the output
        // lane still go first. This makes completion latency independent of
        // how full that lane happened to be when the worker woke up.
        let mut late_priority = Vec::new();
        drain_priority_completions(&shared.completion_overflow, &mut late_priority);
        if !late_priority.is_empty() {
            late_priority.append(&mut messages);
            messages = late_priority;
        }

        let pending_refreshes =
            take_pending_presentation_refreshes(&shared.pending_presentation_refreshes);

        let idle = messages.is_empty() && pending_refreshes.is_empty();
        process_pending_presentation_refreshes(pending_refreshes, store, events);
        process_messages(messages, &shared, &mut live_display);

        let backfill_due = !materialization_backfill_complete
            && idle
            && last_materialization_backfill
                .map(|last: Instant| last.elapsed() >= MATERIALIZATION_BACKFILL_INTERVAL)
                .unwrap_or(true);
        if backfill_due {
            last_materialization_backfill = Some(Instant::now());
            let backfill_budget = max_size_bytes.saturating_mul(9) / 10;
            match store.physical_size() {
                Ok(size) if size < backfill_budget => {
                    match store.backfill_presentation_materializations(1) {
                        Ok(0) => materialization_backfill_complete = true,
                        Ok(_) => {}
                        Err(error) => {
                            warn!(event = "local_presentation_backfill_failed", error = %error)
                        }
                    }
                }
                Ok(_) => {}
                Err(error) => warn!(
                    event = "local_presentation_backfill_size_check_failed",
                    error = %error,
                ),
            }
        }

        // Retention can touch a large history database. Only run it while the
        // writer is idle so maintenance can never starve live output capture
        // and manufacture queue pressure for active model commands.
        let maintenance_due = idle
            && health.maintenance_requested.load(Ordering::Acquire)
            && last_maintenance
                .map(|last| last.elapsed() >= RETENTION_MIN_INTERVAL)
                .unwrap_or(true);
        if maintenance_due && health.maintenance_requested.swap(false, Ordering::AcqRel) {
            last_maintenance = Some(Instant::now());
            let active_process_invocation_ids = shared
                .active_process_invocation_ids
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .keys()
                .copied()
                .collect::<Vec<_>>();
            match store.run_retention(
                max_age_seconds,
                max_size_bytes,
                &active_process_invocation_ids,
            ) {
                Ok(()) => health.maintenance_requested.store(
                    store
                        .physical_size()
                        .is_ok_and(|size| size > max_size_bytes),
                    Ordering::Release,
                ),
                Err(error) => mark_retention_error_best_effort(store, &error.to_string()),
            }
        }
    }

    health.persist_degraded_state(store);
}

fn process_messages(
    messages: Vec<WorkerMessage>,
    shared: &HistoryWorkerShared,
    live_display: &mut LiveDisplayStreams,
) {
    let store = shared.store.as_ref();
    let health = shared.health.as_ref();
    let events = shared.events.as_ref();
    let active_process_invocation_ids = shared.active_process_invocation_ids.as_ref();
    let active_process_counts = shared.active_process_counts.as_ref();
    let active_process_count = shared.active_process_count.as_ref();
    let mut output_events = Vec::new();
    let mut dirty_presentations = HashMap::<i64, (Option<i64>, Option<String>, bool)>::new();
    #[cfg(test)]
    let mut barriers = Vec::new();
    for message in messages {
        match message {
            WorkerMessage::Output(event) => output_events.push(event),
            WorkerMessage::Complete {
                context,
                outcome,
                file_evidence,
            } => {
                flush_output_events(&mut output_events, store, health, events, live_display);
                let invocation_id = context.invocation_id;
                match store.complete(&context, outcome.clone()) {
                    Err(error) => {
                        if let Some(invocation_id) = invocation_id {
                            health.note_evidence_incomplete(invocation_id);
                        }
                        health.degrade_persisting(
                            store,
                            format!("invocation completion persistence failed: {error}"),
                        );
                    }
                    Ok(completion) => {
                        if let Some(invocation_id) = invocation_id {
                            persist_completed_file_evidence(store, invocation_id, &file_evidence);
                        }
                        let emit_live = events.emit_invocation_completion(
                            &context,
                            &outcome,
                            completion.presentation_root_invocation_id,
                        );
                        if let Some(root_invocation_id) = completion.presentation_root_invocation_id
                        {
                            events.clear_presentation(root_invocation_id);
                            dirty_presentations.insert(
                                root_invocation_id,
                                (
                                    invocation_id,
                                    context.agent_id.as_deref().map(str::to_owned),
                                    emit_live,
                                ),
                            );
                        }
                        health.maintenance_requested.store(true, Ordering::Release);
                    }
                }
            }
            WorkerMessage::ProcessStarted {
                invocation_id,
                agent_id,
                active_process_count,
                agent_active_process_count,
            } => {
                flush_output_events(&mut output_events, store, health, events, live_display);
                match store.process_started(invocation_id) {
                    Ok(()) => {
                        events.clear_presentation(invocation_id);
                        let emit_live = events.emit_with(
                            "process_started",
                            agent_id.as_deref(),
                            Some(invocation_id),
                            Some(invocation_id),
                            Some(PRESENTATION_SCHEMA_VERSION),
                            || {
                                json!({
                                    "active_process_count": active_process_count,
                                    "agent_active_process_count": agent_active_process_count,
                                    "process_state": "running",
                                })
                            },
                        );
                        refresh_and_emit_presentation(
                            store,
                            events,
                            invocation_id,
                            Some(invocation_id),
                            agent_id.as_deref(),
                            "process_started",
                            emit_live,
                        );
                    }
                    Err(error) => {
                        health.note_evidence_incomplete(invocation_id);
                        health.degrade_persisting(
                            store,
                            format!("process lifecycle start persistence failed: {error}"),
                        );
                    }
                }
            }
            WorkerMessage::ProcessEnded {
                invocation_id,
                agent_id,
                end,
                was_active,
            } => {
                flush_output_events(&mut output_events, store, health, events, live_display);
                match store.process_ended(invocation_id, &end) {
                    Ok(()) => {
                        let removed = active_process_invocation_ids
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .remove(&invocation_id)
                            .is_some();
                        let active_process_count = if was_active && removed {
                            active_process_count
                                .try_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                                    Some(count.saturating_sub(1))
                                })
                                .unwrap_or(0)
                                .saturating_sub(1)
                        } else {
                            active_process_count.load(Ordering::Acquire)
                        };
                        let agent_active_process_count = agent_id.as_deref().map(|agent_id| {
                            let mut counts = active_process_counts
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner);
                            let count = counts.entry(agent_id.to_string()).or_default();
                            if was_active && removed {
                                *count = count.saturating_sub(1);
                            }
                            let active = *count;
                            if active == 0 {
                                counts.remove(agent_id);
                            }
                            active
                        });
                        events.clear_presentation(invocation_id);
                        let process_state = if end.is_complete() {
                            "exited"
                        } else {
                            "incomplete"
                        };
                        let emit_live = events.emit_with(
                            "process_ended",
                            agent_id.as_deref(),
                            Some(invocation_id),
                            Some(invocation_id),
                            Some(PRESENTATION_SCHEMA_VERSION),
                            || {
                                json!({
                                    "active_process_count": active_process_count,
                                    "agent_active_process_count": agent_active_process_count,
                                    "process_state": process_state,
                                    "exit_code": end.exit_code,
                                    "termination_reason": end.termination_reason,
                                    "incomplete_reason": end.incomplete_reason,
                                })
                            },
                        );
                        refresh_and_emit_presentation(
                            store,
                            events,
                            invocation_id,
                            Some(invocation_id),
                            agent_id.as_deref(),
                            "process_ended",
                            emit_live,
                        );
                    }
                    Err(error) => {
                        health.note_evidence_incomplete(invocation_id);
                        health.degrade_persisting(
                            store,
                            format!("process lifecycle end persistence failed: {error}"),
                        );
                    }
                }
            }
            #[cfg(test)]
            WorkerMessage::Barrier(acknowledge) => {
                flush_output_events(&mut output_events, store, health, events, live_display);
                barriers.push(acknowledge);
            }
            WorkerMessage::Shutdown => {
                unreachable!("shutdown messages are consumed by the worker loop")
            }
        }
    }
    flush_output_events(&mut output_events, store, health, events, live_display);
    for (root_invocation_id, (invocation_id, fallback_agent_id, emit_live)) in dirty_presentations {
        refresh_and_emit_presentation(
            store,
            events,
            root_invocation_id,
            invocation_id,
            fallback_agent_id.as_deref(),
            "invocation_completed",
            emit_live,
        );
    }
    #[cfg(test)]
    for acknowledge in barriers {
        let _ = acknowledge.send(());
    }
    health.persist_capture_failures(store);
    if health.degraded.load(Ordering::Acquire) {
        health.persist_degraded_state(store);
    }
}

fn drain_priority_completions(
    completion_overflow: &Mutex<VecDeque<WorkerMessage>>,
    messages: &mut Vec<WorkerMessage>,
) {
    while messages.len() < OUTPUT_BATCH_LIMIT {
        let message = completion_overflow
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pop_front();
        let Some(message) = message else {
            break;
        };
        messages.push(message);
    }
}

fn take_pending_presentation_refreshes(
    pending: &Mutex<HashMap<i64, PendingPresentationRefresh>>,
) -> Vec<(i64, PendingPresentationRefresh)> {
    pending
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .drain()
        .collect()
}

fn process_pending_presentation_refreshes(
    refreshes: Vec<(i64, PendingPresentationRefresh)>,
    store: &HistoryStore,
    events: &HistoryEventHub,
) {
    for (root_invocation_id, refresh) in refreshes {
        refresh_and_emit_presentation(
            store,
            events,
            root_invocation_id,
            refresh.invocation_id,
            refresh.fallback_agent_id.as_deref(),
            refresh.source,
            refresh.emit_live,
        );
    }
}

fn persist_completed_file_evidence(
    store: &HistoryStore,
    invocation_id: i64,
    evidence: &[CompletedFileEvidence],
) {
    if evidence.is_empty() {
        return;
    }
    if let Err(error) = store.persist_file_evidence_completion(invocation_id, evidence) {
        warn!(
            event = "local_file_evidence_after_persistence_failed",
            invocation_id,
            error = %error,
        );
        let reason = format!("after-file evidence persistence failed: {error}");
        if let Err(mark_error) = store.mark_file_evidence_unavailable(invocation_id, &reason) {
            warn!(
                event = "local_file_evidence_unavailable_persistence_failed",
                invocation_id,
                error = %mark_error,
            );
        }
    }
}

fn flush_output_events(
    events: &mut Vec<OutputEvent>,
    store: &HistoryStore,
    health: &HistoryHealth,
    live_events: &HistoryEventHub,
    live_display: &mut LiveDisplayStreams,
) {
    if events.is_empty() {
        return;
    }
    if let Err(error) = store.persist_output_batch(events) {
        let reason = format!("output evidence batch persistence failed: {error}");
        for invocation_id in distinct_invocation_ids(events) {
            health.note_capture_incomplete(invocation_id, reason.clone());
        }
        health.degrade_persisting(store, reason);
    } else {
        let mut live_output_batch = None;
        for event in events.iter() {
            match event {
                OutputEvent::Chunk {
                    invocation_id,
                    agent_id,
                    sequence,
                    text,
                    ..
                } => {
                    let capture_text = live_events.has_output_subscribers();
                    let display =
                        live_display.observe(*invocation_id, *sequence, text, capture_text);
                    if capture_text {
                        append_live_output_batch(
                            &mut live_output_batch,
                            live_events,
                            *invocation_id,
                            agent_id.as_deref(),
                            *sequence,
                            display.text,
                            display.state,
                            display.reason,
                        );
                    }
                }
                OutputEvent::Complete {
                    invocation_id,
                    agent_id,
                } => {
                    flush_live_output_batch(&mut live_output_batch, live_events);
                    let display = live_display.complete(*invocation_id);
                    live_events.emit_with(
                        "output_complete",
                        agent_id.as_deref(),
                        Some(*invocation_id),
                        Some(*invocation_id),
                        Some(PRESENTATION_SCHEMA_VERSION),
                        || {
                            json!({
                                "display_state": display.state,
                                "display_reason": display.reason,
                            })
                        },
                    );
                }
                OutputEvent::Incomplete {
                    invocation_id,
                    agent_id,
                    reason,
                } => {
                    flush_live_output_batch(&mut live_output_batch, live_events);
                    let display = live_display.complete(*invocation_id);
                    live_events.emit_with(
                        "output_complete",
                        agent_id.as_deref(),
                        Some(*invocation_id),
                        Some(*invocation_id),
                        Some(PRESENTATION_SCHEMA_VERSION),
                        || {
                            json!({
                                "display_state": display.state,
                                "display_reason": display.reason,
                                "capture_incomplete": true,
                                "capture_reason": reason,
                            })
                        },
                    );
                }
            }
        }
        flush_live_output_batch(&mut live_output_batch, live_events);
    }
    events.clear();
}

struct LiveOutputEventBatch {
    invocation_id: i64,
    agent_id: Option<String>,
    chunks: Vec<(u64, String)>,
    display_state: &'static str,
    display_reason: Option<String>,
}

#[allow(clippy::too_many_arguments)]
fn append_live_output_batch(
    pending: &mut Option<LiveOutputEventBatch>,
    live_events: &HistoryEventHub,
    invocation_id: i64,
    agent_id: Option<&str>,
    sequence: u64,
    text: String,
    display_state: &'static str,
    display_reason: Option<String>,
) {
    let can_append = pending.as_ref().is_some_and(|batch| {
        batch.invocation_id == invocation_id
            && batch.agent_id.as_deref() == agent_id
            && batch.chunks.len() < LIVE_OUTPUT_EVENT_BATCH_MAX_CHUNKS
    });
    if !can_append {
        flush_live_output_batch(pending, live_events);
        *pending = Some(LiveOutputEventBatch {
            invocation_id,
            agent_id: agent_id.map(str::to_owned),
            chunks: Vec::with_capacity(LIVE_OUTPUT_EVENT_BATCH_MAX_CHUNKS.min(8)),
            display_state,
            display_reason: display_reason.clone(),
        });
    }
    let batch = pending
        .as_mut()
        .expect("live output batch must exist after starting one");
    batch.chunks.push((sequence, text));
    batch.display_state = display_state;
    batch.display_reason = display_reason;
}

fn flush_live_output_batch(
    pending: &mut Option<LiveOutputEventBatch>,
    live_events: &HistoryEventHub,
) {
    let Some(batch) = pending.take() else {
        return;
    };
    let chunks = batch
        .chunks
        .into_iter()
        .map(|(sequence, text)| json!({"sequence": sequence, "text": text}))
        .collect::<Vec<_>>();
    live_events.emit_with(
        "output",
        batch.agent_id.as_deref(),
        Some(batch.invocation_id),
        Some(batch.invocation_id),
        Some(PRESENTATION_SCHEMA_VERSION),
        || {
            json!({
                "chunks": chunks,
                "display_state": batch.display_state,
                "display_reason": batch.display_reason,
            })
        },
    );
}
