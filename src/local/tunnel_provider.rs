use std::ffi::{OsStr, OsString};
use std::path::Path;
#[cfg(any(target_os = "macos", target_os = "windows"))]
use std::process::Command;

#[cfg(target_os = "macos")]
use std::fs;

use anyhow::{Context, Result, bail};

use super::RuntimeKey;

pub(crate) const PROVIDER_ENV_ALLOWLIST: &[&str] = &[
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "NO_PROXY",
    "ALL_PROXY",
    "http_proxy",
    "https_proxy",
    "no_proxy",
    "all_proxy",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
];

const WINDOWS_PROVIDER_ENV_ALLOWLIST: &[&str] = &[
    "SystemRoot",
    "SYSTEMROOT",
    "WINDIR",
    "SystemDrive",
    "TEMP",
    "TMP",
    "USERPROFILE",
    "APPDATA",
    "LOCALAPPDATA",
    "PROGRAMDATA",
    "ComSpec",
    "PATHEXT",
    "PROCESSOR_ARCHITECTURE",
];

pub trait ArchiveExtractor: Send + Sync {
    fn extract_tunnel_bundle(&self, archive_path: &Path, bundle_dir: &Path) -> Result<()>;
}

pub(crate) fn provider_environment(
    inherited: &[(OsString, OsString)],
    runtime_key: &RuntimeKey,
) -> Vec<(OsString, OsString)> {
    provider_environment_for_target(inherited, runtime_key, cfg!(target_os = "windows"))
}

fn provider_environment_for_target(
    inherited: &[(OsString, OsString)],
    runtime_key: &RuntimeKey,
    windows: bool,
) -> Vec<(OsString, OsString)> {
    let mut environment = Vec::new();
    for (key, value) in inherited {
        let matches = |allowed: &&str| {
            if windows {
                key.to_string_lossy().eq_ignore_ascii_case(allowed)
            } else {
                key.as_os_str() == OsStr::new(allowed)
            }
        };
        let allowed_provider_variable = PROVIDER_ENV_ALLOWLIST.iter().any(matches);
        let allowed_windows_variable =
            windows && WINDOWS_PROVIDER_ENV_ALLOWLIST.iter().any(matches);
        if allowed_provider_variable || allowed_windows_variable {
            environment.push((key.clone(), value.clone()));
        }
    }
    if windows
        && let Some((_, system_root)) = inherited
            .iter()
            .find(|(key, _)| key.to_string_lossy().eq_ignore_ascii_case("SystemRoot"))
    {
        let mut system32 = system_root.clone();
        system32.push("\\System32");
        environment.push((OsString::from("PATH"), system32));
    }
    // Set exactly the runtime credential the operator supplied. The spawning
    // command uses env_clear, so ambient admin/fallback OpenAI credentials are
    // deliberately absent.
    environment.push((
        OsString::from("CONTROL_PLANE_API_KEY"),
        OsString::from(runtime_key.expose()),
    ));
    environment
}

#[cfg(target_os = "macos")]
pub struct MacDittoArchiveExtractor;

#[cfg(target_os = "windows")]
pub struct WindowsTarArchiveExtractor;

#[cfg(target_os = "linux")]
pub struct LinuxZipArchiveExtractor;

#[cfg(target_os = "macos")]
impl ArchiveExtractor for MacDittoArchiveExtractor {
    fn extract_tunnel_bundle(&self, archive_path: &Path, bundle_dir: &Path) -> Result<()> {
        use std::os::unix::fs::PermissionsExt as _;

        let parent = bundle_dir
            .parent()
            .context("staged tunnel-client bundle path must have a parent directory")?;
        let extracted = tempfile::Builder::new()
            .prefix("extract-")
            .tempdir_in(parent)
            .context("failed to create tunnel-client extraction directory")?;
        let status = Command::new("/usr/bin/ditto")
            .args([OsStr::new("-x"), OsStr::new("-k")])
            .arg(archive_path)
            .arg(extracted.path())
            .env_clear()
            .status()
            .context("failed to invoke macOS ditto for tunnel-client archive")?;
        if !status.success() {
            bail!("macOS ditto failed to extract the verified tunnel-client archive ({status})");
        }

        fs::create_dir_all(bundle_dir).context("failed to create staged tunnel-client bundle")?;
        for (name, mode) in [
            ("tunnel-client", 0o755),
            ("cloudflared", 0o755),
            ("cloudflared-manifest.json", 0o644),
        ] {
            let source = extracted.path().join(name);
            if !source.is_file() {
                bail!("verified tunnel-client archive did not contain the expected `{name}` file");
            }
            let destination = bundle_dir.join(name);
            fs::copy(&source, &destination).with_context(|| {
                format!("failed to stage extracted tunnel-client file `{name}`")
            })?;
            fs::set_permissions(&destination, fs::Permissions::from_mode(mode)).with_context(
                || format!("failed to set staged tunnel-client file mode for `{name}`"),
            )?;
        }
        Ok(())
    }
}

