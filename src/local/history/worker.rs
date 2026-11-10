use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender};
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
    CompletedFileEvidence, PendingFileEvidence, committed_patch_evidence,
};
use crate::local::presentation::PRESENTATION_SCHEMA_VERSION;
use crate::session::{SessionOutputChunk, SessionOutputCompletion, SessionOutputObserver};

use super::admission::AdmissionState;
use super::events::{HistoryEventHub, HistoryLiveEvent};
use super::live_display::LiveDisplayStreams;
use super::materialized::refresh_and_emit_presentation;
use super::store::{
    HistoryStore, OutputEvent, mark_retention_error_best_effort, normalize_declared_workdir, now_ms,
};

mod admission_queue;
mod output_queue;
use admission_queue::AdmissionQueue;
mod lifecycle_queue;
mod process_observer;
use lifecycle_queue::LifecycleQueue;

const DEFAULT_OUTPUT_QUEUE_CAPACITY: usize = 4096;
const OUTPUT_BATCH_LIMIT: usize = 128;
const LIVE_OUTPUT_EVENT_BATCH_MAX_CHUNKS: usize = 64;
const COMPLETION_OVERFLOW_MAX_ITEMS: usize = 4096;
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
    unflushed: AtomicBool,
    storage_full: AtomicBool,
    degraded: AtomicBool,
    degradation_logged: AtomicBool,
    reason: Mutex<Option<String>>,
    incomplete_evidence_invocations: Mutex<HashSet<i64>>,
    incomplete_capture_invocations: Mutex<HashMap<i64, String>>,
    maintenance_requested: AtomicBool,
}

impl HistoryHealth {
    fn new() -> Self {
        Self {
            unflushed: AtomicBool::new(false),
            storage_full: AtomicBool::new(false),
            degraded: AtomicBool::new(false),
            degradation_logged: AtomicBool::new(false),
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
        // A tracing sink can itself block on storage. Producers only update memory.
        drop(guard);
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
        if !self.degradation_logged.swap(true, Ordering::AcqRel) {
            error!(event = "local_history_degraded", error = %reason);
        }
        if let Err(error) = store.set_health("degraded", Some(&reason)) {
            error!(
                event = "local_history_health_persistence_failed",
                original_error = %reason,
                error = %error,
            );
            if storage_retryable(&error) {
                return;
            }
        }
        let incomplete_evidence = self
            .incomplete_evidence_invocations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .take(128)
            .copied()
            .collect::<Vec<_>>();
        for invocation_id in incomplete_evidence {
            if let Err(error) = store.mark_evidence_incomplete(invocation_id, &reason) {
                error!(
                    event = "local_history_evidence_incomplete_persistence_failed",
                    invocation_id,
                    error = %error,
                );
                if storage_retryable(&error) {
                    break;
                }
            } else {
                self.incomplete_evidence_invocations
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .remove(&invocation_id);
            }
        }
        self.persist_capture_failures(store);
    }

    fn persist_capture_failures(&self, store: &HistoryStore) {
        let incomplete_capture = self
            .incomplete_capture_invocations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .take(128)
            .map(|(id, reason)| (*id, reason.clone()))
            .collect::<Vec<_>>();
        for (invocation_id, reason) in incomplete_capture {
            if let Err(error) = store.mark_capture_incomplete(invocation_id, &reason) {
                error!(
                    event = "local_history_capture_incomplete_persistence_failed",
                    invocation_id,
                    error = %error,
                );
                if storage_retryable(&error) {
                    break;
                }
            } else {
                self.incomplete_capture_invocations
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .remove(&invocation_id);
            }
        }
    }

    fn note_storage_error(&self, error: &anyhow::Error) {
        if error.chain().any(|cause| matches!(cause.downcast_ref::<rusqlite::Error>(),
            Some(rusqlite::Error::SqliteFailure(code, _)) if code.code == rusqlite::ErrorCode::DiskFull))
        {
            self.storage_full.store(true, Ordering::Release);
        }
    }

    fn note_evidence_incomplete(&self, invocation_id: i64) {
        let mut pending = self
            .incomplete_evidence_invocations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if pending.len() < 8192 {
            pending.insert(invocation_id);
        }
        // If even diagnostics overflow, the sticky degraded store warning remains.
    }

    fn note_capture_incomplete(&self, invocation_id: i64, reason: String) {
        let reason = reason.chars().take(2048).collect::<String>();
        let mut pending = self
            .incomplete_capture_invocations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if pending.len() < 8192 {
            pending.entry(invocation_id).or_insert(reason);
        }
    }
}

#[derive(Clone)]
enum WorkerMessage {
    PatchEvidence {
        invocation_id: i64,
        before: Vec<PendingFileEvidence>,
        after: Vec<CompletedFileEvidence>,
        warning: Option<String>,
    },
    Identify(InvocationContext),
    ContextClaim {
        provider: ProviderCallMetadata,
        workdir: Option<String>,
        skills: bool,
    },
    Output(OutputEvent),
    Complete {
        context: InvocationContext,
        outcome: InvocationOutcome,
        completed_at_ms: i64,
    },
    #[cfg(test)]
    Barrier(std::sync::mpsc::Sender<()>),
    Shutdown,
}

#[derive(Default)]
struct PriorityQueue {
    messages: VecDeque<WorkerMessage>,
    bytes: usize,
}

impl PriorityQueue {
    fn push(&mut self, message: WorkerMessage) -> bool {
        let bytes = message_bytes(&message);
        if self.messages.len() >= COMPLETION_OVERFLOW_MAX_ITEMS
            || self.bytes.saturating_add(bytes) > 16 * 1024 * 1024
        {
            return false;
        }
        self.bytes += bytes;
        self.messages.push_back(message);
        true
    }

