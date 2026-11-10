use std::fmt::Write as _;
use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use crate::local::presentation::sanitize_display_text;

use super::query::{HistoryFormat, HistoryStoreStatus, LocalHistoryReader, open_read_only};
use super::schema::readable_schema_version;

/// Tool-call times describe retained evidence, not the lifetime of a model session.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HistoryAgentTiming {
    pub agent_id: String,
    pub first_tool_call_at_ms: Option<i64>,
    pub latest_tool_call_at_ms: Option<i64>,
    pub latest_tool_call_id: Option<i64>,
    pub latest_tool_name: Option<String>,
    pub latest_tool_call_completed_at_ms: Option<i64>,
    pub last_completed_tool_call_at_ms: Option<i64>,
    pub elapsed_since_latest_tool_call_ms: Option<u64>,
    pub clock_skew_detected: bool,
    pub retained_tool_call_count: u64,
    pub unfinished_tool_call_count: u64,
    pub incomplete_evidence_count: u64,
    /// A persisted observation, not a live process probe. None means old schema.
    pub recorded_running_process_count: Option<u64>,
    pub incomplete_process_count: Option<u64>,
    pub history_gaps: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HistoryAgentTimingDocument {
    pub schema_version: u32,
    pub as_of_ms: i64,
    pub scope: String,
    pub agent_filter: Option<String>,
    pub store: HistoryStoreStatus,
    pub unattributed_tool_call_count: u64,
    pub history_gaps: Vec<String>,
    pub agents: Vec<HistoryAgentTiming>,
}

