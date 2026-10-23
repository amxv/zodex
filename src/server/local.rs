use std::convert::Infallible;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use axum::Router;
use axum::extract::{Request, State};
use axum::http::{HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::Response;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tower::{ServiceExt, service_fn};

use crate::invocation::{InvocationEvidenceRecorder, McpResultContextProvider};
use crate::local::{AGENT_ALIAS_GUIDANCE, LocalAgentAliasService};
use crate::service::ZodexService;
use crate::workdir::validate_absolute_existing_workdir;

use super::{
    McpHttpService, McpServerPolicy, build_mcp_service_with_policy, rewrite_mcp_transport_root_uri,
};

pub const LOCAL_MCP_TOKEN_HEADER: &str = "x-zodex-local-token";

#[derive(Clone)]
pub struct LocalMcpServerConfig {
    pub start_directory: PathBuf,
    pub token: Arc<str>,
    invocation_recorder: Option<Arc<dyn InvocationEvidenceRecorder>>,
    result_context_provider: Option<Arc<dyn McpResultContextProvider>>,
    alias_service: Option<LocalAgentAliasService>,
}

impl LocalMcpServerConfig {
    pub fn new(start_directory: impl Into<PathBuf>, token: impl Into<Arc<str>>) -> Self {
        Self {
            start_directory: start_directory.into(),
            token: token.into(),
            invocation_recorder: None,
            result_context_provider: None,
            alias_service: None,
        }
    }

    pub fn with_invocation_recorder(
        mut self,
        recorder: Arc<dyn InvocationEvidenceRecorder>,
    ) -> Self {
        self.invocation_recorder = Some(recorder);
        self
    }

    pub fn with_result_context_provider(
        mut self,
        provider: Arc<dyn McpResultContextProvider>,
    ) -> Self {
        self.result_context_provider = Some(provider);
        self
    }

    pub(crate) fn with_alias_service(mut self, service: LocalAgentAliasService) -> Self {
        self.alias_service = Some(service);
        self
    }
}

pub struct LocalMcpServer {
    addr: SocketAddr,
    cancellation: CancellationToken,
    task: JoinHandle<Result<()>>,
}

impl LocalMcpServer {
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn url(&self) -> String {
        format!("http://{}/mcp", self.addr)
    }

    pub fn request_shutdown(&self) {
        self.cancellation.cancel();
    }

    pub async fn shutdown(self) -> Result<()> {
        self.request_shutdown();
        self.task
            .await
            .context("Local MCP server task failed to join")??;
        Ok(())
    }
}

pub async fn start_local_mcp_server(
    service: ZodexService,
    config: LocalMcpServerConfig,
) -> Result<LocalMcpServer> {
    start_local_mcp_server_with_observer(service, config, None).await
}

pub(super) async fn start_local_mcp_server_with_observer(
    service: ZodexService,
    config: LocalMcpServerConfig,
    provider_metadata_observer: Option<super::ProviderMetadataObserver>,
) -> Result<LocalMcpServer> {
    let start_directory = validate_absolute_existing_workdir(
        config
            .start_directory
            .to_str()
            .context("Local MCP start directory is not valid UTF-8")?,
    )?;
    let token = validate_token(config.token)?;
    let instructions: Arc<str> = Arc::from(format!(
        "Zodex Local runtime start directory: {}. Use it as the suggested initial explicit workdir. Every exec_command and apply_patch call must still provide an absolute existing workdir; the server never substitutes this path when workdir is missing. {AGENT_ALIAS_GUIDANCE}",
        start_directory.display(),
    ));

    let cancellation = CancellationToken::new();
    let mcp = build_mcp_service_with_policy(
        service,
        cancellation.child_token(),
        McpServerPolicy {
            legacy_session_mode: false,
            json_response: true,
            stateless_protocol_metadata_required: true,
            disable_allowed_hosts: false,
            instructions,
            provider_metadata_observer,
            invocation_recorder: config.invocation_recorder,
            result_context_provider: config.result_context_provider,
            alias_service: config.alias_service,
        },
    );
    let app = local_mcp_app(mcp, token);
    let listener =
        tokio::net::TcpListener::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0))
            .await
            .context("failed to bind Zodex Local MCP loopback listener")?;
    let addr = listener
        .local_addr()
        .context("failed to inspect Zodex Local MCP listener address")?;
    if !addr.ip().is_loopback() {
        bail!("Zodex Local MCP listener unexpectedly bound outside loopback: {addr}");
    }

    let shutdown = cancellation.clone();
    let task = tokio::spawn(async move {
        axum::serve(listener, app.into_make_service())
            .with_graceful_shutdown(shutdown.cancelled_owned())
            .await
            .context("Zodex Local MCP server terminated unexpectedly")
    });

    Ok(LocalMcpServer {
        addr,
        cancellation,
        task,
    })
}

fn local_mcp_app(mcp_service: McpHttpService, token: HeaderValue) -> Router {
    let root_service = |mcp_service: McpHttpService| {
        service_fn(move |mut request: Request| {
            let mcp_service = mcp_service.clone();
            async move {
                let uri = rewrite_mcp_transport_root_uri(request.uri())
                    .expect("Local MCP root service only handles /mcp and /mcp/");
                *request.uri_mut() = uri;
                let response = mcp_service
                    .oneshot(request)
                    .await
                    .unwrap_or_else(|never| match never {});
                Ok::<_, Infallible>(response)
            }
        })
    };

    Router::new()
        .route_service("/mcp", root_service(mcp_service.clone()))
        .route_service("/mcp/", root_service(mcp_service))
        .layer(middleware::from_fn_with_state(token, local_token_auth))
}

async fn local_token_auth(
    State(expected): State<HeaderValue>,
    request: Request,
    next: Next,
) -> std::result::Result<Response, StatusCode> {
    if request.headers().get(LOCAL_MCP_TOKEN_HEADER) == Some(&expected) {
        return Ok(next.run(request).await);
    }
    Err(StatusCode::UNAUTHORIZED)
}

fn validate_token(token: Arc<str>) -> Result<HeaderValue> {
    if token.is_empty() {
        bail!("Zodex Local MCP token must not be empty");
    }
    HeaderValue::from_str(&token).context("Zodex Local MCP token is not a valid HTTP header value")
}
