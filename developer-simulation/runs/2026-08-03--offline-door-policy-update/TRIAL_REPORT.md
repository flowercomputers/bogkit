# Blind trial report: offline door policy update

## Result

**Decision: explicit no-fit for BogKit on the door controller.**

Fold is useful for one narrow part of the problem: it can apply grant retractions and a policy-version record in one logical transaction, and it did so quickly in the host probe. It does not expose the storage and recovery controls that define this controller problem: a fixed 16 MiB flash image, bounded working memory, signed/truncation-safe bundle intake, and failure injection after each modeled 4 KiB write. ESE and ANNy have no relationship to exact authorization policy lookup.

The runnable probe is intentionally a fit test, not a claim that the acceptance contract has been implemented. It demonstrates the useful portion and makes the missing portions observable.

## Perspective and scope

I approached the repository as a building-access systems developer who is new to Rust and had no prior BogKit knowledge. I read the root `README.md` first, then all four public examples. I did not inspect earlier simulations, lab reports, another checkout, GitHub, automation state, or internet resources.

Starting revision: `80fd3c9`.

Finishing criteria were:

1. Evaluate the existing nightly-file baseline before selecting a component.
2. Select only a BogKit component with a plausible connection to the problem.
3. Produce a runnable minimal prototype or failure reproducer under `trial-output`.
4. Check formatting, tests, strict lint, representative correctness, timing, memory, storage, restart, and version-order behavior.
5. Preserve every untested acceptance item as an explicit limitation.

## Baseline evaluation

The current complete-nightly-file design is not safe if it overwrites the active sorted table in place. The probe models an 8 KiB old generation, writes the first 4 KiB block of the new generation, and then stops. The result is neither the complete old generation nor the complete new generation:

```text
baseline_mixed_after_first_4k_write=true
```

A controller-specific design could stage a complete image in a second slot and switch a redundant activation record only after verification. That would solve mixed generations for snapshots, but it would not solve frequent emergency deltas, contiguous-version enforcement, signatures, or bounded delta recovery by itself. The baseline therefore justifies a change, but it does not make a general incremental database automatically suitable.

## Ordered discovery and friction trail

1. The root README describes Fold as an incremental programming framework whose materialized views update as data changes. It describes ESE as embeddings and ANNy as approximate nearest-neighbor search.
2. The `starter` example showed the relevant idea: a batch of inserts/removals commits atomically and can be reopened from a directory path.
3. The `timeseries` example showed incremental retractions, but no fixed-storage or fault model. `chat` and `search` were unrelated to exact offline authorization.
4. Fold's public crate documentation says its state is an embedded Fjall LSM store. `Stream::new` opens a path as a database directory, transactions are logical and crash-safe, and `checkpoint` performs a filesystem synchronization for OS/power durability.
5. That justified a narrow probe: use `KeyedStream` for credential/door grants, store the active version in the same transaction, and keep version-contiguity checks in controller application code.
6. The first `cargo test` attempted to update the package index and failed because the host could not resolve `index.crates.io`. No network access was used. Repeating with `--offline` succeeded from the existing cache.
7. Opening a pre-created 16 MiB file as a Fold database panicked inside `Stream::new`; the probe catches this and reports `fixed_16mib_image_rejected=true`. Fold expects a filesystem directory, not the required flash image/block interface.
8. The logical prototype passed generated authorization comparisons, version-order cases, and restart. Its host performance was fast.
9. Skeptical review found that a contiguous next version with the wrong
   `based_on` value was mislabeled as a version gap and rendered the impossible
   range `missing=2..1`. The coordinator separated base mismatch from missing
   versions, added a no-mutation/reopen regression, and reran all evidence.
10. In the final nested workspace, the open store had 14 files and a
    67,115,332-byte logical extent, of which 3,129,344 bytes were physically
    allocated on this sparse-file-capable host. After a clean close it had 11
    files, 3,072,778 logical bytes, and 3,104,768 allocated bytes. The layout
    relies on host filesystem behavior and is not a fixed 16 MiB image.
11. Three final runs reported 25,526,272-26,820,608 bytes whole-process peak
    resident memory. This includes the reference model, generated bundles,
    runtime, code, and database mappings, so the 4 MiB controller working-memory
    requirement remains not demonstrated rather than attributed to Fold.

## Component fit audit

