use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::{Result, bail};
use reqwest::Response;
use serde_json::{Value, json};
use tempfile::tempdir;

use crate::config::Config;
use crate::invocation::{
    InvocationContext, InvocationEvidenceRecorder, InvocationOutcome, InvocationStart,
    McpResultContextProvider,
};
use crate::local::{
    HistoryQuery, LiveboardPreferencesStore, LocalAgentAliasService, LocalHistoryReader,
    LocalHistoryRuntime, LocalHistoryRuntimeConfig, LocalPaths,
};
use crate::protocol::{CommandStatus, TerminationReason, ToolOutput};
use crate::service::ZodexService;
use crate::session::{SessionOutputChunk, SessionOutputObserver, SessionRuntimePolicy};

use super::local::{
    LOCAL_MCP_TOKEN_HEADER, LocalMcpServerConfig, start_local_mcp_server_with_observer,
};
use super::start_local_mcp_server;

pub(super) const TOKEN: &str = "local-mcp-test-token";

fn local_environment() -> Vec<(OsString, OsString)> {
    [
        ("HOME", "/tmp/zodex-local-test-home"),
        ("USER", "zodex-local-test"),
        ("LOGNAME", "zodex-local-test"),
        ("PATH", "/usr/bin:/bin"),
    ]
    .into_iter()
    .map(|(key, value)| (OsString::from(key), OsString::from(value)))
    .collect()
}

fn local_service(config: Config) -> ZodexService {
    ZodexService::with_session_policy(
        Arc::new(config),
        SessionRuntimePolicy::local("/bin/sh", local_environment()).unwrap(),
    )
}

pub(super) fn history_runtime(path: PathBuf) -> Arc<LocalHistoryRuntime> {
    LocalHistoryRuntime::open(LocalHistoryRuntimeConfig::new(
        path,
        "local-http-history-test-runtime",
        365 * 24 * 60 * 60,
        1024 * 1024 * 1024,
    ))
    .unwrap()
}

pub(super) fn local_service_with_history(
    config: Config,
    history: Arc<LocalHistoryRuntime>,
) -> ZodexService {
    let policy = SessionRuntimePolicy::local("/bin/sh", local_environment())
        .unwrap()
        .with_output_observer(history);
    ZodexService::with_session_policy(Arc::new(config), policy)
}

pub(super) async fn shutdown_history_runtime(history: Arc<LocalHistoryRuntime>) {
    tokio::task::spawn_blocking(move || history.shutdown_blocking())
        .await
        .unwrap()
        .unwrap();
}

pub(super) fn test_http_client() -> reqwest::Client {
    crate::install_rustls_crypto_provider();
    reqwest::Client::new()
}

fn modern_meta(openai_session: Option<&str>) -> Value {
    let mut meta = serde_json::Map::from_iter([
        (
            "io.modelcontextprotocol/protocolVersion".to_string(),
            json!("2026-07-28"),
        ),
        (
            "io.modelcontextprotocol/clientInfo".to_string(),
            json!({"name": "zodex-local-mcp-test", "version": "1.0"}),
        ),
        (
            "io.modelcontextprotocol/clientCapabilities".to_string(),
            json!({}),
        ),
    ]);
    if let Some(session) = openai_session {
        meta.insert("openai/session".to_string(), json!(session));
    }
    Value::Object(meta)
}

pub(super) async fn post_mcp(
    client: &reqwest::Client,
    url: &str,
    token: Option<&str>,
    method_header: Option<&str>,
    body: Value,
) -> Response {
    let mut request = client
        .post(url)
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .json(&body);
    if let Some(token) = token {
        request = request.header(LOCAL_MCP_TOKEN_HEADER, token);
    }
    if let Some(method) = method_header {
        request = request
            .header("MCP-Protocol-Version", "2026-07-28")
            .header("Mcp-Method", method);
        if method == "tools/call"
            && let Some(name) = body
                .get("params")
                .and_then(|params| params.get("name"))
                .and_then(Value::as_str)
        {
            request = request.header("Mcp-Name", name);
        }
    }
    request.send().await.unwrap()
}

pub(super) async fn json_response(response: Response) -> Value {
    let status = response.status();
    let bytes = response.bytes().await.unwrap();
    assert!(
        status.is_success(),
        "unexpected HTTP status {status}: {}",
        String::from_utf8_lossy(&bytes)
    );
    serde_json::from_slice(&bytes).unwrap()
}

fn tool_output(value: &Value) -> ToolOutput {
    serde_json::from_value(value["result"]["structuredContent"].clone())
        .unwrap_or_else(|error| panic!("invalid ToolOutput in MCP response ({error}): {value}"))
}

fn text_content(value: &Value) -> Vec<&str> {
    value["result"]["content"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|block| block.get("text").and_then(Value::as_str))
        .collect()
}

