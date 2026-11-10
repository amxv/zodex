//! Producer regressions deliberately keep both storage locks held until a
//! separate producer has signalled completion. A slow storage path cannot pass
//! merely by finishing a little faster than a generous elapsed-time assertion.
use std::collections::HashSet;
use std::path::Path;
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use rusqlite::Connection;
use serde_json::json;

use super::query::{HistoryInvocation, HistoryQuery, LocalHistoryReader};
use super::store::{HistoryStore, now_ms};
use super::worker::{LocalHistoryRuntime, LocalHistoryRuntimeConfig};
use crate::invocation::{
    InvocationContext, InvocationContinuationKind, InvocationEvidenceRecorder, InvocationOutcome,
    InvocationStart, ProviderCallMetadata,
};
use crate::protocol::TerminationReason;
use crate::session::{
    OwnedProcess, OwnedProcessEnd, OwnedProcessObserver, ProcessBirthIdentity, ProcessIdentity,
    SessionOutputChunk, SessionOutputCompletion, SessionOutputObserver,
};

fn fixture(path: &Path) -> (Arc<HistoryStore>, Arc<LocalHistoryRuntime>) {
    let store = Arc::new(HistoryStore::open(path.to_owned(), Arc::from("async-tests")).unwrap());
    let runtime = LocalHistoryRuntime::open_with_store(
        LocalHistoryRuntimeConfig::new(
            path.to_owned(),
            "async-tests",
            365 * 86_400,
            1024 * 1024 * 1024,
        ),
        Some(store.clone()),
    )
    .unwrap();
    (store, runtime)
}

fn context(provider: &str, correlation: &str) -> InvocationContext {
    InvocationContext::default()
        .with_provider(ProviderCallMetadata::new("openai/session", provider))
        .with_correlation_id(correlation)
}

fn records(path: &Path, last: usize) -> Vec<HistoryInvocation> {
    LocalHistoryReader::query(
        path,
        &HistoryQuery {
            last,
            include_raw: true,
            ..HistoryQuery::default()
        },
    )
    .unwrap()
}

