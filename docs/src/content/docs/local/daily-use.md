---
title: "Daily use"
description: "Start, inspect, observe, and stop Zodex Local while one or more ChatGPT conversations work on your trusted Linux, macOS, or Windows host."
order: 3
category: Local
summary: "The practical Local workflow for runtime TTLs, status, Agents, watch, durable history, logs, and clean shutdown."
---

Once [Local setup](/docs/local/setup) is complete, use the CLI on any supported platform. macOS also provides a native menu bar app, while Windows and graphical Linux desktops offer a lightweight Rust system tray.

On all supported graphical desktops, `zodex local setup` enables the menu bar or system tray app by default. It opens automatically at your next login, while Zodex Local itself remains stopped until you explicitly start it unless you opt into tunnel autostart. If you opted out during setup, you can still open the app manually:

```bash
zodex local menu
```

The default Start Folder is your home directory, so you can use **Start Zodex** immediately after setup. Existing custom Start Folders remain unchanged. The main menu offers **Start Zodex**, **Stop Zodex**, **Open Liveboard**, **Copy Liveboard Link**, **Configure**, and **Quit Menu Bar** (or **Quit Tray App**). Hover over **Configure** for the current folder, **Change Start Folder**, **Set Liveboard Port**, **Check for Updates**, **Automatic Updates (Daily)**, and login settings. Quit exits only the convenience app; it does not stop the Local tunnel.

Automatic updates are on by default. While the menu bar/tray is running it checks for releases once per 24 hours and installs updates when Local is stopped. A running or stale Local tunnel is never stopped automatically for an update: the update waits until Local is stopped. You can switch off **Automatic Updates (Daily)** in Configure or run `zodex local preferences automatic-updates false`. The Mac app refreshes runtime status when you open its menu; the Windows/Linux tray continues polling status every two seconds, or every half-second during lifecycle transitions.

### Launch the tunnel automatically at login (optional)

Two separate checkboxes now appear in each desktop menu. **Launch Menu Bar at Login** (Mac) or **Launch Tray at Login** (Windows/Linux) controls whether the convenience app returns after a restart. **Launch Tunnel at Login**, immediately below it, is an independent opt-in that starts Zodex Local using the saved Start Folder whenever that app launches at a future desktop login. It's off by default, so existing installations keep their original behavior. Enabling it also enables the menu/tray login item; disabling the menu/tray login item disables tunnel autostart.

The same settings are available in any terminal, without repeating setup:

```bash
zodex local autostart enable /absolute/workspace/path
zodex local autostart status
zodex local autostart disable
```

When enabled, Local starts in the background through the normal CLI with the same permissions and lifetime rules. Disabling the setting does not stop the running tunnel; it only prevents the next automatic login launch. Changes to the startup folder or the checkboxes are persisted. Headless Linux remains CLI-controlled rather than starting a desktop tray.

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

On Linux, macOS, or Windows, open the first-party browser Liveboard:

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

The release checksum is verified before installation. Your Local configuration, platform credential storage, durable history, and platform-specific viewer/control preferences are preserved. On macOS and Windows, if the menu/tray app is open, the upgrade restarts it into the new version; if intentionally quit, it remains quit. On Linux, the new tray executable is installed without changing the running Local runtime and takes effect when the tray next starts.

**Upgrading Windows from v0.5.6 to v0.5.8 or later:** rerun the regular PowerShell installer instead of using `zodex upgrade` for this one transition:

```powershell
irm https://zodex.ashray.xyz/install.ps1 | iex
```

The v0.5.6 self-updater copied only the CLI executable, so it cannot install a brand-new tray. The current installer checks the release archive, installs the missing tray, enables Launch at Login, and opens the tray immediately. It **does not** rerun Local setup or start the Local runtime. If Local is currently running, stop it before replacing the operator. Later upgrades preserve a user's decision to disable Launch at Login or quit the tray.

The menu bar and tray use this same CLI upgrade contract. They show **Update to v…** when one is available and offer **Stop and Update** only when the CLI reports that Local is blocking the update.

## What happens on logout or reboot?

Local is not an auto-start login service. A later login/reboot requires another explicit:

```bash
zodex local start
```

Or simply click **Start Zodex** in the menu bar/system tray, which returns automatically at login unless you disabled that preference.

That is intentional: access is something you turn on, optionally give a TTL, and turn off.

## Next

- [Watch and Liveboard](/docs/local/watch)
- [Local observability API](/docs/local/observability-api)
- [Local command reference](/docs/local/command-reference)
- [Local configuration](/docs/local/configuration)
- [Local troubleshooting](/docs/local/troubleshooting)
