use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalPaths {
    config_root: PathBuf,
    data_root: PathBuf,
    state_root: PathBuf,
}

impl LocalPaths {
    pub fn discover() -> Result<Self> {
        #[cfg(target_os = "windows")]
        {
            let home = env::var_os("USERPROFILE")
                .filter(|value| !value.is_empty())
                .map(PathBuf::from);
            let config_root = windows_root(
                "XDG_CONFIG_HOME",
                "APPDATA",
                home.as_deref(),
                "AppData/Roaming",
            )?;
            let data_root = windows_root(
                "XDG_DATA_HOME",
                "LOCALAPPDATA",
                home.as_deref(),
                "AppData/Local",
            )?;
            let state_root = windows_root(
                "XDG_STATE_HOME",
                "LOCALAPPDATA",
                home.as_deref(),
                "AppData/Local",
            )?;
            return Self::from_roots(config_root, data_root, state_root);
        }

        #[cfg(not(target_os = "windows"))]
        {
            let home = env::var_os("HOME")
                .filter(|value| !value.is_empty())
                .map(PathBuf::from);
            let config_root = xdg_root("XDG_CONFIG_HOME", home.as_deref(), ".config")?;
            let data_root = xdg_root("XDG_DATA_HOME", home.as_deref(), ".local/share")?;
            let state_root = xdg_root("XDG_STATE_HOME", home.as_deref(), ".local/state")?;
            Self::from_roots(config_root, data_root, state_root)
        }
    }

    pub fn from_roots(
        config_root: impl Into<PathBuf>,
        data_root: impl Into<PathBuf>,
        state_root: impl Into<PathBuf>,
    ) -> Result<Self> {
        let paths = Self {
            config_root: require_absolute("config root", config_root.into())?,
            data_root: require_absolute("data root", data_root.into())?,
            state_root: require_absolute("state root", state_root.into())?,
        };
        paths.validate_layout()?;
        Ok(paths)
    }

    pub fn config_file(&self) -> PathBuf {
        self.config_root.join("zodex/local.toml")
    }

    pub(crate) fn config_root(&self) -> &Path {
        &self.config_root
    }

    pub(crate) fn data_root(&self) -> &Path {
        &self.data_root
    }

    pub(crate) fn state_root(&self) -> &Path {
        &self.state_root
    }

    pub fn managed_tunnel_client(&self) -> PathBuf {
        self.data_root
            .join("zodex/bin")
            .join(format!("tunnel-client{}", env::consts::EXE_SUFFIX))
    }

    pub fn managed_cloudflared(&self) -> PathBuf {
        self.data_root
            .join("zodex/bin")
            .join(format!("cloudflared{}", env::consts::EXE_SUFFIX))
    }

    pub fn managed_cloudflared_manifest(&self) -> PathBuf {
        self.data_root.join("zodex/bin/cloudflared-manifest.json")
    }

    pub fn local_state_root(&self) -> PathBuf {
        self.state_root.join("zodex/local")
    }

    pub fn credentials_dir(&self) -> PathBuf {
        self.local_state_root().join("credentials")
    }

    pub fn observability_bearer_file(&self) -> PathBuf {
        self.credentials_dir().join("observability-bearer")
    }

    pub fn runtime_key_file(&self) -> PathBuf {
        self.credentials_dir().join("openai-tunnel-runtime-key")
    }

    pub fn history_dir(&self) -> PathBuf {
        self.local_state_root().join("history")
    }

    pub fn history_database(&self) -> PathBuf {
        self.history_dir().join("history.sqlite3")
    }

    pub fn logs_dir(&self) -> PathBuf {
        self.local_state_root().join("logs")
    }

    pub fn runtime_dir(&self) -> PathBuf {
        self.local_state_root().join("runtime")
    }

    pub fn liveboard_dir(&self) -> PathBuf {
        self.local_state_root().join("liveboard")
    }

    pub fn liveboard_preferences_file(&self) -> PathBuf {
        self.liveboard_dir().join("preferences.json")
    }

