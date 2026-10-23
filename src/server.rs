use std::convert::Infallible;
use std::sync::Arc;

use anyhow::{Context, Result};
use axum::extract::{Request, State};
use axum::http::{StatusCode, Uri};
use axum::middleware::{self, Next};
use axum::response::Response;
use axum::routing::get;
use axum::{Json as AxumJson, Router};
use rmcp::handler::server::tool::IntoCallToolResult;
use rmcp::handler::server::{router::tool::ToolRouter, wrapper::Parameters};
use rmcp::model::{
    CallToolResponse, CallToolResult, ContentBlock, RequestMetaObject, ServerCapabilities,
    ServerInfo,
};
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};
use rmcp::{ServerHandler, tool, tool_handler, tool_router};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use tower::{ServiceExt, service_fn};
use tracing::{error, info};

use crate::config::Config;
use crate::invocation::{
    InvocationContext, InvocationContinuationKind, InvocationEvidenceRecorder, InvocationOutcome,
    InvocationStart, McpResultContextProvider, ProviderCallMetadata,
};
use crate::local::LocalAgentAliasService;
use crate::protocol::{ApplyPatchInput, ExecCommandInput, ToolOutput, WriteStdinInput};
use crate::service::{ServiceRequest, ZodexService};
use crate::session::{SessionCreatorContext, SessionOrigin};

type McpHttpService = StreamableHttpService<ZodexMcpService, LocalSessionManager>;
const DEFAULT_MCP_INSTRUCTIONS: &str = "zodex remote execution tools";

mod local;

pub use local::{
    LOCAL_MCP_TOKEN_HEADER, LocalMcpServer, LocalMcpServerConfig, start_local_mcp_server,
};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct ProviderMetadata {
    openai_session: Option<String>,
}

fn extract_provider_metadata(meta: &RequestMetaObject) -> ProviderMetadata {
    ProviderMetadata {
        openai_session: meta
            .get("openai/session")
            .and_then(Value::as_str)
            .map(str::to_owned),
    }
}

fn invocation_context(meta: &RequestMetaObject) -> InvocationContext {
    let provider = extract_provider_metadata(meta);
    let mut context = InvocationContext::default()
        .with_correlation_id(format!("{:032x}", rand::random::<u128>()));
    if let Some(session_key) = provider.openai_session {
        context = context.with_provider(ProviderCallMetadata::new("openai/session", session_key));
    }
    context
}

type ProviderMetadataObserver = Arc<dyn Fn(&ProviderMetadata) + Send + Sync>;

#[derive(Clone)]
struct McpServerPolicy {
    legacy_session_mode: bool,
    json_response: bool,
    stateless_protocol_metadata_required: bool,
    disable_allowed_hosts: bool,
    instructions: Arc<str>,
    provider_metadata_observer: Option<ProviderMetadataObserver>,
    invocation_recorder: Option<Arc<dyn InvocationEvidenceRecorder>>,
    result_context_provider: Option<Arc<dyn McpResultContextProvider>>,
    alias_service: Option<LocalAgentAliasService>,
}

impl Default for McpServerPolicy {
    fn default() -> Self {
        Self {
            legacy_session_mode: true,
            json_response: false,
            stateless_protocol_metadata_required: false,
            disable_allowed_hosts: true,
            instructions: Arc::from(DEFAULT_MCP_INSTRUCTIONS),
            provider_metadata_observer: None,
            invocation_recorder: None,
            result_context_provider: None,
            alias_service: None,
        }
    }
}

