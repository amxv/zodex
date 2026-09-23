use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Result, bail};
use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::routing::get;
use serde_json::json;
use tempfile::TempDir;

use super::{
    ArchiveExtractor, LocalConfig, LocalPaths, LocalSetupRequest, LocalSetupService,
    LocalStatusDocument, OfficialTunnelReleaseClient, RuntimeKey, RuntimeKeyStore,
    TunnelArchitecture, sha256_hex,
};

const TUNNEL_ID: &str = "tunnel_0123456789abcdef0123456789abcdef";

#[tokio::test]
async fn setup_installs_verified_release_without_serializing_secret() {
    let fixture = SetupFixture::new().await;
    let server = FakeReleaseServer::start("v9.9.9", b"release-one".to_vec(), None).await;
    let key = RuntimeKey::new("fixture-runtime-key-one").unwrap();

    let result = fixture.run(&server, key.clone()).await.unwrap();

    assert!(result.binary_updated);
    assert!(result.observability_bearer_rotated);
    assert_eq!(result.release_version, "v9.9.9");
    assert_eq!(server.asset_hits(), 1);
    assert_eq!(fixture.secrets.current().unwrap(), key);
    assert_eq!(
        fs::read(fixture.paths.managed_tunnel_client()).unwrap(),
        FixtureExtractor::binary_for(b"release-one")
    );
    assert_eq!(
        fs::read(fixture.paths.managed_cloudflared()).unwrap(),
        FixtureExtractor::cloudflared_for(b"release-one")
    );
    assert_eq!(
        fs::read(fixture.paths.managed_cloudflared_manifest()).unwrap(),
        FixtureExtractor::manifest_for(b"release-one")
    );

    let config = LocalConfig::load(&fixture.paths.config_file()).unwrap();
    assert!(config.is_provider_configured());
    assert_eq!(config.tunnel.id.as_deref(), Some(TUNNEL_ID));
    assert_eq!(config.tunnel.release.as_ref().unwrap().version, "v9.9.9");
    assert_eq!(
        config.tunnel.release.as_ref().unwrap().archive_sha256,
        sha256_hex(b"release-one")
    );

    let config_raw = fs::read_to_string(fixture.paths.config_file()).unwrap();
    let status_raw =
        serde_json::to_string(&LocalStatusDocument::inspect(&fixture.paths).unwrap()).unwrap();
    for rendered in [config_raw.as_str(), status_raw.as_str()] {
        assert!(!rendered.contains(key.expose()));
        assert!(!rendered.contains("runtime_key"));
        assert!(!rendered.contains("api_key"));
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            fs::metadata(fixture.paths.observability_bearer_file())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}

#[tokio::test]
async fn setup_same_release_reuses_verified_binary_and_bearer() {
    let fixture = SetupFixture::new().await;
    let server = FakeReleaseServer::start("v9.9.9", b"same-release".to_vec(), None).await;
    let key = RuntimeKey::new("same-runtime-key").unwrap();

    let first = fixture.run(&server, key.clone()).await.unwrap();
    assert!(first.binary_updated);
    let binary_before = fs::read(fixture.paths.managed_tunnel_client()).unwrap();
    let cloudflared_before = fs::read(fixture.paths.managed_cloudflared()).unwrap();
    let manifest_before = fs::read(fixture.paths.managed_cloudflared_manifest()).unwrap();
    let bearer_before = fs::read(fixture.paths.observability_bearer_file()).unwrap();

    let second = fixture.run(&server, key).await.unwrap();
    assert!(!second.binary_updated);
    assert!(!second.observability_bearer_rotated);
    assert_eq!(
        server.asset_hits(),
        1,
        "same verified release should not redownload archive"
    );
    assert_eq!(server.latest_hits(), 2);
    assert_eq!(
        fs::read(fixture.paths.managed_tunnel_client()).unwrap(),
        binary_before
    );
    assert_eq!(
        fs::read(fixture.paths.managed_cloudflared()).unwrap(),
        cloudflared_before
    );
    assert_eq!(
        fs::read(fixture.paths.managed_cloudflared_manifest()).unwrap(),
        manifest_before
    );
    assert_eq!(
        fs::read(fixture.paths.observability_bearer_file()).unwrap(),
        bearer_before
    );
}

#[tokio::test]
async fn setup_same_release_repairs_missing_or_corrupt_bundle_companion() {
    let fixture = SetupFixture::new().await;
    let server = FakeReleaseServer::start("v9.9.9", b"repair-release".to_vec(), None).await;
    let key = RuntimeKey::new("repair-runtime-key").unwrap();

    fixture.run(&server, key.clone()).await.unwrap();
    fs::write(fixture.paths.managed_cloudflared(), b"tampered").unwrap();

    let repaired = fixture.run(&server, key).await.unwrap();

    assert!(repaired.binary_updated);
    assert_eq!(server.asset_hits(), 2);
    assert_eq!(
        fs::read(fixture.paths.managed_cloudflared()).unwrap(),
        FixtureExtractor::cloudflared_for(b"repair-release")
    );
}

#[tokio::test]
async fn setup_good_upgrade_atomically_replaces_managed_binary_and_metadata() {
    let fixture = SetupFixture::new().await;
    let first = FakeReleaseServer::start("v9.9.9", b"old-release".to_vec(), None).await;
    fixture
        .run(&first, RuntimeKey::new("old-key").unwrap())
        .await
        .unwrap();

    let second = FakeReleaseServer::start("v9.9.10", b"new-release".to_vec(), None).await;
    let result = fixture
        .run(&second, RuntimeKey::new("new-key").unwrap())
        .await
        .unwrap();

    assert!(result.binary_updated);
    assert_eq!(
        fs::read(fixture.paths.managed_tunnel_client()).unwrap(),
        FixtureExtractor::binary_for(b"new-release")
    );
    let config = LocalConfig::load(&fixture.paths.config_file()).unwrap();
    assert_eq!(config.tunnel.release.as_ref().unwrap().version, "v9.9.10");
    assert_eq!(fixture.secrets.current().unwrap().expose(), "new-key");
}

#[tokio::test]
async fn checksum_failure_preserves_prior_binary_config_secret_and_bearer() {
    let fixture = SetupFixture::new().await;
    fixture.seed_healthy().await;
    let before = fixture.snapshot();

    let bad_checksum = "0".repeat(64);
    let server =
        FakeReleaseServer::start("v9.9.10", b"corrupt-upgrade".to_vec(), Some(bad_checksum)).await;
    let error = fixture
        .run(&server, RuntimeKey::new("new-secret").unwrap())
        .await
        .unwrap_err();

    assert!(format!("{error:#}").contains("checksum mismatch"));
    fixture.assert_snapshot(&before);
}

#[tokio::test]
async fn key_store_failure_restores_prior_secret_and_files() {
    let fixture = SetupFixture::new().await;
    fixture.seed_healthy().await;
    let before = fixture.snapshot();
    fixture.secrets.fail_next_set_after_write();

    let server = FakeReleaseServer::start("v9.9.10", b"key-store-fail".to_vec(), None).await;
    let error = fixture
        .run(
            &server,
            RuntimeKey::new("partially-written-secret").unwrap(),
        )
        .await
        .unwrap_err();

    assert!(format!("{error:#}").contains("prior secure credential state was restored"));
    fixture.assert_snapshot(&before);
}

#[tokio::test]
async fn active_runtime_marker_blocks_setup_before_release_or_secret_work() {
    let fixture = SetupFixture::new().await;
    fs::create_dir_all(fixture.paths.runtime_dir()).unwrap();
    fs::write(
        fixture.paths.runtime_state_file(),
        br#"{"schema_version":1,"runtime_id":"fixture","lifecycle":"ready"}"#,
    )
    .unwrap();
    let server = FakeReleaseServer::start("v9.9.9", b"unused".to_vec(), None).await;

    let error = fixture
        .run(&server, RuntimeKey::new("unused-secret").unwrap())
        .await
        .unwrap_err();

    assert!(format!("{error:#}").contains("zodex local stop"));
    assert_eq!(server.latest_hits(), 0);
    assert!(fixture.secrets.current().is_none());
    assert!(!fixture.paths.managed_tunnel_client().exists());
}

struct SetupFixture {
    _root: TempDir,
    paths: LocalPaths,
    extractor: FixtureExtractor,
    secrets: MemorySecretStore,
}

impl SetupFixture {
    async fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let paths = LocalPaths::from_roots(
            root.path().join("config"),
            root.path().join("data"),
            root.path().join("state"),
        )
        .unwrap();
        Self {
            _root: root,
            paths,
            extractor: FixtureExtractor,
            secrets: MemorySecretStore::default(),
        }
    }

    async fn run(
        &self,
        server: &FakeReleaseServer,
        runtime_key: RuntimeKey,
    ) -> Result<super::LocalSetupResult> {
        let releases = OfficialTunnelReleaseClient::with_latest_release_url(server.latest_url())?;
        LocalSetupService::new(&self.paths, &releases, &self.extractor, &self.secrets)
            .run(LocalSetupRequest {
                tunnel_id: TUNNEL_ID.to_string(),
                runtime_key,
                architecture: TunnelArchitecture::DarwinArm64,
                rotate_observability_bearer: false,
            })
            .await
    }

    async fn seed_healthy(&self) {
        let server = FakeReleaseServer::start("v9.9.9", b"healthy-seed".to_vec(), None).await;
        self.run(&server, RuntimeKey::new("healthy-secret").unwrap())
            .await
            .unwrap();
    }

    fn snapshot(&self) -> HealthySnapshot {
        HealthySnapshot {
            config: fs::read(self.paths.config_file()).unwrap(),
            binary: fs::read(self.paths.managed_tunnel_client()).unwrap(),
            cloudflared: fs::read(self.paths.managed_cloudflared()).unwrap(),
            cloudflared_manifest: fs::read(self.paths.managed_cloudflared_manifest()).unwrap(),
            bearer: fs::read(self.paths.observability_bearer_file()).unwrap(),
            secret: self.secrets.current().unwrap(),
        }
    }

    fn assert_snapshot(&self, expected: &HealthySnapshot) {
        assert_eq!(fs::read(self.paths.config_file()).unwrap(), expected.config);
        assert_eq!(
            fs::read(self.paths.managed_tunnel_client()).unwrap(),
            expected.binary
        );
        assert_eq!(
            fs::read(self.paths.managed_cloudflared()).unwrap(),
            expected.cloudflared
        );
        assert_eq!(
            fs::read(self.paths.managed_cloudflared_manifest()).unwrap(),
            expected.cloudflared_manifest
        );
        assert_eq!(
            fs::read(self.paths.observability_bearer_file()).unwrap(),
            expected.bearer
        );
        assert_eq!(self.secrets.current().unwrap(), expected.secret);
    }
}