#[tokio::test]
async fn local_alias_tool_is_optional_identifies_first_call_and_never_enters_timeline() {
    let root = tempdir().unwrap();
    let paths = LocalPaths::from_roots(
        root.path().join("config"),
        root.path().join("data"),
        root.path().join("state"),
    )
    .unwrap();
    let preferences = LiveboardPreferencesStore::new(&paths);
    let history = history_runtime(paths.history_database());
    let service = local_service_with_history(Config::default(), history.clone());
    let server = start_local_mcp_server(
        service.clone(),
        LocalMcpServerConfig::new(root.path(), TOKEN)
            .with_invocation_recorder(history.clone())
            .with_alias_service(LocalAgentAliasService::new(
                history.clone(),
                preferences.clone(),
            )),
    )
    .await
    .unwrap();
    let client = test_http_client();

    let tools = json_response(post_mcp(
        &client,
        &server.url(),
        Some(TOKEN),
        Some("tools/list"),
        json!({"jsonrpc":"2.0","id":1,"method":"tools/list", "params":{"_meta":modern_meta(None)}}),
    ).await).await;
    let names = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(names.len(), 4);
    assert!(names.contains(&"set_agent_alias"));
    // Check the actual MCP tools/list response for EVERY Local tool, not just
    // the alias. Future tools must retain the same read-only annotations.
    for tool in tools["result"]["tools"].as_array().unwrap() {
        let name = tool["name"].as_str().unwrap();
        assert_eq!(
            tool["annotations"]["readOnlyHint"], true,
            "{name} must advertise read-only"
        );
        assert_eq!(
            tool["annotations"]["openWorldHint"], false,
            "{name} must disable open-world"
        );
        assert_eq!(
            tool["annotations"]["destructiveHint"], false,
            "{name} must disable destructive hint"
        );
    }
    let alias_schema = &tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "set_agent_alias")
        .unwrap()["inputSchema"];
    assert_eq!(alias_schema["required"], json!(["alias"]));
    assert!(alias_schema["properties"].get("agent_id").is_none());

    // First call is the alias tool: it must allocate a stable Agent ID without
    // creating an invocation, command timeline card, or process.
    let renamed = json_response(
        post_mcp(
            &client,
            &server.url(),
            Some(TOKEN),
            Some("tools/call"),
            tool_call(
                2,
                "set_agent_alias",
                json!({"alias":"Rindle implementation"}),
                "alias-first-session",
            ),
        )
        .await,
    )
    .await;
    assert_eq!(renamed["result"]["isError"], false);
    let id = renamed["result"]["structuredContent"]["agent_id"]
        .as_str()
        .unwrap();
    assert_eq!(id.len(), 4);
    assert_eq!(
        renamed["result"]["structuredContent"]["alias"],
        "Rindle implementation"
    );
    assert_eq!(
        preferences.load().unwrap().agents[id].alias.as_deref(),
        Some("Rindle implementation")
    );
    assert_eq!(
        LocalHistoryReader::agent_count(&paths.history_database(), None).unwrap(),
        1
    );
    assert!(
        LocalHistoryReader::query(&paths.history_database(), &HistoryQuery::recent(10))
            .unwrap()
            .is_empty()
    );

    // A normal execution in the same conversation must retain that identity.
    let execution = json_response(
        post_mcp(
            &client,
            &server.url(),
            Some(TOKEN),
            Some("tools/call"),
            tool_call(
                3,
                "exec_command",
                json!({"cmd":"printf 'ok'", "workdir":root.path(), "yield_time_ms":2000}),
                "alias-first-session",
            ),
        )
        .await,
    )
    .await;
    assert_eq!(execution["result"]["isError"], false);
    let recorded =
        LocalHistoryReader::query(&paths.history_database(), &HistoryQuery::recent(20)).unwrap();
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].agent_id.as_deref(), Some(id));

    // Another thread can skip naming and execute normally, still receiving
    // its own independent Agent ID, with no default alias in preferences.
    let plain = json_response(
        post_mcp(
            &client,
            &server.url(),
            Some(TOKEN),
            Some("tools/call"),
            tool_call(
                4,
                "exec_command",
                json!({"cmd":"true", "workdir":root.path(), "yield_time_ms":2000}),
                "plain-session",
            ),
        )
        .await,
    )
    .await;
    assert_eq!(plain["result"]["isError"], false);
    let recorded =
        LocalHistoryReader::query(&paths.history_database(), &HistoryQuery::recent(20)).unwrap();
    let plain_id = recorded
        .iter()
        .find(|call| call.provider_session_key.as_deref() == Some("plain-session"))
        .and_then(|call| call.agent_id.clone())
        .unwrap();
    assert_ne!(plain_id, id);
    assert!(!preferences.load().unwrap().agents.contains_key(&plain_id));

    let updated = json_response(
        post_mcp(
            &client,
            &server.url(),
            Some(TOKEN),
            Some("tools/call"),
            tool_call(
                5,
                "set_agent_alias",
                json!({"alias":"Rindle CLI tests"}),
                "alias-first-session",
            ),
        )
        .await,
    )
    .await;
    assert_eq!(updated["result"]["structuredContent"]["agent_id"], id);
    assert_eq!(
        preferences.load().unwrap().agents[id].alias.as_deref(),
        Some("Rindle CLI tests")
    );
    let cleared = json_response(
        post_mcp(
            &client,
            &server.url(),
            Some(TOKEN),
            Some("tools/call"),
            tool_call(
                6,
                "set_agent_alias",
                json!({"alias":"  "}),
                "alias-first-session",
            ),
        )
        .await,
    )
    .await;
    assert!(cleared["result"]["structuredContent"]["alias"].is_null());
    assert!(preferences.load().unwrap().agents[id].alias.is_none());

    let invalid = json_response(
        post_mcp(
            &client,
            &server.url(),
            Some(TOKEN),
            Some("tools/call"),
            tool_call(
                7,
                "set_agent_alias",
                json!({"alias":"A".repeat(81)}),
                "alias-first-session",
            ),
        )
        .await,
    )
    .await;
    assert_eq!(invalid["result"]["isError"], true);
    assert!(preferences.load().unwrap().agents[id].alias.is_none());

    let unauthenticated = json_response(post_mcp(
        &client, &server.url(), Some(TOKEN), Some("tools/call"),
        json!({"jsonrpc":"2.0", "id":8, "method":"tools/call", "params":{
            "name":"set_agent_alias", "arguments":{"alias":"Not my agent"}, "_meta":modern_meta(None)
        }}),
    ).await).await;
    assert_eq!(unauthenticated["result"]["isError"], true);
    assert_eq!(
        LocalHistoryReader::agent_count(&paths.history_database(), None).unwrap(),
        2
    );
    let invocations =
        LocalHistoryReader::query(&paths.history_database(), &HistoryQuery::recent(20)).unwrap();
    assert_eq!(invocations.len(), 2);
    assert!(
        invocations
            .iter()
            .all(|call| call.tool_name == "exec_command")
    );

    server.shutdown().await.unwrap();
    service.shutdown_sessions().await.unwrap();
    shutdown_history_runtime(history).await;
}

