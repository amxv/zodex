# zodex

`zodex` is a ChatGPT-native remote coding workspace.

It gives ChatGPT a real Sprite-backed Linux machine and a tiny MCP tool surface that GPT models already know how to use well:

- `exec_command`
- `write_stdin`
- `apply_patch`

ChatGPT can clone repos, inspect code, edit files, run tests, keep long-lived sessions alive, and commit locally. The operator decides how GitHub writes happen:

- PR-only publishing without direct shell write tokens
- one-off repo-scoped push approval from the Sprite
- remote operator-granted push windows
- timed YOLO mode for trusted sessions
- repo-scoped YOLO for selected repos
- no-TTL YOLO for intentionally trusted environments

The supported repository slug for this project is `amxv/zodex`.

## Why it exists

ChatGPT coding works best when the model has familiar tools and a real machine. zodex gives it both:

1. a Sprite Linux workspace instead of a simulated sandbox
2. command/stdin/patch tools that fit GPT coding behavior
3. normal Git history and normal test commands
4. operator-controlled GitHub write autonomy

Sprites are a good fit for this because coding-agent work is bursty. You can run real remote work when ChatGPT is active instead of renting an always-on VPS for a full month and leaving it idle most of the time.

## Write modes

Start safe, then open more autonomy when the session earns it.

### Review-first PR

```bash
zodex-agent github publish-pr \
  --repo owner/repo \
  --title "Title" \
  --base main \
  --body "Summary and tests."
```

`publish-pr` bundles the current committed `HEAD`, sends it to the local publisher daemon, and lets that daemon push a generated branch and open a PR. The writer-app token stays inside `zodex-prd` instead of being exposed to the agent shell.

### One-off push approval

```bash
zodex-agent github request-push --repo owner/repo
# then normal Git works
git push origin main
zodex-agent github revoke-push --repo owner/repo
```

The default active grant TTL is `30m`. Change it with `--ttl 2h`, disable TTL enforcement with `--no-ttl`, and opt into refresh-token caching with `--cache-refresh-token` only when intended.

### Operator-granted push

```bash
zodex github grant-push --sprite dev-sprite --repo owner/repo
git push origin main
zodex github revoke-push --sprite dev-sprite --repo owner/repo
```

Use this when the human operator should open the write window from their own machine.

### YOLO mode

```bash
zodex github mode yolo --sprite dev-sprite
zodex github mode yolo --sprite dev-sprite --ttl 4h
zodex github mode yolo --sprite dev-sprite --repo owner/repo
zodex github mode yolo --sprite dev-sprite --no-ttl
zodex github mode status --sprite dev-sprite
zodex github mode default --sprite dev-sprite
```

`mode yolo` defaults to a `2h` TTL and all installed repositories. Passing `--repo` changes the scope to a repo allowlist; repeated repo-scoped YOLO commands merge with active repo grants instead of replacing them, and each repo keeps its own TTL. Passing `--no-ttl` makes the new window indefinite until the operator disables it. `mode default` removes only YOLO state and leaves explicit push grants alone.

## Quick setup shape

See the Quickstart for the no-clone installer path. The setup flow is:

1. install the local `zodex` operator CLI
2. install and authenticate the Sprite CLI
3. create and select a Sprite
4. make the Sprite URL public for ChatGPT MCP access
5. create the reader and writer GitHub Apps
6. run `zodex sprite setup`
7. connect ChatGPT to the `/mcp?key=...` URL

Create two GitHub Apps:

- reader app: `Contents: Read-only`
- writer app: `Contents: Read & write`, `Pull requests: Read & write`, Device Flow enabled, user access token expiration enabled

Install zodex on the Sprite:

```bash
zodex sprite setup \
  --sprite zodex-dev \
  --repo owner/repo \
  --reader-app-id <reader-app-id> \
  --reader-pem /absolute/path/to/reader.pem \
  --publisher-app-id <writer-app-id> \
  --publisher-pem /absolute/path/to/writer.pem \
  --default-base main \
  --url-auth sprite
```

Connect ChatGPT to:

```text
https://<sprite-host>/mcp?key=<zodex-api-key>
```

## Core commands

```bash
zodex sprite status --sprite zodex-dev
zodex sprite logs --sprite zodex-dev --service zodexd --lines 100
zodex sprite sync --sprite zodex-dev --force-recreate
zodex sprite upgrade --sprite zodex-dev
zodex proxy inspect --sprite zodex-dev
zodex proxy verify-origin --sprite zodex-dev
zodex-agent github publish-pr --repo owner/repo --title "Title"
zodex-agent github request-push --repo owner/repo
zodex github grant-push --sprite zodex-dev --repo owner/repo
zodex github mode yolo --sprite zodex-dev --repo owner/repo --ttl 4h
zodex github mode default --sprite zodex-dev
zodex-agent show-url --host <public-host>
```

## Documentation site

This repository includes an Astro documentation site for zodex. It covers ChatGPT setup, Sprite runtime architecture, GitHub App access, write modes, proxy and MCP front door, direct HTTP API, command reference, troubleshooting, and docs maintenance.

Run it locally with:

```bash
bun install
bun run dev
```

Validate the docs site with:

```bash
bun run check
bun run build
```

Deploy the docs worker with:

```bash
bun run deploy:docs
```

Production routing keeps `zodex.ashray.xyz` on the existing Cloudflare proxy worker. That worker forwards `/mcp`, `/mcp/*`, and `/health` to the live Sprite and sends all other paths to the `zodex-docs` worker origin.

The Astro docs content lives in `src/content/docs`, with site-wide navigation and metadata in `src/data/docs.ts`.
