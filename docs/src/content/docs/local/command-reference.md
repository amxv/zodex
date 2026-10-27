---
title: "Command reference"
description: "A compact reference for every public zodex local command, argument, option, and common example."
order: 7
category: Local
summary: "The public Local CLI: setup, start, status, watch, history, config, logs, stop, plus cross-platform menu/tray controls."
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

On macOS, setup enables and opens the lightweight Zodex Swift menu bar app by default. It is registered to return when you next log in, but it does not start the Zodex Local runtime.

On Linux, setup stores the runtime key in a mode-`0600` user-only credential file, installs the Linux tunnel-client bundle, and enables a Rust D-Bus status notifier tray for graphical desktop logins by default. A headless setup never attempts to open a tray but still registers it for a future desktop login.

On Windows, setup stores the runtime key in Windows Credential Manager, installs the Windows tunnel-client bundle, and enables the Rust notification-area tray for login by default. **All three platforms** support `--no-menu-bar` (alias `--no-tray`) to skip enabling/opening the control surface during setup.

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
zodex local watch [OPTIONS] [COMMAND]
```

Options:

```text
--tui            use the terminal viewer instead of the default web Liveboard
--agent <AGENT>  focus one four-character Agent ID in web or TUI mode
--all            in TUI mode, combine all Agents
```

Commands:

```text
url      print the stable Liveboard URL
copyurl  copy the stable Liveboard URL to the system clipboard
```

`url` and `copyurl` cannot be combined with `--tui`. `--agent` cannot be combined with `--all`. `--all` requires `--tui`.

Examples:

```bash
zodex local watch
zodex local watch url
zodex local watch copyurl
zodex local watch --agent k7m2
zodex local watch url --agent k7m2
zodex local watch --tui
zodex local watch --tui --agent k7m2
zodex local watch --tui --all
```

On Linux, macOS, and Windows, plain `watch` opens the runtime-owned same-origin Liveboard in the default browser. See [Watch and Liveboard](/docs/local/watch) and [Local observability API](/docs/local/observability-api).

## `zodex local menu`

```bash
zodex local menu
```

On Apple Silicon macOS, this opens the Swift menu bar app. On Windows and graphical Linux desktops, it opens the Rust system tray. On headless Linux, continue using the CLI.

**Launch Menu Bar at Login** (macOS) or **Launch Tray at Login** (Windows/Linux) is enabled by default after `zodex local setup`. Only the lightweight control app opens; the Local tunnel stays stopped unless you also enable **Launch Tunnel at Login**. The latter is off by default, uses your saved Start Folder, and takes effect at the next login. Enabling tunnel autostart automatically enables the menu/tray login item. Turning off the menu/tray login item disables tunnel autostart too.

The main menu groups non-lifecycle controls in **Configure**. Choosing **Quit Menu Bar** or **Quit Tray App** only exits the convenience app; it does not change the Local runtime or the saved login settings. On fresh installs, Start Folder defaults to the user's home directory without additional setup; an explicitly selected folder is never overwritten.

## `zodex local autostart`

Read and change the same persistent **Launch Tunnel at Login** setting available in the menu/tray on all three desktop platforms:

```bash
zodex local autostart status
zodex local autostart status --json
zodex local autostart enable
zodex local autostart enable /absolute/workspace/path
zodex local autostart folder /absolute/workspace/path
zodex local autostart disable
```

`enable` uses the last saved Start Folder, or the user's home directory if none was configured. Passing an explicit folder also saves that location for subsequent login launches and manual Start actions. The folder must exist and be accessible. `folder` changes that location without enabling autostart. `disable` stops **future automatic login starts** but does not stop an already-running Local runtime, and leaves **Launch Menu Bar at Login** or **Launch Tray at Login** as-is.

### Desktop settings

In **Configure** on macOS, Windows, or graphical Linux, use **Change Liveboard Port** (network icon) to change the loopback port and **Automatic Updates (Daily)** to turn daily checks/automatic installation on or off. The matching terminal commands are:

```bash
zodex local preferences liveboard-port 65000
zodex local preferences liveboard-port 64973 # restore the default
zodex local preferences automatic-updates true
zodex local preferences automatic-updates false
```

Ports must be between `1024` and `65535`. Changing the port requires restarting Zodex Local; the current runtime keeps its original Liveboard URL until then. Automatic update checks run once per 24 hours while the desktop app is open and default to enabled. They can update a running tunnel **only after 15 minutes without active processes or agent tool activity**. Downloads and checksums are verified before stopping; then Local restarts in its previous directory with the remaining TTL. An offline or busy check retries once on the next computer restart (not merely on app relaunch), or at the next daily check.

The setting persists in the user's non-secret Zodex desktop preferences. The menu/tray observes terminal changes without requiring another setup, and on your next graphical login it uses the same `zodex local start` lifecycle, credentials, and runtime behavior as a manual Start. Headless Linux does not automatically start a desktop tray; use an appropriate external service manager if you need headless boot-time operation.

## `zodex upgrade`

The root upgrade command is also the menu bar/tray update backend:

```bash
zodex upgrade
zodex upgrade --check
zodex upgrade --check --refresh
zodex upgrade --version 0.3.5
zodex upgrade --stop-local
zodex upgrade --check --format json
```

An already-current upgrade exits before downloading the release archive. Human mode streams progress; JSON mode emits the stable upgrade event stream used by the menu app. `--stop-local` is explicit authorization to stop blocking Local state before installation. Release downloads are pinned to the version resolved by the CLI and checksum-verified before the embedded low-level installer replaces the operator and menu app.

**Start Zodex**, whether clicked manually or launched at login when enabled, captures the configured interactive login shell on macOS/Linux before invoking `zodex local start`, so the menu/tray sees language toolchains, user-installed CLIs, credentials, and PATH customizations. Windows inherits the logged-in user's environment.

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
# Delete just one Agent's durable records (safe with Local running if inactive).
zodex local history clear --agent k7m2
zodex local history clear --agent k7m2 --yes

# Delete every Agent's history (Local must be stopped).
zodex local history clear
zodex local history clear --yes
```

Use `--agent` to remove only that Agent's invocations, output chunks, file evidence, presentations, identity, and provider-specific context. Other Agents' records are preserved. The Agent must have no unfinished invocations or running processes recorded in history. A successful deletion compacts SQLite and truncates its write-ahead log. `--yes` skips the interactive confirmation.

**Without `--agent`, `clear` deletes the entire history database** and requires Local to be stopped. Do not use `--last`, `--since`, `--workdir`, `--id`, or `--raw` with `clear`; these query filters never limit deletion.

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

The CLI above manages/observes Local. ChatGPT receives the [three shared execution tools and the Local-only alias tool](/docs/reference/tools).