struct FixedResultContextProvider;

impl McpResultContextProvider for FixedResultContextProvider {
    fn appended_context(
        &self,
        context: &InvocationContext,
        _workdir: Option<&str>,
        _tool_succeeded: bool,
    ) -> Result<Option<String>> {
        Ok(context.agent_id.as_ref().map(|_| {
            "Global skills on this machine:\n- demo — demo skill — /tmp/demo/SKILL.md".to_string()
        }))
    }
}

pub(super) fn tool_call(id: u64, name: &str, arguments: Value, session: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "tools/call",
        "params": {
            "name": name,
            "arguments": arguments,
            "_meta": modern_meta(Some(session)),
        }
    })
}

#[tokio::test]
async fn local_mcp_puts_context_inside_primary_structured_tool_output() {
    let root = tempdir().unwrap();
    let history = history_runtime(root.path().join("history.sqlite3"));
    let service = local_service_with_history(Config::default(), history.clone());
    let server = start_local_mcp_server(
        service.clone(),
        LocalMcpServerConfig::new(root.path(), TOKEN)
            .with_invocation_recorder(history.clone())
            .with_result_context_provider(Arc::new(FixedResultContextProvider)),
    )
    .await
    .unwrap();
    let client = test_http_client();
    let response = json_response(
        post_mcp(
            &client,
            &server.url(),
            Some(TOKEN),
            Some("tools/call"),
            tool_call(
                91,
                "exec_command",
                json!({
                    "cmd":"printf 'original-output\\n'",
                    "workdir":root.path(),
                    "yield_time_ms":2000
                }),
                "context-append-session",
            ),
        )
        .await,
    )
    .await;

    let output = tool_output(&response);
    assert!(output.output.contains("original-output"));
    assert!(!output.output.contains("Global skills on this machine:"));
    assert_eq!(
        output.zodex_context.as_deref(),
        Some("Global skills on this machine:\n- demo — demo skill — /tmp/demo/SKILL.md")
    );
    let content = text_content(&response);
    assert_eq!(content.len(), 1);
    assert!(content[0].contains("original-output"));
    assert!(content[0].contains("\"zodex_context\""));

    server.shutdown().await.unwrap();
    service.shutdown_sessions().await.unwrap();
    shutdown_history_runtime(history).await;
}

