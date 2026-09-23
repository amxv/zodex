---
title: "Development"
description: "Maintainer reference for building, testing, and releasing Zodex itself; not required to operate Sprite or Local."
order: 3
category: Reference
summary: "Repository-maintainer checks for runtime, CLI, scripts, releases, and docs."
---

## Rust checks

Run the full Rust test suite:

```bash
cargo test --quiet
```

For CLI behavior changes, also inspect help output:

```bash
cargo run --quiet --bin zodex -- --help
cargo run --quiet --bin zodex -- sprite --help
cargo run --quiet --bin zodex -- proxy --help
cargo run --quiet --bin zodex -- github --help
cargo run --quiet --bin zodex-agent -- --help
cargo run --quiet --bin zodex-agent -- github publish-pr --help
```

The tests cover binary manifests, CLI behavior, GitHub App scripts, install behavior, Sprite scripts, zodex-agent forwarding, MCP tool registration, session handling, redaction, patch application, and mode-first product contracts.

## Stable Keychain access for source builds

macOS Keychain tracks the code signature of the program that reads the Local tunnel key. A newly compiled ad-hoc binary has a new identity and therefore prompts again. Maintainers who repeatedly install source builds can create a stable, machine-local signing identity once:

```bash
bash scripts/setup-local-codesigning.sh
```

macOS asks for one-time approval to trust that identity for code signing. Afterward, `scripts/install.sh` automatically signs ad-hoc local operator builds with `Zodex Local Development`. Release binaries that already have a stable signature are left unchanged. Set `ZODEX_CODESIGN_IDENTITY` to another available identity, or to `-` to disable local signing explicitly.

The signing private key remains in the user's login Keychain. The tunnel runtime key remains a separate Keychain item and is never exported by this setup.

## Liveboard checks

Liveboard is an isolated frontend app under `apps/liveboard`. Its production assets are embedded into the Linux, macOS, and Windows `zodex` operator binaries by `build.rs`.

For visual development against the real currently running Local runtime, use the attached dev server:

```bash
cd apps/liveboard
bun run dev:live
```

The launcher builds a repo-local Zodex viewer once, resolves the active runtime with `zodex local watch url`, and runs Vite with HMR. Vite proxies Liveboard API, SSE, and preference requests through that runtime-owned same-origin capability host, so the observability bearer remains outside browser JavaScript. It does not restart or replace the running Local runtime.

```bash
cd apps/liveboard
bun install --frozen-lockfile
bunx playwright install chromium webkit
bun run typecheck
bun run test
bun run test:browser
bun run test:browser:webkit
bun run build
```

Do not commit `apps/liveboard/node_modules/` or `apps/liveboard/dist/`.

For an embed-required operator validation, build the frontend first and then run Cargo with:

```bash
ZODEX_LIVEBOARD_EMBED_REQUIRED=1 cargo test
```

CI builds the frontend for native Linux, macOS, and Windows operator validation. Release jobs also embed the same Liveboard assets into every supported Local operator artifact.

## Docs site checks

Run:

```bash
cd docs
bun install
bun run test:vercel
bun run check
bun run build
```

Do not commit generated Astro output:

```text
.astro/
dist/
node_modules/
```

These paths are ignored.

## Docs content rules

Keep docs tied to actual zodex behavior:

- mention the real binaries: `zodex`, `zodex-agent`, `git-remote-zodex`, `zodexd`, `zodex-prd`
- distinguish operator-machine commands from Sprite-side commands
- keep the read/write access model explicit
- explain when a command needs an active grant
- keep MCP as the supported remote coding transport; do not reintroduce deleted legacy transports
- update command examples when Clap arguments change
- when Local observability routes, response fields, filters, SSE event types, discovery fields, API/presentation versions, or recovery semantics change, update [Local observability API](/docs/local/observability-api) in the same change
- when Liveboard/TUI controls, board behavior, presentation, or recovery UX changes, update [Watch and Liveboard](/docs/local/watch) in the same change

## Repository scripts

Useful scripts include:

```bash
scripts/install.sh
scripts/setup-local-codesigning.sh
scripts/mint-gh-app-installation-token.sh
scripts/protect-main-branch.sh
scripts/github_actions_fail_fast.py
```

Run script-specific tests when changing them:

```bash
cargo test --quiet --test install_script
cargo test --quiet --test github_app_scripts
```

## Release awareness

The crate version is defined in `Cargo.toml`. The repository uses tagged releases.

When a release changes CLI arguments, binary names, setup behavior, service layout, Liveboard assets, or public observer contracts, update the docs site in the same change.
