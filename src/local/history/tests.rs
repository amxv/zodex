use std::collections::{HashSet, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier, Mutex};
use std::time::{Duration, Instant};

use rusqlite::Connection;
use serde_json::json;
use tempfile::tempdir;

use crate::invocation::{
    InvocationContext, InvocationEvidenceRecorder, InvocationOutcome, InvocationStart,
    ProviderCallMetadata,
};
use crate::protocol::TerminationReason;
use crate::session::{
    OwnedProcess, OwnedProcessEnd, OwnedProcessObserver, ProcessBirthIdentity, ProcessIdentity,
    SessionOutputChunk, SessionOutputCompletion, SessionOutputObserver,
};

use super::query::{HistoryQuery, LocalHistoryReader};
use super::schema::HISTORY_SCHEMA_VERSION;
use super::store::{AgentIdSource, HistoryStore, OutputEvent, now_ms};
use super::worker::{LocalHistoryRuntime, LocalHistoryRuntimeConfig};

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

fn exec_start(workdir: &std::path::Path, command: &str) -> InvocationStart {
    InvocationStart::new(
        "exec_command",
        json!({
            "cmd": command,
            "workdir": workdir,
            "yield_time_ms": 1000,
        }),
    )
}

fn complete_ok(store: &HistoryStore, context: &InvocationContext, marker: &str) {
    store
        .complete(
            context,
            InvocationOutcome::Success(json!({
                "status": "exited",
                "output": marker,
                "summary": marker,
                "cwd": "/tmp",
                "session_handle": null,
                "exit_code": 0,
                "termination_reason": null,
            })),
        )
        .unwrap();
}

#[test]
fn schema_initializes_storage_prerequisites_and_rejects_unknown_layouts() {
    let dir = tempdir().unwrap();
    let path = history_path(dir.path());
    let store = HistoryStore::open(path.clone(), Arc::from("runtime-a")).unwrap();

    let connection = Connection::open(&path).unwrap();
    let version: u32 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    let auto_vacuum: i64 = connection
        .pragma_query_value(None, "auto_vacuum", |row| row.get(0))
        .unwrap();
    let journal_mode: String = connection
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, HISTORY_SCHEMA_VERSION);
    assert_eq!(auto_vacuum, 2, "history must use incremental auto-vacuum");
    assert_eq!(journal_mode.to_ascii_lowercase(), "wal");
    drop(connection);
    drop(store);

    HistoryStore::open(path, Arc::from("runtime-b")).expect("current schema should reopen cleanly");

    let future_path = dir.path().join("future.sqlite3");
    let future = Connection::open(&future_path).unwrap();
    future
        .pragma_update(None, "user_version", HISTORY_SCHEMA_VERSION + 1)
        .unwrap();
    drop(future);
    let error = HistoryStore::open(future_path, Arc::from("runtime"))
        .err()
        .expect("future schema must fail closed");
    assert!(error.to_string().contains("newer"));
    let status = LocalHistoryReader::status(&dir.path().join("future.sqlite3")).unwrap();
    assert_eq!(status.health_state, "unreadable");
    assert!(status.health_reason.unwrap().contains("unsupported"));

    let legacy_path = dir.path().join("legacy.sqlite3");
    let legacy = Connection::open(&legacy_path).unwrap();
    legacy
        .execute_batch("CREATE TABLE mystery(id INTEGER);")
        .unwrap();
    drop(legacy);
    let error = HistoryStore::open(legacy_path, Arc::from("runtime"))
        .err()
        .expect("unversioned non-empty database must not be guessed");
    assert!(error.to_string().contains("unversioned non-empty"));
}