fn wait_for_records(
    runtime: &LocalHistoryRuntime,
    path: &Path,
    count: usize,
) -> Vec<HistoryInvocation> {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        runtime.flush_for_test().unwrap();
        let rows = records(path, count + 1);
        if rows.len() >= count && rows.iter().all(|row| row.completed_at_ms.is_some()) {
            return rows;
        }
        assert!(
            Instant::now() < deadline,
            "background history did not drain: {} of {count} rows",
            rows.len()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn async_producers_finish_32_calls_while_both_storage_locks_remain_held() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.sqlite3");
    let (store, runtime) = fixture(&path);
    let storage = store.lock_connection();
    let locker = Connection::open(&path).unwrap();
    locker.execute_batch("BEGIN IMMEDIATE").unwrap();
    let before = now_ms().unwrap();
    let (sent, received) = mpsc::channel();
    let producer_runtime = runtime.clone();
    let producer = std::thread::spawn(move || {
        let mut admitted = Vec::new();
        for index in 0..32 {
            let args = json!({"cmd":format!("printf '{index}'"),"workdir":"/tmp","marker":index});
            let context = producer_runtime
                .begin(
                    context("one-provider", &format!("call-{index}")),
                    InvocationStart::new("exec_command", args),
                )
                .unwrap();
            producer_runtime.observe_output(SessionOutputChunk {
                internal_session_id: index + 1,
                session_handle: Arc::from(format!("handle-{index}")),
                invocation: context.clone(),
                sequence: 0,
                text: format!("raw-{index}\n"),
            });
            producer_runtime.observe_output_complete(SessionOutputCompletion {
                internal_session_id: index + 1,
                session_handle: Arc::from(format!("handle-{index}")),
                invocation: context.clone(),
            });
            producer_runtime.complete(&context, InvocationOutcome::Success(json!({"status":"exited","output":format!("result-{index}"),"marker":index}))).unwrap();
            admitted.push(context);
        }
        sent.send((admitted, now_ms().unwrap())).unwrap();
    });
    let finished_while_locked = received.recv_timeout(Duration::from_millis(250));
    // Cleanup precedes assertions, including timeout/panic paths.
    locker.execute_batch("ROLLBACK").unwrap();
    drop(storage);
    producer.join().unwrap();
    let (admitted, producer_finished) =
        finished_while_locked.expect("history producers waited on storage");
    let rows = wait_for_records(&runtime, &path, 32);
    assert_eq!(
        admitted
            .iter()
            .map(|entry| entry.agent_id.clone())
            .collect::<HashSet<_>>()
            .len(),
        1
    );
    assert_eq!(
        admitted
            .iter()
            .map(|entry| entry.invocation_id)
            .collect::<HashSet<_>>()
            .len(),
        32
    );
    for (index, context) in admitted.iter().enumerate() {
        let row = rows
            .iter()
            .find(|row| Some(row.id) == context.invocation_id)
            .unwrap();
        assert_eq!(
            row.arguments,
            json!({"cmd":format!("printf '{index}'"),"workdir":"/tmp","marker":index})
        );
        assert_eq!(
            row.result,
            Some(json!({"status":"exited","output":format!("result-{index}"),"marker":index}))
        );
        assert_eq!(
            row.full_output.as_deref(),
            Some(format!("raw-{index}\n").as_str())
        );
        assert_eq!(row.agent_id.as_deref(), context.agent_id.as_deref());
        assert!(row.started_at_ms >= before && row.started_at_ms <= producer_finished);
        assert!(
            row.completed_at_ms.unwrap() >= row.started_at_ms
                && row.completed_at_ms.unwrap() <= producer_finished
        );
        assert_eq!(row.capture_state, "complete");
        assert_eq!(row.evidence_state, "complete");
    }
    runtime.shutdown_blocking().unwrap();
}

#[test]
fn async_identity_claims_and_process_lifecycle_do_not_wait_for_storage() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.sqlite3");
    let (store, runtime) = fixture(&path);
    let storage = store.lock_connection();
    let locker = Connection::open(&path).unwrap();
    locker.execute_batch("BEGIN IMMEDIATE").unwrap();
    let (sent, received) = mpsc::channel();
    let history = runtime.clone();
    let producer = std::thread::spawn(move || {
        let identified = history
            .identify_agent(context("lifecycle-provider", "identity-only"))
            .unwrap();
        let provider = identified.provider.as_ref().unwrap();
        let claims = [
            history.claim_global_context_injection(provider).unwrap(),
            history.claim_repo_agents_check(provider, "/tmp").unwrap(),
            history.claim_repo_skills_check(provider, "/tmp").unwrap(),
        ];
        let repeated = [
            history.claim_global_context_injection(provider).unwrap(),
            history.claim_repo_agents_check(provider, "/tmp").unwrap(),
            history.claim_repo_skills_check(provider, "/tmp").unwrap(),
        ];
        let creator = history
            .begin(
                context("lifecycle-provider", "lifecycle-call"),
                InvocationStart::new("exec_command", json!({"cmd":"true", "workdir":"/tmp"})),
            )
            .unwrap();
        let process = OwnedProcess {
            internal_session_id: 1,
            session_handle: Arc::from("async-lifecycle"),
            identity: ProcessIdentity {
                pid: 4242,
                birth: ProcessBirthIdentity::LinuxProcStartTicks { ticks: 99 },
            },
            created_by: creator.clone(),
        };
        history.process_started(&process).unwrap();
        let active = history.active_process_count();
        history
            .complete(
                &creator,
                InvocationOutcome::Success(
                    json!({"status":"running","session_handle":"async-lifecycle"}),
                ),
            )
            .unwrap();
        history
            .process_ended(
                &process,
                &OwnedProcessEnd::exited(7, TerminationReason::Exit, "/tmp".to_owned()),
            )
            .unwrap();
        history.observe_output_complete(SessionOutputCompletion {
            internal_session_id: 1,
            session_handle: Arc::from("async-lifecycle"),
            invocation: creator.clone(),
        });
        sent.send((identified, creator, claims, repeated, active))
            .unwrap();
    });
    let finished_while_locked = received.recv_timeout(Duration::from_millis(250));
    locker.execute_batch("ROLLBACK").unwrap();
    drop(storage);
    producer.join().unwrap();
    let (identified, creator, claims, repeated, active) =
        finished_while_locked.expect("identity, claims or lifecycle callback waited on storage");
    assert_eq!(claims, [true; 3]);
    assert_eq!(repeated, [false; 3]);
    assert_eq!(active, 1);
    assert_eq!(identified.agent_id, creator.agent_id);
    let rows = wait_for_records(&runtime, &path, 1);
    assert_eq!(rows[0].process_state.as_deref(), Some("exited"));
    assert_eq!(rows[0].process_exit_code, Some(7));
    assert_eq!(rows[0].result.as_ref().unwrap()["status"], "running");
    assert_eq!(runtime.active_process_count(), 0);
    runtime.shutdown_blocking().unwrap();
}

