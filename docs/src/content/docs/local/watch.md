---
title: "Watch"
description: "Open the Local Liveboard in your browser or use the terminal viewer on Linux, macOS, and Windows."
order: 4
category: Local
summary: "The first-party read-only Local viewer: multi-Agent browser Liveboard and terminal TUI on Linux, macOS, and Windows."
---

`zodex local watch` is the first-party real-time viewer for Zodex Local. It shows what ChatGPT is doing through the Local MCP server without giving the viewer command/control authority.

On Linux, macOS, and Windows, the default viewer is **Liveboard**, a browser UI built on the same public [Local observability API](/docs/local/observability-api) available to custom clients. The terminal UI remains available with `--tui`.

## Open Liveboard

```bash
zodex local watch
```

Zodex exposes Liveboard at the stable local-only address `http://127.0.0.1:64973/`, prints that URL, and opens it in your default browser. The browser-visible address stays stable across Local restarts; Zodex still keeps the internal read-only API behind a per-run private capability path. The Liveboard host is owned by the Local runtime, so the `watch` command does not need to stay running.

To print or copy the same stable URL without opening a browser:

```bash
zodex local watch url
zodex local watch copyurl
```

If the browser cannot be opened automatically, the CLI prints the URL so you can open it manually.

Closing the browser does **not** stop the Local runtime or revoke ChatGPT access. Use `zodex local stop` for that.

## What Liveboard shows

Liveboard is organized around the canonical four-character Local Agent IDs. Each visible Agent gets an independent column with its own virtualized timeline, follow position, unseen-activity count, recent workdir, and active-process status.

The timeline renders Zodex's server-owned presentation model rather than re-parsing MCP arguments in the browser. It includes:

- shell commands with running/final status, duration, exit state, folded polling, and expandable output;
- structured Added / Edited / Deleted / Renamed file cards with unified diff rows when evidence is available;
- explicit stdin and process-kill interactions;
- cross-Agent process attribution where Local can prove it;
- degraded/incomplete evidence notices rather than presenting partial capture as complete;
- stable presentation IDs that remain usable for reconnect recovery and audit drill-down.

Repeated no-input polls are folded into the command presentation. Exact logical invocations remain available through audit/history instead of filling the normal timeline.

## Board controls

Liveboard stores **UI preferences only**. They do not alter Agent identity, MCP permissions, execution routing, or history evidence.

The toolbar provides:

- **All Agents** — open the current-runtime Agent drawer and add hidden Agents to the board;
- **Columns** — set the maximum visible Agent columns from 1 through 8; the default is 4;
- **Cmd open / closed** — set the global default for command-output expansion; the default is closed;
- **Diff open / closed** — set the global default for file-diff expansion; the default is open;
- **Theme** — follow system appearance or force light/dark.

Each Agent column can also be:

- given a local alias of up to 80 characters;
- reordered by drag or the left/right controls;
- resized against an adjacent visible column;
- removed from the board without deleting the Agent or its history.

Aliases are labels for your browser view. The public Agent ID remains the four-character Zodex identity everywhere else.

A newly observed Agent does not evict an Agent you intentionally kept visible when the board is full. Open **All Agents** to decide what should be on the board.

## Command output

Collapsed command cards do not maintain an unbounded hidden terminal buffer. When output is expanded, Liveboard uses a bounded display-safe live tail and coalesces visible text updates to the browser's animation-frame cadence.

If output was missed, the viewer reconnects, or a final command is expanded later, Liveboard can lazily restore a bounded recent tail from the durable `view=display` output endpoint. It does not replay an entire long PTY stream into the normal card.

The display view is deliberately sanitized for presentation. Raw output remains an explicit evidence surface through the observability API and audit drill-down. When an invocation's capture state is `complete`, that raw evidence is exact; oversized or otherwise interrupted captures are explicitly marked `incomplete` rather than being allowed to backpressure the model-facing tool call.

## File diffs and highlighting

File cards render only the canonical operation/path/count/diff rows supplied by Local. The browser does not infer edits from raw patch text or shell arguments.

Common Rust, TypeScript, JavaScript, Go, Bash, Python, JSON, TOML/INI files are syntax-highlighted in one local browser worker. Unknown languages fall back to plain source text. Collapsed or virtualized-offscreen diffs do no highlighting work.

## Live attach, history, and recovery

Liveboard records one viewer attach boundary and joins live activity from there. Each visible Agent hydrates independently, and older history is fetched only when you request it by scrolling/loading backward rather than preloading the full database.

If the SSE connection drops, the board rereads current runtime state and uses durable timeline recovery before resuming live updates. An explicit SSE `gap` is a recovery signal; numeric gaps in a filtered stream are not, because events for other Agents may have been omitted intentionally.

If Local restarts, its `runtime_id` changes. Liveboard treats that as a runtime replacement instead of mixing sequence state from two runtimes.

## Use the terminal viewer

Use the terminal viewer explicitly on any supported OS:

```bash
zodex local watch --tui
```

TUI-only Agent filters require `--tui`:

```bash
zodex local watch --tui --agent k7m2
zodex local watch --tui --all
```

`--agent` opens or waits for one four-character Agent ID. In web mode it creates a focused Liveboard URL; in TUI mode it focuses the terminal viewer. `--all` combines current Agent activity in one terminal viewer.

### TUI keyboard controls

| Key | Action |
| --- | --- |
| `↑` / `k` | Move up |
| `↓` / `j` | Move down |
| `Enter` | Expand/collapse selected detail; choose an Agent in the picker |
| `Tab` | Next Agent |
| `Shift-Tab` | Previous Agent |
| `a` | Open the Agent picker |
| `r` | Toggle exact raw logical evidence for the selected invocation |
| `y` | Copy selected content |
| `g` | Jump to top |
| `G` | Jump to bottom |
| `/` | Search/filter the visible timeline |
| `q` or `Ctrl-C` | Quit |

You can run several TUI viewers at once, for example one per terminal pane:

```bash
zodex local watch --tui --agent k7m2
zodex local watch --tui --agent m4n8
```

The TUI remains a client of the public observer; it does not receive private execution authority.

## Security boundary

The observability listener requires a managed Bearer token and deliberately does not enable arbitrary-origin CORS. Liveboard does not weaken that contract.

Instead, the Local runtime owns a separate loopback **same-origin capability host**. That host reads the observer bearer on the native side, proxies only the allowlisted read-only observer resources Liveboard needs, serves the embedded frontend, and keeps the bearer out of browser JavaScript. `zodex local watch` only resolves that runtime-owned host and opens, prints, or copies its URL.

The browser-visible root is the stable `http://127.0.0.1:64973/` address. A random per-run capability path is injected into the page internally for asset and API requests, so it does not need to appear in the address bar or copied Liveboard links.

## Build another viewer

Liveboard and the TUI are two presentations of the same Local evidence model. A custom Swift app, menu-bar tool, editor panel, terminal UI, or automation client can use the public API directly.

Start with [Local observability API](/docs/local/observability-api) for discovery, Bearer auth, canonical timeline pagination, output views, checkpoint/audit resources, selective live output, SSE sequence semantics, and durable gap recovery.
