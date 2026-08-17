// Child process for smoke.mjs: a second replica that serves sync sessions.
// Usage: node smoke-peer.mjs <data-dir>   — prints "READY <addr> <id>".
import { createRequire } from "node:module";
const require = createRequire(import.meta.url);
const sod = require("../addon/index.js");

const dir = process.argv[2];
const id = sod.open(dir);
sod.react("🔥");
sod.react("🔥");
sod.react("👀");
const addr = sod.startServeLoop("127.0.0.1:0");
console.log(`READY ${addr} ${id}`);
// keep serving until the parent kills us
setInterval(() => {}, 1_000);
