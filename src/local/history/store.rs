use std::collections::HashSet;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, TryLockError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tracing::{error, warn};

use super::event_identity::{HistoryCompletionResult, presentation_root_invocation_id};
use super::history_store_paths;
use super::schema::initialize_or_migrate;
use crate::invocation::{
    InvocationContext, InvocationOutcome, InvocationStart, ProviderCallMetadata,
};

const AGENT_ID_ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
const AGENT_ID_ATTEMPTS: usize = 128;
const FOREGROUND_GATE_TIMEOUT: Duration = Duration::from_millis(250);
const FOREGROUND_SQLITE_BUSY_TIMEOUT: Duration = Duration::from_millis(50);

pub(super) type AgentIdSource = Arc<dyn Fn() -> String + Send + Sync>;

pub(super) struct HistoryStore {
    path: PathBuf,
    connection: Mutex<Connection>,
    foreground_gate: Mutex<()>,
    runtime_id: Arc<str>,
    agent_id_source: AgentIdSource,
}

pub(super) struct HistoryBeginResult {
    pub(super) context: InvocationContext,
    pub(super) agent_first_seen_in_runtime: bool,
    pub(super) new_workdir: Option<String>,
    pub(super) presentation_root_invocation_id: Option<i64>,
}

#[derive(Debug, Clone)]
pub(super) enum OutputEvent {
    Chunk {
        invocation_id: i64,
        agent_id: Option<String>,
        sequence: u64,
        observed_at_ms: i64,
        text: String,
    },
    Complete {
        invocation_id: i64,
        agent_id: Option<String>,
    },
    Incomplete {
        invocation_id: i64,
        agent_id: Option<String>,
        reason: String,
    },
}

impl HistoryStore {
    pub(super) fn open(path: PathBuf, runtime_id: Arc<str>) -> Result<Self> {
        Self::open_with_agent_id_source(path, runtime_id, Arc::new(random_agent_id))
    }

    pub(super) fn open_with_agent_id_source(
        path: PathBuf,
        runtime_id: Arc<str>,
        agent_id_source: AgentIdSource,
    ) -> Result<Self> {
        ensure_history_parent(&path)?;
        let mut connection = Connection::open(&path)
            .with_context(|| format!("failed to open Local history database {}", path.display()))?;
        initialize_or_migrate(&mut connection)?;
        connection
            .execute_batch(
                "CREATE TEMP TABLE IF NOT EXISTS active_process_invocations(
                    invocation_id INTEGER PRIMARY KEY
                 );",
            )
            .context("failed to initialize Local history active-process retention guard")?;
        crate::local::private_fs::set_user_only_file(&path)?;

