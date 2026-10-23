#!/usr/bin/env node
// Self-contained headless acceptance test: 18 focused Liveboards share one
// physical SSE, receive alias notifications, and navigate without deadlocks.
import assert from 'node:assert/strict'
import { createServer } from 'node:http'
import { readFile } from 'node:fs/promises'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { chromium } from 'playwright'

const root = join(dirname(fileURLToPath(import.meta.url)), '..')
const indexHtml = (await readFile(join(root, 'dist/index.html'), 'utf8'))
  .replace('<head>', '<head>\n<base href="/capability/">')
const agentIds = Array.from({ length: 18 }, (_, index) => index.toString(36).padStart(4, '0'))
const aliases = Object.fromEntries(agentIds.map((agent) => [agent, {alias: 'Before rename'}]))
const peers = new Set()
let maxStreams = 0
let totalStreams = 0
let outputPagesLoaded = 0
const invocationIds = new Map(agentIds.map((id, index) => [id, 1000 + index]))
const outputByInvocation = new Map([...invocationIds].map(([id, invocation]) => [
  invocation, `Recovered output for ${id}\n`,
]))

const fixture = createServer(async (request, response) => {
  const url = new URL(request.url ?? '/', 'http://localhost')
  const path = url.pathname
  response.setHeader('Cache-Control', 'no-store')
  if (path === '/' || path === '/capability/') {
    response.setHeader('Content-Type', 'text/html; charset=utf-8')
    response.end(indexHtml)
    return
  }
  if (path.startsWith('/capability/assets/')) {
    const filename = path.slice('/capability/assets/'.length)
    if (!/^[a-zA-Z0-9_.-]+$/.test(filename)) {
      response.writeHead(404).end()
      return
    }
    try {
      const contents = await readFile(join(root, 'dist/assets', filename))
      response.setHeader('Content-Type', filename.endsWith('.js') ? 'text/javascript' : 'text/css')
      response.end(contents)
    } catch {
      response.writeHead(404).end()
    }
    return
  }
  if (path === '/capability/preferences') {
    response.setHeader('Content-Type', 'application/json')
    response.end(JSON.stringify({
      schema_version: 1, theme: 'system', max_visible_agents: 4,
      command_outputs_expanded: false, diffs_expanded: true,
      show_raw_button: false, editor_command: 'zed', agents: aliases,
    }))
    return
  }
  const status = {
    schema_version: 1, api_version: 1, presentation_version: 3,
    runtime_id: 'multitab-runtime', current_runtime_agent_count: agentIds.length,
    active_process_count: 0,
  }
  const agent = (id) => ({
    id, seen_in_current_runtime: true, active_process_count: 0,
    first_seen_at_ms: 1, last_seen_at_ms: 2, workdirs: [],
  })
  response.setHeader('Content-Type', 'application/json')
  if (path === '/capability/api/status') {
    response.end(JSON.stringify(status))
  } else if (path.startsWith('/capability/api/agents/')) {
    const id = path.split('/').pop()
    if (!agentIds.includes(id)) response.writeHead(404).end('{}')
    else response.end(JSON.stringify({schema_version: 1, runtime_id: status.runtime_id, agent: agent(id)}))
  } else if (path === '/capability/api/agents') {
    response.end(JSON.stringify({schema_version: 1, runtime_id: status.runtime_id, agents: agentIds.map(agent)}))
  } else if (path === '/capability/api/timeline') {
    const agentId = url.searchParams.get('agent_id')
    const invocation = invocationIds.get(agentId)
    response.end(JSON.stringify({
      schema_version: 1, presentation_version: 3, runtime_id: status.runtime_id,
      records: invocation === undefined ? [] : [{
        presentation_id: `inv-${invocation}`, primary_invocation_id: invocation,
        raw_evidence_count: 1, raw_invocation_ids: [invocation], raw_invocation_ids_truncated: false,
        kind: 'command', agent_id: agentId, command: 'bun run typecheck && bun run build && bun run test:multitab',
        declared_workdir: '/workspace', normalized_workdir: '/workspace', new_workdir: null,
        started_at_ms: 1, duration_ms: 2500, status: 'exited', effective_cwd: '/workspace',
        exit_code: 0, termination_reason: 'exit', output: null,
        output_truncated: false, polls: null,
        evidence: { evidence_state: 'complete', capture_state: 'complete', degraded: false, reason: null },
      }], has_more: false, next_cursor: null,
    }))
  } else if (path.match(/^\/capability\/api\/invocations\/\d+\/output-metadata$/)) {
    const invocation = Number(path.split('/')[4])
    const text = outputByInvocation.get(invocation)
    if (text === undefined) response.writeHead(404).end('{}')
    else response.end(JSON.stringify({
      schema_version: 1, runtime_id: status.runtime_id, invocation_id: invocation,
      output: { available: true, chunk_count: 1, size_bytes: text.length,
        capture_state: 'complete', capture_reason: null, first_cursor: 0, last_cursor: 0 },
    }))
  } else if (path.match(/^\/capability\/api\/invocations\/\d+\/output$/)) {
    const invocation = Number(path.split('/')[4])
    const text = outputByInvocation.get(invocation)
    if (text === undefined) response.writeHead(404).end('{}')
    else {
      outputPagesLoaded++
      response.end(JSON.stringify({
        schema_version: 1, runtime_id: status.runtime_id,
        invocation_id: invocation, view: 'display',
        chunks: [{ sequence: 0, observed_at_ms: 1, text }], next_cursor: null,
        display_state: 'available', display_reason: null,
      }))
    }
  } else if (path === '/capability/api/events') {
    response.setHeader('Content-Type', 'text/event-stream')
    response.setHeader('Connection', 'keep-alive')
    response.setHeader('Cache-Control', 'no-cache')
    response.flushHeaders()
    totalStreams++
    peers.add(response)
    maxStreams = Math.max(maxStreams, peers.size)
    response.on('close', () => peers.delete(response))
  } else {
    response.writeHead(404).end('{}')
  }
})
await new Promise((resolve) => fixture.listen(0, '127.0.0.1', resolve))
const base = `http://127.0.0.1:${fixture.address().port}`

