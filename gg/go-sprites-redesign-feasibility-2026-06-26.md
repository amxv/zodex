# Go Redesign Feasibility For Sprites.dev

- Date: 2026-06-26
- Scope: research only, stayed on `main`
- Author: `stable_deer`

## Executive Verdict

The redesign is feasible and worth pursuing.

My recommendation is **not** a literal one-binary rewrite of the current Rust deployment. The better target is:

1. a Go core that owns tool semantics and session behavior
2. thin HTTP and MCP wrappers over that same core
3. a Sprite-optimized operator CLI that handles install, upgrade, service sync, health checks, and temporary credential grants
4. a Cloudflare Worker package in the same repo, treated as a first-class compatibility layer
5. a GitHub auth model that keeps long-lived **write** credentials off the Sprite

The core technical work is straightforward. The difficult part is not Go itself; it is preserving behavior exactly while improving lifecycle and permissions without regressing the current agent experience.

My overall call is:

- **Go rewrite:** feasible
- **Tool-surface preservation:** feasible
- **Sprite-first cold-start/lifecycle model:** feasible
- **Safer direct-push workflow:** feasible, but only with a different GitHub credential strategy than the current resident publisher key model
- **Proxy elimination:** not currently advisable

## Current State Summary

### External tool surface

The externally exposed agent tool surface is intentionally narrow:

- `exec_command`
- `write_stdin`
- `apply_patch`

Those tools are exposed in two transports:

- MCP over streamable HTTP at `/mcp` and `/mcp/`
- Bearer-authenticated HTTP JSON endpoints:
  - `/v1/exec-command`
  - `/v1/write-stdin`
  - `/v1/apply-patch`

There is also a thin remote HTTP CLI in `computer` that calls the HTTP API and persists a connection profile locally.

This is already close to the shape the redesign wants: one conceptual tool model, multiple transport wrappers.

### Internal architecture today

The Rust service is split into:

- `ComputerService`: common tool implementation boundary
- `SessionManager`: PTY-backed command runtime and session continuation logic
- `http_api.rs`: HTTP wrappers around `ComputerService`
- `server.rs`: MCP registration plus `/health` and HTTP routing
- `apply_patch.rs`: path-rewriting adapter over the upstream Rust `codex-apply-patch` crate

That means the codebase already contains the architectural seam needed for a Go port:

- core behavior
- transport adapters
- deployment/runtime plumbing

### Behavior that must be preserved

The redesign must preserve more than just tool names.

Observed behavior worth keeping stable:

- `exec_command` returns either a finished result or a live `session_handle`
- `write_stdin` both writes and polls an existing PTY session
- session status values are `running` and `exited`
- termination reasons are `exit`, `timeout`, and `killed`
- output includes cwd tracking
- PTY sessions have idle timeout behavior and `kill_process` handling
- command output is truncated to configured limits, with truncation notice semantics
- `apply_patch` requires a `workdir`, rewrites relative patch paths against that workdir, and otherwise delegates to Codex-style patch semantics

These are the real compatibility constraints for the Go rewrite.

### Deployment and lifecycle model today

There are effectively three runtime modes:

- `systemd` mode on normal Linux hosts
- detached process mode on container-like hosts without `systemd`
- Sprite Services mode on Sprites

For Sprites, the repo already treats Sprite Services as the source of truth. The key operational choices are:

- `computer-mcpd` runs as `computer-mcp-agent`
- `computer-mcp-prd` runs as `computer-mcp-publisher`
- Sprite URL traffic goes to `http_bind_port = 8080`
- Sprite-safe TLS bind is `8443`
- the built-in `sprite` user is intentionally avoided because it often has passwordless `sudo`

This is the correct direction for a Sprite-first design: the platform lifecycle owner is the Sprite Services API, not guest-local detached processes.

### GitHub model today

The current design uses two GitHub Apps:

- a read-only reader app for clone/fetch over HTTPS
- a write-capable publisher app for branch push and PR creation

Important characteristics:

- the agent gets read access by a Git credential helper that mints short-lived reader installation tokens
- the write key is isolated behind a local publisher daemon on a Unix socket
- `publish-pr` sends a local `git bundle` to that daemon
- the agent never needs the write credential directly