#[test]
fn async_cross_agent_poll_keeps_reserved_creator_identity_after_delayed_admission() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.sqlite3");
    let (store, runtime) = fixture(&path);
    let storage = store.lock_connection();
    let locker = Connection::open(&path).unwrap();
    locker.execute_batch("BEGIN IMMEDIATE").unwrap();
    let (sent, received) = mpsc::channel();
    let history = runtime.clone();
    let producer = std::thread::spawn(move || {
        let creator = history
            .begin(
                context("creator-provider", "creator-call"),
                InvocationStart::new("exec_command", json!({"cmd":"true","workdir":"/tmp"})),
            )
            .unwrap();
        let poll = history
            .begin(
                context("poller-provider", "poll-call"),
                InvocationStart::new(
                    "write_stdin",
                    json!({"session_handle":"cross-agent-handle","workdir":"/tmp"}),
                )
                .with_target_created_by_agent_id(creator.agent_id.clone())
                .with_target_created_by_invocation_id(creator.invocation_id)
                .with_continuation_kind(InvocationContinuationKind::Poll),
            )
            .unwrap();
        // Complete the dependent invocation first to stress admission ordering.
        history
            .complete(
                &poll,
                InvocationOutcome::Success(json!({"status":"exited","output":"polled"})),
            )
            .unwrap();
        history
            .complete(
                &creator,
                InvocationOutcome::Success(
                    json!({"status":"running","session_handle":"cross-agent-handle"}),
                ),
            )
            .unwrap();
        history.observe_output_complete(SessionOutputCompletion {
            internal_session_id: 1,
            session_handle: Arc::from("cross-agent-handle"),
            invocation: creator.clone(),
        });
        sent.send((creator, poll)).unwrap();
    });
    let finished_while_locked = received.recv_timeout(Duration::from_millis(250));
    locker.execute_batch("ROLLBACK").unwrap();
    drop(storage);
    producer.join().unwrap();
    let (creator, poll) =
        finished_while_locked.expect("poll admission waited on creator persistence");
    let rows = wait_for_records(&runtime, &path, 2);
    let record = rows
        .iter()
        .find(|row| Some(row.id) == poll.invocation_id)
        .unwrap();
    assert_ne!(creator.agent_id, poll.agent_id);
    assert_eq!(record.agent_id.as_deref(), poll.agent_id.as_deref());
    assert_eq!(
        record.target_created_by_agent_id.as_deref(),
        creator.agent_id.as_deref()
    );
    assert_eq!(
        record.target_created_by_invocation_id,
        creator.invocation_id
    );
    assert_eq!(record.cross_agent, Some(true));
    assert_eq!(record.continuation_kind.as_deref(), Some("poll"));
    assert_eq!(record.result.as_ref().unwrap()["output"], "polled");
    runtime.shutdown_blocking().unwrap();
}

