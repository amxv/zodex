---
title: "Command reference"
description: "A compact reference for every public zodex local command, argument, option, and common example."
order: 7
category: Local
summary: "The public Local CLI: setup, start, status, watch, history, config, logs, stop, plus macOS menu-bar controls."
---

Use this page when you already understand [Local](/docs/local) and just need the exact command surface.

## `zodex local setup`

One-time or repair provisioning.

```text
zodex local setup [OPTIONS]
```

Options:

```text
--tunnel-id <TUNNEL_ID>
--runtime-key-stdin
--runtime-key-env <ENV>
--runtime-key-fd <FD>
--rotate-observability-bearer
--no-menu-bar
```

Interactive:

```bash
zodex local setup
```

Automation:

```bash
printf '%s\n' "$OPENAI_TUNNEL_RUNTIME_KEY" \
  | zodex local setup \
      --tunnel-id tunnel_<id> \
      --runtime-key-stdin
```

`--runtime-key-stdin`, `--runtime-key-env`, and `--runtime-key-fd` are mutually exclusive.

`--runtime-key-fd` is Unix-only. Windows automation should use stdin or an environment variable.

On macOS, setup enables and opens the lightweight Zodex menu bar app by default. It is registered to return when you next log in, but it does not start the Zodex Local runtime. Pass `--no-menu-bar` to leave the bundled menu app disabled and unopened.

On Linux, setup stores the runtime key in a mode-`0600` user-only credential file, installs the Linux tunnel-client bundle, and ignores the menu-bar option.

On Windows, setup stores the runtime key in Windows Credential Manager, installs the Windows tunnel-client bundle, and ignores the menu-bar option.

## `zodex local start`

```text
zodex local start [OPTIONS] [PATH]
```

Arguments/options:

```text
[PATH]       start directory; defaults to current directory
--ttl <TTL>  optional wall-clock runtime lifetime
```

Examples:

```bash
cd ~/code/project && zodex local start
zodex local start ~/code/project
zodex local start ~/code/project --ttl 4h
```

Common TTL forms: `30min`, `4h`, `2d`.

## `zodex local status`

```text
zodex local status [--json]
```

Examples:

```bash
zodex local status
zodex local status --json
```

## `zodex local watch`

```text
zodex local watch [OPTIONS]
```

Options:

```text
--tui            use the terminal viewer instead of the default web Liveboard
--no-open        serve Liveboard without opening the default browser
--agent <AGENT>  in TUI mode, watch/wait for one four-character Agent ID
--all            in TUI mode, combine all Agents
```

`--no-open` is web-Liveboard only and cannot be combined with `--tui`. `--agent` requires `--tui` and cannot be combined with `--all`. `--all` also requires `--tui`.

Examples:

```bash
zodex local watch
zodex local watch --no-open
zodex local watch --tui
zodex local watch --tui --agent k7m2
zodex local watch --tui --all
```

On macOS, plain `watch` starts the temporary same-origin Liveboard host and opens the browser UI; `--no-open` keeps that host in the foreground. On Linux and Windows, plain `watch` uses the terminal viewer and `--no-open` is not applicable. See [Watch and Liveboard](/docs/local/watch) and [Local observability API](/docs/local/observability-api).

## `zodex local menu`

```bash
zodex local menu
```

On Apple Silicon macOS, this opens the lightweight Zodex menu bar app. Linux and Windows do not provide this macOS-only control surface; use `zodex local start`, `watch`, `status`, and `stop` from your terminal.

**Launch at Login** is enabled by default after `zodex local setup`. Turn that checked menu item off if you no longer want the menu app to return after logout or restart. Choosing **Quit** only exits the current menu app session; it does not start or stop Zodex Local and does not make the app relaunch before the next login.

## `zodex upgrade`

The root upgrade command is also the menu bar's update backend:

```bash
zodex upgrade
zodex upgrade --check
zodex upgrade --check --refresh
zodex upgrade --version 0.3.5
zodex upgrade --stop-local
zodex upgrade --check --format json
```

An already-current upgrade exits before downloading the release archive. Human mode streams progress; JSON mode emits the stable upgrade event stream used by the menu app. `--stop-local` is explicit authorization to stop blocking Local state before installation. Release downloads are pinned to the version resolved by the CLI and checksum-verified before the embedded low-level installer replaces the operator and menu app.

**Start Zodex** enters your configured interactive login shell before it invokes `zodex local start`, so a menu app launched by macOS still captures the normal shell environment used for Homebrew, language toolchains, user-installed CLIs, credentials, and PATH customizations.

## `zodex local history`

```text
zodex local history [OPTIONS] [COMMAND]
```

Options:

```text
--last <N>
--since <DURATION>
--agent <AGENT>
--workdir <ABSOLUTE_PATH>
--id <INVOCATION_ID>
--format <markdown|json>
--raw
```

Examples:

```bash
zodex local history --last 20
zodex local history --since 2h
zodex local history --agent k7m2 --since 1h
zodex local history --workdir /absolute/repo/path
zodex local history --id <invocation-id> --raw
zodex local history --format json
```

### Clear history

```bash
zodex local history clear
zodex local history clear --yes
```

Local must be stopped before clearing history.

## `zodex local config`

### Read settings

```bash
zodex local config get
zodex local config get history.max-age
zodex local config get history.max-size
zodex local config get tunnel.id
zodex local config get tunnel.client-path
zodex local config get context.enabled
zodex local config get context.global-agents
zodex local config get context.repo-agents
zodex local config get context.repo-skills
zodex local config get context.skills.enabled
zodex local config get context.skills.agents
zodex local config get context.skills.codex
zodex local config get context.skills.paths
```

Readable keys:

```text
history.max-age
history.max-size
tunnel.id
tunnel.client-path
context.enabled
context.global-agents
context.repo-agents
context.repo-skills
context.skills.enabled
context.skills.agents
context.skills.codex
context.skills.paths
```

### Change a setting

```bash
zodex local config set history.max-age 60d
zodex local config set history.max-size 500mb
zodex local config set tunnel.id tunnel_<id>
zodex local config set context.enabled false
zodex local config set context.global-agents false
zodex local config set context.repo-agents false
zodex local config set context.repo-skills false
zodex local config set context.skills.enabled false
zodex local config set context.skills.agents true
zodex local config set context.skills.codex false
zodex local config set context.skills.paths '["~/team-skills", "/opt/company/skills"]'
```

`context.*` settings control the Codex-style global `AGENTS`/skills context, per-workdir `AGENTS` hints, and exact-workdir `.agents/skills` catalogs sent to ChatGPT. They can be changed while Local is running. Custom skill paths are additive to the global catalog only; use a TOML array of absolute or `~/` paths.

`history.max-age`, `history.max-size`, and `tunnel.id` remain writable only while Local is stopped. `tunnel.client-path` is inspectable but is managed by setup rather than `config set`.

See [Local configuration](/docs/local/configuration) for the default context behavior, `CODEX_HOME` fallback, skill-root precedence, and `AGENTS.override.md` handling.

## `zodex local logs`

```text
zodex local logs [--lines <LINES>]
```

`--lines` defaults to `200`.

Examples:

```bash
zodex local logs
zodex local logs --lines 500
```

## `zodex local stop`

```bash
zodex local stop
```

It is idempotent; running it when Local is already stopped is safe.

## Shared MCP tools

The CLI above manages/observes Local. ChatGPT itself receives only the shared [three MCP tools](/docs/reference/tools).