This is a good security model for “agent can open PRs but should not hold write credentials.” It is **not** the same as the user’s proposed direct-push grant model.

### Proxy and direct Sprite URL reality

The current local evidence says the Cloudflare Worker is not just cosmetic.

The proxy repo documents an actual compatibility problem:

- direct Sprite `/mcp?key=...` returned `404`
- direct Sprite `/mcp/?key=...` returned `200`
- direct Sprite `/health` could return `502 Bad Gateway` on cold wake
- the Worker compensates with `/mcp` normalization, warmup probes, retries, and response streaming preservation

The repo-local `server.rs` already routes both `/mcp` and `/mcp/`, but that does **not** prove the raw Sprite public edge is reliable for connector clients. The observed March 21 behavior strongly suggests the Worker is masking edge-specific issues, not server-side route issues alone.

My conclusion is that the proxy should be treated as part of the deployable system unless and until direct raw Sprite URL reliability is re-proven against the actual target clients.

## Feasibility Assessment

## What is easy

- Rewriting the HTTP API in Go
- Rewriting the MCP wrapper in Go
- Reusing one core implementation for both transports
- Building a Sprite-first operator CLI in Go
- Replacing the current shell-heavy install/upgrade flows with a more coherent control-plane CLI
- Preserving config defaults and operator UX concepts

## What is moderate

- PTY-backed session lifecycle parity in Go
- matching output truncation, cwd tracking, and timeout semantics precisely
- preserving remote CLI behavior and self-signed TLS fallback ergonomics
- reproducing current tests and parity guarantees across service, HTTP, and CLI layers

## What is hard

- preserving `apply_patch` behavior exactly
- redesigning GitHub write access so that it is safer on Sprites while still fast for operators
- deciding whether the Sprite should ever hold a private key capable of minting write credentials
- guaranteeing direct raw Sprite URL compatibility without the Worker

## Feasibility verdict by area

- **Core tools in Go:** high confidence
- **Lifecycle/upgrade flow in Go:** high confidence
- **GitHub direct-push grant model:** medium confidence, but only if write access is treated as ephemeral and operator-mediated
- **Exact patch semantics preservation:** medium-high confidence if `apply-patch-go` is adopted and verified against golden parity cases
- **Removing the proxy entirely:** low confidence today

## Recommended Target Architecture

## Shape

Use one repo with three deployable parts:

1. `computerd` Go service inside the Sprite
2. `computerctl` Go operator CLI on the local workstation
3. `proxy/` Cloudflare Worker package in the same repo

I would keep the Worker package in TypeScript or JavaScript even in a Go-centered repo. “Same repo” matters here; “same language” does not.

## Go service layout

Recommended package structure:

- `internal/core`
  - exact tool contracts and shared business logic
- `internal/core/session`
  - PTY process runtime, handles, timeouts, cwd resolution, output truncation
- `internal/core/patch`
  - adapter around `apply-patch-go`
- `internal/transports/httpapi`
  - `/v1/exec-command`, `/v1/write-stdin`, `/v1/apply-patch`
- `internal/transports/mcp`
  - MCP tool registration and wrapper types
- `internal/github`
  - repo inventory, installation discovery, token lease materialization
- `internal/config`
  - TOML or YAML config, defaults, validation
- `internal/sprite`
  - service definition rendering, health verification, control-plane helpers
- `cmd/computerd`
  - Sprite-resident daemon
- `cmd/computerctl`
  - operator CLI for install/upgrade/grant/revoke/status

## Service runtime model

For Sprites, prefer a **single resident agent-facing Go service**.

I do **not** recommend keeping the current resident publisher-daemon model as the primary future design. It solves one problem well, but it conflicts with the new requirement for temporary direct push grants and keeps a long-lived write-capable private key inside the Sprite.

Instead:

- keep the Sprite-resident service focused on tool execution and optional read-only Git support
- move write credential minting and grant orchestration to the local operator CLI
- let the CLI place short-lived repo-scoped credentials on the Sprite only for the duration of a session

## Credential model

My recommended steady-state split is:

- **Read path:** resident, bounded, low-risk
- **Write path:** ephemeral, repo-scoped, operator-granted

Specifically:

- keep a **read-only GitHub App** available for autonomous clone/fetch of a bounded repo set
- do **not** keep a long-lived write-capable GitHub App private key on the Sprite
- issue direct push ability only through short-lived session grants

This preserves autonomy for inspection and coding while sharply reducing the blast radius of compromise.

## GitHub Auth And Permission Options

## Option A: GitHub App installation tokens

### Summary

The app private key mints installation tokens. Tokens are short-lived and can be narrowed to specific repositories and permissions.

### What the docs support

- installation tokens can be narrowed with `repositories` or `repository_ids`
- token permissions can be narrowed with `permissions`
- installation tokens expire after 1 hour
- installation tokens can authenticate HTTP Git if the app has `Contents` permission

### Pros

- short-lived
- repo-scopable per token
- permission-scopable per token
- first-class automation model
- no user password or PAT needed for the actual Git operation
- excellent fit for machine access

### Cons

- if the private key is on the Sprite, compromise of that key is serious
- activity is attributed to the app, not the operator
- repo access is limited by app installation scope, so token narrowing does not solve installation overbreadth
- changing installation repo selection programmatically is awkward

### Important limitation

GitHub does support add/remove repository operations on an app installation, but the current docs say:

- `PUT /user/installations/{installation_id}/repositories/{repository_id}` only works for PAT classic with `repo`
- `DELETE /user/installations/{installation_id}/repositories/{repository_id}` only works for PAT classic with `repo`
- those endpoints do not work with GitHub App tokens or fine-grained PATs

That means dynamic repo selection at the installation layer cannot itself be driven by modern app tokens.

### Recommendation

Use installation tokens for:

- resident read access
- possibly local-CLI-mediated temporary write grants

Do **not** keep a broad write-app private key on the Sprite if the redesign goal is to minimize long-lived broad credentials there.

## Option B: GitHub App user access tokens

### Summary

The operator authorizes the app, and the app receives a short-lived user token whose permissions are the intersection of:

- the app’s permissions and installation access
- the user’s own access

The docs also allow further restriction to a single `repository_id`.

### What the docs support

- user access token permissions are the intersection of app permissions and user permissions
- user access tokens can be further restricted with `repository_id`
- device flow is supported for headless/CLI use
- user access tokens expire after 8 hours
- refresh tokens expire after 6 months
- user access tokens can authenticate HTTP Git with `Contents`

### Pros

- better security boundary than resident write installation tokens
- actions are attributable to the operator plus the app
- naturally bounded by the operator’s real repo access
- works well for “grant push access to repo X for this session”
- no private app key is needed on the Sprite

### Cons

- the app still must be installed on the target repo/account
- 8-hour access tokens are longer-lived than 1-hour installation tokens
- refresh-token handling is sensitive
- device flow should only be enabled for real CLI/headless needs
- org owners cannot directly revoke one user token the same way they can revoke PAT approvals; they mainly suspend/uninstall app access, or the app/user revokes authorization

### Recommendation

This is the strongest match for the desired operator workflow **if** you want:

- direct push access
- user attribution
- intersection with operator rights
- no long-lived write secret on the Sprite

I would use this as the **preferred write-grant mechanism** for direct pushes.

## Option C: Fine-grained PATs

### Summary

Repo-scoped, user-owned PATs with configurable expiration and explicit permission selection.

### Pros

- easy conceptual model
- repo selection is explicit
- permissions are explicit
- can be pre-filled with URL templates
- organizations can require approval

### Cons

- still a user-owned secret copied onto infrastructure
- can last up to 1 year or indefinitely
- GitHub explicitly recommends GitHub Apps for organization access and long-lived integrations
- limitations remain for outside collaborators, multi-org access, and some API/features
- 50-token creation limit makes it unattractive as a session-grant primitive

### Recommendation

Use only as a fallback escape hatch for edge cases. Do not build the redesign around PATs.

## Option D: PAT classic

### Pros

- broad compatibility
- required for some app-installation repo add/remove APIs

### Cons

- broad and long-lived
- large blast radius
- explicitly the wrong default for a Sprite-resident agent

