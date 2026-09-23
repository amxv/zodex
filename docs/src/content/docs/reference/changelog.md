---
title: "Changelog"
description: "Release notes for zodex."
order: 5
category: Reference
summary: "Version-by-version changes across the Zodex CLI, Sprite runtime/workflows, and Local mode."
---

This changelog tracks code and product changes in zodex. It intentionally skips docs-site-only updates.

## 0.5.3 — 2026-09-23

- Fixed Windows `zodex local setup` tunnel metadata validation by preserving the required Windows process environment while continuing to exclude the user's full `PATH` and ambient OpenAI credentials.
- Tunnel metadata validation failures now include the underlying `tunnel-client` diagnostic with the supplied runtime key redacted, making permission, credential, and Windows environment failures distinguishable.

## 0.5.2 — 2026-09-23

- Stabilized the macOS Liveboard at the local-only `http://127.0.0.1:64973/` address across Local restarts while keeping the read-only API behind a fresh private capability path for each run.
- Added **Copy Liveboard Link** to the macOS menu bar app and updated Liveboard development, discovery, status, and regression coverage around the stable browser URL.

## 0.5.1 — 2026-09-23

- Fixed Windows private Local file and directory permissions by replacing PowerShell ACL mutation with native Win32 security APIs and reapplying the user-only ACL whenever a private path is secured.
- Added regression coverage for deleting and recreating private Windows files/directories at the same path, plus defensive validation for malformed or missing Windows security descriptors.

## 0.5.0 — 2026-09-23

- Added first-class x86_64 and aarch64 Linux support for Zodex Local, including detached Unix-session lifecycle, captured login-shell execution, user-only runtime-key storage, managed Linux OpenAI tunnel-client assets, terminal-watch defaults, and Linux-aware self-upgrade safety.
- Added native Linux Local smoke coverage and hardened cross-platform Local/history/operator tests so Linux, macOS, and Windows validation remain deterministic in CI.

## 0.4.0 — 2026-09-23

- Added first-class x86_64 Windows support for Zodex Local, including Windows-native path discovery, PowerShell command execution, pipe-backed long-running sessions, process birth-identity/tree cleanup, Windows Credential Manager storage, and managed Windows OpenAI tunnel-client assets.
- Added detached Windows Local start/stop lifecycle, terminal-watch defaults with `clip.exe` clipboard support, and preserved the existing macOS Liveboard/menu-bar experience as a macOS-specific control surface.
- Added Windows operator release artifacts, a checksum-verifying PowerShell installer, Windows-safe self-upgrades, and native Windows CI/package smoke validation.

## 0.3.17 — 2026-09-16

- Fixed `zodex local setup` failing with "contains multiple -darwin-arm64.zip assets" after the upstream `openai/tunnel-client` release started publishing additional `tunnel-client-runtime-*` and `tunnel-client-runtime-cloudflared-*` archives alongside the primary asset. The managed tunnel-client installer now matches the exact `tunnel-client-<version>-<platform>.zip` asset name instead of a bare suffix.

## 0.3.16 — 2026-09-01

- Fixed Local invocation-history admission under short SQLite writer contention so successful `exec_command` and `apply_patch` calls no longer disappear from Liveboard while the underlying tool continues normally.
- Made the Liveboard connection indicator track the live SSE transport independently from slower durable-history recovery, preventing a healthy board from remaining stuck on **Connecting** while catch-up work continues in the background.
- Fixed lazy command-output hydration so completed commands only expand when output is actually available, show an explicit loading state during recovery, and fall back to the exact tool result when PTY chunks were not retained instead of rendering an empty output box.
- Added regression coverage for concurrent invocation bursts, zero-PTY exact-result recovery, outputless commands, delayed output hydration, and the Liveboard connection state.

## 0.3.15 — 2026-08-30

