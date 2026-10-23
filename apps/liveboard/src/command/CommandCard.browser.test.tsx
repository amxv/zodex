import { createSignal } from 'solid-js'
import { render } from 'solid-js/web'
import { afterEach, describe, expect, it, vi } from 'vitest'

import type {
  ApiOutputMetadataDocument,
  ApiOutputPage,
  PresentationRecord,
} from '../api/client'
import '../command.css'
import '../styles.css'
import { createAgentStreamController } from '../streams/AgentStreamController'
import { TimelineCard } from '../timeline/TimelineCard'
import { CommandCard } from './CommandCard'

type CommandRecord = Extract<PresentationRecord, { kind: 'command' }>

function command(overrides: Partial<CommandRecord> = {}): CommandRecord {
  return {
    presentation_id: 'inv-10',
    primary_invocation_id: 10,
    raw_evidence_count: 1,
    raw_invocation_ids: [10],
    raw_invocation_ids_truncated: false,
    agent_id: 'a111',
    declared_workdir: '/repo',
    normalized_workdir: '/repo',
    new_workdir: null,
    started_at_ms: 1_000,
    duration_ms: null,
    evidence: {
      evidence_state: 'complete',
      capture_state: 'complete',
      degraded: false,
      reason: null,
    },
    kind: 'command',
    command: 'cargo test --workspace',
    status: 'running',
    effective_cwd: '/repo',
    exit_code: null,
    termination_reason: null,
    output: null,
    output_truncated: false,
    polls: {
      count: 3,
      final_status: 'running',
      caller_agent_ids: ['a111'],
      cross_agent: false,
    },
    ...overrides,
  }
}

function noOutputMetadata(): ApiOutputMetadataDocument {
  return {
    schema_version: 1,
    runtime_id: 'runtime-one',
    invocation_id: 10,
    output: {
      available: false,
      chunk_count: 0,
      size_bytes: 0,
      capture_state: 'complete',
      capture_reason: null,
      first_cursor: null,
      last_cursor: null,
    },
  }
}

function emptyDisplayPage(): ApiOutputPage {
  return {
    schema_version: 1,
    runtime_id: 'runtime-one',
    invocation_id: 10,
    view: 'display',
    chunks: [],
    next_cursor: null,
    display_state: 'available',
  }
}

function controller() {
  return createAgentStreamController({
    agentId: 'a111',
    attachWatermarkMs: 2_000,
    loadHistoryPage: async () => ({
      schema_version: 1,
      presentation_version: 3,
      runtime_id: 'runtime-one',
      records: [],
      has_more: false,
      next_cursor: null,
    }),
    loadOutputMetadata: async () => noOutputMetadata(),
    loadDisplayOutputPage: async () => emptyDisplayPage(),
  })
}

let disposeCurrent: (() => void) | undefined
let containerCurrent: HTMLElement | undefined

afterEach(() => {
  disposeCurrent?.()
  containerCurrent?.remove()
  disposeCurrent = undefined
  containerCurrent = undefined
  vi.unstubAllGlobals()
})

function button(label: string) {
  const value = document.querySelector<HTMLButtonElement>(
    `button[aria-label="${label}"]`,
  )
  if (!value) throw new Error(`missing button ${label}`)
  return value
}