#[tokio::test]
async fn local_mcp_keeps_context_in_primary_text_block_for_errors() {
    let root = tempdir().unwrap();
    let history = history_runtime(root.path().join("history.sqlite3"));
    let service = local_service_with_history(Config::default(), history.clone());
    let server = start_local_mcp_server(
        service.clone(),
        LocalMcpServerConfig::new(root.path(), TOKEN)
            .with_invocation_recorder(history.clone())
            .with_result_context_provider(Arc::new(FixedResultContextProvider)),
    )
    .await
    .unwrap();
    let client = test_http_client();
    let missing_workdir = root.path().join("missing");
    let response = json_response(
        post_mcp(
            &client,
            &server.url(),
            Some(TOKEN),
            Some("tools/call"),
            tool_call(
                92,
                "exec_command",
                json!({
                    "cmd":"true",
                    "workdir":missing_workdir,
                    "yield_time_ms":2000
                }),
                "context-error-session",
            ),
        )
        .await,
    )
    .await;

    assert_eq!(response["result"]["isError"], true);
    assert!(response["result"]["structuredContent"].is_null());
    let content = text_content(&response);
    assert_eq!(content.len(), 1);
    assert!(content[0].contains("workdir"));
    assert!(content[0].contains("Global skills on this machine:"));

    server.shutdown().await.unwrap();
    service.shutdown_sessions().await.unwrap();
    shutdown_history_runtime(history).await;
}

#[tokio::test]
async fn local_mcp_keeps_context_in_primary_apply_patch_text_block() {
    let root = tempdir().unwrap();
    let history = history_runtime(root.path().join("history.sqlite3"));
    let service = local_service_with_history(Config::default(), history.clone());
    let server = start_local_mcp_server(
        service.clone(),
        LocalMcpServerConfig::new(root.path(), TOKEN)
            .with_invocation_recorder(history.clone())
            .with_result_context_provider(Arc::new(FixedResultContextProvider)),
    )
    .await
    .unwrap();
    let client = test_http_client();
    let response = json_response(
        post_mcp(
            &client,
            &server.url(),
            Some(TOKEN),
            Some("tools/call"),
            tool_call(
                93,
                "apply_patch",
                json!({
                    "patch":"*** Begin Patch\n*** Add File: context-smoke.txt\n+ok\n*** End Patch\n",
                    "workdir":root.path()
                }),
                "context-patch-session",
            ),
        )
        .await,
    )
    .await;

    assert_ne!(response["result"]["isError"], true);
    let content = text_content(&response);
    assert_eq!(content.len(), 1);
    assert!(content[0].contains("Success. Updated the following files:"));
    assert!(content[0].contains("Global skills on this machine:"));

    server.shutdown().await.unwrap();
    service.shutdown_sessions().await.unwrap();
    shutdown_history_runtime(history).await;
}

