use std::collections::HashSet;
use std::convert::Infallible;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use axum::extract::{Path as AxumPath, Query, Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;
use tokio::task::JoinHandle;
use tokio_stream::StreamExt as _;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::wrappers::errors::BroadcastStreamRecvError;
use tokio_util::sync::CancellationToken;

use crate::local::history::{
    HISTORY_LIVE_EVENT_SCHEMA_VERSION, HistoryDiffProjection, HistoryLiveEvent,
    HistoryTimelineCursor, HistoryTimelineMode, HistoryTimelineQuery, LocalHistoryReader,
    normalize_declared_workdir,
};
use crate::local::presentation::{PRESENTATION_SCHEMA_VERSION, build_presentation};
use crate::local::{HistoryQuery, LocalHistoryRuntime};

use super::model::{
    ApiAgent, ApiAgentDetail, ApiAgentList, ApiError, ApiInvocationDetail, ApiInvocationList,
    ApiLogicalInvocation, ApiOutputMetadataDocument, ApiOutputPage, ApiStatusDocument,
    ApiTimelineCheckpointPage, ApiTimelineDetail, ApiTimelineDiffBatch, ApiTimelinePage,
    LOCAL_OBSERVABILITY_API_VERSION, presentation_version, schema_version,
};

const DEFAULT_INVOCATION_LIMIT: usize = 50;
const MAX_INVOCATION_LIMIT: usize = 100;
const DEFAULT_OUTPUT_CHUNK_LIMIT: usize = 16;
const MAX_OUTPUT_CHUNK_LIMIT: usize = 64;
const DEFAULT_TIMELINE_LIMIT: usize = 50;
const MAX_TIMELINE_LIMIT: usize = 100;
const DEFAULT_CHECKPOINT_LIMIT: usize = 25;
const MAX_CHECKPOINT_LIMIT: usize = 100;
const MAX_OUTPUT_AGENT_SELECTION: usize = 32;

#[derive(Clone)]
struct ApiState {
    history: Arc<LocalHistoryRuntime>,
}

#[derive(Clone)]
struct AuthState {
    token: HeaderValue,
}

pub struct LocalObservabilityServer {
    addr: SocketAddr,
    cancellation: CancellationToken,
    task: JoinHandle<Result<()>>,
}

impl LocalObservabilityServer {
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    pub fn request_shutdown(&self) {
        self.cancellation.cancel();
    }

    pub async fn shutdown(self) -> Result<()> {
        self.request_shutdown();
        self.task
            .await
            .context("Local observability server task failed to join")??;
        Ok(())
    }
}

pub async fn start_local_observability_server(
    history: Arc<LocalHistoryRuntime>,
    bearer: impl Into<Arc<str>>,
) -> Result<LocalObservabilityServer> {
    let token = validate_bearer(bearer.into())?;
    let app = build_router(history, token);
    let listener =
        tokio::net::TcpListener::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0))
            .await
            .context("failed to bind Zodex Local observability loopback listener")?;
    let addr = listener
        .local_addr()
        .context("failed to inspect Zodex Local observability listener address")?;
    if !addr.ip().is_loopback() {
        bail!("Zodex Local observability listener bound outside loopback: {addr}");
    }
    let cancellation = CancellationToken::new();
    let shutdown = cancellation.clone();
    let task = tokio::spawn(async move {
        tokio::select! {
            result = axum::serve(listener, app.into_make_service()) => {
                result.context("Zodex Local observability server terminated unexpectedly")
            }
            _ = shutdown.cancelled_owned() => Ok(()),
        }
    });
    Ok(LocalObservabilityServer {
        addr,
        cancellation,
        task,
    })
}

pub(super) fn build_router(history: Arc<LocalHistoryRuntime>, token: HeaderValue) -> Router {
    Router::new()
        .route("/v1/status", get(status))
        .route("/v1/agents", get(agents))
        .route("/v1/agents/{id}", get(agent_detail))
        .route("/v1/invocations", get(invocations))
        .route("/v1/invocations/{id}", get(invocation_detail))
        .route(
            "/v1/invocations/{id}/output-metadata",
            get(invocation_output_metadata),
        )
        .route("/v1/invocations/{id}/output", get(invocation_output))
        .route("/v1/timeline", get(timeline))
        .route("/v1/timeline/diffs", get(timeline_diffs))
        .route("/v1/timeline/{presentation_id}", get(timeline_detail))
        .route(
            "/v1/timeline/{presentation_id}/checkpoints",
            get(timeline_checkpoints),
        )
        .route("/v1/events", get(events))
        .with_state(Arc::new(ApiState { history }))
        .layer(middleware::from_fn_with_state(
            AuthState { token },
            bearer_auth_no_store,
        ))
}

