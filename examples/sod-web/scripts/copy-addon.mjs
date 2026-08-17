// Copies the built cdylib into the addon package as sod_web_addon.node.
import { copyFileSync, existsSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const workspaceTarget = join(here, "..", "..", "..", "target", "release");
const candidates = [
  "libsod_web_addon.dylib",
  "libsod_web_addon.so",
  "sod_web_addon.dll",
];
const src = candidates
  .map((name) => join(workspaceTarget, name))
  .find(existsSync);
if (!src) {
  console.error(`no built addon found in ${workspaceTarget} — run: cargo build -p sod-web-addon --release`);
  process.exit(1);
}
const dest = join(here, "..", "addon", "sod_web_addon.node");
copyFileSync(src, dest);
console.log(`copied ${src} -> ${dest}`);
