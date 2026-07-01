---
title: Push modes and PRs
description: Publish pull requests, request temporary push access, use normal Git commands after approval, list active grants, and understand when to use YOLO mode instead.
order: 7
category: GitHub Access
summary: "The complete write workflow for ChatGPT inside a zodex Sprite: PRs, request-push, grant-push, revoke, and when to switch to YOLO."
---

ChatGPT can do almost all coding work before GitHub direct push is enabled: clone, inspect, edit, test, and commit. This page covers the write step after the local commit exists.

Use this quick rule:

- use `publish-pr` when you want review
- use `request-push` when ChatGPT should ask for one repo-scoped push window
- use `grant-push` when the operator should approve remotely
- use `github mode yolo` when repeated approvals are slowing down a trusted session

## Open a pull request

After committing locally, ChatGPT can publish a generated branch and open a PR without `gh` or a direct push grant:

```bash
zodex-agent github publish-pr \
  --repo amxv/zodex \
  --title "Improve zodex runtime docs" \
  --base main \
  --body "Adds detailed runtime, grant, and operations documentation."
```

For a draft PR:

```bash
zodex-agent github publish-pr \
  --repo amxv/zodex \
  --title "Improve zodex runtime docs" \
  --base main \
  --draft
```

`publish-pr` sends a bundle of the current committed `HEAD` to the publisher daemon. The daemon mints short-lived writer-app credentials, pushes a generated branch, opens the PR, and keeps credentials inside the daemon.

## Request access from the Sprite

When a direct push should be allowed from inside the ChatGPT session, request a repo-scoped push grant:

```bash
zodex-agent github request-push --repo amxv/zodex
```

The command runs a GitHub App device-flow authorization. It prints a verification URL and user code, tries to open the URL, and activates the grant after approval.

The default grant TTL is `30m`:

```bash
zodex-agent github request-push --repo amxv/zodex --ttl 30m
```

For a longer grant:

```bash
zodex-agent github request-push --repo amxv/zodex --ttl 2h
```

For a grant without TTL enforcement:

```bash
zodex-agent github request-push --repo amxv/zodex --no-ttl
```

Use `--no-ttl` only for an intentional operator-controlled window.

## Push normally

After the grant is active, use Git directly:

```bash
git push origin main
```

For branch-based work:

```bash
git switch -c docs-runtime-guide
git push origin docs-runtime-guide
```

The zodex Git credential helper uses the active repo grant. It does not make unrelated repositories writable.

## List grants

On the Sprite:

```bash
zodex-agent github list-grants
```

From the operator machine:

```bash
zodex github list-grants --sprite dev-sprite
```

Use this before assuming a push failure is caused by code, branch protection, or GitHub permissions.

## Revoke when finished

Revoke after the push step:

```bash
zodex-agent github revoke-push --repo amxv/zodex
```

To also clear local device-flow auth state:

```bash
zodex-agent github revoke-push --repo amxv/zodex --forget-local-auth
```

## When to use YOLO instead

Use YOLO mode when direct push should remain available across repeated commits in a trusted session:

```bash
zodex github mode yolo --sprite dev-sprite --repo amxv/zodex --ttl 4h
```

Return to the default policy when finished:

```bash
zodex github mode default --sprite dev-sprite
```

For the full decision guide, see [Write modes](/docs/write-modes).