    pub fn liveboard_preferences_lock_file(&self) -> PathBuf {
        self.liveboard_dir().join("preferences.lock")
    }

    pub fn lifecycle_lock_file(&self) -> PathBuf {
        self.local_state_root().join("lifecycle.lock")
    }

    pub fn runtime_state_file(&self) -> PathBuf {
        self.runtime_dir().join("state.json")
    }

    pub fn discovery_file(&self) -> PathBuf {
        self.runtime_dir().join("discovery.json")
    }

    pub fn liveboard_discovery_file(&self) -> PathBuf {
        self.runtime_dir().join("liveboard.json")
    }

    pub fn owned_process_registry_file(&self) -> PathBuf {
        self.runtime_dir().join("owned-processes.json")
    }

    pub fn environment_handoff_file(&self) -> PathBuf {
        self.runtime_dir().join("environment.json")
    }

    pub fn runtime_bootstrap_file(&self) -> PathBuf {
        self.runtime_dir().join("bootstrap.json")
    }

    pub fn launchd_plist_file(&self) -> PathBuf {
        self.runtime_dir().join("local-launch-agent.plist")
    }

    pub fn mcp_token_file(&self) -> PathBuf {
        self.runtime_dir().join("mcp-token")
    }

    pub fn tunnel_profile_file(&self) -> PathBuf {
        self.runtime_dir().join("tunnel-profile.yaml")
    }

    pub fn tunnel_health_url_file(&self) -> PathBuf {
        self.runtime_dir().join("tunnel-health-url")
    }

    pub fn tunnel_process_state_file(&self) -> PathBuf {
        self.runtime_dir().join("tunnel-process.json")
    }

    pub fn stop_request_file(&self) -> PathBuf {
        self.runtime_dir().join("stop-request")
    }

    pub fn diagnostic_log_file(&self) -> PathBuf {
        self.logs_dir().join("local-runtime.log")
    }

    pub fn ensure_persistent_dirs(&self) -> Result<()> {
        for path in [
            self.credentials_dir(),
            self.history_dir(),
            self.logs_dir(),
            self.liveboard_dir(),
        ] {
            fs::create_dir_all(&path)
                .with_context(|| format!("failed to create {}", path.display()))?;
            super::private_fs::set_user_only_directory(&path)?;
        }
        Ok(())
    }

    pub fn clear_runtime_state(&self) -> Result<()> {
        self.validate_layout()?;
        let runtime = self.runtime_dir();
        if runtime.exists() {
            fs::remove_dir_all(&runtime)
                .with_context(|| format!("failed to remove runtime state {}", runtime.display()))?;
        }
        Ok(())
    }

    fn validate_layout(&self) -> Result<()> {
        let local_state = self.local_state_root();
        let runtime = self.runtime_dir();
        if runtime == local_state || !runtime.starts_with(&local_state) {
            bail!("Local runtime directory must be a child of the Local state root");
        }
        for persistent in [
            self.credentials_dir(),
            self.history_dir(),
            self.logs_dir(),
            self.liveboard_dir(),
        ] {
            if persistent.starts_with(&runtime) || runtime.starts_with(&persistent) {
                bail!(
                    "Local persistent path {} must be disjoint from runtime path {}",
                    persistent.display(),
                    runtime.display()
                );
            }
        }
        Ok(())
    }
}

#[cfg(not(target_os = "windows"))]
fn xdg_root(variable: &str, home: Option<&Path>, fallback: &str) -> Result<PathBuf> {
    if let Some(value) = env::var_os(variable).filter(|value| !value.is_empty()) {
        return require_absolute(variable, PathBuf::from(value));
    }
    let home = home.with_context(|| format!("HOME must be set when {variable} is not set"))?;
    Ok(home.join(fallback))
}

