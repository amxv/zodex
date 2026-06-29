export const siteConfig = {
  name: "zodex",
  strapline: "Sprite-first remote coding runtime",
  description:
    "Documentation for zodex, a Sprite-first remote coding runtime and operator CLI that gives coding agents a real Linux workspace, a focused MCP tool surface, read-only GitHub access by default, and explicit repo-scoped push grants for writes.",
  repoUrl: "https://github.com/amxv/zodex"
} as const;

export const docCategories = [
  "Start",
  "Architecture",
  "GitHub Access",
  "Operations",
  "Reference"
] as const;

export const primaryNav = [
  { href: "/", label: "Overview" },
  { href: "/docs", label: "Docs" },
  { href: siteConfig.repoUrl, label: "GitHub", external: true }
];
