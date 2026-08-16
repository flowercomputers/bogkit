# sod

Symmetric replication for fold apps. A **sod** is a replica: a local bog
that always accepts writes and converges with its peers by exchanging what
the other is missing — PouchDB's posture, with a protocol built for fold's
data model.

Spec: [`docs/superpowers/specs/2026-08-15-sod-design.md`](../docs/superpowers/specs/2026-08-15-sod-design.md).
Runnable example + app template: [`examples/sod-demo`](../examples/sod-demo).

## Why it converges

Fold's write primitive is a Z-set delta (datum + signed multiplicity).
Deltas commute: the multiset is the sum of applied deltas, and views are
deterministic functions of the multiset. So replicas that hold the same set
of frames hold the same views, regardless of arrival order or topology —
convergence by algebra, not coordination. Sync is anti-entropy: exchange
version vectors, stream missing frames, done.

There are no client/server roles on the wire. Client↔server is a star of
pairwise symmetric sessions; p2p is any other graph of the same sessions.
Order-sensitive operations (uniqueness, claims) are an *application*
pattern — route them to a designated replica as ordinary RPC — never
protocol machinery.

## The model

- Every replica has a random 128-bit `ReplicaId`, born and dying with its
  log (SOD-3).
- A commit is a **frame**: `(prev_hash, origin, seq, event_time, deltas)`,
  identified by the BLAKE3 hash of its bytes. Each origin's frames form a
  hash chain (à la Secure Scuttlebutt): tamper-evident, equivocation-
  detectable (a fork poisons that feed, SOD-2), and relayable through
  untrusted peers — any peer can carry any feed, and receivers verify.
- The log is the truth (SOD-1); the engine — fold, or anything else — is a
  rebuildable cache with a transactional applied-cursor, so a crash at any
  point heals on open (SOD-5).
- The **watermark** (max event-time applied) is the only "now" (SOD-7);
  sod itself never reads a clock — apps stamp event time at commit.
- Sync sessions start with a protocol + app-schema version handshake and
  refuse mismatches (SOD-9). Interrupted sessions need no cleanup: the
  version vector is the resume point (SOD-6).

## Ports (what makes it portable)

| Port | Purpose | Shipped implementations |
|---|---|---|
| `engine::Engine` | "the bog machinery" materializing deltas | `MemEngine` (always; oracle + wasm-viable), `engine_fold::FoldEngine` (feature `fold-engine`) |
| `store::LogStore` | append-only frame storage | `MemLog` (always), `log_file::FileLog` (torn-tail recovery) |
| transport | drives the sans-io `sync::Session` | `transport::ws` blocking websockets (feature `ws`) |
| entropy | `ReplicaId::generate` | `getrandom` (feature `os-rng`); or pass bytes via `ReplicaId::from_bytes` |

Feature flags: `default = ["fold-engine", "ws", "os-rng"]`. The core —
frames, vectors, log, replica, session, `MemEngine` — has no platform
dependencies: `cargo check -p sod --no-default-features --target
wasm32-unknown-unknown` passes and is enforced by `tests/wasm_check.rs`.

## Targets

- **Server / native / Node.js**: full stack. Node packaging is a per-app
  napi-rs addon (see `examples/sod-demo/node`).
- **Browser**: core + `MemEngine` compile to wasm32 today; an OPFS/
  IndexedDB `LogStore` and a browser transport are follow-on port
  implementations. Fold itself reaches the browser only once it grows a
  storage port replacing fjall.
- **React Native**: the native stack behind a UniFFI/JSI binding
  (follow-on packaging; phones have real filesystems, fjall works).

## Using it

```rust
use sod::sinks::Bag;
use sod::{Replica, ReplicaId};
use sod::{engine_fold::FoldEngine, log_file::FileLog, time::Watermark};

let log = FileLog::open("my.sod/sod.log")?;
let engine = FoldEngine::open("my.sod/db", Bag::<String>::new("notes"), Watermark::new());
let mut replica = Replica::open(my_persisted_id, log, engine)?;

replica.commit(vec![(postcard::to_stdvec(&note)?, 1)], event_time_ms)?;
sod::transport::ws::sync_with("ws://peer:7171", &mut replica, SCHEMA)?;
```

Sod **consumes fold's public API and never modifies fold** — the
applied-cursor is an ordinary pipeline node (sink name `sod_cursor`,
reserved), and where a stock fold sink doesn't fit replication, sod ships
its own in `sod::sinks`.

Rules for a sod-compatible pipeline:

1. Sinks must be pure functions of the net multiset (the differential
   oracle test enforces this; it caught fold's stock `Bag` clamping
   negative sums — use `sod::sinks::Bag` instead; fold's `Count` is safe
   as-is).
2. No wall-clock or arrival-order-dependent operators — in particular
   fold's `Retain` is not yet sod-compatible (see the spec's Time section
   for the analysis; an event-time retain in fold is the fix).
3. Bump your app schema version whenever the datum type or pipeline
   changes shape.

## Tests

`cargo test -p sod` runs, among others: a 100-case randomized convergence
suite (interleaved writes, partial syncs, relays), torn-tail and
crash-window recovery, equivocation adversaries, a golden log-format
fixture, the fold-vs-MemEngine differential oracle, and the wasm32
portability gate.
