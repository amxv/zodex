use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use axum::body::{Body, Bytes};
use axum::extract::{DefaultBodyLimit, Path as AxumPath, RawQuery, Request, State};
use axum::http::{HeaderName, HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use futures_util::TryStreamExt as _;
use serde::{Deserialize, Serialize};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::warn;

use super::super::LocalPaths;
use super::super::observer_client::LocalObserverClient;
use super::assets;
use super::bridge::LiveboardObserverBridge;
use super::prefs::{LiveboardPreferencesPatch, LiveboardPreferencesStore};

const PREFERENCE_BODY_LIMIT: usize = 64 * 1024;
const LIVEBOARD_SHUTDOWN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);
pub(crate) const LOCAL_LIVEBOARD_PORT: u16 = 64_973;
const CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'self'; worker-src 'self'; object-src 'none'; base-uri 'self'; frame-ancestors 'none'; form-action 'none'";

#[derive(Clone)]
struct LiveboardState {
    observer: Arc<LiveboardObserverBridge>,
    preferences: LiveboardPreferencesStore,
    private_base_path: String,
}

#[derive(Clone)]
struct SecurityState {
    expected_host: HeaderValue,
    expected_origin: HeaderValue,
}

pub(crate) struct LocalLiveboardHost {
    url: String,
    #[cfg(test)]
    private_url: String,
    cancellation: CancellationToken,
    task: JoinHandle<Result<()>>,
}

impl LocalLiveboardHost {
    pub(crate) fn url(&self) -> &str {
        &self.url
    }

    #[cfg(test)]
    pub(crate) fn private_url(&self) -> &str {
        &self.private_url
    }

    pub(crate) fn is_finished(&self) -> bool {
        self.task.is_finished()
    }

    pub(crate) fn request_shutdown(&self) {
        self.cancellation.cancel();
    }

    pub(crate) async fn shutdown(self) -> Result<()> {
        self.request_shutdown();
        let mut task = self.task;
        match tokio::time::timeout(LIVEBOARD_SHUTDOWN_TIMEOUT, &mut task).await {
            Ok(joined) => {
                joined.context("Liveboard host task failed to join")??;
                Ok(())
            }
            Err(_) => {
                task.abort();
                let _ = task.await;
                bail!(
                    "Liveboard host did not stop within the bounded {}s shutdown deadline",
                    LIVEBOARD_SHUTDOWN_TIMEOUT.as_secs()
                )
            }
        }
    }
}

pub(crate) async fn start_liveboard_host(
    paths: &LocalPaths,
    observer_client: LocalObserverClient,
) -> Result<LocalLiveboardHost> {
    assets::ensure_available()?;
    let observer = Arc::new(LiveboardObserverBridge::runtime_bound(observer_client));
    let preferences = LiveboardPreferencesStore::new(paths);
    preferences.load()?;

    let listener = tokio::net::TcpListener::bind(SocketAddr::new(
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        liveboard_bind_port(),
    ))
    .await
    .with_context(|| {
        format!("failed to bind Liveboard loopback listener on 127.0.0.1:{LOCAL_LIVEBOARD_PORT}")
    })?;
    let addr = listener
        .local_addr()
        .context("failed to inspect Liveboard loopback listener")?;
    if !addr.ip().is_loopback() {
        bail!("Liveboard listener bound outside loopback: {addr}");
    }

    let capability = URL_SAFE_NO_PAD.encode(rand::random::<[u8; 24]>());
    let private_base_path = format!("/{capability}/");
    let expected_host = HeaderValue::from_str(&addr.to_string())
        .context("Liveboard listener address was not a valid Host header")?;
    let origin = format!("http://{addr}");
    let expected_origin = HeaderValue::from_str(&origin)
        .context("Liveboard listener address was not a valid Origin header")?;
    let app = build_router(
        &capability,
        Arc::new(LiveboardState {
            observer,
            preferences,
            private_base_path: private_base_path.clone(),
        }),
        SecurityState {
            expected_host,
            expected_origin,
        },
    );
    let cancellation = CancellationToken::new();
    let shutdown = cancellation.clone();
    let task = tokio::spawn(async move {
        axum::serve(listener, app.into_make_service())
            .with_graceful_shutdown(shutdown.cancelled_owned())
            .await
            .context("Liveboard host terminated unexpectedly")
    });
    let url = format!("{origin}/");
    Ok(LocalLiveboardHost {
        url,
        #[cfg(test)]
        private_url: format!("{origin}{private_base_path}"),
        cancellation,
        task,
    })
}

const fn liveboard_bind_port() -> u16 {
    if cfg!(test) { 0 } else { LOCAL_LIVEBOARD_PORT }
}

