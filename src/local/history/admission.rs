//! Producer operations are memory-only. Only startup and background `replenish` touch storage.
use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::Hash;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, bail};
use rusqlite::TransactionBehavior;
use sha2::{Digest, Sha256};

use super::store::{AgentIdSource, HistoryStore};
use crate::invocation::{InvocationContext, InvocationStart, ProviderCallMetadata};

const ID_RANGE_SIZE: i64 = 1_048_576;
const ID_REFILL_THRESHOLD: i64 = 1024;
const MAX_CONTEXT_CLAIMS: usize = 16_384;

// FIFO eviction bounds memory, including when loading durable claims. Evicted
// context may be injected again, including after a restart; identity is never
// evicted or changed as a consequence of context-cache pressure.
struct BoundedClaims<T> {
    entries: HashSet<T>,
    order: VecDeque<T>,
}

impl<T> Default for BoundedClaims<T> {
    fn default() -> Self {
        Self {
            entries: HashSet::new(),
            order: VecDeque::new(),
        }
    }
}

impl<T: Eq + Hash + Clone> BoundedClaims<T> {
    fn contains(&self, value: &T) -> bool {
        self.entries.contains(value)
    }

    fn insert(&mut self, value: T) -> bool {
        if self.entries.contains(&value) {
            return false;
        }
        if self.entries.len() >= MAX_CONTEXT_CLAIMS
            && let Some(oldest) = self.order.pop_front()
        {
            self.entries.remove(&oldest);
        }
        self.order.push_back(value.clone());
        self.entries.insert(value)
    }
}

struct IdRange {
    next: i64,
    end: i64,
}

fn reserve_range(transaction: &rusqlite::Transaction<'_>) -> Result<IdRange> {
    let high: i64 = transaction.query_row(
        "SELECT MAX(COALESCE((SELECT MAX(id) FROM invocations), 0), COALESCE((SELECT MAX(seq) FROM sqlite_sequence WHERE name = 'invocations'), 0))",
        [], |row| row.get(0),
    )?;
    let end = high
        .checked_add(ID_RANGE_SIZE)
        .context("Local history invocation ID space exhausted")?;
    let changed = transaction.execute(
        "UPDATE sqlite_sequence SET seq = ?1 WHERE name = 'invocations'",
        [end],
    )?;
    if changed == 0 {
        transaction.execute(
            "INSERT INTO sqlite_sequence(name, seq) VALUES ('invocations', ?1)",
            [end],
        )?;
    }
    Ok(IdRange {
        next: high + 1,
        end,
    })
}
type Fingerprint = Vec<u8>;
type WorkdirClaim = (Fingerprint, Fingerprint);

#[derive(Default)]
struct Cache {
    agents: HashMap<(String, String), Arc<str>>,
    used_ids: HashSet<String>,
    global: BoundedClaims<Fingerprint>,
    agents_claims: BoundedClaims<WorkdirClaim>,
    skills_claims: BoundedClaims<WorkdirClaim>,
}

pub(super) struct AdmissionState {
    ids: Mutex<IdRange>,
    // Only background refill uses this mutex, never a producer.
    refill: Mutex<()>,
    source: AgentIdSource,
    cache: Mutex<Cache>,
}

