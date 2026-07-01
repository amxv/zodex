---
title: Docs site maintenance
description: Run, edit, validate, and deploy the Astro documentation site that lives inside the zodex repository.
order: 16
category: Reference
summary: Developer notes for maintaining the embedded Astro docs app alongside the Rust runtime.
---

## Local development

Install dependencies and start Astro:

```bash
bun install
bun run dev
```

Astro serves the site locally, usually at:

```text
http://localhost:4321
```

## Files to edit

The docs site lives next to the Rust project:

```text
src/data/docs.ts                site name, repo URL, navigation, categories
src/pages/index.astro           overview page
src/pages/docs/index.astro      grouped docs index
src/pages/docs/[...slug].astro  article route
src/pages/docs.md.ts            raw markdown docs index
src/pages/docs/[...slug].md.ts  raw markdown page route
src/content/docs/*.md           documentation pages
src/styles/global.css           visual system
```

Use `src/content/docs` for most documentation changes. Use `src/pages/index.astro` for the overview narrative and product positioning.

## Validate the docs site

Run:

```bash
bun run check
bun run build
```

`check` catches Astro and TypeScript issues. `build` verifies static output and route generation.

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
bun run build
```

Use the host’s static output setting:

```text
dist
```

When the docs site is deployed behind a custom domain, keep the repository link pointed at `https://github.com/amxv/zodex`.