- Added default-on, independently configurable repo-local skill discovery for Zodex Local. On the first successful invocation in an exact workdir, Local can advertise parsed skills found under `<workdir>/.agents/skills` once per ChatGPT conversation/workdir without traversing parent directories or changing the underlying tool result.
- Made focused Liveboard history loading dramatically faster by filtering canonical history roots before hydration and adding an index for legacy orphan-poll grouping, eliminating multi-second timeline queries that could hit the observer proxy timeout.
- Hardened live command lifecycle delivery so process completion, poll-count refreshes, and presentation updates use a priority/coalesced control path instead of lossy queue writes; terminal state now converges promptly even under heavy output.
- Split ephemeral PTY output from durable Liveboard control events, batch live output updates, and reconcile the final durable output tail at EOF so noisy commands cannot evict lifecycle events or leave the UI missing the last output chunk.
- Removed the active-process retention lock race by deriving retention protection from the worker's in-memory lifecycle state, and made Liveboard GETs retry transient observer failures while surfacing the backend error message when recovery still fails.

## 0.3.14 — 2026-08-28

- Made Local history and audit capture fail open so a locked, saturated, unavailable, or degraded evidence pipeline can never reject a model tool call or poison the shared bridge for other Agents.
- Isolated oversized command-output capture to the individual invocation, with bounded raw history and nonblocking completion handling instead of global history backpressure.
- Added large-output spooling for command results: oversized output is saved to a private bounded temporary file while the model receives a compact tail preview plus the file path, character count, line count, and truncation state for follow-up inspection.

## 0.3.13 — 2026-08-28

- Fixed Local context delivery in ChatGPT by carrying `AGENTS`/skill/workdir context inside the primary model-visible tool result instead of a sibling MCP content block that ChatGPT did not forward to the model.
- Added optional `zodex_context` structured output for command tools while keeping stdout, status, cwd, exit code, and stored invocation evidence unchanged; text and error results keep their original result first and append context in the same primary text block.

## 0.3.12 — 2026-08-28

- Added configurable Codex-style Local context injection: each ChatGPT conversation can receive the machine's global skills plus `$CODEX_HOME/AGENTS.override.md` or `AGENTS.md` alongside its first Zodex result, without changing the underlying tool output.
- Added one-time per-Agent/workdir hints when the exact requested workdir contains `AGENTS.override.md` or `AGENTS.md`, with durable delivery state across Local restarts and history retention.
- Added Local config controls for disabling global instructions, repo instruction hints, skill injection, either built-in skill root, or all automatic context, plus additive custom skill roots.

## 0.3.11 — 2026-08-27

- Added Agent-focused Liveboard links for Local so a ChatGPT conversation can resolve to its Zodex Agent and open or copy a Liveboard view focused on that Agent.
- Hardened Local history retention during startup and added stable machine-local code signing for source builds so repeated developer installs can reuse Keychain access without a fresh prompt after every rebuild.

## 0.3.10 — 2026-08-22

- Raised the managed Local runtime's soft and hard open-file limits to 10,240 so larger concurrent Agent workloads have sufficient descriptor capacity.
- Prevented stale asynchronous recovery snapshots from overwriting newer Liveboard SSE updates, keeping concurrent Agent timelines stable and repository-correct.

## 0.3.9 — 2026-08-20

- Fixed Local shutdown getting stuck on a leaderless command process group by persisting exact member birth identities and using them for identity-safe stale cleanup.

## 0.3.8 — 2026-08-18

- Fixed macOS Launch at Login after in-place upgrades by ensuring login-item operations run from the current installed app bundle instead of a stale process whose previous bundle was replaced.
- Hardened menu-bar app replacement so upgrades reliably find and stop the running helper through its lifetime lock before swapping `Zodex.app`, then relaunch it from the new bundle.

## 0.3.7 — 2026-08-18

- Fixed macOS menu validation so AppKit no longer re-enables lifecycle actions that Zodex status marked disabled.
- Made the lifecycle row show `Zodex is Running`, `Starting Zodex…`, and `Stopping Zodex…` so Local transitions are visible immediately while the CLI action is in flight.

## 0.3.6 — 2026-08-18

- Fixed the operator upgrade state model to compile warning-free under Linux CI while preserving the macOS Local safety states.

## 0.3.5 — 2026-08-18

