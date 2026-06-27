# zodex Setup

This is the one canonical setup document for `zodex`.

For this project's own deployment and release paths, the canonical repository slug is `amxv/zodex`.

## Outcome

When setup is complete:

- the Sprite guest keeps `zodex-agent`, `zodexd`, and `zodex-prd`
- `zodexd` is running behind Sprite Services
- the proxy-backed MCP front door is available
- the runtime has read-only GitHub access through a reader app
- the operator can grant and revoke temporary repo-scoped push access

## One-Time Inputs

- Sprite name
- optional Sprite organization
- target repo slug, for example `amxv/zodex`
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
  --repo amxv/zodex \
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
3. uploads operator-built runtime binaries and the installer to the Sprite
4. installs the guest runtime without leaving the full `zodex` operator CLI on-box
5. configures the reader helper and agent commit identity
6. syncs Sprite Services
7. verifies local health, workspace writeability, and reader-backed Git access
8. prints the MCP URL hint for the Sprite host

Operator build note:

- `zodex sprite setup` uploads operator-built runtime binaries to the Sprite.
- The uploaded binaries must be runnable on the Sprite target.
- If you run setup from a non-Linux machine, do not assume the local development build is suitable for the Sprite guest. Use a Linux-compatible build or install from a release artifact instead.

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
cd proxy/cloudflare-worker
# set vars.SPRITE_ORIGIN in wrangler.jsonc first
npx wrangler deploy
```

The proxy normalizes `/mcp`, warms cold Sprites, retries transient edge failures, and preserves streaming MCP responses.

## Write Flow

The supported write path is:

```bash
zodex-agent github request-push --repo <owner/repo>
# agent pushes normally with git push
zodex-agent github revoke-push --repo <owner/repo>
# remote operator alternative
zodex github grant-push --sprite <sprite> --repo <owner/repo>
# optional full local logout for this repo
zodex-agent github revoke-push --repo <owner/repo> --forget-local-auth
```

Read access stays on. Write access is temporary and repo-scoped.
This is temporary repo-scoped direct push access, not a long-lived write credential.
By default, `zodex-agent github request-push` runs GitHub App device flow on the Sprite, requests a user access token for the target repo, and writes only the repo-scoped temporary token locally.
It starts as a single blocking flow: show the device code, try to open the verification URL, best-effort copy the code, poll until approval completes, and then activate the grant automatically.
The default active grant TTL is `30m`. Change it with `--ttl <duration>` or disable TTL enforcement with `--no-ttl`.
By default, `zodex-agent github request-push` does not persist refresh-token state. Add `--cache-refresh-token` only when you explicitly want local refresh reuse on the Sprite.
Expired grants stop working in the credential-helper path even if the grant file is still present.
`grant-push` remains available as the operator-machine alternative and still places only the temporary token on the Sprite.
When practical, it also opens the GitHub verification URL automatically and copies the device code to the clipboard, with manual fallback output if either integration is unavailable.
By default, `zodex-agent github revoke-push` removes the active repo grant and retains any local device-flow refresh state so repeated grants are faster when that cache exists. Use `--forget-local-auth` when you want a full local logout for that repo too.

## Migration Notes

If you are migrating an older pre-`zodex` Sprite rather than doing a clean install, check these before debugging the new runtime:

- remove or disable legacy `computer-mcpd` and `computer-mcp-prd` Sprite Services so `zodexd` can claim its ports cleanly
- migrate any old `/etc/computer-mcp` repo target references to the current repo slug expected by `/etc/zodex/config.toml`
- verify `/var/lib/zodex/publisher` is writable by the configured publisher user before expecting `zodex-prd` to start
- if TLS artifacts do not exist yet, run the TLS setup path and then re-sync Sprite Services

These are migration quirks from older installs, not part of the normal clean setup flow.

## Day-To-Day Commands

```bash
zodex sprite status --sprite <sprite>
zodex sprite logs --sprite <sprite> --service zodexd --lines 100
zodex sprite sync --sprite <sprite> --force-recreate
zodex sprite upgrade --sprite <sprite>
zodex-agent github list-grants
```

## Verification Checklist

- `zodex sprite status` shows `zodexd` and `zodex-prd`
- the proxy-backed public URL serves `/health` and `/mcp`
- the agent can create a commit in `/workspace`
- plain `git clone https://github.com/amxv/zodex.git` works for installed private repos without a manual prompt
- the agent can `git clone` and `git fetch` private repos without a manual prompt
- an active grant enables `git push` for the granted repo only
- `grant-push` shows a GitHub device code, tries to open the verification URL, and succeeds after browser authorization

## Stop Conditions

Stop and ask before continuing if:

- the reader app has any write permission
- the push-grant app has broader permissions than intended
- the app installation scope is broader than intended
- `zodexd` cannot bind after setup
- token minting validation fails
