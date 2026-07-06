use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result, anyhow};
#[cfg(unix)]
use nix::errno::Errno;
#[cfg(unix)]
use nix::pty::openpty;
#[cfg(unix)]
use nix::sys::signal::{Signal, killpg};
#[cfg(unix)]
use nix::unistd::{Pid, setpgid};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, RwLock};
use tracing::info;

use crate::config::Config;
use crate::protocol::{
    CommandStatus, ExecCommandInput, TerminationReason, ToolOutput, WriteStdinInput,
};

const POLL_INTERVAL_MS: u64 = 30;
const TIMEOUT_NOTICE: &str = "\n[zodexd] process timed out and was terminated\n";
const TERMINATE_GRACE_PERIOD_MS: u64 = 5_000;
const EXIT_OUTPUT_DRAIN_RETRIES: usize = 4;
const EXIT_OUTPUT_DRAIN_DELAY_MS: u64 = 10;
const SESSION_HANDLE_LEN: usize = 8;
const HANDLE_LOG_PREFIX_LEN: usize = 4;
const COMMAND_SUMMARY_MAX_CHARS: usize = 120;
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
struct OutputState {
    text: String,
    dropped_bytes: usize,
}

#[derive(Debug)]
struct OutputBuffer {
    inner: Mutex<OutputState>,
    max_chars: usize,
}

impl OutputBuffer {
    fn new(max_chars: usize) -> Self {
        Self {
            inner: Mutex::new(OutputState {
                text: String::new(),
                dropped_bytes: 0,
            }),
            max_chars,
        }
    }

    async fn append(&self, chunk: &str) {
        let mut state = self.inner.lock().await;
        state.text.push_str(chunk);

        if state.text.len() <= self.max_chars {
            return;
        }

        let overflow = state.text.len() - self.max_chars;
        let cut = next_char_boundary(&state.text, overflow);
        state.text.drain(..cut);
        state.dropped_bytes += cut;
    }

    async fn snapshot(&self) -> String {
        let state = self.inner.lock().await;
        if state.dropped_bytes == 0 {
            return state.text.clone();
        }

        format!(
            "[... {} bytes truncated ...]\n{}",
            state.dropped_bytes, state.text
        )
    }
}

