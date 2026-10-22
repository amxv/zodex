use serde::Deserialize;
use std::{
    io::{Read as _, Seek as _},
    path::Path,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

#[derive(Clone, Debug)]
pub struct CommandResult {
    pub ok: bool,
    pub output: String,
}

#[derive(Debug, Deserialize)]
pub struct Status {
    pub state: String,
}

#[derive(Debug, Deserialize)]
pub struct UpgradeEvent {
    pub event: String,
    pub current_version: String,
    pub target_version: Option<String>,
    pub update_available: Option<bool>,
    pub local_blocks_upgrade: Option<bool>,
    pub code: Option<String>,
    pub message: String,
}

pub fn run(executable: &Path, arguments: &[&str], start: bool) -> CommandResult {
    // Work queued before Windows notified us of logoff may run on a worker
    // after the notification. Never start another process at that point:
    // Windows may reject its DLL initialization with 0xc0000142.
    #[cfg(target_os = "windows")]
    if crate::windows_session::launches_paused() {
        return CommandResult {
            ok: false,
            output: "Windows is suspending or ending the session; Zodex did not launch a command"
                .into(),
        };
    }
    let mut command = Command::new(executable);
    command.args(arguments);
    command.stdin(Stdio::null());
    if start {
        #[cfg(target_os = "linux")]
        match crate::platform::developer_environment() {
            Ok(environment) => {
                command.env_clear();
                command.envs(environment);
            }
            Err(error) => {
                return CommandResult {
                    ok: false,
                    output: format!("Could not capture your login-shell environment: {error}"),
                };
            }
        }
    }
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    // The Local start/stop command can spawn a detached process that holds
    // inherited stdout/stderr handles. Command::output waits for pipe EOF,
    // not just the CLI process to exit, so the tray could show "Starting…"
    // indefinitely even after status is already "running". A temporary file
    // lets us retain errors while waiting only for the direct CLI process.
    let lifecycle = matches!(arguments, ["local", "start", ..] | ["local", "stop"]);
    let status_query = matches!(arguments, ["local", "status", "--json"]);
    if lifecycle || status_query {
        let mut log = match tempfile::tempfile() {
            Ok(file) => file,
            Err(error) => {
                return CommandResult {
                    ok: false,
                    output: format!("Could not capture Zodex lifecycle diagnostics: {error}"),
                };
            }
        };
        let stdout = match log.try_clone() {
            Ok(file) => file,
            Err(error) => {
                return CommandResult {
                    ok: false,
                    output: error.to_string(),
                };
            }
        };
        let stderr = match log.try_clone() {
            Ok(file) => file,
            Err(error) => {
                return CommandResult {
                    ok: false,
                    output: error.to_string(),
                };
            }
        };
        command.stdout(Stdio::from(stdout));
        command.stderr(Stdio::from(stderr));
        #[cfg(target_os = "windows")]
        if crate::windows_session::launches_paused() {
            return CommandResult {
                ok: false,
                output:
                    "Windows is suspending or ending the session; Zodex did not launch a command"
                        .into(),
            };
        }
        let wait_result = if status_query {
            // A wedged status CLI must not pin status_in_flight forever.
            // The command is read-only, so it is safe to terminate on timeout.
            (|| {
                let mut child = command.spawn()?;
                let deadline = Instant::now() + Duration::from_secs(6);
                loop {
                    #[cfg(target_os = "windows")]
                    if crate::windows_session::launches_paused() {
                        let _ = child.kill();
                        let _ = child.wait();
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::Interrupted,
                            "Windows is suspending or ending the session",
                        ));
                    }
                    if let Some(exit) = child.try_wait()? {
                        return Ok(exit);
                    }
                    if Instant::now() >= deadline {
                        let _ = child.kill();
                        let _ = child.wait();
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::TimedOut,
                            "Zodex status check took more than 6 seconds",
                        ));
                    }
                    thread::sleep(Duration::from_millis(30));
                }
            })()
        } else {
            command.status()
        };
        return match wait_result {
            Ok(status) => {
                let _ = log.rewind();
                let mut output = String::new();
                let _ = log.take(64 * 1024).read_to_string(&mut output);
                CommandResult {
                    ok: status.success(),
                    output,
                }
            }
            Err(error) => CommandResult {
                ok: false,
                output: format!("Could not execute {}: {error}", executable.display()),
            },
        };
    }
    #[cfg(target_os = "windows")]
    if crate::windows_session::launches_paused() {
        return CommandResult {
            ok: false,
            output: "Windows is suspending or ending the session; Zodex did not launch a command"
                .into(),
        };
    }
    match command.output() {
        Ok(output) => CommandResult {
            ok: output.status.success(),
            output: format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ),
        },
        Err(error) => CommandResult {
            ok: false,
            output: format!("Could not execute {}: {error}", executable.display()),
        },
    }
}

#[cfg(test)]
mod process_tests {
    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    fn waiting_for_cli_does_not_wait_for_descendant_inherited_stdout() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "(sleep 3) & echo 'CLI finished'"]);
        let mut output_file = tempfile::tempfile().unwrap();
        command.stdout(Stdio::from(output_file.try_clone().unwrap()));
        command.stderr(Stdio::from(output_file.try_clone().unwrap()));
        let started = Instant::now();
        let exit = command.status().unwrap();
        assert!(exit.success());
        assert!(started.elapsed() < Duration::from_secs(2));
        output_file.rewind().unwrap();
        let mut output = String::new();
        output_file.read_to_string(&mut output).unwrap();
        assert!(output.contains("CLI finished"));
    }
}

pub fn status(result: &CommandResult) -> Option<Status> {
    result
        .ok
        .then(|| serde_json::from_str(&result.output).ok())
        .flatten()
}

pub fn upgrade_event(result: &CommandResult, kind: &str) -> Option<UpgradeEvent> {
    result
        .output
        .lines()
        .rev()
        .filter_map(|line| serde_json::from_str::<UpgradeEvent>(line).ok())
        .find(|event| event.event == kind)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_status_json_without_importing_the_zodex_runtime() {
        let value = CommandResult {
            ok: true,
            output: r#"{"schema_version":3,"state":"running","runtime":{}}"#.into(),
        };
        assert_eq!(status(&value).unwrap().state, "running");
    }

    #[test]
    fn extracts_last_upgrade_event_from_json_lines() {
        let value = CommandResult { ok: true, output: concat!(
            "{\"event\":\"checking\",\"current_version\":\"0.5.6\",\"message\":\"Checking\"}\n",
            "{\"event\":\"check_complete\",\"current_version\":\"0.5.6\",\"target_version\":\"0.5.7\",\"update_available\":true,\"message\":\"New release\"}\n"
        ).into() };
        let event = upgrade_event(&value, "check_complete").unwrap();
        assert_eq!(event.target_version.as_deref(), Some("0.5.7"));
        assert_eq!(event.update_available, Some(true));
    }
}
