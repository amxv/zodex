import { createSignal } from 'solid-js'
import { render } from 'solid-js/web'
import { afterEach, describe, expect, it, vi } from 'vitest'

import type {
  ApiOutputMetadataDocument,
  ApiOutputPage,
  PresentationFileChange,
  PresentationRecord,
} from '../api/client'
import '../diff.css'
import '../styles.css'
import { applyTheme } from '../app/bootstrap'
import { createAgentStreamController } from '../streams/AgentStreamController'
import {
  defaultDiffHighlighter,
  type DiffHighlighter,
} from './HighlightWorkerClient'
import { FileChangesCard } from './FileChangeCard'
import type { DiffHighlightInput, DiffHighlightResult } from './protocol'

type FileChangesRecord = Extract<PresentationRecord, { kind: 'file_changes' }>

function noOutputMetadata(): ApiOutputMetadataDocument {
  return {
    schema_version: 1,
    runtime_id: 'runtime-one',
    invocation_id: 1,
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

function emptyOutputPage(): ApiOutputPage {
  return {
    schema_version: 1,
    runtime_id: 'runtime-one',
    invocation_id: 1,
    view: 'display',
    chunks: [],
    next_cursor: null,
    display_state: 'available',
  }
}

function controller(requestFullPresentation: (presentationId: string) => void = () => undefined) {
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
    loadDisplayOutputPage: async () => emptyOutputPage(),
    requestFullPresentation,
  })
}

function change(
  operation: PresentationFileChange['operation'],
  path: string,
  overrides: Partial<PresentationFileChange> = {},
): PresentationFileChange {
  return {
    operation,
    path,
    old_path: null,
    write_mode: null,
    added: 1,
    removed: 1,
    diff_truncated: false,
    diff_lines_included: true,
    lines: [{ kind: 'context', old_line: 1, new_line: 1, text: 'source' }],
    ...overrides,
  }
}

function record(changes: PresentationFileChange[]): FileChangesRecord {
  return {
    presentation_id: 'inv-40',
    primary_invocation_id: 40,
    raw_evidence_count: 1,
    raw_invocation_ids: [40],
    raw_invocation_ids_truncated: false,
    agent_id: 'a111',
    declared_workdir: '/repo',
    normalized_workdir: '/repo',
    new_workdir: null,
    started_at_ms: 1_000,
    duration_ms: 20,
    evidence: {
      evidence_state: 'complete',
      capture_state: 'complete',
      degraded: false,
      reason: null,
    },
    kind: 'file_changes',
    source_tool: 'apply_patch',
    changes,
  }
}

function escaped(text: string) {
  return text
    .replaceAll('&', '&amp;')
    .replaceAll('<', '&lt;')
    .replaceAll('>', '&gt;')
    .replaceAll('"', '&quot;')
}

class ImmediateHighlighter implements DiffHighlighter {
  readonly calls: DiffHighlightInput[] = []
  isReady = () => true
  eagerLanguages = () => []
  highlight = async (input: DiffHighlightInput): Promise<DiffHighlightResult> => {
    this.calls.push(input)
    return {
      subjectKey: input.subjectKey,
      revision: input.revision,
      language: input.language,
      rows: input.rows.map((row) => ({
        index: row.index,
        html: `<span class="hljs-keyword">${escaped(row.text)}</span>`,
      })),
    }
  }
}

class DeferredHighlighter implements DiffHighlighter {
  readonly calls: DiffHighlightInput[] = []
  readonly resolvers: Array<(result: DiffHighlightResult) => void> = []
  isReady = () => true
  eagerLanguages = () => []
  highlight = (input: DiffHighlightInput) => {
    this.calls.push(input)
    return new Promise<DiffHighlightResult>((resolve) => this.resolvers.push(resolve))
  }

  resolve(index: number, html: string) {
    const input = this.calls[index]!
    this.resolvers[index]?.({
      subjectKey: input.subjectKey,
      revision: input.revision,
      language: input.language,
      rows: input.rows.map((row) => ({ index: row.index, html })),
    })
  }
}

let disposeCurrent: (() => void) | undefined
let containerCurrent: HTMLDivElement | undefined

afterEach(() => {
  disposeCurrent?.()
  containerCurrent?.remove()
  disposeCurrent = undefined
  containerCurrent = undefined
  vi.useRealTimers()
  vi.unstubAllGlobals()
  Reflect.deleteProperty(navigator, 'clipboard')
  applyTheme('system')
})