### Recommendation

Do not use for agent Git access.

The only justified use is on the operator workstation for rare installation-maintenance operations that GitHub still has not exposed through safer token types.

## Option E: Deploy keys

### Summary

Repo-attached SSH keys with optional write access.

### Pros

- single-repo scope
- no user token needed
- easy Git over SSH

### Cons

- long-lived private key on the Sprite
- no strong user attribution
- docs warn that write deploy keys can act like a collaborator or organization admin equivalent in practice for that repository
- operational cleanup and auditing are worse than app tokens
- if created by a token, token deletion semantics do not necessarily clean up the deploy key automatically in the way you want

### Recommendation

Not recommended for the primary design.

## Option F: OAuth app tokens

Not recommended. GitHub’s current guidance favors GitHub Apps because they provide finer permissions, better repository control, and short-lived tokens.

## Recommended GitHub Model

## Baseline read model

Keep a **read-only GitHub App** for autonomous clone/fetch of a bounded repo set.

Why:

- the agent needs to browse and clone without operator intervention
- read-only app permissions are low-risk compared with write credentials
- the current system already does this well

Two implementation choices:

- acceptable risk / simpler operations: keep the read-only app private key on the Sprite
- stricter security / more operator dependence: mint read tokens locally and ship leases into the Sprite

My recommendation is the first one unless the user explicitly wants zero resident GitHub private keys on the Sprite.

## Direct push grant model

For direct push, prefer **GitHub App user access tokens via device flow**, mediated by the local operator CLI.

Recommended flow:

1. Operator runs `computerctl grant-push --sprite computer --repo owner/repo`
2. CLI performs GitHub App device flow locally
3. CLI requests a user access token restricted to that repository
4. CLI installs a temporary Git credential entry or ephemeral credential file on the Sprite for `computer-mcp-agent`
5. CLI records a lease with expiry metadata
6. Operator can revoke with `computerctl revoke-push --sprite computer --repo owner/repo`
7. Revocation removes the Sprite-side credential material and optionally revokes the OAuth grant/token on the app side

### Why this is the best fit

- push rights are scoped to a specific repo
- write ability inherits the operator’s actual rights
- the Sprite never needs a long-lived write-capable private key
- auditability is better because actions are on behalf of a user and app
- the default expiration boundary is already 8 hours

## Alternative write model

If user attribution is less important than tighter token lifetime, a second-best design is:

- local operator CLI holds the write-app private key
- CLI mints 1-hour repo-scoped installation tokens locally
- CLI refreshes them while the grant is active
- Sprite only ever sees the short-lived token, never the private key

This is also viable. I would choose it only if the user prefers “app-authored writes” over “operator-authored writes”.

## Suggested Operator UX

## Install and lifecycle

Recommended high-level commands:

- `computerctl sprite install --sprite computer --repo owner/repo`
- `computerctl sprite upgrade --sprite computer`
- `computerctl sprite status --sprite computer`
- `computerctl sprite logs --sprite computer --service computerd`
- `computerctl sprite sync-services --sprite computer`

## Read access setup

- `computerctl github grant-read --sprite computer --repos owner/a,owner/b`

This can configure the reader-app installation metadata and the credential helper wiring.

## Temporary direct push

- `computerctl github grant-push --sprite computer --repo owner/repo`
- `computerctl github grant-push --sprite computer --repo owner/repo --duration 4h`
- `computerctl github revoke-push --sprite computer --repo owner/repo`
- `computerctl github list-grants --sprite computer`

The grant output should include:

- repo
- credential type
- granted by
- granted at
- expires at
- refresh state

## Safety defaults

- default to one repo per grant
- default to no workflow-file write unless explicitly requested
- default to automatic expiry
- default to deleting Sprite-side credential material on revoke
- default to refusing grants outside the configured allowlist

## Cloudflare Proxy Strategy

Treat the Cloudflare Worker as part of the system, not as an optional afterthought.

### Why

The current local evidence says the Worker is compensating for at least two real classes of issues:

- path compatibility:
  - raw Sprite `/mcp` was observed returning `404`
  - raw Sprite `/mcp/` was observed returning `200`
