// Gets the source of Hoshidicts, the dictionary engine inside Hachidori
// (GPL-3.0), into src-tauri/vendor/hoshidicts, where Telly for Android's build
// compiles it into the app (src-tauri/build.rs) for the phone's lookups
// (src-tauri/src/lookups.rs).
//
// The commit is the one the Hachidori pinned in prepare-hachidori.mjs was
// built from (third_party/hoshidicts at its tag), so the phone and the PC read
// a dictionary the same way. To update: change COMMIT with Hachidori's release.
import { execFileSync } from "node:child_process";
import { existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const REPOSITORY = "https://github.com/bee-san/hoshidicts.git";
const COMMIT = "7efb66c63b04fbaa36ee93b8bded4e4394e732e6";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const to = join(root, "src-tauri", "vendor", "hoshidicts");
const stamp = join(to, ".telly-commit");

if (existsSync(stamp) && readFileSync(stamp, "utf8").trim() === COMMIT) {
  console.log(`Hoshidicts ${COMMIT.slice(0, 12)} is already in ${to}`);
  process.exit(0);
}

const git = (...args) => execFileSync("git", args, { cwd: to, stdio: "inherit" });
rmSync(to, { recursive: true, force: true });
mkdirSync(to, { recursive: true });
git("init", "--quiet");
git("remote", "add", "origin", REPOSITORY);
git("fetch", "--quiet", "--depth", "1", "origin", COMMIT);
git("checkout", "--quiet", "FETCH_HEAD");
git("submodule", "update", "--quiet", "--init", "--depth", "1");
writeFileSync(stamp, COMMIT + "\n");
console.log(`Hoshidicts ${COMMIT.slice(0, 12)} is in ${to}`);
