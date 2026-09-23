use std::io::Write as _;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, anyhow, bail};

use super::super::{LocalPaths, LocalRuntimeLifecycle, load_runtime_discovery, load_runtime_state};
use super::discovery::{load_liveboard_discovery, validate_agent_id};

pub async fn local_liveboard_url(paths: &LocalPaths, agent_id: Option<&str>) -> Result<String> {
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
    Ok(url)
}

pub async fn run_local_liveboard(paths: &LocalPaths, agent_id: Option<&str>) -> Result<()> {
    let url = local_liveboard_url(paths, agent_id).await?;
    println!("Liveboard: {url}");
    if let Err(error) = open_browser(&url) {
        eprintln!(
            "warning: could not open the default browser automatically: {error:#}. Use the Liveboard URL printed above."
        );
    }
    Ok(())
}

pub async fn copy_local_liveboard_url(
    paths: &LocalPaths,
    agent_id: Option<&str>,
) -> Result<String> {
    let url = local_liveboard_url(paths, agent_id).await?;
    copy_to_clipboard(&url)?;
    Ok(url)
}

fn open_browser(url: &str) -> Result<()> {
    let attempts = browser_open_attempts(url);
    let mut last_error = None;
    for (program, args) in attempts {
        match Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
        {
            Ok(status) if status.success() => return Ok(()),
            Ok(status) => last_error = Some(anyhow!("{program} exited with {status}")),
            Err(error) => {
                last_error = Some(anyhow!(error).context(format!("failed to start {program}")))
            }
        }
    }
    Err(last_error.unwrap_or_else(|| anyhow!("no supported browser launcher is available")))
}

fn browser_open_attempts(url: &str) -> Vec<(&'static str, Vec<String>)> {
    #[cfg(target_os = "macos")]
    {
        return vec![("/usr/bin/open", vec![url.to_string()])];
    }
    #[cfg(target_os = "windows")]
    {
        return vec![(
            "cmd.exe",
            vec!["/C".into(), "start".into(), "".into(), url.to_string()],
        )];
    }
    #[cfg(target_os = "linux")]
    {
        return vec![
            ("xdg-open", vec![url.to_string()]),
            ("gio", vec!["open".into(), url.to_string()]),
        ];
    }
    #[allow(unreachable_code)]
    Vec::new()
}

fn copy_to_clipboard(text: &str) -> Result<()> {
    let attempts = clipboard_copy_attempts();
    let mut last_error = None;
    for (program, args) in attempts {
        let mut child = match Command::new(program)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(child) => child,
            Err(error) => {
                last_error = Some(anyhow!(error).context(format!("failed to start {program}")));
                continue;
            }
        };
        if let Some(stdin) = child.stdin.as_mut()
            && let Err(error) = stdin.write_all(text.as_bytes())
        {
            last_error = Some(anyhow!(error).context(format!("failed to write to {program}")));
            let _ = child.kill();
            let _ = child.wait();
            continue;
        }
        match child.wait() {
            Ok(status) if status.success() => return Ok(()),
            Ok(status) => last_error = Some(anyhow!("{program} exited with {status}")),
            Err(error) => {
                last_error = Some(anyhow!(error).context(format!("failed to wait for {program}")))
            }
        }
    }
    Err(last_error.unwrap_or_else(|| anyhow!("no supported clipboard helper is available")))
}

fn clipboard_copy_attempts() -> Vec<(&'static str, Vec<&'static str>)> {
    #[cfg(target_os = "macos")]
    {
        return vec![("/usr/bin/pbcopy", vec![])];
    }
    #[cfg(target_os = "windows")]
    {
        return vec![("clip.exe", vec![])];
    }
    #[cfg(target_os = "linux")]
    {
        return vec![
            ("wl-copy", vec![]),
            ("xclip", vec!["-selection", "clipboard"]),
            ("xsel", vec!["--clipboard", "--input"]),
        ];
    }
    #[allow(unreachable_code)]
    Vec::new()
}

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

#[cfg(test)]
mod tests {
    use super::{browser_open_attempts, clipboard_copy_attempts};

    #[test]
    fn supported_platform_has_browser_and_clipboard_attempts() {
        if cfg!(any(
            target_os = "macos",
            target_os = "windows",
            target_os = "linux"
        )) {
            assert!(!browser_open_attempts("http://127.0.0.1:64973/").is_empty());
            assert!(!clipboard_copy_attempts().is_empty());
        }
    }
}