- cold-start compatibility:
  - raw Sprite public health could return `502`
  - Worker warmup and retry logic recovers that state

### Recommended role in the new design

Keep the Worker in the same repo with explicit responsibilities:

- normalize `/mcp` to the upstream path that actually works
- warm the Sprite before proxying
- retry cold-start edge failures
- preserve streaming responses
- provide the stable custom-domain entrypoint

### Repo structure recommendation

- `cmd/computerd/` or `internal/...` for Go service
- `proxy/worker/` for Cloudflare Worker code and deploy config
- one operator CLI that can deploy both

### Operational stance

Default all end-user MCP clients to the proxy/custom-domain URL unless the raw Sprite URL is re-validated against the real client matrix and proven reliable.

## `apply-patch-go` Reuse Assessment

`/Users/ashray/code/amxv/apply-patch-go` looks strong enough to reuse.

Evidence observed locally:

- it is explicitly a Go port of the upstream Rust apply-patch crate
- it includes direct CLI tests
- it includes broad tool-behavior tests
- it includes upstream scenario fixture coverage
- it includes a direct Rust binary parity harness
- the local `PORTING_STATUS.md` claims:
  - full-suite green
  - 100% statement coverage
  - 20 curated CLI/tool parity cases
  - all 23 upstream scenario fixtures

### Caveat

The current Rust service adds one important wrapper behavior that `apply-patch-go` alone does not replace:

- relative patch paths are resolved against the required `workdir`

So the Go redesign should keep a small adapter layer:

- resolve relative patch paths against `workdir`
- preserve current error text shape as much as practical
- then call `apply-patch-go`

### Recommendation

Reuse `apply-patch-go` as the patch engine, but wrap it with:

- current `workdir` path-rewrite semantics
- a golden compatibility test suite copied from this repo’s current behavior

I would not reimplement patch semantics from scratch.

## Migration Strategy

## Phase 0: freeze compatibility expectations

Before rewriting, capture golden behavior from the Rust system:

- JSON tool schemas
- tool names and descriptions
- representative success/failure outputs
- session timeout behavior
- output truncation behavior
- cwd behavior
- `apply_patch` path resolution behavior
- current CLI connection/profile behavior

## Phase 1: build Go core and parity tests

Implement:

- PTY session engine
- config model
- patch adapter
- service layer

Add parity tests that compare:

- service output
- HTTP output
- remote CLI output

The current `phase6_cli_parity.rs` is a useful model for what to preserve.

## Phase 2: add transports

Implement:

- HTTP API wrappers
- MCP wrappers
- health endpoints

Do not change external tool names or payload shapes.

## Phase 3: build `computerctl`

Move Sprite install/upgrade/service-sync behavior into the Go operator CLI:

- install binaries or release assets
- write config
- register Sprite Services
- verify health
- verify read access
- manage proxy target config

## Phase 4: introduce new GitHub grant model

Implement the new direct-push path behind feature flags:

- reader app stays as-is or close to it
- direct push grants via user access token device flow
- revoke path
- lease introspection

Keep the old publisher-bundle flow available during migration.

## Phase 5: move proxy into same repo

- copy Worker package into this repo
- parameterize origin and domain cleanly
- add one CLI command to deploy/update the Worker

## Phase 6: canary on Sprite

Run the Go service in parallel with Rust on a separate Sprite or alternate route:

- replay tool calls
- test cold starts
- validate MCP client compatibility through the Worker
- test Git direct-push grant and revoke flows end to end

## Phase 7: default cutover

Cut over only after:

- tool parity passes
- cold-start proxy path is verified
- direct push grant/revoke is operationally clean
- rollback path exists

## Main Risks

- **Session semantic drift:** PTY behavior, cwd reporting, yield timing, and output truncation are easy to get almost right and still break clients.
- **Patch drift:** even with `apply-patch-go`, wrapper semantics must match current service behavior.
- **GitHub repo-selection maintenance:** installation repo add/remove APIs still depend on PAT classic with `repo`, which is operationally awkward.
- **User-token revocation ergonomics:** GitHub App user tokens are a better fit for direct push, but revocation and org-admin controls are not as clean as a pure app-installation model.
- **Proxy underestimation:** the Worker likely hides upstream Sprite edge quirks that will reappear if treated as optional.
- **Too much simplification:** collapsing everything into one resident binary would improve shape but could reintroduce the very secret-sharing problems the current split publisher design avoided.