        let store = Self {
            path,
            connection: Mutex::new(connection),
            foreground_gate: Mutex::new(()),
            runtime_id,
            agent_id_source,
        };
        store.recover_interrupted_capture()?;
        store.recover_interrupted_file_evidence()?;
        store.recover_interrupted_process_lifecycle()?;
        store.set_health("healthy", None)?;
        Ok(store)
    }

    pub(super) fn path(&self) -> &Path {
        &self.path
    }

    pub(super) fn claim_global_context_injection(
        &self,
        provider: &ProviderCallMetadata,
    ) -> Result<bool> {
        let now = now_ms()?;
        let provider_fingerprint = provider_fingerprint(provider);
        self.with_foreground_connection(|connection| {
            let changed = connection.execute(
                "UPDATE mcp_context_sessions SET global_context_injected_at_ms = ?2
                 WHERE provider_fingerprint = ?1 AND global_context_injected_at_ms IS NULL",
                params![provider_fingerprint, now],
            )?;
            Ok(changed == 1)
        })
        .context("failed to claim one-time Local Agent context injection")
    }

    pub(super) fn claim_repo_agents_check(
        &self,
        provider: &ProviderCallMetadata,
        normalized_workdir: &str,
    ) -> Result<bool> {
        let now = now_ms()?;
        let provider_fingerprint = provider_fingerprint(provider);
        let workdir_fingerprint = value_fingerprint(normalized_workdir.as_bytes());
        self.with_foreground_connection(|connection| {
            let changed = connection.execute(
                "INSERT OR IGNORE INTO mcp_context_workdirs(
                    provider_fingerprint, workdir_fingerprint, repo_agents_checked_at_ms
                 ) VALUES (?1, ?2, ?3)",
                params![provider_fingerprint, workdir_fingerprint, now],
            )?;
            Ok(changed == 1)
        })
        .context("failed to claim one-time Local Agent workdir context check")
    }

    pub(super) fn claim_repo_skills_check(
        &self,
        provider: &ProviderCallMetadata,
        normalized_workdir: &str,
    ) -> Result<bool> {
        let now = now_ms()?;
        let provider_fingerprint = provider_fingerprint(provider);
        let workdir_fingerprint = value_fingerprint(normalized_workdir.as_bytes());
        self.with_foreground_connection(|connection| {
            let changed = connection.execute(
                "INSERT OR IGNORE INTO mcp_context_workdir_skills(
                    provider_fingerprint, workdir_fingerprint, repo_skills_checked_at_ms
                 ) VALUES (?1, ?2, ?3)",
                params![provider_fingerprint, workdir_fingerprint, now],
            )?;
            Ok(changed == 1)
        })
        .context("failed to claim one-time Local Agent repo-skill context check")
    }

    #[cfg(test)]
    pub(super) fn begin(
        &self,
        context: InvocationContext,
        start: InvocationStart,
    ) -> Result<InvocationContext> {
        self.begin_with_metadata(context, start)
            .map(|result| result.context)
    }

    pub(super) fn begin_with_metadata(
        &self,
        mut context: InvocationContext,
        start: InvocationStart,
    ) -> Result<HistoryBeginResult> {
        let correlation_id = context
            .correlation_id
            .clone()
            .unwrap_or_else(|| Arc::from(format!("{:032x}", rand::random::<u128>())));
        context.correlation_id = Some(correlation_id.clone());
        let now = now_ms()?;
        self.with_foreground_connection(|connection| {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .context("failed to start mandatory Local invocation-envelope transaction")?;

        let (agent_id, agent_first_seen_in_runtime) = resolve_agent(
            &transaction,
            context.provider.as_ref(),
            now,
            &self.runtime_id,
            &self.agent_id_source,
        )?;
        if let Some(agent_id) = agent_id.as_deref() {
            context.agent_id = Some(Arc::from(agent_id));
        }
        if let Some(provider) = context.provider.as_ref() {
            ensure_context_session(&transaction, provider)?;
            context.global_context_pending = agent_global_context_pending(&transaction, provider)?;
        }

        let canonical_args = canonical_json(&start.arguments)?;
        let declared_workdir_exact = start
            .arguments
            .get("workdir")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let declared_workdir_normalized = declared_workdir_exact
            .as_deref()
            .and_then(normalize_declared_workdir);
        context.repo_agents_context_pending = match (
            context.provider.as_ref(),
            declared_workdir_normalized.as_deref(),
        ) {
            (Some(provider), Some(workdir)) => {
                agent_repo_agents_context_pending(&transaction, provider, workdir)?
            }
            _ => false,
        };
        context.repo_skills_context_pending = match (
            context.provider.as_ref(),
            declared_workdir_normalized.as_deref(),
        ) {
            (Some(provider), Some(workdir)) => {
                agent_repo_skills_context_pending(&transaction, provider, workdir)?
            }
            _ => false,
        };
        let target_session_handle = start
            .arguments
            .get("session_handle")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let target_creator = start
            .target_created_by_agent_id
            .as_deref()
            .map(str::to_owned);
        let target_creator_invocation_id = start.target_created_by_invocation_id;
        let continuation_kind = start.continuation_kind.map(|kind| kind.as_str());
        let cross_agent = match (agent_id.as_deref(), target_creator.as_deref()) {
            (Some(caller), Some(creator)) => Some(i64::from(caller != creator)),
            _ => None,
        };
        let capture_state = if start.tool_name.as_ref() == "exec_command" {
            "pending"
        } else {
            "not_applicable"
        };
        let provider_kind = context.provider.as_ref().map(|value| value.kind.as_ref());
        let provider_key = context
            .provider
            .as_ref()
            .map(|value| value.session_key.as_ref());

        transaction
            .execute(
                "INSERT INTO invocations(
                    correlation_id, agent_id, provider_kind, provider_session_key, tool_name,
                    args_json, declared_workdir_exact, declared_workdir_normalized,
                    started_at_ms, evidence_state, capture_state, target_session_handle,
                    target_created_by_agent_id, target_created_by_invocation_id,
                    continuation_kind, cross_agent
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'pending', ?10, ?11, ?12, ?13, ?14, ?15)",
                params![
                    correlation_id.as_ref(),
                    agent_id,
                    provider_kind,
                    provider_key,
                    start.tool_name.as_ref(),
                    canonical_args,
                    declared_workdir_exact,
                    declared_workdir_normalized,
                    now,
                    capture_state,
                    target_session_handle,
                    target_creator,
                    target_creator_invocation_id,
                    continuation_kind,
                    cross_agent,
                ],
            )
            .context("failed to persist mandatory Local invocation envelope")?;
        let invocation_id = transaction.last_insert_rowid();
        let presentation_root_invocation_id = presentation_root_invocation_id(
            invocation_id,
            continuation_kind,
            target_creator_invocation_id,
        );

        let is_new_workdir = update_agent_workdir_summary(
            &transaction,
            agent_id.as_deref(),
            declared_workdir_normalized.as_deref(),
            now,
            invocation_id,
        )?;
        if is_new_workdir {
            transaction
                .execute(
                    "UPDATE invocations SET is_new_workdir = 1 WHERE id = ?1",
                    [invocation_id],
                )
                .context("failed to record new-workdir invocation evidence")?;
        }

            transaction
                .commit()
                .context("failed to commit mandatory Local invocation envelope")?;
            Ok(HistoryBeginResult {
                context: context.with_invocation_id(invocation_id),
                agent_first_seen_in_runtime,
                new_workdir: is_new_workdir.then(|| {
                    declared_workdir_normalized
                        .expect("new Local Agent workdir requires normalized workdir")
                }),
                presentation_root_invocation_id,
            })
        })
        .context("Local history invocation envelope is busy or unavailable")
    }

    pub(super) fn complete(
        &self,
        context: &InvocationContext,
        outcome: InvocationOutcome,
    ) -> Result<HistoryCompletionResult> {
        let invocation_id = context
            .invocation_id
            .context("Local invocation completion is missing durable invocation ID")?;
        let completed_at_ms = now_ms()?;
        let connection = self.lock_connection();
        let (started_at_ms, continuation_kind, target_created_by_invocation_id): (
            i64,
            Option<String>,
            Option<i64>,
        ) = connection
            .query_row(
                "SELECT started_at_ms, continuation_kind, target_created_by_invocation_id
                 FROM invocations WHERE id = ?1",
                [invocation_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .with_context(|| {
                format!("missing Local invocation {invocation_id} during completion")
            })?;
        let duration_ms = completed_at_ms.saturating_sub(started_at_ms);

        match outcome {
            InvocationOutcome::Success(value) => {
                let result_json = canonical_json(&value)?;
                let metadata = ResultMetadata::from_json(&value);
                connection
                    .execute(
                        "UPDATE invocations SET
                            completed_at_ms = ?2, duration_ms = ?3, outcome_kind = 'success',
                            result_json = ?4, error_text = NULL, result_status = ?5,
                            result_cwd = ?6, result_session_handle = ?7,
                            result_exit_code = ?8, result_termination_reason = ?9,
                            evidence_state = 'complete', evidence_reason = NULL
                         WHERE id = ?1",
                        params![
                            invocation_id,
                            completed_at_ms,
                            duration_ms,
                            result_json,
                            metadata.status,
                            metadata.cwd,
                            metadata.session_handle,
                            metadata.exit_code,
                            metadata.termination_reason,
                        ],
                    )
                    .context("failed to persist exact Local invocation success result")?;
            }
            InvocationOutcome::Error(error_text) => {
                connection
                    .execute(
                        "UPDATE invocations SET
                            completed_at_ms = ?2, duration_ms = ?3, outcome_kind = 'error',
                            result_json = NULL, error_text = ?4,
                            evidence_state = 'complete', evidence_reason = NULL,
                            capture_state = CASE
                                WHEN capture_state = 'pending' THEN 'complete'
                                ELSE capture_state
                            END,
                            capture_reason = CASE
                                WHEN capture_state = 'pending' THEN NULL
                                ELSE capture_reason
                            END
                         WHERE id = ?1",
                        params![invocation_id, completed_at_ms, duration_ms, error_text],
                    )
                    .context("failed to persist exact Local invocation error")?;
            }
        }
        Ok(HistoryCompletionResult {
            presentation_root_invocation_id: presentation_root_invocation_id(
                invocation_id,
                continuation_kind.as_deref(),
                target_created_by_invocation_id,
            ),
        })
    }

    pub(super) fn persist_output_batch(&self, events: &[OutputEvent]) -> Result<()> {
        if events.is_empty() {
            return Ok(());
        }
        let mut connection = self.lock_connection();
        let transaction = connection
            .transaction()
            .context("failed to start Local output-evidence batch transaction")?;
        for event in events {
            match event {
                OutputEvent::Chunk {
                    invocation_id,
                    agent_id: _,
                    sequence,
                    observed_at_ms,
                    text,
                } => {
                    transaction
                        .execute(
                            "INSERT OR IGNORE INTO invocation_output_chunks(
                                invocation_id, sequence, observed_at_ms, text
                             ) VALUES (?1, ?2, ?3, ?4)",
                            params![invocation_id, *sequence as i64, observed_at_ms, text],
                        )
                        .with_context(|| {
                            format!("failed to persist output chunk for invocation {invocation_id}")
                        })?;
                }
                OutputEvent::Complete {
                    invocation_id,
                    agent_id: _,
                } => {
                    transaction
                        .execute(
                            "UPDATE invocations
                             SET capture_state = 'complete', capture_reason = NULL
                             WHERE id = ?1 AND capture_state = 'pending'",
                            [invocation_id],
                        )
                        .with_context(|| {
                            format!(
                                "failed to finalize output capture for invocation {invocation_id}"
                            )
                        })?;
                }
                OutputEvent::Incomplete {
                    invocation_id,
                    agent_id: _,
                    reason,
                } => {
                    transaction
                        .execute(
                            "UPDATE invocations
                             SET capture_state = 'incomplete', capture_reason = ?2
                             WHERE id = ?1 AND capture_state IN ('pending', 'complete')",
                            params![invocation_id, reason],
                        )
                        .with_context(|| {
                            format!(
                                "failed to mark truncated output capture for invocation {invocation_id}"
                            )
                        })?;
                }
            }
        }
        transaction
            .commit()
            .context("failed to commit Local output-evidence batch")?;
        Ok(())
    }

    pub(super) fn mark_capture_incomplete(&self, invocation_id: i64, reason: &str) -> Result<()> {
        self.lock_connection()
            .execute(
                "UPDATE invocations
                 SET capture_state = CASE
                         WHEN capture_state IN ('pending', 'complete') THEN 'incomplete'
                         ELSE capture_state
                     END,
                     capture_reason = CASE
                         WHEN capture_state IN ('pending', 'complete') THEN ?2
                         ELSE capture_reason
                     END
                 WHERE id = ?1",
                params![invocation_id, reason],
            )
            .with_context(|| {
                format!("failed to mark invocation {invocation_id} output capture incomplete")
            })?;
        Ok(())
    }

    pub(super) fn mark_evidence_incomplete(&self, invocation_id: i64, reason: &str) -> Result<()> {
        self.lock_connection()
            .execute(
                "UPDATE invocations
                 SET evidence_state = 'incomplete', evidence_reason = ?2
                 WHERE id = ?1",
                params![invocation_id, reason],
            )
            .with_context(|| {
                format!("failed to mark invocation {invocation_id} evidence incomplete")
            })?;
        Ok(())
    }

    pub(super) fn set_health(&self, state: &str, reason: Option<&str>) -> Result<()> {
        self.lock_connection()
            .execute(
                "UPDATE history_state
                 SET health_state = ?1, health_reason = ?2, updated_at_ms = ?3
                 WHERE singleton = 1",
                params![state, reason, now_ms()?],
            )
            .context("failed to update Local history health state")?;
        Ok(())
    }

    pub(super) fn set_retention_state(
        &self,
        over_budget: bool,
        error_message: Option<&str>,
    ) -> Result<()> {
        self.lock_connection()
            .execute(
                "UPDATE history_state
                 SET over_budget = ?1, last_retention_error = ?2, updated_at_ms = ?3
                 WHERE singleton = 1",
                params![i64::from(over_budget), error_message, now_ms()?],
            )
            .context("failed to update Local history retention state")?;
        Ok(())
    }

    pub(super) fn pending_capture_ids(&self) -> Result<Vec<i64>> {
        let connection = self.lock_connection();
        let mut statement = connection
            .prepare("SELECT id FROM invocations WHERE capture_state = 'pending' ORDER BY id")
            .context("failed to prepare pending Local output-capture query")?;
        statement
            .query_map([], |row| row.get(0))
            .context("failed to query pending Local output captures")?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("failed to decode pending Local output captures")
    }

    fn recover_interrupted_capture(&self) -> Result<()> {
        self.lock_connection()
            .execute(
                "UPDATE invocations
                 SET evidence_state = CASE
                         WHEN evidence_state = 'pending' THEN 'incomplete'
                         ELSE evidence_state
                     END,
                     evidence_reason = CASE
                         WHEN evidence_state = 'pending' THEN COALESCE(evidence_reason, 'previous Local runtime ended before invocation evidence completed')
                         ELSE evidence_reason
                     END,
                     capture_state = CASE
                         WHEN capture_state = 'pending' THEN 'incomplete'
                         ELSE capture_state
                     END,
                     capture_reason = CASE
                         WHEN capture_state = 'pending' THEN COALESCE(capture_reason, 'previous Local runtime ended before output capture completed')
                         ELSE capture_reason
                     END
                 WHERE evidence_state = 'pending' OR capture_state = 'pending'",
                [],
            )
            .context("failed to recover interrupted Local output captures")?;
        Ok(())
    }

    pub(super) fn lock_connection(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.connection
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn lock_foreground_gate(&self) -> Result<MutexGuard<'_, ()>> {
        let deadline = Instant::now() + FOREGROUND_GATE_TIMEOUT;
        loop {
            match self.foreground_gate.try_lock() {
                Ok(guard) => return Ok(guard),
                Err(TryLockError::Poisoned(poisoned)) => return Ok(poisoned.into_inner()),
                Err(TryLockError::WouldBlock) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err(TryLockError::WouldBlock) => {
                    bail!("Local history foreground writer stayed busy past its bounded wait")
                }
            }
        }
    }

    pub(super) fn with_foreground_connection<T>(
        &self,
        operation: impl FnOnce(&mut Connection) -> Result<T>,
    ) -> Result<T> {
        // Foreground evidence uses an independent SQLite connection so the
        // background writer's mutex, batching, presentation work, or retention
        // can never serialize a model tool call behind it. Concurrent foreground
        // evidence writes are briefly serialized so normal multi-Agent bursts
        // retain evidence atomically, but both this gate and SQLite writer-lock
        // waiting are hard-bounded. After either bound, the caller fails open.
        let mut connection = Connection::open(&self.path).with_context(|| {
            format!(
                "failed to open foreground Local history connection {}",
                self.path.display()
            )
        })?;
        connection
            .busy_timeout(FOREGROUND_SQLITE_BUSY_TIMEOUT)
            .context("failed to configure foreground Local history SQLite wait bound")?;
        connection
            .pragma_update(None, "foreign_keys", "ON")
            .context("failed to enable foreground Local history foreign keys")?;
        // Opening/configuring independent SQLite handles does not need the
        // foreground writer gate. Acquire it only for the actual evidence
        // mutation so concurrent model tool calls spend as little time as
        // possible serialized behind one another.
        let _foreground = self.lock_foreground_gate()?;
        operation(&mut connection)
    }
}

