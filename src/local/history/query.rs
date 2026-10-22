use std::collections::HashSet;
use std::fmt::Write as _;
use std::path::Path;

use anyhow::{Context, Result, bail};
use rusqlite::types::Value as SqlValue;
use rusqlite::{Connection, OpenFlags, OptionalExtension, params_from_iter};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::local::presentation::{markdown_code_span, sanitize_display_text};

use super::schema::{readable_schema_version, verify_readable_schema};
use super::store::{canonical_json, normalize_declared_workdir, physical_store_size};

const PRESENTATION_OUTPUT_PREVIEW_CHARS: usize = 16_384;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryFormat {
    Markdown,
    Json,
}

impl HistoryFormat {
    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "markdown" => Ok(Self::Markdown),
            "json" => Ok(Self::Json),
            _ => bail!("history --format must be `markdown` or `json`"),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct HistoryQuery {
    pub last: usize,
    pub since_ms: Option<i64>,
    /// Recovery-only window: include invocations that started or completed in
    /// the window, plus any logically in-flight invocation. Active process
    /// creator IDs are supplied separately because an `exec_command` logical
    /// invocation is already completed after it yields a running session.
    pub active_or_changed_since_ms: Option<i64>,
    pub active_process_invocation_ids: Vec<i64>,
    pub agent_id: Option<String>,
    pub normalized_workdir: Option<String>,
    pub invocation_id: Option<i64>,
    pub include_raw: bool,
}

impl HistoryQuery {
    pub fn recent(last: usize) -> Self {
        Self {
            last,
            ..Self::default()
        }
    }