#[test]
fn provider_agent_mapping_is_stable_collision_safe_and_missing_provider_stays_unattributed() {
    let dir = tempdir().unwrap();
    let path = history_path(dir.path());
    let candidates = Arc::new(Mutex::new(VecDeque::from([
        "aaaa".to_string(),
        "aaaa".to_string(),
        "bbbb".to_string(),
    ])));
    let source_candidates = candidates.clone();
    let source: AgentIdSource = Arc::new(move || {
        source_candidates
            .lock()
            .unwrap()
            .pop_front()
            .expect("agent ID test source exhausted")
    });
    let store =
        HistoryStore::open_with_agent_id_source(path.clone(), Arc::from("runtime-a"), source)
            .unwrap();

    let first = store
        .begin(
            provider_context("provider-one", "call-1"),
            patch_start(dir.path(), "one"),
        )
        .unwrap();
    let same = store
        .begin(
            provider_context("provider-one", "call-2"),
            patch_start(dir.path(), "two"),
        )
        .unwrap();
    let second = store
        .begin(
            provider_context("provider-two", "call-3"),
            patch_start(dir.path(), "three"),
        )
        .unwrap();
    let missing = store
        .begin(
            InvocationContext::default().with_correlation_id("call-4"),
            patch_start(dir.path(), "four"),
        )
        .unwrap();

    assert_eq!(first.agent_id.as_deref(), Some("aaaa"));
    assert_eq!(same.agent_id.as_deref(), Some("aaaa"));
    assert_eq!(second.agent_id.as_deref(), Some("bbbb"));
    assert!(missing.agent_id.is_none());
    for (context, marker) in [
        (&first, "one"),
        (&same, "two"),
        (&second, "three"),
        (&missing, "four"),
    ] {
        complete_ok(&store, context, marker);
    }
    drop(store);

    let reopened = HistoryStore::open(path, Arc::from("runtime-b")).unwrap();
    let again = reopened
        .begin(
            provider_context("provider-one", "call-5"),
            patch_start(dir.path(), "five"),
        )
        .unwrap();
    assert_eq!(again.agent_id.as_deref(), Some("aaaa"));
}

#[test]
fn concurrent_first_seen_provider_mapping_resolves_to_one_agent_atomically() {
    let dir = tempdir().unwrap();
    let path = history_path(dir.path());
    let allocations = Arc::new(AtomicUsize::new(0));
    let source_allocations = allocations.clone();
    let source: AgentIdSource = Arc::new(move || {
        let value = source_allocations.fetch_add(1, Ordering::SeqCst);
        format!("c{:03}", value % 1000)
    });
    let store = Arc::new(
        HistoryStore::open_with_agent_id_source(path.clone(), Arc::from("runtime"), source)
            .unwrap(),
    );
    let barrier = Arc::new(Barrier::new(12));
    let mut joins = Vec::new();
    for index in 0..12 {
        let store = store.clone();
        let barrier = barrier.clone();
        let workdir = dir.path().to_path_buf();
        joins.push(std::thread::spawn(move || {
            barrier.wait();
            store
                .begin(
                    provider_context("simultaneous-provider", &format!("parallel-{index}")),
                    patch_start(&workdir, &format!("patch-{index}")),
                )
                .ok()
                .and_then(|context| context.agent_id)
                .map(|agent_id| agent_id.to_string())
        }));
    }
    let agent_ids = joins
        .into_iter()
        .filter_map(|join| join.join().unwrap())
        .collect::<HashSet<_>>();
    assert!(!agent_ids.is_empty());
    assert_eq!(agent_ids.len(), 1);
    assert_eq!(allocations.load(Ordering::SeqCst), 1);

    let connection = Connection::open(path).unwrap();
    let agent_count: i64 = connection
        .query_row("SELECT COUNT(*) FROM agents", [], |row| row.get(0))
        .unwrap();
    assert_eq!(agent_count, 1);
}

