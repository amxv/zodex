import {
  For,
  Show,
  createEffect,
  createMemo,
  createResource,
  createSignal,
  on,
  onCleanup,
  type JSX,
} from 'solid-js'

import type {
  ApiAgent,
  LiveboardPreferences,
  LiveboardPreferencesPatch,
} from '../api/client'
import type { RuntimeConnectionState } from '../streams/runtime'
import { AgentColumn } from '../agents/AgentColumn'
import { AgentDrawer } from '../agents/AgentDrawer'
import { SettingsDrawer } from '../ui/SettingsDrawer'
import { Toolbar } from '../ui/Toolbar'
import {
  addAgentToBoard,
  admitAgent,
  columnWeights,
  currentRuntimeAgents,
  initialVisibleAgentIds,
  moveAgent,
  orderPatch,
  removeAgentFromBoard,
  resizeAdjacentWeights,
  shrinkBoardToMaximum,
} from './model'

interface BoardProps {
  agents: readonly ApiAgent[]
  preferences: LiveboardPreferences
  nowMs: number
  saving: boolean
  error?: string
  connectionState?: RuntimeConnectionState
  connectionError?: string
  focusedAgentId?: string
  onPatch: (patch: LiveboardPreferencesPatch) => void
  onVisibleAgentsChange?: (agentIds: readonly string[]) => void
  onFocusAgent?: (agentId: string) => void
  onLoadAllAgents?: () => Promise<readonly ApiAgent[]>
  renderTimeline?: (agentId: string) => JSX.Element
}

function sameOrder(left: readonly string[], right: readonly string[]) {
  return left.length === right.length && left.every((id, index) => id === right[index])
}

function reconcileMountedIds(current: string[], visible: readonly string[]): string[] {
  const visibleSet = new Set(visible)
  const next = current.filter((id) => visibleSet.has(id))
  for (const id of visible) {
    if (!next.includes(id)) next.push(id)
  }
  return sameOrder(current, next) ? current : next
}

function copyScrollPositions(source: HTMLElement, clone: HTMLElement) {
  const sourceElements = [source, ...source.querySelectorAll<HTMLElement>('*')]
  const cloneElements = [clone, ...clone.querySelectorAll<HTMLElement>('*')]
  for (let index = 0; index < sourceElements.length; index += 1) {
    const sourceElement = sourceElements[index]
    const cloneElement = cloneElements[index]
    if (!sourceElement || !cloneElement) continue
    cloneElement.scrollTop = sourceElement.scrollTop
    cloneElement.scrollLeft = sourceElement.scrollLeft
  }
}