    pub fn with_workdir(mut self, workdir: &Path) -> Result<Self> {
        if !workdir.is_absolute() {
            bail!("history --workdir must be an absolute path");
        }
        self.normalized_workdir = normalize_declared_workdir(&workdir.display().to_string());
        Ok(self)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HistoryInvocation {
    pub id: i64,
    pub correlation_id: String,
    pub agent_id: Option<String>,
    pub provider_kind: Option<String>,
    pub provider_session_key: Option<String>,
    pub tool_name: String,
    pub arguments: Value,
    pub declared_workdir_exact: Option<String>,
    pub declared_workdir_normalized: Option<String>,
    pub is_new_workdir: bool,
    pub started_at_ms: i64,
    pub completed_at_ms: Option<i64>,
    pub duration_ms: Option<i64>,
    pub outcome_kind: Option<String>,
    pub result: Option<Value>,
    pub error: Option<String>,
    pub evidence_state: String,
    pub evidence_reason: Option<String>,
    pub capture_state: String,
    pub capture_reason: Option<String>,
    pub target_session_handle: Option<String>,
    pub target_created_by_agent_id: Option<String>,
    pub target_created_by_invocation_id: Option<i64>,
    pub continuation_kind: Option<String>,
    pub cross_agent: Option<bool>,
    pub result_status: Option<String>,
    pub result_cwd: Option<String>,
    pub result_session_handle: Option<String>,
    pub result_exit_code: Option<i64>,
    pub result_termination_reason: Option<String>,
    pub process_state: Option<String>,
    pub process_started_at_ms: Option<i64>,
    pub process_ended_at_ms: Option<i64>,
    pub process_updated_at_ms: Option<i64>,
    pub process_exit_code: Option<i64>,
    pub process_termination_reason: Option<String>,
    pub process_cwd: Option<String>,
    pub process_incomplete_reason: Option<String>,
    #[serde(skip)]
    pub output_preview: Option<String>,
    #[serde(skip)]
    pub output_preview_truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub full_output: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub file_evidence: Vec<HistoryFileEvidence>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HistoryFileEvidence {
    pub ordinal: u32,
    pub source_kind: String,
    pub operation_hint: String,
    pub path_before: String,
    pub path_after: String,
    pub before_state: String,
    pub before_text: Option<String>,
    pub before_reason: Option<String>,
    pub destination_before_state: Option<String>,
    pub destination_before_text: Option<String>,
    pub destination_before_reason: Option<String>,
    pub after_state: String,
    pub after_text: Option<String>,
    pub after_reason: Option<String>,
    pub source_after_state: Option<String>,
    pub source_after_text: Option<String>,
    pub source_after_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HistoryAgentWorkdir {
    pub normalized_workdir: String,
    pub ordinal: u32,
    pub first_seen_at_ms: i64,
    pub last_seen_at_ms: i64,
    pub first_invocation_id: i64,
    pub last_invocation_id: i64,
    pub retained_invocation_count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HistoryAgentSummary {
    pub id: String,
    pub first_seen_at_ms: i64,
    pub last_seen_at_ms: i64,
    pub workdirs: Vec<HistoryAgentWorkdir>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HistoryAgentRecord {
    pub summary: HistoryAgentSummary,
    pub last_seen_runtime_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct HistoryOutputChunk {
    pub sequence: u64,
    pub observed_at_ms: i64,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct HistoryOutputPage {
    pub chunks: Vec<HistoryOutputChunk>,
    pub next_cursor: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct HistoryOutputMetadata {
    pub available: bool,
    pub chunk_count: u64,
    pub size_bytes: u64,
    pub capture_state: String,
    pub capture_reason: Option<String>,
    pub first_cursor: Option<u64>,
    pub last_cursor: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HistoryStoreStatus {
    pub database_exists: bool,
    pub physical_size_bytes: u64,
    pub health_state: String,
    pub health_reason: Option<String>,
    pub over_budget: bool,
    pub last_retention_error: Option<String>,
}

/// Conservative activity evidence for a Local automatic update. This is
/// intentionally raw, not the presentation timeline (which folds polls).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct LocalRecentActivity {
    pub last_activity_ms: Option<i64>,
    pub unfinished_invocations: u64,
    pub running_processes: u64,
}

pub struct LocalHistoryReader;

impl LocalHistoryReader {
    /// Include every invocation, continuation poll, file patch, process
    /// lifecycle update, and streamed output chunk. Any unreadable database
    /// fails closed in the caller instead of treating missing evidence as idle.
    pub fn recent_activity(path: &Path, runtime_started_ms: i64) -> Result<LocalRecentActivity> {
        if !path.exists() {
            return Ok(LocalRecentActivity::default());
        }
        let connection = open_read_only(path)?;
        verify_readable_schema(&connection)?;
        let (started, completed, updated, ended, unfinished, running): (
            Option<i64>,
            Option<i64>,
            Option<i64>,
            Option<i64>,
            i64,
            i64,
        ) = connection
            .query_row(
                "SELECT MAX(started_at_ms), MAX(completed_at_ms),
                    MAX(process_updated_at_ms), MAX(process_ended_at_ms),
                    COALESCE(SUM(CASE WHEN completed_at_ms IS NULL AND started_at_ms >= ?1
                                      THEN 1 ELSE 0 END), 0),
                    COALESCE(SUM(CASE WHEN process_state = 'running'
                                      AND process_started_at_ms >= ?1
                                      THEN 1 ELSE 0 END), 0)
             FROM invocations",
                [runtime_started_ms],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )
            .context("failed to inspect recent Local invocation activity")?;
        let output: Option<i64> = connection
            .query_row(
                "SELECT MAX(observed_at_ms) FROM invocation_output_chunks",
                [],
                |row| row.get(0),
            )
            .context("failed to inspect recent Local process output activity")?;
        Ok(LocalRecentActivity {
            last_activity_ms: [started, completed, updated, ended, output]
                .into_iter()
                .flatten()
                .max(),
            unfinished_invocations: u64::try_from(unfinished.max(0)).unwrap_or(u64::MAX),
            running_processes: u64::try_from(running.max(0)).unwrap_or(u64::MAX),
        })
    }

    pub fn agent_count(path: &Path, runtime_id: Option<&str>) -> Result<usize> {
        if !path.exists() {
            return Ok(0);
        }
        let connection = open_read_only(path)?;
        verify_readable_schema(&connection)?;
        let count: i64 = match runtime_id {
            Some(runtime_id) => connection
                .query_row(
                    "SELECT COUNT(*) FROM agents WHERE last_seen_runtime_id = ?1",
                    [runtime_id],
                    |row| row.get(0),
                )
                .context("failed to count current-runtime Local Agents")?,
            None => connection
                .query_row("SELECT COUNT(*) FROM agents", [], |row| row.get(0))
                .context("failed to count Local Agents")?,
        };
        Ok(usize::try_from(count).unwrap_or(usize::MAX))
    }

    pub fn query(path: &Path, query: &HistoryQuery) -> Result<Vec<HistoryInvocation>> {
        if !path.exists() {
            return Ok(Vec::new());
        }
        let connection = open_read_only(path)?;
        let schema_version = readable_schema_version(&connection)?;

        let lifecycle_columns = if schema_version >= 4 {
            "target_created_by_invocation_id, continuation_kind,
             process_state, process_started_at_ms, process_ended_at_ms, process_updated_at_ms,
             process_exit_code, process_termination_reason, process_cwd, process_incomplete_reason"
        } else {
            "NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL"
        };
        let mut sql = format!(
            "SELECT id, correlation_id, agent_id, provider_kind, provider_session_key,
                    tool_name, args_json, declared_workdir_exact, declared_workdir_normalized,
                    is_new_workdir, started_at_ms, completed_at_ms, duration_ms, outcome_kind,
                    result_json, error_text, evidence_state, evidence_reason, capture_state, capture_reason, target_session_handle,
                    target_created_by_agent_id, cross_agent, result_status, result_cwd,
                    result_session_handle, result_exit_code, result_termination_reason,
                    {lifecycle_columns}
             FROM invocations WHERE 1 = 1",
        );
        let mut parameters = Vec::<SqlValue>::new();
        if let Some(id) = query.invocation_id {
            sql.push_str(" AND id = ?");
            parameters.push(SqlValue::Integer(id));
        }
        if let Some(since_ms) = query.since_ms {
            sql.push_str(" AND started_at_ms >= ?");
            parameters.push(SqlValue::Integer(since_ms));
        }
        if let Some(since_ms) = query.active_or_changed_since_ms {
            sql.push_str(
                " AND (started_at_ms >= ? OR completed_at_ms >= ? OR completed_at_ms IS NULL",
            );
            parameters.push(SqlValue::Integer(since_ms));
            parameters.push(SqlValue::Integer(since_ms));
            if schema_version >= 4 {
                sql.push_str(" OR process_updated_at_ms >= ?");
                parameters.push(SqlValue::Integer(since_ms));
            }
            if !query.active_process_invocation_ids.is_empty() {
                sql.push_str(" OR id IN (");
                for (index, invocation_id) in query
                    .active_process_invocation_ids
                    .iter()
                    .copied()
                    .enumerate()
                {
                    if index > 0 {
                        sql.push_str(", ");
                    }
                    sql.push('?');
                    parameters.push(SqlValue::Integer(invocation_id));
                }
                sql.push(')');
            }
            sql.push(')');
        }
        if let Some(agent_id) = &query.agent_id {
            sql.push_str(" AND agent_id = ?");
            parameters.push(SqlValue::Text(agent_id.clone()));
        }
        if let Some(workdir) = &query.normalized_workdir {
            sql.push_str(" AND declared_workdir_normalized = ?");
            parameters.push(SqlValue::Text(workdir.clone()));
        }
        sql.push_str(" ORDER BY started_at_ms DESC, id DESC LIMIT ?");
        parameters.push(SqlValue::Integer(
            i64::try_from(query.last.max(1)).unwrap_or(i64::MAX),
        ));

        let mut statement = connection
            .prepare(&sql)
            .context("failed to prepare Local history query")?;
        let mut records = statement
            .query_map(params_from_iter(parameters), map_invocation)
            .context("failed to query Local history")?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("failed to decode Local history rows")?;
        for record in &mut records {
            if record.tool_name == "exec_command" {
                let (preview, truncated) =
                    load_output_preview(&connection, record.id, PRESENTATION_OUTPUT_PREVIEW_CHARS)?;
                record.output_preview = preview;
                record.output_preview_truncated = truncated;
            }
        }
        if query.include_raw {
            for record in &mut records {
                record.full_output = Some(load_full_output(&connection, record.id)?);
            }
        }
        if schema_version >= 3 {
            for record in &mut records {
                record.file_evidence = load_file_evidence(&connection, record.id)?;
            }
        }
        Ok(records)
    }

    pub fn agent_summaries(
        path: &Path,
        records: &[HistoryInvocation],
    ) -> Result<Vec<HistoryAgentSummary>> {
        if !path.exists() {
            return Ok(Vec::new());
        }
        let connection = open_read_only(path)?;
        verify_readable_schema(&connection)?;
        let mut seen = HashSet::new();
        let mut agent_ids = Vec::new();
        for record in records.iter().rev() {
            if let Some(agent_id) = record.agent_id.as_ref()
                && seen.insert(agent_id.clone())
            {
                agent_ids.push(agent_id.clone());
            }
        }
        agent_ids
            .into_iter()
            .map(|agent_id| load_agent_summary(&connection, &agent_id))
            .collect()
    }

    pub(crate) fn agent_records(
        path: &Path,
        runtime_id: Option<&str>,
    ) -> Result<Vec<HistoryAgentRecord>> {
        if !path.exists() {
            return Ok(Vec::new());
        }
        let connection = open_read_only(path)?;
        verify_readable_schema(&connection)?;
        let mut sql = String::from("SELECT id, last_seen_runtime_id FROM agents");
        let mut parameters = Vec::<SqlValue>::new();
        if let Some(runtime_id) = runtime_id {
            sql.push_str(" WHERE last_seen_runtime_id = ?");
            parameters.push(SqlValue::Text(runtime_id.to_string()));
        }
        sql.push_str(" ORDER BY first_seen_at_ms ASC, id ASC");
        let mut statement = connection
            .prepare(&sql)
            .context("failed to prepare Local Agent enumeration")?;
        let identities = statement
            .query_map(params_from_iter(parameters), |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .context("failed to enumerate Local Agents")?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("failed to decode Local Agent identities")?;
        identities
            .into_iter()
            .map(|(id, last_seen_runtime_id)| {
                Ok(HistoryAgentRecord {
                    summary: load_agent_summary(&connection, &id)?,
                    last_seen_runtime_id,
                })
            })
            .collect()
    }

    pub(crate) fn agent_record(path: &Path, agent_id: &str) -> Result<Option<HistoryAgentRecord>> {
        if !path.exists() {
            return Ok(None);
        }
        let connection = open_read_only(path)?;
        verify_readable_schema(&connection)?;
        let last_seen_runtime_id = connection
            .query_row(
                "SELECT last_seen_runtime_id FROM agents WHERE id = ?1",
                [agent_id],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .context("failed to query Local Agent detail")?;
        last_seen_runtime_id
            .map(|last_seen_runtime_id| {
                Ok(HistoryAgentRecord {
                    summary: load_agent_summary(&connection, agent_id)?,
                    last_seen_runtime_id,
                })
            })
            .transpose()
    }

    pub(crate) fn output_metadata(
        path: &Path,
        invocation_id: i64,
    ) -> Result<Option<HistoryOutputMetadata>> {
        if !path.exists() {
            return Ok(None);
        }
        let connection = open_read_only(path)?;
        verify_readable_schema(&connection)?;
        let capture = connection
            .query_row(
                "SELECT capture_state, capture_reason FROM invocations WHERE id = ?1",
                [invocation_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
            )
            .optional()
            .context("failed to query Local invocation output metadata")?;
        let Some((capture_state, capture_reason)) = capture else {
            return Ok(None);
        };
        let (chunk_count, size_bytes, first_cursor, last_cursor): (
            i64,
            i64,
            Option<i64>,
            Option<i64>,
        ) = connection
            .query_row(
                "SELECT COUNT(*), COALESCE(SUM(length(CAST(text AS BLOB))), 0), MIN(sequence), MAX(sequence)
                 FROM invocation_output_chunks WHERE invocation_id = ?1",
                [invocation_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .context("failed to summarize Local invocation output")?;
        let fallback_output = if chunk_count == 0 {
            result_output_for_invocation(&connection, invocation_id)?
                .filter(|output| !output.is_empty())
        } else {
            None
        };
        let (available, chunk_count, size_bytes, first_cursor, last_cursor) =
            if let Some(output) = fallback_output {
                (
                    true,
                    1,
                    i64::try_from(output.len()).unwrap_or(i64::MAX),
                    Some(0),
                    Some(0),
                )
            } else {
                (
                    chunk_count > 0,
                    chunk_count,
                    size_bytes,
                    first_cursor,
                    last_cursor,
                )
            };
        Ok(Some(HistoryOutputMetadata {
            available,
            chunk_count: u64::try_from(chunk_count).unwrap_or(0),
            size_bytes: u64::try_from(size_bytes).unwrap_or(0),
            capture_state,
            capture_reason,
            first_cursor: first_cursor.and_then(|value| u64::try_from(value).ok()),
            last_cursor: last_cursor.and_then(|value| u64::try_from(value).ok()),
        }))
    }

    pub(crate) fn output_page(
        path: &Path,
        invocation_id: i64,
        cursor: u64,
        limit: usize,
    ) -> Result<Option<HistoryOutputPage>> {
        if !path.exists() {
            return Ok(None);
        }
        let connection = open_read_only(path)?;
        verify_readable_schema(&connection)?;
        let invocation = connection
            .query_row(
                "SELECT COALESCE(completed_at_ms, started_at_ms)
                 FROM invocations WHERE id = ?1",
                [invocation_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .context("failed to check Local invocation for output page")?;
        let Some(result_observed_at_ms) = invocation else {
            return Ok(None);
        };
        let limit = limit.max(1);
        let sql_limit = i64::try_from(limit.saturating_add(1)).unwrap_or(i64::MAX);
        let cursor = i64::try_from(cursor).unwrap_or(i64::MAX);
        let mut statement = connection
            .prepare(
                "SELECT sequence, observed_at_ms, text FROM invocation_output_chunks
                 WHERE invocation_id = ?1 AND sequence >= ?2
                 ORDER BY sequence ASC LIMIT ?3",
            )
            .context("failed to prepare Local invocation output page")?;
        let mut chunks = statement
            .query_map(rusqlite::params![invocation_id, cursor, sql_limit], |row| {
                let sequence: i64 = row.get(0)?;
                Ok(HistoryOutputChunk {
                    sequence: u64::try_from(sequence).unwrap_or(u64::MAX),
                    observed_at_ms: row.get(1)?,
                    text: row.get(2)?,
                })
            })
            .context("failed to query Local invocation output page")?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("failed to decode Local invocation output page")?;
        let next_cursor = if chunks.len() > limit {
            chunks.pop().map(|chunk| chunk.sequence)
        } else {
            None
        };
        if chunks.is_empty()
            && cursor == 0
            && let Some(output) = result_output_for_invocation(&connection, invocation_id)?
            && !output.is_empty()
        {
            chunks.push(HistoryOutputChunk {
                sequence: 0,
                observed_at_ms: result_observed_at_ms,
                text: output,
            });
        }
        Ok(Some(HistoryOutputPage {
            chunks,
            next_cursor,
        }))
    }

    pub fn status(path: &Path) -> Result<HistoryStoreStatus> {
        if !path.exists() {
            return Ok(HistoryStoreStatus {
                database_exists: false,
                physical_size_bytes: 0,
                health_state: "empty".to_string(),
                health_reason: None,
                over_budget: false,
                last_retention_error: None,
            });
        }
        let physical_size_bytes = physical_store_size(path)?;
        let unreadable = |reason: String| HistoryStoreStatus {
            database_exists: true,
            physical_size_bytes,
            health_state: "unreadable".to_string(),
            health_reason: Some(reason),
            over_budget: false,
            last_retention_error: None,
        };
        let connection = match open_read_only(path) {
            Ok(connection) => connection,
            Err(error) => return Ok(unreadable(error.to_string())),
        };
        if let Err(error) = verify_readable_schema(&connection) {
            return Ok(unreadable(error.to_string()));
        }
        let (health_state, health_reason, over_budget, last_retention_error) = match connection
            .query_row(
                "SELECT health_state, health_reason, over_budget, last_retention_error
                 FROM history_state WHERE singleton = 1",
                [],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, i64>(2)? != 0,
                        row.get::<_, Option<String>>(3)?,
                    ))
                },
            ) {
            Ok(state) => state,
            Err(error) => {
                return Ok(unreadable(format!(
                    "failed to read Local history health state: {error}"
                )));
            }
        };
        Ok(HistoryStoreStatus {
            database_exists: true,
            physical_size_bytes,
            health_state,
            health_reason,
            over_budget,
            last_retention_error,
        })
    }

    pub fn render(
        records: &[HistoryInvocation],
        format: HistoryFormat,
        raw: bool,
    ) -> Result<String> {
        match format {
            HistoryFormat::Json => {
                serde_json::to_string_pretty(records).context("failed to render Local history JSON")
            }
            HistoryFormat::Markdown => Ok(render_markdown(records, raw)),
        }
    }
}

pub(super) fn result_output_from_json(raw: Option<&str>) -> Option<String> {
    raw.and_then(|raw| serde_json::from_str::<Value>(raw).ok())
        .and_then(|value| {
            value
                .get("output")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
}

pub(super) fn result_output_for_invocation(
    connection: &Connection,
    invocation_id: i64,
) -> Result<Option<String>> {
    let raw = connection
        .query_row(
            "SELECT result_json FROM invocations
             WHERE id = ?1 AND tool_name = 'exec_command'",
            [invocation_id],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()
        .context("failed to query exact Local invocation result output")?
        .flatten();
    Ok(result_output_from_json(raw.as_deref()))
}

pub(super) fn open_read_only(path: &Path) -> Result<Connection> {
    Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(|| {
        format!(
            "failed to open Local history database {} read-only",
            path.display()
        )
    })
}

fn map_invocation(row: &rusqlite::Row<'_>) -> rusqlite::Result<HistoryInvocation> {
    let args_json: String = row.get(6)?;
    let result_json: Option<String> = row.get(14)?;
    Ok(HistoryInvocation {
        id: row.get(0)?,
        correlation_id: row.get(1)?,
        agent_id: row.get(2)?,
        provider_kind: row.get(3)?,
        provider_session_key: row.get(4)?,
        tool_name: row.get(5)?,
        arguments: serde_json::from_str(&args_json).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                args_json.len(),
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?,
        declared_workdir_exact: row.get(7)?,
        declared_workdir_normalized: row.get(8)?,
        is_new_workdir: row.get::<_, i64>(9)? != 0,
        started_at_ms: row.get(10)?,
        completed_at_ms: row.get(11)?,
        duration_ms: row.get(12)?,
        outcome_kind: row.get(13)?,
        result: result_json
            .map(|raw| {
                serde_json::from_str(&raw).map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        raw.len(),
                        rusqlite::types::Type::Text,
                        Box::new(error),
                    )
                })
            })
            .transpose()?,
        error: row.get(15)?,
        evidence_state: row.get(16)?,
        evidence_reason: row.get(17)?,
        capture_state: row.get(18)?,
        capture_reason: row.get(19)?,
        target_session_handle: row.get(20)?,
        target_created_by_agent_id: row.get(21)?,
        target_created_by_invocation_id: row.get(28)?,
        continuation_kind: row.get(29)?,
        cross_agent: row.get::<_, Option<i64>>(22)?.map(|value| value != 0),
        result_status: row.get(23)?,
        result_cwd: row.get(24)?,
        result_session_handle: row.get(25)?,
        result_exit_code: row.get(26)?,
        result_termination_reason: row.get(27)?,
        process_state: row.get(30)?,
        process_started_at_ms: row.get(31)?,
        process_ended_at_ms: row.get(32)?,
        process_updated_at_ms: row.get(33)?,
        process_exit_code: row.get(34)?,
        process_termination_reason: row.get(35)?,
        process_cwd: row.get(36)?,
        process_incomplete_reason: row.get(37)?,
        output_preview: None,
        output_preview_truncated: false,
        full_output: None,
        file_evidence: Vec::new(),
    })
}

fn load_file_evidence(
    connection: &Connection,
    invocation_id: i64,
) -> Result<Vec<HistoryFileEvidence>> {
    let mut statement = connection
        .prepare(
            "SELECT ordinal, source_kind, operation_hint, path_before, path_after,
                    before_state, before_text, before_reason,
                    destination_before_state, destination_before_text, destination_before_reason,
                    after_state, after_text, after_reason, source_after_state, source_after_text, source_after_reason
             FROM invocation_file_evidence WHERE invocation_id = ?1 ORDER BY ordinal ASC",
        )
        .context("failed to prepare Local file-evidence query")?;
    statement
        .query_map([invocation_id], map_file_evidence)
        .context("failed to query Local file evidence")?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("failed to decode Local file evidence")
}

pub(super) fn map_file_evidence(row: &rusqlite::Row<'_>) -> rusqlite::Result<HistoryFileEvidence> {
    map_file_evidence_at(row, 0)
}

pub(super) fn map_file_evidence_at(
    row: &rusqlite::Row<'_>,
    offset: usize,
) -> rusqlite::Result<HistoryFileEvidence> {
    let ordinal: i64 = row.get(offset)?;
    Ok(HistoryFileEvidence {
        ordinal: ordinal as u32,
        source_kind: row.get(offset + 1)?,
        operation_hint: row.get(offset + 2)?,
        path_before: row.get(offset + 3)?,
        path_after: row.get(offset + 4)?,
        before_state: row.get(offset + 5)?,
        before_text: row.get(offset + 6)?,
        before_reason: row.get(offset + 7)?,
        destination_before_state: row.get(offset + 8)?,
        destination_before_text: row.get(offset + 9)?,
        destination_before_reason: row.get(offset + 10)?,
        after_state: row.get(offset + 11)?,
        after_text: row.get(offset + 12)?,
        after_reason: row.get(offset + 13)?,
        source_after_state: row.get(offset + 14)?,
        source_after_text: row.get(offset + 15)?,
        source_after_reason: row.get(offset + 16)?,
    })
}

fn load_agent_summary(connection: &Connection, agent_id: &str) -> Result<HistoryAgentSummary> {
    let (first_seen_at_ms, last_seen_at_ms) = connection
        .query_row(
            "SELECT first_seen_at_ms, last_seen_at_ms FROM agents WHERE id = ?1",
            [agent_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .with_context(|| format!("failed to read Local Agent {agent_id}"))?;
    let mut statement = connection
        .prepare(
            "SELECT normalized_workdir, ordinal, first_seen_at_ms, last_seen_at_ms,
                    first_invocation_id, last_invocation_id, retained_invocation_count
             FROM agent_workdirs WHERE agent_id = ?1 ORDER BY ordinal ASC",
        )
        .context("failed to prepare Local Agent workdir query")?;
    let workdirs = statement
        .query_map([agent_id], |row| {
            let ordinal: i64 = row.get(1)?;
            let retained_invocation_count: i64 = row.get(6)?;
            Ok(HistoryAgentWorkdir {
                normalized_workdir: row.get(0)?,
                ordinal: ordinal as u32,
                first_seen_at_ms: row.get(2)?,
                last_seen_at_ms: row.get(3)?,
                first_invocation_id: row.get(4)?,
                last_invocation_id: row.get(5)?,
                retained_invocation_count: retained_invocation_count as u64,
            })
        })
        .context("failed to query Local Agent workdirs")?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("failed to decode Local Agent workdirs")?;
    Ok(HistoryAgentSummary {
        id: agent_id.to_string(),
        first_seen_at_ms,
        last_seen_at_ms,
        workdirs,
    })
}

fn load_full_output(connection: &Connection, invocation_id: i64) -> Result<String> {
    let mut statement = connection
        .prepare(
            "SELECT text FROM invocation_output_chunks
             WHERE invocation_id = ?1 ORDER BY sequence ASC",
        )
        .context("failed to prepare Local full-output query")?;
    let chunks = statement
        .query_map([invocation_id], |row| row.get::<_, String>(0))
        .context("failed to query Local full-output chunks")?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("failed to decode Local full-output chunks")?;
    Ok(chunks.concat())
}

fn load_output_preview(
    connection: &Connection,
    invocation_id: i64,
    max_chars: usize,
) -> Result<(Option<String>, bool)> {
    let mut statement = connection
        .prepare(
            "SELECT text FROM invocation_output_chunks
             WHERE invocation_id = ?1 ORDER BY sequence ASC",
        )
        .context("failed to prepare Local output-preview query")?;
    let mut rows = statement
        .query([invocation_id])
        .context("failed to query Local output-preview chunks")?;
    let mut preview = String::new();
    let mut chars_seen = 0usize;
    let mut saw_chunk = false;
    let preview_limit = max_chars.saturating_add(1);

    'chunks: while let Some(row) = rows
        .next()
        .context("failed to iterate Local output-preview chunks")?
    {
        saw_chunk = true;
        let chunk: String = row
            .get(0)
            .context("failed to decode Local output-preview chunk")?;
        for ch in chunk.chars() {
            if chars_seen >= preview_limit {
                break 'chunks;
            }
            preview.push(ch);
            chars_seen = chars_seen.saturating_add(1);
        }
    }

    if !saw_chunk {
        return Ok((None, false));
    }
    if chars_seen <= max_chars {
        return Ok((Some(preview), false));
    }

    let cut = preview
        .char_indices()
        .nth(max_chars)
        .map(|(index, _)| index)
        .unwrap_or(preview.len());
    preview.truncate(cut);
    Ok((Some(preview), true))
}

fn render_markdown(records: &[HistoryInvocation], raw: bool) -> String {
    if records.is_empty() {
        return "No Local history.\n".to_string();
    }
    let mut output = String::new();
    for record in records {
        let agent = record.agent_id.as_deref().unwrap_or("----");
        let outcome = record.outcome_kind.as_deref().unwrap_or("in-progress");
        let workdir = record
            .declared_workdir_normalized
            .as_deref()
            .map(sanitize_display_text)
            .map(|value| format!(" · {}", markdown_code_span(&value)))
            .unwrap_or_default();
        let cross_agent = if record.cross_agent == Some(true) {
            " · cross-agent"
        } else {
            ""
        };
        let _ = writeln!(
            output,
            "- `#{}` `{agent}` **{}** — {outcome}{workdir}{cross_agent}",
            record.id, record.tool_name
        );
        if raw {
            let args = canonical_json(&record.arguments).unwrap_or_else(|_| "null".to_string());
            let args = sanitize_display_text(&args);
            let _ = writeln!(output, "  - arguments: {}", markdown_code_span(&args));
            if let Some(result) = &record.result {
                let result = canonical_json(result).unwrap_or_else(|_| "null".to_string());
                let result = sanitize_display_text(&result);
                let _ = writeln!(output, "  - result: {}", markdown_code_span(&result));
            }
            if let Some(error) = &record.error {
                let escaped = serde_json::to_string(error).unwrap_or_else(|_| "null".to_string());
                let escaped = sanitize_display_text(&escaped);
                let _ = writeln!(output, "  - error: {}", markdown_code_span(&escaped));
            }
            if let Some(full_output) = &record.full_output {
                let escaped =
                    serde_json::to_string(full_output).unwrap_or_else(|_| "null".to_string());
                let escaped = sanitize_display_text(&escaped);
                let _ = writeln!(
                    output,
                    "  - full PTY text (evidence {}, capture {}): {}",
                    record.evidence_state,
                    record.capture_state,
                    markdown_code_span(&escaped)
                );
            }
        }
    }
    output
}

#[cfg(test)]
mod automatic_update_tests {
    use rusqlite::{Connection, params};
    use tempfile::tempdir;

    use super::{LocalHistoryReader, LocalRecentActivity};
    use crate::local::history::schema::initialize_or_migrate;

    #[test]
    fn activity_probe_tracks_patches_polls_output_and_unfinished_work() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("history.db");
        assert_eq!(
            LocalHistoryReader::recent_activity(&path, 1000).unwrap(),
            LocalRecentActivity::default()
        );
        let mut database = Connection::open(&path).unwrap();
        initialize_or_migrate(&mut database).unwrap();
        assert_eq!(
            LocalHistoryReader::recent_activity(&path, 1000).unwrap(),
            LocalRecentActivity::default()
        );

        database
            .execute(
                "INSERT INTO invocations(
                correlation_id, tool_name, args_json, started_at_ms,
                completed_at_ms, evidence_state, capture_state
             ) VALUES ('patch', 'apply_patch', '{}', 1000, 1100, 'complete', 'not_applicable')",
                [],
            )
            .unwrap();
        let patch = database.last_insert_rowid();
        let status = LocalHistoryReader::recent_activity(&path, 900).unwrap();
        assert_eq!(status.last_activity_ms, Some(1100));
        assert_eq!(status.unfinished_invocations, 0);

        database
            .execute(
                "INSERT INTO invocations(
                correlation_id, tool_name, args_json, started_at_ms,
                evidence_state, capture_state, continuation_kind
             ) VALUES ('poll', 'write_stdin', '{}', 1200, 'pending', 'not_applicable', 'poll')",
                [],
            )
            .unwrap();
        let status = LocalHistoryReader::recent_activity(&path, 900).unwrap();
        assert_eq!(status.last_activity_ms, Some(1200));
        assert_eq!(status.unfinished_invocations, 1);

        database
            .execute(
                "UPDATE invocations SET completed_at_ms = 1300 WHERE correlation_id = 'poll'",
                [],
            )
            .unwrap();
        database.execute(
            "INSERT INTO invocation_output_chunks(invocation_id, sequence, observed_at_ms, text)
             VALUES (?1, 1, 1500, 'new output')",
            params![patch],
        ).unwrap();
        assert_eq!(
            LocalHistoryReader::recent_activity(&path, 900)
                .unwrap()
                .last_activity_ms,
            Some(1500)
        );

        database
            .execute(
                "UPDATE invocations SET process_state = 'running',
               process_started_at_ms = 1600, process_updated_at_ms = 1700
             WHERE correlation_id = 'patch'",
                [],
            )
            .unwrap();
        let status = LocalHistoryReader::recent_activity(&path, 900).unwrap();
        assert_eq!(status.last_activity_ms, Some(1700));
        assert_eq!(status.running_processes, 1);
        let newer_runtime = LocalHistoryReader::recent_activity(&path, 1800).unwrap();
        assert_eq!(newer_runtime.running_processes, 0);
    }
}
