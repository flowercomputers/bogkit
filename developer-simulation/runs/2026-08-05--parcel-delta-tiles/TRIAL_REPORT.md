# Trial report: `parcel-delta-tiles`

Date: 2026-08-05 (America/New_York)

Checkout: sanitized detached `80fd3c9a023e877fff2e5d127accca386d437af0`

## Outcome

The prototype is a successful, bounded implementation of the requested planner,
but the BogKit fit decision is **no fit**. Fold's value is durable incremental
state derived from inserts and retractions. This command is required to be a
stateless, plan-only transformation whose old and new geometries are already the
authoritative delta, and it must not read or write a parcel store. ESE and ANNy
solve embedding and nearest-neighbor problems, not polygon/tile intersection.

The corrected Rust command streams NDJSON one line at a time, rejects duplicate
JSON members and invalid simple topology before emitting anything, enumerates
only geometry-bounding candidate tiles, tests
closed tile rectangles against polygon fill and hole boundaries, deduplicates,
and renders lexicographically sorted `z/x/y` IDs. It uses only `serde` and
`serde_json`; it does not depend on or change BogKit core.

## Finishing criteria used

- One runnable Rust NDJSON CLI in the required top-level directory.
- Old and new `Polygon`/`MultiPolygon` geometry handled without parcel state.
- Exact agreement with a runnable TypeScript full-scan mirror on the named cases
  and a 10,000-edit seeded batch, plus an analytical 500-rectangle check that
  does not share the polygon predicate.
- A tile wholly inside a hole excluded; boundary-only contact included.
- Ten input permutations produce byte-identical output.
- Malformed coordinate, open ring, nonfinite number, unsupported geometry,
  duplicate JSON members, zero-area/self-intersecting rings, outside holes, and
  overlapping holes fail deterministically with the input line and empty stdout.
- A 1,000-edit, 200-distinct-vertex workload stays below 5 seconds and 256 MiB.
- Unit tests, format check, lint with warnings denied, release build, demo, and
  workload comparison pass.
- Only `trial-parcel-delta-tiles` is added; no public example or core file changes.

All criteria passed for the supplied synthetic scope.

## Ordered discovery and friction trail

1. Read the root `README.md` first. It introduces Fold, ESE, ANNy, then lists
   examples in this order: `starter`, `timeseries`, `chat`, `search`.
2. Read each listed example in that order, including its manifest and complete
   public `main.rs`.
   - `starter` showed persistent `Count`/`Bag` views and retraction.
   - `timeseries` showed keyed aggregation maintained from mutations.
   - `chat` showed Fold as durable source of truth.
   - `search` showed keyed upsert/remove driving three maintained indexes.
3. Compared those public contracts with the stated baseline: the input already
   carries authoritative old/new geometry, planning is one-shot, and state access
   is prohibited. This ruled out Fold before implementation; ESE and ANNy were
   plainly unrelated.
4. Inspected the public scaffold. `scripts/new-project.sh` creates under
   `examples/` and unconditionally adds local Fold, ESE, and ANNy dependencies.
   The task required a unique top-level directory and no example changes, so the
   scaffold was not run.
5. No runnable TypeScript parcel baseline or parcel fixture existed in this
   sanitized checkout. To make the stated reference behavior executable, added
   `scripts/reference.ts`. It deliberately scans every candidate tile across the
   complete edited extent, then scans old/new geometries for that tile.
6. Ran the TypeScript mirror on `fixtures/demo.ndjson` before compiling Rust. It
   exited 0 and produced a deterministic plan. The final aligned demo contains
   72 tiles.
7. Implemented the Rust parser and intersection planner. The first normal
   `cargo test` attempted to update crates.io and failed after three DNS retries.
   This was restricted-network environment friction, not a source or BogKit
   failure. `cargo test --offline` used cached dependencies and passed.
8. The first `cargo fmt --check` reported formatting diffs. Ran `cargo fmt`; all
   subsequent format checks passed. No behavior changed.
9. The first sandboxed `/usr/bin/time -l` run completed the planner in 0.05 s,
   but `time` itself exited 1 because sandboxed `sysctl kern.clockrate` was
   denied. Re-running the exact command with host permission produced valid
   timing and peak-RSS evidence.
10. Literal “lexicographically sorted” output was made explicit by sorting the
    rendered `z/x/y` strings. A focused unit test covers differing digit widths.
11. The performance generator was corrected to use 200 distinct vertices plus
    the required repeated ring closure (201 coordinate positions), rather than
    counting the closure as a vertex. All final evidence below uses the corrected
    7,910,712-byte workload.
12. Skeptical review reproduced the declared checks but showed that duplicate
    JSON members were last-member-wins accepted by both implementations and that
    self-intersecting, zero-area, and invalid-hole geometry was silently planned.
    Recursive strict JSON parsing and explicit simple-ring, containment, and
    non-overlap validation now reject those inputs with no stdout.
