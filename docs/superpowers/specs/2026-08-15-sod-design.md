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
- **SOD-4 (convergence).** Two replicas running the same schema version whose
  version vectors are equal have byte-identical exact views. Approximate
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

```
sod/                          workspace crate, path-dep on fold
├── replica.rs                Replica<T>: write path, open/recovery, apply
├── log.rs                    append-only frame log: append, scan, truncate-torn-tail
├── frame.rs                  frame encoding, BLAKE3 hashing, chain verification
├── vector.rs                 version vectors: compare, diff, merge
├── sync.rs                   session state machine over a Transport trait
├── transport/ws.rs           websocket transport (first implementation)
└── time.rs                   watermark clock, wired into fold Retain via with_clock

examples/sod-demo/            two-replica demo; one side packaged as a
                              napi-rs Node addon (doubles as the app template)
```

`Replica<T>` is generic over the app's datum type `T: Serialize +
DeserializeOwned` — the same bound fold's sinks already require. The app
constructs its fold pipeline exactly as today and hands it to
`Replica::open(dir, pipeline)`.

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
3. Open one fold write transaction: push the deltas, and write the
   applied-cursor — `(origin, seq)` per origin, kept in a sod-owned keyspace
   inside fold's store — in the same transaction. Commit.

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

`sod::time::Watermark` implements the clock fold's `Retain` accepts via
`with_clock`. It returns the max `event_time` over all frames applied so far.
Max is commutative and associative over the replicated frame set, so the
watermark converges exactly as the data does — replicas with equal vectors
retain identically (SOD-7, SOD-4). Consequences accepted: a frame from a
long-offline peer may be aged out immediately upon arrival (deterministic on
every replica), and retention advances only when writes arrive. Pipelines that
read any other clock are not sod-compatible; the example demonstrates the
correct wiring.

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
  (SOD-4, SOD-6, SOD-8). Sink coverage across fold's terminals; any
  order-sensitivity found in a fold sink is a fold bug, filed and fixed
  in-tree.
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
- **Browser / React Native targets.** Deliberately out of scope; Node.js
  first. Nothing in the log or protocol is Node-specific.

## Non-goals

- Any asymmetric or authoritative wire protocol.
- Cross-version sync or migration (refuse, don't translate — v1).
- Multi-writer concurrency within one replica (fold is single-writer; so is
  sod).
- Anything to do with clog.
