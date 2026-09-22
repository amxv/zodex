---
title: "Troubleshooting"
description: "Diagnose Zodex Local setup, tunnel, ChatGPT app, credential storage, startup, host permissions, Agent, history, and shutdown problems."
order: 8
category: Local
summary: "A symptom-first checklist for getting the Local runtime and Secure MCP Tunnel healthy again."
---

Start with these three commands:

```bash
zodex local status
zodex local logs --lines 500
zodex local config get
```

Then match the symptom below.

## `zodex local setup` rejects the tunnel or key

Check:

1. the tunnel ID belongs to the Platform organization you intended;
2. the runtime key has **Tunnels Read + Use**;
3. the key is still active;
4. Local is stopped before rerunning setup.

Rerun interactively to remove shell/environment ambiguity:

```bash
zodex local stop
zodex local setup
```

You do not need an admin key or a Tunnels Manage-capable runtime key.

## The tunnel does not appear in ChatGPT

Platform and ChatGPT permissions are separate.

Verify:

- the tunnel is associated with the target ChatGPT workspace;
- the app creator can use developer mode/custom apps;
- the tunnel/app identity has Tunnels Read + Use;
- `zodex local status` reports the Local tunnel ready.

For a personal account, use the personal Platform organization tied to that account. If you are using a separate ChatGPT workspace, make sure the tunnel is associated with the workspace you actually selected in ChatGPT.

See [Local setup and ChatGPT connection](/docs/local/setup).

## ChatGPT scans zero or unexpected tools

Zodex should expose exactly:

```text
exec_command
write_stdin
apply_patch
```

First make sure the ChatGPT app is connected through the intended **Tunnel** and that Local is currently running.

Then:

```bash
zodex local status
zodex local logs --lines 500
```

If the app was created while the tunnel/runtime was unhealthy, refresh/rescan the app after Local is ready.

## `zodex local start` fails

Run:

```bash
zodex local status
zodex local logs --lines 500
```

Common causes:

- setup was never completed;
- the macOS Keychain / Windows Credential Manager runtime key is missing or invalid;
- the configured tunnel no longer exists or the key lost Use permission;
- the start directory does not exist or macOS blocks access;
- a stale runtime/tunnel state needs cleanup;
- outbound HTTPS to OpenAI is blocked.

On macOS, when repeatedly installing locally compiled builds, a Keychain password prompt after each rebuild means the ad-hoc code signature changed. Maintainers can enroll the stable local signer described in [Development](/docs/reference/development#stable-keychain-access-for-source-builds). Normal signed release upgrades do not need this development setup. Windows uses Credential Manager and does not use this macOS code-signing workaround.

Try a clean stop/start:

```bash
zodex local stop
cd /absolute/path/to/repo
zodex local start
```

If setup state itself is broken:

```bash
zodex local stop
zodex local setup
zodex local start
```

## Status says `stale runtime state`

A previous runtime ended without its normal cleanup path.

```bash
zodex local stop
zodex local status
zodex local start
```

Zodex validates process identity before stale cleanup rather than blindly killing a reused PID. If cleanup refuses, read `zodex local logs` instead of deleting runtime files manually.

## A command cannot access Desktop, Documents, Downloads, iCloud, or app data

This is usually macOS privacy/TCC, not the Zodex start directory.

Local runs with your user permissions but does not bypass macOS privacy controls. Use **System Settings → Privacy & Security** to grant the appropriate Files & Folders or Full Disk Access permission to the effective Zodex runtime identity when needed.

Do not modify the TCC database manually.

## A CLI works in my terminal but ChatGPT cannot find it

Local captures the environment when you run `zodex local start`.

After changing PATH, installing a tool, or switching toolchain managers:

```bash
zodex local stop
# use the terminal environment where the command works
zodex local start
```

Check the tool with an explicit shell command from ChatGPT, such as `command -v <tool>`.

Aliases and shell functions are not carried as a compatibility feature; PATH-visible executables are.

## ChatGPT starts in the wrong repository

The start directory is guidance, not a hidden execution default. Check what Local is currently advertising:

```bash
zodex local status
```

To change it:

```bash
zodex local stop
cd /absolute/path/to/the/right/repo
zodex local start
```

Then open a fresh ChatGPT conversation/app context so it receives the new runtime guidance.

Before opening that conversation, open the Zodex Local app in ChatGPT app settings and choose **Refresh**. ChatGPT caches MCP server instructions, so restarting Local from another directory does not automatically invalidate the app's previously cached start-directory guidance.

Every `exec_command` and `apply_patch` request still has its own explicit absolute workdir.

## `watch` shows no Agent yet

Agents appear after attributed tool activity, not merely because a ChatGPT tab is open.

Trigger a harmless Zodex tool call in the ChatGPT conversation, then reopen or leave Liveboard running:

```bash
zodex local watch
```

To inspect recent calls regardless of live viewer state:

```bash
zodex local history --last 20
```

## One conversation appears as `Unattributed`

Zodex does not guess Agent identity from time or workdir. If the client does not send supported provider conversation metadata, history stays explicitly unattributed.

Confirm you are testing through the intended ChatGPT developer-mode app/tunnel rather than a different MCP client.

If Liveboard is already open, check **All Agents**: a newly observed Agent can remain off-board when your configured column maximum is already full.

## `watch --agent` or `watch --all` is rejected

Those are terminal-viewer filters. Select the TUI explicitly:

```bash
zodex local watch --tui --all
zodex local watch --tui --agent k7m2
```

Agent IDs are exactly four lowercase ASCII letters/digits, for example `k7m2`.

Find current IDs in Liveboard's **All Agents** drawer, `zodex local history`, or the observability API.

## Liveboard opens a URL but the browser cannot connect

Keep the `zodex local watch` process running. The printed capability URL belongs to that foreground process; `Ctrl-C` shuts the temporary host down.

If the browser did not open automatically, copy the `Liveboard: http://127.0.0.1:.../<capability>/` URL printed by the CLI into your browser. Do not reuse an old capability URL from an earlier `watch` process.

If the page reports a version mismatch or observer failure, confirm the running Local runtime and the `zodex` binary you launched agree:

```bash
zodex local status
zodex local stop
zodex local start
zodex local watch
```

## History is over its configured budget

Inspect:

```bash
zodex local status
zodex local config get history.max-age
zodex local config get history.max-size
```

A single newest complete invocation can temporarily remain over budget rather than being corrupted. Status can also remain temporarily over budget while already-pruned database pages are reclaimed incrementally. If you want different retention:

```bash
zodex local stop
zodex local config set history.max-size 1gb
zodex local start
```

## Clear history completely

```bash
zodex local stop
zodex local history clear --yes
```

## `zodex local stop` completed but I intentionally daemonized something

Local promises cleanup for normal Zodex-owned foreground/background process trees. It is not a containment system for a process deliberately detached into another lifecycle manager such as launchd.

Stop that process using the mechanism that owns it.

## Building/debugging a custom Local observability client

Liveboard and the built-in TUI use the same public observer model as custom clients. If first-party `watch` works but your direct API client does not, compare your discovery/version/auth flow with [Local observability API](/docs/local/observability-api). For first-party viewer behavior, see [Watch and Liveboard](/docs/local/watch).

The API guide covers API v1 / presentation v2 / event v2, Bearer authentication, runtime discovery, canonical timeline pagination, checkpoints, raw-vs-display output, selective live PTY streams, explicit `gap` events, durable recovery, browser security constraints, and version handling.
