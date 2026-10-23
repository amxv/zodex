import { createSharedEventHub } from './sharedEventHub'

// The same named worker and script URL are shared by every Liveboard tab on
// this localhost origin. The only network SSE connection lives here.
const hub = createSharedEventHub(self.location.origin, (url) => new EventSource(url))

type SharedWorkerScope = typeof self & {
  onconnect: ((event: MessageEvent) => void) | null
}

;(self as SharedWorkerScope).onconnect = (event) => {
  for (const port of event.ports) hub.attach(port)
}