13. Review also rejected calling the TypeScript implementation an independent
    geometry oracle because it shares the same planar predicate and tolerance.
    It is now labeled a mirror/enumeration cross-check. The verifier adds 500
    worldwide axis-aligned rectangles checked against an analytical tile-range
    construction, plus exact corner and horizontal/vertical boundary cases.
14. The final workload was diversified across inserts, deletes, replacements,
    concave polygons, holes, MultiPolygons, four extents, and a wider synthetic
    county. It has 1,000 lines, 200 distinct vertices per line, 2,922 output
    tiles, and is 7,977,879 bytes. The corrected Rust result matched the mirror,
    then completed in 0.14 seconds with 2,670,592 bytes maximum RSS. The mirror
    took 11.99 seconds and 169,312,256 bytes RSS on the same host-specific input.

## Implementation boundary

Durable files are all under this directory:

- `src/lib.rs`: strict per-line parsing, simple-topology validation, Web Mercator conversion, candidate
  enumeration, rectangle/polygon intersection, hole handling, set construction,
  and output formatting.
- `src/main.rs`: stdin/file CLI and delayed output.
- `scripts/reference.ts`: slow TypeScript full-scan mirror with independent
  enumeration but shared planar geometry rules.
- `scripts/verify.ts`: named mirror comparisons, 10,000 seeded edits, 500
  analytical rectangles, ten permutations, and deterministic malformed checks.
- `scripts/generate-workload.ts`: deterministic diverse 1,000 × 200-vertex workload.
- `fixtures/`: demo and four malformed cases.
- `README.md`: exact input contract and reproduction commands.

Generated workload data is disposable at
`/private/tmp/parcel-delta-tiles-2026-08-05-diverse.ndjson`; it is not part of the trial.

## Exact verification and observed results

Commands were run from `trial-parcel-delta-tiles` unless noted.

### Focused tests, formatting, lint, release

```console
$ cargo test --offline
```

Exit 0. Eight unit tests passed, including exact edge and four-way corner
touches, a tile wholly inside a hole, recursive duplicate rejection, invalid
topology rejection, line-specific malformed input, and lexicographic ordering.

```console
$ cargo fmt --check
```

Exit 0 after applying the formatter once.

```console
$ cargo clippy --offline --all-targets -- -D warnings
```

Exit 0, no warnings.

```console
$ cargo build --release --offline
```

Exit 0. Final incremental release build completed in 0.48 s.

### Demo and baseline comparison

```console
$ target/release/parcel-delta-tiles fixtures/demo.ndjson | wc -l
      72
$ cmp <(target/release/parcel-delta-tiles fixtures/demo.ndjson) \
      <(node scripts/reference.ts fixtures/demo.ndjson)
```

`cmp` exited 0: the 72-line plans were byte-identical.

### Mirror, analytical, permutation, and malformed cases

```console
$ node scripts/verify.ts target/release/parcel-delta-tiles
```

Exit 0 with:

```text
mirror insertion: exact (17 tiles)
mirror deletion: exact (17 tiles)
mirror translation: exact (19 tiles)
mirror concavity: exact (37 tiles)
mirror holes: exact (69 tiles)
mirror multipolygon: exact (8 tiles)
mirror boundary-touch: exact (6 tiles)
mirror seeded-10000: exact (11 tiles)
analytical rectangles: 500 edits exact (34650 tiles)
permutations: 10/10 byte-identical
malformed open-ring: deterministic line 2, empty stdout
malformed coordinate: deterministic line 1, empty stdout
malformed nonfinite: deterministic line 1, empty stdout
malformed unsupported-type: deterministic line 1, empty stdout
malformed duplicate-edit: deterministic line 2, empty stdout
malformed duplicate-geometry: deterministic line 1, empty stdout
malformed duplicate-nested: deterministic line 1, empty stdout
malformed self-intersection: deterministic line 1, empty stdout
malformed zero-area: deterministic line 1, empty stdout
malformed hole-outside: deterministic line 1, empty stdout
malformed hole-overlap: deterministic line 1, empty stdout
verification complete
```

Each malformed case was run twice by the verifier. Diagnostics identify the
input line and a static structural reason without partial stdout. The duplicate
edit regression deliberately places valid line 1 before invalid line 2.

```text
input line 1: new.coordinates[0][1][0] longitude is outside [-180, 180]
input line 1: invalid JSON: number out of range at line 1 column 70
input line 1: new.type "LineString" is unsupported
input line 2: invalid JSON: duplicate object member `new` ...
input line 1: new.coordinates[0] self-intersects
input line 1: new.coordinates[0] has zero area
input line 1: new.coordinates[1] must be strictly inside the exterior ring
input line 1: new.coordinates[1] and new.coordinates[2] overlap or nest
```

