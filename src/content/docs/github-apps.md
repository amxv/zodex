---
title: GitHub Apps setup
description: Create the reader and writer GitHub Apps with the narrow permissions zodex expects for ChatGPT clone/fetch, PR publishing, push grants, and YOLO mode.
order: 6
category: GitHub Access
summary: The one-time GitHub App checklist for read access, PR creation, device-flow push grants, writer-app credentials, app IDs, client IDs, and installation scope.
---

## Why there are two apps

zodex uses two GitHub Apps because reading code and writing to GitHub have different risk profiles.

The reader app stays available so ChatGPT can clone and fetch. The writer app powers PR publishing, one-off push grants, and operator-controlled YOLO mode.

In config and CLI output, the writer app is often called the publisher or push-grant app because it publishes generated PR branches and backs direct-push windows.

## Reader app checklist

Create a GitHub App named for the zodex reader role. Configure it with:

```text
Repository permissions:
  Contents: Read-only
Installation scope:
  Only select repositories
Private key:
  download PEM and store it on the operator machine for setup
```

Record the app ID and install the app on the repositories ChatGPT is allowed to read.

## Writer app checklist

Create a second GitHub App for the writer role. Configure it with:

```text
Repository permissions:
  Contents: Read & write
  Pull requests: Read & write
Installation scope:
  Only select repositories
User access tokens:
  expiration enabled
Device Flow:
  enabled
Private key:
  download PEM and store it on the operator machine for setup
```

Record both the app ID and the client ID. The app ID is used during setup. The client ID is used by device-flow grant commands.

## Install on selected repositories

Install both apps on the same target repositories unless you intentionally want different read and writer scopes.

```text
amxv/zodex
```

Avoid organization-wide installation unless every repository in the organization is allowed for ChatGPT access and for the write modes you plan to use.

## Use the values in setup

Run setup with both app IDs and PEM paths:

```bash
zodex sprite setup   --sprite dev-sprite   --repo amxv/zodex   --reader-app-id 123456   --reader-pem /secure/zodex/reader.pem   --publisher-app-id 987654   --publisher-pem /secure/zodex/writer.pem   --default-base main   --url-auth sprite
```

Then configure the push-grant client ID for day-to-day grants:

```toml
publisher_client_id = "Iv1.real-device-flow-client-id"
```

or pass it directly:

```bash
zodex-agent github request-push --repo amxv/zodex --publisher-client-id Iv1.real-device-flow-client-id
```

## How the apps map to write modes

| Write mode | App used | Credential exposure |
| --- | --- | --- |
| Clone/fetch | Reader app | Read-only helper path |
| `publish-pr` | Writer app | Token stays inside the publisher daemon |
| `request-push` | Writer app + device flow | Repo-scoped grant for the requested window |
| `grant-push` | Writer app + device flow | Operator-created repo-scoped grant |
| `mode yolo` | Writer app | Operator-controlled direct-push policy for the selected scope |