#[derive(Default)]
struct ResultMetadata {
    status: Option<String>,
    cwd: Option<String>,
    session_handle: Option<String>,
    exit_code: Option<i64>,
    termination_reason: Option<String>,
}

impl ResultMetadata {
    fn from_json(value: &Value) -> Self {
        let Some(object) = value.as_object() else {
            return Self::default();
        };
        Self {
            status: object
                .get("status")
                .and_then(Value::as_str)
                .map(str::to_owned),
            cwd: object.get("cwd").and_then(Value::as_str).map(str::to_owned),
            session_handle: object
                .get("session_handle")
                .and_then(Value::as_str)
                .map(str::to_owned),
            exit_code: object.get("exit_code").and_then(Value::as_i64),
            termination_reason: object
                .get("termination_reason")
                .and_then(Value::as_str)
                .map(str::to_owned),
        }
    }
}

fn resolve_agent(
    transaction: &Transaction<'_>,
    provider: Option<&ProviderCallMetadata>,
    now: i64,
    runtime_id: &str,
    source: &AgentIdSource,
) -> Result<(Option<String>, bool)> {
    let Some(provider) = provider else {
        return Ok((None, false));
    };
    let existing: Option<(String, String)> = transaction
        .query_row(
            "SELECT id, last_seen_runtime_id FROM agents
             WHERE provider_kind = ?1 AND provider_session_key = ?2",
            params![provider.kind.as_ref(), provider.session_key.as_ref()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .context("failed to resolve Local Agent provider mapping")?;
    if let Some((id, last_seen_runtime_id)) = existing {
        let first_seen_in_runtime = last_seen_runtime_id != runtime_id;
        transaction
            .execute(
                "UPDATE agents SET last_seen_at_ms = ?2, last_seen_runtime_id = ?3 WHERE id = ?1",
                params![id, now, runtime_id],
            )
            .context("failed to update Local Agent last-seen evidence")?;
        return Ok((Some(id), first_seen_in_runtime));
    }

    for _ in 0..AGENT_ID_ATTEMPTS {
        let candidate = source();
        validate_agent_id(&candidate)?;
        match transaction.execute(
            "INSERT INTO agents(
                id, provider_kind, provider_session_key, first_seen_at_ms, last_seen_at_ms,
                last_seen_runtime_id
             ) VALUES (?1, ?2, ?3, ?4, ?4, ?5)",
            params![
                candidate,
                provider.kind.as_ref(),
                provider.session_key.as_ref(),
                now,
                runtime_id,
            ],
        ) {
            Ok(_) => return Ok((Some(candidate), true)),
            Err(error) if is_unique_constraint(&error) => {
                if let Some(existing) = transaction
                    .query_row(
                        "SELECT id FROM agents WHERE provider_kind = ?1 AND provider_session_key = ?2",
                        params![provider.kind.as_ref(), provider.session_key.as_ref()],
                        |row| row.get(0),
                    )
                    .optional()
                    .context("failed to recheck concurrent Local Agent mapping")?
                {
                    return Ok((Some(existing), false));
                }
            }
            Err(error) => return Err(error).context("failed to create Local Agent mapping"),
        }
    }
    bail!(
        "failed to allocate unique four-character Local Agent ID after {AGENT_ID_ATTEMPTS} attempts"
    )
}

fn ensure_context_session(
    transaction: &Transaction<'_>,
    provider: &ProviderCallMetadata,
) -> Result<()> {
    transaction
        .execute(
            "INSERT OR IGNORE INTO mcp_context_sessions(provider_fingerprint)
             VALUES (?1)",
            [provider_fingerprint(provider)],
        )
        .context("failed to initialize Local conversation context delivery state")?;
    Ok(())
}

fn agent_global_context_pending(
    transaction: &Transaction<'_>,
    provider: &ProviderCallMetadata,
) -> Result<bool> {
    transaction
        .query_row(
            "SELECT global_context_injected_at_ms IS NULL FROM mcp_context_sessions
             WHERE provider_fingerprint = ?1",
            [provider_fingerprint(provider)],
            |row| row.get(0),
        )
        .context("failed to inspect Local Agent global-context delivery state")
}

fn agent_repo_agents_context_pending(
    transaction: &Transaction<'_>,
    provider: &ProviderCallMetadata,
    normalized_workdir: &str,
) -> Result<bool> {
    let provider_fingerprint = provider_fingerprint(provider);
    let workdir_fingerprint = value_fingerprint(normalized_workdir.as_bytes());
    let existing: Option<i64> = transaction
        .query_row(
            "SELECT repo_agents_checked_at_ms FROM mcp_context_workdirs
             WHERE provider_fingerprint = ?1 AND workdir_fingerprint = ?2",
            params![provider_fingerprint, workdir_fingerprint],
            |row| row.get(0),
        )
        .optional()
        .context("failed to inspect Local Agent workdir-context delivery state")?;
    Ok(existing.is_none())
}

fn agent_repo_skills_context_pending(
    transaction: &Transaction<'_>,
    provider: &ProviderCallMetadata,
    normalized_workdir: &str,
) -> Result<bool> {
    let provider_fingerprint = provider_fingerprint(provider);
    let workdir_fingerprint = value_fingerprint(normalized_workdir.as_bytes());
    let existing: Option<i64> = transaction
        .query_row(
            "SELECT repo_skills_checked_at_ms FROM mcp_context_workdir_skills
             WHERE provider_fingerprint = ?1 AND workdir_fingerprint = ?2",
            params![provider_fingerprint, workdir_fingerprint],
            |row| row.get(0),
        )
        .optional()
        .context("failed to inspect Local Agent repo-skill delivery state")?;
    Ok(existing.is_none())
}

fn provider_fingerprint(provider: &ProviderCallMetadata) -> Vec<u8> {
    let mut hasher = Sha256::new();
    hasher.update(provider.kind.as_bytes());
    hasher.update([0]);
    hasher.update(provider.session_key.as_bytes());
    hasher.finalize().to_vec()
}

fn value_fingerprint(value: &[u8]) -> Vec<u8> {
    Sha256::digest(value).to_vec()
}

fn update_agent_workdir_summary(
    transaction: &Transaction<'_>,
    agent_id: Option<&str>,
    normalized_workdir: Option<&str>,
    now: i64,
    invocation_id: i64,
) -> Result<bool> {
    let (Some(agent_id), Some(normalized_workdir)) = (agent_id, normalized_workdir) else {
        return Ok(false);
    };
    let existing: Option<i64> = transaction
        .query_row(
            "SELECT ordinal FROM agent_workdirs WHERE agent_id = ?1 AND normalized_workdir = ?2",
            params![agent_id, normalized_workdir],
            |row| row.get(0),
        )
        .optional()
        .context("failed to inspect Local Agent workdir summary")?;
    if existing.is_some() {
        transaction
            .execute(
                "UPDATE agent_workdirs SET
                    last_seen_at_ms = ?3, last_invocation_id = ?4,
                    retained_invocation_count = retained_invocation_count + 1
                 WHERE agent_id = ?1 AND normalized_workdir = ?2",
                params![agent_id, normalized_workdir, now, invocation_id],
            )
            .context("failed to update Local Agent workdir summary")?;
        return Ok(false);
    }

    let next_ordinal: i64 = transaction
        .query_row(
            "SELECT COALESCE(MAX(ordinal), 0) + 1 FROM agent_workdirs WHERE agent_id = ?1",
            [agent_id],
            |row| row.get(0),
        )
        .context("failed to allocate Local Agent workdir order")?;
    transaction
        .execute(
            "INSERT INTO agent_workdirs(
                agent_id, normalized_workdir, ordinal, first_seen_at_ms, last_seen_at_ms,
                first_invocation_id, last_invocation_id, retained_invocation_count
             ) VALUES (?1, ?2, ?3, ?4, ?4, ?5, ?5, 1)",
            params![
                agent_id,
                normalized_workdir,
                next_ordinal,
                now,
                invocation_id
            ],
        )
        .context("failed to create Local Agent workdir summary")?;
    Ok(true)
}