fn build_router(capability: &str, state: Arc<LiveboardState>, security: SecurityState) -> Router {
    let prefix = format!("/{capability}");
    let scoped = Router::new()
        .route("/assets/{*path}", get(asset))
        .route("/preferences", get(preferences).patch(patch_preferences))
        .route("/api/status", get(proxy_status))
        .route("/api/agents", get(proxy_agents))
        .route("/api/agents/{id}", get(proxy_agent))
        .route("/api/timeline", get(proxy_timeline))
        .route("/api/timeline/diffs", get(proxy_timeline_diffs))
        .route(
            "/api/timeline/{presentation_id}",
            get(proxy_timeline_detail),
        )
        .route(
            "/api/timeline/{presentation_id}/checkpoints",
            get(proxy_timeline_checkpoints),
        )
        .route("/api/invocations/{id}", get(proxy_invocation))
        .route(
            "/api/invocations/{id}/output-metadata",
            get(proxy_output_metadata),
        )
        .route("/api/invocations/{id}/output", get(proxy_output))
        .route("/api/events", get(proxy_events))
        .route("/api/open-file", post(open_file))
        .layer(DefaultBodyLimit::max(PREFERENCE_BODY_LIMIT));
    Router::new()
        .route("/", get(index))
        .route(&prefix, get(index))
        .route(&format!("{prefix}/"), get(index))
        .nest(&prefix, scoped)
        .layer(middleware::from_fn_with_state(security, security_boundary))
        .with_state(state)
}

async fn index(State(state): State<Arc<LiveboardState>>) -> Response {
    serve_index(&state.private_base_path)
}

fn serve_index(private_base_path: &str) -> Response {
    let Some(asset) = assets::find("index.html") else {
        return error_response(StatusCode::NOT_FOUND, "asset was not found");
    };
    let html = String::from_utf8_lossy(asset.bytes);
    let base = format!("<base href=\"{private_base_path}\" />");
    let html = html.replacen("<head>", &format!("<head>\n    {base}"), 1);
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .header(header::CACHE_CONTROL, "no-store")
        .body(Body::from(html.into_owned()))
        .expect("Liveboard index response must be valid")
}

async fn asset(AxumPath(path): AxumPath<String>) -> Response {
    if path.is_empty() || path.contains("..") || path.contains('\\') {
        return error_response(StatusCode::NOT_FOUND, "asset was not found");
    }
    serve_asset(&format!("assets/{path}"))
}

fn serve_asset(path: &str) -> Response {
    let Some(asset) = assets::find(path) else {
        return error_response(StatusCode::NOT_FOUND, "asset was not found");
    };
    let cache_control = if assets::immutable(path) {
        "public, max-age=31536000, immutable"
    } else {
        "no-store"
    };
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, assets::content_type(path))
        .header(header::CACHE_CONTROL, cache_control)
        .body(Body::from(Bytes::from_static(asset.bytes)))
        .expect("static Liveboard asset response must be valid")
}

async fn preferences(State(state): State<Arc<LiveboardState>>) -> Response {
    let store = state.preferences.clone();
    match tokio::task::spawn_blocking(move || store.load()).await {
        Ok(Ok(preferences)) => no_store(Json(preferences).into_response()),
        Ok(Err(error)) => internal_error(error),
        Err(error) => internal_error(error),
    }
}

async fn patch_preferences(
    State(state): State<Arc<LiveboardState>>,
    Json(patch): Json<LiveboardPreferencesPatch>,
) -> Response {
    if let Err(error) = patch.validate() {
        return error_response(StatusCode::BAD_REQUEST, error.to_string());
    }
    let store = state.preferences.clone();
    match tokio::task::spawn_blocking(move || store.mutate(&patch)).await {
        Ok(Ok(preferences)) => no_store(Json(preferences).into_response()),
        Ok(Err(error)) => internal_error(error),
        Err(error) => internal_error(error),
    }
}

#[derive(Deserialize)]
struct OpenFileRequest {
    path: String,
}

async fn open_file(
    State(state): State<Arc<LiveboardState>>,
    Json(request): Json<OpenFileRequest>,
) -> Response {
    let path = PathBuf::from(request.path);
    if !path.is_absolute() {
        return error_response(StatusCode::BAD_REQUEST, "file path must be absolute");
    }
    match std::fs::metadata(&path) {
        Ok(metadata) if metadata.is_file() => {}
        Ok(_) => return error_response(StatusCode::BAD_REQUEST, "path is not a file"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return error_response(StatusCode::NOT_FOUND, "file no longer exists");
        }
        Err(error) => return internal_error(error),
    }

    let store = state.preferences.clone();
    let editor_command = match tokio::task::spawn_blocking(move || store.load()).await {
        Ok(Ok(preferences)) => preferences.editor_command,
        Ok(Err(error)) => return internal_error(error),
        Err(error) => return internal_error(error),
    };
    match launch_editor(editor_command, path) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => error_response(StatusCode::BAD_GATEWAY, error.to_string()),
    }
}

