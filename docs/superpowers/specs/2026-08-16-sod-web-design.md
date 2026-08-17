# sod-web: the three-bog convergence demo

Date: 2026-08-16
Status: approved in brainstorming (see conversation); implements on top of
`docs/superpowers/specs/2026-08-15-sod-design.md`

## What this is

A Next.js **emoji reaction board** where every app instance embeds its own
sod replica — a per-app compiled napi addon, fold engine and all — plus the
small additive sod changes required to embed a replica inside a live web
server. The demo topology is three bogs:

- a **remote hub** deployed on Fly.io,
- two **local instances** at `localhost:3000` and `localhost:3001`.

The demo script: react on all three boards; kill the wifi; the two local
bogs keep syncing with each other over loopback (a *partial partition* —
real, not simulated) while the hub goes dark; keep reacting everywhere;
reconnect; the hub absorbs both locals' writes and everything converges.
A top-nav badge on every instance shows how many other bogs it is
currently connected to, so the partition and the heal are visible.

## Design rules carried forward

- Fold is consumed, never modified.
- Sod core protocol semantics are unchanged; every sod change below is
  additive API or a pre-release wire-field addition.
- Symmetry: dial-vs-serve is about *reachability*, never protocol role.
  Every instance may serve and dial; `SOD_PEERS` lists "everyone you can
  reach", and empty is valid (serve-only — e.g. a NAT'd-from hub).

## Sod changes

### 1. Transport accept/session split (`sod::transport::ws`)

Today `serve()` holds `&mut Replica` for the entire accept loop, including
idle time — unusable inside a server that also takes writes. Additive API:

```rust
pub struct SyncListener { .. }                    // owns no replica
impl SyncListener {
    pub fn bind(addr: &str) -> Result<Self, SodError>;
    pub fn local_addr(&self) -> Result<SocketAddr, SodError>;
    /// Block until a peer connects and completes the ws handshake.
    pub fn accept(&self) -> Result<IncomingSession, SodError>;
}
pub struct IncomingSession { .. }                 // handshaken socket
impl IncomingSession {
    /// Run the whole session; the replica is borrowed only for this call
    /// (sessions are milliseconds).
    pub fn run<E: Engine, L: LogStore>(
        self, r: &mut Replica<E, L>, schema: u32,
    ) -> Result<SyncReport, SodError>;
}
```

`serve()` is reimplemented as a thin bind/accept/run loop; its behavior
(one at a time, count only completed sessions, log refusals) is unchanged.
Host apps accept on a dedicated thread and lock the replica per session.

### 2. `Hello` carries the sender's replica id

`Msg::Hello` gains `id: ReplicaId`. Without it a receiver knows what
frames a peer lacks but not who the peer *is*, and the connected-badge
needs peer identity. Pre-release wire change: nothing has shipped, and
the protocol/schema handshake already refuses mismatched builds, so
`PROTOCOL_VERSION` stays 1.

### 3. `SyncReport` replaces bare refusal lists

```rust
pub struct SyncReport {
    pub peer: ReplicaId,
    pub peer_vector: VersionVector,   // as of the peer's Hello
    pub skipped: Vec<SodError>,       // ≤ 1 per origin per session
}
```

Returned by `sync_with`, `IncomingSession::run`, and `sync_pair` (tests
updated). Strictly more information; refusal semantics unchanged.

Deferred, recorded: tungstenite TLS client feature (`wss://`) — not
needed while the Fly sync port uses a plain TCP handler; flipped on
(one Cargo feature + Fly handler change) before any public showing.

## The app (`examples/sod-web/`)

Layout — the JS app owns the directory root; the addon nests inside:

```
examples/sod-web/         Next.js app (App Router, package.json here)
├── addon/                Rust crate `sod-web-addon` (napi cdylib)
├── Dockerfile            addon build → Next standalone → slim runtime
└── fly.toml
```

Workspace mechanics: add `examples/sod-web/addon` to `members`,
`examples/sod-web` to `exclude` (the `examples/*` glob requires a
Cargo.toml at each match). `.gitignore` grows `node_modules/`, `.next/`.

### Addon (`sod-web-addon`)

Datum `String` (emoji slug); pipeline `(sod::sinks::Bag<String>,
fold Count)`; `SCHEMA = 1`. Replica dir handling identical to sod-demo
(fresh `replica_id` whenever the log is created; id dies with the log).
Surface (async where it does I/O — never blocks the event loop):

