export const siteConfig = {
  name: "zodex",
  strapline: "ChatGPT-native remote coding workspace",
  description:
    "Documentation for zodex, a ChatGPT-native remote coding workspace that gives GPT models a real Sprite-backed Linux machine, a familiar command/stdin/patch MCP surface, and operator-chosen GitHub write modes: PR-only, push-on-approval, or scoped YOLO.",
  repoUrl: "https://github.com/amxv/zodex",
  accentColor: "#be123c",
  accentColorDark: "#fb7185",
  footerSections: [
    {
      title: "zodex",
      text:
        "A ChatGPT-native remote coding workspace for real Linux work, normal Git workflows, and operator-controlled write autonomy."
    },
    {
      title: "What this site covers",
      text:
        "ChatGPT setup, Sprite deployment, write modes, GitHub permissions, MCP tooling, service operations, and the runtime behavior agents depend on."
    },
    {
      title: "Repository",
      linkPrefix: "Source: ",
      linkHref: "https://github.com/amxv/zodex",
      linkLabel: "github.com/amxv/zodex"
    }
  ]
} as const;

export const docCategories = [
  "Start",
  "Architecture",
  "GitHub Access",
  "Operations",
  "Reference"
] as const;

export const primaryNav = [
  { href: "/docs", label: "Docs" },
  { href: siteConfig.repoUrl, label: "GitHub", external: true }
];
