---
title: "Observability API"
description: "Build read-only Local clients with the versioned localhost HTTP/SSE API, canonical timeline, bounded output views, audit checkpoints, and recovery."
order: 5
category: Local
summary: "The public read-only Local contract behind Liveboard, the terminal viewer, and custom observability clients."
---

Zodex Local exposes a versioned, read-only HTTP/SSE API on IPv4 loopback. It is the supported contract for observing Local Agents, canonical activity, durable output, and live updates without reading private Rust modules, SQLite tables, or provider metadata.

The built-in [Liveboard and terminal viewer](/docs/local/watch) use this same observer. Custom Swift, TypeScript, Python, Rust, Go, editor, terminal, or automation clients can use it directly.

Local deliberately separates **execution** from **observation**:

- ChatGPT uses the authenticated Local MCP listener and the three model-visible tools.
- Viewer clients use a different read-only observer listener.
- The observer has no command/control route.

A dashboard should consume the public Agent/presentation/timeline model. Do **not** parse `_meta["openai/session"]`, infer identity from workdirs, or read the history database schema directly.

## Current versions

The current public versions are:

| Contract | Version |
| --- | ---: |
| Observer HTTP API | `1` |
| Canonical presentation schema | `2` |
| Live event schema | `2` |

Discovery publishes the API and presentation versions. SSE events carry their own event `schema_version`.

Fail clearly when a server advertises a newer version than your client understands. Do not guess newer field semantics from provider payloads or database layout.

## Client flow

A normal timeline client should:

1. read the active runtime's `discovery.json`;
2. read the observability Bearer from the path in discovery;
3. verify API/presentation versions and record `runtime_id`;
4. fetch current Agents and an initial canonical `/v1/timeline` page;
5. attach `/v1/events`, selecting live PTY output only for the Agents currently visible if useful;
6. merge live notifications into canonical records rather than treating SSE payloads as a complete database;
7. on disconnect or explicit `gap`, reread discovery and recover through `/v1/timeline?recovery_since_ms=...` before continuing live;
8. fetch older timeline pages, bounded display output, checkpoints, and exact invocation evidence only when the user asks for them.

This keeps initial state bounded and gives late-joining or reconnecting clients the same durable truth as Liveboard.

## Discovery

While Local is running:

```text
${XDG_STATE_HOME:-~/.local/state}/zodex/local/runtime/discovery.json
```

The relevant shape is:

```json
{
  "schema_version": 1,
  "runtime_id": "...",
  "pid": 12345,
  "start_directory": "/Users/me/code/repo",
  "started_at": "...",
  "expires_at": "2026-08-17T08:00:00Z",
  "observability": {
    "api_version": 1,
    "presentation_version": 3,
    "base_url": "http://127.0.0.1:54321",
    "bearer_token_path": "/Users/me/.local/state/zodex/local/credentials/observability-bearer",
    "history_available": true,
    "sse_available": true
  }
}
```

Do not cache `base_url` across runtime restarts. A new Local runtime has a new `runtime_id` and can bind a different random loopback port. Reread discovery when reconnecting.

`expires_at` is the one runtime-wide Local TTL. It is `null` when Local was started without a TTL.

The observer exists only while Local is running. Durable history survives `zodex local stop`, but the HTTP listener is not an always-on history daemon; use `zodex local history` while the runtime is stopped.

## Authentication and browser boundary

Read `observability.bearer_token_path`, then send:

```http
Authorization: Bearer <token>
```

Every observer response, including errors, carries:

```http
Cache-Control: no-store
```

The observer binds only to IPv4 loopback. Missing or wrong auth returns `401`. It does not expose permissive arbitrary-origin CORS.

Treat the Bearer as local secret material. To intentionally rotate it:

```bash
zodex local setup --rotate-observability-bearer
```

Then reread discovery and the token file.

### Browser clients

A normal browser page should not receive the observer Bearer just to work around CORS. `EventSource` also cannot set the required Authorization header.

The first-party Liveboard solves this with a runtime-owned same-origin loopback capability host. Native code reads the Bearer, proxies only allowlisted observer resources, and serves the embedded frontend. `zodex local watch` resolves that host and opens, prints, or copies its stable URL. **The Bearer is not injected into Liveboard JavaScript.**

A custom browser UI should use an equivalent trusted localhost/native wrapper rather than weakening the observer itself.

## Resource map

Current read-only routes:

```text
GET /v1/status
GET /v1/agents
GET /v1/agents/{id}

GET /v1/timeline
GET /v1/timeline/diffs
GET /v1/timeline/{presentation_id}
GET /v1/timeline/{presentation_id}/checkpoints

GET /v1/invocations
GET /v1/invocations/{id}
GET /v1/invocations/{id}/output-metadata
GET /v1/invocations/{id}/output

GET /v1/events
```

