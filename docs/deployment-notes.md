# Deployment Notes

This file holds the details that were intentionally kept out of the main README.

## Proxy Component

For Sprite deployments, the Cloudflare Worker in [proxy/cloudflare-worker](../proxy/cloudflare-worker) is part of the supported `zodex` product shape.

Its responsibilities are explicit:

- normalize `/mcp` to the Sprite origin's working `/mcp/` upstream path
- warm a cold Sprite before proxying
- retry transient cold-start and edge failures
- preserve streamed MCP responses

Useful commands:

- `zodex proxy inspect --sprite <sprite>`
- `zodex proxy verify-origin --sprite <sprite>`
- `zodex proxy deploy --sprite <sprite>`

Treat the proxy as the default public MCP front door for Sprite deployments unless the raw Sprite URL has been re-validated against the actual MCP clients in use.

## Default Paths And Defaults

These are the main defaults:

- config file: `/etc/computer-mcp/config.toml`
- reader key: `/etc/computer-mcp/reader/private-key.pem`
- publisher key: `/etc/computer-mcp/publisher/private-key.pem`
- bind address: `0.0.0.0:443`
- agent user: `computer-mcp-agent`
- agent home: `/home/computer-mcp-agent`
- default workdir: `/workspace`
- publisher user: `computer-mcp-publisher`
- publisher socket: `/var/lib/computer-mcp/publisher/run/computer-mcp-prd.sock`

Most deployments only need to change:

- `reader_app_id`
- `reader_installation_id`
- `publisher_app_id`
- `publisher_targets`

Use overrides only when you actually need them, for example a non-443 port or a custom config path.

## Install From A Private Repo Checkout

The public installer tries GitHub Release artifacts first. If no matching release asset exists, it falls back to a source build.

If the public installer URL is not usable, build from a local checkout and point the installer at the built binaries:

```bash
cargo build --release --bin computer-mcp --bin computer-mcpd --bin computer-mcp-prd
sudo COMPUTER_MCP_BINARY_SOURCE_DIR="$PWD/target/release" bash scripts/install.sh
```

## Container Hosts

Before using the standard start flow, check whether the host actually has a usable `systemd`:

```bash
ps -p 1 -o pid=,comm=,args=
which systemctl || true
systemctl is-system-running || true
```

If PID 1 is not `systemd`, `computer-mcp` uses process mode instead.

On container-style hosts:
- `computer-mcp start` runs `computer-mcp-prd` and `computer-mcpd` as detached processes
- pid and log files are stored under the state directory
- restart persistence depends on the container lifecycle, not `systemd`

On Sprite-like hosts:
- keep the coding daemon on the dedicated `computer-mcp-agent` user instead of the built-in `sprite` user
- keep the publisher daemon on `computer-mcp-publisher`
- prefer `zodex sprite upgrade` as the normal operator upgrade path
- `zodex sprite setup` and `zodex sprite upgrade` upload the local `zodex` runtime binaries and run the remote Rust install path instead of relying on the legacy shell installer
- register Sprite Services with `zodex sprite sync` instead of relying on detached process mode
- prefer `zodex proxy verify-origin` before assuming the raw Sprite URL is safe to expose directly to MCP clients
- deploy or update the supported Worker with `zodex proxy deploy --sprite <sprite>` when you need a stable public MCP edge
- if Sprite Services drift into stale "running" state, use `zodex sprite sync --force-recreate --sprite <sprite> [--org <org>]` from the control-plane side
- treat `sprite api -s <sprite> /services` and `.../logs` as the lifecycle source of truth
- `computer-mcp upgrade` and `computer-mcp restart` in guest only cover already-healthy Sprite-managed processes; they are not the primary control-plane upgrade interface

The built-in Sprite user may have passwordless `sudo`, which would effectively hand the coding agent root and break the publisher-key isolation model.

## Security Model

The deployment is split into two local services:

- `computer-mcpd` runs the remote coding tools as `agent_user`
- `computer-mcp-prd` holds the GitHub App private key as `publisher_user`

`computer-mcp publish-pr` creates a local `git bundle` and sends it over a Unix socket to the publisher daemon. The agent never needs the GitHub write credential directly.

Important limits:
- do not run the coding agent as `root` if you want publisher-key isolation
- do not give the coding agent unrestricted `sudo`
- prefer a dedicated writable workspace such as `/workspace`, owned by `agent_user`
- keep the publisher key readable only by `publisher_user`
- keep `publisher_targets` restricted to approved repositories

The installer configures a host-scoped Git credential helper for `https://github.com` under the agent user's home. That helper mints short-lived reader-app installation tokens on demand, so normal HTTPS clone/fetch operations can read private repos without exposing the publisher write credential.

The installer also sets a default global commit identity for the agent user so fresh repos can commit immediately:

- `Computer MCP Agent`
- `computer-mcp-agent@local.invalid`

If you want a different identity, pass `COMPUTER_MCP_GIT_USER_NAME` and `COMPUTER_MCP_GIT_USER_EMAIL` during install or upgrade. Reinstalls preserve an existing custom identity unless you explicitly override it.

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

Example:

```bash
COMPUTER_MCP_VERSION=v0.1.0 \
COMPUTER_MCP_INSTALL_DIR=/usr/local/bin \
curl -fsSL https://raw.githubusercontent.com/amxv/computer-mcp/main/scripts/install.sh | sudo -E bash
```

If you use a non-default config file, add `--config /path/to/config.toml` to the `computer-mcp` commands from the main README.