struct HealthySnapshot {
    config: Vec<u8>,
    binary: Vec<u8>,
    cloudflared: Vec<u8>,
    cloudflared_manifest: Vec<u8>,
    bearer: Vec<u8>,
    secret: RuntimeKey,
}

struct FixtureExtractor;

impl FixtureExtractor {
    fn binary_for(archive: &[u8]) -> Vec<u8> {
        let mut binary = b"fixture-tunnel-client:".to_vec();
        binary.extend_from_slice(archive);
        binary
    }

    fn cloudflared_for(archive: &[u8]) -> Vec<u8> {
        let mut binary = b"fixture-cloudflared:".to_vec();
        binary.extend_from_slice(archive);
        binary
    }

    fn manifest_for(archive: &[u8]) -> Vec<u8> {
        let mut manifest = b"fixture-cloudflared-manifest:".to_vec();
        manifest.extend_from_slice(archive);
        manifest
    }
}

impl ArchiveExtractor for FixtureExtractor {
    fn extract_tunnel_bundle(&self, archive_path: &Path, bundle_dir: &Path) -> Result<()> {
        let archive = fs::read(archive_path)?;
        fs::create_dir_all(bundle_dir)?;
        let binary_path = bundle_dir.join(format!("tunnel-client{}", std::env::consts::EXE_SUFFIX));
        let cloudflared_path =
            bundle_dir.join(format!("cloudflared{}", std::env::consts::EXE_SUFFIX));
        let manifest_path = bundle_dir.join("cloudflared-manifest.json");
        fs::write(&binary_path, Self::binary_for(&archive))?;
        fs::write(&cloudflared_path, Self::cloudflared_for(&archive))?;
        fs::write(&manifest_path, Self::manifest_for(&archive))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(&binary_path, fs::Permissions::from_mode(0o755))?;
            fs::set_permissions(&cloudflared_path, fs::Permissions::from_mode(0o755))?;
            fs::set_permissions(&manifest_path, fs::Permissions::from_mode(0o644))?;
        }
        Ok(())
    }
}

