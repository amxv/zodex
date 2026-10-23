import { render } from 'solid-js/web'
import { afterEach, describe, expect, it, vi } from 'vitest'

import type {
  ApiAgent,
  LiveboardAgentPreference,
  LiveboardPreferences,
  LiveboardPreferencesPatch,
} from '../api/client'
import '../styles.css'
import { App } from './App'
import { applyTheme } from './bootstrap'

function agent(id: string, index: number): ApiAgent {
  const workdirs = [
    {
      normalized_workdir: `/workspace/repos/project-${index + 1}`,
      ordinal: 0,
      first_seen_at_ms: 1_000,
      last_seen_at_ms: 2_000 + index,
      first_invocation_id: index + 1,
      last_invocation_id: index + 1,
      retained_invocation_count: 1,
    },
  ]
  if (index === 0) {
    workdirs.push(
      {
        normalized_workdir: '/workspace/repos/project-1/packages/a-very-long-workdir-one',
        ordinal: 1,
        first_seen_at_ms: 1_100,
        last_seen_at_ms: 2_100,
        first_invocation_id: 10,
        last_invocation_id: 10,
        retained_invocation_count: 1,
      },
      {
        normalized_workdir: '/workspace/repos/project-1/packages/a-very-long-workdir-two',
        ordinal: 2,
        first_seen_at_ms: 1_200,
        last_seen_at_ms: 2_200,
        first_invocation_id: 11,
        last_invocation_id: 11,
        retained_invocation_count: 1,
      },
    )
  }
  return {
    id,
    first_seen_at_ms: 1_000 + index * 100,
    last_seen_at_ms: Date.now() - index * 10_000,
    seen_in_current_runtime: true,
    active_process_count: index === 0 ? 1 : 0,
    workdirs,
  }
}

function mergePreferences(
  current: LiveboardPreferences,
  patch: LiveboardPreferencesPatch,
): LiveboardPreferences {
  const agents = { ...current.agents }
  for (const [id, update] of Object.entries(patch.agents ?? {})) {
    agents[id] = { ...(agents[id] ?? {}), ...update }
  }
  return {
    ...current,
    ...(patch.theme === undefined ? {} : { theme: patch.theme }),
    ...(patch.max_visible_agents === undefined
      ? {}
      : { max_visible_agents: patch.max_visible_agents }),
    ...(patch.command_outputs_expanded === undefined
      ? {}
      : { command_outputs_expanded: patch.command_outputs_expanded }),
    ...(patch.diffs_expanded === undefined
      ? {}
      : { diffs_expanded: patch.diffs_expanded }),
    ...(patch.show_raw_button === undefined
      ? {}
      : { show_raw_button: patch.show_raw_button }),
    ...(patch.editor_command === undefined
      ? {}
      : { editor_command: patch.editor_command }),
    agents,
  }
}

function element<T extends Element>(selector: string): T {
  const found = document.querySelector<T>(selector)
  if (!found) throw new Error(`missing browser-test element: ${selector}`)
  return found
}

function buttonWithText(text: string): HTMLButtonElement {
  const found = Array.from(document.querySelectorAll<HTMLButtonElement>('button')).find(
    (button) => button.textContent?.includes(text),
  )
  if (!found) throw new Error(`missing browser-test button containing: ${text}`)
  return found
}

function dispatchChange(select: HTMLSelectElement, value: string) {
  select.value = value
  select.dispatchEvent(new Event('change', { bubbles: true }))
}

let disposeCurrent: (() => void) | undefined
let containerCurrent: HTMLDivElement | undefined

afterEach(() => {
  disposeCurrent?.()
  containerCurrent?.remove()
  disposeCurrent = undefined
  containerCurrent = undefined
  vi.unstubAllGlobals()
  applyTheme('system')
})