async fn status(State(state): State<Arc<ApiState>>) -> Result<Json<ApiStatusDocument>, ApiFailure> {
    let path = state.history.database_path().to_path_buf();
    let runtime_id = state.history.runtime_id().to_string();
    let current_runtime = runtime_id.clone();
    let (history, agent_count) = run_blocking(move || {
        let history = LocalHistoryReader::status(&path)?;
        let agent_count = LocalHistoryReader::agent_count(&path, Some(&current_runtime))?;
        Ok((history, agent_count))
    })
    .await?;
    Ok(Json(ApiStatusDocument {
        schema_version: schema_version(),
        api_version: LOCAL_OBSERVABILITY_API_VERSION,
        presentation_version: presentation_version(),
        runtime_id,
        history,
        current_runtime_agent_count: agent_count,
        active_process_count: state.history.active_process_count(),
    }))
}

#[derive(Debug, Deserialize)]
struct AgentListQuery {
    runtime: Option<String>,
}

async fn agents(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<AgentListQuery>,
) -> Result<Json<ApiAgentList>, ApiFailure> {
    let runtime_id = state.history.runtime_id().to_string();
    let runtime_filter = match query.runtime.as_deref() {
        None => None,
        Some("current") => Some(runtime_id.clone()),
        Some(other) => return Err(bad_request(format!("unsupported runtime filter `{other}`"))),
    };
    let path = state.history.database_path().to_path_buf();
    let records =
        run_blocking(move || LocalHistoryReader::agent_records(&path, runtime_filter.as_deref()))
            .await?;
    let active = state.history.active_process_counts();
    let agents = records
        .into_iter()
        .map(|record| {
            let count = active.get(&record.summary.id).copied().unwrap_or(0);
            ApiAgent::from_record(record, &runtime_id, count)
        })
        .collect();
    Ok(Json(ApiAgentList {
        schema_version: schema_version(),
        runtime_id,
        agents,
    }))
}

