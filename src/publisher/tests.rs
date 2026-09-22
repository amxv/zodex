#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use super::api::{GithubModeRecord, PublisherRequest};
use super::git::parse_github_remote_repo;
use super::github::TokenPermissionProfile;
use super::server::{
    decode_publisher_metadata, decode_publisher_wire_header, encode_publisher_metadata,
    encode_publisher_wire_header, ensure_publisher_socket_parent_dir, receive_declared_bundle,
    validate_publisher_request_before_body,
};
use super::validation::{github_mode_allows_repo, github_mode_expired, resolve_publisher_target};
use super::{
    DirectPushRequest, MAX_PUBLISHER_METADATA_BYTES, PUBLISHER_STREAM_BUFFER_BYTES,
    PUBLISHER_WIRE_HEADER_BYTES, PUBLISHER_WIRE_VERSION, PublishPrRequest, SOCKET_DIR_MODE,
    build_publish_branch_name, build_publish_request, create_head_bundle, ensure_clean_worktree,
    submit_direct_push_request, submit_publish_request, validate_publish_request,
};
use crate::config::{
    Config, DEFAULT_PUBLISHER_MAX_BUNDLE_BYTES, PublishTarget, PublisherInstallation,
};
use tempfile::tempdir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};

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

    let target = validate_publish_request(&cfg, &publish_pr_request("owner/other"))
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

    let target = validate_publish_request(&cfg, &publish_pr_request("owner/repo"))
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
fn publisher_metadata_roundtrips_direct_push_without_bundle_material() {
    let request = DirectPushRequest {
        repo: "owner/repo".to_string(),
        src: "refs/heads/smoke".to_string(),
        dst: "refs/heads/smoke".to_string(),
        force: false,
        src_oid: Some("0123456789abcdef0123456789abcdef01234567".to_string()),
        src_object_type: Some("commit".to_string()),
    };

    let payload = encode_publisher_metadata(&PublisherRequest::DirectPush(request.clone()))
        .expect("encode direct push metadata");
    let value: serde_json::Value = serde_json::from_slice(&payload).expect("json payload");
    assert_eq!(
        value.get("kind").and_then(|kind| kind.as_str()),
        Some("direct_push")
    );
    assert_eq!(
        decode_publisher_metadata(&payload).expect("decode direct push"),
        PublisherRequest::DirectPush(request)
    );
    assert!(!String::from_utf8(payload).unwrap().contains("bundle"));
}

