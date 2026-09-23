use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::{Body, BodyDataStream, to_bytes};
use axum::http::{HeaderValue, Method, Request, StatusCode, header};
use serde_json::{Value, json};
use tempfile::tempdir;
use tokio_stream::StreamExt as _;
use tower::ServiceExt as _;

use crate::invocation::{
    InvocationContext, InvocationEvidenceRecorder, InvocationOutcome, InvocationStart,
    ProviderCallMetadata,
};
use crate::local::{
    HistoryQuery, LocalHistoryReader, LocalHistoryRuntime, LocalHistoryRuntimeConfig, LocalPaths,
    ensure_observability_bearer,
};
use crate::protocol::TerminationReason;
use crate::session::{
    OwnedProcess, OwnedProcessEnd, OwnedProcessObserver, ProcessBirthIdentity, ProcessIdentity,
    SessionOutputChunk, SessionOutputCompletion, SessionOutputObserver,
};

use super::server::{build_router, start_local_observability_server};

const TOKEN: &str = "observability-test-token-0123456789abcdef";

fn open_history(path: &std::path::Path, runtime_id: &str) -> Arc<LocalHistoryRuntime> {
    LocalHistoryRuntime::open(LocalHistoryRuntimeConfig::new(
        path.to_path_buf(),
        runtime_id,
        60 * 60,
        64 * 1024 * 1024,
    ))
    .unwrap()
}

fn provider_context(session_key: &str) -> InvocationContext {
    InvocationContext::default().with_provider(ProviderCallMetadata::new("openai", session_key))
}

fn begin(
    history: &LocalHistoryRuntime,
    context: InvocationContext,
    tool_name: &str,
    workdir: Option<&std::path::Path>,
) -> InvocationContext {
    let mut arguments = json!({"marker": tool_name});
    if let Some(workdir) = workdir {
        arguments["workdir"] = Value::String(workdir.display().to_string());
    }
    history
        .begin(context, InvocationStart::new(tool_name, arguments))
        .unwrap()
}

fn complete(history: &LocalHistoryRuntime, context: &InvocationContext) {
    history
        .complete(context, InvocationOutcome::Success(json!({"ok": true})))
        .unwrap();
}

fn router(history: Arc<LocalHistoryRuntime>) -> Router {
    build_router(history, HeaderValue::from_static(TOKEN))
}

