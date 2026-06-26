# zodex Agent Sprites Setup Runbook

This runbook is for an agent helping a human set up `zodex` on a Sprite.

Use this when the target environment is Sprites (`sprite` CLI), not a traditional VPS over SSH.

For traditional VPS setup, use [agent-vps-setup-runbook.md](agent-vps-setup-runbook.md).
For Runpod-specific rollout behavior, use [../.agents/skills/runpod-deployment/SKILL.md](../.agents/skills/runpod-deployment/SKILL.md).

## Outcome

When this runbook is complete:

- latest `zodex` runtime is installed in the target Sprite
- reader + push-grant GitHub App auth is configured
- the MCP daemon and supporting runtime services are registered as Sprite Services
- the coding agent starts in a writable non-root workspace (`/workspace`)
- the coding agent can commit immediately with the default global Git identity unless the operator overrides it
- the coding agent can `git clone` private GitHub repos over HTTPS through the reader app
- MCP endpoint is reachable through the Sprite URL
- proxy deployment can be managed from the same repo when a stable public MCP front door is needed

## Why Sprites Need A Slightly Different Path

Sprites are Linux boxes, but ad hoc processes do not survive Sprite sleep and wake cycles.

For `zodex`, the durable deployment model on Sprites is:

- non-root local service users inside the Sprite
- Sprite Services as the platform lifecycle owner
- `http_bind_port = 8080` as the public Sprite URL target

Detached process mode is still part of the installer fallback, but it is not the operational source of truth on Sprites.

To avoid privileged port binding failures in process mode, this runbook uses:

- `bind_port = 8443` for TLS listener
- `http_bind_port = 8080` for Sprite URL routing

To avoid collapsing the security boundary around the publisher key, this runbook does not run the coding daemon as `sprite`.

Instead it uses:

- `computer-mcp-agent` as a normal non-root workspace user
- `computer-mcp-publisher` as the isolated publisher user
- `/home/computer-mcp-agent` as the agent home
- `/workspace` as the default writable workdir

This matters on Sprites because the built-in `sprite` user commonly has passwordless `sudo`. Running the coding daemon as `sprite` would effectively give the agent root and let it break publisher-key isolation.

## Required Inputs

- Sprite name (example: `computer`)
- optional organization name
- target repo slug (example: `owner/repo`)
- reader app ID
- absolute local path to reader PEM
- push-grant app ID
- absolute local path to push-grant PEM

Do not ask the human for installation IDs manually. Derive them.

## Fast Path (Recommended)

Use the Rust operator CLI directly:

Example:

```bash
zodex sprite setup \
  --sprite computer \
  --repo amxv/computer-mcp \
  --reader-app-id <reader-app-id> \
  --reader-pem /absolute/path/to/reader-private-key.pem \
  --publisher-app-id <publisher-app-id> \
  --publisher-pem /absolute/path/to/publisher-private-key.pem \
  --default-base main \
  --url-auth sprite
```

If the Sprite is in a non-default org, add:

```bash
--org <org-name>
```

What `zodex sprite setup` does:

1. derives reader and push-grant installation IDs from app ID + PEM + repo
2. validates both apps from the local Rust CLI path
3. uploads the local `zodex` runtime binaries and supporting components to the Sprite
4. runs the remote Rust install path to create users, directories, and config state
5. installs the reader key and records the push-grant app details needed by the control-plane flow
6. writes a managed GitHub app config block
7. enforces Sprite-safe ports (`8443` TLS + `8080` HTTP)
8. configures the agent Git identity and reader credential helper
9. stops any detached process-mode daemons left from older installs
10. creates or updates the Sprite-managed runtime services
11. verifies Service inventory, Service logs, and public Sprite health
12. verifies the agent can commit with the default Git identity
13. verifies the agent can mint reader-backed Git credentials for GitHub HTTPS access
14. prints MCP URL hint based on Sprite URL host

After setup, the normal write flow is:

```bash
zodex github grant-push --sprite <sprite> --repo <owner/repo>
# agent pushes normally with git push
zodex github revoke-push --sprite <sprite> --repo <owner/repo>
```

If the Sprite URL needs a more reliable public MCP front door, manage the supported Cloudflare Worker from this repo:

```bash
zodex proxy inspect --sprite <sprite>
zodex proxy verify-origin --sprite <sprite>
zodex proxy deploy --sprite <sprite>
```

The proxy exists to preserve MCP streaming, normalize `/mcp` to `/mcp/` when the raw Sprite edge disagrees, and recover cold wakes with warmup plus retries.

## Routine Upgrades

For an already-configured Sprite, prefer the Rust control-plane upgrade flow:

Example:

```bash
zodex sprite upgrade --sprite computer --org amxv
```

