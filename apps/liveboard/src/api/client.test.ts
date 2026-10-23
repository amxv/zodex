import { afterEach, describe, expect, it, vi } from 'vitest'

import { fetchJson } from './client'

afterEach(() => {
  vi.useRealTimers()
  vi.unstubAllGlobals()
})

describe('bounded local requests', () => {
  it('aborts a queued fetch after 15s instead of hanging forever', async () => {
    vi.useFakeTimers()
    const fetch = vi.fn((_path: string, options: RequestInit) =>
      new Promise<Response>((_resolve, reject) => {
        options.signal?.addEventListener('abort', () => reject(new Error('AbortError')))
      }),
    )
    vi.stubGlobal('fetch', fetch)
    const request = fetchJson('api/invocations/10/output-metadata')
    const result = expect(request).rejects.toThrow('Liveboard request timed out after 15s')
    await vi.advanceTimersByTimeAsync(15_001)
    await result
    expect(fetch).toHaveBeenCalledTimes(1)
    expect((fetch.mock.calls[0]?.[1]?.signal as AbortSignal).aborted).toBe(true)
  })
})
