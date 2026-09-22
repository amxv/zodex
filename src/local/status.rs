use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::session::{ProcessIdentity, ProcessInspector, SystemProcessInspector, identity_matches};

use super::{
    LOCAL_OBSERVABILITY_API_VERSION, LocalConfig, LocalHistoryReader, LocalPaths,
    PRESENTATION_SCHEMA_VERSION, active_process_record_count,
};

pub const LOCAL_STATUS_SCHEMA_VERSION: u32 = 3;
pub const LOCAL_DISCOVERY_SCHEMA_VERSION: u32 = 1;
pub const LOCAL_RUNTIME_STATE_SCHEMA_VERSION: u32 = 2;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LocalRuntimeState {
    pub schema_version: u32,
    pub runtime_id: String,
    pub lifecycle: LocalRuntimeLifecycle,
    pub process: Option<ProcessIdentity>,
    pub start_directory: Option<PathBuf>,
    pub started_at: Option<String>,
    pub expires_at: Option<String>,
    pub health: LocalRuntimeHealth,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct LocalRuntimeHealth {
    pub mcp_ready: bool,
    pub observability_ready: bool,
    pub tunnel_process_running: bool,
    pub tunnel_control_plane_ready: bool,
    pub tunnel_ready: bool,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LocalRuntimeLifecycle {
    Starting,
    Ready,
    Stopping,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LocalRuntimeDiscovery {
    pub schema_version: u32,
    pub runtime_id: String,
    pub pid: u32,
    pub start_directory: PathBuf,
    pub started_at: String,
    pub expires_at: Option<String>,
    pub observability: LocalObservabilityDiscovery,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LocalObservabilityDiscovery {
    pub api_version: u32,
    pub presentation_version: u32,
    pub base_url: String,
    pub bearer_token_path: PathBuf,
    pub history_available: bool,
    pub sse_available: bool,
}

impl LocalObservabilityDiscovery {
    pub fn active(base_url: impl Into<String>, bearer_token_path: impl Into<PathBuf>) -> Self {
        Self {
            api_version: LOCAL_OBSERVABILITY_API_VERSION,
            presentation_version: PRESENTATION_SCHEMA_VERSION,
            base_url: base_url.into(),
            bearer_token_path: bearer_token_path.into(),
            history_available: true,
            sse_available: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct LocalStatusDocument {
    pub schema_version: u32,
    pub configured: bool,
    pub state: LocalStatusState,
    pub config_path: PathBuf,
    pub runtime_state_path: PathBuf,
    pub discovery_path: PathBuf,
    pub runtime: Option<LocalRuntimeState>,
    pub discovery: Option<LocalRuntimeDiscovery>,
    pub current_runtime_agent_count: usize,
    pub active_process_count: usize,
    pub history: LocalHistoryStatus,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LocalStatusState {
    Unconfigured,
    Stopped,
    Running,
    Stale,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct LocalHistoryStatus {
    pub database_path: PathBuf,
    pub database_exists: bool,
    pub physical_size_bytes: u64,
    pub store_state: String,
    pub store_reason: Option<String>,
    pub over_budget: bool,
    pub last_retention_error: Option<String>,
    pub max_age: String,
    pub max_age_seconds: u64,
    pub max_size: String,
    pub max_size_bytes: u64,
}

impl LocalStatusDocument {
    pub fn inspect(paths: &LocalPaths) -> Result<Self> {
        Self::inspect_with_process_inspector(paths, &SystemProcessInspector)
    }

    pub fn inspect_with_process_inspector(
        paths: &LocalPaths,
        inspector: &dyn ProcessInspector,
    ) -> Result<Self> {
        let config_path = paths.config_file();
        let config_exists = config_path.exists();
        let config = LocalConfig::load(&config_path)?;
        let runtime_state_path = paths.runtime_state_file();
        let discovery_path = paths.discovery_file();
        let runtime = load_runtime_state(paths)?;
        let discovery = load_runtime_discovery(paths)?;
        let history_store = LocalHistoryReader::status(&paths.history_database())?;

        let runtime_live = match runtime.as_ref().and_then(|state| state.process.as_ref()) {
            Some(expected) => identity_matches(inspector, expected)?,
            None => false,
        };
        let discovery_consistent = match (&runtime, &discovery) {
            (Some(runtime), Some(discovery)) => runtime.runtime_id == discovery.runtime_id,
            _ => true,
        };
        let markers_present = runtime.is_some() || discovery.is_some();
        let state = if runtime_live && discovery_consistent {
            LocalStatusState::Running
        } else if markers_present {
            LocalStatusState::Stale
        } else if !config_exists || !config.is_provider_configured() {
            LocalStatusState::Unconfigured
        } else {
            LocalStatusState::Stopped
        };

        let (current_runtime_agent_count, active_process_count) =
            if state == LocalStatusState::Running {
                let runtime_id = runtime
                    .as_ref()
                    .map(|runtime| runtime.runtime_id.as_str())
                    .context("running Local status is missing runtime identity")?;
                (
                    LocalHistoryReader::agent_count(&paths.history_database(), Some(runtime_id))?,
                    active_process_record_count(&paths.owned_process_registry_file())?,
                )
            } else {
                (0, 0)
            };

        Ok(Self {
            schema_version: LOCAL_STATUS_SCHEMA_VERSION,
            configured: config.is_provider_configured(),
            state,
            config_path,
            runtime_state_path,
            discovery_path,
            runtime,
            discovery,
            current_runtime_agent_count,
            active_process_count,
            history: LocalHistoryStatus {
                database_path: paths.history_database(),
                database_exists: history_store.database_exists,
                physical_size_bytes: history_store.physical_size_bytes,
                store_state: history_store.health_state,
                store_reason: history_store.health_reason,
                over_budget: history_store.over_budget
                    || history_store.physical_size_bytes > config.history.max_size.bytes(),
                last_retention_error: history_store.last_retention_error,
                max_age: config.history.max_age.to_string(),
                max_age_seconds: config.history.max_age.seconds(),
                max_size: config.history.max_size.to_string(),
                max_size_bytes: config.history.max_size.bytes(),
            },
        })
    }
}

pub fn load_runtime_state(paths: &LocalPaths) -> Result<Option<LocalRuntimeState>> {
    load_versioned_json(
        &paths.runtime_state_file(),
        LOCAL_RUNTIME_STATE_SCHEMA_VERSION,
        "runtime state",
        |state: &LocalRuntimeState| state.schema_version,
    )
}

pub fn write_runtime_state(paths: &LocalPaths, state: &LocalRuntimeState) -> Result<()> {
    if state.schema_version != LOCAL_RUNTIME_STATE_SCHEMA_VERSION {
        bail!(
            "refusing to write Local runtime state schema version {}; expected {}",
            state.schema_version,
            LOCAL_RUNTIME_STATE_SCHEMA_VERSION
        );
    }
    write_user_only_json_atomic(&paths.runtime_state_file(), state)
}

pub fn write_runtime_discovery(
    paths: &LocalPaths,
    discovery: &LocalRuntimeDiscovery,
) -> Result<()> {
    if discovery.schema_version != LOCAL_DISCOVERY_SCHEMA_VERSION {
        bail!(
            "refusing to write Local discovery schema version {}; expected {}",
            discovery.schema_version,
            LOCAL_DISCOVERY_SCHEMA_VERSION
        );
    }
    write_user_only_json_atomic(&paths.discovery_file(), discovery)
}

pub fn ensure_offline_mutation(paths: &LocalPaths, operation: &str) -> Result<()> {
    for marker in [paths.runtime_state_file(), paths.discovery_file()] {
        if marker.exists() {
            bail!(
                "cannot {operation} while Zodex Local runtime state is present at {}; run `zodex local stop` first",
                marker.display()
            );
        }
    }
    Ok(())
}

pub fn load_runtime_discovery(paths: &LocalPaths) -> Result<Option<LocalRuntimeDiscovery>> {
    load_versioned_json(
        &paths.discovery_file(),
        LOCAL_DISCOVERY_SCHEMA_VERSION,
        "discovery",
        |discovery: &LocalRuntimeDiscovery| discovery.schema_version,
    )
}

fn load_versioned_json<T>(
    path: &Path,
    expected_schema: u32,
    label: &str,
    schema: impl FnOnce(&T) -> u32,
) -> Result<Option<T>>
where
    T: for<'de> Deserialize<'de>,
{
    if !path.exists() {
        return Ok(None);
    }
    let raw = fs::read(path)
        .with_context(|| format!("failed to read Local {label} at {}", path.display()))?;
    let value: T = serde_json::from_slice(&raw)
        .with_context(|| format!("failed to parse Local {label} at {}", path.display()))?;
    let actual = schema(&value);
    if actual != expected_schema {
        bail!(
            "unsupported Local {label} schema version {actual} at {}; expected {expected_schema}",
            path.display()
        );
    }
    Ok(Some(value))
}

pub(super) fn write_user_only_json_atomic(path: &Path, value: &impl Serialize) -> Result<()> {
    let parent = path
        .parent()
        .context("Local runtime JSON path has no parent")?;
    fs::create_dir_all(parent).with_context(|| {
        format!(
            "failed to create Local runtime directory {}",
            parent.display()
        )
    })?;
    super::private_fs::set_user_only_directory(parent)?;
    let encoded =
        serde_json::to_vec_pretty(value).context("failed to encode Local runtime JSON")?;
    let temp = parent.join(format!(
        ".{}.{}.{:016x}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("state"),
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
        let mut file = options.open(&temp).with_context(|| {
            format!("failed to create temporary Local state {}", temp.display())
        })?;
        file.write_all(&encoded)
            .with_context(|| format!("failed to write temporary Local state {}", temp.display()))?;
        file.sync_all()
            .with_context(|| format!("failed to sync temporary Local state {}", temp.display()))?;
        fs::rename(&temp, path)
            .with_context(|| format!("failed to publish Local state {}", path.display()))?;
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
    use tempfile::tempdir;

    use super::{
        LOCAL_DISCOVERY_SCHEMA_VERSION, LOCAL_RUNTIME_STATE_SCHEMA_VERSION,
        LocalObservabilityDiscovery, LocalRuntimeDiscovery, LocalRuntimeLifecycle,
        LocalRuntimeState, LocalStatusDocument, LocalStatusState, ensure_offline_mutation,
    };
    use crate::local::{LocalConfig, LocalPaths, ManagedTunnelClientRelease};
    use crate::session::{ProcessInspector, SystemProcessInspector};

    fn test_paths() -> (tempfile::TempDir, LocalPaths) {
        let dir = tempdir().unwrap();
        let paths = LocalPaths::from_roots(
            dir.path().join("config"),
            dir.path().join("data"),
            dir.path().join("state"),
        )
        .unwrap();
        (dir, paths)
    }

    #[test]
    fn first_run_status_is_versioned_and_unconfigured() {
        let (_dir, paths) = test_paths();
        let status = LocalStatusDocument::inspect(&paths).unwrap();
        assert_eq!(status.schema_version, 3);
        assert!(!status.configured);
        assert_eq!(status.state, LocalStatusState::Unconfigured);
        assert_eq!(status.history.max_age_seconds, 60 * 24 * 60 * 60);
        assert_eq!(status.history.max_size_bytes, 500 * 1024 * 1024);
        assert!(status.discovery.is_none());
        assert!(!status.discovery_path.exists());
    }

    #[test]
    fn status_serialization_never_exposes_private_liveboard_discovery() {
        let (_dir, paths) = test_paths();
        paths.ensure_persistent_dirs().unwrap();
        std::fs::create_dir_all(paths.runtime_dir()).unwrap();
        let capability = "supersecretliveboardcapability012345";
        std::fs::write(
            paths.liveboard_discovery_file(),
            format!(
                "{{\"schema_version\":1,\"runtime_id\":\"runtime-a\",\"base_url\":\"http://127.0.0.1:43123/{capability}/\"}}"
            ),
        )
        .unwrap();

        let json = serde_json::to_string(&LocalStatusDocument::inspect(&paths).unwrap()).unwrap();
        assert!(!json.contains(capability));
        assert!(!json.contains("liveboard.json"));
        assert!(!json.contains("43123"));
    }

    #[test]
    fn offline_mutation_guard_rejects_active_runtime_marker() {
        let (_dir, paths) = test_paths();
        std::fs::create_dir_all(paths.runtime_dir()).unwrap();
        let state = LocalRuntimeState {
            schema_version: LOCAL_RUNTIME_STATE_SCHEMA_VERSION,
            runtime_id: "runtime-test".to_string(),
            lifecycle: LocalRuntimeLifecycle::Ready,
            process: None,
            start_directory: None,
            started_at: None,
            expires_at: None,
            health: Default::default(),
        };
        std::fs::write(
            paths.runtime_state_file(),
            serde_json::to_vec(&state).unwrap(),
        )
        .unwrap();

        let error = ensure_offline_mutation(&paths, "change Local configuration").unwrap_err();
        assert!(error.to_string().contains("zodex local stop"));
    }

    #[test]
    fn discovery_schema_is_stable_and_status_treats_unverified_marker_as_stale() {
        let (_dir, paths) = test_paths();
        std::fs::create_dir_all(paths.runtime_dir()).unwrap();
        let discovery = LocalRuntimeDiscovery {
            schema_version: LOCAL_DISCOVERY_SCHEMA_VERSION,
            runtime_id: "runtime-test".to_string(),
            pid: 1234,
            start_directory: "/tmp/repo".into(),
            started_at: "2026-08-15T00:00:00Z".to_string(),
            expires_at: None,
            observability: LocalObservabilityDiscovery::active(
                "http://127.0.0.1:12345",
                paths.observability_bearer_file(),
            ),
        };
        std::fs::write(
            paths.discovery_file(),
            serde_json::to_vec(&discovery).unwrap(),
        )
        .unwrap();

        let status = LocalStatusDocument::inspect(&paths).unwrap();
        assert_eq!(status.state, LocalStatusState::Stale);
        assert_eq!(status.discovery, Some(discovery));
    }

    #[test]
    fn provider_fields_are_required_before_status_calls_configured() {
        let (_dir, paths) = test_paths();
        let mut config = LocalConfig::default();
        config.set("history.max-age", "30d").unwrap();
        config.save(&paths.config_file()).unwrap();
        assert!(!LocalStatusDocument::inspect(&paths).unwrap().configured);

        config
            .set("tunnel.id", "tunnel_0123456789abcdef0123456789abcdef")
            .unwrap();
        config.tunnel.client_path = Some(paths.managed_tunnel_client());
        config.save(&paths.config_file()).unwrap();
        assert!(!LocalStatusDocument::inspect(&paths).unwrap().configured);

        config.tunnel.release = Some(ManagedTunnelClientRelease {
            version: "v0.0.11".to_string(),
            asset_name: "tunnel-client-v0.0.11-darwin-arm64.zip".to_string(),
            archive_sha256: "a".repeat(64),
            binary_sha256: "b".repeat(64),
            cloudflared_sha256: "c".repeat(64),
            cloudflared_manifest_sha256: "d".repeat(64),
            source_url: "https://example.invalid/archive.zip".to_string(),
        });
        config.save(&paths.config_file()).unwrap();
        let status = LocalStatusDocument::inspect(&paths).unwrap();
        assert!(status.configured);
        assert_eq!(status.state, LocalStatusState::Stopped);
    }

    #[test]
    fn running_status_counts_persisted_active_process_records_without_trusting_discovery_alone() {
        let (_dir, paths) = test_paths();
        let inspector = SystemProcessInspector;
        let process = inspector
            .identity(std::process::id() as i32)
            .unwrap()
            .unwrap();
        let state = LocalRuntimeState {
            schema_version: LOCAL_RUNTIME_STATE_SCHEMA_VERSION,
            runtime_id: "runtime-counts".to_string(),
            lifecycle: LocalRuntimeLifecycle::Ready,
            process: Some(process.clone()),
            start_directory: Some("/tmp".into()),
            started_at: Some("2026-08-16T00:00:00Z".to_string()),
            expires_at: None,
            health: super::LocalRuntimeHealth {
                mcp_ready: true,
                observability_ready: true,
                tunnel_process_running: true,
                tunnel_control_plane_ready: true,
                tunnel_ready: true,
                last_error: None,
            },
        };
        super::write_runtime_state(&paths, &state).unwrap();
        super::write_runtime_discovery(
            &paths,
            &LocalRuntimeDiscovery {
                schema_version: LOCAL_DISCOVERY_SCHEMA_VERSION,
                runtime_id: state.runtime_id.clone(),
                pid: std::process::id(),
                start_directory: "/tmp".into(),
                started_at: state.started_at.clone().unwrap(),
                expires_at: None,
                observability: LocalObservabilityDiscovery::active(
                    "http://127.0.0.1:41000",
                    paths.observability_bearer_file(),
                ),
            },
        )
        .unwrap();
        std::fs::write(
            paths.owned_process_registry_file(),
            serde_json::to_vec(&crate::local::LocalProcessRegistryDocument {
                schema_version: crate::local::LOCAL_PROCESS_REGISTRY_SCHEMA_VERSION,
                runtime_id: state.runtime_id.clone(),
                processes: vec![crate::local::LocalOwnedProcessRecord {
                    internal_session_id: 1,
                    session_handle: "fixture".to_string(),
                    identity: process,
                    group_members: Vec::new(),
                    created_by_agent_id: None,
                    invocation_correlation_id: None,
                }],
            })
            .unwrap(),
        )
        .unwrap();

        let status = LocalStatusDocument::inspect(&paths).unwrap();
        assert_eq!(status.state, LocalStatusState::Running);
        assert_eq!(status.current_runtime_agent_count, 0);
        assert_eq!(status.active_process_count, 1);
    }
}