All eleven malformed cases had status 1 and zero stdout.

### Full numeric workload and resource evidence

```console
$ node scripts/generate-workload.ts /private/tmp/parcel-delta-tiles-2026-08-05-diverse.ndjson
wrote 1000 mixed-operation edits with 200 distinct vertices per line to /private/tmp/parcel-delta-tiles-2026-08-05-diverse.ndjson
$ wc -l -c /private/tmp/parcel-delta-tiles-2026-08-05-diverse.ndjson
    1000 7977879 /private/tmp/parcel-delta-tiles-2026-08-05-diverse.ndjson
```

The final generator mixes inserts, deletes, and replacements; concave polygons,
polygons with holes, and MultiPolygons; four feature extents; and varied locations
across a wider synthetic county. Every line contains exactly 200 distinct vertices
across its old/new geometry, plus required repeated closures.

```console
$ cmp <(target/release/parcel-delta-tiles /private/tmp/parcel-delta-tiles-2026-08-05-diverse.ndjson) \
      <(node scripts/reference.ts /private/tmp/parcel-delta-tiles-2026-08-05-diverse.ndjson)
$ target/release/parcel-delta-tiles /private/tmp/parcel-delta-tiles-2026-08-05-diverse.ndjson | wc -l
    2922
```

`cmp` exited 0: the full diverse 1,000 × 200 workload matched the mirror exactly.

Final host-permitted measurement:

```console
$ /usr/bin/time -l target/release/parcel-delta-tiles \
    /private/tmp/parcel-delta-tiles-2026-08-05-diverse.ndjson > /dev/null
```

The corrected diverse run observed 0.14 s real and 2,670,592 bytes maximum RSS.
This one-host input-specific result is far below 5 s and 256 MiB.

For a bounded same-input comparison only:

```console
$ /usr/bin/time -l node scripts/reference.ts \
    /private/tmp/parcel-delta-tiles-2026-08-05-diverse.ndjson > /dev/null
```

Observed 11.99 s real and 169,312,256 bytes maximum RSS. This is one run of the
trial-created mirror, not evidence for or against the brief's reported roughly
40-minute production baseline.

## Acceptance audit

| Requirement | Result | Evidence |
| --- | --- | --- |
| Insertion/deletion/replacement | Pass | named mirror cases and mixed final workload |
| Concavity | Pass | named case and mixed final workload |
| Holes | Pass | mirror case, wholly-inside-hole assertion, and mixed workload |
| Multipolygons | Pass | mirror case and mixed workload |
| Boundary touches | Pass | vertical edge, four-way corner, hole boundary, and reviewer probes |
| 10,000 seeded valid edits | Pass | byte-exact mirror comparison |
| 500 analytical rectangles | Pass | 34,650 tiles matched independent range construction |
| Ten permutations | Pass | 10/10 byte-identical |
| 1,000 × 200 under 5 s/256 MiB | Pass | diverse run: 0.14 s, 2,670,592-byte RSS |
| Malformed line/no partial plan | Pass | eleven deterministic cases, empty stdout |
| Plan only | Pass | CLI has no rendering, publishing, deletion, or parcel-store path |

## Categorized findings

### 1. No BogKit component fits the authoritative-delta planner

- Category: fit decision, not a defect
- Severity: informational
- Confidence: high
- Reproduction: read the public examples, then compare their durable stream/view
  contracts with the prohibited-state, authoritative-old/new brief.
- Smallest improvement: a short “when not to use Fold” paragraph in the root
  README would help new Rust users reach this conclusion quickly.

### 2. The prototype meets the bounded functional and numeric acceptance checks

- Category: prototype result
- Severity: success
- Confidence: high for the generated and named fixtures; medium for arbitrary
  production cadastral data
- Reproduction: `node scripts/verify.ts target/release/parcel-delta-tiles` and
  the full-workload commands above.
- Smallest improvement: before production adoption, replay a captured county
  corpus against the actual existing TypeScript reference.

### 3. The stated runnable TypeScript baseline was absent from the sanitized checkout

- Category: trial-input/baseline gap, not a BogKit defect
- Severity: medium because exact production equivalence cannot be established
- Confidence: high
- Reproduction: the initial public file inventory contained only BogKit root,
  crates, scripts, and examples; no parcel baseline or county fixture.
- Smallest improvement: supply the baseline command, fixture/schema, expected
  checksum, and host description with the brief.

### 4. The public project generator installs all three local components

- Category: BogKit onboarding friction, not a core correctness defect
- Severity: low
- Confidence: high
- Reproduction: inspect `scripts/new-project.sh`; it always adds `fold`, `ese`,
  and `anny`, while the README notes they may not all be used.
