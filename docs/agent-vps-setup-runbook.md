# computer-mcp Agent VPS Setup Runbook

This runbook is for an agent that is helping a human set up `computer-mcp` on a fresh Linux VPS.

If the target host is Sprites, use [agent-sprites-setup-runbook.md](agent-sprites-setup-runbook.md) instead.
For routine upgrades on an already-configured Sprite, use `zodex sprite upgrade --sprite <sprite>` from a machine with Sprite CLI access rather than relying on an in-guest `computer-mcp upgrade`.

Use this document exactly as written. It is optimized for:

- a fresh VPS
- a human who can do GitHub UI steps
- an agent who can run shell commands locally and over SSH
- the split auth model:
  - a **reader app** for read-only private repo access
  - a **publisher app** for branch push + PR creation

If the target host is Runpod, use [../.agents/skills/runpod-deployment/SKILL.md](../.agents/skills/runpod-deployment/SKILL.md) as the single source of truth for Runpod-specific rollout and template behavior.

The human should not be asked to make product decisions during setup. The defaults are already chosen here.

## Outcome

When this runbook is complete:

- `computer-mcp` is installed on the VPS
- the MCP HTTPS endpoint is live
- the VPS has both GitHub apps configured
- the reader app is stored for read-only repo access
- the publisher app is stored for local `publish-pr`
- the agent user can commit immediately with the default global Git identity unless the operator overrides it

## Important Constraints

- Both GitHub Apps are required. Do not offer one-app or optional-app variants.
- The **reader app** must be read-only.
- The **publisher app** must have only the minimum write permissions needed for PR publishing.
- The stack is started with one command:
  - `computer-mcp start`
- This runbook assumes the default config path:
  - `/etc/computer-mcp/config.toml`

## Information You Need From The Human

You need these inputs before you can finish setup:

- VPS SSH host
- VPS SSH port
- VPS SSH user
- SSH private key path to access the VPS
- target GitHub repo slug, for example `owner/repo`
- reader GitHub App ID
- absolute local path to the reader app PEM file
- publisher GitHub App ID
- absolute local path to the publisher app PEM file

Do not ask the human to find installation IDs manually. You will derive them yourself after they create and install the apps.

## Step 1: Send The Human The Exact Setup Message

Send this message to the human exactly once:

```text
I need you to create two private GitHub Apps and install both of them on only the repositories this VPS agent should touch.

App 1: computer-mcp-reader
- Create it from this pre-filled URL:
  https://github.com/settings/apps/new?name=computer-mcp-reader&description=Read-only%20private%20repo%20access%20for%20computer-mcp%20agent&url=https%3A%2F%2Fgithub.com%2Famxv%2Fcomputer-mcp&public=false&request_oauth_on_install=false&webhook_active=false&contents=read
- Purpose: read-only private repo access for the coding agent
- Homepage URL: use your repo URL or GitHub profile URL
- After the form opens, uncheck Webhook Active before creating the app
- Request user authorization during installation: OFF
- Device Flow: OFF
- Setup URL: blank
- Repository permissions:
  - Contents: Read-only
  - Everything else: No access
- Where can this GitHub App be installed?: Only on this account
- After creating it, open:
  https://github.com/settings/apps/computer-mcp-reader/installations
- Install it on your account, choose Only select repositories, and select only the repos this agent should be able to read

App 2: computer-mcp-publisher
- Create it from this pre-filled URL:
  https://github.com/settings/apps/new?name=computer-mcp-publisher&description=Branch%20push%20and%20PR%20creation%20for%20computer-mcp&url=https%3A%2F%2Fgithub.com%2Famxv%2Fcomputer-mcp&public=false&request_oauth_on_install=false&webhook_active=false&contents=write&pull_requests=write
- Purpose: create a branch and open a PR
- Homepage URL: use your repo URL or GitHub profile URL
- After the form opens, uncheck Webhook Active before creating the app
- Request user authorization during installation: OFF
- Device Flow: OFF
- Setup URL: blank
- Repository permissions:
  - Contents: Read & write
  - Pull requests: Read & write
  - Everything else: No access
- Where can this GitHub App be installed?: Only on this account
- After creating it, open:
  https://github.com/settings/apps/computer-mcp-publisher/installations
- Install it on your account, choose Only select repositories, and select only the repos this agent should be able to publish PRs to

When both apps are created and installed, send me:
- the Reader App ID
- the absolute local path to the Reader PEM file
- the Publisher App ID
- the absolute local path to the Publisher PEM file
- the target repo slug, for example owner/repo
- the VPS SSH command or the VPS host / port / user / key path
```

