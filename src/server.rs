use std::convert::Infallible;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use axum::extract::{Request, State};
use axum::http::{StatusCode, Uri};
use axum::middleware::{self, Next};
use axum::response::Response;
use axum::routing::get;
use axum::{Json, Router};
use axum_server::Handle;
use axum_server::tls_rustls::RustlsConfig;
use rmcp::handler::server::{router::tool::ToolRouter, wrapper::Parameters};
use rmcp::model::{ServerCapabilities, ServerInfo};
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};
use rmcp::{Json as McpJson, ServerHandler, tool, tool_handler, tool_router};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use tower::{ServiceExt, service_fn};
use tracing::info;

use crate::config::Config;
use crate::http_api;
use crate::protocol::{ApplyPatchInput, ExecCommandInput, ToolOutput, WriteStdinInput};
use crate::service::{ServiceRequest, ZodexService};
use crate::session::SessionOrigin;

type McpHttpService = StreamableHttpService<ZodexMcpService, LocalSessionManager>;

#[derive(Clone)]
struct ZodexMcpService {
    zodex_service: ZodexService,
    tool_router: ToolRouter<Self>,
}

impl ZodexMcpService {
    fn new(zodex_service: ZodexService) -> Self {
        Self {
            zodex_service,
            tool_router: Self::tool_router(),
        }
    }

    async fn execute_tool_output(
        &self,
        request: ServiceRequest,
    ) -> Result<McpJson<ToolOutput>, String> {
        self.zodex_service
            .execute(request)
            .await
            .and_then(|response| response.into_tool_output())
            .map(McpJson)
            .map_err(|e| e.to_string())
    }

    async fn execute_apply_patch(&self, input: ApplyPatchInput) -> Result<String, String> {
        self.zodex_service
            .execute(ServiceRequest::ApplyPatch { input })
            .await
            .and_then(|response| response.into_apply_patch_output())
            .map(|output| output.output)
            .map_err(|e| e.to_string())
    }
}

#[tool_router]
impl ZodexMcpService {
    #[tool(
        name = "exec_command",
        description = "Run a shell command",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            open_world_hint = true
        )
    )]
    async fn exec_command(
        &self,
        Parameters(input): Parameters<ExecCommandInput>,
    ) -> Result<McpJson<ToolOutput>, String> {
        self.execute_tool_output(ServiceRequest::ExecCommand {
            input,
            origin: SessionOrigin::mcp(None),
        })
        .await
    }

    #[tool(
        name = "write_stdin",
        description = "Write to or poll a running session",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            open_world_hint = true
        )
    )]
    async fn write_stdin(
        &self,
        Parameters(input): Parameters<WriteStdinInput>,
    ) -> Result<McpJson<ToolOutput>, String> {
        self.execute_tool_output(ServiceRequest::WriteStdin { input })
            .await
    }

    #[tool(
        name = "apply_patch",
        description = "Apply a Codex-style patch to files",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            open_world_hint = true
        )
    )]
    async fn apply_patch(
        &self,
        Parameters(input): Parameters<ApplyPatchInput>,
    ) -> Result<String, String> {
        self.execute_apply_patch(input).await
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for ZodexMcpService {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_instructions("zodex remote execution tools")
    }
}

fn build_mcp_service(
    service: ZodexService,
    cancellation_token: CancellationToken,
) -> McpHttpService {
    StreamableHttpService::new(
        move || Ok(ZodexMcpService::new(service.clone())),
        LocalSessionManager::default().into(),
        StreamableHttpServerConfig {
            cancellation_token,
            ..Default::default()
        },
    )
}

fn build_app(
    config: Arc<Config>,
    mcp_service: McpHttpService,
    zodex_service: ZodexService,
) -> Router {
    let mcp_auth_config = config.clone();
    let http_auth_config = config;
    let mcp_root_service = |mcp_service: McpHttpService| {
        service_fn(move |mut request: Request| {
            let mcp_service = mcp_service.clone();
            async move {
                let uri = rewrite_mcp_transport_root_uri(request.uri())
                    .expect("mcp root service only handles /mcp and /mcp/");
                *request.uri_mut() = uri;

                let response = mcp_service
                    .oneshot(request)
                    .await
                    .unwrap_or_else(|never| match never {});
                Ok::<_, Infallible>(response)
            }
        })
    };
    let protected_mcp_router = Router::new()
        .route_service("/mcp", mcp_root_service(mcp_service.clone()))
        .route_service("/mcp/", mcp_root_service(mcp_service))
        .layer(middleware::from_fn_with_state(
            mcp_auth_config,
            query_key_auth,
        ));
    let http_api_router = http_api::build_http_api_router(http_auth_config, zodex_service);

    Router::new()
        .route("/health", get(health))
        .merge(protected_mcp_router)
        .merge(http_api_router)
}