impl LocalHistoryReader {
    /// Aggregate all retained raw calls, including polls, without loading their
    /// arguments/output or applying the invocation viewer's default row limit.
    /// Supplying the clock makes elapsed-time behavior reproducible in tests.
    pub fn agent_timings(
        path: &Path,
        agent_id: Option<&str>,
        as_of_ms: Option<i64>,
    ) -> Result<HistoryAgentTimingDocument> {
        let mut document = HistoryAgentTimingDocument {
            schema_version: 1,
            as_of_ms: 0,
            scope: "retained_tool_calls".into(),
            agent_filter: agent_id.map(str::to_owned),
            store: Self::status(path)?,
            unattributed_tool_call_count: 0,
            history_gaps: Vec::new(),
            agents: Vec::new(),
        };
        if !document.store.database_exists {
            document
                .history_gaps
                .push("history_database_missing".into());
            document.set_clock(as_of_ms);
            return Ok(document);
        }
        // All logical evidence and health flags come from one read snapshot.
        // No schema migration, maintenance, or runtime connection is needed.
        let mut connection = open_read_only(path)?;
        let transaction = connection.transaction()?;
        let schema_version = readable_schema_version(&transaction)?;
        let store = &mut document.store;
        (
            store.health_state,
            store.health_reason,
            store.over_budget,
            store.last_retention_error,
        ) = transaction
            .query_row(
                "SELECT health_state, health_reason, over_budget, last_retention_error
                 FROM history_state WHERE singleton = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .context("failed to read Local agent timing history health")?;
        if store.health_state != "healthy" {
            document.history_gaps.push("history_store_unhealthy".into());
        }
        if store.last_retention_error.is_some() {
            document.history_gaps.push("retention_error".into());
        }
        let lifecycle = if schema_version >= 4 {
            "SUM(CASE WHEN process_state = 'running' THEN 1 ELSE 0 END) AS running,
             SUM(CASE WHEN process_state = 'incomplete' THEN 1 ELSE 0 END) AS incomplete_processes"
        } else {
            document
                .history_gaps
                .push("process_lifecycle_unavailable".into());
            "NULL AS running, NULL AS incomplete_processes"
        };
        let sql = format!(
            "SELECT a.id, s.first_call, s.latest_call, s.last_completed,
                    COALESCE(s.calls, 0), COALESCE(s.unfinished, 0),
                    COALESCE(s.incomplete, 0), s.running, s.incomplete_processes,
                    latest.id, latest.tool_name, latest.completed_at_ms
             FROM agents a LEFT JOIN (
                 SELECT agent_id, MIN(started_at_ms) AS first_call,
                        MAX(started_at_ms) AS latest_call,
                        MAX(completed_at_ms) AS last_completed, COUNT(*) AS calls,
                        SUM(CASE WHEN completed_at_ms IS NULL THEN 1 ELSE 0 END) AS unfinished,
                        SUM(CASE WHEN evidence_state = 'incomplete' OR capture_state = 'incomplete'
                                 THEN 1 ELSE 0 END) AS incomplete,
                        {lifecycle}
                 FROM invocations WHERE (?1 IS NULL OR agent_id = ?1)
                 GROUP BY agent_id
             ) AS s
             ON s.agent_id = a.id
             LEFT JOIN invocations latest ON latest.id = (
                 SELECT id FROM invocations WHERE agent_id = a.id
                 ORDER BY started_at_ms DESC, id DESC LIMIT 1
             )
             WHERE (?1 IS NULL OR a.id = ?1)
             ORDER BY s.latest_call DESC, a.id ASC"
        );
        let mut statement = transaction
            .prepare(&sql)
            .context("failed to prepare Local agent timing query")?;
        document.agents = statement
            .query_map(params![agent_id], |row| {
                let latest: Option<i64> = row.get(2)?;
                let last_completed: Option<i64> = row.get(3)?;
                let count = row.get::<_, i64>(4)? as u64;
                let incomplete = row.get::<_, i64>(6)? as u64;
                let incomplete_processes = row.get::<_, Option<i64>>(8)?.map(|value| value as u64);
                let mut gaps = Vec::new();
                if count == 0 {
                    gaps.push("no_retained_tool_calls".into());
                }
                if incomplete > 0 {
                    gaps.push("incomplete_tool_evidence".into());
                }
                if incomplete_processes.is_some_and(|count| count > 0) {
                    gaps.push("incomplete_process_evidence".into());
                }
                Ok(HistoryAgentTiming {
                    agent_id: row.get(0)?,
                    first_tool_call_at_ms: row.get(1)?,
                    latest_tool_call_at_ms: latest,
                    latest_tool_call_id: row.get(9)?,
                    latest_tool_name: row.get(10)?,
                    latest_tool_call_completed_at_ms: row.get(11)?,
                    last_completed_tool_call_at_ms: last_completed,
                    elapsed_since_latest_tool_call_ms: None,
                    clock_skew_detected: false,
                    retained_tool_call_count: count,
                    unfinished_tool_call_count: row.get::<_, i64>(5)? as u64,
                    incomplete_evidence_count: incomplete,
                    recorded_running_process_count: if schema_version >= 4 {
                        Some(row.get::<_, Option<i64>>(7)?.unwrap_or(0) as u64)
                    } else {
                        None
                    },
                    incomplete_process_count: if schema_version >= 4 {
                        Some(incomplete_processes.unwrap_or(0))
                    } else {
                        None
                    },
                    history_gaps: gaps,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("failed to read Local agent timing evidence")?;
        // This count is store-wide even when one Agent is selected: these calls
        // cannot truthfully be assigned to any individual Agent.
        document.unattributed_tool_call_count = transaction.query_row(
            "SELECT COUNT(*) FROM invocations WHERE agent_id IS NULL",
            [],
            |row| row.get::<_, i64>(0),
        )? as u64;
        document.set_clock(as_of_ms);
        Ok(document)
    }
}

impl HistoryAgentTimingDocument {
    fn set_clock(&mut self, as_of_ms: Option<i64>) {
        // Sample the real clock after reading the snapshot so concurrent new
        // calls cannot appear to come from the future merely due to query time.
        self.as_of_ms = as_of_ms.unwrap_or_else(|| {
            (OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000) as i64
        });
        for agent in &mut self.agents {
            agent.elapsed_since_latest_tool_call_ms = agent
                .latest_tool_call_at_ms
                .and_then(|time| u64::try_from(i128::from(self.as_of_ms) - i128::from(time)).ok());
            agent.clock_skew_detected = agent
                .latest_tool_call_at_ms
                .is_some_and(|time| time > self.as_of_ms)
                || agent
                    .last_completed_tool_call_at_ms
                    .is_some_and(|time| time > self.as_of_ms);
        }
    }

    pub fn render(&self, format: HistoryFormat) -> Result<String> {
        if format == HistoryFormat::Json {
            return serde_json::to_string_pretty(self)
                .context("failed to render Local agent timing JSON");
        }
        let mut text = format!(
            "Local Agent tool-call timing as of {}\nScope: retained raw tool calls, including polls. Retention and recording gaps can hide earlier or later calls.\nHistory store: {}\n",
            timestamp(Some(self.as_of_ms)),
            sanitize_display_text(&self.store.health_state),
        );
        if let Some(reason) = &self.store.health_reason {
            let _ = writeln!(text, "History detail: {}", sanitize_display_text(reason));
        }
        if !self.history_gaps.is_empty() {
            let _ = writeln!(text, "History gaps: {}", self.history_gaps.join(", "));
        }
        if self.agents.is_empty() {
            let _ = writeln!(
                text,
                "No retained Agent matches{}.",
                self.agent_filter
                    .as_ref()
                    .map(|id| format!(" {}", sanitize_display_text(id)))
                    .unwrap_or_default()
            );
        }
        for agent in &self.agents {
            let _ = writeln!(text, "\nAgent {}", sanitize_display_text(&agent.agent_id));
            let _ = writeln!(
                text,
                "  First tool call: {}",
                timestamp(agent.first_tool_call_at_ms)
            );
            let _ = writeln!(
                text,
                "  Latest tool call: {}",
                timestamp(agent.latest_tool_call_at_ms)
            );
            let _ = writeln!(
                text,
                "  Since latest call: {}",
                agent
                    .elapsed_since_latest_tool_call_ms
                    .map(elapsed)
                    .unwrap_or_else(|| "unknown".into())
            );
            if let Some(id) = agent.latest_tool_call_id {
                let _ = writeln!(
                    text,
                    "  Latest invocation: #{id} {}",
                    sanitize_display_text(agent.latest_tool_name.as_deref().unwrap_or("unknown"))
                );
            }
            let _ = writeln!(
                text,
                "  Latest call completed: {}",
                timestamp(agent.latest_tool_call_completed_at_ms)
            );
            let _ = writeln!(
                text,
                "  Last completed call: {}",
                timestamp(agent.last_completed_tool_call_at_ms)
            );
            let _ = writeln!(
                text,
                "  Retained calls: {}; unfinished calls: {}; incomplete evidence: {}",
                agent.retained_tool_call_count,
                agent.unfinished_tool_call_count,
                agent.incomplete_evidence_count
            );
            let _ = writeln!(
                text,
                "  Recorded running processes: {}; incomplete processes: {}",
                count(agent.recorded_running_process_count),
                count(agent.incomplete_process_count)
            );
            if agent.clock_skew_detected {
                let _ = writeln!(
                    text,
                    "  Clock skew: recorded timestamps are after the current clock."
                );
            }
            if !agent.history_gaps.is_empty() {
                let _ = writeln!(text, "  History gaps: {}", agent.history_gaps.join(", "));
            }
        }
        let _ = writeln!(
            text,
            "\nUnattributed retained calls (store-wide): {}",
            self.unattributed_tool_call_count
        );
        text.push_str("Running and unfinished counts are recorded evidence, not live process or model-session status.\n");
        Ok(text)
    }
}

fn timestamp(time: Option<i64>) -> String {
    time.map(|time| {
        OffsetDateTime::from_unix_timestamp_nanos(i128::from(time) * 1_000_000)
            .ok()
            .and_then(|time| time.format(&Rfc3339).ok())
            .unwrap_or_else(|| format!("{time} ms since Unix epoch"))
    })
    .unwrap_or_else(|| "unknown".into())
}

fn count(value: Option<u64>) -> String {
    value
        .map(|value| value.to_string())
        .unwrap_or_else(|| "unknown".into())
}

fn elapsed(ms: u64) -> String {
    let seconds = ms / 1000;
    format!(
        "{}d {:02}h {:02}m {:02}.{:03}s",
        seconds / 86400,
        seconds / 3600 % 24,
        seconds / 60 % 60,
        seconds % 60,
        ms % 1000
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use tempfile::{TempDir, tempdir};

    use crate::local::history::schema::{SCHEMA_V1, initialize_or_migrate};

    fn fixture(legacy: bool) -> (TempDir, std::path::PathBuf, Connection) {
        let root = tempdir().unwrap();
        let path = root.path().join("history.sqlite3");
        let mut connection = Connection::open(&path).unwrap();
        if legacy {
            connection.execute_batch(SCHEMA_V1).unwrap();
            connection.pragma_update(None, "user_version", 1).unwrap();
        } else {
            initialize_or_migrate(&mut connection).unwrap();
        }
        connection.execute_batch(
            "INSERT INTO agents VALUES ('aaaa', 'test', 'a', 50, 999999, 'old-runtime');
             INSERT INTO agents VALUES ('bbbb', 'test', 'b', 50, 999999, 'old-runtime');
             INSERT INTO invocations(id, correlation_id, agent_id, tool_name, args_json,
                                     started_at_ms, completed_at_ms, evidence_state, capture_state)
             VALUES (1, 'one', 'aaaa', 'exec_command', '{}', 1000, 9000, 'complete', 'complete'),
                    (2, 'two', 'aaaa', 'exec_command', '{}', 2000, 2500, 'complete', 'complete'),
                    (3, 'poll', 'aaaa', 'write_stdin', '{}', 3000, NULL, 'pending', 'not_applicable'),
                    (4, 'tie', 'aaaa', 'apply_patch', '{}', 3000, 4000, 'incomplete', 'incomplete'),
                    (5, 'unattributed', NULL, 'exec_command', '{}', 4000, 4100, 'complete', 'complete');"
        ).unwrap();
        if !legacy {
            connection
                .execute_batch(
                    "UPDATE invocations SET process_state = 'incomplete' WHERE id = 1;
                 UPDATE invocations SET process_state = 'running' WHERE id = 2;",
                )
                .unwrap();
        }
        (root, path, connection)
    }

    #[test]
    fn agent_timing_uses_all_raw_calls_and_distinguishes_start_completion_and_processes() {
        let (_root, path, _connection) = fixture(false);
        let doc = LocalHistoryReader::agent_timings(&path, None, Some(10000)).unwrap();
        assert_eq!(doc.scope, "retained_tool_calls");
        assert_eq!(doc.agents.len(), 2);
        assert_eq!(doc.unattributed_tool_call_count, 1);
        let agent = &doc.agents[0];
        assert_eq!(agent.agent_id, "aaaa");
        assert_eq!(agent.first_tool_call_at_ms, Some(1000));
        assert_eq!(agent.latest_tool_call_at_ms, Some(3000));
        assert_eq!(agent.latest_tool_call_id, Some(4));
        assert_eq!(agent.latest_tool_call_completed_at_ms, Some(4000));
        assert_eq!(agent.last_completed_tool_call_at_ms, Some(9000));
        assert_eq!(agent.elapsed_since_latest_tool_call_ms, Some(7000));
        assert!(!agent.clock_skew_detected);
        assert_eq!(agent.retained_tool_call_count, 4);
        assert_eq!(agent.unfinished_tool_call_count, 1);
        assert_eq!(
            agent.incomplete_evidence_count, 1,
            "do not double-count evidence/capture failures"
        );
        assert_eq!(agent.recorded_running_process_count, Some(1));
        assert_eq!(agent.incomplete_process_count, Some(1));
        assert_eq!(
            agent.history_gaps,
            ["incomplete_tool_evidence", "incomplete_process_evidence"]
        );
        let empty = &doc.agents[1];
        assert_eq!(empty.agent_id, "bbbb");
        assert_eq!(empty.first_tool_call_at_ms, None);
        assert_eq!(empty.elapsed_since_latest_tool_call_ms, None);
        assert_eq!(empty.recorded_running_process_count, Some(0));
        assert_eq!(empty.history_gaps, ["no_retained_tool_calls"]);
    }

    #[test]
    fn agent_timing_json_human_and_filter_are_consistent() {
        let (_root, path, _connection) = fixture(false);
        let doc = LocalHistoryReader::agent_timings(&path, Some("aaaa"), Some(10000)).unwrap();
        assert_eq!(doc.agents.len(), 1);
        let json: serde_json::Value =
            serde_json::from_str(&doc.render(HistoryFormat::Json).unwrap()).unwrap();
        assert_eq!(json["schema_version"], 1);
        assert_eq!(json["as_of_ms"], 10000);
        assert_eq!(json["agent_filter"], "aaaa");
        assert_eq!(json["agents"][0]["elapsed_since_latest_tool_call_ms"], 7000);
        let text = doc.render(HistoryFormat::Markdown).unwrap();
        assert!(
            text.contains("Agent aaaa\n  First tool call: 1970-01-01T00:00:01Z"),
            "{text}"
        );
        assert!(text.contains("Latest tool call: 1970-01-01T00:00:03Z"));
        assert!(text.contains("Since latest call: 0d 00h 00m 07.000s"));
        assert!(text.contains("not live process or model-session status"));
        assert!(!text.contains("Agent bbbb"));
        let missing = LocalHistoryReader::agent_timings(&path, Some("zzzz"), Some(10000)).unwrap();
        assert!(missing.agents.is_empty());
        assert!(
            missing
                .render(HistoryFormat::Markdown)
                .unwrap()
                .contains("No retained Agent matches zzzz")
        );
    }

    #[test]
    fn agent_timing_marks_missing_unhealthy_legacy_and_clock_skew_without_inventing_times() {
        let root = tempdir().unwrap();
        let path = root.path().join("absent.sqlite3");
        let doc = LocalHistoryReader::agent_timings(&path, None, Some(10000)).unwrap();
        assert!(!path.exists(), "read must not create a database");
        assert_eq!(doc.history_gaps, ["history_database_missing"]);
        assert!(doc.agents.is_empty());

        let (_root, path, connection) = fixture(true);
        connection.execute_batch("UPDATE history_state SET health_state = 'degraded', health_reason = 'lost records', last_retention_error = 'busy'").unwrap();
        let doc = LocalHistoryReader::agent_timings(&path, None, Some(2000)).unwrap();
        assert_eq!(
            doc.history_gaps,
            [
                "history_store_unhealthy",
                "retention_error",
                "process_lifecycle_unavailable"
            ]
        );
        let agent = &doc.agents[0];
        assert!(agent.clock_skew_detected);
        assert_eq!(agent.elapsed_since_latest_tool_call_ms, None);
        assert_eq!(agent.recorded_running_process_count, None);
        assert_eq!(agent.incomplete_process_count, None);
        assert_eq!(
            connection
                .pragma_query_value(None, "user_version", |row| row.get::<_, u32>(0))
                .unwrap(),
            1,
            "inspection must not migrate an older database"
        );
    }

    #[test]
    fn agent_timing_retention_changes_bounds_and_pending_latest_remains_unknown() {
        let (_root, path, connection) = fixture(false);
        connection
            .execute_batch("DELETE FROM invocations WHERE id IN (1, 4)")
            .unwrap();
        let doc = LocalHistoryReader::agent_timings(&path, Some("aaaa"), Some(10000)).unwrap();
        let agent = &doc.agents[0];
        assert_eq!(agent.first_tool_call_at_ms, Some(2000));
        assert_eq!(agent.latest_tool_call_id, Some(3));
        assert_eq!(agent.latest_tool_name.as_deref(), Some("write_stdin"));
        assert_eq!(agent.latest_tool_call_completed_at_ms, None);
        assert_eq!(agent.last_completed_tool_call_at_ms, Some(2500));
        assert_eq!(agent.retained_tool_call_count, 2);
        // No normal twenty-call history limit may narrow the aggregate.
        for id in 6..40 {
            connection.execute("INSERT INTO invocations(correlation_id, agent_id, tool_name, args_json, started_at_ms, evidence_state, capture_state) VALUES (?1, 'aaaa', 'write_stdin', '{}', ?2, 'pending', 'not_applicable')", params![format!("call-{id}"), id * 1000]).unwrap();
        }
        let doc = LocalHistoryReader::agent_timings(&path, Some("aaaa"), Some(40000)).unwrap();
        assert_eq!(doc.agents[0].retained_tool_call_count, 36);
        assert_eq!(doc.agents[0].first_tool_call_at_ms, Some(2000));
        assert_eq!(doc.agents[0].latest_tool_call_at_ms, Some(39000));
    }

    #[test]
    fn agent_timing_rejects_unreadable_or_future_schema_instead_of_reporting_idle() {
        let root = tempdir().unwrap();
        let path = root.path().join("broken.sqlite3");
        std::fs::write(&path, "not sqlite").unwrap();
        assert!(LocalHistoryReader::agent_timings(&path, None, Some(10000)).is_err());
        let path = root.path().join("future.sqlite3");
        Connection::open(&path)
            .unwrap()
            .pragma_update(None, "user_version", 999)
            .unwrap();
        assert!(LocalHistoryReader::agent_timings(&path, None, Some(10000)).is_err());
    }
}
