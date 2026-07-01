# AGENTS.md

## Product Story

Treat the supported product story as:

- product and operator CLI: `zodex`
- daemon: `zodexd`
- primary client focus: ChatGPT MCP sessions
- primary deployment target: Sprites.dev
- default public front door for Sprite deployments: the proxy-backed MCP URL
- default access model: reader GitHub App for read, PR publishing without direct shell write tokens, and operator-chosen write modes for direct push
- write modes: PR-only, agent-requested push, operator-granted push, timed YOLO, repo-scoped YOLO, and no-TTL YOLO for trusted sessions

Keep the repo, docs, and operator guidance centered on the current `zodex` surface only. Position zodex as a ChatGPT-native remote coding workspace: the MCP tools intentionally resemble the command, stdin, and patch surfaces GPT models already know how to use well.

## Validation Baseline

As of March 19, 2026, `main` has a healthy local Rust gate baseline:

- `cargo test`
- `cargo clippy --all-targets -- -D warnings`

Treat unexpected failures in those commands as real regressions unless you can prove they come from an unrelated in-flight branch.

## Default Access Model

When updating operator or setup guidance, treat this as the supported product story:

- read access comes from the reader GitHub App
- PR publishing goes through `zodex-agent github publish-pr` and keeps publisher credentials inside `zodex-prd`
- one-off direct push can be opened with `zodex-agent github request-push` or `zodex github grant-push`
- trusted direct-push sessions can be opened by the operator with `zodex github mode yolo`
- YOLO mode can be scoped to all installed repos or one or more `--repo` allowlist entries
- YOLO mode defaults to a TTL, can be changed with `--ttl`, and can be made indefinite with `--no-ttl`
- `zodex github mode default` turns YOLO mode off
- `zodex github revoke-push` turns explicit push grants back off
- Sprite deployments should assume the proxy-backed MCP front door by default
