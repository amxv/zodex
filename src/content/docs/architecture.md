---
title: Runtime architecture
description: Understand how the operator CLI, Sprite services, MCP server, HTTP API, agent helper, and publisher daemon fit together.
order: 2
category: Architecture
summary: The component map for the Rust binaries and services that make zodex work.
---

## Component overview

The Rust package builds five binaries:

```text
zodex         full operator CLI
zodex-agent   restricted guest-side helper for agents
zodex-client  thin HTTP API client/debug CLI
zodexd        MCP and HTTP daemon
zodex-prd     internal push-grant support daemon
```

The operator machine uses `zodex`. The Sprite guest uses `zodex-agent`, `zodexd`, and `zodex-prd`. The `zodex-client` binary exists for direct HTTP API testing and automation.

## Operator CLI

`zodex` handles setup and operations:

```bash
zodex sprite setup --sprite dev-sprite --repo amxv/zodex --reader-app-id 123456 --reader-pem /secure/zodex/reader.pem --publisher-app-id 987654 --publisher-pem /secure/zodex/push-grant.pem
zodex sprite status --sprite dev-sprite
zodex sprite logs --sprite dev-sprite --service zodexd --lines 100
zodex proxy verify-origin --sprite dev-sprite
zodex github grant-push --sprite dev-sprite --repo amxv/zodex
```

It also contains local service commands such as `install`, `start`, `stop`, `restart`, `status`, `logs`, `set-key`, `rotate-key`, and `tls setup` for direct non-Sprite service control.

## Sprite runtime

`zodexd` is the daemon that serves:

- `/health`, public health check
- `/mcp` and `/mcp/`, MCP transport behind query-key auth
- `/v1/exec-command`, `/v1/write-stdin`, and `/v1/apply-patch`, HTTP JSON endpoints behind Bearer auth

`zodex-prd` is the internal publisher-side service used by the push-grant and publishing support path. It is not exposed as an MCP tool.

## Agent helper

`zodex-agent` is deliberately smaller than the operator CLI. It forwards a restricted command set to the guest runtime helper:

```bash
zodex-agent show-url --host dev-sprite.example.net
zodex-agent github request-push --repo amxv/zodex
zodex-agent github publish-pr --repo amxv/zodex --title "Improve docs"
zodex-agent github list-grants
zodex-agent github revoke-push --repo amxv/zodex
```

The agent helper can request and revoke direct-push grants, publish PRs through the publisher daemon, print connection URLs, and serve as the Git credential helper.

## Service flow

A normal coding session looks like this:

1. MCP client connects to the proxy-backed `/mcp` route.
2. `zodexd` authenticates the `key` query parameter.
3. The agent runs shell commands through `exec_command`.
4. Long-running commands return a `session_handle`.
5. The agent polls or writes stdin through `write_stdin`.
6. File edits are applied through shell commands or `apply_patch`.
7. Git clone and fetch use reader-backed access.
8. Git push uses a temporary grant only after `request-push` or `grant-push` succeeds.

The design keeps code execution powerful while making GitHub writes explicit and time-bound.
