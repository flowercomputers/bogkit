# sod-demo

A replicated notes bag: the smallest complete [sod](../../sod) app, and the
template for building your own.

Each data directory is one replica (a **sod**):

```
<dir>/
├── sod.log      append-only frame log — the source of truth
├── db/          fold database — rebuildable cache of the log
└── replica_id   16 random bytes, generated with the log, dies with it
```

The pipeline is `(Bag<String>, Count)`; the datum is a `String` note. Bump
`SCHEMA` in `main.rs` whenever you change either — replicas with different
schemas refuse to sync instead of corrupting.

## Commands

```console
$ cargo run -p sod-demo -- <dir> add <text...>     # insert a note
$ cargo run -p sod-demo -- <dir> remove <text...>  # retract a note
$ cargo run -p sod-demo -- <dir> list              # notes + total count
$ cargo run -p sod-demo -- <dir> serve <addr>      # accept sync sessions
$ cargo run -p sod-demo -- <dir> sync <url>        # sync once with a peer
```

## Two replicas converging (verified transcript)

```console
$ sod-demo ./a add hello from a
$ sod-demo ./a add hello from a
$ sod-demo ./a add unique to a
$ sod-demo ./b add greetings from b

$ sod-demo ./a list
1× unique to a
2× hello from a
-- 3 note(s), replica d4df4436dda922715e0a0506a4c69edf

$ sod-demo ./b list
1× greetings from b
-- 1 note(s), replica 49b3315ec43a13239266fa5957b78e48

$ sod-demo ./b serve 127.0.0.1:7171     # terminal 1
$ sod-demo ./a sync ws://127.0.0.1:7171 # terminal 2
synced with ws://127.0.0.1:7171

$ sod-demo ./a list
1× unique to a
2× hello from a
1× greetings from b
-- 4 note(s), replica d4df4436dda922715e0a0506a4c69edf

$ sod-demo ./b list                     # after stopping the server
1× unique to a
2× hello from a
1× greetings from b
-- 4 note(s), replica 49b3315ec43a13239266fa5957b78e48
```

One session syncs both directions — there are no client/server roles in the
protocol, only in who dialed. Notes list in postcard-key order (length
first), identically on every replica.

Offline is the default posture: `add`/`remove` always succeed locally, and
the next `sync` (through any chain of peers — sessions relay third-party
feeds) converges. Deleting a directory's `sod.log` resets that replica: the
next command generates a fresh `replica_id`, because a reused id with a
restarted feed would silently diverge at peers that remember the old one.

## The same app from Node.js (verified transcript)

`node/` packages this app as a napi-rs native addon — the per-app compiled
addon pattern: your datum type + pipeline + sod compiled into one `.node`
module, with a thin app-specific JS surface.

```console
$ cargo build -p sod-demo-node
$ cp target/debug/libsod_demo_node.dylib examples/sod-demo/node/sod_demo_node.node  # .so on linux

$ sod-demo ./a serve 127.0.0.1:7172        # terminal 1: a native peer
$ node examples/sod-demo/node/demo.mjs ./n ws://127.0.0.1:7172   # terminal 2
replica 02a243b228c194356507a7b85fed502f
local: [ '2x note from node', '1x only node has this' ] total=3
after sync: [
  '1x unique to a',
  '2x hello from a',
  '2x note from node',
  '1x greetings from b',
  '1x only node has this'
] total=7
```

Note `greetings from b`: the Node replica has never met replica b — its
feed arrived relayed through a. Node, the native binary, and any future
browser replica speak the same log format and protocol.

## Making it your app

Copy this crate and change three things:

1. the datum type (any `Serialize + DeserializeOwned` type),
2. the fold pipeline handed to `FoldEngine::open`,
3. `SCHEMA`.

Everything else — log, recovery, identity, sync, relay — is sod.
