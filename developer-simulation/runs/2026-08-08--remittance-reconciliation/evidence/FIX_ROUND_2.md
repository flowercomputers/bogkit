# Skeptical-review fix round 2/5

Date: 2026-08-08

## Remaining B1 correction

Round one stopped only fully disjoint exact-reference and complete-fallback candidate sets. That allowed a partial-overlap case to score through the contradiction: reference `{A,B}`, fallback `{B,C}`, shared `B` underfunded, and one-source `A` or `C` feasible.

The reconciler now applies this policy before building any assignment option:

- If only one candidate source is nonempty, that source remains eligible.
- If both candidate sources are nonempty, the eligible list is their intersection.
- Single-line and same-claim split enumeration receive only that eligible list, so they cannot reintroduce a one-source candidate.
- When the sources differ and no assignment from their intersection is selected, the stable review reason is `conflicting_identity_sources`.
- When the sources fully agree, ordinary shape-specific reasons such as `unsupported_partial_split` remain accurate.

## Adversarial coverage

Four new integration regressions cover:

1. The exact reviewer shape `{A,B}` / `{B,C}` with `B=50` and a 100-cent remittance. `A` differs in patient/date/procedure, `C` has another reference, and either could fund the remittance alone. The result reviews with one eligible candidate and never accepts `A` or `C`.
2. Reversed claim order, shuffled claim order, mixed unrelated remittance order, byte-identical canonical outputs, a passing verifier, and acceptance of the unrelated cluster.
3. An agreement control where shared `B=100`, which accepts only `B`, plus exact-only and fallback-only controls that remain accepted.
4. A split safeguard where two reference-only lines from the same claim could jointly fund the remittance. Because split generation sees only shared `B`, it reviews instead of reconstructing the one-source split.

The permanent synthetic CLI reproducer is under `tests/fixtures/overlapping-identity/`.

## Exact commands and observed results

Format, all tests, and strict lint:

```console
CARGO_NET_OFFLINE=true CARGO_TARGET_DIR=/private/tmp/bogkit-fix2-target cargo fmt --all -- --check
CARGO_NET_OFFLINE=true CARGO_TARGET_DIR=/private/tmp/bogkit-fix2-target cargo test --all-targets --locked
CARGO_NET_OFFLINE=true CARGO_TARGET_DIR=/private/tmp/bogkit-fix2-target cargo clippy --all-targets --all-features --locked -- -D warnings
```

Observed after the final source change: formatting passed; 6 library unit tests and 17 adversarial integration tests passed, the binary target had 0 tests, so 23 passed and 0 failed total; Clippy passed with warnings denied.

Focused CLI reproduction and privacy/verifier scan:

```console
CARGO_NET_OFFLINE=true CARGO_TARGET_DIR=/private/tmp/bogkit-fix2-target cargo run --release --quiet --locked -- reconcile --claims tests/fixtures/overlapping-identity/claims.jsonl --remittances tests/fixtures/overlapping-identity/remittances.jsonl --out /private/tmp/bogkit-fix2-cli.SGcGJo/results
CARGO_NET_OFFLINE=true CARGO_TARGET_DIR=/private/tmp/bogkit-fix2-target cargo run --release --quiet --locked -- verify --claims tests/fixtures/overlapping-identity/claims.jsonl --remittances tests/fixtures/overlapping-identity/remittances.jsonl --ground-truth tests/fixtures/overlapping-identity/ground-truth.jsonl --results /private/tmp/bogkit-fix2-cli.SGcGJo/results
```

Observed: 2 remittances produced 1 accepted row and 1 review. The verifier passed at 1/1 correct link, precision 1.0, recall 1.0, and 0 invariant/privacy failures. `R-OK` accepted `C-OK`; `R-OVERLAP` reviewed as `conflicting_identity_sources` with `candidate_count: 1`.

Demo:

```console
CARGO_NET_OFFLINE=true CARGO_TARGET_DIR=/private/tmp/bogkit-fix2-target cargo run --release --quiet --locked -- demo --work-dir /private/tmp/bogkit-fix2-demo.qisCTT
```

Observed:

```text
demo improved: accepted=183 review=17 precision=100.0000% recall=100.0000% passed=true
demo greedy baseline: accepted=171 review=29 precision=71.9298% recall=63.0769% passed=false
```

Full deterministic authored acceptance and ten shuffles:

```console
CARGO_NET_OFFLINE=true CARGO_TARGET_DIR=/private/tmp/bogkit-fix2-target ./scripts/run-acceptance.sh
```

Observed: generated 62,000 claim rows and 50,000 remittance rows. Ordered reconciliation took 4.705s and emitted 49,983 accepted remittance IDs, 17 reviews, and 49,995 links. The authored verifier reported 49,995/49,995 correct links, 100% generator-only precision/recall, and 0 invariant/privacy failures. The staged comparator emitted 49,971 links in 0.256s, with 99.903944% precision, 99.855986% recall, and 12 obsolete-revision invariant failures. Seeds `1 2 3 5 8 13 21 34 55 89` produced byte-identical `accepted.jsonl`, `review.jsonl`, and `summary.json`; reconciliation times were 4.823–5.195s.

## Remaining concerns

- The external fixture, independent truth, supplied seed files, specified laptop, and peak-RSS measurement remain unavailable.
- The 100% result applies only to the deterministic authored generator and its authored truth.
- Maximum revision, 31-day fallback range, full-balance-only splits, and same-snapshot reversal authority remain explicit prototype policies.
- The temporary child `[workspace]` and local `Cargo.lock` remain for the assigned out-of-member fix. The coordinator must remove both during the nested-workspace archive transform, use the daily workspace lockfile, and rerun its archive gates.
