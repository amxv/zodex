//! Atomic, same-path full-file replacements for Codex delete-then-add patches.
//! Keep the old file intact until the new bytes are written and ready to rename.

use std::fs;
use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result, bail};
use tempfile::NamedTempFile;

pub(super) fn replace_regular_file(path: &Path, contents: &str) -> Result<()> {
    let metadata = fs::symlink_metadata(path).with_context(|| {
        format!(
            "Cannot replace {}: original file is missing or inaccessible",
            path.display()
        )
    })?;
    if !metadata.file_type().is_file() {
        bail!(
            "Cannot replace {}: only existing regular files are supported (not directories or symlinks)",
            path.display()
        );
    }
    if metadata.permissions().readonly() {
        bail!(
            "Cannot replace {}: original file is read-only",
            path.display()
        );
    }

    // A directory's write permission alone can permit replacing a file which
    // the caller cannot modify. Preserve the normal write-access requirement.
    fs::OpenOptions::new()
        .write(true)
        .open(path)
        .with_context(|| {
            format!(
                "Cannot replace {}: no write access to original file",
                path.display()
            )
        })?;

    let parent = path
        .parent()
        .context("Replacement file has no parent directory")?;
    let mut staged = NamedTempFile::new_in(parent)
        .with_context(|| format!("Cannot stage replacement next to {}", path.display()))?;
    staged
        .write_all(contents.as_bytes())
        .with_context(|| format!("Cannot write replacement for {}", path.display()))?;
    staged
        .as_file()
        .set_permissions(metadata.permissions())
        .with_context(|| format!("Cannot preserve permissions on {}", path.display()))?;
    staged
        .as_file_mut()
        .sync_all()
        .with_context(|| format!("Cannot flush replacement for {}", path.display()))?;

    // Same-directory rename replaces the complete file, rather than leaving it
    // deleted or truncated if staging, permissions or writes fail. On Windows,
    // an open/locked destination can make persist fail without losing the old file.
    staged
        .persist(path)
        .map_err(|error| error.error)
        .with_context(|| format!("Cannot install replacement for {}", path.display()))?;
    Ok(())
}