fn launch_editor(editor_command: String, path: PathBuf) -> Result<()> {
    let mut child = Command::new(&editor_command)
        .arg(&path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("failed to launch editor command `{editor_command}`"))?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

macro_rules! fixed_proxy {
    ($name:ident, $path:literal) => {
        async fn $name(
            State(state): State<Arc<LiveboardState>>,
            RawQuery(query): RawQuery,
        ) -> Response {
            proxy_observer(&state, $path, query.as_deref()).await
        }
    };
}

fixed_proxy!(proxy_status, "v1/status");
fixed_proxy!(proxy_agents, "v1/agents");
fixed_proxy!(proxy_timeline, "v1/timeline");
fixed_proxy!(proxy_timeline_diffs, "v1/timeline/diffs");
fixed_proxy!(proxy_events, "v1/events");

async fn proxy_agent(
    State(state): State<Arc<LiveboardState>>,
    AxumPath(id): AxumPath<String>,
    RawQuery(query): RawQuery,
) -> Response {
    proxy_observer(&state, &format!("v1/agents/{id}"), query.as_deref()).await
}

async fn proxy_timeline_detail(
    State(state): State<Arc<LiveboardState>>,
    AxumPath(presentation_id): AxumPath<String>,
    RawQuery(query): RawQuery,
) -> Response {
    proxy_observer(
        &state,
        &format!("v1/timeline/{presentation_id}"),
        query.as_deref(),
    )
    .await
}

async fn proxy_timeline_checkpoints(
    State(state): State<Arc<LiveboardState>>,
    AxumPath(presentation_id): AxumPath<String>,
    RawQuery(query): RawQuery,
) -> Response {
    proxy_observer(
        &state,
        &format!("v1/timeline/{presentation_id}/checkpoints"),
        query.as_deref(),
    )
    .await
}

async fn proxy_invocation(
    State(state): State<Arc<LiveboardState>>,
    AxumPath(id): AxumPath<i64>,
    RawQuery(query): RawQuery,
) -> Response {
    proxy_observer(&state, &format!("v1/invocations/{id}"), query.as_deref()).await
}

async fn proxy_output(
    State(state): State<Arc<LiveboardState>>,
    AxumPath(id): AxumPath<i64>,
    RawQuery(query): RawQuery,
) -> Response {
    proxy_observer(
        &state,
        &format!("v1/invocations/{id}/output"),
        query.as_deref(),
    )
    .await
}

async fn proxy_output_metadata(
    State(state): State<Arc<LiveboardState>>,
    AxumPath(id): AxumPath<i64>,
    RawQuery(query): RawQuery,
) -> Response {
    proxy_observer(
        &state,
        &format!("v1/invocations/{id}/output-metadata"),
        query.as_deref(),
    )
    .await
}

async fn proxy_observer(state: &LiveboardState, path: &str, query: Option<&str>) -> Response {
    let started = std::time::Instant::now();
    let upstream = match state.observer.get(path, query).await {
        Ok(response) => response,
        Err(error) => {
            warn!(
                event = "local_liveboard_observer_proxy_failed",
                path,
                elapsed_ms = started.elapsed().as_millis(),
                error = %error,
            );
            return error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                format!("Local observer unavailable: {error:#}"),
            );
        }
    };
    let status = upstream.status();
    let content_type = upstream.headers().get(header::CONTENT_TYPE).cloned();
    let stream = upstream.bytes_stream().map_err(|error| {
        std::io::Error::other(format!("Local observer response stream failed: {error}"))
    });
    let mut response = Response::builder()
        .status(status)
        .header(header::CACHE_CONTROL, "no-store");
    if let Some(content_type) = content_type {
        response = response.header(header::CONTENT_TYPE, content_type);
    }
    response
        .body(Body::from_stream(stream))
        .expect("proxied Liveboard observer response must be valid")
}

async fn security_boundary(
    State(security): State<SecurityState>,
    request: Request,
    next: Next,
) -> Response {
    if request.headers().get(header::HOST) != Some(&security.expected_host) {
        return security_headers(error_response(
            StatusCode::MISDIRECTED_REQUEST,
            "invalid Liveboard Host header",
        ));
    }
    if let Some(origin) = request.headers().get(header::ORIGIN)
        && origin != security.expected_origin
    {
        return security_headers(error_response(
            StatusCode::FORBIDDEN,
            "cross-origin Liveboard request rejected",
        ));
    }
    security_headers(next.run(request).await)
}

fn security_headers(mut response: Response) -> Response {
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(CSP),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    headers.insert(
        HeaderName::from_static("permissions-policy"),
        HeaderValue::from_static("camera=(), microphone=(), geolocation=()"),
    );
    response
}

fn no_store(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

#[derive(Serialize)]
struct ErrorBody {
    error: String,
}

fn error_response(status: StatusCode, message: impl Into<String>) -> Response {
    no_store(
        (
            status,
            Json(ErrorBody {
                error: message.into(),
            }),
        )
            .into_response(),
    )
}

fn internal_error(error: impl std::fmt::Display) -> Response {
    error_response(
        StatusCode::INTERNAL_SERVER_ERROR,
        format!("Liveboard state error: {error}"),
    )
}
