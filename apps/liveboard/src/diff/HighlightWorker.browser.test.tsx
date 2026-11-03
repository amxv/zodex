import { describe, expect, it, vi } from 'vitest'

import { defaultDiffHighlighter } from './HighlightWorkerClient'
import { EAGER_DIFF_LANGUAGES, type DiffSyntaxLanguage } from './language'

describe('eager diff highlight worker', () => {
  it('boots locally with every common grammar and highlights without external resources', async () => {
    const highlighter = defaultDiffHighlighter()
    await vi.waitFor(() => expect(highlighter.isReady()).toBe(true), { timeout: 5_000 })
    expect([...highlighter.eagerLanguages()].sort()).toEqual([...EAGER_DIFF_LANGUAGES].sort())

    const fixtures: Array<[DiffSyntaxLanguage, string]> = [
      ['rust', 'fn main() {}'],
      ['typescript', 'const value: string = "x"'],
      ['javascript', 'const value = "x"'],
      ['go', 'package main'],
      ['bash', 'echo "$HOME"'],
      ['python', 'def value():'],
      ['css', '.button { display: grid; }'],
      ['json', '{"value": 1}'],
      ['ini', 'edition = "2024"'],
      ['markdown', '## Heading'],
      ['yaml', 'name: zodex'],
      ['xml', '<main class="app">'],
      ['sql', 'SELECT * FROM users;'],
      ['java', 'public class Main {}'],
      ['cpp', '#include <iostream>'],
      ['kotlin', 'fun main() {}'],
      ['swift', 'let value: String = "hello"'],
      ['dockerfile', 'FROM alpine:3.20'],
      ['diff', '@@ -1,2 +1,3 @@'],
    ]
    for (const [language, text] of fixtures) {
      const result = await highlighter.highlight({
        subjectKey: `worker-smoke:${language}`,
        revision: '1',
        language,
        rows: [{ index: 0, text }],
      })
      expect(result.rows[0]?.html, language).toContain('hljs-')
    }
  })

  it('keeps four 500-row highlight batches off the animation frame path', async () => {
    const highlighter = defaultDiffHighlighter()
    const rows = Array.from({ length: 500 }, (_, index) => ({
      index,
      text: `let value_${index}: usize = ${index};`,
    }))
    let frameRan = false
    const frame = new Promise<void>((resolve) =>
      requestAnimationFrame(() => {
        frameRan = true
        resolve()
      }),
    )
    const work = Promise.all(
      Array.from({ length: 4 }, (_, index) =>
        highlighter.highlight({
          subjectKey: `stress:${index}`,
          revision: '500',
          language: 'rust',
          rows,
        }),
      ),
    )
    await frame
    expect(frameRan).toBe(true)
    const results = await work
    expect(results).toHaveLength(4)
    expect(results.every((result) => result.rows.length === 500)).toBe(true)
  })
})
