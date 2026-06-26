# Zodex Rust Evolution Plan

- Date: 2026-06-26
- Scope: planning only
- Current project/repo name: `computer-mcp`
- Target product/CLI name: `zodex`

## State of Current System

The current system is already a functional Rust remote-coding stack with three useful properties:

1. it exposes a narrow remote coding surface
2. it already separates shared tool logic from HTTP and MCP wrappers
3. it already has a working Sprite deployment path

### Current external surface

The current agent-facing tool surface is:

- `exec_command`
- `write_stdin`
- `apply_patch`

These are exposed through:

- MCP over streamable HTTP at `/mcp` and `/mcp/`
- HTTP API at:
  - `/v1/exec-command`
  - `/v1/write-stdin`
  - `/v1/apply-patch`
- a thin remote HTTP CLI in the `computer` binary

### Current internal structure

The Rust code already has the right structural direction:

- `src/service.rs` provides a shared service layer
- `src/http_api.rs` wraps that service for HTTP
- `src/server.rs` wraps that service for MCP
- `src/session.rs` owns PTY-backed command/session behavior
- `src/apply_patch.rs` wraps the patch engine with current `workdir` semantics
- `src/client.rs` and `src/bin/computer.rs` provide the remote CLI path

### Current runtime shape

The current repo supports:

- `systemd` on standard Linux hosts
- detached process mode for non-`systemd` environments
- Sprite Services as the lifecycle owner on Sprites

For Sprites, the current model intentionally avoids running as the built-in `sprite` user and instead uses:

- `computer-mcp-agent`
- `computer-mcp-publisher`

That preserves a security boundary between agent execution and write-capable GitHub credentials.

### Current GitHub model

The current GitHub model uses two GitHub Apps:

- read-only reader app for clone/fetch
- write-capable publisher app behind a local publisher daemon

That model is good for “agent opens PRs without ever holding a write credential,” but it is not the same as the desired new workflow.

### Approved direction

The approved new direction is:

- keep the project in Rust for now
- do not do a full Go rewrite
- standardize on a simpler, operator-friendly GitHub write model
- keep read access available by default
- make push access temporary, repo-specific, and easy to toggle on/off from the operator’s local machine
- rename the project and CLI to `zodex`

### Approved GitHub access model

The intended day-to-day workflow is:

1. ChatGPT uses the remote tools to clone and inspect a repo on the Sprite
2. the operator decides whether direct push should be allowed
3. the operator runs a local command to grant push access to one repo
4. ChatGPT pushes normally with `git push`
5. the operator turns push access off afterward

The standard model for this plan is:

- **read access**: always available through a read-only GitHub App
- **write access**: off by default, granted temporarily per repo from the operator machine

## State of Ideal System

The ideal system is a cleaned-up Rust product named `zodex` with a clear split between:

- remote execution runtime on the Sprite
- local operator control plane on the user’s machine
- optional but first-class proxy layer for public MCP compatibility

### Ideal product shape

The target product should consist of:

- a Rust daemon on the Sprite for tool execution
- a Rust local operator CLI for install, upgrade, service sync, health checks, and credential grants
- the Cloudflare Worker in the same repo as a supported deployable component

### Ideal naming

Target names:

- product: `zodex`
- primary operator CLI: `zodex`
- daemon: `zodexd`
- optional publisher-compatible helper names should be minimized or removed if obsolete

The safest path is to rename user-facing binaries and docs first, then retire internal legacy names gradually.

### Ideal tool/transport architecture

The Rust code should be organized around:

- one shared core execution/service layer
- one HTTP adapter
- one MCP adapter
- one local operator CLI

That means the current `service.rs` pattern should become more explicit and more central, not less.

### Ideal GitHub model

The standardized GitHub model should be:

- a resident read-only GitHub App path for clone/fetch
- temporary write grants installed from the operator machine
- no long-lived write-capable GitHub App private key stored on the Sprite by default

Preferred write grant model:

- GitHub App user access token via device flow from the operator machine
- restricted to the target repo
- placed temporarily on the Sprite for the agent user
- removable with one local command

Fallback write grant model:

- locally minted repo-scoped GitHub App installation token from the operator machine

### Ideal operator workflow

Target operator flow:

1. `zodex` installs or upgrades the Sprite deployment
2. ChatGPT clones and explores repos through read access
3. operator grants push access with one command:
   - `zodex github grant-push --sprite <name> --repo owner/repo`
4. ChatGPT pushes changes normally
5. operator revokes push access:
   - `zodex github revoke-push --sprite <name> --repo owner/repo`

