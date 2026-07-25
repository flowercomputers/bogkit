# Bogkit backend

This submission is a package in the BogKit workspace, with local path dependencies on Fold, ESE, and ANNy.

The package contains:

- `src/lib.rs`: the versioned JSON protocol, appearance/presence records, match state, and three-hit rules;
- `src/main.rs`: the authenticated HTTP/WebSocket service and redacted browser inspector;
- `src/bin/fps-bot.rs`: a persistent deterministic account for one-person social and match testing.

## Bogkit data paths

The server uses Bogkit directly rather than wrapping an unrelated database:

- Fold `KeyedStream -> Table` materializes accounts, token/handle lookup, appearances, presence, matches, friend requests, friendships, invitations, and processed command IDs;
- a Fold fanout maps generated outfit descriptions into BM25 and ESE's 512-dimensional semantic encoder;
- ANNy-backed Fold HNSW terminals index semantic descriptions, non-face whole-body embeddings, and Earth-coordinate presence vectors;
- a Fold `Stream -> FilterMap -> Aggregate -> Table` maintains event-derived damage totals;
- a Fold `FlatMap -> KeyedRanked` view maintains newest-first completed matches for each participant;
- Fold transactions append match events and update server-authoritative snapshots.

Face, region, and silhouette descriptors are persisted in the appearance record but are not mapped into global search indices. The authenticated match-peer endpoint exposes them only during the two players' briefing or active match. Appearance identity and update timestamps come from the authenticated server account, not client-supplied fields.

Appearance profiles are stored in `appearances-v3.db`. On the first startup after upgrading, the server reads both supported positional Postcard layouts from `appearances-v2.db`, imports any players missing from v3, rebuilds the BM25 and HNSW indexes, and writes `appearances-v3.migrated-from-v2`. The v2 database is retained as a backup. A successful migration is therefore safe to retry if startup is interrupted.

Accounts are local to one server. The server returns an opaque credential once and persists only its SHA-256 hash. Private HTTP routes require that credential; realtime connections use one-use, 60-second tickets bound to the authenticated player and selected match.

Nearby Interaction discovery tokens are not persisted. The server keeps only the latest token for each player in an in-memory, match-scoped cache and returns the cached peer token whenever either phone retries, so WebSocket subscription order cannot strand one side at `waiting for peer`.

## Run

From the BogKit repository root:

```sh
cargo test -p untitled-mobile-fps
cargo run -p untitled-mobile-fps
```

The service listens on `0.0.0.0:3000` by default. Open <http://127.0.0.1:3000/inspector> for redacted materialization counts, aggregate match readiness, and search diagnostics. Its public shape contains no invite codes, match/player identifiers, appearance payloads, or coordinates. `GET /health` returns the stable server ID, protocol, environment, capabilities, and required calibration model. Override configuration with `FPS_PORT`, `FPS_DATA_DIR`, `FPS_SERVER_NAME`, `FPS_ENVIRONMENT`, and `FPS_MINIMUM_CLIENT_VERSION`.

For a physical iPhone, choose **Custom Server** and enter `http://<your-mac-lan-address>:3000`. Do not use `localhost` on the phone.

## Bot

Create an invite in the app, then run the full match scenario:

```sh
cargo run -p untitled-mobile-fps --bin fps-bot -- \
  scenario full-match http://127.0.0.1:3000 INVITE_CODE
```

Use `seed-social SERVER_URL YOUR_HANDLE` first when testing friend requests and direct challenges. Set `FPS_BOT_STATE_PATH` to a file beside the disposable server data so the bot can reuse its one-time credential. The file is written atomically with owner-only permissions and is bound to the server's stable ID before the credential is sent. The bot joins or accepts the match, obtains a realtime ticket, readies with the current calibration model, acknowledges the briefing, and continuously sends foreground presence plus a 3 m proximity report. The phone still performs gesture recognition, person segmentation, reticle containment, and physical shot submission.

See [`PHASE_2_TESTING.md`](PHASE_2_TESTING.md) for the complete one-person and two-phone scripts.
