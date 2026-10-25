import type { ApiAgent, ApiAgentWorkdir } from '../api/client'

export type AgentActivity = 'running' | 'recent' | 'idle'

const RECENT_WINDOW_MS = 60_000

export function mostRecentWorkdir(agent: ApiAgent): ApiAgentWorkdir | undefined {
  return [...agent.workdirs].sort(
    (left, right) => right.last_seen_at_ms - left.last_seen_at_ms,
  )[0]
}

export function compactWorkdir(path: string): string {
  const normalized = path.replaceAll('\\', '/').replace(/\/$/u, '')
  const segments = normalized.split('/').filter(Boolean)
  if (segments.length <= 2) return normalized || '/'
  return `…/${segments.slice(-2).join('/')}`
}

export function agentActivity(agent: ApiAgent, nowMs: number): AgentActivity {
  if (agent.active_process_count > 0) return 'running'
  if (Math.max(0, nowMs - agent.last_seen_at_ms) < RECENT_WINDOW_MS) {
    return 'recent'
  }
  return 'idle'
}

export function relativeActivityLabel(lastSeenAtMs: number, nowMs: number): string {
  const elapsedMs = Math.max(0, nowMs - lastSeenAtMs)
  if (elapsedMs < 5_000) return 'now'
  const seconds = Math.floor(elapsedMs / 1_000)
  if (seconds < 60) return `${seconds}s ago`
  const minutes = Math.floor(seconds / 60)
  if (minutes < 60) return `${minutes}m ago`
  const hours = Math.floor(minutes / 60)
  if (hours < 24) return `${hours}h ago`
  return `${Math.floor(hours / 24)}d ago`
}

/** Hide idle, stale agents only in the roster, never from the board or history. */
export function visibleDrawerAgents(
  agents: readonly ApiAgent[],
  visibleIds: readonly string[],
  focusedAgentId: string | undefined,
  nowMs: number,
  hideAfterHours: number,
): ApiAgent[] {
  if (hideAfterHours === 0) return [...agents]
  const selected = new Set(visibleIds)
  const cutoffMs = hideAfterHours * 60 * 60 * 1_000
  return agents.filter((agent) =>
    agent.active_process_count > 0 ||
    agent.id === focusedAgentId ||
    selected.has(agent.id) ||
    nowMs - agent.last_seen_at_ms < cutoffMs,
  )
}
