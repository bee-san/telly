// Copies Hoshidicts, the dictionary engine inside Hachidori (GPL-3.0), from
// the Hachidori that `npm run prepare:hachidori` fetched into static/hoshi,
// where Telly for Android's lookups load it (src/lib/lookup.svelte.ts).
import { copyFileSync, existsSync, mkdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const from = join(root, "src-tauri", "extensions", "hachidori", "vendor");
const to = join(root, "static", "hoshi");
if (!existsSync(join(from, "hoshidicts.wasm"))) {
  console.error("Hachidori isn't here yet: run `npm run prepare:hachidori` first.");
  process.exit(1);
}
mkdirSync(to, { recursive: true });
for (const file of ["hoshidicts.mjs", "hoshidicts.wasm"]) copyFileSync(join(from, file), join(to, file));
console.log(`Hoshidicts copied into ${to}`);
