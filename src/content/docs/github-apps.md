---
title: GitHub Apps setup
description: Create the reader and push-grant GitHub Apps with the narrow permissions zodex expects.
order: 5
category: GitHub Access
summary: The one-time GitHub App checklist for read-only access, device-flow push grants, PEM files, app IDs, client IDs, and installation scope.
---

## Why there are two apps

zodex uses two GitHub Apps because read access and write access have different risk profiles.

The reader app stays available to the runtime so agents can clone and fetch. The push-grant app is used only when an operator or agent intentionally starts a grant flow.

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

Record the app ID and install the app on the repositories agents are allowed to read.

## Push-grant app checklist

Create a second GitHub App for the push-grant role. Configure it with:

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

Install both apps on the same target repositories unless you intentionally want different read and write scopes. For this repository, the supported slug is:

```text
amxv/zodex
```

Avoid organization-wide installation unless every repository in the organization is allowed for agent access.

## Use the values in setup

Run setup with both app IDs and PEM paths:

```bash
zodex sprite setup   --sprite dev-sprite   --repo amxv/zodex   --reader-app-id 123456   --reader-pem /secure/zodex/reader.pem   --publisher-app-id 987654   --publisher-pem /secure/zodex/push-grant.pem   --default-base main   --url-auth sprite
```

Then configure the push-grant client ID for day-to-day grants:

```toml
publisher_client_id = "Iv1.real-device-flow-client-id"
```

or pass it directly:

```bash
zodex-agent github request-push --repo amxv/zodex --publisher-client-id Iv1.real-device-flow-client-id
```
