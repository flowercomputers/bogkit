# Consequential decision audit

## Decisions taken

| Decision | Evidence used | Consequence | Reversibility |
|---|---|---|---|
| Use no BogKit component in the core prototype. | Public README and all four public examples; immutable batch and exact-money constraints. | The result tests product fit honestly and avoids approximate or persistence-oriented machinery in the posting decision. | Easy: a later adapter can feed validated events into Fold without changing output contracts. |
| Reproduce the stated staged greedy behavior as a separate command. | Scenario baseline description and fix-round reference-order regressions. | Exact reference runs first; fallback runs only when no reference candidate is feasible. This remains an authored comparator, not the production Java/SQL executable. | Easy. |
| Treat the highest revision for a claim-line ID as current. | Revised-claim pain and current-open-snapshot premise. | Prevents posting to obsolete revisions. | Requires an explicit upstream current-revision marker if real exports use different semantics. |
| Require exact insurer reference or complete exact fallback identity. | Precision threshold and prohibition on guessing. | Missing fallback fields go to review; unique exact references can still match. | Threshold/fact policy is isolated in candidate generation. |
| Resolve a connected cluster as a whole. | Greedy consumption and mutually inconsistent split/reversal cases. | Later stronger matches and shared capacities influence the joint answer. | Core algorithm choice; output contract remains stable. |
| Support only single links, multiple remittances to one line, and same-claim splits equal to a subset of full open balances. | Explicit prototype boundary plus skeptical-review partial-split counterexample. | Cross-claim bundles are `unsupported_bundle`; partial same-claim allocations are `unsupported_partial_split`. | Expand only with an authoritative allocation rule and independent truth. |
| Support reversals only when offset by same-snapshot positive allocation. | No prior-applied authority exists in the schema. | Standalone reversals are reviewed as `unsupported_standalone_reversal`, not mislabeled as amount errors. | Add authoritative prior-applied capacity before broadening. |
| Quarantine all physical logical duplicates before indexing. | Duplicate remittance and claim-key counterexamples. | Identical and divergent duplicates cannot create decisions or capacity; remittance review ordinals account for every physical row. | The duplicate policy can change only with an explicit idempotency contract. |
| Intersect nonempty exact-reference and complete-fallback sets before generating any allocation. | Disjoint two-claim counterexample and partial-overlap `{A,B}` / `{B,C}` counterexample. | Every accepted single or split allocation is supported by both available identity sources; an infeasible disagreement is reviewed as `conflicting_identity_sources`. | Candidate provenance remains available if policy evolves. |
| Review equal-best solutions and exhausted searches. | Uncertainty-as-review requirement. | Precision is favored over speculative recall; pathological clusters are bounded. | Search budget and tie policy are constants. |
| Keep runtime out of `summary.json`. | Byte-identical shuffle requirement. | Result files contain only deterministic values; timing stays in command output/evidence. | Easy. |
| Emit IDs, physical ordinals, and enumerated codes only. | Privacy-safe diagnostics requirement. | Explanations remain machine-checkable without copying patient keys; verifier scans both input sources against serialized outputs. | Output schema can add other non-identifying codes later. |
| Cap fallback date ranges at 31 days and quarantine longer ranges. | Need to avoid an unbounded range scan without silently truncating it. | Long-range fallback records are reviewed as `unsupported_date_range`; exact external semantics remain a future choice. | Constant/policy change with tests. |
| Keep all generated fixtures, result files, and build output outside the archive. | Archive-cleanliness requirement. | Deliverable contains only source, lockfile, scripts, README, and compact evidence. | Not applicable. |

## Rejected alternatives

- **Fold as the reconciliation engine:** rejected because materialized incremental views do not themselves choose a globally consistent bounded assignment. Adding persistence to a read-only snapshot would expand the prototype without meeting the core acceptance criteria.
- **ESE or ANNy candidate generation:** rejected because approximate semantic proximity is neither necessary nor safe for exact normalized fields and integer money.
- **Keep greedy matching and add a score threshold:** rejected because a locally strong first choice can still consume capacity needed by a later stronger choice.
- **Auto-select a deterministic winner for a tied optimum:** rejected because input-order independence is not the same as correctness; a stable guess is still a guess.
- **Generic many-to-many min-cost flow:** rejected because the scenario explicitly excludes arbitrary many-to-many settlement and same-claim split rules need domain-specific option generation. It would also add a dependency for a bounded prototype.
- **Database, network, ML, or X12 integration:** rejected as explicit non-goals and unnecessary risk.
- **Write generated fixtures or result archives into the deliverable:** rejected to keep the archive self-contained and clean.

## Dependencies

- Rust toolchain; recorded with Rust 1.95.0.
- `serde` 1.0.229 and `serde_json` 1.0.150 plus their transitive crates, resolved
  by the lab's single nested workspace lockfile after archival.
- Standard shell utilities `mktemp`, `cmp`, and `rm` for the acceptance script only.
- No BogKit crate, database, network service, medical terminology service, or production integration.

## Uncertainty and untested claims

- The externally described 50,000-line fixture, verifier-only truth, and ten supplied seeds were not present. The recorded full workload comes from this crate's deterministic authored generator and truth; it cannot prove production prevalence, performance, or accuracy.
- The current-revision rule assumes the highest revision number is authoritative inside one immutable snapshot. A real export should state this invariant explicitly.
- Adjustment and reversal signs are normalized to a signed `paid_cents + adjustment_cents`; raw X12 CAS semantics are outside scope.
- Peak RSS was not captured because the sandbox denied a `sysctl` requested by `/usr/bin/time -l`. Source inspection shows one reused raw line buffer cleared at the next read boundary; this is not measured erasure or a measured memory ceiling.
- The runtime was measured in this environment, not on a separately characterized four-core laptop. The implementation is single-threaded, so it does not depend on more than one core.
- Dense adversarial clusters can exhaust the deterministic search-node budget and will be reviewed. This is intentional safety behavior, but its recall effect on an unavailable real fixture is unknown.
- Duplicate remittance IDs are not modeled by the 50,000-row generator. This fix chooses the conservative contract that every physical duplicate is invalid, including byte-identical rows; dedicated adversarial tests cover that policy.
- The simulator's temporary `[workspace]` and member `Cargo.lock` were removed
  during archival. Locked metadata, tests, Clippy, and archive validation are
  recorded in the dated lab report.
