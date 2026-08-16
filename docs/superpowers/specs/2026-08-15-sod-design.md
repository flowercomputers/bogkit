# Sod: symmetric replication for fold apps

Date: 2026-08-15
Status: draft for review

## What sod is

Sod is a replication layer for fold applications. A **sod** is any replica
embedding it: a small one inside a Node.js process, a big always-on one on a
server — same crate, same protocol, different deployment. Sod turns a
single-process fold `Stream` into an offline-first replica that converges with
its peers, PouchDB-style: local writes always succeed, and replicas exchange
what the other is missing whenever connectivity allows.

Sod lives in this workspace as a crate (`sod/`) with a path dependency on
`fold`. This is deliberate: fold's API is alpha and fast-moving, and sod is
compiled against it, so fold changes break sod at `cargo build` time and get
fixed in-tree — never a lagging external binding chasing a moving API.

**Sod consumes fold; it never modifies it.** Everything sod needs from fold
comes through fold's public API — the `Push` trait, sink keyspaces, the
startup snapshot, transactional writes. Where fold's behavior doesn't fit
replication (see the `Bag` clamp finding below), sod ships its own sink
rather than patching the core. This keeps fold pristine and
upstream-mergeable, and keeps sod a pure consumer with nothing to rebase.

The goal is that both **client↔server patterns and decentralized p2p
patterns** can be enabled against **different bog machinery** — fold today,
other engines tomorrow — with replicas running in browsers, native apps,
React Native, and on servers. The protocol and core never assume a topology,
an engine, or a platform; those are all ports. The first shipped
implementations of the ports are native (std filesystem log, websocket
transport, fold engine), but `sod`'s core compiles for `wasm32-unknown-unknown`
from day one and this is enforced in the test suite.

Sod has nothing to do with clog.

### Why symmetric replication is correct for fold

Fold's write primitive is a Z-set delta: a datum plus a signed multiplicity.
Deltas commute — the multiset state is the sum of applied deltas, and sums are
order-independent. Retraction of a not-yet-seen datum is algebraically fine
(the multiplicity goes negative until the matching insert arrives). Fold views
are deterministic functions of the multiset. Therefore: replicas that hold the
same set of deltas hold the same views, regardless of the order or topology by
which the deltas arrived. Convergence comes from algebra, not coordination.

### Authority is policy, not protocol

The protocol is symmetric-only, permanently. There is no client role and no
server role on the wire. An application that needs order-sensitive operations
(uniqueness constraints, claims, invariant-preserving read-modify-write)
designates one replica as the sequencer *for those operations* and routes such
requests to it as ordinary application RPC, outside the sync protocol. The
sequencer's decisions come back as normal deltas that replicate like anything
else. Nothing in the log or protocol special-cases this, and nothing precludes
it.

## Prior art, and what each contributes

| System | Lesson | Where it lands in sod |
|---|---|---|
| Secure Scuttlebutt | Per-origin append-only feeds, hash-chained, gossiped by version vector | The core model: per-replica logs, chained frames, vector exchange |
| git | Content-addressed identity; integrity is structural, not bolted on | Frame identity is its BLAKE3 hash |
| Blockchains | Hash chains make equivocation detectable | Two frames claiming one `(origin, seq)` = poisoned feed, refused |
| WebTorrent | Piece verification enables trustless relay and swarming | Hash-verified frames can be relayed by any peer; mesh is retrofittable |
| IPFS | Content-address big payloads; separate data from replication metadata | Future work: blob store for large values (e.g. embedding vectors) |
| CouchDB/PouchDB | Resumable idempotent replication; version-metadata handshake; the re-initialized-replica bug; compaction pressure | Vector-based resume; schema-version handshake; replica-id freshness rule; compaction constraints recorded |
| Streaming systems | Time must be data (watermarks), never local wall clock | The watermark clock is the only clock a pipeline may observe |

## Invariants

Each invariant is enforced by a named test.

- **SOD-1 (log is truth).** The sod log is the source of truth. The fold db is
  a rebuildable cache: deleting it and replaying the log yields an equivalent
  replica.
- **SOD-2 (chained identity).** A frame's identity is the BLAKE3 hash of its
  encoded bytes. Every frame carries the hash of its predecessor from the same
  origin (zero hash for `seq == 1`). A frame that fails hash verification, or
  a second distinct frame claiming an already-seen `(origin, seq)`, marks that
  origin's feed as poisoned: sod stops accepting frames for that origin and
  surfaces the error. Already-applied frames are not rolled back.
- **SOD-3 (fresh replica id).** `replica_id` is 128 random bits generated when
  the local log is created, and never outlives the log: deleting or resetting
  the log requires generating a new id. Ids are never reused, configured, or
  derived from hardware.
