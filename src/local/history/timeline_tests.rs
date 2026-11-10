use std::sync::Arc;
use std::time::Duration;

use rusqlite::{Connection, ErrorCode};
use serde_json::json;
use tempfile::tempdir;

use crate::invocation::{
    InvocationContext, InvocationContinuationKind, InvocationEvidenceRecorder, InvocationOutcome,
    InvocationStart, ProviderCallMetadata,
};
use crate::local::presentation::{PresentationFileOperation, PresentationKind};
use crate::session::{SessionOutputChunk, SessionOutputCompletion, SessionOutputObserver};

use super::timeline::LEAN_TIMELINE_INVOCATION_SELECT;
use super::*;

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

fn complete(history: &LocalHistoryRuntime, context: &InvocationContext, result: serde_json::Value) {
    history
        .complete(context, InvocationOutcome::Success(result))
        .unwrap();
}

fn begin_poll(
    history: &LocalHistoryRuntime,
    caller: InvocationContext,
    parent: &InvocationContext,
    handle: &str,
) -> InvocationContext {
    let start = InvocationStart::new(
        "write_stdin",
        json!({"session_handle": handle, "chars": "", "kill_process": false}),
    )
    .with_target_created_by_agent_id(parent.agent_id.clone())
    .with_target_created_by_invocation_id(parent.invocation_id)
    .with_continuation_kind(InvocationContinuationKind::Poll);

    // This test creates 1,000 polls while the background writer materializes
    // their timeline. On slower CI runners, an occasional foreground BEGIN
    // IMMEDIATE can hit Zodex's deliberately short SQLite busy timeout.
    // Retry only that pre-write lock failure; never retry errors after a
    // transaction may have persisted an invocation (which could duplicate it).
    for attempt in 0..=20 {
        match history.begin(caller.clone(), start.clone()) {
            Ok(context) => return context,
            Err(error)
                if attempt < 20
                    && error.chain().any(|cause| {
                        cause.to_string()
                            == "failed to start mandatory Local invocation-envelope transaction"
                    })
                    && error.chain().any(|cause| {
                        matches!(
                            cause.downcast_ref::<rusqlite::Error>(),
                            Some(rusqlite::Error::SqliteFailure(code, _))
                                if matches!(code.code, ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked)
                        )
                    }) =>
            {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!("poll invocation failed on attempt {attempt}: {error:#}"),
        }
    }
    unreachable!("the retry loop either returns or panics")
}

fn history_query(limit: usize, cursor: Option<HistoryTimelineCursor>) -> HistoryTimelineQuery {
    HistoryTimelineQuery {
        limit,
        cursor,
        agent_id: None,
        normalized_workdir: None,
        diff_projection: HistoryDiffProjection::Full,
        mode: HistoryTimelineMode::History { before_ms: None },
    }
}

#[test]
fn kill_presentation_resolves_exact_parent_command_from_invocation_link() {
    let dir = tempdir().unwrap();
    let database = dir.path().join("history.sqlite3");
    let history = open_history(&database, "runtime-kill-command");
    let command_text = "cargo test --workspace --all-targets";
    let command = history
        .begin(
            provider_context("kill-command-agent"),
            InvocationStart::new(
                "exec_command",
                json!({"cmd": command_text, "workdir": dir.path().display().to_string()}),
            ),
        )
        .unwrap();
    let command_id = command.invocation_id.unwrap();
    let handle = "kill-command-handle";
    complete(
        &history,
        &command,
        json!({"status":"running","session_handle":handle,"cwd":dir.path()}),
    );

    let kill = history
        .begin(
            provider_context("kill-command-agent"),
            InvocationStart::new(
                "write_stdin",
                json!({"session_handle":handle,"chars":"","kill_process":true}),
            )
            .with_target_created_by_agent_id(command.agent_id.clone())
            .with_target_created_by_invocation_id(Some(command_id))
            .with_continuation_kind(InvocationContinuationKind::Kill),
        )
        .unwrap();
    let kill_id = kill.invocation_id.unwrap();
    complete(
        &history,
        &kill,
        json!({"status":"exited","session_handle":handle}),
    );
    history.flush_for_test().unwrap();

    let page = LocalHistoryReader::timeline(&database, &history_query(10, None)).unwrap();
    let kill_record = page
        .records
        .iter()
        .find(|record| record.primary_invocation_id == kill_id)
        .expect("kill presentation should remain a separate timeline root");
    match &kill_record.kind {
        PresentationKind::Kill {
            target_command,
            target_session_handle,
            ..
        } => {
            assert_eq!(target_command.as_deref(), Some(command_text));
            assert_eq!(target_session_handle, handle);
        }
        other => panic!("expected kill presentation, got {other:?}"),
    }
}

#[tokio::test]
async fn timeline_folds_large_poll_families_keeps_real_stdin_and_bulk_file_evidence() {
    let dir = tempdir().unwrap();
    let database = dir.path().join("history.sqlite3");
    let workdir = dir.path().join("repo");
    std::fs::create_dir_all(&workdir).unwrap();
    let history = open_history(&database, "runtime-timeline-folding");

    let command = history
        .begin(
            provider_context("timeline-agent"),
            InvocationStart::new(
                "exec_command",
                json!({
                    "cmd": "cargo test --workspace",
                    "workdir": workdir.display().to_string(),
                    "yield_time_ms": 1000
                }),
            ),
        )
        .unwrap();
    let command_id = command.invocation_id.unwrap();
    let agent_id = command.agent_id.clone().unwrap();
    let handle = "timeline-long-command";
    complete(
        &history,
        &command,
        json!({
            "status": "running",
            "session_handle": handle,
            "cwd": workdir.display().to_string(),
            "output": "INITIAL-RESULT-MUST-NOT-BE-IN-TIMELINE"
        }),
    );

    let mut poll_ids = Vec::new();
    for index in 0..1_000 {
        let poll = begin_poll(
            &history,
            provider_context("timeline-agent"),
            &command,
            handle,
        );
        poll_ids.push(poll.invocation_id.unwrap());
        complete(
            &history,
            &poll,
            json!({
                "status": if index == 999 { "exited" } else { "running" },
                "cwd": workdir.display().to_string(),
                "exit_code": if index == 999 { Some(0) } else { None::<i64> },
                "termination_reason": if index == 999 { Some("exit") } else { None::<&str> },
                "output": format!("CUMULATIVE-POLL-SECRET-{index:03}")
            }),
        );
        if index % 100 == 99 {
            history.flush_for_test().unwrap();
        }
    }

    let stdin = history
        .begin(
            provider_context("timeline-agent"),
            InvocationStart::new(
                "write_stdin",
                json!({"session_handle": handle, "chars": "y\n", "kill_process": false}),
            )
            .with_target_created_by_agent_id(Some(agent_id.clone()))
            .with_target_created_by_invocation_id(Some(command_id))
            .with_continuation_kind(InvocationContinuationKind::Stdin),
        )
        .unwrap();
    complete(
        &history,
        &stdin,
        json!({"status": "exited", "session_handle": handle}),
    );

    let patch_path = workdir.join("bulk-evidence.rs");
    std::fs::write(&patch_path, "fn value() -> i32 { 1 }\n").unwrap();
    let patch = history
        .begin(
            provider_context("timeline-agent"),
            InvocationStart::new(
                "apply_patch",
                json!({
                    "workdir": workdir.display().to_string(),
                    "patch": "*** Begin Patch\n*** Update File: bulk-evidence.rs\n@@\n-fn value() -> i32 { 1 }\n+fn value() -> i32 { 2 }\n*** End Patch\n"
                }),
            ),
        )
        .unwrap();
    let patch_id = patch.invocation_id.unwrap();
    let (result, evidence) = crate::apply_patch::apply_patch_with_delta(
        "*** Begin Patch\n*** Update File: bulk-evidence.rs\n@@\n-fn value() -> i32 { 1 }\n+fn value() -> i32 { 2 }\n*** End Patch\n",
        workdir.to_str().unwrap(),
    )
    .await;
    result.unwrap();
    history.record_patch_evidence(&patch, evidence);
    // A later writer must not change the already committed patch evidence.
    std::fs::write(&patch_path, "unrelated later content\n").unwrap();
    complete(&history, &patch, json!({"status": "exited"}));
    history.flush_for_test().unwrap();

    let page = LocalHistoryReader::timeline(&database, &history_query(20, None)).unwrap();
    assert_eq!(
        page.records.len(),
        3,
        "1,000 no-input polls must consume no root slots"
    );
    let command_record = page
        .records
        .iter()
        .find(|record| record.primary_invocation_id == command_id)
        .unwrap();
    assert_eq!(command_record.presentation_id, format!("inv-{command_id}"));
    assert_eq!(command_record.raw_evidence_count, 1_001);
    assert_eq!(command_record.raw_invocation_ids.len(), 32);
    assert!(command_record.raw_invocation_ids_truncated);
    assert_eq!(command_record.raw_invocation_ids[0], command_id);
    match &command_record.kind {
        PresentationKind::Command {
            status,
            output,
            polls,
            exit_code,
            ..
        } => {
            assert_eq!(status, "exited");
            assert_eq!(*exit_code, Some(0));
            assert!(
                output.is_none(),
                "collapsed timeline must not preload command output"
            );
            let polls = polls.as_ref().unwrap();
            assert_eq!(polls.count, 1_000);
            assert_eq!(polls.final_status.as_deref(), Some("exited"));
        }
        other => panic!("expected command presentation, got {other:?}"),
    }
    assert!(
        page.records
            .iter()
            .any(|record| matches!(record.kind, PresentationKind::Stdin { .. })),
        "real stdin must remain a separate canonical root"
    );
    let file_record = page
        .records
        .iter()
        .find(|record| matches!(record.kind, PresentationKind::FileChanges { .. }))
        .unwrap();
    match &file_record.kind {
        PresentationKind::FileChanges { changes, .. } => {
            assert_eq!(changes.len(), 1);
            assert_eq!(changes[0].operation, PresentationFileOperation::Edited);
            assert_eq!(changes[0].added, 1);
            assert_eq!(changes[0].removed, 1);
            assert!(changes[0].diff_lines_included);
            assert!(!changes[0].lines.is_empty());
        }
        _ => unreachable!(),
    }
    let serialized = serde_json::to_string(&page).unwrap();
    assert!(!serialized.contains("INITIAL-RESULT-MUST-NOT-BE-IN-TIMELINE"));
    assert!(!serialized.contains("CUMULATIVE-POLL-SECRET"));

    let mut summary_query = history_query(20, None);
    summary_query.diff_projection = HistoryDiffProjection::Summary;
    let summary_page = LocalHistoryReader::timeline(&database, &summary_query).unwrap();
    let summary_file = summary_page
        .records
        .iter()
        .find(|record| record.primary_invocation_id == patch_id)
        .unwrap();
    match &summary_file.kind {
        PresentationKind::FileChanges { changes, .. } => {
            assert_eq!(changes.len(), 1);
            assert_eq!(changes[0].added, 1);
            assert_eq!(changes[0].removed, 1);
            assert!(!changes[0].diff_lines_included);
            assert!(changes[0].lines.is_empty());
        }
        other => panic!("expected summary file presentation, got {other:?}"),
    }
    let hydrated = LocalHistoryReader::timeline_details(&database, &[patch_id]).unwrap();
    assert_eq!(hydrated.len(), 1);
    match &hydrated[0].kind {
        PresentationKind::FileChanges { changes, .. } => {
            assert!(changes[0].diff_lines_included);
            assert!(!changes[0].lines.is_empty());
        }
        other => panic!("expected hydrated file presentation, got {other:?}"),
    }
    let connection = Connection::open(&database).unwrap();
    let materialized: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM presentation_materializations WHERE root_invocation_id = ?1",
            [patch_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(materialized, 1);
    drop(connection);

    let detail = LocalHistoryReader::timeline_detail(&database, command_id)
        .unwrap()
        .unwrap();
    assert_eq!(detail.raw_evidence_count, 1_001);

    let mut cursor = None;
    let mut checkpoints = Vec::new();
    loop {
        let page =
            LocalHistoryReader::timeline_checkpoints(&database, command_id, 17, cursor.as_ref())
                .unwrap()
                .unwrap();
        assert!(
            !serde_json::to_string(&page)
                .unwrap()
                .contains("CUMULATIVE-POLL-SECRET")
        );
        checkpoints.extend(
            page.checkpoints
                .iter()
                .map(|checkpoint| checkpoint.invocation_id),
        );
        match page.next_cursor {
            Some(raw) => cursor = Some(HistoryTimelineCursor::decode(&raw).unwrap()),
            None => break,
        }
    }
    assert_eq!(checkpoints.len(), 1_001);
    assert_eq!(checkpoints[0], command_id);
    assert_eq!(&checkpoints[1..], poll_ids.as_slice());

    history.shutdown_blocking().unwrap();
}

#[test]
fn timeline_renders_exclusive_shell_creation_from_successful_literal_command() {
    let dir = tempdir().unwrap();
    let database = dir.path().join("history.sqlite3");
    let history = open_history(&database, "runtime-exclusive-shell-creation");
    let created = history
        .begin(
            provider_context("exclusive-creation-agent"),
            InvocationStart::new(
                "exec_command",
                json!({
                    "cmd": "python3 -I -S <<'PY'\nwith open(\"new.txt\", \"x\", encoding=\"utf-8\", newline=\"\") as f:\n    f.write(\"known content\\n\")\nPY",
                    "workdir": dir.path(),
                }),
            ),
        )
        .unwrap();
    // Simulate the exact successful tool result. No destination exists for the
    // history worker to read: all preview bytes must come from the command.
    complete(
        &history,
        &created,
        json!({"status":"exited","exit_code":0,"termination_reason":"exit"}),
    );
    history.flush_for_test().unwrap();
    assert!(!dir.path().join("new.txt").exists());
    let records = LocalHistoryReader::query(
        &database,
        &HistoryQuery {
            invocation_id: created.invocation_id,
            last: 1,
            include_raw: true,
            ..HistoryQuery::default()
        },
    )
    .unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].file_evidence.len(), 1);
    let evidence = &records[0].file_evidence[0];
    assert_eq!(evidence.before_state, "missing");
    assert_eq!(evidence.operation_hint, "create");
    assert_eq!(evidence.after_text.as_deref(), Some("known content\n"));

    let page = LocalHistoryReader::timeline(&database, &history_query(10, None)).unwrap();
    assert_eq!(page.records.len(), 1);
    match &page.records[0].kind {
        PresentationKind::FileChanges { changes, .. } => {
            assert_eq!(changes.len(), 1);
            assert_eq!(changes[0].operation, PresentationFileOperation::Created);
            assert_eq!(changes[0].added, 1);
            assert_eq!(changes[0].removed, 0);
        }
        other => panic!("expected known creation preview, got {other:?}"),
    }
    history.shutdown_blocking().unwrap();
}

