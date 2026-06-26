# Operator Guide

This is the steady-state operator guide for the supported `zodex` workflow.

## Default Model

- read access is always available through the reader GitHub App
- write access is off by default
- the operator grants repo-scoped push access only when the agent should push
- the operator revokes that access afterward

## Daily Commands

Sprite lifecycle:

```bash
zodex sprite status --sprite <sprite>
zodex sprite logs --sprite <sprite> --service computer-mcpd --lines 100
zodex sprite sync --sprite <sprite> --force-recreate
zodex sprite upgrade --sprite <sprite>
```

Proxy lifecycle:

```bash
zodex proxy inspect --sprite <sprite>
zodex proxy verify-origin --sprite <sprite>
zodex proxy deploy --sprite <sprite>
```

GitHub write control:

```bash
zodex github grant-push --sprite <sprite> --repo <owner/repo>
zodex github list-grants --sprite <sprite>
zodex github revoke-push --sprite <sprite> --repo <owner/repo>
```

## Normal Session Flow

1. Ensure the Sprite is healthy.
2. Let the agent clone, inspect, edit, test, and commit.
3. When it is time to push, grant access for exactly one repo.
4. Let the agent push with normal `git push`.
5. Revoke the grant after the push.

## What Not To Do

- do not leave permanent GitHub write credentials on the Sprite
- do not run the coding agent as `root`
- do not give the reader app write permissions
- do not treat Runpod or raw in-guest process control as the default operational path

## When To Use The VPS Or Runpod Docs

- Use [agent-vps-setup-runbook.md](agent-vps-setup-runbook.md) only for a real non-Sprite Linux target.
- Use [.agents/skills/runpod-deployment/SKILL.md](../.agents/skills/runpod-deployment/SKILL.md) only for legacy Runpod-specific rollout work.
