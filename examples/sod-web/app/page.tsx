"use client";

import { useCallback, useEffect, useState } from "react";

const PALETTE = ["👍", "❤️", "😂", "🎉", "🚀", "👀", "🔥", "🥲"];

type Board = { reactions: Array<{ emoji: string; count: number }>; total: number };
type Peer = { url: string; paused: boolean; lastOkMs: number | null; lastError: string | null };
type Status = {
  id: string;
  vector: Array<{ origin: string; seq: number }>;
  watermark: number;
  connectedIds: string[];
  heardFrom: number;
  serveAddr: string | null;
  peers: Peer[];
  online: boolean;
  hasPeers: boolean;
};

/** Deterministic hue from a replica id — the same id gets the same color
 *  in every window, so a peer's dot here matches its own page's tint. */
function hueOf(id: string): number {
  return parseInt(id.slice(0, 4), 16) % 360;
}

function isLocalUrl(url: string): boolean {
  return url.includes("127.0.0.1") || url.includes("localhost");
}

export default function Page() {
  const [board, setBoard] = useState<Board | null>(null);
  const [status, setStatus] = useState<Status | null>(null);

  const refresh = useCallback(async () => {
    try {
      const [b, s] = await Promise.all([
        fetch("/api/board").then((r) => r.json()),
        fetch("/api/status").then((r) => r.json()),
      ]);
      setBoard(b);
      setStatus(s);
    } catch {
      // server restarting; next poll will catch up
    }
  }, []);

  useEffect(() => {
    void refresh();
    const t = setInterval(() => void refresh(), 1000);
    return () => clearInterval(t);
  }, [refresh]);

  useEffect(() => {
    if (status) {
      document.documentElement.style.setProperty("--hue", String(hueOf(status.id)));
    }
  }, [status]);

  const counts = new Map(board?.reactions.map((r) => [r.emoji, r.count]) ?? []);

  const act = async (path: string, body: unknown) => {
    await fetch(path, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(body),
    });
    void refresh();
  };

  const remotePeers = status?.peers.filter((p) => !isLocalUrl(p.url)) ?? [];
  const wifiKilled = remotePeers.length > 0 && remotePeers.every((p) => p.paused);

  return (
    <>
      <nav className="nav">
        {status && (
          <>
            <span className="self">
              <span className="dot" style={{ background: `hsl(${hueOf(status.id)} 52% 40%)` }} />
              sod {status.id.slice(0, 8)}
            </span>
            <span className="peer-dots">
              {status.connectedIds.map((peer) => (
                <span
                  key={peer}
                  className="dot"
                  title={`connected: ${peer.slice(0, 8)}`}
                  style={{ background: `hsl(${hueOf(peer)} 52% 40%)` }}
                />
              ))}
              <span className="badge">
                {status.connectedIds.length} bog{status.connectedIds.length === 1 ? "" : "s"} connected
              </span>
            </span>
            {status.hasPeers ? (
              <span className={`pill ${status.online ? "online" : "offline"}`}>
                {status.online ? "online" : "unreachable"}
              </span>
            ) : (
              <span className="pill serving">serving</span>
            )}
            {remotePeers.length > 0 && (
              <button
                className={`offline-btn ${wifiKilled ? "engaged" : ""}`}
                onClick={() => {
                  for (const p of remotePeers) {
                    void act("/api/peer-toggle", { url: p.url, paused: !wifiKilled });
                  }
                }}
              >
                {wifiKilled ? "Resume remote sync" : "Pause remote sync"}
              </button>
            )}
          </>
        )}
      </nav>

      <main>
        <div className="board">
          {PALETTE.map((emoji) => {
            const count = counts.get(emoji) ?? 0;
            return (
              <button
                key={emoji}
                className="tile"
                onClick={() => void act("/api/react", { emoji })}
                onContextMenu={(e) => {
                  e.preventDefault();
                  if (count > 0) void act("/api/unreact", { emoji });
                }}
                title="click to react · right-click to unreact"
              >
                <span className="emoji">{emoji}</span>
                <span className="count" key={`${emoji}-${count}`}>
                  {count}
                </span>
              </button>
            );
          })}
        </div>
        {board && <p className="total">{board.total} reaction{board.total === 1 ? "" : "s"} total</p>}

        {status && (
          <section className="nerd">
            <h2>Under the bog</h2>
            {status.peers.map((peer) => {
              const fresh =
                peer.lastOkMs !== null && Date.now() - peer.lastOkMs <= 10_000 && !peer.paused;
              return (
                <div className="row" key={peer.url}>
                  <span className={fresh ? "state-ok" : "state-bad"}>{fresh ? "●" : "○"}</span>
                  <span className="url">{peer.url}</span>
                  <span className={fresh ? "state-ok" : "state-bad"}>
                    {peer.paused
                      ? "paused"
                      : fresh
                        ? `ok ${Math.round((Date.now() - (peer.lastOkMs ?? 0)) / 1000)}s ago`
                        : "unreachable"}
                  </span>
                  <span className="spacer" />
                  <button
                    onClick={() => void act("/api/peer-toggle", { url: peer.url, paused: !peer.paused })}
                  >
                    {peer.paused ? "Resume" : "Pause"}
                  </button>
                </div>
              );
            })}
            {status.peers.length === 0 && (
              <div className="row">no peers configured — this bog serves; others dial it</div>
            )}
            <div className="facts">
              connected now: {status.connectedIds.length} · heard from ever: {status.heardFrom}
              {status.serveAddr ? ` · serving on ${status.serveAddr}` : ""}
              <br />
              vector:{" "}
              {status.vector
                .map(({ origin, seq }) => `${origin.slice(0, 8)}·${seq}`)
                .join("  ") || "empty"}
            </div>
          </section>
        )}
      </main>
    </>
  );
}
