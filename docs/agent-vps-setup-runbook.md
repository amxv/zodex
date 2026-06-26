# zodex Agent VPS Setup Runbook

This runbook is for an agent helping a human set up `zodex` on a fresh Linux VPS.

This is a supported secondary path. The primary product path is Sprites.dev. If the target host is a Sprite, use [agent-sprites-setup-runbook.md](agent-sprites-setup-runbook.md) instead.

For Runpod-specific rollout behavior, use [../.agents/skills/runpod-deployment/SKILL.md](../.agents/skills/runpod-deployment/SKILL.md). That is a legacy compatibility surface, not the default product path.

## Outcome

When this runbook is complete:

- `zodex` is installed on the VPS
- the MCP HTTPS endpoint is live
- the VPS has both GitHub Apps configured
- the reader app is stored for read-only repo access
- the push-grant app can later be used for temporary direct push grants
- the agent user can commit immediately with the default global Git identity unless the operator overrides it

## Supported Model

Use this runbook only when a standard Linux VPS is the right target.

The supported access model stays the same as on Sprites:

- reader app for read-only clone and fetch
- temporary repo-scoped push grants for write
- non-root agent workspace
- operator-controlled write enable and revoke

## Information You Need

- VPS SSH host
- VPS SSH port
- VPS SSH user
- SSH private key path
- target GitHub repo slug, for example `owner/repo`
- reader GitHub App ID
- absolute local path to the reader app PEM file
- push-grant GitHub App ID
- absolute local path to the push-grant app PEM file

Do not ask the human to find installation IDs manually. Derive them.

## One-Time GitHub App Rules

Reader app:

- `Contents: Read-only`
- everything else: `No access`

Push-grant app:

- `Contents: Read & write`
- `Pull requests: Read & write`
- everything else: `No access`

Install both apps on `Only select repositories`.

For the full rationale and steady-state flow, use [github-app-agent-auth.md](github-app-agent-auth.md).

## Install Path

Primary install path on a VPS:

```bash
vps_ssh 'curl -fsSL https://raw.githubusercontent.com/amxv/computer-mcp/main/scripts/install.sh | sudo bash'
```

That installer now installs `zodex` and `zodexd` as the primary operator-facing binaries while preserving legacy compatibility links on the host where needed.

## Configure Keys And Runtime

Current runtime paths remain:

- config: `/etc/computer-mcp/config.toml`
- reader key: `/etc/computer-mcp/reader/private-key.pem`
- publisher key: `/etc/computer-mcp/publisher/private-key.pem`

Install the PEM files into those paths with the existing ownership model:

- the reader PEM is group-readable by `computer-mcp`
- the push-grant PEM is readable only by `computer-mcp-publisher`

Append the app settings to `/etc/computer-mcp/config.toml`:

- `reader_app_id`
- `reader_installation_id`
- `publisher_app_id`
- `[[publisher_targets]]`

Then start the stack with:

```bash
zodex start
```

## Verify

Run:

```bash
zodex status
curl -k "https://<public_ip_or_host>/health"
zodex show-url --host "<public_ip_or_host>"
```

Confirm:

- the runtime is healthy
- the agent user can commit in a writable workspace
- the agent user can read approved private repos through the reader helper

## Day-To-Day Workflow

Keep the same access discipline as the Sprite path:

- write off by default
- grant only when needed
- revoke after the push

The repo-scoped grant automation in this codebase is Sprite-first. If you are operating a plain VPS, keep the same reader-app and temporary-write model rather than widening credentials just because the host is not a Sprite.

## Notes

- Prefer the Sprite path for new installs unless there is a strong reason to stay on a traditional VPS.
- Keep the supported product story as `zodex`, not `computer-mcp`, even though the host config paths still use legacy names.