| Component | Possible value | Decision |
|---|---|---|
| Fold | Atomic grant retractions, persisted keyed lookup, consistent version update | Rejected for controller use: filesystem/LSM persistence is not the fixed flash protocol; signed/truncated intake, bounded-memory evidence, and 4 KiB fault controls remain outside the probe |
| ESE | Static text embeddings | No fit: authorization is exact structured lookup, not semantic similarity |
| ANNy | Approximate vector search | No fit: approximate results are unacceptable and unrelated to badge/door/time checks |

Using Fold only in the central compiler would not address the problem either: PostgreSQL remains authoritative, and the requested prototype value lies in controller image/recovery behavior. Adding Fold there would duplicate state without proving a required property.

## Prototype

Files:

- `Cargo.toml`: nested-workspace member with a local Fold dependency.
- `src/main.rs`: baseline mixed-generation reproducer, versioned grant
  controller, reference comparer, regular-file-path rejection probe, timing,
  storage measurement, and restart check.
- `README.md`: exact reproduction using caller-supplied state under
  `/private/tmp`; tests and demos do not generate state inside the archive.

The probe uses one simulated controller for door 7. Version 1 contains 60,000
time-bounded grants. Version 2 revokes 50,000. It compares 20,000 deterministic
badge/door/time queries before and 20,000 after against a straightforward
`BTreeMap` reference. It then rejects a next-version wrong-base bundle without
mutation, ignores the same active version without verifying payload identity,
rejects an older snapshot, rejects version 4 before version 3, and applies the
version 3 repair followed by version 4. Version and last-verified time are
persisted with the grants in the same Fold transaction. A rejected gap's
diagnostic range remains process-local and is not claimed to survive restart.

This is deliberately not a signed-bundle parser, flash emulator, or exhaustive power-loss harness. Those are the capabilities whose absence drives the no-fit decision.

## Representative acceptance evidence

| Acceptance item | Evidence | Result |
|---|---|---|
| Queries match reference | 20,000 before + 20,000 after; zero mismatches | Pass in host probe |
| Old bundle cannot restore activated revocations | Version 1 after active version 2 returned `RejectedOld`; badge 0 stayed denied | Pass in application layer |
| Same active version ignored | Repeated version 2 returned `SameVersionIgnoredUnverified`; payload identity/authenticity is absent | Narrow pass in application layer |
| Wrong base rejected | Version 2 based on version 0 returned `BaseMismatch`; policy/status stayed at version 1 and the grant survived reopen | Pass after reviewer-required fix |
| Gap rejected and contiguous repair accepted | Version 4 returned missing 3; version 3 then version 4 activated | Pass in application layer |
| Deterministic status | Gap status was deterministic in-process; final/reopened active status was version 4, time 1030, no missing versions | Narrow pass; rejected-gap diagnostic is volatile |
| Restart preserves active policy | Reopen reported version 4 and authorized the repaired temporary grant at its valid time | Pass after clean checkpoint |
| 50,000 changes under 2 seconds | Three final runs applied in 48-51 ms and checkpointed in 4-5 ms | Pass for unsigned host transaction only |
| Signed and truncated bundles | The probe has no envelope/parser/signature implementation | Not implemented; application-specific boundary |
| Complete old or new at every 4 KiB power cut | Baseline mixing reproduced; Fold API does not expose each physical 4 KiB write or an injectable block device | Not demonstrated / missing capability |
| Peak working memory below 4 MiB | Whole probe RSS 25,526,272-26,820,608 bytes; not isolated to controller state or Fold | Not demonstrated |
| Active + recovery within 16 MiB fixed image | Fixed file rejected. Open directory: 67,115,332 logical / 3,129,344 allocated bytes. Closed: 3,072,778 logical / 3,104,768 allocated bytes | Required storage model not supported |

## Exact commands and observed results

Final commands ran from the repository root after this crate joined the nested
`developer-simulation` workspace and resolved its shared lock. Generated state
and build output stayed under `/private/tmp`.

```console
cargo fmt --manifest-path developer-simulation/Cargo.toml \
  -p offline-door-policy-fit-probe -- --check
CARGO_TARGET_DIR=/private/tmp/bogkit-sim-final-target \
  cargo test --manifest-path developer-simulation/Cargo.toml \
  -p offline-door-policy-fit-probe --offline --locked
CARGO_TARGET_DIR=/private/tmp/bogkit-sim-final-target \
  cargo clippy --manifest-path developer-simulation/Cargo.toml \
  -p offline-door-policy-fit-probe --all-targets --offline --locked -- -D warnings
CARGO_TARGET_DIR=/private/tmp/bogkit-sim-final-target \
  cargo build --manifest-path developer-simulation/Cargo.toml \
  -p offline-door-policy-fit-probe --release --offline --locked

DOOR_BIN=/private/tmp/bogkit-sim-final-target/release/offline-door-policy-fit-probe
for RUN_NUMBER in 1 2 3; do
  DOOR_ROOT="$(mktemp -d /private/tmp/offline-door-final-${RUN_NUMBER}.XXXXXX)"
  "$DOOR_BIN" "$DOOR_ROOT"
done
```