    fn pop_front(&mut self) -> Option<WorkerMessage> {
        let message = self.messages.pop_front()?;
        self.bytes = self.bytes.saturating_sub(message_bytes(&message));
        Some(message)
    }
}

#[derive(Debug, Clone)]
struct PendingPresentationRefresh {
    invocation_id: Option<i64>,
    fallback_agent_id: Option<String>,
    source: &'static str,
    emit_live: bool,
}

struct HistoryWorkerShared {
    admission_state: Arc<AdmissionState>,
    admissions: Arc<Mutex<AdmissionQueue>>,
    lifecycle: Arc<Mutex<LifecycleQueue>>,
    agent_first_seen_observer: Arc<Mutex<Option<AgentFirstSeenObserver>>>,
    store: Arc<HistoryStore>,
    health: Arc<HistoryHealth>,
    events: Arc<HistoryEventHub>,
    completion_overflow: Arc<Mutex<PriorityQueue>>,
    pending_presentation_refreshes: Arc<Mutex<HashMap<i64, PendingPresentationRefresh>>>,
    active_process_invocation_ids: Arc<Mutex<HashMap<i64, bool>>>,
    active_process_counts: Arc<Mutex<HashMap<String, u64>>>,
    active_process_count: Arc<AtomicU64>,
    worker_running: Arc<AtomicBool>,
}

pub struct LocalHistoryRuntime {
    lease: Mutex<Option<super::lease::HistoryLease>>,
    admission_state: Arc<AdmissionState>,
    admissions: Arc<Mutex<AdmissionQueue>>,
    lifecycle: Arc<Mutex<LifecycleQueue>>,
    store: Arc<HistoryStore>,
    runtime_id: Arc<str>,
    events: Arc<HistoryEventHub>,
    sender: SyncSender<WorkerMessage>,
    completion_overflow: Arc<Mutex<PriorityQueue>>,
    worker_running: Arc<AtomicBool>,
    output_overflow: Mutex<output_queue::OutputOverflow>,
    worker: Mutex<Option<JoinHandle<()>>>,
    health: Arc<HistoryHealth>,
    // false = running; true = process end observed but not durably committed.
    // Both remain retention-protected until the worker commits the end state.
    active_process_invocation_ids: Arc<Mutex<HashMap<i64, bool>>>,
    active_process_counts: Arc<Mutex<HashMap<String, u64>>>,
    active_process_count: Arc<AtomicU64>,
    #[cfg(any(target_os = "macos", test))]
    agent_first_seen_observer: Arc<Mutex<Option<AgentFirstSeenObserver>>>,
}

type AgentFirstSeenObserver = Arc<dyn Fn(String) + Send + Sync>;

impl LocalHistoryRuntime {
    /// Ephemeral control-plane hint delivered over the EXISTING observer SSE.
    /// This is intentionally absent from invocation evidence and the timeline.
    pub(crate) fn notify_liveboard_preferences_changed(&self) {
        self.events
            .emit_with("preferences_changed", None, None, None, None, || json!({}));
    }

