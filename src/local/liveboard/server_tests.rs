use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt as _;
use reqwest::StatusCode;
use serde_json::{Value, json};
use tempfile::tempdir;

use crate::invocation::{InvocationContext, InvocationEvidenceRecorder, InvocationStart};
use crate::local::observer_client::LocalObserverClient;
use crate::local::{
    LOCAL_DISCOVERY_SCHEMA_VERSION, LocalHistoryRuntime, LocalHistoryRuntimeConfig,
    LocalObservabilityDiscovery, LocalPaths, LocalRuntimeDiscovery,
    start_local_observability_server, write_runtime_discovery,
};

use super::assets;
use super::server::start_liveboard_host;

const BEARER: &str = "liveboard-host-observer-bearer-0123456789abcdef";

struct ObserverFixture {
    paths: LocalPaths,
    runtime_id: String,
    history: Arc<LocalHistoryRuntime>,
    server: crate::local::LocalObservabilityServer,
}

impl ObserverFixture {
    async fn start(root: &std::path::Path, runtime_id: &str) -> Self {
        crate::install_rustls_crypto_provider();
        let paths =
            LocalPaths::from_roots(root.join("config"), root.join("data"), root.join("state"))
                .unwrap();
        paths.ensure_persistent_dirs().unwrap();
        std::fs::write(paths.observability_bearer_file(), format!("{BEARER}\n")).unwrap();
        let history = LocalHistoryRuntime::open(LocalHistoryRuntimeConfig::new(
            paths.history_database(),
            runtime_id,
            60 * 60,
            64 * 1024 * 1024,
        ))
        .unwrap();
        let server = start_local_observability_server(history.clone(), BEARER)
            .await
            .unwrap();
        write_runtime_discovery(
            &paths,
            &LocalRuntimeDiscovery {
                schema_version: LOCAL_DISCOVERY_SCHEMA_VERSION,
                runtime_id: runtime_id.to_string(),
                pid: std::process::id(),
                start_directory: root.to_path_buf(),
                started_at: "2026-08-17T00:00:00Z".to_string(),
                expires_at: None,
                observability: LocalObservabilityDiscovery::active(
                    server.base_url(),
                    paths.observability_bearer_file(),
                ),
            },
        )
        .unwrap();
        Self {
            paths,
            runtime_id: runtime_id.to_string(),
            history,
            server,
        }
    }

    fn client(&self) -> LocalObserverClient {
        LocalObserverClient::attach(&self.server.base_url(), BEARER, &self.runtime_id).unwrap()
    }

    async fn shutdown(self) {
        self.server.shutdown().await.unwrap();
        self.history.shutdown_blocking().unwrap();
    }
}

fn capability_url(host_url: &str, suffix: &str) -> String {
    format!("{}{}", host_url, suffix)
}

