# Ordered discovery, debugging, and friction trail

This trail was recorded from the sanitized checkout only. No Git history, GitHub, prior simulation material, automation memory, other trial, or BogKit source internals were inspected.

1. Read the complete scenario.

   Command: `sed -n '1,260p' /private/tmp/bogkit-sim-2026-08-08-designer/trial1-brief.md`

   Observed: an immutable nightly batch; 62,000 claim revisions; 50,000 remittance lines; exact-cent conservation; bounded clusters of at most 24 nodes; uncertainty must become review; required greedy-baseline comparison, hidden-truth accuracy, ten-seed stability, and 60-second runtime.

2. Started from the public root README and enumerated only public examples, as required.

   Commands:

   ```sh
   sed -n '1,320p' README.md
   find examples -maxdepth 3 -type f -print
   ```

   Observed: Fold is described as a durable incremental-programming framework; ESE as static embeddings; ANNy as HNSW nearest-neighbor search. Public examples were `starter`, `timeseries`, `search`, and `chat`.

3. Read every public example before making the fit decision.

   Commands: `sed -n '1,420p'` over each example's `Cargo.toml` and `src/main.rs`.

   Observed: Fold examples materialize counts, bags, aggregates, tables, and search indexes under insert/retract/upsert workloads. None demonstrates exact, bounded global assignment. ESE/ANNy are used for semantic document retrieval.

4. Chose an evidence-backed BogKit no-fit for the reconciliation core.

   Reason: an immutable batch does not benefit from durable incremental materialization during the prototype, while approximate semantic similarity is unsafe for money posting. In this one trial, no public primitive directly performed the constrained assignment. This is a one-off fit observation, not a defect or promoted feature candidate.

5. Reproduced the baseline and implemented the safer path.

   The one binary contains `baseline`, `reconcile`, `generate`, `shuffle`, `verify`, and `demo`. After fix round 1, the comparator runs exact-reference candidates first and fallback only when that stage has no feasible match; greediness remains inside each stage. The safer path keeps candidate-source provenance, filters obsolete revisions and duplicates, builds connected components, and searches the allowed cluster assignments under exact-cent constraints.

6. First test attempt hit temporary lab-packaging behavior because the assigned output crate sat beneath, but outside, the parent Cargo workspace.

   Command: `CARGO_TARGET_DIR=/private/tmp/bogkit-trial1-target cargo test --all-targets`

   Observed: `current package believes it's in a workspace when it's not`; Cargo suggested adding the package to root workspace members or adding an empty `[workspace]` table. Modifying the root manifest was prohibited, so the temporary simulator package used `[workspace]`. This is not BogKit API/scaffold friction. The coordinator later removed it and the member lockfile during the required nested-workspace archive transform.

7. Second test attempt failed on network access.

   Same command after the workspace fix.

   Observed: three retries ended with `Could not resolve host: index.crates.io`. Re-running with `CARGO_NET_OFFLINE=true` used already cached dependencies. No network access was added to the prototype.

8. First offline compile exposed one prototype-authoring type-inference error.

   Command: `CARGO_NET_OFFLINE=true CARGO_TARGET_DIR=/private/tmp/bogkit-trial1-target cargo test --all-targets`

   Observed: `E0282` at the candidate index set. Adding the explicit `BTreeSet<usize>` type was sufficient.

9. Unit tests passed after that fix.

   Observed: 5 passed, 0 failed. Tests covered same-claim split enforcement, rejected cross-claim split, current-revision filtering, deterministic shuffle, and verifier ratio behavior.

10. Strict Clippy initially reported style and API-shape warnings.

    Command: `CARGO_NET_OFFLINE=true CARGO_TARGET_DIR=/private/tmp/bogkit-trial1-target cargo clippy --all-targets --all-features -- -D warnings`

    Observed: the initial manifest also enabled the optional `pedantic` group, producing 25 findings. After removing that extra policy, fixing all remaining standard Clippy findings, and factoring one complex verifier return type, the exact required command passed with warnings denied.

11. The first demo run found an over-restrictive fixture-size guard.

    Command: `cargo run --quiet -- demo --work-dir /private/tmp/bogkit-trial1-demo.Jja1ou`

    Observed: `fixture sizes are too small for the authored edge cases`. The guard had used a rough claim/remittance ratio instead of the exact edge-case count. It was corrected to derive the minimum from the constructed cases.

12. The corrected demo passed and made the baseline failure visible.

    Observed:

    ```text
    demo improved: accepted=183 review=17 precision=100.0000% recall=100.0000% passed=true
    demo greedy baseline: accepted=171 review=29 precision=71.9298% recall=63.0769% passed=false
    ```

13. Generated and reconciled the deterministic authored full-size workload.

    Commands:

    ```sh
    remittance-reconciliation generate --out /private/tmp/bogkit-trial1-full.s0APkQ/fixture --claim-count 62000 --remittance-count 50000
    remittance-reconciliation reconcile --claims /private/tmp/bogkit-trial1-full.s0APkQ/fixture/claims.jsonl --remittances /private/tmp/bogkit-trial1-full.s0APkQ/fixture/remittances.jsonl --out /private/tmp/bogkit-trial1-full.s0APkQ/results
    ```

    Observed: generation took 0.126s. Reconciliation took 4.427s and produced 49,983 accepted remittance decisions, 17 review decisions, and 49,995 links.

14. Independent verification passed.

    Command: `remittance-reconciliation verify` with the full claims, remittances, generated hidden truth, and result directory.

    Observed: 100% precision, 100% recall, 49,995 correct links, zero invariant failures against this crate's own deterministic generator and truth only.

