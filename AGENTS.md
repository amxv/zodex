# AGENTS.md

## Project overview

Zodex is a Rust MCP coding harness with two first-class execution modes:

- **Local** runs directly as the logged-in user on a trusted Apple Silicon Mac or x86_64 Windows machine and connects through an OpenAI Secure MCP Tunnel.
- **Sprite** runs on a wake-on-demand remote Linux Sprite and connects through the canonical Cloudflare Worker front door.

Both modes expose exactly three model-facing tools: `exec_command`, `write_stdin`, and `apply_patch`. Model-visible execution and patch calls require an explicit absolute existing `workdir`; there is no ambient working-directory fallback.

## Repository layout

The repository is organized by product responsibility:

```text
src/                        Rust core, runtime, CLI, and MCP implementation
apps/liveboard/             SolidJS observability dashboard
apps/menubar/               native macOS menu bar app
services/cloudflare-proxy/  Cloudflare Worker used by Sprite deployments
docs/                       Astro documentation site
scripts/                    repository, install, and release automation
examples/                   public integration examples
tests/                      Rust integration tests
```

Keep component-specific source inside its owning directory. Do not put frontend, Swift, docs-site, or Worker source into the root Rust `src/` tree.

## Product command shape

The public operator root is mode-first:

```text
zodex local ...
zodex sprite ...
zodex upgrade
```

Do not reintroduce generic root Linux lifecycle, TLS, proxy, GitHub, or direct HTTP command families. Root `zodex upgrade` upgrades only the operator; remote runtime upgrades use `zodex sprite upgrade`.

Sprite power state is automatic. There is no public `zodex sprite start` or `stop`; incoming work wakes the Sprite. `zodex sprite restart` repairs the managed Zodex service stack, while `zodex sprite sync` is advanced desired-state reconciliation.

## Sprite trust boundary

The operator `zodex` binary lives on the user's machine. A Sprite receives the restricted guest runtime:

- `zodex-agent`
- `git-remote-zodex`
- `zodexd`
- `zodex-prd`

Do not install the operator CLI into the guest merely for convenience.

GitHub access intentionally uses two Apps:

- reader App: read-only repository contents for clone/fetch;
- writer App: contents, pull requests, and workflows write access for configured repositories, with Device Flow enabled for push-grant workflows.

The writer PEM belongs to `zodex-publisher` and must remain unreadable by `zodex-agent`. PR publishing keeps writer tokens inside `zodex-prd`. Direct push requires an explicit repo grant or active YOLO policy plus writer installation coverage.

Operator GitHub policy commands are under `zodex sprite github`:

```text
zodex sprite github grant-push
zodex sprite github revoke-push
zodex sprite github list-grants
zodex sprite github yolo
zodex sprite github default
zodex sprite github status
```

The restricted guest interface remains:

```text
zodex-agent github request-push
zodex-agent github revoke-push
zodex-agent github list-grants
zodex-agent github publish-pr
```

`zodex sprite github default` removes YOLO policy only; it must not silently revoke unrelated explicit push grants.

## Sprite connection architecture

The supported ChatGPT path is Cloudflare Worker → public Sprite HTTPS wake edge → plain-HTTP `zodexd` Sprite Service. The raw Sprite URL is an upstream/wake origin, not the normal ChatGPT connector endpoint. The MCP query capability remains secret and must be redacted from routine status/log output.

Worker deployment is release-self-contained through `zodex sprite proxy`; do not require a Zodex source checkout or manual Wrangler project edits. The Worker may retry idempotent readiness probes, but a dispatched MCP request must be forwarded at most once.

## Validation

Before submitting changes, run the repository checks appropriate to the files touched:

```bash
bash scripts/check.sh
(cd apps/liveboard && bun run typecheck && bun run test && bun run build)
(cd services/cloudflare-proxy && bun test src/index.test.js)
(cd docs && bun run test:vercel && bun run check && bun run build)
actionlint .github/workflows/*.yml
```

Normal CI must validate product/code contracts directly and must not depend on private or historical planning files under `gg/` or `tmp/gg/`.
