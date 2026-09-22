import { afterEach, describe, expect, test } from 'bun:test'
import { mkdtempSync, mkdirSync, readFileSync, renameSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { spawnSync } from 'node:child_process'

const script = join(dirname(fileURLToPath(import.meta.url)), 'should-build.mjs')
const repos = []

const run = (command, args, cwd, env = process.env) => {
  const result = spawnSync(command, args, {
    cwd,
    env,
    encoding: 'utf8',
    stdio: ['ignore', 'pipe', 'pipe'],
  })
  if (result.status !== 0) {
    throw new Error(
      `${command} ${args.join(' ')} failed (${result.status})\n${result.stdout}\n${result.stderr}`,
    )
  }
  return result.stdout.trim()
}

const git = (cwd, ...args) => run('git', args, cwd)

const write = (root, relative, contents) => {
  const path = join(root, relative)
  mkdirSync(dirname(path), { recursive: true })
  writeFileSync(path, contents)
}

const commit = (root, message) => {
  git(root, 'add', '-A')
  git(root, 'commit', '-m', message)
  return git(root, 'rev-parse', 'HEAD')
}

const fixture = () => {
  const root = mkdtempSync(join(tmpdir(), 'zodex-docs-vercel-'))
  repos.push(root)
  git(root, 'init', '-q')
  git(root, 'config', 'user.name', 'Zodex Test')
  git(root, 'config', 'user.email', 'zodex-test@local.invalid')
  write(root, 'docs/index.md', 'docs v1\n')
  write(root, 'scripts/install.sh', '#!/bin/sh\necho v1\n')
  write(root, 'scripts/install.ps1', 'Write-Host "v1"\n')
  write(root, 'src/lib.rs', 'pub fn v1() {}\n')
  const base = commit(root, 'base')
  return { root, docs: join(root, 'docs'), base }
}

const decision = ({ docs, head, base }) =>
  spawnSync('node', [script], {
    cwd: docs,
    env: {
      ...process.env,
      VERCEL_GIT_COMMIT_SHA: head,
      VERCEL_GIT_PREVIOUS_SHA: base,
      VERCEL_GIT_PULL_REQUEST_BASE_SHA: '',
      SITE_DEPLOY_DIFF_BASE: '',
    },
    stdio: ['ignore', 'pipe', 'pipe'],
  }).status

afterEach(() => {
  for (const repo of repos.splice(0)) rmSync(repo, { recursive: true, force: true })
})

describe('Vercel docs affected-path check', () => {
  test('builds for a docs edit', () => {
    const repo = fixture()
    write(repo.root, 'docs/index.md', 'docs v2\n')
    const head = commit(repo.root, 'docs edit')
    expect(decision({ ...repo, head })).toBe(1)
  })

  test('builds when the canonical installer changes', () => {
    const repo = fixture()
    write(repo.root, 'scripts/install.sh', '#!/bin/sh\necho v2\n')
    const head = commit(repo.root, 'installer edit')
    expect(decision({ ...repo, head })).toBe(1)
  })

  test('builds when the canonical Windows installer changes', () => {
    const repo = fixture()
    write(repo.root, 'scripts/install.ps1', 'Write-Host "v2"\n')
    const head = commit(repo.root, 'windows installer edit')
    expect(decision({ ...repo, head })).toBe(1)
  })

  test('skips unrelated source-only changes', () => {
    const repo = fixture()
    write(repo.root, 'src/lib.rs', 'pub fn v2() {}\n')
    const head = commit(repo.root, 'rust edit')
    expect(decision({ ...repo, head })).toBe(0)
  })

  test('keeps an earlier relevant change in a multi-commit push', () => {
    const repo = fixture()
    write(repo.root, 'docs/index.md', 'docs v2\n')
    commit(repo.root, 'docs edit')
    write(repo.root, 'src/lib.rs', 'pub fn v2() {}\n')
    const head = commit(repo.root, 'later rust edit')
    expect(decision({ ...repo, head })).toBe(1)
  })

  test('builds for docs deletion', () => {
    const repo = fixture()
    rmSync(join(repo.root, 'docs/index.md'))
    const head = commit(repo.root, 'delete docs page')
    expect(decision({ ...repo, head })).toBe(1)
  })

  test('builds for docs rename', () => {
    const repo = fixture()
    renameSync(join(repo.root, 'docs/index.md'), join(repo.root, 'docs/start.md'))
    const head = commit(repo.root, 'rename docs page')
    expect(decision({ ...repo, head })).toBe(1)
  })

  test('fails open when the comparison base cannot be resolved', () => {
    const repo = fixture()
    write(repo.root, 'src/lib.rs', 'pub fn v2() {}\n')
    const head = commit(repo.root, 'rust edit')
    expect(decision({ ...repo, head, base: 'missing-base' })).toBe(1)
  })

  test('fails open when the deployment head cannot be resolved', () => {
    const repo = fixture()
    expect(decision({ ...repo, head: 'missing-head' })).toBe(1)
  })

  test('keeps the committed Vercel routing contract', () => {
    const config = JSON.parse(
      readFileSync(join(dirname(fileURLToPath(import.meta.url)), '..', 'vercel.json'), 'utf8'),
    )
    expect(config.ignoreCommand).toBe('node scripts/should-build.mjs')
    expect(config.git?.deploymentEnabled).toEqual({ main: true, '*': false })
  })
})
