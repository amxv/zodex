use std::fs::{self, File, OpenOptions};

use anyhow::{Context, Result};
use fs2::FileExt as _;

use super::LocalPaths;

pub(super) struct LocalLifecycleLock {
    _file: File,
}

impl LocalLifecycleLock {
    pub(super) fn acquire(paths: &LocalPaths) -> Result<Self> {
        let path = paths.lifecycle_lock_file();
        let parent = path
            .parent()
            .context("Local lifecycle lock path has no parent")?;
        fs::create_dir_all(parent).with_context(|| {
            format!(
                "failed to create Local state directory {}",
                parent.display()
            )
        })?;
        super::private_fs::set_user_only_directory(parent)?;

        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let file = options
            .open(&path)
            .with_context(|| format!("failed to open Local lifecycle lock {}", path.display()))?;
        super::private_fs::set_user_only_file(&path)?;
        file.lock_exclusive().with_context(|| {
            format!("failed to acquire Local lifecycle lock {}", path.display())
        })?;
        Ok(Self { _file: file })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::time::Duration;

    use tempfile::tempdir;

    use super::LocalLifecycleLock;
    use crate::local::LocalPaths;

    #[test]
    fn lifecycle_lock_serializes_independent_callers_and_survives_runtime_cleanup() {
        let dir = tempdir().unwrap();
        let paths = LocalPaths::from_roots(
            dir.path().join("config"),
            dir.path().join("data"),
            dir.path().join("state"),
        )
        .unwrap();
        let first = LocalLifecycleLock::acquire(&paths).unwrap();
        std::fs::create_dir_all(paths.runtime_dir()).unwrap();
        std::fs::write(paths.runtime_dir().join("ephemeral"), "remove").unwrap();
        paths.clear_runtime_state().unwrap();
        assert!(paths.lifecycle_lock_file().exists());

        let (acquired, receiver) = mpsc::channel();
        let contender_paths = paths.clone();
        let thread = std::thread::spawn(move || {
            let lock = LocalLifecycleLock::acquire(&contender_paths).unwrap();
            acquired.send(()).unwrap();
            drop(lock);
        });
        assert!(receiver.recv_timeout(Duration::from_millis(50)).is_err());
        drop(first);
        receiver.recv_timeout(Duration::from_secs(1)).unwrap();
        thread.join().unwrap();
    }
}