- Smallest improvement: add a dependency-free/minimal mode or prompt for only
  the components selected after fit evaluation.

### 5. Duplicate members and invalid topology were false-accepted, fixed

- Category: prototype correctness defects, not BogKit defects
- Severity: high
- Confidence: high
- Reproduction: review showed last-member-wins duplicate `new`, a bow-tie ring,
  a zero-area ring, a hole outside its exterior, and overlapping holes all
  produced plans. The corrected verifier preserves each minimal case.
- Smallest improvement completed: reject duplicate object members recursively;
  reject zero-length/zero-area/self-intersecting rings, non-contained holes,
  overlapping/nested holes, and overlapping/nested MultiPolygon exteriors.

### 6. The TypeScript comparison is a mirror, not an independent geometry oracle

- Category: evidence limitation
- Severity: medium for a production go/no-go decision
- Confidence: high
- Reproduction: Rust and TypeScript use different enumeration strategies but
  share parsing rules, tolerance, clipping, point-in-ring, and hole logic.
- Smallest improvement completed: relabel it as an enumeration mirror and add a
  500-rectangle analytical range check. Production adoption still needs the real
  county reference or a genuinely independent geometry engine.

### 7. The normal Cargo path attempted network access despite cached dependencies

- Category: environment/onboarding friction, not a BogKit defect
- Severity: low
- Confidence: high
- Reproduction: `cargo test` failed with `Could not resolve host:
  index.crates.io`; `cargo test --offline` immediately resolved cached crates and
  passed.
- Smallest improvement: document `--offline` for sanitized lab runs or pre-create
  the trial lockfile before the first build.

### 8. Evidence remains scoped to a planar, synthetic contract

- Category: evidence limitation
- Severity: medium for a production go/no-go decision
- Confidence: high
- Reproduction: compare the local straight-longitude/latitude predicate and
  synthetic workload with the absent production convention and county corpus.
- Smallest improvement: replay captured production data against the supplied
  production reference and independently chosen geometry library.

## Decision audit

| Choice | Decision | Reason |
| --- | --- | --- |
| Fold | Reject | Requires durable mutation-derived state; this input is already the delta and state access is forbidden. |
| ESE | Reject | Embeddings do not contribute to spatial tile planning. |
| ANNy | Reject | Approximate nearest-neighbor indexing does not answer exact polygon/tile intersection. |
| Stateless Rust + strict `serde`/`serde_json` parsing | Select | Meets the bounded streaming, deterministic, and reviewed simple-topology contract. |
| External geometry crate | Reject for one-day prototype | Cached availability was unverified. The corrected simple-topology validator covers the named boundary, but a proven library should be reconsidered for production validity. |
| Emit as each line parses | Reject | Would violate no-partial-plan behavior on a later malformed line. |
| Buffer all parsed edits | Reject | Unnecessary; parse and accumulate tiles per line to keep memory bounded by one edit plus the result set. |
| Scan all world/county tiles in Rust | Reject | Repeats baseline pain; enumerate each polygon's closed bounding tile range instead. |
| Use the public scaffold | Reject | It would modify `examples/`, violate the requested top-level layout, and add unrelated dependencies. |

## Unresolved uncertainty

- The real TypeScript baseline, real synthetic-county fixture, and reported
  40-minute environment were unavailable, so production equivalence and speedup
  are unverified.
- The corrected validator covers recursive duplicates, simple nonzero rings,
  hole containment/non-overlap, and MultiPolygon exterior non-overlap. It is not
  a formal implementation of every GeoJSON validity recommendation.
- The implementation treats GeoJSON edges as straight longitude/latitude
  segments and tile rectangles as closed. That matches the local reference, but
  the production reference's geodesic/planar convention was not supplied.
- Boundary decisions use a small floating-point tolerance. Exact vertical edge,
  four-way corner, hole boundary, horizontal edge, and maximum-Mercator probes
  passed, but a production corpus should add many generated boundary cases.
- The resource evidence is host-specific. The final workload is more diverse
  than the initial 16-tile fixture but still synthetic.
  Very large polygons generate proportionally more candidate tiles and a very
  large final plan necessarily consumes more memory.
- Output order is lexicographic over rendered `z/x/y` strings. If the consumer
  intended numeric tuple order instead, the schema must say so explicitly.

## Scope and defect attribution

No BogKit core defect was found because no BogKit runtime component fits this
workload and none is used. The unconditional scaffold dependencies are a small
onboarding issue. Missing production baseline evidence belongs to the trial
setup. Geometry-validity and evidence limits belong to this prototype. Git status
at handoff showed detached HEAD with only `?? trial-parcel-delta-tiles/`; no core
or public example was modified.
