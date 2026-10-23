---
title: "Sprite"
description: "Run ChatGPT on a wake-on-demand remote Linux Sprite through Zodex's canonical Cloudflare Worker front door."
order: 1
category: Sprite
summary: "Install the operator, create a Sprite and two GitHub Apps, run one setup command, claim/auth Cloudflare if needed, and connect ChatGPT."
---

Sprite is Zodex's **wake-on-demand remote Linux** mode. It exposes the same three **execution** MCP tools as [Zodex Local](/docs/local), without Local's optional Liveboard alias tool, and uses a different lifecycle and trust boundary:

```text
ChatGPT custom app
  → Cloudflare Worker
  → public Sprite HTTPS wake edge
  → zodexd
  → restricted zodex-agent workspace
```

The Worker is the supported ChatGPT front door. The raw Sprite URL is kept public so the Worker can wake and reach the Sprite; it is an upstream origin, **not** the connector URL you should register in ChatGPT.

## Before you start

You need:

- the Zodex operator CLI on Apple Silicon macOS or Linux (x86_64/aarch64);
- the Sprite CLI, authenticated to the account/org that will own the Sprite;
- two user-owned GitHub Apps for the repositories ChatGPT should access;
- a Wrangler-capable operator environment (`wrangler`, `bunx`, or `npx`) for the Cloudflare Worker.

Most OpenAI paid plans support custom MCP servers.

## 1. Install the operator and Sprite CLI

```bash
curl -fsSL https://zodex.ashray.xyz/install.sh | bash
export PATH="$HOME/.local/bin:$PATH"
curl -fsSL https://sprites.dev/install.sh | sh

sprite login
zodex --version
sprite --version
```

The normal non-root Zodex install uses `~/.local/bin`; add the PATH line above to your shell profile to keep it available in new terminals. An explicitly root-run Zodex install uses `/usr/local/bin` instead.

Create the remote workspace:

```bash
sprite create zodex-dev
sprite info zodex-dev
```

You do **not** start the Sprite manually. Operator/HTTP work wakes it, and it can sleep again when idle.

## 2. Create the two GitHub Apps

Keep App creation and installation user-owned so the repository scope is an explicit security decision.

**Reader App** — always-on clone/fetch:

```text
Repository permissions:
  Contents: Read-only
Installation:
  Only select repositories
Private key:
  Generate/download PEM
```

Keep its **App ID** and PEM path.

**Writer App** — PR publishing, grants, and YOLO-backed push:

```text
Repository permissions:
  Contents: Read & write
  Pull requests: Read & write
  Workflows: Read & write
Device Flow:
  Enabled
Installation:
  Only select repositories
Private key:
  Generate/download PEM
```

Keep its **App ID**, **Client ID**, and PEM path. Device Flow is required for the CLI push-grant flow; the Client ID is different from the App ID. See [GitHub Apps](/docs/sprite/github-apps) for the detailed checklist.

## 3. Run one Sprite setup command

```bash
zodex sprite setup \
  --sprite zodex-dev \
  --repo owner/repo \
  --reader-app-id <reader-app-id> \
  --reader-pem /absolute/path/to/reader.pem \
  --publisher-app-id <writer-app-id> \
  --publisher-client-id <writer-client-id> \
  --publisher-pem /absolute/path/to/writer.pem
```

If the Sprite belongs to a non-default org, add `--org <org-name>`.

Before remote mutation, setup validates both App identities/installations, checks the reader's repository access, checks the writer permissions, and preflights Device Flow. It then installs only the guest runtime, configures reader/publisher isolation, reconciles Sprite Services, makes the raw Sprite wake origin public, verifies the runtime, and brings up the Worker.

There is no follow-up manual `publisher_client_id` edit.

## 4. Finish the Worker deployment

If Wrangler is already authenticated and a Cloudflare account is unambiguous, setup records a permanent Worker automatically.

