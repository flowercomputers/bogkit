# Conservative remittance-to-claim reconciliation prototype

This is an offline Rust command-line prototype for matching normalized remittance service lines to the current revisions of open claim service lines without silently guessing. It uses integer cents, resolves each bounded candidate cluster as a whole, sends ties and unsupported shapes to review, and writes deterministic JSONL results.

All included fixture generation is synthetic. The program has no database or network integration and does not auto-post anything.

## BogKit fit decision

The prototype deliberately does not depend on Fold, ESE, or ANNy.

- Fold's public examples center on durable, incrementally maintained views. This job receives one immutable nightly snapshot and needs a bounded global assignment before emitting any decision. Persistence and incremental retraction do not solve that core constraint.
- ESE and ANNy provide semantic similarity and nearest-neighbor search. Approximate similarity is the wrong safety primitive for exact identifiers, integer-money conservation, and a policy that turns uncertainty into review work.

Fold could become useful outside this prototype for durable ingestion or audit views if the surrounding system becomes incremental. In this one trial, the public BogKit surface did not provide the exact constrained assignment needed here. That is a one-off fit observation, not a BogKit defect or feature candidate.

## What the prototype does

1. Parses each JSONL record into an owned typed value while reusing one line buffer. Source inspection confirms buffer reuse and clearing at the next read boundary; this is not a claim of measured memory erasure.
2. Quarantines malformed rows, every physical row sharing a duplicate remittance ID, and every physical claim row sharing a `(claim_line_id, revision)` key before indexing. Identical duplicates are invalid too; they are not treated as idempotent capacity.
3. Keeps only the highest revision for each claim-line ID.
4. Builds separate exact-reference and complete-fallback-identity candidate sets. When both are nonempty, only their intersection may be allocated, for both single-line and split options. If the sources disagree and their shared candidates cannot fund a complete assignment, the remittance is reviewed as `conflicting_identity_sources`; an additive score never authorizes a one-source candidate.
5. Finds connected candidate clusters, capped at 24 claim plus remittance nodes.
6. Enumerates one-to-one, multiple-payments-to-one-line, and same-claim splits only when the remittance exactly equals a subset of full current open balances. Partial line allocation is not implemented and is reviewed as `unsupported_partial_split`.
7. Searches the cluster as a whole under claim capacity, sign, and cent-conservation rules. The search has a deterministic one-million-node ceiling; clusters that exceed it are reviewed as unsupported instead of running without a bound.
8. Reviews equal-best assignments as `conflicting_strong_candidates`. Standalone reversals are reviewed as `unsupported_standalone_reversal` because this input has no authoritative prior-applied amount; only reversals offset by a positive same-snapshot allocation are supported.
9. Writes canonically sorted `accepted.jsonl`, `review.jsonl`, and `summary.json`.

Accepted explanations contain record IDs and normalized fact/reason codes only. They never copy a patient key.

## Build and quick demonstration

Rust 1.95.0 was used for the recorded run. The only external crates are
`serde` and `serde_json`. The simulator's temporary child workspace marker and
lockfile were removed during archival; this package now uses the lab's single
`developer-simulation/Cargo.lock`.

```sh
cargo build --release --locked
cargo run --release --locked -- demo --work-dir /tmp/remittance-demo
```

The demo creates 400 claim records and 200 remittance records, runs both solvers, and verifies the safer solver. Its output remains under the work directory named in the command.

If crates.io is unavailable but the locked dependencies are already cached, prefix Cargo commands with `CARGO_NET_OFFLINE=true`.

## Exact full acceptance run

The script builds in a temporary directory, generates exactly 62,000 claim records and 50,000 remittance records, runs and verifies the cluster solver, measures the greedy comparator, checks ten fixed shuffle seeds, and removes all generated fixture/build/result data when it exits.

```sh
./scripts/run-acceptance.sh
```

The ten seeds are `1 2 3 5 8 13 21 34 55 89` because no externally supplied seed files accompanied the scenario.

Run the source checks separately:

```sh
cargo fmt -p remittance-reconciliation -- --check
cargo test --offline --locked -p remittance-reconciliation --all-targets
cargo clippy --offline --locked -p remittance-reconciliation --all-targets --all-features -- -D warnings
```

The focused overlapping-identity regression is also runnable directly:

```sh
cargo run --release --locked -- reconcile \
  --claims tests/fixtures/overlapping-identity/claims.jsonl \
  --remittances tests/fixtures/overlapping-identity/remittances.jsonl \
  --out /tmp/remittance-overlap-results
cargo run --release --locked -- verify \
  --claims tests/fixtures/overlapping-identity/claims.jsonl \
  --remittances tests/fixtures/overlapping-identity/remittances.jsonl \
  --ground-truth tests/fixtures/overlapping-identity/ground-truth.jsonl \
  --results /tmp/remittance-overlap-results
```

