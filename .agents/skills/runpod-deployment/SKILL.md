---
name: runpod-deployment
description: Use when updating, verifying, or rolling out zodex on Runpod, including deciding between binary-only upgrades and full Runpod image/template/pod rollouts.
---

# Runpod Deployment

Runpod is a legacy compatibility deployment surface in this repo, not the primary supported product path.

Use `zodex` and `zodexd` in operator-facing guidance. Keep legacy `computer-mcp` identifiers only where they are still the live image, repo, service, or compatibility names.

Use this skill when the target host is Runpod and the task involves any of:

- updating the dedicated Runpod image
- updating a Runpod template and pod
- verifying a live Runpod pod
- rolling out a new binary-only `computer-mcp` release on an existing pod
- smoke testing the `computer` CLI against a live Runpod deployment

Read these files first:

- [`scripts/runpod_api.py`](../../../scripts/runpod_api.py)
- [`Dockerfile.runpod`](../../../Dockerfile.runpod)
- [`docker/runpod-bootstrap.sh`](../../../docker/runpod-bootstrap.sh)
- [`docker/runpod-run.sh`](../../../docker/runpod-run.sh)
- [`docs/github-app-agent-auth.md`](../../../docs/github-app-agent-auth.md)

Hard rules:

- Do not hardcode live template IDs, pod IDs, public IPs, SSH ports, or real MCP URLs in the public repo.
- Prefer `scripts/runpod_api.py` over ad hoc `curl` for normal Runpod operations.
- Prefer direct SSH to the pod public IP + mapped port. Do not rely on the `ssh.runpod.io` gateway for non-interactive automation.
- On Runpod, use `root` for rollout operations and `computer-mcp-agent` for non-root interactive coding checks.

## Runpod Model In This Repo

This repo has a validated dedicated Runpod image family:

```text
ghcr.io/amxv/computer-mcp-runpod
```

That image is separate from the generic image:

- `ghcr.io/amxv/computer-mcp` is the generic VPS/container image
- `ghcr.io/amxv/computer-mcp-runpod` is the dedicated Runpod template image

The validated Runpod image uses:

- base image: `runpod/base:1.0.3-ubuntu2204`
- `/start.sh` from the Runpod base image for pod services
- [`docker/runpod-run.sh`](../../../docker/runpod-run.sh) to launch Runpod services first
- [`docker/runpod-bootstrap.sh`](../../../docker/runpod-bootstrap.sh) to create users, write env/config, fix ownership, and start `computer-mcp`

Runpod commonly does not expose a usable `systemd`. On Runpod, `computer-mcp` should usually be expected to run in process mode:

- `computer-mcpd` under `computer-mcp-agent`
- `computer-mcp-prd` under `computer-mcp-publisher`

The validated image also provisions:

- direct SSH login for both `root` and `computer-mcp-agent`
- `/workspace` owned by `computer-mcp-agent`
- Node, Bun, Python, Go, Rust, and common Unix/dev tools on login
- user-writable install paths for `pip`, `uv`, `npm`, `go install`, and `cargo install`

The security split from [`docs/github-app-agent-auth.md`](../../../docs/github-app-agent-auth.md) still applies:

- do not run the agent daemon as `root`
- do not give the agent unrestricted `sudo`
- keep the publisher key readable only by `computer-mcp-publisher`
- the narrow write bridge is still `computer-mcp publish-pr`

## Public URL And SSH Shape

Preferred public MCP URL shape on Runpod:

```text
https://<pod-id>-8080.proxy.runpod.net/mcp?key=<api_key>
```

Preferred direct SSH shape:

```bash
ssh -p <runpod_tcp_port_22> root@<runpod_public_ip> -i ~/.ssh/id_ed25519 '<command>'
```

Do not rely on `ssh.runpod.io` for non-interactive automation.

The direct SSH port may change after a pod reset. Rediscover it with:

```bash
python3 scripts/runpod_api.py pod get <pod-id>
```

## Fresh Dedicated Runpod Template Setup

Use this when you want the dedicated prebuilt image instead of installing manually on a generic pod.

### GHCR Visibility Note

GitHub Container Registry packages often start out private even for public repos. If a newly published Runpod image cannot be pulled by Runpod, check the GHCR package visibility once and make the package public.

### Recommended Template Settings

Template basics:

- Name: whatever you want locally; do not commit live names or IDs
- Image name: `ghcr.io/amxv/computer-mcp-runpod:latest` or a specific `vX.Y.Z`
- Container start command: leave blank to use the image default
- Visibility: private for your own account/team unless you intentionally want a public template

Storage:

- Container disk: `40 GB`
- Volume disk: `20 GB`
- Volume mount path: `/workspace`

