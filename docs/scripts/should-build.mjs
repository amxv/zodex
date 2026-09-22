#!/usr/bin/env node

import { spawnSync } from 'node:child_process'

const relevantPaths = ['docs', 'scripts/install.sh', 'scripts/install.ps1']

let gitCwd
const git = (args, cwd = gitCwd) =>
  spawnSync('git', args, {
    cwd,
    encoding: 'utf8',
    stdio: ['ignore', 'pipe', 'pipe'],
  })

const resolveCommit = ref => {
  if (!ref) return null
  const result = git(['rev-parse', '--verify', `${ref}^{commit}`])
  return result.status === 0 ? result.stdout.trim() : null
}

const shouldBuild = () => {
  const root = git(['rev-parse', '--show-toplevel'], process.cwd())
  if (root.status !== 0) return true
  gitCwd = root.stdout.trim()

  const head = resolveCommit(process.env.VERCEL_GIT_COMMIT_SHA ?? 'HEAD')
  if (!head) return true

  const baseRef = [
    process.env.VERCEL_GIT_PREVIOUS_SHA,
    process.env.VERCEL_GIT_PULL_REQUEST_BASE_SHA,
    process.env.SITE_DEPLOY_DIFF_BASE,
  ].find(Boolean)
  const base = resolveCommit(baseRef)
  if (!base) return true

  const mergeBaseResult = git(['merge-base', base, head])
  if (mergeBaseResult.status !== 0) return true
  const comparisonBase = mergeBaseResult.stdout.trim()

  const diff = git([
    'diff',
    '--quiet',
    comparisonBase,
    head,
    '--',
    ...relevantPaths,
  ])
  if (diff.status === 0) return false
  if (diff.status === 1) return true
  return true
}

process.exit(shouldBuild() ? 1 : 0)
