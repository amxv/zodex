use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::session::{
    OwnedProcess, OwnedProcessEnd, OwnedProcessObserver, ProcessControl, ProcessIdentity,
    ProcessSignal, identity_matches, signal_process_if_matching,
};

pub const LOCAL_PROCESS_REGISTRY_SCHEMA_VERSION: u32 = 1;
const STALE_PROCESS_DISCOVERY_LIMIT: usize = 1024;
const STALE_PROCESS_TERM_GRACE: Duration = Duration::from_secs(5);
const STALE_PROCESS_KILL_GRACE: Duration = Duration::from_secs(1);
const STALE_PROCESS_POLL: Duration = Duration::from_millis(50);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LocalProcessRegistryDocument {
    pub schema_version: u32,
    #[serde(default)]
    pub runtime_id: String,
    pub processes: Vec<LocalOwnedProcessRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LocalOwnedProcessRecord {
    pub internal_session_id: u64,
    pub session_handle: String,
    pub identity: ProcessIdentity,
    #[serde(default)]
    pub group_members: Vec<ProcessIdentity>,
    pub created_by_agent_id: Option<String>,
    pub invocation_correlation_id: Option<String>,
}

impl From<&OwnedProcess> for LocalOwnedProcessRecord {
    fn from(process: &OwnedProcess) -> Self {
        Self {
            internal_session_id: process.internal_session_id,
            session_handle: process.session_handle.to_string(),
            identity: process.identity.clone(),
            group_members: Vec::new(),
            created_by_agent_id: process.created_by.agent_id.as_deref().map(str::to_owned),
            invocation_correlation_id: process
                .created_by
                .correlation_id
                .as_deref()
                .map(str::to_owned),
        }
    }
}

#[derive(Clone)]
pub struct LocalOwnedProcessRegistry {
    path: PathBuf,
    runtime_id: Arc<str>,
    records: Arc<Mutex<Vec<LocalOwnedProcessRecord>>>,
}

impl LocalOwnedProcessRegistry {
    pub fn fresh(path: impl Into<PathBuf>, runtime_id: impl Into<Arc<str>>) -> Result<Self> {
        let path = path.into();
        let runtime_id = runtime_id.into();
        if runtime_id.trim().is_empty() {
            bail!("Local owned-process registry runtime identity must not be empty");
        }
        if path.exists() {
            let existing = load_document(&path)?;
            if !existing.processes.is_empty() {
                bail!(
                    "Local owned-process registry at {} still contains {} process record(s); stale recovery must resolve it before a fresh runtime starts",
                    path.display(),
                    existing.processes.len()
                );
            }
            fs::remove_file(&path).with_context(|| {
                format!("failed to remove empty stale registry {}", path.display())
            })?;
        }
        Ok(Self {
            path,
            runtime_id,
            records: Arc::new(Mutex::new(Vec::new())),
        })
    }

    pub fn snapshot(&self) -> Vec<LocalOwnedProcessRecord> {
        self.records
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn update(&self, mutate: impl FnOnce(&mut Vec<LocalOwnedProcessRecord>)) -> Result<()> {
        let mut guard = self
            .records
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut next = guard.clone();
        mutate(&mut next);
        persist_records(&self.path, &self.runtime_id, &next)?;
        *guard = next;
        Ok(())
    }
}

impl OwnedProcessObserver for LocalOwnedProcessRegistry {
    fn process_started(&self, process: &OwnedProcess) -> Result<()> {
        let record = LocalOwnedProcessRecord::from(process);
        self.update(|records| {
            records.retain(|existing| existing.internal_session_id != record.internal_session_id);
            records.push(record);
        })
    }

    fn process_group_members_updated(
        &self,
        process: &OwnedProcess,
        members: &[ProcessIdentity],
    ) -> Result<()> {
        self.update(|records| {
            if let Some(record) = records
                .iter_mut()
                .find(|record| record.internal_session_id == process.internal_session_id)
            {
                record.group_members = members.to_vec();
            }
        })
    }

    fn process_ended(&self, process: &OwnedProcess, _end: &OwnedProcessEnd) -> Result<()> {
        self.update(|records| {
            records.retain(|record| record.internal_session_id != process.internal_session_id);
        })
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StaleProcessCleanupReport {
    pub signaled: usize,
    pub force_killed: usize,
    pub descendants_signaled: usize,
    pub descendants_force_killed: usize,
    pub already_gone: usize,
    pub identity_mismatch: usize,
    pub unresolved_leaderless_groups: usize,
    pub survivors: usize,
}

#[cfg(test)]
fn signal_matching_stale_processes(
    registry_path: &Path,
    control: &dyn ProcessControl,
    signal: ProcessSignal,
) -> Result<StaleProcessCleanupReport> {
    if !registry_path.exists() {
        return Ok(StaleProcessCleanupReport::default());
    }
    let document = load_document(registry_path)?;
    let mut report = StaleProcessCleanupReport::default();
    for process in document.processes {
        match control.identity(process.identity.pid)? {
            None => report.already_gone += 1,
            Some(current) if current != process.identity => report.identity_mismatch += 1,
            Some(_) => {
                control.signal_group(process.identity.pid, signal)?;
                report.signaled += 1;
            }
        }
    }
    Ok(report)
}

/// Recover command-process ownership after the outer Local runtime is gone.
///
/// A healthy runtime owns TERM -> shared grace -> KILL through `SessionManager`.
/// This is the crash/forced-stop fallback and therefore must preserve the same
/// safety property without trusting reusable numeric PIDs/PGIDs. We only send
/// a process-group signal while its recorded leader still has the exact birth
/// identity. Before TERM we also snapshot member/descendant birth identities,
/// so children that outlive or escape the leader can be signaled individually
/// after that leader exits.
pub fn terminate_matching_stale_processes(
    registry_path: &Path,
    expected_runtime_id: Option<&str>,
    control: &dyn ProcessControl,
) -> Result<StaleProcessCleanupReport> {
    terminate_matching_stale_processes_with_timing(
        registry_path,
        expected_runtime_id,
        control,
        STALE_PROCESS_TERM_GRACE,
        STALE_PROCESS_KILL_GRACE,
        STALE_PROCESS_POLL,
    )
}

fn terminate_matching_stale_processes_with_timing(
    registry_path: &Path,
    expected_runtime_id: Option<&str>,
    control: &dyn ProcessControl,
    term_grace: Duration,
    kill_grace: Duration,
    poll: Duration,
) -> Result<StaleProcessCleanupReport> {
    if !registry_path.exists() {
        return Ok(StaleProcessCleanupReport::default());
    }
    let document = load_document(registry_path)?;
    if let Some(expected_runtime_id) = expected_runtime_id
        && !document.runtime_id.is_empty()
        && document.runtime_id != expected_runtime_id
    {
        bail!(
            "Local owned-process registry belongs to runtime {} but lifecycle state expects {}; refusing cross-runtime stale cleanup",
            document.runtime_id,
            expected_runtime_id
        );
    }
    let mut report = StaleProcessCleanupReport::default();
    let mut matched_leaders = Vec::new();
    let mut discovered = Vec::new();

    for process in document.processes {
        for identity in &process.group_members {
            if identity != &process.identity && !discovered.contains(identity) {
                discovered.push(identity.clone());
            }
        }
        match control.identity(process.identity.pid)? {
            Some(current) if current == process.identity => {
                let mut members = control
                    .process_group_members(process.identity.pid, STALE_PROCESS_DISCOVERY_LIMIT)?;
                let descendants =
                    control.descendants(process.identity.pid, STALE_PROCESS_DISCOVERY_LIMIT)?;
                members.extend(descendants);
                for identity in members {
                    if identity != process.identity && !discovered.contains(&identity) {
                        discovered.push(identity);
                    }
                }
                if !identity_matches(control, &process.identity)? {
                    report.identity_mismatch += 1;
                    continue;
                }
                control.signal_group(process.identity.pid, ProcessSignal::Terminate)?;
                matched_leaders.push(process.identity);
                report.signaled += 1;
            }
            Some(_) => report.identity_mismatch += 1,
            None => {
                let members = control
                    .process_group_members(process.identity.pid, STALE_PROCESS_DISCOVERY_LIMIT)?;
                if members.is_empty() {
                    report.already_gone += 1;
                } else if members
                    .iter()
                    .any(|member| process.group_members.contains(member))
                {
                    // A persisted birth-identity match proves that this is a
                    // continuation of the owned group after its leader exited.
                    // Snapshot every current member, then signal each exact
                    // identity individually so a reused numeric PGID is never
                    // trusted as the cleanup boundary.
                    for identity in members {
                        if identity != process.identity && !discovered.contains(&identity) {
                            discovered.push(identity);
                        }
                    }
                } else {
                    // Legacy registries contain only the leader identity. If
                    // the leader is gone, preserve that ambiguous evidence
                    // rather than adopting an unproven process group.
                    report.unresolved_leaderless_groups += 1;
                }
            }
        }
    }
    discovered.retain(|identity| !matched_leaders.contains(identity));

    for descendant in &discovered {
        if signal_process_if_matching(control, descendant, ProcessSignal::Terminate)? {
            report.descendants_signaled += 1;
        }
    }

    wait_for_stale_processes(control, &matched_leaders, &discovered, term_grace, poll)?;

    for leader in &matched_leaders {
        if identity_matches(control, leader)? {
            control.signal_group(leader.pid, ProcessSignal::Kill)?;
            report.force_killed += 1;
        }
    }
    for descendant in &discovered {
        if signal_process_if_matching(control, descendant, ProcessSignal::Kill)? {
            report.descendants_force_killed += 1;
        }
    }

    wait_for_stale_processes(control, &matched_leaders, &discovered, kill_grace, poll)?;
    for identity in matched_leaders.iter().chain(discovered.iter()) {
        if identity_matches(control, identity)? {
            report.survivors += 1;
        }
    }
    Ok(report)
}

fn wait_for_stale_processes(
    control: &dyn ProcessControl,
    leaders: &[ProcessIdentity],
    descendants: &[ProcessIdentity],
    grace: Duration,
    poll: Duration,
) -> Result<()> {
    let deadline = Instant::now() + grace;
    while Instant::now() < deadline {
        let any_alive = leaders
            .iter()
            .chain(descendants.iter())
            .try_fold(false, |alive, identity| {
                Ok::<_, anyhow::Error>(alive || identity_matches(control, identity)?)
            })?;
        if !any_alive {
            break;
        }
        std::thread::sleep(poll.min(deadline.saturating_duration_since(Instant::now())));
    }
    Ok(())
}

pub fn active_process_record_count(registry_path: &Path) -> Result<usize> {
    if !registry_path.exists() {
        return Ok(0);
    }
    Ok(load_document(registry_path)?.processes.len())
}

fn load_document(path: &Path) -> Result<LocalProcessRegistryDocument> {
    let raw = fs::read(path)
        .with_context(|| format!("failed to read Local process registry {}", path.display()))?;
    let document: LocalProcessRegistryDocument = serde_json::from_slice(&raw)
        .with_context(|| format!("failed to parse Local process registry {}", path.display()))?;
    if document.schema_version != LOCAL_PROCESS_REGISTRY_SCHEMA_VERSION {
        bail!(
            "unsupported Local process registry schema version {} at {}; expected {}",
            document.schema_version,
            path.display(),
            LOCAL_PROCESS_REGISTRY_SCHEMA_VERSION
        );
    }
    Ok(document)
}

fn persist_records(
    path: &Path,
    runtime_id: &str,
    records: &[LocalOwnedProcessRecord],
) -> Result<()> {
    if records.is_empty() {
        if path.exists() {
            fs::remove_file(path).with_context(|| {
                format!(
                    "failed to remove empty Local process registry {}",
                    path.display()
                )
            })?;
        }
        return Ok(());
    }

    let parent = path
        .parent()
        .context("Local process registry path has no parent")?;
    fs::create_dir_all(parent).with_context(|| {
        format!(
            "failed to create Local runtime directory {}",
            parent.display()
        )
    })?;
    super::private_fs::set_user_only_directory(parent)?;

    let document = LocalProcessRegistryDocument {
        schema_version: LOCAL_PROCESS_REGISTRY_SCHEMA_VERSION,
        runtime_id: runtime_id.to_string(),
        processes: records.to_vec(),
    };
    let encoded =
        serde_json::to_vec(&document).context("failed to encode Local process registry")?;
    let temp = parent.join(format!(
        ".owned-processes.{}.{:016x}.tmp",
        std::process::id(),
        rand::random::<u64>()
    ));

    let result = (|| -> Result<()> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let mut file = options
            .open(&temp)
            .with_context(|| format!("failed to create temporary registry {}", temp.display()))?;
        file.write_all(&encoded)
            .with_context(|| format!("failed to write temporary registry {}", temp.display()))?;
        file.sync_all()
            .with_context(|| format!("failed to sync temporary registry {}", temp.display()))?;
        fs::rename(&temp, path).with_context(|| {
            format!(
                "failed to publish Local process registry {} -> {}",
                temp.display(),
                path.display()
            )
        })?;
        super::private_fs::set_user_only_file(path)?;
        Ok(())
    })();

    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use tempfile::tempdir;

    use crate::invocation::InvocationContext;
    use crate::protocol::TerminationReason;
    use crate::session::{
        OwnedProcess, OwnedProcessEnd, OwnedProcessObserver, ProcessBirthIdentity, ProcessControl,
        ProcessIdentity, ProcessInspector, ProcessSignal,
    };

    use super::{
        LocalOwnedProcessRegistry, signal_matching_stale_processes,
        terminate_matching_stale_processes_with_timing,
    };

    fn identity(pid: i32, ticks: u64) -> ProcessIdentity {
        ProcessIdentity {
            pid,
            birth: ProcessBirthIdentity::LinuxProcStartTicks { ticks },
        }
    }

    fn owned_process(id: u64, pid: i32, ticks: u64) -> OwnedProcess {
        OwnedProcess {
            internal_session_id: id,
            session_handle: format!("handle{id}").into(),
            identity: identity(pid, ticks),
            created_by: InvocationContext {
                invocation_id: None,
                correlation_id: Some(format!("invoke-{id}").into()),
                provider: None,
                agent_id: Some("k7m2".into()),
                global_context_pending: false,
                repo_agents_context_pending: false,
                repo_skills_context_pending: false,
            },
        }
    }

    fn exited_end() -> OwnedProcessEnd {
        OwnedProcessEnd::exited(0, TerminationReason::Exit, "/tmp".to_string())
    }

    #[test]
    fn registry_atomically_tracks_start_and_end_without_leaving_empty_state() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("runtime/owned-processes.json");
        let registry = LocalOwnedProcessRegistry::fresh(&path, "runtime-a").unwrap();
        let first = owned_process(1, 1001, 11);
        let second = owned_process(2, 1002, 22);

        registry.process_started(&first).unwrap();
        registry.process_started(&second).unwrap();
        let first_member = identity(1101, 33);
        registry
            .process_group_members_updated(&first, std::slice::from_ref(&first_member))
            .unwrap();
        assert_eq!(registry.snapshot().len(), 2);
        assert!(path.exists());
        let raw: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(raw["schema_version"], 1);
        assert_eq!(raw["runtime_id"], "runtime-a");
        assert_eq!(raw["processes"].as_array().unwrap().len(), 2);
        assert_eq!(registry.snapshot()[0].group_members, vec![first_member]);

        registry.process_ended(&first, &exited_end()).unwrap();
        assert_eq!(registry.snapshot().len(), 1);
        registry.process_ended(&second, &exited_end()).unwrap();
        assert!(registry.snapshot().is_empty());
        assert!(!path.exists(), "empty ephemeral registry should be removed");
    }

    struct FakeControl {
        identities: HashMap<i32, ProcessIdentity>,
        signaled: Mutex<Vec<(i32, ProcessSignal)>>,
    }

    impl ProcessInspector for FakeControl {
        fn identity(&self, pid: i32) -> anyhow::Result<Option<ProcessIdentity>> {
            Ok(self.identities.get(&pid).cloned())
        }

        fn live_cwd(&self, _pid: i32) -> Option<String> {
            None
        }
    }

    impl ProcessControl for FakeControl {
        fn signal_group(&self, pid: i32, signal: ProcessSignal) -> anyhow::Result<()> {
            self.signaled.lock().unwrap().push((pid, signal));
            Ok(())
        }
    }

    #[test]
    fn stale_cleanup_signals_only_matching_birth_identity() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("runtime/owned-processes.json");
        let registry = LocalOwnedProcessRegistry::fresh(&path, "runtime-a").unwrap();
        let matching = owned_process(1, 2001, 101);
        let reused_pid = owned_process(2, 2002, 202);
        let gone = owned_process(3, 2003, 303);
        registry.process_started(&matching).unwrap();
        registry.process_started(&reused_pid).unwrap();
        registry.process_started(&gone).unwrap();

        let control = FakeControl {
            identities: HashMap::from([(2001, identity(2001, 101)), (2002, identity(2002, 999))]),
            signaled: Mutex::new(Vec::new()),
        };
        let report =
            signal_matching_stale_processes(&path, &control, ProcessSignal::Terminate).unwrap();

        assert_eq!(report.signaled, 1);
        assert_eq!(report.identity_mismatch, 1);
        assert_eq!(report.already_gone, 1);
        assert_eq!(
            *control.signaled.lock().unwrap(),
            vec![(2001, ProcessSignal::Terminate)]
        );
    }

    #[test]
    fn stale_cleanup_refuses_cross_runtime_registry_before_signaling() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("runtime/owned-processes.json");
        let registry = LocalOwnedProcessRegistry::fresh(&path, "runtime-a").unwrap();
        registry
            .process_started(&owned_process(1, 2001, 101))
            .unwrap();
        let control = FakeControl {
            identities: HashMap::from([(2001, identity(2001, 101))]),
            signaled: Mutex::new(Vec::new()),
        };

        let error = terminate_matching_stale_processes_with_timing(
            &path,
            Some("runtime-b"),
            &control,
            Duration::from_millis(1),
            Duration::from_millis(1),
            Duration::from_millis(1),
        )
        .unwrap_err();

        assert!(error.to_string().contains("cross-runtime stale cleanup"));
        assert!(control.signaled.lock().unwrap().is_empty());
        assert!(
            path.exists(),
            "mismatched runtime evidence must be preserved"
        );
    }

    struct VanishingLeaderControl {
        identity: ProcessIdentity,
        identity_calls: AtomicUsize,
        signaled: Mutex<Vec<(i32, ProcessSignal)>>,
    }

    impl ProcessInspector for VanishingLeaderControl {
        fn identity(&self, pid: i32) -> anyhow::Result<Option<ProcessIdentity>> {
            if pid != self.identity.pid {
                return Ok(None);
            }
            Ok((self.identity_calls.fetch_add(1, Ordering::SeqCst) == 0)
                .then(|| self.identity.clone()))
        }

        fn live_cwd(&self, _pid: i32) -> Option<String> {
            None
        }
    }

    impl ProcessControl for VanishingLeaderControl {
        fn signal_group(&self, pid: i32, signal: ProcessSignal) -> anyhow::Result<()> {
            self.signaled.lock().unwrap().push((pid, signal));
            Ok(())
        }
    }

    #[test]
    fn stale_cleanup_revalidates_leader_after_discovery_before_group_signal() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("runtime/owned-processes.json");
        let leader = identity(2101, 101);
        let registry = LocalOwnedProcessRegistry::fresh(&path, "runtime-a").unwrap();
        registry
            .process_started(&owned_process(1, leader.pid, 101))
            .unwrap();
        let control = VanishingLeaderControl {
            identity: leader,
            identity_calls: AtomicUsize::new(0),
            signaled: Mutex::new(Vec::new()),
        };

        let report = terminate_matching_stale_processes_with_timing(
            &path,
            Some("runtime-a"),
            &control,
            Duration::from_millis(1),
            Duration::from_millis(1),
            Duration::from_millis(1),
        )
        .unwrap();

        assert_eq!(report.identity_mismatch, 1);
        assert_eq!(report.signaled, 0);
        assert!(control.signaled.lock().unwrap().is_empty());
        assert!(
            path.exists(),
            "changed ownership evidence must be preserved"
        );
    }

    struct LeaderlessGroupControl {
        group_member: ProcessIdentity,
        signaled: Mutex<Vec<(i32, ProcessSignal)>>,
    }

    impl ProcessInspector for LeaderlessGroupControl {
        fn identity(&self, pid: i32) -> anyhow::Result<Option<ProcessIdentity>> {
            Ok((pid == self.group_member.pid).then(|| self.group_member.clone()))
        }

        fn live_cwd(&self, _pid: i32) -> Option<String> {
            None
        }

        fn process_group_members(
            &self,
            pgid: i32,
            _limit: usize,
        ) -> anyhow::Result<Vec<ProcessIdentity>> {
            Ok(if pgid == 3001 {
                vec![self.group_member.clone()]
            } else {
                Vec::new()
            })
        }
    }

    impl ProcessControl for LeaderlessGroupControl {
        fn signal_group(&self, pid: i32, signal: ProcessSignal) -> anyhow::Result<()> {
            self.signaled.lock().unwrap().push((pid, signal));
            Ok(())
        }
    }

    #[test]
    fn stale_cleanup_preserves_leaderless_group_when_ownership_cannot_be_proven() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("runtime/owned-processes.json");
        let registry = LocalOwnedProcessRegistry::fresh(&path, "runtime-a").unwrap();
        registry
            .process_started(&owned_process(1, 3001, 111))
            .unwrap();
        let control = LeaderlessGroupControl {
            group_member: identity(3002, 222),
            signaled: Mutex::new(Vec::new()),
        };

        let report = terminate_matching_stale_processes_with_timing(
            &path,
            Some("runtime-a"),
            &control,
            Duration::from_millis(1),
            Duration::from_millis(1),
            Duration::from_millis(1),
        )
        .unwrap();

        assert_eq!(report.unresolved_leaderless_groups, 1);
        assert_eq!(report.signaled, 0);
        assert_eq!(report.force_killed, 0);
        assert_eq!(report.survivors, 0);
        assert!(control.signaled.lock().unwrap().is_empty());
        assert!(
            path.exists(),
            "ambiguous ownership evidence must be preserved"
        );
    }

    struct PersistedLeaderlessGroupControl {
        group_member: ProcessIdentity,
        member_identity_calls: AtomicUsize,
    }

    impl ProcessInspector for PersistedLeaderlessGroupControl {
        fn identity(&self, pid: i32) -> anyhow::Result<Option<ProcessIdentity>> {
            if pid != self.group_member.pid {
                return Ok(None);
            }
            Ok(
                (self.member_identity_calls.fetch_add(1, Ordering::SeqCst) == 0)
                    .then(|| self.group_member.clone()),
            )
        }

        fn live_cwd(&self, _pid: i32) -> Option<String> {
            None
        }

        fn process_group_members(
            &self,
            pgid: i32,
            _limit: usize,
        ) -> anyhow::Result<Vec<ProcessIdentity>> {
            Ok(if pgid == 3101 {
                vec![self.group_member.clone()]
            } else {
                Vec::new()
            })
        }
    }

    impl ProcessControl for PersistedLeaderlessGroupControl {
        fn signal_group(&self, _pid: i32, _signal: ProcessSignal) -> anyhow::Result<()> {
            panic!("leaderless cleanup must signal persisted identities individually")
        }
    }

    #[test]
    fn stale_cleanup_resolves_leaderless_group_from_persisted_member_identity() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("runtime/owned-processes.json");
        let leader = owned_process(1, 3101, 111);
        let member = identity(3102, 222);
        let registry = LocalOwnedProcessRegistry::fresh(&path, "runtime-a").unwrap();
        registry.process_started(&leader).unwrap();
        registry
            .process_group_members_updated(&leader, std::slice::from_ref(&member))
            .unwrap();
        let control = PersistedLeaderlessGroupControl {
            group_member: member,
            member_identity_calls: AtomicUsize::new(0),
        };

        let report = terminate_matching_stale_processes_with_timing(
            &path,
            Some("runtime-a"),
            &control,
            Duration::from_millis(1),
            Duration::from_millis(1),
            Duration::from_millis(1),
        )
        .unwrap();

        assert_eq!(report.unresolved_leaderless_groups, 0);
        assert_eq!(report.descendants_signaled, 1);
        assert_eq!(report.survivors, 0);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn stale_cleanup_escalates_term_ignoring_owned_group_before_registry_can_be_discarded() {
        use std::io::{BufRead as _, BufReader};
        use std::os::unix::process::{CommandExt as _, ExitStatusExt as _};
        use std::process::{Command, Stdio};

        struct KillGroupOnDrop(i32);
        impl Drop for KillGroupOnDrop {
            fn drop(&mut self) {
                let _ = nix::sys::signal::killpg(
                    nix::unistd::Pid::from_raw(self.0),
                    nix::sys::signal::Signal::SIGKILL,
                );
            }
        }

        let mut child = Command::new("/bin/sh")
            .arg("-c")
            .arg("trap '' TERM; echo ready; exec /bin/sleep 60")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .unwrap();
        let pid = i32::try_from(child.id()).unwrap();
        let _cleanup = KillGroupOnDrop(pid);
        let mut ready = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut ready)
            .unwrap();
        assert_eq!(ready.trim(), "ready", "TERM trap must be installed first");
        let reaper = std::thread::spawn(move || child.wait().unwrap());
        let inspector = crate::session::SystemProcessInspector;
        let process_identity = inspector
            .identity(pid)
            .unwrap()
            .expect("TERM-ignoring stale process should be alive");
        let dir = tempdir().unwrap();
        let path = dir.path().join("runtime/owned-processes.json");
        let registry = LocalOwnedProcessRegistry::fresh(&path, "runtime-a").unwrap();
        registry
            .process_started(&OwnedProcess {
                internal_session_id: 1,
                session_handle: "stale001".into(),
                identity: process_identity.clone(),
                created_by: InvocationContext::default(),
            })
            .unwrap();

        let report = terminate_matching_stale_processes_with_timing(
            &path,
            Some("runtime-a"),
            &inspector,
            Duration::from_millis(100),
            Duration::from_secs(2),
            Duration::from_millis(20),
        )
        .unwrap();

        assert_eq!(report.signaled, 1);
        assert_eq!(report.force_killed, 1);
        assert_eq!(report.identity_mismatch, 0);
        assert_eq!(report.unresolved_leaderless_groups, 0);
        assert_eq!(report.survivors, 0);
        assert!(!crate::session::identity_matches(&inspector, &process_identity).unwrap());
        assert!(
            path.exists(),
            "the lifecycle owner removes registry state only after success"
        );
        assert_eq!(reaper.join().unwrap().signal(), Some(9));
    }

    #[test]
    fn fresh_registry_refuses_to_overwrite_unresolved_ownership() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("runtime/owned-processes.json");
        let registry = LocalOwnedProcessRegistry::fresh(&path, "runtime-a").unwrap();
        registry.process_started(&owned_process(1, 42, 1)).unwrap();
        let error = LocalOwnedProcessRegistry::fresh(&path, "runtime-b")
            .err()
            .expect("unresolved registry should block fresh runtime");
        assert!(error.to_string().contains("stale recovery"));
    }
}
