# sod-web

Three bogs, one board. An emoji reaction wall where every app instance
embeds its own [sod](../../sod) replica — writes always land locally, and
instances converge by syncing whoever they can reach. The top nav shows it
happening: your replica's identity tints the whole page, and every
currently-connected peer appears as a dot *in its own color*. Kill the
network and you watch your neighbors' colors drain out of your header.

The wifi-kill demo, in three acts:

1. **Converge.** A Fly-deployed hub and two local instances
   (`localhost:3000`, `localhost:3001`) all show the same board.
2. **Partition.** Kill the wifi. The two local bogs keep syncing with each
   other over loopback — that's a *real* partial partition, not a
   simulation — while the hub goes dark and lags. Keep reacting everywhere.
3. **Heal.** Wifi back on. The hub absorbs both locals' writes (relay
   included) and all three boards converge byte-identically.

The scripted rehearsal of exactly this (`npm run demo:local`) runs in CI
distance: three instances, partition, heal, byte-identical assertion.

## Build & run one instance

```console
$ cd examples/sod-web
$ npm install
$ npm run build:addon        # cargo build + copy the .node binary
$ npm run build
$ SOD_DATA_DIR=./.sod-a npm run dev
```

Config is uniform — dial-vs-serve is about *reachability*, never role:

| env | meaning |
|---|---|
| `SOD_DATA_DIR` | replica directory (log + fold db + replica id) |
| `SOD_SERVE_ADDR` | optional: accept sync sessions here |
| `SOD_PEERS` | optional: comma-separated ws URLs of everyone you can reach |

## The three-bog topology

```console
# hub (stands in for Fly when rehearsing locally; serve-only — NAT'd
# peers can reach it, it can reach no one)
$ SOD_DATA_DIR=./.sod-hub SOD_SERVE_ADDR=127.0.0.1:7302 PORT=3002 npm run dev

# local a — serves and dials b + hub
$ SOD_DATA_DIR=./.sod-a SOD_SERVE_ADDR=127.0.0.1:7300 \
  SOD_PEERS=ws://127.0.0.1:7301,ws://127.0.0.1:7302 PORT=3000 npm run dev

# local b — serves and dials a + hub
$ SOD_DATA_DIR=./.sod-b SOD_SERVE_ADDR=127.0.0.1:7301 \
  SOD_PEERS=ws://127.0.0.1:7300,ws://127.0.0.1:7302 PORT=3001 npm run dev
```

Or scripted end-to-end (also the regression test):

```console
$ npm run build:addon && npm run build && npm run demo:local
...
act 1 PASS: three bogs converged; locals connected to 2
act 2 PASS: partition held — locals kept syncing, hub lagged
act 3 PASS: heal converged all three boards byte-identically
DEMO-LOCAL PASS
```

"Pause remote sync" in the nav is the deterministic stand-in for the wifi
kill (it pauses every non-localhost peer); the per-peer Pause buttons in
the nerd panel partition selectively.

## Deploying the hub to Fly.io

> Status: **deployed and verified** — `https://sod-web-demo.fly.dev`
> (sanctuary-computer org), sync on `ws://sod-web-demo.fly.dev:10700`.
> A laptop replica has synced bidirectionally with it over the public
> internet. Note: the raw-TCP sync port requires a **dedicated IPv4**
> (~$2/mo, `fly ips allocate-v4`) — Fly's free shared IPv4 only routes
> HTTP/TLS-handler services. The TLS flip (below) would lift that.

From the **repo root** (the Docker context needs `sod/` and `fold/`):

```console
$ fly launch --no-deploy -c examples/sod-web/fly.toml   # first time
$ fly volumes create sod_data -c examples/sod-web/fly.toml --size 1
$ fly deploy . -c examples/sod-web/fly.toml
```

The hub serves the UI at `https://sod-web-demo.fly.dev` and sync on raw
TCP port `10700`. Point the locals at it:

```console
$ SOD_DATA_DIR=./.sod-a SOD_SERVE_ADDR=127.0.0.1:7300 \
  SOD_PEERS=ws://127.0.0.1:7301,ws://sod-web-demo.fly.dev:10700 PORT=3000 npm run dev
$ SOD_DATA_DIR=./.sod-b SOD_SERVE_ADDR=127.0.0.1:7301 \
  SOD_PEERS=ws://127.0.0.1:7300,ws://sod-web-demo.fly.dev:10700 PORT=3001 npm run dev
```

Plain TCP first; before showing outside the room, add `handlers = ["tls"]`
to the sync port in `fly.toml` and enable sod's wss client support.

### Stage runbook

1. Open the Fly URL, `localhost:3000`, and `localhost:3001` side by side.
   Each window wears its replica's color; locals show `2 bogs connected`.
2. React everywhere; boards agree within a tick or two.
3. Kill the wifi. Locals drop to `1 bog connected` (each other, over
   loopback) and the Fly peer row goes `unreachable`; the hub's own
   badge drops to `0`. Keep reacting on all three.
4. Reconnect. Within a sync tick the hub jumps to `2 bogs connected` and
   every board converges — including writes it never saw directly,
   relayed through whichever local reached it first.

## Anatomy

- `addon/` — the per-app compiled sod: datum `String` (emoji slug),
  pipeline `(sod::sinks::Bag<String>, fold Count)`, `SCHEMA = 1`. Async
  `syncWithPeer` (libuv worker), serve loop on a Rust thread that borrows
  the replica only per session, peer liveness from `SyncReport`s.
- `lib/sod.ts` / `lib/sync-loop.ts` — module-singleton replica + the 3 s
  dial loop ("offline" is just these attempts failing; local writes kick
  an immediate pass).
- `app/` — the board, the nav badge, and the nerd panel (per-peer status
  and pause, connected-now vs heard-from-ever, the raw version vector).
- `scripts/smoke.mjs` — addon smoke test (includes a two-process sync).
- `scripts/demo-local.mjs` — the scripted three-act rehearsal.

Design spec: `docs/superpowers/specs/2026-08-16-sod-web-design.md`.