impl AdmissionState {
    /// Called before serving tools. Requires one runtime owner for this database;
    /// reserving disjoint ID ranges alone does not coordinate provider identity.
    pub(super) fn open(store: &HistoryStore) -> Result<Self> {
        let mut connection = store.lock_connection();
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let ids = reserve_range(&transaction)?;
        let mut cache = Cache::default();
        {
            let mut statement = transaction
                .prepare("SELECT provider_kind, provider_session_key, id FROM agents")?;
            for entry in statement.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })? {
                let (kind, key, id) = entry?;
                cache.used_ids.insert(id.clone());
                cache.agents.insert((kind, key), Arc::from(id));
            }
            let mut statement = transaction.prepare("SELECT provider_fingerprint FROM mcp_context_sessions WHERE global_context_injected_at_ms IS NOT NULL")?;
            for entry in statement.query_map([], |row| row.get::<_, Vec<u8>>(0))? {
                cache.global.insert(entry?);
            }
            for (table, target) in [
                ("mcp_context_workdirs", &mut cache.agents_claims),
                ("mcp_context_workdir_skills", &mut cache.skills_claims),
            ] {
                let mut statement = transaction.prepare(&format!(
                    "SELECT provider_fingerprint, workdir_fingerprint FROM {table}"
                ))?;
                for entry in statement.query_map([], |row| {
                    Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, Vec<u8>>(1)?))
                })? {
                    target.insert(entry?);
                }
            }
        }
        transaction.commit()?;
        Ok(Self {
            ids: Mutex::new(ids),
            refill: Mutex::new(()),
            source: store.agent_id_source(),
            cache: Mutex::new(cache),
        })
    }

    /// Background worker only. No producer-shared lock spans storage access.
    /// Publish only after commit, discarding the old tail rather than assuming
    /// that another range reservation could not have advanced SQLite highwater.
    pub(super) fn replenish(&self, store: &HistoryStore) -> Result<()> {
        let _refill = self
            .refill
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        {
            let ids = self
                .ids
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if ids.end.saturating_sub(ids.next).saturating_add(1) >= ID_REFILL_THRESHOLD {
                return Ok(());
            }
        }
        let replacement = {
            let mut connection = store.lock_connection();
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let range = reserve_range(&transaction)?;
            transaction.commit()?;
            range
        };
        *self
            .ids
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = replacement;
        Ok(())
    }

    pub(super) fn reserve(
        &self,
        context: InvocationContext,
        start: &InvocationStart,
    ) -> Result<InvocationContext> {
        let mut context = self.identify(context)?;
        let id = {
            let mut ids = self
                .ids
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if ids.next > ids.end {
                bail!(
                    "Local history invocation ID reservation exhausted; background refill pending"
                );
            }
            let id = ids.next;
            ids.next = ids
                .next
                .checked_add(1)
                .context("Local history invocation ID space exhausted")?;
            id
        };
        context.invocation_id = Some(id);
        if context.correlation_id.is_none() {
            context.correlation_id = Some(Arc::from(format!("{:032x}", rand::random::<u128>())));
        }
        context.repo_agents_context_pending = false;
        context.repo_skills_context_pending = false;
        if let Some(provider) = context.provider.as_ref()
            && let Some(workdir) = start
                .arguments
                .get("workdir")
                .and_then(serde_json::Value::as_str)
                .and_then(lexical_workdir)
        {
            let claim = workdir_claim(provider, &workdir);
            let cache = self
                .cache
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            context.repo_agents_context_pending = !cache.agents_claims.contains(&claim);
            context.repo_skills_context_pending = !cache.skills_claims.contains(&claim);
        }
        Ok(context)
    }

    pub(super) fn identify(&self, mut context: InvocationContext) -> Result<InvocationContext> {
        context.global_context_pending = false;
        if let Some(provider) = context.provider.as_ref() {
            let key = (provider.kind.to_string(), provider.session_key.to_string());
            let mut cache = self
                .cache
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let id = match cache.agents.get(&key) {
                Some(id) => id.clone(),
                None => {
                    let mut allocated = None;
                    for _ in 0..128 {
                        let candidate = (self.source)();
                        if candidate.len() != 4
                            || !candidate
                                .bytes()
                                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
                        {
                            bail!("invalid Local Agent ID candidate");
                        }
                        if cache.used_ids.insert(candidate.clone()) {
                            allocated = Some(Arc::<str>::from(candidate));
                            break;
                        }
                    }
                    let id = allocated.context("failed to allocate unique four-character Local Agent ID after 128 attempts")?;
                    cache.agents.insert(key, id.clone());
                    id
                }
            };
            context.agent_id = Some(id);
            context.global_context_pending =
                !cache.global.contains(&provider_fingerprint(provider));
        }
        Ok(context)
    }

    pub(super) fn claim_global(&self, provider: &ProviderCallMetadata) -> bool {
        self.cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .global
            .insert(provider_fingerprint(provider))
    }

    pub(super) fn claim_agents(&self, provider: &ProviderCallMetadata, workdir: &str) -> bool {
        self.cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .agents_claims
            .insert(workdir_claim(provider, workdir))
    }

    pub(super) fn claim_skills(&self, provider: &ProviderCallMetadata, workdir: &str) -> bool {
        self.cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .skills_claims
            .insert(workdir_claim(provider, workdir))
    }
}