#[test]
fn async_admission_saturation_is_bounded_and_accepts_future_calls_after_recovery() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.sqlite3");
    let (store, runtime) = fixture(&path);
    let storage = store.lock_connection();
    let locker = Connection::open(&path).unwrap();
    locker.execute_batch("BEGIN IMMEDIATE").unwrap();
    let (sent, received) = mpsc::channel();
    let history = runtime.clone();
    let producer = std::thread::spawn(move || {
        let mut accepted = 0;
        let mut omitted = 0;
        let mut executed = 0;
        for index in 0..1100 {
            let admission = history.begin(
                context("saturation-provider", &format!("saturated-{index}")),
                InvocationStart::new("apply_patch", json!({"workdir":"/tmp","marker":index})),
            );
            // The invocation recorder reports evidence failure only. The caller
            // continues its handler regardless, as MCP begin_invocation does.
            executed += 1;
            match admission {
                Ok(context) => {
                    accepted += 1;
                    history
                        .complete(
                            &context,
                            InvocationOutcome::Success(json!({"marker":index})),
                        )
                        .unwrap();
                }
                Err(_) => omitted += 1,
            }
        }
        sent.send((accepted, omitted, executed, history.history_degraded()))
            .unwrap();
    });
    let finished_while_locked = received.recv_timeout(Duration::from_millis(250));
    locker.execute_batch("ROLLBACK").unwrap();
    drop(storage);
    producer.join().unwrap();
    let (accepted, omitted, executed, degraded) =
        finished_while_locked.expect("saturated admission waited on storage");
    assert_eq!(accepted, 1024);
    assert_eq!(omitted, 76);
    assert_eq!(executed, 1100);
    assert!(degraded);
    wait_for_records(&runtime, &path, accepted);
    let future = runtime
        .begin(
            context("saturation-provider", "after-recovery"),
            InvocationStart::new("apply_patch", json!({"workdir":"/tmp","marker":"future"})),
        )
        .unwrap();
    runtime
        .complete(&future, InvocationOutcome::Success(json!({"ok":true})))
        .unwrap();
    let rows = wait_for_records(&runtime, &path, accepted + 1);
    let row = rows
        .iter()
        .find(|row| Some(row.id) == future.invocation_id)
        .unwrap();
    assert_eq!(row.result, Some(json!({"ok":true})));
    assert_eq!(
        LocalHistoryReader::status(&path).unwrap().health_state,
        "degraded"
    );
    runtime.shutdown_blocking().unwrap();
}

#[test]
fn admitted_output_retries_transient_storage_failure_without_loss_or_duplicates() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.sqlite3");
    let (_store, runtime) = fixture(&path);
    let invocation = runtime
        .begin(
            context("retry-output", "already-admitted-output"),
            InvocationStart::new(
                "exec_command",
                json!({"cmd":"printf output","workdir":"/tmp"}),
            ),
        )
        .unwrap();
    runtime.flush_for_test().unwrap();
    assert!(!runtime.history_degraded());
    assert_eq!(records(&path, 1).len(), 1);

    let locker = Connection::open(&path).unwrap();
    locker.execute_batch("BEGIN IMMEDIATE").unwrap();
    let observed_before = now_ms().unwrap();
    let (sent, received) = mpsc::channel();
    let history = runtime.clone();
    let producer_context = invocation.clone();
    let expected_result =
        json!({"status":"exited","exit_code":0,"output":"exact response","marker":{"retry":true}});
    let producer = std::thread::spawn(move || {
        for (sequence, text) in ["first\n", "second 世界\n", "third\n"]
            .into_iter()
            .enumerate()
        {
            history.observe_output(SessionOutputChunk {
                internal_session_id: 901,
                session_handle: Arc::from("retry-output-handle"),
                invocation: producer_context.clone(),
                sequence: sequence as u64,
                text: text.to_owned(),
            });
        }
        history.observe_output_complete(SessionOutputCompletion {
            internal_session_id: 901,
            session_handle: Arc::from("retry-output-handle"),
            invocation: producer_context.clone(),
        });
        sent.send(now_ms().unwrap()).unwrap();
    });
    let producer_finished = received.recv_timeout(Duration::from_millis(250));
    // A successful enqueue alone does not exercise retry: keep the independent
    // writer lock beyond the actual background busy timeout and observe failure.
    std::thread::sleep(super::schema::WRITER_BUSY_TIMEOUT + Duration::from_millis(100));
    let deadline = Instant::now() + Duration::from_secs(5);
    while !runtime.history_degraded() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    let retried = runtime.history_degraded();
    // Only output was enqueued so far, proving that failure exercised output
    // persistence itself rather than merely the result writer.
    let (completion_sent, completion_received) = mpsc::channel();
    let history = runtime.clone();
    let producer_context = invocation.clone();
    let producer_result = expected_result.clone();
    let completion = std::thread::spawn(move || {
        history
            .complete(
                &producer_context,
                InvocationOutcome::Success(producer_result),
            )
            .unwrap();
        completion_sent.send(now_ms().unwrap()).unwrap();
    });
    let completion_finished = completion_received.recv_timeout(Duration::from_millis(250));
    let released_at = now_ms().unwrap();
    locker.execute_batch("ROLLBACK").unwrap();
    producer.join().unwrap();
    completion.join().unwrap();
    let completion_finished = completion_finished.expect("result callback waited for storage");
    let producer_finished =
        producer_finished.expect("admitted output callbacks waited for storage");
    assert!(
        retried,
        "external lock must cause a real writer failure before release"
    );
    assert!(producer_finished < released_at);
    assert!(completion_finished <= released_at);

    let rows = wait_for_records(&runtime, &path, 1);
    runtime.flush_for_test().unwrap();
    let row = &rows[0];
    assert_eq!(Some(row.id), invocation.invocation_id);
    assert_eq!(row.result, Some(expected_result));
    assert_eq!(
        row.full_output.as_deref(),
        Some("first\nsecond 世界\nthird\n")
    );
    assert_eq!(row.capture_state, "complete");
    assert!(row.capture_reason.is_none());
    assert_eq!(row.evidence_state, "complete");
    assert!((observed_before..=completion_finished).contains(&row.completed_at_ms.unwrap()));
    let connection = Connection::open(&path).unwrap();
    let mut statement = connection.prepare("SELECT sequence, observed_at_ms, text FROM invocation_output_chunks WHERE invocation_id = ?1 ORDER BY sequence").unwrap();
    let chunks = statement
        .query_map([row.id], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(chunks.len(), 3);
    for (index, ((sequence, timestamp, text), expected)) in chunks
        .iter()
        .zip(["first\n", "second 世界\n", "third\n"])
        .enumerate()
    {
        assert_eq!(*sequence, index as i64);
        assert_eq!(text, expected);
        assert!((observed_before..=producer_finished).contains(timestamp));
    }
    drop(statement);
    drop(connection);
    runtime.shutdown_blocking().unwrap();
}

