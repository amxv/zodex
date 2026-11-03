import { playwright } from '@vitest/browser-playwright'
import { defineConfig } from 'vitest/config'
import solid from 'vite-plugin-solid'

const runtimeProcess = (globalThis as {
  process?: { env?: Record<string, string | undefined> }
}).process
const browser = runtimeProcess?.env?.LIVEBOARD_BROWSER === 'webkit' ? 'webkit' : 'chromium'

export default defineConfig({
  plugins: [solid()],
  optimizeDeps: {
    include: [
      'highlight.js/lib/core',
      'highlight.js/lib/languages/bash',
      'highlight.js/lib/languages/cpp',
      'highlight.js/lib/languages/css',
      'highlight.js/lib/languages/diff',
      'highlight.js/lib/languages/dockerfile',
      'highlight.js/lib/languages/go',
      'highlight.js/lib/languages/ini',
      'highlight.js/lib/languages/java',
      'highlight.js/lib/languages/javascript',
      'highlight.js/lib/languages/json',
      'highlight.js/lib/languages/kotlin',
      'highlight.js/lib/languages/markdown',
      'highlight.js/lib/languages/python',
      'highlight.js/lib/languages/rust',
      'highlight.js/lib/languages/sql',
      'highlight.js/lib/languages/swift',
      'highlight.js/lib/languages/typescript',
      'highlight.js/lib/languages/xml',
      'highlight.js/lib/languages/yaml',
    ],
  },
  test: {
    name: 'browser',
    include: ['src/**/*.browser.test.tsx'],
    browser: {
      enabled: true,
      headless: true,
      provider: playwright(),
      instances: [{ browser }],
    },
  },
})
