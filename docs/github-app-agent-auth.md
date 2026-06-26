# GitHub App Auth

`zodex` uses two GitHub Apps.

- a **reader app** for read-only private repo access
- a **publisher app** for branch push + PR creation

The publisher side uses a split local model:

- the coding agent edits code, runs tests, and makes a local commit
- the local publisher daemon holds the GitHub App private key
- `zodex publish-pr` sends a local `git bundle` to the publisher daemon
- the publisher daemon mints a short-lived installation token internally, pushes a generated branch, opens the PR, and returns the PR URL

By default the coding daemon runs as `computer-mcp-agent`, with:

- home: `/home/computer-mcp-agent`
- default workdir: `/workspace`

The goal is simple:

- the agent gets read-only GitHub access through the reader app
- write access stays off by default until the operator grants it for one repo
- the operator can grant and revoke direct push access with `zodex github ...`
- plain `git clone https://github.com/<owner>/<repo>.git` works for the agent through a short-lived reader credential helper

For full operator runbooks:

- Sprites: [agent-sprites-setup-runbook.md](agent-sprites-setup-runbook.md)
- VPS: [agent-vps-setup-runbook.md](agent-vps-setup-runbook.md)

Default config file: `/etc/computer-mcp/config.toml`

Most installs only need to add:

- `reader_app_id`
- `reader_installation_id`
- `publisher_app_id`
- one or more `publisher_targets`

The built-in defaults already cover:

- `agent_user = "computer-mcp-agent"`
- `agent_home = "/home/computer-mcp-agent"`
- `default_workdir = "/workspace"`
- `publisher_user = "computer-mcp-publisher"`

## Required App Permissions

Reader app:

- `Contents: Read-only`
- everything else: `No access`

Publisher app:

- `Contents: Read & write`
- `Pull requests: Read & write`
- everything else: `No access`

## Manual GitHub Setup

GitHub App registration is still manual.

Create two private GitHub Apps, install both on `Only select repositories`, then record:

- reader app ID
- reader installation ID
- reader PEM path
- publisher app ID
- publisher installation ID
- publisher PEM path

## Configure `zodex`

Example config:

```toml
reader_app_id = 123456
reader_installation_id = 234567890
publisher_app_id = 3123864

[[publisher_targets]]
id = "amxv/computer-mcp"
repo = "amxv/computer-mcp"
default_base = "main"
installation_id = 117314785
```

Place the keys at the default paths:

```bash
sudo install -d -m 0750 -o root -g computer-mcp /etc/computer-mcp/reader
sudo install -m 0640 -o root -g computer-mcp \
  /path/to/reader-app.pem \
  /etc/computer-mcp/reader/private-key.pem

sudo install -m 0600 -o computer-mcp-publisher -g computer-mcp \
  /path/to/publisher-app.pem \
  /etc/computer-mcp/publisher/private-key.pem
```

Then start the stack:

```bash
zodex start
```

`zodex start` validates both apps, creates TLS artifacts if needed, starts the publisher daemon, and starts the MCP daemon.

The installer also configures the agent user's Git config with a host-scoped helper for `https://github.com`. Once `reader_app_id`, `reader_installation_id`, and the reader PEM are present, normal HTTPS `git clone`, `git fetch`, and `git ls-remote` use short-lived reader tokens automatically.

The installer also ensures the agent can make commits without per-repo setup. By default it sets:

- `user.name = "Computer MCP Agent"`
- `user.email = "computer-mcp-agent@local.invalid"`

If you want a different commit identity, override `COMPUTER_MCP_GIT_USER_NAME` and `COMPUTER_MCP_GIT_USER_EMAIL` when running `scripts/install.sh`. Existing custom values are preserved on reinstall unless you explicitly override them.

## Direct Push Grants

`zodex` keeps read access always-on through the reader app helper and layers temporary repo-scoped write access on top only when the operator requests it.

Grant push access for one Sprite and one repo:

```bash
zodex github grant-push --sprite computer --repo amxv/computer-mcp
```

Revoke it when the task is done:

```bash
zodex github revoke-push --sprite computer --repo amxv/computer-mcp
```

Inspect active grants:

```bash
zodex github list-grants --sprite computer
```

The grant model in this phase uses a locally minted GitHub App installation token:

- write is off by default
- the token is scoped to one repo
- the token is stored on the Sprite only while the grant is active
- clone/fetch for every other repo still goes through the read-only helper path

## Legacy `publish-pr` Path

Run `publish-pr` from inside the repo checkout after the change has already been committed:

```bash
zodex publish-pr \
  --repo amxv/computer-mcp \
  --title "Agent: example change" \
  --body "Automated change from zodex."
```

Current requirements:

- the current directory must be inside a git repo
- the worktree must be clean
- the commit you want in the PR must already be on `HEAD`
- the `--repo` value must match one of the configured `publisher_targets`

`publish-pr` does not expose or print the GitHub installation token. It remains available for PR-only workflows, but the standard operator-facing write path is `zodex github grant-push` plus normal `git push`.

## What This Does And Does Not Protect

This architecture protects the GitHub write credential from the coding agent only if the agent is not running with unrestricted root-level access.

Good:
- `computer-mcpd` runs as `computer-mcp-agent`
- `computer-mcp-prd` runs as `computer-mcp-publisher`
- the publisher key is readable only by `computer-mcp-publisher`
- the agent gets a writable non-root workspace such as `/workspace`
- the agent's GitHub clone access is limited to the reader app permissions

Bad:
- the coding agent runs as `root`
- the coding agent has unrestricted `sudo`
- the coding agent can read the publisher user's files or processes
- on Sprites, the coding agent runs as the built-in `sprite` user

## Private Repo Branch Protection Note

On a private personal GitHub repo without GitHub Pro, GitHub will not enforce protected branches server-side.

With this architecture, the main safety property does not come from GitHub blocking `main`. It comes from keeping the GitHub write credential inside the publisher daemon instead of handing it to the coding agent.

If the coding agent also needs to clone private repos directly, use the built-in reader helper and keep the reader app permissions read-only. Do not reuse the publisher credential for clone access.
