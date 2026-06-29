---
title: Access model
description: Learn how zodex separates always-on read access from temporary write grants, and which GitHub App permissions belong in each role.
order: 4
category: GitHub Access
summary: The security boundary behind reader apps, push-grant apps, local commits, short-lived grants, PR creation, and revocation.
---

## Default state

zodex assumes an agent should be able to work productively without standing write credentials.

The default state is:

- Git clone and fetch work through the reader GitHub App.
- Shell execution, patching, tests, and local commits work in the Sprite workspace.
- Git push fails until a repo-scoped grant is active.
- PR creation through `zodex-agent github create-pr` requires the same active grant.
- Revocation removes the write path while read access remains available.

## Reader app permissions

The reader app should have only:

```text
Contents: Read-only
```

Install it on `Only select repositories`. If the reader app has write permission, stop and fix it before installing the runtime.

## Push-grant app permissions

The push-grant app should have:

```text
Contents: Read & write
Pull requests: Read & write
Device Flow enabled
User access token expiration enabled
```

It should also be installed on `Only select repositories`. The device-flow user authorization is what lets an operator approve a temporary push grant without putting a permanent write token into the agent environment.

## Local work before remote writes

The agent can complete almost the entire coding loop before a grant exists:

```bash
cd /workspace/zodex
git status
cargo test
git add src/content/docs
git commit -m "Improve zodex docs"
```

Only the network write step needs `request-push` or `grant-push`.

## Grant storage and expiration

`zodex-agent github request-push` writes a local repo-scoped grant for the Git credential helper. The default TTL is `30m`.

Expired grants stop working in the credential-helper path even if an old grant file is still present. That makes expiration an enforcement boundary, not just a display value.

## Refresh-token cache

By default, Sprite-side `request-push` does not persist refresh-token state. Add `--cache-refresh-token` only when the operator deliberately wants local refresh reuse on the Sprite.

Revocation normally removes the active repo grant. Add `--forget-local-auth` when you also want to clear cached device-flow auth state for that repo.
