use std::fs;

use serde_json::json;
use tempfile::tempdir;

use crate::config::Config;
use crate::local::{HistoryQuery, LocalHistoryReader};

use super::local::{LocalMcpServerConfig, start_local_mcp_server_with_observer};
use super::local_tests::{
    TOKEN, history_runtime, json_response, local_service_with_history, post_mcp,
    shutdown_history_runtime, test_http_client, tool_call,
};

#[tokio::test]
async fn local_mcp_captures_bounded_actual_before_and_after_file_evidence() {
    let root = tempdir().unwrap();
    let database = root.path().join("history.sqlite3");
    let shell_path = root.path().join("shell file.txt");
    fs::write(&shell_path, "before\n").unwrap();

    let history = history_runtime(database.clone());
    let service = local_service_with_history(Config::default(), history.clone());
    let server = start_local_mcp_server_with_observer(
        service.clone(),
        LocalMcpServerConfig::new(root.path(), TOKEN).with_invocation_recorder(history.clone()),
        None,
    )
    .await
    .unwrap();
    let client = test_http_client();

    let shell = json_response(
        post_mcp(
            &client,
            &server.url(),
            Some(TOKEN),
            Some("tools/call"),
            tool_call(
                90,
                "exec_command",
                json!({
                    "cmd":"printf 'after\\n' >> 'shell file.txt'",
                    "workdir":root.path(),
                    "yield_time_ms":60_000
                }),
                "file-evidence-session",
            ),
        )
        .await,
    )
    .await;
    assert_ne!(shell["result"]["isError"], json!(true));

    let patch = json_response(
        post_mcp(
            &client,
            &server.url(),
            Some(TOKEN),
            Some("tools/call"),
            tool_call(
                91,
                "apply_patch",
                json!({
                    "patch":"*** Begin Patch\n*** Add File: patch file.txt\n+hello\n*** End Patch\n",
                    "workdir":root.path()
                }),
                "file-evidence-session",
            ),
        )
        .await,
    )
    .await;
    assert_ne!(patch["result"]["isError"], json!(true));
    history.flush_for_test().unwrap();

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
    let shell = records
        .iter()
        .find(|record| record.tool_name == "exec_command")
        .unwrap();
    assert_eq!(shell.file_evidence.len(), 1);
    assert_eq!(shell.file_evidence[0].operation_hint, "append");
    assert_eq!(
        shell.file_evidence[0].before_text.as_deref(),
        Some("before\n")
    );
    assert_eq!(
        shell.file_evidence[0].after_text.as_deref(),
        Some("before\nafter\n")
    );

    let patch = records
        .iter()
        .find(|record| record.tool_name == "apply_patch")
        .unwrap();
    assert_eq!(patch.file_evidence.len(), 1);
    assert_eq!(patch.file_evidence[0].operation_hint, "create");
    assert_eq!(patch.file_evidence[0].before_state, "missing");
    assert_eq!(
        patch.file_evidence[0].after_text.as_deref(),
        Some("hello\n")
    );
}
