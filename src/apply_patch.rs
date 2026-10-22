mod recovery;

use std::collections::HashSet;
use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};

use anyhow::{Result, anyhow, bail};
use codex_apply_patch::{ApplyPatchFailure, Hunk, ParseError};
use codex_exec_server::LOCAL_FS;
use codex_utils_path_uri::PathUri;

use crate::workdir::validate_absolute_existing_workdir;

const ADD_FILE_PREFIX: &str = "*** Add File: ";
const UPDATE_FILE_PREFIX: &str = "*** Update File: ";
const DELETE_FILE_PREFIX: &str = "*** Delete File: ";
const MOVE_TO_PREFIX: &str = "*** Move to: ";

/// Apply independent file operations even when another file has stale context.
/// Each update hunk is calculated by upstream Codex before its file is written.
/// Failures are returned as a single MCP error containing both the committed
/// operations and the operations the agent needs to retry.
pub async fn apply_patch(patch: &str, workdir: &str) -> Result<String> {
    validate_absolute_existing_workdir(workdir)?;
    let rewritten_patch = rewrite_patch_paths(patch, workdir)?;
    let parsed = codex_apply_patch::parse_patch(&rewritten_patch).map_err(format_parse_error)?;
    if parsed.hunks.is_empty() {
        bail!("No files were modified.");
    }

    let cwd = PathUri::from_host_native_path(Path::new(workdir))?;
    let conflicts = conflicting_hunks(&parsed.hunks);
    let mut applied = Vec::new();
    let mut failed = Vec::new();
    let mut recovered = Vec::new();

    // Preserve operation order, but don't let one file's failure stop unrelated
    // files later in the batch. Overlapping paths (including move destinations)
    // are intentionally excluded from partial application.
    for (index, hunk) in parsed.hunks.iter().enumerate() {
        let label = hunk_label(hunk);
        if conflicts.contains(&index) {
            failed.push(format!(
                "{label}: another operation in this patch targets the same path; combine those changes into a single file section"
            ));
            continue;
        }

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let attempt = codex_apply_patch::apply_hunks(
            std::slice::from_ref(hunk),
            &cwd,
            &mut stdout,
            &mut stderr,
            LOCAL_FS.as_ref(),
            None,
        )
        .await;

        match attempt {
            Ok(_) => applied.push(label),
            Err(failure) => {
                // A failed *matching* operation has not written its file. Only
                // then is it safe to try uniquely located, reordered hunks.
                let reason = failure_reason(&failure, &stderr);
                if is_context_failure(&reason)
                    && failure.delta().is_empty()
                    && failure.delta().is_exact()
                    && let Some(reordered) = recovery::reorder_unique_nonoverlapping_hunks(hunk)
                {
                    let mut retry_stdout = Vec::new();
                    let mut retry_stderr = Vec::new();
                    match codex_apply_patch::apply_hunks(
                        &[reordered],
                        &cwd,
                        &mut retry_stdout,
                        &mut retry_stderr,
                        LOCAL_FS.as_ref(),
                        None,
                    )
                    .await
                    {
                        Ok(_) => {
                            applied.push(label.clone());
                            recovered.push(label);
                            continue;
                        }
                        Err(retry_failure)
                            if !retry_failure.delta().is_empty()
                                || !retry_failure.delta().is_exact() =>
                        {
                            failed.push(format_failure(&label, &retry_failure, &retry_stderr));
                            continue;
                        }
                        Err(_) => {} // Keep the original actionable error.
                    }
                }
                failed.push(format_failure(&label, &failure, &stderr));
            }
        }
    }

    let mut report = String::new();
    if failed.is_empty() {
        report.push_str("Success. Updated the following files:\n");
    } else if applied.is_empty() {
        report.push_str("Patch failed. No file operations completed successfully; inspect the failed operation details for possible partial writes.\n");
    } else {
        report.push_str(&format!(
            "Partial success: {} file operation(s) applied; {} failed.\nSuccessfully applied:\n",
            applied.len(),
            failed.len()
        ));
    }

    for label in &applied {
        report.push_str(label);
        report.push('\n');
    }
    if !recovered.is_empty() {
        report.push_str("Safely recovered out-of-order hunks in:\n");
        for label in recovered {
            report.push_str(&label);
            report.push('\n');
        }
    }
    if !failed.is_empty() {
        report.push_str(
            "Failed operations (retry only these after inspecting their current files):\n",
        );
        for reason in failed {
            report.push_str("- ");
            report.push_str(&reason);
            report.push('\n');
        }
        report.push_str(
            "Successful operations above have already been committed; do not resend them.\n",
        );
        bail!("{}", report.trim_end());
    }
    Ok(report)
}

