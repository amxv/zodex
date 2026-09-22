use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

use crate::server::LOCAL_MCP_TOKEN_HEADER;
use crate::session::{
    ProcessIdentity, ProcessInspector, ProcessSignal, SystemProcessInspector,
    signal_process_if_matching,
};

use super::LocalPaths;
use super::secret::RuntimeKey;
use super::tunnel_provider::provider_environment;

pub const LOCAL_TUNNEL_PROFILE_MAX_CONCURRENT_REQUESTS: usize = 10;
pub const LOCAL_TUNNEL_PROCESS_STATE_SCHEMA_VERSION: u32 = 1;
const TUNNEL_STOP_GRACE: Duration = Duration::from_secs(5);
const STALE_TUNNEL_STOP_GRACE: Duration = Duration::from_secs(3);
const STALE_TUNNEL_KILL_GRACE: Duration = Duration::from_secs(2);
const STALE_TUNNEL_POLL: Duration = Duration::from_millis(50);
const DIAGNOSTIC_LOG_MAX_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct LocalTunnelProfile {
    pub tunnel_id: String,
    pub mcp_url: String,
    pub mcp_token_path: PathBuf,
    pub health_url_file: PathBuf,
}

impl LocalTunnelProfile {
    pub fn render(&self) -> Result<String> {
        if !self.mcp_url.starts_with("http://127.0.0.1:") || !self.mcp_url.ends_with("/mcp") {
            bail!("Local tunnel MCP target must be the Zodex loopback /mcp endpoint");
        }
        if !self.mcp_token_path.is_absolute() || !self.health_url_file.is_absolute() {
            bail!("Local tunnel secret and health paths must be absolute");
        }
        if self.tunnel_id.trim().is_empty() {
            bail!("Local tunnel id must not be empty");
        }

        let tunnel_id = yaml_string(&self.tunnel_id)?;
        let mcp_url = yaml_string(&self.mcp_url)?;
        let health_url_file = yaml_string(&self.health_url_file.display().to_string())?;
        let token_ref = yaml_string(&format!("file:{}", self.mcp_token_path.display()))?;
        Ok(format!(
            concat!(
                "config_version: 1\n",
                "control_plane:\n",
                "  tunnel_id: {tunnel_id}\n",
                "  api_key: env:CONTROL_PLANE_API_KEY\n",
                "log:\n",
                "  level: info\n",
                "  format: json\n",
                "health:\n",
                "  listen_addr: 127.0.0.1:0\n",
                "  url_file: {health_url_file}\n",
                "mcp:\n",
                "  server_urls:\n",
                "    - channel: main\n",
                "      url: {mcp_url}\n",
                "  extra_headers:\n",
                "    {header}: {token_ref}\n",
                "  max_concurrent_requests: {max_concurrency}\n",
            ),
            tunnel_id = tunnel_id,
            health_url_file = health_url_file,
            mcp_url = mcp_url,
            token_ref = token_ref,
            header = canonical_local_mcp_header(),
            max_concurrency = LOCAL_TUNNEL_PROFILE_MAX_CONCURRENT_REQUESTS,
        ))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TunnelProcessState {
    pub schema_version: u32,
    pub runtime_id: String,
    pub process: ProcessIdentity,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TunnelHealthEvidence {
    pub live: bool,
    pub ready: bool,
    pub diagnostic: String,
}

pub struct ManagedTunnelChild {
    child: Child,
    pub identity: ProcessIdentity,
    stdout_task: Option<JoinHandle<()>>,
    stderr_task: Option<JoinHandle<()>>,
}

impl ManagedTunnelChild {
    pub fn pid(&self) -> u32 {
        self.child.id().unwrap_or(self.identity.pid as u32)
    }

    pub fn try_wait(&mut self) -> Result<Option<std::process::ExitStatus>> {
        self.child
            .try_wait()
            .context("failed to inspect tunnel-client process")
    }

    pub async fn wait(&mut self) -> Result<std::process::ExitStatus> {
        self.child
            .wait()
            .await
            .context("failed waiting for tunnel-client process")
    }

    pub async fn terminate(mut self) -> Result<()> {
        let inspector = SystemProcessInspector;
        let _ = signal_process_if_matching(&inspector, &self.identity, ProcessSignal::Terminate)?;
        let wait = tokio::time::timeout(TUNNEL_STOP_GRACE, self.child.wait()).await;
        if wait.is_err() && self.child.try_wait()?.is_none() {
            let _ = signal_process_if_matching(&inspector, &self.identity, ProcessSignal::Kill)?;
            let _ = tokio::time::timeout(TUNNEL_STOP_GRACE, self.child.wait()).await;
        }
        self.finish_log_tasks().await;
        Ok(())
    }

    async fn finish_log_tasks(&mut self) {
        if let Some(task) = self.stdout_task.take() {
            let _ = tokio::time::timeout(Duration::from_secs(1), task).await;
        }
        if let Some(task) = self.stderr_task.take() {
            let _ = tokio::time::timeout(Duration::from_secs(1), task).await;
        }
    }
}

pub fn write_tunnel_profile(path: &Path, profile: &LocalTunnelProfile) -> Result<()> {
    write_user_only_file(path, profile.render()?.as_bytes())
}

pub fn write_mcp_token(path: &Path, token: &str) -> Result<()> {
    if token.is_empty()
        || token
            .chars()
            .any(|character| matches!(character, '\r' | '\n' | '\0'))
    {
        bail!("Local MCP token must be a non-empty single-line value");
    }
    write_user_only_file(path, token.as_bytes())
}

pub fn write_tunnel_process_state(path: &Path, state: &TunnelProcessState) -> Result<()> {
    if state.schema_version != LOCAL_TUNNEL_PROCESS_STATE_SCHEMA_VERSION {
        bail!("unsupported Local tunnel process state schema version");
    }
    let encoded =
        serde_json::to_vec_pretty(state).context("failed to encode tunnel process state")?;
    write_user_only_file(path, &encoded)
}

pub fn load_tunnel_process_state(path: &Path) -> Result<Option<TunnelProcessState>> {
    if !path.exists() {
        return Ok(None);
    }
    let raw = fs::read(path)
        .with_context(|| format!("failed to read tunnel process state {}", path.display()))?;
    let state: TunnelProcessState = serde_json::from_slice(&raw)
        .with_context(|| format!("failed to parse tunnel process state {}", path.display()))?;
    if state.schema_version != LOCAL_TUNNEL_PROCESS_STATE_SCHEMA_VERSION {
        bail!(
            "unsupported Local tunnel process state schema version {}",
            state.schema_version
        );
    }
    Ok(Some(state))
}

pub fn cleanup_stale_tunnel_child(
    paths: &LocalPaths,
    inspector: &dyn ProcessInspector,
) -> Result<StaleTunnelCleanup> {
    let state_path = paths.tunnel_process_state_file();
    let Some(state) = load_tunnel_process_state(&state_path)? else {
        return Ok(StaleTunnelCleanup::NoRecordedChild);
    };
    let matched = signal_process_if_matching(inspector, &state.process, ProcessSignal::Terminate)?;
    if matched {
        let terminate_deadline = Instant::now() + STALE_TUNNEL_STOP_GRACE;
        while Instant::now() < terminate_deadline
            && crate::session::identity_matches(inspector, &state.process)?
        {
            std::thread::sleep(STALE_TUNNEL_POLL);
        }
        if crate::session::identity_matches(inspector, &state.process)? {
            let _ = signal_process_if_matching(inspector, &state.process, ProcessSignal::Kill)?;
            let kill_deadline = Instant::now() + STALE_TUNNEL_KILL_GRACE;
            while Instant::now() < kill_deadline
                && crate::session::identity_matches(inspector, &state.process)?
            {
                std::thread::sleep(STALE_TUNNEL_POLL);
            }
        }
        if crate::session::identity_matches(inspector, &state.process)? {
            bail!(
                "stale tunnel-client PID {} still matches its recorded birth identity after TERM/KILL; refusing replacement",
                state.process.pid
            );
        }
        fs::remove_file(&state_path).with_context(|| {
            format!(
                "failed to remove stopped tunnel process state {}",
                state_path.display()
            )
        })?;
        return Ok(StaleTunnelCleanup::SignaledMatchingChild(state.process.pid));
    }
    if inspector.identity(state.process.pid)?.is_some() {
        return Ok(StaleTunnelCleanup::IdentityMismatch(state.process.pid));
    }
    let _ = fs::remove_file(&state_path);
    Ok(StaleTunnelCleanup::AlreadyExited(state.process.pid))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StaleTunnelCleanup {
    NoRecordedChild,
    SignaledMatchingChild(i32),
    AlreadyExited(i32),
    IdentityMismatch(i32),
}

pub async fn spawn_tunnel_client(
    paths: &LocalPaths,
    binary: &Path,
    profile_path: &Path,
    runtime_key: &RuntimeKey,
    runtime_id: &str,
    inherited_infrastructure_environment: &[(OsString, OsString)],
    redacted_values: &[String],
) -> Result<ManagedTunnelChild> {
    if !binary.is_absolute() || !profile_path.is_absolute() {
        bail!("managed tunnel-client binary/profile paths must be absolute");
    }
    let mut command = Command::new(binary);
    command
        .args(["run", "--profile-file"])
        .arg(profile_path)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in provider_environment(inherited_infrastructure_environment, runtime_key) {
        command.env(key, value);
    }

    let mut child = command.spawn().with_context(|| {
        format!(
            "failed to start managed tunnel-client at {}",
            binary.display()
        )
    })?;
    let pid = child
        .id()
        .context("managed tunnel-client did not expose a process id")? as i32;
    let inspector = SystemProcessInspector;
    let Some(identity) = inspector.identity(pid)? else {
        let _ = child.kill().await;
        bail!("could not establish stable process identity for tunnel-client PID {pid}");
    };
    write_tunnel_process_state(
        &paths.tunnel_process_state_file(),
        &TunnelProcessState {
            schema_version: LOCAL_TUNNEL_PROCESS_STATE_SCHEMA_VERSION,
            runtime_id: runtime_id.to_string(),
            process: identity.clone(),
        },
    )?;

    let log_path = paths.diagnostic_log_file();
    let lock = Arc::new(Mutex::new(()));
    let redactions: Arc<[String]> = redacted_values.to_vec().into();
    let stdout_task = child.stdout.take().map(|stdout| {
        spawn_log_reader(
            stdout,
            log_path.clone(),
            lock.clone(),
            redactions.clone(),
            "tunnel stdout",
        )
    });
    let stderr_task = child
        .stderr
        .take()
        .map(|stderr| spawn_log_reader(stderr, log_path, lock, redactions, "tunnel stderr"));

    Ok(ManagedTunnelChild {
        child,
        identity,
        stdout_task,
        stderr_task,
    })
}

pub async fn probe_tunnel_health(
    binary: &Path,
    health_url_file: &Path,
    runtime_key: &RuntimeKey,
    inherited_infrastructure_environment: &[(OsString, OsString)],
) -> Result<TunnelHealthEvidence> {
    if !binary.is_absolute() || !health_url_file.is_absolute() {
        bail!("managed tunnel-client health paths must be absolute");
    }
    let mut command = Command::new(binary);
    command
        .arg("health")
        .arg("--url-file")
        .arg(health_url_file)
        .env_clear()
        .stdin(Stdio::null())
        .kill_on_drop(true);
    for (key, value) in provider_environment(inherited_infrastructure_environment, runtime_key) {
        command.env(key, value);
    }
    let output = command
        .output()
        .await
        .context("failed to run structured tunnel-client health probe")?;
    let mut diagnostic =
        String::from_utf8_lossy(if output.status.success() || output.stderr.is_empty() {
            &output.stdout
        } else {
            &output.stderr
        })
        .trim()
        .to_string();
    diagnostic = sanitize_diagnostic(&diagnostic, &[runtime_key.expose().to_string()]);
    if diagnostic.len() > 4096 {
        diagnostic.truncate(4096);
        diagnostic.push('…');
    }
    Ok(TunnelHealthEvidence {
        live: output.status.success(),
        ready: output.status.success(),
        diagnostic,
    })
}

fn spawn_log_reader<R>(
    mut reader: R,
    path: PathBuf,
    lock: Arc<Mutex<()>>,
    redactions: Arc<[String]>,
    label: &'static str,
) -> JoinHandle<()>
where
    R: AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut buffer = vec![0_u8; 8192];
        loop {
            let read = match reader.read(&mut buffer).await {
                Ok(0) | Err(_) => return,
                Ok(read) => read,
            };
            let text = String::from_utf8_lossy(&buffer[..read]);
            let text = sanitize_diagnostic(&text, &redactions);
            let _guard = lock.lock().await;
            let _ = append_bounded_log(&path, label, text.as_bytes());
        }
    })
}

fn append_bounded_log(path: &Path, label: &str, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .context("Local diagnostic log path has no parent")?;
    fs::create_dir_all(parent)
        .with_context(|| format!("failed to create Local log directory {}", parent.display()))?;
    let current_size = fs::metadata(path)
        .map(|metadata| metadata.len())
        .unwrap_or(0);
    if current_size.saturating_add(bytes.len() as u64) > DIAGNOSTIC_LOG_MAX_BYTES {
        let rotated = path.with_extension("log.1");
        let _ = fs::remove_file(&rotated);
        if path.exists() {
            fs::rename(path, &rotated).with_context(|| {
                format!("failed to rotate Local diagnostic log {}", path.display())
            })?;
        }
    }
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .with_context(|| format!("failed to open Local diagnostic log {}", path.display()))?;
    writeln!(file, "[{label}]")?;
    file.write_all(bytes)?;
    if !bytes.ends_with(b"\n") {
        file.write_all(b"\n")?;
    }
    super::private_fs::set_user_only_file(path)?;
    Ok(())
}

fn write_user_only_file(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .context("Local runtime file path has no parent")?;
    fs::create_dir_all(parent).with_context(|| {
        format!(
            "failed to create Local runtime directory {}",
            parent.display()
        )
    })?;
    super::private_fs::set_user_only_directory(parent)?;
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .with_context(|| format!("failed to write Local runtime file {}", path.display()))?;
    file.write_all(bytes)?;
    file.sync_all()?;
    super::private_fs::set_user_only_file(path)?;
    Ok(())
}

fn canonical_local_mcp_header() -> &'static str {
    // Hyper/HTTP header names are case-insensitive, but keep the documented
    // product spelling in generated provider configuration.
    debug_assert_eq!(LOCAL_MCP_TOKEN_HEADER, "x-zodex-local-token");
    "X-Zodex-Local-Token"
}

