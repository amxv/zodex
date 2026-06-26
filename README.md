# zodex

`zodex` is a Sprite-first remote coding runtime plus operator CLI.

It gives ChatGPT or another coding agent a narrow remote execution surface:

- `exec_command`
- `write_stdin`
- `apply_patch`

The supported product story is:

- the runtime gets read access to approved GitHub repos through a read-only GitHub App
- the agent can inspect code, edit files, run tests, and commit locally without GitHub write access
- direct GitHub write access is off by default
- the operator grants temporary push access for one repo only when it is time to push
- the operator revokes that push access when the task is done

The primary deployment target is Sprites.dev. VPS support remains available, but the supported default workflow is Sprite-first and proxy-backed.

## Why It Exists

`zodex` is for the case where you want a coding agent to work inside a real remote Linux environment without handing it permanent GitHub write credentials.

It keeps the surface intentionally small:

- remote shell execution
- persistent PTY sessions
- structured file patching

That is enough for the normal coding loop:

1. clone and inspect a repo
2. edit code and rerun checks
3. discuss changes with the operator
4. grant push access briefly
5. push normally with `git push`
6. revoke write access again

## Supported Workflow

1. Set up the two GitHub Apps once:
   - a read-only reader app
   - a temporary push-grant app
2. Install `zodex` on a Sprite.
3. Point MCP clients at the proxy-backed public URL.
4. Let the agent clone, inspect, edit, test, and commit.
5. When the operator wants a push, run:

```bash
zodex github grant-push --sprite <sprite> --repo <owner/repo>
```

6. The agent pushes normally with `git push`.
7. Revoke the grant:

```bash
zodex github revoke-push --sprite <sprite> --repo <owner/repo>
```

That temporary repo-scoped grant flow is the supported write path.

## Quickstart

### 1. Read the one-time setup docs

- Sprite setup runbook: [docs/agent-sprites-setup-runbook.md](docs/agent-sprites-setup-runbook.md)
- GitHub App setup and permissions: [docs/github-app-agent-auth.md](docs/github-app-agent-auth.md)
- Day-to-day operator flow: [docs/operator-guide.md](docs/operator-guide.md)
- Agent expectations and access model: [docs/agent-instructions.md](docs/agent-instructions.md)
- Deployment details and compatibility notes: [docs/deployment-notes.md](docs/deployment-notes.md)

### 2. Install on a Sprite

The supported install path is the Rust operator CLI:

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

If the Sprite is in a non-default org, add:

```bash
--org <org-name>
```

### 3. Verify the public MCP front door

For Sprite deployments, the Cloudflare Worker under [proxy/cloudflare-worker](proxy/cloudflare-worker) is part of the supported system.

Useful commands:

```bash
zodex proxy inspect --sprite <sprite>
zodex proxy verify-origin --sprite <sprite>
zodex proxy deploy --sprite <sprite>
```

Treat the proxy or its custom domain as the default public MCP front door for Sprite deployments unless the raw Sprite URL has been re-validated against the MCP clients you care about.

## Core Commands

Sprite lifecycle:

```bash
zodex sprite status --sprite <sprite>
zodex sprite logs --sprite <sprite> --service computer-mcpd --lines 100
zodex sprite sync --sprite <sprite> --force-recreate
zodex sprite upgrade --sprite <sprite>
```

GitHub access control:

```bash
zodex github grant-push --sprite <sprite> --repo <owner/repo>
zodex github list-grants --sprite <sprite>
zodex github revoke-push --sprite <sprite> --repo <owner/repo>
```

Local non-Sprite service management:

```bash
zodex start
zodex stop
zodex restart
zodex status
zodex logs
```

## Access Model

Read access:

- comes from the reader GitHub App
- is intended for clone, fetch, and inspection
- stays available without granting write access

Write access:

- is off by default
- is granted explicitly by the operator
- is scoped to one repo at a time
- should be revoked after the push

This model only means anything if the coding runtime is not effectively root. The supported deployment path keeps the agent on a dedicated non-root user with a writable `/workspace`.

## Installation And Compatibility Notes

The operator-facing product names are:

- `zodex` for the operator CLI
- `zodexd` for the daemon

Compatibility details still exist where needed:

- host config path remains `/etc/computer-mcp/config.toml`
- Sprite service labels remain `computer-mcpd` and `computer-mcp-prd`
- some on-host users, groups, and sockets still use legacy `computer-mcp` names
- legacy `computer-mcp`, `computer`, and `computer-mcpd` entrypoints may still exist during the migration window

Those compatibility details are implementation facts, not the supported product narrative.

## Secondary Paths

- VPS setup remains documented in [docs/agent-vps-setup-runbook.md](docs/agent-vps-setup-runbook.md) for non-Sprite environments.
- Runpod-specific rollout guidance remains in [.agents/skills/runpod-deployment/SKILL.md](.agents/skills/runpod-deployment/SKILL.md) as a legacy compatibility surface, not the primary product path.

## Repository Direction

This repo is being finalized around the `zodex` product shape:

- Sprite-first deployment
- proxy-backed MCP front door
- read-only by default
- temporary repo-scoped write grants
- aligned operator and agent docs

If you are updating docs or workflows, preserve that story and remove competing setup guidance from the supported path instead of adding more variants.