#[test]
fn history_cursor_is_exclusive_for_equal_timestamps_and_ignores_newer_insertions() {
    let dir = tempdir().unwrap();
    let database = dir.path().join("history.sqlite3");
    let history = open_history(&database, "runtime-cursor-one");
    let mut original_ids = Vec::new();
    for index in 0..6 {
        let context = history
            .begin(
                InvocationContext::default(),
                InvocationStart::new("test_tool", json!({"index": index})),
            )
            .unwrap();
        original_ids.push(context.invocation_id.unwrap());
        complete(&history, &context, json!({"ok": true}));
    }
    history.flush_for_test().unwrap();
    history.shutdown_blocking().unwrap();

    let connection = Connection::open(&database).unwrap();
    connection
        .execute("UPDATE invocations SET started_at_ms = 1000", [])
        .unwrap();
    drop(connection);

    let first = LocalHistoryReader::timeline(&database, &history_query(2, None)).unwrap();
    assert!(first.has_more);
    let first_ids = first
        .records
        .iter()
        .map(|record| record.primary_invocation_id)
        .collect::<Vec<_>>();
    assert_eq!(first_ids, original_ids[4..].to_vec());
    let mut cursor =
        Some(HistoryTimelineCursor::decode(first.next_cursor.as_deref().unwrap()).unwrap());

    let history = open_history(&database, "runtime-cursor-two");
    let newer = history
        .begin(
            InvocationContext::default(),
            InvocationStart::new("newer_tool", json!({"new": true})),
        )
        .unwrap();
    complete(&history, &newer, json!({"ok": true}));
    history.flush_for_test().unwrap();

    let mut continued = Vec::new();
    loop {
        let page =
            LocalHistoryReader::timeline(&database, &history_query(2, cursor.clone())).unwrap();
        continued.extend(
            page.records
                .iter()
                .map(|record| record.primary_invocation_id),
        );
        cursor = page
            .next_cursor
            .as_deref()
            .map(HistoryTimelineCursor::decode)
            .transpose()
            .unwrap();
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(
        continued,
        vec![
            original_ids[2],
            original_ids[3],
            original_ids[0],
            original_ids[1]
        ],
        "each page is chronological, while successive history pages move to older keyset windows"
    );
    assert!(!continued.contains(&newer.invocation_id.unwrap()));
    let mut all = first_ids;
    all.extend(continued);
    all.sort_unstable();
    assert_eq!(all, original_ids);
    history.shutdown_blocking().unwrap();
}

#[test]
fn recovery_pages_past_one_hundred_roots_and_refreshes_parent_for_new_poll() {
    let dir = tempdir().unwrap();
    let database = dir.path().join("history.sqlite3");
    let history = open_history(&database, "runtime-recovery-timeline");
    for index in 0..125 {
        let context = history
            .begin(
                InvocationContext::default(),
                InvocationStart::new("changed", json!({"index": index})),
            )
            .unwrap();
        complete(&history, &context, json!({"ok": true}));
    }
    history.flush_for_test().unwrap();

    let mut cursor = None;
    let mut recovered = Vec::new();
    loop {
        let page = LocalHistoryReader::timeline(
            &database,
            &HistoryTimelineQuery {
                limit: 37,
                cursor: cursor.clone(),
                agent_id: None,
                normalized_workdir: None,
                diff_projection: HistoryDiffProjection::Full,
                mode: HistoryTimelineMode::Recovery {
                    since_ms: 0,
                    active_process_invocation_ids: Vec::new(),
                },
            },
        )
        .unwrap();
        recovered.extend(
            page.records
                .iter()
                .map(|record| record.primary_invocation_id),
        );
        cursor = page
            .next_cursor
            .as_deref()
            .map(HistoryTimelineCursor::decode)
            .transpose()
            .unwrap();
        if cursor.is_none() {
            break;
        }
    }
    recovered.sort_unstable();
    recovered.dedup();
    assert_eq!(recovered.len(), 125);

    let command = history
        .begin(
            provider_context("recovery-parent"),
            InvocationStart::new("exec_command", json!({"cmd": "sleep 1"})),
        )
        .unwrap();
    complete(
        &history,
        &command,
        json!({"status": "running", "session_handle": "recovery-parent-handle"}),
    );
    history.flush_for_test().unwrap();
    let raw = LocalHistoryReader::query(
        &database,
        &HistoryQuery {
            last: 1,
            invocation_id: command.invocation_id,
            ..HistoryQuery::default()
        },
    )
    .unwrap();
    let cutoff = raw[0].completed_at_ms.unwrap().saturating_add(1);
    std::thread::sleep(Duration::from_millis(2));
    let poll = begin_poll(
        &history,
        provider_context("recovery-parent"),
        &command,
        "recovery-parent-handle",
    );
    complete(&history, &poll, json!({"status": "exited", "exit_code": 0}));
    history.flush_for_test().unwrap();
    let refreshed = LocalHistoryReader::timeline(
        &database,
        &HistoryTimelineQuery {
            limit: 10,
            cursor: None,
            agent_id: None,
            normalized_workdir: None,
            diff_projection: HistoryDiffProjection::Full,
            mode: HistoryTimelineMode::Recovery {
                since_ms: cutoff,
                active_process_invocation_ids: Vec::new(),
            },
        },
    )
    .unwrap();
    assert!(
        refreshed
            .records
            .iter()
            .any(|record| record.primary_invocation_id == command.invocation_id.unwrap()),
        "a new child poll must refresh its older canonical parent"
    );

    let mut large_active_set = (100_000_i64..101_500_i64).collect::<Vec<_>>();
    large_active_set.push(command.invocation_id.unwrap());
    let forced_active = LocalHistoryReader::timeline(
        &database,
        &HistoryTimelineQuery {
            limit: 10,
            cursor: None,
            agent_id: None,
            normalized_workdir: None,
            diff_projection: HistoryDiffProjection::Full,
            mode: HistoryTimelineMode::Recovery {
                since_ms: i64::MAX / 4,
                active_process_invocation_ids: large_active_set,
            },
        },
    )
    .unwrap();
    assert!(
        forced_active
            .records
            .iter()
            .any(|record| record.primary_invocation_id == command.invocation_id.unwrap()),
        "large active-process ID sets must remain recoverable without SQLite bind ceilings"
    );
    history.shutdown_blocking().unwrap();
}

#[test]
fn legacy_orphan_polls_remain_one_conservative_stable_aggregate() {
    let dir = tempdir().unwrap();
    let database = dir.path().join("history.sqlite3");
    let history = open_history(&database, "runtime-orphan-timeline");
    let mut ids = Vec::new();
    for index in 0..3 {
        let poll = history
            .begin(
                provider_context("orphan-agent"),
                InvocationStart::new(
                    "write_stdin",
                    json!({"session_handle": "orphan-handle", "chars": "", "kill_process": false}),
                )
                .with_continuation_kind(InvocationContinuationKind::Poll),
            )
            .unwrap();
        ids.push(poll.invocation_id.unwrap());
        complete(
            &history,
            &poll,
            json!({"status": if index == 2 { "exited" } else { "running" }}),
        );
    }
    history.flush_for_test().unwrap();
    let page = LocalHistoryReader::timeline(&database, &history_query(10, None)).unwrap();
    assert_eq!(page.records.len(), 1);
    let record = &page.records[0];
    assert_eq!(record.primary_invocation_id, ids[0]);
    assert_eq!(record.presentation_id, format!("inv-{}", ids[0]));
    assert_eq!(record.raw_evidence_count, 3);
    match &record.kind {
        PresentationKind::PollAggregate {
            count,
            final_status,
            ..
        } => {
            assert_eq!(*count, 3);
            assert_eq!(final_status.as_deref(), Some("exited"));
        }
        other => panic!("expected orphan poll aggregate, got {other:?}"),
    }
    history.shutdown_blocking().unwrap();
}

#[test]
fn focused_timeline_filters_roots_before_hydration_without_changing_cross_agent_ownership() {
    let dir = tempdir().unwrap();
    let database = dir.path().join("history.sqlite3");
    let history = open_history(&database, "runtime-focused-roots");

    let command = history
        .begin(
            provider_context("focused-agent-a"),
            InvocationStart::new("exec_command", json!({"cmd":"long-running"})),
        )
        .unwrap();
    let command_id = command.invocation_id.unwrap();
    let agent_a = command.agent_id.clone().unwrap().to_string();
    complete(
        &history,
        &command,
        json!({"status":"running","session_handle":"focused-command"}),
    );

    let attached_from_b = begin_poll(
        &history,
        provider_context("focused-agent-b"),
        &command,
        "focused-command",
    );
    let agent_b = attached_from_b.agent_id.clone().unwrap().to_string();
    complete(&history, &attached_from_b, json!({"status":"running"}));

    let orphan_a = history
        .begin(
            provider_context("focused-agent-a"),
            InvocationStart::new(
                "write_stdin",
                json!({"session_handle":"shared-orphan","chars":"","kill_process":false}),
            )
            .with_continuation_kind(InvocationContinuationKind::Poll),
        )
        .unwrap();
    complete(&history, &orphan_a, json!({"status":"running"}));
    let orphan_b = history
        .begin(
            provider_context("focused-agent-b"),
            InvocationStart::new(
                "write_stdin",
                json!({"session_handle":"shared-orphan","chars":"","kill_process":false}),
            )
            .with_continuation_kind(InvocationContinuationKind::Poll),
        )
        .unwrap();
    complete(&history, &orphan_b, json!({"status":"exited"}));
    history.flush_for_test().unwrap();

    let focused = |agent_id: String| HistoryTimelineQuery {
        limit: 20,
        cursor: None,
        agent_id: Some(agent_id),
        normalized_workdir: None,
        diff_projection: HistoryDiffProjection::Full,
        mode: HistoryTimelineMode::History { before_ms: None },
    };
    let page_a = LocalHistoryReader::timeline(&database, &focused(agent_a)).unwrap();
    assert_eq!(page_a.records.len(), 1);
    assert_eq!(page_a.records[0].primary_invocation_id, command_id);
    match &page_a.records[0].kind {
        PresentationKind::Command { polls, .. } => {
            assert_eq!(polls.as_ref().unwrap().count, 1);
        }
        other => panic!("expected focused command root, got {other:?}"),
    }

    let page_b = LocalHistoryReader::timeline(&database, &focused(agent_b)).unwrap();
    assert_eq!(
        page_b.records.len(),
        1,
        "Agent B owns the shared legacy orphan aggregate, but not Agent A's attached command root"
    );
    assert_eq!(
        page_b.records[0].primary_invocation_id,
        orphan_a.invocation_id.unwrap()
    );
    match &page_b.records[0].kind {
        PresentationKind::PollAggregate { count, .. } => assert_eq!(*count, 2),
        other => panic!("expected focused orphan aggregate, got {other:?}"),
    }

    history.shutdown_blocking().unwrap();
}

#[test]
fn display_output_replays_split_terminal_state_and_fails_closed_on_missing_sequence() {
    let dir = tempdir().unwrap();
    let database = dir.path().join("history.sqlite3");
    let history = open_history(&database, "runtime-display-output");
    let command = history
        .begin(
            provider_context("display-output"),
            InvocationStart::new("exec_command", json!({"cmd": "color-output"})),
        )
        .unwrap();
    let invocation_id = command.invocation_id.unwrap();
    let raw_chunks = [
        "hello \u{1b}[31",
        "mred\u{1b}[0m world\n",
        "\u{1b}]0;hidden-title",
        "\u{7}tail\n",
    ];
    for (sequence, text) in raw_chunks.iter().enumerate() {
        history.observe_output(SessionOutputChunk {
            internal_session_id: 41,
            session_handle: Arc::from("display-output-handle"),
            invocation: command.clone(),
            sequence: sequence as u64,
            text: (*text).to_string(),
        });
    }
    history.observe_output_complete(SessionOutputCompletion {
        internal_session_id: 41,
        session_handle: Arc::from("display-output-handle"),
        invocation: command.clone(),
    });
    complete(
        &history,
        &command,
        json!({"status": "exited", "exit_code": 0}),
    );
    history.flush_for_test().unwrap();

    let metadata = LocalHistoryReader::output_metadata(&database, invocation_id)
        .unwrap()
        .unwrap();
    assert_eq!(metadata.first_cursor, Some(0));
    assert_eq!(metadata.last_cursor, Some(3));
    assert_eq!(metadata.chunk_count, 4);

    let raw = LocalHistoryReader::output_page(&database, invocation_id, 0, 8)
        .unwrap()
        .unwrap();
    assert_eq!(
        raw.chunks
            .iter()
            .map(|chunk| chunk.text.as_str())
            .collect::<Vec<_>>(),
        raw_chunks
    );
    let display = LocalHistoryReader::display_output_page(&database, invocation_id, 0, 8)
        .unwrap()
        .unwrap();
    assert_eq!(display.display_state, "available");
    assert_eq!(
        display.chunks.len(),
        4,
        "display view preserves durable sequence identity"
    );
    assert_eq!(
        display
            .chunks
            .iter()
            .map(|chunk| chunk.text.as_str())
            .collect::<String>(),
        "hello red world\ntail\n"
    );
    assert!(
        display
            .chunks
            .iter()
            .all(|chunk| !chunk.text.contains('\u{1b}'))
    );

    let from_second = LocalHistoryReader::display_output_page(&database, invocation_id, 1, 2)
        .unwrap()
        .unwrap();
    assert_eq!(from_second.chunks[0].sequence, 1);
    assert_eq!(from_second.chunks[0].text, "red world\n");
    assert_eq!(from_second.next_cursor, Some(3));

    history.shutdown_blocking().unwrap();
    let connection = Connection::open(&database).unwrap();
    connection
        .execute(
            "DELETE FROM invocation_output_chunks WHERE invocation_id = ?1 AND sequence = 1",
            [invocation_id],
        )
        .unwrap();
    drop(connection);
    let degraded = LocalHistoryReader::display_output_page(&database, invocation_id, 2, 8)
        .unwrap()
        .unwrap();
    assert_eq!(degraded.display_state, "unavailable");
    assert!(degraded.chunks.is_empty());
    assert!(degraded.display_reason.unwrap().contains("incomplete"));
}

#[test]
fn lean_timeline_projection_never_selects_exact_result_or_pty_bodies() {
    let projection = LEAN_TIMELINE_INVOCATION_SELECT.to_ascii_lowercase();
    assert!(!projection.contains("result_json"));
    assert!(!projection.contains("invocation_output_chunks"));
    assert!(!projection.contains("output_preview"));
}

#[test]
fn output_views_fall_back_to_exact_tool_result_when_no_pty_chunk_was_captured() {
    let dir = tempdir().unwrap();
    let database = dir.path().join("history.sqlite3");
    let history = open_history(&database, "runtime-result-output-fallback");
    let command = history
        .begin(
            provider_context("result-output-fallback"),
            InvocationStart::new("exec_command", json!({"cmd": "synthetic-timeout"})),
        )
        .unwrap();
    let invocation_id = command.invocation_id.unwrap();
    history.observe_output_complete(SessionOutputCompletion {
        internal_session_id: 42,
        session_handle: Arc::from("result-output-fallback-handle"),
        invocation: command.clone(),
    });
    complete(
        &history,
        &command,
        json!({
            "status": "exited",
            "exit_code": -1,
            "termination_reason": "timeout",
            "output": "\n[zodexd] process timed out and was terminated\n"
        }),
    );
    history.flush_for_test().unwrap();

    let metadata = LocalHistoryReader::output_metadata(&database, invocation_id)
        .unwrap()
        .unwrap();
    assert!(metadata.available);
    assert_eq!(metadata.chunk_count, 1);
    assert_eq!(metadata.first_cursor, Some(0));
    assert_eq!(metadata.last_cursor, Some(0));

    let raw = LocalHistoryReader::output_page(&database, invocation_id, 0, 8)
        .unwrap()
        .unwrap();
    assert_eq!(raw.chunks.len(), 1);
    assert_eq!(
        raw.chunks[0].text,
        "\n[zodexd] process timed out and was terminated\n"
    );

    let display = LocalHistoryReader::display_output_page(&database, invocation_id, 0, 8)
        .unwrap()
        .unwrap();
    assert_eq!(display.display_state, "available");
    assert_eq!(display.chunks.len(), 1);
    assert_eq!(
        display.chunks[0].text,
        "\n[zodexd] process timed out and was terminated\n"
    );

    history.shutdown_blocking().unwrap();
}