fn format_parse_error(error: ParseError) -> anyhow::Error {
    match error {
        ParseError::InvalidPatchError(message) => anyhow!("Invalid patch: {message}"),
        ParseError::InvalidHunkError {
            message,
            line_number,
        } => anyhow!("Invalid patch hunk on line {line_number}: {message}"),
    }
}

fn is_context_failure(reason: &str) -> bool {
    reason.starts_with("Failed to find expected lines in ")
        || reason.starts_with("Failed to find context '")
}

fn failure_reason(failure: &ApplyPatchFailure, stderr: &[u8]) -> String {
    let from_stderr = String::from_utf8_lossy(stderr);
    let message = from_stderr.trim();
    if message.is_empty() {
        failure.to_string()
    } else {
        message.to_owned()
    }
}

fn format_failure(label: &str, failure: &ApplyPatchFailure, stderr: &[u8]) -> String {
    let mut result = format!("{label}: {}", failure_reason(failure, stderr));
    if !failure.delta().is_empty() {
        result.push_str("\n  Changes already committed by this failed operation:");
        for change in failure.delta().changes() {
            result.push_str("\n  - ");
            result.push_str(&change.path.inferred_native_path_string());
        }
    }
    if !failure.delta().is_exact() {
        result.push_str("\n  WARNING: the filesystem may have additional partial writes. Inspect before retrying.");
    } else if failure.delta().is_empty() {
        result.push_str("\n  No changes from this failed operation were applied.");
    }
    result
}

fn hunk_label(hunk: &Hunk) -> String {
    let (status, path) = match hunk {
        Hunk::AddFile { path, .. } => ("A", path.as_path()),
        Hunk::DeleteFile { path } => ("D", path.as_path()),
        Hunk::UpdateFile {
            path, move_path, ..
        } => ("M", move_path.as_deref().unwrap_or(path)),
    };
    format!("{status} {}", path.display())
}

fn conflicting_hunks(hunks: &[Hunk]) -> HashSet<usize> {
    let mut owners: Vec<(PathBuf, usize)> = Vec::new();
    let mut conflicts = HashSet::new();
    for (index, hunk) in hunks.iter().enumerate() {
        let (path, move_path) = match hunk {
            Hunk::AddFile { path, .. } | Hunk::DeleteFile { path } => (path, None),
            Hunk::UpdateFile {
                path, move_path, ..
            } => (path, move_path.as_ref()),
        };
        for path in std::iter::once(path).chain(move_path) {
            let resolved = physical_path_key(path);
            // An ancestor/descendant pair is also not independent: adding
            // `file` while adding/updating `file/child` cannot safely proceed.
            for (existing, previous) in &owners {
                if *previous != index
                    && (resolved.starts_with(existing) || existing.starts_with(&resolved))
                {
                    conflicts.insert(*previous);
                    conflicts.insert(index);
                }
            }
            owners.push((resolved, index));
        }
    }
    conflicts
}

// Physical path identity catches filesystem symlink aliases. For a new file,
// canonicalize the closest existing ancestor and append its missing suffix.
fn physical_path_key(path: &Path) -> PathBuf {
    let mut ancestor = path;
    let mut missing: Vec<OsString> = Vec::new();
    loop {
        if let Ok(mut resolved) = std::fs::canonicalize(ancestor) {
            for segment in missing.iter().rev() {
                resolved.push(segment);
            }
            return lexical_normalize(&resolved);
        }
        let Some(parent) = ancestor.parent() else {
            return lexical_normalize(path);
        };
        let Some(file_name) = ancestor.file_name() else {
            return lexical_normalize(path);
        };
        missing.push(file_name.to_os_string());
        ancestor = parent;
    }
}

// Paths were rewritten against an absolute workdir; normalize `..` and `.`
// as the final step so that simple aliases share the same identity.
fn lexical_normalize(path: &Path) -> PathBuf {
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                result.pop();
            }
            other => result.push(other.as_os_str()),
        }
    }
    result
}

