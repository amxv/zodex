---
title: Operator write controls
description: Activate, inspect, and revoke direct-push access from the operator machine, including one-off push grants and scoped YOLO mode for trusted ChatGPT sessions.
order: 8
category: GitHub Access
summary: The operator-machine controls for grant-push, revoke-push, mode yolo, mode status, and mode default.
---

The operator CLI controls GitHub write autonomy from outside the ChatGPT session. Use it when the human should decide exactly when direct push opens, how wide the scope is, and how long it stays active.

## One-off operator grant

The Sprite-side command is usually the clearest flow:

```bash
zodex-agent github request-push --repo amxv/zodex
```

The operator can also start the same write window remotely:

```bash
zodex github grant-push --sprite dev-sprite --repo amxv/zodex
```

When the publisher client ID is not already configured, pass it directly:

```bash
zodex github grant-push \
  --sprite dev-sprite \
  --repo amxv/zodex \
  --publisher-client-id Iv1.real-device-flow-client-id
```

The command uses the GitHub App device-flow path and places only the short-lived repo-scoped grant on the Sprite.

## YOLO mode

Use YOLO mode when repeated approvals are getting in the way and you trust the ChatGPT session for a bounded scope.

```bash
zodex github mode yolo --sprite dev-sprite
zodex github mode yolo --sprite dev-sprite --ttl 4h
zodex github mode yolo --sprite dev-sprite --repo amxv/zodex
zodex github mode yolo --sprite dev-sprite --repo amxv/zodex --repo amxv/webctx
zodex github mode yolo --sprite dev-sprite --no-ttl
```

`mode yolo` defaults to a `2h` TTL and an all-installed-repos scope. Passing `--repo` changes the scope to a repo allowlist. Repo-scoped YOLO commands merge with the active YOLO state instead of replacing other active repo grants, so each repo expires according to the TTL from the command that granted it. Passing `--no-ttl` makes the new window indefinite until the operator disables YOLO mode.

The mode state stores no GitHub token. Direct push is enabled through the zodex-managed remote helper path, while publisher-app credentials remain isolated from the agent shell.

## Inspect mode and grant state

Check YOLO mode:

```bash
zodex github mode status --sprite dev-sprite
```

List explicit grants:

```bash
zodex github list-grants --sprite dev-sprite
```

Run these when a push unexpectedly succeeds, fails, or appears to be using stale credentials.

## Return to default mode

Disable YOLO mode:

```bash
zodex github mode default --sprite dev-sprite
```

`mode default` disables only YOLO state. It leaves explicit push grants alone so one-off grant state remains visible and revocable.

## Revoke explicit grants

When a one-off write step is finished:

```bash
zodex github revoke-push --sprite dev-sprite --repo amxv/zodex
```

To clear local device-flow auth state as part of the remote revoke:

```bash
zodex github revoke-push \
  --sprite dev-sprite \
  --repo amxv/zodex \
  --forget-local-auth
```

## When this path is useful

Use operator-side controls when:

- the operator wants to control exactly when a write window opens
- a browser is easier to use on the operator machine than on the Sprite
- ChatGPT should not be responsible for requesting its own push access
- a trusted session should push repeatedly without repeated approvals
- you are debugging grant or YOLO state across multiple Sprites

The access model is unchanged: read stays available, PR publishing remains available, and direct push follows the selected operator policy.