- Added a tiny native macOS menu-bar app for Zodex Local with persistent Start Folder controls, Start/Stop/Liveboard actions, opt-out setup, and user-controlled Launch at Login without auto-starting the Local runtime.
- Rebuilt `zodex upgrade` around a single Rust-owned upgrade state machine with fast no-op checks, streamed human/JSON progress, semver-aware targeting, checksum verification, upgrade locking, Local safety, short check caching, and in-place updates for the CLI and menu app.
- Added menu-bar update controls that consume the CLI upgrade contract directly, including on-demand checks, update progress, and explicit Stop and Update handling when Local is running.

## 0.3.4 — 2026-08-18

- Kept large file changes as structured Liveboard diff cards instead of falling back to generic `apply_patch completed` rows, with fast large-file trimming, time-bounded diffing, and a 500-row preview cap for very large changes.
- Made kill events resolve their exact parent command through the retained process/invocation link, so Liveboard shows a `kill` badge with the command being terminated instead of an opaque termination-request message.

## 0.3.3 — 2026-08-18

- Added presentation schema v3 and history schema v5 with persisted materialized file-change summaries/full bodies, projection-aware timeline/SSE delivery, and batch diff hydration so expanded diffs stay single-request while collapsed diffs remain lightweight.
- Improved Liveboard rendering and streaming performance with lower virtualizer overscan, bounded O(1) SSE/output caches, smaller manual-history pages, stale-detail rejection, unmounted closed drawers, and more reliable Follow Live settling.
- Expanded Liveboard controls with Lucide icons, configurable editor links for changed files, Raw-button visibility, consistent copy controls, clearer runtime/settings presentation, and refined command/diff metadata layout.
- Reworked Agent drag ordering with a floating overlay and sliding sibling columns while preserving each virtualized timeline DOM node and avoiding unnecessary SSE handovers during UI-only reorders.

## 0.3.2 — 2026-08-17

- Made `zodex local watch` open the read-only browser Liveboard by default while preserving the terminal viewer behind explicit `--tui` / TUI-only Agent filters.
- Added a multi-Agent Liveboard with persistent UI-only aliases/order/column sizing, independent virtualized Agent timelines, bounded live command output, canonical structured file diffs, theme-aware local syntax highlighting, and lazy audit drill-down.
- Extended Local observability with presentation schema v2, live event schema v2, canonical timeline/detail/checkpoint routes, output metadata plus raw/display views, selective live PTY output, and durable reconnect/gap recovery.
- Kept browser credentials behind a temporary same-origin capability host: Liveboard JavaScript never receives the managed observer Bearer.

## 0.3.1 - 2026-08-17

- Made Sprite daemon cleanup match exact process commands, preventing the cleanup shell from terminating itself during upgrades.
- Recreated Sprite services in dependency-safe order and added bounded health retries for clean startup transitions.
- Verified the installed Sprite runtime version before restarting services, so requested release drift fails clearly.
- Exposed Sprite-provided language shims to the agent account while keeping user-specific toolchain provisioning outside the public installer.

## 0.3.0 — 2026-08-17

- Added Zodex Local for trusted direct ChatGPT coding on Apple Silicon Macs through a managed OpenAI Secure MCP Tunnel, Keychain-backed credentials, and explicit launchd lifecycle.
- Added one Mac-wide, multi-Agent Local runtime with provider-correlated Agent identities, explicit absolute workdirs, runtime-wide TTL enforcement, and comprehensive child-process cleanup.
- Added durable Local invocation history, normalized file and command evidence, a read-only loopback observability API with SSE, and the `zodex local watch` TUI.
- Hardened command session reaping, output finalization, Local shutdown, tunnel readiness, and stalled launchd startup recovery.
- Added native Apple Silicon Local CI coverage, release-package safety checks, and faster Sprite agent source builds through persistent Cargo caches.

## 0.2.27 — 2026-08-16

- Allowed authenticated public Sprite MCP requests to pass RMCP 3's loopback-oriented host validation, restoring direct and proxy-backed connector access while retaining strict host validation for Local loopback servers.