- **SOD-4 (convergence).** Two replicas running the same schema version, the
  same engine, and the same pipeline, whose version vectors are equal,
  observe byte-identical exact views **through sink readers**. (Internal
  operator state may differ in tie-break bytes — e.g. arrival sequence
  numbers — as long as no reader can observe the difference.) Approximate
  indexes (HNSW) converge on the vector *set*; their query results are
  order-sensitive and may differ until rebuilt from the store (see Known
  deviations).
- **SOD-5 (crash healing).** After a crash at any point — mid log append, or
  between log fsync and fold commit — reopening yields a replica equivalent to
  replaying the durable log prefix. Torn tail frames are truncated; the
  applied-cursor replays exactly the un-applied suffix.
- **SOD-6 (resumable sync).** A sync session killed at any byte leaves both
  replicas correct; the next session resumes from the current version vectors
  with no duplicated application (dedup on `(origin, seq)`).
- **SOD-7 (watermark time).** The only clock observable by a sod-compatible
  pipeline is the watermark: the maximum event-time across frames applied so
  far. No wall-clock reads anywhere in the apply path.
- **SOD-8 (deterministic apply).** Applying the same set of frames yields the
  same view bytes regardless of arrival interleaving across origins. Frames
  from a single origin apply in contiguous seq order.
- **SOD-9 (versioned handshake).** Sync sessions begin by exchanging a schema
  version (app-declared) and a sod protocol version. Any mismatch refuses the
  session with a clear error. No partial or best-effort cross-version sync.

## Architecture

The crate splits into a **portable core** (pure logic, no I/O, no fold, no
std-only dependencies) and **port implementations** behind feature flags.
Everything platform- or engine-specific enters through a port.

```
sod/                          workspace crate
├── frame.rs        core      frame encoding, BLAKE3 hashing, chain verification
├── vector.rs       core      version vectors: compare, diff, merge
├── store.rs        core      LogStore port (append/scan/truncate) + MemLog impl
├── replica.rs      core      Replica<E>: write path, open/recovery, apply, dedup,
│                             poisoning; generic over the Engine port
├── engine.rs       core      Engine port ("the bog machinery") + MemEngine, a
│                             minimal deterministic multiset engine for tests,
│                             wasm builds, and non-fold deployments
├── sync.rs         core      sans-io session state machine: consumes/produces
│                             protocol messages, owns no sockets
├── time.rs         core      watermark clock
├── log_file.rs     [std]     filesystem LogStore with torn-tail recovery
├── transport/ws.rs [ws]      blocking websocket peer + listener
├── engine_fold.rs  [fold]    fold-backed Engine: wraps the app pipeline in
│                             an AppliedCursor node (fold public API only)
└── sinks.rs        [fold]    replication-safe sinks (Bag) for sod pipelines

examples/sod-demo/            two-replica convergence demo over websocket;
                              doubles as the app template
```

### The Engine port

An engine is whatever materializes deltas into readable state:

```rust
pub trait Engine {
    /// Apply one frame's deltas plus the new watermark, atomically,
    /// together with the applied-cursor update for `(origin, seq)`.
    fn apply(&mut self, frame: &Frame, watermark: u64) -> Result<...>;
    /// The cursor the engine has durably applied through, per origin —
    /// read at open to replay exactly the un-applied log suffix.
    fn applied(&self) -> VersionVector;
}
```

`engine::MemEngine` (always compiled) is a deterministic in-memory multiset —
the differential oracle for property tests and the engine available on targets
fold cannot reach yet. `engine_fold::FoldEngine<T>` (feature `fold-engine`,
default on) wraps a fold `Stream` with the app's pipeline; `T: Serialize +
DeserializeOwned` is the same bound fold's sinks already require. Because
engines differ in what views they materialize, cross-replica convergence
claims (SOD-4) apply between replicas running the *same* engine and pipeline.

### Targets

- **Server / native apps / Node.js**: full stack — fold engine, file log,
  websocket transport. Node packaging is a per-app napi-rs addon (below).
- **Browser**: `sod` core + `MemEngine` compile to `wasm32-unknown-unknown`
  today (`cargo check --target wasm32-unknown-unknown --no-default-features`
  is part of the test suite). A persistent browser LogStore (OPFS/IndexedDB)
  and a WebSocket/WebRTC transport are follow-on port implementations, not
  core changes. The fold engine reaches the browser only when fold grows a
  storage port to replace fjall — recorded as fold future work, not sod's.
- **React Native**: native Rust via a UniFFI/JSI binding — same full stack as
  native apps (phones have real filesystems and threads, so fjall works).
  Packaging follow-on; no core changes.
- **Topologies**: client↔server is a star of pairwise symmetric sessions;
  p2p is any other graph of the same sessions. The protocol cannot tell the
  difference — that is the point.

## The log

Each replica keeps a single append-only file holding every frame it knows —
its own and those received from other origins — in arrival order. Ordering is
a per-origin property (the hash chain and contiguous seqs), not a property of
the file. On-disk record:

```
u32 len | frame_bytes | [u8; 32] blake3(frame_bytes)