15. The greedy comparator failed as intended.

    Commands: `remittance-reconciliation baseline` followed by `verify --allow-failure`.

    Observed: 99.903944% precision, 99.855986% recall, 12 links to obsolete revisions, and failure of the 99.95% precision threshold. Its faster 0.207s runtime does not compensate for unsafe decisions.

16. Ten shuffled-input checks passed byte for byte.

    Command: `scripts/check-shuffles.sh` with seeds `1 2 3 5 8 13 21 34 55 89`.

    Observed: all three canonical files matched the ordered reference for every seed. In the final acceptance run, reconciliation times ranged from 4.597s to 5.278s.

17. Peak-resident-memory measurement was blocked by the sandbox after a successful timed application run.

    Command: `/usr/bin/time -l remittance-reconciliation reconcile ...`

    Observed: the application completed in 4.591s, then `/usr/bin/time` exited 1 because `sysctl kern.clockrate` was not permitted. Peak RSS is therefore not claimed. Code inspection and verifier checks cover line-buffer reuse and absence of patient-key values in outputs.

18. Manual output inspection confirmed stable review codes, paired same-snapshot negative reversal allocations, full-open-balance-subset same-claim splits, current-revision selection, and competitor explanations. The final source checks were then rerun after documentation and safety-bound changes.

19. Skeptical re-review exposed a partial-overlap hole in the first identity-source fix.

    Reproducer: exact-reference candidates `{A,B}`, fallback candidates `{B,C}`, shared `B` with 50 cents of capacity, and a 100-cent remittance. Round one tested only disjoint sets, so the union still let the optimizer accept reference-only `A`.

    Correction: whenever both sources are nonempty, the allocation engine now receives only their intersection. The stable conflict reason is retained when differing sources leave no feasible shared assignment. A dedicated split case proves that downstream option generation cannot restore reference-only candidates.

20. Focused permutation and control checks passed.

    The exact overlap reproducer ran with original, reversed, and shuffled claims plus mixed unrelated remittance order. Canonical outputs were byte-identical, the verifier passed, the unrelated cluster accepted, and the overlap row reviewed with only shared `B` counted. A 100-cent `B` control accepted only `B`; exact-only and fallback-only controls also accepted.

21. Round-two package and workload gates passed.

    Formatting passed; 6 unit and 17 adversarial tests passed; strict Clippy passed with warnings denied. The demo remained 183 accepted / 17 review at 100% authored-generator precision and recall. The full 62,000/50,000 authored run completed in 4.705s with 49,995/49,995 correct links and no verifier failures. All ten shuffles were byte-identical at 4.823–5.195s. The acceptance script removed its fixture/results/build directory when it exited.

## Categorized findings

| Category | Finding | Severity | Confidence | Reproduction | Smallest improvement |
|---|---|---:|---:|---|---|
| Correctness defect | Greedy input-order choice consumes a line needed by a later exact-reference match. | Critical | High | Run the `baseline` command; `R-TRAP-*-WEAK` takes claim A and blocks the strong line. | Resolve each connected cluster jointly and review tied optima. |
| Correctness defect | Baseline treats an obsolete claim revision as eligible. | Critical | High | Full baseline verifier lists 12 non-current targets. | Filter to the highest revision before candidate generation. |
| Correctness defect | Greedy baseline auto-accepts equal strong candidates rather than reviewing uncertainty. | Critical | High | Inspect baseline `R-AMBIG-*` decisions. | Make equal-best global assignments a review outcome. |
| Correctness defect | A union of partially overlapping exact-reference and fallback sets can authorize a candidate supported by only one identity source. | Critical; resolved in round 2 | High | Run `overlapping_identity_sources_require_a_feasible_shared_candidate_in_all_orders` or the focused CLI fixture. | Intersect both nonempty source sets before all single/split option generation and review infeasible disagreements. |
| Performance problem | No generator performance failure was observed; worst recorded shuffle was 5.278s versus 60s. The solver needed an explicit worst-case search bound despite that narrow speed result. | High if absent; resolved | High | Run both search-budget tests. | Keep the deterministic node ceiling and review an exhausted cluster. |
| Lab packaging | The assigned standalone output crate sat below but outside an existing workspace, requiring temporary `[workspace]`. | Low | High | Run Cargo in the temporary simulator location. | Coordinator removed `[workspace]` and member `Cargo.lock` at nested-workspace archival, then reran gates. |
| Authoring history | The first Rust compile required an explicit candidate-index collection type (`E0282`). | Informational | High | See the recorded compiler output. | No BogKit change; this was repaired in prototype source. |
| Documentation gap | Public docs describe Fold primitives and demos but do not give a fit/no-fit guide for immutable global-constraint batches. | Medium | High | Read the root README and public examples. | Add a short decision table: incremental materialized views versus one-shot global optimization. |
| One-off capability observation | This trial did not find a public exact constrained-assignment primitive. It is one independent use case, below any feature-candidate threshold. | Observation only | Medium | Compare this brief to public README/example APIs. | Record qualitatively; do not propose or promote a candidate from one trial. |
| Poor product fit | ESE/ANNy approximate semantic similarity cannot establish exact money-posting identity. Fold persistence is not needed inside this immutable batch prototype. | Critical for ESE/ANNy; Medium for Fold | High | Compare the search example to cent conservation and uncertainty requirements. | Keep approximate retrieval out of auto-posting; consider Fold only for later ingestion/audit views. |
