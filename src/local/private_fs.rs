use std::fs;
use std::path::Path;

#[cfg(target_os = "windows")]
use std::collections::HashSet;
#[cfg(target_os = "windows")]
use std::path::PathBuf;
#[cfg(target_os = "windows")]
use std::sync::{Mutex, OnceLock};

use anyhow::{Context, Result, bail};

pub(crate) fn set_user_only_directory(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).with_context(|| {
            format!("failed to set user-only permissions on {}", path.display())
        })?;
    }
    #[cfg(target_os = "windows")]
    set_windows_user_only_acl(path, true)?;
    Ok(())
}

pub(crate) fn set_user_only_file(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).with_context(|| {
            format!("failed to set user-only permissions on {}", path.display())
        })?;
    }
    #[cfg(target_os = "windows")]
    set_windows_user_only_acl(path, false)?;
    Ok(())
}

pub(crate) fn verify_user_only_file(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("failed to inspect private Local file {}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!(
            "private Local state must be a regular file: {}",
            path.display()
        );
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = metadata.permissions().mode() & 0o777;
        if mode & 0o077 != 0 {
            bail!(
                "private Local file permissions are too broad ({mode:o}); expected user-only access"
            );
        }
    }
    #[cfg(target_os = "windows")]
    verify_windows_user_only_acl(path)?;
    Ok(())
}

#[cfg(target_os = "windows")]
fn windows_powershell() -> std::path::PathBuf {
    std::env::var_os("SystemRoot")
        .map(std::path::PathBuf::from)
        .map(|root| root.join("System32/WindowsPowerShell/v1.0/powershell.exe"))
        .unwrap_or_else(|| std::path::PathBuf::from("powershell.exe"))
}

#[cfg(target_os = "windows")]
fn set_windows_user_only_acl(path: &Path, directory: bool) -> Result<()> {
    use std::process::{Command, Stdio};

    static SECURED_PATHS: OnceLock<Mutex<HashSet<(PathBuf, bool)>>> = OnceLock::new();
    let secured = SECURED_PATHS.get_or_init(|| Mutex::new(HashSet::new()));
    let cache_key = (path.to_path_buf(), directory);
    if secured
        .lock()
        .map_err(|_| anyhow::anyhow!("Windows private-path ACL cache was poisoned"))?
        .contains(&cache_key)
    {
        return Ok(());
    }

    // Construct a protected DACL from scratch instead of only removing
    // inheritance. That prevents an unrelated explicit ACE from surviving on
    // runtime files containing captured environment data, bearer tokens, or
    // process ownership state.
    const SCRIPT: &str = r#"
$ErrorActionPreference = 'Stop'
$identity = [System.Security.Principal.WindowsIdentity]::GetCurrent()
$sid = $identity.User
$path = $env:ZODEX_PRIVATE_PATH
$isDir = $env:ZODEX_PRIVATE_IS_DIR -eq '1'
if ($isDir) {
    $acl = New-Object System.Security.AccessControl.DirectorySecurity
    $inheritance = [System.Security.AccessControl.InheritanceFlags]::ContainerInherit -bor [System.Security.AccessControl.InheritanceFlags]::ObjectInherit
} else {
    $acl = New-Object System.Security.AccessControl.FileSecurity
    $inheritance = [System.Security.AccessControl.InheritanceFlags]::None
}
$acl.SetOwner($sid)
$acl.SetAccessRuleProtection($true, $false)
$rule = New-Object System.Security.AccessControl.FileSystemAccessRule(
    $sid,
    [System.Security.AccessControl.FileSystemRights]::FullControl,
    $inheritance,
    [System.Security.AccessControl.PropagationFlags]::None,
    [System.Security.AccessControl.AccessControlType]::Allow
)
[void]$acl.AddAccessRule($rule)
Set-Acl -LiteralPath $path -AclObject $acl
"#;

    let output = Command::new(windows_powershell())
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            SCRIPT,
        ])
        .env("ZODEX_PRIVATE_PATH", path.as_os_str())
        .env("ZODEX_PRIVATE_IS_DIR", if directory { "1" } else { "0" })
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("failed to launch Windows ACL helper for {}", path.display()))?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
        bail!(
            "failed to restrict Windows permissions on {}{}",
            path.display(),
            if detail.is_empty() {
                String::new()
            } else {
                format!(": {detail}")
            }
        );
    }
    secured
        .lock()
        .map_err(|_| anyhow::anyhow!("Windows private-path ACL cache was poisoned"))?
        .insert(cache_key);
    Ok(())
}

#[cfg(target_os = "windows")]
fn verify_windows_user_only_acl(path: &Path) -> Result<()> {
    use std::process::{Command, Stdio};

    const SCRIPT: &str = r#"
$ErrorActionPreference = 'Stop'
$sid = [System.Security.Principal.WindowsIdentity]::GetCurrent().User
$acl = Get-Acl -LiteralPath $env:ZODEX_PRIVATE_PATH
if (-not $acl.AreAccessRulesProtected) { exit 11 }
$found = $false
foreach ($rule in $acl.Access) {
    $ruleSid = $rule.IdentityReference.Translate([System.Security.Principal.SecurityIdentifier])
    if ($ruleSid.Value -ne $sid.Value -or $rule.AccessControlType -ne [System.Security.AccessControl.AccessControlType]::Allow) {
        exit 12
    }
    $found = $true
}
if (-not $found) { exit 13 }
"#;
    let output = Command::new(windows_powershell())
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            SCRIPT,
        ])
        .env("ZODEX_PRIVATE_PATH", path.as_os_str())
        .stdin(Stdio::null())
        .output()
        .with_context(|| {
            format!(
                "failed to inspect Windows permissions on {}",
                path.display()
            )
        })?;
    if !output.status.success() {
        bail!(
            "private Local file has broader Windows ACLs than expected: {}",
            path.display()
        );
    }
    Ok(())
}

#[cfg(all(test, target_os = "windows"))]
mod windows_tests {
    use std::fs;

    use tempfile::tempdir;

    use super::{set_user_only_directory, set_user_only_file, verify_user_only_file};

    #[test]
    fn windows_private_file_acl_is_user_only_and_verifiable() {
        let temp = tempdir().unwrap();
        let private_dir = temp.path().join("private");
        fs::create_dir(&private_dir).unwrap();
        set_user_only_directory(&private_dir).unwrap();

        let private_file = private_dir.join("secret.txt");
        fs::write(&private_file, b"secret").unwrap();
        set_user_only_file(&private_file).unwrap();
        verify_user_only_file(&private_file).unwrap();
    }
}
