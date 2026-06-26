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
- the default push-grant path uses GitHub App user access tokens obtained locally with device flow
- the operator revokes that access after the push

`zodex` is designed for Sprites.dev and assumes a proxy-backed public MCP front door.

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
5. When the operator wants a push, run:

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

6. The agent pushes normally with `git push`.
7. Revoke the grant:

```bash
zodex github revoke-push --sprite <sprite> --repo <owner/repo>
```

That temporary repo-scoped grant flow is the supported write path.
`grant-push` prefers the GitHub App device-flow path and falls back to the installation-token app-key path only when needed.

## Setup

The one canonical setup document is [docs/setup.md](docs/setup.md).

The install path is the Rust operator CLI:

```bash
zodex sprite setup \
  --sprite <sprite> \
  --repo <owner/repo> \
  --reader-app-id <reader-app-id> \
  --reader-pem /absolute/path/to/reader.pem \
  --publisher-app-id <push-grant-app-id> \
  --publisher-pem /absolute/path/to/push-grant-app.pem \
  --url-auth sprite
```

For day-to-day push grants, also set `publisher_client_id` in `/etc/zodex/config.toml` or export `ZODEX_PUBLISHER_CLIENT_ID` on the operator machine.
The publisher app key remains available as the fallback grant path and for the internal `zodex-prd` publish flow.

## Proxy Front Door

Useful commands:

```bash
zodex proxy inspect --sprite <sprite>
zodex proxy verify-origin --sprite <sprite>
zodex proxy deploy --sprite <sprite>
```

Treat the proxy or its custom domain as the default public MCP front door for Sprite deployments unless the raw Sprite URL has been re-validated against the MCP clients you care about.

## Core Commands

```bash
zodex sprite status --sprite <sprite>
zodex sprite logs --sprite <sprite> --service zodexd --lines 100
zodex sprite sync --sprite <sprite> --force-recreate
zodex sprite upgrade --sprite <sprite>
zodex github grant-push --sprite <sprite> --repo <owner/repo>
zodex github list-grants --sprite <sprite>
zodex github revoke-push --sprite <sprite> --repo <owner/repo>
```

## Access Model

- Read access comes from the reader GitHub App.
- Write access is temporary, explicit, and repo-scoped.
- The preferred write grant path is a GitHub App user token from device flow, cached locally per repo for refresh until revoked.
- The agent should not run as root.
- The operator should treat `grant-push` and `revoke-push` as part of every push.