#[derive(Default)]
struct MemorySecretStore {
    value: Mutex<Option<RuntimeKey>>,
    fail_next_set_after_write: AtomicBool,
}

impl MemorySecretStore {
    fn current(&self) -> Option<RuntimeKey> {
        self.value.lock().unwrap().clone()
    }

    fn fail_next_set_after_write(&self) {
        self.fail_next_set_after_write.store(true, Ordering::SeqCst);
    }
}

impl RuntimeKeyStore for MemorySecretStore {
    fn get(&self) -> Result<Option<RuntimeKey>> {
        Ok(self.current())
    }

    fn set(&self, key: &RuntimeKey) -> Result<()> {
        *self.value.lock().unwrap() = Some(key.clone());
        if self.fail_next_set_after_write.swap(false, Ordering::SeqCst) {
            bail!("injected secret-store write failure");
        }
        Ok(())
    }

    fn delete(&self) -> Result<()> {
        *self.value.lock().unwrap() = None;
        Ok(())
    }
}

#[derive(Clone)]
struct FakeReleaseState {
    base_url: String,
    tag: String,
    archive: Arc<Vec<u8>>,
    checksum: String,
    latest_hits: Arc<AtomicUsize>,
    asset_hits: Arc<AtomicUsize>,
}

struct FakeReleaseServer {
    base_url: String,
    state: FakeReleaseState,
    task: tokio::task::JoinHandle<()>,
}

