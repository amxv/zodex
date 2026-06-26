# zodex

Remote coding MCP server for Linux VPS and Sprite deployment.

Phase 1 rename note:
- use `zodex` for the operator CLI
- use `zodexd` for the daemon
- legacy `computer-mcp`, `computer`, and `computer-mcpd` entrypoints still work during the compatibility window

This README is the fast path for a fresh VPS.

Agents doing a full operator-led setup should read the runbook that matches the target environment first:

- Sprites: [docs/agent-sprites-setup-runbook.md](docs/agent-sprites-setup-runbook.md)
- VPS: [docs/agent-vps-setup-runbook.md](docs/agent-vps-setup-runbook.md)

For extra detail, see:
- [docs/agent-sprites-setup-runbook.md](docs/agent-sprites-setup-runbook.md)
- [docs/agent-vps-setup-runbook.md](docs/agent-vps-setup-runbook.md)
- [docs/deployment-notes.md](docs/deployment-notes.md)
- [docs/github-app-agent-auth.md](docs/github-app-agent-auth.md)
- [.agents/skills/runpod-deployment/SKILL.md](.agents/skills/runpod-deployment/SKILL.md)
- [gg/agent-outputs/computer-cli-quickstart-for-agents.md](gg/agent-outputs/computer-cli-quickstart-for-agents.md)

If the target host is Runpod, use [.agents/skills/runpod-deployment/SKILL.md](.agents/skills/runpod-deployment/SKILL.md).
If the target host is Sprites, use [docs/agent-sprites-setup-runbook.md](docs/agent-sprites-setup-runbook.md).
The main README below is the standard Linux VPS path.

Container images:
- `ghcr.io/amxv/computer-mcp` is the generic image built from [Dockerfile](Dockerfile)
- `ghcr.io/amxv/computer-mcp-runpod` is the dedicated Runpod template image built from [Dockerfile.runpod](Dockerfile.runpod)

## How It Works

`zodex` exposes a small remote coding surface over MCP so a model can control a Linux VPS the same way Codex-style agents control a local coding sandbox.

At a high level:

- `exec_command` starts a shell command on the VPS and returns output plus session metadata (`status`, `cwd`, and `termination_reason` when finished) and a `session_handle` if the command is still running
- `write_stdin` writes to or polls that running session by `session_handle`, returns the same session metadata shape, and keeps the session alive by resetting the idle timeout
- `apply_patch` applies structured Codex-style patches to files without handing the model raw filesystem write primitives; patch input includes a required `workdir` used to resolve relative patch paths

Those three tools are enough to simulate the core Codex workflow:

1. inspect and run code with `exec_command`
2. keep stateful terminal sessions alive with `write_stdin`
3. make precise code edits with `apply_patch`
4. rerun commands to validate the result

That is the main purpose of this repository: give models a narrow remote execution interface that feels like a Codex environment, while keeping GitHub write access separated behind the local publisher daemon described in [docs/github-app-agent-auth.md](docs/github-app-agent-auth.md).

## What You Need

- A Linux VPS
- `root` or `sudo`
- A public IP or host for the MCP endpoint
- A reader GitHub App private key
- A publisher GitHub App private key

Default config file: `/etc/computer-mcp/config.toml`

The compatibility release keeps legacy paths, service names, and payload formats unchanged while moving operator-facing commands to `zodex`.

The commands below assume that default path. If you use a different config file, add `--config /path/to/config.toml`.

## 1. Install

If you have a public installer URL:

```bash
curl -fsSL https://raw.githubusercontent.com/amxv/computer-mcp/main/scripts/install.sh | sudo bash
```

The installer downloads prebuilt Linux release artifacts when they are available.
It falls back to a source build only if no matching release asset exists.

If this repository is private or the raw installer URL is not accessible, use the local-source install in [docs/deployment-notes.md](docs/deployment-notes.md).

## 2. Edit Only What You Need

Edit `/etc/computer-mcp/config.toml`.

Most installs can keep the defaults. The installer already creates a strong random API key, default users, default paths, and the default HTTPS bind.

You usually only need to add the two GitHub App settings:

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

## 3. Place Both GitHub App Keys

Default key paths:

- reader: `/etc/computer-mcp/reader/private-key.pem`
- publisher: `/etc/computer-mcp/publisher/private-key.pem`

```bash
sudo install -d -m 0750 -o root -g computer-mcp /etc/computer-mcp/reader
sudo install -m 0640 -o root -g computer-mcp \
  /path/to/reader-app.pem \
  /etc/computer-mcp/reader/private-key.pem

sudo install -m 0600 -o computer-mcp-publisher -g computer-mcp \
  /path/to/publisher-app.pem \
  /etc/computer-mcp/publisher/private-key.pem
```

## 4. Start

```bash
zodex start
```

`zodex start` does the rest:

- checks both GitHub Apps are configured
- creates TLS artifacts if they do not exist yet
- starts the publisher daemon
- starts the MCP daemon

The installer already generated an API key. Rotate it only if you want a new one:

```bash
zodex set-key "<strong-random-key>"
```

## 5. Verify

```bash
zodex status
zodex show-url --host "<public_ip_or_host>"
curl -k "https://<public_ip_or_host>/health"
```

Expected MCP URL shape:

```text
https://<public_ip_or_host>/mcp?key=<api_key>
```

## 6. Open A PR From The Agent

After the agent has finished work in a local git checkout and committed the change:

```bash
zodex publish-pr \
  --repo amxv/computer-mcp \
  --title "Agent: example change" \
  --body "Automated change from zodex."
```

Requirements:
- run it from inside the repo checkout
- keep the worktree clean
- make sure the change is already committed on `HEAD`

## Common Commands

```bash
zodex start
zodex stop
zodex status
zodex logs
zodex publisher status
zodex publisher logs
zodex restart
```