function mount(
  value: () => FileChangesRecord,
  stream: ReturnType<typeof controller>,
  highlighter: DiffHighlighter,
) {
  const container = document.createElement('div')
  container.style.width = '520px'
  document.body.append(container)
  containerCurrent = container
  disposeCurrent = render(
    () => (
      <FileChangesCard record={value()} controller={stream} highlighter={highlighter} />
    ),
    container,
  )
  return container
}

describe('file change cards', () => {
  it('opens the exact changed path through the configured local editor endpoint without toggling the diff', async () => {
    const fetchMock = vi.fn(
      async (_input: RequestInfo | URL, _init?: RequestInit) =>
        new Response(null, { status: 204 }),
    )
    vi.stubGlobal('fetch', fetchMock)
    const stream = controller()
    const value = record([
      change('edited', '/Users/example/repo/src/main.rs', {
        lines: [{ kind: 'add', old_line: null, new_line: 1, text: 'fn main() {}' }],
      }),
    ])
    const container = mount(() => value, stream, new ImmediateHighlighter())
    expect(container.querySelector('[aria-label="Unified diff for /Users/example/repo/src/main.rs"]')).not.toBeNull()

    container
      .querySelector<HTMLButtonElement>(
        'button[aria-label="Open /Users/example/repo/src/main.rs in configured editor"]',
      )!
      .click()
    await vi.waitFor(() => expect(fetchMock).toHaveBeenCalledTimes(1))
    expect(fetchMock.mock.calls[0]?.[0]).toBe('api/open-file')
    expect(fetchMock.mock.calls[0]?.[1]).toMatchObject({ method: 'POST' })
    expect(JSON.parse(String(fetchMock.mock.calls[0]?.[1]?.body))).toEqual({
      path: '/Users/example/repo/src/main.rs',
    })
    expect(container.querySelector('[aria-label="Unified diff for /Users/example/repo/src/main.rs"]')).not.toBeNull()
  })

  it('shows copy success as a check icon for two seconds without changing button width', async () => {
    vi.useFakeTimers()
    const writeText = vi.fn().mockResolvedValue(undefined)
    Object.defineProperty(navigator, 'clipboard', {
      configurable: true,
      value: { writeText },
    })
    const stream = controller()
    const highlighter = new ImmediateHighlighter()
    const value = record([
      change('edited', '/repo/src/main.rs', {
        lines: [{ kind: 'add', old_line: null, new_line: 1, text: 'const next = 1' }],
      }),
    ])
    const container = mount(() => value, stream, highlighter)
    const copyButton = container.querySelector<HTMLButtonElement>(
      'button[aria-label="Copy diff for /repo/src/main.rs"]',
    )!
    const widthBefore = copyButton.getBoundingClientRect().width

    copyButton.click()
    await Promise.resolve()
    await Promise.resolve()

    expect(writeText).toHaveBeenCalledWith('+const next = 1')
    expect(copyButton.getAttribute('aria-label')).toBe('Diff copied for /repo/src/main.rs')
    expect(copyButton.getBoundingClientRect().width).toBe(widthBefore)

    await vi.advanceTimersByTimeAsync(1_999)
    expect(copyButton.getAttribute('aria-label')).toBe('Diff copied for /repo/src/main.rs')
    await vi.advanceTimersByTimeAsync(1)
    expect(copyButton.getAttribute('aria-label')).toBe('Copy diff for /repo/src/main.rs')
  })

  it('renders canonical operations, gutters, tones and one highlight batch per expanded file', async () => {
    const stream = controller()
    const highlighter = new ImmediateHighlighter()
    const value = record([
      change('edited', '/repo/src/main.rs', {
        added: 1,
        removed: 1,
        lines: [
          { kind: 'context', old_line: 8, new_line: 8, text: 'fn main() {' },
          { kind: 'remove', old_line: 9, new_line: null, text: '    let old = 1;' },
          { kind: 'add', old_line: null, new_line: 9, text: '    let next = 2;' },
        ],
      }),
      change('created', '/repo/ui/View.tsx', { added: 1, removed: 0 }),
      change('deleted', '/repo/README.md', { added: 0, removed: 1 }),
      change('renamed', '/repo/Cargo.toml', {
        old_path: '/repo/config.toml',
        write_mode: 'overwrite',
      }),
    ])
    const container = mount(() => value, stream, highlighter)

    await vi.waitFor(() => expect(highlighter.calls).toHaveLength(4))
    expect(container.textContent).toContain('Edited')
    expect(container.textContent).toContain('Added')
    expect(container.textContent).toContain('Deleted')
    expect(container.textContent).toContain('Renamed')
    expect(container.textContent).toContain('config.toml')
    expect(container.textContent).toContain('Cargo.toml')
    expect(container.textContent).toContain('+1')
    expect(container.textContent).toContain('-1')
    expect(container.querySelectorAll('[data-diff-row-kind]')).toHaveLength(6)
    expect(container.querySelectorAll('.diff-row-context')).toHaveLength(4)
    expect(container.querySelectorAll('.diff-row-remove')).toHaveLength(1)
    expect(container.querySelectorAll('.diff-row-add')).toHaveLength(1)
    expect(container.querySelector('.diff-gutter-old')?.textContent).toBe('8')
    expect(container.querySelector('.diff-gutter-new')?.textContent).toBe('8')
    expect(highlighter.calls[0]?.language).toBe('rust')
    expect(highlighter.calls[0]?.rows).toHaveLength(3)
    expect(highlighter.calls[1]?.language).toBe('typescript')
    expect(highlighter.calls[2]?.language).toBe('markdown')
    expect(highlighter.calls[3]?.language).toBe('ini')
    expect(container.querySelector('.diff-source-highlight .hljs-keyword')).not.toBeNull()
    expect(
      container.querySelector(
        '[data-diff-key="inv-40:file-change:2"] .diff-source-highlight',
      ),
    ).not.toBeNull()
    expect(
      container.querySelector(
        '[data-diff-key="inv-40:file-change:2"] .diff-source-highlight',
      )?.textContent,
    ).toBe('source')

    const editedHeader = container.querySelector<HTMLElement>(
      '[data-diff-key="inv-40:file-change:0"] .file-change-header',
    )!
    expect(editedHeader.querySelector('.diff-expand-button')).toBeNull()
    editedHeader.click()
    await vi.waitFor(() =>
      expect(container.querySelector('[aria-label="Unified diff for /repo/src/main.rs"]')).toBeNull(),
    )
    expect(container.querySelectorAll('[data-diff-row-kind]')).toHaveLength(3)

    stream.setDiffExpansionDefault(false)
    expect(container.querySelectorAll('[data-diff-row-kind]')).toHaveLength(0)
    stream.setDiffExpansionDefault(true)
    await vi.waitFor(() => expect(container.querySelectorAll('[data-diff-row-kind]')).toHaveLength(6))
    expect(highlighter.calls.length).toBeGreaterThanOrEqual(8)
  })

  it('does no highlighting work while collapsed, then batches all bounded rows once on expansion', async () => {
    const stream = controller()
    stream.setDiffExpansionDefault(false)
    const highlighter = new ImmediateHighlighter()
    const lines = Array.from({ length: 500 }, (_, index) => ({
      kind: index % 3 === 0 ? 'add' : index % 3 === 1 ? 'remove' : 'context',
      old_line: index % 3 === 0 ? null : index + 1,
      new_line: index % 3 === 1 ? null : index + 1,
      text: `let value_${index} = ${index};`,
    }))
    const value: FileChangesRecord = {
      ...record([
      change('edited', '/repo/src/large.rs', {
        added: 1_100,
        removed: 1_100,
        diff_truncated: true,
        lines,
      }),
      ]),
      evidence: {
        evidence_state: 'partial',
        capture_state: 'degraded',
        degraded: true,
        reason: 'snapshot unavailable',
      },
    }
    const container = mount(() => value, stream, highlighter)
    expect(container.querySelector('.diff-body')).toBeNull()
    expect(highlighter.calls).toHaveLength(0)

    container.querySelector<HTMLElement>(
      '[data-diff-key="inv-40:file-change:0"] .file-change-header',
    )!.click()
    await vi.waitFor(() => expect(highlighter.calls).toHaveLength(1))
    expect(highlighter.calls[0]?.rows).toHaveLength(500)
    expect(container.querySelectorAll('[data-diff-row-kind]')).toHaveLength(500)
    expect(container.textContent).toContain('Diff preview truncated at the server display bound.')
    expect(container.textContent).toContain('Evidence incomplete · snapshot unavailable')
    expect(container.querySelector('.file-change-card-degraded')).not.toBeNull()
  })

  it('requests a summary-only diff body only when that diff becomes expanded', async () => {
    const requestFullPresentation = vi.fn()
    const stream = controller(requestFullPresentation)
    stream.setDiffExpansionDefault(false)
    const highlighter = new ImmediateHighlighter()
    const value = record([
      change('edited', '/repo/src/summary.ts', {
        diff_lines_included: false,
        lines: [],
      }),
    ])
    const container = mount(() => value, stream, highlighter)
    expect(requestFullPresentation).not.toHaveBeenCalled()
    expect(container.textContent).not.toContain('Loading diff…')

    container.querySelector<HTMLElement>(
      '[data-diff-key="inv-40:file-change:0"] .file-change-header',
    )!.click()
    await vi.waitFor(() =>
      expect(requestFullPresentation).toHaveBeenCalledWith('inv-40'),
    )
    expect(requestFullPresentation).toHaveBeenCalledTimes(1)
    expect(container.textContent).toContain('Loading diff…')
    expect(highlighter.calls).toHaveLength(0)
  })

  it('rejects a late highlight response for an older content revision', async () => {
    const stream = controller()
    const highlighter = new DeferredHighlighter()
    const [value, setValue] = createSignal(
      record([change('edited', '/repo/src/main.rs', { lines: [
        { kind: 'context', old_line: 1, new_line: 1, text: 'old source' },
      ] })]),
    )
    const container = mount(value, stream, highlighter)
    await vi.waitFor(() => expect(highlighter.calls).toHaveLength(1))

    setValue(record([change('edited', '/repo/src/main.rs', { lines: [
      { kind: 'context', old_line: 1, new_line: 1, text: 'new source' },
    ] })]))
    await vi.waitFor(() => expect(highlighter.calls).toHaveLength(2))
    highlighter.resolve(1, '<span class="hljs-string">NEW</span>')
    await vi.waitFor(() => expect(container.textContent).toContain('NEW'))
    highlighter.resolve(0, '<span class="hljs-string">OLD</span>')
    await new Promise<void>((resolve) => queueMicrotask(() => resolve()))
    expect(container.textContent).toContain('NEW')
    expect(container.textContent).not.toContain('OLD')
  })

  it('renders all eager languages at practical board widths and recolors tokens without re-highlighting', async () => {
    const stream = controller()
    const worker = defaultDiffHighlighter()
    let highlightCalls = 0
    const highlighter: DiffHighlighter = {
      isReady: worker.isReady,
      eagerLanguages: worker.eagerLanguages,
      highlight: (input) => {
        highlightCalls += 1
        return worker.highlight(input)
      },
    }
    const fixtures = [
      ['/repo/src/main.rs', 'fn main() {}'],
      ['/repo/ui/View.tsx', 'const value: string = "x"'],
      ['/repo/ui/client.js', 'const value = "x"'],
      ['/repo/cmd/server.go', 'package main'],
      ['/repo/scripts/build.sh', 'echo "$HOME"'],
      ['/repo/tools/check.py', 'def value():'],
      ['/repo/ui/styles.css', '.button { display: grid; }'],
      ['/repo/config/data.json', '{"value": 1}'],
      ['/repo/Cargo.toml', 'edition = "2024"'],
    ] as const
    const value = record(
      fixtures.map(([path, text]) =>
        change('edited', path, {
          added: 1,
          removed: 0,
          lines: [{ kind: 'add', old_line: null, new_line: 1, text }],
        }),
      ),
    )
    const container = mount(() => value, stream, highlighter)
    await vi.waitFor(
      () => expect(container.querySelectorAll('.diff-source-highlight')).toHaveLength(9),
      { timeout: 5_000 },
    )
    expect(highlightCalls).toBe(9)

    for (const width of [920, 460, 280]) {
      container.style.width = `${width}px`
      await new Promise<void>((resolve) => requestAnimationFrame(() => resolve()))
      for (const header of container.querySelectorAll<HTMLElement>('.file-change-header')) {
        expect(header.scrollWidth).toBeLessThanOrEqual(header.clientWidth + 1)
        expect(
          getComputedStyle(header.querySelector<HTMLButtonElement>('.diff-copy-button')!).display,
        ).not.toBe('none')
      }
    }

    applyTheme('light')
    const keyword = container.querySelector<HTMLElement>('.hljs-keyword')!
    const addRow = container.querySelector<HTMLElement>('.diff-row-add')!
    const lightToken = getComputedStyle(keyword).color
    const lightRow = getComputedStyle(addRow).backgroundColor
    applyTheme('dark')
    await new Promise<void>((resolve) => requestAnimationFrame(() => resolve()))
    const darkToken = getComputedStyle(keyword).color
    const darkRow = getComputedStyle(addRow).backgroundColor
    expect(darkToken).not.toBe(lightToken)
    expect(darkRow).not.toBe(lightRow)
    expect(highlightCalls).toBe(9)
  })
})
