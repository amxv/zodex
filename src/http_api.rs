use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::Response;
use axum::routing::post;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::protocol::{
    ApplyPatchInput, ApplyPatchOutput, ExecCommandInput, ToolOutput, WriteStdinInput,
};
use crate::service::{ServiceRequest, ZodexService};
use crate::session::SessionOrigin;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct ErrorOutput {
    error: String,
}

pub fn build_http_api_router(config: Arc<Config>, zodex_service: ZodexService) -> Router {
    Router::new()
        .route("/v1/exec-command", post(exec_command))
        .route("/v1/write-stdin", post(write_stdin))
        .route("/v1/apply-patch", post(apply_patch))
        .with_state(zodex_service)
        .layer(middleware::from_fn_with_state(config, bearer_auth))
}

async fn exec_command(
    State(zodex_service): State<ZodexService>,
    headers: HeaderMap,
    Json(input): Json<ExecCommandInput>,
) -> Result<Json<ToolOutput>, (StatusCode, Json<ErrorOutput>)> {
    let caller_label = caller_label_from_headers(&headers);
    zodex_service
        .execute(ServiceRequest::ExecCommand {
            input,
            origin: SessionOrigin::http(caller_label),
        })
        .await
        .and_then(|response| response.into_tool_output())
        .map(Json)
        .map_err(bad_request)
}

async fn write_stdin(
    State(zodex_service): State<ZodexService>,
    Json(input): Json<WriteStdinInput>,
) -> Result<Json<ToolOutput>, (StatusCode, Json<ErrorOutput>)> {
    zodex_service
        .execute(ServiceRequest::WriteStdin { input })
        .await
        .and_then(|response| response.into_tool_output())
        .map(Json)
        .map_err(bad_request)
}

async fn apply_patch(
    State(zodex_service): State<ZodexService>,
    Json(input): Json<ApplyPatchInput>,
) -> Result<Json<ApplyPatchOutput>, (StatusCode, Json<ErrorOutput>)> {
    zodex_service
        .execute(ServiceRequest::ApplyPatch { input })
        .await
        .and_then(|response| response.into_apply_patch_output())
        .map(Json)
        .map_err(bad_request)
}

fn bad_request(err: anyhow::Error) -> (StatusCode, Json<ErrorOutput>) {
    (
        StatusCode::BAD_REQUEST,
        Json(ErrorOutput {
            error: err.to_string(),
        }),
    )
}

async fn bearer_auth(
    State(config): State<Arc<Config>>,
    request: Request,
    next: Next,
) -> std::result::Result<Response, StatusCode> {
    let supplied = bearer_token(request.headers());
    if supplied == Some(config.api_key.as_str()) {
        return Ok(next.run(request).await);
    }

    Err(StatusCode::UNAUTHORIZED)
}

fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    let raw = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    raw.strip_prefix("Bearer ")
}