frame (postcard) = {
  prev_hash:  [u8; 32],      // hash of this origin's previous frame; zero at seq 1
  origin:     [u8; 16],      // replica_id
  seq:        u64,           // 1-based, contiguous per origin
  event_time: u64,           // origin-stamped, milliseconds since epoch
  payload:    Vec<(Vec<u8>, i64)>,   // (postcard-encoded T, multiplicity)
}
```

- **Durability.** `fsync` policy is configurable; the default fsyncs on every
  local commit before fold apply (matching Pouch's durable default). Received
  frames during sync may batch fsyncs.
- **Recovery scan.** On open, the log is scanned; the first record whose
  length is short or whose hash fails verification marks the torn tail, which
  is truncated. Everything before it is trusted (SOD-5).
- **Ordering rule.** Log append (and its fsync, per policy) strictly precedes
  fold apply. The reverse is impossible by construction.

## The write path

Local commit of a batch of deltas:

1. Assign `seq` (local counter + 1), stamp `event_time` from the system clock
   (the only wall-clock read in sod — it produces *data*, it is never
   *observed* by the pipeline).
2. Encode the frame, chain it to the previous local frame, append, fsync per
   policy.
3. Hand the frame to the engine's `apply`, which must commit the deltas and
   the applied-cursor advance for `(origin, seq)` atomically. In the fold
   engine this is one fold write transaction: sod wraps the app pipeline in
   an `AppliedCursor` node — an ordinary `Push` node claiming the sink name
   `sod_cursor` — which persists the cursor at commit, inside the same
   transaction as the deltas (fold public API only, no fold changes). In
   `MemEngine` it is a plain in-memory update.

Remote frames (from sync) follow the same steps 2–3 after chain verification
and dedup. On open, sod compares the log against the applied-cursor and
replays exactly the un-applied suffix, making step 2→3 crashes self-healing
and apply exactly-once (SOD-5, SOD-6).

## Sync protocol

A session between any two peers, over a `Transport` trait (first
implementation: websocket; the transport carries ordered reliable frames and
nothing else).

1. **Handshake.** Exchange `(sod_protocol_version, app_schema_version,
   version_vector)`. Version mismatch → refuse (SOD-9).
2. **Diff.** Each side computes what the other lacks: for every origin, the
   suffix above the peer's vector entry. Relayed origins are included — a peer
   syncs *everything it holds*, not just its own feed (this is what makes
   hub-and-spoke work with a dumb hub, and mesh work later).
3. **Stream.** Both directions concurrently, per-origin in contiguous seq
   order, in bounded batches. Receiver verifies chain + hash per frame,
   appends, applies, advances its vector. Non-contiguous or chain-breaking
   frames are protocol errors.
4. **Completion or interruption.** There is no session-completion state to
   persist: the version vector *is* the resume point (SOD-6). Couch-style
   per-peer checkpoints are unnecessary.

Equivocation discovered mid-session (SOD-2) poisons the offending origin's
feed locally and is reported to the application; the session continues for
other origins.

## Time

`sod::time::Watermark` is the only "now" a sod replica has: the max
`event_time` over all frames applied so far. Max is commutative and
associative over the replicated frame set, so the watermark converges exactly
as the data does (SOD-7). It is exposed for application reads and advances
only when writes arrive.

**Time-windowed operators are excluded from v1 sod compatibility.** The
skeptical finding, recorded so nobody re-attempts the shortcut: fold's
`Retain` is a processing-time window that stamps each record with the clock
value at the transaction that *inserts* it. Under replication, insertion
order differs per replica, so the stamps differ — no injected clock fixes
this:

- clock = watermark (max event-time so far): a record's stamp is the
  watermark *at its arrival*, which is arrival-order-dependent → replicas
  expire it at different horizons → divergence.
- clock = current frame's event-time: a record stamped `t=10` applied
  *after* a frame at `t=15` was already applied never sees a cutoff pass
  above `10` on this replica until the next write, while a replica that
  applied them in the other order already expired it → divergence.

Convergent windowing needs an **event-time retain** in fold: stamp records
with their frame's event time and expire against the watermark — two
different time reads per commit, which `Retain`'s single-clock design cannot
express. That operator is future work in fold (in-tree); until it exists,
sod-compatible pipelines must not use `Retain` or any other wall-clock- or
arrival-order-dependent operator.

## Node.js packaging

Per-app compiled addon. An app is a small Rust crate that:

1. defines its datum type `T` and its fold pipeline,
2. wraps them in `sod::Replica`,
3. exposes a thin napi-rs surface: `commit(deltas)`, typed view read methods,
   `sync(peer_url)` / `serve(addr)`, `open`/`close`.

The JS surface is app-specific and small; all fold-facing code is Rust,
compiled in-tree. The workspace example is the copyable template. Offline
behavior needs no special mode: writes land in the local log unconditionally,
and sync catches up when a peer is reachable.

## Testing

- **Convergence property tests** (the heart): N in-memory replicas, random
  interleaved writes, random pairwise syncs, partitions, and session kills →
  whenever two replicas' vectors are equal, their exact-view bytes are equal
  (SOD-4, SOD-6, SOD-8). Run against `MemEngine` and the fold engine, with
  `MemEngine` doubling as the differential oracle for fold-engine multiset
  state. Sink coverage across fold's terminals; any order-sensitivity found
  in a fold sink is a fold bug, filed and fixed in-tree.
- **Portability gate.** `cargo check --target wasm32-unknown-unknown
  --no-default-features` for the `sod` crate must pass (skipped with a notice
  if the target isn't installed).
- **Crash tests.** Kill between every pair of write-path steps (torn append,
  post-append pre-apply, mid-apply), reopen, assert equivalence with clean
  replay (SOD-5).
- **Adversarial frames.** Corrupted bytes, broken chains, equivocating
  origins, seq gaps, version mismatches → correct refusal, poisoning, and
  reporting (SOD-2, SOD-9).
- **Golden log format test.** A checked-in log fixture must parse
  byte-identically forever; format changes require a deliberate fixture and
  version bump.
- **Watermark determinism.** Retain-bearing pipeline under shuffled delivery
  orders → identical views (SOD-7).

## Known deviations and consequences

- **HNSW is order-sensitive.** Graph construction depends on insertion order,
  so approximate search results may differ across replicas holding identical
  vector sets; they re-align after a rebuild from the store (which iterates in
  key order). SOD-4 therefore covers exact views only. Full determinism for
  ANN would require canonical-order rebuilds and is future work.
- **Negative multiplicities are visible.** A retraction arriving before its
  insert leaves a transient negative count. This is correct Z-set behavior;
  apps that surface raw counts should expect it.
- **Sinks must not clamp.** The differential oracle caught fold's `Bag`
  dropping negative running sums, making its state arrival-order-dependent
  ('-1 then +2' and '+2 then -1' converge differently). Per the
  fold-unmodified policy, sod ships `sod::sinks::Bag` with
  order-independent semantics (nonzero sums persist; readers surface
  positives) instead of patching fold; the finding stands as an upstream
  report. Every sink in a sod pipeline must be a pure function of the net
  multiset — use `sod::sinks` or audited fold sinks (`Count` is a plain
  commutative sum) — and the differential test is the enforcement.

## Future work (recorded now, built later)

- **Compaction.** Logs grow without bound. Snapshot-plus-truncate is only
  safe for a *closed* peer set whose vectors all cover the truncated prefix —
  the design constraint is recorded so nothing in v1 assumes infinite
  retention is acceptable, but v1 does not compact.
- **Blob store.** Large payload values (embedding vectors, media) should be
  content-addressed and deduplicated out of frames, IPFS-style.
- **Signatures.** Per-origin signing keys (SSB-style) upgrade hash chains
  from tamper-evidence to authorship proof, enabling sync among mutually
  untrusting peers. The chain format is already compatible.
- **Swarming.** Hash-verified frames + relay already permit mesh topologies;
  a gossip/peer-discovery layer would exploit them.
- **Browser persistence and transports.** An OPFS/IndexedDB LogStore and a
  browser WebSocket/WebRTC transport, implementing the existing ports. The
  core already compiles for wasm32; these are additive.
- **React Native packaging.** A UniFFI/JSI binding of the same native stack.
- **Fold on wasm.** Requires fold to grow a storage port replacing fjall;
  tracked as fold future work. Until then, browser replicas run `MemEngine`.

## Non-goals

- Any asymmetric or authoritative wire protocol.
- Cross-version sync or migration (refuse, don't translate — v1).
- Multi-writer concurrency within one replica (fold is single-writer; so is
  sod).
- Anything to do with clog.
