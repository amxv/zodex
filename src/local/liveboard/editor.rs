use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};

/// Launch the user's selected editor without a shell. A Local runtime can be
/// started by launchd with a minimal PATH, even when the editor CLI is installed
/// in a standard location and works in the user's terminal.
pub(super) fn launch_editor(editor_command: &str, path: &Path) -> Result<()> {
    match spawn_first_available(&editor_executables(editor_command), path) {
        Ok(()) => return Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to launch editor command `{editor_command}`"));
        }
    }

    #[cfg(target_os = "macos")]
    if let Some(app) = macos_editor_app(editor_command) {
        return open_macos_editor(app, path)
            .with_context(|| format!("failed to launch editor command `{editor_command}`"));
    }

    bail!(
        "failed to launch editor command `{editor_command}`: executable not found; set an absolute executable path in Liveboard settings"
    )
}

fn editor_executables(editor_command: &str) -> Vec<PathBuf> {
    let command = PathBuf::from(editor_command);
    // Explicit paths must never silently resolve to a different executable.
    if command.components().count() > 1 {
        return vec![command];
    }

    #[cfg(target_os = "macos")]
    {
        let mut candidates = vec![command];
        // LaunchAgents commonly inherit /usr/bin:/bin:/usr/sbin:/sbin rather
        // than the interactive shell PATH where these CLI shims are installed.
        if let Some(home) = std::env::var_os("HOME") {
            let home = PathBuf::from(home);
            candidates.push(home.join(".local/bin").join(editor_command));
            candidates.push(home.join("bin").join(editor_command));
        }
        candidates.push(PathBuf::from("/opt/homebrew/bin").join(editor_command));
        candidates.push(PathBuf::from("/usr/local/bin").join(editor_command));
        candidates
    }

    #[cfg(not(target_os = "macos"))]
    {
        vec![command]
    }
}

fn spawn_first_available(executables: &[PathBuf], path: &Path) -> io::Result<()> {
    for executable in executables {
        match spawn_editor(executable, path) {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::from(io::ErrorKind::NotFound))
}

fn spawn_editor(executable: &Path, path: &Path) -> io::Result<()> {
    let mut child = Command::new(executable)
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    // Reap the CLI process without waiting for the GUI editor to exit.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

#[cfg(target_os = "macos")]
fn macos_editor_app(editor_command: &str) -> Option<&'static str> {
    match editor_command {
        "zed" => Some("Zed"),
        "code" => Some("Visual Studio Code"),
        "cursor" => Some("Cursor"),
        _ => None,
    }
}

#[cfg(target_os = "macos")]
fn open_macos_editor(app: &str, path: &Path) -> Result<()> {
    let status = Command::new("/usr/bin/open")
        .args(["-a", app])
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .with_context(|| format!("failed to open macOS application `{app}`"))?;
    if !status.success() {
        bail!(
            "macOS application `{app}` could not open the file ({status}); configure an absolute editor executable path instead"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absolute_editor_command_never_falls_back_to_other_binaries() {
        let executable = std::env::temp_dir().join("custom-editor");
        assert_eq!(
            editor_executables(executable.to_str().unwrap()),
            vec![executable]
        );
    }

    #[test]
    fn missing_editor_gives_actionable_error() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nonexistent-editor-command");
        let error = launch_editor(missing.to_str().unwrap(), Path::new("/tmp/test-file"))
            .unwrap_err()
            .to_string();
        assert!(error.contains("absolute executable path"), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn missing_path_falls_through_to_an_existing_executable() {
        // The daemon PATH is not needed when a CLI is at a known absolute path.
        let attempts = [
            PathBuf::from("/zodex-missing-editor-command-for-test"),
            PathBuf::from("/usr/bin/true"),
        ];
        spawn_first_available(&attempts, Path::new("/tmp/a filename with spaces.txt")).unwrap();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_editor_shims_are_resolved_without_login_shell_path() {
        let paths = editor_executables("zed");
        assert_eq!(paths[0], PathBuf::from("zed"));
        assert!(paths.contains(&PathBuf::from("/usr/local/bin/zed")));
        assert!(paths.contains(&PathBuf::from("/opt/homebrew/bin/zed")));
        assert_eq!(macos_editor_app("zed"), Some("Zed"));
        assert_eq!(macos_editor_app("code"), Some("Visual Studio Code"));
        assert_eq!(macos_editor_app("cursor"), Some("Cursor"));
        assert_eq!(macos_editor_app("/usr/local/bin/zed"), None);
    }
}
