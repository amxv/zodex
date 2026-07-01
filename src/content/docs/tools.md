---
title: MCP tools
description: Document the three MCP tools exposed by zodexd, their inputs, outputs, annotations, and expected usage patterns.
order: 11
category: Reference
summary: "The exact remote tool surface: exec_command, write_stdin, and apply_patch."
---

## Tool list

`zodexd` exposes exactly three MCP tools:

```text
exec_command
write_stdin
apply_patch
```

The MCP server instructions are:

```text
zodex remote execution tools
```

No publisher or PR tool is exposed directly over MCP. GitHub push actions are intentionally routed through `zodex-agent` commands and active grants; `publish-pr` is exposed through `zodex-agent github publish-pr`; it routes branch publication and PR creation through the local publisher daemon without exposing a write token to MCP tools.

## Tool annotations

Each tool is registered with these annotations:

```text
read_only_hint = true
destructive_hint = false
open_world_hint = false
```

The annotations describe the MCP operation surface from the model/client perspective. They do not mean shell commands cannot modify files. A command like `rm` or a patch can still change the workspace. GitHub network writes remain controlled by the grant workflow.

## exec_command

Description:

```text
Run a shell command
```

Input:

```json
{
  "cmd": "cargo test --quiet",
  "workdir": "/workspace/zodex",
  "yield_time_ms": 1000,
  "timeout_ms": 7200000
}
```

Fields:

- `cmd`: shell command string
- `workdir`: optional working directory; defaults to configured `default_workdir`
- `yield_time_ms`: how long to wait before returning partial output
- `timeout_ms`: command timeout, capped by `max_exec_timeout_ms`

## write_stdin

Description:

```text
Write to or poll a running session
```

Poll:

```json
{
  "session_handle": "session-token",
  "yield_time_ms": 1000
}
```

Write input:

```json
{
  "session_handle": "session-token",
  "chars": "continue
",
  "yield_time_ms": 1000
}
```

Kill session:

```json
{
  "session_handle": "session-token",
  "kill_process": true
}
```

`session_handle` is required.

## apply_patch

Description:

```text
Apply a Codex-style patch to files
```

Input:

```json
{
  "workdir": "/workspace/zodex",
  "patch": "*** Begin Patch
*** Update File: docs/setup.md
@@
-old
+new
*** End Patch
"
}
```

`workdir` is required. Relative paths in the patch are resolved against `workdir`.

## Output model

Command-style tools return:

```json
{
  "summary": "still running after 1.0s; use session_handle session-token to poll",
  "output": "...",
  "status": "running",
  "cwd": "/workspace/zodex",
  "session_handle": "session-token"
}
```

or, after exit:

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

`output` is stripped of ANSI color/control escape sequences before it is returned. `summary` is a single scan-friendly line such as `exited 1 after 0.2s`, `still running after 30.1s; use session_handle session-token to poll`, `timed out after 120.0s`, or `killed after 12.6s`. `termination_reason` can be `exit`, `timeout`, or `killed`.
