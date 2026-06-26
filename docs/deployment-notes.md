# Deployment Notes

This file holds details that are intentionally kept out of the main README.

## Supported Product Shape

The supported path is:

- `zodex` as the operator CLI
- `zodexd` as the daemon
- Sprites.dev as the primary deployment target
- proxy-backed public MCP access by default
- read-only GitHub access by default
- temporary repo-scoped push grants when the operator wants a push

## Proxy Component

For Sprite deployments, the Cloudflare Worker in [proxy/cloudflare-worker](../proxy/cloudflare-worker) is part of the supported `zodex` system.

Its responsibilities are explicit:

- normalize `/mcp` to the Sprite origin's working `/mcp/` upstream path
- warm a cold Sprite before proxying
- retry transient cold-start and edge failures
- preserve streamed MCP responses

Useful commands:

- `zodex proxy inspect --sprite <sprite>`
- `zodex proxy verify-origin --sprite <sprite>`
- `zodex proxy deploy --sprite <sprite>`

Treat the proxy as the default public MCP front door for Sprite deployments unless the raw Sprite URL has already been re-validated against the MCP clients you care about.

## Current Host-Level Defaults

These remain the runtime defaults today:

- config file: `/etc/computer-mcp/config.toml`
- reader key: `/etc/computer-mcp/reader/private-key.pem`
- publisher key: `/etc/computer-mcp/publisher/private-key.pem`
- bind address: `0.0.0.0:443`
- agent user: `computer-mcp-agent`
- agent home: `/home/computer-mcp-agent`
- default workdir: `/workspace`
- publisher user: `computer-mcp-publisher`
- publisher socket: `/var/lib/computer-mcp/publisher/run/computer-mcp-prd.sock`

These are compatibility paths and identifiers. They are still supported, but they are not the operator-facing product naming.

## Release And Install Shape

The supported release/install story should read as `zodex`:

- release archives are published under `zodex` names
- the installer prefers `zodex` artifacts
- host installs still provide compatibility links for legacy `computer-mcp` entrypoints where needed

If the public installer URL is not usable, build from a local checkout and point the installer at the built binaries:

```bash
cargo build --release --bin zodex --bin zodexd --bin computer-mcp-prd
sudo COMPUTER_MCP_BINARY_SOURCE_DIR="$PWD/target/release" bash scripts/install.sh
```

## Sprite Runtime Notes

On Sprites:

- prefer `zodex sprite setup` for initial installation
- prefer `zodex sprite upgrade` for routine upgrades
- prefer `zodex sprite sync` for service reconciliation
- `zodex sprite setup` and `zodex sprite upgrade` run the remote Rust install path
- prefer `zodex proxy verify-origin` before exposing the raw Sprite URL directly
- use `zodex proxy deploy` when you need the stable public MCP edge
- treat Sprite Services as the lifecycle source of truth

If Sprite Services drift into stale `running` state, use:

```bash
zodex sprite sync --sprite <sprite> --force-recreate
```

The built-in `sprite` user may have passwordless `sudo`, which would effectively hand the coding agent root and break the temporary-write-control model. The supported setup keeps the coding runtime on `computer-mcp-agent` instead.

## Standard Linux Hosts

If the target host is a normal Linux VPS with working `systemd`, the existing CLI flow is still appropriate:

```bash
zodex start
zodex stop
zodex restart
zodex status
zodex logs
```

If PID 1 is not `systemd`, the runtime falls back to process mode.

## Access Model

The supported operator story is:

- the runtime has read access through the reader app helper
- the agent can clone, fetch, edit, test, and commit without write access
- the operator grants temporary repo-scoped push access only when needed
- the operator revokes that access afterward

Daily operator commands:

- `zodex github grant-push --sprite <sprite> --repo <owner/repo>`
- `zodex github list-grants --sprite <sprite>`
- `zodex github revoke-push --sprite <sprite> --repo <owner/repo>`

Important limits:

- do not run the coding agent as `root` if you want repo-scoped write control to mean anything
- do not give the coding agent unrestricted `sudo`
- prefer a dedicated writable workspace such as `/workspace`
- keep the push-grant app private key on the operator machine
- keep the push-grant app installed only on approved repositories

## Useful Installer Overrides

`scripts/install.sh` supports these environment overrides:

- `COMPUTER_MCP_VERSION`
- `COMPUTER_MCP_REPO`
- `COMPUTER_MCP_ASSET_URL`
- `COMPUTER_MCP_SOURCE_REF`
- `COMPUTER_MCP_BINARY_SOURCE_DIR`
- `COMPUTER_MCP_INSTALL_DIR`
- `COMPUTER_MCP_CONFIG_PATH`
- `COMPUTER_MCP_STATE_DIR`
- `COMPUTER_MCP_TLS_DIR`
- `COMPUTER_MCP_AGENT_USER`
- `COMPUTER_MCP_AGENT_HOME`
- `COMPUTER_MCP_AGENT_SHELL`
- `COMPUTER_MCP_DEFAULT_WORKDIR`
- `COMPUTER_MCP_PUBLISHER_USER`
- `COMPUTER_MCP_PUBLISHER_HOME`
- `COMPUTER_MCP_SERVICE_GROUP`
- `COMPUTER_MCP_GIT_USER_NAME`
- `COMPUTER_MCP_GIT_USER_EMAIL`
- `COMPUTER_MCP_READER_KEY_DIR`
- `COMPUTER_MCP_PUBLISHER_KEY_DIR`
- `COMPUTER_MCP_HTTP_BIND_PORT`
- `COMPUTER_MCP_PUBLIC_HOST`
- `COMPUTER_MCP_ENABLE_CERTBOT`

Those names remain legacy for compatibility with the current host layout.
