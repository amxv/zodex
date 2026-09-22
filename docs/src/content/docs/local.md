---
title: "Quick Start"
description: "Run ChatGPT directly on your trusted macOS or Windows machine with your normal files, credentials, and developer tools."
order: 1
category: Local
summary: "Install Zodex, provision an OpenAI Secure MCP Tunnel, connect ChatGPT, start Local from a repo, and inspect what each ChatGPT Agent does."
---

Zodex Local lets ChatGPT work directly on a **trusted Apple Silicon Mac or x86_64 Windows machine**. It uses the same three Zodex tools as Sprite mode, but commands run with the logged-in user's captured environment and normal filesystem permissions.

Local is intentionally **not a sandbox**. There is **no Zodex confinement boundary** around the repository you start from. If your user account can read, write, execute, or authenticate to something, a Local tool call can generally do the same, subject to the host operating system's normal permissions.

If you want an isolated remote Linux workspace with scoped GitHub write permissions instead, use [Quick Start: Zodex (Sprite)](/docs/sprite).

## Local quick start

### 1. Install the Zodex operator CLI

macOS:

```bash
curl -fsSL https://zodex.ashray.xyz/install.sh | bash
export PATH="$HOME/.local/bin:$PATH"
```

Windows PowerShell:

```powershell
irm https://zodex.ashray.xyz/install.ps1 | iex
```

Then verify with `zodex --help` and `zodex local --help`.

Published Local releases support Apple Silicon macOS and x86_64 Windows.

The normal non-root install uses `~/.local/bin`; add the PATH line above to your shell profile to keep it available in new terminals. An explicitly root-run install uses `/usr/local/bin` instead.

### 2. Create an OpenAI Secure MCP Tunnel

