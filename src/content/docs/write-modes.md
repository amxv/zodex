---
title: Write modes
description: "Choose how much GitHub autonomy ChatGPT gets in a zodex session: PR-only, one-off push approval, operator-granted push, timed YOLO, repo-scoped YOLO, or no-TTL YOLO."
order: 2
category: Start
summary: The decision guide for PR-only workflows, push grants, and scoped YOLO mode.
---

zodex is safe by default, but it is not approval-only by design. The operator chooses how much GitHub autonomy ChatGPT gets for the current session, repo, and risk level.

The core idea is:

```text
read by default -> local work -> PR or push policy -> revoke or expire
```

ChatGPT can clone, inspect, edit, test, and commit on the Sprite before any GitHub write path is open. When it is time to send work back to GitHub, choose one of the modes below.

## Mode map

| Mode | Command | Best for |
| --- | --- | --- |
| PR-only | `zodex-agent github publish-pr` | Review-first work, risky repos, new agents, protected branches |
| Agent-requested push | `zodex-agent github request-push --repo owner/repo` | One-off approval from inside the ChatGPT session |
| Operator-granted push | `zodex github grant-push --sprite dev --repo owner/repo` | Human-controlled approval from the operator machine |
| Timed YOLO | `zodex github mode yolo --sprite dev --ttl 2h` | Trusted work sessions where repeated approvals would slow the loop |
| Repo-scoped YOLO | `zodex github mode yolo --sprite dev --repo owner/repo --ttl 4h` | Trusted work on specific repos only |
| No-TTL YOLO | `zodex github mode yolo --sprite dev --no-ttl` | Fully trusted personal or development environments |

## PR-only

Use PR-only mode when you want ChatGPT to do the work but keep final review explicit.

```bash
zodex-agent github publish-pr \
  --repo owner/repo \
  --title "Describe the change" \
  --base main \
  --body "Summary and tests."
```

`publish-pr` bundles the current committed `HEAD`, sends it to `zodex-prd`, pushes a generated branch, and opens the PR. The publisher credentials stay inside the publisher daemon instead of being exposed to the agent shell.

Use this mode when:

- the repo is important or branch-protected
- you are trying a new model or prompt
- you want a reviewable diff before anything lands on `main`
- you want ChatGPT to avoid direct `git push` entirely

## Agent-requested push

Use `request-push` when ChatGPT has a commit ready and direct push should be allowed once.

```bash
zodex-agent github request-push --repo owner/repo
# then normal Git works inside the Sprite
git push origin main
```

The default active grant TTL is `30m`:

```bash
zodex-agent github request-push --repo owner/repo --ttl 30m
```

Use a longer window when the push flow needs more time:

```bash
zodex-agent github request-push --repo owner/repo --ttl 2h
```

Disable TTL enforcement only when the operator intentionally wants that behavior:

```bash
zodex-agent github request-push --repo owner/repo --no-ttl
```

Use this mode when:

- ChatGPT has already completed and tested the change
- you want a normal `git push` without enabling broader YOLO mode
- the approval should happen directly from the agent-side workflow

## Operator-granted push

Use the operator-side grant when the human should open the write window from their own machine.

```bash
zodex github grant-push --sprite dev --repo owner/repo
```

Then the Sprite-side agent can push normally:

```bash
git push origin main
```

Revoke when finished:

```bash
zodex github revoke-push --sprite dev --repo owner/repo
```

Use this mode when:

- the browser approval should stay with the operator
- ChatGPT cannot conveniently open the device-flow URL from the Sprite
- you want to control the exact moment a repo becomes writable

## YOLO mode

Use YOLO mode when repeated push approvals are just friction and you trust the ChatGPT session for the selected scope.

```bash
zodex github mode yolo --sprite dev
```

By default, YOLO mode uses a `2h` TTL and applies to all repositories installed for the writer app. Scope it to one or more repos when you want narrower autonomy:

```bash
zodex github mode yolo --sprite dev --repo owner/repo --ttl 4h
zodex github mode yolo --sprite dev --repo owner/repo --repo owner/another-repo
```

Disable TTL only for intentionally trusted environments:

```bash
zodex github mode yolo --sprite dev --no-ttl
```

Check mode state:

```bash
zodex github mode status --sprite dev
```

Return to the default policy:

```bash
zodex github mode default --sprite dev
```

Use YOLO mode when:

- the repo is yours or low-risk
- the agent is trusted for the current task
- the session needs to push multiple small fixes
- docs, examples, or generated assets are changing quickly
- repeated approval prompts are slowing down the actual work

## Recommended progression

For a new setup, start here:

1. Use PR-only for the first few sessions.
2. Use `request-push` once you trust the workflow.
3. Use repo-scoped YOLO for trusted repos where speed matters.
4. Use all-installed or no-TTL YOLO only for environments where that level of trust is intentional.

zodex is built so you can move up and down this ladder without changing the ChatGPT MCP connection.