## 0.2.26 — 2026-08-16

- Upgraded the shared MCP server from RMCP 1.2 to RMCP 3.1.2, adding modern stateless MCP `2026-07-28` support while retaining legacy Sprite client compatibility.
- Required absolute existing workdirs for command and patch calls, preventing implicit or relative execution routing.
- Added automatic access to ChatGPT's opaque `openai/session` request metadata without adding bookkeeping fields to model-visible tool arguments.

## 0.2.25 — 2026-08-15

- Reaped yielded command sessions even when clients never poll again, preventing zombie child processes while preserving final session output and status.

## 0.2.24 — 2026-07-03

- Added support for YOLO direct pushes to Git tags.

## 0.2.23 — 2026-07-02

- Fixed Sprite setup document validation.

## 0.2.22 — 2026-07-01

- Allowed `publish-pr` to work through publisher installations.

## 0.2.21 — 2026-07-01

- Repaired YOLO direct-push plumbing.
- Added regression coverage for YOLO direct-push Git plumbing.

## 0.2.20 — 2026-07-01

- Polished zodex command output.

## 0.2.19 — 2026-07-01

- Installed ARM64 cross-libc headers in the release workflow.

## 0.2.18 — 2026-07-01

- Refined the zodex install/setup flow.
- Raised publisher bundle limits.
- Stabilized CLI parity truncation checks.

## 0.2.17 — 2026-07-01

- Included GitHub Actions workflow permission in publisher tokens.

## 0.2.16 — 2026-06-30

- Fixed the direct-push publisher wire format.

## 0.2.15 — 2026-06-30

- Fixed YOLO direct-push bundle imports.

## 0.2.14 — 2026-06-30

- Enabled YOLO direct `git push` mode.

## 0.2.13 — 2026-06-30

- Allowed registry defaults for Sprite operations.

## 0.2.12 — 2026-06-30

- Fixed push-grant list parsing.

## 0.2.11 — 2026-06-30

- Added GitHub mode commands.

## 0.2.10 — 2026-06-30

- Added the controlled `publish-pr` flow.
- Bound `publish-pr` to the active checkout repository.

## 0.2.9 — 2026-06-28

- Added agent-side GitHub PR creation that reuses temporary push grants.

## 0.2.8 — 2026-06-27

- Refactored Sprite guests to stay runtime-only.

## 0.2.7 — 2026-06-27

- Maintenance release. No direct code behavior changes beyond release preparation.

## 0.2.6 — 2026-06-27

- Maintenance release. No direct code behavior changes beyond release preparation.

## 0.2.5 — 2026-06-27

- Maintenance release. No direct code behavior changes beyond release preparation.

## 0.2.4 — 2026-06-27

- Maintenance release. No direct code behavior changes beyond release preparation.

## 0.2.3 — 2026-06-27

- Maintenance release. No direct code behavior changes beyond release preparation.

## 0.2.2 — 2026-06-27

- Added Sprite-side push-request flow.
- Added the restricted `zodex-agent` surface.
- Dropped self-symlinks from the install script.
- Defaulted Sprite TLS to port `8443`.
- Polished Sprite sync and installer output.

## 0.2.1 — 2026-06-26

- Removed no-op Sprite symlink steps.
- Passed Sprite exec arguments after a separator.
- Improved GitHub push-grant auth UX.
- Fixed clippy warnings in auth UX helpers.
- Updated the canonical repo slug to `amxv/zodex`.
- Added Apple Silicon release target support.

## 0.2.0 — 2026-06-26

- Introduced the zodex compatibility layer.
- Centralized service dispatch across transports.
- Added the Rust operator control plane.
- Integrated the zodex proxy component.
- Made push grants the default workflow.
- Fixed repo-scoped push-grant selection.
- Finished the zodex cleanup and supported product surface.
- Removed Docker-specific identity from the runtime surface.
- Preferred device-flow push grants.
- Switched the project license to Apache 2.0.

## 0.1.30 — 2026-03-21

- Added force-recreate recovery for Sprite Services.
- Added a one-command Sprite upgrade flow.
- Implemented the concurrent session broker.

