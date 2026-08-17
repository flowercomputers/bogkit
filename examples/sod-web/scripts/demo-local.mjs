// Three-instance local rehearsal of the wifi-kill demo, fully scripted:
//   hub  :3002  serve-only (stands in for the Fly deployment)
//   a    :3000  serves :7300, dials b + hub
//   b    :3001  serves :7301, dials a + hub
// Drives reactions over HTTP, asserts convergence, then partitions the hub
// (pause its peer entries on a and b), asserts a↔b still converge while
// the hub lags, heals the partition, and asserts global convergence.
//
// Prereqs: npm run build:addon && npm run build. Run: npm run demo:local
import { spawn } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const appDir = join(here, "..");
const children = [];
const tmpDirs = [];

// cleanup must run on EVERY exit path — a mid-act crash must not leak
// three servers holding the demo ports
function cleanup() {
  for (const c of children) c.kill();
  for (const d of tmpDirs) rmSync(d, { recursive: true, force: true });
}
process.on("exit", cleanup);

function fail(msg) {
  throw new Error(msg);
}

function boot(port, env) {
  // spawn the standalone server directly — no npm wrapper, so kill()
  // reaches the actual server process and nothing leaks
  const child = spawn(process.execPath, [join(appDir, ".next", "standalone", "server.js")], {
    cwd: appDir,
    env: { ...process.env, PORT: String(port), HOSTNAME: "127.0.0.1", ...env },
    stdio: ["ignore", "ignore", "inherit"],
  });
  children.push(child);
  return child;
}

async function until(desc, timeoutMs, f) {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    try {
      if (await f()) return;
    } catch {
      // not up yet
    }
    if (Date.now() > deadline) fail(`timeout waiting for: ${desc}`);
    await new Promise((r) => setTimeout(r, 500));
  }
}

const get = (port, path) => fetch(`http://127.0.0.1:${port}${path}`).then((r) => r.json());
const post = (port, path, body) =>
  fetch(`http://127.0.0.1:${port}${path}`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body),
  });

const HUB_SYNC = "ws://127.0.0.1:7302";
const A_SYNC = "ws://127.0.0.1:7300";
const B_SYNC = "ws://127.0.0.1:7301";

const dirs = ["hub", "a", "b"].map((n) => mkdtempSync(join(tmpdir(), `sod-web-demo-${n}-`)));
tmpDirs.push(...dirs);

try {
boot(3002, { SOD_DATA_DIR: dirs[0], SOD_SERVE_ADDR: "127.0.0.1:7302" });
boot(3000, {
  SOD_DATA_DIR: dirs[1],
  SOD_SERVE_ADDR: "127.0.0.1:7300",
  SOD_PEERS: `${B_SYNC},${HUB_SYNC}`,
});
boot(3001, {
  SOD_DATA_DIR: dirs[2],
  SOD_SERVE_ADDR: "127.0.0.1:7301",
  SOD_PEERS: `${A_SYNC},${HUB_SYNC}`,
});

const PORTS = [3002, 3000, 3001];
for (const p of PORTS) {
  await until(`:${p} up`, 30_000, async () => (await get(p, "/api/status")).id?.length === 32);
}
console.log("all three instances up");

// act 1: everyone reacts; everyone converges
await post(3002, "/api/react", { emoji: "👍" });
await post(3000, "/api/react", { emoji: "❤️" });
await post(3001, "/api/react", { emoji: "🔥" });
await until("act 1 convergence (total 3 everywhere)", 30_000, async () => {
  const totals = await Promise.all(PORTS.map(async (p) => (await get(p, "/api/board")).total));
  return totals.every((t) => t === 3);
});
await until("locals see 2 connected bogs", 20_000, async () => {
  const [a, b] = await Promise.all([get(3000, "/api/status"), get(3001, "/api/status")]);
  return a.connectedIds.length === 2 && b.connectedIds.length === 2;
});
console.log("act 1 PASS: three bogs converged; locals connected to 2");

// act 2: partition the hub (the deterministic stand-in for killing wifi)
for (const p of [3000, 3001]) {
  await post(p, "/api/peer-toggle", { url: HUB_SYNC, paused: true });
}
await post(3000, "/api/react", { emoji: "🎉" });
await post(3001, "/api/react", { emoji: "🎉" });
await until("a↔b converge to 5 during partition", 30_000, async () => {
  const [a, b] = await Promise.all([get(3000, "/api/board"), get(3001, "/api/board")]);
  return a.total === 5 && b.total === 5;
});
const hubDuring = await get(3002, "/api/board");
if (hubDuring.total !== 3) fail(`hub should lag at 3 during partition, has ${hubDuring.total}`);
await until("locals drop to 1 connected bog", 20_000, async () => {
  const [a, b] = await Promise.all([get(3000, "/api/status"), get(3001, "/api/status")]);
  return a.connectedIds.length === 1 && b.connectedIds.length === 1;
});
console.log("act 2 PASS: partition held — locals kept syncing, hub lagged");

// act 3: heal; hub catches up on both locals' partition-era writes
await post(3002, "/api/react", { emoji: "👀" }); // hub wrote while partitioned too
for (const p of [3000, 3001]) {
  await post(p, "/api/peer-toggle", { url: HUB_SYNC, paused: false });
}
await until("act 3 global convergence (total 6 everywhere)", 30_000, async () => {
  const totals = await Promise.all(PORTS.map(async (p) => (await get(p, "/api/board")).total));
  return totals.every((t) => t === 6);
});
const finals = await Promise.all(PORTS.map((p) => get(p, "/api/board")));
const canon = JSON.stringify(finals[0]);
if (!finals.every((f) => JSON.stringify(f) === canon)) {
  fail(`boards differ after heal: ${finals.map((f) => JSON.stringify(f)).join(" vs ")}`);
}
console.log("act 3 PASS: heal converged all three boards byte-identically");
console.log("DEMO-LOCAL PASS");
process.exit(0); // cleanup runs via the exit handler
} catch (e) {
  console.error(`DEMO-LOCAL FAIL: ${e.message}`);
  process.exit(1);
}
