# Ordered discovery, debugging, and verification trail

All commands ran in the sanitized checkout. Prototype commands ran from
`simulation-output/container-yard-planner`. Generated inputs and build output
were kept under `/private/tmp`.

1. Read the supplied scenario before anything else:

   ```console
   sed -n '1,240p' /private/tmp/bogkit-sim-2026-08-08-designer/trial2-brief.md
   ```

   Observed the advisory-only boundary, frozen-snapshot model, explicit yard
   constraints, nearest-slot baseline, 10-second limit, deterministic-output
   requirement, and 30-snapshot acceptance target.

2. Read the root public README, listed public examples, then read all four
   example manifests and source files:

   ```console
   sed -n '1,260p' README.md
   find examples -path '*/target' -prune -o -type f -print | sort
   sed -n '1,320p' examples/starter/src/main.rs
   sed -n '1,320p' examples/timeseries/src/main.rs
   sed -n '1,320p' examples/search/src/main.rs
   sed -n '1,320p' examples/chat/src/main.rs
   ```

   Observed durable stream materialization in Fold, ESE/ANNy-based text search,
   and no public constraint-planning primitive. Chose no BogKit dependency.

3. The first test attempt failed before compilation:

   ```console
   CARGO_TARGET_DIR=/private/tmp/bogkit-sim-2026-08-08-trial2-target cargo test --all-targets
   ```

   Observed: `current package believes it's in a workspace when it's not`.
   Temporary lab packaging response: added an empty `[workspace]` section to the
   prototype manifest, preserving the root workspace unchanged. This is not a
   BogKit API defect; the archive coordinator removes it at the nested-workspace
   copy step.

4. The next online attempt retried crates.io three times and failed DNS lookup:

   ```text
   failed to download from https://index.crates.io/config.json
   Could not resolve host: index.crates.io
   ```

   Smallest fix: used the cached dependencies with `--offline`; this generated
   a temporary child `Cargo.lock` that the coordinator later removed during
   nested-workspace archival.

5. The first offline compile found a moved-string borrow error in the relocation
   record. Cloning the small identifier at the output boundary fixed it.

6. The first complete test run passed six tests and failed the constraint
   destination test because the synthetic reefer blocker had no hazardous group,
   making `BAD_HAZARD` genuinely legal. Corrected the fixture to hazardous group
   `A`, added reciprocal neighbor declaration, and reran. All seven passed.

7. `cargo fmt --all -- --check` initially showed only formatting diffs. Ran
   `cargo fmt --all`, then the check passed with no output.

8. Strict Clippy initially reported documentation, must-use, long-function,
   similar-name, and test-cast findings. Added public API documentation and
   must-use annotations, removed the ambiguous local name and unchecked casts,
   and explicitly scoped the two long state-machine functions. Final command:

   ```console
   CARGO_TARGET_DIR=/private/tmp/bogkit-sim-2026-08-08-trial2-target cargo clippy --offline --all-targets --all-features -- -D warnings
   ```

   Observed: `Finished dev profile ...` with no warnings.

9. Final release acceptance run:

   ```console
   CARGO_TARGET_DIR=/private/tmp/bogkit-sim-2026-08-08-trial2-target cargo test --offline --release --test acceptance -- --nocapture --test-threads=1
   ```

   Observed: seven passed, zero failed. The displayed 81 baseline vs 54 planned
   relocation total is one 3-to-2 micro-geometry repeated with 27 identifier
   suffixes, not 27 diverse cases. Three small infeasible cases returned review.
   A distinct 288-stack, 1,280-container, 40-pickup workload used 42 baseline vs
   41 planned relocations and roughly 8-16 ms planning/replay time across
   observed runs.

10. Runnable in-memory demonstration:

    ```console
    CARGO_TARGET_DIR=/private/tmp/bogkit-sim-2026-08-08-trial2-target cargo run --offline --release -- demo
    ```

    Observed JSON:

    ```json
    {
      "baseline_relocations": 3,
      "bounded_lookahead_relocations": 2,
      "improvement_percent": 33,
      "planner_replay_verified": true,
      "deterministic": true
    }
    ```

11. File-based demonstration used inputs under
    `/private/tmp/bogkit-trial2-demo/input` and output under
    `/private/tmp/bogkit-trial2-demo/output`:

    ```console
    CARGO_TARGET_DIR=/private/tmp/bogkit-sim-2026-08-08-trial2-target cargo run --offline --release -- plan /private/tmp/bogkit-trial2-demo/input/yard.json /private/tmp/bogkit-trial2-demo/input/pickups.json /private/tmp/bogkit-trial2-demo/output
    CARGO_TARGET_DIR=/private/tmp/bogkit-sim-2026-08-08-trial2-target cargo run --offline --release -- verify /private/tmp/bogkit-trial2-demo/input/yard.json /private/tmp/bogkit-trial2-demo/input/pickups.json /private/tmp/bogkit-trial2-demo/output/moves.json
    ```

    Observed: `executable plan -> .../moves.json`, followed by
    `verified 4 legal moves`. The plan contained two relocations and two pickups,
    with rule-check names on every move.

## Skeptical-review fix round 1 of 5

