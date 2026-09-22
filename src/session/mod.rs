use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result, anyhow, bail};
#[cfg(unix)]
use nix::pty::openpty;
use tokio::io::AsyncWriteExt;
use tokio::process::Child;
#[cfg(not(unix))]
use tokio::process::ChildStdin;
use tokio::sync::{Mutex, RwLock};
use tracing::{info, warn};

use crate::config::Config;
use crate::invocation::InvocationContext;
use crate::protocol::{
    CommandStatus, ExecCommandInput, TerminationReason, ToolOutput, WriteStdinInput,
};
use crate::workdir::validate_absolute_existing_workdir;

mod lifecycle;
mod output;
mod policy;
mod process;
mod query;
mod shutdown;

use lifecycle::{owned_process_end, process_termination_reason};
#[cfg(not(unix))]
use output::spawn_pipe_readers;
#[cfg(unix)]
use output::spawn_reader;
use output::{OutputBuffer, OutputSnapshot, next_char_boundary, spill_path_for_session};
use shutdown::{
    maybe_force_kill, reap_exit_code, request_termination, signal_owned_group_members,
    snapshot_descendants_before_termination, wait_for_session_exits_until,
};

pub use policy::{
    OwnedProcess, OwnedProcessEnd, OwnedProcessObserver, SessionCreatorContext, SessionOutputChunk,
    SessionOutputCompletion, SessionOutputObserver, SessionRuntimePolicy,
};
pub use process::{
    ProcessBirthIdentity, ProcessControl, ProcessIdentity, ProcessInspector, ProcessSignal,
    SystemProcessInspector, identity_matches, signal_process_if_matching,
};

const POLL_INTERVAL_MS: u64 = 30;
const TIMEOUT_NOTICE: &str = "\n[zodexd] process timed out and was terminated\n";
const TERMINATE_GRACE_PERIOD_MS: u64 = 5_000;
const EXIT_OUTPUT_DRAIN_TIMEOUT_MS: u64 = 500;
const SESSION_HANDLE_LEN: usize = 8;
const HANDLE_LOG_PREFIX_LEN: usize = 4;
const COMMAND_SUMMARY_MAX_CHARS: usize = 120;
const RUNTIME_DESCENDANT_DISCOVERY_LIMIT: usize = 1024;
const SESSION_HANDLE_ALPHABET: &[u8; 62] =
    b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

#[derive(Debug, Clone)]
pub struct SessionOrigin {
    pub transport: SessionTransport,
    pub caller_label: Option<String>,
}

impl SessionOrigin {
    pub fn direct() -> Self {
        Self {
            transport: SessionTransport::Direct,
            caller_label: None,
        }
    }

    pub fn http(caller_label: Option<String>) -> Self {
        Self {
            transport: SessionTransport::Http,
            caller_label,
        }
    }

