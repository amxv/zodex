import { readFile } from "node:fs/promises";
import { join } from "node:path";

export async function GET() {
  const script = await readFile(join(process.cwd(), "scripts", "install.sh"), "utf8");

  return new Response(script, {
    headers: {
      "Content-Type": "text/x-shellscript; charset=utf-8",
      "Cache-Control": "public, max-age=300"
    }
  });
}
