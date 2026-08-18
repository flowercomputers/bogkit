# Trial 2 repaired report — deterministic freight-capacity clearing

Date: 2026-08-18
Repair round: 1 of 5
Detached checkout: `/private/tmp/bogkit-sim-2026-08-18-trial2.2S4LeW`
BogKit HEAD throughout: `20f2ca50d5d06f51edfe8b8570c0fb48caf9eb81`
Review source: `/private/tmp/bogkit-sim-2026-08-18-review.Qji3Ki/review.md`

## Repaired outcome

The original safe-Rust advisory prototype remains at `simulation-output/freight-clearing/`. Repair round 1 closes the four Important Trial 2 defects reproduced by the skeptical reviewer and normalizes the Minor report issue. Each code repair began with a permanent failing regression in `tests/reviewer_repairs.rs`, then passed its focused GREEN run and the complete suite.

The workload-specific decision remains **no fit** for Fold, ESE, and ANNy. The review agreed with that decision; its rejection concerned prototype and report quality, not pressure to adopt a component.

This handoff is repaired and locally verified, but it is not self-approved for archival. The same skeptical reviewer must recheck the frozen repaired tree.

## Repair summary

| Reviewer finding | Category | Prior severity | Current severity | Repair state |
|---|---|---|---|---|
| T2-I1 proposal can overwrite an input | correctness defect | Important | None | Repaired; reviewer recheck pending |
| T2-I2 relative output reports a post-publication failure as nonpublication | correctness defect | Important | None | Repaired; reviewer recheck pending |
| T2-I3 4 KiB check follows unbounded line buffering | performance problem | Important | None | Repaired; reviewer recheck pending |
| T2-I4 required reference-integrity fields are optional | correctness defect | Important | None | Repaired; reviewer recheck pending |
| T2-M1 finding severities/resolved states are not normalized | documentation gap | Minor | None | Repaired; reviewer recheck pending |

The earlier global duplicate-proposal memory issue is also normalized below as prior `Important`, current `None`. Evidence that was not supplied remains in Evidence limits and is not counted as a product finding.

## What changed

### T2-I1 — Input alias protection

- `run_files` rejects a proposal path that resolves to the orders or manifest input before temporary-file creation.
- Existing output identity is compared where supported; the Unix implementation compares device and inode, catching hard links as well as symlinks/resolved names.
- Permanent tests cover the ordinary orders alias, a lexically different resolved manifest alias, and a hard link to orders. The orders and manifest digests remain unchanged.

### T2-I2 — Publication lifecycle and relative paths

- A proposal with no explicit parent uses `.`.
- The proposal directory is opened before temporary-file creation and its handle is retained across rename for directory sync.
- `RunErrorState::NotPublished` identifies errors before rename, displays with `NOT_PUBLISHED:`, and retains the previous final bytes.
- `RunErrorState::PublishedDurabilityUncertain` displays with `PUBLISHED_DURABILITY_UNCERTAIN:` and identifies an error after the complete proposal was renamed but before its directory entry was durably synced. It does not claim the prior file was preserved.
- Permanent tests cover relative CLI publication, pre-rename preservation, and injected post-rename durability uncertainty with the new complete proposal already visible.

### T2-I3 — Bounded line reading

- `read_until` was replaced with a `fill_buf`/`consume` loop whose retained buffer is capped at `MAX_LINE_BYTES + 1`.
- A one-MiB corrupt-line regression exposes the whole source slice but proves the validator consumes no more than 4,097 bytes before rejection. The old implementation consumed all 1,048,577 bytes.

### T2-I4 — Required evidence integrity

- `run_files` and therefore the CLI require `orders_sha256` and `expected_gross_value_cents`.
- The digest must be exactly 64 hexadecimal characters before the order file is hashed and compared.
- Missing digest, null digest, short digest, nonhex digest, missing expected value, and null expected value have permanent nonpublication regressions.
- `validate_orders` remains reusable for narrow unit tests with optional reference fields; the relaxation cannot reach the evidence-producing `run_files` boundary.

### Adjacent lifecycle audit

- Alias checks cover ordinary, resolved/symlink, and existing hard-link identity cases.
- The parent directory is opened before a sibling temporary file can appear.
- Pre-rename errors remove the sibling temporary file. Post-rename uncertainty does not misreport nonpublication; the temporary name no longer exists because it became the complete final proposal.
- A missing output parent fails before temporary-file creation or rename.
- Digest/value validation occurs before orders are parsed, matched, or any proposal temporary file is created.

## Test-driven repair evidence

Focused RED observations before implementation:

1. Ordinary orders alias, resolved manifest alias, and hard-link alias all returned success and replaced the input identity.
2. Relative CLI output exited nonzero after `proposal.ndjson` had appeared.
3. The post-rename injected state did not exist and initially returned success.
4. The one-MiB corrupt line consumed all 1,048,577 bytes before `line too long`.
5. Missing/null digest and missing/null expected gross were accepted; malformed digest was reported only as a value mismatch.

Focused GREEN runs then passed, and `cargo test --offline --locked` passed all retained suites: 33 integration tests plus unit/doc targets. This includes:

- 12 skeptical-review repair regressions.
- 40 literal golden markets.
- 10,000 generated small markets compared fill-for-fill with an independently structured slow scanner.
- Partial fills, one order spanning counterparties, equal-price and bytewise-ID ties, noncrossing markets, exact exhaustion, extrema, checked gross value, and the fill cap.
- Full validation, invalid UTF-8, truncated NDJSON, invalid dates/sides/sequences/values, duplicate IDs, bounded overlong input, inconsistent counts, and hand-written over-cap output.
- Failure injection after validation, 1 percent and 50 percent of markets, temporary sync, and rename-before-directory-sync.
- Forced read/write errors, relative publication, input aliases, deterministic shuffles, repeats, and the real CLI.

The named supplied corpus of 80 negative fixtures was not present. The retained cases are not relabelled as that corpus.

## Repaired full-shape evidence

Host: macOS, not the declared two-core Linux machine. Each seed permuted the same 650,000 orders across 5,000 nonempty markets.

| Shuffle seed | Validate/clear/publish ms | Proposal SHA-256 |
|---:|---:|---|
| 1 | 1,019 | `e7ce11984ca3e04197df9dd43013361b123e5ecf4b877f3a36f0a08dd64ebcad` |
| 2 | 1,080 | same |
| 3 | 1,030 | same |
| 4 | 1,040 | same |
| 5 | 1,025 | same |
| 6 | 1,001 | same |
| 7 | 1,059 | same |
| 8 | 983 | same |
| 9 | 1,038 | same |
| 10 | 1,053 | same |

Selected repaired prototype times before reporting: 983 ms, 1,038 ms, and 1,053 ms; median 1,038 ms. Three additional clears of the seed-10 source emitted the identical digest.

- Orders: 650,000, comprising 400,000 buys and 250,000 sells.
- Markets: 5,000.
- Fills: 498,521, below 645,000.
- Self-generated checked gross value: 312,220,517,981,477,594 cents.
- Proposal size: 80,960,196 bytes, below 160 MiB.
- Maximum sibling temporary bytes: 80,960,196, below 1 GiB.
- Maximum scratch files open: one proposal temporary.
- Fresh repaired clear wall time under `/usr/bin/time -l`: 1.03 seconds.
- Fresh repaired clear peak RSS: 445,087,744 bytes, below 512 MiB on this host.
- The digest, counts, gross value, and proposal size did not change after repair.

Machine output reports `measured_acceptance_pass:null` on macOS. `verified_gates_pass:true` covers only locally measured count/time/size gates and is not an overall acceptance claim. The self-generated manifest value does not replace the unavailable authoritative Ruby result.

## Exact final verification commands

From `simulation-output/freight-clearing/`:

```console
cargo test --offline --locked
# exit 0; all retained targets and 33 integration tests passed

cargo fmt --all -- --check
# exit 0

cargo clippy --offline --locked --all-targets --all-features -- -D warnings -D clippy::all -D clippy::pedantic
# exit 0; no warnings

cargo build --release --offline --locked
# exit 0

target/release/freight-clearing benchmark /private/tmp/bogkit-trial2-repair-bench.rcl7jr 5000 400000 250000 SEED
# exit 0 for SEED 1 through 10; digest and measurements above

target/release/freight-clearing clear /private/tmp/bogkit-trial2-repair-bench.rcl7jr/orders.ndjson /private/tmp/bogkit-trial2-repair-bench.rcl7jr/manifest.json /private/tmp/bogkit-trial2-repair-bench.rcl7jr/proposal.ndjson
# exit 0 for three repeats; identical digest

/usr/bin/time -l target/release/freight-clearing clear /private/tmp/bogkit-trial2-repair-bench.rcl7jr/orders.ndjson /private/tmp/bogkit-trial2-repair-bench.rcl7jr/manifest.json /private/tmp/bogkit-trial2-repair-bench.rcl7jr/proposal.ndjson
# exit 0; 1.03 real seconds; 445087744 maximum resident set size
```

## Component fit and decision audit