describe('Liveboard board shell', () => {
  it('applies external alias changes immediately in columns and All Agents, with last write winning', async () => {
    const liveSources: Array<{
      url: string
      emit: () => void
      closed: () => boolean
    }> = []
    vi.stubGlobal('EventSource', class {
      onopen: (() => void) | null = null
      onerror: (() => void) | null = null
      private listeners = new Map<string, (event: Event) => void>()
      private isClosed = false
      constructor(readonly url: string) {
        liveSources.push({
          url,
          emit: () => this.listeners.get('preferences_changed')?.(new Event('preferences_changed')),
          closed: () => this.isClosed,
        })
      }
      addEventListener(type: string, callback: (event: Event) => void) {
        this.listeners.set(type, callback)
      }
      close() { this.isClosed = true }
    })

    let preferences: LiveboardPreferences = {
      schema_version: 1,
      theme: 'system',
      max_visible_agents: 4,
      command_outputs_expanded: false,
      diffs_expanded: true,
      show_raw_button: false,
      editor_command: 'zed',
      agents: {},
    }
    vi.stubGlobal('fetch', vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const path = typeof input === 'string' ? input : input.toString()
      if (path === 'preferences') {
        if (init?.method === 'PATCH') {
          preferences = mergePreferences(preferences, JSON.parse(String(init.body)) as LiveboardPreferencesPatch)
        }
        return Response.json(preferences)
      }
      if (path === 'api/status') return Response.json({
        schema_version: 1, api_version: 1, presentation_version: 3,
        runtime_id: 'alias-browser-runtime', current_runtime_agent_count: 1, active_process_count: 0,
      })
      if (path === 'api/agents?runtime=current') return Response.json({
        schema_version: 1, runtime_id: 'alias-browser-runtime', agents: [agent('a111', 0)],
      })
      return Response.json({error: 'missing'}, {status: 404})
    }))

    const container = document.createElement('div')
    document.body.append(container)
    containerCurrent = container
    disposeCurrent = render(() => <App />, container)
    await vi.waitFor(() => expect(element<HTMLElement>('[data-agent-id="a111"] .agent-id').textContent).toBe('a111'))
    expect(document.querySelector('.agent-alias')).toBeNull()

    const aliasEvents = liveSources.find((source) => source.url === 'preferences/events')
    expect(aliasEvents).toBeDefined()
    preferences = mergePreferences(preferences, {agents: {a111: {alias: 'Rindle implementation'}}})
    aliasEvents!.emit()
    await vi.waitFor(() => expect(element<HTMLElement>('[data-agent-id="a111"] .agent-alias').textContent).toBe('Rindle implementation'))

    buttonWithText('All Agents').click()
    await vi.waitFor(() => expect(element<HTMLElement>('.drawer-agent-identity strong').textContent).toBe('Rindle implementation'))
    preferences = mergePreferences(preferences, {agents: {a111: {alias: 'Rindle CLI test suite'}}})
    aliasEvents!.emit()
    await vi.waitFor(() => {
      expect(element<HTMLElement>('[data-agent-id="a111"] .agent-alias').textContent).toBe('Rindle CLI test suite')
      expect(element<HTMLElement>('.drawer-agent-identity strong').textContent).toBe('Rindle CLI test suite')
    })

    disposeCurrent?.()
    disposeCurrent = undefined
    expect(aliasEvents!.closed()).toBe(true)
  })

  it('manages current Agents without eviction and persists UI-only board changes', async () => {
    const agents = [
      agent('a111', 0),
      agent('b222', 1),
      agent('c333', 2),
      agent('d444', 3),
      agent('e555', 4),
    ]
    let preferences: LiveboardPreferences = {
      schema_version: 1,
      theme: 'system',
      max_visible_agents: 4,
      command_outputs_expanded: false,
      diffs_expanded: true,
      show_raw_button: false,
      editor_command: 'zed',
      agents: {} satisfies Record<string, LiveboardAgentPreference>,
    }
    const patches: LiveboardPreferencesPatch[] = []

    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
        const path =
          typeof input === 'string'
            ? input
            : input instanceof URL
              ? input.toString()
              : input.url
        if (path === 'api/status') {
          return Response.json({
            schema_version: 1,
            api_version: 1,
            presentation_version: 3,
            runtime_id: 'runtime-board-browser',
            current_runtime_agent_count: agents.length,
            active_process_count: 1,
          })
        }
        if (path === 'api/agents?runtime=current') {
          return Response.json({
            schema_version: 1,
            runtime_id: 'runtime-board-browser',
            agents,
          })
        }
        if (path === 'preferences' && (init?.method ?? 'GET') === 'GET') {
          return Response.json(preferences)
        }
        if (path === 'preferences' && init?.method === 'PATCH') {
          const patch = JSON.parse(String(init.body)) as LiveboardPreferencesPatch
          patches.push(patch)
          preferences = mergePreferences(preferences, patch)
          return Response.json(preferences)
        }
        return Response.json({ error: 'not found' }, { status: 404 })
      }),
    )

    const container = document.createElement('div')
    document.body.append(container)
    containerCurrent = container
    disposeCurrent = render(() => <App />, container)
    await vi.waitFor(() => expect(document.body.textContent).toContain('zodex'))

    const visibleColumns = () =>
      Array.from(document.querySelectorAll<HTMLElement>('[data-agent-column]'))
        .sort(
          (left, right) =>
            left.getBoundingClientRect().left - right.getBoundingClientRect().left,
        )
        .map((column) => column.dataset.agentId)
    expect(visibleColumns()).toEqual(['a111', 'b222', 'c333', 'd444'])
    expect(visibleColumns()).not.toContain('e555')
    expect(
      element<HTMLElement>('[data-agent-id="a111"] .workdir-badge').textContent,
    ).toContain('/workspace/repos/project-1')
    buttonWithText('All Agents').click()
    await vi.waitFor(() => expect(document.body.textContent).toContain('All Agents'))
    expect(document.body.textContent).not.toContain('Current Local runtime')
    expect(document.body.textContent).toContain('1 active process')
    const firstDrawerRow = element<HTMLElement>('.drawer-agent-main')
    expect(firstDrawerRow.firstElementChild?.classList.contains('activity-dot')).toBe(true)
    const firstDrawerButton = element<HTMLButtonElement>('.drawer-agent-selected')
    await vi.waitFor(() => expect(firstDrawerButton.disabled).toBe(false))
    const firstWorkdirToggle = element<HTMLButtonElement>(
      '.agent-drawer button[aria-label="Expand workdirs for Agent a111"]',
    )
    expect(
      firstWorkdirToggle.getBoundingClientRect().top -
        firstDrawerButton.getBoundingClientRect().top,
    ).toBeLessThan(16)
    firstDrawerButton.click()
    await vi.waitFor(() =>
      expect(document.querySelector('[aria-label="Expanded Agent workdirs"]')).not.toBeNull(),
    )
    const expandedWorkdirs = element<HTMLElement>('[aria-label="Expanded Agent workdirs"]')
    expect(parseFloat(getComputedStyle(expandedWorkdirs).borderTopWidth)).toBeGreaterThan(0)
    firstDrawerButton.click()
    await vi.waitFor(() =>
      expect(document.querySelector('[aria-label="Expanded Agent workdirs"]')).toBeNull(),
    )
    const fifthRow = Array.from(
      document.querySelectorAll<HTMLButtonElement>('.drawer-agent'),
    ).find((button) => button.textContent?.includes('e555'))
    expect(fifthRow).toBeDefined()
    expect(fifthRow?.disabled).toBe(true)
    expect(document.querySelectorAll('.drawer-agent-selected')).toHaveLength(4)
    expect(document.body.textContent).not.toContain('On board')
    element<HTMLButtonElement>('button[aria-label="Close All Agents"]').click()

    element<HTMLButtonElement>('button[aria-label="Remove Agent d444 from board"]').click()
    expect(visibleColumns()).toEqual(['a111', 'b222', 'c333'])
    await vi.waitFor(() =>
      expect(
        patches.some((patch) => patch.agents?.d444?.visible === false),
      ).toBe(true),
    )

    buttonWithText('All Agents').click()
    const addFifth = Array.from(
      document.querySelectorAll<HTMLButtonElement>('.drawer-agent'),
    ).find((button) => button.textContent?.includes('e555'))
    expect(addFifth?.disabled).toBe(false)
    addFifth?.click()
    expect(visibleColumns()).toEqual(['a111', 'b222', 'c333', 'e555'])
    element<HTMLButtonElement>('button[aria-label="Close All Agents"]').click()

    element<HTMLButtonElement>('button[aria-label="Edit alias for Agent e555"]').click()
    const aliasInput = element<HTMLInputElement>('input[aria-label="Alias for Agent e555"]')
    expect(getComputedStyle(aliasInput).outlineStyle).toBe('none')
    aliasInput.value = 'release checks'
    aliasInput.dispatchEvent(new InputEvent('input', { bubbles: true, data: 'release checks' }))
    aliasInput.dispatchEvent(new KeyboardEvent('keydown', { bubbles: true, key: 'Enter' }))
    await vi.waitFor(() =>
      expect(
        patches.some((patch) => patch.agents?.e555?.alias === 'release checks'),
      ).toBe(true),
    )
    await vi.waitFor(() => expect(document.body.textContent).toContain('release checks'))
    const aliasedColumn = element<HTMLElement>('[data-agent-id="e555"]')
    const alias = element<HTMLElement>('[data-agent-id="e555"] .agent-alias')
    const agentId = element<HTMLElement>('[data-agent-id="e555"] .agent-id')
    expect(agentId.textContent).toBe('e555')
    const aliasBox = alias.getBoundingClientRect()
    const idBox = agentId.getBoundingClientRect()
    expect(
      Math.abs((aliasBox.top + aliasBox.bottom) / 2 - (idBox.top + idBox.bottom) / 2),
    ).toBeLessThan(1)

    const activityTime = aliasedColumn.querySelector<HTMLElement>('.activity-time')!
    const workdirBadges = aliasedColumn.querySelector<HTMLElement>('.workdir-badges')!
    const workdirGap =
      workdirBadges.getBoundingClientRect().left - activityTime.getBoundingClientRect().right
    expect(workdirGap).toBeGreaterThanOrEqual(0)
    expect(workdirGap).toBeLessThanOrEqual(8)

    const patchCountBeforeReorderDrag = patches.length
    const reorderHandle = element<HTMLButtonElement>(
      'button[aria-label="Drag Agent c333 to reorder"]',
    )
    const reorderColumn = element<HTMLElement>('[data-agent-id="c333"]')
    const firstColumn = element<HTMLElement>('[data-agent-id="a111"]')
    const timelineBefore = element<HTMLElement>('[data-agent-timeline="c333"]')
    const reorderBox = reorderColumn.getBoundingClientRect()
    const firstBox = firstColumn.getBoundingClientRect()
    const startX = reorderBox.left + reorderBox.width / 2
    const targetX = firstBox.left + firstBox.width / 2
    reorderHandle.setPointerCapture = () => undefined
    reorderHandle.dispatchEvent(
      new PointerEvent('pointerdown', { bubbles: true, pointerId: 5, clientX: startX }),
    )
    reorderHandle.dispatchEvent(
      new PointerEvent('pointermove', { bubbles: true, pointerId: 5, clientX: targetX }),
    )
    await new Promise<void>((resolve) => requestAnimationFrame(() => resolve()))
    expect(patches).toHaveLength(patchCountBeforeReorderDrag)
    const dragOverlay = element<HTMLElement>('.agent-column-drag-overlay')
    expect(reorderColumn.classList.contains('agent-column-drag-source')).toBe(true)
    expect(Number.parseInt(getComputedStyle(dragOverlay).zIndex, 10)).toBeGreaterThan(20)
    expect(
      [firstColumn, element<HTMLElement>('[data-agent-id="b222"]')].some(
        (column) => column.style.transform.includes('translate3d'),
      ),
    ).toBe(true)
    reorderHandle.dispatchEvent(
      new PointerEvent('pointerup', { bubbles: true, pointerId: 5, clientX: targetX }),
    )
    await vi.waitFor(() => expect(visibleColumns()[0]).toBe('c333'))
    await vi.waitFor(() =>
      expect(patches.length).toBe(patchCountBeforeReorderDrag + 1),
    )
    expect(element<HTMLElement>('[data-agent-timeline="c333"]')).toBe(timelineBefore)
    expect(timelineBefore.isConnected).toBe(true)
    await vi.waitFor(() =>
      expect(document.querySelector('.agent-column-drag-overlay')).toBeNull(),
    )
    expect(document.querySelector('[aria-label^="Move Agent "]')).toBeNull()

    const patchCountBeforeResize = patches.length
    const resizeHandle = element<HTMLButtonElement>(
      'button[aria-label="Resize Agent a111 column"]',
    )
    resizeHandle.setPointerCapture = () => undefined
    resizeHandle.dispatchEvent(
      new PointerEvent('pointerdown', { bubbles: true, pointerId: 7, clientX: 300 }),
    )
    resizeHandle.dispatchEvent(
      new PointerEvent('pointermove', { bubbles: true, pointerId: 7, clientX: 340 }),
    )
    expect(patches).toHaveLength(patchCountBeforeResize)
    resizeHandle.dispatchEvent(
      new PointerEvent('pointerup', { bubbles: true, pointerId: 7, clientX: 340 }),
    )
    await vi.waitFor(() => expect(patches.length).toBe(patchCountBeforeResize + 1))
    expect(patches.at(-1)?.agents?.a111?.width_weight).toBeTypeOf('number')

    buttonWithText('Settings').click()
    await vi.waitFor(() => expect(document.body.textContent).toContain('Timeline defaults'))
    expect(document.body.textContent).toContain('Command output')
    expect(document.body.textContent).toContain('File diffs')
    expect(document.body.textContent).toContain('Raw tool button')
    expect(document.body.textContent).toContain('Editor command')
    expect(document.body.textContent).not.toContain('Cmd closed')
    expect(document.body.textContent).not.toContain('Diff open')

    dispatchChange(element<HTMLSelectElement>('select[aria-label="Maximum visible Agents"]'), '2')
    expect(visibleColumns()).toHaveLength(2)
    await vi.waitFor(() =>
      expect(patches.some((patch) => patch.max_visible_agents === 2)).toBe(true),
    )
    const maximumPatch = patches.find((patch) => patch.max_visible_agents === 2)
    expect(
      Object.values(maximumPatch?.agents ?? {}).filter(
        (preference) => preference.visible === false,
      ),
    ).toHaveLength(2)

    dispatchChange(
      element<HTMLSelectElement>('select[aria-label="Default command output state"]'),
      'expanded',
    )
    await vi.waitFor(() =>
      expect(
        patches.some((patch) => patch.command_outputs_expanded === true),
      ).toBe(true),
    )
    dispatchChange(
      element<HTMLSelectElement>('select[aria-label="Default file diff state"]'),
      'collapsed',
    )
    await vi.waitFor(() =>
      expect(patches.some((patch) => patch.diffs_expanded === false)).toBe(true),
    )

    const rawVisibility = element<HTMLSelectElement>(
      'select[aria-label="Raw tool button visibility"]',
    )
    expect(rawVisibility.value).toBe('hidden')
    dispatchChange(rawVisibility, 'shown')
    await vi.waitFor(() =>
      expect(patches.some((patch) => patch.show_raw_button === true)).toBe(true),
    )

    dispatchChange(element<HTMLSelectElement>('select[aria-label="Liveboard theme"]'), 'dark')
    await vi.waitFor(() => expect(document.documentElement.dataset.theme).toBe('dark'))
    expect(getComputedStyle(document.documentElement).colorScheme).toBe('dark')

    buttonWithText('Cursor').click()
    await vi.waitFor(() =>
      expect(patches.some((patch) => patch.editor_command === 'cursor')).toBe(true),
    )
    expect(element<HTMLInputElement>('input[aria-label="Editor command"]').value).toBe('cursor')
  })
})
