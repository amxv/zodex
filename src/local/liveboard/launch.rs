#[cfg(target_os = "macos")]
use std::process::Command;

#[cfg(target_os = "macos")]
use anyhow::{Context, anyhow};
use anyhow::{Result, bail};

use super::super::LocalPaths;
#[cfg(target_os = "macos")]
use super::super::{LocalRuntimeLifecycle, load_runtime_discovery, load_runtime_state};
#[cfg(target_os = "macos")]
use super::discovery::{load_liveboard_discovery, validate_agent_id};

#[cfg(target_os = "macos")]
pub(crate) trait BrowserLauncher: Send + Sync {
    fn open(&self, url: &str) -> Result<()>;
}

#[cfg(target_os = "macos")]
struct SystemBrowserLauncher;

#[cfg(target_os = "macos")]
impl BrowserLauncher for SystemBrowserLauncher {
    fn open(&self, url: &str) -> Result<()> {
        let status = Command::new("/usr/bin/open")
            .arg(url)
            .status()
            .context("failed to launch the default browser with /usr/bin/open")?;
        if !status.success() {
            bail!("/usr/bin/open exited with status {status}");
        }
        Ok(())
    }
}

#[cfg(target_os = "macos")]
pub async fn run_local_liveboard(paths: &LocalPaths, agent_id: Option<&str>) -> Result<()> {
    run_local_liveboard_with_launcher(paths, agent_id, Some(&SystemBrowserLauncher)).await
}

#[cfg(not(target_os = "macos"))]
pub async fn run_local_liveboard(_paths: &LocalPaths, _agent_id: Option<&str>) -> Result<()> {
    bail!("Zodex Local Liveboard is only available on macOS")
}

#[cfg(target_os = "macos")]
pub async fn run_local_liveboard_without_open(
    paths: &LocalPaths,
    agent_id: Option<&str>,
) -> Result<()> {
    run_local_liveboard_with_launcher(paths, agent_id, None).await
}

#[cfg(not(target_os = "macos"))]
pub async fn run_local_liveboard_without_open(
    _paths: &LocalPaths,
    _agent_id: Option<&str>,
) -> Result<()> {
    bail!("Zodex Local Liveboard is only available on macOS")
}

#[cfg(target_os = "macos")]
pub(crate) async fn run_local_liveboard_with_launcher(
    paths: &LocalPaths,
    agent_id: Option<&str>,
    launcher: Option<&dyn BrowserLauncher>,
) -> Result<()> {
    if let Some(agent_id) = agent_id {
        validate_agent_id(agent_id)?;
    }
    let runtime = load_runtime_state(paths)?
        .context("Zodex Local is not running: runtime state is unavailable")?;
    if runtime.lifecycle != LocalRuntimeLifecycle::Ready {
        bail!("Zodex Local is not ready; inspect `zodex local status`")
    }
    let discovery = load_runtime_discovery(paths)?
        .context("Zodex Local is not ready: active runtime discovery is unavailable")?;
    if discovery.runtime_id != runtime.runtime_id {
        bail!("Zodex Local runtime discovery is stale; restart Local")
    }
    let liveboard = load_liveboard_discovery(paths, &runtime.runtime_id)?;
    let url = match agent_id {
        Some(agent_id) => liveboard.focused_url(agent_id)?,
        None => liveboard.base_url.clone(),
    };
    probe_liveboard(&url).await?;

    println!("Liveboard: {url}");
    if let Some(launcher) = launcher
        && let Err(error) = launcher.open(&url)
    {
        eprintln!(
            "warning: could not open the default browser automatically: {error:#}. Use the Liveboard URL printed above."
        );
    }
    Ok(())
}

#[cfg(target_os = "macos")]
async fn probe_liveboard(url: &str) -> Result<()> {
    let response = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| anyhow!("failed to construct Liveboard readiness client"))?
        .get(url)
        .send()
        .await
        .map_err(|_| anyhow!("Local Liveboard host is unavailable; restart Zodex Local"))?;
    if !response.status().is_success() {
        bail!("Local Liveboard host rejected its stable local URL; restart Zodex Local")
    }
    Ok(())
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use std::sync::Mutex;

    use anyhow::Result;

    use super::BrowserLauncher;

    struct RecordingLauncher {
        urls: Mutex<Vec<String>>,
        fail: bool,
    }

    impl BrowserLauncher for RecordingLauncher {
        fn open(&self, url: &str) -> Result<()> {
            self.urls.lock().unwrap().push(url.to_string());
            if self.fail {
                anyhow::bail!("browser unavailable")
            }
            Ok(())
        }
    }

    #[test]
    fn browser_launcher_abstraction_records_capability_url_and_can_fail_without_panicking() {
        let launcher = RecordingLauncher {
            urls: Mutex::new(Vec::new()),
            fail: true,
        };
        assert!(launcher.open("http://127.0.0.1:1234/capability/").is_err());
        assert_eq!(
            launcher.urls.lock().unwrap().as_slice(),
            ["http://127.0.0.1:1234/capability/"]
        );
    }
}
