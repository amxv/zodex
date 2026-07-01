---
title: Proxy and MCP front door
description: Expose the Sprite-hosted zodexd server through a proxy-backed MCP URL that normalizes paths, warms cold Sprites, retries edge failures, and preserves streaming responses.
order: 9
category: Operations
summary: How zodex serves `/mcp`, `/health`, and direct HTTP routes through the Sprite origin and Cloudflare Worker proxy.
---

## Public entry point

zodex assumes a proxy-backed public MCP front door for Sprite deployments. The proxy is preferred when the raw Sprite URL is not reliable enough for MCP clients on its own.

The important routes are:

```text
/health -> Sprite /health
/mcp    -> Sprite /mcp/
/mcp/   -> Sprite /mcp/
```

`zodexd` itself serves `/health`, `/mcp`, `/mcp/`, and the `/v1/*` HTTP API routes.

## Verify the Sprite origin

Before deploying or updating the proxy:

```bash
zodex proxy inspect --sprite dev-sprite
zodex proxy verify-origin --sprite dev-sprite
```

`verify-origin` checks the raw Sprite URL behavior so the operator can tell whether the proxy is still required for a given deployment.

## Deploy the Cloudflare Worker

The Worker lives in `proxy/cloudflare-worker`:

```bash
cd proxy/cloudflare-worker
```

Set `vars.SPRITE_ORIGIN` in `wrangler.jsonc` to the public Sprite URL you want to front. Then deploy:

```bash
npx wrangler deploy
```

The operator CLI also has a proxy deploy path:

```bash
zodex proxy deploy --sprite dev-sprite
```

The `deploy` command also has an `update` alias.

## MCP authentication

The MCP transport uses a query parameter named `key`:

```text
https://dev-zodex.example.net/mcp?key=secret-runtime-key
```

The key must match `api_key` in `/etc/zodex/config.toml`. Treat the full URL as a secret.

## Why the proxy normalizes `/mcp`

The MCP transport root is internally rewritten so both `/mcp` and `/mcp/` reach the streamable HTTP service root. This avoids client-specific trailing slash behavior and gives operators a stable URL to hand to ChatGPT or another MCP client.
