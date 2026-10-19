use fs2::FileExt as _;
use serde::{Deserialize, Serialize};
use std::{
    env, fs,
    fs::{File, OpenOptions},
    path::{Path, PathBuf},
    process::Command,
};

#[cfg(target_os = "linux")]
use std::process::Stdio;

#[derive(Default, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Preferences {
    pub start_folder: Option<PathBuf>,
    pub launch_tunnel_at_login: bool,
}

#[cfg(target_os = "linux")]
fn home() -> Result<PathBuf, String> {
    env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .ok_or_else(|| "Your home directory could not be found".to_owned())
}

fn config_root() -> Result<PathBuf, String> {
    #[cfg(target_os = "windows")]
    {
        env::var_os("APPDATA")
            .map(PathBuf::from)
            .map(|p| p.join("Zodex"))
            .ok_or_else(|| "APPDATA is missing".to_owned())
    }
    #[cfg(target_os = "linux")]
    {
        Ok(env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or(home()?.join(".config"))
            .join("zodex"))
    }
}

pub fn preferences() -> Preferences {
    let Ok(path) = config_root().map(|p| p.join("tray.json")) else {
        return Preferences::default();
    };
    fs::read_to_string(path)
        .ok()
        .and_then(|json| serde_json::from_str(&json).ok())
        .unwrap_or_default()
}

pub fn save_preferences(preferences: &Preferences) -> Result<(), String> {
    let directory = config_root()?;
    fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
    let data = serde_json::to_vec_pretty(preferences).map_err(|e| e.to_string())?;
    fs::write(directory.join("tray.json"), data).map_err(|e| e.to_string())
}

pub fn single_instance() -> Result<Option<File>, String> {
    let directory = config_root()?;
    fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(directory.join("tray.lock"))
        .map_err(|e| e.to_string())?;
    match file.try_lock_exclusive() {
        Ok(()) => Ok(Some(file)),
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
        Err(e) => Err(format!("Could not acquire tray single-instance lock: {e}")),
    }
}

pub fn zodex_executable() -> Result<PathBuf, String> {
    let current = env::current_exe().map_err(|e| e.to_string())?;
    let executable = current.with_file_name(if cfg!(windows) { "zodex.exe" } else { "zodex" });
    if executable.is_file() {
        Ok(executable)
    } else {
        Err(format!(
            "The Zodex CLI is missing alongside the tray app at {}. Reinstall Zodex.",
            executable.display()
        ))
    }
}

#[cfg(target_os = "linux")]
fn autostart_path() -> Result<PathBuf, String> {
    Ok(config_root()?
        .parent()
        .ok_or("Missing XDG config path")?
        .join("autostart/zodex-tray.desktop"))
}

#[cfg(target_os = "linux")]
fn desktop_exec_quote(path: &Path) -> Result<String, String> {
    let text = path.to_str().ok_or("The tray path is not valid UTF-8")?;
    if text.contains(['\n', '\r']) {
        return Err("The tray path contains an unsupported newline".to_owned());
    }
    // Desktop Entry Specification: double-quote the entire argument and escape
    // backslash, quote, dollar and backtick. This does NOT invoke a shell.
    let escaped = text
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('$', "\\$")
        .replace('`', "\\`");
    Ok(format!("\"{escaped}\""))
}

#[cfg(target_os = "windows")]
const WINDOWS_TRAY_RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";

#[cfg(target_os = "windows")]
fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}

#[cfg(target_os = "windows")]
fn registry_value_exists(subkey: &str, name: &str) -> bool {
    use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_ANY, RegGetValueW};

    let mut byte_count = 0;
    unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            wide(subkey).as_ptr(),
            wide(name).as_ptr(),
            RRF_RT_ANY,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut byte_count,
        ) == 0
    }
}

pub fn launch_at_login_enabled() -> bool {
    #[cfg(target_os = "windows")]
    {
        registry_value_exists(WINDOWS_TRAY_RUN_KEY, "ZodexTray")
    }
    #[cfg(target_os = "linux")]
    {
        autostart_path().is_ok_and(|path| {
            fs::read_to_string(path).is_ok_and(|contents| {
                contents.contains("Type=Application") && !contents.contains("Hidden=true")
            })
        })
    }
}