async fn request(
    app: &Router,
    method: Method,
    path: &str,
    token: Option<&str>,
) -> axum::response::Response {
    let mut builder = Request::builder().method(method).uri(path);
    if let Some(token) = token {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    app.clone()
        .oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap()
}

async fn body_json(response: axum::response::Response) -> Value {
    let bytes = to_bytes(response.into_body(), 2 * 1024 * 1024)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

async fn next_sse_frame(stream: &mut BodyDataStream) -> String {
    let frame = tokio::time::timeout(Duration::from_secs(2), stream.next())
        .await
        .expect("timed out waiting for SSE frame")
        .expect("SSE stream ended unexpectedly")
        .expect("failed to read SSE frame");
    String::from_utf8(frame.to_vec()).unwrap()
}

async fn next_sse_frame_containing(stream: &mut BodyDataStream, needle: &str) -> String {
    for _ in 0..8 {
        let frame = next_sse_frame(stream).await;
        if frame.contains(needle) {
            return frame;
        }
    }
    panic!("SSE stream did not produce frame containing `{needle}`");
}

fn wait_for_invocation(history: &LocalHistoryRuntime, invocation_id: i64) {
    history.flush_for_test().unwrap();
    let records = LocalHistoryReader::query(
        history.database_path(),
        &HistoryQuery {
            last: 1,
            invocation_id: Some(invocation_id),
            ..HistoryQuery::default()
        },
    )
    .unwrap();
    assert!(
        records
            .first()
            .is_some_and(|record| record.completed_at_ms.is_some()),
        "invocation {invocation_id} did not complete durably before the history queue barrier"
    );
}

#[tokio::test]
async fn api_is_loopback_bearer_read_only_agent_aware_and_secret_free() {
    let dir = tempdir().unwrap();
    let database = dir.path().join("history.sqlite3");
    let old_workdir = dir.path().join("old");
    let first_workdir = dir.path().join("current-one");
    let second_workdir = dir.path().join("current-two");
    for workdir in [&old_workdir, &first_workdir, &second_workdir] {
        std::fs::create_dir_all(workdir).unwrap();
    }

    let old = open_history(&database, "runtime-old");
    let old_context = begin(
        &old,
        provider_context("provider-secret-old"),
        "test_tool",
        Some(&old_workdir),
    );
    complete(&old, &old_context);
    wait_for_invocation(&old, old_context.invocation_id.unwrap());
    old.shutdown_blocking().unwrap();

    let history = open_history(&database, "runtime-current");
    let first = begin(
        &history,
        provider_context("provider-secret-current"),
        "test_tool",
        Some(&first_workdir),
    );
    complete(&history, &first);
    let second = begin(
        &history,
        provider_context("provider-secret-current"),
        "test_tool",
        Some(&second_workdir),
    );
    complete(&history, &second);
    let unattributed = begin(
        &history,
        InvocationContext::default(),
        "test_tool",
        Some(&first_workdir),
    );
    complete(&history, &unattributed);
    wait_for_invocation(&history, second.invocation_id.unwrap());

    let process = OwnedProcess {
        internal_session_id: 42,
        session_handle: Arc::from("observability-process"),
        identity: ProcessIdentity {
            pid: 4242,
            birth: ProcessBirthIdentity::LinuxProcStartTicks { ticks: 7 },
        },
        created_by: second.clone(),
    };
    history.process_started(&process).unwrap();
    let unattributed_process = OwnedProcess {
        internal_session_id: 43,
        session_handle: Arc::from("observability-unattributed-process"),
        identity: ProcessIdentity {
            pid: 4243,
            birth: ProcessBirthIdentity::LinuxProcStartTicks { ticks: 8 },
        },
        created_by: unattributed.clone(),
    };
    history.process_started(&unattributed_process).unwrap();

    let server = start_local_observability_server(history.clone(), TOKEN)
        .await
        .unwrap();
    assert!(server.addr().ip().is_loopback());
    server.shutdown().await.unwrap();

    let app = router(history.clone());
    let unauthorized = request(&app, Method::GET, "/v1/status", None).await;
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        unauthorized.headers().get(header::CACHE_CONTROL).unwrap(),
        "no-store"
    );

    let status = request(&app, Method::GET, "/v1/status", Some(TOKEN)).await;
    assert_eq!(status.status(), StatusCode::OK);
    assert_eq!(
        status.headers().get(header::CACHE_CONTROL).unwrap(),
        "no-store"
    );
    assert!(
        status
            .headers()
            .get("access-control-allow-origin")
            .is_none()
    );
    let status = body_json(status).await;
    assert_eq!(status["runtime_id"], "runtime-current");
    assert_eq!(status["current_runtime_agent_count"], 1);
    assert_eq!(status["active_process_count"], 2);

    let post = request(&app, Method::POST, "/v1/status", Some(TOKEN)).await;
    assert_eq!(post.status(), StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(
        post.headers().get(header::CACHE_CONTROL).unwrap(),
        "no-store"
    );

    let hidden_exec = request(&app, Method::POST, "/v1/exec-command", Some(TOKEN)).await;
    assert_eq!(hidden_exec.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        hidden_exec.headers().get(header::CACHE_CONTROL).unwrap(),
        "no-store"
    );

    let current_agents_response =
        request(&app, Method::GET, "/v1/agents?runtime=current", Some(TOKEN)).await;
    assert_eq!(
        current_agents_response
            .headers()
            .get(header::CACHE_CONTROL)
            .unwrap(),
        "no-store"
    );
    let current_agents = body_json(current_agents_response).await;
    assert_eq!(current_agents["agents"].as_array().unwrap().len(), 1);
    let current_agent = &current_agents["agents"][0];
    let agent_id = first.agent_id.as_deref().unwrap();
    assert_eq!(current_agent["id"], agent_id);
    assert_eq!(current_agent["seen_in_current_runtime"], true);
    assert_eq!(current_agent["active_process_count"], 1);
    assert_eq!(current_agent["workdirs"].as_array().unwrap().len(), 2);
    assert_eq!(current_agent["workdirs"][0]["ordinal"], 1);
    assert_eq!(current_agent["workdirs"][1]["ordinal"], 2);

    let agent_detail = request(
        &app,
        Method::GET,
        &format!("/v1/agents/{agent_id}"),
        Some(TOKEN),
    )
    .await;
    assert_eq!(agent_detail.status(), StatusCode::OK);
    assert_eq!(
        agent_detail.headers().get(header::CACHE_CONTROL).unwrap(),
        "no-store"
    );
    let agent_detail = body_json(agent_detail).await;
    assert_eq!(agent_detail["agent"]["id"], agent_id);
    assert!(agent_detail["agent"].get("expires_at").is_none());

    let all_agents_response = request(&app, Method::GET, "/v1/agents", Some(TOKEN)).await;
    assert_eq!(
        all_agents_response
            .headers()
            .get(header::CACHE_CONTROL)
            .unwrap(),
        "no-store"
    );
    let all_agents_bytes = to_bytes(all_agents_response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&all_agents_bytes).unwrap()["agents"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let all_agents_text = String::from_utf8(all_agents_bytes.to_vec()).unwrap();
    assert!(!all_agents_text.contains("provider-secret-old"));
    assert!(!all_agents_text.contains("provider-secret-current"));

    let filtered_response = request(
        &app,
        Method::GET,
        &format!("/v1/invocations?agent_id={agent_id}&last=20"),
        Some(TOKEN),
    )
    .await;
    assert_eq!(
        filtered_response
            .headers()
            .get(header::CACHE_CONTROL)
            .unwrap(),
        "no-store"
    );
    let filtered = body_json(filtered_response).await;
    assert_eq!(filtered["invocations"].as_array().unwrap().len(), 2);
    assert!(
        filtered["invocations"]
            .as_array()
            .unwrap()
            .iter()
            .all(|record| { record["agent_id"].as_str() == Some(agent_id) })
    );
    assert!(!filtered.to_string().contains("provider-secret-current"));

    let global =
        body_json(request(&app, Method::GET, "/v1/invocations?last=20", Some(TOKEN)).await).await;
    assert!(
        global["invocations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|record| { record["agent_id"].is_null() })
    );

    let workdir_filtered = body_json(
        request(
            &app,
            Method::GET,
            &format!(
                "/v1/invocations?workdir={}&last=20",
                second_workdir.display()
            ),
            Some(TOKEN),
        )
        .await,
    )
    .await;
    assert_eq!(workdir_filtered["invocations"].as_array().unwrap().len(), 1);
    assert_eq!(
        workdir_filtered["invocations"][0]["id"],
        second.invocation_id.unwrap()
    );

    let wrong_bearer = request(&app, Method::GET, "/v1/status", Some("wrong-token")).await;
    assert_eq!(wrong_bearer.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        wrong_bearer.headers().get(header::CACHE_CONTROL).unwrap(),
        "no-store"
    );

    let end = OwnedProcessEnd::exited(
        0,
        TerminationReason::Exit,
        second_workdir.display().to_string(),
    );
    history.process_ended(&process, &end).unwrap();
    history.process_ended(&unattributed_process, &end).unwrap();
    drop(app);
    history.shutdown_blocking().unwrap();
}

#[tokio::test]
async fn actual_listener_accepts_auto_managed_bearer_without_exposing_it() {
    crate::install_rustls_crypto_provider();
    let dir = tempdir().unwrap();
    let paths = LocalPaths::from_roots(
        dir.path().join("config"),
        dir.path().join("data"),
        dir.path().join("state"),
    )
    .unwrap();
    paths.ensure_persistent_dirs().unwrap();
    assert!(ensure_observability_bearer(&paths, false).unwrap());
    let bearer = std::fs::read_to_string(paths.observability_bearer_file()).unwrap();
    let history = open_history(&paths.history_database(), "runtime-managed-bearer");
    let server = start_local_observability_server(history.clone(), bearer.trim())
        .await
        .unwrap();

    let client = reqwest::Client::new();
    let response = client
        .get(format!("{}/v1/status", server.base_url()))
        .bearer_auth(bearer.trim())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    let body = response.text().await.unwrap();
    assert!(!body.contains(bearer.trim()));

    let live_sse = client
        .get(format!("{}/v1/events", server.base_url()))
        .bearer_auth(bearer.trim())
        .send()
        .await
        .unwrap();
    assert_eq!(live_sse.status(), reqwest::StatusCode::OK);
    tokio::time::timeout(Duration::from_secs(1), server.shutdown())
        .await
        .expect("an attached SSE observer must not delay runtime shutdown")
        .unwrap();
    drop(live_sse);
    history.shutdown_blocking().unwrap();
}

#[tokio::test]
async fn output_is_cursor_paginated_exact_and_detail_stays_bounded() {
    let dir = tempdir().unwrap();
    let database = dir.path().join("history.sqlite3");
    let workdir = dir.path().join("repo");
    std::fs::create_dir_all(&workdir).unwrap();
    let history = open_history(&database, "runtime-output");
    let context = history
        .begin(
            provider_context("provider-output-secret"),
            InvocationStart::new(
                "exec_command",
                json!({"cmd":"large-output","workdir":workdir.display().to_string()}),
            ),
        )
        .unwrap();
    let invocation_id = context.invocation_id.unwrap();
    let mut expected = String::new();
    for sequence in 0..48_u64 {
        let mut text = format!("chunk-{sequence:03}-{}", "x".repeat(1010));
        if sequence == 47 {
            text.push_str("END-OF-PAGINATED-EVIDENCE");
        }
        expected.push_str(&text);
        history.observe_output(SessionOutputChunk {
            internal_session_id: 1,
            session_handle: Arc::from("output-handle"),
            invocation: context.clone(),
            sequence,
            text,
        });
    }
    history.observe_output_complete(SessionOutputCompletion {
        internal_session_id: 1,
        session_handle: Arc::from("output-handle"),
        invocation: context.clone(),
    });
    history
        .complete(
            &context,
            InvocationOutcome::Success(json!({
                "status":"exited",
                "output":"bounded-handler-result"
            })),
        )
        .unwrap();
    wait_for_invocation(&history, invocation_id);
    for _ in 0..100 {
        if LocalHistoryReader::output_metadata(history.database_path(), invocation_id)
            .unwrap()
            .is_some_and(|metadata| metadata.chunk_count == 48)
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let app = router(history.clone());
    let detail_response = request(
        &app,
        Method::GET,
        &format!("/v1/invocations/{invocation_id}"),
        Some(TOKEN),
    )
    .await;
    assert_eq!(detail_response.status(), StatusCode::OK);
    assert_eq!(
        detail_response
            .headers()
            .get(header::CACHE_CONTROL)
            .unwrap(),
        "no-store"
    );
    let detail_bytes = to_bytes(detail_response.into_body(), 128 * 1024)
        .await
        .unwrap();
    assert!(detail_bytes.len() < 64 * 1024);
    let detail: Value = serde_json::from_slice(&detail_bytes).unwrap();
    assert_eq!(detail["output"]["chunk_count"], 48);
    assert!(detail["output"]["size_bytes"].as_u64().unwrap() > 40 * 1024);
    assert!(!String::from_utf8_lossy(&detail_bytes).contains("END-OF-PAGINATED-EVIDENCE"));
    assert!(!String::from_utf8_lossy(&detail_bytes).contains("provider-output-secret"));

    let mut reconstructed = String::new();
    let mut cursor = 0_u64;
    loop {
        let page_response = request(
            &app,
            Method::GET,
            &format!("/v1/invocations/{invocation_id}/output?cursor={cursor}&limit=7"),
            Some(TOKEN),
        )
        .await;
        assert_eq!(
            page_response.headers().get(header::CACHE_CONTROL).unwrap(),
            "no-store"
        );
        let page = body_json(page_response).await;
        for chunk in page["chunks"].as_array().unwrap() {
            reconstructed.push_str(chunk["text"].as_str().unwrap());
        }
        match page["next_cursor"].as_u64() {
            Some(next) => cursor = next,
            None => break,
        }
    }
    assert_eq!(reconstructed, expected);

    let too_large = request(
        &app,
        Method::GET,
        &format!("/v1/invocations/{invocation_id}/output?limit=65"),
        Some(TOKEN),
    )
    .await;
    assert_eq!(too_large.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        too_large.headers().get(header::CACHE_CONTROL).unwrap(),
        "no-store"
    );

    drop(app);
    history.shutdown_blocking().unwrap();
}

#[tokio::test]
async fn sse_starts_now_filters_live_events_and_surfaces_recoverable_lag() {
    let dir = tempdir().unwrap();
    let database = dir.path().join("history.sqlite3");
    let workdir = dir.path().join("repo");
    std::fs::create_dir_all(&workdir).unwrap();
    let history = LocalHistoryRuntime::open(
        LocalHistoryRuntimeConfig::new(database, "runtime-events", 60 * 60, 64 * 1024 * 1024)
            .with_event_capacity(16),
    )
    .unwrap();
    assert_eq!(history.live_event_subscriber_count(), 0);
    assert_eq!(history.live_event_sequence(), 0);

    let command = history
        .begin(
            provider_context("target-provider"),
            InvocationStart::new(
                "exec_command",
                json!({"cmd":"mid-command","workdir":workdir.display().to_string()}),
            ),
        )
        .unwrap();
    assert_eq!(
        history.live_event_sequence(),
        0,
        "live events must not be constructed when there are no subscribers"
    );
    let invocation_id = command.invocation_id.unwrap();
    let agent_id = command.agent_id.as_deref().unwrap().to_string();

    let app = router(history.clone());
    let response = request(
        &app,
        Method::GET,
        &format!("/v1/events?agent_id={agent_id}&diffs=full"),
        Some(TOKEN),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get(header::CACHE_CONTROL).unwrap(),
        "no-store"
    );
    let mut stream = response.into_body().into_data_stream();
    assert_eq!(history.live_event_subscriber_count(), 1);
    assert!(
        tokio::time::timeout(Duration::from_millis(100), stream.next())
            .await
            .is_err(),
        "SSE must not preload pre-subscription history"
    );

    let lifecycle_response = request(&app, Method::GET, "/v1/events", Some(TOKEN)).await;
    let mut lifecycle_stream = lifecycle_response.into_body().into_data_stream();
    let lifecycle = begin(
        &history,
        provider_context("lifecycle-provider"),
        "test_tool",
        Some(&workdir),
    );
    for expected in [
        "event: agent_first_seen",
        "event: agent_workdir_added",
        "event: invocation_started",
    ] {
        let text = next_sse_frame(&mut lifecycle_stream).await;
        assert!(text.contains(expected), "{text}");
        assert!(
            text.contains(lifecycle.agent_id.as_deref().unwrap()),
            "{text}"
        );
    }
    drop(lifecycle_stream);
    complete(&history, &lifecycle);
    wait_for_invocation(&history, lifecycle.invocation_id.unwrap());

    let other = begin(
        &history,
        provider_context("other-provider"),
        "test_tool",
        Some(&workdir),
    );
    complete(&history, &other);
    assert!(
        tokio::time::timeout(Duration::from_millis(100), stream.next())
            .await
            .is_err(),
        "Agent filter must suppress unrelated live events"
    );

    history.observe_output(SessionOutputChunk {
        internal_session_id: 7,
        session_handle: Arc::from("mid-command-handle"),
        invocation: command.clone(),
        sequence: 0,
        text: "later-output\u{1b}[31mred\u{1b}[0m".to_string(),
    });
    let output_text = next_sse_frame_containing(&mut stream, "event: output").await;
    assert!(output_text.contains("event: output"), "{output_text}");
    assert!(output_text.contains(&format!("\"invocation_id\":{invocation_id}")));
    assert!(!output_text.contains('\u{1b}'));

    history.observe_output_complete(SessionOutputCompletion {
        internal_session_id: 7,
        session_handle: Arc::from("mid-command-handle"),
        invocation: command.clone(),
    });
    history
        .complete(
            &command,
            InvocationOutcome::Success(json!({"status":"exited"})),
        )
        .unwrap();
    // Invocation completion is a priority control event and may intentionally
    // beat PTY EOF. Capture both in either order so this test enforces delivery
    // without requiring terminal card state to wait for output draining.
    let mut output_complete = None;
    let mut completed = None;
    for _ in 0..8 {
        let frame = next_sse_frame(&mut stream).await;
        if frame.contains("event: output_complete") {
            output_complete = Some(frame);
        } else if frame.contains("event: invocation_completed") {
            completed = Some(frame);
        }
        if output_complete.is_some() && completed.is_some() {
            break;
        }
    }
    let output_complete = output_complete.expect("SSE stream omitted output_complete");
    assert!(
        output_complete.contains(&format!("\"invocation_id\":{invocation_id}")),
        "{output_complete}"
    );
    let completed = completed.expect("SSE stream omitted invocation_completed");
    assert!(
        completed.contains(&format!("\"invocation_id\":{invocation_id}")),
        "{completed}"
    );
    let presentation = next_sse_frame_containing(&mut stream, "event: presentation_updated").await;
    assert!(presentation.contains(&format!(
        "\"presentation_revision\":{}",
        crate::local::PRESENTATION_SCHEMA_VERSION
    )));
    wait_for_invocation(&history, invocation_id);

    let detail = request(
        &app,
        Method::GET,
        &format!("/v1/invocations/{invocation_id}"),
        Some(TOKEN),
    )
    .await;
    assert_eq!(detail.status(), StatusCode::OK);
    assert_eq!(
        detail.headers().get(header::CACHE_CONTROL).unwrap(),
        "no-store"
    );

    let stdin = history
        .begin(
            provider_context("target-provider"),
            InvocationStart::new(
                "write_stdin",
                json!({
                    "session_handle":"mid-command-handle",
                    "chars":"continue\n",
                    "kill_process":false
                }),
            ),
        )
        .unwrap();
    let stdin_id = stdin.invocation_id.unwrap();
    let stdin_started =
        next_sse_frame_containing(&mut stream, "\"tool_name\":\"write_stdin\"").await;
    assert!(
        stdin_started.contains(&format!("\"invocation_id\":{stdin_id}")),
        "{stdin_started}"
    );
    history
        .complete(
            &stdin,
            InvocationOutcome::Success(json!({
                "status":"running",
                "session_handle":"mid-command-handle"
            })),
        )
        .unwrap();
    let stdin_updated = next_sse_frame_containing(&mut stream, "event: presentation_updated").await;
    assert!(
        stdin_updated.contains(&format!("\"invocation_id\":{stdin_id}")),
        "{stdin_updated}"
    );
    wait_for_invocation(&history, stdin_id);

    let patch_path = workdir.join("patch-target.txt");
    std::fs::write(&patch_path, "before\n").unwrap();
    let summary_response = request(
        &app,
        Method::GET,
        &format!("/v1/events?agent_id={agent_id}&diffs=summary"),
        Some(TOKEN),
    )
    .await;
    let mut summary_stream = summary_response.into_body().into_data_stream();
    let patch = history
        .begin(
            provider_context("target-provider"),
            InvocationStart::new(
                "apply_patch",
                json!({
                    "workdir":workdir.display().to_string(),
                    "patch":"*** Begin Patch\n*** Update File: patch-target.txt\n@@\n-before\n+after\n*** End Patch\n"
                }),
            ),
        )
        .unwrap();
    let patch_id = patch.invocation_id.unwrap();
    let patch_started =
        next_sse_frame_containing(&mut stream, "\"tool_name\":\"apply_patch\"").await;
    let summary_patch_started =
        next_sse_frame_containing(&mut summary_stream, "\"tool_name\":\"apply_patch\"").await;
    assert!(
        patch_started.contains(&format!("\"invocation_id\":{patch_id}")),
        "{patch_started}"
    );
    assert!(summary_patch_started.contains(&format!("\"invocation_id\":{patch_id}")));
    std::fs::write(&patch_path, "after\n").unwrap();
    history
        .complete(
            &patch,
            InvocationOutcome::Success(json!({"status":"exited"})),
        )
        .unwrap();
    let patch_updated = next_sse_frame_containing(&mut stream, "\"kind\":\"file_changes\"").await;
    let summary_patch_updated =
        next_sse_frame_containing(&mut summary_stream, "\"kind\":\"file_changes\"").await;
    assert!(
        patch_updated.contains(&format!("\"invocation_id\":{patch_id}")),
        "{patch_updated}"
    );
    assert!(patch_updated.contains("\"diff_lines_included\":true"));
    assert!(patch_updated.contains("\"text\":\"after\""));
    assert!(summary_patch_updated.contains("\"diff_lines_included\":false"));
    assert!(!summary_patch_updated.contains("\"text\":\"after\""));
    wait_for_invocation(&history, patch_id);
    let patch_detail = body_json(
        request(
            &app,
            Method::GET,
            &format!("/v1/invocations/{patch_id}"),
            Some(TOKEN),
        )
        .await,
    )
    .await;
    assert_eq!(
        patch_detail["presentation"]["records"][0]["kind"],
        "file_changes"
    );
    assert_eq!(
        patch_detail["presentation"]["records"][0]["changes"][0]["operation"],
        "edited"
    );

    let lag_response = request(&app, Method::GET, "/v1/events", Some(TOKEN)).await;
    let mut lag_stream = lag_response.into_body().into_data_stream();
    let lag_start_sequence = history.live_event_sequence();
    for index in 0..32 {
        let context = begin(
            &history,
            InvocationContext::default(),
            &format!("lag-{index}"),
            None,
        );
        complete(&history, &context);
        history.flush_for_test().unwrap();
    }
    let prompt_started = Instant::now();
    let prompt = begin(
        &history,
        InvocationContext::default(),
        "lag-backpressure-proof",
        None,
    );
    complete(&history, &prompt);
    wait_for_invocation(&history, prompt.invocation_id.unwrap());
    assert!(
        prompt_started.elapsed() < Duration::from_secs(1),
        "a stalled SSE subscriber must not add one second of execution/history latency"
    );
    let gap_text = next_sse_frame(&mut lag_stream).await;
    assert!(gap_text.contains("event: gap"), "{gap_text}");
    assert!(gap_text.contains("skipped_events"), "{gap_text}");
    assert!(gap_text.contains("durable_history_or_invocation_detail"));
    let gap_id = gap_text
        .lines()
        .find_map(|line| line.strip_prefix("id: "))
        .unwrap()
        .parse::<u64>()
        .unwrap();
    assert!(gap_id > lag_start_sequence.saturating_add(1));
    let recovered = request(
        &app,
        Method::GET,
        &format!("/v1/invocations/{}", prompt.invocation_id.unwrap()),
        Some(TOKEN),
    )
    .await;
    assert_eq!(recovered.status(), StatusCode::OK);

    drop(stream);
    drop(summary_stream);
    drop(lag_stream);
    drop(app);
    assert_eq!(history.live_event_subscriber_count(), 0);
    history.shutdown_blocking().unwrap();
}

#[tokio::test]
async fn two_agent_sse_filters_are_independent_and_global_stream_keeps_unattributed_activity() {
    let dir = tempdir().unwrap();
    let database = dir.path().join("history.sqlite3");
    let workdir = dir.path().join("repo");
    std::fs::create_dir_all(&workdir).unwrap();
    let history = open_history(&database, "runtime-filters");

    let seed_a = begin(
        &history,
        provider_context("provider-a"),
        "seed",
        Some(&workdir),
    );
    let seed_b = begin(
        &history,
        provider_context("provider-b"),
        "seed",
        Some(&workdir),
    );
    complete(&history, &seed_a);
    complete(&history, &seed_b);
    wait_for_invocation(&history, seed_b.invocation_id.unwrap());
    let agent_a = seed_a.agent_id.as_deref().unwrap().to_string();
    let agent_b = seed_b.agent_id.as_deref().unwrap().to_string();
    assert_ne!(agent_a, agent_b);

    let app = router(history.clone());
    let mut stream_a = request(
        &app,
        Method::GET,
        &format!("/v1/events?agent_id={agent_a}"),
        Some(TOKEN),
    )
    .await
    .into_body()
    .into_data_stream();
    let mut stream_b = request(
        &app,
        Method::GET,
        &format!("/v1/events?agent_id={agent_b}"),
        Some(TOKEN),
    )
    .await
    .into_body()
    .into_data_stream();
    let mut global = request(&app, Method::GET, "/v1/events", Some(TOKEN))
        .await
        .into_body()
        .into_data_stream();

    let a = begin(
        &history,
        provider_context("provider-a"),
        "agent-a-only",
        Some(&workdir),
    );
    let b = begin(
        &history,
        provider_context("provider-b"),
        "agent-b-only",
        Some(&workdir),
    );
    let unattributed = begin(
        &history,
        InvocationContext::default(),
        "unattributed-live",
        None,
    );

    let a_frame = next_sse_frame(&mut stream_a).await;
    assert!(a_frame.contains("\"tool_name\":\"agent-a-only\""));
    assert!(a_frame.contains(&agent_a));
    assert!(!a_frame.contains(&agent_b));
    let b_frame = next_sse_frame(&mut stream_b).await;
    assert!(b_frame.contains("\"tool_name\":\"agent-b-only\""));
    assert!(b_frame.contains(&agent_b));
    assert!(!b_frame.contains(&agent_a));

    let global_a = next_sse_frame_containing(&mut global, "\"tool_name\":\"agent-a-only\"").await;
    let global_b = next_sse_frame_containing(&mut global, "\"tool_name\":\"agent-b-only\"").await;
    let global_unattributed =
        next_sse_frame_containing(&mut global, "\"tool_name\":\"unattributed-live\"").await;
    assert!(global_a.contains("\"tool_name\":\"agent-a-only\""));
    assert!(global_b.contains("\"tool_name\":\"agent-b-only\""));
    assert!(global_unattributed.contains("\"tool_name\":\"unattributed-live\""));
    assert!(global_unattributed.contains("\"agent_id\":null"));

    complete(&history, &a);
    complete(&history, &b);
    complete(&history, &unattributed);
    wait_for_invocation(&history, unattributed.invocation_id.unwrap());
    drop(stream_a);
    drop(stream_b);
    drop(global);
    drop(app);
    history.shutdown_blocking().unwrap();
}

#[tokio::test]
async fn retained_agent_emits_first_seen_on_first_call_in_new_runtime() {
    let dir = tempdir().unwrap();
    let database = dir.path().join("history.sqlite3");
    let workdir = dir.path().join("repo");
    std::fs::create_dir_all(&workdir).unwrap();

    let old = open_history(&database, "runtime-old");
    let old_context = begin(
        &old,
        provider_context("retained-provider"),
        "test_tool",
        Some(&workdir),
    );
    complete(&old, &old_context);
    wait_for_invocation(&old, old_context.invocation_id.unwrap());
    let retained_agent_id = old_context.agent_id.clone().unwrap();
    old.shutdown_blocking().unwrap();

    let history = open_history(&database, "runtime-new");
    let (_sequence, mut events) = history.subscribe_live_events();
    let current = begin(
        &history,
        provider_context("retained-provider"),
        "test_tool",
        Some(&workdir),
    );
    assert_eq!(
        current.agent_id.as_deref(),
        Some(retained_agent_id.as_ref())
    );
    let first = tokio::time::timeout(Duration::from_secs(2), events.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first.event_type, "agent_first_seen");
    assert_eq!(first.agent_id.as_deref(), Some(retained_agent_id.as_ref()));
    assert_eq!(first.invocation_id, current.invocation_id);
    complete(&history, &current);
    wait_for_invocation(&history, current.invocation_id.unwrap());
    history.shutdown_blocking().unwrap();
}
