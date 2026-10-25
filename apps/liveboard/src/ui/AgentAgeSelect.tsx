import { For, Show, createSignal } from 'solid-js'

const HOUR_OPTIONS = [
  { hours: 1, label: '1 hour' },
  { hours: 3, label: '3 hours' },
  { hours: 6, label: '6 hours' },
  { hours: 12, label: '12 hours' },
  { hours: 24, label: '24 hours' },
  { hours: 48, label: '2 days' },
  { hours: 168, label: '7 days' },
  { hours: 720, label: '30 days' },
  { hours: 0, label: 'Never' },
] as const

export function AgentAgeSelect(props: {
  hours: number
  onChangeHours: (hours: number) => void
}) {
  const [editingCustom, setEditingCustom] = createSignal(false)
  const [customHours, setCustomHours] = createSignal('12')
  const isPreset = () => HOUR_OPTIONS.some((option) => option.hours === props.hours)
  const showCustomEditor = () => editingCustom() || !isPreset()

  const choose = (value: string) => {
    if (value === 'custom') {
      setCustomHours(String(props.hours || 12))
      setEditingCustom(true)
      return
    }
    setEditingCustom(false)
    props.onChangeHours(Number(value))
  }

  const applyCustom = () => {
    const hours = Number(customHours())
    if (!Number.isInteger(hours) || hours < 1 || hours > 8_760) return
    setEditingCustom(false)
    props.onChangeHours(hours)
  }

  return (
    <div class="agent-age-select">
      <select
        aria-label="Hide inactive Agents after"
        value={showCustomEditor() ? 'custom' : String(props.hours)}
        onChange={(event) => choose(event.currentTarget.value)}
      >
        <For each={HOUR_OPTIONS}>
          {(option) => <option value={option.hours}>{option.label}</option>}
        </For>
        <option value="custom">
          {!isPreset() && !editingCustom() ? `${props.hours} hours (custom)` : 'Custom…'}
        </option>
      </select>
      <Show when={showCustomEditor()}>
        <form
          class="agent-age-custom"
          onSubmit={(event) => {
            event.preventDefault()
            applyCustom()
          }}
        >
          <input
            aria-label="Custom inactive Agent cutoff in hours"
            type="number"
            min="1"
            max="8760"
            step="1"
            required
            value={editingCustom() ? customHours() : String(props.hours)}
            onFocus={() => {
              if (!editingCustom()) setCustomHours(String(props.hours || 12))
              setEditingCustom(true)
            }}
            onInput={(event) => setCustomHours(event.currentTarget.value)}
          />
          <button type="submit" class="text-button">Apply</button>
        </form>
      </Show>
    </div>
  )
}
