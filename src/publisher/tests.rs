use base64::Engine as _;
use std::os::unix::fs::PermissionsExt;

use super::api::{GithubModeRecord, PublisherRequest};
use super::git::parse_github_remote_repo;
use super::github::TokenPermissionProfile;
use super::server::{
    decode_request, encode_direct_push_wire_request, ensure_publisher_socket_parent_dir,
};
use super::validation::{github_mode_allows_repo, github_mode_expired, resolve_publisher_target};
use super::{
    DirectPushRequest, MAX_SOCKET_REQUEST_BYTES, PublishPrRequest, SOCKET_DIR_MODE,
    build_publish_branch_name, build_publish_request, create_head_bundle, ensure_clean_worktree,
    validate_publish_request,
};
use crate::config::{Config, PublishTarget, PublisherInstallation};
use tempfile::tempdir;

#[test]
fn branch_name_uses_prefix_namespace_and_never_equals_main() {
    let branch = build_publish_branch_name("main");
    assert!(branch.starts_with("main/"));
    assert_ne!(branch, "main");
}

#[test]
fn validate_publish_request_rejects_unknown_repo_id() {
    let cfg = Config::default();
    let err = validate_publish_request(&cfg, &publish_pr_request("missing"))
        .expect_err("request should be rejected");

    assert!(
        err.to_string()
            .contains("repo is not covered by publisher installation config: missing")
    );
}

#[test]
fn validate_publish_request_accepts_account_installation_repo() {
    let cfg = Config {
        publisher_installations: vec![PublisherInstallation {
            account: "owner".to_string(),
            installation_id: 11,
            default_base: "main".to_string(),
        }],
        ..Config::default()
    };

    let (target, _) = validate_publish_request(&cfg, &publish_pr_request("owner/other"))
        .expect("account installation should authorize publish-pr");

    assert_eq!(target.id, "owner/other");
    assert_eq!(target.repo, "owner/other");
    assert_eq!(target.installation_id, 11);
    assert_eq!(target.default_base, "main");
}

#[test]
fn validate_publish_request_prefers_explicit_target_over_account_installation() {
    let cfg = Config {
        publisher_installations: vec![PublisherInstallation {
            account: "owner".to_string(),
            installation_id: 11,
            default_base: "main".to_string(),
        }],
        publisher_targets: vec![PublishTarget {
            id: "custom".to_string(),
            repo: "owner/repo".to_string(),
            default_base: "trunk".to_string(),
            installation_id: 22,
        }],
        ..Config::default()
    };

    let (target, _) = validate_publish_request(&cfg, &publish_pr_request("owner/repo"))
        .expect("explicit target should authorize publish-pr");

    assert_eq!(target.id, "custom");
    assert_eq!(target.repo, "owner/repo");
    assert_eq!(target.installation_id, 22);
    assert_eq!(target.default_base, "trunk");
}

#[test]
fn validate_publish_request_rejects_repo_outside_installation_accounts() {
    let cfg = Config {
        publisher_installations: vec![PublisherInstallation {
            account: "owner".to_string(),
            installation_id: 11,
            default_base: "main".to_string(),
        }],
        ..Config::default()
    };

    let err = validate_publish_request(&cfg, &publish_pr_request("other/repo"))
        .expect_err("outside account should be rejected");

    assert!(
        err.to_string()
            .contains("repo is not covered by publisher installation config: other/repo")
    );
}

#[test]
fn direct_push_wire_request_decodes_to_direct_push_variant() {
    let request = DirectPushRequest {
        repo: "owner/repo".to_string(),
        src: "refs/heads/smoke".to_string(),
        dst: "refs/heads/smoke".to_string(),
        force: false,
        bundle_base64: Some("YnVuZGxl".to_string()),
        src_oid: Some("0123456789abcdef0123456789abcdef01234567".to_string()),
        src_object_type: Some("commit".to_string()),
    };

    let payload = encode_direct_push_wire_request(&request).expect("encode direct push");
    let value: serde_json::Value = serde_json::from_slice(&payload).expect("json payload");
    assert_eq!(
        value.get("kind").and_then(|kind| kind.as_str()),
        Some("direct_push")
    );
    assert_eq!(
        decode_request(&payload).expect("decode direct push"),
        PublisherRequest::DirectPush(request)
    );
}

#[test]
fn legacy_publish_request_without_kind_still_decodes() {
    let request = PublishPrRequest {
        repo_id: "repo".to_string(),
        base: None,
        title: "title".to_string(),
        body: String::new(),
        draft: false,
        bundle_base64: base64::engine::general_purpose::STANDARD.encode(b"hello"),
    };
    let payload = serde_json::to_vec(&request).expect("encode publish request");

    assert_eq!(
        decode_request(&payload).expect("decode legacy publish"),
        PublisherRequest::PublishPr(request)
    );
}

