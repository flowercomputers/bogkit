# sod-web Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The three-bog emoji-reaction demo: additive sod transport/wire changes, a per-app napi addon, a Next.js app with sync loop + connected badge, a local three-instance rehearsal, and Fly deploy config.

**Architecture:** Each Next.js instance embeds a sod replica server-side via `sod-web-addon`. Sod gains `SyncListener`/`IncomingSession` (per-session replica locking), `Hello.id`, and `SyncReport`. Peers are a uniform reachability list; the UI's connected badge counts peers with a completed session in a 10 s window.

**Tech Stack:** Rust (sod, napi-rs), Next.js (App Router, standalone output), Fly.io (Docker, raw TCP sync port, volume).

**Spec:** `docs/superpowers/specs/2026-08-16-sod-web-design.md`

## Global Constraints

- Fold is consumed, never modified (`git diff main -- fold` stays empty).
- Sod changes are additive API / pre-release wire-field only; `PROTOCOL_VERSION` stays 1; golden log fixture untouched (record format unchanged).
- Workspace: add `"examples/sod-web/addon"` to `members`, `"examples/sod-web"` to `exclude`. `.gitignore` grows `node_modules/`, `.next/`.
- Addon never blocks the Node event loop on I/O (Promise/thread for sync; serve loop on a Rust thread).
- `SCHEMA: u32 = 1` in the addon. Liveness window 10_000 ms; sync loop tick 3_000 ms.
- Run `cargo test -p sod` per sod task; full `cargo test --workspace` + wasm gate before claiming done.

---

### Task 1: `Hello.id` + `SyncReport`

**Files:** Modify `sod/src/sync.rs`, `sod/src/transport/ws.rs`, `sod/src/lib.rs` (export `SyncReport`), tests in `sod/src/sync.rs`, `sod/tests/ws_sync.rs`, `sod/tests/convergence.rs`.

**Interfaces (produces):**
```rust
pub enum Msg { Hello { protocol: u16, schema: u32, id: ReplicaId, vector: VersionVector }, .. }
pub struct SyncReport { pub peer: ReplicaId, pub peer_vector: VersionVector, pub skipped: Vec<SodError> }
impl Session { pub fn report(self) -> SyncReport; }   // replaces into_skipped; peer fields captured from the peer's Hello
// sync_pair -> Result<(SyncReport, SyncReport), SodError>   (a's report, b's report)
// ws::sync_with -> Result<SyncReport, SodError>
```
`Session` stores `peer: Option<(ReplicaId, VersionVector)>` set in the Hello handler; `report()` panics with "session saw no Hello" if called before one (transports always see one).

