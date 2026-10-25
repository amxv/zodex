import { describe, expect, it } from 'vitest'

import type { ApiAgent } from '../api/client'
import {
  agentActivity,
  compactWorkdir,
  mostRecentWorkdir,
  relativeActivityLabel,
  visibleDrawerAgents,
} from './model'

const fixture: ApiAgent = {
  id: 'k7m2',
  first_seen_at_ms: 1,
  last_seen_at_ms: 90_000,
  seen_in_current_runtime: true,
  active_process_count: 0,
  workdirs: [
    {
      normalized_workdir: '/Users/example/older',
      ordinal: 0,
      first_seen_at_ms: 1,
      last_seen_at_ms: 10,
      first_invocation_id: 1,
      last_invocation_id: 2,
      retained_invocation_count: 2,
    },
    {
      normalized_workdir: '/Users/example/newer',
      ordinal: 1,
      first_seen_at_ms: 20,
      last_seen_at_ms: 100,
      first_invocation_id: 3,
      last_invocation_id: 4,
      retained_invocation_count: 2,
    },
  ],
}

describe('Agent display model', () => {
  it('uses the newest observed workdir and keeps compact paths useful', () => {
    expect(mostRecentWorkdir(fixture)?.normalized_workdir).toBe('/Users/example/newer')
    expect(compactWorkdir('/Users/example/newer')).toBe('…/example/newer')
    expect(compactWorkdir('/repo')).toBe('/repo')
  })

  it('separates running, recent, and idle without inventing disconnects', () => {
    expect(agentActivity({ ...fixture, active_process_count: 1 }, 100_000)).toBe('running')
    expect(agentActivity(fixture, 100_000)).toBe('recent')
    expect(agentActivity(fixture, 200_000)).toBe('idle')
  })

  it('renders stable coarse recency labels', () => {
    expect(relativeActivityLabel(99_000, 100_000)).toBe('now')
    expect(relativeActivityLabel(80_000, 100_000)).toBe('20s ago')
    expect(relativeActivityLabel(0, 120_000)).toBe('2m ago')
    expect(relativeActivityLabel(0, 7_200_000)).toBe('2h ago')
  })

  it('hides agents at the 12-hour inactivity boundary without hiding active or selected agents', () => {
    const now = 50 * 60 * 60 * 1_000
    const hoursAgo = (hours: number, id: string, active_process_count = 0): ApiAgent => ({
      ...fixture,
      id,
      last_seen_at_ms: now - hours * 60 * 60 * 1_000,
      active_process_count,
    })
    const agents = [
      hoursAgo(11, 'a111'),
      hoursAgo(12, 'b222'),
      hoursAgo(13, 'c333', 1),
      hoursAgo(72, 'd444'),
      hoursAgo(100, 'e555'),
    ]
    const visible = (hours: number, selected: string[] = [], focusedId?: string) =>
      visibleDrawerAgents(agents, selected, focusedId, now, hours).map((agent) => agent.id)

    expect(visible(12)).toEqual(['a111', 'c333'])
    expect(visible(12, ['b222'])).toEqual(['a111', 'b222', 'c333'])
    expect(visible(12, [], 'e555')).toEqual(['a111', 'c333', 'e555'])
    expect(visible(24)).toEqual(['a111', 'b222', 'c333'])
    expect(visible(0)).toEqual(['a111', 'b222', 'c333', 'd444', 'e555'])
    // A future clock skew must not accidentally hide an agent.
    expect(visibleDrawerAgents([hoursAgo(-1, 'f666')], [], undefined, now, 12)).toHaveLength(1)
  })
})