export function Board(props: BoardProps) {
  const [drawerOpen, setDrawerOpen] = createSignal(false)
  const [settingsOpen, setSettingsOpen] = createSignal(false)
  // Focused views bootstrap only one Agent. Fetch the full roster on demand
  // for the drawer without widening the focused timeline or its SSE scope.
  const [focusedDrawerAgents, { refetch: retryFocusedDrawerAgents }] = createResource(
    () => (props.focusedAgentId && drawerOpen() ? props.focusedAgentId : undefined),
    () => props.onLoadAllAgents?.() ?? Promise.resolve(props.agents),
  )
  const [locallyHiddenIds, setLocallyHiddenIds] = createSignal(
    new Set(
      Object.entries(props.preferences.agents)
        .filter(([, preference]) => preference.visible === false)
        .map(([agentId]) => agentId),
    ),
  )
  const [visibleIds, setVisibleIds] = createSignal(
    props.focusedAgentId
      ? props.agents.some((agent) => agent.id === props.focusedAgentId)
        ? [props.focusedAgentId]
        : []
      : initialVisibleAgentIds(props.agents, props.preferences),
  )
  // Keep mounted column identity independent from visual board order. Reordering
  // must never reparent a virtualized timeline subtree just to move a column.
  const [mountedIds, setMountedIds] = createSignal([...visibleIds()])
  const [weights, setWeights] = createSignal(
    columnWeights(visibleIds(), props.preferences),
  )
  let boardElement: HTMLDivElement | undefined
  let cancelActiveReorder: (() => void) | undefined

  const agents = createMemo(() => currentRuntimeAgents(props.agents))
  const agentById = createMemo(
    () => new Map(agents().map((agent) => [agent.id, agent] as const)),
  )
  const activeProcessCount = createMemo(() =>
    agents().reduce((count, agent) => count + agent.active_process_count, 0),
  )

  createEffect(
    on(
      () => props.agents,
      (nextAgents) => {
        setVisibleIds((previous) => {
          if (props.focusedAgentId) {
            return nextAgents.some(
              (agent) => agent.id === props.focusedAgentId && agent.seen_in_current_runtime,
            )
              ? [props.focusedAgentId]
              : []
          }
          let next = previous.filter((id) =>
            nextAgents.some(
              (agent) => agent.id === id && agent.seen_in_current_runtime,
            ),
          )
          for (const agent of nextAgents) {
            if (!locallyHiddenIds().has(agent.id)) {
              next = admitAgent(next, agent, props.preferences)
            }
          }
          return next
        })
      },
    ),
  )

  createEffect(
    on(visibleIds, (ids) => {
      setMountedIds((current) => reconcileMountedIds(current, ids))
      setWeights((current) => {
        const next: Record<string, number> = {}
        for (const id of ids) {
          next[id] =
            current[id] ?? props.preferences.agents[id]?.width_weight ?? 1
        }
        return next
      })
      props.onVisibleAgentsChange?.(ids)
    }),
  )

  onCleanup(() => cancelActiveReorder?.())

  const persistOrder = (next: string[]) => {
    setVisibleIds(next)
    props.onPatch({ agents: orderPatch(next) })
  }

  const hideAgent = (agentId: string) => {
    setLocallyHiddenIds((current) => new Set(current).add(agentId))
    setVisibleIds((current) => removeAgentFromBoard(current, agentId))
    props.onPatch({ agents: { [agentId]: { visible: false } } })
  }

  const addAgent = (agentId: string) => {
    const current = visibleIds()
    const next = addAgentToBoard(
      current,
      agentId,
      props.preferences.max_visible_agents,
    )
    if (next.length === current.length) return
    setLocallyHiddenIds((hidden) => {
      const nextHidden = new Set(hidden)
      nextHidden.delete(agentId)
      return nextHidden
    })
    setVisibleIds(next)
    props.onPatch({
      agents: {
        [agentId]: { visible: true, order: next.length - 1 },
      },
    })
  }

  const setMaximum = (maximum: number) => {
    const result = shrinkBoardToMaximum(visibleIds(), maximum)
    if (result.hidden.length > 0) {
      setLocallyHiddenIds((current) => {
        const next = new Set(current)
        for (const agentId of result.hidden) next.add(agentId)
        return next
      })
    }
    setVisibleIds(result.visible)
    props.onPatch({
      max_visible_agents: maximum,
      agents: Object.fromEntries(
        result.hidden.map((agentId) => [agentId, { visible: false }]),
      ),
    })
  }

  const openAgents = () => {
    setSettingsOpen(false)
    setDrawerOpen(true)
  }

  const openSettings = () => {
    setDrawerOpen(false)
    setSettingsOpen(true)
  }

  const beginReorder = (agentId: string, event: PointerEvent) => {
    if (!boardElement) return
    cancelActiveReorder?.()
    const handle = event.currentTarget as HTMLElement
    const column = handle.closest<HTMLElement>('[data-agent-column]')
    if (!column) return
    const initialIds = [...visibleIds()]
    const startIndex = initialIds.indexOf(agentId)
    if (startIndex < 0) return
    const columns = new Map(
      initialIds.flatMap((id) => {
        const element = boardElement?.querySelector<HTMLElement>(`[data-agent-id="${id}"]`)
        return element ? [[id, element] as const] : []
      }),
    )
    if (columns.size !== initialIds.length) return

    event.preventDefault()
    const startX = event.clientX
    let latestX = startX
    let frame: number | undefined
    let targetIndex = startIndex
    let settled = false
    let finishTimer: ReturnType<typeof setTimeout> | undefined
    const startRect = column.getBoundingClientRect()
    const slotRects = initialIds.map((id) => columns.get(id)!.getBoundingClientRect())
    const overlay = column.cloneNode(true) as HTMLElement
    overlay.classList.add('agent-column-drag-overlay')
    overlay.classList.remove('agent-column-drag-source', 'agent-column-reorder-shift')
    overlay.removeAttribute('data-agent-column')
    overlay.removeAttribute('data-agent-id')
    overlay.setAttribute('aria-hidden', 'true')
    overlay.setAttribute('inert', '')
    overlay.style.left = `${startRect.left}px`
    overlay.style.top = `${startRect.top}px`
    overlay.style.width = `${startRect.width}px`
    overlay.style.height = `${startRect.height}px`
    document.body.append(overlay)
    copyScrollPositions(column, overlay)
    column.classList.add('agent-column-drag-source')
    for (const [id, candidate] of columns) {
      if (id !== agentId) candidate.classList.add('agent-column-reorder-shift')
    }

    const targetFor = (clientX: number) => {
      const draggedCenter = startRect.left + startRect.width / 2 + (clientX - startX)
      return slotRects.reduce(
        (best, rect, index) => {
          const distance = Math.abs(draggedCenter - (rect.left + rect.width / 2))
          return distance < best.distance ? { index, distance } : best
        },
        { index: startIndex, distance: Number.POSITIVE_INFINITY },
      ).index
    }

    const applySiblingPreview = (nextTargetIndex: number) => {
      const preview = moveAgent(initialIds, agentId, nextTargetIndex)
      for (const [id, candidate] of columns) {
        if (id === agentId) continue
        const fromIndex = initialIds.indexOf(id)
        const toIndex = preview.indexOf(id)
        const delta = slotRects[toIndex]!.left - slotRects[fromIndex]!.left
        candidate.style.transform = Math.abs(delta) < 0.5
          ? ''
          : `translate3d(${delta}px, 0, 0)`
      }
    }

    const render = () => {
      frame = undefined
      const delta = latestX - startX
      overlay.style.transform = `translate3d(${delta}px, 0, 0) scale(1.01)`
      const nextTargetIndex = targetFor(latestX)
      if (nextTargetIndex !== targetIndex) {
        targetIndex = nextTargetIndex
        applySiblingPreview(targetIndex)
      }
    }
    const move = (pointerEvent: PointerEvent) => {
      latestX = pointerEvent.clientX
      if (frame === undefined) frame = requestAnimationFrame(render)
    }
    const removeListeners = () => {
      if (frame !== undefined) cancelAnimationFrame(frame)
      handle.removeEventListener('pointermove', move)
      handle.removeEventListener('pointerup', end)
      handle.removeEventListener('pointercancel', cancel)
    }

    const finishVisual = () => {
      if (settled) return
      settled = true
      if (finishTimer !== undefined) clearTimeout(finishTimer)
      overlay.remove()
      column.classList.remove('agent-column-drag-source')
      for (const [id, candidate] of columns) {
        if (id === agentId) continue
        candidate.classList.remove('agent-column-reorder-shift')
        candidate.style.transform = ''
      }
      if (cancelActiveReorder === cancel) cancelActiveReorder = undefined
    }

    const animateOverlayTo = (left: number) => {
      overlay.classList.add('agent-column-drag-overlay-settling')
      requestAnimationFrame(() => {
        overlay.style.transform = `translate3d(${left - startRect.left}px, 0, 0) scale(1)`
      })
      overlay.addEventListener('transitionend', finishVisual, { once: true })
      finishTimer = setTimeout(finishVisual, 220)
    }

    const cancel = () => {
      removeListeners()
      for (const [id, candidate] of columns) {
        if (id !== agentId) candidate.style.transform = ''
      }
      animateOverlayTo(startRect.left)
    }
    const end = (pointerEvent: PointerEvent) => {
      latestX = pointerEvent.clientX
      if (frame !== undefined) {
        cancelAnimationFrame(frame)
        frame = undefined
        render()
      }
      removeListeners()
      const next = moveAgent(initialIds, agentId, targetIndex)
      if (!sameOrder(next, initialIds)) persistOrder(next)

      // The columns now lay out in their final CSS-order slots. Remove preview
      // transforms before the browser paints so the virtualized DOM never moves.
      for (const [id, candidate] of columns) {
        if (id === agentId) continue
        candidate.classList.remove('agent-column-reorder-shift')
        candidate.style.transform = ''
      }
      requestAnimationFrame(() => {
        const finalRect = column.getBoundingClientRect()
        animateOverlayTo(finalRect.left)
      })
    }

    handle.setPointerCapture(event.pointerId)
    handle.addEventListener('pointermove', move)
    handle.addEventListener('pointerup', end, { once: true })
    handle.addEventListener('pointercancel', cancel, { once: true })
    cancelActiveReorder = cancel
  }

  const beginResize = (
    leftAgentId: string,
    rightAgentId: string,
    event: PointerEvent,
  ) => {
    if (!boardElement) return
    const handle = event.currentTarget as HTMLElement
    const leftColumn = boardElement.querySelector<HTMLElement>(
      `[data-agent-id="${leftAgentId}"]`,
    )
    const rightColumn = boardElement.querySelector<HTMLElement>(
      `[data-agent-id="${rightAgentId}"]`,
    )
    if (!leftColumn || !rightColumn) return
    event.preventDefault()
    const startX = event.clientX
    let latestX = startX
    let frame: number | undefined
    const currentWeights = weights()
    const leftWeight = currentWeights[leftAgentId] ?? 1
    const rightWeight = currentWeights[rightAgentId] ?? 1
    const combinedPixels =
      leftColumn.getBoundingClientRect().width +
      rightColumn.getBoundingClientRect().width

    const pairAt = (clientX: number) =>
      resizeAdjacentWeights(
        leftWeight,
        rightWeight,
        clientX - startX,
        combinedPixels,
      )
    const render = () => {
      frame = undefined
      const [nextLeft, nextRight] = pairAt(latestX)
      leftColumn.style.setProperty('--column-weight', String(nextLeft))
      rightColumn.style.setProperty('--column-weight', String(nextRight))
    }
    const move = (pointerEvent: PointerEvent) => {
      latestX = pointerEvent.clientX
      if (frame === undefined) frame = requestAnimationFrame(render)
    }
    const cleanup = () => {
      if (frame !== undefined) cancelAnimationFrame(frame)
      handle.removeEventListener('pointermove', move)
      handle.removeEventListener('pointerup', end)
      handle.removeEventListener('pointercancel', cancel)
    }
    const restore = () => {
      leftColumn.style.setProperty('--column-weight', String(leftWeight))
      rightColumn.style.setProperty('--column-weight', String(rightWeight))
    }
    const cancel = () => {
      cleanup()
      restore()
    }
    const end = (pointerEvent: PointerEvent) => {
      latestX = pointerEvent.clientX
      cleanup()
      const [nextLeft, nextRight] = pairAt(latestX)
      setWeights((current) => ({
        ...current,
        [leftAgentId]: nextLeft,
        [rightAgentId]: nextRight,
      }))
      props.onPatch({
        agents: {
          [leftAgentId]: { width_weight: nextLeft },
          [rightAgentId]: { width_weight: nextRight },
        },
      })
    }

    handle.setPointerCapture(event.pointerId)
    handle.addEventListener('pointermove', move)
    handle.addEventListener('pointerup', end, { once: true })
    handle.addEventListener('pointercancel', cancel, { once: true })
  }

  return (
    <>
      <Toolbar
        currentAgentCount={agents().length}
        activeProcessCount={activeProcessCount()}
        saving={props.saving}
        error={props.error}
        connectionState={props.connectionState}
        connectionError={props.connectionError}
        focusedAgentId={props.focusedAgentId}
        onOpenAgents={openAgents}
        onOpenSettings={openSettings}
      />
      <div class="board-wrap">
        <div ref={boardElement} class="agent-board" aria-label="Agent board">
          <Show
            when={visibleIds().length > 0}
            fallback={
              <section class="empty-board">
                <p>Waiting for the first Agent activity in this Local runtime.</p>
                <button type="button" class="text-button" onClick={openAgents}>
                  Open All Agents
                </button>
              </section>
            }
          >
            <For each={mountedIds()}>
              {(agentId) => {
                const agent = () => agentById().get(agentId)
                const visualIndex = () => visibleIds().indexOf(agentId)
                const nextId = () => visibleIds()[visualIndex() + 1]
                return (
                  <Show when={agent()}>
                    {(value) => (
                      <AgentColumn
                        agent={value()}
                        alias={props.preferences.agents[agentId]?.alias}
                        nowMs={props.nowMs}
                        weight={weights()[agentId] ?? 1}
                        order={Math.max(0, visualIndex())}
                        focused={Boolean(props.focusedAgentId)}
                        onFocus={
                          props.onFocusAgent ? () => props.onFocusAgent?.(agentId) : undefined
                        }
                        onHide={() => hideAgent(agentId)}
                        onAliasSave={(alias) =>
                          props.onPatch({ agents: { [agentId]: { alias } } })
                        }
                        onReorderPointerDown={(event) => beginReorder(agentId, event)}
                        onResizePointerDown={
                          nextId()
                            ? (event) => beginResize(agentId, nextId()!, event)
                            : undefined
                        }
                      >
                        {props.renderTimeline?.(agentId)}
                      </AgentColumn>
                    )}
                  </Show>
                )
              }}
            </For>
          </Show>
        </div>
      </div>
      <AgentDrawer
        open={drawerOpen()}
        agents={props.focusedAgentId ? (focusedDrawerAgents() ?? []) : agents()}
        visibleIds={visibleIds()}
        preferences={props.preferences}
        nowMs={props.nowMs}
        focusedAgentId={props.focusedAgentId}
        loading={Boolean(props.focusedAgentId && focusedDrawerAgents.loading)}
        error={focusedDrawerAgents.error instanceof Error
          ? focusedDrawerAgents.error.message
          : focusedDrawerAgents.error ? String(focusedDrawerAgents.error) : undefined}
        onRetry={() => void retryFocusedDrawerAgents()}
        onClose={() => setDrawerOpen(false)}
        onAdd={props.focusedAgentId
          ? (agentId) => props.onFocusAgent?.(agentId)
          : addAgent}
      />
      <SettingsDrawer
        open={settingsOpen()}
        preferences={props.preferences}
        onClose={() => setSettingsOpen(false)}
        onPatch={props.onPatch}
        onMaximumChange={setMaximum}
        showBoardSettings={!props.focusedAgentId}
      />
    </>
  )
}