#[test]
fn concurrent_output_capture_keeps_invocation_and_agent_streams_separate() {
    let dir = tempdir().unwrap();
    let path = history_path(dir.path());
    let runtime = LocalHistoryRuntime::open(LocalHistoryRuntimeConfig::new(
        path.clone(),
        "runtime",
        365 * 86_400,
        1024 * 1024 * 1024,
    ))
    .unwrap();
    let first = runtime
        .begin(
            provider_context("provider-a", "concurrent-a"),
            exec_start(dir.path(), "stream-a"),
        )
        .unwrap();
    let second = runtime
        .begin(
            provider_context("provider-b", "concurrent-b"),
            exec_start(dir.path(), "stream-b"),
        )
        .unwrap();
    assert_ne!(first.agent_id, second.agent_id);

    let mut joins = Vec::new();
    for (context, handle, marker) in [
        (first.clone(), "concurrent-handle-a", "stream-a"),
        (second.clone(), "concurrent-handle-b", "stream-b"),
    ] {
        let runtime = runtime.clone();
        joins.push(std::thread::spawn(move || {
            for sequence in 0..64 {
                runtime.observe_output(SessionOutputChunk {
                    internal_session_id: if marker == "stream-a" { 1 } else { 2 },
                    session_handle: Arc::from(handle),
                    invocation: context.clone(),
                    sequence,
                    text: format!("{marker}-{sequence:02}\n"),
                });
            }
            runtime.observe_output_complete(SessionOutputCompletion {
                internal_session_id: if marker == "stream-a" { 1 } else { 2 },
                session_handle: Arc::from(handle),
                invocation: context.clone(),
            });
            runtime
                .complete(
                    &context,
                    InvocationOutcome::Success(json!({"output":format!("{marker}-done")})),
                )
                .unwrap();
        }));
    }
    for join in joins {
        join.join().unwrap();
    }
    runtime.shutdown_blocking().unwrap();

    for (context, own_marker, other_marker) in [
        (&first, "stream-a", "stream-b"),
        (&second, "stream-b", "stream-a"),
    ] {
        let record = LocalHistoryReader::query(
            &path,
            &HistoryQuery {
                last: 1,
                invocation_id: context.invocation_id,
                include_raw: true,
                ..HistoryQuery::default()
            },
        )
        .unwrap()
        .pop()
        .unwrap();
        let full_output = record.full_output.unwrap();
        assert!(full_output.contains(&format!("{own_marker}-00")));
        assert!(full_output.contains(&format!("{own_marker}-63")));
        assert!(!full_output.contains(other_marker));
        assert_eq!(record.capture_state, "complete");
        assert_eq!(record.evidence_state, "complete");
    }
}

#[cfg(unix)]
#[test]
fn canonical_workdir_deduplicates_symlinks_and_preserves_first_seen_order() {
    use std::os::unix::fs::symlink;

    let dir = tempdir().unwrap();
    let path = history_path(dir.path());
    let real = dir.path().join("real");
    let other = dir.path().join("other");
    let alias = dir.path().join("alias");
    std::fs::create_dir_all(&real).unwrap();
    std::fs::create_dir_all(&other).unwrap();
    symlink(&real, &alias).unwrap();
    let store = HistoryStore::open(path.clone(), Arc::from("runtime")).unwrap();

    let first = store
        .begin(
            provider_context("provider", "workdir-1"),
            patch_start(&alias, "one"),
        )
        .unwrap();
    let second = store
        .begin(
            provider_context("provider", "workdir-2"),
            patch_start(&real, "two"),
        )
        .unwrap();
    let third = store
        .begin(
            provider_context("provider", "workdir-3"),
            patch_start(&other, "three"),
        )
        .unwrap();
    for (context, marker) in [(&first, "one"), (&second, "two"), (&third, "three")] {
        complete_ok(&store, context, marker);
    }

    let connection = Connection::open(path).unwrap();
    let mut statement = connection
        .prepare(
            "SELECT normalized_workdir, ordinal, retained_invocation_count
             FROM agent_workdirs ORDER BY ordinal",
        )
        .unwrap();
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(
        rows[0].0,
        real.canonicalize().unwrap().display().to_string()
    );
    assert_eq!(rows[0].1, 1);
    assert_eq!(rows[0].2, 2);
    assert_eq!(
        rows[1].0,
        other.canonicalize().unwrap().display().to_string()
    );
    assert_eq!(rows[1].1, 2);
    assert_eq!(rows[1].2, 1);

    let flags = LocalHistoryReader::query(
        store.path(),
        &HistoryQuery {
            last: 10,
            agent_id: first.agent_id.as_deref().map(str::to_owned),
            ..HistoryQuery::default()
        },
    )
    .unwrap();
    let mut flags = flags
        .into_iter()
        .map(|record| (record.correlation_id, record.is_new_workdir))
        .collect::<std::collections::HashMap<_, _>>();
    assert_eq!(flags.remove("workdir-1"), Some(true));
    assert_eq!(flags.remove("workdir-2"), Some(false));
    assert_eq!(flags.remove("workdir-3"), Some(true));
}

