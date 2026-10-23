import type { EventSourceLike } from './runtime'
import type { HubRequest, HubResponse } from './sharedEventHub'

// One MessagePort per tab, and a single SharedWorker EventSource shared
// between every open Liveboard in this browser profile/storage partition.
let shared: SharedWorker | undefined
let nextId = 0
const subscriptions = new Map<number, WorkerEventSource>()

function getSharedWorker(): SharedWorker | undefined {
  if (typeof SharedWorker === 'undefined') return undefined
  if (shared) return shared
  try {
    const worker = new SharedWorker(
      new URL('./SharedEventStream.worker.ts', import.meta.url),
      { type: 'module', name: 'zodex-liveboard-events' },
    )
    worker.port.onmessage = (event: MessageEvent<HubResponse>) => {
      const message = event.data
      const subscription = subscriptions.get(message.id)
      if (subscription) subscription.dispatch(message)
    }
    worker.port.start()
    shared = worker
    return worker
  } catch {
    // Browsers without SharedWorker still retain the original SSE behavior.
    // Modern Chromium, Firefox, and Safari support the shared transport.
    return undefined
  }
}

class WorkerEventSource implements EventSourceLike {
  onopen: ((event: Event) => void) | null = null
  onerror: ((event: Event) => void) | null = null
  private readonly listeners = new Map<string, Set<EventListener>>()
  private closed = false

  constructor(readonly id: number) {}

  addEventListener(type: string, listener: EventListener): void {
    const listeners = this.listeners.get(type) ?? new Set<EventListener>()
    listeners.add(listener)
    this.listeners.set(type, listeners)
  }

  dispatch(message: HubResponse): void {
    if (this.closed) return
    if (message.kind === 'open') {
      this.onopen?.(new Event('open'))
      return
    }
    if (message.kind === 'error') {
      this.onerror?.(new Event('error'))
      return
    }
    const event = new MessageEvent(message.eventType, { data: message.data })
    for (const listener of this.listeners.get(message.eventType) ?? []) {
      listener(event)
    }
  }

  close(): void {
    if (this.closed) return
    this.closed = true
    subscriptions.delete(this.id)
    shared?.port.postMessage({ kind: 'unsubscribe', id: this.id } satisfies HubRequest)
    if (subscriptions.size === 0 && shared) {
      shared.port.postMessage({ kind: 'disconnect' } satisfies HubRequest)
      shared.port.close()
      shared = undefined
    }
  }
}

export function openBrowserEventSource(url: string): EventSourceLike {
  const worker = getSharedWorker()
  if (!worker) return new EventSource(url)
  const id = ++nextId
  const source = new WorkerEventSource(id)
  subscriptions.set(id, source)
  worker.port.postMessage({
    kind: 'subscribe',
    id,
    url: new URL(url, document.baseURI).toString(),
  } satisfies HubRequest)
  return source
}

// A page can be replaced by a focused navigation while its old runtime SSE
// remains alive. Release its subscriptions before navigation, including BFCache.
if (typeof window !== 'undefined') {
  window.addEventListener('pagehide', () => {
    for (const subscription of [...subscriptions.values()]) subscription.close()
  })
  window.addEventListener('pageshow', (event) => {
    if (event.persisted) window.location.reload()
  })
}