pub(super) fn recompute_summaries(transaction: &Transaction<'_>) -> Result<()> {
    transaction
        .execute("DELETE FROM agent_workdirs", [])
        .context("failed to reset retained Local Agent workdir summaries")?;
    let agent_ids = {
        let mut statement = transaction
            .prepare("SELECT id FROM agents ORDER BY id")
            .context("failed to prepare retained Local Agent enumeration")?;
        statement
            .query_map([], |row| row.get::<_, String>(0))
            .context("failed to enumerate retained Local Agents")?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("failed to collect retained Local Agents")?
    };
    for agent_id in agent_ids {
        let groups = {
            let mut statement = transaction
                .prepare(
                    "SELECT declared_workdir_normalized, MIN(started_at_ms), MAX(started_at_ms),
                            MIN(id), MAX(id), COUNT(*)
                     FROM invocations
                     WHERE agent_id = ?1 AND declared_workdir_normalized IS NOT NULL
                     GROUP BY declared_workdir_normalized
                     ORDER BY MIN(id) ASC",
                )
                .context("failed to prepare retained Agent workdir recomputation")?;
            statement
                .query_map([&agent_id], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, i64>(5)?,
                    ))
                })
                .context("failed to query retained Agent workdirs")?
                .collect::<rusqlite::Result<Vec<_>>>()
                .context("failed to collect retained Agent workdirs")?
        };
        for (index, (workdir, first_seen, last_seen, first_id, last_id, count)) in
            groups.into_iter().enumerate()
        {
            transaction
                .execute(
                    "INSERT INTO agent_workdirs(
                        agent_id, normalized_workdir, ordinal, first_seen_at_ms, last_seen_at_ms,
                        first_invocation_id, last_invocation_id, retained_invocation_count
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    params![
                        agent_id,
                        workdir,
                        (index + 1) as i64,
                        first_seen,
                        last_seen,
                        first_id,
                        last_id,
                        count,
                    ],
                )
                .context("failed to rebuild retained Agent workdir summary")?;
        }
    }
    transaction
        .execute(
            "DELETE FROM agents WHERE NOT EXISTS(
                SELECT 1 FROM invocations WHERE invocations.agent_id = agents.id
             )",
            [],
        )
        .context("failed to prune unsupported retained Local Agents")?;
    transaction
        .execute(
            "UPDATE agents SET
                first_seen_at_ms = (SELECT MIN(started_at_ms) FROM invocations WHERE agent_id = agents.id),
                last_seen_at_ms = (SELECT MAX(started_at_ms) FROM invocations WHERE agent_id = agents.id)
             WHERE EXISTS(SELECT 1 FROM invocations WHERE invocations.agent_id = agents.id)",
            [],
        )
        .context("failed to recompute retained Local Agent time bounds")?;
    Ok(())
}

