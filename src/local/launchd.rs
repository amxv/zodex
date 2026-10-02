use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};

pub const LOCAL_LAUNCHD_LABEL: &str = "com.amxv.zodex.local.runtime";
const LOCAL_OPEN_FILE_LIMIT: u32 = 10_240;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalLaunchdJob {
    pub executable: PathBuf,
    pub bootstrap_path: PathBuf,
}

impl LocalLaunchdJob {
    pub fn new(executable: impl Into<PathBuf>, bootstrap_path: impl Into<PathBuf>) -> Result<Self> {
        let job = Self {
            executable: executable.into(),
            bootstrap_path: bootstrap_path.into(),
        };
        if !job.executable.is_absolute() {
            bail!("Local launchd executable path must be absolute");
        }
        if !job.bootstrap_path.is_absolute() {
            bail!("Local launchd bootstrap path must be absolute");
        }
        Ok(job)
    }

    pub fn render_plist(&self) -> String {
        // Local serves explicit user/model requests. Background classification
        // throttles startup disk I/O during login and is inherited by tool and
        // tunnel children. Interactive keeps this user-started service responsive.
        let executable = xml_escape(&self.executable.display().to_string());
        let bootstrap = xml_escape(&self.bootstrap_path.display().to_string());
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{label}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{executable}</string>
    <string>local</string>
    <string>__runtime</string>
    <string>--bootstrap</string>
    <string>{bootstrap}</string>
  </array>
  <key>RunAtLoad</key>
  <true/>
  <key>ProcessType</key>
  <string>Interactive</string>
  <key>SoftResourceLimits</key>
  <dict>
    <key>NumberOfFiles</key>
    <integer>{open_file_limit}</integer>
  </dict>
  <key>HardResourceLimits</key>
  <dict>
    <key>NumberOfFiles</key>
    <integer>{open_file_limit}</integer>
  </dict>
  <key>StandardOutPath</key>
  <string>/dev/null</string>
  <key>StandardErrorPath</key>
  <string>/dev/null</string>
</dict>
</plist>
"#,
            label = LOCAL_LAUNCHD_LABEL,
            open_file_limit = LOCAL_OPEN_FILE_LIMIT,
        )
    }
}

pub trait LaunchdController: Send + Sync {
    fn is_loaded(&self) -> Result<bool>;
    fn bootstrap(&self, plist: &Path) -> Result<()>;
    fn bootout(&self) -> Result<()>;
}

#[derive(Debug, Clone, Copy)]
pub struct SystemLaunchdController {
    uid: u32,
}

impl SystemLaunchdController {
    #[cfg(unix)]
    pub fn for_current_user() -> Self {
        Self {
            uid: nix::unistd::Uid::effective().as_raw(),
        }
    }

    #[cfg(not(unix))]
    pub fn for_current_user() -> Self {
        Self { uid: 0 }
    }

    fn domain(&self) -> String {
        format!("gui/{}", self.uid)
    }

    fn service_target(&self) -> String {
        format!("{}/{}", self.domain(), LOCAL_LAUNCHD_LABEL)
    }

    fn ensure_macos() -> Result<()> {
        if cfg!(target_os = "macos") {
            Ok(())
        } else {
            bail!("Local launchd lifecycle is only available on macOS")
        }
    }
}

impl LaunchdController for SystemLaunchdController {
    fn is_loaded(&self) -> Result<bool> {
        Self::ensure_macos()?;
        let status = Command::new("/bin/launchctl")
            .args(["print", &self.service_target()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .context("failed to inspect Zodex Local launchd job")?;
        Ok(status.success())
    }

    fn bootstrap(&self, plist: &Path) -> Result<()> {
        Self::ensure_macos()?;
        if !plist.is_absolute() {
            bail!("Local launchd plist path must be absolute");
        }
        let status = Command::new("/bin/launchctl")
            .arg("bootstrap")
            .arg(self.domain())
            .arg(plist)
            .status()
            .context("failed to bootstrap Zodex Local launchd job")?;
        if !status.success() {
            bail!("launchctl bootstrap failed for Zodex Local ({status})");
        }
        Ok(())
    }

    fn bootout(&self) -> Result<()> {
        Self::ensure_macos()?;
        if !self.is_loaded()? {
            return Ok(());
        }
        let status = Command::new("/bin/launchctl")
            .arg("bootout")
            .arg(self.service_target())
            .status()
            .context("failed to boot out Zodex Local launchd job")?;
        if !status.success() {
            bail!("launchctl bootout failed for Zodex Local ({status})");
        }
        Ok(())
    }
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    use super::{LOCAL_LAUNCHD_LABEL, LocalLaunchdJob};

    #[test]
    fn generated_launchd_job_is_ephemeral_foreground_and_never_keepalive() {
        let job = LocalLaunchdJob::new(
            "/Applications/Zodex & Tools/zodex",
            "/Users/example/Library/Application Support/zodex/runtime/bootstrap.json",
        )
        .unwrap();
        let plist = job.render_plist();

        assert!(plist.contains(LOCAL_LAUNCHD_LABEL));
        assert!(plist.contains("<string>local</string>"));
        assert!(plist.contains("<string>__runtime</string>"));
        assert!(plist.contains("<key>RunAtLoad</key>\n  <true/>"));
        assert!(plist.contains("<key>ProcessType</key>\n  <string>Interactive</string>"));
        assert!(!plist.contains("KeepAlive"));
        assert!(!plist.contains("~/Library/LaunchAgents"));
        assert!(!plist.contains("EnvironmentVariables"));
        assert!(plist.contains("<key>SoftResourceLimits</key>"));
        assert!(plist.contains("<key>HardResourceLimits</key>"));
        assert_eq!(plist.matches("<integer>10240</integer>").count(), 2);
        assert!(plist.contains("Zodex &amp; Tools"));
        assert!(plist.contains("/dev/null"));
    }
}