No GitHub settings editing, no manual key copy/paste, no ad hoc token handling during normal usage.

### Ideal proxy stance

The Cloudflare Worker should live in the same repo and be treated as part of the system.

Reason:

- current evidence indicates it solves real compatibility issues around:
  - `/mcp` vs `/mcp/`
  - Sprite cold wake behavior
  - public-edge reliability for MCP clients

The ideal system does not pretend the proxy is incidental.

## Cross-provider requirements

These requirements must remain true across local CLI, HTTP API, MCP, and Sprite deployment:

- tool names remain stable unless intentionally versioned
- tool payload semantics remain stable
- PTY session behavior remains stable
- `apply_patch` keeps current `workdir`-based path resolution semantics
- output truncation and timeout semantics remain stable
- read access can be enabled independently from write access
- write access is disabled by default
- any temporary write grant is repo-scoped and observable
- revocation removes Sprite-side credential material
- self-signed TLS and Sprite public/proxy paths remain supported during migration

## Plan Phases

## Phase 1: Freeze Compatibility And Introduce `zodex`

### Files to read before starting

- `src/bin/computer-mcp.rs`
- `src/bin/computer.rs`
- `src/server.rs`
- `src/http_api.rs`
- `src/service.rs`
- `src/client.rs`
- `Cargo.toml`
- `README.md`
- `docs/github-app-agent-auth.md`

### What to do

- Introduce `zodex` in binaries, help text, docs, and operator-facing commands.
- Finalize binary mapping:
  - `zodex` for the operator CLI
  - `zodexd` for the daemon
- Preserve compatibility with old names using aliases or temporary wrappers.
- Freeze a compatibility baseline for:
  - tool schemas
  - session behavior
  - current remote CLI outputs that matter

### Validation strategy

- ensure existing CLI and service tests still pass
- add or expand golden tests for:
  - tool JSON shapes
  - MCP tool registration names
  - current remote CLI behavior
- verify help text and binary naming are internally consistent

### Risks / fallbacks

- Risk: renaming everything at once causes churn
- Fallback: rename user-facing surfaces first and defer internal renames

## Phase 2: Consolidate The Rust Core

### Files to read before starting

- `src/service.rs`
- `src/session.rs`
- `src/server.rs`
- `src/http_api.rs`
- `src/protocol.rs`
- `tests/phase6_cli_parity.rs`

### What to do

- Make the shared service layer the explicit center of the system.
- Reduce duplicate HTTP/MCP logic.
- Clarify boundaries between:
  - core tool execution
  - transport wrappers
  - runtime/lifecycle management
  - operator control functions
- Leave the external tool surface unchanged.

### Validation strategy

- run existing parity tests
- add regression tests for:
  - session handles
  - `kill_process`
  - timeout behavior
  - cwd reporting
  - output truncation

### Risks / fallbacks

- Risk: cleanup changes behavior subtly
- Fallback: expand parity coverage before large refactors and refactor in smaller slices

## Phase 3: Build The Operator Control Plane And Standardize GitHub Access

### Files to read before starting

- `src/publisher.rs`
- `src/bin/computer-mcp.rs`
- `src/client.rs`
- `docs/github-app-agent-auth.md`
- `scripts/mint-gh-app-installation-token.sh`
- `tests/github_app_scripts.rs`
- `scripts/setup-sprite.sh`
- `scripts/upgrade-sprite.sh`
- `scripts/sprite-services.sh`
- `docs/agent-sprites-setup-runbook.md`
- `docs/deployment-notes.md`

### What to do

- Move install/upgrade/service-sync behavior out of shell-script-first flows and into the Rust operator CLI.
- Keep the read-only GitHub App path as the default clone/fetch mechanism.
- Standardize the write model:
  - write access is off by default
  - the operator CLI grants temporary repo-specific push access
  - the Sprite receives only temporary credential material
- Implement first-class operator commands for:
  - install
  - upgrade
  - service sync
  - status
  - logs
  - health verification
  - `grant-push`
  - `revoke-push`
  - `list-grants`
- Prefer GitHub App user access token device flow for direct pushes.
- Keep locally minted repo-scoped installation tokens as a fallback.
- Keep the current publisher-daemon PR flow only as a legacy or PR-only mode.
- Keep shell scripts only as compatibility wrappers if needed.

### Validation strategy

- verify install from a clean Sprite
- verify upgrade on an existing Sprite
- verify Sprite Services are recreated correctly
- verify agent writeability of the workspace
- verify clone/fetch still works with read-only app setup
- verify grant installs a temporary write credential for one repo only
- verify `git push` succeeds during an active grant
- verify `git push` fails again after revoke
- verify revocation removes Sprite-side credential material

