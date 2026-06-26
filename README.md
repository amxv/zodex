# zodex

`zodex` is a remote coding runtime plus operator CLI for Sprite-first deployments.

It gives ChatGPT or another coding agent a narrow remote execution surface:

- `exec_command`
- `write_stdin`
- `apply_patch`

The default product story is simple:

- the Sprite always has read access to approved GitHub repos through a read-only GitHub App
- the agent can inspect code, edit code, run tests, and commit locally at any time
- direct GitHub write access is off by default
- the operator grants temporary push access for one repo only when it is time to push
- the operator revokes that push access when the task is done

This repo is in a rename window:

- use `zodex` for the operator CLI
- use `zodexd` for the daemon
- legacy `computer-mcp`, `computer`, and `computer-mcpd` entrypoints still exist for compatibility

## Default Workflow

1. install `zodex` on a Sprite
2. configure the reader GitHub App so the agent can clone and fetch private repos
3. point MCP clients at the proxy-backed public URL
4. let the agent inspect, edit, test, and commit
5. when the operator wants the agent to push, run:
   `zodex github grant-push --sprite <sprite> --repo <owner/repo> [--publisher-app-id ... --publisher-pem ...]`
6. the agent pushes normally with `git push`
7. revoke the grant:
   `zodex github revoke-push --sprite <sprite> --repo <owner/repo>`

That push-grant flow is the only supported write path described by the primary docs.

## Sprite Front Door

For Sprite deployments, the Cloudflare Worker under [proxy/cloudflare-worker](proxy/cloudflare-worker) is a supported `zodex` component.

It is responsible for:

- normalizing `/mcp` to the Sprite origin's working `/mcp/` path
- warming a cold Sprite before proxying
- retrying transient cold-start and edge failures
- preserving streaming MCP responses

Useful commands:

```bash
zodex proxy inspect --sprite <sprite>
zodex proxy verify-origin --sprite <sprite>
zodex proxy deploy --sprite <sprite>
```

Treat the proxy or its custom domain as the default public MCP front door for Sprite deployments unless the raw Sprite URL has been re-validated against the MCP clients you care about.

## Read This First

- Sprite setup: [docs/agent-sprites-setup-runbook.md](docs/agent-sprites-setup-runbook.md)
- VPS setup: [docs/agent-vps-setup-runbook.md](docs/agent-vps-setup-runbook.md)
- GitHub app and push-grant model: [docs/github-app-agent-auth.md](docs/github-app-agent-auth.md)
- deployment details: [docs/deployment-notes.md](docs/deployment-notes.md)

If the target host is Runpod, use [.agents/skills/runpod-deployment/SKILL.md](.agents/skills/runpod-deployment/SKILL.md).

## How It Works

`zodexd` exposes a small remote coding surface over MCP and a matching HTTP API.

At a high level:

- `exec_command` starts a shell command and returns output plus session metadata
- `write_stdin` writes to or polls a running session by handle
- `apply_patch` applies structured Codex-style patches using an explicit `workdir`

Those three tools are enough for the standard coding loop:

1. inspect the repo and run code
2. keep terminal state alive across calls
3. edit files precisely
4. rerun checks and tests

## One-Time Setup

You need:

- a Sprite or Linux VPS
- `root` or `sudo`
- a reader GitHub App private key
- a push-grant GitHub App private key kept on the operator machine

Default config path:

```text
/etc/computer-mcp/config.toml
```

The clean Sprite-first path is:

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

The setup command derives installation IDs, uploads the runtime, configures read access, verifies the workspace, syncs Sprite Services, and leaves the deployment ready for proxy verification and later push grants.

## Day-To-Day Commands

```bash
zodex sprite status --sprite <sprite>
zodex sprite logs --sprite <sprite> --service computer-mcpd --lines 100
zodex proxy deploy --sprite <sprite>
zodex github grant-push --sprite <sprite> --repo <owner/repo>
zodex github list-grants --sprite <sprite>
zodex github revoke-push --sprite <sprite> --repo <owner/repo>
```

For local service management on non-Sprite hosts:

```bash
zodex start
zodex stop
zodex restart
zodex status
zodex logs
```

## Notes

- the runtime keeps remote execution stable during the product rename
- the config path is still `/etc/computer-mcp/config.toml` during this migration window
- the proxy is part of the supported system for Sprite deployments, not an afterthought
- the main safety model is read-mostly by default plus temporary repo-scoped write grants
