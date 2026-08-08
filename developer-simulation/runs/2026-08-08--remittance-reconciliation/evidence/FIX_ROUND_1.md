# Skeptical-review fix round 1/5

Date: 2026-08-08

Correction from round 2: this round proved only the fully disjoint identity-source case. Partial overlap still exposed one-source candidates to optimization. `FIX_ROUND_2.md` records the complete intersection-only correction and supersedes the B1 completion claim below; the B2 and other round-one evidence remains current.

## Blocker work

### B1: contradictory strong identity sources (partial fix, superseded)

Candidate generation now retains two sets: exact insurer-reference claims and complete payer/provider/patient/date/procedure fallback claims. If both sets are nonempty and disjoint, the remittance is excluded from optimization and reviewed as `conflicting_identity_sources`. Scores are never used to cross that contradiction.

Regression `conflicting_identity_sources_review_in_all_orders_while_agreement_accepts` runs the exact two-claim conflict in three claim/remittance orders and an agreement control. All conflicting outputs are byte-identical review decisions; the control accepts; the independent verifier passes.

### B2: duplicate logical identities

Before any active index is built:

- Every physical remittance row sharing an ID is invalid, including byte-identical rows. No row is accepted. `review.jsonl` contains ordinals `1..N`, each with `duplicate_remittance_id`.
- Every physical claim row sharing `(claim_line_id, revision)` is invalid, including byte-identical rows. All are excluded from active/current capacity. A remittance matching the quarantined rows is reviewed as `duplicate_claim_key`.

Regressions cover identical and divergent duplicates, adjacent/separated/reordered inputs, byte-identical outputs, exact invalid physical-row counts, and unrelated valid clusters that continue to accept. Two negative verifier regressions prove that manually accepting either a duplicate remittance identity or duplicated claim capacity is rejected.

## Important fixes and claim corrections

- The comparator now runs exact-reference candidates first and invokes fallback only if no reference candidate is feasible. Tests cover unique reference in both orders, duplicate reference in both orders, missing reference, and an exact-reference candidate with insufficient capacity.
- Split support is explicitly limited to same-claim subsets of entire current open balances. A partial-line case is reviewed as `unsupported_partial_split`.
- Standalone reversal support is explicitly absent because no prior-applied authority exists. It is reviewed as `unsupported_standalone_reversal`; paired same-snapshot payment/reversal cases remain exact-cent accepted.
- Generator accuracy is labeled generator-only. Per-shape counts show that 49,851 of 50,000 rows are bulk unique-reference rows.
- Privacy verification now extracts patient keys from both claim and remittance inputs, including a remittance-only secret on the malformed generated row, and compares them with string values parsed from the serialized outputs.
- Raw-line behavior is described as source-audited reuse of one buffer cleared at the next read boundary, not erasure or a measured memory property.
- The internal dense-search budget test is joined by a public end-to-end 12-claim/12-remittance dense cluster that produces 12 deterministic `search_budget_exhausted` review rows.
- Rust `E0282` is recategorized as authoring history. The child workspace is temporary lab packaging. Exact assignment is one one-off capability observation, not a BogKit defect or feature candidate.
- The temporary child `[workspace]` and local `Cargo.lock` remain only because this fix runs in the assigned out-of-member directory. The coordinator must remove both when copying into the nested daily workspace and rerun its metadata/test/Clippy/archive gates.

## Exact commands and observed results

Focused and package tests:

```console
CARGO_NET_OFFLINE=true CARGO_TARGET_DIR=/private/tmp/bogkit-fix1-target cargo test --all-targets --locked
```

Observed after the final adversarial additions: 6 library unit tests passed, 13 integration adversarial tests passed, binary target had 0 tests; 19 total passed, 0 failed. Dense end-to-end exhaustion completed inside the test run.

Strict lint:

```console
CARGO_NET_OFFLINE=true CARGO_TARGET_DIR=/private/tmp/bogkit-fix1-target cargo clippy --all-targets --all-features --locked -- -D warnings
```

Observed: passed with warnings denied. This command is rerun at the final gate after all source changes.

Demo:

```console
CARGO_NET_OFFLINE=true CARGO_TARGET_DIR=/private/tmp/bogkit-fix1-target cargo run --release --quiet --locked -- demo --work-dir /private/tmp/bogkit-fix1-demo.TrLEFy
```

Observed:

```text
demo improved: accepted=183 review=17 precision=100.0000% recall=100.0000% passed=true
demo greedy baseline: accepted=171 review=29 precision=71.9298% recall=63.0769% passed=false
```

Full-size generator and ten shuffles:

```console
CARGO_NET_OFFLINE=true ./scripts/run-acceptance.sh
```

Observed in the final rerun: generated 62,000 claim and 50,000 remittance physical rows; ordered reconciliation took 4.614s and emitted 49,983 accepted remittance IDs, 17 reviews, and 49,995 links. The authored verifier reported 49,995/49,995 correct links, 100% generator-only precision/recall, and zero invariant failures. The corrected staged comparator emitted 49,971 links in 0.242s and retained the same authored metrics: 99.903944% precision, 99.855986% recall, and 12 obsolete-revision failures. All three canonical files were byte-identical for seeds `1 2 3 5 8 13 21 34 55 89`; reconciliation times were 4.639–5.091s.

## Remaining concerns

- External fixture, verifier-only truth, supplied seed files, and specified laptop remain unavailable.
- Generator and truth share authorship; 100% is not independent or production accuracy.
- Peak RSS remains unmeasured.
- Maximum numeric revision, 31-day fallback range, full-balance-only splits, and same-snapshot-only reversal authority remain consequential prototype policies.
- Nested-workspace archive compatibility is intentionally deferred to the coordinator transform described above.
