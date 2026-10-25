export interface ApiStatus {
  schema_version: number
  api_version: number
  presentation_version: number
  runtime_id: string
  current_runtime_agent_count: number
  active_process_count: number
}

export interface ApiAgent {
  id: string
  first_seen_at_ms: number
  last_seen_at_ms: number
  seen_in_current_runtime: boolean
  active_process_count: number
  workdirs: ApiAgentWorkdir[]
}

export interface ApiAgentWorkdir {
  normalized_workdir: string
  ordinal: number
  first_seen_at_ms: number
  last_seen_at_ms: number
  first_invocation_id: number
  last_invocation_id: number
  retained_invocation_count: number
}

export interface ApiAgentList {
  schema_version: number
  runtime_id: string
  agents: ApiAgent[]
}

export interface ApiAgentDetail {
  schema_version: number
  runtime_id: string
  agent: ApiAgent
}

export interface PresentationEvidence {
  evidence_state: string
  capture_state: string
  degraded: boolean
  reason: string | null
}

export interface PresentationPollSummary {
  count: number
  final_status: string | null
  caller_agent_ids: string[]
  cross_agent: boolean
}

export interface PresentationDiffLine {
  kind: string
  old_line: number | null
  new_line: number | null
  text: string
}

export interface PresentationFileChange {
  operation: 'created' | 'edited' | 'deleted' | 'renamed'
  path: string
  old_path: string | null
  write_mode: 'overwrite' | 'append' | null
  added: number
  removed: number
  diff_truncated: boolean
  diff_lines_included: boolean
  lines: PresentationDiffLine[]
}

export type DiffProjection = 'summary' | 'full'

interface PresentationRecordBase {
  presentation_id: string
  primary_invocation_id: number
  raw_evidence_count: number
  raw_invocation_ids: number[]
  raw_invocation_ids_truncated: boolean
  agent_id: string | null
  declared_workdir: string | null
  normalized_workdir: string | null
  new_workdir: string | null
  started_at_ms: number
  duration_ms: number | null
  evidence: PresentationEvidence
}

export type PresentationRecord = PresentationRecordBase &
  (
    | {
        kind: 'command'
        command: string
        status: string
        effective_cwd: string | null
        exit_code: number | null
        termination_reason: string | null
        output: string | null
        output_truncated: boolean
        polls: PresentationPollSummary | null
      }
    | {
        kind: 'file_changes'
        source_tool: string
        changes: PresentationFileChange[]
      }
    | {
        kind: 'stdin'
        target_session_handle: string
        chars: string
        chars_truncated: boolean
        creator_agent_id: string | null
        cross_agent: boolean
        result_status: string | null
      }
    | {
        kind: 'kill'
        target_session_handle: string
        target_command: string | null
        creator_agent_id: string | null
        cross_agent: boolean
        result_status: string | null
      }
    | {
        kind: 'poll_aggregate'
        target_session_handle: string
        count: number
        final_status: string | null
        creator_agent_id: string | null
        caller_agent_ids: string[]
        cross_agent: boolean
      }
    | {
        kind: 'generic'
        tool_name: string
        status: string
        summary: string | null
      }
  )

export interface ApiTimelinePage {
  schema_version: number
  presentation_version: number
  runtime_id: string
  records: PresentationRecord[]
  has_more: boolean
  next_cursor: string | null
}

export interface ApiTimelineDetail {
  schema_version: number
  presentation_version: number
  runtime_id: string
  record: PresentationRecord
}

export interface ApiTimelineDiffBatch {
  schema_version: number
  presentation_version: number
  runtime_id: string
  records: PresentationRecord[]
}

export interface OutputMetadata {
  available: boolean
  chunk_count: number
  size_bytes: number
  capture_state: string
  capture_reason: string | null
  first_cursor: number | null
  last_cursor: number | null
}

export interface ApiOutputMetadataDocument {
  schema_version: number
  runtime_id: string
  invocation_id: number
  output: OutputMetadata
}

