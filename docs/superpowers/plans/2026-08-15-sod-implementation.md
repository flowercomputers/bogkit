# Sod Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build `sod/`, the symmetric replication crate for fold apps: portable core (frames, version vectors, log, replica, engine port, sans-io sync), file log, websocket transport, fold engine, demo app, and the test suite that enforces SOD-1..9.

**Architecture:** Portable core with four ports (Engine, LogStore, Transport, entropy). Per-origin hash-chained feeds, version-vector anti-entropy, log-first write path. Fold is one engine behind a default-on feature; `MemEngine` is the always-compiled oracle and the wasm-viable engine.

**Tech Stack:** Rust, postcard, blake3, tungstenite (feature `ws`), fold (feature `fold-engine`), getrandom (feature `os-rng`), napi-rs (demo addon only).

**Spec:** `docs/superpowers/specs/2026-08-15-sod-design.md`

## Global Constraints

- Feature flags: `default = ["fold-engine", "ws", "os-rng"]`. `cargo check -p sod --no-default-features` must always pass, including for `--target wasm32-unknown-unknown`.
- No wall-clock reads anywhere in `sod` (SOD-7): `event_time` is a caller-supplied argument. `grep -rn "SystemTime\|Instant::now" sod/src/` must return nothing.
- No HashMap iteration at any output or serialization boundary; use `BTreeMap`/`BTreeSet`.
- All persisted/wire encoding is postcard; frame hash is BLAKE3 of the encoded frame bytes.
- `PROTOCOL_VERSION: u16 = 1`. On-disk record: `u32 LE len | frame_bytes | [u8;32] blake3(frame_bytes)`.
- Every public item gets rustdoc; `sod/README.md` and the workspace `README.md` updated in the same change as the public API (repo standing rule, applied to sod).
- Commit after every green test cycle. Run `cargo test -p sod` per task; full `cargo test --workspace` before claiming done.

---

### Task 1: Crate scaffold, `Frame`, record encoding, golden fixture

**Files:**
- Create: `sod/Cargo.toml`, `sod/src/lib.rs`, `sod/src/frame.rs`, `sod/tests/golden_log.rs`, `sod/tests/fixtures/golden.sodlog`
- Modify: `Cargo.toml` (workspace members — `examples/*` already globs; add `"sod"`)

**Interfaces (produces):**
```rust
pub struct ReplicaId(pub [u8; 16]);           // Ord, Copy, Serialize, Display=hex
impl ReplicaId { pub fn generate() -> Self;   /* feature os-rng */ }
pub struct FrameHash(pub [u8; 32]);           // Ord, Copy, Serialize
pub const ZERO_HASH: FrameHash;
pub struct Frame {
    pub prev_hash: FrameHash,
    pub origin: ReplicaId,
    pub seq: u64,                              // 1-based
    pub event_time: u64,                       // ms epoch, origin-stamped
    pub payload: Vec<(Vec<u8>, i64)>,          // (postcard datum, multiplicity)
}
impl Frame {
    pub fn encode(&self) -> Vec<u8>;           // postcard body
    pub fn hash(&self) -> FrameHash;           // blake3(encode())
    pub fn encode_record(&self, out: &mut Vec<u8>);  // len|body|hash
}
pub fn decode_record(buf: &[u8]) -> Result<Option<(Frame, FrameHash, usize)>, SodError>;
// Ok(None) = clean partial tail (needs more bytes); Err = corrupt (hash mismatch)
pub enum SodError { Corrupt(&'static str), Gap { .. }, Equivocation { .. },
    Poisoned(ReplicaId), VersionMismatch { .. }, Io(String) }
```

