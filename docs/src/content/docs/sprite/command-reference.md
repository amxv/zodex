---
title: "Command reference"
description: "A compact reference for Sprite setup, connection, status, health, logs, restart, sync, upgrade, Worker, GitHub policy, and guest commands."
order: 10
category: Sprite
summary: "The mode-first operator and restricted guest commands used to provision, maintain, connect, and control GitHub write access."
---

For a guided setup, start with [Sprite](/docs/sprite).

## Operator root

```text
zodex local ...
zodex sprite ...
zodex upgrade
```

Root `upgrade` upgrades only the local operator CLI.

```bash
zodex upgrade
zodex upgrade --check
zodex upgrade --version 0.3.4
zodex upgrade --version v0.3.4
```

`latest`, `0.3.4`, and `v0.3.4`-style release selectors are accepted; numeric versions are normalized to the repository's `v...` release tags.

Useful operator-upgrade options:

```text
--check              check without installing
--refresh            bypass the short latest-release check cache
--stop-local         explicitly stop blocking Linux/macOS/Windows Local state before installing
--format human|json  human output or the stable JSON event stream
```

The current version is compared before downloading the release archive, so an already-current `zodex upgrade` is a fast no-op. On Linux, macOS, and Windows, active/stale Local state blocks replacement before the archive download unless `--stop-local` was explicitly supplied.

## Sprite lifecycle and setup

```bash
zodex sprite setup --help
zodex sprite status --sprite dev
zodex sprite health --sprite dev
zodex sprite logs --sprite dev --service zodexd --lines 100
zodex sprite restart --sprite dev
zodex sprite sync --sprite dev
zodex sprite upgrade --sprite dev --version latest
zodex sprite connect --sprite dev
```

There is no Sprite start/stop command. `restart` repairs Zodex Services; `sync` reconciles desired service definitions.

## Setup required inputs

```text
--sprite
--repo
--reader-app-id
--reader-pem
--publisher-app-id
--publisher-client-id
--publisher-pem
```

Useful optional inputs include `--org`, `--default-base`, and `--remote-config`. The supported raw-origin auth is `public` and is already the setup default.

## Worker/front door

```bash
zodex sprite proxy status --sprite dev
zodex sprite proxy verify --sprite dev
zodex sprite proxy deploy --sprite dev
```

Deploy options include `--cloudflare-account <id-or-name>` and `--worker-name` when an explicit override is required. Normal setup derives a stable per-Sprite Worker name.

## Operator GitHub policy

```bash
zodex sprite github grant-push --sprite dev --repo owner/repo
zodex sprite github revoke-push --sprite dev --repo owner/repo
zodex sprite github list-grants --sprite dev
zodex sprite github yolo --sprite dev --repo owner/repo --ttl 2h
zodex sprite github status --sprite dev
zodex sprite github default --sprite dev
```

## Restricted guest GitHub commands

```bash
zodex-agent github request-push --repo owner/repo
zodex-agent github revoke-push --repo owner/repo
zodex-agent github list-grants
zodex-agent github publish-pr --repo owner/repo --title "Title"
```

`zodex-agent` also exposes the internal `git-credential-helper` used by configured Git workflows; it does not expose operator setup/Worker/YOLO authority.

## Current Sprite CLI helpers used around Zodex

```bash
sprite info dev
sprite config update --url-auth public dev
sprite checkpoint create -s dev --comment "before repair"
sprite checkpoint list -s dev
```
