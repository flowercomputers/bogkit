// The background sync loop: every 3 s, one session per unpaused peer.
// "Offline" is nothing more than these attempts failing.
import type { Addon } from "./sod";

const TICK_MS = 3_000;
export const LIVENESS_MS = 10_000;

export type PeerState = {
  url: string;
  paused: boolean;
  lastOkMs: number | null;
  lastError: string | null;
};

export class SyncLoop {
  private addon: Addon;
  readonly peers: PeerState[];
  private timer: ReturnType<typeof setInterval> | null = null;
  private inFlight = false;

  constructor(addon: Addon, urls: string[]) {
    this.addon = addon;
    this.peers = urls.map((url) => ({
      url,
      paused: false,
      lastOkMs: null,
      lastError: null,
    }));
  }

  start() {
    if (this.timer) return;
    this.timer = setInterval(() => void this.tick(), TICK_MS);
    void this.tick();
  }

  /** Immediate pass — called after local writes so propagation feels instant. */
  kick() {
    void this.tick();
  }

  async tick() {
    if (this.inFlight) return;
    this.inFlight = true;
    try {
      for (const peer of this.peers) {
        if (peer.paused) continue;
        try {
          const refusals = await this.addon.syncWithPeer(peer.url);
          peer.lastOkMs = Date.now();
          peer.lastError = null;
          for (const r of refusals) {
            console.warn(`sod-web: refused during sync with ${peer.url}: ${r}`);
          }
        } catch (e) {
          peer.lastError = e instanceof Error ? e.message : String(e);
        }
      }
    } finally {
      this.inFlight = false;
    }
  }

  setPaused(url: string, paused: boolean): boolean {
    const peer = this.peers.find((p) => p.url === url);
    if (!peer) return false;
    peer.paused = paused;
    return true;
  }

  /** Reachable = an unpaused peer succeeded within the liveness window. */
  online(): boolean {
    const now = Date.now();
    return this.peers.some(
      (p) => !p.paused && p.lastOkMs !== null && now - p.lastOkMs <= LIVENESS_MS,
    );
  }
}