- [ ] Step 1: Write `sod/Cargo.toml` (deps: serde+derive, postcard alloc+use-std, blake3; optional: getrandom, fold path dep, tungstenite; features as in Global Constraints) and empty module skeleton; add to workspace members. `cargo check -p sod` passes.
- [ ] Step 2: Failing tests in `frame.rs` `#[cfg(test)]`: `frame_roundtrip` (encode_record → decode_record → equal frame + hash), `decode_partial_tail_is_none` (truncated record → Ok(None)), `decode_flipped_byte_is_corrupt` (flip a body byte → Err(Corrupt)), `hash_chains` (frame2.prev_hash = frame1.hash()).
- [ ] Step 3: Implement; run `cargo test -p sod` to green.
- [ ] Step 4: Golden fixture: a test-generated two-frame log written once to `sod/tests/fixtures/golden.sodlog` (deterministic content: fixed ReplicaId bytes, fixed event times); `golden_log.rs` asserts byte-exact parse forever. Regenerating requires deliberately re-running the ignored generator test — document in the test.
- [ ] Step 5: Commit.

### Task 2: Version vectors

**Files:** Create `sod/src/vector.rs`

**Interfaces (produces):**
```rust
#[derive(Default, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct VersionVector(BTreeMap<ReplicaId, u64>);   // seq held through, contiguous
impl VersionVector {
    pub fn get(&self, id: &ReplicaId) -> u64;          // 0 if absent
    pub fn advance(&mut self, id: ReplicaId, seq: u64); // panics unless seq == get+1
    pub fn set(&mut self, id: ReplicaId, seq: u64);     // for engine cursors
    pub fn iter(&self) -> impl Iterator<Item = (&ReplicaId, &u64)>;
    /// origins+ranges self holds beyond `other`: (origin, other_have, self_have]
    pub fn ahead_of(&self, other: &Self) -> Vec<(ReplicaId, u64, u64)>;
}
```

- [ ] Step 1: Failing tests: `advance_contiguous`, `advance_gap_panics`, `ahead_of_disjoint_and_overlap` (covers: origin unknown to other, partially known, fully known).
- [ ] Step 2: Implement; green; commit.

### Task 3: LogStore port, `MemLog`, `FileLog` with torn-tail recovery

**Files:** Create `sod/src/store.rs` (trait + MemLog), `sod/src/log_file.rs` (FileLog), `sod/tests/log_recovery.rs`

**Interfaces (produces):**
```rust
pub trait LogStore {
    fn append(&mut self, frame: &Frame) -> Result<(), SodError>;
    fn sync(&mut self) -> Result<(), SodError>;
    /// decoded + hash-verified frames, in file order; recovery already done
    fn frames(&self) -> Result<Vec<Frame>, SodError>;
}
pub struct MemLog { .. }         // Vec<Frame>; Default
pub struct FileLog { .. }
impl FileLog { pub fn open(path: &Path) -> Result<(Self, Vec<Frame>), SodError>; }
// open scans; first short/бad record truncates the file there (torn tail);
// a corrupt record *followed by further valid data* is Err(Corrupt) — that is
// not a torn tail, it is corruption, and we refuse rather than drop data.
```
Note: `frames()` on FileLog returns the frames captured at `open` plus appends since — FileLog keeps them in memory (v1 keeps the whole log in memory by design; documented).

- [ ] Step 1: Failing tests in `log_recovery.rs`: `roundtrip_reopen`, `torn_tail_truncated` (append 3, truncate file by k bytes for k in 1..last record len, reopen → 2 frames, file length restored to end of frame 2, appends still work), `mid_file_corruption_refuses` (flip byte in frame 1 of 3 → Err).
- [ ] Step 2: Implement MemLog + FileLog (open with recovery scan, append = encode_record + write, sync = File::sync_data); green; commit.

### Task 4: Engine port and `MemEngine`

**Files:** Create `sod/src/engine.rs`

