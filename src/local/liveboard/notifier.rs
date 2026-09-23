use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use tracing::warn;

use super::discovery::{LocalLiveboardDiscovery, validate_agent_id};

const COALESCE_WINDOW: Duration = Duration::from_millis(40);
const COPY_PROCESS_TIMEOUT: Duration = Duration::from_secs(2);
const COPY_PROCESS_POLL: Duration = Duration::from_millis(10);
const NOTIFIER_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(6);

trait LinkCopier: Send + Sync {
    fn copy(&self, agent_id: &str, url: &str) -> Result<()>;
}

trait CopyProcessRunner: Send + Sync {
    fn run(&self, executable: &Path, arguments: &[&str], stdin: &str) -> Result<()>;
}

struct SystemCopyProcessRunner;

impl CopyProcessRunner for SystemCopyProcessRunner {
    fn run(&self, executable: &Path, arguments: &[&str], stdin: &str) -> Result<()> {
        run_copy_process(executable, arguments, stdin)
    }
}

struct SystemLinkCopier {
    helper: Option<PathBuf>,
    runner: Arc<dyn CopyProcessRunner>,
}

impl SystemLinkCopier {
    fn discover() -> Self {
        let helper = std::env::current_exe()
            .ok()
            .and_then(|path| path.canonicalize().ok())
            .and_then(|path| path.parent().map(Path::to_path_buf))
            .map(|parent| parent.join("Zodex.app/Contents/MacOS/zodex-menubar"))
            .filter(|path| path.is_file());
        Self {
            helper,
            runner: Arc::new(SystemCopyProcessRunner),
        }
    }

    #[cfg(test)]
    fn with_runner(helper: Option<PathBuf>, runner: Arc<dyn CopyProcessRunner>) -> Self {
        Self { helper, runner }
    }
}

impl LinkCopier for SystemLinkCopier {
    fn copy(&self, agent_id: &str, url: &str) -> Result<()> {
        if let Some(helper) = self.helper.as_deref()
            && self
                .runner
                .run(helper, &["--copy-liveboard-link", "--agent", agent_id], url)
                .is_ok()
        {
            return Ok(());
        }
        self.runner
            .run(Path::new("/usr/bin/pbcopy"), &[], url)
            .context("focused Liveboard clipboard fallback failed")
    }
}

struct PendingState {
    accepting: bool,
    agent_id: Option<String>,
}

struct Shared {
    discovery: LocalLiveboardDiscovery,
    state: Mutex<PendingState>,
    changed: Condvar,
}

pub(crate) struct LiveboardLinkNotifier {
    shared: Arc<Shared>,
    worker: Mutex<Option<JoinHandle<()>>>,
    done: Mutex<Option<Receiver<()>>>,
}

impl LiveboardLinkNotifier {
    pub(crate) fn start(discovery: LocalLiveboardDiscovery) -> Result<Self> {
        Self::start_with_copier(discovery, Arc::new(SystemLinkCopier::discover()))
    }

    fn start_with_copier(
        discovery: LocalLiveboardDiscovery,
        copier: Arc<dyn LinkCopier>,
    ) -> Result<Self> {
        let shared = Arc::new(Shared {
            discovery,
            state: Mutex::new(PendingState {
                accepting: true,
                agent_id: None,
            }),
            changed: Condvar::new(),
        });
        let worker_shared = shared.clone();
        let (done_tx, done_rx) = mpsc::sync_channel(1);
        let worker = std::thread::Builder::new()
            .name("zodex-liveboard-link-notifier".to_string())
            .spawn(move || {
                run_worker(worker_shared, copier);
                let _ = done_tx.send(());
            })
            .context("failed to start Local Liveboard link notifier")?;
        Ok(Self {
            shared,
            worker: Mutex::new(Some(worker)),
            done: Mutex::new(Some(done_rx)),
        })
    }

    pub(crate) fn observer(&self) -> Arc<dyn Fn(String) + Send + Sync> {
        let shared = self.shared.clone();
        Arc::new(move |agent_id| enqueue(&shared, agent_id))
    }

    pub(crate) fn stop_accepting(&self) {
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.accepting = false;
        state.agent_id = None;
        self.shared.changed.notify_all();
    }

    pub(crate) fn shutdown(self) -> Result<()> {
        self.stop_accepting();
        let done = self
            .done
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(done) = done
            && done.recv_timeout(NOTIFIER_SHUTDOWN_TIMEOUT).is_err()
        {
            // Dropping the JoinHandle detaches a pathological worker instead
            // of allowing auxiliary clipboard UI to stall Local shutdown.
            self.worker
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take();
            bail!("Local Liveboard link notifier exceeded its bounded shutdown deadline");
        }
        if let Some(worker) = self
            .worker
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            worker
                .join()
                .map_err(|_| anyhow::anyhow!("Local Liveboard link notifier thread panicked"))?;
        }
        Ok(())
    }
}

fn enqueue(shared: &Shared, agent_id: String) {
    if validate_agent_id(&agent_id).is_err() {
        return;
    }
    let mut state = shared
        .state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if !state.accepting {
        return;
    }
    // One replaceable slot deliberately collapses bursts. A clipboard can
    // represent only one link, so the most recently attributed conversation
    // is the useful paste target.
    state.agent_id = Some(agent_id);
    shared.changed.notify_one();
}