Notes for the agent:

- These links are for personal-account apps. For an organization-owned app, use the same query string on `https://github.com/organizations/ORG/settings/apps/new`.
- GitHub App names must be globally unique. If GitHub says the name is unavailable, change only the `name=` value and leave the rest unchanged.
- The webhook checkbox must be turned off manually for both apps before the human submits the form.
- If the human had to change the app name, also change the install URL slug under `https://github.com/settings/apps/<app-slug>/installations`.

## Step 2: Collect The Human's Reply

Wait for the human to send:

- Reader App ID
- absolute local path to Reader PEM
- Publisher App ID
- absolute local path to Publisher PEM
- target repo slug
- VPS SSH command or VPS host / port / user / key path

## Step 3: Set Local Variables

After the human replies, set these variables locally:

```bash
export VPS_HOST="<vps_host>"
export VPS_PORT="<vps_port>"
export VPS_USER="<vps_user>"
export VPS_KEY="<path_to_ssh_private_key>"

export TARGET_REPO="owner/repo"

export READER_APP_ID="<reader_app_id>"
export READER_PEM="<absolute_local_path_to_reader_pem>"

export PUBLISHER_APP_ID="<publisher_app_id>"
export PUBLISHER_PEM="<absolute_local_path_to_publisher_pem>"
```

Define helpers:

```bash
vps_ssh() {
  ssh -o StrictHostKeyChecking=accept-new -p "$VPS_PORT" "$VPS_USER@$VPS_HOST" -i "$VPS_KEY" "$@"
}

vps_scp() {
  scp -P "$VPS_PORT" -i "$VPS_KEY" "$@"
}
```

## Step 4: Derive The Installation IDs Yourself

Use the app ID and PEM file to query the exact installation for `TARGET_REPO`.

Create a temporary helper script locally:

```bash
cat > tmp/get-installation-id.sh <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
app_id="$1"
key_path="$2"
repo="$3"

b64url() { openssl base64 -A | tr '+/' '-_' | tr -d '='; }
now=$(date +%s)
iat=$((now - 60))
exp=$((now + 540))
header='{"alg":"RS256","typ":"JWT"}'
payload=$(printf '{"iat":%s,"exp":%s,"iss":"%s"}' "$iat" "$exp" "$app_id")
unsigned="$(printf '%s' "$header" | b64url).$(printf '%s' "$payload" | b64url)"
sig=$(printf '%s' "$unsigned" | openssl dgst -binary -sha256 -sign "$key_path" | b64url)
jwt="$unsigned.$sig"

curl -fsSL \
  -H "Accept: application/vnd.github+json" \
  -H "Authorization: Bearer ${jwt}" \
  -H "X-GitHub-Api-Version: 2022-11-28" \
  "https://api.github.com/repos/${repo}/installation" | jq -r '.id'
EOF
chmod +x tmp/get-installation-id.sh
```

Find the reader installation ID:

```bash
export READER_INSTALLATION_ID="$(bash tmp/get-installation-id.sh "$READER_APP_ID" "$READER_PEM" "$TARGET_REPO")"
```

Find the publisher installation ID:

```bash
export PUBLISHER_INSTALLATION_ID="$(bash tmp/get-installation-id.sh "$PUBLISHER_APP_ID" "$PUBLISHER_PEM" "$TARGET_REPO")"
```

Then verify that both are non-empty:

```bash
test -n "$READER_INSTALLATION_ID"
test -n "$PUBLISHER_INSTALLATION_ID"
```

If either variable is empty, stop and inspect the app installation in GitHub before continuing.

Remove the temporary helper script after you have both IDs:

```bash
rm -f tmp/get-installation-id.sh
```

## Step 5: Validate The App Permissions Before Touching The VPS

The repo already includes a token mint helper. Use it to verify that both apps authenticate correctly.

Validate the reader app:

```bash
GITHUB_APP_ID="$READER_APP_ID" \
GITHUB_APP_INSTALLATION_ID="$READER_INSTALLATION_ID" \
GITHUB_APP_PRIVATE_KEY_PATH="$READER_PEM" \
GITHUB_APP_PERMISSIONS_JSON='{"contents":"read"}' \
./scripts/mint-gh-app-installation-token.sh >/dev/null
```

