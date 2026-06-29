---
title: Sprite operations
description: Check runtime health, inspect logs, resync Sprite Services, upgrade binaries, verify the proxy origin, and diagnose older installs.
order: 9
category: Operations
summary: Day-to-day commands for keeping a zodex Sprite deployment healthy.
---

## Status

Check the installed Sprite services:

```bash
zodex sprite status --sprite dev-sprite
```

The expected services are `zodexd` and `zodex-prd`.

The `status` command also has the alias:

```bash
zodex sprite services-status --sprite dev-sprite
```

## Logs

Read service logs from the operator machine:

```bash
zodex sprite logs --sprite dev-sprite --service zodexd --lines 100
zodex sprite logs --sprite dev-sprite --service zodex-prd --lines 100
```

Use `--duration` when investigating a time window:

```bash
zodex sprite logs --sprite dev-sprite --service zodexd --duration 30m
```

The `logs` command also has the alias:

```bash
zodex sprite service-logs --sprite dev-sprite --service zodexd --lines 100
```

## Health

Check runtime health through the Sprite path:

```bash
zodex sprite health --sprite dev-sprite
```

You can also verify the public origin and proxy:

```bash
zodex proxy verify-origin --sprite dev-sprite
curl https://dev-zodex.example.net/health
```

A healthy daemon returns:

```json
{"status":"ok"}
```

## Sync services

After changing service definitions or runtime configuration:

```bash
zodex sprite sync --sprite dev-sprite --force-recreate
```

To avoid stopping detached services during a targeted sync:

```bash
zodex sprite sync --sprite dev-sprite --skip-stop-detached
```

## Upgrade

Upgrade an installed runtime:

```bash
zodex sprite upgrade --sprite dev-sprite --version latest
```

To upgrade while also updating repo or URL auth behavior:

```bash
zodex sprite upgrade   --sprite dev-sprite   --version v0.2.9   --repo amxv/zodex   --url-auth sprite
```

After an upgrade, verify service status, logs, proxy origin, reader-backed clone, and one short command execution through MCP.

## Migration checks

For older pre-zodex Sprites, check these before debugging the new runtime:

- remove or disable `computer-mcpd` and `computer-mcp-prd`
- migrate old `/etc/computer-mcp` repo references into `/etc/zodex/config.toml`
- verify `/var/lib/zodex/publisher` is writable by the publisher user
- run TLS setup before expecting `zodex-prd` and `zodexd` to start cleanly