pub(super) fn canonical_json(value: &Value) -> Result<String> {
    serde_json::to_string(&sort_json(value))
        .context("failed to serialize canonical Local evidence JSON")
}

fn sort_json(value: &Value) -> Value {
    match value {
        Value::Object(object) => {
            let mut keys = object.keys().collect::<Vec<_>>();
            keys.sort();
            let mut sorted = serde_json::Map::new();
            for key in keys {
                sorted.insert(key.clone(), sort_json(&object[key]));
            }
            Value::Object(sorted)
        }
        Value::Array(values) => Value::Array(values.iter().map(sort_json).collect()),
        _ => value.clone(),
    }
}

pub(crate) fn normalize_declared_workdir(raw: &str) -> Option<String> {
    let path = Path::new(raw);
    if !path.is_absolute() {
        return None;
    }
    if let Ok(canonical) = fs::canonicalize(path) {
        return Some(canonical.display().to_string());
    }
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::RootDir | Component::Prefix(_) | Component::Normal(_) => {
                normalized.push(component.as_os_str())
            }
            Component::CurDir => {}
            Component::ParentDir => {
                let _ = normalized.pop();
            }
        }
    }
    Some(normalized.display().to_string())
}

pub(super) fn now_ms() -> Result<i64> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before Unix epoch")?;
    i64::try_from(duration.as_millis()).context("system timestamp exceeds SQLite integer range")
}