fn run_worker(shared: Arc<Shared>, copier: Arc<dyn LinkCopier>) {
    loop {
        let agent_id = {
            let mut state = shared
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            while state.accepting && state.agent_id.is_none() {
                state = shared
                    .changed
                    .wait(state)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
            }
            if !state.accepting {
                return;
            }
            let (mut state, _) = shared
                .changed
                .wait_timeout(state, COALESCE_WINDOW)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if !state.accepting {
                return;
            }
            state.agent_id.take()
        };

        let Some(agent_id) = agent_id else {
            continue;
        };
        let url = match shared.discovery.focused_url(&agent_id) {
            Ok(url) => url,
            Err(_) => continue,
        };
        if copier.copy(&agent_id, &url).is_err() {
            warn!(
                event = "local_liveboard_link_copy_failed",
                agent_id = %agent_id,
                "could not copy the focused Local Liveboard link"
            );
        }
    }
}

fn run_copy_process(executable: &Path, arguments: &[&str], url: &str) -> Result<()> {
    let mut child = Command::new(executable)
        .args(arguments)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("failed to start clipboard helper")?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(url.as_bytes())
            .context("failed to send clipboard helper input")?;
    }
    let deadline = Instant::now() + COPY_PROCESS_TIMEOUT;
    loop {
        if let Some(status) = child
            .try_wait()
            .context("failed to inspect clipboard helper")?
        {
            if status.success() {
                return Ok(());
            }
            bail!("clipboard helper exited unsuccessfully")
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            bail!("clipboard helper exceeded its bounded deadline")
        }
        std::thread::sleep(COPY_PROCESS_POLL);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;

    fn discovery() -> LocalLiveboardDiscovery {
        LocalLiveboardDiscovery::new("runtime-a", "http://127.0.0.1:43123/").unwrap()
    }

    struct FakeLinkCopier {
        delay: Duration,
        calls: Mutex<Vec<(String, String)>>,
    }

    impl FakeLinkCopier {
        fn wait_for_calls(&self, count: usize) {
            let deadline = Instant::now() + Duration::from_secs(1);
            while self.calls.lock().unwrap().len() < count && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }

    impl LinkCopier for FakeLinkCopier {
        fn copy(&self, agent_id: &str, url: &str) -> Result<()> {
            std::thread::sleep(self.delay);
            self.calls
                .lock()
                .unwrap()
                .push((agent_id.to_string(), url.to_string()));
            Ok(())
        }
    }

    #[test]
    fn notifier_is_latest_wins_and_observer_never_waits_for_clipboard_work() {
        let copier = Arc::new(FakeLinkCopier {
            delay: Duration::from_millis(150),
            calls: Mutex::new(Vec::new()),
        });
        let notifier =
            LiveboardLinkNotifier::start_with_copier(discovery(), copier.clone()).unwrap();
        let observer = notifier.observer();

        let started = Instant::now();
        observer("a111".to_string());
        observer("b222".to_string());
        observer("INVALID".to_string());
        assert!(started.elapsed() < Duration::from_millis(25));

        copier.wait_for_calls(1);
        let calls = copier.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "b222");
        assert_eq!(calls[0].1, "http://127.0.0.1:43123/?agent=b222");
        drop(calls);
        notifier.shutdown().unwrap();
    }

    #[derive(Debug, PartialEq, Eq)]
    struct ProcessCall {
        executable: PathBuf,
        arguments: Vec<String>,
        stdin: String,
    }

    struct RecordingProcessRunner {
        fail_helper: AtomicBool,
        calls: Mutex<Vec<ProcessCall>>,
    }

    impl CopyProcessRunner for RecordingProcessRunner {
        fn run(&self, executable: &Path, arguments: &[&str], stdin: &str) -> Result<()> {
            self.calls.lock().unwrap().push(ProcessCall {
                executable: executable.to_path_buf(),
                arguments: arguments.iter().map(|value| (*value).to_string()).collect(),
                stdin: stdin.to_string(),
            });
            if executable != Path::new("/usr/bin/pbcopy") && self.fail_helper.load(Ordering::SeqCst)
            {
                bail!("synthetic helper failure")
            }
            Ok(())
        }
    }

    #[test]
    fn helper_gets_agent_only_argv_url_only_on_stdin_and_pbcopy_is_fallback() {
        let runner = Arc::new(RecordingProcessRunner {
            fail_helper: AtomicBool::new(false),
            calls: Mutex::new(Vec::new()),
        });
        let helper = PathBuf::from("/fake/Zodex.app/Contents/MacOS/zodex-menubar");
        let copier = SystemLinkCopier::with_runner(Some(helper.clone()), runner.clone());
        let url = "http://127.0.0.1:43123/?agent=k7m2";
        copier.copy("k7m2", url).unwrap();

        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].executable, helper);
        assert_eq!(
            calls[0].arguments,
            ["--copy-liveboard-link", "--agent", "k7m2"]
        );
        assert_eq!(calls[0].stdin, url);
        assert!(
            !calls[0]
                .arguments
                .iter()
                .any(|argument| argument.contains("127.0.0.1"))
        );
        drop(calls);

        runner.fail_helper.store(true, Ordering::SeqCst);
        copier.copy("k7m2", url).unwrap();
        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls.len(), 3);
        assert_eq!(calls[2].executable, Path::new("/usr/bin/pbcopy"));
        assert!(calls[2].arguments.is_empty());
        assert_eq!(calls[2].stdin, url);
    }

    #[test]
    fn stopped_notifier_ignores_late_notifications() {
        let copier = Arc::new(FakeLinkCopier {
            delay: Duration::ZERO,
            calls: Mutex::new(Vec::new()),
        });
        let notifier =
            LiveboardLinkNotifier::start_with_copier(discovery(), copier.clone()).unwrap();
        let observer = notifier.observer();
        notifier.stop_accepting();
        observer("c333".to_string());
        std::thread::sleep(Duration::from_millis(60));
        assert!(copier.calls.lock().unwrap().is_empty());
        notifier.shutdown().unwrap();
    }
}