Formatting and strict Clippy passed. Both tests passed, including the
reviewer-required wrong-base/no-mutation/reopen regression. Each of the three
release demonstrations printed the same decisions and storage figures. One
representative decision/status sequence was:

```text
baseline_mixed_after_first_4k_write=true
fixed_16mib_image_rejected=true
queries_before=20000 mismatches=0
queries_after=20000 mismatches=0
wrong_base=BaseMismatch { expected_base: 1, received_base: 0 } wrong_base_status=active_version=1 last_verified_at=1000 missing=none wrong_base_unchanged=true
same_version=SameVersionIgnoredUnverified old=RejectedOld gap=Missing { expected: 3, received: 4 }
gap_status=active_version=2 last_verified_at=1010 missing=3..3
repair=Activated after_repair=Activated
final_status=active_version=4 last_verified_at=1030 missing=none
reopened_status=active_version=4 last_verified_at=1030 missing=none reopened_query=true
fold_store_open_files=14 open_logical_bytes=67115332 open_allocated_bytes=3129344
fold_store_closed_files=11 closed_logical_bytes=3072778 closed_allocated_bytes=3104768 policy_limit_bytes=16777216
signature_verification=not_provided_by_fold
truncated_bundle_detection=not_provided_by_fold
power_cut_at_each_4k_write=not_injectable_through_fold_api
```

Across the three final nested-workspace runs, the 60,000-entry snapshot applied
in 49-63 ms, 50,000 revocations applied in 48-51 ms, checkpoint took 4-5 ms,
and whole-probe RSS was 25,526,272-26,820,608 bytes. These are host observations
for this full harness, not controller-hardware or Fold-only bounds.

## Categorized findings

### F-0 — Wrong-base bundle was mislabeled, fixed after review

- Category: **prototype correctness defect**
- Severity: **high**
- Confidence: **high**
- Reproduction: after active version 1, submit version 2 with `based_on=0`.
  The original probe returned `Missing { expected: 2, received: 2 }` and
  rendered `missing=2..1`.
- Fix and regression: the corrected application layer returns `BaseMismatch`,
  keeps the policy and coherent status unchanged, and proves the grant survives
  reopen in `contiguous_version_with_wrong_base_is_rejected_without_mutation`.
- Smallest improvement: keep base mismatch distinct from a missing version and
  fail closed without applying changes.

This was a defect in the trial prototype, not in Fold.

### F-1 — Fixed-image storage mismatch

- Category: **poor product fit**
- Severity: **critical**
- Confidence: **high**
- Reproduction: run the release demonstration; it pre-creates a 16 MiB file and attempts `Stream::new` on it. Output is `fixed_16mib_image_rejected=true`.
- Impact: the required simulator must control every byte and every 4 KiB flash write. Fold opens a directory-backed Fjall database instead.
- Smallest improvement: document the fixed/raw-flash non-goal and use a
  purpose-built fixed-extent controller format; do not infer a new BogKit
  storage engine from this trial.

### F-2 — No controllable 4 KiB recovery boundary

- Category: **missing capability**
- Severity: **critical**
- Confidence: **high**
- Reproduction: Fold exposes a logical transaction and `checkpoint`, but no write-plan enumeration, raw block backend, dual-root activation record, or power-cut injection point. The demo can only report `power_cut_at_each_4k_write=not_injectable_through_fold_api`.
- Impact: “crash-safe” at the host filesystem layer cannot establish the required claim that every modeled flash cut yields exactly the old or new policy.
- Smallest improvement: build the controller-specific recovery harness around a
  purpose-built block format and retain this boundary in the public capability
  matrix.

### F-3 — No trusted bundle boundary

- Category: **missing capability**
- Severity: **high**
- Confidence: **high**
- Reproduction: the public component API accepts typed inserts/removals; it has no bundle framing, length validation, signature verification, monotonic sequence envelope, or snapshot/delta distinction.
- Impact: untrusted, duplicated, skipped, reordered, or truncated network input must be solved entirely outside BogKit before a transaction begins.
- Smallest improvement: implement a streaming, signed application envelope that
  verifies header, payload length/hash/signature, controller identity, base
  version, and target version before yielding changes. This one-trial need does
  not justify a BogKit subsystem candidate.

