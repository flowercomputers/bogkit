# Consequential decision audit

## Decisions taken

1. **Kept the tool advisory and local.** It reads files and writes a review
   artifact; it has no crane, database, network, or dispatcher-write path.
2. **Used no BogKit component.** This followed the public-fit evaluation rather
   than treating component use as a goal. Fold's incremental persistence and the
   text/vector search tools do not establish move legality or solve constraint
   planning.
3. **Made current-result publication generation-safe.** Planning occurs on a
   clone. Before reading a new input generation, the tool invalidates its three
   owned artifact names. It fully writes and syncs a hidden temporary file, then
   atomically renames exactly one `moves.json` or `review.json`. Malformed input
   and injected write failure leave no current or partial artifact.
4. **Favored deterministic bounded lookahead.** Eight upcoming pickup IDs affect
   destination ranking, followed by stack flexibility, distance, and stack ID.
   The fixed bound limits work and makes repeated output stable.
5. **Required reciprocal neighbors.** Hazardous adjacency is operationally
   undirected; rejecting ambiguous input is safer than silently checking only
   one direction.
6. **Kept generated material outside the archive.** Cargo target files and demo
   JSON/output live in `/private/tmp`. No source, example, root, Git, GitHub, or
   automation state was modified.

## Rejected alternatives

- **Fold as planner state:** rejected because the run is a frozen snapshot and
  does not need durable incremental materialized views. A plain cloned state is
  smaller and easier for a separate transition replay to duplicate.
- **ANNy or ESE for destination selection:** rejected because vector similarity
  is not a legality proof and would make exact deterministic reasoning harder.
- **Globally optimal exhaustive search:** rejected by the explicit non-goal and
  the 10-second bound.
- **Randomized local search:** rejected because canonical repeated output is a
  hard requirement.
- **Partial best-effort move lists:** rejected because an unverified prefix must
  never be mistaken for executable guidance.
- **Live integrations or persistence:** rejected by the advisory prototype
  boundary.

## Dependencies and assumptions

- Rust toolchain with edition-2024 support.
- Locked `serde` and `serde_json` crates, available from the local cache during
  this trial; online dependency retrieval was unavailable.
- A single-slot normalized container model, integer weight classes 1-5, unique
  IDs, reciprocal neighbor lists, and Manhattan distance over supplied `x/y`
  coordinates.
- The snapshot is authoritative and already expresses the applicable hazardous
  segregation table. Missing mid-run operational facts require a fresh plan.

## Uncertainty

The dominant uncertainty is external validity. The real 30 snapshots, C#
baseline outputs, dispatcher edits, and specified laptop were absent. The
synthetic evidence is useful for mechanics and regression, not a substitute for
acceptance on those artifacts. Search incompleteness is confirmed, not merely
uncertain: the preserved three-stack witness has a legal sequence that the
heuristic misses. Timing is fixture-level, not one hard end-to-end deadline.