Use **timeline** routes for normal UI. Use **invocation** routes and raw output for audit evidence. A raw capture with `capture_state: "complete"` is exact; oversized or interrupted captures are marked `incomplete` and may contain only the retained prefix. History capture is deliberately fail-open with respect to MCP execution.

## Errors

JSON errors use:

```json
{
  "schema_version": 1,
  "error": "human-readable message"
}
```

Typical status codes:

- `400` — malformed filters/cursors, invalid Agent IDs, relative workdirs, incompatible query combinations, unsupported output views, or limits above the documented caps;
- `401` — missing/wrong Bearer;
- `404` — Agent, invocation, or presentation record not found;
- `500` — durable observer state could not be read.

## Status

```text
GET /v1/status
```

Returns observer versions, `runtime_id`, durable history health, current-runtime Agent count, and runtime-wide active-process count:

```json
{
  "schema_version": 1,
  "api_version": 1,
  "presentation_version": 3,
  "runtime_id": "...",
  "history": {
    "database_exists": true,
    "physical_size_bytes": 123456,
    "health_state": "healthy",
    "health_reason": null,
    "over_budget": false,
    "last_retention_error": null
  },
  "current_runtime_agent_count": 2,
  "active_process_count": 1
}
```

Use discovery for `start_directory`, `started_at`, and `expires_at`.

## Agents

```text
GET /v1/agents
GET /v1/agents?runtime=current
GET /v1/agents/k7m2
```

Agent IDs match `[a-z0-9]{4}`. They are stable public identities derived by Local from provider correlation; clients do not need provider session keys.

An Agent includes:

- first/last seen timestamps;
- whether it appeared in the current runtime;
- active-process count;
- ordered unique declared workdirs with first/last invocation IDs and retained counts.

Workdirs are routing/context anchors, not identity and not confinement evidence. Different Agents can legitimately use the same workdir.

## Canonical timeline

For a normal activity UI, prefer:

```text
GET /v1/timeline
```

Query fields:

| Field | Meaning |
| --- | --- |
| `limit` | Records per page; default `50`, maximum `100`. |
| `cursor` | Opaque server cursor from a previous page. Do not decode or synthesize it. |
| `agent_id` | Optional four-character Agent filter. |
| `workdir` | Optional absolute declared-workdir filter. |
| `before_ms` | History boundary for older-page traversal. |
| `recovery_since_ms` | Durable reconnect window that also keeps relevant active roots visible. |
| `diffs` | `full` (default) returns bounded diff rows in the same response; `summary` returns file operations, exact paths, and line counts without the rows. |

`before_ms` and `recovery_since_ms` cannot be combined. Cursors are bound to the query mode; do not reuse a history cursor as a recovery cursor or vice versa.

Response:

```json
{
  "schema_version": 1,
  "presentation_version": 3,
  "runtime_id": "...",
  "records": [],
  "has_more": true,
  "next_cursor": "opaque..."
}
```

Each record is one canonical **presentation root** with a stable `presentation_id`. A root may summarize several logical MCP invocations—for example a command plus repeated process polls.

### Presentation v3 common fields

```json
{
  "presentation_id": "inv-123",
  "primary_invocation_id": 123,
  "raw_evidence_count": 4,
  "raw_invocation_ids": [123, 124, 125, 126],
  "raw_invocation_ids_truncated": false,
  "agent_id": "k7m2",
  "declared_workdir": "/Users/me/code/repo",
  "normalized_workdir": "/Users/me/code/repo",
  "new_workdir": null,
  "started_at_ms": 0,
  "duration_ms": 1250,
  "evidence": {
    "evidence_state": "complete",
    "capture_state": "complete",
    "degraded": false,
    "reason": null
  },
  "kind": "command"
}
```

`raw_invocation_ids` is only a bounded convenience sample (currently up to 32 IDs). Use `raw_evidence_count`, `raw_invocation_ids_truncated`, and checkpoint/audit routes rather than assuming the array contains every logical call.

Presentation kinds:

| `kind` | Main normalized fields |
| --- | --- |
| `command` | `command`, `status`, `effective_cwd`, `exit_code`, `termination_reason`, bounded presentation `output`, optional folded `polls` |
| `file_changes` | `source_tool`, canonical `changes[]` with operation/path/count data and optionally projected diff rows |
| `stdin` | target handle, bounded chars, creator Agent, `cross_agent`, result status |
| `kill` | target handle, creator Agent, `cross_agent`, result status |
| `poll_aggregate` | target handle, count, final status, caller Agents, cross-Agent state |
| `generic` | tool name, status, optional normalized summary |

