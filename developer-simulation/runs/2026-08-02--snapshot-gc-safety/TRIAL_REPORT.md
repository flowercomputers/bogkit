# Snapshot GC Safety Trial Report

## Outcome

After skeptical-review correction, the prototype met every prototype acceptance check at the requested scale. Planning
selected all 51,000 eligible unreachable blobs and zero referenced blobs. A committed
truncated or malformed record stopped plan/apply before quarantine and named the file
and record. Cooperative publication during planning and from an existing quarantine
preserved the newly referenced blob. Subprocess exits after every tested quarantine,
finalization, and phase-marker boundary resumed to completion without losing live
blobs. Repeating plan, apply, and resume was harmless.

Skeptical review reproduced a serious same-name publication race: two cooperative
publishers could both pass the pre-lock existence check, both report success, and the
second rename could replace the first manifest. The final-name check now occurs while
holding the publication lock, temporary output is cleaned on rejection, and a forced-
contention regression requires exactly one publisher to succeed.

The production integration requirement is explicit: manifest publishers must use the
same advisory publication lock and temporary-file rename protocol demonstrated by the
`publish` command. No collector can close the check-to-rename race against an entirely
uncooperative POSIX writer. Integrating that protocol into the daemon was a stated
non-goal, so it is not attempted here.

## Ordered discovery and friction trail

1. Read the public root `README.md`. It described Fold as an incremental materialized
   view engine and directed new users to the public examples.
2. Read the public starter, time-series, chat, and search examples in that order. The
   useful concepts were atomic transactions and consistent snapshots; none addressed
   ownership of external manifest files, publication fencing, quarantine, or bounded
   external sorting.
3. Wrote and ran `baseline.py` before inspecting Fold internals. On 100,000 records it
   loaded 25,000 unique strings and directly deleted 5,000 blobs in 0.36 seconds. The
   file was then frozen at the SHA-256 recorded in the README.
4. Inspected the workspace manifest and Fold's public crate documentation. Fold stores
   application-ingested deltas in an embedded LSM database. Using it would require a
   second durable copy of external manifest state and would not provide the missing
   publisher/quarantine protocol.
5. Chose no BogKit component. Implemented a small standalone Rust package using only
   the standard library plus `serde_json` for correct parsing.
6. The first online Cargo check could not resolve `index.crates.io`. The dependency was
   already locally available, so all subsequent builds and checks used `--offline`.
7. Strict lint found two ambiguous file-opening/extension checks; both were corrected.
8. The first small demonstration showed per-object directory flushes were unnecessarily
   slow. Phase markers retain durability and the state is derivable after process exit,
   so directory flushes were moved to phase commits. The crash matrix was rerun after
   that change.
9. Ran the 30-repository oracle matrix, malformed guards, concurrency cases, crash
   matrix, idempotence checks, and the full million-record fixture.
10. Skeptical review reproduced the full workload and then forced two same-name
    publishers to wait behind the same lock. Both originally reported success and the
    later rename replaced the earlier manifest. The existence check was moved under
    the lock, cleanup was added, and the acceptance harness now requires one success
    and one existing-name failure.
11. Review also found that Python baseline memory varied materially across runs. The
    direct-deletion and publication-race failures remain proven, but the memory result
    is now reported as inadequate and variable headroom rather than a stable breach.

## Exact validation and observed results

All commands ran from the `trial-output` directory. Generated Rust output was directed
to `/private/tmp/snapshot-gc-target-trial-b`.

```text
cargo fmt --all -- --check
  PASS

cargo test --all-targets --offline
  3 passed; 0 failed

cargo clippy --all-targets --offline -- -D warnings
  PASS; zero warnings

python3 acceptance.py /private/tmp/snapshot-gc-target-trial-b/release/snapshot-gc-safety
  acceptance: 30/30 oracle repos, malformed guards, concurrent publication,
  all crash boundaries, idempotence: PASS
```

The post-fix acceptance includes a forced same-name contention regression that
observed exactly one successful publisher and one existing-name failure.

Requested-scale fixture creation (developer run):

```text
fixture: 10000 manifests, 1000000 references, 250000 unique,
300000 inventory, 1000 missing, 51000 unreachable
wall_seconds=17.410, peak_rss_bytes=1,982,464
manifest corpus logical bytes=76,000,000
```

Requested-scale post-fix plan:

```text
planned 51000 candidates from 1000000 records and 300000 blobs in 1.475s
measured wall_seconds=1.485
peak_rss_bytes=6,160,384
peak_scratch_bytes=38,376,656
```

The corrected plan therefore remained comfortably inside the 90-second, 128 MiB,
and manifest-corpus scratch limits on the measured host.

Requested-scale independent oracle and mutations:

```text
oracle match: all 51000 eligible unreachable blobs selected;
zero referenced blobs selected
oracle: independently rechecked all 51,000 candidates

apply realistic: quarantined 51000 blobs in 6.864s
resume realistic: removed 51000, restored 0 in 2.963s
status: complete
```

The oracle intentionally uses an independent in-memory `HashSet`; its memory is not
part of the collector's planning bound.

Frozen Python baseline on the same requested-scale Rust-generated fixture:

```text
loaded 250000 unique references; directly removed 51000 blobs
wall_seconds=8.938
peak_rss_bytes=183,648,256
```

