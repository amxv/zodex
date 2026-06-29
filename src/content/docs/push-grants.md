---
title: Push grants and PRs
description: Request temporary push access, push with normal Git commands, open pull requests without gh, list active grants, and revoke when the write path is finished.
order: 6
category: GitHub Access
summary: The complete write workflow for an agent working inside a zodex Sprite.
---

## Request access from the Sprite

When the agent has a local commit ready, request a repo-scoped push grant:

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

## Open a pull request

After committing locally, the agent can publish a generated branch and open a PR without `gh` or a direct push grant:

```bash
zodex-agent github publish-pr   --repo amxv/zodex     --title "Improve zodex runtime docs"   --base main   --body "Adds detailed runtime, grant, and operations documentation."
```

For a draft PR:

```bash
zodex-agent github publish-pr   --repo amxv/zodex     --title "Improve zodex runtime docs"   --base main   --draft
```

`publish-pr` sends a bundle of the current committed `HEAD` to the publisher daemon. The daemon mints short-lived publisher-app credentials, pushes a generated branch, opens the PR, and keeps credentials inside the daemon.

## Revoke when finished

Revoke after the push or PR step:

```bash
zodex-agent github revoke-push --repo amxv/zodex
```

To also clear local device-flow auth state:

```bash
zodex-agent github revoke-push --repo amxv/zodex --forget-local-auth
```
