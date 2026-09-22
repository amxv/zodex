---
title: "Docs site"
description: "Maintainer reference for running, validating, and deploying the Zodex documentation site."
order: 4
category: Reference
summary: "Repository-maintainer notes for the Astro documentation site."
---

## Local development

Install dependencies and start Astro:

```bash
cd docs
bun install
bun run dev
```

Astro serves the site locally, usually at:

```text
http://localhost:4321
```

## Files to edit

The docs site is isolated under `docs/`:

```text
docs/src/data/docs.ts                site name, repo URL, navigation, categories
docs/src/pages/index.astro           overview page
docs/src/pages/docs/index.astro      grouped docs index
docs/src/pages/docs/[...slug].astro  article route
docs/src/pages/docs.md.ts            raw markdown docs index
docs/src/pages/docs/[...slug].md.ts  raw markdown page route
docs/src/content/docs/**/*.md        documentation pages
docs/src/styles/global.css           visual system
docs/src/styles/landing.css          landing-page style entrypoint
docs/src/styles/landing/*.css        semantic landing-page style sections
docs/scripts/should-build.mjs        Vercel affected-path decision
```

Use `docs/src/content/docs` for most documentation changes. Use `docs/src/pages/index.astro` for the overview narrative and product positioning.

## Validate the docs site

Run:

```bash
cd docs
bun run test:vercel
bun run check
bun run build
```

`test:vercel` protects the monorepo deployment filter, `check` catches Astro and TypeScript issues, and `build` verifies static output and route generation.

## Keep generated files out of commits

The repository ignores:

```text
node_modules/
.astro/
dist/
```

Commit source files, lockfiles, and content. Do not commit local build output.

## Deployment

The site builds to static output in `dist` and can be deployed by any static host:

```bash
cd docs
bun run build
```

Use the host’s static output setting:

```text
dist
```

When the docs site is deployed behind a custom domain, keep the repository link pointed at `https://github.com/amxv/zodex`.

The Vercel project root is `docs/`, but the published `/install.sh` and `/install.ps1` routes read the canonical repository-level `scripts/install.sh` and `scripts/install.ps1`. Vercel outside-root source access must remain enabled. Automatic production deploys should run when `docs/**` or either installer changes and skip unrelated repository-only changes.
