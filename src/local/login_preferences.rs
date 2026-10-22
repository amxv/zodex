//! Shared, non-secret desktop login preferences. The CLI, native macOS menu
//! bar app, and Windows/Linux Rust tray all use this JSON document.

use std::{
    env, fs, io,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use fs2::FileExt as _;
use serde::{Deserialize, Serialize};

use super::LocalPaths;

pub const DEFAULT_LIVEBOARD_PORT: u16 = 64_973;
pub const AUTOMATIC_UPDATE_INTERVAL_SECONDS: i64 = 24 * 60 * 60;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct LocalLoginPreferences {
    pub start_folder: Option<PathBuf>,
    pub launch_tunnel_at_login: bool,
    pub automatic_updates: bool,
    pub last_automatic_update_check: Option<i64>,
    pub liveboard_port: u16,
}

impl Default for LocalLoginPreferences {
    fn default() -> Self {
        Self {
            start_folder: None,
            launch_tunnel_at_login: false,
            automatic_updates: true,
            last_automatic_update_check: None,
            liveboard_port: DEFAULT_LIVEBOARD_PORT,
        }
    }
}

impl LocalLoginPreferences {
    pub fn path(paths: &LocalPaths) -> PathBuf {
        // Same file used by the existing Windows/Linux tray. On macOS the
        // menu bar imports its previous UserDefaults startFolder once.
        paths.config_file().with_file_name("tray.json")
    }

    pub fn load(paths: &LocalPaths) -> Result<Self> {
        let path = Self::path(paths);
        let prefs: Self = match fs::read(&path) {
            Ok(data) => serde_json::from_slice(&data)
                .with_context(|| format!("Invalid Zodex login preferences at {}", path.display())),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error).with_context(|| format!("Could not read {}", path.display())),
        }?;
        Self::validate_liveboard_port(prefs.liveboard_port)?;
        Ok(prefs)
    }

    pub fn save(&self, paths: &LocalPaths) -> Result<()> {
        let path = Self::path(paths);
        let parent = path
            .parent()
            .context("Zodex login preferences have no parent folder")?;
        fs::create_dir_all(parent)
            .with_context(|| format!("Could not create {}", parent.display()))?;
        let value = serde_json::to_vec_pretty(self)?;
        fs::write(&path, value).with_context(|| {
            format!(
                "Could not save Zodex login preferences at {}",
                path.display()
            )
        })
    }

    pub fn set_start_folder(&mut self, path: &Path) -> Result<()> {
        self.start_folder = Some(super::validate_runtime_start_directory(path)?);
        Ok(())
    }

    /// A missing saved choice means the logged-in user's home, not an unusable
    /// menu. Never replace a previously selected (even unavailable) folder.
    pub fn effective_start_folder(&self) -> Result<PathBuf> {
        match &self.start_folder {
            Some(folder) => Ok(folder.clone()),
            None => {
                let home = env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
                    .filter(|value| !value.is_empty())
                    .context("cannot determine the current user's home directory")?;
                super::validate_runtime_start_directory(Path::new(&home))
            }
        }
    }

    pub fn validate_liveboard_port(port: u16) -> Result<()> {
        if port < 1024 {
            bail!("Liveboard port must be between 1024 and 65535");
        }
        Ok(())
    }

    /// Mutations from the CLI and either desktop app must not race or
    /// overwrite an independently changed start folder/login setting.
    pub fn update<T>(paths: &LocalPaths, change: impl FnOnce(&mut Self) -> Result<T>) -> Result<T> {
        let lock_path = Self::path(paths).with_file_name("tray-prefs.lock");
        fs::create_dir_all(
            lock_path
                .parent()
                .context("missing preferences directory")?,
        )?;
        let lock_file = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(lock_path)?;
        lock_file.lock_exclusive()?;
        let mut prefs = Self::load(paths)?;
        let result = change(&mut prefs)?;
        prefs.save(paths)?;
        Ok(result)
    }

    /// Reserve at most one automatic check per day. Persist before contacting
    /// GitHub so a temporary failure does not create a tight retry loop.
    pub fn claim_automatic_update_check(&mut self, now: i64) -> bool {
        if !self.automatic_updates {
            return false;
        }
        if self.last_automatic_update_check.is_some_and(|last| {
            now >= last && now.saturating_sub(last) < AUTOMATIC_UPDATE_INTERVAL_SECONDS
        }) {
            return false;
        }
        self.last_automatic_update_check = Some(now);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_no_tunnel_autostart_and_preserves_existing_tray_folders() {
        let preferences: LocalLoginPreferences =
            serde_json::from_str(r#"{"start_folder":"/work/projects"}"#).unwrap();
        assert_eq!(
            preferences.start_folder,
            Some(PathBuf::from("/work/projects"))
        );
        assert!(!preferences.launch_tunnel_at_login);
        assert!(preferences.automatic_updates);
        assert_eq!(preferences.liveboard_port, DEFAULT_LIVEBOARD_PORT);
    }

    #[test]
    fn persists_across_processes_and_can_be_disabled_without_losing_folder() {
        let root = tempfile::tempdir().unwrap();
        let paths = LocalPaths::from_roots(
            root.path().join("config"),
            root.path().join("data"),
            root.path().join("state"),
        )
        .unwrap();
        let mut prefs = LocalLoginPreferences::load(&paths).unwrap();
        prefs.set_start_folder(root.path()).unwrap();
        prefs.launch_tunnel_at_login = true;
        prefs.save(&paths).unwrap();
        let loaded = LocalLoginPreferences::load(&paths).unwrap();
        assert_eq!(loaded, prefs);
        prefs.launch_tunnel_at_login = false;
        prefs.save(&paths).unwrap();
        assert_eq!(
            LocalLoginPreferences::load(&paths).unwrap().start_folder,
            loaded.start_folder
        );
        assert!(
            !LocalLoginPreferences::load(&paths)
                .unwrap()
                .launch_tunnel_at_login
        );
    }

    #[test]
    fn rejects_nonexistent_folder_without_overwriting_existing_preference() {
        let root = tempfile::tempdir().unwrap();
        let mut prefs = LocalLoginPreferences::default();
        assert!(
            prefs
                .set_start_folder(&root.path().join("missing"))
                .is_err()
        );
        assert!(prefs.start_folder.is_none());
    }

    #[test]
    fn new_user_defaults_and_explicit_folder_are_distinct() {
        let prefs = LocalLoginPreferences::default();
        assert!(prefs.start_folder.is_none());
        let expected_home = PathBuf::from(
            std::env::var(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).unwrap(),
        )
        .canonicalize()
        .unwrap();
        assert_eq!(prefs.effective_start_folder().unwrap(), expected_home);
        let previously_chosen: LocalLoginPreferences =
            serde_json::from_str(r#"{"start_folder":"/previous/workspace"}"#).unwrap();
        assert_eq!(
            previously_chosen.effective_start_folder().unwrap(),
            PathBuf::from("/previous/workspace")
        );
    }

    #[test]
    fn auto_update_claim_is_daily_and_opt_out_is_preserved() {
        let mut prefs = LocalLoginPreferences::default();
        assert!(prefs.claim_automatic_update_check(100_000));
        assert!(!prefs.claim_automatic_update_check(100_001));
        assert!(
            !prefs.claim_automatic_update_check(100_000 + AUTOMATIC_UPDATE_INTERVAL_SECONDS - 1)
        );
        assert!(prefs.claim_automatic_update_check(100_000 + AUTOMATIC_UPDATE_INTERVAL_SECONDS));
        prefs.automatic_updates = false;
        assert!(!prefs.claim_automatic_update_check(300_000));
        assert_eq!(
            prefs.last_automatic_update_check,
            Some(100_000 + AUTOMATIC_UPDATE_INTERVAL_SECONDS)
        );
    }

    #[test]
    fn concurrent_daily_claims_cannot_both_win() {
        use std::sync::Arc;

        let root = tempfile::tempdir().unwrap();
        let paths = Arc::new(
            LocalPaths::from_roots(
                root.path().join("config"),
                root.path().join("data"),
                root.path().join("state"),
            )
            .unwrap(),
        );
        let threads = (0..8)
            .map(|_| {
                let paths = paths.clone();
                std::thread::spawn(move || {
                    LocalLoginPreferences::update(&paths, |prefs| {
                        Ok(prefs.claim_automatic_update_check(100_000))
                    })
                    .unwrap()
                })
            })
            .collect::<Vec<_>>();
        assert_eq!(
            threads
                .into_iter()
                .map(|thread| thread.join().unwrap())
                .filter(|won| *won)
                .count(),
            1
        );
    }

    #[test]
    fn configurable_liveboard_port_rejects_invalid_values() {
        assert!(LocalLoginPreferences::validate_liveboard_port(0).is_err());
        assert!(LocalLoginPreferences::validate_liveboard_port(1023).is_err());
        assert!(LocalLoginPreferences::validate_liveboard_port(1024).is_ok());
        assert!(LocalLoginPreferences::validate_liveboard_port(65535).is_ok());
    }
}
