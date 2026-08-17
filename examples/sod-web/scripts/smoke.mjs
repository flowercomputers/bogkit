// Addon smoke test: single-replica behavior, then a two-process sync leg.
// Run from examples/sod-web: npm run build:addon && npm run smoke
import { spawn } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { createRequire } from "node:module";

const here = dirname(fileURLToPath(import.meta.url));
const require = createRequire(import.meta.url);
const sod = require("../addon/index.js");

function assert(cond, msg) {
  if (!cond) {
    console.error(`FAIL: ${msg}`);
    process.exit(1);
  }
}

const dirA = mkdtempSync(join(tmpdir(), "sod-web-smoke-a-"));
const dirB = mkdtempSync(join(tmpdir(), "sod-web-smoke-b-"));

// --- single replica ---
const idA = sod.open(dirA);
assert(/^[0-9a-f]{32}$/.test(idA), "open returns hex id");
sod.react("👍");
sod.react("👍");
sod.react("❤️");
let b = sod.board();
assert(b.total === 3, `total is 3, got ${b.total}`);
const counts = Object.fromEntries(b.reactions.map((r) => [r.emoji, r.count]));
assert(counts["👍"] === 2 && counts["❤️"] === 1, "per-emoji counts");
sod.unreact("❤️");
assert(sod.board().total === 2, "unreact decrements");
let threw = false;
try {
  sod.unreact("❤️");
} catch {
  threw = true;
}
assert(threw, "guarded unreact throws at zero");
let st = sod.status();
assert(st.id === idA, "status id");
assert(st.connectedIds.length === 0, "no peers yet");
const own = st.vector.find((v) => v.origin === idA);
assert(own && own.seq >= 4, "own feed advanced");

// --- two-process sync ---
const child = spawn(process.execPath, [join(here, "smoke-peer.mjs"), dirB], {
  stdio: ["ignore", "pipe", "inherit"],
});
const ready = await new Promise((resolve, reject) => {
  let buf = "";
  child.stdout.on("data", (d) => {
    buf += d.toString();
    const m = buf.match(/READY (\S+) (\S+)/);
    if (m) resolve({ addr: m[1], id: m[2] });
  });
  child.on("exit", (code) => reject(new Error(`peer died: ${code}`)));
  setTimeout(() => reject(new Error("peer never became ready")), 15_000);
});

const refusals = await sod.syncWithPeer(`ws://${ready.addr}`);
assert(refusals.length === 0, "clean sync has no refusals");
b = sod.board();
assert(b.total === 5, `converged total is 5 (2 local + 3 peer), got ${b.total}`);
st = sod.status();
assert(st.connectedIds.includes(ready.id), "peer counted as connected");
assert(st.heardFrom === 1, `heard from 1 other bog, got ${st.heardFrom}`);

child.kill();
sod.close();
rmSync(dirA, { recursive: true, force: true });
rmSync(dirB, { recursive: true, force: true });
console.log("SMOKE PASS");