export interface OutputChunk {
  sequence: number
  observed_at_ms: number
  text: string
}

export interface ApiOutputPage {
  schema_version: number
  runtime_id: string
  invocation_id: number
  view: 'raw' | 'display'
  chunks: OutputChunk[]
  next_cursor: number | null
  display_state?: string
  display_reason?: string
}

export interface TimelineCheckpoint {
  invocation_id: number
  checkpoint_kind: string
  agent_id: string | null
  started_at_ms: number
  completed_at_ms: number | null
  status: string
  cross_agent: boolean | null
  evidence_state: string
  capture_state: string
}

export interface ApiTimelineCheckpointPage {
  schema_version: number
  presentation_version: number
  runtime_id: string
  presentation_id: string
  checkpoints: TimelineCheckpoint[]
  has_more: boolean
  next_cursor: string | null
}

export interface ApiLogicalInvocation {
  id: number
  correlation_id: string
  agent_id: string | null
  provider_kind: string | null
  tool_name: string
  arguments: unknown
  declared_workdir_exact: string | null
  declared_workdir_normalized: string | null
  is_new_workdir: boolean
  started_at_ms: number
  completed_at_ms: number | null
  duration_ms: number | null
  outcome_kind: string | null
  result: unknown | null
  error: string | null
  evidence_state: string
  evidence_reason: string | null
  capture_state: string
  capture_reason: string | null
  target_session_handle: string | null
  target_created_by_agent_id: string | null
  cross_agent: boolean | null
}

export interface ApiInvocationDetail {
  schema_version: number
  presentation_version: number
  runtime_id: string
  invocation: ApiLogicalInvocation
  output: OutputMetadata
}

export interface HistoryLiveEvent {
  schema_version: number
  runtime_id: string
  sequence: number
  emitted_at_ms: number
  event_type: string
  agent_id: string | null
  invocation_id: number | null
  presentation_id: string | null
  presentation_revision: number | null
  payload: Record<string, unknown>
}

export type ThemePreference = 'system' | 'light' | 'dark'

export interface LiveboardPreferences {
  schema_version: number
  theme: ThemePreference
  max_visible_agents: number
  hide_inactive_agents_after_hours: number
  command_outputs_expanded: boolean
  diffs_expanded: boolean
  show_raw_button: boolean
  editor_command: string
  agents: Record<string, LiveboardAgentPreference>
}

export interface LiveboardAgentPreference {
  alias?: string
  visible?: boolean
  order?: number
  width_weight?: number
}

export interface LiveboardPreferencesPatch {
  schema_version?: number
  theme?: ThemePreference
  max_visible_agents?: number
  hide_inactive_agents_after_hours?: number
  command_outputs_expanded?: boolean
  diffs_expanded?: boolean
  show_raw_button?: boolean
  editor_command?: string
  agents?: Record<string, LiveboardAgentPreference>
}

export function fetchPreferences(): Promise<LiveboardPreferences> {
  return fetchJson<LiveboardPreferences>('preferences')
}

export async function openFileInEditor(path: string): Promise<void> {
  const response = await fetch('api/open-file', {
    method: 'POST',
    credentials: 'same-origin',
    headers: {
      Accept: 'application/json',
      'Content-Type': 'application/json',
    },
    body: JSON.stringify({ path }),
  })
  if (!response.ok) {
    let message = `Could not open file (${response.status})`
    try {
      const body = (await response.json()) as { error?: string }
      if (body.error) message = body.error
    } catch {
      // Keep the status-only fallback when the response is not JSON.
    }
    throw new Error(message)
  }
}

// Local HTTP endpoints should respond quickly. In particular, a browser with
// exhausted connections can leave fetch() queued indefinitely without ever
// giving CommandOutputState an error. Bound each attempt so the UI can retry.
const LOCAL_REQUEST_TIMEOUT_MS = 15_000