fn caller_label_from_headers(headers: &HeaderMap) -> Option<String> {
    let candidate = headers
        .get("x-caller-label")
        .and_then(|v| v.to_str().ok())
        .or_else(|| {
            headers
                .get(header::USER_AGENT)
                .and_then(|v| v.to_str().ok())
        })?;
    let trimmed = candidate.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed.chars().take(128).collect())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::Arc;

    use axum::Router;
    use axum::body::{Body, to_bytes};
    use axum::http::{Request, StatusCode, header};
    use serde::de::DeserializeOwned;
    use serde_json::{Value, json};
    use tempfile::tempdir;
    use tower::util::ServiceExt;

    use crate::config::Config;
    use crate::protocol::{CommandStatus, ExecCommandInput, ToolOutput, WriteStdinInput};
    use crate::service::ZodexService;

    use super::{ApplyPatchOutput, bearer_token, build_http_api_router};

    const TEST_API_KEY: &str = "http-test-key";

    fn test_config() -> Arc<Config> {
        Arc::new(Config {
            api_key: TEST_API_KEY.to_string(),
            ..Config::default()
        })
    }

    fn test_router_with_service(service: ZodexService) -> Router {
        build_http_api_router(test_config(), service)
    }

    async fn post_json(
        app: &Router,
        path: &str,
        body: Value,
        auth_header: Option<&str>,
    ) -> axum::response::Response {
        let mut builder = Request::builder()
            .method("POST")
            .uri(path)
            .header(header::CONTENT_TYPE, "application/json");
        if let Some(auth) = auth_header {
            builder = builder.header(header::AUTHORIZATION, auth);
        }

        app.clone()
            .oneshot(
                builder
                    .body(Body::from(body.to_string()))
                    .expect("request build"),
            )
            .await
            .expect("request should succeed")
    }

    async fn response_json<T: DeserializeOwned>(response: axum::response::Response) -> T {
        let bytes = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body should be readable");
        serde_json::from_slice(&bytes).expect("response should be valid json")
    }

    #[test]
    fn bearer_token_extracts_bearer_scheme() {
        let mut headers = header::HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            "Bearer token-value".parse().expect("header value"),
        );
        assert_eq!(bearer_token(&headers), Some("token-value"));
    }

    #[test]
    fn bearer_token_rejects_missing_or_non_bearer() {
        let mut headers = header::HeaderMap::new();
        assert_eq!(bearer_token(&headers), None);

        headers.insert(
            header::AUTHORIZATION,
            "Basic abc123".parse().expect("header value"),
        );
        assert_eq!(bearer_token(&headers), None);
    }

    #[tokio::test]
    async fn rejects_unauthorized_v1_request() {
        let app = test_router_with_service(ZodexService::new(test_config()));

        let response = post_json(
            &app,
            "/v1/exec-command",
            json!({
                "cmd": "echo denied",
                "yield_time_ms": 500
            }),
            None,
        )
        .await;

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn publish_pr_route_is_not_exposed_on_http_api() {
        let app = test_router_with_service(ZodexService::new(test_config()));
        let auth = format!("Bearer {TEST_API_KEY}");

        let response = post_json(
            &app,
            "/v1/publish-pr",
            json!({
                "repo": "amxv/zodex",
                "title": "should-not-exist",
                "body": "nope",
            }),
            Some(&auth),
        )
        .await;

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn exec_command_http_reports_exited_and_running_states() {
        let app = test_router_with_service(ZodexService::new(test_config()));
        let auth = format!("Bearer {TEST_API_KEY}");

        let exited = post_json(
            &app,
            "/v1/exec-command",
            json!({
                "cmd": "echo http-exit",
                "yield_time_ms": 2_000
            }),
            Some(&auth),
        )
        .await;
        assert_eq!(exited.status(), StatusCode::OK);
        let exited_output: ToolOutput = response_json(exited).await;
        assert_eq!(exited_output.status, CommandStatus::Exited);
        assert!(exited_output.output.contains("http-exit"));
        assert!(exited_output.session_handle.is_none());

        let running = post_json(
            &app,
            "/v1/exec-command",
            json!({
                "cmd": "sleep 5",
                "yield_time_ms": 50
            }),
            Some(&auth),
        )
        .await;
        assert_eq!(running.status(), StatusCode::OK);
        let running_output: ToolOutput = response_json(running).await;
        assert_eq!(running_output.status, CommandStatus::Running);
        let running_session_handle = running_output
            .session_handle
            .expect("session handle should exist");

        let cleanup = post_json(
            &app,
            "/v1/write-stdin",
            json!({
                "session_handle": running_session_handle,
                "kill_process": true,
                "yield_time_ms": 2_000
            }),
            Some(&auth),
        )
        .await;
        assert_eq!(cleanup.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn write_stdin_http_continues_real_session() {
        let app = test_router_with_service(ZodexService::new(test_config()));
        let auth = format!("Bearer {TEST_API_KEY}");

        let started = post_json(
            &app,
            "/v1/exec-command",
            json!({
                "cmd": "bash --noprofile --norc",
                "yield_time_ms": 50,
                "timeout_ms": 60_000
            }),
            Some(&auth),
        )
        .await;
        assert_eq!(started.status(), StatusCode::OK);
        let started_output: ToolOutput = response_json(started).await;
        let session_handle = started_output
            .session_handle
            .expect("stateful shell should remain running");

        let echoed = post_json(
            &app,
            "/v1/write-stdin",
            json!({
                "session_handle": session_handle,
                "chars": "echo http-write\n",
                "yield_time_ms": 500,
                "kill_process": false
            }),
            Some(&auth),
        )
        .await;
        assert_eq!(echoed.status(), StatusCode::OK);
        let echoed_output: ToolOutput = response_json(echoed).await;
        assert_eq!(echoed_output.status, CommandStatus::Running);
        assert!(echoed_output.output.contains("http-write"));

        let done = post_json(
            &app,
            "/v1/write-stdin",
            json!({
                "session_handle": session_handle,
                "chars": "exit\n",
                "yield_time_ms": 2_000,
                "kill_process": false
            }),
            Some(&auth),
        )
        .await;
        assert_eq!(done.status(), StatusCode::OK);
        let done_output: ToolOutput = response_json(done).await;
        assert_eq!(done_output.status, CommandStatus::Exited);
        assert!(done_output.session_handle.is_none());
    }

    #[tokio::test]
    async fn apply_patch_http_preserves_relative_path_semantics() {
        let app = test_router_with_service(ZodexService::new(test_config()));
        let auth = format!("Bearer {TEST_API_KEY}");
        let dir = tempdir().expect("tempdir");
        let patch =
            "*** Begin Patch\n*** Add File: nested/http-test.txt\n+hello-http\n*** End Patch\n";

        let response = post_json(
            &app,
            "/v1/apply-patch",
            json!({
                "patch": patch,
                "workdir": dir.path()
            }),
            Some(&auth),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body: ApplyPatchOutput = response_json(response).await;

        let expected = dir.path().join("nested/http-test.txt");
        assert!(
            body.output
                .contains("Success. Updated the following files:")
        );
        assert!(body.output.contains(&format!("A {}", expected.display())));
        assert_eq!(
            fs::read_to_string(expected).expect("file should be created from relative path"),
            "hello-http\n"
        );
    }

    #[tokio::test]
    async fn exec_command_http_parity_with_service() {
        let direct = ZodexService::new(test_config());
        let app = test_router_with_service(ZodexService::new(test_config()));
        let auth = format!("Bearer {TEST_API_KEY}");
        let input = ExecCommandInput {
            cmd: "printf 'http-parity-exec\\n'".to_string(),
            yield_time_ms: Some(2_000),
            workdir: None,
            timeout_ms: None,
        };

        let direct_output = direct
            .exec_command(input.clone())
            .await
            .expect("direct exec should succeed");
        let http = post_json(
            &app,
            "/v1/exec-command",
            serde_json::to_value(&input).expect("input should serialize"),
            Some(&auth),
        )
        .await;
        assert_eq!(http.status(), StatusCode::OK);
        let http_output: ToolOutput = response_json(http).await;

        assert_eq!(http_output.status, direct_output.status);
        assert_eq!(http_output.exit_code, direct_output.exit_code);
        assert_eq!(
            http_output.termination_reason,
            direct_output.termination_reason
        );
        assert!(http_output.output.contains("http-parity-exec"));
        assert!(direct_output.output.contains("http-parity-exec"));
    }

    #[tokio::test]
    async fn write_stdin_http_parity_with_service() {
        let direct = ZodexService::new(test_config());
        let app = test_router_with_service(ZodexService::new(test_config()));
        let auth = format!("Bearer {TEST_API_KEY}");
        let shell = ExecCommandInput {
            cmd: "bash --noprofile --norc".to_string(),
            yield_time_ms: Some(50),
            workdir: None,
            timeout_ms: Some(60_000),
        };

        let direct_started = direct
            .exec_command(shell.clone())
            .await
            .expect("direct shell should start");
        let direct_handle = direct_started
            .session_handle
            .expect("direct session handle");

        let http_started = post_json(
            &app,
            "/v1/exec-command",
            serde_json::to_value(&shell).expect("input should serialize"),
            Some(&auth),
        )
        .await;
        assert_eq!(http_started.status(), StatusCode::OK);
        let http_started_output: ToolOutput = response_json(http_started).await;
        let http_handle = http_started_output
            .session_handle
            .expect("http session handle");

        let direct_write = direct
            .write_stdin(WriteStdinInput {
                session_handle: direct_handle.clone(),
                chars: Some("echo http-parity-write\n".to_string()),
                yield_time_ms: Some(500),
                kill_process: Some(false),
            })
            .await
            .expect("direct write should succeed");
        let http_write = post_json(
            &app,
            "/v1/write-stdin",
            json!({
                "session_handle": http_handle.clone(),
                "chars": "echo http-parity-write\n",
                "yield_time_ms": 500,
                "kill_process": false
            }),
            Some(&auth),
        )
        .await;
        assert_eq!(http_write.status(), StatusCode::OK);
        let http_write_output: ToolOutput = response_json(http_write).await;

        assert_eq!(http_write_output.status, direct_write.status);
        assert_eq!(
            http_write_output.termination_reason,
            direct_write.termination_reason
        );
        assert!(http_write_output.output.contains("http-parity-write"));
        assert!(direct_write.output.contains("http-parity-write"));

        let _ = direct
            .write_stdin(WriteStdinInput {
                session_handle: direct_handle,
                chars: Some("exit\n".to_string()),
                yield_time_ms: Some(2_000),
                kill_process: Some(false),
            })
            .await
            .expect("direct shell should exit");
        let http_cleanup = post_json(
            &app,
            "/v1/write-stdin",
            json!({
                "session_handle": http_handle,
                "chars": "exit\n",
                "yield_time_ms": 2_000,
                "kill_process": false
            }),
            Some(&auth),
        )
        .await;
        assert_eq!(http_cleanup.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn apply_patch_http_parity_with_service() {
        let direct = ZodexService::new(test_config());
        let app = test_router_with_service(ZodexService::new(test_config()));
        let auth = format!("Bearer {TEST_API_KEY}");
        let direct_dir = tempdir().expect("direct tempdir");
        let http_dir = tempdir().expect("http tempdir");
        let patch = "*** Begin Patch\n*** Add File: parity-http.txt\n+http-patch\n*** End Patch\n";

        let direct_output = direct
            .apply_patch(crate::protocol::ApplyPatchInput {
                patch: patch.to_string(),
                workdir: direct_dir.path().to_string_lossy().to_string(),
            })
            .expect("direct patch should succeed");
        let http = post_json(
            &app,
            "/v1/apply-patch",
            json!({
                "patch": patch,
                "workdir": http_dir.path()
            }),
            Some(&auth),
        )
        .await;
        assert_eq!(http.status(), StatusCode::OK);
        let http_output: ApplyPatchOutput = response_json(http).await;

        assert!(direct_output.contains("Success. Updated the following files:"));
        assert!(
            http_output
                .output
                .contains("Success. Updated the following files:")
        );
        assert_eq!(
            fs::read_to_string(direct_dir.path().join("parity-http.txt"))
                .expect("read direct file"),
            "http-patch\n"
        );
        assert_eq!(
            fs::read_to_string(http_dir.path().join("parity-http.txt")).expect("read http file"),
            "http-patch\n"
        );
    }
}