That command uploads the current local operator/runtime binaries, installs them inside the Sprite through the remote Rust CLI, force-recreates the Sprite Services from the control plane, verifies local health, and verifies reader-backed GitHub HTTPS access still works.

## Manual Path (If You Need It)

If you cannot use the script, follow the same sequence manually:

1. Derive installation IDs using JWT + GitHub `/repos/<repo>/installation`.
2. Validate both apps with `scripts/mint-gh-app-installation-token.sh`.
3. Run installer inside Sprite:
   - `curl -fsSL https://raw.githubusercontent.com/amxv/computer-mcp/main/scripts/install.sh | sudo env COMPUTER_MCP_HTTP_BIND_PORT=8080 COMPUTER_MCP_AGENT_HOME=/home/computer-mcp-agent COMPUTER_MCP_DEFAULT_WORKDIR=/workspace bash`
4. Install PEMs to:
   - `/etc/computer-mcp/reader/private-key.pem`
   - `/etc/computer-mcp/publisher/private-key.pem`
5. Set config:
   - `bind_port = 8443`
   - `http_bind_port = 8080`
   - `agent_home = "/home/computer-mcp-agent"`
   - `default_workdir = "/workspace"`
   - reader app fields
   - publisher app fields and target repo
6. Stop any old detached daemons:
   - `sprite exec -s <sprite> -- sudo computer-mcp stop || true`
7. Register Sprite Services:
   - `zodex sprite sync --sprite <sprite> [--org <org-name>]`
   - if Sprite reports stale running state, use `zodex sprite sync --sprite <sprite> [--org <org-name>] --force-recreate`
8. Verify:
   - `zodex sprite status --sprite <sprite> [--org <org-name>]`
   - `zodex sprite logs --sprite <sprite> [--org <org-name>] --service computer-mcp-prd --lines 20`
   - `zodex sprite logs --sprite <sprite> [--org <org-name>] --service computer-mcpd --lines 20`
   - `curl -fsS https://<sprite-host>/health`
   - `sudo -u computer-mcp-agent env HOME=/home/computer-mcp-agent bash -lc 'cd /workspace && touch .ok && rm -f .ok'`
   - `sudo -u computer-mcp-agent env HOME=/home/computer-mcp-agent git -C /workspace ls-remote https://github.com/<owner>/<private-repo>.git HEAD`

## Sprite Service Lifecycle

For Sprite deployments, the authoritative runtime view is the Sprite Services API, not detached pid files inside the Sprite.

Useful commands:

- `zodex sprite setup --sprite <sprite> --repo <owner/repo> ...`
- `zodex sprite upgrade --sprite <sprite> [--org <org-name>] [--version <tag|latest>]`
- `zodex sprite status --sprite <sprite> [--org <org-name>]`
- `zodex sprite sync --sprite <sprite> [--org <org-name>] --force-recreate`
- `zodex sprite logs --sprite <sprite> [--org <org-name>] --service computer-mcpd --lines 100`
- `zodex sprite logs --sprite <sprite> [--org <org-name>] --service computer-mcp-prd --lines 100`
- `zodex proxy inspect --sprite <sprite>`
- `zodex proxy verify-origin --sprite <sprite>`
- `zodex proxy deploy --sprite <sprite>`
- `zodex github grant-push --sprite <sprite> --repo <owner/repo>`
- `zodex github list-grants --sprite <sprite>`
- `zodex github revoke-push --sprite <sprite> --repo <owner/repo>`

If `zodex sprite status` shows a service as running but guest-side `ps` or `/health` disagrees, prefer `zodex sprite sync --force-recreate` from a machine with Sprite CLI access. That clears stale control-plane state without widening agent-user access or exposing the publisher key.

## Verification Checklist

- `zodex sprite status` shows both:
  - `computer-mcp-prd`
  - `computer-mcpd`
- `computer-mcpd` depends on `computer-mcp-prd`
- `computer-mcpd` exposes `http_port = 8080`
- config file contains expected app IDs and installation IDs
- reader and publisher PEM permissions are correct
- `computer-mcp-agent` can write inside `/workspace`
- `computer-mcp-agent` can create a commit in a fresh repo without extra Git config
- `computer-mcp-agent` can access private GitHub repos over HTTPS without a manual username/password prompt
- Sprite URL auth mode is intentional (`sprite` by default; `public` only if required)
- Service logs are readable under the Sprite Services API

## Stop Conditions

Stop and ask before continuing if:

- reader app has any write permission
- publisher app has permissions beyond `contents:write` and `pull_requests:write`
- app installation scope is broader than intended
- `computer-mcpd` cannot bind even after Sprite-safe ports are set
- app token minting validation fails