fn rewrite_patch_paths(patch: &str, workdir: &str) -> Result<String> {
    let mut resolved = String::with_capacity(patch.len() + 128);
    let mut validated_workdir: Option<PathBuf> = None;

    for line in patch.lines() {
        if let Some(path) = line.strip_prefix(ADD_FILE_PREFIX) {
            let abs = resolve_patch_path(path, workdir, &mut validated_workdir)?;
            resolved.push_str(ADD_FILE_PREFIX);
            resolved.push_str(abs.to_string_lossy().as_ref());
            resolved.push('\n');
            continue;
        }
        if let Some(path) = line.strip_prefix(UPDATE_FILE_PREFIX) {
            let abs = resolve_patch_path(path, workdir, &mut validated_workdir)?;
            resolved.push_str(UPDATE_FILE_PREFIX);
            resolved.push_str(abs.to_string_lossy().as_ref());
            resolved.push('\n');
            continue;
        }
        if let Some(path) = line.strip_prefix(DELETE_FILE_PREFIX) {
            let abs = resolve_patch_path(path, workdir, &mut validated_workdir)?;
            resolved.push_str(DELETE_FILE_PREFIX);
            resolved.push_str(abs.to_string_lossy().as_ref());
            resolved.push('\n');
            continue;
        }
        if let Some(path) = line.strip_prefix(MOVE_TO_PREFIX) {
            let abs = resolve_patch_path(path, workdir, &mut validated_workdir)?;
            resolved.push_str(MOVE_TO_PREFIX);
            resolved.push_str(abs.to_string_lossy().as_ref());
            resolved.push('\n');
            continue;
        }

        resolved.push_str(line);
        resolved.push('\n');
    }

    Ok(resolved)
}

fn resolve_patch_path(
    path: &str,
    workdir: &str,
    validated_workdir: &mut Option<PathBuf>,
) -> Result<PathBuf> {
    let path = Path::new(path);
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }

    let workdir = validated_workdir
        .get_or_insert_with(|| PathBuf::from(workdir))
        .clone();

    Ok(workdir.join(path))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::apply_patch;

    #[tokio::test]
    async fn add_file_patch_creates_file() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("add.txt");
        let patch = format!(
            "*** Begin Patch\n*** Add File: {}\n+alpha\n+beta\n*** End Patch\n",
            path.display()
        );

        let output = apply_patch(&patch, dir.path().to_string_lossy().as_ref())
            .await
            .expect("apply add patch");
        assert!(output.contains("Success. Updated the following files:"));
        assert!(output.contains(&format!("A {}", path.display())));
        assert_eq!(
            fs::read_to_string(&path).expect("read added file"),
            "alpha\nbeta\n"
        );
    }

    #[tokio::test]
    async fn update_file_patch_modifies_file() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("update.txt");
        fs::write(&path, "old\n").expect("seed file");

        let patch = format!(
            "*** Begin Patch\n*** Update File: {}\n@@\n-old\n+new\n*** End Patch\n",
            path.display()
        );

        let output = apply_patch(&patch, dir.path().to_string_lossy().as_ref())
            .await
            .expect("apply update patch");
        assert!(output.contains(&format!("M {}", path.display())));
        assert_eq!(
            fs::read_to_string(&path).expect("read updated file"),
            "new\n"
        );
    }

    #[tokio::test]
    async fn delete_file_patch_removes_file() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("delete.txt");
        fs::write(&path, "to-delete\n").expect("seed file");

        let patch = format!(
            "*** Begin Patch\n*** Delete File: {}\n*** End Patch\n",
            path.display()
        );

        let output = apply_patch(&patch, dir.path().to_string_lossy().as_ref())
            .await
            .expect("apply delete patch");
        assert!(output.contains(&format!("D {}", path.display())));
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn move_patch_renames_and_updates_file() {
        let dir = tempdir().expect("tempdir");
        let src = dir.path().join("src.txt");
        let dst = dir.path().join("moved").join("dst.txt");
        fs::write(&src, "line\n").expect("seed file");

        let patch = format!(
            "*** Begin Patch\n*** Update File: {}\n*** Move to: {}\n@@\n-line\n+line2\n*** End Patch\n",
            src.display(),
            dst.display()
        );

        let output = apply_patch(&patch, dir.path().to_string_lossy().as_ref())
            .await
            .expect("apply move patch");
        assert!(output.contains(&format!("M {}", dst.display())));
        assert!(!src.exists());
        assert_eq!(
            fs::read_to_string(&dst).expect("read moved file"),
            "line2\n"
        );
    }

    #[tokio::test]
    async fn relative_paths_resolve_from_workdir() {
        let dir = tempdir().expect("tempdir");
        let nested = dir.path().join("sub");
        fs::create_dir_all(&nested).expect("create nested dir");
        let patch = "*** Begin Patch\n*** Add File: sub/relative.txt\n+hello\n*** End Patch\n";

        let output = apply_patch(patch, dir.path().to_string_lossy().as_ref())
            .await
            .expect("apply relative patch");
        let path = nested.join("relative.txt");
        assert!(output.contains(&format!("A {}", path.display())));
        assert_eq!(
            fs::read_to_string(path).expect("read added file"),
            "hello\n"
        );
    }

    #[tokio::test]
    async fn absolute_paths_still_require_valid_workdir() {
        let dir = tempdir().expect("tempdir");
        let absolute = dir.path().join("absolute.txt");
        let patch = format!(
            "*** Begin Patch\n*** Add File: {}\n+ok\n*** End Patch\n",
            absolute.display()
        );

        let err = apply_patch(&patch, "/definitely/not/a/real/workdir")
            .await
            .expect_err("declared workdir is always validated");
        assert!(err.to_string().contains("workdir does not exist"));
        assert!(!absolute.exists(), "patch side effect must not occur");
    }

    #[tokio::test]
    async fn invalid_workdir_rejected_for_relative_paths() {
        let patch = "*** Begin Patch\n*** Add File: relative.txt\n+oops\n*** End Patch\n";
        let err = apply_patch(patch, "/definitely/not/a/real/workdir")
            .await
            .expect_err("relative patch should fail with invalid workdir");
        let message = err.to_string();
        assert!(message.contains("workdir does not exist"));
    }

    #[tokio::test]
    async fn relative_declared_workdir_is_rejected_before_patch_side_effect() {
        let dir = tempdir().expect("tempdir");
        let absolute = dir.path().join("must-not-exist.txt");
        let patch = format!(
            "*** Begin Patch\n*** Add File: {}\n+oops\n*** End Patch\n",
            absolute.display()
        );

        let err = apply_patch(&patch, "relative/workdir")
            .await
            .expect_err("relative declared workdir must be rejected");
        assert!(err.to_string().contains("workdir must be an absolute path"));
        assert!(!absolute.exists(), "patch side effect must not occur");
    }

    #[tokio::test]
    async fn invalid_patch_returns_error() {
        let patch = "*** Begin Patch\n*** Add File: bad.txt\nbad-line\n*** End Patch\n";
        let workdir = std::env::current_dir().expect("test current directory");

        let err = apply_patch(patch, workdir.to_string_lossy().as_ref())
            .await
            .expect_err("invalid patch should fail");
        let message = err.to_string();
        assert!(
            message.contains("Invalid patch"),
            "unexpected error: {message}"
        );
    }

    #[tokio::test]
    async fn independent_files_succeed_after_other_files_fail() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("one.txt"), "old one\n").unwrap();
        fs::write(dir.path().join("two.txt"), "different two\n").unwrap();
        fs::write(dir.path().join("three.txt"), "old three\n").unwrap();
        fs::write(dir.path().join("four.txt"), "different four\n").unwrap();
        let patch = "*** Begin Patch