#[test]
fn github_yolo_mode_scope_checks_repo_allowlist_and_expiry() {
    let record = GithubModeRecord {
        mode: "yolo".to_string(),
        all_installed: false,
        repos: vec!["owner/repo".to_string()],
        repo_grants: Vec::new(),
        expires_at_epoch_seconds: Some(u64::MAX),
    };
    assert!(github_mode_allows_repo(&record, "owner/repo"));
    assert!(!github_mode_allows_repo(&record, "owner/other"));

    let expiring = GithubModeRecord {
        expires_at_epoch_seconds: Some(1_000),
        ..record.clone()
    };
    assert!(!github_mode_expired(&expiring, 999));
    assert!(github_mode_expired(&expiring, 1_000));

    let all_installed = GithubModeRecord {
        all_installed: true,
        repos: Vec::new(),
        repo_grants: Vec::new(),
        ..record
    };
    assert!(github_mode_allows_repo(&all_installed, "owner/other"));
}

#[test]
fn publisher_target_resolves_exact_target_before_account_installation() {
    let cfg = Config {
        publisher_installations: vec![PublisherInstallation {
            account: "owner".to_string(),
            installation_id: 11,
            default_base: "main".to_string(),
        }],
        publisher_targets: vec![PublishTarget {
            id: "custom".to_string(),
            repo: "owner/repo".to_string(),
            default_base: "trunk".to_string(),
            installation_id: 22,
        }],
        ..Config::default()
    };

    let exact = resolve_publisher_target(&cfg, "owner/repo").expect("exact target");
    assert_eq!(exact.installation_id, 22);
    assert_eq!(exact.default_base, "trunk");

    let account = resolve_publisher_target(&cfg, "owner/other").expect("account target");
    assert_eq!(account.repo, "owner/other");
    assert_eq!(account.installation_id, 11);

    assert!(resolve_publisher_target(&cfg, "other/repo").is_none());
}

fn publish_pr_request(repo_id: &str) -> PublishPrRequest {
    PublishPrRequest {
        repo_id: repo_id.to_string(),
        base: None,
        title: "title".to_string(),
        body: String::new(),
        draft: false,
        bundle_base64: base64::engine::general_purpose::STANDARD.encode(b"hello"),
    }
}

#[test]
fn token_permission_profiles_keep_reader_and_publisher_separate() {
    assert_eq!(
        TokenPermissionProfile::Reader.github_permissions(),
        serde_json::json!({ "contents": "read" })
    );
    assert_eq!(
        TokenPermissionProfile::Publisher.github_permissions(),
        serde_json::json!({
            "contents": "write",
            "pull_requests": "write",
            "workflows": "write"
        })
    );
}

#[test]
fn publisher_socket_request_limit_has_base64_bundle_headroom() {
    assert_eq!(MAX_SOCKET_REQUEST_BYTES, 48 * 1024 * 1024);
}

#[test]
fn validate_publish_request_rejects_oversize_fields() {
    let cfg = Config {
        publisher_max_title_chars: 5,
        publisher_max_body_chars: 5,
        publisher_max_bundle_bytes: 4,
        publisher_targets: vec![PublishTarget {
            id: "repo".to_string(),
            repo: "owner/repo".to_string(),
            default_base: "main".to_string(),
            installation_id: 1,
        }],
        ..Config::default()
    };

    let err = validate_publish_request(
        &cfg,
        &PublishPrRequest {
            repo_id: "owner/repo".to_string(),
            base: None,
            title: "too long".to_string(),
            body: "123456".to_string(),
            draft: false,
            bundle_base64: base64::engine::general_purpose::STANDARD.encode(b"12345"),
        },
    )
    .expect_err("oversize request should fail");

    assert!(err.to_string().contains("PR title exceeds limit"));
}