fn random_agent_id() -> String {
    rand::random::<[u8; 4]>()
        .into_iter()
        .map(|value| AGENT_ID_ALPHABET[value as usize % AGENT_ID_ALPHABET.len()] as char)
        .collect()
}

fn validate_agent_id(value: &str) -> Result<()> {
    if value.len() == 4
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
    {
        return Ok(());
    }
    bail!(
        "Agent ID source produced invalid ID `{value}`; expected four lowercase alphanumeric characters"
    )
}

fn is_unique_constraint(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(code, _)
            if code.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_PRIMARYKEY
                || code.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE
    )
}

fn ensure_history_parent(path: &Path) -> Result<()> {
    let parent = path
        .parent()
        .context("Local history database path has no parent")?;
    fs::create_dir_all(parent).with_context(|| {
        format!(
            "failed to create Local history directory {}",
            parent.display()
        )
    })?;
    crate::local::private_fs::set_user_only_directory(parent)
}

pub(super) fn physical_store_size(path: &Path) -> Result<u64> {
    let mut total = 0_u64;
    for candidate in history_store_paths(path) {
        if candidate.exists() {
            total = total.saturating_add(
                fs::metadata(&candidate)
                    .with_context(|| format!("failed to stat {}", candidate.display()))?
                    .len(),
            );
        }
    }
    Ok(total)
}

pub(super) fn mark_retention_error_best_effort(store: &HistoryStore, error_message: &str) {
    if let Err(error) = store.set_retention_state(false, Some(error_message)) {
        error!(
            event = "local_history_retention_state_persistence_failed",
            error = %error,
        );
    }
    warn!(
        event = "local_history_retention_failed",
        error = error_message
    );
}

pub(super) fn distinct_invocation_ids(events: &[OutputEvent]) -> HashSet<i64> {
    events
        .iter()
        .map(|event| match event {
            OutputEvent::Chunk { invocation_id, .. }
            | OutputEvent::Complete { invocation_id, .. }
            | OutputEvent::Incomplete { invocation_id, .. } => *invocation_id,
        })
        .collect()
}
