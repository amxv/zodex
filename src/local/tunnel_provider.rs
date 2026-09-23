use std::env;
use std::ffi::{OsStr, OsString};
use std::path::Path;
use std::process::Command;

#[cfg(target_os = "macos")]
use std::fs;

use anyhow::{Context, Result, bail};
use serde_json::Value;

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

pub trait TunnelMetadataValidator: Send + Sync {
    fn validate(&self, binary_path: &Path, tunnel_id: &str, runtime_key: &RuntimeKey)
    -> Result<()>;
}

pub struct ProcessTunnelMetadataValidator {
    inherited_environment: Vec<(OsString, OsString)>,
}

impl ProcessTunnelMetadataValidator {
    pub fn new() -> Self {
        Self {
            inherited_environment: env::vars_os().collect(),
        }
    }

    #[cfg(all(test, unix))]
    pub(crate) fn with_environment(environment: Vec<(OsString, OsString)>) -> Self {
        Self {
            inherited_environment: environment,
        }
    }
}

impl Default for ProcessTunnelMetadataValidator {
    fn default() -> Self {
        Self::new()
    }
}

impl TunnelMetadataValidator for ProcessTunnelMetadataValidator {
    fn validate(
        &self,
        binary_path: &Path,
        tunnel_id: &str,
        runtime_key: &RuntimeKey,
    ) -> Result<()> {
        if !binary_path.is_absolute() {
            bail!("managed tunnel-client validation path must be absolute");
        }

        let mut command = Command::new(binary_path);
        command
            .args(["admin", "--json", "tunnels", "get", tunnel_id])
            .env_clear();
        apply_provider_environment(&mut command, &self.inherited_environment, runtime_key);

        let output = command.output().with_context(|| {
            format!(
                "failed to run managed tunnel-client at {}",
                binary_path.display()
            )
        })?;
        if !output.status.success() {
            let provider_output = if output.stderr.is_empty() {
                &output.stdout
            } else {
                &output.stderr
            };
            let provider_diagnostic = redact_runtime_key(provider_output, runtime_key);
            if !provider_diagnostic.is_empty() {
                bail!(
                    "OpenAI tunnel metadata validation failed ({}); verify the tunnel ID, runtime key, and Tunnels Read permission; tunnel-client: {}",
                    output.status,
                    provider_diagnostic
                );
            }
            bail!(
                "OpenAI tunnel metadata validation failed ({}); verify the tunnel ID, runtime key, and Tunnels Read permission",
                output.status
            );
        }

        let metadata: Value = serde_json::from_slice(&output.stdout)
            .context("tunnel-client metadata validation returned invalid JSON")?;
        let returned_id = metadata
            .get("id")
            .and_then(Value::as_str)
            .context("tunnel-client metadata response did not contain a tunnel id")?;
        if returned_id != tunnel_id {
            bail!(
                "tunnel-client metadata response returned an unexpected tunnel id; refusing setup"
            );
        }
        Ok(())
    }
}

fn apply_provider_environment(
    command: &mut Command,
    inherited: &[(OsString, OsString)],
    runtime_key: &RuntimeKey,
) {
    for (key, value) in provider_environment(inherited, runtime_key) {
        command.env(key, value);
    }
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
        let allowed_provider_variable = PROVIDER_ENV_ALLOWLIST
            .iter()
            .any(|allowed| key.as_os_str() == OsStr::new(allowed));
        let allowed_windows_variable = windows
            && WINDOWS_PROVIDER_ENV_ALLOWLIST
                .iter()
                .any(|allowed| key.as_os_str() == OsStr::new(allowed));
        if allowed_provider_variable || allowed_windows_variable {
            environment.push((key.clone(), value.clone()));
        }
    }
    if windows
        && let Some((_, system_root)) = inherited.iter().find(|(key, _)| {
            key.as_os_str() == OsStr::new("SystemRoot")
                || key.as_os_str() == OsStr::new("SYSTEMROOT")
        })
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

