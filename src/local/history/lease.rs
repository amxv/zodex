use std::fs::{File, OpenOptions};
use std::path::Path;

use anyhow::{Context, Result};
use fs2::FileExt;

/// Startup/control-plane only. The OS releases this lease on crash, and no tool
/// callback accesses it. It protects memory identity/claims and queued evidence
/// from another writer or a live clear that could otherwise resurrect records.
pub(super) struct HistoryLease(File);

impl HistoryLease {
    pub(super) fn acquire(database: &Path) -> Result<Self> {
        let parent = database
            .parent()
            .context("Local history path has no parent")?;
        std::fs::create_dir_all(parent)?;
        crate::local::private_fs::set_user_only_directory(parent)?;
        let mut path = database.as_os_str().to_os_string();
        path.push(".runtime-lock");
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(&path)?;
        crate::local::private_fs::set_user_only_file(Path::new(&path))?;
        FileExt::try_lock_exclusive(&file).context(
            "Local history is in use; stop Local before clearing history or opening another writer",
        )?;
        Ok(Self(file))
    }
}

impl Drop for HistoryLease {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.0);
    }
}
