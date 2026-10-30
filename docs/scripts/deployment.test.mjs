import test from 'node:test'
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { resolve, dirname } from 'node:path'

const docsRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const repoRoot = resolve(docsRoot, '..')

test('docs and both published installers are the only deployment triggers', () => {
  const workflow = readFileSync(resolve(repoRoot, '.github/workflows/docs.yml'), 'utf8')
  for (const event of ['push', 'pull_request']) {
    const block = workflow.match(new RegExp(`^  ${event}:\\n([\\s\\S]*?)(?=^  (?:push|pull_request|workflow_dispatch):|^permissions:)`, 'm'))?.[1]
    assert.ok(block, `missing ${event} trigger`)
    assert.match(block, /- main/)
    for (const entry of ['docs/src/**', 'docs/public/**', 'docs/package.json', 'docs/bun.lock', 'docs/astro.config.mjs', 'docs/vercel.json', 'scripts/install.sh', 'scripts/install.ps1']) {
      assert.ok(block.includes(`- ${entry}`), `${event} must include ${entry}`)
    }
    assert.doesNotMatch(block, /Cargo\.lock|crates\/|apps\//)
  }
  assert.match(workflow, /needs: validate/)
  assert.match(workflow, /vercel@65\.1\.0 deploy/)
  assert.match(workflow, /github\.event\.pull_request\.head\.repo\.full_name == github\.repository/)
})

test('Vercel Git cannot create deployments independently of the filtered workflow', () => {
  const config = JSON.parse(readFileSync(resolve(docsRoot, 'vercel.json'), 'utf8'))
  assert.equal(config.git.deploymentEnabled, false)
  assert.equal(Object.hasOwn(config, 'ignoreCommand'), false)
})
