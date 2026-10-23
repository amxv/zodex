---
title: "Tools"
description: "Learn Zodex's three shared execution tools and the optional Local-only Liveboard alias tool."
order: 2
category: Reference
summary: "The shared ChatGPT-facing surface: exec_command, write_stdin, and apply_patch."
---

Zodex exposes the same three execution tools in Sprite and Local modes:

```text
exec_command
write_stdin
apply_patch
```

Local also offers the optional `set_agent_alias` tool to label the current ChatGPT conversation in Liveboard. The infrastructure and permissions around the shared execution tools differ by mode, but the coding workflow is the same.

## `set_agent_alias` (Local only)

Assign a short, descriptive **2–7-word** display label for the current Agent, such as `Rindle implementation`, and update it when the main task changes. The tool has a single input:

```text
alias    required string (max 80 characters; empty string clears the alias)
```

Using this tool is encouraged but **not required**. If it is the first tool call, Local assigns the conversation its normal four-character Agent ID without recording a timeline invocation. If you call `exec_command`, `apply_patch`, or `write_stdin` first instead, everything works as usual and Liveboard displays the Agent ID until it is named.

The Agent ID is resolved from conversation metadata, not passed as a model-visible input. Agent alias updates and manual pencil-button edits are persistent, **last-write-wins**, and reflected immediately in every connected Liveboard. Alias changes do not appear in the execution timeline or affect process routing, permissions, or history. This tool is not available in Sprite mode.

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

When a valid multi-file patch contains stale context in one file, Zodex still
applies independent file operations that match. The MCP error text lists the
files already changed and the specific operations that failed. **Retry only
the failed file operations** after reading their current content. A successful
file operation is not rolled back because a different file failed.

Within one file, Zodex only recovers out-of-order edits when all original
regions match uniquely and do not overlap. Ambiguous context stays an error;
Zodex never guesses which occurrence the agent intended. Malformed patch
syntax is rejected before applying any file operations.

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