pub fn set_launch_at_login(enabled: bool) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        use std::ffi::OsString;
        use std::os::windows::ffi::OsStrExt as _;
        use windows_sys::Win32::System::Registry::{
            HKEY_CURRENT_USER, REG_SZ, RegDeleteKeyValueW, RegSetKeyValueW,
        };

        let subkey = wide(WINDOWS_TRAY_RUN_KEY);
        let name = wide("ZodexTray");
        let status = if enabled {
            let executable = env::current_exe().map_err(|e| e.to_string())?;
            let mut quoted = OsString::from("\"");
            quoted.push(executable.as_os_str());
            quoted.push("\"");
            let value = quoted.encode_wide().chain(Some(0)).collect::<Vec<_>>();
            let bytes = u32::try_from(value.len() * size_of::<u16>())
                .map_err(|_| "Windows tray startup path is too long".to_owned())?;
            unsafe {
                RegSetKeyValueW(
                    HKEY_CURRENT_USER,
                    subkey.as_ptr(),
                    name.as_ptr(),
                    REG_SZ,
                    value.as_ptr().cast(),
                    bytes,
                )
            }
        } else {
            if !launch_at_login_enabled() {
                return Ok(());
            }
            unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, subkey.as_ptr(), name.as_ptr()) }
        };
        if status != 0 {
            return Err(format!(
                "Cannot update Windows tray startup registration: {}",
                std::io::Error::from_raw_os_error(status as i32)
            ));
        }
        Ok(())
    }
    #[cfg(target_os = "linux")]
    {
        let path = autostart_path()?;
        if !enabled {
            return if path.exists() {
                fs::remove_file(path).map_err(|e| e.to_string())
            } else {
                Ok(())
            };
        }
        let executable = env::current_exe().map_err(|e| e.to_string())?;
        let command = desktop_exec_quote(&executable)?;
        fs::create_dir_all(path.parent().ok_or("Missing autostart directory")?)
            .map_err(|e| e.to_string())?;
        fs::write(&path, format!(
            "[Desktop Entry]\nType=Application\nName=Zodex\nComment=Zodex Local tray controls (does not start Local)\nExec={command}\nTerminal=false\nX-GNOME-Autostart-enabled=true\n"
        ))
        .map_err(|e| e.to_string())
    }
}

#[cfg(target_os = "linux")]
pub fn graphical_session() -> bool {
    env::var_os("DISPLAY").is_some() || env::var_os("WAYLAND_DISPLAY").is_some()
}

#[cfg(target_os = "linux")]
pub fn developer_environment() -> Result<Vec<(std::ffi::OsString, std::ffi::OsString)>, String> {
    use std::{
        ffi::CStr,
        os::unix::ffi::{OsStrExt, OsStringExt},
    };

    let shell = unsafe {
        let entry = libc::getpwuid(libc::geteuid());
        if entry.is_null() || (*entry).pw_shell.is_null() {
            None
        } else {
            Some(PathBuf::from(std::ffi::OsStr::from_bytes(
                CStr::from_ptr((*entry).pw_shell).to_bytes(),
            )))
        }
    }
    .or_else(|| env::var_os("SHELL").map(PathBuf::from))
    .ok_or("Could not determine your login shell")?;
    if !shell.is_absolute() || !shell.is_file() {
        return Err(format!("Invalid login shell: {}", shell.display()));
    }
    use std::io::{Read as _, Seek as _};
    // As with Local start, do not wait for pipe EOF from unrelated processes
    // started by the login shell. A temporary file avoids inherited-pipe hangs.
    let mut output_file = tempfile::tempfile().map_err(|error| error.to_string())?;
    let stdout = output_file.try_clone().map_err(|error| error.to_string())?;
    let status = Command::new(&shell)
        .args(["-lic", "printf 'ZODEX_TRAY_ENV\\0'; /usr/bin/env -0"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .stdout(Stdio::from(stdout))
        .status()
        .map_err(|e| e.to_string())?;
    if !status.success() {
        return Err(format!("Login shell {} failed", shell.display()));
    }
    output_file.rewind().map_err(|e| e.to_string())?;
    let mut output = Vec::new();
    output_file
        .take(4 * 1024 * 1024)
        .read_to_end(&mut output)
        .map_err(|error| error.to_string())?;
    let marker = b"ZODEX_TRAY_ENV\0";
    let offset = output
        .windows(marker.len())
        .position(|bytes| bytes == marker)
        .ok_or("Login shell did not return its environment")?
        + marker.len();
    let vars = output[offset..]
        .split(|byte| *byte == 0)
        .filter_map(|entry| {
            let position = entry.iter().position(|byte| *byte == b'=')?;
            Some((&entry[..position], &entry[position + 1..]))
        })
        .map(|(name, value)| {
            (
                std::ffi::OsString::from_vec(name.to_vec()),
                std::ffi::OsString::from_vec(value.to_vec()),
            )
        })
        .collect::<Vec<_>>();
    if vars.is_empty() {
        return Err("Login shell returned an empty environment".into());
    }
    Ok(vars)
}

pub fn choose_folder(_current: Option<&Path>) -> Result<Option<PathBuf>, String> {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;
        let root = env::var_os("SystemRoot").ok_or("SystemRoot missing")?;
        let script = concat!(
            "Add-Type -AssemblyName System.Windows.Forms;",
            "$picker = New-Object System.Windows.Forms.FolderBrowserDialog;",
            "$picker.Description = 'Choose Zodex Start Folder';",
            "$picker.ShowNewFolderButton = $true;",
            "if ($picker.ShowDialog() -eq [System.Windows.Forms.DialogResult]::OK) {",
            "[Console]::Out.WriteLine($picker.SelectedPath) }"
        );
        let output = Command::new(
            PathBuf::from(root).join("System32/WindowsPowerShell/v1.0/powershell.exe"),
        )
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Sta",
            "-Command",
            script,
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| e.to_string())?;
        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
        }
        let path = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        Ok((!path.is_empty()).then(|| PathBuf::from(path)))
    }
    #[cfg(target_os = "linux")]
    {
        let current = _current.map(Path::to_path_buf).unwrap_or(home()?);
        let kde = env::var("XDG_CURRENT_DESKTOP")
            .unwrap_or_default()
            .to_uppercase()
            .contains("KDE");
        let order = if kde {
            ["kdialog", "zenity", "yad"]
        } else {
            ["zenity", "kdialog", "yad"]
        };
        for name in order {
            let mut cmd = Command::new(name);
            if name == "kdialog" {
                cmd.arg("--getexistingdirectory").arg(&current);
            } else {
                cmd.args([
                    "--file-selection",
                    "--directory",
                    "--title=Choose Zodex Start Folder",
                ])
                .arg(format!("--filename={}/", current.display()));
            }
            match cmd.output() {
                Ok(output) if output.status.success() => {
                    let path = String::from_utf8_lossy(&output.stdout).trim().to_owned();
                    return Ok((!path.is_empty()).then(|| PathBuf::from(path)));
                }
                Ok(_) => return Ok(None), // User cancelled the dialog.
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e.to_string()),
            }
        }
        Err("No desktop folder chooser is available. Install zenity or kdialog.".into())
    }
}