#[test]
fn exact_result_error_and_full_output_survive_restart_with_interrupted_capture_marked() {
    let dir = tempdir().unwrap();
    let path = history_path(dir.path());
    let store = HistoryStore::open(path.clone(), Arc::from("runtime-a")).unwrap();
    let exec = store
        .begin(
            provider_context("provider", "exec-exact"),
            exec_start(dir.path(), "printf exact"),
        )
        .unwrap();
    let exec_id = exec.invocation_id.unwrap();
    let timestamp = now_ms().unwrap();
    store
        .persist_output_batch(&[
            OutputEvent::Chunk {
                invocation_id: exec_id,
                agent_id: None,
                sequence: 0,
                observed_at_ms: timestamp,
                text: "\u{1b}[31mraw".to_string(),
            },
            OutputEvent::Chunk {
                invocation_id: exec_id,
                agent_id: None,
                sequence: 1,
                observed_at_ms: timestamp + 1,
                text: "-pty\u{1b}[0m\n".to_string(),
            },
            OutputEvent::Complete {
                invocation_id: exec_id,
                agent_id: None,
            },
        ])
        .unwrap();
    let exact_result = json!({
        "status": "exited",
        "output": "bounded-result",
        "summary": "summary",
        "cwd": dir.path(),
        "session_handle": null,
        "exit_code": 0,
        "termination_reason": null,
    });
    store
        .complete(&exec, InvocationOutcome::Success(exact_result.clone()))
        .unwrap();

    let failed = store
        .begin(
            provider_context("provider", "exact-error"),
            patch_start(dir.path(), "bad patch"),
        )
        .unwrap();
    store
        .complete(
            &failed,
            InvocationOutcome::Error("exact provider-facing failure".to_string()),
        )
        .unwrap();

    let interrupted = store
        .begin(
            provider_context("provider", "interrupted"),
            exec_start(dir.path(), "sleep 30"),
        )
        .unwrap();
    drop(store);

    let _reopened = HistoryStore::open(path.clone(), Arc::from("runtime-b")).unwrap();
    let raw = LocalHistoryReader::query(
        &path,
        &HistoryQuery {
            last: 1,
            invocation_id: exec.invocation_id,
            include_raw: true,
            ..HistoryQuery::default()
        },
    )
    .unwrap()
    .pop()
    .unwrap();
    assert_eq!(raw.result, Some(exact_result));
    assert_eq!(raw.evidence_state, "complete");
    assert_eq!(raw.capture_state, "complete");
    assert_eq!(
        raw.full_output.as_deref(),
        Some("\u{1b}[31mraw-pty\u{1b}[0m\n")
    );

    let failed = LocalHistoryReader::query(
        &path,
        &HistoryQuery {
            last: 1,
            invocation_id: failed.invocation_id,
            ..HistoryQuery::default()
        },
    )
    .unwrap()
    .pop()
    .unwrap();
    assert_eq!(
        failed.error.as_deref(),
        Some("exact provider-facing failure")
    );
    assert_eq!(failed.evidence_state, "complete");

    let interrupted = LocalHistoryReader::query(
        &path,
        &HistoryQuery {
            last: 1,
            invocation_id: interrupted.invocation_id,
            ..HistoryQuery::default()
        },
    )
    .unwrap()
    .pop()
    .unwrap();
    assert_eq!(interrupted.evidence_state, "incomplete");
    assert_eq!(interrupted.capture_state, "incomplete");
    assert!(
        interrupted
            .evidence_reason
            .unwrap()
            .contains("previous Local runtime")
    );
    assert!(
        interrupted
            .capture_reason
            .unwrap()
            .contains("previous Local runtime")
    );
}

#[test]
fn locked_database_degrades_history_without_poisoning_later_envelopes() {
    let dir = tempdir().unwrap();
    let path = history_path(dir.path());
    let runtime = LocalHistoryRuntime::open(LocalHistoryRuntimeConfig::new(
        path.clone(),
        "runtime",
        365 * 86_400,
        1024 * 1024 * 1024,
    ))
    .unwrap();
    let locker = Connection::open(&path).unwrap();
    locker.execute_batch("BEGIN IMMEDIATE").unwrap();

    let result = runtime.begin(
        provider_context("provider", "blocked-envelope"),
        patch_start(dir.path(), "must not be admitted"),
    );
    assert!(
        result.is_err(),
        "locked history envelope should fail to persist"
    );
    assert!(runtime.history_degraded());

    locker.execute_batch("ROLLBACK").unwrap();
    let recovered = runtime
        .begin(
            provider_context("provider", "recovered-envelope"),
            patch_start(dir.path(), "history can recover"),
        )
        .expect("a prior history failure must not gate future envelopes");
    runtime
        .complete(&recovered, InvocationOutcome::Success(json!({"ok":true})))
        .unwrap();
    runtime.flush_for_test().unwrap();
    runtime.shutdown_blocking().unwrap();

    assert_eq!(
        LocalHistoryReader::query(&path, &HistoryQuery::recent(20))
            .unwrap()
            .len(),
        1
    );
    let status = LocalHistoryReader::status(&path).unwrap();
    assert_eq!(status.health_state, "degraded");
    assert!(
        status
            .health_reason
            .unwrap_or_default()
            .contains("invocation envelope persistence failed")
    );
}

