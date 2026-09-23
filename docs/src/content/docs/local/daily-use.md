---
title: "Daily use"
description: "Start, inspect, observe, and stop Zodex Local while one or more ChatGPT conversations work on your trusted Linux, macOS, or Windows host."
order: 3
category: Local
summary: "The practical Local workflow for runtime TTLs, status, Agents, watch, durable history, logs, and clean shutdown."
---

Once [Local setup](/docs/local/setup) is complete, use the CLI on any supported platform. macOS also offers the optional Zodex menu bar app.

On macOS, `zodex local setup` enables the small menu bar app by default. It returns automatically when you next log in, while the Zodex Local runtime itself remains stopped until you explicitly start it. If you opted out during setup, you can still open the app manually:

```bash
zodex local menu
```

Set a persistent start folder such as `~/code/amxv`, then **Start Zodex**, **Stop Zodex**, **Open Liveboard**, or update Zodex from the menu bar. The app refreshes status when you open its menu and immediately after relevant user actions, with no background polling timer. Update checks also happen only when you open the menu or choose **Check for Updates…**; repeated menu opens use the CLI's short cache. Toggle **Launch at Login** off whenever you no longer want the menu app to return after logout or restart.

## Start from the project you want ChatGPT to begin with

```bash
cd ~/code/my-project
zodex local start
```

The current directory becomes the start directory ChatGPT is shown as a suggested explicit workdir.

You can also provide it directly:

```bash
zodex local start ~/code/my-project
```

### Give access an expiry

```bash
zodex local start --ttl 30min
zodex local start --ttl 4h
zodex local start --ttl 2d
```

The TTL belongs to the one Local runtime. It is not renewed when another ChatGPT conversation starts or when an existing Agent runs more commands.

## Starting again while Local is already running

A second `zodex local start` does not silently switch repositories or change the TTL. It reports the active runtime and returns successfully.

To change runtime-level start state:

```bash
zodex local stop
cd ~/code/another-project
zodex local start --ttl 2h
```

After changing the start directory, open the Zodex Local app in ChatGPT app settings and choose **Refresh** before opening a fresh conversation. ChatGPT caches MCP server instructions, so restarting Local alone cannot replace the start-directory guidance already cached in the app definition.

## Check status

Human-readable status:

```bash
zodex local status
```

Machine-readable status:

```bash
zodex local status --json
```

The human view tells you whether Local is configured/running, when it expires, the start directory, MCP/tunnel readiness, current Agent count, active process count, and history health.

If status says the runtime is stale, run:

```bash
zodex local stop
zodex local start
```

and use [Local troubleshooting](/docs/local/troubleshooting) if it cannot recover.

## Work with several ChatGPT conversations

You can open multiple ChatGPT conversations against the same Local app. Zodex automatically groups each attributed conversation under a short Agent ID such as `k7m2`.

The conversations can work concurrently in different repositories or worktrees. Their tool calls carry explicit absolute workdirs, so you do not need a daemon per repository.

Local does not restrict an Agent to its first workdir. A new declared workdir is recorded so unexpected workspace drift is visible, but it is not blocked.

## Watch live activity

On macOS, open the first-party browser Liveboard:

```bash
zodex local watch
```

The CLI stays in the foreground while the temporary Liveboard host is running. `Ctrl-C` closes the viewer host but does not stop Local.

Liveboard gives each visible Agent an independent timeline. Use **All Agents** to manage the board, **Columns** for a 1–8 column cap, the Cmd/Diff controls for global expansion defaults, and the Agent headers to alias, reorder, resize, or hide columns. Those preferences are UI-only and do not change Agent identity or permissions.

On Linux and Windows, plain `zodex local watch` uses the TUI by default. On macOS, opt into the TUI explicitly:

```bash
zodex local watch --tui
zodex local watch --tui --agent k7m2
zodex local watch --tui --all
```

Useful TUI keys include:

- arrows or `j` / `k` — move;
- `Enter` — expand/collapse;
- `Tab` / `Shift-Tab` — cycle Agents;
- `a` — Agent picker;
- `r` — raw evidence view;
- `y` — copy;
- `g` / `G` — top/bottom;
- `/` — search;
- `q` — quit.

Closing `watch` never stops the runtime.

For Liveboard controls, the TUI keyboard map, output/diff semantics, and recovery behavior, see [Watch and Liveboard](/docs/local/watch). To build another interface on the same API, see [Local observability API](/docs/local/observability-api).

## Inspect history

Recent activity:

```bash
zodex local history
zodex local history --last 20
zodex local history --since 2h
```

One Agent:

```bash
zodex local history --agent k7m2
zodex local history --agent k7m2 --since 1h
```

One declared workdir:

```bash
zodex local history --workdir /absolute/repo/path
```

One invocation:

```bash
zodex local history --id <invocation-id>
```

Exact logical tool evidence:

```bash
zodex local history --id <invocation-id> --raw
```

Structured output:

```bash
zodex local history --format json
```

History works while Local is running or stopped.

## Clear history

Local must be stopped first:

```bash
zodex local stop
zodex local history clear
```

For non-interactive cleanup:

```bash
zodex local history clear --yes
```

Clearing history also removes retained Agent mappings, so a conversation seen again later may receive a new short Agent ID.

## Read diagnostic logs

```bash
zodex local logs
zodex local logs --lines 500
```

Use logs for setup/start/tunnel lifecycle problems. Use `history` for what ChatGPT actually asked Zodex to do.

## Stop Local

```bash
zodex local stop
```

Stopping Local:

- rejects new command/patch/stdin work;
- shuts down tunnel ingress;
- terminates active Zodex-owned process groups and ordinary descendants where they can be identified;
- finalizes durable history;
- removes active runtime state.

Persistent configuration, history, and the managed setup survive for the next `start`.

## Upgrade Zodex

Check without installing:

```bash
zodex upgrade --check
```

Upgrade to the latest release or pin a release explicitly:

```bash
zodex upgrade

# Optional pinned form; the leading v is optional.
zodex upgrade --version 0.3.4
```

`zodex upgrade` first resolves the target version. If the installed version is already current it exits without downloading the release archive. During an update it prints progress while downloading, verifying, and installing. A five-minute cache is used only by `--check`; pass `--refresh` to force a fresh check.

On Linux, macOS, and Windows, an update that would replace a running Local runtime is refused before the release archive is downloaded. Stop Local yourself, or explicitly authorize the upgrade command to do it:

```bash
zodex upgrade --stop-local
```

The release checksum is verified before installation. Your Local configuration, platform credential storage, durable history, and platform-specific viewer/control preferences are preserved. On macOS, if the menu bar app is open, it restarts into the new version; if you had intentionally quit it, the upgrade leaves it quit.

The menu bar uses this same CLI upgrade contract. It checks for updates when you open the menu, shows **Update to v…** when one is available, and offers **Stop and Update** only when the CLI reports that Local is blocking the update.

## What happens on logout or reboot?

Local is not an auto-start login service. A later login/reboot requires another explicit:

```bash
zodex local start
```

That is intentional: access is something you turn on, optionally give a TTL, and turn off.

## Next

- [Watch and Liveboard](/docs/local/watch)
- [Local observability API](/docs/local/observability-api)
- [Local command reference](/docs/local/command-reference)
- [Local configuration](/docs/local/configuration)
- [Local troubleshooting](/docs/local/troubleshooting)