#[test]
fn admitted_completion_retry_preserves_exact_result_and_original_timestamp() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.sqlite3");
    let (_store, runtime) = fixture(&path);
    let invocation = runtime
        .begin(
            context("retry-completion", "already-admitted-completion"),
            InvocationStart::new("test_tool", json!({"marker":"completion-retry"})),
        )
        .unwrap();
    runtime.flush_for_test().unwrap();
    assert!(!runtime.history_degraded());
    let locker = Connection::open(&path).unwrap();
    locker.execute_batch("BEGIN IMMEDIATE").unwrap();
    let before = now_ms().unwrap();
    let expected = json!({"output":"keep this exact result","nested":[1,"two",{"three":true}]});
    let (sent, received) = mpsc::channel();
    let history = runtime.clone();
    let producer_context = invocation.clone();
    let producer_result = expected.clone();
    let producer = std::thread::spawn(move || {
        history
            .complete(
                &producer_context,
                InvocationOutcome::Success(producer_result),
            )
            .unwrap();
        sent.send(now_ms().unwrap()).unwrap();
    });
    let finished = received.recv_timeout(Duration::from_millis(250));
    std::thread::sleep(super::schema::WRITER_BUSY_TIMEOUT + Duration::from_millis(100));
    let deadline = Instant::now() + Duration::from_secs(5);
    while !runtime.history_degraded() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    let retried = runtime.history_degraded();
    let released_at = now_ms().unwrap();
    locker.execute_batch("ROLLBACK").unwrap();
    producer.join().unwrap();
    let finished = finished.expect("completion callback waited for storage");
    assert!(
        retried,
        "completion must actually fail and enter retry before lock release"
    );
    assert!(finished < released_at);
    let rows = wait_for_records(&runtime, &path, 1);
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(Some(row.id), invocation.invocation_id);
    assert_eq!(row.result, Some(expected));
    assert_eq!(row.evidence_state, "complete");
    assert!(
        (before..=finished).contains(&row.completed_at_ms.unwrap()),
        "retry must retain enqueue-time completion, not use storage recovery time"
    );
    runtime.shutdown_blocking().unwrap();
}