#[test]
fn brief_sqlite_writer_contention_does_not_drop_an_invocation_envelope() {
    let dir = tempdir().unwrap();
    let path = history_path(dir.path());
    let runtime = LocalHistoryRuntime::open(LocalHistoryRuntimeConfig::new(
        path.clone(),
        "runtime",
        365 * 86_400,
        1024 * 1024 * 1024,
    ))
    .unwrap();
    let barrier = Arc::new(Barrier::new(2));
    let locker_path = path.clone();
    let locker_barrier = barrier.clone();
    let locker = std::thread::spawn(move || {
        let connection = Connection::open(locker_path).unwrap();
        connection.execute_batch("BEGIN IMMEDIATE").unwrap();
        locker_barrier.wait();
        std::thread::sleep(Duration::from_millis(20));
        connection.execute_batch("ROLLBACK").unwrap();
    });
    barrier.wait();

    let started = Instant::now();
    let context = runtime
        .begin(
            provider_context("provider", "brief-contention-envelope"),
            patch_start(dir.path(), "must survive brief writer contention"),
        )
        .expect("brief SQLite writer contention should be absorbed");
    assert!(
        started.elapsed() < Duration::from_millis(350),
        "bounded history admission waited too long: {:?}",
        started.elapsed()
    );
    locker.join().unwrap();
    runtime
        .complete(&context, InvocationOutcome::Success(json!({"ok":true})))
        .unwrap();
    runtime.flush_for_test().unwrap();
    runtime.shutdown_blocking().unwrap();

    let record = LocalHistoryReader::query(
        &path,
        &HistoryQuery {
            last: 1,
            invocation_id: context.invocation_id,
            ..HistoryQuery::default()
        },
    )
    .unwrap()
    .pop()
    .expect("briefly contended invocation should remain observable");
    assert_eq!(record.evidence_state, "complete");
}

#[test]
fn concurrent_foreground_invocation_burst_keeps_every_envelope() {
    let dir = tempdir().unwrap();
    let path = history_path(dir.path());
    let store = Arc::new(HistoryStore::open(path, Arc::from("runtime")).unwrap());
    let workdir = dir.path().to_path_buf();
    let workers = 8;
    let barrier = Arc::new(Barrier::new(workers + 1));
    let mut handles = Vec::with_capacity(workers);
    for index in 0..workers {
        let store = store.clone();
        let workdir = workdir.clone();
        let barrier = barrier.clone();
        handles.push(std::thread::spawn(move || {
            let correlation = format!("concurrent-envelope-{index}");
            let marker = format!("parallel patch {index}");
            barrier.wait();
            store.begin(
                provider_context("parallel-provider", &correlation),
                patch_start(&workdir, &marker),
            )
        }));
    }
    barrier.wait();

    let mut invocation_ids = handles
        .into_iter()
        .map(|handle| {
            handle
                .join()
                .expect("foreground history thread panicked")
                .expect("ordinary concurrent invocation burst should remain observable")
                .invocation_id
                .expect("persisted invocation must have a durable ID")
        })
        .collect::<Vec<_>>();
    invocation_ids.sort_unstable();
    invocation_ids.dedup();
    assert_eq!(invocation_ids.len(), workers);
}

#[test]
fn foreground_history_never_waits_for_the_background_connection_mutex() {
    let dir = tempdir().unwrap();
    let path = history_path(dir.path());
    let store = Arc::new(HistoryStore::open(path, Arc::from("runtime")).unwrap());
    let barrier = Arc::new(Barrier::new(2));
    let holder_store = store.clone();
    let holder_barrier = barrier.clone();
    let holder = std::thread::spawn(move || {
        let _connection = holder_store.lock_connection();
        holder_barrier.wait();
        std::thread::sleep(Duration::from_millis(200));
    });
    barrier.wait();

    let result = store.begin(
        provider_context("provider", "mutex-busy-envelope"),
        patch_start(dir.path(), "best effort only"),
    );
    assert!(
        result.is_ok(),
        "foreground history should use an independent SQLite connection"
    );
    holder.join().unwrap();
}