Do not rebuild these semantics from raw tool arguments in the client.

### Canonical file change

```json
{
  "operation": "edited",
  "path": "src/main.rs",
  "old_path": null,
  "write_mode": null,
  "added": 4,
  "removed": 2,
  "diff_truncated": false,
  "diff_lines_included": true,
  "lines": [
    { "kind": "context", "old_line": 10, "new_line": 10, "text": "..." },
    { "kind": "remove", "old_line": 11, "new_line": null, "text": "..." },
    { "kind": "add", "old_line": null, "new_line": 11, "text": "..." }
  ]
}
```

Operations are `created`, `edited`, `deleted`, or `renamed`. Optional write mode is `overwrite` or `append`. `diff_truncated` means the server deliberately bounded display evidence; do not silently present it as a complete diff. With `diffs=summary`, `diff_lines_included` is `false` and `lines` is empty while operation, path, `added`, `removed`, and truncation metadata remain canonical.

File-change presentations are materialized from durable evidence once and persisted as both summary and full projections. `diffs=full` therefore does not trigger per-card diff recomputation or follow-up requests; it is the right mode when a UI expands diffs by default. `diffs=summary` avoids transferring/storing rows when diffs are collapsed by default.

To hydrate several already-loaded summary records in one round trip, use:

```text
GET /v1/timeline/diffs?presentation_ids=inv-123,inv-456
```

The endpoint accepts up to 100 presentation IDs and returns their current full canonical records. Prefer one batch when a UI switches multiple collapsed diffs to expanded rather than issuing one detail request per card.

## Timeline detail

```text
GET /v1/timeline/inv-123
```

Returns the current canonical record for one stable `presentation_id`:

```json
{
  "schema_version": 1,
  "presentation_version": 3,
  "runtime_id": "...",
  "record": { "presentation_id": "inv-123", "kind": "command" }
}
```

Live metadata notifications can use this route to refresh one card without rebuilding a broad history query.

## Audit checkpoints

```text
GET /v1/timeline/inv-123/checkpoints?limit=25
```

`limit` defaults to `25`, maximum `100`; pagination uses an opaque `cursor`.

A checkpoint is intentionally lighter than a raw invocation:

```json
{
  "invocation_id": 124,
  "checkpoint_kind": "poll",
  "agent_id": "k7m2",
  "started_at_ms": 0,
  "completed_at_ms": 0,
  "status": "success",
  "cross_agent": false,
  "evidence_state": "complete",
  "capture_state": "complete"
}
```

Use checkpoints to show an audit index lazily. Fetch `/v1/invocations/{id}` only after the user chooses exact evidence. This avoids loading large logical argument/result bodies merely to render the normal timeline.

## Logical invocation evidence

The older invocation query remains a public evidence/query layer:

```text
GET /v1/invocations?last=50
GET /v1/invocations/123
```

`last` defaults to `50`, maximum `100`. Supported list filters include `agent_id`, absolute `workdir`, `since_ms`, and `recovery_since_ms`.

Invocation records contain logical tool arguments/results, exact/normalized declared workdir, timing/outcome, evidence/capture state, process target/creator fields, and cross-Agent attribution where relevant.

For a UI, prefer `/v1/timeline`; use invocation data for explicit detail/audit workflows.

## Output metadata

Before requesting chunks:

```text
GET /v1/invocations/123/output-metadata
```

Response:

```json
{
  "schema_version": 1,
  "runtime_id": "...",
  "invocation_id": 123,
  "output": {
    "available": true,
    "chunk_count": 500,
    "size_bytes": 123456,
    "capture_state": "complete",
    "capture_reason": null,
    "first_cursor": 1,
    "last_cursor": 500
  }
}
```

This metadata-only route is useful for bounded recent-tail loading; it does not return the invocation body.

## Output pages: raw vs display

```text
GET /v1/invocations/123/output?cursor=0&limit=16&view=raw
GET /v1/invocations/123/output?cursor=437&limit=64&view=display
```

`limit` defaults to `16` and is capped at `64`. `view` defaults to `raw`.

Each chunk contains:

```json
{
  "sequence": 438,
  "observed_at_ms": 0,
  "text": "..."
}
```

`next_cursor` is the sequence of the first chunk not returned. Pass it back unchanged. It is not a page number.

### `view=raw`

Raw is durable evidence. It preserves the recorded PTY text rather than making it safe terminal/browser markup. Render it inertly and apply your own evidence-display policy.

### `view=display`

Display is the server's **statefully sanitized presentation stream**. It is appropriate for a normal terminal-like text card and can also report `display_state` / `display_reason` when display reconstruction is unavailable or degraded.

Do not substitute `display` for raw audit evidence, and do not substitute raw PTY text for a safe display surface. They intentionally serve different purposes.

