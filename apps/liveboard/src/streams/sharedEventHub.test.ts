import { describe, expect, it } from 'vitest'

import { createSharedEventHub, type HubPort, type HubRequest, type HubResponse, type HubSource } from './sharedEventHub'

class FakeSource implements HubSource {
  onopen: ((event: Event) => void) | null = null
  onerror: ((event: Event) => void) | null = null
  closed = false
  readonly listeners = new Map<string, (event: MessageEvent<string>) => void>()

  constructor(readonly url: string) {}

  addEventListener(type: string, callback: (event: MessageEvent<string>) => void): void {
    this.listeners.set(type, callback)
  }

  open() { this.onopen?.(new Event('open')) }
  error() { this.onerror?.(new Event('error')) }
  close() { this.closed = true }
  emit(type: string, agent: string | null, extra: Record<string, unknown> = {}) {
    this.listeners.get(type)?.({ data: JSON.stringify({
      schema_version: 4, runtime_id: 'runtime', sequence: 1,
      emitted_at_ms: 1_000, event_type: type, agent_id: agent,
      invocation_id: 1, presentation_id: null, presentation_revision: null,
      payload: {}, ...extra,
    }) } as MessageEvent<string>)
  }
}

class FakePort implements HubPort {
  onmessage: ((event: MessageEvent<HubRequest>) => void) | null = null
  readonly responses: HubResponse[] = []
  start() {}
  postMessage(message: HubResponse) { this.responses.push(message) }
  send(request: HubRequest) {
    this.onmessage?.({ data: request } as MessageEvent<HubRequest>)
  }
  get events() { return this.responses.filter((message) => message.kind === 'event') }
}

const upstream = 'http://127.0.0.1:7777/private/api/events'
const subscribed = (agent: string, id = 1) => ({
  kind: 'subscribe' as const,
  id,
  url: `${upstream}?agent_id=${agent}&output_agent_ids=${agent}&diffs=full`,
})

describe('one browser-wide SSE stream for many Liveboard tabs', () => {
  it('multiplexes 20 focused views over one upstream, with output routed per Agent', async () => {
    const sources: FakeSource[] = []
    const hub = createSharedEventHub('http://127.0.0.1:7777', (url) => {
      const source = new FakeSource(url)
      sources.push(source)
      return source
    })
    const ports = Array.from({ length: 20 }, () => new FakePort())
    for (const [index, port] of ports.entries()) {
      hub.attach(port)
      port.send(subscribed(index.toString(36).padStart(4, '0')))
    }
    await Promise.resolve()
    expect(sources).toHaveLength(1)
    expect(new URL(sources[0]!.url).searchParams.has('output_agent_ids')).toBe(false)
    expect(new URL(sources[0]!.url).searchParams.has('agent_id')).toBe(false)
    expect(new URL(sources[0]!.url).searchParams.get('diffs')).toBe('summary')
    sources[0]!.open()
    for (const port of ports) expect(port.responses).toContainEqual({ kind: 'open', id: 1 })

    sources[0]!.emit('output', '0007')
    expect(ports[7]!.events).toHaveLength(1)
    expect(ports[0]!.events).toHaveLength(0)
    expect(ports[19]!.events).toHaveLength(0)

    // All focus views receive alias invalidations on this same SSE socket.
    sources[0]!.emit('preferences_changed', null)
    for (const port of ports) {
      expect(port.events.at(-1)).toMatchObject({ kind: 'event', eventType: 'preferences_changed' })
    }

    for (const port of ports) port.send({ kind: 'disconnect' })
    await Promise.resolve()
    expect(sources[0]!.closed).toBe(true)
  })

  it('keeps exactly one socket during dynamic Agent selection, and recovers on reconnect', async () => {
    const sources: FakeSource[] = []
    const hub = createSharedEventHub('http://127.0.0.1:7777', (url) => {
      const source = new FakeSource(url)
      sources.push(source)
      return source
    })
    const first = new FakePort()
    const second = new FakePort()
    hub.attach(first)
    hub.attach(second)
    first.send(subscribed('a111'))
    await Promise.resolve()
    sources[0]!.open()
    second.send(subscribed('b222'))
    await Promise.resolve()
    expect(sources).toHaveLength(1)
    expect(sources[0]!.closed).toBe(false)
    expect(second.responses).toContainEqual({ kind: 'open', id: 1 })
    sources[0]!.error()
    expect(first.responses).toContainEqual({ kind: 'error', id: 1 })
    sources[0]!.open()
    expect(first.responses.filter((item) => item.kind === 'open')).toHaveLength(2)
    first.send({ kind: 'disconnect' })
    second.send({ kind: 'disconnect' })
    await Promise.resolve()
    expect(sources[0]!.closed).toBe(true)
  })

  it('passes canonical summary diffs without per-tab copies and scales beyond 32 focused Agents', async () => {
    const sources: FakeSource[] = []
    const hub = createSharedEventHub('http://127.0.0.1:7777', (url) => {
      const source = new FakeSource(url)
      sources.push(source)
      return source
    })
    const ports = Array.from({ length: 40 }, () => new FakePort())
    for (const [index, port] of ports.entries()) {
      hub.attach(port)
      const subscription = subscribed(index.toString(36).padStart(4, '0'))
      port.send(subscription)
    }
    await Promise.resolve()
    expect(sources).toHaveLength(1)
    expect(new URL(sources[0]!.url).searchParams.has('output_agent_ids')).toBe(false)
    expect(new URL(sources[0]!.url).searchParams.get('diffs')).toBe('summary')
    sources[0]!.open()
    sources[0]!.emit('presentation_updated', '0000', {payload: {record: {
      kind: 'file_changes', changes: [{lines: [], diff_lines_included: false}],
    }}})
    const delivered = JSON.parse((ports[0]!.events.at(-1) as Extract<HubResponse, {kind: 'event'}>).data)
    expect(delivered.payload.record.changes[0].lines).toEqual([])
    expect(delivered.payload.record.changes[0].diff_lines_included).toBe(false)
    expect(ports[1]!.events).toHaveLength(0)
    ports.forEach((port) => port.send({kind:'disconnect'}))
  })
})