Validate the publisher app:

```bash
GITHUB_APP_ID="$PUBLISHER_APP_ID" \
GITHUB_APP_INSTALLATION_ID="$PUBLISHER_INSTALLATION_ID" \
GITHUB_APP_PRIVATE_KEY_PATH="$PUBLISHER_PEM" \
GITHUB_APP_PERMISSIONS_JSON='{"contents":"write","pull_requests":"write"}' \
./scripts/mint-gh-app-installation-token.sh >/dev/null
```

Do not continue if either command fails.

## Step 6: Install `computer-mcp` On The VPS

Primary path for normal users:

```bash
vps_ssh 'curl -fsSL https://raw.githubusercontent.com/amxv/computer-mcp/main/scripts/install.sh | bash'
```

If that URL returns `404` because the repo is private, stop and switch to the local-source install flow from [deployment-notes.md](deployment-notes.md).

## Step 7: Copy Both PEM Files To The VPS

Copy the PEM files to temporary root-owned paths:

```bash
vps_scp "$READER_PEM" "$VPS_USER@$VPS_HOST:/root/computer-mcp-reader.pem"
vps_scp "$PUBLISHER_PEM" "$VPS_USER@$VPS_HOST:/root/computer-mcp-publisher.pem"
```

Install them into their final locations:

```bash
vps_ssh '
  install -d -m 0750 -o root -g computer-mcp /etc/computer-mcp/reader &&
  install -m 0640 -o root -g computer-mcp /root/computer-mcp-reader.pem /etc/computer-mcp/reader/private-key.pem &&
  install -m 0600 -o computer-mcp-publisher -g computer-mcp /root/computer-mcp-publisher.pem /etc/computer-mcp/publisher/private-key.pem
'
```

Meaning:

- the reader PEM is group-readable by `computer-mcp`
- the publisher PEM is readable only by `computer-mcp-publisher`

## Step 8: Write The Config

Append the app settings to `/etc/computer-mcp/config.toml`:

```bash
vps_ssh "cat >> /etc/computer-mcp/config.toml <<'EOF'

reader_app_id = ${READER_APP_ID}
reader_installation_id = ${READER_INSTALLATION_ID}
publisher_app_id = ${PUBLISHER_APP_ID}

[[publisher_targets]]
id = \"${TARGET_REPO}\"
repo = \"${TARGET_REPO}\"
default_base = \"main\"
installation_id = ${PUBLISHER_INSTALLATION_ID}
EOF"
```

## Step 9: Start The Stack

Run exactly this command on the VPS:

```bash
vps_ssh 'computer-mcp start'
```

`computer-mcp start` does all of this automatically:

- validates the reader app config and reader key
- validates the publisher app config and publisher key
- creates TLS artifacts if they do not exist yet
- starts the publisher daemon
- starts the MCP daemon

## Step 10: Verify The Deployment

Check the local status:

```bash
vps_ssh 'computer-mcp status'
```

Print the public MCP URL:

```bash
vps_ssh 'computer-mcp show-url --host "<public_ip_or_host>"'
```

Check health:

```bash
curl -k "https://<public_ip_or_host>/health"
```

Expected MCP URL shape:

```text
https://<public_ip_or_host>/mcp?key=<api_key>
```

## Step 11: Send The Final Summary To The Human

Once everything is working, send the human this summary:

```text
Setup is complete.

The VPS now has:
- a reader GitHub App for read-only repo access
- a publisher GitHub App for branch push + PR creation
- a live computer-mcp endpoint

Important files:
- config: /etc/computer-mcp/config.toml
- reader key: /etc/computer-mcp/reader/private-key.pem
- publisher key: /etc/computer-mcp/publisher/private-key.pem

Useful commands:
- computer-mcp start
- computer-mcp stop
- computer-mcp status
- computer-mcp logs
- computer-mcp publisher status
- computer-mcp publisher logs
- computer-mcp show-url --host "<public_ip_or_host>"
```

## Stop Conditions

Stop and ask before continuing if any of these happen:

- the reader app has any write permission
- the publisher app has permissions beyond `Contents: Read & write` and `Pull requests: Read & write`
- the app is installed on `All repositories` instead of `Only select repositories`
- the public installer returns `404`
- the VPS does not expose a public port for the MCP HTTPS listener
- the host is a container environment and the operator expects `systemd` semantics