pub fn show_message(title: &str, detail: &str, question: bool) -> bool {
    #[cfg(target_os = "windows")]
    {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            IDYES, MB_ICONWARNING, MB_OK, MB_YESNO, MessageBoxW,
        };
        let title = title.encode_utf16().chain(Some(0)).collect::<Vec<_>>();
        let detail = detail.encode_utf16().chain(Some(0)).collect::<Vec<_>>();
        let flags = if question {
            MB_YESNO | MB_ICONWARNING
        } else {
            MB_OK | MB_ICONWARNING
        };
        unsafe {
            MessageBoxW(std::ptr::null_mut(), detail.as_ptr(), title.as_ptr(), flags) == IDYES
        }
    }
    #[cfg(target_os = "linux")]
    {
        for name in ["zenity", "kdialog"] {
            let mut command = Command::new(name);
            if name == "zenity" {
                command
                    .arg(if question { "--question" } else { "--error" })
                    .arg(format!("--title={title}"))
                    .arg(format!("--text={detail}"));
            } else {
                command
                    .arg(if question {
                        "--warningyesno"
                    } else {
                        "--error"
                    })
                    .arg(detail)
                    .arg("--title")
                    .arg(title);
            }
            match command.status() {
                Ok(status) => return question && status.success(),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(_) => break,
            }
        }
        eprintln!("{title}: {detail}");
        false
    }
}

#[cfg(target_os = "windows")]
pub fn pump_events() -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, MSG, PM_REMOVE, PeekMessageW, TranslateMessage, WM_QUIT,
    };
    unsafe {
        let mut msg: MSG = std::mem::zeroed();
        while PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
            if msg.message == WM_QUIT {
                return false;
            }
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    true
}

#[cfg(target_os = "linux")]
pub fn pump_events() -> bool {
    true
}

#[cfg(target_os = "windows")]
pub fn tray_icon_foreground() -> [u8; 3] {
    use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};

    // Query the registry in-process. Spawning reg.exe during desktop sign-out
    // can produce Windows' 0xc0000142 "application error" dialog.
    let mut theme = 0u32;
    let mut size = size_of::<u32>() as u32;
    let light = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            wide("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize").as_ptr(),
            wide("SystemUsesLightTheme").as_ptr(),
            RRF_RT_REG_DWORD,
            std::ptr::null_mut(),
            (&mut theme as *mut u32).cast(),
            &mut size,
        ) == 0
            && theme == 1
    };
    if light { [28, 28, 28] } else { [247, 247, 247] }
}

#[cfg(target_os = "linux")]
pub fn tray_icon_foreground() -> [u8; 3] {
    [247, 247, 247]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_preferences_round_trip_without_secrets() {
        let value = Preferences {
            start_folder: Some(PathBuf::from("/tmp/zodex start folder")),
            launch_tunnel_at_login: true,
        };
        let text = serde_json::to_string(&value).unwrap();
        assert_eq!(
            serde_json::from_str::<Preferences>(&text)
                .unwrap()
                .start_folder,
            value.start_folder
        );
        assert!(!text.contains("runtime_key"));
    }

    #[test]
    fn existing_tray_settings_do_not_start_tunnel_by_default() {
        let old = r#"{"start_folder":"/tmp/previous-workspace"}"#;
        let loaded: Preferences = serde_json::from_str(old).unwrap();
        assert!(!loaded.launch_tunnel_at_login);
        assert_eq!(
            loaded.start_folder,
            Some(PathBuf::from("/tmp/previous-workspace"))
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn desktop_exec_quotes_special_characters_without_a_shell() {
        let quoted = desktop_exec_quote(Path::new("/home/a name/with$\"`\\bin")).unwrap();
        assert_eq!(quoted, "\"/home/a name/with\\$\\\"\\`\\\\bin\"");
    }
}
