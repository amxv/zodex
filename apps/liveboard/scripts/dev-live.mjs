import { spawn } from 'node:child_process'
import { existsSync } from 'node:fs'
import { dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const scriptDir = dirname(fileURLToPath(import.meta.url))
const liveboardDir = resolve(scriptDir, '..')
const repoRoot = resolve(liveboardDir, '../..')
const zodex = resolve(repoRoot, 'target/debug/zodex')

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

function resolveViewerUrl() {
  return new Promise((resolvePromise, reject) => {
    const child = spawn(zodex, ['local', 'watch', 'url'], {
      cwd: repoRoot,
      env: process.env,
      stdio: ['ignore', 'pipe', 'inherit'],
    })
    let stdout = ''
    child.stdout.on('data', (chunk) => {
      stdout += chunk.toString()
    })
    child.on('error', reject)
    child.on('exit', (code, signal) => {
      if (code !== 0) {
        reject(
          new Error(
            `repo Liveboard URL lookup exited ${signal ? `from ${signal}` : `with ${code}`}`,
          ),
        )
        return
      }
      const url = stdout.trim()
      if (!/^http:\/\/127\.0\.0\.1:\d+\/$/.test(url)) {
        reject(new Error(`repo Liveboard URL lookup returned an invalid URL: ${url}`))
        return
      }
      console.log(`[zodex] Liveboard: ${url}`)
      resolvePromise(url)
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
  setTimeout(() => process.exit(exitCode), 250).unref()
}

process.on('SIGINT', () => shutdown(0))
process.on('SIGTERM', () => shutdown(0))

try {
  await ensureViewerBinary()
  const publicUrl = await resolveViewerUrl()
  const upstream = await resolvePrivateUpstream(publicUrl)
  startVite(upstream)
} catch (error) {
  console.error(error instanceof Error ? error.message : error)
  shutdown(1)
}