    pub fn mcp(caller_label: Option<String>) -> Self {
        Self {
            transport: SessionTransport::Mcp,
            caller_label,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum SessionTransport {
    Mcp,
    Http,
    Direct,
}

impl SessionTransport {
    fn as_str(self) -> &'static str {
        match self {
            Self::Mcp => "mcp",
            Self::Http => "http",
            Self::Direct => "direct",
        }
    }
}

#[derive(Debug)]
struct SessionInner {
    pid: i32,
    last_known_cwd: String,
    child: Child,
    reaped_exit_code: Option<i32>,
    input_writer: Option<SessionInputWriter>,
    last_used_at: SystemTime,
    last_input_at: Instant,
    idle_timeout: Duration,
    timed_out: bool,
    kill_requested: bool,
    terminate_started_at: Option<Instant>,
    force_killed: bool,
    require_exit_before_return: bool,
    leader_identity: Option<ProcessIdentity>,
    owned_group_members: Vec<ProcessIdentity>,
    persisted_group_members: Vec<ProcessIdentity>,
}

#[derive(Debug)]
enum SessionInputWriter {
    #[cfg(unix)]
    Pty(tokio::fs::File),
    #[cfg(not(unix))]
    Pipe(ChildStdin),
}

impl SessionInputWriter {
    async fn write_all(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        match self {
            #[cfg(unix)]
            Self::Pty(writer) => writer.write_all(bytes).await,
            #[cfg(not(unix))]
            Self::Pipe(writer) => writer.write_all(bytes).await,
        }
    }

    async fn flush(&mut self) -> std::io::Result<()> {
        match self {
            #[cfg(unix)]
            Self::Pty(writer) => writer.flush().await,
            #[cfg(not(unix))]
            Self::Pipe(writer) => writer.flush().await,
        }
    }
}

struct SessionRuntime {
    internal_session_id: u64,
    session_handle: String,
    created_at: SystemTime,
    started_at: Instant,
    initial_command: String,
    transport: SessionTransport,
    caller_label: Option<String>,
    output: Arc<OutputBuffer>,
    op_lock: Mutex<()>,
    inner: Mutex<SessionInner>,
    process_inspector: Arc<dyn ProcessInspector>,
    process_observer: Arc<dyn OwnedProcessObserver>,
    owned_process: Option<OwnedProcess>,
    ownership_released: AtomicBool,
}

impl SessionRuntime {
    fn handle_prefix(&self) -> &str {
        let end = self
            .session_handle
            .char_indices()
            .nth(HANDLE_LOG_PREFIX_LEN)
            .map(|(idx, _)| idx)
            .unwrap_or(self.session_handle.len());
        &self.session_handle[..end]
    }

    fn command_summary(&self) -> String {
        summarize_command(&self.initial_command)
    }

    async fn last_used_at(&self) -> SystemTime {
        self.inner.lock().await.last_used_at
    }

    fn reap_and_record_exit(&self, inner: &mut SessionInner) -> Result<Option<i32>> {
        let exit_code = reap_exit_code(inner, self.process_inspector.as_ref())?;
        if inner.persisted_group_members != inner.owned_group_members {
            if let Some(process) = self.owned_process.as_ref() {
                self.process_observer
                    .process_group_members_updated(process, &inner.owned_group_members)?;
            }
            inner.persisted_group_members = inner.owned_group_members.clone();
        }
        Ok(exit_code)
    }

    async fn is_exited(&self) -> Result<bool> {
        let mut inner = self.inner.lock().await;
        let leader_exited = self.reap_and_record_exit(&mut inner)?.is_some();
        Ok(leader_exited && inner.owned_group_members.is_empty())
    }

    async fn continue_session(
        &self,
        input: WriteStdinInput,
        yield_time_ms: u64,
        poll_interval: Duration,
    ) -> Result<ToolOutput> {
        let _session_guard = self.op_lock.lock().await;

        info!(
            event = "session_continued",
            internal_session_id = self.internal_session_id,
            session_handle_prefix = self.handle_prefix(),
            transport = self.transport.as_str(),
            command_summary = self.command_summary(),
            caller_label = self.caller_label.as_deref().unwrap_or(""),
            has_input = input.chars.is_some(),
            kill_process = input.kill_process.unwrap_or(false),
        );

        {
            let mut inner = self.inner.lock().await;
            inner.last_used_at = SystemTime::now();
            inner.last_input_at = Instant::now();

            if input.kill_process.unwrap_or(false) {
                inner.kill_requested = true;
                inner.require_exit_before_return = true;
                request_termination(&mut inner);
            }
        }

        if input.kill_process.unwrap_or(false) {
            self.output
                .append("\n[zodexd] process terminated by kill_process\n");
            info!(
                event = "session_killed",
                internal_session_id = self.internal_session_id,
                session_handle_prefix = self.handle_prefix(),
                transport = self.transport.as_str(),
                command_summary = self.command_summary(),
            );
        } else if let Some(chars) = input.chars.as_deref() {
            let mut input_writer = {
                let mut inner = self.inner.lock().await;
                inner.input_writer.take()
            };

            if let Some(writer) = input_writer.as_mut() {
                writer
                    .write_all(chars.as_bytes())
                    .await
                    .context("failed to write stdin")?;
                writer.flush().await.context("failed to flush stdin")?;
            }

            let mut inner = self.inner.lock().await;
            inner.input_writer = input_writer;
        }

        self.wait_for_yield_or_exit_locked(yield_time_ms, poll_interval)
            .await
    }

    async fn initial_wait(
        &self,
        yield_time_ms: u64,
        poll_interval: Duration,
    ) -> Result<ToolOutput> {
        let _session_guard = self.op_lock.lock().await;
        self.wait_for_yield_or_exit_locked(yield_time_ms, poll_interval)
            .await
    }

    async fn wait_for_yield_or_exit_locked(
        &self,
        yield_time_ms: u64,
        poll_interval: Duration,
    ) -> Result<ToolOutput> {
        let started = Instant::now();
        let yield_for = Duration::from_millis(yield_time_ms);

        loop {
            let mut timeout_notice = false;
            let mut finished: Option<(i32, String, TerminationReason)> = None;
            let mut running_cwd: Option<String> = None;

            {
                let mut inner = self.inner.lock().await;
                inner.last_used_at = SystemTime::now();

                if let Some(live_cwd) = self.process_inspector.live_cwd(inner.pid) {
                    inner.last_known_cwd = live_cwd;
                }

                if inner.last_input_at.elapsed() >= inner.idle_timeout && !inner.timed_out {
                    inner.timed_out = true;
                    inner.require_exit_before_return = true;
                    request_termination(&mut inner);
                    timeout_notice = true;

                    info!(
                        event = "session_timed_out",
                        internal_session_id = self.internal_session_id,
                        session_handle_prefix = self.handle_prefix(),
                        transport = self.transport.as_str(),
                        command_summary = self.command_summary(),
                        cwd = inner.last_known_cwd,
                    );
                }

                let reaped_exit_code = self.reap_and_record_exit(&mut inner)?;
                maybe_force_kill(&mut inner, self.process_inspector.as_ref())?;

                match reaped_exit_code {
                    Some(_)
                        if inner.require_exit_before_return
                            && !inner.owned_group_members.is_empty() => {}
                    Some(code) => {
                        let termination_reason = process_termination_reason(&inner);
                        finished = Some((code, inner.last_known_cwd.clone(), termination_reason));
                    }
                    None if started.elapsed() >= yield_for && !inner.require_exit_before_return => {
                        running_cwd = Some(inner.last_known_cwd.clone());
                    }
                    None => {}
                }
            }

            if timeout_notice {
                self.output.append(TIMEOUT_NOTICE);
            }

            if let Some((exit_code, cwd, termination_reason)) = finished {
                let snapshot = snapshot_output_after_exit(&self.output).await;
                let text = strip_ansi_codes(snapshot.text);
                let elapsed = self.started_at.elapsed();
                return Ok(ToolOutput {
                    summary: command_result_summary(
                        CommandStatus::Exited,
                        elapsed,
                        None,
                        Some(exit_code),
                        Some(termination_reason),
                    ),
                    output: text,
                    output_file: snapshot.output_file,
                    output_chars: snapshot.output_chars,
                    output_lines: snapshot.output_lines,
                    output_file_truncated: snapshot.output_file_truncated,
                    status: CommandStatus::Exited,
                    cwd,
                    zodex_context: None,
                    session_id: None,
                    session_handle: None,
                    exit_code: Some(exit_code),
                    termination_reason: Some(termination_reason),
                });
            }

            if let Some(cwd) = running_cwd {
                let snapshot = self.output.snapshot();
                let text = strip_ansi_codes(snapshot.text);
                let elapsed = self.started_at.elapsed();
                return Ok(ToolOutput {
                    summary: command_result_summary(
                        CommandStatus::Running,
                        elapsed,
                        Some(&self.session_handle),
                        None,
                        None,
                    ),
                    output: text,
                    output_file: snapshot.output_file,
                    output_chars: snapshot.output_chars,
                    output_lines: snapshot.output_lines,
                    output_file_truncated: snapshot.output_file_truncated,
                    status: CommandStatus::Running,
                    cwd,
                    zodex_context: None,
                    session_id: Some(self.internal_session_id),
                    session_handle: Some(self.session_handle.clone()),
                    exit_code: None,
                    termination_reason: None,
                });
            }

            tokio::time::sleep(poll_interval).await;
        }
    }
}

pub struct SessionManager {
    sessions: RwLock<HashMap<String, Arc<SessionRuntime>>>,
    admission_lock: Mutex<()>,
    admission_closed: AtomicBool,
    next_internal_session_id: AtomicU64,
    max_sessions: usize,
    max_output_chars: usize,
    poll_interval: Duration,
    policy: SessionRuntimePolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionCounts {
    pub retained: usize,
    pub running: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeShutdownResult {
    pub sessions_signaled: usize,
    pub sessions_force_killed: usize,
    pub descendants_signaled: usize,
    pub descendants_force_killed: usize,
}

impl SessionManager {
    pub fn new(max_sessions: usize, max_output_chars: usize) -> Self {
        Self::with_policy(
            max_sessions,
            max_output_chars,
            SessionRuntimePolicy::sprite(),
        )
    }

    pub fn with_policy(
        max_sessions: usize,
        max_output_chars: usize,
        policy: SessionRuntimePolicy,
    ) -> Self {
        Self {
            sessions: RwLock::new(HashMap::new()),
            admission_lock: Mutex::new(()),
            admission_closed: AtomicBool::new(false),
            next_internal_session_id: AtomicU64::new(1),
            max_sessions,
            max_output_chars,
            poll_interval: Duration::from_millis(POLL_INTERVAL_MS),
            policy,
        }
    }

    pub async fn exec_command(
        &self,
        input: ExecCommandInput,
        cfg: &Config,
        origin: SessionOrigin,
    ) -> Result<ToolOutput> {
        self.exec_command_with_context(input, cfg, origin, InvocationContext::default())
            .await
    }

    pub async fn exec_command_with_context(
        &self,
        input: ExecCommandInput,
        cfg: &Config,
        origin: SessionOrigin,
        invocation: InvocationContext,
    ) -> Result<ToolOutput> {
        let command_cwd = validate_absolute_existing_workdir(&input.workdir)?;
        if self.admission_closed.load(Ordering::Acquire) {
            bail!("session runtime is stopping; new commands are not accepted");
        }
        let _admission_guard = self.admission_lock.lock().await;
        if self.admission_closed.load(Ordering::Acquire) {
            bail!("session runtime is stopping; new commands are not accepted");
        }
        self.evict_if_needed().await?;

        let timeout_ms = cfg.clamp_exec_timeout_ms(input.timeout_ms);
        let yield_time_ms = cfg.clamp_exec_yield_ms(input.yield_time_ms);
        let now = Instant::now();
        let now_system = SystemTime::now();

        let command_cwd_display = command_cwd.display().to_string();

        #[cfg(unix)]
        let pty = openpty(None, None).context("failed to allocate PTY")?;
        #[cfg(unix)]
        let master_file = std::fs::File::from(pty.master);
        #[cfg(unix)]
        let slave_file = std::fs::File::from(pty.slave);
        #[cfg(unix)]
        let slave_stdin = slave_file
            .try_clone()
            .context("failed to clone PTY slave for stdin")?;
        #[cfg(unix)]
        let slave_stdout = slave_file
            .try_clone()
            .context("failed to clone PTY slave for stdout")?;

        let mut command = self.policy.command(&input.cmd, cfg);

        #[cfg(unix)]
        command
            .stdin(Stdio::from(slave_stdin))
            .stdout(Stdio::from(slave_stdout))
            .stderr(Stdio::from(slave_file));

        #[cfg(not(unix))]
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        #[cfg(unix)]
        command.process_group(0);

        command.current_dir(&command_cwd);

        let internal_session_id = self
            .next_internal_session_id
            .fetch_add(1, Ordering::Relaxed);
        let session_handle = generate_session_handle();
        let spill_path = spill_path_for_session(&session_handle);
        let output = Arc::new(OutputBuffer::new(self.max_output_chars, spill_path));
        let session_handle_arc: Arc<str> = Arc::from(session_handle.as_str());

        #[cfg(unix)]
        let master_reader_std = master_file
            .try_clone()
            .context("failed to clone PTY master for reader")?;
        #[cfg(unix)]
        let master_writer_std = master_file;
        #[cfg(unix)]
        let master_writer = tokio::fs::File::from_std(master_writer_std);
        #[cfg(unix)]
        spawn_reader(
            master_reader_std,
            output.clone(),
            self.policy.output_observer(),
            internal_session_id,
            session_handle_arc.clone(),
            invocation.clone(),
        )?;

        let mut child = command
            .spawn()
            .with_context(|| format!("failed to spawn command: {}", input.cmd))?;

        #[cfg(unix)]
        let input_writer = Some(SessionInputWriter::Pty(master_writer));
        #[cfg(not(unix))]
        let input_writer = Some(SessionInputWriter::Pipe(
            child
                .stdin
                .take()
                .context("failed to capture command stdin pipe")?,
        ));
        #[cfg(not(unix))]
        {
            let stdout = child
                .stdout
                .take()
                .context("failed to capture command stdout pipe")?;
            let stderr = child
                .stderr
                .take()
                .context("failed to capture command stderr pipe")?;
            spawn_pipe_readers(
                stdout,
                stderr,
                output.clone(),
                self.policy.output_observer(),
                internal_session_id,
                session_handle_arc.clone(),
                invocation.clone(),
            );
        }
        let pid = child
            .id()
            .ok_or_else(|| anyhow!("failed to obtain child process id"))? as i32;
        let identity = self.policy.process_inspector().identity(pid)?;
        if identity.is_none()
            && self.policy.require_process_identity()
            && child.try_wait()?.is_none()
        {
            let _ = process::signal_process_group(pid, ProcessSignal::Kill);
            let _ = child.wait().await;
            bail!(
                "Local runtime could not establish a stable process identity for PID {pid}; command was terminated before being admitted"
            );
        }
        let owned_process = identity.clone().map(|identity| OwnedProcess {
            internal_session_id,
            session_handle: session_handle_arc,
            identity,
            created_by: invocation.clone(),
        });
        let process_observer = self.policy.process_observer();
        if let Some(process) = owned_process.as_ref()
            && let Err(error) = process_observer.process_started(process)
        {
            let _ = process::signal_process_group(pid, ProcessSignal::Kill);
            let _ = child.wait().await;
            let rollback = OwnedProcessEnd::incomplete(
                "process ownership admission failed before the session was admitted",
                Some(command_cwd_display.clone()),
            );
            if let Err(end_error) = process_observer.process_ended(process, &rollback) {
                warn!(
                    event = "session_process_observer_rollback_failed",
                    internal_session_id,
                    error = %end_error,
                );
            }
            return Err(error).context(
                "failed to record command process ownership; command was terminated before admission",
            );
        }

        let runtime = Arc::new(SessionRuntime {
            internal_session_id,
            session_handle: session_handle.clone(),
            created_at: now_system,
            started_at: now,
            initial_command: input.cmd.clone(),
            transport: origin.transport,
            caller_label: origin.caller_label,
            output,
            op_lock: Mutex::new(()),
            inner: Mutex::new(SessionInner {
                pid,
                last_known_cwd: command_cwd_display.clone(),
                child,
                reaped_exit_code: None,
                input_writer,
                last_used_at: now_system,
                last_input_at: now,
                idle_timeout: Duration::from_millis(timeout_ms),
                timed_out: false,
                kill_requested: false,
                terminate_started_at: None,
                force_killed: false,
                require_exit_before_return: false,
                leader_identity: identity,
                owned_group_members: Vec::new(),
                persisted_group_members: Vec::new(),
            }),
            process_inspector: self.policy.process_inspector().clone(),
            process_observer,
            owned_process,
            ownership_released: AtomicBool::new(false),
        });

        {
            let mut sessions = self.sessions.write().await;
            sessions.insert(session_handle.clone(), runtime.clone());
        }
        drop(_admission_guard);

        spawn_child_reaper(runtime.clone(), self.poll_interval);

        info!(
            event = "session_created",
            internal_session_id,
            session_handle_prefix = runtime.handle_prefix(),
            transport = runtime.transport.as_str(),
            command_summary = runtime.command_summary(),
            cwd = command_cwd_display,
            caller_label = runtime.caller_label.as_deref().unwrap_or(""),
            created_at_epoch_ms = system_time_epoch_ms(runtime.created_at),
        );

        let output = runtime
            .initial_wait(yield_time_ms, self.poll_interval)
            .await
            .map_err(|err| anyhow!("failed while waiting for new session output: {err}"))?;

        if output.status == CommandStatus::Exited && runtime.is_exited().await? {
            self.remove_session(&session_handle).await;
        }

        Ok(output)
    }

    pub async fn write_stdin(&self, input: WriteStdinInput, cfg: &Config) -> Result<ToolOutput> {
        self.write_stdin_with_context(input, cfg, InvocationContext::default())
            .await
    }

    pub async fn write_stdin_with_context(
        &self,
        input: WriteStdinInput,
        cfg: &Config,
        _invocation: InvocationContext,
    ) -> Result<ToolOutput> {
        let yield_time_ms = cfg.clamp_write_yield_ms(input.yield_time_ms);
        let session_handle = input.session_handle.clone();
        let runtime = {
            let sessions = self.sessions.read().await;
            sessions
                .get(&session_handle)
                .cloned()
                .ok_or_else(|| unknown_session_handle(&session_handle))?
        };

        let output = runtime
            .continue_session(input, yield_time_ms, self.poll_interval)
            .await?;

        if output.status == CommandStatus::Exited && runtime.is_exited().await? {
            self.remove_session(&session_handle).await;
        }

        Ok(output)
    }

    pub fn accepting_new_sessions(&self) -> bool {
        !self.admission_closed.load(Ordering::Acquire)
    }

    pub async fn session_counts(&self) -> Result<SessionCounts> {
        let runtimes = self
            .sessions
            .read()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        let retained = runtimes.len();
        let mut running = 0;
        for runtime in runtimes {
            if !runtime.is_exited().await? {
                running += 1;
            }
        }
        Ok(SessionCounts { retained, running })
    }

    pub async fn shutdown_all(&self) -> Result<RuntimeShutdownResult> {
        self.admission_closed.store(true, Ordering::Release);
        // Establish a barrier with any command that passed the first admission
        // check but has not yet completed spawning/registering its child.
        let admission_guard = self.admission_lock.lock().await;
        drop(admission_guard);

        let runtimes = self
            .sessions
            .read()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();

        // Snapshot ordinary descendants before TERM while each tracked group
        // leader still provides stable ownership evidence. A shell wrapper can
        // exit immediately on TERM while a child in the same process group
        // survives and is reparented to PID 1; discovering only after TERM
        // loses that child and can leave it running after a seemingly clean
        // shutdown. Stable descendant identities make the later force pass
        // safe without blindly trusting a numeric PID/PGID after leader exit.
        let discovered_descendants =
            snapshot_descendants_before_termination(&runtimes, RUNTIME_DESCENDANT_DISCOVERY_LIMIT)
                .await;

        let mut sessions_signaled = 0;
        for runtime in &runtimes {
            let mut inner = runtime.inner.lock().await;
            if runtime.reap_and_record_exit(&mut inner)?.is_none() {
                inner.kill_requested = true;
                inner.require_exit_before_return = true;
                request_termination(&mut inner);
                sessions_signaled += 1;
            } else if !inner.owned_group_members.is_empty() {
                inner.kill_requested = true;
                inner.require_exit_before_return = true;
                inner.terminate_started_at.get_or_insert_with(Instant::now);
                signal_owned_group_members(
                    &inner,
                    runtime.process_inspector.as_ref(),
                    ProcessSignal::Terminate,
                )?;
                sessions_signaled += 1;
            } else if let Some(end) = owned_process_end(&inner) {
                runtime.release_process_ownership(end);
            }
        }

        // Process groups remain the primary ownership boundary. Also signal
        // the pre-TERM stable descendant snapshot so ordinary children that
        // escape or outlive their group leader participate in the same global
        // grace window.
        let mut descendants_signaled = 0;
        for (inspector, descendant) in &discovered_descendants {
            match process::signal_process_if_matching(
                inspector.as_ref(),
                descendant,
                ProcessSignal::Terminate,
            ) {
                Ok(true) => descendants_signaled += 1,
                Ok(false) => {}
                Err(error) => warn!(
                    event = "session_descendant_terminate_failed",
                    descendant_pid = descendant.pid,
                    error = %error,
                ),
            }
        }
        let deadline = Instant::now() + self.policy.shutdown_grace();
        wait_for_session_exits_until(&runtimes, self.poll_interval, deadline).await?;

        let mut sessions_force_killed = 0;
        for runtime in &runtimes {
            let mut inner = runtime.inner.lock().await;
            if runtime.reap_and_record_exit(&mut inner)?.is_none() {
                inner.force_killed = true;
                process::signal_process_group(inner.pid, ProcessSignal::Kill)?;
                sessions_force_killed += 1;
            } else if !inner.owned_group_members.is_empty() {
                inner.force_killed = true;
                if signal_owned_group_members(
                    &inner,
                    runtime.process_inspector.as_ref(),
                    ProcessSignal::Kill,
                )? > 0
                {
                    sessions_force_killed += 1;
                }
            }
        }

        let mut descendants_force_killed = 0;
        for (inspector, descendant) in &discovered_descendants {
            match process::signal_process_if_matching(
                inspector.as_ref(),
                descendant,
                ProcessSignal::Kill,
            ) {
                Ok(true) => descendants_force_killed += 1,
                Ok(false) => {}
                Err(error) => warn!(
                    event = "session_descendant_force_kill_failed",
                    descendant_pid = descendant.pid,
                    error = %error,
                ),
            }
        }

        let force_deadline = Instant::now() + Duration::from_secs(1);
        let survivors =
            wait_for_session_exits_until(&runtimes, self.poll_interval, force_deadline).await?;
        if survivors > 0 {
            bail!("timed out waiting for {survivors} Local command process group(s) to exit");
        }

        self.sessions.write().await.clear();
        Ok(RuntimeShutdownResult {
            sessions_signaled,
            sessions_force_killed,
            descendants_signaled,
            descendants_force_killed,
        })
    }

    async fn remove_session(&self, session_handle: &str) {
        let runtime = {
            let sessions = self.sessions.read().await;
            sessions.get(session_handle).cloned()
        };
        let Some(runtime) = runtime else {
            return;
        };
        let end = {
            let inner = runtime.inner.lock().await;
            owned_process_end(&inner)
        };
        let Some(end) = end else {
            warn!(
                event = "session_removal_deferred_without_final_process_evidence",
                internal_session_id = runtime.internal_session_id,
                session_handle_prefix = runtime.handle_prefix(),
            );
            return;
        };
        let removed = {
            let mut sessions = self.sessions.write().await;
            sessions.remove(session_handle)
        };

        if let Some(runtime) = removed {
            runtime.release_process_ownership(end);
            info!(
                event = "session_removed",
                internal_session_id = runtime.internal_session_id,
                session_handle_prefix = runtime.handle_prefix(),
                transport = runtime.transport.as_str(),
                command_summary = runtime.command_summary(),
            );
        }
    }

    async fn evict_if_needed(&self) -> Result<()> {
        loop {
            let session_count = self.sessions.read().await.len();
            if session_count < self.max_sessions {
                return Ok(());
            }

            let candidates = {
                let sessions = self.sessions.read().await;
                sessions
                    .iter()
                    .map(|(handle, runtime)| (handle.clone(), runtime.clone()))
                    .collect::<Vec<_>>()
            };

            let mut oldest_exited: Option<(String, SystemTime, Arc<SessionRuntime>)> = None;

            for (handle, runtime) in candidates {
                let last_used = runtime.last_used_at().await;
                if runtime.is_exited().await?
                    && oldest_exited
                        .as_ref()
                        .map(|(_, ts, _)| last_used < *ts)
                        .unwrap_or(true)
                {
                    oldest_exited = Some((handle, last_used, runtime));
                }
            }

            if let Some((handle, _, _)) = oldest_exited {
                self.remove_session(&handle).await;
            } else {
                bail!(
                    "session capacity reached ({} running sessions); finish or kill one before starting another command",
                    self.max_sessions
                );
            }
        }
    }
}

fn spawn_child_reaper(runtime: Arc<SessionRuntime>, poll_interval: Duration) {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(poll_interval).await;
            let reaped = {
                let mut inner = runtime.inner.lock().await;
                let result = runtime.reap_and_record_exit(&mut inner);
                result.map(|exit| {
                    let end = exit.and_then(|_| owned_process_end(&inner));
                    (exit, end)
                })
            };

            match reaped {
                Ok((Some(exit_code), Some(end))) => {
                    info!(
                        event = "session_child_reaped",
                        internal_session_id = runtime.internal_session_id,
                        session_handle_prefix = runtime.handle_prefix(),
                        exit_code,
                    );
                    runtime.release_process_ownership(end);
                    break;
                }
                Ok((Some(_), None)) | Ok((None, _)) => {}
                Err(err) => {
                    warn!(
                        event = "session_child_reap_failed",
                        internal_session_id = runtime.internal_session_id,
                        session_handle_prefix = runtime.handle_prefix(),
                        error = %err,
                    );
                    break;
                }
            }
        }
    });
}

fn generate_session_handle() -> String {
    let random = rand::random::<[u8; SESSION_HANDLE_LEN]>();
    let mut handle = String::with_capacity(SESSION_HANDLE_LEN);
    for byte in random {
        handle
            .push(SESSION_HANDLE_ALPHABET[(byte as usize) % SESSION_HANDLE_ALPHABET.len()] as char);
    }
    handle
}

fn summarize_command(command: &str) -> String {
    let cleaned = command.replace(['\n', '\r'], " ");
    if cleaned.len() <= COMMAND_SUMMARY_MAX_CHARS {
        return cleaned;
    }

    let cut = next_char_boundary(&cleaned, COMMAND_SUMMARY_MAX_CHARS);
    format!("{}...", &cleaned[..cut])
}

fn strip_ansi_codes(output: String) -> String {
    strip_ansi_escapes::strip_str(output)
}

fn command_result_summary(
    status: CommandStatus,
    elapsed: Duration,
    session_handle: Option<&str>,
    exit_code: Option<i32>,
    termination_reason: Option<TerminationReason>,
) -> String {
    let elapsed_secs = elapsed.as_secs_f64();
    match status {
        CommandStatus::Running => {
            let handle = session_handle.unwrap_or("<unknown>");
            format!("still running after {elapsed_secs:.1}s; use session_handle {handle} to poll")
        }
        CommandStatus::Exited => match termination_reason {
            Some(TerminationReason::Timeout) => format!("timed out after {elapsed_secs:.1}s"),
            Some(TerminationReason::Killed) => format!("killed after {elapsed_secs:.1}s"),
            _ => format!(
                "exited {} after {elapsed_secs:.1}s",
                exit_code.unwrap_or(-1)
            ),
        },
    }
}

fn system_time_epoch_ms(t: SystemTime) -> u128 {
    t.duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

async fn snapshot_output_after_exit(output: &Arc<OutputBuffer>) -> OutputSnapshot {
    // Child exit can race the asynchronous PTY reader. Wait for the reader's
    // terminal EOF/EIO signal before taking the final snapshot so trailing
    // command output is not lost merely because one short sample was quiet.
    // A bounded fallback preserves existing behavior for descendants that keep
    // the PTY slave open after the shell leader exits.
    output
        .wait_for_reader_done(Duration::from_millis(EXIT_OUTPUT_DRAIN_TIMEOUT_MS))
        .await;
    output.snapshot()
}

fn unknown_session_handle(session_handle: &str) -> anyhow::Error {
    anyhow!("Unknown session handle: {session_handle}")
}

#[cfg(all(test, unix))]
mod runtime_lifecycle_tests;
#[cfg(all(test, unix))]
mod tests;