let browser
async function until(predicate, label, ms = 30_000) {
  const deadline = Date.now() + ms
  while (Date.now() < deadline) {
    if (await predicate()) return
    await new Promise((resolve) => setTimeout(resolve, 40))
  }
  throw new Error(`Timed out waiting for ${label}`)
}

try {
  browser = await chromium.launch({headless: true})
  const context = await browser.newContext()
  const errors = []
  const pages = await Promise.all(agentIds.map(async (id) => {
    const page = await context.newPage()
    page.on('pageerror', (error) => errors.push(error.message))
    await page.goto(`${base}/?agent=${id}`, {waitUntil: 'domcontentloaded'})
    await page.locator('.focused-agent-badge').getByText(`Focused · ${id}`).waitFor()
    return page
  }))
  await until(() => peers.size === 1, 'one multiplexed SSE after 18 tabs')
  assert.ok(maxStreams <= 1, `Unexpected second SSE connection: ${maxStreams}`)

  // One global observer event updates the alias in every distinct focus view.
  for (const id of agentIds) aliases[id] = {alias: `Focused worker ${id}`}
  const data = JSON.stringify({
    schema_version: 4, runtime_id: 'multitab-runtime', sequence: 1,
    emitted_at_ms: Date.now(), event_type: 'preferences_changed',
    agent_id: null, invocation_id: null, presentation_id: null,
    presentation_revision: null, payload: {},
  })
  for (const peer of peers) peer.write(`event: preferences_changed\ndata: ${data}\n\n`)
  await Promise.all(pages.map((page, index) =>
    page.locator('.agent-alias').getByText(`Focused worker ${agentIds[index]}`).waitFor(),
  ))

  // The exact previously hanging path: expand 18 different command cards at
  // once while all 18 focused SSE viewers remain connected. Durable output
  // must load with no never-ending "Loading output…" placeholder.
  await Promise.all(pages.map(async (page, index) => {
    await page.locator('.command-card-header-expandable').click()
    await page.locator('.command-output-text')
      .getByText(`Recovered output for ${agentIds[index]}`).waitFor()
    assert.equal(await page.getByText('Loading output…').count(), 0)
  }))
  assert.equal(outputPagesLoaded, pages.length)

  // Reproduce the precise user action: while 18 focused tabs have open
  // Liveboards, click Focus in a newly opened unified Liveboard. Navigation
  // must not queue behind an exhausted per-origin HTTP/1.1 connection pool.
  const unified = await context.newPage()
  unified.on('pageerror', (error) => errors.push(error.message))
  await unified.goto(base, {waitUntil: 'domcontentloaded'})
  await unified.getByRole('button', {name: 'Focus Agent 0000'}).click()
  await unified.locator('.focused-agent-badge').getByText('Focused · 0000').waitFor()
  assert.equal(new URL(unified.url()).searchParams.get('agent'), '0000')
  await unified.close()
  await until(() => peers.size === 1, 'one SSE after clicking Focus')

  // Navigate an already-open focused tab with 17 others still listening.
  await pages[0].goto(`${base}/?agent=${agentIds[5]}`, {waitUntil: 'domcontentloaded'})
  await pages[0].locator('.focused-agent-badge').getByText(`Focused · ${agentIds[5]}`).waitFor()
  await until(() => peers.size === 1, 'one SSE after focused navigation')
  assert.deepEqual(errors, [], `Unexpected frontend errors: ${errors.join('; ')}`)

  await Promise.all(pages.map((page) => page.close()))
  await until(() => peers.size === 0, 'release shared stream when tabs close')
  console.log(JSON.stringify({
    tabs: agentIds.length, steadyStateSseConnections: 1,
    peakSseConnections: maxStreams, totalSseOpens: totalStreams,
    aliasFanout: 'pass', concurrentOutputHydration: outputPagesLoaded,
    focusedNavigation: 'pass', cleanup: 'pass',
  }, null, 2))
} finally {
  await browser?.close()
  for (const peer of peers) peer.end()
  await new Promise((resolve) => fixture.close(resolve))
}
