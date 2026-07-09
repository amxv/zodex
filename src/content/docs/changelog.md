---
title: Changelog
description: "Release notes for zodex."
order: 99
category: Reference
summary: Version-by-version changes for the zodex CLI, agent runtime, proxy, publisher, and Sprite workflows.
---

This changelog tracks code and product changes in zodex. It intentionally skips docs-site-only updates.

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