fn yaml_string(value: &str) -> Result<String> {
    serde_json::to_string(value).context("failed to quote Local tunnel profile value")
}

fn sanitize_diagnostic(value: &str, redactions: &[String]) -> String {
    let mut sanitized = value.to_string();
    for secret in redactions.iter().filter(|secret| !secret.is_empty()) {
        sanitized = sanitized.replace(secret, "<redacted>");
    }
    sanitized
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::ffi::OsString;
    #[cfg(target_os = "linux")]
    use std::process::Command as StdCommand;

    use tempfile::tempdir;

    use crate::session::{ProcessBirthIdentity, ProcessIdentity, ProcessInspector};

    use super::{
        LOCAL_TUNNEL_PROFILE_MAX_CONCURRENT_REQUESTS, LocalTunnelProfile, StaleTunnelCleanup,
        TunnelProcessState, cleanup_stale_tunnel_child, write_tunnel_process_state,
    };
    use crate::local::LocalPaths;

    #[test]
    fn tunnel_profile_targets_only_local_mcp_and_never_serializes_secrets() {
        let profile = LocalTunnelProfile {
            tunnel_id: "tunnel_fixture".to_string(),
            mcp_url: "http://127.0.0.1:43123/mcp".to_string(),
            mcp_token_path: "/tmp/zodex runtime/mcp-token".into(),
            health_url_file: "/tmp/zodex runtime/health-url".into(),
        };
        let rendered = profile.render().unwrap();
        assert!(rendered.contains("url: \"http://127.0.0.1:43123/mcp\""));
        assert!(rendered.contains("api_key: env:CONTROL_PLANE_API_KEY"));
        assert!(rendered.contains("control_plane:\n  tunnel_id:"));
        assert!(rendered.contains("mcp:\n  server_urls:\n    - channel: main"));
        assert!(rendered.contains("  extra_headers:\n    X-Zodex-Local-Token:"));
        // The normal MCP header map is also the baseline for provider
        // discovery/probes. Repeating the same header in
        // `discovery_extra_headers` is rejected by tunnel-client v0.0.11.
        assert_eq!(rendered.matches("X-Zodex-Local-Token").count(), 1);
        assert_eq!(
            rendered
                .matches("file:/tmp/zodex runtime/mcp-token")
                .count(),
            1
        );
        assert!(!rendered.contains("discovery_extra_headers"));
        assert!(rendered.contains("listen_addr: 127.0.0.1:0"));
        assert!(rendered.contains(&format!(
            "max_concurrent_requests: {LOCAL_TUNNEL_PROFILE_MAX_CONCURRENT_REQUESTS}"
        )));
        assert!(!rendered.contains("Authorization:"));
        assert!(!rendered.contains("?token="));
        assert!(!rendered.contains("http_raw_unsafe"));
        assert!(!rendered.contains("sk-runtime-secret"));
    }

    #[test]
    fn profile_rejects_non_loopback_or_non_mcp_target() {
        for target in [
            "https://example.com/mcp",
            "http://127.0.0.1:1234/v1/status",
            "http://localhost:1234/mcp",
        ] {
            let profile = LocalTunnelProfile {
                tunnel_id: "tunnel_fixture".to_string(),
                mcp_url: target.to_string(),
                mcp_token_path: "/tmp/mcp-token".into(),
                health_url_file: "/tmp/health".into(),
            };
            assert!(profile.render().is_err(), "target should fail: {target}");
        }
    }

    #[test]
    fn stale_child_cleanup_never_signals_reused_pid_identity() {
        let dir = tempdir().unwrap();
        let paths = LocalPaths::from_roots(
            dir.path().join("config"),
            dir.path().join("data"),
            dir.path().join("state"),
        )
        .unwrap();
        let expected = ProcessIdentity {
            pid: 4242,
            birth: ProcessBirthIdentity::LinuxProcStartTicks { ticks: 100 },
        };
        write_tunnel_process_state(
            &paths.tunnel_process_state_file(),
            &TunnelProcessState {
                schema_version: super::LOCAL_TUNNEL_PROCESS_STATE_SCHEMA_VERSION,
                runtime_id: "old-runtime".to_string(),
                process: expected,
            },
        )
        .unwrap();
        let inspector = FakeInspector::new(ProcessIdentity {
            pid: 4242,
            birth: ProcessBirthIdentity::LinuxProcStartTicks { ticks: 101 },
        });
        let result = cleanup_stale_tunnel_child(&paths, &inspector).unwrap();
        assert_eq!(result, StaleTunnelCleanup::IdentityMismatch(4242));
        assert!(paths.tunnel_process_state_file().exists());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn stale_child_cleanup_terminates_matching_orphan_before_removing_ownership_state() {
        let output = StdCommand::new("/bin/sh")
            .arg("-c")
            .arg("nohup sleep 60 </dev/null >/dev/null 2>&1 & echo $!")
            .output()
            .unwrap();
        assert!(output.status.success());
        let pid: i32 = String::from_utf8(output.stdout)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let inspector = crate::session::SystemProcessInspector;
        let identity = inspector
            .identity(pid)
            .unwrap()
            .expect("orphaned fixture process should be alive");
        let dir = tempdir().unwrap();
        let paths = LocalPaths::from_roots(
            dir.path().join("config"),
            dir.path().join("data"),
            dir.path().join("state"),
        )
        .unwrap();
        write_tunnel_process_state(
            &paths.tunnel_process_state_file(),
            &TunnelProcessState {
                schema_version: super::LOCAL_TUNNEL_PROCESS_STATE_SCHEMA_VERSION,
                runtime_id: "crashed-runtime".to_string(),
                process: identity,
            },
        )
        .unwrap();

        let result = cleanup_stale_tunnel_child(&paths, &inspector).unwrap();
        assert_eq!(result, StaleTunnelCleanup::SignaledMatchingChild(pid));
        assert!(inspector.identity(pid).unwrap().is_none());
        assert!(!paths.tunnel_process_state_file().exists());
    }

    struct FakeInspector {
        identities: HashMap<i32, ProcessIdentity>,
    }

    impl FakeInspector {
        fn new(identity: ProcessIdentity) -> Self {
            Self {
                identities: HashMap::from([(identity.pid, identity)]),
            }
        }
    }

    impl ProcessInspector for FakeInspector {
        fn identity(&self, pid: i32) -> anyhow::Result<Option<ProcessIdentity>> {
            Ok(self.identities.get(&pid).cloned())
        }

        fn live_cwd(&self, _pid: i32) -> Option<String> {
            None
        }

        fn descendants(
            &self,
            _root_pid: i32,
            _limit: usize,
        ) -> anyhow::Result<Vec<ProcessIdentity>> {
            Ok(Vec::new())
        }
    }

    #[test]
    fn provider_environment_helper_excludes_developer_and_openai_fallback_secrets() {
        let inherited = vec![
            (
                OsString::from("HTTPS_PROXY"),
                OsString::from("http://proxy"),
            ),
            (
                OsString::from("OPENAI_ADMIN_KEY"),
                OsString::from("admin-secret"),
            ),
            (
                OsString::from("OPENAI_API_KEY"),
                OsString::from("api-secret"),
            ),
            (OsString::from("PATH"), OsString::from("/developer/path")),
        ];
        let key = crate::local::RuntimeKey::new("runtime-secret").unwrap();
        let environment = super::super::tunnel_provider::provider_environment(&inherited, &key);
        let mapped = environment
            .into_iter()
            .map(|(key, value)| {
                (
                    key.to_string_lossy().to_string(),
                    value.to_string_lossy().to_string(),
                )
            })
            .collect::<HashMap<_, _>>();
        assert_eq!(
            mapped.get("HTTPS_PROXY").map(String::as_str),
            Some("http://proxy")
        );
        assert_eq!(
            mapped.get("CONTROL_PLANE_API_KEY").map(String::as_str),
            Some("runtime-secret")
        );
        assert!(!mapped.contains_key("OPENAI_ADMIN_KEY"));
        assert!(!mapped.contains_key("OPENAI_API_KEY"));
        assert!(!mapped.contains_key("PATH"));
    }
}
