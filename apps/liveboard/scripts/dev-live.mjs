import { spawn } from 'node:child_process'
import { existsSync } from 'node:fs'
import { dirname, resolve } from 'node:path'
import { createInterface } from 'node:readline'
import { fileURLToPath } from 'node:url'

const scriptDir = dirname(fileURLToPath(import.meta.url))
const liveboardDir = resolve(scriptDir, '..')
const repoRoot = resolve(liveboardDir, '../..')
const zodex = resolve(repoRoot, 'target/debug/zodex')

let viewer
let vite
let shuttingDown = false

function run(command, args, options = {}) {
  return new Promise((resolvePromise, reject) => {
    const child = spawn(command, args, {
      cwd: options.cwd ?? repoRoot,
      env: options.env ?? process.env,
      stdio: options.stdio ?? 'inherit',
    })
    child.on('error', reject)
    child.on('exit', (code, signal) => {
      if (code === 0) {
        resolvePromise()
      } else {
        reject(
          new Error(
            `${command} ${args.join(' ')} exited ${signal ? `from ${signal}` : `with ${code}`}`,
          ),
        )
      }
    })
  })
}

async function ensureViewerBinary() {
  if (!existsSync(resolve(liveboardDir, 'dist/index.html'))) {
    console.log('[liveboard] building one embedded asset snapshot for the dev capability host…')
    await run(process.execPath, ['run', 'build'], { cwd: liveboardDir })
  }

  console.log('[liveboard] building the repo Zodex viewer…')
  await run('cargo', ['build', '--bin', 'zodex'], {
    cwd: repoRoot,
    env: {
      ...process.env,
      ZODEX_LIVEBOARD_EMBED_REQUIRED: '1',
    },
  })
}

function startViewer() {
  return new Promise((resolvePromise, reject) => {
    viewer = spawn(zodex, ['local', 'watch', '--no-open'], {
      cwd: repoRoot,
      env: process.env,
      stdio: ['ignore', 'pipe', 'inherit'],
    })

    const lines = createInterface({ input: viewer.stdout })
    let settled = false
    lines.on('line', (line) => {
      console.log(`[zodex] ${line}`)
      const match = /^Liveboard:\s+(http:\/\/\S+)$/.exec(line.trim())
      if (match && !settled) {
        settled = true
        resolvePromise(match[1])
      }
    })
    viewer.on('error', (error) => {
      if (!settled) {
        settled = true
        reject(error)
      }
    })
    viewer.on('exit', (code, signal) => {
      if (!settled) {
        settled = true
        reject(
          new Error(
            `repo Liveboard viewer exited before startup ${signal ? `from ${signal}` : `with ${code}`}`,
          ),
        )
      } else if (!shuttingDown) {
        console.error('[liveboard] capability host exited; stopping Vite')
        shutdown(code ?? 1)
      }
    })
  })
}

function startVite(upstream) {
  console.log('[liveboard] attaching Vite to the currently running Local observer')
  vite = spawn(process.execPath, ['x', 'vite'], {
    cwd: liveboardDir,
    env: {
      ...process.env,
      LIVEBOARD_DEV_UPSTREAM: upstream,
    },
    stdio: 'inherit',
  })
  vite.on('error', (error) => {
    console.error(error)
    shutdown(1)
  })
  vite.on('exit', (code) => {
    if (!shuttingDown) shutdown(code ?? 0)
  })
}

async function resolvePrivateUpstream(publicUrl) {
  const response = await fetch(publicUrl)
  if (!response.ok) {
    throw new Error(`stable Liveboard root returned HTTP ${response.status}`)
  }
  const html = await response.text()
  const match = /<base href="([^"]+)" \/>/.exec(html)
  if (!match) {
    throw new Error('stable Liveboard root did not expose its private asset base')
  }
  return new URL(match[1], publicUrl).toString()
}

function shutdown(exitCode = 0) {
  if (shuttingDown) return
  shuttingDown = true
  vite?.kill('SIGINT')
  viewer?.kill('SIGINT')
  setTimeout(() => process.exit(exitCode), 250).unref()
}

process.on('SIGINT', () => shutdown(0))
process.on('SIGTERM', () => shutdown(0))

try {
  await ensureViewerBinary()
  const publicUrl = await startViewer()
  const upstream = await resolvePrivateUpstream(publicUrl)
  startVite(upstream)
} catch (error) {
  console.error(error instanceof Error ? error.message : error)
  shutdown(1)
}