#[test]
fn create_head_bundle_roundtrips_head_ref() {
    let tempdir = tempdir().expect("tempdir");
    let repo = tempdir.path().join("repo");
    std::fs::create_dir_all(&repo).expect("repo dir");

    let init_status = std::process::Command::new("git")
        .current_dir(&repo)
        .args(["init", "-q"])
        .status()
        .expect("git init");
    assert!(init_status.success(), "git init should succeed");
    std::process::Command::new("git")
        .current_dir(&repo)
        .args(["config", "user.email", "test@example.com"])
        .status()
        .expect("git config email");
    std::process::Command::new("git")
        .current_dir(&repo)
        .args(["config", "user.name", "Test"])
        .status()
        .expect("git config name");
    std::fs::write(repo.join("a.txt"), "hello\n").expect("write file");
    std::process::Command::new("git")
        .current_dir(&repo)
        .args(["add", "a.txt"])
        .status()
        .expect("git add");
    std::process::Command::new("git")
        .current_dir(&repo)
        .args(["commit", "-qm", "init"])
        .status()
        .expect("git commit");

    let bundle = create_head_bundle(&repo).expect("bundle should be created");
    let bundle_path = tempdir.path().join("request.bundle");
    std::fs::write(&bundle_path, bundle).expect("write bundle");

    let output = std::process::Command::new("git")
        .args(["bundle", "list-heads", bundle_path.to_str().unwrap()])
        .output()
        .expect("list bundle heads");
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("HEAD"));
}

#[test]
fn parse_github_remote_repo_supports_common_remote_shapes() {
    assert_eq!(
        parse_github_remote_repo("https://github.com/amxv/zodex.git"),
        Some("amxv/zodex".to_string())
    );
    assert_eq!(
        parse_github_remote_repo("ssh://git@github.com/amxv/zodex.git"),
        Some("amxv/zodex".to_string())
    );
    assert_eq!(
        parse_github_remote_repo("git@github.com:amxv/zodex.git"),
        Some("amxv/zodex".to_string())
    );
}

#[test]
fn build_publish_request_rejects_checkout_repo_mismatch() {
    let tempdir = tempdir().expect("tempdir");
    let repo = tempdir.path().join("repo");
    std::fs::create_dir_all(&repo).expect("repo dir");

    let init_status = std::process::Command::new("git")
        .current_dir(&repo)
        .args(["init", "-q"])
        .status()
        .expect("git init");
    assert!(init_status.success(), "git init should succeed");
    std::process::Command::new("git")
        .current_dir(&repo)
        .args(["config", "user.email", "test@example.com"])
        .status()
        .expect("git config email");
    std::process::Command::new("git")
        .current_dir(&repo)
        .args(["config", "user.name", "Test"])
        .status()
        .expect("git config name");
    std::process::Command::new("git")
        .current_dir(&repo)
        .args([
            "remote",
            "add",
            "origin",
            "https://github.com/amxv/other.git",
        ])
        .status()
        .expect("git remote add origin");
    std::fs::write(repo.join("a.txt"), "hello\n").expect("write file");
    std::process::Command::new("git")
        .current_dir(&repo)
        .args(["add", "a.txt"])
        .status()
        .expect("git add");
    std::process::Command::new("git")
        .current_dir(&repo)
        .args(["commit", "-qm", "init"])
        .status()
        .expect("git commit");

    let err = build_publish_request(
        &Config::default(),
        "amxv/zodex".to_string(),
        None,
        "Title".to_string(),
        String::new(),
        false,
        &repo,
    )
    .expect_err("mismatched checkout should fail");

    assert!(
        err.to_string()
            .contains("current checkout is for amxv/other")
    );
}

#[test]
fn ensure_clean_worktree_rejects_dirty_repo() {
    let tempdir = tempdir().expect("tempdir");
    let repo = tempdir.path().join("repo");
    std::fs::create_dir_all(&repo).expect("repo dir");
    std::process::Command::new("git")
        .current_dir(&repo)
        .args(["init", "-q"])
        .status()
        .expect("git init");
    std::process::Command::new("git")
        .current_dir(&repo)
        .args(["config", "user.email", "test@example.com"])
        .status()
        .expect("git config email");
    std::process::Command::new("git")
        .current_dir(&repo)
        .args(["config", "user.name", "Test"])
        .status()
        .expect("git config name");
    std::fs::write(repo.join("a.txt"), "hello\n").expect("write file");

    let err = ensure_clean_worktree(&repo).expect_err("dirty repo should fail");
    assert!(
        err.to_string()
            .contains("publish-pr requires a clean worktree")
    );
}

#[test]
fn ensure_publisher_socket_parent_dir_sets_group_traversable_mode() {
    let tempdir = tempdir().expect("tempdir");
    let socket_path = tempdir.path().join("publisher/run/zodex-prd.sock");

    ensure_publisher_socket_parent_dir(&socket_path).expect("socket parent dir");

    let metadata = std::fs::metadata(socket_path.parent().expect("socket parent"))
        .expect("socket parent metadata");
    assert_eq!(metadata.permissions().mode() & 0o777, SOCKET_DIR_MODE);
}
