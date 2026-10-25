import { Show, createEffect, createSignal } from 'solid-js'

import type {
  LiveboardPreferences,
  LiveboardPreferencesPatch,
  ThemePreference,
} from '../api/client'
import { CloseIcon, CogIcon } from './icons'
import { AgentAgeSelect } from './AgentAgeSelect'
import { createDrawerPresence } from './drawerPresence'

export function SettingsDrawer(props: {
  open: boolean
  preferences: LiveboardPreferences
  onClose: () => void
  onPatch: (patch: LiveboardPreferencesPatch) => void
  onMaximumChange: (maximum: number) => void
  showBoardSettings?: boolean
}) {
  const [editorDraft, setEditorDraft] = createSignal(props.preferences.editor_command)
  let closeButton: HTMLButtonElement | undefined
  const presence = createDrawerPresence(() => props.open)

  createEffect(() => setEditorDraft(props.preferences.editor_command))
  createEffect(() => {
    if (presence.visible()) queueMicrotask(() => closeButton?.focus())
  })

  const saveEditor = () => {
    const command = editorDraft().trim()
    if (!command) {
      setEditorDraft(props.preferences.editor_command)
      return
    }
    if (command !== props.preferences.editor_command) {
      props.onPatch({ editor_command: command })
    }
  }

  return (
    <Show when={presence.present()}>
      <div
        class="drawer-backdrop"
        classList={{ 'drawer-backdrop-open': presence.visible() }}
        aria-hidden={!props.open}
        inert={!props.open}
        onPointerDown={(event) => {
          if (props.open && event.target === event.currentTarget) props.onClose()
        }}
        onKeyDown={(event) => {
          if (props.open && event.key === 'Escape') props.onClose()
        }}
      >
        <aside
          class="agent-drawer settings-drawer"
          role="dialog"
          aria-modal="true"
          aria-labelledby="settings-drawer-title"
        >
          <header class="drawer-header">
            <div class="drawer-title-row">
              <CogIcon />
              <h2 id="settings-drawer-title">Settings</h2>
            </div>
            <button
              ref={closeButton}
              type="button"
              class="icon-button"
              aria-label="Close Settings"
              onClick={props.onClose}
            >
              <CloseIcon />
            </button>
          </header>
          <div class="drawer-list settings-list">
            <section class="settings-section">
              <h3>Appearance</h3>
              <label class="settings-row">
                <span class="settings-copy">
                  <strong>Theme</strong>
                  <small>Choose how Liveboard follows your system appearance.</small>
                </span>
                <select
                  aria-label="Liveboard theme"
                  value={props.preferences.theme}
                  onChange={(event) =>
                    props.onPatch({ theme: event.currentTarget.value as ThemePreference })
                  }
                >
                  <option value="system">System</option>
                  <option value="light">Light</option>
                  <option value="dark">Dark</option>
                </select>
              </label>
            </section>

            <Show when={props.showBoardSettings ?? true}>
              <section class="settings-section">
                <h3>Board</h3>
                <label class="settings-row">
                  <span class="settings-copy">
                    <strong>Maximum visible agents</strong>
                    <small>Limit how many agent columns can be on the board at once.</small>
                  </span>
                  <select
                    aria-label="Maximum visible Agents"
                    value={props.preferences.max_visible_agents}
                    onChange={(event) =>
                      props.onMaximumChange(Number(event.currentTarget.value))
                    }
                  >
                    {[1, 2, 3, 4, 5, 6, 7, 8].map((value) => (
                      <option value={value}>{value}</option>
                    ))}
                  </select>
                </label>
              </section>
            </Show>

            <section class="settings-section">
              <h3>Agents</h3>
              <div class="settings-row">
                <span class="settings-copy">
                  <strong>Hide inactive agents after</strong>
                  <small>Only filters All Agents. Running and selected agents stay visible.</small>
                </span>
                <AgentAgeSelect
                  hours={props.preferences.hide_inactive_agents_after_hours}
                  onChangeHours={(hours) => props.onPatch({ hide_inactive_agents_after_hours: hours })}
                />
              </div>
            </section>

            <section class="settings-section">
              <h3>Timeline defaults</h3>
              <label class="settings-row">
                <span class="settings-copy">
                  <strong>Command output</strong>
                  <small>Choose whether command output starts expanded or collapsed.</small>
                </span>
                <select
                  aria-label="Default command output state"
                  value={props.preferences.command_outputs_expanded ? 'expanded' : 'collapsed'}
                  onChange={(event) =>
                    props.onPatch({
                      command_outputs_expanded: event.currentTarget.value === 'expanded',
                    })
                  }
                >
                  <option value="collapsed">Collapsed</option>
                  <option value="expanded">Expanded</option>
                </select>
              </label>
              <label class="settings-row">
                <span class="settings-copy">
                  <strong>File diffs</strong>
                  <small>Choose whether apply-patch diffs start expanded or collapsed.</small>
                </span>
                <select
                  aria-label="Default file diff state"
                  value={props.preferences.diffs_expanded ? 'expanded' : 'collapsed'}
                  onChange={(event) =>
                    props.onPatch({ diffs_expanded: event.currentTarget.value === 'expanded' })
                  }
                >
                  <option value="expanded">Expanded</option>
                  <option value="collapsed">Collapsed</option>
                </select>
              </label>
              <label class="settings-row">
                <span class="settings-copy">
                  <strong>Raw tool button</strong>
                  <small>Show a Raw button on command cards for exact tool input and output.</small>
                </span>
                <select
                  aria-label="Raw tool button visibility"
                  value={props.preferences.show_raw_button ? 'shown' : 'hidden'}
                  onChange={(event) =>
                    props.onPatch({ show_raw_button: event.currentTarget.value === 'shown' })
                  }
                >
                  <option value="hidden">Hidden</option>
                  <option value="shown">Shown</option>
                </select>
              </label>
            </section>

            <section class="settings-section">
              <h3>Editor</h3>
              <label class="settings-editor-row">
                <span class="settings-copy">
                  <strong>Editor command</strong>
                  <small>
                    Used when you click a changed filename. The exact file path is passed as one
                    argument; no shell is used.
                  </small>
                </span>
                <input
                  aria-label="Editor command"
                  value={editorDraft()}
                  placeholder="zed"
                  spellcheck={false}
                  onInput={(event) => setEditorDraft(event.currentTarget.value)}
                  onBlur={saveEditor}
                  onKeyDown={(event) => {
                    if (event.key === 'Enter') event.currentTarget.blur()
                    if (event.key === 'Escape') {
                      setEditorDraft(props.preferences.editor_command)
                      event.currentTarget.blur()
                    }
                  }}
                />
              </label>
              <div class="settings-editor-presets" aria-label="Editor command presets">
                {[
                  ['Zed', 'zed'],
                  ['VS Code', 'code'],
                  ['Cursor', 'cursor'],
                ].map(([label, command]) => (
                  <button
                    type="button"
                    class="settings-preset-button"
                    onClick={() => {
                      setEditorDraft(command!)
                      props.onPatch({ editor_command: command! })
                    }}
                  >
                    {label}
                  </button>
                ))}
              </div>
            </section>
          </div>
        </aside>
      </div>
    </Show>
  )
}