async fn agent_detail(
    State(state): State<Arc<ApiState>>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<ApiAgentDetail>, ApiFailure> {
    validate_agent_id(&id)?;
    let path = state.history.database_path().to_path_buf();
    let requested = id.clone();
    let record = run_blocking(move || LocalHistoryReader::agent_record(&path, &requested))
        .await?
        .ok_or_else(|| not_found(format!("Local Agent `{id}` was not found")))?;
    let runtime_id = state.history.runtime_id().to_string();
    let active = state
        .history
        .active_process_counts()
        .get(&id)
        .copied()
        .unwrap_or(0);
    Ok(Json(ApiAgentDetail {
        schema_version: schema_version(),
        runtime_id: runtime_id.clone(),
        agent: ApiAgent::from_record(record, &runtime_id, active),
    }))
}

#[derive(Debug, Deserialize)]
struct InvocationListQuery {
    last: Option<usize>,
    since_ms: Option<i64>,
    recovery_since_ms: Option<i64>,
    agent_id: Option<String>,
    workdir: Option<String>,
}

async fn invocations(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<InvocationListQuery>,
) -> Result<Json<ApiInvocationList>, ApiFailure> {
    let last = query.last.unwrap_or(DEFAULT_INVOCATION_LIMIT);
    if last == 0 || last > MAX_INVOCATION_LIMIT {
        return Err(bad_request(format!(
            "last must be between 1 and {MAX_INVOCATION_LIMIT}"
        )));
    }
    if let Some(agent_id) = query.agent_id.as_deref() {
        validate_agent_id(agent_id)?;
    }
    let normalized_workdir = query
        .workdir
        .as_deref()
        .map(|workdir| {
            if !Path::new(workdir).is_absolute() {
                return Err(bad_request("workdir must be an absolute path"));
            }
            normalize_declared_workdir(workdir)
                .ok_or_else(|| bad_request("workdir could not be normalized"))
        })
        .transpose()?;
    let path = state.history.database_path().to_path_buf();
    let active_process_invocation_ids = if query.recovery_since_ms.is_some() {
        state.history.active_process_invocation_ids()
    } else {
        Vec::new()
    };
    let history_query = HistoryQuery {
        last,
        since_ms: query.since_ms,
        active_or_changed_since_ms: query.recovery_since_ms,
        active_process_invocation_ids,
        agent_id: query.agent_id,
        normalized_workdir,
        invocation_id: None,
        include_raw: false,
    };
    let (records, presentation) = run_blocking(move || {
        let records = LocalHistoryReader::query(&path, &history_query)?;
        let agents = LocalHistoryReader::agent_summaries(&path, &records)?;
        let presentation = build_presentation(&records, &agents);
        Ok((records, presentation))
    })
    .await?;
    Ok(Json(ApiInvocationList {
        schema_version: schema_version(),
        presentation_version: PRESENTATION_SCHEMA_VERSION,
        runtime_id: state.history.runtime_id().to_string(),
        invocations: records
            .into_iter()
            .map(ApiLogicalInvocation::from)
            .collect(),
        presentation,
    }))
}

async fn invocation_detail(
    State(state): State<Arc<ApiState>>,
    AxumPath(id): AxumPath<i64>,
) -> Result<Json<ApiInvocationDetail>, ApiFailure> {
    if id <= 0 {
        return Err(not_found("invocation was not found"));
    }
    let path = state.history.database_path().to_path_buf();
    let (record, presentation, output) = run_blocking(move || {
        let mut records = LocalHistoryReader::query(
            &path,
            &HistoryQuery {
                last: 1,
                invocation_id: Some(id),
                ..HistoryQuery::default()
            },
        )?;
        let Some(record) = records.pop() else {
            return Ok(None);
        };
        let agents = LocalHistoryReader::agent_summaries(&path, std::slice::from_ref(&record))?;
        let presentation = build_presentation(std::slice::from_ref(&record), &agents);
        let output = LocalHistoryReader::output_metadata(&path, id)?
            .context("Local invocation disappeared while reading output metadata")?;
        Ok(Some((record, presentation, output)))
    })
    .await?
    .ok_or_else(|| not_found(format!("invocation {id} was not found")))?;
    Ok(Json(ApiInvocationDetail {
        schema_version: schema_version(),
        presentation_version: PRESENTATION_SCHEMA_VERSION,
        runtime_id: state.history.runtime_id().to_string(),
        invocation: ApiLogicalInvocation::from(record),
        presentation,
        output,
    }))
}

async fn invocation_output_metadata(
    State(state): State<Arc<ApiState>>,
    AxumPath(id): AxumPath<i64>,
) -> Result<Json<ApiOutputMetadataDocument>, ApiFailure> {
    if id <= 0 {
        return Err(not_found("invocation was not found"));
    }
    let path = state.history.database_path().to_path_buf();
    let output = run_blocking(move || LocalHistoryReader::output_metadata(&path, id))
        .await?
        .ok_or_else(|| not_found(format!("invocation {id} was not found")))?;
    Ok(Json(ApiOutputMetadataDocument {
        schema_version: schema_version(),
        runtime_id: state.history.runtime_id().to_string(),
        invocation_id: id,
        output,
    }))
}

#[derive(Debug, Deserialize)]
struct OutputQuery {
    cursor: Option<u64>,
    limit: Option<usize>,
    view: Option<String>,
}

async fn invocation_output(
    State(state): State<Arc<ApiState>>,
    AxumPath(id): AxumPath<i64>,
    Query(query): Query<OutputQuery>,
) -> Result<Json<ApiOutputPage>, ApiFailure> {
    let limit = query.limit.unwrap_or(DEFAULT_OUTPUT_CHUNK_LIMIT);
    if limit == 0 || limit > MAX_OUTPUT_CHUNK_LIMIT {
        return Err(bad_request(format!(
            "limit must be between 1 and {MAX_OUTPUT_CHUNK_LIMIT}"
        )));
    }
    let cursor = query.cursor.unwrap_or(0);
    let path = state.history.database_path().to_path_buf();
    let view = query.view.as_deref().unwrap_or("raw");
    let runtime_id = state.history.runtime_id().to_string();
    let response = match view {
        "raw" => {
            let page =
                run_blocking(move || LocalHistoryReader::output_page(&path, id, cursor, limit))
                    .await?
                    .ok_or_else(|| not_found(format!("invocation {id} was not found")))?;
            ApiOutputPage {
                schema_version: schema_version(),
                runtime_id,
                invocation_id: id,
                view: "raw".to_string(),
                chunks: page.chunks,
                next_cursor: page.next_cursor,
                display_state: None,
                display_reason: None,
            }
        }
        "display" => {
            let page = run_blocking(move || {
                LocalHistoryReader::display_output_page(&path, id, cursor, limit)
            })
            .await?
            .ok_or_else(|| not_found(format!("invocation {id} was not found")))?;
            ApiOutputPage {
                schema_version: schema_version(),
                runtime_id,
                invocation_id: id,
                view: "display".to_string(),
                chunks: page.chunks,
                next_cursor: page.next_cursor,
                display_state: Some(page.display_state),
                display_reason: page.display_reason,
            }
        }
        other => return Err(bad_request(format!("unsupported output view `{other}`"))),
    };
    Ok(Json(response))
}

#[derive(Debug, Deserialize)]
struct TimelineQuery {
    limit: Option<usize>,
    cursor: Option<String>,
    agent_id: Option<String>,
    workdir: Option<String>,
    before_ms: Option<i64>,
    recovery_since_ms: Option<i64>,
    diffs: Option<String>,
}

async fn timeline(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<TimelineQuery>,
) -> Result<Json<ApiTimelinePage>, ApiFailure> {
    let limit = query.limit.unwrap_or(DEFAULT_TIMELINE_LIMIT);
    if limit == 0 || limit > MAX_TIMELINE_LIMIT {
        return Err(bad_request(format!(
            "limit must be between 1 and {MAX_TIMELINE_LIMIT}"
        )));
    }
    if query.before_ms.is_some() && query.recovery_since_ms.is_some() {
        return Err(bad_request(
            "before_ms cannot be combined with recovery_since_ms",
        ));
    }
    if let Some(agent_id) = query.agent_id.as_deref() {
        validate_agent_id(agent_id)?;
    }
    let normalized_workdir = normalize_api_workdir(query.workdir.as_deref())?;
    let diff_projection =
        parse_diff_projection(query.diffs.as_deref(), HistoryDiffProjection::Full)?;
    let cursor = query
        .cursor
        .as_deref()
        .map(HistoryTimelineCursor::decode)
        .transpose()
        .map_err(|error| bad_request(format!("invalid timeline cursor: {error}")))?;
    let mode = if let Some(since_ms) = query.recovery_since_ms {
        if cursor
            .as_ref()
            .is_some_and(|cursor| !cursor.matches_recovery(since_ms))
        {
            return Err(bad_request(
                "timeline cursor does not match recovery_since_ms",
            ));
        }
        HistoryTimelineMode::Recovery {
            since_ms,
            active_process_invocation_ids: state.history.active_process_invocation_ids(),
        }
    } else {
        if cursor
            .as_ref()
            .is_some_and(|cursor| !cursor.matches_history(query.before_ms))
        {
            return Err(bad_request("timeline cursor does not match before_ms"));
        }
        HistoryTimelineMode::History {
            before_ms: query.before_ms,
        }
    };
    let path = state.history.database_path().to_path_buf();
    let page = run_blocking(move || {
        LocalHistoryReader::timeline(
            &path,
            &HistoryTimelineQuery {
                limit,
                cursor,
                agent_id: query.agent_id,
                normalized_workdir,
                diff_projection,
                mode,
            },
        )
    })
    .await?;
    Ok(Json(ApiTimelinePage {
        schema_version: schema_version(),
        presentation_version: PRESENTATION_SCHEMA_VERSION,
        runtime_id: state.history.runtime_id().to_string(),
        records: page.records,
        has_more: page.has_more,
        next_cursor: page.next_cursor,
    }))
}

async fn timeline_detail(
    State(state): State<Arc<ApiState>>,
    AxumPath(presentation_id): AxumPath<String>,
) -> Result<Json<ApiTimelineDetail>, ApiFailure> {
    let root_id = parse_presentation_id(&presentation_id)?;
    let path = state.history.database_path().to_path_buf();
    let record = run_blocking(move || LocalHistoryReader::timeline_detail(&path, root_id))
        .await?
        .ok_or_else(|| not_found(format!("timeline record `{presentation_id}` was not found")))?;
    Ok(Json(ApiTimelineDetail {
        schema_version: schema_version(),
        presentation_version: PRESENTATION_SCHEMA_VERSION,
        runtime_id: state.history.runtime_id().to_string(),
        record,
    }))
}

#[derive(Debug, Deserialize)]
struct TimelineDiffBatchQuery {
    presentation_ids: String,
}

async fn timeline_diffs(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<TimelineDiffBatchQuery>,
) -> Result<Json<ApiTimelineDiffBatch>, ApiFailure> {
    let values = if query.presentation_ids.is_empty() {
        Vec::new()
    } else {
        query.presentation_ids.split(',').collect::<Vec<_>>()
    };
    if values.len() > MAX_TIMELINE_LIMIT {
        return Err(bad_request(format!(
            "presentation_ids accepts at most {MAX_TIMELINE_LIMIT} IDs"
        )));
    }
    let root_ids = values
        .into_iter()
        .map(parse_presentation_id)
        .collect::<Result<Vec<_>, _>>()?;
    let path = state.history.database_path().to_path_buf();
    let records =
        run_blocking(move || LocalHistoryReader::timeline_details(&path, &root_ids)).await?;
    Ok(Json(ApiTimelineDiffBatch {
        schema_version: schema_version(),
        presentation_version: PRESENTATION_SCHEMA_VERSION,
        runtime_id: state.history.runtime_id().to_string(),
        records,
    }))
}

#[derive(Debug, Deserialize)]
struct CheckpointQuery {
    limit: Option<usize>,
    cursor: Option<String>,
}

async fn timeline_checkpoints(
    State(state): State<Arc<ApiState>>,
    AxumPath(presentation_id): AxumPath<String>,
    Query(query): Query<CheckpointQuery>,
) -> Result<Json<ApiTimelineCheckpointPage>, ApiFailure> {
    let root_id = parse_presentation_id(&presentation_id)?;
    let limit = query.limit.unwrap_or(DEFAULT_CHECKPOINT_LIMIT);
    if limit == 0 || limit > MAX_CHECKPOINT_LIMIT {
        return Err(bad_request(format!(
            "limit must be between 1 and {MAX_CHECKPOINT_LIMIT}"
        )));
    }
    let cursor = query
        .cursor
        .as_deref()
        .map(HistoryTimelineCursor::decode)
        .transpose()
        .map_err(|error| bad_request(format!("invalid checkpoint cursor: {error}")))?;
    if cursor
        .as_ref()
        .is_some_and(|cursor| !cursor.matches_checkpoints(root_id))
    {
        return Err(bad_request(
            "checkpoint cursor belongs to a different timeline record",
        ));
    }
    let path = state.history.database_path().to_path_buf();
    let page = run_blocking(move || {
        LocalHistoryReader::timeline_checkpoints(&path, root_id, limit, cursor.as_ref())
    })
    .await?
    .ok_or_else(|| not_found(format!("timeline record `{presentation_id}` was not found")))?;
    Ok(Json(ApiTimelineCheckpointPage {
        schema_version: schema_version(),
        presentation_version: PRESENTATION_SCHEMA_VERSION,
        runtime_id: state.history.runtime_id().to_string(),
        presentation_id,
        checkpoints: page.checkpoints,
        has_more: page.has_more,
        next_cursor: page.next_cursor,
    }))
}

#[derive(Debug, Deserialize)]
struct EventQuery {
    agent_id: Option<String>,
    include_output: Option<bool>,
    output_agent_ids: Option<String>,
    diffs: Option<String>,
}

enum OutputSelection {
    All,
    None,
    Agents(HashSet<String>),
}

struct EventFilter {
    agent_id: Option<String>,
    output: OutputSelection,
    diff_projection: HistoryDiffProjection,
}

enum LiveEventReceive {
    Control(Result<HistoryLiveEvent, BroadcastStreamRecvError>),
    Output(Result<HistoryLiveEvent, BroadcastStreamRecvError>),
}

async fn events(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<EventQuery>,
) -> Result<Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>>, ApiFailure> {
    let filter = event_filter(query)?;
    let runtime_id = state.history.runtime_id().to_string();
    let history = state.history.clone();
    let (sequence, control_receiver, output_receiver) =
        state.history.subscribe_live_event_channels();
    let mut last_global_sequence = sequence;
    let gap_agent_id = filter.agent_id.clone();
    let control = BroadcastStream::new(control_receiver).map(LiveEventReceive::Control);
    let output = BroadcastStream::new(output_receiver).map(LiveEventReceive::Output);
    let stream = control
        .merge(output)
        .filter_map(move |received| match received {
            LiveEventReceive::Control(Ok(event)) => {
                last_global_sequence = event.sequence;
                if !filter.allows(&event) {
                    return None;
                }
                let record = if event.event_type == "presentation_updated" {
                    event
                        .presentation_id
                        .as_deref()
                        .and_then(presentation_root_id)
                        .and_then(|root_id| {
                            history.live_presentation(root_id, filter.diff_projection)
                        })
                } else {
                    None
                };
                Some(Ok(sse_event(&event, record.as_deref())))
            }
            LiveEventReceive::Control(Err(BroadcastStreamRecvError::Lagged(skipped))) => {
                last_global_sequence = last_global_sequence.saturating_add(skipped);
                let through_sequence = last_global_sequence;
                let gap = json!({
                    "schema_version": HISTORY_LIVE_EVENT_SCHEMA_VERSION,
                    "runtime_id": runtime_id,
                    "sequence": through_sequence,
                    "emitted_at_ms": current_time_ms(),
                    "event_type": "gap",
                    "agent_id": gap_agent_id,
                    "invocation_id": null,
                    "presentation_id": null,
                    "presentation_revision": null,
                    "payload": {
                        "skipped_events": skipped,
                        "recovery": "durable_history_or_invocation_detail"
                    }
                });
                Some(Ok(Event::default()
                    .id(through_sequence.to_string())
                    .event("gap")
                    .data(gap.to_string())))
            }
            LiveEventReceive::Output(Ok(event)) => {
                filter.allows(&event).then(|| Ok(sse_event(&event, None)))
            }
            // PTY output has its own exact per-invocation chunk sequence. If its
            // ephemeral channel overruns, the next observed chunk creates a local
            // output-sequence gap and the client hydrates the recent durable tail;
            // do not manufacture a control/history recovery gap.
            LiveEventReceive::Output(Err(BroadcastStreamRecvError::Lagged(_))) => None,
        });
    Ok(Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keepalive"),
    ))
}

