---
title: "Tools"
description: "Learn the three tools ChatGPT gets from Zodex and the command, process-session, workdir, and patch patterns that work in both Sprite and Local modes."
order: 2
category: Reference
summary: "The shared ChatGPT-facing surface: exec_command, write_stdin, and apply_patch."
---

Zodex exposes exactly three ChatGPT-facing MCP tools in both Sprite and Local modes:

```text
exec_command
write_stdin
apply_patch
```

The infrastructure and permissions around those tools differ by mode, but the coding workflow is the same.

## `exec_command`

Runs a command in an explicit absolute existing directory.

Inputs:

```text
cmd            required string
workdir        required absolute existing directory
yield_time_ms  optional
timeout_ms     optional
```

Typical request conceptually:

```text
cmd: git status --short --branch
workdir: /absolute/path/to/repo
```

If the command finishes during the initial yield window, Zodex returns its final output/status immediately.

If it is still running, Zodex returns a random `session_handle`. The Agent then uses `write_stdin` to continue it.

Command output is bounded for model delivery. When a command exceeds the inline output limit, Zodex keeps the tool call responsive, writes the decoded PTY stream to a private temporary file, and returns a short tail preview plus `output_file`, `output_chars`, `output_lines`, and `output_file_truncated`. The Agent can inspect that file with later tool calls instead of forcing a multi-megabyte MCP result through the model context. Spill files are capped at 16 MiB each and stale files are cleaned up after 24 hours; if a command exceeds the file cap, the saved file contains the first 16 MiB while the returned tail preview still reflects the latest output and the counts describe the full stream.

There is no backend cwd/default-workdir substitution when the field is missing. The model is expected to send the intended workdir every time.

In Sprite mode, that path is inside the remote Linux workspace.

In **Local mode**, commands run as the trusted logged-in host user with the captured developer environment. Linux and macOS use the captured login shell; Windows uses Windows PowerShell. The Local start directory is supplied to ChatGPT as guidance for the first explicit workdir, not as a filesystem boundary.

Local may add model-visible context to the primary tool result. By default the first result in a ChatGPT conversation includes the user's Codex-style global `AGENTS` instructions and global skill catalog, and the first successful invocation in a workdir may include a one-line `AGENTS.md`/`AGENTS.override.md` hint plus a compact catalog of skills found directly under `<workdir>/.agents/skills`. `exec_command` and `write_stdin` expose this through an optional `zodex_context` structured field; text/error results keep the original result first and append context in the same text block. Stdout, status, cwd, exit code, and stored invocation evidence remain unchanged. See [Local configuration](/docs/local/configuration#automatic-codex-style-context) to change or disable each part.

## `write_stdin`

Continues a process created by `exec_command`.

Inputs:

```text
session_handle  required
chars           optional
yield_time_ms   optional
kill_process    optional
```

### Poll a long command

Send no characters and no kill flag. Zodex waits for more output or exit and returns the current state.

### Send input

For an interactive prompt:

```text
chars: "y\n"
```

### Kill it

```text
kill_process: true
```

A session handle is a process continuation capability. It is not a ChatGPT conversation ID.

Local records which Agent created/called a process for observability, but Agent identity is not an authorization layer for `write_stdin` in the current Local design.

## `apply_patch`

Applies a Codex-style patch in an explicit absolute existing workdir.

Inputs:

```text
patch    required string
workdir  required absolute existing directory
```

Use it for targeted source/document edits instead of constructing fragile shell replacement commands.

The declared workdir is routing context, not confinement: a deliberately absolute path in a supported patch/command can still reach elsewhere if the deployment's OS permissions allow it.

## Workdir rules

For both `exec_command` and `apply_patch`:

- the field is required;
- it must be absolute;
- the directory must exist;
- invalid workdir input fails before the command/patch side effect.

This makes one shared Zodex server safe to route explicitly across different repos/worktrees without guessing a caller's intended cwd.

## What the tools do not expose

There is no ChatGPT-facing MCP tool for:

- starting/stopping Local;
- changing Local config;
- reading Local history;
- managing the OpenAI tunnel;
- granting Sprite GitHub push/YOLO permissions;
- operating Sprite infrastructure.

Those are operator/CLI concerns. ChatGPT gets the coding surface, not the deployment control plane.

## Permissions depend on the mode

### Sprite

Commands run inside the remote Sprite. GitHub clone/fetch and direct push availability are determined by the Sprite's GitHub Apps and active [write mode](/docs/sprite/write-modes).

### Local

Commands run with the logged-in host user's normal permissions. There is no Zodex repo sandbox around the tool. macOS privacy controls and Windows filesystem/profile permissions remain authoritative.

See [Local](/docs/local) for the trusted-host boundary.