#[tokio::test]
async fn host_serves_embedded_assets_and_only_allowlisted_same_origin_resources() {
    if assets::ensure_available().is_err() {
        return;
    }
    let dir = tempdir().unwrap();
    let observer = ObserverFixture::start(dir.path(), "runtime-liveboard-host").await;
    let host = start_liveboard_host(&observer.paths, observer.client())
        .await
        .unwrap();
    let client = reqwest::Client::new();

    let root = client.get(host.url()).send().await.unwrap();
    assert_eq!(root.status(), StatusCode::OK);
    assert_eq!(root.headers()["cache-control"], "no-store");
    assert_eq!(root.headers()["x-content-type-options"], "nosniff");
    assert_eq!(root.headers()["x-frame-options"], "DENY");
    assert_eq!(root.headers()["referrer-policy"], "no-referrer");
    assert!(
        root.headers()["content-security-policy"]
            .to_str()
            .unwrap()
            .contains("connect-src 'self'")
    );
    let csp = root.headers()["content-security-policy"].to_str().unwrap();
    assert!(csp.contains("script-src 'self'"));
    assert!(!csp.contains("script-src 'self' 'unsafe-inline'"));
    assert!(csp.contains("worker-src 'self'"));
    assert!(csp.contains("style-src 'self' 'unsafe-inline'"));
    assert!(root.headers().get("access-control-allow-origin").is_none());
    let html = root.text().await.unwrap();
    assert!(html.contains("Zodex Liveboard"));
    assert!(html.contains("./assets/"));
    assert!(!html.contains(BEARER));
    let public_url = reqwest::Url::parse(host.url()).unwrap();
    assert_eq!(public_url.path(), "/");
    let private_url = reqwest::Url::parse(host.private_url()).unwrap();
    assert_ne!(private_url.path(), public_url.path());
    assert!(html.contains(&format!("<base href=\"{}\"", private_url.path())));

    let public_api = client
        .get(format!("{}api/status", host.url()))
        .send()
        .await
        .unwrap();
    assert_eq!(public_api.status(), StatusCode::NOT_FOUND);

    let fingerprinted_asset = assets::all()
        .iter()
        .find(|asset| asset.path.starts_with("assets/") && assets::immutable(asset.path))
        .expect("production Liveboard build should contain a fingerprinted asset");
    let asset = client
        .get(capability_url(host.private_url(), fingerprinted_asset.path))
        .send()
        .await
        .unwrap();
    assert_eq!(asset.status(), StatusCode::OK);
    assert_eq!(
        asset.headers()["cache-control"],
        "public, max-age=31536000, immutable"
    );
    assert!(asset.headers().get("content-type").is_some());

    let status = client
        .get(capability_url(host.private_url(), "api/status"))
        .send()
        .await
        .unwrap();
    assert_eq!(status.status(), StatusCode::OK);
    let status: Value = status.json().await.unwrap();
    assert_eq!(status["runtime_id"], "runtime-liveboard-host");
    assert!(!status.to_string().contains(BEARER));

    let invocation = observer
        .history
        .begin(
            InvocationContext::default(),
            InvocationStart::new("host-output-metadata-proof", json!({})),
        )
        .unwrap();
    let invocation_id = invocation.invocation_id.unwrap();
    let output_metadata = client
        .get(capability_url(
            host.private_url(),
            &format!("api/invocations/{invocation_id}/output-metadata"),
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(output_metadata.status(), StatusCode::OK);
    assert_eq!(output_metadata.headers()["cache-control"], "no-store");
    let output_metadata: Value = output_metadata.json().await.unwrap();
    assert_eq!(output_metadata["invocation_id"], invocation_id);
    assert!(output_metadata.get("invocation").is_none());
    assert!(!output_metadata.to_string().contains(BEARER));

    let prefs = client
        .get(capability_url(host.private_url(), "preferences"))
        .send()
        .await
        .unwrap();
    assert_eq!(prefs.status(), StatusCode::OK);
    let prefs: Value = prefs.json().await.unwrap();
    assert_eq!(prefs["theme"], "system");
    assert_eq!(prefs["max_visible_agents"], 4);
    assert_eq!(prefs["command_outputs_expanded"], false);
    assert_eq!(prefs["diffs_expanded"], true);
    assert_eq!(prefs["show_raw_button"], false);
    assert_eq!(prefs["editor_command"], "zed");

    let patched = client
        .patch(capability_url(host.private_url(), "preferences"))
        .json(&json!({"theme":"dark","max_visible_agents":5,"show_raw_button":true,"editor_command":"/usr/bin/true"}))
        .send()
        .await
        .unwrap();
    assert_eq!(patched.status(), StatusCode::OK);
    let patched: Value = patched.json().await.unwrap();
    assert_eq!(patched["theme"], "dark");
    assert_eq!(patched["max_visible_agents"], 5);
    assert_eq!(patched["show_raw_button"], true);
    assert_eq!(patched["editor_command"], "/usr/bin/true");
    assert!(!patched.to_string().contains(BEARER));

    let open_path = dir.path().join("open-me.txt");
    std::fs::write(&open_path, "open me").unwrap();
    let opened = client
        .post(capability_url(host.private_url(), "api/open-file"))
        .json(&json!({"path": open_path}))
        .send()
        .await
        .unwrap();
    assert_eq!(opened.status(), StatusCode::NO_CONTENT);
    let missing = client
        .post(capability_url(host.private_url(), "api/open-file"))
        .json(&json!({"path": dir.path().join("missing.txt")}))
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);

    let unknown_post = client
        .post(capability_url(host.private_url(), "api/status"))
        .send()
        .await
        .unwrap();
    assert_eq!(unknown_post.status(), StatusCode::METHOD_NOT_ALLOWED);
    let execution = client
        .get(capability_url(host.private_url(), "api/exec-command"))
        .send()
        .await
        .unwrap();
    assert_eq!(execution.status(), StatusCode::NOT_FOUND);
    let traversal = client
        .get(capability_url(
            host.private_url(),
            "assets/%2e%2e/index.html",
        ))
        .send()
        .await
        .unwrap();
    assert!(matches!(
        traversal.status(),
        StatusCode::NOT_FOUND | StatusCode::BAD_REQUEST
    ));

    let cross_origin = client
        .patch(capability_url(host.private_url(), "preferences"))
        .header("origin", "http://evil.invalid")
        .json(&json!({"theme":"light"}))
        .send()
        .await
        .unwrap();
    assert_eq!(cross_origin.status(), StatusCode::FORBIDDEN);
    let wrong_host = client
        .get(host.url())
        .header("host", "127.0.0.1:1")
        .send()
        .await
        .unwrap();
    assert_eq!(wrong_host.status(), StatusCode::MISDIRECTED_REQUEST);
    let mut wrong_capability = reqwest::Url::parse(host.url()).unwrap();
    wrong_capability.set_path("/not-the-liveboard-capability/");
    let wrong_capability = client.get(wrong_capability).send().await.unwrap();
    assert_eq!(wrong_capability.status(), StatusCode::NOT_FOUND);

    let focused_root = client
        .get(format!("{}?agent=k7m2", host.url()))
        .send()
        .await
        .unwrap();
    assert_eq!(focused_root.status(), StatusCode::OK);
    assert_eq!(focused_root.headers()["referrer-policy"], "no-referrer");
    assert!(
        focused_root
            .headers()
            .get("access-control-allow-origin")
            .is_none()
    );

    let capability = reqwest::Url::parse(host.private_url())
        .unwrap()
        .path_segments()
        .unwrap()
        .next()
        .unwrap()
        .to_string();
    assert!(capability.len() >= 32);
    assert!(!capability.contains(BEARER));
    assert!(
        assets::all()
            .iter()
            .all(|asset| { !String::from_utf8_lossy(asset.bytes).contains(BEARER) })
    );

    host.shutdown().await.unwrap();
    observer.shutdown().await;
}

#[tokio::test]
async fn host_streams_sse_without_buffering_and_stays_bound_to_its_runtime() {
    if assets::ensure_available().is_err() {
        return;
    }
    let dir = tempdir().unwrap();
    let observer = ObserverFixture::start(dir.path(), "runtime-one").await;
    let paths = observer.paths.clone();
    let host = start_liveboard_host(&paths, observer.client())
        .await
        .unwrap();
    let client = reqwest::Client::new();

    let response = client
        .get(capability_url(
            host.private_url(),
            "api/events?include_output=false",
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        response.headers()["content-type"]
            .to_str()
            .unwrap()
            .starts_with("text/event-stream")
    );
    let mut stream = response.bytes_stream();
    let invocation = observer
        .history
        .begin(
            InvocationContext::default(),
            InvocationStart::new("host-stream-proof", json!({})),
        )
        .unwrap();
    let first = tokio::time::timeout(Duration::from_secs(2), stream.next())
        .await
        .expect("same-origin SSE did not stream a live frame")
        .expect("same-origin SSE ended unexpectedly")
        .expect("same-origin SSE frame failed");
    let first = String::from_utf8_lossy(&first);
    assert!(first.contains("host-stream-proof"), "{first}");
    assert!(!first.contains(BEARER));
    drop(stream);

    observer.server.shutdown().await.unwrap();
    observer.history.shutdown_blocking().unwrap();
    let replacement = ObserverFixture::start(dir.path(), "runtime-two").await;
    let status = client
        .get(capability_url(host.private_url(), "api/status"))
        .send()
        .await
        .unwrap();
    assert_eq!(status.status(), StatusCode::SERVICE_UNAVAILABLE);
    let status = status.text().await.unwrap();
    assert!(!status.contains("runtime-two"));

    host.shutdown().await.unwrap();
    replacement.shutdown().await;
    let _ = invocation;
}

#[tokio::test]
async fn host_cancellation_stops_only_the_runtime_sidecar() {
    if assets::ensure_available().is_err() {
        return;
    }
    let dir = tempdir().unwrap();
    let observer = ObserverFixture::start(dir.path(), "runtime-cancel-host").await;
    let host = start_liveboard_host(&observer.paths, observer.client())
        .await
        .unwrap();
    host.request_shutdown();
    tokio::time::timeout(Duration::from_secs(2), host.shutdown())
        .await
        .expect("Liveboard foreground host did not honor cancellation")
        .unwrap();

    let invocation = observer
        .history
        .begin(
            InvocationContext::default(),
            InvocationStart::new("observer-still-running", json!({})),
        )
        .unwrap();
    assert!(invocation.invocation_id.is_some());
    observer.shutdown().await;
}