**Interfaces (produces):**
```rust
pub trait Engine {
    /// Commit deltas + cursor advance for (frame.origin, frame.seq) atomically.
    fn apply(&mut self, frame: &Frame, watermark: u64) -> Result<(), SodError>;
    /// Cursor durably applied through, per origin (read at open).
    fn applied(&self) -> VersionVector;
    /// Seed the watermark at open before any apply (default no-op).
    fn seed_watermark(&mut self, _wm: u64) {}
}
pub struct MemEngine { multiset: BTreeMap<Vec<u8>, i64>, applied: VersionVector, watermark: u64 }
impl MemEngine {
    pub fn new() -> Self;
    pub fn view_bytes(&self) -> Vec<u8>;   // postcard of multiset — the convergence probe
    pub fn count(&self, datum: &[u8]) -> i64;
    pub fn watermark(&self) -> u64;
}
```
MemEngine removes zero entries (so transient negatives are representable and visible, per spec).

- [ ] Step 1: Failing tests: `apply_advances_cursor`, `retraction_before_insert_goes_negative_then_zero_entry_removed`, `view_bytes_order_independent` (two MemEngines, same frames in different origin-interleavings → equal view_bytes).
- [ ] Step 2: Implement; green; commit.

### Task 5: `Replica` — open/replay, commit, ingest, poisoning

**Files:** Create `sod/src/replica.rs`; Modify `sod/src/lib.rs` (exports)

**Interfaces (produces):**
```rust
pub struct Replica<E: Engine, L: LogStore> { .. }
impl<E: Engine, L: LogStore> Replica<E, L> {
    /// Rebuild feeds/vector/watermark from log, verify chains, then replay
    /// into the engine every frame beyond engine.applied() (SOD-1, SOD-5).
    pub fn open(id: ReplicaId, log: L, engine: E) -> Result<Self, SodError>;
    /// Local write: build frame (chain to local head, seq = local+1), append
    /// log, sync, engine.apply. Returns the frame's hash.
    pub fn commit(&mut self, payload: Vec<(Vec<u8>, i64)>, event_time: u64)
        -> Result<FrameHash, SodError>;
    /// Remote frame from sync. Dedup: seq <= have and same hash → Ok(false).
    /// seq <= have, different hash → poison origin, Err(Equivocation).
    /// seq > have+1 → Err(Gap). Poisoned origin → Err(Poisoned).
    /// Else verify prev_hash == feed head (else poison), append, apply.
    pub fn ingest(&mut self, frame: Frame) -> Result<bool, SodError>;
    pub fn id(&self) -> ReplicaId;
    pub fn vector(&self) -> &VersionVector;
    pub fn watermark(&self) -> u64;                 // max event_time applied
    pub fn frames_after(&self, origin: ReplicaId, after: u64) -> &[Frame];
    pub fn poisoned(&self) -> impl Iterator<Item = &ReplicaId>;
    pub fn engine(&self) -> &E;  pub fn engine_mut(&mut self) -> &mut E;
}
```
Internal: `feeds: BTreeMap<ReplicaId, Vec<Frame>>` (in-memory copy of the log per origin, chain-verified at open), `poisoned: BTreeSet<ReplicaId>`.

- [ ] Step 1: Failing unit tests (MemEngine + MemLog): `commit_chains_and_applies`, `reopen_replays_only_unapplied` (engine pre-seeded with partial cursor → only suffix re-applied — the SOD-5 crash-heal path), `ingest_dedups`, `ingest_gap_rejected`, `equivocation_poisons` (second frame, same (origin,seq), different payload → Err + subsequent ingest for that origin → Err(Poisoned), other origins unaffected), `watermark_is_max_event_time` (out-of-order event times).
- [ ] Step 2: Implement; green; commit.
- [ ] Step 3: Crash-healing test with FileLog in `sod/tests/log_recovery.rs`: commit twice, simulate crash-between-append-and-apply by reopening with a *fresh* MemEngine (cursor empty) → replay restores counts; then simulate torn tail on a third commit → reopen heals to two frames.
- [ ] Step 4: Green; commit.

### Task 6: Sans-io sync session + convergence property tests

**Files:** Create `sod/src/sync.rs`, `sod/tests/convergence.rs`

