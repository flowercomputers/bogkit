# Parcel delta tile planner trial

This is a stateless Rust command that reads parcel edits as NDJSON and prints the
unique Web Mercator tiles at zooms 12 through 16 touched by an old or new filled
geometry. It plans only: it does not read parcel state or render, publish, or
delete tiles.

No BogKit component is used. The root README and examples describe Fold as a
durable incremental-state engine. This input is already the authoritative delta,
and the command is forbidden from maintaining or consulting state, so Fold would
add persistence and retraction machinery without removing the geometry work.
ESE and ANNy do not address geometry.

## Input and output

Each nonblank input line has this shape:

```json
{"id":"parcel-42","old":null,"new":{"type":"Polygon","coordinates":[[[0,0],[1,0],[1,1],[0,0]]]}}
```

`id` is a non-empty string. `old` and `new` are optional or null, but at least
one must be a GeoJSON `Polygon` or `MultiPolygon`. Positions must contain exactly
longitude and latitude. Rings must contain at least four positions and be
closed. Longitudes must be in `[-180, 180]`; latitudes must be within Web
Mercator's limits. Duplicate JSON members are rejected recursively. Rings must
have nonzero area and no self-intersection; holes must be strictly contained and
non-overlapping; MultiPolygon exteriors may not overlap or nest.
Antimeridian-crossing geometry is explicitly rejected.

Output is one `z/x/y` tile per line, sorted lexicographically by that rendered
tile ID. A tile counts when its closed rectangle intersects the
filled polygon, including boundary-only contact. A tile wholly inside a hole is
excluded; hole boundaries count as contact.

The command validates the complete stream before writing any plan, so an error
names the input line on stderr and leaves stdout empty.

## Reproduce

Run from the BogKit repository root:

```console
export CARGO_TARGET_DIR=/private/tmp/parcel-delta-tiles-target
trial=developer-simulation/runs/2026-08-05--parcel-delta-tiles
cargo test --offline --locked --manifest-path developer-simulation/Cargo.toml \
  -p parcel-delta-tiles
cargo fmt --manifest-path developer-simulation/Cargo.toml -p parcel-delta-tiles -- --check
cargo clippy --offline --locked --manifest-path developer-simulation/Cargo.toml \
  -p parcel-delta-tiles --all-targets -- -D warnings
cargo build --release --offline --locked --manifest-path developer-simulation/Cargo.toml \
  -p parcel-delta-tiles
/private/tmp/parcel-delta-tiles-target/release/parcel-delta-tiles "$trial/fixtures/demo.ndjson"
node "$trial/scripts/reference.ts" "$trial/fixtures/demo.ndjson"
node "$trial/scripts/verify.ts" /private/tmp/parcel-delta-tiles-target/release/parcel-delta-tiles
node "$trial/scripts/generate-workload.ts" /private/tmp/parcel-delta-tiles-2026-08-05-diverse.ndjson
/usr/bin/time -l /private/tmp/parcel-delta-tiles-target/release/parcel-delta-tiles \
  /private/tmp/parcel-delta-tiles-2026-08-05-diverse.ndjson > /dev/null
```

`scripts/reference.ts` deliberately scans every candidate tile across the
complete edited county extent and then scans edit geometries for that tile. It
is a slow full-scan enumeration mirror, not an independent geometry oracle or a
production algorithm: it shares the Rust planner's planar intersection design.
The verifier separately checks 500 axis-aligned rectangles against an analytical
tile-range construction.

See `TRIAL_REPORT.md` for observed results and limitations.