## Live SSE v2

Attach from now:

```bash
curl -N \
  -H "Authorization: Bearer $TOKEN" \
  "$BASE/v1/events"
```

Live event v2:

```json
{
  "schema_version": 2,
  "runtime_id": "...",
  "sequence": 17,
  "emitted_at_ms": 0,
  "event_type": "presentation_updated",
  "agent_id": "k7m2",
  "invocation_id": 123,
  "presentation_id": "inv-123",
  "presentation_revision": 3,
  "payload": {
    "record": { "presentation_id": "inv-123", "kind": "command" }
  }
}
```

The live stream defaults to `diffs=summary`. Use `?diffs=full` when expanded file diffs should arrive in the same SSE event. The first-party Liveboard chooses this from its saved diff-expansion preference.

Current event types include:

```text
agent_first_seen
agent_workdir_added
invocation_started
invocation_completed
presentation_updated
output
output_complete
process_started
process_ended
gap
```

Metadata/lifecycle payloads are deliberately compact notifications. `presentation_updated` carries the current canonical record in `payload.record`; its file-diff projection follows the subscriber's `diffs=full|summary` query. This lets a live UI update the card without a detail GET. Refetch durable timeline state after a `gap`, or if a presentation update cannot be materialized.

`output` carries display-safe live text plus output sequence and display state, for example:

```json
{
  "output_sequence": 12,
  "text": "...",
  "display_state": "available",
  "display_reason": null
}
```

`output_complete` carries the final display state/reason. Exact PTY evidence remains on `view=raw`.

SSE frames also use the global runtime sequence as the SSE `id` and the JSON `event_type` as the SSE `event`. Keepalives are emitted approximately every 15 seconds and are not tool activity.

## Selective live output

Full PTY text can be the highest-volume live stream. Clients that show only a few Agents can subscribe globally for metadata while selecting output only for visible Agents:

```text
GET /v1/events?output_agent_ids=k7m2,m4n8
```

`output_agent_ids` accepts at most 32 valid Agent IDs. It filters only `output` / `output_complete`; other metadata events remain global.

To suppress live output entirely while keeping metadata:

```text
GET /v1/events?include_output=false
```

`include_output=false` wins over an output selection.

To filter **all** events to one Agent:

```text
GET /v1/events?agent_id=k7m2
```

A UI may overlap old/new SSE subscriptions briefly when changing visible output selections so it does not introduce a handover blind spot; sequence dedupe keeps duplicate metadata/output harmless.

## Sequence and gap semantics

SSE sequence is global to one runtime. Numeric jumps are normal on a filtered stream because unrelated events can be omitted.

Only an explicit `event_type: "gap"` means the broadcast receiver lagged and durable recovery is required. The gap payload includes `skipped_events` and a recovery hint.

Never use a sequence number from one `runtime_id` after Local restarts.

## Reconnect and durable recovery

For a canonical timeline client:

1. retain a bounded last-known timestamp for recovery bookkeeping;
2. on disconnect or explicit `gap`, reread `discovery.json`;
3. if `runtime_id` changed, reset runtime-scoped sequence state and treat it as a new runtime;
4. refresh `/v1/status` and current Agents;
5. query `/v1/timeline?recovery_since_ms=<last-known-ms>` with the same Agent/workdir scope;
6. merge returned canonical roots by stable `presentation_id`;
7. refresh specific timeline detail/output only when needed;
8. reconnect SSE from now.

Recovery mode includes roots that started or changed in the window and relevant still-active process roots, so a long-running command does not disappear merely because it began before the reconnect boundary.

Do not implement recovery by replaying a giant invocation list from sequence zero.

## Minimal Python client

The repository includes `examples/local_observability_client.py`. It uses only discovery, the public HTTP/SSE API, and version checks. By default it lists current Agents and a canonical timeline page; `--events` adds the authorized SSE stream.

```bash
python3 examples/local_observability_client.py
python3 examples/local_observability_client.py --agent k7m2 --events
```

The example intentionally stays small. A production UI should additionally maintain bounded render state, overlap live-output selection handovers when necessary, retry with backoff, preserve stable presentation IDs, and make raw evidence an explicit user action.

## Compatibility checklist

Review client compatibility whenever any of these change:

- routes or HTTP methods;
- response fields or semantics;
- Agent/workdir filtering;
- opaque cursor/pagination rules;
- presentation kinds or common fields;
- raw/display output semantics;
- live event fields/types/payloads;
- selective-output filtering;
- gap/recovery semantics;
- discovery fields or credential-discovery behavior;
- observer API, presentation, or event version numbers.

That versioned boundary is what lets Local's internal Rust/history implementation evolve without forcing every external viewer to follow source refactors.