Open [OpenAI Platform tunnel settings](https://platform.openai.com/settings/organization/tunnels) and sign in with the same OpenAI account/email you use for ChatGPT.

1. Choose the Platform organization you want to use.
2. Create a tunnel in **Organization settings → Tunnels**.
3. Associate the tunnel with the ChatGPT workspace that should use Zodex Local.
4. Copy the tunnel ID. It looks like `tunnel_...`.

Creating or editing the tunnel requires **Tunnels Read + Manage**. That is an administrative setup permission; Zodex does not need to keep a Manage-capable key afterward.

Local needs no Sprite proxy, Cloudflare Worker, public inbound port, or separate proxy URL. The OpenAI Secure MCP Tunnel is the ChatGPT connection path for Local.

Most OpenAI paid plans support custom MCP servers.

### 3. Create a runtime API key with only tunnel Read + Use

Create a scoped Platform API key for the runtime. Give it only:

- **Tunnels: Read**
- **Tunnels: Use**

Do not give the runtime key **Manage** unless you independently need that permission. Zodex Local uses the key to read the selected tunnel and run the tunnel client; it does not need an OpenAI admin key.

Keep both values handy:

```text
Tunnel ID:   tunnel_...
Runtime key: sk-...
```

### 4. Run Local setup

The easiest setup is interactive:

```bash
zodex local setup
```

Zodex prompts for the tunnel ID and runtime key. It stores the runtime key in macOS Keychain or Windows Credential Manager, installs and verifies the matching managed OpenAI tunnel client, and creates the Local state it needs. Setup does **not** leave the Local runtime running when it exits.

On macOS, setup also enables the lightweight Zodex menu bar app by default. Windows does not install the macOS menu app; use the CLI and terminal watch UI there. Use `zodex local setup --no-menu-bar` on macOS if you prefer to leave the bundled menu app disabled.

For automation, use one of the non-argv secret inputs:

```bash
printf '%s\n' "$OPENAI_TUNNEL_RUNTIME_KEY" \
  | zodex local setup \
      --tunnel-id tunnel_<id> \
      --runtime-key-stdin
```

or:

```bash
zodex local setup \
  --tunnel-id tunnel_<id> \
  --runtime-key-env OPENAI_TUNNEL_RUNTIME_KEY
```

See [Local setup and ChatGPT connection](/docs/local/setup) for the full setup flow and every setup flag.

### 5. Start Local from the repo you want ChatGPT to use

```bash
cd ~/code/my-project
zodex local start
```

Or choose a path explicitly:

```bash
zodex local start ~/code/my-project
```

Add a wall-clock TTL when you want access to expire automatically:

```bash
zodex local start ~/code/my-project --ttl 4h
```

`start` waits until the Local runtime and OpenAI tunnel are ready, then returns. You can close that terminal; Local keeps running until you stop it, its TTL expires, you log out/reboot, or the runtime fails.

The directory you start from is published to ChatGPT as the **suggested initial explicit workdir**. It is a convenience for the model, not a filesystem boundary. Every `exec_command` and `apply_patch` request still carries an explicit absolute workdir, and an Agent can intentionally use another accessible path later.

ChatGPT caches MCP server instructions and tool descriptions in the app definition. If you stop Local and restart it from a different directory, open the Zodex Local app in ChatGPT app settings and choose **Refresh** before starting a fresh conversation. Restarting Local alone cannot invalidate ChatGPT's app cache.

### 6. Add the tunnel to ChatGPT

ChatGPT developer-mode access and Platform tunnel permissions are separate.

In ChatGPT on the web:

1. Enable developer mode for your account/workspace if needed.
2. Open **Settings → Apps → Create** (or the developer-mode Plugins page).
3. Choose **Tunnel** as the connection method.
4. Select your tunnel, or paste its `tunnel_...` ID.
5. Scan the tools and create the app.
6. Open a fresh chat, enable/select the Zodex app, and ask ChatGPT to inspect or work on the repo you started Local from.

Most OpenAI paid plans support custom MCP servers.

For workspace-specific steps and tunnel visibility troubleshooting, see [Local setup and ChatGPT connection](/docs/local/setup).

## What Local gives ChatGPT

The model sees exactly three Zodex tools:

- `exec_command` — run a command in an explicit absolute workdir;
- `write_stdin` — continue, poll, type into, or kill a long-running process;
- `apply_patch` — apply a patch in an explicit absolute workdir.

See [MCP tools](/docs/reference/tools) for the shared tool contract.

## The Local trust model

Local is for a machine you intentionally trust ChatGPT to operate.

Commands run as your logged-in user with the environment captured when you run `zodex local start`. On macOS Zodex uses the captured login shell. On Windows it uses Windows PowerShell and preserves the captured Windows environment, including case-insensitive environment variable names.

This also means the start directory is **not** a permission boundary. A command can use absolute paths, `cd` elsewhere, read another repo, or use credentials available to your account.

The host OS remains in charge of permissions. On macOS, protected locations may require normal Files & Folders or Full Disk Access grants; Zodex does not edit TCC databases. On Windows, normal NTFS/profile permissions remain authoritative.

If you want remote isolation and a GitHub-specific autonomy boundary, use [Sprite permissions and autonomy](/docs/sprite/permissions) instead.

## One runtime, many ChatGPT Agents

One Local runtime can serve multiple ChatGPT conversations at the same time. You do not start another daemon or tunnel for each chat.

ChatGPT supplies `_meta["openai/session"]` automatically with tool calls. Zodex uses it to group calls from a conversation and assigns a short **four-character** Agent ID such as `k7m2`. The model does not need to send an Agent ID in tool arguments.

Each Agent can work in a different repository or Git worktree as long as every command/patch supplies the intended absolute workdir.

By default, the first Zodex tool result for each ChatGPT conversation also includes a compact catalog of global skills plus the user's global Codex `AGENTS` instructions. When an Agent first successfully uses a workdir, Zodex can add a one-line `AGENTS.override.md`/`AGENTS.md` hint and a compact catalog of skills found directly under that workdir's `.agents/skills`. These additions are separate from the real tool output and can be independently configured or disabled. See [Local configuration](/docs/local/configuration#automatic-codex-style-context).

## One runtime-wide TTL

**One runtime-wide TTL** controls access for the entire Local service.

There is exactly **one** TTL for the whole Local runtime. Starting another chat, creating another Agent, running commands, or opening `watch` never extends it.

Examples:

```bash
zodex local start --ttl 30min
zodex local start --ttl 4h
zodex local start --ttl 2d
```

The TTL is wall-clock time. Sleep does not pause it.

## See what ChatGPT is doing now

Local exposes a **first-class localhost observability API** for viewing ChatGPT tool activity in real time. Start the first-party viewer with:

```bash
zodex local watch
```

On macOS, `watch` starts a temporary loopback capability host and opens the read-only multi-Agent Liveboard in your browser. On Windows, plain `watch` opens the terminal viewer instead. Both views use the same read-only observability model and durable history.

The terminal viewer remains available explicitly:

```bash
zodex local watch --tui
zodex local watch --tui --agent k7m2
zodex local watch --tui --all
```

`--agent` and `--all` are TUI-only filters. Liveboard manages visible Agents through its **All Agents** drawer instead.

`watch` is only a viewer. Opening or closing it does not start, stop, or extend Local. The browser never receives the observer Bearer: native Zodex proxies an allowlisted read-only surface through the temporary same-origin capability URL.

See [Watch and Liveboard](/docs/local/watch) for the full first-party interface, or [Local observability API](/docs/local/observability-api) to build your own client.

## Inspect durable history

History remains available after `watch` closes and after Local stops:

```bash
zodex local history --last 20
zodex local history --agent k7m2 --since 1h
zodex local history --workdir /absolute/repo/path
zodex local history --id <invocation-id> --raw
zodex local history --format json
```

The default history is compact and readable. `--raw` is for exact logical tool input/result evidence when you need to audit a specific invocation.

By default Local keeps history for 60 days or 500 MB, whichever requires cleanup first. See [Local configuration](/docs/local/configuration).

## Stop access

```bash
zodex local stop
```

Stopping Local closes new MCP work, removes the tunnel connection, and attempts to **kill everything normally spawned by Zodex** across all Agents. Deliberately self-daemonized or separately launchd-registered processes are outside the containment promise.

`stop` is safe to run again if Local is already stopped.

## Your normal Local workflow

A typical day looks like this:

```bash
cd ~/code/my-project
zodex local start --ttl 4h

# Use one or more ChatGPT conversations.
# Optional: open the viewer (browser Liveboard on macOS, TUI on Windows).
zodex local watch

# When finished:
zodex local stop
```

You only need `zodex local setup` again when you want to replace tunnel credentials/configuration or repair/update the managed tunnel setup.

## Local guides

- [Setup and connect ChatGPT](/docs/local/setup) — Platform tunnel, runtime key, setup flags, and ChatGPT app creation.
- [Daily use](/docs/local/daily-use) — start, status, watch, history, logs, stop, multiple Agents, and TTLs.
- [Configuration](/docs/local/configuration) — retention and non-secret Local settings.
- [Local command reference](/docs/local/command-reference) — every `zodex local` command and flag.
- [Local troubleshooting](/docs/local/troubleshooting) — tunnel, startup, credential-store, host-permission, Agent, and history problems.
- [Watch and Liveboard](/docs/local/watch) — macOS browser Liveboard, the macOS/Windows terminal TUI, output/diff behavior, and recovery.
- [Local observability API](/docs/local/observability-api) — build your own web dashboard, Swift/menu-bar client, terminal UI, editor integration, or other read-only observer.
