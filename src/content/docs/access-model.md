---
title: Access and autonomy model
description: Learn how zodex separates read access, local workspace writes, PR publishing, one-off push grants, and scoped YOLO mode for ChatGPT sessions.
order: 5
category: GitHub Access
summary: The security boundary behind reader apps, publisher apps, local commits, PR-only mode, push grants, YOLO mode, TTLs, repo scopes, and revocation.
---

zodex separates three things that are often blurred together:

- **workspace writes**: ChatGPT can edit files, run tests, and commit inside the Sprite
- **GitHub reads**: ChatGPT can clone and fetch through the reader app
- **GitHub writes**: ChatGPT can push only when the selected write mode allows it

That separation is what lets the same MCP connection support careful review-first sessions and trusted YOLO sessions.

## Default state

The default state is productive but conservative:

- Git clone and fetch work through the reader GitHub App.
- Shell execution, patching, tests, and local commits work in the Sprite workspace.
- PR creation through `zodex-agent github publish-pr` works without a direct push grant.
- Plain `git push` fails until a repo-scoped grant or operator-controlled YOLO mode is active.
- Revocation or expiration closes direct push while read access and PR publishing remain available.

In other words: ChatGPT can do real work immediately, but GitHub write autonomy is a policy choice.

## Reader app permissions

The reader app should have:

```text
Contents: Read-only
```

Install it on `Only select repositories` unless broader read access is intentional. zodex mints clone/fetch tokens with read-only contents access.

## Writer app permissions

The writer app is called the publisher / push-grant app in config because it powers both PR publishing and direct-push grants.

It should have:

```text
Contents: Read & write
Pull requests: Read & write
Device Flow enabled
User access token expiration enabled
```

Install it on `Only select repositories` unless every installed repo is meant to be eligible for PR publishing, push grants, or YOLO mode.

## PR publishing without shell write tokens

`publish-pr` is the review-first write path:

```bash
zodex-agent github publish-pr --repo owner/repo --title "Describe the change" --base main
```

The agent commits locally, then `zodex-agent` sends a bundle of the current `HEAD` to `zodex-prd`. The publisher daemon mints short-lived writer-app credentials, pushes a generated branch, and opens the PR. Those credentials stay inside the daemon.

## Direct push grants

For one-off direct push, open a repo-scoped grant:

```bash
zodex-agent github request-push --repo owner/repo
```

or from the operator machine:

```bash
zodex github grant-push --sprite dev-sprite --repo owner/repo
```

The default active grant TTL is `30m`. Expired grants stop working in the credential-helper path even if a stale grant file remains.

## Operator YOLO mode

YOLO mode is the trusted-session write path:

```bash
zodex github mode yolo --sprite dev-sprite
zodex github mode yolo --sprite dev-sprite --ttl 4h
zodex github mode yolo --sprite dev-sprite --repo owner/repo
zodex github mode yolo --sprite dev-sprite --no-ttl
```

`mode yolo` is operator-only. It defaults to a `2h` TTL and all installed repositories. Passing `--repo` narrows the scope to a repo allowlist. Passing `--no-ttl` makes the window indefinite until the operator returns to default mode.

Disable YOLO mode with:

```bash
zodex github mode default --sprite dev-sprite
```

`mode default` removes only YOLO state. It does not revoke explicit push grants.

## Refresh-token cache

By default, Sprite-side `request-push` does not persist refresh-token state. Add `--cache-refresh-token` only when the operator deliberately wants local refresh reuse on the Sprite.

Revocation normally removes the active repo grant. Add `--forget-local-auth` when you also want to clear cached device-flow auth state for that repo.