The developer observed one 183,648,256-byte run. Skeptical review reproduced highly
variable one-host results from 124,436,480 to 133,971,968 bytes; the largest was only
245,760 bytes below 128 MiB. This does not establish a stable breach, but it leaves
inadequate production headroom. Independently of memory, the baseline has no
recoverable quarantine or publisher fence and directly deletes candidates.

## Categorized findings

### Evidence: Python reference-set memory is variable with inadequate headroom

- Severity: medium.
- Confidence: high that headroom is inadequate on the measured host; low that it
  always breaches the threshold.
- Reproduction: generate the default Rust fixture, then run
  `python3 measure.py -- python3 baseline.py collect <fixture>`.
- Smallest improvement: replace the in-memory string set with fixed-width external
  chunk sorting and a streaming merge, and repeat under the production runtime.

### Evidence: direct deletion has a publication race

- Severity: critical for data safety.
- Confidence: high; the baseline freezes the manifest list, then directly unlinks.
- Reproduction: publish a committed manifest after its manifest enumeration and before
  its inventory deletion.
- Smallest improvement: make publishers write temporary files and take the shared
  publication lock for blob validation/resurrection plus the final manifest rename.

### Prototype correctness defect, fixed: same-name publishers could both succeed

- Severity: critical for append-only publication safety.
- Confidence: high; skeptical review forced the contention and observed the first
  successful manifest being replaced before the fix.
- Reproduction: run `acceptance.py`; its same-name case holds the publication lock
  until both publishers have created temporary files and then requires one success
  and one failure.
- Smallest improvement: check the final name while holding the shared publication
  lock, reject an existing destination, and clean the losing temporary file.

### Evidence: malformed committed input is fail-closed

- Severity: critical safeguard.
- Confidence: high; tested before both plan and apply mutations.
- Reproduction: `printf` a JSON object without its final newline into
  `manifests/bad.jsonl`, then run plan or apply.
- Smallest improvement: preserve the current file-and-record diagnostic in any daemon
  integration and alert on it.

### Evidence: crash recovery is derived from filesystem state

- Severity: high.
- Confidence: high for ordinary process termination; the harness injected exit code 86
  after each of four object mutations and the phase marker in both apply and resume.
- Reproduction: set `SNAPSHOT_GC_CRASH_AFTER=1` through `5`, rerun on a four-candidate
  fixture, then invoke resume.
- Smallest improvement: add power-loss testing on the production filesystem before
  claiming durability against kernel or hardware failure.

### Evidence: Fold is not a fit for this bounded prototype

- Severity: medium architecture decision.
- Confidence: high for the stated non-integration prototype.
- Reproduction: compare Fold's public stream/table persistence model with the required
  external manifest validation, bounded scratch, publication fence, and quarantine.
- Smallest improvement: no BogKit change is justified by this trial. Re-evaluate only
  if Fold gains a bounded external-set primitive that directly owns this protocol.

## Decision audit

- Chose fixed 32-byte binary hashes and 65,536-hash sort chunks. This bounds working
  memory and makes scratch smaller than the original JSONL corpus.
- Chose a two-step apply/resume flow. A normal successful apply leaves recoverable
  quarantine; resume revalidates live references immediately before final deletion.
- Chose an advisory file lock instead of a lock directory. The operating system releases
  a file lock when an injected crash exits, so resume never needs unsafe stale-lock theft.
- Chose strict final-newline validation. A partial last JSONL record is treated as a
  truncated committed manifest, not a record to ignore.
- Chose to rescan every committed manifest under the publication lock for apply and
  resume. A stale plan can over-select, but cannot move a hash that is referenced at
  apply time, and cannot finalize a hash referenced at resume time.
- Rejected Fold because it duplicates source state, consumes additional durable scratch,
  and does not solve the publication race.
- Rejected SQLite or another embedded database because it adds a dependency and a second
  data model when sorted fixed-width files are sufficient.
- Rejected probabilistic filters because zero false negatives are mandatory; false
  positives would also prevent selection of every eligible unreachable blob.
- Rejected a grace-period-only design because a new manifest can legitimately reference
  an old blob, so age alone cannot prove safety.
- Uncertainty: the prototype tests process exits, not sudden power loss. Directory syncs
  protect committed phase markers and publisher ordering, but production filesystems and
  mount options need a dedicated power-failure qualification.
- Uncertainty: non-cooperative writers can bypass advisory locks. The CLI refuses to
  publish a reference whose blob is neither live nor recoverable, but daemon integration
  is required to make every real publisher follow that rule.
- Uncertainty: evidence covers ordinary exits after completed filesystem operations,
  not interruption inside a syscall, kernel failure, or power loss. Production also
  needs same-filesystem rename, directory-sync, canonical lowercase filename, and
  case-sensitive-filesystem qualification.
- Uncertainty: concurrent planners sharing a plan name, publisher crash points,
  candidate-file corruption, and an input-independent memory bound remain untested.

## Files

- `Cargo.toml` and `Cargo.lock`: standalone package and reproducible dependency lock.
- `src/main.rs`: fixture, plan, apply, resume, status, publisher, oracle, bounded sorter,
  validation, and unit tests.
- `acceptance.py`: 30-seed oracle matrix and subprocess safety harness.
- `baseline.py`: frozen Python comparison.
- `measure.py`: wall-time, child RSS, and peak scratch measurement.
- `README.md`: exact reproduction and operating contract.
- `TRIAL_REPORT.md`: this evidence and decision audit.