*** Update File: one.txt
@@
-old one
+new one
*** Update File: two.txt
@@
-stale two
+new two
*** Update File: three.txt
@@
-old three
+new three
*** Update File: four.txt
@@
-stale four
+new four
*** Add File: five.txt
+new five
*** End Patch";
        let result = apply_patch(patch, dir.path().to_str().unwrap()).await;
        let message = result
            .expect_err("partial success must be an MCP error")
            .to_string();

        assert!(message.contains("3 file operation(s) applied; 2 failed"));
        assert!(message.contains("M "));
        assert!(message.contains("Failed to find expected lines"));
        assert!(message.contains("two.txt"));
        assert!(message.contains("four.txt"));
        assert!(message.contains("Successful operations above have already been committed"));
        assert_eq!(
            fs::read_to_string(dir.path().join("one.txt")).unwrap(),
            "new one\n"
        );
        assert_eq!(
            fs::read_to_string(dir.path().join("three.txt")).unwrap(),
            "new three\n"
        );
        assert_eq!(
            fs::read_to_string(dir.path().join("five.txt")).unwrap(),
            "new five\n"
        );
        assert_eq!(
            fs::read_to_string(dir.path().join("two.txt")).unwrap(),
            "different two\n"
        );
        assert_eq!(
            fs::read_to_string(dir.path().join("four.txt")).unwrap(),
            "different four\n"
        );
    }

    #[tokio::test]
    async fn out_of_order_unique_hunks_recover_without_model_retry() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("ordered.txt"), "first\nmiddle\nlast\n").unwrap();
        let patch = "*** Begin Patch