#[derive(Clone)]
struct ZodexMcpService {
    zodex_service: ZodexService,
    tool_router: ToolRouter<Self>,
    instructions: Arc<str>,
    provider_metadata_observer: Option<ProviderMetadataObserver>,
    invocation_recorder: Option<Arc<dyn InvocationEvidenceRecorder>>,
    result_context_provider: Option<Arc<dyn McpResultContextProvider>>,
    alias_service: Option<LocalAgentAliasService>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SetAgentAliasInput {
    /// Short display name (ideally 2–7 words); an empty string clears it.
    alias: String,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct SetAgentAliasOutput {
    agent_id: String,
    alias: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    zodex_context: Option<String>,
}

// Keep this wrapper named `Json`: rmcp's #[tool] macro recognizes Json<T>
// and continues advertising T as the structured output schema.
struct Json<T> {
    result: Result<T, String>,
    appended_context: Option<String>,
}

trait StructuredModelContext {
    fn set_zodex_context(&mut self, context: String);
}

impl StructuredModelContext for ToolOutput {
    fn set_zodex_context(&mut self, context: String) {
        self.zodex_context = Some(context);
    }
}

impl StructuredModelContext for SetAgentAliasOutput {
    fn set_zodex_context(&mut self, context: String) {
        self.zodex_context = Some(context);
    }
}

impl<T> Json<T> {
    fn new(result: Result<T, String>, appended_context: Option<String>) -> Self {
        Self {
            result,
            appended_context,
        }
    }

    #[cfg(test)]
    fn into_result(self) -> Result<T, String> {
        self.result
    }
}

impl<T: Serialize + schemars::JsonSchema + StructuredModelContext + 'static> IntoCallToolResult
    for Json<T>
{
    fn into_call_tool_result(self) -> Result<CallToolResponse, rmcp::ErrorData> {
        let result = match self.result {
            Ok(mut value) => {
                if let Some(context) = self.appended_context {
                    value.set_zodex_context(context);
                }
                let value = serde_json::to_value(value).map_err(|error| {
                    rmcp::ErrorData::internal_error(
                        format!("Failed to serialize structured content: {error}"),
                        None,
                    )
                })?;
                CallToolResult::structured(value)
            }
            Err(error) => CallToolResult::error(vec![ContentBlock::text(primary_text(
                error,
                self.appended_context,
            ))]),
        };
        Ok(result.into())
    }
}

struct TextResult {
    result: Result<String, String>,
    appended_context: Option<String>,
}

impl TextResult {
    fn new(result: Result<String, String>, appended_context: Option<String>) -> Self {
        Self {
            result,
            appended_context,
        }
    }

    #[cfg(test)]
    fn into_result(self) -> Result<String, String> {
        self.result
    }
}

impl IntoCallToolResult for TextResult {
    fn into_call_tool_result(self) -> Result<CallToolResponse, rmcp::ErrorData> {
        let result = match self.result {
            Ok(value) => CallToolResult::success(vec![ContentBlock::text(primary_text(
                value,
                self.appended_context,
            ))]),
            Err(error) => CallToolResult::error(vec![ContentBlock::text(primary_text(
                error,
                self.appended_context,
            ))]),
        };
        Ok(result.into())
    }
}

fn primary_text(value: String, appended_context: Option<String>) -> String {
    match appended_context {
        Some(context) if !context.is_empty() => format!("{value}\n\n{context}"),
        _ => value,
    }
}

impl ZodexMcpService {
    #[cfg(test)]
    fn new(zodex_service: ZodexService) -> Self {
        Self::with_options(
            zodex_service,
            Arc::from(DEFAULT_MCP_INSTRUCTIONS),
            None,
            None,
            None,
            None,
        )
    }

    fn with_options(
        zodex_service: ZodexService,
        instructions: Arc<str>,
        provider_metadata_observer: Option<ProviderMetadataObserver>,
        invocation_recorder: Option<Arc<dyn InvocationEvidenceRecorder>>,
        result_context_provider: Option<Arc<dyn McpResultContextProvider>>,
        alias_service: Option<LocalAgentAliasService>,
    ) -> Self {
        let mut tool_router = Self::tool_router();
        if alias_service.is_none() {
            tool_router.remove_route("set_agent_alias");
        }
        Self {
            zodex_service,
            tool_router,
            instructions,
            provider_metadata_observer,
            invocation_recorder,
            result_context_provider,
            alias_service,
        }
    }

    fn observe_provider_metadata(&self, meta: &RequestMetaObject) {
        if let Some(observer) = &self.provider_metadata_observer {
            observer(&extract_provider_metadata(meta));
        }
    }

    fn begin_invocation<T: Serialize>(
        &self,
        tool_name: &'static str,
        input: &T,
        request_meta: &RequestMetaObject,
        target_creator: Option<SessionCreatorContext>,
        continuation_kind: Option<InvocationContinuationKind>,
    ) -> InvocationContext {
        self.observe_provider_metadata(request_meta);
        let context = invocation_context(request_meta);
        let Some(recorder) = &self.invocation_recorder else {
            return context;
        };
        let arguments = match serde_json::to_value(input) {
            Ok(arguments) => arguments,
            Err(error) => {
                error!(
                    event = "local_invocation_input_evidence_serialization_failed",
                    tool_name,
                    error = %error,
                );
                return context;
            }
        };
        let mut start = InvocationStart::new(tool_name, arguments);
        if let Some(target_creator) = target_creator {
            start = start
                .with_target_created_by_agent_id(target_creator.agent_id)
                .with_target_created_by_invocation_id(target_creator.invocation_id);
        }
        if let Some(continuation_kind) = continuation_kind {
            start = start.with_continuation_kind(continuation_kind);
        }
        match recorder.begin(context.clone(), start) {
            Ok(recorded) => recorded,
            Err(error) => {
                // Invocation evidence is an observability surface, never an
                // execution dependency. A broken or saturated history store
                // must not reject a model tool call or poison the MCP bridge.
                error!(
                    event = "local_invocation_evidence_begin_failed_open",
                    tool_name,
                    error = %error,
                );
                context
            }
        }
    }

    fn complete_invocation<T: Serialize>(
        &self,
        context: &InvocationContext,
        result: &Result<T, String>,
    ) {
        let Some(recorder) = &self.invocation_recorder else {
            return;
        };
        // A missing durable ID means the begin-recording path failed open.
        // Do not turn that observability failure into another request-path
        // failure at completion time.
        if context.invocation_id.is_none() {
            return;
        }
        let outcome = match result {
            Ok(value) => match serde_json::to_value(value) {
                Ok(value) => InvocationOutcome::Success(value),
                Err(error) => {
                    error!(
                        event = "local_invocation_result_serialization_failed",
                        invocation_id = ?context.invocation_id,
                        error = %error,
                    );
                    InvocationOutcome::Error(format!(
                        "internal evidence serialization failure after tool completion: {error}"
                    ))
                }
            },
            Err(error) => InvocationOutcome::Error(error.clone()),
        };
        if let Err(error) = recorder.complete(context, outcome) {
            error!(
                event = "local_invocation_completion_persistence_failed",
                invocation_id = ?context.invocation_id,
                error = %error,
            );
        }
    }

    fn appended_context(
        &self,
        invocation: &InvocationContext,
        workdir: Option<&str>,
        tool_succeeded: bool,
    ) -> Option<String> {
        let provider = self.result_context_provider.as_ref()?;
        match provider.appended_context(invocation, workdir, tool_succeeded) {
            Ok(context) => context,
            Err(error) => {
                error!(
                    event = "local_mcp_result_context_failed",
                    invocation_id = ?invocation.invocation_id,
                    error = %error,
                );
                None
            }
        }
    }

    async fn execute_tool_output(
        &self,
        request: ServiceRequest,
        invocation: InvocationContext,
    ) -> Result<ToolOutput, String> {
        self.zodex_service
            .execute_with_context(request, invocation)
            .await
            .and_then(|response| response.into_tool_output())
            .map_err(|e| e.to_string())
    }

    async fn execute_apply_patch(
        &self,
        input: ApplyPatchInput,
        invocation: InvocationContext,
    ) -> Result<String, String> {
        self.zodex_service
            .execute_with_context(ServiceRequest::ApplyPatch { input }, invocation)
            .await
            .and_then(|response| response.into_apply_patch_output())
            .map(|output| output.output)
            .map_err(|e| e.to_string())
    }
}

#[tool_router]
impl ZodexMcpService {
    #[tool(
        name = "set_agent_alias",
        description = "Set this conversation's Liveboard display alias (prefer 2-7 descriptive words). Optional; ordinary tool calls work without an alias. Call again when the main task changes. An empty alias clears it. Never changes the stable Agent ID.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    async fn set_agent_alias(
        &self,
        Parameters(input): Parameters<SetAgentAliasInput>,
        request_meta: RequestMetaObject,
    ) -> Json<SetAgentAliasOutput> {
        self.observe_provider_metadata(&request_meta);
        let Some(service) = self.alias_service.clone() else {
            return Json::new(
                Err("Agent aliases are available only in Zodex Local".into()),
                None,
            );
        };
        let context = invocation_context(&request_meta);
        let result =
            tokio::task::spawn_blocking(move || service.set_alias(context, input.alias)).await;
        match result {
            Ok(Ok(updated)) => {
                let appended = self.appended_context(&updated.context, None, true);
                Json::new(
                    Ok(SetAgentAliasOutput {
                        agent_id: updated.agent_id,
                        alias: updated.alias,
                        zodex_context: None,
                    }),
                    appended,
                )
            }
            Ok(Err(error)) => Json::new(Err(error.to_string()), None),
            Err(error) => Json::new(Err(format!("Alias update task failed: {error}")), None),
        }
    }

    #[tool(
        name = "exec_command",
        description = "Run a shell command in a required absolute existing workdir",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    async fn exec_command(
        &self,
        Parameters(input): Parameters<ExecCommandInput>,
        request_meta: RequestMetaObject,
    ) -> Json<ToolOutput> {
        let workdir = input.workdir.clone();
        let invocation = self.begin_invocation("exec_command", &input, &request_meta, None, None);
        let result = self
            .execute_tool_output(
                ServiceRequest::ExecCommand {
                    input,
                    origin: SessionOrigin::mcp(None),
                },
                invocation.clone(),
            )
            .await;
        self.complete_invocation(&invocation, &result);
        let appended_context = self.appended_context(&invocation, Some(&workdir), result.is_ok());
        Json::new(result, appended_context)
    }

    #[tool(
        name = "write_stdin",
        description = "Write to or poll a running session",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    async fn write_stdin(
        &self,
        Parameters(input): Parameters<WriteStdinInput>,
        request_meta: RequestMetaObject,
    ) -> Json<ToolOutput> {
        let target_creator = self
            .zodex_service
            .session_creator_context(&input.session_handle)
            .await;
        let continuation_kind = write_stdin_continuation_kind(&input);
        let invocation = self.begin_invocation(
            "write_stdin",
            &input,
            &request_meta,
            target_creator,
            Some(continuation_kind),
        );
        let result = self
            .execute_tool_output(ServiceRequest::WriteStdin { input }, invocation.clone())
            .await;
        self.complete_invocation(&invocation, &result);
        let appended_context = self.appended_context(&invocation, None, result.is_ok());
        Json::new(result, appended_context)
    }

    #[tool(
        name = "apply_patch",
        description = "Apply a Codex-style patch using a required absolute existing workdir",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    async fn apply_patch(
        &self,
        Parameters(input): Parameters<ApplyPatchInput>,
        request_meta: RequestMetaObject,
    ) -> TextResult {
        let workdir = input.workdir.clone();
        let invocation = self.begin_invocation("apply_patch", &input, &request_meta, None, None);
        let result = self.execute_apply_patch(input, invocation.clone()).await;
        self.complete_invocation(&invocation, &result);
        let appended_context = self.appended_context(&invocation, Some(&workdir), result.is_ok());
        TextResult::new(result, appended_context)
    }
}

fn write_stdin_continuation_kind(input: &WriteStdinInput) -> InvocationContinuationKind {
    if input.kill_process.unwrap_or(false) {
        InvocationContinuationKind::Kill
    } else if input
        .chars
        .as_deref()
        .is_some_and(|chars| !chars.is_empty())
    {
        InvocationContinuationKind::Stdin
    } else {
        InvocationContinuationKind::Poll
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for ZodexMcpService {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_instructions(self.instructions.to_string())
    }
}

fn build_mcp_service(
    service: ZodexService,
    cancellation_token: CancellationToken,
) -> McpHttpService {
    build_mcp_service_with_policy(service, cancellation_token, McpServerPolicy::default())
}

fn build_mcp_service_with_policy(
    service: ZodexService,
    cancellation_token: CancellationToken,
    policy: McpServerPolicy,
) -> McpHttpService {
    let instructions = policy.instructions.clone();
    let provider_metadata_observer = policy.provider_metadata_observer.clone();
    let invocation_recorder = policy.invocation_recorder.clone();
    let result_context_provider = policy.result_context_provider.clone();
    let alias_service = policy.alias_service.clone();
    let mut config = StreamableHttpServerConfig::default()
        .with_legacy_session_mode(policy.legacy_session_mode)
        .with_json_response(policy.json_response)
        .with_stateless_protocol_metadata_required(policy.stateless_protocol_metadata_required)
        .with_cancellation_token(cancellation_token);
    if policy.disable_allowed_hosts {
        config = config.disable_allowed_hosts();
    }
    StreamableHttpService::new(
        move || {
            Ok(ZodexMcpService::with_options(
                service.clone(),
                instructions.clone(),
                provider_metadata_observer.clone(),
                invocation_recorder.clone(),
                result_context_provider.clone(),
                alias_service.clone(),
            ))
        },
        LocalSessionManager::default().into(),
        config,
    )
}

fn build_app(config: Arc<Config>, mcp_service: McpHttpService) -> Router {
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
        .route_layer(middleware::from_fn_with_state(config, query_key_auth));

    Router::new()
        .route("/health", get(health))
        .merge(protected_mcp_router)
}

pub async fn run_server(config: Config) -> Result<()> {
    let bind = format!("{}:{}", config.bind_host, config.service_port);
    let addr: std::net::SocketAddr = bind
        .parse()
        .with_context(|| format!("invalid bind address {bind}"))?;

    let config = Arc::new(config);
    let zodex_service = ZodexService::new(config.clone());

    let cancellation = CancellationToken::new();
    let mcp_service = build_mcp_service(zodex_service.clone(), cancellation.child_token());
    let app = build_app(config, mcp_service);
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("failed to bind Sprite HTTP listener on {addr}"))?;
    let shutdown = cancellation.clone();

    info!("zodexd listening on http://{bind}");
    axum::serve(listener, app.into_make_service())
        .with_graceful_shutdown(async move {
            let _ = tokio::signal::ctrl_c().await;
            shutdown.cancel();
        })
        .await
        .context("Sprite HTTP server terminated unexpectedly")
}

async fn health() -> AxumJson<Value> {
    AxumJson(json!({
        "status": "ok",
        "component": "zodexd",
        "version": env!("CARGO_PKG_VERSION")
    }))
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
mod http_surface_tests;
#[cfg(test)]
mod local_history_tests;
#[cfg(test)]
mod local_tests;
#[cfg(test)]
mod tests;