export async function fetchJson<T>(path: string): Promise<T> {
  const retryDelaysMs = [25, 100]
  let lastNetworkError: unknown
  for (let attempt = 0; attempt <= retryDelaysMs.length; attempt += 1) {
    let response: Response | undefined
    const controller = new AbortController()
    const timeout = setTimeout(() => controller.abort(), LOCAL_REQUEST_TIMEOUT_MS)
    try {
      response = await fetch(path, {
        credentials: 'same-origin',
        headers: { Accept: 'application/json' },
        signal: controller.signal,
      })
      if (response.ok) return (await response.json()) as T

      if ([502, 503, 504].includes(response.status) && attempt < retryDelaysMs.length) {
        await response.body?.cancel().catch(() => undefined)
        await new Promise((resolve) => setTimeout(resolve, retryDelaysMs[attempt]))
        continue
      }

      let message = `Liveboard request failed (${response.status})`
      try {
        const body = (await response.json()) as { error?: string }
        if (body.error) message = body.error
      } catch {
        // Keep the status-only fallback when the response is not JSON.
      }
      throw new Error(message)
    } catch (error) {
      if (controller.signal.aborted) {
        throw new Error(`Liveboard request timed out after ${LOCAL_REQUEST_TIMEOUT_MS / 1000}s`)
      }
      // Server-side errors are actionable, not transient network failures.
      if (response && !response.ok && ![502, 503, 504].includes(response.status)) {
        throw error
      }
      lastNetworkError = error
      if (attempt < retryDelaysMs.length) {
        await new Promise((resolve) => setTimeout(resolve, retryDelaysMs[attempt]))
        continue
      }
      throw error
    } finally {
      clearTimeout(timeout)
    }
  }
  throw lastNetworkError instanceof Error
    ? lastNetworkError
    : new Error('Liveboard request failed')
}

export async function patchPreferences(
  patch: LiveboardPreferencesPatch,
): Promise<LiveboardPreferences> {
  const response = await fetch('preferences', {
    method: 'PATCH',
    credentials: 'same-origin',
    headers: {
      Accept: 'application/json',
      'Content-Type': 'application/json',
    },
    body: JSON.stringify(patch),
  })
  if (!response.ok) {
    throw new Error(`Liveboard preference update failed (${response.status})`)
  }
  return (await response.json()) as LiveboardPreferences
}

export const OBSERVER_API_VERSION = 1
export const PRESENTATION_VERSION = 3
export const LIVE_EVENT_VERSION = 4

export function validateStatus(status: ApiStatus): void {
  if (
    status.schema_version !== OBSERVER_API_VERSION ||
    status.api_version !== OBSERVER_API_VERSION ||
    status.presentation_version !== PRESENTATION_VERSION
  ) {
    throw new Error('Local observer version is incompatible with this Liveboard')
  }
}

export function validateAgentList(list: ApiAgentList, runtimeId: string): void {
  if (list.schema_version !== OBSERVER_API_VERSION || list.runtime_id !== runtimeId) {
    throw new Error('Local Agent list belongs to an incompatible or changed runtime')
  }
}

export function validateTimelinePage(page: ApiTimelinePage, runtimeId: string): void {
  if (
    page.schema_version !== OBSERVER_API_VERSION ||
    page.presentation_version !== PRESENTATION_VERSION ||
    page.runtime_id !== runtimeId
  ) {
    throw new Error('Local timeline belongs to an incompatible or changed runtime')
  }
}

export function validateTimelineDetail(
  detail: ApiTimelineDetail,
  runtimeId: string,
): void {
  if (
    detail.schema_version !== OBSERVER_API_VERSION ||
    detail.presentation_version !== PRESENTATION_VERSION ||
    detail.runtime_id !== runtimeId
  ) {
    throw new Error('Local timeline detail belongs to an incompatible or changed runtime')
  }
}

