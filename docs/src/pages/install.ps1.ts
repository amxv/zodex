import { readFile } from "node:fs/promises";
import { join } from "node:path";

export async function GET() {
  const script = await readFile(join(process.cwd(), "..", "scripts", "install.ps1"), "utf8");

  return new Response(script, {
    headers: {
      "Content-Type": "text/plain; charset=utf-8",
      "Cache-Control": "public, max-age=300"
    }
  });
}