*** Update File: ordered.txt
@@
-last
+LAST
@@
-first
+FIRST
*** End Patch";
        let output = apply_patch(patch, dir.path().to_str().unwrap())
            .await
            .unwrap();
        assert!(output.contains("Safely recovered out-of-order hunks"));
        assert_eq!(
            fs::read_to_string(dir.path().join("ordered.txt")).unwrap(),
            "FIRST\nmiddle\nLAST\n"
        );
    }

    #[tokio::test]
    async fn ambiguous_out_of_order_hunks_fail_without_writing_file() {
        let dir = tempdir().unwrap();
        let original = "first\nrepeat\nrepeat\nlast\n";
        fs::write(dir.path().join("ambiguous.txt"), original).unwrap();
        let patch = "*** Begin Patch
*** Update File: ambiguous.txt
@@
-last
+LAST
@@
-repeat
+REPEAT
*** End Patch";
        let error = apply_patch(patch, dir.path().to_str().unwrap())
            .await
            .expect_err("ambiguous context should not be guessed")
            .to_string();
        assert!(error.contains("No changes from this failed operation were applied"));
        assert_eq!(
            fs::read_to_string(dir.path().join("ambiguous.txt")).unwrap(),
            original
        );
    }

    #[tokio::test]
    async fn overlapping_file_sections_are_rejected_without_blocking_unrelated_files() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("duplicate.txt"), "old\n").unwrap();
        let patch = "*** Begin Patch
*** Update File: duplicate.txt
@@
-old
+first
*** Update File: ./duplicate.txt
@@
-old
+second
*** Add File: independent.txt
+done
*** End Patch";
        let error = apply_patch(patch, dir.path().to_str().unwrap())
            .await
            .expect_err("duplicate target needs a targeted error")
            .to_string();
        assert!(error.contains("another operation in this patch targets the same path"));
        assert_eq!(
            fs::read_to_string(dir.path().join("duplicate.txt")).unwrap(),
            "old\n"
        );
        assert_eq!(
            fs::read_to_string(dir.path().join("independent.txt")).unwrap(),
            "done\n"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlink_aliases_cannot_modify_the_same_file_twice() {
        use std::os::unix::fs::symlink;

        let dir = tempdir().unwrap();
        let original = dir.path().join("original.txt");
        fs::write(&original, "old\n").unwrap();
        symlink(&original, dir.path().join("alias.txt")).unwrap();
        let patch = "*** Begin Patch
*** Update File: original.txt
@@
-old
+new original
*** Update File: alias.txt
@@
-old
+new alias
*** Add File: unrelated.txt
+works
*** End Patch";

        let error = apply_patch(patch, dir.path().to_str().unwrap())
            .await
            .expect_err("symlink aliases refer to one mutable file")
            .to_string();
        assert!(error.contains("another operation in this patch targets the same path"));
        assert_eq!(fs::read_to_string(&original).unwrap(), "old\n");
        assert_eq!(
            fs::read_to_string(dir.path().join("unrelated.txt")).unwrap(),
            "works\n"
        );
    }

    #[tokio::test]
    async fn parent_child_file_conflict_does_not_create_an_incoherent_result() {
        let dir = tempdir().unwrap();
        let patch = "*** Begin Patch
*** Add File: nested
+not a directory
*** Add File: nested/child.txt
+child
*** Add File: independent.txt
+works
*** End Patch";

        let error = apply_patch(patch, dir.path().to_str().unwrap())
            .await
            .expect_err("ancestor/descendant paths conflict")
            .to_string();
        assert!(error.contains("another operation in this patch targets the same path"));
        assert!(!dir.path().join("nested").exists());
        assert_eq!(
            fs::read_to_string(dir.path().join("independent.txt")).unwrap(),
            "works\n"
        );
    }

    #[tokio::test]
    async fn preserves_original_crlf_after_update() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("crlf.txt");
        fs::write(&path, b"alpha\r\nbeta\r\ngamma\r\n").unwrap();
        let patch = "*** Begin Patch\n*** Update File: crlf.txt\n@@\n-beta\n+BETA\n*** End Patch";
        apply_patch(patch, dir.path().to_str().unwrap())
            .await
            .expect("upstream handles line endings");
        assert_eq!(fs::read(path).unwrap(), b"alpha\r\nBETA\r\ngamma\r\n");
    }
}