## Final Recommendation

Build the redesign around this model:

- Go service on the Sprite for tool execution
- one shared core with MCP and HTTP wrappers
- Go operator CLI as the lifecycle and security control plane
- read-only GitHub App for autonomous bounded repo access
- temporary direct push via operator-mediated GitHub App user access tokens, repo-restricted per session
- Cloudflare Worker kept in the same repo and treated as required compatibility infrastructure
- `apply-patch-go` reused as the patch engine behind a compatibility adapter

That gives you the Sprite-first lifecycle you want, keeps the narrow tool surface unchanged, simplifies the implementation structure, and materially improves the security story for direct push access.

## Source URLs

Primary-source docs used:

- https://docs.github.com/en/apps/creating-github-apps/authenticating-with-a-github-app/generating-an-installation-access-token-for-a-github-app
- https://docs.github.com/en/apps/creating-github-apps/authenticating-with-a-github-app/generating-a-user-access-token-for-a-github-app
- https://docs.github.com/en/apps/creating-github-apps/about-creating-github-apps/best-practices-for-creating-a-github-app
- https://docs.github.com/en/apps/creating-github-apps/registering-a-github-app/choosing-permissions-for-a-github-app
- https://docs.github.com/en/rest/apps/installations?apiVersion=2022-11-28
- https://docs.github.com/en/rest/apps/oauth-applications
- https://docs.github.com/en/rest/deploy-keys/deploy-keys
- https://docs.github.com/en/authentication/keeping-your-account-and-data-secure/managing-your-personal-access-tokens
- https://docs.github.com/en/organizations/managing-programmatic-access-to-your-organization/github-credential-types
- https://docs.github.com/en/apps/using-github-apps/reviewing-and-modifying-installed-github-apps
- https://docs.sprites.dev/working-with-sprites/
- https://docs.sprites.dev/cli/authentication/

Repo-local and adjacent local sources used:

- `/Users/ashray/code/amxv/computer-mcp/README.md`
- `/Users/ashray/code/amxv/computer-mcp/docs/agent-sprites-setup-runbook.md`
- `/Users/ashray/code/amxv/computer-mcp/docs/deployment-notes.md`
- `/Users/ashray/code/amxv/computer-mcp/docs/github-app-agent-auth.md`
- `/Users/ashray/code/amxv/computer-mcp/src/server.rs`
- `/Users/ashray/code/amxv/computer-mcp/src/http_api.rs`
- `/Users/ashray/code/amxv/computer-mcp/src/service.rs`
- `/Users/ashray/code/amxv/computer-mcp/src/session.rs`
- `/Users/ashray/code/amxv/computer-mcp/src/publisher.rs`
- `/Users/ashray/code/amxv/computer-mcp/src/apply_patch.rs`
- `/Users/ashray/code/amxv/computer-mcp/src/client.rs`
- `/Users/ashray/code/amxv/computer-mcp/src/bin/computer.rs`
- `/Users/ashray/code/amxv/computer-mcp/src/bin/computer-mcp.rs`
- `/Users/ashray/code/amxv/computer-mcp/scripts/setup-sprite.sh`
- `/Users/ashray/code/amxv/computer-mcp/scripts/upgrade-sprite.sh`
- `/Users/ashray/code/amxv/computer-mcp/scripts/sprite-services.sh`
- `/Users/ashray/code/amxv/computer-mcp/gg/sprite-mcp-investigation-2026-06-26.md`
- `/Users/ashray/code/amxv/computer-mcp-cloudflare-proxy/README.md`
- `/Users/ashray/code/amxv/computer-mcp-cloudflare-proxy/src/index.js`
- `/Users/ashray/code/amxv/apply-patch-go/README.md`
- `/Users/ashray/code/amxv/apply-patch-go/PORTING_STATUS.md`
- `/Users/ashray/code/amxv/apply-patch-go/rust_parity_test.go`