- [ ] Step 1: Failing tests: `hello_carries_id` (Session::hello of replica 1 → matches `Msg::Hello { id, .. }` with id 1), `sync_pair_reports_peers` (a↔b: a's report.peer == b.id(), vector snapshot equals b's pre-session vector; b's mirror), update `version_mismatch_refuses` construction.
- [ ] Step 2: Implement; fix all callers (`ws.rs` run_session returns `Ok(session.report())`; convergence tests destructure or ignore reports).
- [ ] Step 3: `cargo test -p sod` green; commit `feat(sod): Hello carries replica id; SyncReport from sessions`.

### Task 2: Transport accept/session split

**Files:** Modify `sod/src/transport/ws.rs`; Test `sod/tests/ws_sync.rs`.

**Interfaces (produces):**
```rust
pub struct SyncListener { listener: TcpListener }
impl SyncListener {
    pub fn bind(addr: &str) -> Result<Self, SodError>;
    pub fn local_addr(&self) -> Result<std::net::SocketAddr, SodError>;
    pub fn accept(&self) -> Result<IncomingSession, SodError>;   // blocks; ws handshake done here
}
pub struct IncomingSession { sock: WebSocket<TcpStream> }
impl IncomingSession {
    pub fn run<E: Engine, L: LogStore>(mut self, r: &mut Replica<E, L>, schema: u32)
        -> Result<SyncReport, SodError>;   // responder role; closes socket on exit
}
```
`serve()` becomes: bind once, loop `{ accept; run; count only Ok }` — behavior identical (stray-probe test must stay green; a failed *accept handshake* returns `Err` from `accept()`, so serve loops on accept errors too, logging them).

- [ ] Step 1: Failing test `listener_idles_without_replica_lock`: bind `SyncListener` on port 47165, hold it idle on a thread that does NOT own the replica; meanwhile commit 100 writes to the replica on the main thread (proving no lock interaction); then dial with `sync_with` from a second replica after passing the replica into the accept thread via channel; assert convergence.
- [ ] Step 2: Implement; rewrite `serve()` over the new API.
- [ ] Step 3: `cargo test -p sod` green (including existing `stray_connection_does_not_consume_one_shot_serve`); commit `feat(sod): SyncListener/IncomingSession — per-session replica locking`.

### Task 3: Workspace scaffold for `examples/sod-web`

**Files:** Modify root `Cargo.toml`, `.gitignore`; Create `examples/sod-web/package.json`, `next.config.mjs`, `tsconfig.json`, `app/layout.tsx`, `app/page.tsx` (placeholder), `examples/sod-web/addon/Cargo.toml`, `addon/build.rs`, `addon/src/lib.rs` (stub: napi `ping()` returning "pong").

Next.js scaffolding is manual (no create-next-app): `package.json` with `next@15`, `react`, `react-dom`, `typescript`, `@types/*`; `output: "standalone"` in next.config. Addon Cargo.toml mirrors `examples/sod-demo/node/` (napi/napi-derive/napi-build, `crate-type = ["cdylib"]`, deps sod/fold/postcard).

- [ ] Step 1: Scaffold files; `members += "examples/sod-web/addon"`, `exclude = ["examples/sod-web"]`.
- [ ] Step 2: Verify: `cargo check -p sod-web-addon` passes; `npm install && npm run build` inside `examples/sod-web` passes; `cargo test --workspace` unaffected.
- [ ] Step 3: Commit `feat(sod-web): workspace + app scaffold`.

### Task 4: The addon

**Files:** Rewrite `examples/sod-web/addon/src/lib.rs`; Create `examples/sod-web/scripts/smoke.mjs`.

**Interfaces (produces — JS surface):**
```
open(dir: string): string                    // replica id hex
close(): void
react(emoji: string): void
unreact(emoji: string): void                 // throws "nothing to unreact" at zero
board(): { reactions: Array<{emoji: string, count: number}>, total: number }
status(): { id: string, vector: Record<string, number>, watermark: number,
            peers: Array<{key: string, lastSyncMs: number|null, lastError: string|null}>,
            connectedIds: string[] }         // distinct peer replica-ids seen ≤10s ago
syncWithPeer(url: string): Promise<string[]> // refusals; worker thread (napi AsyncTask)
startServeLoop(addr: string): string         // bound addr; Rust thread; per-session lock
```
Internals: statics `REPLICA: Mutex<Option<AppReplica>>`, `PEERS_SEEN: Mutex<BTreeMap<ReplicaId, std::time::Instant>>` (updated by both sync directions), `PEER_STATUS: Mutex<BTreeMap<String, PeerStat>>`. Pipeline `(sod::sinks::Bag<String>, Count)`; dir handling copied from sod-demo `open()` (SOD-3 id lifecycle). `connectedIds` computed from `PEERS_SEEN` at call time. Serve loop thread: `SyncListener::bind`, loop `{ accept → lock replica → run → record report.peer }`; on accept error, log and continue.

- [ ] Step 1: Implement addon.
- [ ] Step 2: `smoke.mjs`: build, copy `.node`, then open tmp dir → react ×3 two emoji → board matches → unreact → guarded-unreact throws → status has id + empty connectedIds → close. Two-replica leg: replica B `startServeLoop("127.0.0.1:0")` in process B? (single replica per process → spawn `node smoke-peer.mjs` child process with its own dir + serve; parent `syncWithPeer` to it; assert boards converge and `connectedIds` includes B's id.)
- [ ] Step 3: `node examples/sod-web/scripts/smoke.mjs` passes; commit `feat(sod-web): addon with async sync + serve loop + peer tracking`.

### Task 5: Next.js app — API, sync loop, UI

**Files:** Create `examples/sod-web/lib/sod.ts` (addon loader + init-once from env), `lib/sync-loop.ts`, `app/api/{react,unreact,board,status,peer-toggle}/route.ts`, rewrite `app/page.tsx`, `app/layout.tsx`, `app/ui/*` components (Nav, Board, NerdPanel).

Behavior:
- `lib/sod.ts`: loads `.node`, `open(SOD_DATA_DIR)`, `startServeLoop(SOD_SERVE_ADDR)` if set, starts sync loop with `SOD_PEERS.split(",")` if set; module-level singleton (Next dev double-init guarded by a global).
- `lib/sync-loop.ts`: `setInterval(3000)`; per peer `{url, paused}`; on tick, unpaused peers → `await syncWithPeer(url)` → record ok/err into a status map merged into `/api/status`; `kick()` exported, called after react/unreact.
- `/api/status` merges addon `status()` + loop state: `{...addonStatus, peers: [{url, paused, lastOkMs, lastError}], connected: connectedIds.length, online: anyRemotePeerOkWithin10s}`.
- UI: palette `["👍","❤️","😂","🎉","🚀","👀","🔥","🥲"]`; tiles show count, click → POST react; long-press/right-click → unreact. Nav: color dot + short id, `◉ N bogs connected`, online pill, last-sync age. NerdPanel: per-peer rows with pause toggles, connected-now vs heard-from-ever, raw vector. Poll board+status each 1 s via `useEffect`.

- [ ] Step 1: Implement; `npm run build` clean.
- [ ] Step 2: Manual verify single instance: `SOD_DATA_DIR=/tmp/sw-a npm run dev` — react, reload survives, status shows id.
- [ ] Step 3: Commit `feat(sod-web): reaction board app with sync loop and connected badge`.

### Task 6: Three-instance local rehearsal

**Files:** Create `examples/sod-web/scripts/demo-local.mjs`, `examples/sod-web/README.md` (local section).

`demo-local.mjs`: builds app once, then spawns three `next start` instances (hub :3002 serve-only, :3000, :3001 configured per the spec topology but pointing at the local hub), waits for `/api/status` on each, then drives the checks via HTTP: react on each → poll until all three `/api/board` equal; POST peer-toggle to pause both locals' hub peers (the partition) → react on :3000 → assert :3001 converges but hub doesn't → unpause → assert hub converges; print PASS/FAIL summary and kill children.

- [ ] Step 1: Implement script; run it; iterate until PASS.
- [ ] Step 2: README local-demo section with the exact commands and expected badge behavior.
- [ ] Step 3: Commit `feat(sod-web): three-instance local rehearsal script`.

### Task 7: Fly deploy config + runbook

**Files:** Create `examples/sod-web/Dockerfile`, `examples/sod-web/fly.toml`, `.dockerignore`; extend `examples/sod-web/README.md`.

- Dockerfile: stage 1 `rust:1` — build `sod-web-addon` release (workspace copy, `cargo build -p sod-web-addon --release`); stage 2 `node:22` — `npm ci && npm run build` (standalone); stage 3 `node:22-slim` — standalone output + `.node` artifact + `server.js` entry; `ENV SOD_DATA_DIR=/data/sod`.
- `fly.toml`: app name `sod-web-demo`; `[http_service]` internal_port 3000, force_https, min_machines_running 1, auto_stop disabled; `[[services]]` raw TCP: internal_port 7300, `[[services.ports]]` port 10700 handlers [] (plain TCP first; note the TLS flip); `[mounts]` source `sod_data`, destination `/data`; env `SOD_SERVE_ADDR=0.0.0.0:7300`.
- README: deploy runbook (`fly launch --no-deploy`, `fly volumes create sod_data`, `fly deploy`), local-vs-hub env for the wifi-kill demo, and the stage script (kill wifi → locals keep syncing → reconnect → hub converges) with expected badge states at each step.

- [ ] Step 1: Write config; `docker build` locally if docker available, else validate Dockerfile by review and note in README.
- [ ] Step 2: If `flyctl` is authenticated in this environment, deploy and run the real runbook; otherwise deliver config + runbook and state plainly it hasn't been deployed yet.
- [ ] Step 3: Commit `feat(sod-web): Fly.io deploy config and demo runbook`.

### Task 8: Docs + final gates

**Files:** Modify `sod/README.md` (SyncReport/SyncListener API notes), root `README.md` (sod-web bullet under examples), `examples/sod-web/README.md` (polish).

- [ ] Step 1: Doc updates; `cargo doc -p sod --no-deps` no warnings.
- [ ] Step 2: `cargo test --workspace` + wasm gate green; `node examples/sod-web/scripts/smoke.mjs` and `demo-local.mjs` PASS.
- [ ] Step 3: Commit `docs(sod-web): README + sod API docs`.

## Self-review notes

Spec coverage: transport split (T2), Hello id + SyncReport (T1), addon (T4), app/UI/badge/nerd-panel (T5), uniform peers + partition rehearsal (T6), Fly (T7), docs (T8), scaffold/workspace (T3). Deferred per spec: TLS feature, SSE, hub scale-out. Type consistency: `SyncReport` produced in T1, consumed T2/T4; addon surface of T4 consumed verbatim in T5.
