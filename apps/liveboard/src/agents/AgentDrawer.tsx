import {
  For,
  Show,
  createEffect,
  createMemo,
  createSignal,
  onCleanup,
  onMount,
} from 'solid-js'

import type {
  ApiAgent, LiveboardPreferences, LiveboardPreferencesPatch,
} from '../api/client'
import { AgentsIcon, ChevronDownIcon, CloseIcon } from '../ui/icons'
import { AgentAgeSelect } from '../ui/AgentAgeSelect'
import { createDrawerPresence } from '../ui/drawerPresence'
import {
  agentActivity,
  relativeActivityLabel,
  visibleDrawerAgents,
} from './model'

interface AgentDrawerProps {
  open: boolean
  agents: readonly ApiAgent[]
  visibleIds: readonly string[]
  preferences: LiveboardPreferences
  nowMs: number
  focusedAgentId?: string
  loading?: boolean
  error?: string
  onRetry?: () => void
  onClose: () => void
  onAdd: (agentId: string) => void
  onPatch: (patch: LiveboardPreferencesPatch) => void
}

function AgentDrawerRow(props: {
  agent: ApiAgent
  alias?: string
  onBoard: boolean
  boardFull: boolean
  focused: boolean
  nowMs: number
  onAdd: () => void
}) {
  const [workdirsOverflow, setWorkdirsOverflow] = createSignal(false)
  const [workdirsExpanded, setWorkdirsExpanded] = createSignal(false)
  let workdirsInline: HTMLSpanElement | undefined
  let resizeObserver: ResizeObserver | undefined

  const activity = () => agentActivity(props.agent, props.nowMs)
  const workdirs = () => props.agent.workdirs.map((workdir) => workdir.normalized_workdir)
  const workdirsText = () => workdirs().join(', ')
  const addDisabled = () => props.onBoard || (!props.focused && props.boardFull)
  const toggleWorkdirs = () => {
    if (!workdirsOverflow()) return
    setWorkdirsExpanded((expanded) => !expanded)
  }
  const activateRow = () => {
    if (addDisabled()) {
      toggleWorkdirs()
      return
    }
    props.onAdd()
  }
  const measureWorkdirs = () => {
    const element = workdirsInline
    if (!element) return
    setWorkdirsOverflow(element.scrollWidth > element.clientWidth + 1)
  }

  createEffect(() => {
    workdirsText()
    setWorkdirsExpanded(false)
    queueMicrotask(measureWorkdirs)
  })
  createEffect(() => {
    if (!workdirsOverflow()) setWorkdirsExpanded(false)
  })
  onMount(() => {
    if (typeof ResizeObserver === 'undefined') return
    resizeObserver = new ResizeObserver(measureWorkdirs)
    if (workdirsInline) resizeObserver.observe(workdirsInline)
  })
  onCleanup(() => resizeObserver?.disconnect())

  return (
    <div class="drawer-agent-entry">
      <button
        type="button"
        class="drawer-agent"
        classList={{
          'drawer-agent-selected': props.onBoard,
          'drawer-agent-toggleable': workdirsOverflow(),
        }}
        aria-pressed={props.onBoard}
        disabled={addDisabled() && !workdirsOverflow()}
        title={
          props.onBoard
            ? props.focused
              ? `${props.agent.id} is already focused`
              : `${props.agent.id} is already visible`
            : props.focused
              ? `Focus Agent ${props.agent.id}`
            : props.boardFull
              ? 'Board is at its configured maximum'
              : `Add Agent ${props.agent.id} to board`
        }
        onClick={activateRow}
      >
        <div class="drawer-agent-main">
          <span class={`activity-dot activity-${activity()}`} aria-hidden="true" />
          <div class="drawer-agent-identity">
            <Show when={props.alias}>
              <strong>{props.alias}</strong>
            </Show>
            <code>{props.agent.id}</code>
          </div>
        </div>
        <div class="drawer-agent-meta">
          <span class="drawer-agent-activity">
            {props.agent.active_process_count > 0
              ? `${props.agent.active_process_count} active ${props.agent.active_process_count === 1 ? 'process' : 'processes'}`
              : relativeActivityLabel(props.agent.last_seen_at_ms, props.nowMs)}
          </span>
          <Show when={workdirsText()}>
            <span
              ref={workdirsInline}
              class="drawer-workdirs-inline"
              classList={{ 'drawer-workdirs-inline-overflow': workdirsOverflow() }}
              aria-label="Agent workdirs"
            >
              <For each={workdirs()}>
                {(workdir) => (
                  <span class="workdir-badge" title={workdir}>
                    {workdir}
                  </span>
                )}
              </For>
            </span>
          </Show>
        </div>
      </button>
      <Show when={workdirsOverflow()}>
        <button
          type="button"
          class="drawer-workdirs-toggle"
          aria-label={`${workdirsExpanded() ? 'Collapse' : 'Expand'} workdirs for Agent ${props.agent.id}`}
          aria-expanded={workdirsExpanded()}
          title={workdirsExpanded() ? 'Collapse workdirs' : 'Show all workdirs'}
          onClick={toggleWorkdirs}
        >
          <ChevronDownIcon />
        </button>
      </Show>
      <Show when={workdirsExpanded() && workdirsOverflow()}>
        <div class="drawer-workdirs-expanded" aria-label="Expanded Agent workdirs">
          <For each={workdirs()}>
            {(workdir) => (
              <span class="workdir-badge" title={workdir}>
                {workdir}
              </span>
            )}
          </For>
        </div>
      </Show>
    </div>
  )
}