**Interfaces (produces):**
```rust
pub const PROTOCOL_VERSION: u16 = 1;
#[derive(Serialize, Deserialize)]
pub enum Msg {
    Hello { protocol: u16, schema: u32, vector: VersionVector },
    Frames(Vec<Frame>),
    Done,
}
pub struct Session { .. }
impl Session {
    pub fn new(schema: u32) -> Self;
    pub fn hello<E: Engine, L: LogStore>(&self, r: &Replica<E, L>) -> Msg;
    /// Feed one inbound message; returns outbound messages (empty ok).
    /// Hello → version check (SOD-9) then batched Frames (≤256/msg,
    /// per-origin contiguous, skipping locally-poisoned origins) + Done.
    /// Frames → ingest each (Gap/Equivocation from a peer = SyncError;
    /// dedup Ok(false) is fine). Done → mark peer done.
    pub fn on_msg<E: Engine, L: LogStore>(&mut self, r: &mut Replica<E, L>, m: Msg)
        -> Result<Vec<Msg>, SodError>;
    pub fn finished(&self) -> bool;   // we sent Done and received Done
}
/// Drive a full in-memory session between two replicas (test + local sync).
pub fn sync_pair<..>(a: &mut Replica<..>, b: &mut Replica<..>, schema: u32)
    -> Result<(), SodError>;
```

