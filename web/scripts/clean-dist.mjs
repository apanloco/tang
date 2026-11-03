// Empties `dist/` before a build, except for the `.gitkeep` that makes the folder exist
// in a checkout (the Rust program embeds it, and wants it to be there before any build).
import { mkdirSync, readdirSync, rmSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

const dist = join(fileURLToPath(new URL("..", import.meta.url)), "dist");
mkdirSync(dist, { recursive: true });
for (const name of readdirSync(dist)) {
  if (name !== ".gitkeep") rmSync(join(dist, name), { recursive: true, force: true });
}
