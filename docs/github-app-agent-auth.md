# GitHub App Setup And Access Model

This is the one-time setup runbook for the supported `zodex` GitHub access model.

`zodex` uses two GitHub Apps:

- a **reader app** for always-on read access from the runtime
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

Do not widen those permissions as part of normal setup.

## Installation Scope

Install both apps on `Only select repositories`.

Normal expectations:

- the reader app may be installed on every repo the agent should be allowed to inspect
- the push-grant app should be installed only on repos where the operator may later allow direct pushes

## What The Runtime Uses

The runtime read path needs these values:

- `reader_app_id`
- `reader_installation_id`
- `reader_private_key_path`

The installer configures a host-scoped Git credential helper for `https://github.com`, so normal commands such as:

- `git clone https://github.com/<owner>/<repo>.git`
- `git fetch`
- `git ls-remote`

work through short-lived reader tokens without a manual username/password prompt.

In other words, plain `git clone https://github.com/<owner>/<repo>.git` works once the reader app is configured.

## One-Time Setup Checklist

1. Create the reader app with read-only `Contents`.
2. Create the push-grant app with only `Contents: Read & write` and `Pull requests: Read & write`.
3. Install both apps on `Only select repositories`.
4. Download both PEM files and keep them on the operator machine.
5. Record:
   - reader app ID
   - push-grant app ID
   - the target repo slug
6. Run the Sprite setup flow from [agent-sprites-setup-runbook.md](agent-sprites-setup-runbook.md).

## Temporary Push Grants

When the operator wants the agent to push to one repo, grant access explicitly:

```bash
zodex github grant-push \
  --sprite <sprite> \
  --repo <owner/repo> \
  --publisher-app-id <push-grant-app-id> \
  --publisher-pem /absolute/path/to/push-grant-app.pem
```

If the local machine already has matching config values, `--publisher-app-id` and `--publisher-pem` can be omitted.

Inspect active grants:

```bash
zodex github list-grants --sprite <sprite>
```

Revoke the grant when the task is done:

```bash
zodex github revoke-push --sprite <sprite> --repo <owner/repo>
```

Grant behavior in the supported model:

- write is off by default
- the granted token is scoped to one repo
- the granted token is stored on the Sprite only while the grant is active
- every other repo still uses the read-only helper path

## Runtime Paths And Defaults

The current host-level defaults remain:

- config file: `/etc/computer-mcp/config.toml`
- reader key: `/etc/computer-mcp/reader/private-key.pem`
- publisher key: `/etc/computer-mcp/publisher/private-key.pem`

Those are compatibility paths. They remain supported even though the operator-facing product name is `zodex`.

Then start the runtime:

```bash
zodex start
```

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
- Operator workflow: [operator-guide.md](operator-guide.md)
- Agent expectations: [agent-instructions.md](agent-instructions.md)
- Deployment details: [deployment-notes.md](deployment-notes.md)
