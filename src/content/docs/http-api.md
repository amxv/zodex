---
title: HTTP API and client
description: Use zodex-client or raw HTTP calls against the direct JSON API for command execution, stdin writes, and patch application.
order: 11
category: Operations
summary: The `/v1/*` API routes, Bearer auth model, request shapes, and thin client commands.
---

## HTTP routes

In addition to MCP, `zodexd` serves direct JSON routes:

```text
POST /v1/exec-command
POST /v1/write-stdin
POST /v1/apply-patch
```

These routes use Bearer auth:

```http
Authorization: Bearer secret-runtime-key
```

The token must match `api_key` in `/etc/zodex/config.toml`.

## Thin client

`zodex-client` is a debug and automation CLI for the HTTP API:

```bash
zodex-client --url https://dev-zodex.example.net --key secret-runtime-key exec-command --cmd "pwd"
zodex-client --url https://dev-zodex.example.net --key secret-runtime-key apply-patch --workdir /workspace/zodex --patch-file /tmp/change.patch
```

It exposes these commands:

```text
connect
disconnect
exec-command
write-stdin
apply-patch
```

## exec-command request

Request shape:

```json
{
  "cmd": "cargo test --quiet",
  "workdir": "/workspace/zodex",
  "yield_time_ms": 1000,
  "timeout_ms": 7200000
}
```

Successful responses include a short `summary`, ANSI-stripped `output`, status, working directory, and exit metadata. If a command is still running, the response includes a `session_handle`.

Running response example:

```json
{
  "summary": "still running after 1.0s; use session_handle session-token to poll",
  "output": "...",
  "status": "running",
  "cwd": "/workspace/zodex",
  "session_handle": "session-token"
}
```

Exited response example:

```json
{
  "summary": "exited 0 after 0.3s",
  "output": "...",
  "status": "exited",
  "cwd": "/workspace/zodex",
  "exit_code": 0,
  "termination_reason": "exit"
}
```

## write-stdin request

Poll a running session:

```json
{
  "session_handle": "session-token",
  "yield_time_ms": 1000
}
```

Send input:

```json
{
  "session_handle": "session-token",
  "chars": "yes
",
  "yield_time_ms": 1000
}
```

Terminate a running session:

```json
{
  "session_handle": "session-token",
  "kill_process": true
}
```

## apply-patch request

```json
{
  "workdir": "/workspace/zodex",
  "patch": "*** Begin Patch
*** Update File: README.md
@@
-old
+new
*** End Patch
"
}
```

`workdir` is required. Relative paths inside the patch are resolved against that directory.

## Caller labels

The HTTP API reads `x-caller-label` when present and can fall back to `User-Agent`. That label helps identify request origin in session metadata and logs.