- `open(dir) -> id` / `close()`
- `react(emoji)` — commit +1; `unreact(emoji)` — guarded −1 (refuses when
  the board shows zero, same hidden-debt guard as sod-demo)
- `board() -> { reactions: [{emoji, count}], total }`
- `status() -> { id, vector: {originHex: seq}, watermark,
  peers: [{key, lastSyncMs, lastError?}], connected }` — `connected` =
  peers (dialed *or* accepted) with a completed session in the last 10 s
- `syncWithPeer(url) -> refusals[]` — Promise, worker thread
- `startServeLoop(addr) -> boundAddr` — Rust thread owning a
  `SyncListener`; locks the replica per session; updates peer last-seen
  from each `SyncReport`

### Next.js app

One page: a fixed palette of 8 emoji tiles with live counts and a total.
Clicking reacts; a small "−" affordance unreacts (guarded). **Top nav**:
replica identity (short hex + stable color dot), **`◉ N bogs connected`**,
online/offline pill, last-sync age. A nerd panel lists: each configured
peer with status and a per-peer pause toggle; *connected now* vs *heard
from ever* (distinct origins in the version vector); the raw vector.

API routes wrap the addon: `/api/react`, `/api/unreact`, `/api/board`,
`/api/status`, `/api/peer-toggle`. The UI polls board+status every 1 s
(polling over SSE deliberately: simplest thing that survives proxies).

Config (uniform across roles):

- `SOD_DATA_DIR` — replica directory (Fly: the mounted volume)
- `SOD_SERVE_ADDR` — optional; start the serve loop
- `SOD_PEERS` — optional; comma-separated ws URLs of everyone reachable

Demo topology:

```
Fly hub:  SOD_SERVE_ADDR=0.0.0.0:<sync>   SOD_PEERS=                (NAT: can reach no one)
:3000     SOD_SERVE_ADDR=127.0.0.1:7300   SOD_PEERS=ws://127.0.0.1:7301,ws://<fly>:<sync>
:3001     SOD_SERVE_ADDR=127.0.0.1:7301   SOD_PEERS=ws://127.0.0.1:7300,ws://<fly>:<sync>
```

### Sync loop

`sync-loop.ts` in the Next server process: every 3 s, for each unpaused
peer, `await syncWithPeer(url)`; success stamps last-seen, failure stamps
last-error (offline *is* just this failing — no special mode). Each local
`react`/`unreact` kicks an immediate pass so propagation feels instant.
The nav "go offline" button pauses all non-localhost peers (deterministic
rehearsal of the wifi kill); the real wifi kill is the stage version, and
the loopback edge genuinely survives it.

## Fly deployment

- Multi-stage Dockerfile: Rust stage builds `sod-web-addon`; Node stage
  builds Next standalone; runtime stage carries the `.node` artifact.
- `fly.toml`: HTTP service (Next, port 3000) behind Fly's edge; a second
  **raw TCP service port** for the sod listener (plain `tcp` handler
  first; `tls` handler + sod `wss` feature before public showing);
  `min_machines_running = 1`; a small volume mounted at `SOD_DATA_DIR`.
- **Exactly one machine.** The replica is single-writer; hub scale-out is
  out of scope (a multi-machine hub would be multiple bogs, which the
  protocol supports but this demo does not exercise).

## Testing

- Sod: accept/session split (listener idles while writes proceed;
  sessions serialize per-replica), Hello-id + SyncReport coverage, all
  existing suites updated for the new return types; wasm gate unaffected
  (transport is feature-gated).
- Addon: Node smoke script — open, react, board, status, guarded unreact.
- Demo rehearsal: `npm run demo:local` boots :3000/:3001 plus a local
  stand-in hub; a script asserts all three boards converge and the
  connected counts move correctly through a simulated partition
  (pause/unpause the hub peers).
- Fly: manual runbook in the README (deploy, wifi kill, reconnect,
  expected badge behavior at each step).

## Non-goals

- Browser-wasm replicas (hub + locals are all server-embedded; the
  browser is UI only).
- Hub scale-out, auth, or TLS-by-default (TLS is a pre-showing flip).
- Any fold change; any sod protocol-semantics change.
- SSE/websocket UI push; free-form emoji; per-user identity.
