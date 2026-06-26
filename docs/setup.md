# zodex Setup

This is the one canonical setup document for `zodex`.

## Outcome

When setup is complete:

- `zodex` is installed on a Sprite
- `zodexd` is running behind Sprite Services
- the proxy-backed MCP front door is available
- the runtime has read-only GitHub access through a reader app
- the operator can grant and revoke temporary repo-scoped push access

## One-Time Inputs

- Sprite name
- optional Sprite organization
- target repo slug, for example `owner/repo`
- reader GitHub App ID
- absolute path to the reader app PEM
- push-grant GitHub App client ID with device flow enabled
- push-grant GitHub App ID
- absolute path to the push-grant app PEM

Both apps must be installed on `Only select repositories`.

Permissions:

- reader app: `Contents: Read-only`
- push-grant app: `Contents: Read & write`, `Pull requests: Read & write`

The push-grant app should keep user access token expiration enabled and have **Device Flow** enabled in the GitHub App settings.

## Install

```bash
zodex sprite setup \
  --sprite <sprite> \
  --repo <owner/repo> \
  --reader-app-id <reader-app-id> \
  --reader-pem /absolute/path/to/reader.pem \
  --publisher-app-id <push-grant-app-id> \
  --publisher-pem /absolute/path/to/push-grant-app.pem \
  --default-base main \
  --url-auth sprite
```

If the Sprite is in a non-default org, add:

```bash
--org <org-name>
```

What the setup command does:

1. derives installation IDs for both apps
2. validates app access locally
3. uploads the `zodex` runtime binaries to the Sprite
4. runs the remote Rust install path
5. configures the reader helper and agent commit identity
6. syncs Sprite Services
7. verifies local health, workspace writeability, and reader-backed Git access
8. prints the MCP URL hint for the Sprite host

After setup, add the push-grant app client ID to the operator-side config:

```toml
publisher_client_id = "Iv1.abc123example"
```

You can also provide the same value ad hoc with `--publisher-client-id` or `ZODEX_PUBLISHER_CLIENT_ID`.

## Proxy

Use the proxy as the default public MCP front door:

```bash
zodex proxy inspect --sprite <sprite>
zodex proxy verify-origin --sprite <sprite>
zodex proxy deploy --sprite <sprite>
```

The proxy normalizes `/mcp`, warms cold Sprites, retries transient edge failures, and preserves streaming MCP responses.

## Write Flow

The supported write path is:

```bash
zodex github grant-push --sprite <sprite> --repo <owner/repo>
# agent pushes normally with git push
zodex github revoke-push --sprite <sprite> --repo <owner/repo>
```

Read access stays on. Write access is temporary and repo-scoped.
This is temporary repo-scoped direct push access, not a long-lived write credential.
By default, `grant-push` runs GitHub App device flow on the operator machine, requests a user access token for the target repo, and places only the temporary token on the Sprite.
If the device-flow path is unavailable and the publisher app key is configured, `grant-push` falls back to the installation-token app-key path.

## Day-To-Day Commands

```bash
zodex sprite status --sprite <sprite>
zodex sprite logs --sprite <sprite> --service zodexd --lines 100
zodex sprite sync --sprite <sprite> --force-recreate
zodex sprite upgrade --sprite <sprite>
zodex github list-grants --sprite <sprite>
```

## Verification Checklist

- `zodex sprite status` shows `zodexd` and `zodex-prd`
- the proxy-backed public URL serves `/health` and `/mcp`
- the agent can create a commit in `/workspace`
- plain `git clone https://github.com/<owner>/<repo>.git` works for installed private repos without a manual prompt
- the agent can `git clone` and `git fetch` private repos without a manual prompt
- an active grant enables `git push` for the granted repo only
- `grant-push` shows a GitHub device code and succeeds after browser authorization

## Stop Conditions

Stop and ask before continuing if:

- the reader app has any write permission
- the push-grant app has broader permissions than intended
- the app installation scope is broader than intended
- `zodexd` cannot bind after setup
- token minting validation fails