fn redact_runtime_key(output: &[u8], runtime_key: &RuntimeKey) -> String {
    let mut diagnostic = String::from_utf8_lossy(output)
        .trim()
        .replace(runtime_key.expose(), "<redacted-runtime-key>");
    if diagnostic.len() > 4096 {
        diagnostic.truncate(4096);
        diagnostic.push('…');
    }
    diagnostic
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

#[cfg(all(test, unix))]
mod tests {
    use std::collections::HashMap;
    use std::ffi::OsString;
    use std::fs;
    use std::os::unix::fs::PermissionsExt as _;

    use tempfile::tempdir;

    use super::{
        ProcessTunnelMetadataValidator, TunnelMetadataValidator, provider_environment_for_target,
    };
    use crate::local::RuntimeKey;

    #[test]
    fn provider_subprocess_get_uses_only_runtime_key_and_allowlisted_environment() {
        let dir = tempdir().unwrap();
        let binary = dir.path().join("fake-tunnel-client");
        let capture = dir.path().join("environment.txt");
        let tunnel_id = "tunnel_0123456789abcdef0123456789abcdef";
        let script = format!(
            "#!/bin/sh\n/usr/bin/env > '{}'\nprintf '{{\"id\":\"{}\"}}'\n",
            capture.display(),
            tunnel_id
        );
        fs::write(&binary, script).unwrap();
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).unwrap();

        let validator = ProcessTunnelMetadataValidator::with_environment(vec![
            (
                OsString::from("OPENAI_ADMIN_KEY"),
                OsString::from("admin-secret"),
            ),
            (
                OsString::from("OPENAI_API_KEY"),
                OsString::from("fallback-secret"),
            ),
            (
                OsString::from("UNRELATED_SECRET"),
                OsString::from("unrelated-secret"),
            ),
            (
                OsString::from("HTTPS_PROXY"),
                OsString::from("http://proxy.example"),
            ),
            (
                OsString::from("CONTROL_PLANE_API_KEY"),
                OsString::from("ambient-runtime"),
            ),
        ]);
        let key = RuntimeKey::new("intended-runtime-secret").unwrap();
        validator.validate(&binary, tunnel_id, &key).unwrap();

        let environment = fs::read_to_string(capture).unwrap();
        assert!(environment.contains("CONTROL_PLANE_API_KEY=intended-runtime-secret"));
        assert!(environment.contains("HTTPS_PROXY=http://proxy.example"));
        for forbidden in [
            "OPENAI_ADMIN_KEY",
            "OPENAI_API_KEY",
            "UNRELATED_SECRET",
            "admin-secret",
            "fallback-secret",
            "unrelated-secret",
            "ambient-runtime",
        ] {
            assert!(
                !environment.contains(forbidden),
                "leaked {forbidden}: {environment}"
            );
        }
    }

    #[test]
    fn provider_subprocess_failure_does_not_echo_runtime_key() {
        let dir = tempdir().unwrap();
        let binary = dir.path().join("fake-tunnel-client");
        fs::write(&binary, "#!/bin/sh\necho provider-failed >&2\nexit 7\n").unwrap();
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).unwrap();

        let validator = ProcessTunnelMetadataValidator::with_environment(Vec::new());
        let key = RuntimeKey::new("never-print-me").unwrap();
        let error = validator
            .validate(&binary, "tunnel_0123456789abcdef0123456789abcdef", &key)
            .unwrap_err();
        let rendered = format!("{error:#}");
        assert!(rendered.contains("metadata validation failed"));
        assert!(!rendered.contains("never-print-me"));
        assert!(rendered.contains("provider-failed"));
    }

    #[test]
    fn provider_subprocess_failure_redacts_runtime_key_from_provider_error() {
        let dir = tempdir().unwrap();
        let binary = dir.path().join("fake-tunnel-client");
        fs::write(
            &binary,
            "#!/bin/sh\necho 'provider rejected never-print-me' >&2\nexit 7\n",
        )
        .unwrap();
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).unwrap();

        let validator = ProcessTunnelMetadataValidator::with_environment(Vec::new());
        let key = RuntimeKey::new("never-print-me").unwrap();
        let error = validator
            .validate(&binary, "tunnel_0123456789abcdef0123456789abcdef", &key)
            .unwrap_err();
        let rendered = format!("{error:#}");
        assert!(rendered.contains("provider rejected <redacted-runtime-key>"));
        assert!(!rendered.contains("never-print-me"));
    }

    #[test]
    fn windows_provider_environment_restores_required_os_context_only() {
        let inherited = vec![
            (OsString::from("SystemRoot"), OsString::from(r"C:\Windows")),
            (OsString::from("SYSTEMROOT"), OsString::from(r"C:\Windows")),
            (OsString::from("WINDIR"), OsString::from(r"C:\Windows")),
            (OsString::from("SystemDrive"), OsString::from("C:")),
            (OsString::from("TEMP"), OsString::from(r"C:\Temp")),
            (OsString::from("TMP"), OsString::from(r"C:\Temp")),
            (
                OsString::from("USERPROFILE"),
                OsString::from(r"C:\Users\ashray"),
            ),
            (
                OsString::from("APPDATA"),
                OsString::from(r"C:\Users\ashray\AppData\Roaming"),
            ),
            (
                OsString::from("LOCALAPPDATA"),
                OsString::from(r"C:\Users\ashray\AppData\Local"),
            ),
            (
                OsString::from("PROGRAMDATA"),
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
                OsString::from("HTTPS_PROXY"),
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
        ] {
            assert!(mapped.contains_key(required), "missing {required}");
        }
        assert_eq!(
            mapped.get("PATH").map(String::as_str),
            Some(r"C:\Windows\System32")
        );
        assert_eq!(
            mapped.get("HTTPS_PROXY").map(String::as_str),
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
