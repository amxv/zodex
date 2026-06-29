# zodex

`zodex` is a Sprite-first remote coding runtime plus operator CLI.

It gives a coding agent three remote tools:

- `exec_command`
- `write_stdin`
- `apply_patch`

The product model is:

- read-only GitHub access is always available through a reader app
- the agent can inspect, edit, test, and commit without GitHub write access
- write access is off by default
- the operator grants temporary repo-scoped push access only when a push is intended
- the default Sprite-side push path uses GitHub App user access tokens obtained with device flow on the Sprite
- the operator revokes that access after the push

`zodex` is designed for Sprites.dev and assumes a proxy-backed public MCP front door.

The supported repository slug for this project is `amxv/zodex`.

## Why It Exists

`zodex` is for the case where you want a coding agent to work inside a real remote Linux environment without handing it permanent GitHub write credentials.

That is enough for the normal coding loop:

1. clone and inspect a repo
2. edit code and rerun checks
3. commit locally
4. grant push access briefly
5. push normally with `git push`
6. revoke write access again

## Supported Workflow

1. Set up the two GitHub Apps once:
   - a read-only reader app
   - a temporary push-grant app with device flow enabled
2. Install `zodex` on a Sprite.
3. Point MCP clients at the proxy-backed public URL.
4. Let the agent clone, inspect, edit, test, and commit.
5. When the agent is ready to push on the Sprite, run:

```bash
zodex-agent github request-push --repo <owner/repo>
```

The default active grant TTL is `30m`.
Disable the TTL with `--no-ttl`, change it with `--ttl 2h`, and opt into refresh-token caching with `--cache-refresh-token`.

6. The agent pushes normally with `git push`.
7. While the same grant window is active, the agent can open a PR without `gh`:

```bash
zodex-agent github create-pr \
  --repo <owner/repo> \
  --head <pushed-branch> \
  --title "Title" \
  --base main \
  --body "Optional description"
```

`create-pr` reuses the exact temporary repo-scoped push grant (the same token written by `request-push`) and calls the GitHub REST API directly. Once the grant expires or is revoked, `create-pr` has no usable auth and fails.

8. Revoke the local grant on the Sprite:

```bash
zodex-agent github revoke-push --repo <owner/repo>
```

When an operator wants to activate a grant remotely from their own machine instead, run:

```bash
zodex github grant-push --sprite <sprite> --repo <owner/repo>
```

If the push-grant app client ID is not present in config, pass it directly:

```bash
zodex github grant-push \
  --sprite <sprite> \
  --repo <owner/repo> \
  --publisher-client-id <push-grant-app-client-id>
```

Then revoke the remote grant:

```bash
zodex github revoke-push --sprite <sprite> --repo <owner/repo>
```

That temporary repo-scoped grant flow is the supported write path.
`zodex-agent github request-push` and `zodex github grant-push` both use the GitHub App device-flow path.
By default, `request-push` does not persist refresh-token state and writes a repo-scoped local grant that expires after `30m`.
Expired grants stop working in the credential-helper path even if a stale grant file still exists.
By default, `revoke-push` removes the active repo grant and keeps the local device-flow refresh state so the remote operator path usually avoids a full reauth on the next grant.
If you want to fully forget the local cached auth state too, add `--forget-local-auth`.


## Documentation Site

This repository includes an Astro documentation site for zodex. It covers the Sprite runtime architecture, GitHub App access model, setup flow, temporary push grants, proxy and MCP front door, direct HTTP API, command reference, troubleshooting, and docs maintenance.

Run it locally with:

```bash
bun install
bun run dev
```

Validate the docs site with:

```bash
bun run check
bun run build
```

Deploy the docs worker with:

```bash
bun run deploy:docs
```

Production routing keeps `zodex.ashray.xyz` on the existing Cloudflare proxy worker. That worker forwards `/mcp`, `/mcp/*`, and `/health` to the live Sprite and sends all other paths to the `zodex-docs` worker origin.

The Astro docs content lives in `src/content/docs`, with site-wide navigation and metadata in `src/data/docs.ts`.

## Setup

The one canonical setup document is [docs/setup.md](docs/setup.md).

The install path is the Rust operator CLI:

```bash
zodex sprite setup \
  --sprite <sprite> \
  --repo amxv/zodex \
  --reader-app-id <reader-app-id> \
  --reader-pem /absolute/path/to/reader.pem \
  --publisher-app-id <push-grant-app-id> \
  --publisher-pem /absolute/path/to/push-grant-app.pem \
  --url-auth sprite
```

For day-to-day push grants, set `publisher_client_id` in `/etc/zodex/config.toml` or export `ZODEX_PUBLISHER_CLIENT_ID` in the environment where you run the command.
The publisher app key remains available for the internal `zodex-prd` publish flow.
After setup, the Sprite guest keeps `zodex-agent`, `zodexd`, and `zodex-prd` on-box; the full `zodex` operator CLI remains an operator-machine tool.
The Sprite agent should use `zodex-agent`, not the full `zodex` operator CLI, for `show-url`, Git credential helper access, and local push auth.

## Proxy Front Door

Useful commands:

```bash
zodex proxy inspect --sprite <sprite>
zodex proxy verify-origin --sprite <sprite>
cd proxy/cloudflare-worker
# set vars.SPRITE_ORIGIN in wrangler.jsonc first
npx wrangler deploy
```

Treat the proxy or its custom domain as the default public MCP front door for Sprite deployments unless the raw Sprite URL has been re-validated against the MCP clients you care about.

## Core Commands

```bash
zodex sprite status --sprite <sprite>
zodex sprite logs --sprite <sprite> --service zodexd --lines 100
zodex sprite sync --sprite <sprite> --force-recreate
zodex sprite upgrade --sprite <sprite>
zodex-agent github request-push --repo <owner/repo>
zodex github grant-push --sprite <sprite> --repo <owner/repo>
zodex-agent github list-grants
zodex-agent github create-pr --repo <owner/repo> --head <branch> --title "Title"
zodex-agent github revoke-push --repo <owner/repo>
zodex-agent show-url --host <public-host>
```

## Access Model

- Read access comes from the reader GitHub App.
- Write access is temporary, explicit, and repo-scoped.
- The preferred write grant path is `zodex-agent github request-push`, which uses GitHub App device flow directly on the Sprite with a repo-scoped local grant and a default `30m` TTL.
- The remote operator path `zodex github grant-push --sprite ...` remains supported and keeps local refresh-token cache behavior unless explicitly forgotten.
- The agent should not run as root.
- The operator or agent should treat `zodex-agent github request-push` or `zodex github grant-push`, followed by revoke, as part of every push.
