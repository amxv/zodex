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
