# Wholesale catalog trial B

This is a standalone Rust prototype for a three-category wholesale catalog. It keeps the existing SQLite file as the source of truth and adds:

- Axum create, read, update, delete, and filter routes;
- three hardcoded category validators with laptop revision activation;
- stable nested validation errors such as `specs.battery_wh`;
- version-checked partial updates that preserve omitted fields;
- exact and numeric-range indexes checked against a separate in-memory evaluator;
- explicit laptop schema migration from revision 1 to revision 2; and
- checkpointed imports that bind each job to a source fingerprint, verify
  duplicate content, and resume after an orderly database reopen.

## Component decision

Fold, ESE, and ANNy are deliberately not used in the runtime path. Fold's
durable incremental views use a separate Fjall store. That is useful for
derived views, but this brief requires preserving SQLite as the authority. A
sidecar's synchronization and storage cost were not measured. ESE and ANNy
solve embedding and nearest-neighbor search, which are non-goals here. No
BogKit dependency is included.

## Exact reproduction

Run from this directory:

```console
cargo fmt --check
CARGO_NET_OFFLINE=true cargo test
CARGO_NET_OFFLINE=true cargo clippy --all-targets -- -D warnings
CARGO_NET_OFFLINE=true cargo run --release -- demo
CARGO_NET_OFFLINE=true cargo run --release -- import-check 250000 2048
CARGO_NET_OFFLINE=true cargo run --release -- burst 250000
CARGO_NET_OFFLINE=true cargo run --release -- storage-check 250000 2048
```

The first build uses dependencies already available with BogKit and requires a
linkable system `sqlite3` library. Generated databases stay under
`target/trial-data`.

## Run the HTTP API

```console
CARGO_NET_OFFLINE=true cargo run --release -- serve target/trial-data/server.sqlite
```

In another terminal:

```console
curl -sS http://127.0.0.1:3000/products \
  -H 'content-type: application/json' \
  -d '{"id":"c-1","category":"cable","name":"USB-C cable","price_cents":1299,"description":"2 KiB records are used by the scale checks","specs":{"length_m":2.0,"connector":"usb-c"},"compatibility":[{"system":"inventory","model":"v1"}],"tags":["wholesale"]}'

curl -sS 'http://127.0.0.1:3000/products?exact_path=specs.connector&exact_value=usb-c'

curl -sS -X PATCH http://127.0.0.1:3000/products/c-1 \
  -H 'content-type: application/json' \
  -d '{"expected_version":1,"specs":{"length_m":3.5}}'

curl -sS -X DELETE 'http://127.0.0.1:3000/products/c-1?expected_version=2'
```

Responses retain the product fields used by this prototype. The route-level
smoke test covers POST and GET. PATCH, filtering, stale-version rejection,
validation, and DELETE behavior are implemented and exercised directly or by
the demo, but are not claimed compatible with an unavailable real-service
contract.

## Compact boundary and honest limits

The generator covers laptop, cable, and chair records, including common
scalars, nested category specifications, one compatibility entry, tags, and a
deterministic 2 KiB description. The validators are hardcoded in Rust; this is
not general runtime-defined schema support. Laptop schemas have two revisions.
The scale commands use all 250,000 records at the low end of the requested
2–20 KiB range.

The burst test sends 150 simultaneous in-process HTTP requests, with 120 reads
and 30 writes, against the full population. One global mutex serializes SQLite
access. The test does not emulate network latency, exercise a second process or
same-version write race, or enforce a 512 MiB process limit. Across the author,
reviewer, and post-fix runs, read p95 was 1.518–4.037 ms and write p95 was
1.389–2.721 ms. The storage comparison uses a synthetic baseline SQLite
database with the same 2 KiB product rows and ordinary category/price indexes,
then compares it with the schema/facet-indexed database.

See `EVIDENCE.md` for observed results, friction, the decision audit, and remaining uncertainty.
