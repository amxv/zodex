---
title: Development and release checks
description: Build, test, inspect, and release the Rust runtime and the embedded Astro documentation site.
order: 15
category: Reference
summary: Commands maintainers should run before changing runtime behavior, CLI behavior, scripts, or docs.
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

The tests cover binary manifests, CLI parity, GitHub App scripts, install script behavior, Sprite scripts, zodex-agent CLI forwarding, MCP tool registration, HTTP API parity, session handling, redaction, and patch application.

## Docs site checks

Run:

```bash
bun install
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

- mention the real binaries: `zodex`, `zodex-agent`, `zodex-client`, `zodexd`, `zodex-prd`
- distinguish operator-machine commands from Sprite-side commands
- keep the read/write access model explicit
- explain when a command needs an active grant
- document both MCP and direct HTTP routes when changing server behavior
- update command examples when Clap arguments change

## Repository scripts

Useful scripts include:

```bash
scripts/install.sh
scripts/mint-gh-app-installation-token.sh
scripts/protect-main-branch.sh
scripts/github_actions_fail_fast.py
```

Run script-specific tests when changing them:

```bash
cargo test --quiet --test install_script
cargo test --quiet --test github_app_scripts
cargo test --quiet --test sprite_scripts
```

## Release awareness

The crate version is defined in `Cargo.toml`. The repository uses tagged releases; at the time this docs site was added, the latest checked-out tag was `v0.2.10`.

When a release changes CLI arguments, binary names, setup behavior, or service layout, update the docs site in the same change.
