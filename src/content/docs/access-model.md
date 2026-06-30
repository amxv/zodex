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
- Git push fails until a repo-scoped grant is active or an operator-controlled GitHub mode is active through the token-isolated push path.
- PR creation through `zodex-agent github publish-pr` works without a push grant by sending a bundle of the current committed `HEAD` to the publisher daemon, which pushes a generated branch and opens the PR.
- Revocation removes the direct-push path while read access and `publish-pr` remain available.

## Reader app permissions

The reader app should have:

```text
Contents: Read-only
```

zodex still mints separate installation tokens for separate jobs: clone and fetch request only `Contents: read`; `publish-pr` uses the publisher daemon, whose app has `Contents: write` and `Pull requests: write`, while the token stays inside that daemon. Install the app on `Only select repositories`.

## Publisher / push-grant app permissions

The publisher / push-grant app should have:

```text
Contents: Read & write
Pull requests: Read & write
Device Flow enabled
User access token expiration enabled
```

It should also be installed on `Only select repositories`. The publisher daemon uses this app installation to push generated PR branches and open PRs without exposing a write token to the agent shell. The device-flow user authorization is used only when an operator approves a temporary direct-push grant.

## Operator GitHub modes

The full operator CLI can record GitHub mode state on a Sprite:

```bash
zodex github mode yolo --sprite dev-sprite
zodex github mode default --sprite dev-sprite
zodex github mode status --sprite dev-sprite
```

`mode yolo` is not an agent command. It defaults to a `2h` TTL and all installed repositories, or one or more explicit `--repo` allowlist entries. `mode default` removes YOLO state and does not revoke explicit push grants. Direct push needs a token-isolated publisher daemon/proxy path.

## Local work before remote writes

The agent can complete almost the entire coding loop before a grant exists:

```bash
cd /workspace/zodex
git status
cargo test
git add src/content/docs
git commit -m "Improve zodex docs"
```

`publish-pr` publishes a generated branch through the publisher daemon. Direct `git push` still needs `request-push` or `grant-push`.

## Grant storage and expiration

`zodex-agent github request-push` writes a local repo-scoped grant for the Git credential helper. The default TTL is `30m`.

Expired grants stop working in the credential-helper path even if an old grant file is still present. That makes expiration an enforcement boundary, not just a display value.

## Refresh-token cache

By default, Sprite-side `request-push` does not persist refresh-token state. Add `--cache-refresh-token` only when the operator deliberately wants local refresh reuse on the Sprite.

Revocation normally removes the active repo grant. Add `--forget-local-auth` when you also want to clear cached device-flow auth state for that repo.