    pub(crate) fn identify_agent(&self, context: InvocationContext) -> Result<InvocationContext> {
        let context = self.admission_state.identify(context)?;
        if let Err(error) = self
            .sender
            .try_send(WorkerMessage::Identify(context.clone()))
        {
            self.health
                .degrade_nonblocking(format!("Local Agent identity was not queued: {error}"));
        }
        Ok(context)
    }

    pub fn open(config: LocalHistoryRuntimeConfig) -> Result<Arc<Self>> {
        Self::open_with_store(config, None)
    }

    pub(super) fn open_with_store(
        config: LocalHistoryRuntimeConfig,
        store_override: Option<Arc<HistoryStore>>,
    ) -> Result<Arc<Self>> {
        let lease = super::lease::HistoryLease::acquire(&config.database_path)?;
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
        let completion_overflow = Arc::new(Mutex::new(PriorityQueue::default()));
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
        let admission_state = Arc::new(AdmissionState::open(&store)?);
        let admissions = Arc::new(Mutex::new(AdmissionQueue::default()));
        let lifecycle = Arc::new(Mutex::new(LifecycleQueue::default()));
        let agent_first_seen_observer = Arc::new(Mutex::new(None));
        let worker_shared = HistoryWorkerShared {
            admission_state: admission_state.clone(),
            admissions: admissions.clone(),
            lifecycle: lifecycle.clone(),
            agent_first_seen_observer: agent_first_seen_observer.clone(),
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
            lease: Mutex::new(Some(lease)),
            admission_state,
            admissions,
            lifecycle,
            store,
            runtime_id,
            events,
            sender,
            completion_overflow,
            worker_running,
            output_overflow: Mutex::new(output_queue::OutputOverflow::default()),
            worker: Mutex::new(Some(worker)),
            health,
            active_process_invocation_ids,
            active_process_counts,
            active_process_count,
            #[cfg(any(target_os = "macos", test))]
            agent_first_seen_observer,
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
        let claimed = self.admission_state.claim_global(provider);
        if claimed {
            self.enqueue_context_claim(provider, None, false);
        }
        Ok(claimed)
    }

    pub(crate) fn claim_repo_agents_check(
        &self,
        provider: &ProviderCallMetadata,
        normalized_workdir: &str,
    ) -> Result<bool> {
        let claimed = self
            .admission_state
            .claim_agents(provider, normalized_workdir);
        if claimed {
            self.enqueue_context_claim(provider, Some(normalized_workdir), false);
        }
        Ok(claimed)
    }

    pub(crate) fn claim_repo_skills_check(
        &self,
        provider: &ProviderCallMetadata,
        normalized_workdir: &str,
    ) -> Result<bool> {
        let claimed = self
            .admission_state
            .claim_skills(provider, normalized_workdir);
        if claimed {
            self.enqueue_context_claim(provider, Some(normalized_workdir), true);
        }
        Ok(claimed)
    }

    fn enqueue_context_claim(
        &self,
        provider: &ProviderCallMetadata,
        workdir: Option<&str>,
        skills: bool,
    ) {
        let message = WorkerMessage::ContextClaim {
            provider: provider.clone(),
            workdir: workdir.map(str::to_owned),
            skills,
        };
        if let Err(error) = self.sender.try_send(message) {
            self.health
                .degrade_nonblocking(format!("Local context claim was not queued: {error}"));
        }
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

    pub(crate) fn health_warning(&self) -> Option<String> {
        if self.health.degraded.load(Ordering::Acquire) {
            return Some(
                self.health
                    .reason
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone()
                    .unwrap_or_else(|| "Local history persistence is degraded".to_string()),
            );
        }
        None
    }

    pub(crate) fn has_pending_admissions(&self) -> bool {
        !self
            .admissions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_empty()
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
            if self.has_pending_admissions() && Instant::now() < capture_deadline {
                std::thread::sleep(SHUTDOWN_CAPTURE_POLL_INTERVAL);
                continue;
            }
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
        // Keep the exclusive writer lease until every final database write is over.
        self.lease
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if self.health.unflushed.load(Ordering::Acquire) {
            return Err(anyhow::anyhow!(
                "Local history shutdown could not persist all queued evidence; history is incomplete"
            ));
        }
        if let Some(error) = finalize_error {
            Err(error)
        } else {
            Ok(())
        }
    }
}

impl InvocationEvidenceRecorder for LocalHistoryRuntime {
    fn record_patch_evidence(
        &self,
        context: &InvocationContext,
        evidence: crate::apply_patch::PatchEvidence,
    ) {
        let Some(invocation_id) = context.invocation_id else {
            return;
        };
        let (before, after, warning) = committed_patch_evidence(evidence);
        if !before.is_empty() || warning.is_some() {
            self.enqueue_priority_message(
                WorkerMessage::PatchEvidence {
                    invocation_id,
                    before,
                    after,
                    warning,
                },
                invocation_id,
                "Local committed patch evidence queue is full or unavailable",
            );
        }
    }
    fn warning(&self) -> Option<String> {
        self.health_warning()
    }
    fn begin(
        &self,
        context: InvocationContext,
        start: InvocationStart,
    ) -> Result<InvocationContext> {
        // Admission is memory-only. The worker owns every history database write;
        // no SQLite handle, filesystem write or storage-backed lock is touched here.
        let context = self
            .admission_state
            .reserve(context, &start)
            .inspect_err(|error| {
                self.health.degrade_nonblocking(format!(
                    "Local history memory admission failed: {error:#}"
                ));
            })?;
        if !self.worker_running.load(Ordering::Acquire) {
            self.health
                .degrade_nonblocking("Local history admission worker is unavailable");
            anyhow::bail!("Local history admission worker is unavailable");
        }
        let queued = self
            .admissions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(context.clone(), start, now_ms()?);
        if !queued {
            self.health.degrade_nonblocking("Local history admission queue is full; invocation evidence was omitted without delaying execution");
            anyhow::bail!("Local history admission queue is full");
        }
        Ok(context)
    }

    fn complete(&self, context: &InvocationContext, outcome: InvocationOutcome) -> Result<()> {
        let invocation_id = context.invocation_id.ok_or_else(|| {
            anyhow::anyhow!("Local invocation completion is missing durable invocation ID")
        })?;
        let queued = WorkerMessage::Complete {
            context: context.clone(),
            outcome: outcome.clone(),
            completed_at_ms: now_ms()?,
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

        if self
            .completion_overflow
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(message)
        {
            return true;
        }

        // All control evidence shares one FIFO. Spilling to the output lane
        // would let a later process end overtake its start after a queue drain.
        self.health.note_evidence_incomplete(invocation_id);
        self.health.degrade_nonblocking(failure_reason);
        false
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
    let mut shutdown_deadline = None;
    // `open_with_store` completed the startup retention pass before spawning
    // this worker, so future maintenance can honor the normal coalescing
    // interval instead of racing the first evidence messages.
    let mut last_maintenance = Some(Instant::now());
    let mut last_materialization_backfill = None;
    let mut materialization_backfill_complete = false;
    let mut live_display = LiveDisplayStreams::new();
    let mut waiting_messages = Vec::new();
    loop {
        if let Err(error) = shared.admission_state.replenish(store) {
            health.note_storage_error(&error);
            health.degrade_nonblocking(format!(
                "Local invocation ID reservation queued for retry: {error:#}"
            ));
        }
        if health.storage_full.swap(false, Ordering::AcqRel) {
            let active = shared
                .active_process_invocation_ids
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .keys()
                .copied()
                .collect::<Vec<_>>();
            if let Err(error) = store.recover_storage_full(&active) {
                health.degrade_nonblocking(format!(
                    "Local history storage-full recovery failed: {error:#}"
                ));
            }
        }
        persist_pending_admissions(&shared);
        lifecycle_queue::drain(&shared);
        let mut messages = Vec::with_capacity(OUTPUT_BATCH_LIMIT);
        drain_priority_completions(&shared.completion_overflow, &mut messages);
        if messages.is_empty() {
            match receiver.recv_timeout(OUTPUT_BATCH_WAIT) {
                Ok(
                    message @ (WorkerMessage::Output(_)
                    | WorkerMessage::PatchEvidence { .. }
                    | WorkerMessage::Identify(_)
                    | WorkerMessage::ContextClaim { .. }
                    | WorkerMessage::Complete { .. }),
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
                    | WorkerMessage::PatchEvidence { .. }
                    | WorkerMessage::Identify(_)
                    | WorkerMessage::ContextClaim { .. }
                    | WorkerMessage::Complete { .. }),
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
            messages.append(&mut late_priority);
        }

        let pending_refreshes =
            take_pending_presentation_refreshes(&shared.pending_presentation_refreshes);

        let idle = messages.is_empty()
            && pending_refreshes.is_empty()
            && waiting_messages.is_empty()
            && !shared
                .lifecycle
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .has_pending()
            && shared
                .admissions
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_empty();
        waiting_messages.append(&mut messages);
        messages = take_admitted_messages(&mut waiting_messages, &shared);
        process_pending_presentation_refreshes(pending_refreshes, store, events);
        waiting_messages.extend(process_messages(messages, &shared, &mut live_display));
        bound_waiting_messages(&mut waiting_messages, health);

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
        if shutdown {
            let deadline = *shutdown_deadline
                .get_or_insert_with(|| Instant::now() + SHUTDOWN_CAPTURE_DRAIN_TIMEOUT);
            if idle || Instant::now() >= deadline {
                break;
            }
        }
    }

    if !waiting_messages.is_empty()
        || shared
            .lifecycle
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .has_pending()
        || !shared
            .completion_overflow
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .messages
            .is_empty()
        || receiver.try_recv().is_ok()
        || !shared
            .admissions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_empty()
    {
        health.unflushed.store(true, Ordering::Release);
        health.degrade_nonblocking(
            "Local history shutdown ended with unpersisted admission evidence",
        );
    }
    health.persist_degraded_state(store);
}

fn persist_pending_admissions(shared: &HistoryWorkerShared) {
    for _ in 0..OUTPUT_BATCH_LIMIT {
        let admission = shared
            .admissions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pop();
        let Some(admission) = admission else {
            break;
        };
        // No producer-shared lock is held across any storage operation.
        let result = shared.store.begin_at(
            admission.context.clone(),
            admission.start.clone(),
            admission.started_at_ms,
        );
        match result {
            Ok(begin) => {
                shared
                    .admissions
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .finish(&admission);
                let context = &begin.context;
                let agent = context.agent_id.as_deref();
                let root = begin.presentation_root_invocation_id;
                if begin.agent_first_seen_in_runtime {
                    let observer = shared
                        .agent_first_seen_observer
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .clone();
                    if let (Some(observer), Some(agent)) = (observer, agent) {
                        observer(agent.to_string());
                    }
                    shared.events.emit_with(
                        "agent_first_seen",
                        agent,
                        context.invocation_id,
                        root,
                        None,
                        || json!({}),
                    );
                }
                if let Some(workdir) = begin.new_workdir.as_deref() {
                    shared.events.emit_with(
                        "agent_workdir_added",
                        agent,
                        context.invocation_id,
                        root,
                        Some(PRESENTATION_SCHEMA_VERSION),
                        || json!({"normalized_workdir":workdir}),
                    );
                }
                let live = shared.events.emit_with("invocation_started", agent, context.invocation_id, root,
                    Some(PRESENTATION_SCHEMA_VERSION), || json!({
                        "tool_name": admission.start.tool_name.as_ref(),
                        "normalized_workdir": admission.start.arguments.get("workdir").and_then(serde_json::Value::as_str).and_then(normalize_declared_workdir),
                    }));
                if let Some(root) = root {
                    refresh_and_emit_presentation(
                        &shared.store,
                        &shared.events,
                        root,
                        context.invocation_id,
                        agent,
                        "invocation_started",
                        live,
                    );
                }
            }
            Err(error) => {
                shared.health.note_storage_error(&error);
                shared.health.degrade_nonblocking(format!(
                    "invocation envelope persistence failed; queued for retry: {error:#}"
                ));
                shared
                    .admissions
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .retry(admission);
                break;
            }
        }
    }
}

fn message_invocation_id(message: &WorkerMessage) -> Option<i64> {
    match message {
        WorkerMessage::Output(
            OutputEvent::Chunk { invocation_id, .. }
            | OutputEvent::Complete { invocation_id, .. }
            | OutputEvent::Incomplete { invocation_id, .. },
        )
        | WorkerMessage::PatchEvidence { invocation_id, .. } => Some(*invocation_id),
        WorkerMessage::Complete { context, .. } => context.invocation_id,
        _ => None,
    }
}

fn message_bytes(message: &WorkerMessage) -> usize {
    let payload = match message {
        WorkerMessage::PatchEvidence { before, after, .. } => {
            before
                .iter()
                .map(|item| {
                    item.before.text().map_or(0, str::len)
                        + item
                            .destination_before
                            .as_ref()
                            .and_then(|snapshot| snapshot.text())
                            .map_or(0, str::len)
                })
                .sum::<usize>()
                + after
                    .iter()
                    .map(|item| {
                        item.after.text().map_or(0, str::len)
                            + item
                                .source_after
                                .as_ref()
                                .and_then(|snapshot| snapshot.text())
                                .map_or(0, str::len)
                    })
                    .sum::<usize>()
        }
        WorkerMessage::Output(OutputEvent::Chunk { text, .. }) => text.len(),
        WorkerMessage::Complete {
            outcome: InvocationOutcome::Success(value),
            ..
        } => value.to_string().len(),
        WorkerMessage::Complete {
            outcome: InvocationOutcome::Error(error),
            ..
        } => error.len(),
        _ => 0,
    };
    payload.saturating_add(512)
}

fn bound_waiting_messages(waiting: &mut Vec<WorkerMessage>, health: &HistoryHealth) {
    const MAX_WAITING_BYTES: usize = 16 * 1024 * 1024;
    const MAX_WAITING_MESSAGES: usize = 4096;
    let mut bytes = 0usize;
    let mut count = 0usize;
    waiting.retain(|message| {
        let size = message_bytes(message);
        if count < MAX_WAITING_MESSAGES && bytes.saturating_add(size) <= MAX_WAITING_BYTES {
            count += 1;
            bytes += size;
            return true;
        }
        if let Some(id) = message_invocation_id(message) {
            health.note_evidence_incomplete(id);
            health.note_capture_incomplete(id, "Evidence backlog exceeded its bounded memory budget while storage was unavailable".to_string());
        }
        health.degrade_nonblocking("Local history dependent evidence queue is full; evidence omitted without delaying execution");
        false
    });
}

fn take_admitted_messages(
    waiting: &mut Vec<WorkerMessage>,
    shared: &HistoryWorkerShared,
) -> Vec<WorkerMessage> {
    bound_waiting_messages(waiting, &shared.health);
    let mut ready = Vec::new();
    let mut deferred = Vec::new();
    for message in waiting.drain(..) {
        let pending = {
            let admissions = shared
                .admissions
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match message_invocation_id(&message) {
                Some(id) => admissions.contains(id),
                None => !admissions.is_empty(),
            }
        };
        if pending {
            deferred.push(message);
        } else {
            ready.push(message);
        }
    }
    *waiting = deferred;
    ready
}

fn storage_retryable(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        matches!(cause.downcast_ref::<rusqlite::Error>(),
        Some(rusqlite::Error::SqliteFailure(code, _)) if matches!(code.code,
            rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked |
            rusqlite::ErrorCode::DiskFull | rusqlite::ErrorCode::SystemIoFailure |
            rusqlite::ErrorCode::CannotOpen))
    })
}

fn process_messages(
    messages: Vec<WorkerMessage>,
    shared: &HistoryWorkerShared,
    live_display: &mut LiveDisplayStreams,
) -> Vec<WorkerMessage> {
    let mut retry = Vec::new();
    let mut retry_ids = HashSet::new();
    let mut retry_control = false;
    let mut storage_blocked = false;
    let store = shared.store.as_ref();
    let health = shared.health.as_ref();
    let events = shared.events.as_ref();
    let mut output_events = Vec::new();
    let mut dirty_presentations = HashMap::<i64, (Option<i64>, Option<String>, bool)>::new();
    #[cfg(test)]
    let mut barriers = Vec::new();
    for message in messages {
        let id = message_invocation_id(&message);
        if storage_blocked
            || id.is_some_and(|id| retry_ids.contains(&id))
            || (id.is_none() && retry_control)
        {
            retry.push(message);
            continue;
        }
        let replay = message.clone();
        match message {
            WorkerMessage::PatchEvidence {
                invocation_id,
                before,
                after,
                warning,
            } => match store
                .persist_file_evidence_start(invocation_id, &before)
                .and_then(|()| store.persist_file_evidence_completion(invocation_id, &after))
                .and_then(|()| match warning.as_deref() {
                    Some(reason) => store.mark_evidence_incomplete(invocation_id, reason),
                    None => Ok(()),
                }) {
                Ok(()) => {
                    refresh_and_emit_presentation(
                        store,
                        events,
                        invocation_id,
                        Some(invocation_id),
                        None,
                        "patch_evidence",
                        true,
                    );
                }
                Err(error) => {
                    storage_blocked |= storage_retryable(&error);
                    health.degrade_nonblocking(format!(
                        "Local committed patch evidence queued for retry: {error:#}"
                    ));
                    retry_ids.insert(invocation_id);
                    retry.push(replay);
                }
            },
            WorkerMessage::Identify(context) => match store.identify_agent(context) {
                Ok(identity) if identity.first_seen_in_runtime => {
                    if let Some(agent) = identity.context.agent_id.as_deref() {
                        let observer = shared
                            .agent_first_seen_observer
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .clone();
                        if let Some(observer) = observer {
                            observer(agent.to_owned());
                        }
                        events.emit_with("agent_first_seen", Some(agent), None, None, None, || {
                            json!({})
                        });
                    }
                }
                Ok(_) => {}
                Err(error) => {
                    storage_blocked |= storage_retryable(&error);
                    health.degrade_nonblocking(format!(
                        "Local Agent identity queued for retry: {error:#}"
                    ));
                    retry_control = true;
                    retry.push(replay);
                }
            },
            WorkerMessage::ContextClaim {
                provider,
                workdir,
                skills,
            } => {
                let result = match workdir.as_deref() {
                    None => store.claim_global_context_injection(&provider),
                    Some(workdir) if skills => store.claim_repo_skills_check(&provider, workdir),
                    Some(workdir) => store.claim_repo_agents_check(&provider, workdir),
                };
                if let Err(error) = result {
                    health.degrade_nonblocking(format!(
                        "Local context claim queued for retry: {error:#}"
                    ));
                    retry_control = true;
                    retry.push(replay);
                }
            }
            WorkerMessage::Output(event) => output_events.push(event),
            WorkerMessage::Complete {
                context,
                outcome,
                completed_at_ms,
            } => {
                let invocation_id = context.invocation_id;
                match store.complete_at(&context, outcome.clone(), completed_at_ms) {
                    Err(error) => {
                        storage_blocked |= storage_retryable(&error);
                        health.note_storage_error(&error);
                        if let Some(invocation_id) = invocation_id {
                            retry_ids.insert(invocation_id);
                        }
                        health.degrade_nonblocking(format!(
                            "invocation completion queued for retry: {error:#}"
                        ));
                        retry.push(replay);
                    }
                    Ok(completion) => {
                        if let Some(invocation_id) = invocation_id
                            && let InvocationOutcome::Success(value) = &outcome
                            && let Err(error) =
                                store.persist_proven_shell_creation(invocation_id, value)
                        {
                            health.degrade_nonblocking(format!(
                                "Proven shell creation evidence queued for retry: {error:#}"
                            ));
                            retry_ids.insert(invocation_id);
                            retry.push(replay);
                            continue;
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
            #[cfg(test)]
            WorkerMessage::Barrier(acknowledge) => {
                if !retry.is_empty()
                    || shared
                        .lifecycle
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .has_pending()
                {
                    retry.push(WorkerMessage::Barrier(acknowledge));
                    continue;
                }
                barriers.push(acknowledge);
            }
            WorkerMessage::Shutdown => {
                unreachable!("shutdown messages are consumed by the worker loop")
            }
        }
    }
    retry.extend(
        flush_output_events(&mut output_events, store, health, events, live_display)
            .into_iter()
            .map(WorkerMessage::Output),
    );
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
        if retry.is_empty() {
            let _ = acknowledge.send(());
        } else {
            retry.push(WorkerMessage::Barrier(acknowledge));
        }
    }
    health.persist_capture_failures(store);
    if health.degraded.load(Ordering::Acquire) {
        health.persist_degraded_state(store);
    }
    retry
}

fn drain_priority_completions(
    completion_overflow: &Mutex<PriorityQueue>,
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

fn flush_output_events(
    events: &mut Vec<OutputEvent>,
    store: &HistoryStore,
    health: &HistoryHealth,
    live_events: &HistoryEventHub,
    live_display: &mut LiveDisplayStreams,
) -> Vec<OutputEvent> {
    if events.is_empty() {
        return Vec::new();
    }
    if let Err(error) = store.persist_output_batch(events) {
        health.note_storage_error(&error);
        health.degrade_nonblocking(format!("output evidence batch queued for retry: {error:#}"));
        return std::mem::take(events);
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
    Vec::new()
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