- [ ] Step 1: Failing unit tests: `two_replica_session_converges` (message-pump loop), `version_mismatch_refuses` (schema differs → Err(VersionMismatch), no frames exchanged), `relay_carries_third_party_frames` (A→B, then B→C; C holds A's frames).
- [ ] Step 2: Implement; green; commit.
- [ ] Step 3: Property test `convergence.rs` (no proptest dep — hand-rolled deterministic xorshift PRNG seeded per case, 100 cases): N∈2..=5 replicas over MemEngine/MemLog; 200 steps of random {local commit of random datum/±mult, sync_pair of random pair, interrupted sync (deliver only first k outbound messages, then drop the session)}; after each sync compare: any two replicas with equal vectors must have equal `view_bytes()` and equal `watermark()` (SOD-4, SOD-6, SOD-8). End every case with full pairwise rounds until all vectors equal → assert all views identical.
- [ ] Step 4: Green; commit.

### Task 7: fold additions + `FoldEngine` + differential test

**Files:**
- Modify: `fold/src/stream/mod.rs` (add `Tx::meta`), `fold/src/stream/unkeyed.rs` (add `Stream::meta_keyspace`, `Stream::meta_snapshot`)
- Create: `sod/src/engine_fold.rs`, `sod/src/time.rs`, `sod/tests/fold_engine.rs`

**fold additions (additive, documented, no behavior change):**
```rust
// unkeyed.rs
/// Open (or create) a metadata keyspace `meta_{name}`, outside the
/// pipeline's `sink_*` namespace. For infrastructure layered over Stream
/// (e.g. replication cursors) that must commit atomically with pipeline
/// writes via [`Tx::meta`].
pub fn meta_keyspace(&self, name: &str) -> fjall::SingleWriterTxKeyspace { .. }
/// A read snapshot of committed state, for reading metadata keyspaces.
pub fn meta_snapshot(&self) -> fjall::Snapshot { self.store.read_tx() }
// mod.rs, impl Tx
/// The raw store transaction, for writing metadata keyspaces atomically
/// with this transaction's pipeline pushes.
pub fn meta(&mut self) -> &mut WriteTx<'tx> { self.tx }
```

**Interfaces (produces):**
```rust
pub struct Watermark(Arc<AtomicU64>);
impl Watermark { pub fn new() -> Self; pub fn get(&self) -> u64;
                 pub fn clock(&self) -> impl Fn() -> u64 + Clone; }
pub struct FoldEngine<D, P: Push<D>> { stream: fold::stream::Stream<D, P>,
    cursor_ks: .., applied: VersionVector, watermark: Watermark }
impl<D: Clone + DeserializeOwned, P: Push<D>> FoldEngine<D, P> {
    /// Opens the Stream at `path`, loads the cursor from meta keyspace "sod_cursor".
    pub fn open(path: impl AsRef<Path>, pipeline: P, watermark: Watermark) -> Self;
    pub fn stream(&self) -> &fold::stream::Stream<D, P>;   // for rtx reads
}
impl<..> Engine for FoldEngine<D, P> {
    // apply: watermark.store(max(cur, wm)); stream.wtx(|tx| { decode+push each
    // delta; tx.meta().insert(&cursor_ks, origin bytes, seq BE bytes) });
    // then applied.set(origin, seq). Cursor write is INSIDE the wtx → atomic.
}
```
Cursor layout: key = 16-byte origin, value = 8-byte BE seq. Loaded at open by iterating the keyspace from `meta_snapshot()`.

- [ ] Step 1: fold additions + rustdoc; `cargo test -p fold` still green; commit (separate commit: `feat(fold): metadata keyspaces for infrastructure layered over Stream`).
- [ ] Step 2: Failing tests in `fold_engine.rs` (feature fold-engine): `fold_replica_counts` (Replica<FoldEngine<String, Bag>, FileLog>: commit inserts/retracts, read Bag through `engine().stream().rtx`), `crash_between_log_and_apply_heals` (commit via replica; then append a frame directly to the FileLog *without* applying — simulating the crash window; reopen replica with reopened FoldEngine → cursor causes exactly the orphan frame to replay; counts correct), `differential_vs_mem` (same random frame sequence into FoldEngine(Bag) and MemEngine → Bag contents == MemEngine multiset).
- [ ] Step 3: Implement `time.rs` + `engine_fold.rs`; green; commit.
- [ ] Step 4: Update spec Time section: record the Retain divergence analysis (arrival-order stamping makes processing-time windows non-convergent under any injected clock; event-time retain in fold is the fix, future work; sod v1 excludes time-windowed operators from SOD-4 and the watermark is maintained for app reads and that future operator). Also scope SOD-4 wording to "views observed through sink readers" (Retain-style internal keyspaces may differ in tie-break bytes). Commit spec edit.

### Task 8: Websocket transport

**Files:** Create `sod/src/transport/mod.rs`, `sod/src/transport/ws.rs`, `sod/tests/ws_sync.rs`

**Interfaces (produces):**
```rust
/// Wire = binary websocket messages, each one postcard-encoded `Msg`.
/// Ordering (deadlock-free over blocking sockets):
///   initiator: send Hello → recv Hello → send our Frames+Done → recv theirs
///   responder: recv Hello → send Hello → recv Frames+Done → send ours
pub fn sync_with<E, L>(url: &str, r: &mut Replica<E, L>, schema: u32) -> Result<(), SodError>;
/// Blocking accept loop, one session at a time (v1; documented).
/// Returns after `max_sessions` if Some (tests), else loops forever.
pub fn serve<E, L>(addr: &str, r: &mut Replica<E, L>, schema: u32,
                   max_sessions: Option<usize>) -> Result<(), SodError>;
```

- [ ] Step 1: Failing test `ws_sync.rs::two_processes_converge`: thread A serves `127.0.0.1:0`-style fixed test port with `max_sessions=Some(1)` on a replica with data; main thread `sync_with`; both sides converge (compare view_bytes) — MemEngine so the test runs with `--no-default-features --features ws`.
- [ ] Step 2: Implement with tungstenite (`accept`/`connect`); green; commit.

### Task 9: Portability gate

**Files:** Create `sod/tests/wasm_check.rs`

- [ ] Step 1: Test `core_builds_for_wasm32` (`#[ignore]`-free, std process spawn): run `cargo check -p sod --no-default-features --target wasm32-unknown-unknown`; if the target isn't installed (probe `rustc --print target-libdir --target wasm32-unknown-unknown` failure / check output contains "may not be installed"), print a skip notice and pass. Also assert `cargo check -p sod --no-default-features` (host) succeeds.
- [ ] Step 2: Fix whatever it flushes out (feature-gate leaks: fold/tungstenite/getrandom imports must all sit behind their cfg-features). Green; commit.

### Task 10: `examples/sod-demo` (native binary)

**Files:** Create `examples/sod-demo/Cargo.toml`, `examples/sod-demo/src/main.rs`, `examples/sod-demo/README.md`

App: datum `String`, pipeline `(terminal::Bag::<String>::new("notes"), terminal::Count::new("count"))`, `FoldEngine` + `FileLog` in `<dir>` (log at `<dir>/sod.log`, fold db at `<dir>/db`, replica id persisted at `<dir>/replica_id` — created with `ReplicaId::generate()` on first run, never regenerated while the log exists, deleted with it: SOD-3). `SCHEMA: u32 = 1`. Event time from `SystemTime` **in the demo binary** (apps stamp; sod never does).

Commands: `sod-demo <dir> add <text...>` | `remove <text...>` | `list` (bag + count) | `serve <addr>` | `sync <url>`.

- [ ] Step 1: Scaffold crate (deps: sod, fold; picked up by the `examples/*` workspace glob), implement, plus its README (usage transcript showing two dirs converging).
- [ ] Step 2: Manual verification run: `add` twice in dir A, once in dir B, `serve`+`sync`, `list` both → identical output. Paste transcript into README. Commit.

### Task 11: Node packaging (napi-rs addon for the demo app)

**Files:** Create `examples/sod-demo/node/Cargo.toml` (crate `sod-demo-node`, `crate-type = ["cdylib"]`, deps napi/napi-derive), `examples/sod-demo/node/src/lib.rs`, `examples/sod-demo/node/demo.mjs`, extend `examples/sod-demo/README.md`

Exposed JS surface (thin, app-specific — the template pattern): `open(dir)`, `add(text)`, `remove(text)`, `list(): string[]`, `count(): number`, `syncWith(url)`, `serveOnce(addr)`, `close()`. Implementation holds `Replica<FoldEngine<..>, FileLog>` behind a `Mutex<Option<..>>` (fold Stream is !Send-safe here: use `napi` sync functions, single-threaded access documented).
Build/run: `cargo build -p sod-demo-node`, copy `target/debug/libsod_demo_node.{dylib,so}` → `sod_demo_node.node`, `node demo.mjs`.

- [ ] Step 1: Implement addon + `demo.mjs` (opens two dirs, adds notes, syncs via in-process serve on a thread? No — two Node processes in the README transcript; demo.mjs does add/list/sync against a `sod-demo serve` peer).
- [ ] Step 2: Verify `node demo.mjs` end-to-end against the native binary serving. Paste transcript into README. Commit.
- [ ] Fallback: if napi cannot build in this environment, drop the crate from the workspace, keep the directory with README documenting the pattern, and record the blocker in the PR description. Do not fake the transcript.

### Task 12: Docs + workspace integration

**Files:** Create `sod/README.md`; Modify `README.md` (add sod to "In this workspace"), `sod/src/lib.rs` (crate-level rustdoc: model, invariants table, port map, target matrix)

- [ ] Step 1: Write `sod/README.md`: what sod is, the convergence argument, port map, feature flags, target matrix, how to run the demo + tests, link to spec.
- [ ] Step 2: `cargo doc -p sod --no-deps` builds without warnings. `cargo test --workspace` (full features) green. Commit.

---

## Self-review notes

- Spec coverage: SOD-1 (T5 open/replay), SOD-2 (T1 hashing, T5 poisoning), SOD-3 (T1 generate + T10 id lifecycle), SOD-4 (T6 property + T7 differential), SOD-5 (T3+T5 crash tests), SOD-6 (T6 interrupted syncs), SOD-7 (grep gate + event_time-as-argument), SOD-8 (T4/T6 order independence), SOD-9 (T6 version refusal). Engine port T4/T7; sans-io sync T6; ws T8; wasm gate T9; Node packaging T11; watermark T7; spec Time correction T7.4.
- Known deliberate deviation from spec text: `Replica::commit` takes `event_time` as an argument (stronger than "sod's only wall-clock read"); spec updated in T7.4 alongside the Retain analysis.