#[cfg(target_os = "windows")]
fn windows_root(
    xdg_variable: &str,
    windows_variable: &str,
    home: Option<&Path>,
    fallback: &str,
) -> Result<PathBuf> {
    if let Some(value) = env::var_os(xdg_variable).filter(|value| !value.is_empty()) {
        return require_absolute(xdg_variable, PathBuf::from(value));
    }
    if let Some(value) = env::var_os(windows_variable).filter(|value| !value.is_empty()) {
        return require_absolute(windows_variable, PathBuf::from(value));
    }
    let home = home.with_context(|| {
        format!(
            "USERPROFILE must be set when {xdg_variable} and {windows_variable} are unavailable"
        )
    })?;
    Ok(home.join(fallback))
}

fn require_absolute(label: &str, path: PathBuf) -> Result<PathBuf> {
    if !path.is_absolute() {
        bail!("{label} must be an absolute path: {}", path.display());
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::LocalPaths;

    #[test]
    fn local_paths_separate_durable_and_runtime_state() {
        let dir = tempdir().unwrap();
        let paths = LocalPaths::from_roots(
            dir.path().join("config"),
            dir.path().join("data"),
            dir.path().join("state"),
        )
        .unwrap();

        assert!(paths.config_file().ends_with("zodex/local.toml"));
        assert_eq!(
            paths.managed_tunnel_client().file_name().unwrap(),
            format!("tunnel-client{}", std::env::consts::EXE_SUFFIX).as_str()
        );
        assert_eq!(
            paths.managed_cloudflared().file_name().unwrap(),
            format!("cloudflared{}", std::env::consts::EXE_SUFFIX).as_str()
        );
        assert!(
            paths
                .managed_cloudflared_manifest()
                .ends_with("zodex/bin/cloudflared-manifest.json")
        );
        assert!(
            paths
                .observability_bearer_file()
                .ends_with("local/credentials/observability-bearer")
        );
        assert!(
            paths
                .history_database()
                .ends_with("local/history/history.sqlite3")
        );
        assert!(paths.logs_dir().ends_with("local/logs"));
        assert!(
            paths
                .liveboard_preferences_file()
                .ends_with("local/liveboard/preferences.json")
        );
        assert!(
            paths
                .liveboard_preferences_lock_file()
                .ends_with("local/liveboard/preferences.lock")
        );
        assert!(paths.runtime_dir().ends_with("local/runtime"));
        assert!(
            paths
                .liveboard_discovery_file()
                .ends_with("local/runtime/liveboard.json")
        );
        assert_ne!(
            paths.liveboard_discovery_file(),
            paths.liveboard_preferences_file()
        );
        assert!(
            paths
                .owned_process_registry_file()
                .ends_with("local/runtime/owned-processes.json")
        );
        assert!(!paths.history_dir().starts_with(paths.runtime_dir()));
        assert!(!paths.credentials_dir().starts_with(paths.runtime_dir()));
    }

    #[test]
    fn runtime_cleanup_cannot_delete_persistent_state() {
        let dir = tempdir().unwrap();
        let paths = LocalPaths::from_roots(
            dir.path().join("config"),
            dir.path().join("data"),
            dir.path().join("state"),
        )
        .unwrap();
        paths.ensure_persistent_dirs().unwrap();
        std::fs::create_dir_all(paths.runtime_dir()).unwrap();
        std::fs::write(paths.history_dir().join("keep"), "history").unwrap();
        std::fs::write(paths.credentials_dir().join("keep"), "credential").unwrap();
        std::fs::write(paths.logs_dir().join("keep"), "log").unwrap();
        std::fs::write(paths.liveboard_dir().join("keep"), "liveboard").unwrap();
        std::fs::write(paths.runtime_dir().join("remove"), "runtime").unwrap();

        paths.clear_runtime_state().unwrap();

        assert!(!paths.runtime_dir().exists());
        assert!(paths.history_dir().join("keep").exists());
        assert!(paths.credentials_dir().join("keep").exists());
        assert!(paths.logs_dir().join("keep").exists());
        assert!(paths.liveboard_dir().join("keep").exists());
    }

    #[test]
    fn relative_roots_are_rejected() {
        assert!(LocalPaths::from_roots("relative", "/tmp/data", "/tmp/state").is_err());
    }
}
