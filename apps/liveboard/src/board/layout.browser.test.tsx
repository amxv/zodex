import { createSignal } from 'solid-js'
import { render } from 'solid-js/web'
import { afterEach, describe, expect, it, vi } from 'vitest'

import type {
  ApiAgent,
  ApiTimelinePage,
  LiveboardPreferences,
  PresentationRecord,
} from '../api/client'
import { createAgentStreamController } from '../streams/AgentStreamController'
import { AgentTimeline } from '../timeline/AgentTimeline'
import '../styles.css'
import { Board } from './Board'

function agent(id: string, index: number): ApiAgent {
  return {
    id,
    first_seen_at_ms: index + 1,
    last_seen_at_ms: index + 1,
    seen_in_current_runtime: true,
    active_process_count: 0,
    workdirs: [],
  }
}

const preferences: LiveboardPreferences = {
  schema_version: 1,
  theme: 'system',
  max_visible_agents: 4,
  command_outputs_expanded: false,
  diffs_expanded: true,
  show_raw_button: false,
  editor_command: 'zed',
  agents: {},
}

function record(
  id: number,
  agentId: string,
  repository: string,
): PresentationRecord {
  return {
    presentation_id: `inv-${id}`,
    primary_invocation_id: id,
    raw_evidence_count: 1,
    raw_invocation_ids: [id],
    raw_invocation_ids_truncated: false,
    agent_id: agentId,
    declared_workdir: repository,
    normalized_workdir: repository,
    new_workdir: null,
    started_at_ms: id,
    duration_ms: 1,
    evidence: {
      evidence_state: 'complete',
      capture_state: 'not_applicable',
      degraded: false,
      reason: null,
    },
    kind: 'generic',
    tool_name: `tool-${agentId}-${id}`,
    status: 'success',
    summary: repository,
  }
}

function controller(agentId: string) {
  return createAgentStreamController({
    agentId,
    attachWatermarkMs: 10_000,
    loadHistoryPage: async (): Promise<ApiTimelinePage> => ({
      schema_version: 1,
      presentation_version: 3,
      runtime_id: 'runtime-concurrent',
      records: [],
      has_more: false,
      next_cursor: null,
    }),
    loadOutputMetadata: async () => {
      throw new Error('output metadata not expected')
    },
    loadDisplayOutputPage: async () => {
      throw new Error('output page not expected')
    },
  })
}

let disposeCurrent: (() => void) | undefined
let containerCurrent: HTMLDivElement | undefined

afterEach(() => {
  disposeCurrent?.()
  containerCurrent?.remove()
  disposeCurrent = undefined
  containerCurrent = undefined
})