- **Fold: considered, not used.** A persistent fjall-backed incremental stream would add state and scratch lifecycle while leaving validation, exact two-sided sorting/matching, conservation, integrity verification, and canonical file publication custom.
- **ESE: considered, not used.** Text embeddings do not participate in exact integer price-time clearing.
- **ANNy: considered, not used.** Approximate nearest-neighbor search conflicts with the exact authoritative priority order.
- The skeptical repairs concern the prototype's file and evidence boundaries. They do not reproduce a BogKit defect or justify a BogKit source/example change.
- The program uses safe Rust only and launches no subprocess. It does not book, reserve, charge, notify, connect to a service, or alter the source snapshot.
- No tracked repository file, commit, GitHub state, or automation state was changed.

## Normalized findings

### T2-I1 — Proposal/input aliasing

- Category: **correctness defect**
- Prior severity: **Important**
- Current severity: **None**
- Confidence: high
- Repair state: repaired locally; reviewer recheck pending.
- Retained reproduction: `tests/reviewer_repairs.rs` tests `proposal_equal_to_orders_is_rejected_without_changing_orders`, `resolved_proposal_equal_to_manifest_is_rejected_without_changing_manifest`, and `existing_hard_link_to_orders_is_rejected_by_file_identity`.
- Smallest improvement: implemented resolved-path and existing-file identity rejection before temporary output.

### T2-I2 — Publication state and relative output

- Category: **correctness defect**
- Prior severity: **Important**
- Current severity: **None**
- Confidence: high
- Repair state: repaired locally; reviewer recheck pending.
- Retained reproduction: `relative_proposal_path_publishes_successfully`, `pre_rename_failure_reports_not_published_and_preserves_previous_bytes`, and `post_rename_failure_reports_durability_uncertainty_not_nonpublication`.
- Smallest improvement: implemented `.` normalization, a retained directory handle, and distinct pre/post-rename error states.

### T2-I3 — Unbounded overlong-line buffering

- Category: **performance problem**
- Prior severity: **Important**
- Current severity: **None**
- Confidence: high
- Repair state: repaired locally; reviewer recheck pending.
- Retained reproduction: `one_mib_overlong_line_is_rejected_without_consuming_beyond_the_bound`.
- Smallest improvement: implemented bounded `fill_buf` reading capped at 4,097 retained/consumed bytes.

### T2-I4 — Optional reference-integrity fields

- Category: **correctness defect**
- Prior severity: **Important**
- Current severity: **None**
- Confidence: high
- Repair state: repaired locally; reviewer recheck pending.
- Retained reproduction: the six missing/null/short/nonhex digest and missing/null gross cases in `tests/reviewer_repairs.rs`.
- Smallest improvement: implemented required, syntactically validated digest and required gross value at `run_files`/CLI.

### T2-M1 — Unnormalized findings

- Category: **documentation gap**
- Prior severity: **Minor**
- Current severity: **None**
- Confidence: high
- Repair state: repaired locally; reviewer recheck pending.
- Retained reproduction: this report uses only Critical/Important/Minor/None for severity and records repair state separately.
- Smallest improvement: normalized every retained finding and moved missing supplied evidence out of findings.

### T2-P1 — Duplicate global proposal retained during validation

- Category: **performance problem**
- Prior severity: **Important**
- Current severity: **None**
- Confidence: high
- Repair state: repaired before initial skeptical review and retained.
- Retained reproduction: full-shape clear-only `/usr/bin/time -l`; initial 652,476,416 bytes, market-at-a-time validation now 445,087,744 bytes on the repaired tree.
- Smallest improvement: retain only one market's expected fills during validation.

The workload-specific `no_fit` conclusion is a decision, not an unresolved product finding.

## Evidence limits

- The production Ruby evaluator, its authoritative outputs, three same-host Ruby timings, supplied 80-fixture negative corpus, and declared two-core Linux runner were unavailable.
- The local slow Rust oracle and self-generated manifest do not establish Ruby byte parity or the one-third runtime criterion.
- macOS timing and RSS are host observations, not Linux qualification or a production capacity claim.
- Injected returned errors model lifecycle points but are not process kills, disk-full, permission races, or power-loss qualification.
- Post-rename directory-sync failure means a complete new final file is visible with durability uncertainty; it does not mean the prior file was preserved.

## Cleanup, repository state, and frozen identity

The full benchmark directory and crate `target/` are removed before handoff. Only Rust source/tests, Cargo metadata, README, discovery, repaired report, and the retained tree-identity manifest remain under `simulation-output/`. No corpus, proposal, database, temporary file, binary, secret-like file, symlink, or file above 1 MiB remains.

Final HEAD is `20f2ca50d5d06f51edfe8b8570c0fb48caf9eb81`. `git diff --exit-code` is clean; `git status --short` contains only untracked `BRIEF.md` and `simulation-output/`. The stable retained-file hashes are recorded in `simulation-output/TREE_IDENTITY.sha256` for reviewer comparison.