### Risks / fallbacks

- Risk: replacing working scripts too aggressively slows delivery
- Fallback: make the Rust CLI call the scripts first, then inline behavior incrementally
- Risk: GitHub App user token flow adds operator-side complexity
- Fallback: ship installation-token-based local grants first, then add user-token grants
- Risk: some users still need the PR-only publisher flow
- Fallback: keep it behind a config flag during transition

## Phase 4: Integrate The Proxy And Complete Runtime Migration

### Files to read before starting

- adjacent local proxy repo implementation currently used operationally
- `src/server.rs`
- Sprite deployment commands and route assumptions in the Rust CLI

### What to do

- Move the Cloudflare Worker into this repo under a dedicated subdirectory.
- Treat it as a supported `zodex` component.
- Add CLI support for:
  - proxy config inspection
  - proxy deploy/update
  - origin verification against the Sprite URL
- Document proxy responsibilities explicitly:
  - path normalization
  - cold-wake warmup
  - retry behavior
  - MCP streaming preservation
- Finish renaming binaries, packaging, service names, and docs to `zodex`.
- Provide temporary compatibility shims for:
  - `computer-mcp`
  - `computer`
  - `computer-mcpd`
  - `computer-mcp-prd`
- Delay config-path migration unless there is a clear reason to change it.

### Validation strategy

- verify raw Sprite path behavior
- verify Worker path behavior
- verify cold-start health through Worker
- verify MCP initialize path through Worker
- verify custom-domain route if configured

### Validation strategy

- verify raw Sprite path behavior
- verify Worker path behavior
- verify cold-start health through the Worker
- verify MCP initialize through the Worker
- install and upgrade from both fresh and existing setups
- verify old commands either continue to function temporarily or fail with a clear migration message
- verify docs and examples only reference supported names

### Risks / fallbacks

- Risk: the team assumes the proxy can be removed prematurely
- Fallback: keep the proxy as the default front door until raw Sprite URL behavior is re-validated against actual MCP clients
- Risk: the rename breaks existing operators and scripts
- Fallback: keep legacy aliases for at least one migration release

## Phase 5: Make The New Workflow The Default

### Files to read before starting

- full operator CLI implementation
- daemon and transport code
- GitHub auth integration
- proxy deployment support
- updated setup and operator docs

### What to do

- make the temporary repo-scoped push-grant model the documented default
- reduce emphasis on the legacy PR-only publisher flow
- update setup docs around:
  - read access setup
  - temporary push grants
  - revoke flow
  - proxy deployment
  - Sprite-first operations

### Validation strategy

- full end-to-end operator scenario:
  - install
  - clone via read access
  - inspect and discuss code
  - grant push to one repo
  - push successfully
  - revoke push
  - verify push fails after revoke
- confirm no GitHub settings interaction is required in the normal flow after one-time app setup

### Risks / fallbacks

- Risk: some users still prefer PR-only isolation
- Fallback: retain an explicit legacy mode for PR-only publishing

## Recommended Implementation Decisions

- Keep the repo in Rust and evolve it instead of rewriting in Go.
- Standardize on:
  - read-only GitHub App for autonomous clone/fetch
  - temporary per-repo push grants from the operator machine
- Prefer GitHub App user access tokens for direct-push grants.
- Keep locally minted repo-scoped installation tokens as fallback.
- Do not keep a long-lived write-capable GitHub App private key on the Sprite by default.
- Treat the Cloudflare Worker as a supported first-class component.
- Rename the project and CLI to `zodex`, with compatibility shims during migration.

## Deliverables

The plan should produce these deliverables:

- renamed Rust operator CLI: `zodex`
- renamed daemon: `zodexd`
- cleaned-up shared core/service architecture
- Rust-native Sprite install/upgrade/control-plane commands
- repo-scoped temporary push grant feature
- revoke/list-grants feature
- in-repo proxy package and deployment support
- updated operator docs reflecting the new default workflow

## Final Recommendation

Do not rewrite the product in Go right now.

The fastest route to the intended operator experience is:

1. keep the existing Rust foundation
2. simplify the architecture around one shared core
3. move the control plane into a clearer Rust CLI
4. replace the default write path with temporary repo-scoped grants
5. rename and re-present the product as `zodex`

That path preserves the working system, minimizes reimplementation risk, and directly targets the operator workflow that has now been explicitly approved.