pub async fn run_server(config: Config) -> Result<()> {
    let bind = format!("{}:{}", config.bind_host, config.bind_port);
    let http_bind = config
        .http_bind_port
        .map(|port| format!("{}:{port}", config.bind_host));
    let cert_path = Path::new(&config.tls_cert_path);
    let key_path = Path::new(&config.tls_key_path);
    if !cert_path.exists() || !key_path.exists() {
        bail!(
            "TLS cert/key not found (cert: {}, key: {}). Run `zodex start` or `zodex tls setup` first.",
            config.tls_cert_path,
            config.tls_key_path
        );
    }

    let rustls = RustlsConfig::from_pem_file(cert_path, key_path)
        .await
        .with_context(|| {
            format!(
                "failed to load TLS cert/key from {} and {}",
                config.tls_cert_path, config.tls_key_path
            )
        })?;
    let addr: std::net::SocketAddr = bind
        .parse()
        .with_context(|| format!("invalid bind address {bind}"))?;
    let http_addr: Option<std::net::SocketAddr> = http_bind
        .as_deref()
        .map(|value| {
            value
                .parse()
                .with_context(|| format!("invalid HTTP bind address {value}"))
        })
        .transpose()?;

    let config = Arc::new(config);
    let zodex_service = ZodexService::new(config.clone());

    let cancellation = CancellationToken::new();
    let mcp_service = build_mcp_service(zodex_service.clone(), cancellation.child_token());
    let app = build_app(config, mcp_service, zodex_service);

    let handle = Handle::new();
    let shutdown_handle = handle.clone();
    let http_shutdown = cancellation.child_token();
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        cancellation.cancel();
        shutdown_handle.graceful_shutdown(Some(Duration::from_secs(5)));
    });

    info!("zodexd listening on https://{bind}");
    let tls_app = app.clone();
    let tls_server = async move {
        axum_server::bind_rustls(addr, rustls)
            .handle(handle)
            .serve(tls_app.into_make_service())
            .await
            .context("axum TLS server terminated unexpectedly")
    };

    if let Some(http_addr) = http_addr {
        info!("zodexd also listening on http://{http_addr}");
        let listener = tokio::net::TcpListener::bind(http_addr)
            .await
            .with_context(|| format!("failed to bind HTTP listener on {http_addr}"))?;

        let http_server = async move {
            axum::serve(listener, app.into_make_service())
                .with_graceful_shutdown(async move {
                    http_shutdown.cancelled().await;
                })
                .await
                .context("axum HTTP server terminated unexpectedly")
        };

        let (_tls, _http) = tokio::try_join!(tls_server, http_server)?;
        Ok(())
    } else {
        tls_server.await
    }
}

async fn health() -> Json<Value> {
    Json(json!({ "status": "ok" }))
}

fn rewrite_mcp_transport_root_uri(uri: &Uri) -> Option<Uri> {
    if uri.path() != "/mcp" && uri.path() != "/mcp/" {
        return None;
    }

    let mut parts = uri.clone().into_parts();
    let path_and_query = match uri.query() {
        Some(query) => format!("/?{query}"),
        None => "/".to_string(),
    };
    parts.path_and_query = Some(path_and_query.parse().ok()?);
    Uri::from_parts(parts).ok()
}

async fn query_key_auth(
    State(config): State<Arc<Config>>,
    request: Request,
    next: Next,
) -> std::result::Result<Response, StatusCode> {
    let supplied_key = key_from_query(request.uri().query());

    if supplied_key.as_deref() == Some(config.api_key.as_str()) {
        return Ok(next.run(request).await);
    }

    Err(StatusCode::UNAUTHORIZED)
}

