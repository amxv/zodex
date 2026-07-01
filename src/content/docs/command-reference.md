---
title: Command reference
description: A complete command map for the operator CLI, Sprite commands, proxy commands, GitHub grant commands, agent helper, local service helpers, and direct HTTP client.
order: 13
category: Reference
summary: The compact command index for day-to-day zodex operation.
---

## Global option

All main CLIs accept a config path:

```bash
zodex --config /etc/zodex/config.toml status
zodex-agent --config /etc/zodex/config.toml github list-grants
```

## Local service commands

```bash
zodex install
zodex upgrade --version latest
zodex start
zodex stop
zodex restart
zodex status
zodex logs
zodex set-key secret-runtime-key
zodex rotate-key
zodex tls setup
zodex show-url --host dev-zodex.example.net
```

## Sprite commands

```bash
zodex sprite setup --sprite dev-sprite --repo amxv/zodex --reader-app-id 123456 --reader-pem /secure/zodex/reader.pem --publisher-app-id 987654 --publisher-pem /secure/zodex/push-grant.pem --default-base main --url-auth sprite
zodex sprite upgrade --sprite dev-sprite --version latest
zodex sprite sync --sprite dev-sprite --force-recreate
zodex sprite sync --sprite dev-sprite --skip-stop-detached
zodex sprite status --sprite dev-sprite
zodex sprite services-status --sprite dev-sprite
zodex sprite logs --sprite dev-sprite --service zodexd --lines 100
zodex sprite service-logs --sprite dev-sprite --service zodexd --duration 30m
zodex sprite health --sprite dev-sprite
```

Add `--org engineering` to Sprite commands when the Sprite belongs to that organization.

## Proxy commands

```bash
zodex proxy inspect --sprite dev-sprite
zodex proxy verify-origin --sprite dev-sprite
zodex proxy deploy --sprite dev-sprite
zodex proxy update --sprite dev-sprite
zodex proxy deploy --origin https://dev-sprite.example.net --skip-verify-origin
```

## Operator-side GitHub grant commands

```bash
zodex github request-push --repo amxv/zodex
zodex github grant-push --sprite dev-sprite --repo amxv/zodex
zodex github grant-push --sprite dev-sprite --repo amxv/zodex --publisher-client-id Iv1.real-device-flow-client-id
zodex github list-grants --sprite dev-sprite
zodex github revoke-push --sprite dev-sprite --repo amxv/zodex
zodex github revoke-push --sprite dev-sprite --repo amxv/zodex --forget-local-auth
zodex github mode yolo --sprite dev-sprite
zodex github mode yolo --sprite dev-sprite --ttl 4h
zodex github mode yolo --sprite dev-sprite --no-ttl
zodex github mode yolo --sprite dev-sprite --repo amxv/zodex
zodex github mode status --sprite dev-sprite
zodex github mode default --sprite dev-sprite
```

`request-push` exists on the operator CLI too, but day-to-day agent sessions normally use `zodex-agent github request-push` on the Sprite. `github mode` is operator-only; it is intentionally not exposed by `zodex-agent`. When exactly one Sprite is registered locally, `--sprite` can be omitted for remote operator commands.

## Agent-side commands

```bash
zodex-agent show-url --host dev-zodex.example.net
zodex-agent github request-push --repo amxv/zodex
zodex-agent github request-push --repo amxv/zodex --ttl 2h
zodex-agent github request-push --repo amxv/zodex --no-ttl
zodex-agent github request-push --repo amxv/zodex --cache-refresh-token
zodex-agent github list-grants
zodex-agent github publish-pr --repo amxv/zodex --title "Improve zodex runtime docs" --base main --body "Adds comprehensive docs."
zodex-agent github publish-pr --repo amxv/zodex --title "Improve zodex runtime docs" --draft
zodex-agent github revoke-push --repo amxv/zodex
zodex-agent github revoke-push --repo amxv/zodex --forget-local-auth
```

## Direct HTTP client

```bash
zodex-client --url https://dev-zodex.example.net --key secret-runtime-key connect
zodex-client --url https://dev-zodex.example.net --key secret-runtime-key exec-command --cmd "pwd"
zodex-client --url https://dev-zodex.example.net --key secret-runtime-key write-stdin --session-handle session-token --chars "yes
"
zodex-client --url https://dev-zodex.example.net --key secret-runtime-key apply-patch --workdir /workspace/zodex --patch-file /tmp/change.patch
zodex-client --url https://dev-zodex.example.net --key secret-runtime-key disconnect
```
