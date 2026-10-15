//! Shared, non-secret desktop login preferences. The CLI, native macOS menu
//! bar app, and Windows/Linux Rust tray all use this JSON document.

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::LocalPaths;

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct LocalLoginPreferences {
    pub start_folder: Option<PathBuf>,
    pub launch_tunnel_at_login: bool,
}

impl LocalLoginPreferences {
    pub fn path(paths: &LocalPaths) -> PathBuf {
        // Same file used by the existing Windows/Linux tray. On macOS the
        // menu bar imports its previous UserDefaults startFolder once.
        paths.config_file().with_file_name("tray.json")
    }

    pub fn load(paths: &LocalPaths) -> Result<Self> {
        let path = Self::path(paths);
        match fs::read(&path) {
            Ok(data) => serde_json::from_slice(&data)
                .with_context(|| format!("Invalid Zodex login preferences at {}", path.display())),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error).with_context(|| format!("Could not read {}", path.display())),
        }
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
}
