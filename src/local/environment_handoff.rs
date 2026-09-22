use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::Path;

use anyhow::{Context, Result, bail};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde::{Deserialize, Serialize};

pub const LOCAL_ENVIRONMENT_HANDOFF_SCHEMA_VERSION: u32 = 1;
const MAX_ENVIRONMENT_ENTRIES: usize = 16_384;
const MAX_ENVIRONMENT_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Serialize, Deserialize)]
struct EnvironmentHandoffDocument {
    schema_version: u32,
    entries: Vec<EnvironmentEntry>,
}

#[derive(Debug, Serialize, Deserialize)]
struct EnvironmentEntry {
    key_base64: String,
    value_base64: String,
}

pub fn write_environment_handoff(path: &Path, environment: &[(OsString, OsString)]) -> Result<()> {
    if environment.len() > MAX_ENVIRONMENT_ENTRIES {
        bail!("Local developer environment contains too many entries");
    }
    let parent = path
        .parent()
        .context("Local environment handoff path has no parent directory")?;
    fs::create_dir_all(parent).with_context(|| {
        format!(
            "failed to create Local runtime directory {}",
            parent.display()
        )
    })?;
    super::private_fs::set_user_only_directory(parent)?;

    let mut encoded_bytes = 0usize;
    let mut entries = Vec::with_capacity(environment.len());
    for (key, value) in environment {
        validate_environment_name(key)
            .context("captured Local developer environment contains an invalid variable name")?;
        validate_environment_value(value)
            .context("captured Local developer environment contains an invalid NUL byte")?;
        let key = os_string_bytes(key);
        let value = os_string_bytes(value);
        encoded_bytes = encoded_bytes
            .checked_add(key.len())
            .and_then(|size| size.checked_add(value.len()))
            .context("captured Local developer environment is too large")?;
        if encoded_bytes > MAX_ENVIRONMENT_BYTES {
            bail!("captured Local developer environment is too large");
        }
        entries.push(EnvironmentEntry {
            key_base64: BASE64.encode(key),
            value_base64: BASE64.encode(value),
        });
    }

    let document = EnvironmentHandoffDocument {
        schema_version: LOCAL_ENVIRONMENT_HANDOFF_SCHEMA_VERSION,
        entries,
    };
    let encoded = serde_json::to_vec(&document)
        .context("failed to encode Local developer environment handoff")?;

    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options.open(path).with_context(|| {
        format!(
            "failed to create Local environment handoff {}",
            path.display()
        )
    })?;
    file.write_all(&encoded).with_context(|| {
        format!(
            "failed to write Local environment handoff {}",
            path.display()
        )
    })?;
    file.sync_all().with_context(|| {
        format!(
            "failed to sync Local environment handoff {}",
            path.display()
        )
    })?;
    super::private_fs::set_user_only_file(path)?;
    Ok(())
}

pub fn consume_environment_handoff(path: &Path) -> Result<Vec<(OsString, OsString)>> {
    super::private_fs::verify_user_only_file(path)?;
    let encoded = fs::read(path).with_context(|| {
        format!(
            "failed to read Local environment handoff {}",
            path.display()
        )
    })?;
    if encoded.len() > MAX_ENVIRONMENT_BYTES.saturating_mul(2) {
        bail!("Local environment handoff is unexpectedly large");
    }
    let document: EnvironmentHandoffDocument =
        serde_json::from_slice(&encoded).with_context(|| {
            format!(
                "failed to parse Local environment handoff {}",
                path.display()
            )
        })?;
    if document.schema_version != LOCAL_ENVIRONMENT_HANDOFF_SCHEMA_VERSION {
        bail!(
            "unsupported Local environment handoff schema version {}; expected {}",
            document.schema_version,
            LOCAL_ENVIRONMENT_HANDOFF_SCHEMA_VERSION
        );
    }
    if document.entries.len() > MAX_ENVIRONMENT_ENTRIES {
        bail!("Local environment handoff contains too many entries");
    }

    let mut decoded_bytes = 0usize;
    let mut environment = Vec::with_capacity(document.entries.len());
    for entry in document.entries {
        let key = BASE64
            .decode(entry.key_base64)
            .context("Local environment handoff contains invalid key encoding")?;
        let value = BASE64
            .decode(entry.value_base64)
            .context("Local environment handoff contains invalid value encoding")?;
        decoded_bytes = decoded_bytes
            .checked_add(key.len())
            .and_then(|size| size.checked_add(value.len()))
            .context("Local environment handoff is too large")?;
        if decoded_bytes > MAX_ENVIRONMENT_BYTES {
            bail!("Local environment handoff is too large");
        }
        let key = os_string_from_bytes(key)
            .context("Local environment handoff contains an invalid variable-name encoding")?;
        let value = os_string_from_bytes(value)
            .context("Local environment handoff contains an invalid variable-value encoding")?;
        validate_environment_name(&key)
            .context("Local environment handoff contains an invalid variable name")?;
        validate_environment_value(&value)
            .context("Local environment handoff contains an invalid variable value")?;
        environment.push((key, value));
    }

    fs::remove_file(path).with_context(|| {
        format!(
            "failed to unlink Local environment handoff {}",
            path.display()
        )
    })?;
    Ok(environment)
}

#[cfg(unix)]
fn os_string_bytes(value: &OsString) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt as _;
    value.as_os_str().as_bytes().to_vec()
}