#[test]
fn publisher_metadata_roundtrips_publish_pr_without_bundle_material() {
    let request = PublishPrRequest {
        repo_id: "repo".to_string(),
        base: None,
        title: "title".to_string(),
        body: String::new(),
        draft: false,
    };
    let payload = encode_publisher_metadata(&PublisherRequest::PublishPr(request.clone()))
        .expect("encode publish metadata");

    assert_eq!(
        decode_publisher_metadata(&payload).expect("decode publish"),
        PublisherRequest::PublishPr(request)
    );
    assert!(!String::from_utf8(payload).unwrap().contains("bundle"));
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
fn publisher_defaults_and_framing_are_sized_for_raw_128_mib_bundles() {
    assert_eq!(DEFAULT_PUBLISHER_MAX_BUNDLE_BYTES, 128 * 1024 * 1024);
    assert_eq!(MAX_PUBLISHER_METADATA_BYTES, 64 * 1024);
    assert_eq!(PUBLISHER_STREAM_BUFFER_BYTES, 64 * 1024);

    let header = encode_publisher_wire_header(128, DEFAULT_PUBLISHER_MAX_BUNDLE_BYTES as u64)
        .expect("encode header");
    assert_eq!(header.len(), PUBLISHER_WIRE_HEADER_BYTES);
    let decoded = decode_publisher_wire_header(&header).expect("decode header");
    assert_eq!(decoded.metadata_len, 128);
    assert_eq!(decoded.bundle_len, 128 * 1024 * 1024);
}

#[test]
fn publisher_wire_header_rejects_oversized_metadata_and_unknown_version() {
    let err = encode_publisher_wire_header(MAX_PUBLISHER_METADATA_BYTES + 1, 0)
        .expect_err("oversize metadata must fail before allocation");
    assert!(err.to_string().contains("metadata exceeds"));

    let mut header = encode_publisher_wire_header(16, 0).expect("encode header");
    header[8..10].copy_from_slice(&(PUBLISHER_WIRE_VERSION + 1).to_be_bytes());
    let err = decode_publisher_wire_header(&header).expect_err("unknown version must fail");
    assert!(
        err.to_string()
            .contains("unsupported publisher wire version")
    );
}

#[tokio::test]
async fn publisher_sender_streams_raw_bundle_at_exact_128_mib_limit() {
    let tempdir = tempdir().expect("tempdir");
    let socket_path = tempdir.path().join("publisher.sock");
    let bundle_path = tempdir.path().join("large.bundle");
    let bundle_file = std::fs::File::create(&bundle_path).expect("create sparse bundle");
    bundle_file
        .set_len(DEFAULT_PUBLISHER_MAX_BUNDLE_BYTES as u64)
        .expect("size sparse bundle");
    drop(bundle_file);

    let listener = UnixListener::bind(&socket_path).expect("bind fake publisher");
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("accept publisher client");
        let mut header = [0u8; PUBLISHER_WIRE_HEADER_BYTES];
        stream
            .read_exact(&mut header)
            .await
            .expect("read frame header");
        let header = decode_publisher_wire_header(&header).expect("decode frame header");
        assert_eq!(header.bundle_len, DEFAULT_PUBLISHER_MAX_BUNDLE_BYTES as u64);
        assert!(header.metadata_len <= MAX_PUBLISHER_METADATA_BYTES);

        let mut metadata = vec![0u8; header.metadata_len];
        stream
            .read_exact(&mut metadata)
            .await
            .expect("read metadata");
        let request = decode_publisher_metadata(&metadata).expect("decode metadata");
        assert!(matches!(request, PublisherRequest::PublishPr(_)));
        let metadata_json: serde_json::Value =
            serde_json::from_slice(&metadata).expect("metadata json");
        assert!(metadata_json.get("bundle").is_none());
        assert!(metadata_json.get("bundle_base64").is_none());

        stream.write_all(&[1]).await.expect("metadata ack");
        let mut remaining = header.bundle_len;
        let mut buffer = vec![0u8; PUBLISHER_STREAM_BUFFER_BYTES];
        let mut largest_read = 0usize;
        while remaining != 0 {
            let limit = remaining.min(buffer.len() as u64) as usize;
            let read = stream
                .read(&mut buffer[..limit])
                .await
                .expect("read streamed bundle");
            assert_ne!(read, 0, "bundle ended early");
            largest_read = largest_read.max(read);
            remaining -= read as u64;
        }
        let mut extra = [0u8; 1];
        assert_eq!(
            stream.read(&mut extra).await.expect("read sender eof"),
            0,
            "sender must not stream bytes beyond the declared bundle"
        );
        assert!(largest_read <= PUBLISHER_STREAM_BUFFER_BYTES);

        let response = serde_json::json!({
            "kind": "publish_pr",
            "pr_url": "https://github.com/owner/repo/pull/1",
            "branch": "agent/test",
            "pull_number": 1
        });
        stream
            .write_all(&serde_json::to_vec(&response).unwrap())
            .await
            .expect("write fake response");
        stream.shutdown().await.expect("shutdown fake response");
    });

    let response = submit_publish_request(
        &socket_path,
        DEFAULT_PUBLISHER_MAX_BUNDLE_BYTES,
        &PublishPrRequest {
            repo_id: "owner/repo".to_string(),
            base: Some("main".to_string()),
            title: "Large bundle".to_string(),
            body: String::new(),
            draft: false,
        },
        &bundle_path,
    )
    .await
    .expect("exact-limit raw bundle should stream successfully");
    assert_eq!(response.pull_number, 1);
    server.await.expect("fake publisher task");
}

#[tokio::test]
async fn publisher_sender_rejects_bundle_above_limit_before_connecting() {
    let tempdir = tempdir().expect("tempdir");
    let bundle_path = tempdir.path().join("too-large.bundle");
    let bundle_file = std::fs::File::create(&bundle_path).expect("create sparse bundle");
    bundle_file
        .set_len(DEFAULT_PUBLISHER_MAX_BUNDLE_BYTES as u64 + 1)
        .expect("size sparse bundle");
    drop(bundle_file);

    let err = submit_publish_request(
        Path::new("/tmp/does-not-need-to-exist-zodex-publisher.sock"),
        DEFAULT_PUBLISHER_MAX_BUNDLE_BYTES,
        &publish_pr_request("owner/repo"),
        &bundle_path,
    )
    .await
    .expect_err("sender must reject over-limit bundle before socket connect");
    assert!(err.to_string().contains("bundle exceeds configured limit"));
    assert!(!err.to_string().contains("connect"));
}