Ports:

- HTTP: `8080/http`
- TCP: `22/tcp`
- direct TCP `443` is optional debug-only access

Compute:

- CPU pod is the normal lightweight coding setup
- Preferred CPU flavor: `cpu3c-2-4` (`4 GB RAM`, `2 vCPU`, `5 GB` total disk on the base pod SKU shown in Runpod)

### Recommended Environment Variables

Preferred full-config path:

- `COMPUTER_MCP_AUTO_START=1`
- `COMPUTER_MCP_FORCE_RECONFIGURE=1`
- `COMPUTER_MCP_PUBLIC_HOST=<pod-id>-8080.proxy.runpod.net`
- `COMPUTER_MCP_CONFIG_TOML={{ secret }}`
- `COMPUTER_MCP_READER_PRIVATE_KEY={{ secret }}`
- `COMPUTER_MCP_PUBLISHER_PRIVATE_KEY={{ secret }}`

Alternative per-field path:

- `COMPUTER_MCP_AUTO_START=1`
- `COMPUTER_MCP_FORCE_RECONFIGURE=1`
- `COMPUTER_MCP_HTTP_BIND_PORT=8080`
- `COMPUTER_MCP_PUBLIC_HOST=<pod-id>-8080.proxy.runpod.net`
- `COMPUTER_MCP_API_KEY=<strong-random-key>`
- `COMPUTER_MCP_READER_APP_ID=<reader_app_id>`
- `COMPUTER_MCP_READER_INSTALLATION_ID=<reader_installation_id>`
- `COMPUTER_MCP_READER_PRIVATE_KEY={{ secret }}`
- `COMPUTER_MCP_PUBLISHER_APP_ID=<publisher_app_id>`
- `COMPUTER_MCP_PUBLISHER_INSTALLATION_ID=<publisher_installation_id>`
- `COMPUTER_MCP_PUBLISHER_TARGET_REPO=owner/repo`
- `COMPUTER_MCP_PUBLISHER_DEFAULT_BASE=main`
- `COMPUTER_MCP_PUBLISHER_PRIVATE_KEY={{ secret }}`

If `COMPUTER_MCP_PUBLIC_HOST` is omitted and Runpod provides `RUNPOD_POD_ID`, the bootstrap derives:

```text
<pod-id>-8080.proxy.runpod.net
```

## Fresh Generic Runpod Pod Install

Use this only if you are not using the dedicated Runpod image and need to install `computer-mcp` onto a generic Runpod pod manually.

1. Configure pod networking in Runpod:
   - HTTP `8080`
   - TCP `22`
   - treat direct TCP `443` as debug-only
2. Create the reader and publisher GitHub Apps and collect their installation IDs.
3. Define local variables:

```bash
export VPS_HOST="<runpod_public_ip>"
export VPS_PORT="<runpod_ssh_port>"
export VPS_USER="root"
export VPS_KEY="$HOME/.ssh/id_ed25519"
export RUNPOD_PROXY_HOST="<pod-id>-8080.proxy.runpod.net"
export TARGET_REPO="owner/repo"
export READER_APP_ID="<reader_app_id>"
export READER_INSTALLATION_ID="<reader_installation_id>"
export READER_PEM="/absolute/path/to/reader.pem"
export PUBLISHER_APP_ID="<publisher_app_id>"
export PUBLISHER_INSTALLATION_ID="<publisher_installation_id>"
export PUBLISHER_PEM="/absolute/path/to/publisher.pem"
```

4. Define a helper:

```bash
vps_ssh() {
  ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null \
    -p "$VPS_PORT" "$VPS_USER@$VPS_HOST" -i "$VPS_KEY" "$@"
}
```

5. Install with the Runpod HTTP listener + public host hint:

```bash
vps_ssh "export COMPUTER_MCP_HTTP_BIND_PORT=8080 COMPUTER_MCP_PUBLIC_HOST=\"$RUNPOD_PROXY_HOST\"; curl -fsSL https://raw.githubusercontent.com/amxv/computer-mcp/main/scripts/install.sh | bash"
```

Why this path is preferred on Runpod:

- internal `8080` is plain HTTP for the Runpod proxy
- public HTTPS is terminated by Runpod on the proxy hostname
- ChatGPT sees a normal `https://...proxy.runpod.net/...` URL on standard `443`

6. Upload the PEMs:

```bash
cat "$READER_PEM" | vps_ssh 'cat > /root/computer-mcp-reader.pem'
cat "$PUBLISHER_PEM" | vps_ssh 'cat > /root/computer-mcp-publisher.pem'
```

7. Install them into final paths:

