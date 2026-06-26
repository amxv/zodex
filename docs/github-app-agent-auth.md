# GitHub App Auth

`zodex` uses two GitHub Apps:

- a **reader app** for always-on read access from the Sprite runtime
- a **push-grant app** for temporary repo-scoped direct push access

The default model is:

- the agent can always clone and fetch approved private repos through the reader app
- the agent can edit, test, and commit locally without write access
- the operator grants temporary push access only when it is time to push
- the operator revokes that access afterward

This is the primary write story for `zodex`.

## Required App Permissions

Reader app:

- `Contents: Read-only`
- everything else: `No access`

Push-grant app:

- `Contents: Read & write`
- `Pull requests: Read & write`
- everything else: `No access`

## Installation Scope

Install both apps on `Only select repositories`.

Normal expectations:

- the reader app may be installed on every repo the agent should be allowed to inspect
- the push-grant app should be installed only on repos where the operator may later allow direct pushes

## Runtime Read Access

The Sprite runtime uses the reader app automatically once these config values are present:

- `reader_app_id`
- `reader_installation_id`
- `reader_private_key_path`

The installer configures a host-scoped Git credential helper for `https://github.com`, so normal commands such as:

- `git clone https://github.com/<owner>/<repo>.git`
- `git fetch`
- `git ls-remote`

use short-lived reader tokens without manual username/password prompts.

In other words, plain `git clone https://github.com/<owner>/<repo>.git` works once the reader app is configured.

## Temporary Push Grants

When the operator wants the agent to push to one repo, grant access explicitly:

```bash
zodex github grant-push \
  --sprite computer \
  --repo amxv/computer-mcp \
  --publisher-app-id <push-grant-app-id> \
  --publisher-pem /absolute/path/to/push-grant-app.pem
```

If the local machine already has matching config values, `--publisher-app-id` and `--publisher-pem` can be omitted.

Inspect active grants:

```bash
zodex github list-grants --sprite computer
```

Revoke the grant when the task is done:

```bash
zodex github revoke-push --sprite computer --repo amxv/computer-mcp
```

Grant behavior in the current implementation:

- write is off by default
- the granted token is scoped to one repo
- the granted token is stored on the Sprite only while the grant is active
- every other repo still uses the read-only helper path

## Example Config

The runtime config only needs the reader-side values for the default read path:

```toml
reader_app_id = 123456
reader_installation_id = 234567890
```

Default reader key path:

```text
/etc/computer-mcp/reader/private-key.pem
```

Place the reader key there:

```bash
sudo install -d -m 0750 -o root -g computer-mcp /etc/computer-mcp/reader
sudo install -m 0640 -o root -g computer-mcp \
  /path/to/reader-app.pem \
  /etc/computer-mcp/reader/private-key.pem
```

Then start the runtime:

```bash
zodex start
```

The push-grant app is still required for the full workflow, but the default runtime path is built around read access plus explicit temporary grants, not a resident write workflow.

## Commit Identity

The installer also ensures the agent can commit without per-repo setup. By default it sets:

- `user.name = "Computer MCP Agent"`
- `user.email = "computer-mcp-agent@local.invalid"`

If you want a different identity, override `COMPUTER_MCP_GIT_USER_NAME` and `COMPUTER_MCP_GIT_USER_EMAIL` during install or upgrade.

## What This Protects

This model is useful only if the coding agent is not effectively root.

Good:

- `computer-mcpd` runs as `computer-mcp-agent`
- the agent has a writable non-root workspace such as `/workspace`
- the reader app is read-only
- the operator grants write only for one repo and only when needed

Bad:

- the coding agent runs as `root`
- the coding agent has unrestricted `sudo`
- the reader app has write permissions
- the push-grant app is installed broadly without operator discipline

## Primary References

- Sprite setup: [agent-sprites-setup-runbook.md](agent-sprites-setup-runbook.md)
- VPS setup: [agent-vps-setup-runbook.md](agent-vps-setup-runbook.md)
- deployment details: [deployment-notes.md](deployment-notes.md)