fn key_from_query(query: Option<&str>) -> Option<String> {
    let query = query?;

    for pair in query.split('&') {
        let (key, value) = pair.split_once('=')?;
        if key == "key" {
            return Some(value.to_string());
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::{ZodexMcpService, key_from_query, rewrite_mcp_transport_root_uri};
    use crate::config::Config;
    use crate::protocol::{
        ApplyPatchInput, CommandStatus, ExecCommandInput, TerminationReason, ToolOutput,
        WriteStdinInput,
    };
    use crate::service::ZodexService;
    use axum::body::{Body, to_bytes};
    use axum::http::{Request, StatusCode, Uri};
    use rmcp::ServerHandler;
    use rmcp::handler::server::wrapper::Parameters;
    use rmcp::model::ToolAnnotations;
    use serde_json::json;
    use std::fs;
    use std::sync::Arc;
    use tempfile::tempdir;
    use tokio_util::sync::CancellationToken;
    use tower::util::ServiceExt;

    fn test_config() -> Arc<Config> {
        Arc::new(Config::default())
    }

    async fn wait_for_service_exit(service: &ZodexService, mut output: ToolOutput) -> ToolOutput {
        for _ in 0..10 {
            if output.status == CommandStatus::Exited {
                return output;
            }

            output = service
                .write_stdin(WriteStdinInput {
                    session_handle: output
                        .session_handle
                        .expect("running output should have a session handle"),
                    chars: None,
                    yield_time_ms: Some(250),
                    kill_process: Some(false),
                })
                .await
                .expect("service poll should succeed");
        }

        panic!("service output did not reach exited state in time");
    }

    async fn wait_for_mcp_exit(mcp: &ZodexMcpService, mut output: ToolOutput) -> ToolOutput {
        for _ in 0..10 {
            if output.status == CommandStatus::Exited {
                return output;
            }

            output = mcp
                .write_stdin(Parameters(WriteStdinInput {
                    session_handle: output
                        .session_handle
                        .expect("running output should have a session handle"),
                    chars: None,
                    yield_time_ms: Some(250),
                    kill_process: Some(false),
                }))
                .await
                .expect("mcp poll should succeed")
                .0;
        }

        panic!("mcp output did not reach exited state in time");
    }

    #[test]
    fn registers_apply_patch_tool() {
        let service = ZodexMcpService::new(ZodexService::new(test_config()));
        let names: Vec<String> = service
            .tool_router
            .list_all()
            .iter()
            .map(|tool| tool.name.to_string())
            .collect();

        assert!(names.iter().any(|name| name == "exec_command"));
        assert!(names.iter().any(|name| name == "write_stdin"));
        assert!(names.iter().any(|name| name == "apply_patch"));
        assert!(
            names.iter().all(|name| name != "publish-pr"),
            "publish-pr must not be exposed on remote MCP surface"
        );
        assert!(
            names.iter().all(|name| name != "publish_pr"),
            "publish_pr must not be exposed on remote MCP surface"
        );
    }

    #[test]
    fn server_info_mentions_zodex_remote_execution_tools() {
        let service = ZodexMcpService::new(ZodexService::new(test_config()));
        let info = service.get_info();
        assert_eq!(
            info.instructions.as_deref().unwrap_or_default(),
            "zodex remote execution tools"
        );
    }

    #[test]
    fn tools_have_expected_annotations() {
        let service = ZodexMcpService::new(ZodexService::new(test_config()));

        let by_name = |name: &str| {
            service
                .tool_router
                .list_all()
                .iter()
                .find(|tool| tool.name == name)
                .and_then(|tool| tool.annotations.clone())
                .unwrap_or_else(ToolAnnotations::default)
        };

        let exec = by_name("exec_command");
        assert_eq!(exec.read_only_hint, Some(false));
        assert_eq!(exec.destructive_hint, Some(true));
        assert_eq!(exec.open_world_hint, Some(true));

        let write = by_name("write_stdin");
        assert_eq!(write.read_only_hint, Some(false));
        assert_eq!(write.destructive_hint, Some(true));
        assert_eq!(write.open_world_hint, Some(true));

        let patch = by_name("apply_patch");
        assert_eq!(patch.read_only_hint, Some(false));
        assert_eq!(patch.destructive_hint, Some(true));
        assert_eq!(patch.open_world_hint, Some(true));
    }

    #[tokio::test]
    async fn exec_command_mcp_parity_with_service() {
        let config = test_config();
        let direct = ZodexService::new(config.clone());
        let mcp = ZodexMcpService::new(ZodexService::new(config));
        let input = ExecCommandInput {
            cmd: "printf 'mcp-exec\\n'".to_string(),
            yield_time_ms: Some(2_000),
            workdir: None,
            timeout_ms: None,
        };

        let direct_output = wait_for_service_exit(
            &direct,
            direct
                .exec_command(input.clone())
                .await
                .expect("direct service exec should succeed"),
        )
        .await;
        let mcp_output = wait_for_mcp_exit(
            &mcp,
            mcp.exec_command(Parameters(input))
                .await
                .expect("mcp exec should succeed")
                .0,
        )
        .await;

        assert_eq!(mcp_output.status, direct_output.status);
        assert_eq!(mcp_output.exit_code, direct_output.exit_code);
        assert_eq!(
            mcp_output.termination_reason,
            direct_output.termination_reason
        );
        assert!(mcp_output.output.contains("mcp-exec"));
        assert!(direct_output.output.contains("mcp-exec"));
    }

    #[tokio::test]
    async fn write_stdin_mcp_parity_with_service() {
        let config = test_config();
        let direct = ZodexService::new(config.clone());
        let mcp = ZodexMcpService::new(ZodexService::new(config));
        let shell_input = ExecCommandInput {
            cmd: "bash --noprofile --norc".to_string(),
            yield_time_ms: Some(50),
            workdir: None,
            timeout_ms: Some(60_000),
        };

        let direct_started = direct
            .exec_command(shell_input.clone())
            .await
            .expect("direct shell should start");
        let mcp_started = mcp
            .exec_command(Parameters(shell_input))
            .await
            .expect("mcp shell should start")
            .0;

        let direct_session_handle = direct_started
            .session_handle
            .expect("direct shell should have a session handle");
        let mcp_session_handle = mcp_started
            .session_handle
            .expect("mcp shell should have a session handle");

        let direct_write = direct
            .write_stdin(WriteStdinInput {
                session_handle: direct_session_handle.clone(),
                chars: Some("echo mcp-write\n".to_string()),
                yield_time_ms: Some(500),
                kill_process: Some(false),
            })
            .await
            .expect("direct write should succeed");
        let mcp_write = mcp
            .write_stdin(Parameters(WriteStdinInput {
                session_handle: mcp_session_handle.clone(),
                chars: Some("echo mcp-write\n".to_string()),
                yield_time_ms: Some(500),
                kill_process: Some(false),
            }))
            .await
            .expect("mcp write should succeed")
            .0;

        assert_eq!(mcp_write.status, direct_write.status);
        assert_eq!(
            mcp_write.termination_reason,
            direct_write.termination_reason
        );
        assert_eq!(mcp_write.status, CommandStatus::Running);
        assert!(mcp_write.output.contains("mcp-write"));
        assert!(direct_write.output.contains("mcp-write"));

        let _ = direct
            .write_stdin(WriteStdinInput {
                session_handle: direct_session_handle,
                chars: Some("exit\n".to_string()),
                yield_time_ms: Some(2_000),
                kill_process: Some(false),
            })
            .await
            .expect("direct shell should exit");
        let _ = mcp
            .write_stdin(Parameters(WriteStdinInput {
                session_handle: mcp_session_handle,
                chars: Some("exit\n".to_string()),
                yield_time_ms: Some(2_000),
                kill_process: Some(false),
            }))
            .await
            .expect("mcp shell should exit");
    }

    #[tokio::test]
    async fn kill_process_mcp_parity_with_service() {
        let config = test_config();
        let direct = ZodexService::new(config.clone());
        let mcp = ZodexMcpService::new(ZodexService::new(config));
        let input = ExecCommandInput {
            cmd: "sleep 30".to_string(),
            yield_time_ms: Some(50),
            workdir: None,
            timeout_ms: Some(60_000),
        };

        let direct_started = direct
            .exec_command(input.clone())
            .await
            .expect("direct sleep should start");
        let mcp_started = mcp
            .exec_command(Parameters(input))
            .await
            .expect("mcp sleep should start")
            .0;

        let direct_killed = direct
            .write_stdin(WriteStdinInput {
                session_handle: direct_started
                    .session_handle
                    .expect("direct running handle"),
                chars: Some("echo ignored-direct\n".to_string()),
                yield_time_ms: Some(6_000),
                kill_process: Some(true),
            })
            .await
            .expect("direct kill should succeed");
        let mcp_killed = mcp
            .write_stdin(Parameters(WriteStdinInput {
                session_handle: mcp_started.session_handle.expect("mcp running handle"),
                chars: Some("echo ignored-mcp\n".to_string()),
                yield_time_ms: Some(6_000),
                kill_process: Some(true),
            }))
            .await
            .expect("mcp kill should succeed")
            .0;

        assert_eq!(mcp_killed.status, direct_killed.status);
        assert_eq!(
            mcp_killed.termination_reason,
            direct_killed.termination_reason
        );
        assert!(mcp_killed.session_handle.is_none());
        assert!(direct_killed.session_handle.is_none());
        assert!(mcp_killed.output.contains("terminated by kill_process"));
        assert!(direct_killed.output.contains("terminated by kill_process"));
        assert!(!mcp_killed.output.contains("ignored-mcp"));
        assert!(!direct_killed.output.contains("ignored-direct"));
    }

    #[tokio::test]
    async fn timeout_and_cwd_mcp_parity_with_service() {
        let config = Arc::new(Config {
            default_exec_timeout_ms: 1_000,
            max_exec_timeout_ms: 1_000,
            ..Config::default()
        });
        let direct = ZodexService::new(config.clone());
        let mcp = ZodexMcpService::new(ZodexService::new(config));
        let dir = tempdir().expect("tempdir");

        let direct_cwd = direct
            .exec_command(ExecCommandInput {
                cmd: "pwd".to_string(),
                yield_time_ms: Some(2_000),
                workdir: Some(dir.path().to_string_lossy().to_string()),
                timeout_ms: None,
            })
            .await
            .expect("direct cwd should succeed");
        let mcp_cwd = mcp
            .exec_command(Parameters(ExecCommandInput {
                cmd: "pwd".to_string(),
                yield_time_ms: Some(2_000),
                workdir: Some(dir.path().to_string_lossy().to_string()),
                timeout_ms: None,
            }))
            .await
            .expect("mcp cwd should succeed")
            .0;

        assert_eq!(mcp_cwd.cwd, direct_cwd.cwd);
        assert!(
            mcp_cwd
                .output
                .contains(dir.path().to_string_lossy().as_ref())
        );
        assert!(
            direct_cwd
                .output
                .contains(dir.path().to_string_lossy().as_ref())
        );

        let direct_timeout = direct
            .exec_command(ExecCommandInput {
                cmd: "sleep 30".to_string(),
                yield_time_ms: Some(2_500),
                workdir: None,
                timeout_ms: Some(1_000),
            })
            .await
            .expect("direct timeout should complete");
        let mcp_timeout = mcp
            .exec_command(Parameters(ExecCommandInput {
                cmd: "sleep 30".to_string(),
                yield_time_ms: Some(2_500),
                workdir: None,
                timeout_ms: Some(1_000),
            }))
            .await
            .expect("mcp timeout should complete")
            .0;

        assert_eq!(mcp_timeout.status, direct_timeout.status);
        assert_eq!(
            mcp_timeout.termination_reason,
            direct_timeout.termination_reason
        );
        assert_eq!(
            mcp_timeout.termination_reason,
            Some(TerminationReason::Timeout)
        );
        assert!(
            mcp_timeout
                .output
                .contains("process timed out and was terminated")
        );
        assert!(
            direct_timeout
                .output
                .contains("process timed out and was terminated")
        );
    }

    #[tokio::test]
    async fn apply_patch_mcp_parity_with_service() {
        let config = test_config();
        let direct = ZodexService::new(config.clone());
        let mcp = ZodexMcpService::new(ZodexService::new(config));
        let direct_dir = tempdir().expect("direct tempdir");
        let mcp_dir = tempdir().expect("mcp tempdir");
        let patch = "*** Begin Patch\n*** Add File: parity.txt\n+mcp-patch\n*** End Patch\n";

        let direct_output = direct
            .apply_patch(ApplyPatchInput {
                patch: patch.to_string(),
                workdir: direct_dir.path().to_string_lossy().to_string(),
            })
            .expect("direct apply_patch should succeed");
        let mcp_output = mcp
            .apply_patch(Parameters(ApplyPatchInput {
                patch: patch.to_string(),
                workdir: mcp_dir.path().to_string_lossy().to_string(),
            }))
            .await
            .expect("mcp apply_patch should succeed");

        assert!(direct_output.contains("Success. Updated the following files:"));
        assert!(mcp_output.contains("Success. Updated the following files:"));
        assert_eq!(
            fs::read_to_string(direct_dir.path().join("parity.txt")).expect("read direct patch"),
            "mcp-patch\n"
        );
        assert_eq!(
            fs::read_to_string(mcp_dir.path().join("parity.txt")).expect("read mcp patch"),
            "mcp-patch\n"
        );
    }

    #[test]
    fn key_from_query_extracts_key_value() {
        assert_eq!(
            key_from_query(Some("foo=1&key=expected-value&bar=2")),
            Some("expected-value".to_string())
        );
    }

    #[test]
    fn key_from_query_rejects_missing_or_malformed_key() {
        assert_eq!(key_from_query(None), None);
        assert_eq!(key_from_query(Some("foo=1&bar=2")), None);
        assert_eq!(key_from_query(Some("foo=1&key&bar=2")), None);
    }

    #[test]
    fn rewrite_mcp_transport_root_uri_rewrites_both_mcp_forms_preserving_query() {
        let uri: Uri = "/mcp?key=secret&x=1".parse().expect("uri parse");
        let rewritten = rewrite_mcp_transport_root_uri(&uri).expect("uri should rewrite");
        assert_eq!(rewritten.path(), "/");
        assert_eq!(rewritten.query(), Some("key=secret&x=1"));

        let slash_uri: Uri = "/mcp/?key=secret&x=1".parse().expect("uri parse");
        let slash_rewritten =
            rewrite_mcp_transport_root_uri(&slash_uri).expect("uri should rewrite");
        assert_eq!(slash_rewritten.path(), "/");
        assert_eq!(slash_rewritten.query(), Some("key=secret&x=1"));
    }

    #[test]
    fn rewrite_mcp_transport_root_uri_skips_other_paths() {
        let uri: Uri = "/health".parse().expect("uri parse");
        assert_eq!(rewrite_mcp_transport_root_uri(&uri), None);
    }

    #[tokio::test]
    async fn health_route_stays_public_and_stable() {
        let config = test_config();
        let service = ZodexService::new(config.clone());
        let app = super::build_app(
            config,
            super::build_mcp_service(service.clone(), CancellationToken::new()),
            service,
        );

        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/health")
                    .body(Body::empty())
                    .expect("request build"),
            )
            .await
            .expect("request should succeed");

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body should be readable");
        let value: serde_json::Value = serde_json::from_slice(&body).expect("json body");
        assert_eq!(value, json!({ "status": "ok" }));
    }

    #[tokio::test]
    async fn mcp_routes_accept_both_with_and_without_trailing_slash() {
        let config = test_config();
        let api_key = config.api_key.clone();
        let service = ZodexService::new(config.clone());
        let app = super::build_app(
            config,
            super::build_mcp_service(service.clone(), CancellationToken::new()),
            service,
        );
        let initialize_request = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-03-26",
                "capabilities": {},
                "clientInfo": {
                    "name": "test-client",
                    "version": "0.1"
                }
            }
        });

        for path in [
            format!("/mcp?key={api_key}"),
            format!("/mcp/?key={api_key}"),
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(&path)
                        .header("content-type", "application/json")
                        .header("accept", "application/json, text/event-stream")
                        .body(Body::from(initialize_request.to_string()))
                        .expect("request build"),
                )
                .await
                .expect("request should succeed");

            assert_eq!(
                response.status(),
                StatusCode::OK,
                "expected initialize to succeed for {path}"
            );
        }
    }
}
