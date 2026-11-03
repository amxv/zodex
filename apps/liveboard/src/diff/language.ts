export type DiffSyntaxLanguage =
  | 'rust'
  | 'typescript'
  | 'javascript'
  | 'go'
  | 'bash'
  | 'python'
  | 'css'
  | 'json'
  | 'ini'
  | 'markdown'
  | 'yaml'
  | 'xml'
  | 'sql'
  | 'java'
  | 'cpp'
  | 'kotlin'
  | 'swift'
  | 'dockerfile'
  | 'diff'

const EXTENSION_TO_LANGUAGE: Readonly<Record<string, DiffSyntaxLanguage>> = {
  bash: 'bash',
  cc: 'cpp',
  cjs: 'javascript',
  cpp: 'cpp',
  cts: 'typescript',
  css: 'css',
  cxx: 'cpp',
  diff: 'diff',
  dockerfile: 'dockerfile',
  fish: 'bash',
  go: 'go',
  h: 'cpp',
  hh: 'cpp',
  hpp: 'cpp',
  htm: 'xml',
  html: 'xml',
  hxx: 'cpp',
  ini: 'ini',
  java: 'java',
  js: 'javascript',
  json: 'json',
  jsonc: 'json',
  jsx: 'javascript',
  kt: 'kotlin',
  kts: 'kotlin',
  markdown: 'markdown',
  md: 'markdown',
  mdx: 'markdown',
  mjs: 'javascript',
  mts: 'typescript',
  patch: 'diff',
  psql: 'sql',
  py: 'python',
  pyw: 'python',
  rs: 'rust',
  sh: 'bash',
  sql: 'sql',
  svg: 'xml',
  swift: 'swift',
  toml: 'ini',
  ts: 'typescript',
  tsx: 'typescript',
  xhtml: 'xml',
  xml: 'xml',
  yaml: 'yaml',
  yml: 'yaml',
  zsh: 'bash',
}

export const EAGER_DIFF_LANGUAGES: readonly DiffSyntaxLanguage[] = [
  'rust',
  'typescript',
  'javascript',
  'go',
  'bash',
  'python',
  'css',
  'json',
  'ini',
  'markdown',
  'yaml',
  'xml',
  'sql',
  'java',
  'cpp',
  'kotlin',
  'swift',
  'dockerfile',
  'diff',
]

export function resolveDiffSyntaxLanguage(filePath: string): DiffSyntaxLanguage | null {
  const fileName = filePath.trim().toLowerCase().split('/').filter(Boolean).pop()
  if (!fileName) return null
  if (
    fileName === 'dockerfile' ||
    fileName.startsWith('dockerfile.') ||
    fileName === 'containerfile' ||
    fileName.startsWith('containerfile.')
  ) return 'dockerfile'
  const dotIndex = fileName.lastIndexOf('.')
  if (dotIndex < 0 || dotIndex === fileName.length - 1) return null
  return EXTENSION_TO_LANGUAGE[fileName.slice(dotIndex + 1)] ?? null
}