describe('responsive Agent board layout', () => {
  it('opens a complete Agent drawer in focused mode without changing the focused timeline or board settings', async () => {
    const patches: unknown[] = []
    let rosterLoads = 0
    const focusTargets: string[] = []
    const container = document.createElement('div')
    container.style.width = '900px'
    container.style.height = '600px'
    document.body.append(container)
    containerCurrent = container
    disposeCurrent = render(
      () => (
        <div class="app-shell">
          <Board
            agents={[agent('a111', 0)]}
            preferences={preferences}
            focusedAgentId="a111"
            nowMs={10_000}
            saving={false}
            onPatch={(patch) => patches.push(patch)}
            onLoadAllAgents={async () => {
              rosterLoads += 1
              return [agent('a111', 0), agent('b222', 1), agent('c333', 2)]
            }}
            onFocusAgent={(agentId) => focusTargets.push(agentId)}
          />
        </div>
      ),
      container,
    )

    expect(container.querySelectorAll('[data-agent-column]')).toHaveLength(1)
    expect(container.textContent).toContain('Focused · a111')
    expect(container.querySelector('[aria-label="Drag Agent a111 to reorder"]')).toBeNull()
    expect(container.querySelector('[aria-label="Remove Agent a111 from board"]')).toBeNull()
    expect(rosterLoads).toBe(0)
    const focusedColumn = container.querySelector('[data-agent-column]')
    const allAgents = container.querySelector<HTMLButtonElement>('[aria-label="All Agents"]')!
    allAgents.click()
    await vi.waitFor(() => expect(container.querySelectorAll('.drawer-agent')).toHaveLength(3))
    expect(rosterLoads).toBe(1)
    expect(container.querySelector('[data-agent-column]')).toBe(focusedColumn)
    expect(container.querySelectorAll('[data-agent-column]')).toHaveLength(1)
    expect(container.textContent).toContain('Focused · a111')
    expect(container.querySelector<HTMLButtonElement>('.drawer-agent-selected')?.title)
      .toBe('a111 is already focused')
    expect(container.querySelector<HTMLButtonElement>('[title="Focus Agent b222"]')).not.toBeNull()
    expect(patches).toEqual([])

    container.querySelector<HTMLButtonElement>('[aria-label="Close All Agents"]')!.click()
    expect(container.querySelector('[data-agent-column]')).toBe(focusedColumn)
    allAgents.click()
    await vi.waitFor(() => expect(rosterLoads).toBe(2))
    await vi.waitFor(() => expect(container.querySelector('[title="Focus Agent b222"]')).not.toBeNull())
    container.querySelector<HTMLButtonElement>('[title="Focus Agent b222"]')!.click()
    expect(focusTargets).toEqual(['b222'])
    expect(patches).toEqual([])
  })

  it('allows retrying the focused Agent drawer roster after a fetch error', async () => {
    let attempts = 0
    const container = document.createElement('div')
    document.body.append(container)
    containerCurrent = container
    disposeCurrent = render(
      () => (
        <Board
          agents={[agent('a111', 0)]}
          preferences={preferences}
          focusedAgentId="a111"
          nowMs={10_000}
          saving={false}
          onPatch={() => undefined}
          onLoadAllAgents={async () => {
            attempts += 1
            if (attempts === 1) throw new Error('Observer offline')
            return [agent('a111', 0), agent('b222', 1)]
          }}
        />
      ),
      container,
    )

    container.querySelector<HTMLButtonElement>('[aria-label="All Agents"]')!.click()
    await vi.waitFor(() => expect(container.textContent).toContain('Observer offline'))
    expect(container.querySelectorAll('[data-agent-column]')).toHaveLength(1)
    container.querySelector<HTMLButtonElement>('.drawer-empty button')!.click()
    await vi.waitFor(() => expect(container.querySelectorAll('.drawer-agent')).toHaveLength(2))
    expect(attempts).toBe(2)
    expect(container.querySelectorAll('[data-agent-column]')).toHaveLength(1)
  })

  it('handles 0/1/2/5 current Agents while respecting the configured four-column cap', async () => {
    const [agents, setAgents] = createSignal<ApiAgent[]>([])
    const container = document.createElement('div')
    container.style.width = '1280px'
    container.style.height = '720px'
    document.body.append(container)
    containerCurrent = container
    disposeCurrent = render(
      () => (
        <div class="app-shell">
          <Board
            agents={agents()}
            preferences={preferences}
            nowMs={10_000}
            saving={false}
            onPatch={() => undefined}
          />
        </div>
      ),
      container,
    )

    const columns = () =>
      Array.from(document.querySelectorAll<HTMLElement>('[data-agent-column]'))
    expect(columns()).toHaveLength(0)
    expect(document.body.textContent).toContain('Waiting for the first Agent activity')

    setAgents([agent('a111', 0)])
    await vi.waitFor(() => expect(columns()).toHaveLength(1))
    const boardWidth = document.querySelector<HTMLElement>('.agent-board')!.getBoundingClientRect().width
    const oneColumnWidth = columns()[0]!.getBoundingClientRect().width
    expect(oneColumnWidth).toBeGreaterThan(boardWidth * 0.9)

    setAgents([agent('a111', 0), agent('b222', 1)])
    await vi.waitFor(() => expect(columns()).toHaveLength(2))
    expect(columns()[0]!.getBoundingClientRect().width).toBeGreaterThan(280)

    setAgents([
      agent('a111', 0),
      agent('b222', 1),
      agent('c333', 2),
      agent('d444', 3),
      agent('e555', 4),
    ])
    await vi.waitFor(() => expect(columns()).toHaveLength(4))
    expect(columns().map((column) => column.dataset.agentId)).toEqual([
      'a111',
      'b222',
      'c333',
      'd444',
    ])
  })

  it('keeps concurrent repository timelines mounted in their original column frames', async () => {
    const first = controller('a111')
    const second = controller('b222')
    const controllers = new Map([
      ['a111', first],
      ['b222', second],
    ])
    const container = document.createElement('div')
    container.style.width = '900px'
    container.style.height = '600px'
    document.body.append(container)
    containerCurrent = container
    disposeCurrent = render(
      () => (
        <div class="app-shell">
          <Board
            agents={[agent('a111', 0), agent('b222', 1)]}
            preferences={preferences}
            nowMs={10_000}
            saving={false}
            onPatch={() => undefined}
            renderTimeline={(agentId) => (
              <AgentTimeline controller={controllers.get(agentId)!} />
            )}
          />
        </div>
      ),
      container,
    )

    const firstFrame = container.querySelector<HTMLElement>(
      '[data-agent-id="a111"] [data-agent-timeline="a111"]',
    )!
    const secondFrame = container.querySelector<HTMLElement>(
      '[data-agent-id="b222"] [data-agent-timeline="b222"]',
    )!
    const firstRecoveryCheckpoint = first.recoveryCheckpoint()
    const secondRecoveryCheckpoint = second.recoveryCheckpoint()

    for (let index = 0; index < 8; index += 1) {
      first.upsert(record(100 + index, 'a111', '/repos/alpha'))
      second.upsert(record(200 + index, 'b222', '/repos/beta'))
      first.mergeRecovery(
        [record(100 + index, 'a111', '/repos/alpha-stale')],
        firstRecoveryCheckpoint,
      )
      second.mergeRecovery(
        [record(200 + index, 'b222', '/repos/beta-stale')],
        secondRecoveryCheckpoint,
      )
      await new Promise<void>((resolve) => requestAnimationFrame(() => resolve()))

      expect(
        container.querySelector('[data-agent-id="a111"] [data-agent-timeline="a111"]'),
      ).toBe(firstFrame)
      expect(
        container.querySelector('[data-agent-id="b222"] [data-agent-timeline="b222"]'),
      ).toBe(secondFrame)
      expect(firstFrame.textContent).toContain('/repos/alpha')
      expect(firstFrame.textContent).not.toContain('/repos/alpha-stale')
      expect(firstFrame.textContent).not.toContain('/repos/beta')
      expect(secondFrame.textContent).toContain('/repos/beta')
      expect(secondFrame.textContent).not.toContain('/repos/beta-stale')
      expect(secondFrame.textContent).not.toContain('/repos/alpha')
    }
  })
})
