mod config;
mod environment_handoff;
mod file_evidence;
mod history;
mod launchd;
mod lifecycle;
mod lifecycle_artifacts;
mod lifecycle_context;
mod lifecycle_lock;
mod lifecycle_start;
#[cfg(all(test, unix))]
mod lifecycle_tests;
mod liveboard;
mod mcp_context;
mod observability;
mod observer_client;
mod parse;
mod paths;
mod presentation;
mod private_fs;
mod process_registry;
mod runtime;
mod secret;
mod setup;
mod status;
mod tunnel;
mod tunnel_provider;
mod tunnel_release;
mod watch;

#[cfg(test)]
mod setup_tests;

pub use config::{
    LocalConfig, LocalContextConfig, LocalContextSkillsConfig, LocalHistoryConfig,
    LocalTunnelConfig, ManagedTunnelClientRelease,
};
pub use environment_handoff::{consume_environment_handoff, write_environment_handoff};
pub use history::{
    HISTORY_SCHEMA_VERSION, HistoryAgentSummary, HistoryAgentWorkdir, HistoryFileEvidence,
    HistoryFormat, HistoryInvocation, HistoryQuery, HistoryStoreStatus, LocalHistoryReader,
    LocalHistoryRuntime, LocalHistoryRuntimeConfig, clear_local_history,
};
pub use launchd::{
    LOCAL_LAUNCHD_LABEL, LaunchdController, LocalLaunchdJob, SystemLaunchdController,
};
#[cfg(target_os = "linux")]
pub use lifecycle::stop_via_linux_process;
#[cfg(target_os = "windows")]
pub use lifecycle::stop_via_windows_process;
pub use lifecycle::{
    LOCAL_RUNTIME_BOOTSTRAP_SCHEMA_VERSION, LocalRuntimeBootstrap, LocalStopOutcome,
    PreparedLocalLaunch, cleanup_stale_runtime, load_runtime_bootstrap, prepare_local_launch,
    probe_local_mcp, run_hidden_runtime, stop_via_launchd, wait_for_runtime_ready,
};
pub use lifecycle_context::{
    paths_from_runtime_bootstrap, resolve_developer_shell, validate_runtime_start_directory,
};
#[cfg(target_os = "linux")]
pub use lifecycle_start::start_via_linux_process;
#[cfg(target_os = "windows")]
pub use lifecycle_start::start_via_windows_process;
pub use lifecycle_start::{LocalStartOutcome, start_via_launchd};
pub use liveboard::{run_local_liveboard, run_local_liveboard_without_open};
pub use observability::{
    LOCAL_OBSERVABILITY_API_VERSION, LocalObservabilityServer, start_local_observability_server,
};
pub use parse::{HumanDuration, StorageSize, parse_human_duration, parse_storage_size};
pub use paths::LocalPaths;
pub use presentation::{
    PRESENTATION_SCHEMA_VERSION, PresentationAgent, PresentationDiffLine, PresentationDocument,
    PresentationEvidence, PresentationFileChange, PresentationFileOperation, PresentationKind,
    PresentationPollSummary, PresentationRecord, PresentationWorkdir, PresentationWriteMode,
    build_presentation, render_presentation,
};
pub use process_registry::{
    LOCAL_PROCESS_REGISTRY_SCHEMA_VERSION, LocalOwnedProcessRecord, LocalOwnedProcessRegistry,
    LocalProcessRegistryDocument, StaleProcessCleanupReport, active_process_record_count,
    terminate_matching_stale_processes,
};
pub use runtime::{LocalHostRuntime, LocalHostRuntimeOptions, start_local_host_runtime};
#[cfg(target_os = "linux")]
pub use secret::LinuxFileRuntimeKeyStore;
#[cfg(target_os = "macos")]
pub use secret::MacKeychainRuntimeKeyStore;
#[cfg(target_os = "windows")]
pub use secret::WindowsCredentialRuntimeKeyStore;
pub use secret::{RuntimeKey, RuntimeKeyStore};
pub use setup::{
    LocalSetupRequest, LocalSetupResult, LocalSetupService, ensure_observability_bearer,
};
pub use status::{
    LOCAL_DISCOVERY_SCHEMA_VERSION, LOCAL_RUNTIME_STATE_SCHEMA_VERSION,
    LOCAL_STATUS_SCHEMA_VERSION, LocalObservabilityDiscovery, LocalRuntimeDiscovery,
    LocalRuntimeHealth, LocalRuntimeLifecycle, LocalRuntimeState, LocalStatusDocument,
    LocalStatusState, ensure_offline_mutation, load_runtime_discovery, load_runtime_state,
    write_runtime_discovery, write_runtime_state,
};
pub use tunnel::{
    LOCAL_TUNNEL_PROCESS_STATE_SCHEMA_VERSION, LOCAL_TUNNEL_PROFILE_MAX_CONCURRENT_REQUESTS,
    LocalTunnelProfile, ManagedTunnelChild, StaleTunnelCleanup, TunnelHealthEvidence,
    TunnelProcessState, cleanup_stale_tunnel_child, load_tunnel_process_state, probe_tunnel_health,
    spawn_tunnel_client, write_mcp_token, write_tunnel_process_state, write_tunnel_profile,
};
#[cfg(target_os = "linux")]
pub use tunnel_provider::LinuxZipArchiveExtractor;
#[cfg(target_os = "macos")]
pub use tunnel_provider::MacDittoArchiveExtractor;
#[cfg(target_os = "windows")]
pub use tunnel_provider::WindowsTarArchiveExtractor;
pub use tunnel_provider::{
    ArchiveExtractor, ProcessTunnelMetadataValidator, TunnelMetadataValidator,
};
pub use tunnel_release::{
    OfficialTunnelReleaseClient, ResolvedTunnelRelease, TunnelArchitecture, sha256_hex,
    validate_tunnel_id,
};
pub use watch::{WatchOptions, run_local_watch};