export function validateTimelineDiffBatch(
  batch: ApiTimelineDiffBatch,
  runtimeId: string,
): void {
  if (
    batch.schema_version !== OBSERVER_API_VERSION ||
    batch.presentation_version !== PRESENTATION_VERSION ||
    batch.runtime_id !== runtimeId
  ) {
    throw new Error('Local timeline diff batch belongs to an incompatible or changed runtime')
  }
}

export function validateLiveEvent(event: HistoryLiveEvent): void {
  if (event.schema_version !== LIVE_EVENT_VERSION) {
    throw new Error('Local live-event version is incompatible with this Liveboard')
  }
}

export async function fetchStatus(): Promise<ApiStatus> {
  const status = await fetchJson<ApiStatus>('api/status')
  validateStatus(status)
  return status
}

export async function fetchCurrentAgents(runtimeId: string): Promise<ApiAgentList> {
  const list = await fetchJson<ApiAgentList>('api/agents?runtime=current')
  validateAgentList(list, runtimeId)
  return list
}

export async function fetchAgent(agentId: string, runtimeId: string): Promise<ApiAgent> {
  const detail = await fetchJson<ApiAgentDetail>(`api/agents/${encodeURIComponent(agentId)}`)
  if (
    detail.schema_version !== OBSERVER_API_VERSION ||
    detail.runtime_id !== runtimeId ||
    detail.agent.id !== agentId
  ) {
    throw new Error('Local Agent detail belongs to an incompatible or changed runtime')
  }
  return detail.agent
}

export async function fetchTimelineDetail(
  presentationId: string,
  runtimeId: string,
): Promise<ApiTimelineDetail> {
  const detail = await fetchJson<ApiTimelineDetail>(
    `api/timeline/${encodeURIComponent(presentationId)}`,
  )
  validateTimelineDetail(detail, runtimeId)
  return detail
}

export async function fetchTimelineDiffBatch(
  presentationIds: readonly string[],
  runtimeId: string,
): Promise<ApiTimelineDiffBatch> {
  const params = new URLSearchParams({ presentation_ids: presentationIds.join(',') })
  const batch = await fetchJson<ApiTimelineDiffBatch>(`api/timeline/diffs?${params.toString()}`)
  validateTimelineDiffBatch(batch, runtimeId)
  return batch
}

export async function fetchOutputMetadata(
  invocationId: number,
  runtimeId: string,
): Promise<ApiOutputMetadataDocument> {
  const document = await fetchJson<ApiOutputMetadataDocument>(
    `api/invocations/${invocationId}/output-metadata`,
  )
  if (
    document.schema_version !== OBSERVER_API_VERSION ||
    document.runtime_id !== runtimeId ||
    document.invocation_id !== invocationId
  ) {
    throw new Error('Local output metadata belongs to an incompatible or changed runtime')
  }
  return document
}

export async function fetchOutputPage(
  invocationId: number,
  input: { cursor: number; limit?: number; view: 'raw' | 'display' },
  runtimeId: string,
): Promise<ApiOutputPage> {
  const params = new URLSearchParams({
    cursor: String(input.cursor),
    view: input.view,
  })
  if (input.limit !== undefined) params.set('limit', String(input.limit))
  const page = await fetchJson<ApiOutputPage>(
    `api/invocations/${invocationId}/output?${params.toString()}`,
  )
  if (
    page.schema_version !== OBSERVER_API_VERSION ||
    page.runtime_id !== runtimeId ||
    page.invocation_id !== invocationId ||
    page.view !== input.view
  ) {
    throw new Error('Local output page belongs to an incompatible or changed runtime')
  }
  return page
}