impl EventFilter {
    fn allows(&self, event: &HistoryLiveEvent) -> bool {
        // UI preferences are global to this Local runtime. Invalidation must
        // reach focused Agent views as well as unified views, irrespective of
        // their Agent/output filters. No alias value is sent over this event.
        if event.event_type == "preferences_changed" {
            return true;
        }
        if self
            .agent_id
            .as_deref()
            .is_some_and(|id| event.agent_id.as_deref() != Some(id))
        {
            return false;
        }
        if !matches!(event.event_type.as_str(), "output" | "output_complete") {
            return true;
        }
        match &self.output {
            OutputSelection::All => true,
            OutputSelection::None => false,
            OutputSelection::Agents(agent_ids) => event
                .agent_id
                .as_ref()
                .is_some_and(|agent_id| agent_ids.contains(agent_id)),
        }
    }
}

fn event_filter(query: EventQuery) -> Result<EventFilter, ApiFailure> {
    if let Some(agent_id) = query.agent_id.as_deref() {
        validate_agent_id(agent_id)?;
    }
    let selected = query
        .output_agent_ids
        .as_deref()
        .map(parse_output_agent_ids)
        .transpose()?;
    let output = if query.include_output == Some(false) {
        OutputSelection::None
    } else if let Some(agent_ids) = selected {
        OutputSelection::Agents(agent_ids)
    } else {
        OutputSelection::All
    };
    let diff_projection =
        parse_diff_projection(query.diffs.as_deref(), HistoryDiffProjection::Summary)?;
    Ok(EventFilter {
        agent_id: query.agent_id,
        output,
        diff_projection,
    })
}