### F-4 — Working-memory limit not demonstrated

- Category: **performance problem**
- Severity: **high**
- Confidence: **medium**
- Reproduction: three final release demonstrations reported whole-process peak
  RSS of 25,526,272-26,820,608 bytes against a 4,194,304-byte target.
- Impact: the controller memory contract is not demonstrated.
- Caveat: RSS includes the reference `BTreeMap`, both generated bundles, Rust runtime, and database; it does not isolate Fold's peak. The conclusion is limited to this implementation, not a precise Fold-only byte count.
- Smallest improvement: provide a streaming transaction interface with a documented fixed memory bound plus allocator/storage telemetry that separates engine memory from the harness.

### F-5 — Regular-file database path causes a documented open panic

- Category: **API friction**
- Severity: **high**
- Confidence: **high**
- Reproduction: opening the pre-created regular 16 MiB file as the database path
  panics; the public constructor documents that store-open failure can panic.
  The probe does not test general corruption or every unopenable-store case.
- Impact: an embedded policy controller needs a recoverable, diagnosable status
  for an incompatible storage region, not process termination.
- Smallest improvement: return a typed `Result` from database construction and checkpoint operations.

### F-6 — Durability boundary is easy to overread

- Category: **documentation gap**
- Severity: **medium**
- Confidence: **high**
- Reproduction: the root-level messaging emphasizes durable, atomic transactions. The more precise Fold API documentation says `wtx` is process-crash durable while `checkpoint` additionally hardens against OS/power failure. Neither defines guarantees for torn physical 4 KiB flash writes or a capacity bound.
- Impact: a new user could mistake logical transaction atomicity for the controller's physical power-loss acceptance criterion.
- Smallest improvement: document the exact durability layers, unsupported raw-flash cases, expected directory/file behavior, storage amplification, and whether embedded/no-std targets are supported.

No Fold correctness defect was demonstrated. The successful logical results should not be relabeled as an engine bug merely because the product boundary does not fit.

## Decision audit

1. **Why not keep the baseline?** Direct in-place full-file replacement demonstrably mixes generations after one 4 KiB write. Dual slots would make snapshots safer but do not provide efficient frequent deltas or the complete delivery protocol.
2. **Why consider Fold?** Grant add/revoke operations and version metadata form a transactional keyed-state update, matching Fold's advertised strength.
3. **What did Fold prove?** Exact lookup matched the reference; retractions and
   version metadata committed together; corrected wrong-base/old/gap rules
   implemented above Fold behaved correctly; the same active version was ignored
   without verifying payload identity; 50,000 retractions were fast on the host.
4. **Why reject it?** The dominant requirements are below Fold's public abstraction: fixed flash extent, bounded memory, signed/truncated input, explicit two-generation activation, and exhaustive 4 KiB power-fault recovery.
5. **Could surrounding code fill the gaps?** Yes, but that surrounding code would contain nearly the entire safety-critical controller design. Fold would then add a host filesystem LSM that the fixed-image prototype cannot use, so the integration cost has no demonstrated payoff.
6. **Final choice:** use no BogKit component in the controller prototype. Build a purpose-specific fixed-image format with redundant generation metadata, a staged inactive region, streaming verification, monotonic contiguous version checks, and an activation record designed for torn-write recovery. Keep PostgreSQL as the authoritative compiler input as required.

## Uncertainties and intentionally unproven items

- The brief does not give a maximum per-controller grant count. The probe used 60,000 grants so one controller could receive the complete 50,000-revocation batch; this may be more concentrated than production.
- Host timing does not include real signature verification or bundle parsing and is not a hardware performance prediction.
- Whole-process RSS is an upper bound for this host prototype, not an isolated engine-memory profile.
- The open store's large logical extent is sparse on this filesystem. Physical allocation was only about 3.1 MiB, but sparse host files do not establish compatibility with a fixed raw-flash image.
- Only clean checkpoint/reopen was tested. No claim is made about the required old-or-new result at each physical write cut.
- Missing-version status in this minimal probe is deterministic within the
  current process but reopens as `missing=none`; a production controller would
  need a specified persisted diagnostic policy.
- Same-version delivery is ignored without proving that its payload matches the
  active bundle. Bundle identity and authenticity remain outside the probe.
- Signature choice, key provisioning, bundle wire format, wear limits, flash erase geometry, and hardware-specific atomic-write size remain outside this no-fit reproducer.