export function AgentDrawer(props: AgentDrawerProps) {
  let closeButton: HTMLButtonElement | undefined
  const presence = createDrawerPresence(() => props.open)
  createEffect(() => {
    if (presence.visible()) queueMicrotask(() => closeButton?.focus())
  })

  const isFull = () => props.visibleIds.length >= props.preferences.max_visible_agents
  const displayedAgents = createMemo(() => visibleDrawerAgents(
    props.agents,
    props.visibleIds,
    props.focusedAgentId,
    props.nowMs,
    props.preferences.hide_inactive_agents_after_hours,
  ))
  const hiddenCount = () => props.agents.length - displayedAgents().length
  const showAll = () => props.onPatch({ hide_inactive_agents_after_hours: 0 })

  return (
    <Show when={presence.present()}>
      <div
        class="drawer-backdrop"
        classList={{ 'drawer-backdrop-open': presence.visible() }}
        aria-hidden={!props.open}
        inert={!props.open}
        onPointerDown={(event) => {
          if (props.open && event.target === event.currentTarget) props.onClose()
        }}
        onKeyDown={(event) => {
          if (props.open && event.key === 'Escape') props.onClose()
        }}
      >
        <aside class="agent-drawer" role="dialog" aria-modal="true" aria-labelledby="drawer-title">
        <header class="drawer-header">
          <div class="drawer-title-row">
            <AgentsIcon />
            <h2 id="drawer-title">All Agents</h2>
          </div>
          <button
            ref={closeButton}
            type="button"
            class="icon-button"
            aria-label="Close All Agents"
            onClick={props.onClose}
          >
            <CloseIcon />
          </button>
        </header>
        <div class="drawer-roster-filter">
          <span>Hide inactive after</span>
          <AgentAgeSelect
            hours={props.preferences.hide_inactive_agents_after_hours}
            onChangeHours={(hours) => props.onPatch({ hide_inactive_agents_after_hours: hours })}
          />
        </div>
        <div class="drawer-list">
          <Show when={props.loading}>
            <p class="drawer-empty" role="status">Loading Agents…</p>
          </Show>
          <Show when={props.error}>
            <div class="drawer-empty" role="alert">
              Could not load Agents: {props.error}
              <button type="button" class="text-button" onClick={props.onRetry}>
                Retry
              </button>
            </div>
          </Show>
          <Show when={!props.loading && !props.error && props.agents.length === 0}>
            <p class="drawer-empty">No Agents have appeared in this runtime yet.</p>
          </Show>
          <Show when={!props.loading && !props.error && props.agents.length > 0 && displayedAgents().length === 0}>
            <p class="drawer-empty">No agents have been active within this time window.</p>
          </Show>
          <Show when={!props.loading && !props.error}>
            <For each={displayedAgents()}>
              {(agent) => {
                const onBoard = () => props.focusedAgentId
                  ? agent.id === props.focusedAgentId
                  : props.visibleIds.includes(agent.id)
                const preference = () => props.preferences.agents[agent.id]
                return (
                  <AgentDrawerRow
                    agent={agent}
                    alias={preference()?.alias}
                    onBoard={onBoard()}
                    boardFull={isFull()}
                    focused={Boolean(props.focusedAgentId)}
                    nowMs={props.nowMs}
                    onAdd={() => props.onAdd(agent.id)}
                  />
                )
              }}
            </For>
          </Show>
          <Show when={!props.loading && !props.error && hiddenCount() > 0}>
            <div class="drawer-hidden-summary">
              {hiddenCount()} older {hiddenCount() === 1 ? 'agent' : 'agents'} hidden
              <button type="button" class="text-button" onClick={showAll}>Show all</button>
            </div>
          </Show>
        </div>
        </aside>
      </div>
    </Show>
  )
}