#[test]
fn unavailable_worker_never_blocks_tool_completion_for_history_recovery() {
    let dir = tempdir().unwrap();
    let path = history_path(dir.path());
    let runtime = LocalHistoryRuntime::open(LocalHistoryRuntimeConfig::new(
        path.clone(),
        "runtime",
        365 * 86_400,
        1024 * 1024 * 1024,
    ))
    .unwrap();
    let context = runtime
        .begin(
            provider_context("provider", "completion-fallback"),
            patch_start(dir.path(), "exact completion fallback"),
        )
        .unwrap();
    runtime.shutdown_blocking().unwrap();

    let exact = json!({"output":"exact result survives unavailable worker"});
    let started = Instant::now();
    runtime
        .complete(&context, InvocationOutcome::Success(exact.clone()))
        .unwrap();
    assert!(
        started.elapsed() < Duration::from_millis(50),
        "history recovery blocked tool completion: {:?}",
        started.elapsed()
    );
    assert!(runtime.history_degraded());

    let record = LocalHistoryReader::query(
        &path,
        &HistoryQuery {
            last: 1,
            invocation_id: context.invocation_id,
            ..HistoryQuery::default()
        },
    )
    .unwrap()
    .pop()
    .unwrap();
    assert_eq!(record.result, None);
    assert_eq!(record.evidence_state, "pending");
}

#[test]
fn busy_sqlite_load_sheds_only_the_noisy_capture_and_keeps_future_history_open() {
    let dir = tempdir().unwrap();
    let path = history_path(dir.path());
    let runtime = LocalHistoryRuntime::open(
        LocalHistoryRuntimeConfig::new(path.clone(), "runtime", 365 * 86_400, 1024 * 1024 * 1024)
            .with_output_queue_capacity(1),
    )
    .unwrap();
    let context = runtime
        .begin(
            provider_context("provider", "busy-output"),
            exec_start(dir.path(), "echo busy"),
        )
        .unwrap();
    let invocation_id = context.invocation_id.unwrap();

    std::thread::sleep(Duration::from_millis(50));
    let locker = Connection::open(&path).unwrap();
    locker.execute_batch("BEGIN IMMEDIATE").unwrap();

    runtime.observe_output(SessionOutputChunk {
        internal_session_id: 1,
        session_handle: Arc::from("historybusyhandle0000"),
        invocation: context.clone(),
        sequence: 0,
        text: "first".to_string(),
    });
    std::thread::sleep(Duration::from_millis(30));
    let started = Instant::now();
    for sequence in 1..20 {
        runtime.observe_output(SessionOutputChunk {
            internal_session_id: 1,
            session_handle: Arc::from("historybusyhandle0000"),
            invocation: context.clone(),
            sequence,
            text: "x".repeat(8192),
        });
    }
    assert!(
        started.elapsed() < Duration::from_millis(100),
        "PTY observer backpressured on SQLite: {:?}",
        started.elapsed()
    );
    assert!(
        !runtime.history_degraded(),
        "per-invocation output load shedding should not degrade the whole history store"
    );

    locker.execute_batch("ROLLBACK").unwrap();
    runtime.observe_output_complete(SessionOutputCompletion {
        internal_session_id: 1,
        session_handle: Arc::from("historybusyhandle0000"),
        invocation: context.clone(),
    });
    std::thread::sleep(Duration::from_millis(700));
    runtime
        .complete(
            &context,
            InvocationOutcome::Success(json!({"status":"exited","output":"truthful"})),
        )
        .expect("current invocation completion may still be queued after writer recovers");
    std::thread::sleep(Duration::from_millis(100));
    let next = runtime
        .begin(
            provider_context("provider", "still-open"),
            patch_start(dir.path(), "history continues"),
        )
        .expect("one noisy capture must not poison later history envelopes");
    runtime
        .complete(&next, InvocationOutcome::Success(json!({"ok":true})))
        .unwrap();
    runtime.shutdown_blocking().unwrap();

    let record = LocalHistoryReader::query(
        &path,
        &HistoryQuery {
            last: 1,
            invocation_id: Some(invocation_id),
            include_raw: true,
            ..HistoryQuery::default()
        },
    )
    .unwrap()
    .pop()
    .unwrap();
    assert_eq!(record.evidence_state, "complete");
    assert_eq!(record.capture_state, "incomplete");
    assert!(record.capture_reason.unwrap().contains("queue"));
    assert_eq!(record.result.unwrap()["output"], "truthful");
    let status = LocalHistoryReader::status(&path).unwrap();
    assert_eq!(status.health_state, "healthy");
}

