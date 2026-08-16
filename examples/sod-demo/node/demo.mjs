// sod from Node.js: offline-first local writes + one sync to converge.
//
// Build & run (from the workspace root):
//   cargo build -p sod-demo-node
//   cp target/debug/libsod_demo_node.dylib examples/sod-demo/node/sod_demo_node.node   # .so on linux
//   node examples/sod-demo/node/demo.mjs <data-dir> <peer-ws-url>
//
// With a native peer serving (terminal 1):
//   cargo run -p sod-demo -- ./b serve 127.0.0.1:7171
import { createRequire } from "node:module";
const require = createRequire(import.meta.url);
const sod = require("./sod_demo_node.node");

const [dir, peer] = process.argv.slice(2);
if (!dir) {
  console.error("usage: node demo.mjs <data-dir> [peer-ws-url]");
  process.exit(2);
}

const id = sod.open(dir);
console.log(`replica ${id}`);

sod.add("note from node");
sod.add("note from node");
sod.add("only node has this");
console.log("local:", sod.list(), `total=${sod.count()}`);

if (peer) {
  const refused = sod.syncWithPeer(peer);
  for (const r of refused) console.warn("refused during sync:", r);
  console.log("after sync:", sod.list(), `total=${sod.count()}`);
}
sod.close();