export async function fetchTimelineCheckpoints(
  presentationId: string,
  input: { cursor?: string; limit?: number },
  runtimeId: string,
): Promise<ApiTimelineCheckpointPage> {
  const params = new URLSearchParams()
  if (input.cursor) params.set('cursor', input.cursor)
  if (input.limit !== undefined) params.set('limit', String(input.limit))
  const suffix = params.size > 0 ? `?${params.toString()}` : ''
  const page = await fetchJson<ApiTimelineCheckpointPage>(
    `api/timeline/${encodeURIComponent(presentationId)}/checkpoints${suffix}`,
  )
  if (
    page.schema_version !== OBSERVER_API_VERSION ||
    page.presentation_version !== PRESENTATION_VERSION ||
    page.runtime_id !== runtimeId ||
    page.presentation_id !== presentationId
  ) {
    throw new Error('Local checkpoint page belongs to an incompatible or changed runtime')
  }
  return page
}

export async function fetchInvocationDetail(
  invocationId: number,
  runtimeId: string,
): Promise<ApiInvocationDetail> {
  const detail = await fetchJson<ApiInvocationDetail>(`api/invocations/${invocationId}`)
  if (
    detail.schema_version !== OBSERVER_API_VERSION ||
    detail.presentation_version !== PRESENTATION_VERSION ||
    detail.runtime_id !== runtimeId ||
    detail.invocation.id !== invocationId
  ) {
    throw new Error('Local invocation detail belongs to an incompatible or changed runtime')
  }
  return detail
}

export interface TimelineQuery {
  agentId?: string
  limit?: number
  cursor?: string
  beforeMs?: number
  recoverySinceMs?: number
  diffs?: DiffProjection
}

export async function fetchTimeline(
  query: TimelineQuery,
  runtimeId: string,
): Promise<ApiTimelinePage> {
  const params = new URLSearchParams()
  if (query.agentId) params.set('agent_id', query.agentId)
  if (query.limit !== undefined) params.set('limit', String(query.limit))
  if (query.cursor) params.set('cursor', query.cursor)
  if (query.beforeMs !== undefined) params.set('before_ms', String(query.beforeMs))
  if (query.recoverySinceMs !== undefined) {
    params.set('recovery_since_ms', String(query.recoverySinceMs))
  }
  if (query.diffs) params.set('diffs', query.diffs)
  const page = await fetchJson<ApiTimelinePage>(`api/timeline?${params.toString()}`)
  validateTimelinePage(page, runtimeId)
  return page
}

export function eventStreamUrl(
  outputAgentIds: readonly string[],
  diffs: DiffProjection = 'full',
  focusedAgentId?: string,
): string {
  const params = new URLSearchParams()
  if (focusedAgentId) params.set('agent_id', focusedAgentId)
  params.set('output_agent_ids', outputAgentIds.join(','))
  params.set('diffs', diffs)
  return `api/events?${params.toString()}`
}

export function presentationRecordFromLiveEvent(
  event: HistoryLiveEvent,
): PresentationRecord | undefined {
  const record = event.payload.record
  if (!record || typeof record !== 'object') return undefined
  const presentation = record as Partial<PresentationRecord>
  if (
    typeof presentation.presentation_id !== 'string' ||
    presentation.presentation_id !== event.presentation_id ||
    typeof presentation.kind !== 'string'
  ) {
    throw new Error('Local live presentation payload is malformed')
  }
  return record as PresentationRecord
}

export async function loadBootstrap(
  view: import('../app/view').LiveboardView = { kind: 'unified' },
) {
  const [status, preferences] = await Promise.all([
    fetchStatus(),
    fetchPreferences(),
  ])
  let agents: ApiAgentList
  if (view.kind === 'focused') {
    const agent = await fetchAgent(view.agentId, status.runtime_id)
    if (!agent.seen_in_current_runtime) {
      throw new Error(`Focused Agent ${view.agentId} is not active in this Local runtime`)
    }
    agents = {
      schema_version: OBSERVER_API_VERSION,
      runtime_id: status.runtime_id,
      agents: [agent],
    }
  } else {
    agents = await fetchCurrentAgents(status.runtime_id)
  }
  return { status, agents, preferences }
}
