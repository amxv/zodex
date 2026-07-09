---
title: Quickstart
description: "Set up zodex for ChatGPT: install the operator CLI, create a Sprite, install the guest runtime, expose the MCP URL, add it to ChatGPT, and choose a GitHub write mode."
order: 1
category: Start
summary: A no-clone path for installing zodex end to end and connecting ChatGPT to a real Sprite-backed coding workspace.
---

This is the canonical setup document for a local operator or agent helping a user connect ChatGPT to zodex. The goal is to install the local `zodex` operator CLI, create or select a Sprite, install the zodex guest runtime, expose the MCP URL, add it to ChatGPT, and verify GitHub reads plus the selected write mode.

For this project's own deployment and release paths, the canonical repository slug is `amxv/zodex`. The public Git URL is `https://github.com/amxv/zodex.git`; substitute `--repo amxv/zodex` into setup, PR, grant, and YOLO examples when operating this repository itself.

## What you are setting up

You are setting up three layers:

1. a Sprite-backed Linux workspace where ChatGPT can run commands, keep sessions alive, edit files, test, and commit
2. a ChatGPT MCP front door at `/mcp?key=...`
3. a GitHub write policy: PR-only, one-off push approval, remote operator grant, or scoped YOLO mode

Sprites are a good fit for ChatGPT coding sessions because they give agents a real remote machine for bursty coding work while keeping the MCP front door simple to expose.

## Outcome

When setup is complete:

- the user's local machine has the `zodex` operator CLI
- the Sprite has `zodex-agent`, `zodexd`, and `zodex-prd`
- Sprite Services keep `zodexd` and `zodex-prd` running
- the Sprite HTTP URL exposes `/health` and `/mcp`
- ChatGPT has a connector/app pointed at the zodex `/mcp?key=...` URL
- the runtime has read-only GitHub access through a reader app
- ChatGPT can publish PRs through the publisher daemon without direct shell token exposure
- direct `git push` is available only through the write mode the operator chooses

## 1. Install the local operator CLI

Install zodex on the local machine without cloning the repository:

```bash
curl -fsSL https://zodex.ashray.xyz/install.sh | sh
```

The installer detects macOS/Linux and CPU architecture, downloads the matching GitHub Release artifact, verifies its checksum, and installs the operator binary. If `zodex` is not on `PATH` after install, add the printed install directory, usually `~/.local/bin`, to the shell profile.

Verify:

```bash
zodex --version
```

Upgrade later with:

```bash
zodex upgrade
zodex upgrade --version v0.2.17
```

## 2. Install and authenticate the Sprite CLI

Install the Sprite CLI:

```bash
curl -fsSL https://sprites.dev/install.sh | sh
```

Authenticate the user's Sprite account:

```bash
sprite org auth
```

Create and select a Sprite:

```bash
sprite create zodex-dev
sprite use zodex-dev
```

Make the Sprite URL publicly reachable so ChatGPT can connect to the MCP server:

```bash
sprite url update --auth public
sprite url
```

## 3. Create the required GitHub Apps

Create two GitHub Apps. Install both apps on `Only select repositories`, not the whole account/org, unless the user explicitly wants broader access.

### Reader app

Use this for always-on clone/fetch access.

Required settings:

```text
Repository permissions:
  Contents: Read-only
Installation scope:
  Only select repositories
Private key:
  Generate and download PEM
```

Collect these values:

```text
reader_app_id
reader_private_key_pem_path
```

The `zodex sprite setup` command resolves the reader installation ID automatically from the repo slug and app key.

### Writer app

Use this for PR publishing, one-off push grants, and YOLO-backed direct push policy.

Required settings:

```text
Repository permissions:
  Contents: Read & write
  Pull requests: Read & write
Installation scope:
  Only select repositories
User access token expiration:
  Enabled
Device Flow:
  Enabled
Private key:
  Generate and download PEM
```

Collect these values:

```text
publisher_app_id
publisher_client_id
publisher_private_key_pem_path
```

The app ID and PEM are used by the publisher daemon. The client ID is used by device-flow push grant commands such as `zodex-agent github request-push`.

## 4. Install zodex on the Sprite

Run setup from the user's local machine:

```bash
zodex sprite setup \
  --sprite zodex-dev \
  --repo owner/repo \
  --reader-app-id <reader-app-id> \
  --reader-pem /absolute/path/to/reader.pem \
  --publisher-app-id <writer-app-id> \
  --publisher-pem /absolute/path/to/writer.pem \
  --default-base main \
  --url-auth sprite
```

If the Sprite is in a non-default org, add:

```bash
--org <org-name>
```

After setup, add the publisher app client ID to `/etc/zodex/config.toml` on the Sprite or pass it when requesting push access:

```toml
publisher_client_id = "Iv1.real-device-flow-client-id"
```

The setup command:

1. resolves installation IDs for both GitHub Apps
2. validates app access locally
3. uploads the setup script and GitHub App PEMs to the Sprite
4. downloads the public installer on the Sprite and installs the Linux guest runtime with service behavior preserved
5. configures reader-backed Git credentials for the `zodex-agent` user
6. configures the publisher daemon
7. syncs Sprite Services
8. verifies health, workspace writeability, Git identity, and reader-backed repo access

