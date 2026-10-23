use std::fs;

use tempfile::tempdir;

use super::apply_patch;

#[tokio::test]
async fn delete_then_add_replaces_existing_file_without_deleting_it_first() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("full rewrite.txt");
    fs::write(&file, "old content\n").unwrap();
    let patch = "*** Begin Patch
*** Delete File: full rewrite.txt
*** Add File: full rewrite.txt
+fresh content
+second line
*** Add File: independent.txt
+unrelated
*** End Patch";

    let message = apply_patch(patch, dir.path().to_str().unwrap())
        .await
        .expect("full-file replacement should succeed");
    assert!(message.contains("(replaced)"));
    assert_eq!(
        fs::read_to_string(file).unwrap(),
        "fresh content\nsecond line\n"
    );
    assert_eq!(
        fs::read_to_string(dir.path().join("independent.txt")).unwrap(),
        "unrelated\n"
    );
}

#[tokio::test]
async fn several_delete_then_add_pairs_in_one_patch_all_succeed() {
    let dir = tempdir().unwrap();
    for name in ["first.md", "second.md"] {
        fs::write(dir.path().join(name), "old\n").unwrap();
    }
    let patch = "*** Begin Patch
*** Delete File: first.md
*** Add File: ./first.md
+one
*** Delete File: second.md
*** Add File: second.md
+two
*** End Patch";
    let message = apply_patch(patch, dir.path().to_str().unwrap())
        .await
        .unwrap();
    assert_eq!(message.matches("(replaced)").count(), 2);
    assert_eq!(
        fs::read_to_string(dir.path().join("first.md")).unwrap(),
        "one\n"
    );
    assert_eq!(
        fs::read_to_string(dir.path().join("second.md")).unwrap(),
        "two\n"
    );
}

#[tokio::test]
async fn missing_original_is_not_created_but_independent_hunks_still_succeed() {
    let dir = tempdir().unwrap();
    let patch = "*** Begin Patch
*** Delete File: missing.txt
*** Add File: missing.txt
+unexpected
*** Add File: independent.txt
+works
*** End Patch";
    let error = apply_patch(patch, dir.path().to_str().unwrap())
        .await
        .expect_err("the original must exist")
        .to_string();
    assert!(error.contains("Partial success: 1 file operation(s) applied; 1 failed"));
    assert!(error.contains("original file is missing"));
    assert!(error.contains("Original file was left intact"));
    assert!(!dir.path().join("missing.txt").exists());
    assert_eq!(
        fs::read_to_string(dir.path().join("independent.txt")).unwrap(),
        "works\n"
    );
}

#[tokio::test]
async fn extra_operation_on_replacement_path_blocks_all_conflicting_hunks() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("original.txt");
    fs::write(&file, "old\n").unwrap();
    let patch = "*** Begin Patch
*** Delete File: original.txt
*** Add File: original.txt
+new
*** Update File: ./original.txt
@@
-old
+different
*** Add File: independent.txt
+works
*** End Patch";
    let error = apply_patch(patch, dir.path().to_str().unwrap())
        .await
        .expect_err("a third conflicting operation is ambiguous")
        .to_string();
    assert!(error.contains("another operation in this patch targets the same path"));
    assert_eq!(fs::read_to_string(file).unwrap(), "old\n");
    assert_eq!(
        fs::read_to_string(dir.path().join("independent.txt")).unwrap(),
        "works\n"
    );
}

#[tokio::test]
async fn reversed_or_nonadjacent_pairs_remain_conflicts() {
    for body in [
        "*** Add File: original.txt\n+new\n*** Delete File: original.txt",
        "*** Delete File: original.txt\n*** Add File: other.txt\n+other\n*** Add File: original.txt\n+new",
    ] {
        let dir = tempdir().unwrap();
        let file = dir.path().join("original.txt");
        fs::write(&file, "old\n").unwrap();
        let patch = format!("*** Begin Patch\n{body}\n*** End Patch");
        let error = apply_patch(&patch, dir.path().to_str().unwrap())
            .await
            .expect_err("only adjacent delete-then-add is unambiguous")
            .to_string();
        assert!(error.contains("another operation in this patch targets the same path"));
        assert_eq!(fs::read_to_string(file).unwrap(), "old\n");
    }
}

#[tokio::test]
async fn replacement_rejects_a_directory_without_removing_it() {
    let dir = tempdir().unwrap();
    fs::create_dir(dir.path().join("folder")).unwrap();
    let patch = "*** Begin Patch
*** Delete File: folder
*** Add File: folder
+unexpected
*** End Patch";
    let error = apply_patch(patch, dir.path().to_str().unwrap())
        .await
        .expect_err("directory replacement must be rejected")
        .to_string();
    assert!(error.contains("only existing regular files"));
    assert!(dir.path().join("folder").is_dir());
}

#[cfg(unix)]
#[tokio::test]
async fn replacement_preserves_executable_mode_and_rejects_read_only_files() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempdir().unwrap();
    let file = dir.path().join("script.sh");
    fs::write(&file, "old\n").unwrap();
    fs::set_permissions(&file, fs::Permissions::from_mode(0o751)).unwrap();
    let patch = "*** Begin Patch
*** Delete File: script.sh
*** Add File: script.sh
+new
*** End Patch";
    apply_patch(patch, dir.path().to_str().unwrap())
        .await
        .unwrap();
    assert_eq!(
        fs::metadata(&file).unwrap().permissions().mode() & 0o777,
        0o751
    );
    fs::set_permissions(&file, fs::Permissions::from_mode(0o444)).unwrap();
    let error = apply_patch(patch, dir.path().to_str().unwrap())
        .await
        .expect_err("read-only original must be rejected")
        .to_string();
    assert!(error.contains("original file is read-only"));
    assert_eq!(fs::read_to_string(&file).unwrap(), "new\n");
    fs::set_permissions(&file, fs::Permissions::from_mode(0o751)).unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn replacement_rejects_symlink_file_and_distinct_symlink_alias() {
    use std::os::unix::fs::symlink;

    let dir = tempdir().unwrap();
    let original = dir.path().join("original.txt");
    fs::write(&original, "old\n").unwrap();
    let alias = dir.path().join("alias.txt");
    symlink(&original, &alias).unwrap();
    for body in [
        "*** Delete File: alias.txt\n*** Add File: alias.txt\n+new",
        "*** Delete File: original.txt\n*** Add File: alias.txt\n+new",
    ] {
        let patch = format!("*** Begin Patch\n{body}\n*** End Patch");
        let error = apply_patch(&patch, dir.path().to_str().unwrap())
            .await
            .expect_err("symlinks must not be treated as regular replacements")
            .to_string();
        assert!(error.contains("symlink") || error.contains("same path"));
        assert_eq!(fs::read_to_string(&original).unwrap(), "old\n");
        assert_eq!(fs::read_link(&alias).unwrap(), original);
    }
}
