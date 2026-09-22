---
title: "Setup"
description: "Provision the OpenAI Secure MCP Tunnel, create a least-privilege runtime key, run zodex local setup, and add the tunnel to ChatGPT."
order: 2
category: Local
summary: "The complete one-time setup path for Zodex Local, from OpenAI Platform tunnel settings through a developer-mode ChatGPT app."
---

Local connects ChatGPT through the OpenAI Secure MCP Tunnel. You do **not** need a Sprite proxy, Cloudflare Worker, public inbound port, or separate proxy URL for Local.

This guide takes a supported macOS or Windows machine from “Zodex is installed” to “ChatGPT can call the three Zodex tools through an OpenAI Secure MCP Tunnel.”

For the shorter path, start with [Local](/docs/local).

OpenAI references: [Secure MCP Tunnel](https://developers.openai.com/api/docs/guides/secure-mcp-tunnels) and [developer mode / MCP apps in ChatGPT](https://help.openai.com/en/articles/12584461-developer-mode-and-mcp-apps-in-chatgpt).

## Before you start

You need:

- an Apple Silicon Mac or x86_64 Windows machine;
- the Zodex operator CLI;
- an OpenAI Platform account/organization that can create or access Secure MCP Tunnels;
- a ChatGPT workspace/account with developer-mode custom MCP access;
- permission to associate the tunnel with the ChatGPT workspace that will use it.

OpenAI Platform tunnel permissions and ChatGPT developer-mode permissions are separate. Being able to create a tunnel does not automatically give you permission to add it as a ChatGPT app, and vice versa.

## Install the operator CLI

macOS:

```bash
curl -fsSL https://zodex.ashray.xyz/install.sh | bash
export PATH="$HOME/.local/bin:$PATH"
```

Windows PowerShell:

```powershell
irm https://zodex.ashray.xyz/install.ps1 | iex
```

Verify:

```bash
zodex --help
zodex local --help
```

On macOS, the normal non-root install uses `~/.local/bin`. On Windows, the PowerShell installer uses `%LOCALAPPDATA%\Programs\Zodex` by default and adds it to the user's PATH.

The Local runtime is part of the `zodex` operator binary. You do not install a separate `zodexd` service on the Local host.

## Create the tunnel in OpenAI Platform

Go to:

```text
https://platform.openai.com/settings/organization/tunnels
```

Sign in with the same OpenAI account/email you use for ChatGPT. For a personal account, use its personal Platform organization. For a workspace account, make sure the tunnel is associated with the ChatGPT workspace that should use Zodex.

In Platform tunnel settings:

1. select the intended Platform organization;
2. create a new tunnel;
3. associate the tunnel with the target ChatGPT workspace where applicable;
4. copy the generated `tunnel_id`.

The ID looks like:

```text
tunnel_0123456789abcdef0123456789abcdef
```

### Permissions while creating the tunnel

OpenAI's current permission model distinguishes administration from runtime use:

- **Tunnels Read + Manage** — create, edit, or delete a tunnel;
- **Tunnels Read + Use** — run a tunnel client or select the tunnel when creating an app.

Tunnel permissions are organization-level Platform permissions rather than ordinary project-level model/API permissions.

You may need Read + Manage while provisioning the tunnel. The runtime key you give to Zodex should use the narrower Read + Use permission.

## Create a least-privilege runtime API key

Create a scoped Platform API key for the runtime and grant only:

```text
Tunnels: Read
Tunnels: Use
```

The runtime key does not need Tunnels Manage and does not need an OpenAI admin key.

Copy the key when OpenAI shows it. You will enter it into `zodex local setup` once; Zodex stores it in macOS Keychain or Windows Credential Manager.

## Run `zodex local setup`

Interactive setup is recommended for a person at the terminal:

```bash
zodex local setup
```

You will be prompted for:

- tunnel ID;
- tunnel runtime API key.

On success, Zodex:

- validates that the runtime key can read the selected tunnel;
- stores the runtime key in macOS Keychain or Windows Credential Manager;
- downloads and checksum-verifies the supported OpenAI tunnel client bundle;
- saves the non-secret tunnel configuration;
- creates the automatically managed Local observability credential;
- on macOS, enables and opens the lightweight Zodex menu bar app so it returns at future logins;
- exits without leaving the Local runtime or tunnel running.

Setup is idempotent, so running it again is also the repair/update path.

The macOS menu app is only a control surface. Windows uses the CLI and terminal viewer instead. Enabling the macOS app at login does **not** start Zodex Local, the tunnel, or any Agent process. If you do not want the menu app on macOS, opt out during setup:

```bash
zodex local setup --no-menu-bar
```

If you enable it initially and change your mind later, turn off **Launch at Login** from the Zodex menu. Choosing **Quit** exits it for the current login session without changing the Zodex Local runtime.

## Scripted setup without putting the key on argv

### Read the key from stdin

```bash
printf '%s\n' "$OPENAI_TUNNEL_RUNTIME_KEY" \
  | zodex local setup \
      --tunnel-id tunnel_<id> \
      --runtime-key-stdin
```

### Read the key from an environment variable

```bash
zodex local setup \
  --tunnel-id tunnel_<id> \
  --runtime-key-env OPENAI_TUNNEL_RUNTIME_KEY
```

### Read the key from an open file descriptor

```bash
zodex local setup \
  --tunnel-id tunnel_<id> \
  --runtime-key-fd <FD>
```

The secret itself is deliberately not a CLI argument.

### Rotate the Local observer credential

This is rarely needed:

```bash
zodex local setup --rotate-observability-bearer
```

Zodex manages that credential automatically; normal users never need to copy it into ChatGPT.

## Start Local

From the repository you want ChatGPT to begin with:

```bash
cd ~/code/my-project
zodex local start
```

Or:

```bash
zodex local start ~/code/my-project --ttl 4h
```

Success means Zodex has started the Local runtime and the managed OpenAI tunnel and reached its readiness checks.

Check it with:

```bash
zodex local status
```

## Enable developer mode in ChatGPT

OpenAI's UI can change, so use the path shown in your current ChatGPT settings.

Current OpenAI guidance uses **Settings → Apps** and developer mode / custom apps. Most OpenAI paid plans support custom MCP servers.

## Create the Zodex app in ChatGPT

With Local running:

1. open ChatGPT on the web;
2. go to **Settings → Apps → Create** or the developer-mode Plugins page;
3. create a new developer-mode app;
4. choose **Tunnel** under Connection;
5. select the tunnel from the list, or paste the `tunnel_...` ID;
6. scan the tools;
7. create/save the app.

The tool scan should show exactly:

```text
exec_command
write_stdin
apply_patch
```

If you see a lifecycle, history, GitHub, tunnel, or configuration tool, you are not looking at the intended Zodex MCP surface.

## Test the connection

Open a fresh chat, select the Zodex app, and try a harmless inspection prompt such as:

> Inspect the repository Zodex Local started from and tell me the current branch and worktree status.

The Local runtime tells ChatGPT the start directory as suggested workdir guidance. The actual command tool call still supplies an explicit absolute workdir.

Then inspect the activity locally:

```bash
zodex local watch
```

or:

```bash
zodex local history --last 20
```

## If the tunnel is not listed in ChatGPT

Check these separately:

1. **Platform organization** — you created the tunnel in the expected organization.
2. **Tunnel association** — the target ChatGPT workspace is associated with the tunnel.
3. **Tunnel permissions** — the app creator/runtime identity has Tunnels Read + Use.
4. **ChatGPT developer mode** — the target account has permission to create/use custom apps.
5. **Local runtime** — `zodex local status` says the tunnel is ready.

If the tunnel belongs to a different Platform organization or ChatGPT workspace than the one you are using, it may not appear in ChatGPT. Check the tunnel's organization/workspace association first.

## macOS privacy is separate from tunnel setup

Tunnel setup does not grant filesystem privacy permissions. Local commands run as your user, but macOS can still protect Desktop, Documents, Downloads, iCloud Drive, other-app data, removable volumes, and similar resources.

If a command receives a macOS permission error, use normal **System Settings → Privacy & Security** controls. Do not disable or bypass TCC.

## Next

- [Local daily use](/docs/local/daily-use)
- [Local configuration](/docs/local/configuration)
- [Local troubleshooting](/docs/local/troubleshooting)
