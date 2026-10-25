import { describe, expect, it } from 'vitest'

import type { ApiAgent, LiveboardPreferences } from '../api/client'
import {
  addAgentToBoard,
  admitAgent,
  columnWeights,
  initialVisibleAgentIds,
  moveAgent,
  orderPatch,
  resizeAdjacentWeights,
  shrinkBoardToMaximum,
} from './model'

function agent(id: string, firstSeen: number, current = true): ApiAgent {
  return {
    id,
    first_seen_at_ms: firstSeen,
    last_seen_at_ms: firstSeen + 10,
    seen_in_current_runtime: current,
    active_process_count: 0,
    workdirs: [],
  }
}

function preferences(
  overrides: Partial<LiveboardPreferences> = {},
): LiveboardPreferences {
  return {
    schema_version: 1,
    theme: 'system',
    max_visible_agents: 4,
    hide_inactive_agents_after_hours: 12,
    command_outputs_expanded: false,
    diffs_expanded: true,
    show_raw_button: false,
    editor_command: 'zed',
    agents: {},
    ...overrides,
  }
}

describe('board admission and layout', () => {
  it('restores explicit visible order, ignores stale Agents, then fills free slots', () => {
    const prefs = preferences({
      max_visible_agents: 3,
      agents: {
        a111: { visible: true, order: 2 },
        b222: { visible: false, order: 0 },
        c333: { visible: true, order: 1 },
        z999: { visible: true, order: 0 },
      },
    })
    expect(
      initialVisibleAgentIds(
        [
          agent('a111', 10),
          agent('b222', 20),
          agent('c333', 30),
          agent('d444', 40),
          agent('z999', 1, false),
        ],
        prefs,
      ),
    ).toEqual(['c333', 'a111', 'd444'])
  })

  it('never evicts a visible Agent when a new Agent appears on a full board', () => {
    const prefs = preferences({ max_visible_agents: 2 })
    const existing = ['a111', 'b222']
    expect(admitAgent(existing, agent('c333', 3), prefs)).toEqual(existing)
    expect(
      admitAgent(['a111'], agent('c333', 3), {
        ...prefs,
        agents: { c333: { visible: false } },
      }),
    ).toEqual(['a111'])
  })

  it('adds from the drawer only when capacity exists and hides rightmost on max reduction', () => {
    expect(addAgentToBoard(['a111'], 'b222', 2)).toEqual(['a111', 'b222'])
    expect(addAgentToBoard(['a111', 'b222'], 'c333', 2)).toEqual([
      'a111',
      'b222',
    ])
    expect(shrinkBoardToMaximum(['a111', 'b222', 'c333'], 2)).toEqual({
      visible: ['a111', 'b222'],
      hidden: ['c333'],
    })
  })

  it('reorders arbitrarily and produces one compact order patch', () => {
    const reordered = moveAgent(['a111', 'b222', 'c333', 'd444'], 'a111', 3)
    expect(reordered).toEqual(['b222', 'c333', 'd444', 'a111'])
    expect(orderPatch(reordered)).toEqual({
      b222: { order: 0 },
      c333: { order: 1 },
      d444: { order: 2 },
      a111: { order: 3 },
    })
  })

  it('restores weights and clamps adjacent resize without changing the pair total', () => {
    const prefs = preferences({ agents: { a111: { width_weight: 1.5 } } })
    expect(columnWeights(['a111', 'b222'], prefs)).toEqual({ a111: 1.5, b222: 1 })
    const [left, right] = resizeAdjacentWeights(1, 1, 100, 400)
    expect(left).toBeCloseTo(1.5)
    expect(right).toBeCloseTo(0.5)
    const [clampedLeft, clampedRight] = resizeAdjacentWeights(1, 1, -1000, 400)
    expect(clampedLeft).toBeCloseTo(0.1)
    expect(clampedLeft + clampedRight).toBeCloseTo(2)
  })
})
