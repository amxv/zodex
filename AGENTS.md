# AGENTS.md

## Product Story

Treat the supported product story as:

- product and operator CLI: `zodex`
- daemon: `zodexd`
- primary deployment target: Sprites.dev
- default access model: reader GitHub App for read, temporary repo-scoped `zodex github grant-push` for write
- default public front door for Sprite deployments: the proxy-backed MCP URL

Keep the repo, docs, and operator guidance centered on the current `zodex` surface only.

## Validation Baseline

As of March 19, 2026, `main` has a healthy local Rust gate baseline:

- `cargo test`
- `cargo clippy --all-targets -- -D warnings`

Treat unexpected failures in those commands as real regressions unless you can prove they come from an unrelated in-flight branch.

## Default Access Model

When updating operator or setup guidance, treat this as the supported product story:

- read access comes from the reader GitHub App
- write access is temporary and repo-scoped through `zodex github grant-push`
- `zodex github revoke-push` turns that write access back off
- Sprite deployments should assume the proxy-backed MCP front door by default