On a first unauthenticated run, Wrangler can create a **temporary** Worker. Zodex prints its live URL plus a one-time Cloudflare claim URL. Treat the claim URL as a bearer secret and claim it within **60 minutes**. Zodex never stores it, although Wrangler may cache temporary provider state in its own global config directory.

After claiming, establish normal Wrangler authentication and register a permanent deployment:

```bash
wrangler login --use-keyring
zodex sprite proxy deploy --sprite zodex-dev
```

If Wrangler reports multiple eligible Cloudflare accounts, choose explicitly:

```bash
zodex sprite proxy deploy \
  --sprite zodex-dev \
  --cloudflare-account <id-or-name>
```

Zodex embeds the Worker source/config in the released operator and materializes a temporary deploy project. **Do not clone Zodex or edit a Wrangler file just to deploy the front door.**

## 5. Verify and connect

```bash
zodex sprite health --sprite zodex-dev
zodex sprite proxy status --sprite zodex-dev
zodex sprite proxy verify --sprite zodex-dev
zodex sprite connect --sprite zodex-dev
```

`connect` validates the registered Worker, reads the remote MCP key deliberately, and copies the full secret endpoint to the clipboard. Use `--show-url` only when you intentionally want the capability URL printed.

In ChatGPT web:

1. enable Developer Mode for custom apps in **Settings → Apps** (or the workspace equivalent);
2. create a custom app;
3. paste the endpoint from `zodex sprite connect`;
4. do not copy the embedded Zodex key into an unrelated authentication field;
5. scan the tools, confirm `exec_command`, `write_stdin`, and `apply_patch`, then create the app.

OpenAI's current custom-app guide documents OAuth when authentication is used but does not document a static custom-header/API-key field. Zodex therefore keeps its current secret query capability in the endpoint URL.

## 6. Work normally

Inside the Sprite, ChatGPT can clone, edit, test, and commit with ordinary Git. GitHub writes remain a separate policy boundary:

```bash
# Review-first PR
zodex-agent github publish-pr --repo owner/repo --title "Ship the change"

# Agent asks for temporary direct push
zodex-agent github request-push --repo owner/repo

# Human/operator opens a repo grant
zodex sprite github grant-push --sprite zodex-dev --repo owner/repo

# Human/operator opens scoped YOLO
zodex sprite github yolo --sprite zodex-dev --repo owner/repo --ttl 2h
```

See [Write modes](/docs/sprite/write-modes) and [Permissions and autonomy](/docs/sprite/permissions).

## Day-to-day operations

```bash
zodex sprite status --sprite zodex-dev
zodex sprite health --sprite zodex-dev
zodex sprite logs --sprite zodex-dev --service zodexd --lines 100
zodex sprite restart --sprite zodex-dev
zodex sprite upgrade --sprite zodex-dev
```

`restart` repairs the managed Zodex service stack; it is **not** Sprite VM power control. `sync` is an advanced desired-state reconciliation command for setup/upgrade/recovery.

## Security notes

- The raw Sprite origin is public for wake/origin reachability, but `/mcp` still rejects a missing or wrong Zodex key.
- The capability URL is encrypted in transit by HTTPS; the risk is that a full URL containing a secret can leak through terminal history, logs, screenshots, or copied text. Treat the whole URL as a secret.
- Routine status, health, logs, and proxy diagnostics redact the MCP key. `zodex sprite connect` is the deliberate reveal/copy path.
- The writer PEM stays owned by `zodex-publisher` and must remain unreadable by `zodex-agent`.

## Next guides

- [Connect ChatGPT](/docs/sprite/connect)
- [GitHub Apps](/docs/sprite/github-apps)
- [Permissions and autonomy](/docs/sprite/permissions)
- [Write modes](/docs/sprite/write-modes)
- [Operations](/docs/sprite/operations)
- [Troubleshooting](/docs/sprite/troubleshooting)