describe('command card', () => {
  it('makes the header expandable only after viewer-observed output actually arrives', async () => {
    const stream = controller()
    const current = command({ started_at_ms: 3_000 })
    stream.upsert(current, false)
    const container = document.createElement('div')
    document.body.append(container)
    containerCurrent = container
    disposeCurrent = render(
      () => (
        <CommandCard
          record={current}
          controller={stream}
          runtimeId="runtime-one"
          nowMs={4_000}
        />
      ),
      container,
    )

    const header = container.querySelector<HTMLElement>('.command-card-header')!
    expect(header.classList.contains('command-card-header-expandable')).toBe(false)
    expect(
      Array.from(container.querySelectorAll<HTMLButtonElement>('button')).some(
        (candidate) => candidate.textContent === 'Raw',
      ),
    ).toBe(false)
    stream.appendLiveOutput({
      presentationId: 'inv-10',
      invocationId: 10,
      sequence: 1,
      text: 'hello\n',
      displayState: 'available',
    })
    await vi.waitFor(() =>
      expect(header.classList.contains('command-card-header-expandable')).toBe(true),
    )
  })

  it('toggles output from the header while command/output copy controls stay independent', async () => {
    const writeText = vi.fn().mockResolvedValue(undefined)
    Object.defineProperty(navigator, 'clipboard', {
      configurable: true,
      value: { writeText },
    })
    const stream = controller()
    const current = command({ started_at_ms: 3_000 })
    stream.upsert(current, false)
    const container = document.createElement('div')
    document.body.append(container)
    containerCurrent = container
    disposeCurrent = render(
      () => (
        <CommandCard
          record={current}
          controller={stream}
          runtimeId="runtime-one"
          nowMs={4_000}
        />
      ),
      container,
    )
    stream.appendLiveOutput({
      presentationId: 'inv-10',
      invocationId: 10,
      sequence: 1,
      text: 'hello output\n',
      displayState: 'available',
    })
    const header = container.querySelector<HTMLElement>('.command-card-header')!
    await vi.waitFor(() => expect(header.classList.contains('command-card-header-expandable')).toBe(true))

    header.click()
    await vi.waitFor(() =>
      expect(container.querySelector('[aria-label="Command output"]')).not.toBeNull(),
    )
    expect(
      getComputedStyle(
        container.querySelector<HTMLButtonElement>('.command-output-copy-button')!,
      ).top,
    ).toBe('6px')

    button('Copy command').click()
    await vi.waitFor(() => expect(writeText).toHaveBeenCalledWith('cargo test --workspace'))
    expect(container.querySelector('[aria-label="Command output"]')).not.toBeNull()

    button('Copy command output').click()
    await vi.waitFor(() => expect(writeText).toHaveBeenCalledWith('hello output\n'))
    expect(container.querySelector('[aria-label="Command output"]')).not.toBeNull()

    header.click()
    await vi.waitFor(() =>
      expect(container.querySelector('[aria-label="Command output"]')).toBeNull(),
    )
  })

  it('keeps Raw and command copy visible beside a long command at narrow widths', () => {
    const stream = controller()
    const current = command({
      command:
        "node --input-type=module - <<'JS' import { chromium } from 'playwright'; console.log('a very long command that must wrap without pushing actions away'); JS",
    })
    const container = document.createElement('div')
    container.style.width = '280px'
    document.body.append(container)
    containerCurrent = container
    disposeCurrent = render(
      () => (
        <CommandCard
          record={current}
          controller={stream}
          runtimeId="runtime-one"
          nowMs={5_000}
          showRawButton
        />
      ),
      container,
    )

    const copy = container.querySelector<HTMLButtonElement>('[aria-label="Copy command"]')!
    const raw = Array.from(container.querySelectorAll<HTMLButtonElement>('button')).find(
      (candidate) => candidate.textContent === 'Raw',
    )!
    const bounds = container.getBoundingClientRect()
    expect(copy.getBoundingClientRect().right).toBeLessThanOrEqual(bounds.right + 1)
    expect(raw.getBoundingClientRect().right).toBeLessThanOrEqual(bounds.right + 1)
    expect(getComputedStyle(copy).display).not.toBe('none')
  })

  it('stays collapsed by default, coalesces 1000 live chunks, and transitions status accessibly', async () => {
    const stream = controller()
    const [record, setRecord] = createSignal(command())
    stream.upsert(record(), false)
    const container = document.createElement('div')
    container.style.width = '520px'
    document.body.append(container)
    containerCurrent = container
    disposeCurrent = render(
      () => (
        <CommandCard
          record={record()}
          controller={stream}
          runtimeId="runtime-one"
          nowMs={43_000}
        />
      ),
      container,
    )

    expect(container.querySelector('[aria-label="Command output"]')).toBeNull()
    expect(container.querySelector('[aria-label="Command running"]')).not.toBeNull()
    expect(container.textContent).toContain('Polled 3×')

    for (let sequence = 1; sequence <= 1_000; sequence += 1) {
      stream.appendLiveOutput({
        presentationId: 'inv-10',
        invocationId: 10,
        sequence,
        text: `${sequence}\n`,
        displayState: 'available',
      })
    }
    expect(container.querySelector('[aria-label="Command output"]')).toBeNull()

    container.querySelector<HTMLElement>('.command-card-header')!.click()
    await vi.waitFor(() =>
      expect(container.querySelector('[aria-label="Command output"]')).not.toBeNull(),
    )
    const output = container.querySelector<HTMLPreElement>(
      '[aria-label="Command output"]',
    )!
    await vi.waitFor(() => expect(output.textContent).toContain('1000\n'))

    let mutations = 0
    const observer = new MutationObserver((records) => {
      mutations += records.length
    })
    observer.observe(output, { childList: true, characterData: true, subtree: true })
    for (let sequence = 1_001; sequence <= 2_000; sequence += 1) {
      stream.appendLiveOutput({
        presentationId: 'inv-10',
        invocationId: 10,
        sequence,
        text: `${sequence}\n`,
        displayState: 'available',
      })
    }
    await new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve)))
    observer.disconnect()
    expect(output.textContent).toContain('2000\n')
    expect(mutations).toBeLessThanOrEqual(2)

    const completed = command({
      status: 'exited',
      duration_ms: 45_000,
      exit_code: 0,
      termination_reason: 'exit',
      polls: {
        count: 3,
        final_status: 'success',
        caller_agent_ids: ['a111'],
        cross_agent: false,
      },
    })
    stream.upsert(completed)
    setRecord(completed)
    await vi.waitFor(() =>
      expect(container.querySelector('[aria-label="Command exited successfully"]')).not.toBeNull(),
    )
    expect(container.textContent).not.toContain('Completed')
    expect(container.textContent).not.toContain('exit 0')
    expect(container.textContent).toContain('duration 45s')

    const failed = command({
      status: 'exited',
      duration_ms: 46_000,
      exit_code: 7,
      termination_reason: 'exit',
    })
    stream.upsert(failed)
    setRecord(failed)
    await vi.waitFor(() =>
      expect(container.querySelector('[aria-label="Failed with exit 7"]')).not.toBeNull(),
    )
    expect(container.textContent).not.toContain('Failed')
    expect(container.textContent).toContain('exit 7')

    const killed = command({
      status: 'exited',
      duration_ms: 47_000,
      exit_code: null,
      termination_reason: 'killed',
    })
    stream.upsert(killed)
    setRecord(killed)
    await vi.waitFor(() =>
      expect(container.querySelector('[aria-label="Killed"]')).not.toBeNull(),
    )

    const timedOut = command({
      status: 'exited',
      duration_ms: 48_000,
      exit_code: null,
      termination_reason: 'timeout',
    })
    stream.upsert(timedOut)
    setRecord(timedOut)
    await vi.waitFor(() =>
      expect(container.querySelector('[aria-label="Timed out"]')).not.toBeNull(),
    )
  })

  it('releases final collapsed output and lazily reloads only a bounded durable display tail on re-expansion', async () => {
    const metadata = vi.fn(async (): Promise<ApiOutputMetadataDocument> => ({
      schema_version: 1,
      runtime_id: 'runtime-one',
      invocation_id: 10,
      output: {
        available: true,
        chunk_count: 500,
        size_bytes: 50_000,
        capture_state: 'complete',
        capture_reason: null,
        first_cursor: 1,
        last_cursor: 500,
      },
    }))
    const page = vi.fn(async (_id: number, cursor: number, limit: number): Promise<ApiOutputPage> => ({
      schema_version: 1,
      runtime_id: 'runtime-one',
      invocation_id: 10,
      view: 'display',
      chunks: [
        { sequence: 499, observed_at_ms: 1, text: 'durable tail A\n' },
        { sequence: 500, observed_at_ms: 2, text: 'durable tail B\n' },
      ],
      next_cursor: null,
      display_state: 'available',
      display_reason: undefined,
    }))
    const stream = createAgentStreamController({
      agentId: 'a111',
      attachWatermarkMs: 2_000,
      loadHistoryPage: async () => ({
        schema_version: 1,
        presentation_version: 3,
        runtime_id: 'runtime-one',
        records: [],
        has_more: false,
        next_cursor: null,
      }),
      loadOutputMetadata: metadata,
      loadDisplayOutputPage: page,
    })
    const finalRecord = command({
      status: 'exited',
      duration_ms: 5_000,
      exit_code: 0,
      termination_reason: 'exit',
    })
    stream.upsert(finalRecord, false)
    const container = document.createElement('div')
    document.body.append(container)
    containerCurrent = container
    disposeCurrent = render(
      () => (
        <CommandCard
          record={finalRecord}
          controller={stream}
          runtimeId="runtime-one"
          nowMs={10_000}
        />
      ),
      container,
    )
    await vi.waitFor(() => expect(metadata).toHaveBeenCalledTimes(1))
    expect(page).not.toHaveBeenCalled()

    const header = container.querySelector<HTMLElement>('.command-card-header')!
    header.click()
    await vi.waitFor(() =>
      expect(container.querySelector('[aria-label="Command output"]')?.textContent).toContain(
        'durable tail B',
      ),
    )
    expect(container.querySelector('.command-output-region-dropped')).not.toBeNull()
    expect(container.querySelector('.output-notice-dropped')).not.toBeNull()
    expect(
      getComputedStyle(
        container.querySelector<HTMLButtonElement>('.command-output-copy-button')!,
      ).top,
    ).toBe('30px')
    expect(metadata).toHaveBeenCalledTimes(1)
    expect(page).toHaveBeenCalledWith(10, 437, 64)

    header.click()
    await vi.waitFor(() =>
      expect(container.querySelector('[aria-label="Command output"]')).toBeNull(),
    )
    header.click()
    await vi.waitFor(() => expect(metadata).toHaveBeenCalledTimes(2))
    await vi.waitFor(() => expect(page).toHaveBeenCalledTimes(2))
  })

  it('keeps final commands with no durable or exact output non-expandable', async () => {
    const metadata = vi.fn(async () => noOutputMetadata())
    const page = vi.fn(async () => emptyDisplayPage())
    const stream = createAgentStreamController({
      agentId: 'a111',
      attachWatermarkMs: 2_000,
      loadHistoryPage: async () => ({
        schema_version: 1,
        presentation_version: 3,
        runtime_id: 'runtime-one',
        records: [],
        has_more: false,
        next_cursor: null,
      }),
      loadOutputMetadata: metadata,
      loadDisplayOutputPage: page,
    })
    const finalRecord = command({
      status: 'exited',
      duration_ms: 20,
      exit_code: 0,
      termination_reason: 'exit',
    })
    const container = document.createElement('div')
    document.body.append(container)
    containerCurrent = container
    disposeCurrent = render(
      () => (
        <CommandCard
          record={finalRecord}
          controller={stream}
          runtimeId="runtime-one"
          nowMs={10_000}
        />
      ),
      container,
    )

    await vi.waitFor(() => expect(metadata).toHaveBeenCalledTimes(1))
    const header = container.querySelector<HTMLElement>('.command-card-header')!
    expect(header.classList.contains('command-card-header-expandable')).toBe(false)
    header.click()
    expect(container.querySelector('[aria-label="Command output"]')).toBeNull()
    expect(page).not.toHaveBeenCalled()
  })

  it('shows an explicit loading state while lazy durable output is being fetched', async () => {
    let resolvePage: ((page: ApiOutputPage) => void) | undefined
    const metadata = vi.fn(async (): Promise<ApiOutputMetadataDocument> => ({
      schema_version: 1,
      runtime_id: 'runtime-one',
      invocation_id: 10,
      output: {
        available: true,
        chunk_count: 1,
        size_bytes: 12,
        capture_state: 'complete',
        capture_reason: null,
        first_cursor: 0,
        last_cursor: 0,
      },
    }))
    const page = vi.fn(
      async () =>
        new Promise<ApiOutputPage>((resolve) => {
          resolvePage = resolve
        }),
    )
    const stream = createAgentStreamController({
      agentId: 'a111',
      attachWatermarkMs: 2_000,
      loadHistoryPage: async () => ({
        schema_version: 1,
        presentation_version: 3,
        runtime_id: 'runtime-one',
        records: [],
        has_more: false,
        next_cursor: null,
      }),
      loadOutputMetadata: metadata,
      loadDisplayOutputPage: page,
    })
    const finalRecord = command({
      status: 'exited',
      duration_ms: 20,
      exit_code: 0,
      termination_reason: 'exit',
    })
    const container = document.createElement('div')
    document.body.append(container)
    containerCurrent = container
    disposeCurrent = render(
      () => (
        <CommandCard
          record={finalRecord}
          controller={stream}
          runtimeId="runtime-one"
          nowMs={10_000}
        />
      ),
      container,
    )

    const header = container.querySelector<HTMLElement>('.command-card-header')!
    await vi.waitFor(() =>
      expect(header.classList.contains('command-card-header-expandable')).toBe(true),
    )
    header.click()
    await vi.waitFor(() => expect(container.textContent).toContain('Loading output…'))
    expect(container.querySelector('[aria-label="Command output"]')?.textContent).toBe('')

    resolvePage!({
      schema_version: 1,
      runtime_id: 'runtime-one',
      invocation_id: 10,
      view: 'display',
      chunks: [{ sequence: 0, observed_at_ms: 1, text: 'loaded output\n' }],
      next_cursor: null,
      display_state: 'available',
    })
    await vi.waitFor(() =>
      expect(container.querySelector('[aria-label="Command output"]')?.textContent).toContain(
        'loaded output',
      ),
    )
    expect(container.textContent).not.toContain('Loading output…')
  })

  it('shows a visible output error and lets the user retry after a transient failure', async () => {
    const metadata = vi.fn(async (): Promise<ApiOutputMetadataDocument> => ({
      schema_version: 1,
      runtime_id: 'runtime-one',
      invocation_id: 10,
      output: {
        available: true,
        chunk_count: 1,
        size_bytes: 25,
        capture_state: 'complete',
        capture_reason: null,
        first_cursor: 0,
        last_cursor: 0,
      },
    }))
    let failed = false
    const page = vi.fn(async (): Promise<ApiOutputPage> => {
      if (!failed) {
        failed = true
        throw new Error('Liveboard request timed out after 15s')
      }
      return {
        schema_version: 1,
        runtime_id: 'runtime-one',
        invocation_id: 10,
        view: 'display',
        chunks: [{ sequence: 0, observed_at_ms: 1, text: 'recovered output\n' }],
        next_cursor: null,
        display_state: 'available',
      }
    })
    const stream = createAgentStreamController({
      agentId: 'a111',
      attachWatermarkMs: 2_000,
      loadHistoryPage: async () => ({
        schema_version: 1, presentation_version: 3,
        runtime_id: 'runtime-one', records: [], has_more: false, next_cursor: null,
      }),
      loadOutputMetadata: metadata,
      loadDisplayOutputPage: page,
    })
    const container = document.createElement('div')
    document.body.append(container)
    containerCurrent = container
    disposeCurrent = render(() => (
      <CommandCard
        record={command({status: 'exited', duration_ms: 10, exit_code: 0, termination_reason: 'exit'})}
        controller={stream}
        runtimeId="runtime-one"
        nowMs={10_000}
      />
    ), container)
    const header = container.querySelector<HTMLElement>('.command-card-header')!
    await vi.waitFor(() => expect(header.classList.contains('command-card-header-expandable')).toBe(true))
    header.click()
    await vi.waitFor(() => expect(container.textContent).toContain('Liveboard request timed out after 15s'))
    expect(container.textContent).not.toContain('Loading output…')
    const retry = Array.from(container.querySelectorAll<HTMLButtonElement>('button'))
      .find((button) => button.textContent?.includes('Retry output'))
    expect(retry).toBeDefined()
    retry!.click()
    await vi.waitFor(() => expect(container.querySelector('[aria-label="Command output"]')?.textContent)
      .toContain('recovered output'))
    expect(page).toHaveBeenCalledTimes(2)
    expect(container.textContent).not.toContain('Retry output')
  })

  it('renders stdin and kill as distinct compact interaction cards with cross-Agent context', () => {
    const stream = controller()
    const base = {
      primary_invocation_id: 20,
      raw_evidence_count: 1,
      raw_invocation_ids: [20],
      raw_invocation_ids_truncated: false,
      agent_id: 'a111',
      declared_workdir: '/repo',
      normalized_workdir: '/repo',
      new_workdir: null,
      started_at_ms: 1_000,
      duration_ms: 10,
      evidence: {
        evidence_state: 'complete',
        capture_state: 'complete',
        degraded: false,
        reason: null,
      },
    }
    const stdinRecord: PresentationRecord = {
      ...base,
      presentation_id: 'inv-20',
      kind: 'stdin',
      target_session_handle: 'opaque-session',
      chars: 'yes\n',
      chars_truncated: false,
      creator_agent_id: 'b222',
      cross_agent: true,
      result_status: 'running',
    }
    const killRecord: PresentationRecord = {
      ...base,
      presentation_id: 'inv-21',
      primary_invocation_id: 21,
      raw_invocation_ids: [21],
      kind: 'kill',
      target_session_handle: 'opaque-session',
      target_command: 'cargo test --workspace',
      creator_agent_id: 'b222',
      cross_agent: true,
      result_status: 'requested',
    }
    const container = document.createElement('div')
    document.body.append(container)
    containerCurrent = container
    disposeCurrent = render(
      () => (
        <>
          <TimelineCard
            record={stdinRecord}
            controller={stream}
            runtimeId="runtime-one"
            nowMs={2_000}
          />
          <TimelineCard
            record={killRecord}
            controller={stream}
            runtimeId="runtime-one"
            nowMs={2_000}
          />
        </>
      ),
      container,
    )
    expect(container.textContent).toContain('stdin')
    expect(container.textContent).toContain('yes')
    expect(container.textContent).toContain('kill')
    expect(container.textContent).toContain('cargo test --workspace')
    expect(container.textContent).not.toContain('Process termination requested')
    expect(container.textContent).not.toContain('requested')
    expect(container.textContent?.match(/targets Agent b222/g)).toHaveLength(2)
  })

  it('renders failed generic tools with the same red status dot language as failed commands', () => {
    const stream = controller()
    const failedGeneric: PresentationRecord = {
      presentation_id: 'inv-22',
      primary_invocation_id: 22,
      raw_evidence_count: 1,
      raw_invocation_ids: [22],
      raw_invocation_ids_truncated: false,
      agent_id: 'a111',
      declared_workdir: '/repo',
      normalized_workdir: '/repo',
      new_workdir: null,
      started_at_ms: 1_000,
      duration_ms: 10,
      evidence: {
        evidence_state: 'complete',
        capture_state: 'complete',
        degraded: false,
        reason: null,
      },
      kind: 'generic',
      tool_name: 'apply_patch',
      status: 'failed',
      summary: 'Failed to find expected lines',
    }
    const container = document.createElement('div')
    document.body.append(container)
    containerCurrent = container
    disposeCurrent = render(
      () => (
        <TimelineCard
          record={failedGeneric}
          controller={stream}
          runtimeId="runtime-one"
          nowMs={2_000}
        />
      ),
      container,
    )
    expect(container.querySelector('.card-status-failed')).not.toBeNull()
    expect(container.textContent).toContain('apply_patch')
    expect(container.textContent).toContain('failed')
  })

  it('loads checkpoint metadata only after Raw opens, then exact input/output only after selection', async () => {
    const requests: string[] = []
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL) => {
        const path =
          typeof input === 'string'
            ? input
            : input instanceof URL
              ? input.toString()
              : input.url
        requests.push(path)
        if (path.startsWith('api/timeline/inv-10/checkpoints')) {
          return Response.json({
            schema_version: 1,
            presentation_version: 3,
            runtime_id: 'runtime-one',
            presentation_id: 'inv-10',
            checkpoints: [
              {
                invocation_id: 10,
                checkpoint_kind: 'initial',
                agent_id: 'a111',
                started_at_ms: 1,
                completed_at_ms: 2,
                status: 'running',
                cross_agent: false,
                evidence_state: 'complete',
                capture_state: 'complete',
              },
            ],
            has_more: false,
            next_cursor: null,
          })
        }
        if (path === 'api/invocations/10') {
          return Response.json({
            schema_version: 1,
            presentation_version: 3,
            runtime_id: 'runtime-one',
            invocation: {
              id: 10,
              correlation_id: 'fixture',
              agent_id: 'a111',
              provider_kind: null,
              tool_name: 'exec_command',
              arguments: { cmd: '<script>argument()</script>' },
              declared_workdir_exact: '/repo',
              declared_workdir_normalized: '/repo',
              is_new_workdir: false,
              started_at_ms: 1,
              completed_at_ms: 2,
              duration_ms: 1,
              outcome_kind: 'success',
              result: { output: '<script>alert(1)</script>' },
              error: null,
              evidence_state: 'complete',
              evidence_reason: null,
              capture_state: 'complete',
              capture_reason: null,
              target_session_handle: null,
              target_created_by_agent_id: null,
              cross_agent: null,
            },
            output: noOutputMetadata().output,
          })
        }
        return Response.json({ error: 'not found' }, { status: 404 })
      }),
    )
    const stream = controller()
    const container = document.createElement('div')
    document.body.append(container)
    containerCurrent = container
    disposeCurrent = render(
      () => (
        <CommandCard
          record={command()}
          controller={stream}
          runtimeId="runtime-one"
          nowMs={5_000}
          showRawButton
        />
      ),
      container,
    )
    expect(requests).toHaveLength(0)

    const auditButton = Array.from(container.querySelectorAll('button')).find(
      (candidate) => candidate.textContent === 'Raw',
    )!
    auditButton.click()
    expect(auditButton.textContent).toBe('Hide Raw')
    await vi.waitFor(() => expect(requests).toHaveLength(1))
    expect(requests[0]).toContain('/checkpoints')
    expect(requests.some((request) => request === 'api/invocations/10')).toBe(false)

    await vi.waitFor(() => expect(container.textContent).toContain('Initial response'))
    const initialButton = Array.from(container.querySelectorAll('button')).find(
      (candidate) => candidate.textContent === 'Initial response',
    )!
    initialButton.click()
    await vi.waitFor(() =>
      expect(requests.some((request) => request === 'api/invocations/10')).toBe(true),
    )
    await vi.waitFor(() => expect(container.querySelectorAll('.audit-evidence')).toHaveLength(2))
    const evidence = Array.from(container.querySelectorAll<HTMLPreElement>('.audit-evidence'))
    expect(evidence[0]!.textContent).toContain('<script>argument()</script>')
    expect(evidence[1]!.textContent).toContain('<script>alert(1)</script>')
    expect(
      container.querySelector('button[aria-label="Copy tool input"]'),
    ).not.toBeNull()
    expect(
      container.querySelector('button[aria-label="Copy tool output"]'),
    ).not.toBeNull()
    expect(container.textContent).not.toContain('Exact logical evidence')
    expect(container.querySelector('script')).toBeNull()
  })
})
