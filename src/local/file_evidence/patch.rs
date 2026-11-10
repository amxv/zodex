use codex_apply_patch::AppliedPatchFileChange;

use super::{
    CompletedFileEvidence, FileCapturePlan, FileEvidenceSource, FileOperationHint, FileSnapshot,
    MAX_FILE_EVIDENCE_BYTES, MAX_FILE_EVIDENCE_FILES, PendingFileEvidence,
};
use crate::apply_patch::PatchEvidence;

/// These snapshots come from the patch executor's necessary reads/writes, never
/// from delayed reads of a filesystem another invocation may already have changed.
pub(crate) fn committed_patch_evidence(
    evidence: PatchEvidence,
) -> (
    Vec<PendingFileEvidence>,
    Vec<CompletedFileEvidence>,
    Option<String>,
) {
    let mut warnings = Vec::new();
    if !evidence.delta.is_exact() {
        warnings.push("Only confirmed committed changes are shown; additional partial writes may be unavailable.".to_owned());
    }
    let total = evidence
        .delta
        .changes()
        .len()
        .saturating_add(evidence.unavailable_paths.len());
    if total > MAX_FILE_EVIDENCE_FILES {
        warnings.push(format!(
            "{} additional file operation(s) omitted from bounded patch evidence.",
            total - MAX_FILE_EVIDENCE_FILES
        ));
    }
    let mut remaining_bytes = MAX_FILE_EVIDENCE_BYTES;
    let mut snapshot = |text: &str| {
        if text.len() as u64 > remaining_bytes {
            FileSnapshot::Unavailable(
                "Committed patch content omitted: bounded evidence byte budget exceeded".to_owned(),
            )
        } else {
            remaining_bytes -= text.len() as u64;
            FileSnapshot::Text(text.to_owned())
        }
    };
    let mut before = Vec::new();
    let mut after = Vec::new();
    for (ordinal, entry) in evidence
        .delta
        .changes()
        .iter()
        .take(MAX_FILE_EVIDENCE_FILES)
        .enumerate()
    {
        let path = std::path::PathBuf::from(entry.path.inferred_native_path_string());
        let (plan, snapshot, destination, updated, source_after) = match &entry.change {
            AppliedPatchFileChange::Add {
                content,
                overwritten_content,
            } => (
                FileCapturePlan {
                    source: FileEvidenceSource::ApplyPatch,
                    operation: if overwritten_content.is_some() {
                        FileOperationHint::Overwrite
                    } else {
                        FileOperationHint::Create
                    },
                    path_before: path.clone(),
                    path_after: path,
                },
                overwritten_content
                    .as_deref()
                    .map(&mut snapshot)
                    .unwrap_or(FileSnapshot::Missing),
                None,
                snapshot(content),
                None,
            ),
            AppliedPatchFileChange::Delete { content } => (
                FileCapturePlan {
                    source: FileEvidenceSource::ApplyPatch,
                    operation: FileOperationHint::Delete,
                    path_before: path.clone(),
                    path_after: path,
                },
                snapshot(content),
                None,
                FileSnapshot::Missing,
                None,
            ),
            AppliedPatchFileChange::Update {
                move_path,
                old_content,
                overwritten_move_content,
                new_content,
            } => {
                let moved = move_path
                    .as_ref()
                    .map(|value| std::path::PathBuf::from(value.inferred_native_path_string()));
                let destination = moved.as_ref().map(|_| {
                    overwritten_move_content
                        .as_deref()
                        .map(&mut snapshot)
                        .unwrap_or(FileSnapshot::Missing)
                });
                let source_after = moved.as_ref().map(|_| FileSnapshot::Missing);
                (
                    FileCapturePlan {
                        source: FileEvidenceSource::ApplyPatch,
                        operation: if moved.is_some() {
                            FileOperationHint::Move
                        } else {
                            FileOperationHint::Update
                        },
                        path_before: path.clone(),
                        path_after: moved.unwrap_or(path),
                    },
                    snapshot(old_content),
                    destination,
                    snapshot(new_content),
                    source_after,
                )
            }
        };
        before.push(PendingFileEvidence {
            ordinal: ordinal as u32,
            plan,
            before: snapshot,
            destination_before: destination,
        });
        after.push(CompletedFileEvidence {
            ordinal: ordinal as u32,
            after: updated,
            source_after,
        });
    }
    let available_slots = MAX_FILE_EVIDENCE_FILES.saturating_sub(before.len());
    for path in evidence.unavailable_paths.into_iter().take(available_slots) {
        let ordinal = before.len() as u32;
        let unavailable = FileSnapshot::Unavailable(
            "Atomic replacement contents unavailable without additional filesystem reads; inspect the tool result for the mutation outcome.".to_owned(),
        );
        before.push(PendingFileEvidence {
            ordinal,
            plan: FileCapturePlan {
                source: FileEvidenceSource::ApplyPatch,
                operation: FileOperationHint::Update,
                path_before: path.clone(),
                path_after: path,
            },
            before: unavailable.clone(),
            destination_before: None,
        });
        after.push(CompletedFileEvidence {
            ordinal,
            after: unavailable,
            source_after: None,
        });
    }
    let warning = (!warnings.is_empty()).then(|| warnings.join(" "));
    (before, after, warning)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apply_patch::apply_patch_with_delta;
    use std::fs;
    use tempfile::tempdir;

    #[tokio::test]
    async fn mixed_replacement_keeps_exact_delta_without_reading_current_files() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("replace"), "old\n").unwrap();
        let path = dir.path().join("replace");
        let (result, evidence) = apply_patch_with_delta(
            "*** Begin Patch\n*** Add File: exact\n+committed\n*** Delete File: replace\n*** Add File: replace\n+replacement\n*** End Patch",
            dir.path().to_str().unwrap(),
        ).await;
        result.unwrap();
        // Conversion must not read files, including files modified after execution.
        dir.close().unwrap();
        let (before, after, warning) = committed_patch_evidence(evidence);
        assert!(warning.is_none());
        assert_eq!(before.len(), 2);
        assert_eq!(before[0].before, FileSnapshot::Missing);
        assert_eq!(after[0].after, FileSnapshot::Text("committed\n".into()));
        assert_eq!(before[1].plan.path_before, path);
        assert!(matches!(before[1].before, FileSnapshot::Unavailable(_)));
        assert!(matches!(after[1].after, FileSnapshot::Unavailable(_)));
    }

    #[tokio::test]
    async fn file_count_limit_keeps_prefix_and_reports_omitted_operations() {
        let dir = tempdir().unwrap();
        let mut patch = "*** Begin Patch\n".to_owned();
        for index in 0..MAX_FILE_EVIDENCE_FILES + 2 {
            patch.push_str(&format!("*** Add File: file-{index}\n+value\n"));
        }
        patch.push_str("*** End Patch");
        let (result, evidence) = apply_patch_with_delta(&patch, dir.path().to_str().unwrap()).await;
        result.unwrap();
        let (before, after, warning) = committed_patch_evidence(evidence);
        assert_eq!(before.len(), MAX_FILE_EVIDENCE_FILES);
        assert_eq!(after.len(), MAX_FILE_EVIDENCE_FILES);
        assert!(
            warning
                .unwrap()
                .contains("2 additional file operation(s) omitted")
        );
        assert!(
            after
                .iter()
                .all(|item| item.after == FileSnapshot::Text("value\n".into()))
        );
    }

    #[tokio::test]
    async fn byte_budget_keeps_actual_text_and_explicitly_marks_omissions() {
        let dir = tempdir().unwrap();
        let content = "a".repeat(MAX_FILE_EVIDENCE_BYTES as usize / 2);
        let patch = format!(
            "*** Begin Patch\n*** Add File: first\n+{content}\n*** Add File: second\n+{content}\n*** Add File: tiny\n+small\n*** End Patch"
        );
        let (result, evidence) = apply_patch_with_delta(&patch, dir.path().to_str().unwrap()).await;
        result.unwrap();
        let (before, after, _) = committed_patch_evidence(evidence);
        assert_eq!(before.len(), 3);
        assert_eq!(after[0].after, FileSnapshot::Text(format!("{content}\n")));
        assert!(
            matches!(&after[1].after, FileSnapshot::Unavailable(reason) if reason.contains("byte budget"))
        );
        assert_eq!(after[2].after, FileSnapshot::Text("small\n".into()));
        let retained_bytes: usize = before
            .iter()
            .map(|item| item.before.text().map_or(0, str::len))
            .sum::<usize>()
            + after
                .iter()
                .map(|item| item.after.text().map_or(0, str::len))
                .sum::<usize>();
        assert!(retained_bytes <= MAX_FILE_EVIDENCE_BYTES as usize);
    }

    #[test]
    fn unavailable_paths_also_obey_file_count_bound() {
        let evidence = PatchEvidence {
            delta: Default::default(),
            unavailable_paths: (0..MAX_FILE_EVIDENCE_FILES + 1)
                .map(|index| std::path::PathBuf::from(format!("/not-read/{index}")))
                .collect(),
        };
        let (before, after, warning) = committed_patch_evidence(evidence);
        assert_eq!(before.len(), MAX_FILE_EVIDENCE_FILES);
        assert_eq!(after.len(), MAX_FILE_EVIDENCE_FILES);
        assert!(
            warning
                .unwrap()
                .contains("1 additional file operation(s) omitted")
        );
        assert!(
            after
                .iter()
                .all(|item| matches!(item.after, FileSnapshot::Unavailable(_)))
        );
    }

    #[test]
    fn unavailable_only_is_not_misrepresented_as_no_edits() {
        let path = std::path::PathBuf::from("/not-read/replaced");
        let (before, after, warning) = committed_patch_evidence(PatchEvidence {
            delta: Default::default(),
            unavailable_paths: vec![path.clone()],
        });
        assert!(warning.is_none());
        assert_eq!(before.len(), 1);
        assert_eq!(before[0].plan.path_after, path);
        assert_eq!(after.len(), 1);
        assert!(matches!(before[0].before, FileSnapshot::Unavailable(_)));
        assert!(matches!(after[0].after, FileSnapshot::Unavailable(_)));
    }
}