```bash
vps_ssh '
install -m 0600 -o computer-mcp-agent -g computer-mcp \
  /root/computer-mcp-reader.pem /etc/computer-mcp/reader/private-key.pem
install -m 0600 -o computer-mcp-publisher -g computer-mcp \
  /root/computer-mcp-publisher.pem /etc/computer-mcp/publisher/private-key.pem
'
```

8. Write `/etc/computer-mcp/config.toml` with the Runpod HTTP listener enabled:

```bash
vps_ssh "cat > /etc/computer-mcp/config.toml <<'EOF'
api_key = \"<existing_or_new_api_key>\"
http_bind_port = 8080
reader_app_id = ${READER_APP_ID}
reader_installation_id = ${READER_INSTALLATION_ID}
publisher_app_id = ${PUBLISHER_APP_ID}

[[publisher_targets]]
id = \"${TARGET_REPO}\"
repo = \"${TARGET_REPO}\"
default_base = \"main\"
installation_id = ${PUBLISHER_INSTALLATION_ID}
EOF
chgrp computer-mcp /etc/computer-mcp/config.toml
chmod 0640 /etc/computer-mcp/config.toml"
```

If you want to preserve the installer-generated API key, read it first:

```bash
vps_ssh "sed -n 's/^api_key = \"\\(.*\\)\"$/\\1/p' /etc/computer-mcp/config.toml"
```

9. Start the stack:

```bash
vps_ssh 'computer-mcp start'
```

10. Verify local health on the pod:

```bash
vps_ssh 'computer-mcp status'
vps_ssh 'computer-mcp publisher status'
vps_ssh 'curl -fsS http://127.0.0.1:8080/health'
vps_ssh 'curl -kfsS https://127.0.0.1/health'
```

Expected local health body:

```json
{"status":"ok"}
```

11. Verify the public Runpod URL:

```bash
curl "https://${RUNPOD_PROXY_HOST}/health"
```

12. Verify a real MCP `initialize`:

```bash
curl -sS -D - "https://${RUNPOD_PROXY_HOST}/mcp?key=<api_key>" \
  -H 'Content-Type: application/json' \
  -H 'Accept: application/json, text/event-stream' \
  --data '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"curl","version":"0.1"}}}'
```

## Runpod Helper Script

The repo includes an official-API helper:

```bash
python3 scripts/runpod_api.py ...
```

It talks directly to `https://rest.runpod.io/v1` and falls back to the macOS keychain for `RUNPOD_API_KEY`.

Common commands:

```bash
python3 scripts/runpod_api.py template create
python3 scripts/runpod_api.py template update <template-id>
python3 scripts/runpod_api.py template get <template-id>
python3 scripts/runpod_api.py pod create
python3 scripts/runpod_api.py pod get <pod-id>
python3 scripts/runpod_api.py pod restart <pod-id>
python3 scripts/runpod_api.py pod wait-ready <pod-id>
python3 scripts/runpod_api.py pod verify <pod-id>
python3 scripts/runpod_api.py rollout-image <template-id> <pod-id> --verify
```

Important defaults:

- `RUNPOD_API_KEY` falls back to the macOS keychain item `RUNPOD_API_KEY`
- reader and publisher PEMs fall back to the newest matching key files in `~/Downloads`
- `SSH_PUBLIC_KEY` falls back to `~/.ssh/id_ed25519.pub`
- the default image is `ghcr.io/amxv/computer-mcp-runpod:v<repo-version>`

Update behavior:

- `template update <template-id>` preserves the current template settings by default and only changes fields you explicitly override, such as `RUNPOD_IMAGE`
- `pod update <pod-id>` preserves the current pod settings by default and only changes fields you explicitly override
- `rollout-image <template-id> <pod-id>` is the preferred image rollout helper; it updates the template, updates the pod, resets the pod, and can verify readiness in one step while reusing the current env/config from the live pod
- `--from-template-id` and `--from-pod-id` intentionally clone env/config from another existing template or pod

## Decide The Rollout Type

Use the binary-only path when the change is only in the Rust binaries and does not change the container/runtime layer.

Typical binary-only examples:

- `src/`
- `tests/`
- CLI / daemon behavior changes
- MCP / HTTP API / `computer` CLI behavior

Use the full image rollout path when the container environment changed.

Typical image/runtime examples:

- `Dockerfile.runpod`
- `docker/runpod-bootstrap.sh`
- `docker/runpod-run.sh`
- system packages or toolchain installs
- SSH bootstrap or account provisioning
- template env, port, storage, or image assumptions

## Binary-Only Rollout

This is the preferred fast path for normal server changes.

1. Cut a normal tagged release such as `v0.1.21`.
2. Wait only for the GitHub `release` workflow.
3. Discover current pod metadata with:

