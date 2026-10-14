use serde::Deserialize;
use std::{path::Path, process::Command};

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
    pub code: Option<String>,
    pub message: String,
}

pub fn run(executable: &Path, arguments: &[&str], start: bool) -> CommandResult {
    let mut command = Command::new(executable);
    command.args(arguments);
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
