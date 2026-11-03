import hljs from 'highlight.js/lib/core'
import type { LanguageFn } from 'highlight.js'
import bash from 'highlight.js/lib/languages/bash'
import cpp from 'highlight.js/lib/languages/cpp'
import css from 'highlight.js/lib/languages/css'
import diff from 'highlight.js/lib/languages/diff'
import dockerfile from 'highlight.js/lib/languages/dockerfile'
import go from 'highlight.js/lib/languages/go'
import ini from 'highlight.js/lib/languages/ini'
import java from 'highlight.js/lib/languages/java'
import javascript from 'highlight.js/lib/languages/javascript'
import json from 'highlight.js/lib/languages/json'
import kotlin from 'highlight.js/lib/languages/kotlin'
import markdown from 'highlight.js/lib/languages/markdown'
import python from 'highlight.js/lib/languages/python'
import rust from 'highlight.js/lib/languages/rust'
import sql from 'highlight.js/lib/languages/sql'
import swift from 'highlight.js/lib/languages/swift'
import typescript from 'highlight.js/lib/languages/typescript'
import xml from 'highlight.js/lib/languages/xml'
import yaml from 'highlight.js/lib/languages/yaml'

import { EAGER_DIFF_LANGUAGES, type DiffSyntaxLanguage } from './language'
import type { DiffHighlightRowInput, DiffHighlightRowResult } from './protocol'

const EAGER_LANGUAGE_REGISTRY = [
  ['rust', rust],
  ['typescript', typescript],
  ['javascript', javascript],
  ['go', go],
  ['bash', bash],
  ['python', python],
  ['css', css],
  ['json', json],
  ['ini', ini],
  ['markdown', markdown],
  ['yaml', yaml],
  ['xml', xml],
  ['sql', sql],
  ['java', java],
  ['cpp', cpp],
  ['kotlin', kotlin],
  ['swift', swift],
  ['dockerfile', dockerfile],
  ['diff', diff],
] as const satisfies readonly [DiffSyntaxLanguage, LanguageFn][]

for (const [language, grammar] of EAGER_LANGUAGE_REGISTRY) {
  if (!hljs.getLanguage(language)) hljs.registerLanguage(language, grammar)
}

const DEFAULT_CACHE_ENTRIES = 4_096
const MAX_CACHE_KEY_CHARS = 4_096

export class DiffHighlightEngine {
  private readonly cache = new Map<string, string | null>()

  constructor(private readonly maximumCacheEntries = DEFAULT_CACHE_ENTRIES) {}

  languages() {
    return EAGER_DIFF_LANGUAGES
  }

  cacheSize() {
    return this.cache.size
  }

  highlightBatch(
    language: DiffSyntaxLanguage,
    rows: readonly DiffHighlightRowInput[],
  ): DiffHighlightRowResult[] {
    // The canonical diff already owns row semantics. Highlighting each source
    // row independently is an intentional fast-path tradeoff: it keeps one
    // bounded worker batch per visible file, but does not carry multiline
    // grammar continuation state across rows. Do not use Highlight.js private
    // continuation APIs to hide that tradeoff.
    return rows.map((row) => ({
      index: row.index,
      html: this.highlightLine(language, row.text),
    }))
  }

  private highlightLine(language: DiffSyntaxLanguage, text: string): string | null {
    if (text.trim().length === 0 || !hljs.getLanguage(language)) return null
    const key = `${language}\0${text}`
    if (key.length <= MAX_CACHE_KEY_CHARS && this.cache.has(key)) {
      const cached = this.cache.get(key) ?? null
      this.cache.delete(key)
      this.cache.set(key, cached)
      return cached
    }

    let highlighted: string | null = null
    try {
      highlighted = hljs.highlight(text, {
        language,
        ignoreIllegals: true,
      }).value
    } catch {
      highlighted = null
    }
    if (key.length <= MAX_CACHE_KEY_CHARS) this.remember(key, highlighted)
    return highlighted
  }

  private remember(key: string, value: string | null) {
    if (this.maximumCacheEntries <= 0) return
    if (this.cache.has(key)) this.cache.delete(key)
    this.cache.set(key, value)
    while (this.cache.size > this.maximumCacheEntries) {
      const oldest = this.cache.keys().next().value
      if (oldest === undefined) break
      this.cache.delete(oldest)
    }
  }
}
