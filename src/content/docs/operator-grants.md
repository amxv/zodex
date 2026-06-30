---
title: Remote operator grants
description: Activate, inspect, and revoke push grants from the operator machine when the Sprite-side request flow is not the desired control point.
order: 7
category: GitHub Access
summary: The operator-machine alternative to Sprite-side request-push.
---

## Grant from the operator machine

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
zodex github grant-push   --sprite dev-sprite   --repo amxv/zodex   --publisher-client-id Iv1.real-device-flow-client-id
```

The command uses the GitHub App device-flow path and places only the temporary repo-scoped token on the Sprite.

## Operator GitHub mode

Use mode commands when the operator wants to control a broader direct-push window from their machine:

```bash
zodex github mode yolo --sprite dev-sprite
zodex github mode yolo --sprite dev-sprite --ttl 4h
zodex github mode yolo --sprite dev-sprite --repo amxv/zodex
zodex github mode status --sprite dev-sprite
zodex github mode default --sprite dev-sprite
```

`mode yolo` defaults to a `2h` TTL and an all-installed-repos scope. Passing `--repo` changes the scope to a repo allowlist. `mode default` disables only YOLO state and leaves explicit push grants alone. The mode state stores no token, and publisher-app tokens must stay inside the publisher daemon or token-isolated push proxy.

## Revoke remotely

When the write step is finished:

```bash
zodex github revoke-push --sprite dev-sprite --repo amxv/zodex
```

To clear local device-flow auth state as part of the remote revoke:

```bash
zodex github revoke-push   --sprite dev-sprite   --repo amxv/zodex   --forget-local-auth
```

## Inspect remote grants

```bash
zodex github list-grants --sprite dev-sprite
```

Run this when a push unexpectedly succeeds, fails, or appears to be using stale credentials.

## When this path is useful

Use operator-side grants when:

- the operator wants to control exactly when a write window opens
- a browser is easier to use on the operator machine than on the Sprite
- the Sprite-side device flow cannot open or copy the verification information
- you are debugging grant state across multiple Sprites

The access model is unchanged: read stays available, writes are temporary, and revoke is part of the workflow.