#[cfg(target_os = "windows")]
impl ArchiveExtractor for WindowsTarArchiveExtractor {
    fn extract_tunnel_bundle(&self, archive_path: &Path, bundle_dir: &Path) -> Result<()> {
        use std::fs;

        let parent = bundle_dir
            .parent()
            .context("staged tunnel-client bundle path must have a parent directory")?;
        let extracted = tempfile::Builder::new()
            .prefix("extract-")
            .tempdir_in(parent)
            .context("failed to create tunnel-client extraction directory")?;
        let output = Command::new("tar")
            .arg("-xf")
            .arg(archive_path)
            .arg("-C")
            .arg(extracted.path())
            .output()
            .context("failed to invoke Windows tar for tunnel-client archive")?;
        if !output.status.success() {
            bail!(
                "Windows tar failed to extract the verified tunnel-client archive: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }

        fs::create_dir_all(bundle_dir).context("failed to create staged tunnel-client bundle")?;
        for name in [
            "tunnel-client.exe",
            "cloudflared.exe",
            "cloudflared-manifest.json",
        ] {
            let source = extracted.path().join(name);
            if !source.is_file() {
                bail!("verified tunnel-client archive did not contain the expected `{name}` file");
            }
            fs::copy(&source, bundle_dir.join(name)).with_context(|| {
                format!("failed to stage extracted tunnel-client file `{name}`")
            })?;
        }
        Ok(())
    }
}

#[cfg(target_os = "linux")]
impl ArchiveExtractor for LinuxZipArchiveExtractor {
    fn extract_tunnel_bundle(&self, archive_path: &Path, bundle_dir: &Path) -> Result<()> {
        use std::fs;
        use std::io;
        use std::os::unix::fs::PermissionsExt as _;

        let archive = fs::File::open(archive_path)
            .context("failed to open verified Linux tunnel-client archive")?;
        let mut archive = zip::ZipArchive::new(archive)
            .context("failed to parse verified Linux tunnel-client zip archive")?;
        fs::create_dir_all(bundle_dir).context("failed to create staged tunnel-client bundle")?;

        for (name, mode) in [
            ("tunnel-client", 0o755),
            ("cloudflared", 0o755),
            ("cloudflared-manifest.json", 0o644),
        ] {
            let mut source = archive.by_name(name).with_context(|| {
                format!("verified tunnel-client archive did not contain the expected `{name}` file")
            })?;
            if source.is_dir() {
                bail!("verified tunnel-client archive entry `{name}` is not a regular file");
            }
            let destination = bundle_dir.join(name);
            let mut output = fs::File::create(&destination)
                .with_context(|| format!("failed to create staged tunnel-client file `{name}`"))?;
            io::copy(&mut source, &mut output)
                .with_context(|| format!("failed to extract staged tunnel-client file `{name}`"))?;
            output
                .sync_all()
                .with_context(|| format!("failed to sync staged tunnel-client file `{name}`"))?;
            fs::set_permissions(&destination, fs::Permissions::from_mode(mode)).with_context(
                || format!("failed to set staged tunnel-client file mode for `{name}`"),
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::provider_environment_for_target;
    use crate::local::RuntimeKey;
    use std::collections::HashMap;
    use std::ffi::OsString;

    #[test]
    fn windows_provider_environment_restores_required_os_context_only() {
        let inherited = vec![
            (OsString::from("sYsTeMrOoT"), OsString::from(r"C:\Windows")),
            (OsString::from("windir"), OsString::from(r"C:\Windows")),
            (OsString::from("systemdrive"), OsString::from("C:")),
            (OsString::from("Temp"), OsString::from(r"C:\Temp")),
            (OsString::from("tmp"), OsString::from(r"C:\Temp")),
            (
                OsString::from("UserProfile"),
                OsString::from(r"C:\Users\ashray"),
            ),
            (
                OsString::from("AppData"),
                OsString::from(r"C:\Users\ashray\AppData\Roaming"),
            ),
            (
                OsString::from("LocalAppData"),
                OsString::from(r"C:\Users\ashray\AppData\Local"),
            ),
            (
                OsString::from("ProgramData"),
                OsString::from(r"C:\ProgramData"),
            ),
            (
                OsString::from("ComSpec"),
                OsString::from(r"C:\Windows\System32\cmd.exe"),
            ),
            (
                OsString::from("PATHEXT"),
                OsString::from(".COM;.EXE;.BAT;.CMD"),
            ),
            (
                OsString::from("PROCESSOR_ARCHITECTURE"),
                OsString::from("AMD64"),
            ),
            (
                OsString::from("PATH"),
                OsString::from(r"C:\Users\ashray\bin;C:\Windows\System32"),
            ),
            (
                OsString::from("OPENAI_ADMIN_KEY"),
                OsString::from("admin-secret"),
            ),
            (
                OsString::from("OPENAI_API_KEY"),
                OsString::from("fallback-secret"),
            ),
            (
                OsString::from("Https_Proxy"),
                OsString::from("http://proxy.example"),
            ),
        ];
        let key = RuntimeKey::new("runtime-secret").unwrap();
        let environment = provider_environment_for_target(&inherited, &key, true);
        let mapped = environment
            .into_iter()
            .map(|(key, value)| {
                (
                    key.to_string_lossy().to_string(),
                    value.to_string_lossy().to_string(),
                )
            })
            .collect::<HashMap<_, _>>();

        for required in [
            "systemroot",
            "windir",
            "systemdrive",
            "temp",
            "tmp",
            "userprofile",
            "appdata",
            "localappdata",
            "programdata",
            "comspec",
            "pathext",
            "processor_architecture",
        ] {
            assert!(
                mapped.keys().any(|key| key.eq_ignore_ascii_case(required)),
                "missing {required}"
            );
        }
        assert_eq!(
            mapped.get("PATH").map(String::as_str),
            Some(r"C:\Windows\System32")
        );
        assert_eq!(
            mapped
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case("HTTPS_PROXY"))
                .map(|(_, value)| value.as_str()),
            Some("http://proxy.example")
        );
        assert_eq!(
            mapped.get("CONTROL_PLANE_API_KEY").map(String::as_str),
            Some("runtime-secret")
        );
        assert!(!mapped.contains_key("OPENAI_ADMIN_KEY"));
        assert!(!mapped.contains_key("OPENAI_API_KEY"));
        assert!(
            !mapped
                .values()
                .any(|value| value.contains(r"C:\Users\ashray\bin"))
        );
    }
}
