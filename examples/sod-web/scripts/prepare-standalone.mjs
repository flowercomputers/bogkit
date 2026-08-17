// Makes the standalone output self-serving: Next's standalone server does
// not include static assets by design; copy them in (same thing the
// Dockerfile runtime stage does).
import { cpSync, existsSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const app = join(dirname(fileURLToPath(import.meta.url)), "..");
const standalone = join(app, ".next", "standalone");
if (!existsSync(standalone)) {
  console.error("no standalone output — run next build first");
  process.exit(1);
}
cpSync(join(app, ".next", "static"), join(standalone, ".next", "static"), {
  recursive: true,
});
if (existsSync(join(app, "public"))) {
  cpSync(join(app, "public"), join(standalone, "public"), { recursive: true });
}
console.log("standalone output prepared");