## 0.1.29 — 2026-03-21

- Fixed publisher socket directory access for `publish-pr`.

## 0.1.28 — 2026-03-21

- Configured default agent Git commit identity.

## 0.1.27 — 2026-03-21

- Made Sprite upgrades recycle managed services.

## 0.1.26 — 2026-03-21

- Added reader-backed Git clone auth for agents.

## 0.1.25 — 2026-03-21

- Added a dedicated agent workspace model for Sprite deployments.

## 0.1.24 — 2026-03-20

- Fixed root MCP routing on the exact `/mcp` path.

## 0.1.23 — 2026-03-20

- Fixed root MCP path canonicalization.

## 0.1.22 — 2026-03-20

- Consolidated the Sprite workflow into the repo skill.
- Fixed connect verification and narrowed insecure TLS retry behavior.
- Added the Sprite setup workflow and sanitized default app IDs.
- Fixed MCP trailing-slash handling.

## 0.1.21 — 2026-03-19

- Extracted the shared computer service.
- Routed MCP through the computer service.
- Added the HTTP computer API.
- Added the computer HTTP CLI.
- Packaged the computer client.
- Verified transport and CLI parity.
- Fixed computer client support for self-signed HTTPS.
- Stabilized MCP exec parity tests.

## 0.1.20 — 2026-03-19

- Improved tool UX with workdir-aware patches, richer session output, and idle timeout behavior.
- Fixed pre-existing clippy warnings.

## 0.1.19 — 2026-03-19

- Added a fail-fast GitHub Actions run watcher script.
- Enabled direct SSH access for the Runpod agent user.

## 0.1.18 — 2026-03-19

- Fixed Go tool installation in the Runpod image.

## 0.1.17 — 2026-03-19

- Updated the Runpod base image.
- Added Bun to the Runpod image.

## 0.1.16 — 2026-03-19

- Fixed Runpod API update requests.
- Polished the Runpod agent development environment.

## 0.1.15 — 2026-03-19

- Added the Runpod API helper script.
- Switched to the ring-only rustls provider.

## 0.1.14 — 2026-03-19

- Built the Runpod image on the Runpod base.

## 0.1.13 — 2026-03-19

- Split generic and Runpod images.

## 0.1.12 — 2026-03-19

- Fixed Runpod container permissions.

## 0.1.11 — 2026-03-19

- Fixed Runpod container bootstrap.

## 0.1.10 — 2026-03-19

- Maintenance release. No direct code behavior changes beyond release preparation.

## 0.1.9 — 2026-03-19

- Maintenance release. No direct code behavior changes beyond release preparation.

## 0.1.8 — 2026-03-19

- Maintenance release. No direct code behavior changes beyond release preparation.

## 0.1.7 — 2026-03-19

- Maintenance release. No direct code behavior changes beyond release preparation.

## 0.1.6 — 2026-03-18

- Maintenance release. No direct code behavior changes beyond release preparation.

## 0.1.5 — 2026-03-18

- Added annotations for MCP tools and verified them in tests.

## 0.1.4 — 2026-03-18

- Shipped Ubuntu 22.04 release artifacts.

## 0.1.3 — 2026-03-18

- Added Runpod HTTP proxy listener mode.

## 0.1.2 — 2026-03-18

- Maintenance release. No direct code behavior changes beyond release metadata.

## 0.1.1 — 2026-03-18

- Added the apply-patch API reference.
- Completed the core server and PTY exec runtime.
- Integrated the codex-style `apply_patch` tool and tests.
- Implemented systemd-backed computer MCP CLI management.
- Added the VPS bootstrap installer script.
- Added TLS setup and HTTPS MCP serving.
- Hardened key redaction and deploy-readiness behavior.
- Added process-mode fallback for container hosts.
- Added GitHub App auth and PR workflow helpers.
- Improved GitHub App plan-error handling and permissions JSON.
- Added the publisher daemon and process-mode PR publishing.
- Trimmed default installer config.
- Added the Runpod proxy and agent image packaging.
- Added GitHub release packaging.
