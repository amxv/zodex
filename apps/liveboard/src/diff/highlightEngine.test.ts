import { describe, expect, it } from 'vitest'

import { DiffHighlightEngine } from './highlightEngine'
import type { DiffSyntaxLanguage } from './language'

describe('diff highlight engine', () => {
  it('has every hot-path grammar registered eagerly and produces class markup', () => {
    const engine = new DiffHighlightEngine()
    const fixtures: Array<[DiffSyntaxLanguage, string]> = [
      ['rust', 'fn main() { let value: usize = 7; }'],
      ['typescript', 'const value: string = "hello"'],
      ['javascript', 'const value = "hello"'],
      ['go', 'package main'],
      ['bash', 'echo "$HOME"'],
      ['python', 'def value() -> str:'],
      ['css', '.button { display: grid; }'],
      ['json', '{"enabled": true}'],
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
    expect(engine.languages()).toHaveLength(fixtures.length)
    for (const [language, text] of fixtures) {
      const result = engine.highlightBatch(language, [{ index: 0, text }])[0]
      expect(result?.html, `${language} should highlight locally`).toContain('hljs-')
    }
  })

  it('escapes hostile source before returning controlled Highlight.js markup', () => {
    const engine = new DiffHighlightEngine()
    const html = engine.highlightBatch('javascript', [
      { index: 0, text: 'const x = "<script>alert(1)</script>&\u202etest"' },
    ])[0]?.html
    expect(html).toContain('&lt;script&gt;')
    expect(html).not.toContain('<script>')
    expect(html).toContain('&amp;')
  })

  it('bounds its line cache with simple LRU eviction', () => {
    const engine = new DiffHighlightEngine(2)
    engine.highlightBatch('rust', [
      { index: 0, text: 'let one = 1;' },
      { index: 1, text: 'let two = 2;' },
      { index: 2, text: 'let three = 3;' },
    ])
    expect(engine.cacheSize()).toBe(2)
  })
})