#[cfg(unix)]
fn os_string_from_bytes(value: Vec<u8>) -> Result<OsString> {
    use std::os::unix::ffi::OsStringExt as _;
    Ok(OsString::from_vec(value))
}

#[cfg(target_os = "windows")]
fn os_string_bytes(value: &OsString) -> Vec<u8> {
    use std::os::windows::ffi::OsStrExt as _;

    value
        .encode_wide()
        .flat_map(u16::to_le_bytes)
        .collect::<Vec<_>>()
}

#[cfg(target_os = "windows")]
fn os_string_from_bytes(value: Vec<u8>) -> Result<OsString> {
    use std::os::windows::ffi::OsStringExt as _;

    if value.len() % 2 != 0 {
        bail!("UTF-16 environment value has an odd byte length");
    }
    let wide = value
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect::<Vec<_>>();
    Ok(OsString::from_wide(&wide))
}

#[cfg(not(any(unix, target_os = "windows")))]
fn os_string_bytes(value: &OsString) -> Vec<u8> {
    value.to_string_lossy().as_bytes().to_vec()
}

#[cfg(not(any(unix, target_os = "windows")))]
fn os_string_from_bytes(value: Vec<u8>) -> Result<OsString> {
    Ok(OsString::from(
        String::from_utf8(value).context("environment handoff value is not valid UTF-8")?,
    ))
}

#[cfg(unix)]
fn validate_environment_name(value: &OsString) -> Result<()> {
    use std::os::unix::ffi::OsStrExt as _;

    let bytes = value.as_os_str().as_bytes();
    if bytes.is_empty() || bytes.contains(&0) || bytes.contains(&b'=') {
        bail!("environment variable names must be non-empty and contain neither NUL nor `=`");
    }
    Ok(())
}

#[cfg(unix)]
fn validate_environment_value(value: &OsString) -> Result<()> {
    use std::os::unix::ffi::OsStrExt as _;

    if value.as_os_str().as_bytes().contains(&0) {
        bail!("environment variable values must not contain NUL");
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn validate_environment_name(value: &OsString) -> Result<()> {
    use std::os::windows::ffi::OsStrExt as _;

    let units = value.encode_wide().collect::<Vec<_>>();
    if units.is_empty() || units.contains(&0) || units.contains(&('=' as u16)) {
        bail!("environment variable names must be non-empty and contain neither NUL nor `=`");
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn validate_environment_value(value: &OsString) -> Result<()> {
    use std::os::windows::ffi::OsStrExt as _;

    if value.encode_wide().any(|unit| unit == 0) {
        bail!("environment variable values must not contain NUL");
    }
    Ok(())
}

#[cfg(not(any(unix, target_os = "windows")))]
fn validate_environment_name(value: &OsString) -> Result<()> {
    let value = value.to_string_lossy();
    if value.is_empty() || value.contains('\0') || value.contains('=') {
        bail!("environment variable names must be non-empty and contain neither NUL nor `=`");
    }
    Ok(())
}

#[cfg(not(any(unix, target_os = "windows")))]
fn validate_environment_value(value: &OsString) -> Result<()> {
    if value.to_string_lossy().contains('\0') {
        bail!("environment variable values must not contain NUL");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use tempfile::tempdir;

    use super::{consume_environment_handoff, write_environment_handoff};

    #[test]
    fn environment_handoff_round_trips_and_is_consumed() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("runtime/environment.json");
        let environment = vec![
            (OsString::from("HOME"), OsString::from("/Users/example")),
            (
                OsString::from("PATH"),
                OsString::from("/opt/homebrew/bin:/Users/example/.local/bin:/usr/bin:/bin"),
            ),
            (OsString::from("CUSTOM_TOOL"), OsString::from("enabled")),
        ];
        write_environment_handoff(&path, &environment).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }

        let decoded = consume_environment_handoff(&path).unwrap();
        assert_eq!(decoded, environment);
        assert!(
            !path.exists(),
            "handoff must be unlinked after successful consumption"
        );
    }

    #[test]
    fn corrupt_environment_handoff_fails_without_silent_fallback() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("environment.json");
        std::fs::write(&path, b"not-json").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        assert!(consume_environment_handoff(&path).is_err());
        assert!(
            path.exists(),
            "failed consumption leaves evidence for stale cleanup"
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_environment_handoff_preserves_utf16_values() {
        use std::os::windows::ffi::OsStringExt as _;

        let dir = tempdir().unwrap();
        let path = dir.path().join("runtime/environment.json");
        let value = OsString::from_wide(&[
            b'C' as u16,
            b':' as u16,
            b'\\' as u16,
            0x5de5,
            0x5177,
            b'\\' as u16,
            0xd83d,
            0xde42,
        ]);
        let environment = vec![
            (OsString::from("USERPROFILE"), value),
            (
                OsString::from("Path"),
                OsString::from(r"C:\Windows\System32"),
            ),
        ];
        write_environment_handoff(&path, &environment).unwrap();
        assert_eq!(consume_environment_handoff(&path).unwrap(), environment);
    }

    #[cfg(unix)]
    #[test]
    fn environment_handoff_rejects_group_or_world_access() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempdir().unwrap();
        let path = dir.path().join("environment.json");
        std::fs::write(&path, br#"{"schema_version":1,"entries":[]}"#).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
        let error = consume_environment_handoff(&path).unwrap_err();
        assert!(error.to_string().contains("permissions are too broad"));
    }
}