fn parse_output_agent_ids(value: &str) -> Result<HashSet<String>, ApiFailure> {
    if value.is_empty() {
        return Ok(HashSet::new());
    }
    let values = value.split(',').collect::<Vec<_>>();
    if values.len() > MAX_OUTPUT_AGENT_SELECTION {
        return Err(bad_request(format!(
            "output_agent_ids accepts at most {MAX_OUTPUT_AGENT_SELECTION} Agent IDs"
        )));
    }
    let mut result = HashSet::with_capacity(values.len());
    for value in values {
        validate_agent_id(value)?;
        result.insert(value.to_string());
    }
    Ok(result)
}

fn sse_event(
    event: &HistoryLiveEvent,
    presentation: Option<&crate::local::presentation::PresentationRecord>,
) -> Event {
    let mut event = event.clone();
    if let Some(presentation) = presentation
        && let Some(payload) = event.payload.as_object_mut()
    {
        payload.insert(
            "record".to_string(),
            serde_json::to_value(presentation).expect("Local presentation must serialize"),
        );
    }
    let event_type = event.event_type.clone();
    let data = serde_json::to_string(&event).expect("Local live event must serialize");
    let response = Event::default().event(event_type.clone()).data(data);
    if event_type == "output" {
        response
    } else {
        response.id(event.sequence.to_string())
    }
}

