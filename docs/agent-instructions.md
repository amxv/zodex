# Agent Instructions

This guide explains how an agent should expect `zodex` to be used.

## Core Expectations

- You usually have read access to approved GitHub repos through the reader app.
- You should assume GitHub write access is off unless the operator explicitly grants it.
- You can still inspect code, edit files, run tests, and commit locally without write access.
- When a push is needed, the operator should grant temporary repo-scoped access.
- After the push, the operator should revoke that access again.

## Expected Coding Loop

1. clone or inspect the repo with the reader-backed HTTPS path
2. make the changes
3. run checks
4. commit locally if appropriate
5. ask for or wait for a temporary push grant
6. push normally with `git push`

## Security Model

The supported `zodex` story assumes:

- the coding runtime is not effectively root
- the writable workspace is non-root, usually `/workspace`
- the push-grant private key stays on the operator machine
- only temporary credential material reaches the Sprite during an active grant

## Product Framing

Use these names in operator-facing guidance:

- `zodex` for the operator CLI
- `zodexd` for the daemon

Legacy host details may still appear in logs, service labels, config paths, usernames, or compatibility commands:

- `/etc/computer-mcp/config.toml`
- `computer-mcpd`
- `computer-mcp-prd`
- `computer-mcp-agent`

Treat those as implementation details, not the supported product story.

## Sprite-First Bias

The default supported deployment target is Sprites.dev with the proxy-backed MCP front door.

If you see old instructions that lead with generic VPS or Runpod setup, treat them as secondary or legacy paths unless the task is explicitly about those environments.