impl FakeReleaseServer {
    async fn start(tag: &str, archive: Vec<u8>, checksum_override: Option<String>) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let base_url = format!("http://{address}");
        let state = FakeReleaseState {
            base_url: base_url.clone(),
            tag: tag.to_string(),
            checksum: checksum_override.unwrap_or_else(|| sha256_hex(&archive)),
            archive: Arc::new(archive),
            latest_hits: Arc::new(AtomicUsize::new(0)),
            asset_hits: Arc::new(AtomicUsize::new(0)),
        };
        let app = Router::new()
            .route("/latest", get(fake_latest_release))
            .route("/checksums", get(fake_checksums))
            .route("/asset", get(fake_archive))
            .with_state(state.clone());
        let task = tokio::spawn(async move {
            axum::serve(listener, app.into_make_service())
                .await
                .unwrap();
        });
        Self {
            base_url,
            state,
            task,
        }
    }

    fn latest_url(&self) -> String {
        format!("{}/latest", self.base_url)
    }

    fn latest_hits(&self) -> usize {
        self.state.latest_hits.load(Ordering::SeqCst)
    }

    fn asset_hits(&self) -> usize {
        self.state.asset_hits.load(Ordering::SeqCst)
    }
}

impl Drop for FakeReleaseServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn fake_latest_release(
    State(state): State<FakeReleaseState>,
) -> axum::Json<serde_json::Value> {
    state.latest_hits.fetch_add(1, Ordering::SeqCst);
    let asset_name = format!("tunnel-client-{}-darwin-arm64.zip", state.tag);
    axum::Json(json!({
        "tag_name": state.tag,
        "draft": false,
        "prerelease": false,
        "assets": [
            {
                "name": asset_name,
                "browser_download_url": format!("{}/asset", state.base_url),
                "size": state.archive.len()
            },
            {
                "name": "SHA256SUMS.txt",
                "browser_download_url": format!("{}/checksums", state.base_url),
                "size": 256
            }
        ]
    }))
}

async fn fake_checksums(State(state): State<FakeReleaseState>) -> String {
    format!(
        "{}  tunnel-client-{}-darwin-arm64.zip\n",
        state.checksum, state.tag
    )
}

async fn fake_archive(State(state): State<FakeReleaseState>) -> Bytes {
    state.asset_hits.fetch_add(1, Ordering::SeqCst);
    Bytes::copy_from_slice(&state.archive)
}
