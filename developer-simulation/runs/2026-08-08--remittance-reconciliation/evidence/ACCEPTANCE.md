# Acceptance evidence

Recorded on 2026-08-08 with Rust 1.95.0. Generated fixtures and build outputs were kept in `/private/tmp` and removed after the final checks.

| Check | Observed result | Status |
|---|---|---|
| Deterministic generator size | 62,000 claim records; 50,000 remittance records | Pass |
| Generator-only precision | 49,995 / 49,995 accepted links correct against authored truth = 100% | Pass, narrow claim only |
| Generator-only recall | 49,995 / 49,995 authored unambiguous truth links found = 100% | Pass, narrow claim only |
| Generator safety invariants | 0 duplicate, over-application, reversal-sign, cross-claim split, or cent-conservation failures | Pass |
| Contradictory identity regressions | Disjoint sets review; partially overlapping `{A,B}` / `{B,C}` sets expose only shared `B`; underfunded `B` reviews in reversed/shuffled mixed clusters with byte-identical output; feasible `B` and one-source controls accept; one-source split candidates stay excluded | Pass |
| Duplicate remittance regressions | Identical and divergent rows, adjacent/separated/reordered, all physical rows reviewed with ordinals; unrelated cluster accepted | Pass |
| Duplicate claim-key regressions | Identical and divergent rows, adjacent/separated/reordered, no capacity created; related remittances reviewed and unrelated cluster accepted | Pass |
| Comparator staging | Unique reference wins regardless order; duplicate references remain greedy within reference stage; missing/unacceptable reference falls back | Pass |
| Narrow settlement scope | Partial same-claim split reviewed as `unsupported_partial_split`; standalone reversal reviewed as `unsupported_standalone_reversal` | Pass |
| Dense exhaustion | Public end-to-end 12-claim/12-remittance cluster exhausted the node budget and emitted 12 `search_budget_exhausted` reviews | Pass |
| Review partition | 17 reviewed remittance lines: 12 ambiguous, 4 unsupported cross-claim bundles, 1 malformed | Pass |
| Shuffle stability | Seeds 1, 2, 3, 5, 8, 13, 21, 34, 55, 89 all byte-identical for all three outputs | Pass |
| Runtime | Final round-two ordered run 4.705s; shuffled runs 4.823s to 5.195s | Pass against 60s limit on this environment/generator |
| Explanation/privacy | Every accepted link had facts and competitor reasons; verifier scanned claim and remittance patient keys, including a remittance-only secret, against serialized outputs | Pass |
| Malformed isolation | One malformed claim and one malformed remittance were quarantined; 49,983 unrelated remittance lines still accepted | Pass |
| Greedy baseline | 99.903944% precision, 99.855986% recall, 12 obsolete-revision integrity failures | Fails required precision and invariants |
| Actual external fixture | No external fixture, hidden truth, or supplied seed files were present | Not tested |
| Peak resident memory | `/usr/bin/time -l` completed the app in 4.591s but then failed on sandboxed `sysctl kern.clockrate` | Not measured |

Exact improved verifier output is preserved in `verification-report.json`; the comparator is in `baseline-report.json`.

## Authored remittance-shape counts

| Shape | Physical remittance rows |
|---|---:|
| Bulk unique exact reference | 49,851 |
| Greedy capacity trap | 24 |
| Same-claim split equal to full-balance subset | 12 |
| Multiple payments to one claim line | 24 |
| Revised claim line | 12 |
| Duplicate insurer reference, resolved by agreeing identity | 12 |
| Same-snapshot paired payment and reversal | 24 |
| Denial | 12 |
| Exact reference with missing optional fields | 12 |
| Deliberately ambiguous | 12 |
| Unsupported cross-claim bundle | 4 |
| Malformed row with remittance-only privacy key | 1 |

These counts describe generator composition, not production prevalence. B1 disjoint and partial-overlap conflicts, B2 duplicates, partial splits, standalone reversals, comparator staging, and dense search exhaustion are covered by dedicated adversarial tests rather than hidden inside bulk volume.
