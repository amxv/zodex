# zodex Cloudflare Worker proxy

This Worker is a supported `zodex` component.

It fronts a Sprite-hosted `zodexd` deployment with a stable public MCP edge when the raw Sprite URL is not reliable enough for MCP clients on its own.

For this repository, pair it with the `amxv/zodex` release and setup flow.

## Responsibilities

- normalize `/mcp` to the Sprite origin's working `/mcp/` upstream path
- warm the Sprite before proxying requests
- retry transient cold-start and edge failures
- preserve streamed MCP responses

## Routes

- `/health` -> `${SPRITE_ORIGIN}/health`
- `/mcp` -> `${SPRITE_ORIGIN}/mcp/`
- `/mcp/` -> `${SPRITE_ORIGIN}/mcp/`

## Deploy

Deploy the Worker directly with Wrangler from this directory:

```bash
cd proxy/cloudflare-worker
```

Set `vars.SPRITE_ORIGIN` in `wrangler.jsonc` to the public Sprite URL you want to front, then deploy:

```bash
npx wrangler deploy
```

## Inspect

```bash
zodex proxy inspect --sprite <sprite>
zodex proxy verify-origin --sprite <sprite>
```

`zodex proxy verify-origin` checks the raw Sprite URL behavior directly so operators can confirm whether the Worker is still required as the front door for a given deployment.