```bash
python3 scripts/runpod_api.py pod get <pod-id>
```

4. SSH to the pod as `root` and upgrade in place:

```bash
ssh -p <runpod_tcp_port_22> root@<runpod_public_ip> -i ~/.ssh/id_ed25519 \
  'computer-mcp upgrade --version vX.Y.Z'
```

5. Verify:

```bash
ssh -p <runpod_tcp_port_22> root@<runpod_public_ip> -i ~/.ssh/id_ed25519 \
  'computer-mcp --version && computer-mcp status && curl -fsS http://127.0.0.1:8080/health'
curl "https://<pod-id>-8080.proxy.runpod.net/health"
```

Why this works:

- `computer-mcp upgrade --version vX.Y.Z` is tag-pinned
- the existing pod config is preserved
- the same pod ID keeps the same proxy hostname

Do not wait for `container-release` for this path.

## Full Image Rollout

Use this when the Runpod image or template/runtime changed.

1. Cut the release and wait for `container-release`.
2. Roll the existing template and pod forward with the helper:

```bash
python3 scripts/runpod_api.py rollout-image <template-id> <pod-id> --verify
```

Important behavior:

- `RUNPOD_IMAGE` defaults to `ghcr.io/amxv/computer-mcp-runpod:v<repo-version>`
- `rollout-image` reuses the current pod env/config by default, so you do not need to manually re-extract PEMs or TOML first
- `--env-source template` switches the rollout source from the live pod to the current template
- `--dry-run` shows the planned update/reset operations

If you need the lower-level path, these commands now preserve each object's current settings by default:

```bash
python3 scripts/runpod_api.py template update <template-id>
python3 scripts/runpod_api.py pod update <pod-id>
python3 scripts/runpod_api.py pod reset <pod-id>
python3 scripts/runpod_api.py pod wait-ready <pod-id>
python3 scripts/runpod_api.py pod verify <pod-id>
```

Use `--from-pod-id` or `--from-template-id` on `template create/update` and `pod create/update` when you intentionally want to clone env/config from another existing object.

## Minimum Verification

After any rollout, verify all of:

1. local service status:

```bash
ssh -p <runpod_tcp_port_22> root@<runpod_public_ip> -i ~/.ssh/id_ed25519 \
  'computer-mcp --version && computer --version && computer-mcp status'
```

2. local pod health:

```bash
ssh -p <runpod_tcp_port_22> root@<runpod_public_ip> -i ~/.ssh/id_ed25519 \
  'curl -fsS http://127.0.0.1:8080/health'
```

3. public proxy health:

```bash
curl "https://<pod-id>-8080.proxy.runpod.net/health"
```

4. a real MCP initialize:

```bash
curl -sS -D - "https://<pod-id>-8080.proxy.runpod.net/mcp?key=<api_key>" \
  -H 'Content-Type: application/json' \
  -H 'Accept: application/json, text/event-stream' \
  --data '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"curl","version":"0.1"}}}'
```

## `computer` CLI Smoke Test

Use this when the HTTP API or `computer` CLI changed, or after a full image rollout.

Set the target once:

```bash
export COMPUTER_URL="https://<pod-id>-8080.proxy.runpod.net"
export COMPUTER_KEY="<api_key>"
```

Then smoke test the three core remote operations:

```bash
computer exec-command --cmd "bash -lc 'mkdir -p /workspace/cli-smoke && printf cli-exec-ok > /workspace/cli-smoke/exec.txt && cat /workspace/cli-smoke/exec.txt'"

computer exec-command --cmd "python3 -c 'print(\"READY\", flush=True); s=input(); open(\"/workspace/cli-smoke/stdin.txt\", \"w\").write(s + \"\\n\"); print(\"ECHO:\" + s, flush=True)'"
computer write-stdin --session-id <session-id> --chars $'stdin-roundtrip\n'

computer apply-patch --workdir /workspace/cli-smoke --patch $'*** Begin Patch\n*** Add File: patch.txt\n+patched-from-cli\n*** End Patch\n'
```

If you need independent proof, verify the files over SSH:

```bash
ssh -p <runpod_tcp_port_22> root@<runpod_public_ip> -i ~/.ssh/id_ed25519 \
  'ls -l /workspace/cli-smoke && sed -n "1,5p" /workspace/cli-smoke/exec.txt /workspace/cli-smoke/stdin.txt /workspace/cli-smoke/patch.txt'
```

## Notes

- On Runpod, the proxy-host URL is preferred over direct TCP `443`.
- Runpod commonly runs without usable `systemd`; `computer-mcp` should be expected to operate in process mode there.
- The validated Runpod image provides direct SSH login for both `root` and `computer-mcp-agent`.
