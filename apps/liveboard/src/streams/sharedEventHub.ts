import { LIVE_EVENT_TYPES } from './eventTypes'

// One EventSource per browser storage partition, shared by every Liveboard tab.
// The worker owns no timeline/output buffers: each tab stores only its own
// visible Agent's bounded state and fetches durable details on demand.
export type HubRequest =
  | { kind: 'subscribe'; id: number; url: string }
  | { kind: 'unsubscribe'; id: number }
  | { kind: 'disconnect' }

export type HubResponse =
  | { kind: 'open'; id: number }
  | { kind: 'error'; id: number }
  | { kind: 'event'; id: number; eventType: string; data: string }

export interface HubPort {
  postMessage(message: HubResponse): void
  onmessage: ((event: MessageEvent<HubRequest>) => void) | null
  start(): void
}

export interface HubSource {
  onopen: ((event: Event) => void) | null
  onerror: ((event: Event) => void) | null
  addEventListener(type: string, callback: (event: MessageEvent<string>) => void): void
  close(): void
}

interface Subscription {
  id: number
  agent: string | null
  outputAgents: Set<string>
  opened: boolean
}

interface Connection {
  source: HubSource
  url: string
  opened: boolean
}

const OUTPUT_EVENTS = new Set(['output', 'output_complete'])
const AGENT_ID = /^[a-z0-9]{4}$/

export function createSharedEventHub(
  origin: string,
  createSource: (url: string) => HubSource,
) {
  const clients = new Map<HubPort, Map<number, Subscription>>()
  let connection: Connection | undefined

  const subscribers = () =>
    [...clients.entries()].flatMap(([port, entries]) =>
      [...entries.values()].map((subscription) => ({ port, subscription })),
    )

  const send = (port: HubPort, message: HubResponse) => {
    try {
      port.postMessage(message)
    } catch {
      // A gone tab must not break other viewers' realtime updates.
      clients.delete(port)
      reconcile()
    }
  }

  const dispatch = (current: Connection, type: string, data: string) => {
    if (connection !== current) return
    let event: { event_type: string; agent_id: string | null }
    try {
      event = JSON.parse(data) as typeof event
      if (event.event_type !== type) return
    } catch {
      // A malformed event can't affect other tabs. Their own recovery path
      // will reread canonical history when EventSource reconnects.
      for (const { port, subscription } of subscribers()) {
        send(port, { kind: 'error', id: subscription.id })
      }
      return
    }

    const global = type === 'preferences_changed' || type === 'gap'
    for (const { port, subscription } of subscribers()) {
      if (!global && subscription.agent && event.agent_id !== subscription.agent) continue
      if (
        OUTPUT_EVENTS.has(type) &&
        (!event.agent_id || !subscription.outputAgents.has(event.agent_id))
      ) continue
      send(port, { kind: 'event', id: subscription.id, eventType: type, data })
    }
  }

  const openSubscribers = () => {
    for (const { port, subscription } of subscribers()) {
      subscription.opened = true
      send(port, { kind: 'open', id: subscription.id })
    }
  }

  function reconcile() {
    if (subscribers().length === 0) {
      connection?.source.close()
      connection = undefined
    }
  }

  const detach = (port: HubPort) => {
    clients.delete(port)
    port.onmessage = null
    reconcile()
  }

  const attach = (port: HubPort) => {
    clients.set(port, new Map())
    port.onmessage = (message) => {
      const request = message.data
      if (request.kind === 'disconnect') {
        detach(port)
        return
      }
      const entries = clients.get(port)
      if (!entries) return
      if (request.kind === 'unsubscribe') {
        entries.delete(request.id)
        reconcile()
        return
      }
      if (request.kind !== 'subscribe') return
      let url: URL
      try {
        url = new URL(request.url)
      } catch { return }
      if (url.origin !== origin || !url.pathname.endsWith('/api/events')) return

      const outputAgents = new Set((url.searchParams.get('output_agent_ids') ?? '')
        .split(',').filter((id) => AGENT_ID.test(id)))
      const focused = url.searchParams.get('agent_id')
      entries.set(request.id, {
        id: request.id,
        agent: focused && AGENT_ID.test(focused) ? focused : null,
        outputAgents,
        opened: false,
      })

      if (!connection) {
        // Never reconfigure the upstream SSE when a tab's focus or output
        // projection changes. The server sends lightweight summaries plus
        // PTY chunks on ONE stable stream; the worker filters per-tab and
        // leaves full diff/history hydration to each tab's on-demand APIs.
        // This trades one local copy of unwatched PTY chunks for guaranteed
        // single-socket operation during 10+ parallel focused Liveboards.
        url.searchParams.delete('agent_id')
        url.searchParams.delete('output_agent_ids')
        url.searchParams.set('diffs', 'summary')
        const upstream = url.toString()
        const current: Connection = {
          source: createSource(upstream),
          url: upstream,
          opened: false,
        }
        connection = current
        current.source.onopen = () => {
          if (connection !== current) return
          current.opened = true
          // EventSource automatically reconnects on network failure. Every
          // tab rereads a bounded timeline/output tail on reconnection.
          openSubscribers()
        }
        current.source.onerror = () => {
          if (connection !== current) return
          for (const { port, subscription } of subscribers()) {
            send(port, { kind: 'error', id: subscription.id })
          }
        }
        for (const type of LIVE_EVENT_TYPES) {
          current.source.addEventListener(type, (event) => dispatch(current, type, event.data))
        }
      } else if (connection.opened) {
        const subscription = entries.get(request.id)
        if (subscription) {
          subscription.opened = true
          send(port, { kind: 'open', id: request.id })
        }
      }
    }
    port.start()
  }

  return { attach, detach }
}