12. Read the complete skeptical review before making any change:

    ```console
    sed -n '1,320p' /private/tmp/bogkit-sim-2026-08-08-review/review.md
    ```

    Confirmed blocker B4 (stale executable publication), the exact feasible
    false-negative witness, incomplete verifier scope, baseline/timing overclaim,
    and temporary archive-packaging issue.

13. Implemented generation-safe publication. Each file-based planning run now
    invalidates `moves.json`, `review.json`, and `.yard-plan.tmp` before reading
    input. Publication writes and syncs the hidden temporary file, calls atomic
    rename, syncs the directory, and removes current/partial artifacts on error.
    Other output-directory filenames are not touched.

14. Implemented executable-contract verification before transition replay. It
    checks status/executable/replay flags, relocation and pickup counts, step and
    pickup ranks, exact immediate reasons, and exact canonical rule-check lists.
    Renamed evidence language from independent simulator to separate transition
    replay sharing static snapshot and hazardous definitions.

15. Final complete verification:

    ```console
    cargo fmt --all -- --check
    CARGO_TARGET_DIR=/private/tmp/bogkit-sim-2026-08-08-trial2-fix1-final-target cargo test --offline --release --all-targets
    CARGO_TARGET_DIR=/private/tmp/bogkit-sim-2026-08-08-trial2-fix1-final-target cargo clippy --offline --all-targets --all-features -- -D warnings
    ```

    Observed: formatting passed with no output; the library passed 1/1 test and
    acceptance passed 15/15, zero failures; strict Clippy finished with no
    warnings.

16. Focused blocker regressions:

    ```console
    CARGO_TARGET_DIR=/private/tmp/bogkit-sim-2026-08-08-trial2-fix1-final-target cargo test --offline --release --test acceptance reused_output -- --nocapture --test-threads=1
    CARGO_TARGET_DIR=/private/tmp/bogkit-sim-2026-08-08-trial2-fix1-final-target cargo test --offline --release --lib publication_tests::injected_atomic_write_failure_leaves_no_current_or_partial_artifact -- --exact --nocapture
    ```

    Observed: 5/5 reused-output regressions passed (review→success,
    success→review, success→malformed, success→zero-timeout, and
    success→injected replay rejection). The injected pre-rename write failure
    passed 1/1 and left no canonical or temporary artifact.

17. Focused correctness/evidence regressions:

    ```console
    CARGO_TARGET_DIR=/private/tmp/bogkit-sim-2026-08-08-trial2-fix1-final-target cargo test --offline --release --test acceptance exact_reviewer_witness_is_a_safe_feasible_false_negative -- --exact --nocapture
    CARGO_TARGET_DIR=/private/tmp/bogkit-sim-2026-08-08-trial2-fix1-final-target cargo test --offline --release --test acceptance verifier_rejects_dishonest_metadata_and_explanations -- --exact --nocapture
    CARGO_TARGET_DIR=/private/tmp/bogkit-sim-2026-08-08-trial2-fix1-final-target cargo test --offline --release --test acceptance review_output_is_byte_deterministic -- --exact --nocapture
    ```

    Observed: each passed 1/1. The exact three-stack case returns review from the
    heuristic while its manual `X1 A→C`, `X2 A→B`, `P A→pickup_lane` witness
    passes full verification. Eleven metadata/explanation corruption variants
    were rejected. Five repeated review serializations were byte-identical.

18. Exact dishonest reviewer file through the CLI:

    ```console
    CARGO_TARGET_DIR=/private/tmp/bogkit-sim-2026-08-08-trial2-fix1-final-target cargo run --offline --release -- verify /private/tmp/bogkit-review-adversarial/t2/input/yard.json /private/tmp/bogkit-review-adversarial/t2/input/pickups-feasible.json /private/tmp/bogkit-review-adversarial/t2/input/dishonest-metadata-moves.json
    ```

    Observed exit code 1 and `error: status must be executable`.

19. Final runnable demo and valid file verification:

    ```console
    CARGO_TARGET_DIR=/private/tmp/bogkit-sim-2026-08-08-trial2-fix1-final-target cargo run --offline --release -- demo
    CARGO_TARGET_DIR=/private/tmp/bogkit-sim-2026-08-08-trial2-fix1-final-target cargo run --offline --release -- verify /private/tmp/bogkit-trial2-demo/input/yard.json /private/tmp/bogkit-trial2-demo/input/pickups.json /private/tmp/bogkit-trial2-demo/output/moves.json
    ```

    Observed demo: 3 baseline relocations, 2 proposed relocations, integer 33%
    for that one micro-geometry, replay verified, deterministic true. Valid file
    verification reported `verified 4 legal moves and their executable metadata`.

20. The child `[workspace]` was intentionally retained for the temporary
    out-of-member lab path. This was lab packaging, not BogKit API friction;
    the archive coordinator removed it and the child lockfile before the final
    nested-workspace gates.

21. Removed only generated trial targets and demo fixtures, then verified no
    target directory exists in the deliverable and no named fixture remains:

    ```console
    rm -rf /private/tmp/bogkit-sim-2026-08-08-trial2-target /private/tmp/bogkit-sim-2026-08-08-trial2-fix1-target /private/tmp/bogkit-sim-2026-08-08-trial2-fix1-final-target /private/tmp/bogkit-trial2-demo
    find simulation-output/container-yard-planner -type d -name target -print
    ```

    Observed: cleanup succeeded and the archive target search printed nothing.
