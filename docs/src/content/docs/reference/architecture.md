---
title: "Architecture"
description: "Understand Zodex's two first-class execution modes, their different trust/connection models, and the three-tool MCP contract they share."
order: 1
category: Reference
summary: "Local is trusted direct macOS/Windows execution through OpenAI Secure MCP Tunnel; Sprite is wake-on-demand remote Linux through the canonical Cloudflare Worker."
---

Zodex has two first-class ways to give ChatGPT a real coding machine:

| | Local | Sprite |
| --- | --- | --- |
| Machine | Your Apple Silicon Mac or x86_64 Windows machine | Wake-on-demand remote Linux Sprite |
| Trust model | Trusted host; commands run as your logged-in user | Restricted agent account plus isolated GitHub writer boundary |
| ChatGPT connection | OpenAI Secure MCP Tunnel | Canonical Cloudflare Worker → public Sprite wake edge |
| GitHub permissions | Your existing local user/network credentials | Reader App + isolated writer App + grants/YOLO policy |
| Lifecycle | Explicit local runtime, optional TTL | Automatic sleep/wake; no user start/stop state |
| Observability | Durable Agent history + live observer/API | Sprite Service/Worker/operator diagnostics |

Choose the host/trust model first. The model-facing coding contract is intentionally the same.

## Shared MCP surface

ChatGPT receives exactly:

```text
exec_command
write_stdin
apply_patch
```

Every `exec_command` and `apply_patch` call names an explicit absolute existing `workdir`. There is no ambient/default model-visible working-directory fallback.

The shared service/session layer provides interactive commands (PTY-backed on Unix and pipe-backed on Windows), long-running process handles, bounded output, process-tree cleanup, and Codex-style patch application. See [MCP tools](/docs/reference/tools).

## Local

```text
ChatGPT custom app
  → OpenAI Secure MCP Tunnel
  → loopback Zodex Local runtime
  → your logged-in macOS or Windows user
```

Local is intentionally **trusted-host execution**, not a sandbox. It inherits your host-user filesystem permissions, developer environment, and credentials. macOS privacy controls or Windows filesystem/profile permissions remain authoritative.

One Local runtime can serve several independent ChatGPT conversations. Agent-aware history/observation groups activity for understanding, not for permission isolation.

Local also exposes a separate authenticated read-only localhost observability API. It owns the canonical Agent/presentation timeline, durable output/audit resources, and live SSE contract independently of the MCP execution listener.

On macOS, `zodex local watch` starts the first-party Liveboard by default. On Windows, it starts the terminal viewer. Both consume the same read-only observer contract; macOS can explicitly select the terminal presentation with `--tui`.

Read [Local](/docs/local), [Daily use](/docs/local/daily-use), [Watch and Liveboard](/docs/local/watch), and [Local observability API](/docs/local/observability-api).

## Sprite

```text
ChatGPT custom app
  → Cloudflare Worker
  → public Sprite HTTPS wake edge
  → plain-HTTP zodexd Sprite Service
  → restricted zodex-agent workspace
```

The **Cloudflare Worker is the supported ChatGPT front door**. It performs idempotent wake/readiness work before forwarding to the raw Sprite origin. A dispatched MCP request is sent upstream at most once; the Worker never blindly replays a possibly side-effecting tool call.

The raw Sprite URL must remain public so the Worker can wake/reach it. That does not make Zodex execution public: `/mcp` still requires the secret Zodex query capability. Users connect with `zodex sprite connect`, which validates the registered Worker and deliberately copies/reveals the capability endpoint.

Inside the guest, the runtime consists of:

- `zodex-agent` — restricted Agent-side GitHub helper;
- `git-remote-zodex` — direct-push remote helper;
- `zodexd` — MCP server on the Sprite Service HTTP port;
- `zodex-prd` — isolated publisher/writer service.

The operator `zodex` binary stays on the user's machine.

## Sprite wake lifecycle

There is no normal manual Sprite start/stop workflow. Incoming HTTP/operator activity wakes the environment, and the provider can suspend it again when idle.

- `zodex sprite restart` restarts the managed Zodex services only.
- `zodex sprite sync` reconciles desired Sprite Service definitions.
- `zodex sprite upgrade` replaces/restarts the remote runtime.
- root `zodex upgrade` upgrades only the local operator.

## Sprite GitHub boundary

Two user-owned GitHub Apps keep read and write authority separate:

```text
reader App
  └─ Contents: Read-only

writer App / zodex-prd
  ├─ Contents: Read & write
  ├─ Pull requests: Read & write
  └─ Workflows: Read & write
```

The writer App also has Device Flow enabled for push-grant workflows. Its PEM is private to `zodex-publisher`; writer installation tokens stay inside the publisher boundary.

A direct push still needs exact repository authorization through an explicit grant or active YOLO policy **and** writer installation/target coverage. `default` removes YOLO policy without deleting unrelated explicit grants.

See [Permissions and autonomy](/docs/sprite/permissions) and [Write modes](/docs/sprite/write-modes).

## Sprite Worker deployment boundary

The released operator embeds the tiny Worker source/config and materializes it in a temporary directory for Wrangler. Users do not need a Zodex checkout or a hand-maintained Wrangler project.

Permanent deployments use explicit Cloudflare account identity recorded as non-secret operator metadata. First unauthenticated setup may use Wrangler's temporary deployment/claim flow; Zodex surfaces the claim URL once but never persists it.

## Credentials are deliberately separate

Do not conflate these credentials:

- OpenAI Secure MCP Tunnel runtime key — Local transport;
- Local loopback/tunnel token — Local private MCP listener;
- Sprite `?key=` capability — Sprite MCP authorization;
- Cloudflare auth/claim state — Worker deployment;
- reader App PEM — GitHub read path;
- writer App PEM/tokens — publisher path;
- repo grant/YOLO policy — direct-push authorization.

Each belongs to a different boundary and should stay out of unrelated logs/configuration.

## Advanced protocol notes

The shared server foundation uses RMCP 3.x and accepts modern stateless MCP requests. Provider correlation metadata is consumed outside model-visible tool arguments, so bookkeeping does not expand the three-tool schema.
