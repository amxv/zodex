import {
  Show,
  createEffect,
  createSignal,
  onCleanup,
  onMount,
} from 'solid-js'

import type { CommandOutputState } from '../output/CommandOutputState'
import { CheckIcon, CopyIcon } from '../ui/icons'

export function ExpandedCommandOutput(props: {
  state: CommandOutputState
  final: boolean
}) {
  const [revision, setRevision] = createSignal(0)
  const [copied, setCopied] = createSignal(false)
  let outputElement: HTMLPreElement | undefined
  let frame: number | undefined
  let copyFeedbackTimeout: ReturnType<typeof setTimeout> | undefined

  const flush = () => {
    frame = undefined
    setRevision((value) => value + 1)
  }
  const scheduleFlush = () => {
    if (frame === undefined) frame = requestAnimationFrame(flush)
  }

  onMount(() => {
    const unsubscribe = props.state.subscribe(scheduleFlush)
    scheduleFlush()
    props.state.requestRecentTail()
    onCleanup(unsubscribe)
  })
  onCleanup(() => {
    if (frame !== undefined) cancelAnimationFrame(frame)
    if (copyFeedbackTimeout !== undefined) clearTimeout(copyFeedbackTimeout)
  })

  createEffect(() => {
    if (props.final) props.state.markFinal()
  })

  createEffect(() => {
    revision()
    if (outputElement) outputElement.textContent = props.state.materialize()
  })

  const hasDroppedPrefix = () => {
    revision()
    return props.state.hasDroppedPrefix()
  }
  const displayUnavailable = () => {
    revision()
    return props.state.isDisplayUnavailable()
  }
  const displayUnavailableReason = () => {
    revision()
    return props.state.displayUnavailableReason()
  }
  const recoveryError = () => {
    revision()
    return props.state.recoveryErrorMessage()
  }
  const loading = () => {
    revision()
    return props.state.isLoading()
  }
  const outputText = () => {
    revision()
    return props.state.materialize()
  }
  const copyOutput = async () => {
    if (!navigator.clipboard) return
    await navigator.clipboard.writeText(outputText())
    setCopied(true)
    if (copyFeedbackTimeout !== undefined) clearTimeout(copyFeedbackTimeout)
    copyFeedbackTimeout = setTimeout(() => {
      setCopied(false)
      copyFeedbackTimeout = undefined
    }, 2_000)
  }

  return (
    <div
      class="command-output-region"
      classList={{ 'command-output-region-dropped': hasDroppedPrefix() }}
    >
      <button
        type="button"
        class="command-output-copy-button"
        disabled={outputText().length === 0}
        aria-label={copied() ? 'Command output copied' : 'Copy command output'}
        title={copied() ? 'Copied' : 'Copy command output'}
        onClick={() => void copyOutput()}
      >
        {copied() ? <CheckIcon /> : <CopyIcon />}
      </button>
      <Show when={hasDroppedPrefix()}>
        <div class="output-notice output-notice-dropped">
          Recent output tail · earlier buffered output dropped
        </div>
      </Show>
      <Show when={displayUnavailable()}>
        <div class="output-notice output-notice-warning">
          Display-safe output unavailable
          {displayUnavailableReason()
            ? ` · ${displayUnavailableReason()}`
            : ''}
        </div>
      </Show>
      <Show when={recoveryError()}>
        {(message) => (
          <div class="output-notice output-notice-warning" role="alert">
            {message()}
            {' · '}
            <button
              type="button"
              class="text-button"
              onClick={() => props.state.requestRecentTail()}
            >
              Retry output
            </button>
          </div>
        )}
      </Show>
      <Show
        when={
          loading() &&
          outputText().length === 0 &&
          !displayUnavailable() &&
          !recoveryError()
        }
      >
        <div class="output-notice">Loading output…</div>
      </Show>
      <pre
        ref={outputElement}
        class="command-output-text"
        aria-label="Command output"
      />
    </div>
  )
}