## 5. Verify the Sprite runtime

```bash
zodex sprite status --sprite zodex-dev
zodex sprite logs --sprite zodex-dev --service zodexd --lines 100
zodex sprite health --sprite zodex-dev
zodex proxy inspect --sprite zodex-dev
zodex proxy verify-origin --sprite zodex-dev
```

Expected signs:

- `zodexd` and `zodex-prd` are running
- `/health` returns `{"status":"ok"}`
- the public Sprite URL can reach `/mcp`
- `git ls-remote https://github.com/owner/repo.git HEAD` works from the Sprite through the reader app

If you use the Cloudflare proxy front door, deploy or update it after verifying the Sprite origin:

```bash
zodex proxy inspect --sprite zodex-dev
zodex proxy verify-origin --sprite zodex-dev
cd proxy/cloudflare-worker
# set vars.SPRITE_ORIGIN in wrangler.jsonc first
npx wrangler deploy
```

## 6. Add the MCP server to ChatGPT

Get the public Sprite URL:

```bash
sprite url
```

Get the API key from the Sprite config or ask the Sprite-side helper to print the redacted shape:

```bash
sprite exec -- sudo cat /etc/zodex/config.toml
sprite exec -- sudo -u zodex-agent env HOME=/home/zodex-agent zodex-agent show-url --host <sprite-host>
```

The ChatGPT connector URL should look like:

```text
https://<sprite-host>/mcp?key=<zodex-api-key>
```

In ChatGPT, go to Settings → Connectors / Apps, create a new connector/app, paste the full HTTPS `/mcp?key=...` URL, and choose **No authentication**. The key is already in the URL query parameter.

## 7. First ChatGPT coding workflow

Inside ChatGPT with the zodex MCP connector enabled, the model gets three tools:

```text
exec_command
write_stdin
apply_patch
```

A typical workspace loop:

```bash
cd /workspace
git clone https://github.com/amxv/zodex.git
cd zodex
# inspect, edit, test
git status
git add .
git commit -m "Describe the change"
```

This verifies plain `git clone https://github.com/amxv/zodex.git` works through the reader app before any write path is used.

## 8. Choose a write mode

Review-first PR path:

```bash
zodex-agent github publish-pr \
  --repo amxv/zodex \
  --title "Describe the change" \
  --base main \
  --body "Summary and tests."
```

One-off direct push from inside ChatGPT:

This opens temporary repo-scoped direct push access for the requested repository. The request-push flow opens the GitHub verification URL automatically when possible.

```bash
zodex-agent github request-push --repo owner/repo
# agent pushes normally with git push
zodex-agent github revoke-push --repo owner/repo
zodex-agent github revoke-push --repo owner/repo --forget-local-auth
```

Remote operator grant alternative:

```bash
zodex github grant-push --sprite zodex-dev --repo owner/repo
zodex github revoke-push --sprite zodex-dev --repo owner/repo
```

Trusted YOLO session:

```bash
zodex github mode yolo --sprite zodex-dev --repo owner/repo --ttl 4h
zodex github mode status --sprite zodex-dev
zodex github mode default --sprite zodex-dev
```

The default active grant TTL is `30m`. `request-push` defaults to a `30m` TTL and does not persist refresh-token state unless `--cache-refresh-token` is explicitly requested. `mode yolo` defaults to a `2h` TTL and all installed repositories unless one or more `--repo` entries are provided. Change either window with `--ttl <duration>`. Repo-scoped YOLO grants merge with other active repo grants and expire independently. Expired grants stop working in the credential-helper path. Both flows can use `--no-ttl` when the operator intentionally wants an indefinite window.

For a full comparison, see [Write modes](/docs/write-modes).

## Day-to-day commands

```bash
zodex sprite status --sprite zodex-dev
zodex sprite logs --sprite zodex-dev --service zodexd --lines 100
zodex sprite sync --sprite zodex-dev --force-recreate
zodex sprite upgrade --sprite zodex-dev
zodex-agent github list-grants
zodex-agent github publish-pr --repo owner/repo --title "Title"
zodex github mode status --sprite zodex-dev
```

## Migration notes

If you are migrating an older pre-`zodex` Sprite rather than doing a clean install, check these before debugging the new runtime:

- remove or disable legacy `computer-mcpd` and `computer-mcp-prd` Sprite Services so `zodexd` can claim its ports cleanly
- migrate any old `/etc/computer-mcp` repo target references to the current repo slug expected by `/etc/zodex/config.toml`
- verify `/var/lib/zodex/publisher` is writable by the configured publisher user before expecting `zodex-prd` to start
- if TLS artifacts do not exist yet, run the TLS setup path and then re-sync Sprite Services

## Stop conditions

Stop and ask the user before continuing if:

- the reader app has permissions beyond `Contents: Read-only`
- the writer app has permissions beyond `Contents: Read & write` and `Pull requests: Read & write`
- either app is installed on more repositories than intended
- `zodexd` cannot bind after setup
- token minting validation fails
- the user is unsure which GitHub account or organization should own the apps