It deliberately presents exact-reference candidates `{A,B}` and fallback candidates `{B,C}`. The shared `B` has only 50 cents of open capacity for a 100-cent remittance, while `A` and `C` could each fund it alone. Expected behavior is `conflicting_identity_sources` review for that row and acceptance of the unrelated control row.

## Manual commands

```sh
remittance-reconciliation generate \
  --out /tmp/remittance-fixture \
  --claim-count 62000 \
  --remittance-count 50000

remittance-reconciliation reconcile \
  --claims /tmp/remittance-fixture/claims.jsonl \
  --remittances /tmp/remittance-fixture/remittances.jsonl \
  --out /tmp/remittance-results

remittance-reconciliation verify \
  --claims /tmp/remittance-fixture/claims.jsonl \
  --remittances /tmp/remittance-fixture/remittances.jsonl \
  --ground-truth /tmp/remittance-fixture/ground-truth.jsonl \
  --results /tmp/remittance-results
```

`baseline` faithfully stages exact insurer-reference candidates first and chooses the first feasible one in input order. Complete fallback identity runs only when that reference stage has no feasible candidate; greediness remains within each stage. It does not support splits. `shuffle` deterministically reorders both inputs for stability testing. Run `remittance-reconciliation help` for their flags.

## Input schema

Dates are integer day numbers and money is signed integer cents. Reversals must have a negative `paid_cents + adjustment_cents`; payments and denials must have a positive total. Date ranges longer than 31 days are quarantined as `unsupported_date_range` instead of partially searched. A valid standalone reversal still requires prior-posting authority that this schema does not contain, so it is reviewed rather than accepted.

Claim records contain:

```json
{"claim_line_id":"C-001","claim_id":"CLAIM-001","revision":2,"payer":"payer-00","provider":"provider-00","patient_key":"synthetic-key-001","service_date":20000,"procedure_code":"PROC-001","billed_cents":500,"open_balance_cents":500,"insurer_references":["REF-001"]}
```

Remittance records contain:

```json
{"remittance_line_id":"R-001","payer":"payer-00","provider":"provider-00","insurer_reference":"REF-001","patient_key":"synthetic-key-001","service_date_start":20000,"service_date_end":20000,"procedure_code":"PROC-001","paid_cents":500,"adjustment_cents":0,"adjustment_codes":[],"transaction_kind":"payment"}
```

## Outputs

- `accepted.jsonl`: one record per accepted allocation. A same-claim split has multiple records with the same remittance-line ID. Each record lists contributing fact codes and rejected candidate IDs with reason codes.
- `review.jsonl`: exactly one record for every non-accepted physical remittance row. `physical_record_ordinal` distinguishes duplicate physical rows deterministically without copying source fields.
- `summary.json`: deterministic counts only; runtime is intentionally excluded so shuffled runs remain byte-identical.

Stable review codes include `missing_identity`, `conflicting_identity_sources`, `conflicting_strong_candidates`, `duplicate_remittance_id`, `duplicate_claim_key`, `amount_inconsistent`, `unsupported_bundle`, `unsupported_partial_split`, `unsupported_standalone_reversal`, `unsupported_date_range`, `search_budget_exhausted`, `no_plausible_candidate`, and `malformed_record`.

## Evidence and limitations

The skeptical-review trail is in `evidence/`. Accuracy figures apply only to this crate's deterministic authored generator and matching authored truth; they are not production accuracy or independent evaluation evidence. The 50,000 remittance rows comprise 49,851 unique-reference bulk rows plus: 24 greedy-trap rows, 12 full-balance-subset split rows, 24 multiple-payment rows, 12 revision rows, 12 duplicate-reference rows, 24 paired payment/reversal rows, 12 denial rows, 12 missing-optional-field rows, 12 deliberately ambiguous rows, 4 unsupported cross-claim bundles, and 1 malformed row carrying a remittance-only privacy secret.

The actual externally described evaluation fixture, its hidden ground truth, supplied seed files, and specified laptop were not present, so claims about those artifacts remain untested. Peak resident memory was not captured because the sandbox denied `/usr/bin/time -l` access to `sysctl`. Raw-line handling is source-audited buffer reuse, not measured erasure. The verifier scans claim and remittance patient keys—including the remittance-only secret—against serialized outputs. Medical adjustment semantics are intentionally normalized and do not substitute for X12 adjudication rules.