#[tokio::test]
async fn local_listener_is_loopback_token_authenticated_and_exposes_only_mcp_surface() {
    let dir = tempdir().unwrap();
    let server = start_local_mcp_server(
        local_service(Config::default()),
        LocalMcpServerConfig::new(dir.path(), TOKEN),
    )
    .await
    .unwrap();
    assert!(server.addr().ip().is_loopback());
    let client = test_http_client();
    let discover = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "server/discover",
        "params": {"_meta": modern_meta(None)}
    });

    for token in [None, Some("wrong-token")] {
        let response = post_mcp(
            &client,
            &server.url(),
            token,
            Some("server/discover"),
            discover.clone(),
        )
        .await;
        assert_eq!(response.status(), reqwest::StatusCode::UNAUTHORIZED);
    }

    let response = client
        .post(server.url())
        .header("Authorization", format!("Bearer {TOKEN}"))
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .header("MCP-Protocol-Version", "2026-07-28")
        .header("Mcp-Method", "server/discover")
        .json(&discover)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::UNAUTHORIZED);

    let response = client
        .post(format!("http://{}/v1/exec-command", server.addr()))
        .header(LOCAL_MCP_TOKEN_HEADER, TOKEN)
        .json(&json!({"cmd":"echo no", "workdir": dir.path()}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::NOT_FOUND);

    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn local_discovery_and_tools_list_are_stateless_and_runtime_specific() {
    let first = tempdir().unwrap();
    let second = tempdir().unwrap();
    let client = test_http_client();

    for (id, dir) in [(10_u64, first.path()), (20_u64, second.path())] {
        let server = start_local_mcp_server(
            local_service(Config::default()),
            LocalMcpServerConfig::new(dir, TOKEN),
        )
        .await
        .unwrap();
        let discover = json_response(
            post_mcp(
                &client,
                &server.url(),
                Some(TOKEN),
                Some("server/discover"),
                json!({
                    "jsonrpc":"2.0",
                    "id": id,
                    "method":"server/discover",
                    "params":{"_meta": modern_meta(None)}
                }),
            )
            .await,
        )
        .await;
        assert!(
            discover["result"]["instructions"]
                .as_str()
                .unwrap()
                .contains(dir.to_string_lossy().as_ref())
        );
        assert!(discover["result"]["instructions"].as_str().unwrap().contains(
            "Every exec_command and apply_patch call must still provide an absolute existing workdir"
        ));

        let listed = json_response(
            post_mcp(
                &client,
                &server.url(),
                Some(TOKEN),
                Some("tools/list"),
                json!({
                    "jsonrpc":"2.0",
                    "id": id + 1,
                    "method":"tools/list",
                    "params":{"_meta": modern_meta(None)}
                }),
            )
            .await,
        )
        .await;
        let mut names = listed["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap().to_string())
            .collect::<Vec<_>>();
        names.sort();
        assert_eq!(names, ["apply_patch", "exec_command", "write_stdin"]);
        assert!(
            listed["result"]["tools"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|tool| matches!(
                    tool["name"].as_str(),
                    Some("exec_command" | "apply_patch")
                ))
                .all(|tool| tool["inputSchema"]["required"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|entry| entry == "workdir"))
        );

        server.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn local_compat_initialize_is_sessionless_but_legacy_tool_traffic_is_not_a_fallback() {
    let dir = tempdir().unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let observer_seen = seen.clone();
    let observer: super::ProviderMetadataObserver = Arc::new(move |metadata| {
        observer_seen.lock().unwrap().push(metadata.clone());
    });
    let server = start_local_mcp_server_with_observer(
        local_service(Config::default()),
        LocalMcpServerConfig::new(dir.path(), TOKEN),
        Some(observer),
    )
    .await
    .unwrap();
    let client = test_http_client();

    let initialize = post_mcp(
        &client,
        &server.url(),
        Some(TOKEN),
        None,
        json!({
            "jsonrpc":"2.0",
            "id":1,
            "method":"initialize",
            "params":{
                "protocolVersion":"2025-06-18",
                "capabilities":{},
                "clientInfo":{"name":"tunnel-probe","version":"1"}
            }
        }),
    )
    .await;
    assert_eq!(initialize.status(), reqwest::StatusCode::OK);
    assert!(initialize.headers().get("Mcp-Session-Id").is_none());
    let initialized: Value = initialize.json().await.unwrap();
    assert_eq!(initialized["result"]["protocolVersion"], "2025-06-18");
    assert!(seen.lock().unwrap().is_empty());

    let legacy_tool = post_mcp(
        &client,
        &server.url(),
        Some(TOKEN),
        None,
        json!({
            "jsonrpc":"2.0",
            "id":2,
            "method":"tools/call",
            "params":{
                "name":"exec_command",
                "arguments":{"cmd":"echo legacy-must-not-run","workdir":dir.path()}
            }
        }),
    )
    .await;
    let status = legacy_tool.status();
    let body = legacy_tool.text().await.unwrap();
    assert!(
        !status.is_success() || body.contains("error"),
        "legacy tool traffic unexpectedly became a Local fallback: {status} {body}"
    );
    assert!(seen.lock().unwrap().is_empty());
    server.shutdown().await.unwrap();
}

#[derive(Default)]
struct InvocationCapture {
    chunks: Mutex<Vec<SessionOutputChunk>>,
}

impl SessionOutputObserver for InvocationCapture {
    fn observe_output(&self, chunk: SessionOutputChunk) {
        self.chunks.lock().unwrap().push(chunk);
    }
}

#[tokio::test]
async fn local_http_exercises_all_three_tools_concurrently_and_preserves_provider_context() {
    let root = tempdir().unwrap();
    let a = root.path().join("a");
    let b = root.path().join("b");
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    let capture = Arc::new(InvocationCapture::default());
    let policy = SessionRuntimePolicy::local("/bin/sh", local_environment())
        .unwrap()
        .with_output_observer(capture.clone());
    let service = ZodexService::with_session_policy(Arc::new(Config::default()), policy);
    let server = start_local_mcp_server(
        service.clone(),
        LocalMcpServerConfig::new(root.path(), TOKEN),
    )
    .await
    .unwrap();
    let client = test_http_client();

    let call = |id, dir: &std::path::Path, marker: &'static str, session: &'static str| {
        let client = client.clone();
        let url = server.url();
        let dir = dir.to_path_buf();
        async move {
            json_response(
                post_mcp(
                    &client,
                    &url,
                    Some(TOKEN),
                    Some("tools/call"),
                    tool_call(
                        id,
                        "exec_command",
                        json!({
                            "cmd": format!("printf '{marker}\\n'; pwd"),
                            "workdir": dir,
                            "yield_time_ms": 2000
                        }),
                        session,
                    ),
                )
                .await,
            )
            .await
        }
    };
    let (out_a, out_b) = tokio::join!(
        call(10, &a, "from-a", "session-a"),
        call(11, &b, "from-b", "session-b")
    );
    let out_a = tool_output(&out_a);
    let out_b = tool_output(&out_b);
    assert_eq!(out_a.status, CommandStatus::Exited);
    assert_eq!(out_b.status, CommandStatus::Exited);
    assert_eq!(
        out_a.cwd,
        std::fs::canonicalize(&a)
            .expect("canonical workdir a")
            .display()
            .to_string()
    );
    assert_eq!(
        out_b.cwd,
        std::fs::canonicalize(&b)
            .expect("canonical workdir b")
            .display()
            .to_string()
    );
    assert!(out_a.output.contains("from-a"));
    assert!(out_b.output.contains("from-b"));

    let patch_path = root.path().join("patched.txt");
    let patched = json_response(
        post_mcp(
            &client,
            &server.url(),
            Some(TOKEN),
            Some("tools/call"),
            tool_call(
                12,
                "apply_patch",
                json!({
                    "patch":"*** Begin Patch\n*** Add File: patched.txt\n+local-mcp-patch\n*** End Patch\n",
                    "workdir":root.path()
                }),
                "session-a",
            ),
        )
        .await,
    )
    .await;
    assert!(
        patched.get("error").is_none(),
        "unexpected patch response: {patched}"
    );
    assert_eq!(
        std::fs::read_to_string(patch_path).unwrap(),
        "local-mcp-patch\n"
    );

    let started = json_response(
        post_mcp(
            &client,
            &server.url(),
            Some(TOKEN),
            Some("tools/call"),
            tool_call(
                13,
                "exec_command",
                json!({"cmd":"sleep 30","workdir":root.path(),"yield_time_ms":50,"timeout_ms":60000}),
                "session-a",
            ),
        )
        .await,
    )
    .await;
    let started = tool_output(&started);
    assert_eq!(started.status, CommandStatus::Running);
    let handle = started.session_handle.unwrap();
    let killed = json_response(
        post_mcp(
            &client,
            &server.url(),
            Some(TOKEN),
            Some("tools/call"),
            tool_call(
                14,
                "write_stdin",
                json!({"session_handle":handle,"kill_process":true,"yield_time_ms":6000}),
                "session-b",
            ),
        )
        .await,
    )
    .await;
    let killed = tool_output(&killed);
    assert_eq!(killed.status, CommandStatus::Exited);
    assert_eq!(killed.termination_reason, Some(TerminationReason::Killed));

    let timed_out = json_response(
        post_mcp(
            &client,
            &server.url(),
            Some(TOKEN),
            Some("tools/call"),
            tool_call(
                15,
                "exec_command",
                json!({"cmd":"sleep 30","workdir":root.path(),"yield_time_ms":2500,"timeout_ms":1000}),
                "session-a",
            ),
        )
        .await,
    )
    .await;
    let timed_out = tool_output(&timed_out);
    assert_eq!(timed_out.status, CommandStatus::Exited);
    assert_eq!(
        timed_out.termination_reason,
        Some(TerminationReason::Timeout)
    );

    {
        let chunks = capture.chunks.lock().unwrap();
        let sessions = chunks
            .iter()
            .filter_map(|chunk| chunk.invocation.provider.as_ref())
            .map(|provider| provider.session_key.to_string())
            .collect::<std::collections::HashSet<_>>();
        assert!(sessions.contains("session-a"));
        assert!(sessions.contains("session-b"));
        assert!(
            chunks
                .iter()
                .filter_map(|chunk| chunk.invocation.correlation_id.as_deref())
                .all(|id| id.len() == 32)
        );
    }

    server.shutdown().await.unwrap();
    service.shutdown_sessions().await.unwrap();
}

#[tokio::test]
async fn local_mcp_history_keeps_exact_bounded_result_and_full_quick_pty_output() {
    let root = tempdir().unwrap();
    let database = root.path().join("history.sqlite3");
    let history = history_runtime(database.clone());
    let service = local_service_with_history(
        Config {
            max_output_chars: 256,
            ..Config::default()
        },
        history.clone(),
    );
    let server = start_local_mcp_server(
        service.clone(),
        LocalMcpServerConfig::new(root.path(), TOKEN).with_invocation_recorder(history.clone()),
    )
    .await
    .unwrap();
    let client = test_http_client();

    let response = json_response(
        post_mcp(
            &client,
            &server.url(),
            Some(TOKEN),
            Some("tools/call"),
            tool_call(
                60,
                "exec_command",
                json!({
                    "cmd":"i=0; while [ \"$i\" -lt 4096 ]; do printf 'abcdefgh'; i=$((i+1)); done; printf '\\nEND-OF-FULL-OUTPUT\\n'",
                    "workdir":root.path(),
                    "yield_time_ms":2000
                }),
                "history-session-a",
            ),
        )
        .await,
    )
    .await;
    let returned = tool_output(&response);
    assert_eq!(returned.status, CommandStatus::Exited);
    assert!(returned.session_handle.is_none());
    assert!(
        returned.output.contains("full output saved to"),
        "{}",
        returned.output
    );
    assert!(returned.output.contains("END-OF-FULL-OUTPUT"));
    assert!(returned.output.len() < 1024);
    let returned_output_file = returned
        .output_file
        .clone()
        .expect("oversized MCP output should expose a spool file");
    assert!(returned.output_chars.is_some_and(|chars| chars > 8_000));
    assert_eq!(returned.output_lines, Some(2));
    assert!(
        std::fs::read_to_string(&returned_output_file)
            .unwrap()
            .contains("END-OF-FULL-OUTPUT")
    );

    server.shutdown().await.unwrap();
    service.shutdown_sessions().await.unwrap();
    shutdown_history_runtime(history).await;

    let records = LocalHistoryReader::query(
        &database,
        &HistoryQuery {
            last: 10,
            include_raw: true,
            ..HistoryQuery::default()
        },
    )
    .unwrap();
    assert_eq!(records.len(), 1);
    let record = &records[0];
    assert_eq!(record.tool_name, "exec_command");
    assert_eq!(record.agent_id.as_deref().map(str::len), Some(4));
    assert_eq!(record.evidence_state, "complete");
    assert_eq!(record.capture_state, "complete");
    assert_eq!(
        record.result.as_ref(),
        Some(&serde_json::to_value(&returned).unwrap())
    );
    let full_output = record.full_output.as_deref().unwrap();
    assert!(
        full_output.len() > 8_000,
        "full output was only {} bytes",
        full_output.len()
    );
    assert!(full_output.contains("abcdefghabcdefgh"));
    assert!(full_output.contains("END-OF-FULL-OUTPUT"));
    assert!(full_output.len() > returned.output.len());
    let preview = record.output_preview.as_deref().unwrap();
    assert!(preview.starts_with("abcdefghabcdefgh"));
    assert_eq!(preview.chars().count(), 16_384);
    assert!(record.output_preview_truncated);
    assert!(!preview.contains("END-OF-FULL-OUTPUT"));

    let compact_detail = LocalHistoryReader::query(
        &database,
        &HistoryQuery {
            invocation_id: Some(record.id),
            ..HistoryQuery::default()
        },
    )
    .unwrap()
    .pop()
    .unwrap();
    assert!(compact_detail.full_output.is_none());
    assert_eq!(compact_detail.output_preview.as_deref(), Some(preview));
    let _ = std::fs::remove_file(returned_output_file);
}

#[tokio::test]
async fn local_mcp_history_records_cross_agent_session_creator_and_unattributed_calls() {
    let root = tempdir().unwrap();
    let database = root.path().join("history.sqlite3");
    let history = history_runtime(database.clone());
    let service = local_service_with_history(Config::default(), history.clone());
    let server = start_local_mcp_server(
        service.clone(),
        LocalMcpServerConfig::new(root.path(), TOKEN).with_invocation_recorder(history.clone()),
    )
    .await
    .unwrap();
    let client = test_http_client();

    let started = json_response(
        post_mcp(
            &client,
            &server.url(),
            Some(TOKEN),
            Some("tools/call"),
            tool_call(
                70,
                "exec_command",
                json!({
                    "cmd":"sleep 30",
                    "workdir":root.path(),
                    "yield_time_ms":50,
                    "timeout_ms":60000
                }),
                "creator-session",
            ),
        )
        .await,
    )
    .await;
    let started = tool_output(&started);
    assert_eq!(started.status, CommandStatus::Running);
    let handle = started.session_handle.clone().unwrap();

    let killed = json_response(
        post_mcp(
            &client,
            &server.url(),
            Some(TOKEN),
            Some("tools/call"),
            tool_call(
                71,
                "write_stdin",
                json!({
                    "session_handle":handle,
                    "kill_process":true,
                    "yield_time_ms":6000
                }),
                "caller-session",
            ),
        )
        .await,
    )
    .await;
    assert_eq!(
        tool_output(&killed).termination_reason,
        Some(TerminationReason::Killed)
    );

    let unattributed = json!({
        "jsonrpc":"2.0",
        "id":72,
        "method":"tools/call",
        "params":{
            "name":"exec_command",
            "arguments":{
                "cmd":"printf 'unattributed-ok\\n'",
                "workdir":root.path(),
                "yield_time_ms":2000
            },
            "_meta":modern_meta(None)
        }
    });
    let unattributed = json_response(
        post_mcp(
            &client,
            &server.url(),
            Some(TOKEN),
            Some("tools/call"),
            unattributed,
        )
        .await,
    )
    .await;
    assert!(
        tool_output(&unattributed)
            .output
            .contains("unattributed-ok")
    );

    let missing_workdir = root.path().join("missing-workdir");
    let rejected = json_response(
        post_mcp(
            &client,
            &server.url(),
            Some(TOKEN),
            Some("tools/call"),
            tool_call(
                73,
                "exec_command",
                json!({
                    "cmd":"printf 'must-not-run\\n'",
                    "workdir":missing_workdir,
                    "yield_time_ms":2000
                }),
                "error-session",
            ),
        )
        .await,
    )
    .await;
    assert!(rejected["result"]["isError"].as_bool().unwrap_or(false));

    server.shutdown().await.unwrap();
    service.shutdown_sessions().await.unwrap();
    shutdown_history_runtime(history).await;

    let records = LocalHistoryReader::query(
        &database,
        &HistoryQuery {
            last: 10,
            include_raw: true,
            ..HistoryQuery::default()
        },
    )
    .unwrap();
    assert_eq!(records.len(), 4);
    let exec = records
        .iter()
        .find(|record| {
            record.tool_name == "exec_command" && record.result_status.as_deref() == Some("running")
        })
        .unwrap();
    let write = records
        .iter()
        .find(|record| record.tool_name == "write_stdin")
        .unwrap();
    let unattributed = records
        .iter()
        .find(|record| record.agent_id.is_none())
        .unwrap();
    assert_ne!(write.agent_id, exec.agent_id);
    assert_eq!(write.target_created_by_agent_id, exec.agent_id);
    assert_eq!(write.target_created_by_invocation_id, Some(exec.id));
    assert_eq!(write.continuation_kind.as_deref(), Some("kill"));
    assert_eq!(write.cross_agent, Some(true));
    assert_eq!(
        write.target_session_handle.as_deref(),
        Some(handle.as_str())
    );
    assert_eq!(write.result_termination_reason.as_deref(), Some("killed"));
    assert_eq!(unattributed.tool_name, "exec_command");
    assert!(unattributed.provider_session_key.is_none());
    let rejected = records
        .iter()
        .find(|record| record.outcome_kind.as_deref() == Some("error"))
        .unwrap();
    assert_eq!(rejected.tool_name, "exec_command");
    assert_eq!(rejected.evidence_state, "complete");
    assert_eq!(rejected.capture_state, "complete");
    assert!(rejected.error.as_deref().unwrap().contains("workdir"));
}

struct RejectingInvocationRecorder;

impl InvocationEvidenceRecorder for RejectingInvocationRecorder {
    fn begin(
        &self,
        _context: InvocationContext,
        _start: InvocationStart,
    ) -> Result<InvocationContext> {
        bail!("injected envelope persistence failure")
    }

    fn complete(&self, _context: &InvocationContext, _outcome: InvocationOutcome) -> Result<()> {
        unreachable!("failed-open invocation recording has no durable ID and skips completion")
    }
}

#[tokio::test]
async fn local_mcp_executes_command_patch_and_stdin_when_history_envelope_fails() {
    let root = tempdir().unwrap();
    let service = local_service(Config::default());
    let direct_shell = service
        .exec_command(crate::protocol::ExecCommandInput {
            cmd: "/bin/sh".to_string(),
            workdir: root.path().display().to_string(),
            yield_time_ms: Some(50),
            timeout_ms: Some(60_000),
        })
        .await
        .unwrap();
    let direct_handle = direct_shell.session_handle.unwrap();
    let server = start_local_mcp_server(
        service.clone(),
        LocalMcpServerConfig::new(root.path(), TOKEN)
            .with_invocation_recorder(Arc::new(RejectingInvocationRecorder)),
    )
    .await
    .unwrap();
    let client = test_http_client();

    let command_marker = root.path().join("command-ran-without-history");
    let command_response = json_response(
        post_mcp(
            &client,
            &server.url(),
            Some(TOKEN),
            Some("tools/call"),
            tool_call(
                80,
                "exec_command",
                json!({
                    "cmd":format!("touch {}", command_marker.display()),
                    "workdir":root.path(),
                    "yield_time_ms":2000
                }),
                "reject-session",
            ),
        )
        .await,
    )
    .await;
    assert!(
        !command_response["result"]["isError"]
            .as_bool()
            .unwrap_or(false)
    );
    assert!(command_marker.exists());

    let patch_response = json_response(
        post_mcp(
            &client,
            &server.url(),
            Some(TOKEN),
            Some("tools/call"),
            tool_call(
                81,
                "apply_patch",
                json!({
                    "patch":"*** Begin Patch\n*** Add File: patch-ran-without-history\n+ok\n*** End Patch\n",
                    "workdir":root.path()
                }),
                "reject-session",
            ),
        )
        .await,
    )
    .await;
    assert!(
        !patch_response["result"]["isError"]
            .as_bool()
            .unwrap_or(false)
    );
    assert!(root.path().join("patch-ran-without-history").exists());

    let stdin_marker = root.path().join("stdin-ran-without-history");
    let stdin_response = json_response(
        post_mcp(
            &client,
            &server.url(),
            Some(TOKEN),
            Some("tools/call"),
            tool_call(
                82,
                "write_stdin",
                json!({
                    "session_handle":direct_handle,
                    "chars":format!("touch {}\n", stdin_marker.display()),
                    "yield_time_ms":500
                }),
                "reject-session",
            ),
        )
        .await,
    )
    .await;
    assert!(
        !stdin_response["result"]["isError"]
            .as_bool()
            .unwrap_or(false)
    );
    assert!(stdin_marker.exists());

    server.shutdown().await.unwrap();
    let _ = service
        .write_stdin(crate::protocol::WriteStdinInput {
            session_handle: direct_handle,
            chars: None,
            yield_time_ms: Some(6_000),
            kill_process: Some(true),
        })
        .await
        .unwrap();
    service.shutdown_sessions().await.unwrap();
}