fn provider_fingerprint(provider: &ProviderCallMetadata) -> Fingerprint {
    let mut hash = Sha256::new();
    hash.update(provider.kind.as_bytes());
    hash.update([0]);
    hash.update(provider.session_key.as_bytes());
    hash.finalize().to_vec()
}

fn workdir_claim(provider: &ProviderCallMetadata, workdir: &str) -> WorkdirClaim {
    let normalized = lexical_workdir(workdir).unwrap_or_else(|| workdir.to_owned());
    (
        provider_fingerprint(provider),
        Sha256::digest(normalized.as_bytes()).to_vec(),
    )
}

/// Lexical only: history must never canonicalize or probe the filesystem.
pub(crate) fn lexical_workdir(raw: &str) -> Option<String> {
    let path = Path::new(raw);
    if !path.is_absolute() {
        return None;
    }
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::RootDir | Component::Prefix(_) | Component::Normal(_) => {
                normalized.push(component.as_os_str())
            }
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
        }
    }
    Some(normalized.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::Ordering;

    fn start() -> InvocationStart {
        InvocationStart::new("exec_command", json!({"workdir": "/tmp/a/../b"}))
    }
    fn context() -> InvocationContext {
        InvocationContext::default()
            .with_provider(ProviderCallMetadata::new("test", "conversation"))
    }

    #[test]
    fn startup_reservations_survive_empty_rows_and_do_not_overlap() {
        let temp = tempfile::tempdir().unwrap();
        let store =
            HistoryStore::open(temp.path().join("history.sqlite3"), Arc::from("test")).unwrap();
        let first = AdmissionState::open(&store).unwrap();
        assert_eq!(
            first.reserve(context(), &start()).unwrap().invocation_id,
            Some(1)
        );
        let second = AdmissionState::open(&store).unwrap();
        assert_eq!(
            second.reserve(context(), &start()).unwrap().invocation_id,
            Some(ID_RANGE_SIZE + 1)
        );
        {
            let mut ids = first.ids.lock().unwrap();
            ids.next = ids.end + 1;
        }
        assert!(first.reserve(context(), &start()).is_err());
    }

    #[test]
    fn concurrent_identity_and_claims_are_memory_only() {
        let temp = tempfile::tempdir().unwrap();
        let store =
            HistoryStore::open(temp.path().join("history.sqlite3"), Arc::from("test")).unwrap();
        let admission = Arc::new(AdmissionState::open(&store).unwrap());
        // Holding the storage mutex must not prevent any admission operation.
        let _storage = store.lock_connection();
        let threads: Vec<_> = (0..16)
            .map(|_| {
                let admission = admission.clone();
                std::thread::spawn(move || {
                    let context = admission.reserve(context(), &start()).unwrap();
                    let provider = context.provider.as_ref().unwrap();
                    (
                        context.agent_id.clone(),
                        context.invocation_id,
                        admission.claim_global(provider),
                        admission.claim_agents(provider, "/tmp/b"),
                        admission.claim_skills(provider, "/tmp/b"),
                    )
                })
            })
            .collect();
        let results: Vec<_> = threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .collect();
        assert!(results.iter().all(|result| result.0 == results[0].0));
        assert_eq!(
            results
                .iter()
                .map(|result| result.1)
                .collect::<HashSet<_>>()
                .len(),
            16
        );
        assert_eq!(results.iter().filter(|result| result.2).count(), 1);
        assert_eq!(results.iter().filter(|result| result.3).count(), 1);
        assert_eq!(results.iter().filter(|result| result.4).count(), 1);
        let next = admission.reserve(context(), &start()).unwrap();
        assert!(
            !next.global_context_pending
                && !next.repo_agents_context_pending
                && !next.repo_skills_context_pending
        );
    }

    #[test]
    fn persisted_identity_and_claims_are_loaded_and_collisions_are_skipped() {
        let temp = tempfile::tempdir().unwrap();
        let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let generated = attempts.clone();
        let store = HistoryStore::open_with_agent_id_source(
            temp.path().join("history.sqlite3"),
            Arc::from("test"),
            Arc::new(move || {
                if generated.fetch_add(1, Ordering::Relaxed) == 0 {
                    "abcd".into()
                } else {
                    "efgh".into()
                }
            }),
        )
        .unwrap();
        let provider = ProviderCallMetadata::new("test", "conversation");
        {
            let connection = store.lock_connection();
            connection.execute("INSERT INTO agents(id, provider_kind, provider_session_key, first_seen_at_ms, last_seen_at_ms, last_seen_runtime_id) VALUES ('abcd', 'test', 'conversation', 1, 1, 'old')", []).unwrap();
            let fingerprint = provider_fingerprint(&provider);
            connection.execute("INSERT INTO mcp_context_sessions(provider_fingerprint, global_context_injected_at_ms) VALUES (?1, 1)", [&fingerprint]).unwrap();
            let (_, workdir) = workdir_claim(&provider, "/tmp/b");
            connection.execute("INSERT INTO mcp_context_workdirs(provider_fingerprint, workdir_fingerprint, repo_agents_checked_at_ms) VALUES (?1, ?2, 1)", rusqlite::params![fingerprint, workdir]).unwrap();
            connection.execute("INSERT INTO mcp_context_workdir_skills(provider_fingerprint, workdir_fingerprint, repo_skills_checked_at_ms) VALUES (?1, ?2, 1)", rusqlite::params![fingerprint, workdir]).unwrap();
        }
        let admission = AdmissionState::open(&store).unwrap();
        let known = admission.reserve(context(), &start()).unwrap();
        assert_eq!(known.agent_id.as_deref(), Some("abcd"));
        assert!(
            !known.global_context_pending
                && !known.repo_agents_context_pending
                && !known.repo_skills_context_pending
        );
        assert!(!admission.claim_global(&provider));
        assert!(!admission.claim_agents(&provider, "/tmp/a/../b"));
        assert!(!admission.claim_skills(&provider, "/tmp/b"));
        let other = admission
            .identify(
                InvocationContext::default()
                    .with_provider(ProviderCallMetadata::new("test", "other")),
            )
            .unwrap();
        assert_eq!(other.agent_id.as_deref(), Some("efgh"));
        assert_eq!(attempts.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn background_refill_skips_external_ranges_and_recovers_exhaustion() {
        let temp = tempfile::tempdir().unwrap();
        let store =
            HistoryStore::open(temp.path().join("history.sqlite3"), Arc::from("test")).unwrap();
        let first = AdmissionState::open(&store).unwrap();
        // Healthy ranges do not reserve another block on every worker tick.
        first.replenish(&store).unwrap();
        let second = AdmissionState::open(&store).unwrap();
        assert_eq!(
            second.reserve(context(), &start()).unwrap().invocation_id,
            Some(ID_RANGE_SIZE + 1)
        );
        {
            let mut ids = first.ids.lock().unwrap();
            ids.next = ids.end - 5;
        }
        first.replenish(&store).unwrap();
        assert_eq!(
            first.reserve(context(), &start()).unwrap().invocation_id,
            Some(ID_RANGE_SIZE * 2 + 1)
        );
        {
            let mut ids = first.ids.lock().unwrap();
            ids.next = ids.end + 1;
        }
        assert!(first.reserve(context(), &start()).is_err());
        first.replenish(&store).unwrap();
        assert_eq!(
            first.reserve(context(), &start()).unwrap().invocation_id,
            Some(ID_RANGE_SIZE * 3 + 1)
        );
    }

    #[test]
    fn blocked_background_refill_does_not_block_producers() {
        let temp = tempfile::tempdir().unwrap();
        let store = Arc::new(
            HistoryStore::open(temp.path().join("history.sqlite3"), Arc::from("test")).unwrap(),
        );
        let admission = Arc::new(AdmissionState::open(&store).unwrap());
        {
            let mut ids = admission.ids.lock().unwrap();
            ids.next = ids.end - 5;
        }
        let storage = store.lock_connection();
        let worker = {
            let admission = admission.clone();
            let store = store.clone();
            std::thread::spawn(move || admission.replenish(&store))
        };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while admission.refill.try_lock().is_ok() {
            assert!(
                std::time::Instant::now() < deadline,
                "refill worker did not start"
            );
            std::thread::yield_now();
        }
        // The worker owns the refill lock and is blocked on storage. Producers
        // must still obtain IDs from the old reserved tail without waiting.
        let (send, receive) = std::sync::mpsc::channel();
        let producer = {
            let admission = admission.clone();
            std::thread::spawn(move || send.send(admission.reserve(context(), &start())).unwrap())
        };
        let result = receive.recv_timeout(std::time::Duration::from_secs(2));
        drop(storage);
        worker.join().unwrap().unwrap();
        producer.join().unwrap();
        assert_eq!(
            result.unwrap().unwrap().invocation_id,
            Some(ID_RANGE_SIZE - 5)
        );
        assert_eq!(
            admission
                .reserve(context(), &start())
                .unwrap()
                .invocation_id,
            Some(ID_RANGE_SIZE + 1)
        );
    }

    #[test]
    fn context_claim_eviction_is_bounded_without_changing_identity() {
        let temp = tempfile::tempdir().unwrap();
        let store =
            HistoryStore::open(temp.path().join("history.sqlite3"), Arc::from("test")).unwrap();
        let admission = AdmissionState::open(&store).unwrap();
        let original = admission.identify(context()).unwrap();
        let provider = original.provider.as_ref().unwrap();
        assert!(admission.claim_global(provider));
        assert!(admission.claim_agents(provider, "/old"));
        assert!(admission.claim_skills(provider, "/old"));
        for i in 0..MAX_CONTEXT_CLAIMS {
            let other = ProviderCallMetadata::new("test", format!("other-{i}"));
            admission.claim_global(&other);
            admission.claim_agents(provider, &format!("/dir-{i}"));
            admission.claim_skills(provider, &format!("/dir-{i}"));
        }
        {
            let cache = admission.cache.lock().unwrap();
            assert_eq!(cache.global.entries.len(), MAX_CONTEXT_CLAIMS);
            assert_eq!(cache.agents_claims.entries.len(), MAX_CONTEXT_CLAIMS);
            assert_eq!(cache.skills_claims.entries.len(), MAX_CONTEXT_CLAIMS);
            assert_eq!(cache.global.order.len(), MAX_CONTEXT_CLAIMS);
        }
        assert!(admission.claim_global(provider));
        assert!(admission.claim_agents(provider, "/old"));
        assert!(admission.claim_skills(provider, "/old"));
        assert!(!admission.claim_global(provider));
        assert_eq!(
            admission.identify(context()).unwrap().agent_id,
            original.agent_id
        );
    }

    #[test]
    fn lexical_normalization_never_needs_existing_paths() {
        assert_eq!(
            lexical_workdir(if cfg!(windows) {
                r"C:\nonexistent\..\x\.\y"
            } else {
                "/nonexistent/../x/./y"
            }),
            Some(if cfg!(windows) { r"C:\x\y" } else { "/x/y" }.to_owned())
        );
        assert_eq!(lexical_workdir("relative"), None);
    }
}