#[test]
fn retention_removes_whole_old_units_recomputes_summaries_and_keeps_newest_over_budget() {
    let dir = tempdir().unwrap();
    let path = history_path(dir.path());
    let store = HistoryStore::open(path.clone(), Arc::from("runtime")).unwrap();
    let old = store
        .begin(
            provider_context("old-provider", "old"),
            exec_start(dir.path(), "old"),
        )
        .unwrap();
    let old_id = old.invocation_id.unwrap();
    store
        .persist_output_batch(&[
            OutputEvent::Chunk {
                invocation_id: old_id,
                agent_id: None,
                sequence: 0,
                observed_at_ms: now_ms().unwrap(),
                text: "old-output".repeat(5000),
            },
            OutputEvent::Complete {
                invocation_id: old_id,
                agent_id: None,
            },
        ])
        .unwrap();
    complete_ok(&store, &old, "old");

    let new = store
        .begin(
            provider_context("new-provider", "new"),
            patch_start(dir.path(), "new"),
        )
        .unwrap();
    complete_ok(&store, &new, "new");

    let connection = Connection::open(&path).unwrap();
    connection
        .execute(
            "UPDATE invocations SET started_at_ms = 1, completed_at_ms = 2 WHERE id = ?1",
            [old_id],
        )
        .unwrap();
    drop(connection);
    let before = store.physical_size().unwrap();
    // The fixture makes the old record epoch-old explicitly, so use a wide
    // age window that still expires it while keeping the freshly-created
    // record independent of host scheduling delays. A one-second wall-clock
    // window made this retention correctness test fail when the parallel test
    // runner was descheduled for more than a second.
    store
        .run_retention(365 * 24 * 60 * 60, u64::MAX, &[])
        .unwrap();
    let after_age = store.physical_size().unwrap();
    assert!(
        LocalHistoryReader::query(
            &path,
            &HistoryQuery {
                last: 10,
                invocation_id: Some(old_id),
                ..HistoryQuery::default()
            }
        )
        .unwrap()
        .is_empty()
    );
    assert!(after_age <= before);
    let connection = Connection::open(&path).unwrap();
    let chunk_count: i64 = connection
        .query_row("SELECT COUNT(*) FROM invocation_output_chunks", [], |row| {
            row.get(0)
        })
        .unwrap();
    let agent_count: i64 = connection
        .query_row("SELECT COUNT(*) FROM agents", [], |row| row.get(0))
        .unwrap();
    assert_eq!(
        chunk_count, 0,
        "whole invocation output must cascade with retention"
    );
    assert_eq!(agent_count, 1, "unsupported old Agent should be pruned");
    drop(connection);

    store.run_retention(u64::MAX, 1, &[]).unwrap();
    let records = LocalHistoryReader::query(&path, &HistoryQuery::recent(20)).unwrap();
    assert_eq!(
        records.len(),
        1,
        "newest invocation is retained even over size budget"
    );
    assert_eq!(records[0].id, new.invocation_id.unwrap());
    let status = LocalHistoryReader::status(&path).unwrap();
    assert!(status.over_budget);
}

