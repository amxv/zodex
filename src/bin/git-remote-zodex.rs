use std::env;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

use anyhow::{Context, Result, anyhow, bail};
use zodex::config::DEFAULT_CONFIG_PATH;

fn resolve_agent_binary() -> Result<PathBuf> {
    let current_exe = env::current_exe().context("failed to resolve current executable path")?;
    if let Some(parent) = current_exe.parent() {
        let sibling = parent.join("zodex-agent");
        if sibling.is_file() {
            return Ok(sibling);
        }
    }

    let fallback = PathBuf::from("/usr/local/bin/zodex-agent");
    if fallback.is_file() {
        return Ok(fallback);
    }

    if command_exists("zodex-agent") {
        return Ok(PathBuf::from("zodex-agent"));
    }

    bail!(
        "failed to locate zodex-agent; expected a sibling binary, /usr/local/bin/zodex-agent, or PATH entry"
    );
}

fn command_exists(program: &str) -> bool {
    let Some(paths) = env::var_os("PATH") else {
        return false;
    };
    env::split_paths(&paths).any(|dir| Path::new(&dir).join(program).is_file())
}

fn run() -> Result<ExitCode> {
    let mut args = env::args().skip(1);
    let remote = args
        .next()
        .ok_or_else(|| anyhow!("git-remote-zodex requires a remote name argument"))?;
    let url = args
        .next()
        .ok_or_else(|| anyhow!("git-remote-zodex requires a URL argument"))?;

    let agent = resolve_agent_binary()?;
    let status = Command::new(&agent)
        .arg("--config")
        .arg(env::var("ZODEX_CONFIG_PATH").unwrap_or_else(|_| DEFAULT_CONFIG_PATH.to_string()))
        .arg("git-remote-zodex")
        .arg(remote)
        .arg(url)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .with_context(|| format!("failed to execute {}", agent.display()))?;

    if let Some(code) = status.code() {
        return Ok(ExitCode::from(code as u8));
    }

    Err(anyhow!(
        "{} terminated without an exit code",
        agent.display()
    ))
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::FAILURE
        }
    }
}
