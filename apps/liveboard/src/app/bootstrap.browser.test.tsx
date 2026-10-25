import { afterEach, describe, expect, it, vi } from 'vitest'

import { loadBootstrap } from '../api/client'
import '../styles.css'
import { applyTheme } from './bootstrap'

afterEach(() => {
  vi.unstubAllGlobals()
  applyTheme('system')
})

describe('Liveboard browser bootstrap', () => {
  it('loads same-origin bootstrap resources and applies the persisted theme', async () => {
    const requests: string[] = []
    const payloads: Record<string, unknown> = {
      'api/status': {
        schema_version: 1,
        api_version: 1,
        presentation_version: 3,
        runtime_id: 'runtime-browser-smoke',
        current_runtime_agent_count: 1,
        active_process_count: 2,
      },
      'api/agents?runtime=current': {
        schema_version: 1,
        runtime_id: 'runtime-browser-smoke',
        agents: [
          {
            id: 'k7m2',
            seen_in_current_runtime: true,
            active_process_count: 2,
          },
        ],
      },
      preferences: {
        schema_version: 1,
        theme: 'dark',
        max_visible_agents: 4,
        hide_inactive_agents_after_hours: 12,
        command_outputs_expanded: false,
        diffs_expanded: true,
        show_raw_button: false,
        editor_command: 'zed',
      },
    }

    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL) => {
        const path =
          typeof input === 'string'
            ? input
            : input instanceof URL
              ? input.toString()
              : input.url
        requests.push(path)
        const payload = payloads[path]
        if (!payload) {
          return new Response('{}', { status: 404 })
        }
        return new Response(JSON.stringify(payload), {
          status: 200,
          headers: { 'content-type': 'application/json' },
        })
      }),
    )

    const bootstrap = await loadBootstrap()
    expect(requests.sort()).toEqual(
      ['api/agents?runtime=current', 'api/status', 'preferences'].sort(),
    )
    expect(bootstrap.status.runtime_id).toBe('runtime-browser-smoke')
    expect(bootstrap.agents.agents[0]?.id).toBe('k7m2')

    applyTheme(bootstrap.preferences.theme)
    expect(document.documentElement.dataset.theme).toBe('dark')
    expect(getComputedStyle(document.documentElement).colorScheme).toBe('dark')
  })

  it('bootstraps a focused view without fetching the global Agent list', async () => {
    const requests: string[] = []
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL) => {
        const path =
          typeof input === 'string'
            ? input
            : input instanceof URL
              ? input.toString()
              : input.url
        requests.push(path)
        const payloads: Record<string, unknown> = {
          'api/status': {
            schema_version: 1,
            api_version: 1,
            presentation_version: 3,
            runtime_id: 'runtime-focused',
            current_runtime_agent_count: 3,
            active_process_count: 1,
          },
          preferences: {
            schema_version: 1,
            theme: 'system',
            max_visible_agents: 4,
            hide_inactive_agents_after_hours: 12,
            command_outputs_expanded: false,
            diffs_expanded: true,
            show_raw_button: false,
            editor_command: 'zed',
            agents: {},
          },
          'api/agents/k7m2': {
            schema_version: 1,
            runtime_id: 'runtime-focused',
            agent: {
              id: 'k7m2',
              first_seen_at_ms: 1,
              last_seen_at_ms: 2,
              seen_in_current_runtime: true,
              active_process_count: 1,
              workdirs: [],
            },
          },
        }
        const payload = payloads[path]
        return payload
          ? new Response(JSON.stringify(payload), { status: 200 })
          : new Response('{}', { status: 404 })
      }),
    )

    const bootstrap = await loadBootstrap({ kind: 'focused', agentId: 'k7m2' })
    expect(bootstrap.agents.agents.map((agent) => agent.id)).toEqual(['k7m2'])
    expect(requests).toContain('api/agents/k7m2')
    expect(requests).not.toContain('api/agents?runtime=current')
  })
})