fn next_char_boundary(s: &str, idx: usize) -> usize {
    if idx >= s.len() {
        return s.len();
    }

    let mut i = idx;
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

#[derive(Debug)]
struct SessionInner {
    pid: i32,
    last_known_cwd: String,
    child: Child,
    pty_writer: Option<tokio::fs::File>,
    last_used_at: SystemTime,
    last_input_at: Instant,
    idle_timeout: Duration,
    timed_out: bool,
    kill_requested: bool,
    terminate_started_at: Option<Instant>,
    force_killed: bool,
    require_exit_before_return: bool,
}

#[derive(Debug)]
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

    async fn is_exited(&self) -> Result<bool> {
        let mut inner = self.inner.lock().await;
        Ok(inner.child.try_wait()?.is_some())
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
                .append("\n[zodexd] process terminated by kill_process\n")
                .await;
            info!(
                event = "session_killed",
                internal_session_id = self.internal_session_id,
                session_handle_prefix = self.handle_prefix(),
                transport = self.transport.as_str(),
                command_summary = self.command_summary(),
            );
        } else if let Some(chars) = input.chars.as_deref() {
            let mut pty_writer = {
                let mut inner = self.inner.lock().await;
                inner.pty_writer.take()
            };

            if let Some(writer) = pty_writer.as_mut() {
                writer
                    .write_all(chars.as_bytes())
                    .await
                    .context("failed to write stdin")?;
                writer.flush().await.context("failed to flush stdin")?;
            }

            let mut inner = self.inner.lock().await;
            inner.pty_writer = pty_writer;
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

                maybe_force_kill(&mut inner);
                if let Some(live_cwd) = resolve_live_cwd(inner.pid) {
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

                match inner.child.try_wait()? {
                    Some(status) => {
                        let code = status.code().unwrap_or(-1);
                        let termination_reason = if inner.timed_out {
                            TerminationReason::Timeout
                        } else if inner.kill_requested || inner.force_killed {
                            TerminationReason::Killed
                        } else {
                            TerminationReason::Exit
                        };
                        finished = Some((code, inner.last_known_cwd.clone(), termination_reason));
                    }
                    None if started.elapsed() >= yield_for && !inner.require_exit_before_return => {
                        running_cwd = Some(inner.last_known_cwd.clone());
                    }
                    None => {}
                }
            }

            if timeout_notice {
                self.output.append(TIMEOUT_NOTICE).await;
            }

            if let Some((exit_code, cwd, termination_reason)) = finished {
                let text = strip_ansi_codes(snapshot_output_after_exit(&self.output).await);
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
                    status: CommandStatus::Exited,
                    cwd,
                    session_id: None,
                    session_handle: None,
                    exit_code: Some(exit_code),
                    termination_reason: Some(termination_reason),
                });
            }

            if let Some(cwd) = running_cwd {
                let text = strip_ansi_codes(self.output.snapshot().await);
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
                    status: CommandStatus::Running,
                    cwd,
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

#[derive(Debug)]
pub struct SessionManager {
    sessions: RwLock<HashMap<String, Arc<SessionRuntime>>>,
    next_internal_session_id: AtomicU64,
    max_sessions: usize,
    max_output_chars: usize,
    poll_interval: Duration,
}

impl SessionManager {
    pub fn new(max_sessions: usize, max_output_chars: usize) -> Self {
        Self {
            sessions: RwLock::new(HashMap::new()),
            next_internal_session_id: AtomicU64::new(1),
            max_sessions,
            max_output_chars,
            poll_interval: Duration::from_millis(POLL_INTERVAL_MS),
        }
    }

    pub async fn exec_command(
        &self,
        input: ExecCommandInput,
        cfg: &Config,
        origin: SessionOrigin,
    ) -> Result<ToolOutput> {
        self.evict_if_needed().await?;

        let timeout_ms = cfg.clamp_exec_timeout_ms(input.timeout_ms);
        let yield_time_ms = cfg.clamp_exec_yield_ms(input.yield_time_ms);
        let now = Instant::now();
        let now_system = SystemTime::now();

        let command_cwd = resolve_command_cwd(input.workdir.as_deref(), cfg)?;
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

        let mut command = Command::new("/bin/bash");
        command.arg("-lc").arg(&input.cmd);

        #[cfg(unix)]
        command
            .stdin(Stdio::from(slave_stdin))
            .stdout(Stdio::from(slave_stdout))
            .stderr(Stdio::from(slave_file));

        #[cfg(unix)]
        unsafe {
            command.pre_exec(|| {
                setpgid(Pid::from_raw(0), Pid::from_raw(0))
                    .map_err(|e| std::io::Error::other(e.to_string()))?;
                Ok(())
            });
        }

        command.current_dir(&command_cwd);
        if !cfg.agent_home.trim().is_empty() {
            command.env("HOME", &cfg.agent_home);
        }
        command.env("USER", &cfg.agent_user);
        command.env("LOGNAME", &cfg.agent_user);
        command.env("PAGER", "cat");
        command.env("GIT_PAGER", "cat");
        command.env("LESS", "FRX");
        command.env("MANPAGER", "cat");
        command.env("SYSTEMD_PAGER", "cat");

        let child = command
            .spawn()
            .with_context(|| format!("failed to spawn command: {}", input.cmd))?;
        let output = Arc::new(OutputBuffer::new(self.max_output_chars));

        #[cfg(unix)]
        let master_reader_std = master_file
            .try_clone()
            .context("failed to clone PTY master for reader")?;
        #[cfg(unix)]
        let master_writer_std = master_file;
        #[cfg(unix)]
        let master_reader = tokio::fs::File::from_std(master_reader_std);
        #[cfg(unix)]
        let master_writer = tokio::fs::File::from_std(master_writer_std);
        #[cfg(unix)]
        spawn_reader(master_reader, output.clone());

        let internal_session_id = self
            .next_internal_session_id
            .fetch_add(1, Ordering::Relaxed);
        let session_handle = generate_session_handle();
        let pid = child
            .id()
            .ok_or_else(|| anyhow!("failed to obtain child process id"))? as i32;

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
                pty_writer: Some(master_writer),
                last_used_at: now_system,
                last_input_at: now,
                idle_timeout: Duration::from_millis(timeout_ms),
                timed_out: false,
                kill_requested: false,
                terminate_started_at: None,
                force_killed: false,
                require_exit_before_return: false,
            }),
        });

        {
            let mut sessions = self.sessions.write().await;
            sessions.insert(session_handle.clone(), runtime.clone());
        }

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

        if output.status == CommandStatus::Exited {
            self.remove_session(&session_handle).await;
        }

        Ok(output)
    }

    pub async fn write_stdin(&self, input: WriteStdinInput, cfg: &Config) -> Result<ToolOutput> {
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

        if output.status == CommandStatus::Exited {
            self.remove_session(&session_handle).await;
        }

        Ok(output)
    }

    async fn remove_session(&self, session_handle: &str) {
        let removed = {
            let mut sessions = self.sessions.write().await;
            sessions.remove(session_handle)
        };

        if let Some(runtime) = removed {
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

            let mut oldest_any: Option<(String, SystemTime, Arc<SessionRuntime>)> = None;
            let mut oldest_exited: Option<(String, SystemTime, Arc<SessionRuntime>)> = None;

            for (handle, runtime) in candidates {
                let last_used = runtime.last_used_at().await;
                if oldest_any
                    .as_ref()
                    .map(|(_, ts, _)| last_used < *ts)
                    .unwrap_or(true)
                {
                    oldest_any = Some((handle.clone(), last_used, runtime.clone()));
                }

                if runtime.is_exited().await?
                    && oldest_exited
                        .as_ref()
                        .map(|(_, ts, _)| last_used < *ts)
                        .unwrap_or(true)
                {
                    oldest_exited = Some((handle, last_used, runtime));
                }
            }

            let evict = oldest_exited
                .map(|(handle, _, _)| handle)
                .or_else(|| oldest_any.map(|(handle, _, _)| handle));

            if let Some(handle) = evict {
                self.remove_session(&handle).await;
            } else {
                return Ok(());
            }
        }
    }
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

