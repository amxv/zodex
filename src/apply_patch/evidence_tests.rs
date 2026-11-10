use std::fs;

use codex_apply_patch::AppliedPatchFileChange;
use tempfile::tempdir;

use super::apply_patch_with_delta;

#[tokio::test]
async fn evidence_preserves_add_update_delete_and_overwritten_move_bytes() {
    let dir = tempdir().unwrap();
    for (name, contents) in [
        ("overwrite", "previous\n"),
        ("update", "before\n"),
        ("delete", "removed\n"),
        ("source", "source\n"),
        ("destination", "destination\n"),
    ] {
        fs::write(dir.path().join(name), contents).unwrap();
    }
    let patch = "*** Begin Patch
*** Add File: new
+created
*** Add File: overwrite
+replacement
*** Update File: update
@@
-before
+after
*** Delete File: delete
*** Update File: source
*** Move to: destination
@@
-source
+moved
*** End Patch";
    let (result, evidence) = apply_patch_with_delta(patch, dir.path().to_str().unwrap()).await;
    assert!(
        result
            .unwrap()
            .starts_with("Success. Updated the following files:\n")
    );
    assert!(evidence.delta.is_exact());
    assert!(evidence.unavailable_paths.is_empty());
    let changes = evidence.delta.changes();
    assert_eq!(changes.len(), 5);
    assert_eq!(
        changes[0].change,
        AppliedPatchFileChange::Add {
            content: "created\n".into(),
            overwritten_content: None,
        }
    );
    assert_eq!(
        changes[1].change,
        AppliedPatchFileChange::Add {
            content: "replacement\n".into(),
            overwritten_content: Some("previous\n".into()),
        }
    );
    assert_eq!(
        changes[2].change,
        AppliedPatchFileChange::Update {
            move_path: None,
            old_content: "before\n".into(),
            overwritten_move_content: None,
            new_content: "after\n".into(),
        }
    );
    assert_eq!(
        changes[3].change,
        AppliedPatchFileChange::Delete {
            content: "removed\n".into()
        }
    );
    match &changes[4].change {
        AppliedPatchFileChange::Update {
            move_path,
            old_content,
            overwritten_move_content,
            new_content,
        } => {
            assert_eq!(
                move_path.as_ref().unwrap().inferred_native_path_string(),
                dir.path().join("destination").to_str().unwrap()
            );
            assert_eq!(old_content, "source\n");
            assert_eq!(overwritten_move_content.as_deref(), Some("destination\n"));
            assert_eq!(new_content, "moved\n");
        }
        other => panic!("expected move evidence, got {other:?}"),
    }
}

#[tokio::test]
async fn partial_failure_keeps_committed_evidence_and_original_report() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("stale"), "actual\n").unwrap();
    let patch = "*** Begin Patch
*** Add File: first
+one
*** Update File: stale
@@
-missing
+changed
*** Add File: last
+two
*** End Patch";
    let (result, evidence) = apply_patch_with_delta(patch, dir.path().to_str().unwrap()).await;
    let error = result.unwrap_err().to_string();
    assert!(error.starts_with(
        "Partial success: 2 file operation(s) applied; 1 failed.\nSuccessfully applied:\n"
    ));
    assert!(
        error.ends_with(
            "Successful operations above are already applied to disk; do not resend them."
        )
    );
    assert_eq!(evidence.delta.changes().len(), 2);
    assert!(evidence.delta.is_exact());
    assert!(evidence.unavailable_paths.is_empty());
    assert_eq!(
        fs::read_to_string(dir.path().join("stale")).unwrap(),
        "actual\n"
    );
}

#[tokio::test]
async fn replacement_marks_unknown_before_bytes_without_fabricating_a_diff() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("replace"), [0xff, 0x00]).unwrap();
    let (result, evidence) = apply_patch_with_delta(
        "*** Begin Patch\n*** Delete File: replace\n*** Add File: replace\n+new\n*** End Patch",
        dir.path().to_str().unwrap(),
    )
    .await;
    assert_eq!(
        result.unwrap(),
        format!(
            "Success. Updated the following files:\nM {} (replaced)\n",
            dir.path().join("replace").display()
        )
    );
    assert!(evidence.delta.is_empty());
    assert_eq!(evidence.unavailable_paths, vec![dir.path().join("replace")]);
}

#[tokio::test]
async fn rejected_patch_has_empty_evidence_and_unchanged_error() {
    let dir = tempdir().unwrap();
    let (result, evidence) = apply_patch_with_delta(
        "*** Begin Patch\n*** End Patch",
        dir.path().to_str().unwrap(),
    )
    .await;
    assert_eq!(result.unwrap_err().to_string(), "No files were modified.");
    assert!(evidence.delta.is_empty());
    assert!(evidence.delta.is_exact());
    assert!(evidence.unavailable_paths.is_empty());
}

#[tokio::test]
async fn recovered_hunks_return_the_committed_retry_delta() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("ordered"), "first\nmiddle\nlast\n").unwrap();
    let (result, evidence) = apply_patch_with_delta(
        "*** Begin Patch\n*** Update File: ordered\n@@\n-last\n+LAST\n@@\n-first\n+FIRST\n*** End Patch",
        dir.path().to_str().unwrap(),
    ).await;
    assert!(
        result
            .unwrap()
            .contains("Safely recovered out-of-order hunks")
    );
    assert_eq!(evidence.delta.changes().len(), 1);
    assert_eq!(
        evidence.delta.changes()[0].change,
        AppliedPatchFileChange::Update {
            move_path: None,
            old_content: "first\nmiddle\nlast\n".into(),
            overwritten_move_content: None,
            new_content: "FIRST\nmiddle\nLAST\n".into(),
        }
    );
    assert!(evidence.delta.is_exact());
}
