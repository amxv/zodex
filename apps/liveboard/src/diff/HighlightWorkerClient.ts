import type { DiffSyntaxLanguage } from './language'
import type {
  DiffHighlightInput,
  DiffHighlightResult,
  HighlightRequestMessage,
  HighlightWorkerRequest,
  HighlightWorkerResponse,
} from './protocol'

interface PendingHighlight {
  resolve: (result: DiffHighlightResult) => void
  reject: (error: Error) => void
}

export interface DiffHighlighter {
  isReady: () => boolean
  eagerLanguages: () => readonly DiffSyntaxLanguage[]
  highlight: (input: DiffHighlightInput) => Promise<DiffHighlightResult>
}

export const PLAIN_DIFF_HIGHLIGHTER: DiffHighlighter = {
  isReady: () => true,
  eagerLanguages: () => [],
  highlight: async (input) => ({
    subjectKey: input.subjectKey,
    revision: input.revision,
    language: input.language,
    rows: input.rows.map((row) => ({ index: row.index, html: null })),
  }),
}

interface WorkerLike {
  postMessage: (message: HighlightWorkerRequest) => void
  terminate: () => void
  addEventListener: (
    type: 'message' | 'error',
    listener: EventListenerOrEventListenerObject,
  ) => void
  removeEventListener: (
    type: 'message' | 'error',
    listener: EventListenerOrEventListenerObject,
  ) => void
}

export class HighlightWorkerClient implements DiffHighlighter {
  private readonly pending = new Map<number, PendingHighlight>()
  private readonly onMessageBound = (event: Event) => this.onMessage(event as MessageEvent)
  private readonly onErrorBound = (event: Event) => this.onError(event)
  private nextRequestId = 1
  private ready = false
  private languages: readonly DiffSyntaxLanguage[] = []
  private disposed = false

  constructor(private readonly worker: WorkerLike) {
    worker.addEventListener('message', this.onMessageBound)
    worker.addEventListener('error', this.onErrorBound)
    worker.postMessage({ type: 'ready_probe' })
  }

  isReady = () => this.ready

  eagerLanguages = () => this.languages

  highlight = (input: DiffHighlightInput) => {
    if (this.disposed) return Promise.reject(new Error('diff highlighter is disposed'))
    const requestId = this.nextRequestId++
    return new Promise<DiffHighlightResult>((resolve, reject) => {
      this.pending.set(requestId, { resolve, reject })
      this.worker.postMessage({
        type: 'highlight',
        requestId,
        ...input,
      })
    })
  }

  dispose() {
    if (this.disposed) return
    this.disposed = true
    this.worker.removeEventListener('message', this.onMessageBound)
    this.worker.removeEventListener('error', this.onErrorBound)
    this.worker.terminate()
    this.rejectAll(new Error('diff highlighter was disposed'))
  }

  private onMessage(event: MessageEvent<HighlightWorkerResponse>) {
    const message = event.data
    if (message.type === 'ready') {
      this.ready = true
      this.languages = message.languages
      return
    }
    const pending = this.pending.get(message.requestId)
    if (!pending) return
    this.pending.delete(message.requestId)
    if (message.type === 'highlight_error') {
      pending.reject(new Error(message.message))
    } else {
      pending.resolve(message)
    }
  }

  private onError(event: Event) {
    const message = event instanceof ErrorEvent ? event.message : 'diff highlight worker failed'
    this.rejectAll(new Error(message))
  }

  private rejectAll(error: Error) {
    for (const pending of this.pending.values()) pending.reject(error)
    this.pending.clear()
  }
}

let defaultHighlighter: HighlightWorkerClient | undefined

// Most focused tabs may show only commands and logs. Don't pay for a separate
// syntax-grammar worker in each tab until an actual file diff is expanded.
export const LAZY_DIFF_HIGHLIGHTER: DiffHighlighter = {
  isReady: () => defaultHighlighter?.isReady() ?? false,
  eagerLanguages: () => defaultHighlighter?.eagerLanguages() ?? [],
  highlight: (input) => defaultDiffHighlighter().highlight(input),
}

export function defaultDiffHighlighter(): DiffHighlighter {
  if (!defaultHighlighter) {
    defaultHighlighter = new HighlightWorkerClient(
      new Worker(new URL('./highlight.worker.ts', import.meta.url), {
        type: 'module',
        name: 'zodex-diff-highlight',
      }),
    )
  }
  return defaultHighlighter
}
