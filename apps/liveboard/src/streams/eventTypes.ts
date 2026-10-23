// Control, presentation and output events share one observer SSE connection.
// Keep this list in sync with the types emitted by Zodex Local.
export const LIVE_EVENT_TYPES = [
  'agent_first_seen',
  'agent_workdir_added',
  'invocation_started',
  'invocation_completed',
  'presentation_updated',
  'output',
  'output_complete',
  'process_started',
  'process_ended',
  'preferences_changed',
  'gap',
] as const

export type LiveEventType = (typeof LIVE_EVENT_TYPES)[number]