#[tokio::test]
async fn direct_push_sender_uses_same_raw_bundle_limit_before_connecting() {
    let tempdir = tempdir().expect("tempdir");
    let bundle_path = tempdir.path().join("too-large-direct.bundle");
    let bundle_file = std::fs::File::create(&bundle_path).expect("create sparse direct bundle");
    bundle_file
        .set_len(DEFAULT_PUBLISHER_MAX_BUNDLE_BYTES as u64 + 1)
        .expect("size sparse direct bundle");
    drop(bundle_file);

    let err = submit_direct_push_request(
        Path::new("/tmp/does-not-need-to-exist-zodex-publisher.sock"),
        DEFAULT_PUBLISHER_MAX_BUNDLE_BYTES,
        &DirectPushRequest {
            repo: "owner/repo".to_string(),
            src: "refs/heads/main".to_string(),
            dst: "refs/heads/main".to_string(),
            force: false,
            src_oid: Some("0123456789abcdef0123456789abcdef01234567".to_string()),
            src_object_type: Some("commit".to_string()),
        },
        Some(&bundle_path),
    )
    .await
    .expect_err("direct push sender must reject over-limit bundle before socket connect");
    assert!(err.to_string().contains("bundle exceeds configured limit"));
    assert!(!err.to_string().contains("connect"));
}

#[tokio::test]
async fn publisher_receiver_rejects_bytes_beyond_declared_bundle_length() {
    let (mut sender, mut receiver) = UnixStream::pair().expect("unix stream pair");
    let writer = tokio::spawn(async move {
        sender
            .write_all(b"abcd")
            .await
            .expect("write overflow body");
        sender.shutdown().await.expect("shutdown writer");
    });

    let err = receive_declared_bundle(&mut receiver, 3)
        .await
        .expect_err("receiver must reject one byte beyond declared length");
    assert!(err.to_string().contains("beyond declared bundle length"));
    writer.await.expect("writer task");
}

#[tokio::test]
async fn publisher_receiver_rejects_early_eof_with_fixed_stream_buffer() {
    let (mut sender, mut receiver) = UnixStream::pair().expect("unix stream pair");
    let writer = tokio::spawn(async move {
        sender.write_all(b"abc").await.expect("write short body");
        sender.shutdown().await.expect("shutdown writer");
    });

    let err = receive_declared_bundle(&mut receiver, 10 * 1024 * 1024)
        .await
        .expect_err("early EOF must fail");
    assert!(err.to_string().contains("bundle ended early"));
    assert_eq!(PUBLISHER_STREAM_BUFFER_BYTES, 64 * 1024);
    writer.await.expect("writer task");
}

#[test]
fn publisher_prebody_validation_accepts_exact_limit_and_rejects_one_byte_over() {
    let cfg = Config {
        publisher_targets: vec![PublishTarget {
            id: "owner/repo".to_string(),
            repo: "owner/repo".to_string(),
            default_base: "main".to_string(),
            installation_id: 1,
        }],
        ..Config::default()
    };
    let request = PublisherRequest::PublishPr(publish_pr_request("owner/repo"));

    validate_publisher_request_before_body(
        &cfg,
        request.clone(),
        DEFAULT_PUBLISHER_MAX_BUNDLE_BYTES as u64,
    )
    .expect("publisher should accept an exact-limit declared bundle after metadata auth");

    let err = validate_publisher_request_before_body(
        &cfg,
        request,
        DEFAULT_PUBLISHER_MAX_BUNDLE_BYTES as u64 + 1,
    )
    .expect_err("publisher must reject declared bundle above configured limit");
    assert!(err.to_string().contains("bundle exceeds configured limit"));
}

#[test]
fn publisher_prebody_validation_rejects_unknown_repo_before_bundle_read() {
    let cfg = Config {
        publisher_targets: vec![PublishTarget {
            id: "owner/allowed".to_string(),
            repo: "owner/allowed".to_string(),
            default_base: "main".to_string(),
            installation_id: 1,
        }],
        ..Config::default()
    };
    let err = validate_publisher_request_before_body(
        &cfg,
        PublisherRequest::PublishPr(publish_pr_request("owner/not-allowed")),
        DEFAULT_PUBLISHER_MAX_BUNDLE_BYTES as u64,
    )
    .expect_err("repo target authorization must run before receiving bundle bytes");
    assert!(
        err.to_string()
            .contains("not covered by publisher installation")
    );
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
    assert!(!bundle.is_empty());

    let output = std::process::Command::new("git")
        .args(["bundle", "list-heads", bundle.path().to_str().unwrap()])
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