fn resolve_command_cwd(requested_workdir: Option<&str>, cfg: &Config) -> Result<PathBuf> {
    if let Some(workdir) = requested_workdir {
        return Ok(PathBuf::from(workdir));
    }

    if !cfg.default_workdir.trim().is_empty() {
        let path = PathBuf::from(&cfg.default_workdir);
        if path.is_dir() {
            return Ok(path);
        }
    }

    if !cfg.agent_home.trim().is_empty() {
        let path = PathBuf::from(&cfg.agent_home);
        if path.is_dir() {
            return Ok(path);
        }
    }

    std::env::current_dir().context("failed to resolve current directory")
}

fn request_termination(inner: &mut SessionInner) {
    if inner.terminate_started_at.is_some() {
        return;
    }

    inner.terminate_started_at = Some(Instant::now());
    #[cfg(unix)]
    {
        let _ = signal_process_group(inner.pid, Signal::SIGTERM);
    }
    #[cfg(not(unix))]
    {
        let _ = inner.child.start_kill();
    }
}

async fn snapshot_output_after_exit(output: &Arc<OutputBuffer>) -> String {
    let mut snapshot = output.snapshot().await;
    for _ in 0..EXIT_OUTPUT_DRAIN_RETRIES {
        tokio::time::sleep(Duration::from_millis(EXIT_OUTPUT_DRAIN_DELAY_MS)).await;
        let refreshed = output.snapshot().await;
        if refreshed == snapshot {
            break;
        }
        snapshot = refreshed;
    }
    snapshot
}

fn maybe_force_kill(inner: &mut SessionInner) {
    let Some(started) = inner.terminate_started_at else {
        return;
    };
    if inner.force_killed {
        return;
    }
    if started.elapsed() < Duration::from_millis(TERMINATE_GRACE_PERIOD_MS) {
        return;
    }

    inner.force_killed = true;
    #[cfg(unix)]
    {
        let _ = signal_process_group(inner.pid, Signal::SIGKILL);
    }
    #[cfg(not(unix))]
    {
        let _ = inner.child.start_kill();
    }
}

#[cfg(unix)]
fn signal_process_group(pid: i32, signal: Signal) -> Result<()> {
    match killpg(Pid::from_raw(pid), signal) {
        Ok(_) => Ok(()),
        Err(Errno::ESRCH) => Ok(()),
        Err(e) => Err(anyhow!(
            "failed to send {signal:?} to process group {pid}: {e}"
        )),
    }
}

fn spawn_reader<R>(mut reader: R, output: Arc<OutputBuffer>)
where
    R: AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut buf = [0_u8; 8192];
        loop {
            let read = match reader.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => n,
                Err(_) => break,
            };

            let chunk = String::from_utf8_lossy(&buf[..read]);
            output.append(&chunk).await;
        }
    });
}

fn unknown_session_handle(session_handle: &str) -> anyhow::Error {
    anyhow!("Unknown session handle: {session_handle}")
}

fn resolve_live_cwd(pid: i32) -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        let target_pgrp = read_proc_pgrp(pid)?;
        let mut best: Option<(i32, String)> = None;

        let proc_entries = std::fs::read_dir("/proc").ok()?;
        for entry in proc_entries {
            let Ok(entry) = entry else {
                continue;
            };
            let name = entry.file_name();
            let raw = name.to_string_lossy();
            if !raw.chars().all(|ch| ch.is_ascii_digit()) {
                continue;
            }

            let proc_pid = match raw.parse::<i32>() {
                Ok(v) => v,
                Err(_) => continue,
            };

            if read_proc_pgrp(proc_pid) != Some(target_pgrp) {
                continue;
            }

            let Some(cwd) = read_proc_cwd(proc_pid) else {
                continue;
            };
            if best
                .as_ref()
                .map(|(best_pid, _)| proc_pid > *best_pid)
                .unwrap_or(true)
            {
                best = Some((proc_pid, cwd));
            }
        }

        if let Some((_, cwd)) = best {
            Some(cwd)
        } else {
            read_proc_cwd(pid)
        }
    }

    #[cfg(not(target_os = "linux"))]
    {
        let _ = pid;
        None
    }
}

#[cfg(target_os = "linux")]
fn read_proc_cwd(pid: i32) -> Option<String> {
    let path = format!("/proc/{pid}/cwd");
    let cwd = std::fs::read_link(path).ok()?;
    Some(cwd.display().to_string())
}

#[cfg(target_os = "linux")]
fn read_proc_pgrp(pid: i32) -> Option<i32> {
    let stat_path = format!("/proc/{pid}/stat");
    let raw = std::fs::read_to_string(stat_path).ok()?;
    let (_, after_comm) = raw.rsplit_once(") ")?;
    let mut fields = after_comm.split_whitespace();
    let _state = fields.next()?;
    let _ppid = fields.next()?;
    let pgrp = fields.next()?.parse::<i32>().ok()?;
    Some(pgrp)
}

#[cfg(test)]
mod tests;
