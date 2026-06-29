---
title: Quickstart
description: Install the zodex runtime on a Sprite, expose the MCP URL, and complete the first clone, edit, commit, push, and revoke cycle.
order: 1
category: Start
summary: The shortest safe path from a configured Sprite to a complete remote coding session.
---

## What zodex gives an agent

`zodex` is a remote coding runtime for Sprites.dev. It gives an AI coding agent a real Linux workspace and a small MCP tool surface:

```text
exec_command
write_stdin
apply_patch
```

The agent can clone repositories, inspect files, edit code, run tests, and create local commits. GitHub write access is not available by default. Push access is activated only through an explicit temporary grant for one repository.

## The first setup

Run setup from the operator machine. This installs the guest runtime on the Sprite and configures the reader and push-grant GitHub Apps:

```bash
zodex sprite setup   --sprite dev-sprite   --repo amxv/zodex   --reader-app-id 123456   --reader-pem /secure/zodex/reader.pem   --publisher-app-id 987654   --publisher-pem /secure/zodex/push-grant.pem   --default-base main   --url-auth sprite
```

For a Sprite in a named organization, add:

```bash
--org engineering
```

After setup, the Sprite contains the guest-side binaries `zodex-agent`, `zodexd`, and `zodex-prd`. The full `zodex` operator CLI stays on the operator machine.

## Verify the install

Check the Sprite services and proxy origin before connecting an MCP client:

```bash
zodex sprite status --sprite dev-sprite
zodex sprite logs --sprite dev-sprite --service zodexd --lines 100
zodex proxy inspect --sprite dev-sprite
zodex proxy verify-origin --sprite dev-sprite
```

The expected runtime has `zodexd` and `zodex-prd` running, `/health` returning `{"status":"ok"}`, and a reachable proxy-backed `/mcp` route.

## Connect an MCP client

On the Sprite, use the agent helper to print the MCP URL for the public host:

```bash
zodex-agent show-url --host dev-sprite.example.net
```

The URL contains the API key as the `key` query parameter. Treat it as a secret. Logs and status output redact query keys where the runtime handles URL rendering.

## First coding loop

Inside the agent workspace:

```bash
cd /workspace
git clone https://github.com/amxv/zodex.git
cd zodex
# inspect, edit, test
git status
git commit -m "Improve runtime docs"
```

The reader app keeps clone and fetch access available. `publish-pr` can publish a generated branch through the publisher daemon without a direct push grant; plain `git push` still fails until a push grant is active.

## First push grant

When the local commit is ready, request a repository-scoped grant from the Sprite:

```bash
zodex-agent github request-push --repo amxv/zodex
```

Approve the GitHub device-flow code in the browser. The grant defaults to a `30m` TTL and is stored locally for the credential-helper path.

Then push normally:

```bash
git push origin main
```

Revoke immediately after the write step:

```bash
zodex-agent github revoke-push --repo amxv/zodex
```