fn parse_diff_projection(
    value: Option<&str>,
    default: HistoryDiffProjection,
) -> Result<HistoryDiffProjection, ApiFailure> {
    match value {
        None => Ok(default),
        Some("full") => Ok(HistoryDiffProjection::Full),
        Some("summary") => Ok(HistoryDiffProjection::Summary),
        _ => Err(bad_request("diffs must be `full` or `summary`")),
    }
}

fn presentation_root_id(value: &str) -> Option<i64> {
    value
        .strip_prefix("inv-")
        .and_then(|raw| raw.parse::<i64>().ok())
        .filter(|id| *id > 0)
}

async fn bearer_auth_no_store(
    State(auth): State<AuthState>,
    request: Request,
    next: Next,
) -> Response {
    let authorized = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .and_then(|value| HeaderValue::from_str(value).ok())
        .is_some_and(|value| value == auth.token);
    let mut response = if authorized {
        next.run(request).await
    } else {
        (
            StatusCode::UNAUTHORIZED,
            Json(ApiError {
                schema_version: schema_version(),
                error: "unauthorized".to_string(),
            }),
        )
            .into_response()
    };
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

fn validate_bearer(token: Arc<str>) -> Result<HeaderValue> {
    if token.trim().len() < 32 {
        bail!("Zodex Local observability bearer must contain at least 32 characters");
    }
    HeaderValue::from_str(token.trim())
        .context("Zodex Local observability bearer is not a valid HTTP header value")
}

fn validate_agent_id(value: &str) -> Result<(), ApiFailure> {
    if value.len() == 4
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
    {
        return Ok(());
    }
    Err(bad_request("agent_id must match [a-z0-9]{4}"))
}

fn normalize_api_workdir(value: Option<&str>) -> Result<Option<String>, ApiFailure> {
    value
        .map(|workdir| {
            if !Path::new(workdir).is_absolute() {
                return Err(bad_request("workdir must be an absolute path"));
            }
            normalize_declared_workdir(workdir)
                .ok_or_else(|| bad_request("workdir could not be normalized"))
        })
        .transpose()
}

fn parse_presentation_id(value: &str) -> Result<i64, ApiFailure> {
    let id = value
        .strip_prefix("inv-")
        .and_then(|raw| raw.parse::<i64>().ok())
        .filter(|id| *id > 0)
        .ok_or_else(|| bad_request("presentation_id must match `inv-<positive invocation id>`"))?;
    if format!("inv-{id}") != value {
        return Err(bad_request(
            "presentation_id must use the canonical `inv-<id>` form",
        ));
    }
    Ok(id)
}

type ApiFailure = (StatusCode, Json<ApiError>);

fn bad_request(message: impl Into<String>) -> ApiFailure {
    failure(StatusCode::BAD_REQUEST, message)
}

fn not_found(message: impl Into<String>) -> ApiFailure {
    failure(StatusCode::NOT_FOUND, message)
}

fn failure(status: StatusCode, message: impl Into<String>) -> ApiFailure {
    (
        status,
        Json(ApiError {
            schema_version: schema_version(),
            error: message.into(),
        }),
    )
}

async fn run_blocking<T: Send + 'static>(
    operation: impl FnOnce() -> anyhow::Result<T> + Send + 'static,
) -> Result<T, ApiFailure> {
    tokio::task::spawn_blocking(operation)
        .await
        .map_err(|error| {
            failure(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("read task failed: {error}"),
            )
        })?
        .map_err(|error| failure(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))
}

fn current_time_ms() -> i64 {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    i64::try_from(millis).unwrap_or(i64::MAX)
}
