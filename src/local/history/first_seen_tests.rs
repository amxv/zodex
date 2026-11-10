use std::collections::HashSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier, Mutex};

use serde_json::json;
use tempfile::tempdir;

use crate::invocation::{
    InvocationContext, InvocationEvidenceRecorder, InvocationStart, ProviderCallMetadata,
};

use super::{LocalHistoryReader, LocalHistoryRuntime, LocalHistoryRuntimeConfig};

fn history_path(root: &std::path::Path) -> std::path::PathBuf {
    root.join("history/history.sqlite3")
}

fn provider_context(session: &str, correlation: &str) -> InvocationContext {
    InvocationContext::default()
        .with_correlation_id(correlation)
        .with_provider(ProviderCallMetadata::new("openai/session", session))
}

fn patch_start(workdir: &std::path::Path, marker: &str) -> InvocationStart {
    InvocationStart::new(
        "apply_patch",
        json!({
            "patch": marker,
            "workdir": workdir,
        }),
    )
}

#[test]
fn first_seen_observer_fires_once_per_attributed_agent_after_durable_commit() {
    let dir = tempdir().unwrap();
    let path = history_path(dir.path());
    let runtime = LocalHistoryRuntime::open(LocalHistoryRuntimeConfig::new(
        path.clone(),
        "runtime-first-seen",
        365 * 86_400,
        1024 * 1024 * 1024,
    ))
    .unwrap();
    let seen = Arc::new(Mutex::new(Vec::<String>::new()));
    let callback_seen = seen.clone();
    let callback_path = path.clone();
    runtime
        .install_agent_first_seen_observer(Arc::new(move |agent_id| {
            assert!(
                LocalHistoryReader::agent_record(&callback_path, &agent_id)
                    .unwrap()
                    .is_some(),
                "first-seen callback must run only after the Agent mapping commits"
            );
            callback_seen.lock().unwrap().push(agent_id);
        }))
        .unwrap();

    let first = runtime
        .begin(
            provider_context("conversation-a", "first-a"),
            patch_start(dir.path(), "a1"),
        )
        .unwrap();
    let repeat = runtime
        .begin(
            provider_context("conversation-a", "repeat-a"),
            patch_start(dir.path(), "a2"),
        )
        .unwrap();
    let second = runtime
        .begin(
            provider_context("conversation-b", "first-b"),
            patch_start(dir.path(), "b1"),
        )
        .unwrap();
    runtime
        .begin(
            InvocationContext::default().with_correlation_id("unattributed"),
            patch_start(dir.path(), "none"),
        )
        .unwrap();

    runtime.flush_for_test().unwrap();
    assert_eq!(first.agent_id, repeat.agent_id);
    assert_ne!(first.agent_id, second.agent_id);
    assert_eq!(
        seen.lock().unwrap().as_slice(),
        [
            first.agent_id.as_deref().unwrap(),
            second.agent_id.as_deref().unwrap(),
        ]
    );
    runtime.shutdown_blocking().unwrap();
}

#[test]
fn first_seen_observer_fires_again_for_existing_agent_in_a_new_runtime() {
    let dir = tempdir().unwrap();
    let path = history_path(dir.path());
    let first_runtime = LocalHistoryRuntime::open(LocalHistoryRuntimeConfig::new(
        path.clone(),
        "runtime-a",
        365 * 86_400,
        1024 * 1024 * 1024,
    ))
    .unwrap();
    let first_seen = Arc::new(AtomicUsize::new(0));
    let first_seen_callback = first_seen.clone();
    first_runtime
        .install_agent_first_seen_observer(Arc::new(move |_| {
            first_seen_callback.fetch_add(1, Ordering::SeqCst);
        }))
        .unwrap();
    let first = first_runtime
        .begin(
            provider_context("conversation-restart", "runtime-a-call"),
            patch_start(dir.path(), "runtime-a"),
        )
        .unwrap();
    first_runtime.flush_for_test().unwrap();
    assert_eq!(first_seen.load(Ordering::SeqCst), 1);
    first_runtime.shutdown_blocking().unwrap();
    drop(first_runtime);

    let second_runtime = LocalHistoryRuntime::open(LocalHistoryRuntimeConfig::new(
        path,
        "runtime-b",
        365 * 86_400,
        1024 * 1024 * 1024,
    ))
    .unwrap();
    let second_seen = Arc::new(AtomicUsize::new(0));
    let second_seen_callback = second_seen.clone();
    second_runtime
        .install_agent_first_seen_observer(Arc::new(move |_| {
            second_seen_callback.fetch_add(1, Ordering::SeqCst);
        }))
        .unwrap();
    let second = second_runtime
        .begin(
            provider_context("conversation-restart", "runtime-b-call"),
            patch_start(dir.path(), "runtime-b"),
        )
        .unwrap();

    second_runtime.flush_for_test().unwrap();
    assert_eq!(first.agent_id, second.agent_id);
    assert_eq!(second_seen.load(Ordering::SeqCst), 1);
    second_runtime.shutdown_blocking().unwrap();
}

#[test]
fn concurrent_first_calls_emit_one_first_seen_observer_notification() {
    let dir = tempdir().unwrap();
    let runtime = LocalHistoryRuntime::open(LocalHistoryRuntimeConfig::new(
        history_path(dir.path()),
        "runtime-concurrent-first-seen",
        365 * 86_400,
        1024 * 1024 * 1024,
    ))
    .unwrap();
    let notifications = Arc::new(AtomicUsize::new(0));
    let callback_notifications = notifications.clone();
    runtime
        .install_agent_first_seen_observer(Arc::new(move |_| {
            callback_notifications.fetch_add(1, Ordering::SeqCst);
        }))
        .unwrap();

    let barrier = Arc::new(Barrier::new(8));
    let mut joins = Vec::new();
    for index in 0..8 {
        let runtime = runtime.clone();
        let barrier = barrier.clone();
        let workdir = dir.path().to_path_buf();
        joins.push(std::thread::spawn(move || {
            barrier.wait();
            runtime
                .begin(
                    provider_context("conversation-concurrent", &format!("call-{index}")),
                    patch_start(&workdir, &format!("patch-{index}")),
                )
                .ok()
                .and_then(|context| context.agent_id)
        }));
    }
    let agent_ids = joins
        .into_iter()
        .filter_map(|join| join.join().unwrap())
        .collect::<HashSet<_>>();

    runtime.flush_for_test().unwrap();
    assert!(!agent_ids.is_empty());
    assert_eq!(agent_ids.len(), 1);
    assert_eq!(notifications.load(Ordering::SeqCst), 1);
    runtime.shutdown_blocking().unwrap();
}