#[test]
fn active_process_creator_invocation_survives_retention_even_when_capture_is_incomplete() {
    let dir = tempdir().unwrap();
    let path = history_path(dir.path());
    let runtime = LocalHistoryRuntime::open(LocalHistoryRuntimeConfig::new(
        path.clone(),
        "runtime",
        365 * 86_400,
        1024 * 1024 * 1024,
    ))
    .unwrap();
    let context = runtime
        .begin(
            provider_context("active-provider", "active-process"),
            exec_start(dir.path(), "sleep 30"),
        )
        .unwrap();
    let invocation_id = context.invocation_id.unwrap();
    runtime
        .complete(
            &context,
            InvocationOutcome::Success(json!({
                "status":"running",
                "output":"",
                "summary":"running",
                "cwd":dir.path(),
                "session_handle":"active-handle",
                "exit_code":null,
                "termination_reason":null
            })),
        )
        .unwrap();
    runtime.flush_for_test().unwrap();
    let connection = Connection::open(&path).unwrap();
    connection
        .execute(
            "UPDATE invocations
             SET started_at_ms = 1, completed_at_ms = 2,
                 capture_state = 'incomplete', capture_reason = 'injected loss'
             WHERE id = ?1",
            [invocation_id],
        )
        .unwrap();
    drop(connection);

    let process = OwnedProcess {
        internal_session_id: 1,
        session_handle: Arc::from("active-handle"),
        identity: ProcessIdentity {
            pid: 4242,
            birth: ProcessBirthIdentity::LinuxProcStartTicks { ticks: 99 },
        },
        created_by: context.clone(),
    };
    runtime.process_started(&process).unwrap();
    runtime.run_retention_now(1, u64::MAX).unwrap();
    assert_eq!(
        LocalHistoryReader::query(
            &path,
            &HistoryQuery {
                last: 1,
                invocation_id: Some(invocation_id),
                ..HistoryQuery::default()
            }
        )
        .unwrap()
        .len(),
        1,
        "active creator evidence must be retention-protected"
    );

    runtime
        .process_ended(
            &process,
            &OwnedProcessEnd::exited(0, TerminationReason::Exit, dir.path().display().to_string()),
        )
        .unwrap();
    runtime.flush_for_test().unwrap();
    runtime.run_retention_now(1, u64::MAX).unwrap();
    assert!(
        LocalHistoryReader::query(
            &path,
            &HistoryQuery {
                last: 1,
                invocation_id: Some(invocation_id),
                ..HistoryQuery::default()
            }
        )
        .unwrap()
        .is_empty(),
        "ended process no longer needs its expired creator evidence"
    );
    runtime.shutdown_blocking().unwrap();
}

#[test]
fn size_retention_after_restart_deletes_oldest_complete_unit_and_reclaims_physical_pages() {
    let dir = tempdir().unwrap();
    let path = history_path(dir.path());
    let store = HistoryStore::open(path.clone(), Arc::from("runtime-a")).unwrap();
    let mut invocation_ids = Vec::new();
    for (session, marker) in [("provider-a", "a"), ("provider-b", "b")] {
        let context = store
            .begin(
                provider_context(session, marker),
                exec_start(dir.path(), marker),
            )
            .unwrap();
        let invocation_id = context.invocation_id.unwrap();
        invocation_ids.push(invocation_id);
        store
            .persist_output_batch(&[
                OutputEvent::Chunk {
                    invocation_id,
                    agent_id: None,
                    sequence: 0,
                    observed_at_ms: now_ms().unwrap(),
                    text: marker.repeat(768 * 1024),
                },
                OutputEvent::Complete {
                    invocation_id,
                    agent_id: None,
                },
            ])
            .unwrap();
        complete_ok(&store, &context, marker);
    }
    let before = store.physical_size().unwrap();
    assert!(
        before > 1024 * 1024,
        "fixture should create a sizeable store"
    );
    drop(store);

    let reopened = HistoryStore::open(path.clone(), Arc::from("runtime-b")).unwrap();
    let budget = before.saturating_mul(3) / 4;
    reopened.run_retention(u64::MAX, budget, &[]).unwrap();
    let after = reopened.physical_size().unwrap();
    assert!(
        after < before,
        "physical retention must reclaim pages/WAL, before={before} after={after}"
    );
    assert!(
        before.saturating_sub(after) > 64 * 1024,
        "bounded retention must drain multiple vacuum steps, before={before} after={after}"
    );
    drop(reopened);

    let records = LocalHistoryReader::query(&path, &HistoryQuery::recent(20)).unwrap();
    assert_eq!(
        records.len(),
        1,
        "size retention keeps one complete newest unit"
    );
    assert_eq!(records[0].id, invocation_ids[1]);
    assert!(
        LocalHistoryReader::query(
            &path,
            &HistoryQuery {
                last: 1,
                invocation_id: Some(invocation_ids[0]),
                ..HistoryQuery::default()
            }
        )
        .unwrap()
        .is_empty(),
        "oldest complete invocation must be deleted as a whole unit"
    );
    let status = LocalHistoryReader::status(&path).unwrap();
    assert!(
        status.physical_size_bytes < before,
        "status should still observe a reclaimed physical store"
    );
    assert_eq!(
        status.over_budget,
        status.physical_size_bytes > budget,
        "retention budget state must describe the footprint it reports"
    );
}
