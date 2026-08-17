// The app's replica: addon loading, env-driven init, module singleton.
//
// Config (uniform across roles — dial-vs-serve is reachability, not role):
//   SOD_DATA_DIR    replica directory        (default: .sod-data)
//   SOD_SERVE_ADDR  start the accept loop    (optional)
//   SOD_PEERS       comma-separated ws URLs  (optional; everyone reachable)
import { createRequire } from "node:module";
import { SyncLoop } from "./sync-loop";

export type Addon = {
  open(dir: string): string;
  close(): void;
  react(emoji: string): void;
  unreact(emoji: string): void;
  board(): { reactions: Array<{ emoji: string; count: number }>; total: number };
  status(): {
    id: string;
    vector: Record<string, number>;
    watermark: number;
    connectedIds: string[];
    heardFrom: number;
  };
  syncWithPeer(url: string): Promise<string[]>;
  startServeLoop(addr: string): string;
};

export type Sod = {
  addon: Addon;
  id: string;
  serveAddr: string | null;
  loop: SyncLoop;
};

declare global {
  // survives Next dev hot reloads; one replica per process
  var __sod: Sod | undefined;
}

function init(): Sod {
  const require = createRequire(import.meta.url);
  const addon = require("sod-web-addon") as Addon;

  const dir = process.env.SOD_DATA_DIR ?? ".sod-data";
  const id = addon.open(dir);

  let serveAddr: string | null = null;
  if (process.env.SOD_SERVE_ADDR) {
    serveAddr = addon.startServeLoop(process.env.SOD_SERVE_ADDR);
    console.log(`sod-web: replica ${id} serving sync on ${serveAddr}`);
  }

  const peers = (process.env.SOD_PEERS ?? "")
    .split(",")
    .map((s) => s.trim())
    .filter(Boolean);
  const loop = new SyncLoop(addon, peers);
  loop.start();

  console.log(
    `sod-web: replica ${id} (dir ${dir}, peers: ${peers.length ? peers.join(", ") : "none"})`,
  );
  return { addon, id, serveAddr, loop };
}

export function getSod(): Sod {
  if (!globalThis.__sod) {
    globalThis.__sod = init();
  }
  return globalThis.__sod;
}
