# AGENTS.md

## Runpod

Phase 1 naming note:
- prefer `zodex` and `zodexd` in operator-facing guidance
- keep legacy `computer-mcp`, `computer`, and daemon/service names when compatibility or exact on-host identifiers matter

For Runpod-specific deployment and rollout work, use the repo-local skill at [.agents/skills/runpod-deployment/SKILL.md](.agents/skills/runpod-deployment/SKILL.md).

Keep only these policy rules in mind here:

- do not hardcode live template IDs, pod IDs, public IPs, SSH ports, or real MCP URLs in the public repo
- prefer [`scripts/runpod_api.py`](scripts/runpod_api.py) for current pod/template metadata and image rollout operations
- prefer direct SSH to the pod public IP + mapped `22/tcp` port, not the `ssh.runpod.io` gateway, for non-interactive automation
- use the binary-only release path for normal Rust/server changes and the full image rollout path only when the container/runtime layer changed

## Validation Baseline

As of March 19, 2026, `main` has a healthy local Rust gate baseline:

- `cargo test`
- `cargo clippy --all-targets -- -D warnings`

Treat unexpected failures in those commands as real regressions unless you can prove they come from an unrelated in-flight branch.

If the deployment target is a normal VM with working `systemd`, the existing CLI flow is still appropriate.
