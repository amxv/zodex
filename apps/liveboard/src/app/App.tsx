import {
  Match,
  Switch,
  createEffect,
  createResource,
  createSignal,
  onCleanup,
  onMount,
} from 'solid-js'

import {
  loadBootstrap,
  fetchPreferences,
  fetchCurrentAgents,
  patchPreferences,
  type LiveboardPreferencesPatch,
} from '../api/client'
import { Board } from '../board/Board'
import { initialVisibleAgentIds } from '../board/model'
import {
  LAZY_DIFF_HIGHLIGHTER,
  type DiffHighlighter,
} from '../diff/HighlightWorkerClient'
import { createRuntimeConnection } from '../streams/runtime'
import { AgentTimeline } from '../timeline/AgentTimeline'
import { applyTheme } from './bootstrap'
import { createCoarseClock } from './clock'
import {
  focusedLiveboardUrl,
  parseLiveboardView,
  unifiedLiveboardUrl,
  type LiveboardView,
} from './view'

type Bootstrap = Awaited<ReturnType<typeof loadBootstrap>>

function LiveboardWorkspace(props: {
  bootstrap: Bootstrap
  viewerAttachWatermarkMs: number
  diffHighlighter: DiffHighlighter
  view: LiveboardView
}) {
  const [preferences, setPreferences] = createSignal(props.bootstrap.preferences)
  const [pendingSaves, setPendingSaves] = createSignal(0)
  const [saveError, setSaveError] = createSignal<string>()
  const now = createCoarseClock()
  const runtime = createRuntimeConnection({
    initialStatus: props.bootstrap.status,
    initialAgents: props.bootstrap.agents.agents,
    initialVisibleAgentIds:
      props.view.kind === 'focused'
        ? [props.view.agentId]
        : initialVisibleAgentIds(
            props.bootstrap.agents.agents,
            props.bootstrap.preferences,
          ),
    initialDiffProjection: props.bootstrap.preferences.diffs_expanded ? 'full' : 'summary',
    viewerAttachWatermarkMs: props.viewerAttachWatermarkMs,
    view: props.view,
  })
  let mutationQueue: Promise<void> = Promise.resolve()

  createEffect(() => applyTheme(preferences().theme))
  createEffect(() =>
    runtime.setDiffProjection(preferences().diffs_expanded ? 'full' : 'summary'),
  )
  onMount(() => {
    const refreshPreferences = () => {
      // Serialize with local pencil-button saves: a stale fetch must never
      // overwrite a more recent saved rename in this tab.
      mutationQueue = mutationQueue
        .then(async () => {
          setPreferences(await fetchPreferences())
        })
        .catch((error: unknown) => {
          setSaveError(error instanceof Error ? error.message : String(error))
        })
    }
    runtime.onPreferencesChanged(refreshPreferences)
    runtime.start()
  })
  onCleanup(() => {
    runtime.dispose()
  })

  const persistPreferences = (patch: LiveboardPreferencesPatch) => {
    const nextPatch =
      props.view.kind === 'focused'
        ? withoutFocusedLayoutMutations(patch)
        : patch
    if (Object.keys(nextPatch).length === 0) return
    setPendingSaves((count) => count + 1)
    setSaveError(undefined)
    mutationQueue = mutationQueue
      .then(async () => {
        const updated = await patchPreferences(nextPatch)
        setPreferences(updated)
      })
      .catch((error: unknown) => {
        setSaveError(error instanceof Error ? error.message : String(error))
      })
      .finally(() => setPendingSaves((count) => Math.max(0, count - 1)))
  }

  return (
    <div class="app-shell">
      <Board
        agents={runtime.agents()}
        preferences={preferences()}
        nowMs={now()}
        saving={pendingSaves() > 0}
        error={saveError()}
        connectionState={runtime.connectionState()}
        connectionError={runtime.connectionError()}
        focusedAgentId={props.view.kind === 'focused' ? props.view.agentId : undefined}
        onPatch={persistPreferences}
        onVisibleAgentsChange={runtime.setVisibleAgentIds}
        onFocusAgent={(agentId) => window.location.assign(focusedLiveboardUrl(agentId))}
        onLoadAllAgents={async () =>
          (await fetchCurrentAgents(runtime.runtimeId())).agents}
        renderTimeline={(agentId) => (
          <AgentTimeline
            controller={runtime.controllerFor(agentId)}
            runtimeId={runtime.runtimeId()}
            nowMs={now()}
            commandOutputsExpanded={preferences().command_outputs_expanded}
            diffsExpanded={preferences().diffs_expanded}
            showRawButton={preferences().show_raw_button}
            diffHighlighter={props.diffHighlighter}
          />
        )}
      />
    </div>
  )
}

function withoutFocusedLayoutMutations(
  patch: LiveboardPreferencesPatch,
): LiveboardPreferencesPatch {
  const next: LiveboardPreferencesPatch = {}
  for (const key of [
    'schema_version',
    'theme',
    'hide_inactive_agents_after_hours',
    'command_outputs_expanded',
    'diffs_expanded',
    'show_raw_button',
    'editor_command',
  ] as const) {
    const value = patch[key]
    if (value !== undefined) Object.assign(next, { [key]: value })
  }
  if (patch.agents) {
    const aliases = Object.fromEntries(
      Object.entries(patch.agents).flatMap(([agentId, preference]) =>
        preference.alias === undefined ? [] : [[agentId, { alias: preference.alias }]],
      ),
    )
    if (Object.keys(aliases).length > 0) next.agents = aliases
  }
  return next
}

export function App(props: { diffHighlighter?: DiffHighlighter }) {
  // Focused views need almost no upfront state. Initialize grammar workers
  // only when a visible diff actually requests syntax highlighting.
  const diffHighlighter = props.diffHighlighter ?? LAZY_DIFF_HIGHLIGHTER
  const viewerAttachWatermarkMs = Date.now()
  let view: LiveboardView | undefined
  let viewError: string | undefined
  try {
    view = parseLiveboardView(window.location.search)
  } catch (error) {
    viewError = error instanceof Error ? error.message : String(error)
  }
  const [bootstrap] = createResource(
    () => view,
    (resolvedView) => loadBootstrap(resolvedView),
  )

  return (
    <Switch>
      <Match when={viewError}>
        <main class="bootstrap-surface">
          <div role="status" class="connection-error">
            <strong>Invalid focused Liveboard link</strong>
            <span>{viewError}</span>
            <button type="button" class="text-button" onClick={() => window.location.assign(unifiedLiveboardUrl())}>
              Open All Agents
            </button>
          </div>
        </main>
      </Match>
      <Match when={bootstrap.loading}>
        <main class="bootstrap-surface">
          <p>Connecting to Zodex Local…</p>
        </main>
      </Match>
      <Match when={bootstrap.error}>
        <main class="bootstrap-surface">
          <div role="status" class="connection-error">
            <strong>{view?.kind === 'focused' ? 'Focused Agent unavailable' : 'Local observer disconnected'}</strong>
            <span>{String(bootstrap.error)}</span>
            {view?.kind === 'focused' ? (
              <button type="button" class="text-button" onClick={() => window.location.assign(unifiedLiveboardUrl())}>
                Open All Agents
              </button>
            ) : null}
          </div>
        </main>
      </Match>
      <Match when={bootstrap()}>
        {(value) => (
          <LiveboardWorkspace
            bootstrap={value()}
            viewerAttachWatermarkMs={viewerAttachWatermarkMs}
            diffHighlighter={diffHighlighter}
            view={view!}
          />
        )}
      </Match>
    </Switch>
  )
}
