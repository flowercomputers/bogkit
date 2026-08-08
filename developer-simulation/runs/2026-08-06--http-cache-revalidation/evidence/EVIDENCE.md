# Trial evidence

Date: 2026-08-06
Checkout: `/private/tmp/bogkit-sim-2026-08-06-two-50bEAG`
Scope: this checkout only; no GitHub, other worktree, prior lab report, developer-simulation directory, or automation state was inspected or changed.

## Ordered discovery and friction trail

1. Confirmed the assigned checkout and located the public README:

   ```console
   $ pwd && rg --files -g 'README*' -g '!**/target/**' -g '!**/.git/**' | sort
   /private/tmp/bogkit-sim-2026-08-06-two-50bEAG
   README.md
   ```

   ```console
   $ sed -n '1,260p' README.md
   ```

   Observed: the documented entry point is `scripts/new-project.sh`; the
   workspace describes Fold, ESE, ANNy, and examples in the order `starter`,
   `timeseries`, `chat`, `search`.

2. Read each public example in that order with:

   ```console
   $ rg --files examples/<example> -g '!**/target/**' -g '!**/.git/**' | sort
   $ for f in $(rg --files examples/<example> -g '!**/target/**' -g '!**/.git/**' | sort); do case "$f" in *.rs|*/Cargo.toml|*.md|*.html|*.js|*.css|*.txt|*.json) echo "--- $f"; sed -n '1,420p' "$f";; esac; done
   ```

   Observed in order:

   - `starter`: one `Stream`, transactional writes, consistent reads, and
     `Count`/`Bag` materialized views.
   - `timeseries`: `KeyBy`, `Aggregate`, `Filter`, retraction, and custom
     accumulators over persisted views.
   - `chat`: a single ingest thread owns Fold and republishes snapshots; this
     is an explicit single-owner model, not a 32-worker lease model.
   - `search`: `KeyedStream` upsert/remove semantics and retraction-safe
     indexes, with ESE/ANNy as optional search support.

3. Confirmed the repository was clean before trial files:

   ```console
   $ git status --short
   ```

   Observed: no output.

4. Inspected only the public workspace manifest and Fold source after the
   onboarding examples:

   ```console
   $ sed -n '1,260p' Cargo.toml
   $ rg --files fold/src -g '*.rs' | sort
   $ rg -n "pub (struct|enum|fn|trait)|pub mod|struct Stream|struct KeyedStream|wtx|rtx|transaction|checkpoint|recover|retract|KeyBy|Aggregate|Table|Bag|Count" fold/src -g '*.rs' | head -n 320
   ```

   Observed: Fold has durable single-writer `wtx`, consistent `rtx`,
   `KeyedStream`, `Table`, `InvertedIndex`, `Ranked`, and `Retain`; `Retain`
   stamps records with a wall-clock processing time. No public API covered the
   separate body store, per-key lease, or purge sequence protocol.

5. Selected a no-fit runtime decision and created only a new trial crate:

   ```console
   $ mkdir -p developer-simulation/runs/2026-08-06--http-cache-revalidation/src
   ```

   The new crate has no dependency on BogKit. Existing core crates and examples
   were not modified.

6. Onboarding compilation checks, still in README order:

   ```console
   $ cargo check --locked -p starter
   ```

   Observed: failed in ESE's build script while trying to download
   `target/ese-cache/model.safetensors`; DNS lookup was unavailable.

   ```console
   $ cargo check --locked -p timeseries
   ```

   Observed: passed.

   ```console
   $ cargo check --locked -p chat
   ```

   Observed: passed.

   ```console
   $ cargo check --locked -p search
   ```

   Observed: failed at the same ESE model download/DNS step as `starter`.

7. First prototype validation pass:

   ```console
   $ cargo fmt --manifest-path developer-simulation/runs/2026-08-06--http-cache-revalidation/Cargo.toml
   $ cargo test --manifest-path developer-simulation/runs/2026-08-06--http-cache-revalidation/Cargo.toml
   ```

   Observed: the first compile caught a format-string arity error and Rust
   borrow-checker errors in mutable entry reads. After those were corrected,
   the first test run had two semantic failures: the baseline purge fixture
   used the expiration boundary incorrectly, and the quota fixture was too
   small to force eviction. Both were corrected and retested.

8. First warnings-denied lint pass:

   ```console
   $ cargo clippy --manifest-path developer-simulation/runs/2026-08-06--http-cache-revalidation/Cargo.toml --all-targets -- -D warnings
   ```

   Observed: Clippy first reported the SHA-256 padding modulo idiom, helper
   argument count, and an identical test branch. Those were corrected without
   weakening the lint command; the final run passed.

9. First end-to-end demo pass:

   ```console
   $ cargo run --quiet --manifest-path developer-simulation/runs/2026-08-06--http-cache-revalidation/Cargo.toml -- demo
   ```

   Observed: the first fixture attempt failed with `completion has no active
   lease` because its second crash request hit a newly fresh entry. The fixture
   was changed to use a separate key, then the demo passed.

10. Public Fold regression check:

    ```console
    $ cargo test --locked -p fold
    ```

    Observed: 18 unit tests and 9 doctests passed.

11. Workspace formatting check, intentionally read-only:

    ```console
    $ cargo fmt --all -- --check
    ```

    Observed: it reported pre-existing formatting differences in
    `examples/search/src/main.rs`. That existing example was not changed.
    Trial-only formatting passed separately.

## Final commands and observed results

```console
$ cargo fmt --manifest-path developer-simulation/runs/2026-08-06--http-cache-revalidation/Cargo.toml -- --check
```

Passed.

```console
$ cargo test --manifest-path developer-simulation/runs/2026-08-06--http-cache-revalidation/Cargo.toml
```

Passed: 12 unit tests, 0 failures; the binary and doctest targets also passed.

```console
$ cargo clippy --manifest-path developer-simulation/runs/2026-08-06--http-cache-revalidation/Cargo.toml --all-targets -- -D warnings
```

Passed with warnings denied.

```console
$ cargo run --quiet --manifest-path developer-simulation/runs/2026-08-06--http-cache-revalidation/Cargo.toml -- demo | rg '^(mode=baseline|summary |demo=)'
mode=baseline wrong_variant=true cross_tenant_collision=true duplicate_revalidations=2 purge_left_servable=true unverified_body_after_crash=true missing_body_after_crash=true
summary requests=8 fresh_hits=1 stale_responses=1 misses=3 revalidation_starts=6 revalidation_waits=1 purges_applied=1 purges_ignored=2 recovery_rollbacks=1 recovery_commits=2 quota_evictions=0 unsafe_body_serves=0 committed_usage_bytes=722 quota_bytes=1000000
demo=PASS modeled_invariants=true
```

```console
$ cargo run --quiet --manifest-path developer-simulation/runs/2026-08-06--http-cache-revalidation/Cargo.toml -- run developer-simulation/runs/2026-08-06--http-cache-revalidation/demo.trace | tail -n 6
```

Observed: the file-driven trace produced the same three final entries and the
same recovery/purge behavior as the embedded demo; output contained only
hashed dynamic identifiers.

```console
$ cargo run --quiet --manifest-path developer-simulation/runs/2026-08-06--http-cache-revalidation/Cargo.toml -- run developer-simulation/runs/2026-08-06--http-cache-revalidation/quota.trace | rg 'QUOTA_EVICT|summary|final_entry'
at=11 event=eviction reason=QUOTA_EVICT_LRU tenant_id=80a707af7dc77ee1228f9127180f3964835e5beb4c4ab0d812f0fe7593579b3a key_id=1acec891bd9223876cfe7ef8542648e84c28ae4ecab6c15562bbb6fc4fe58526 actor_id=- lease_id=- body_id=b8f5d4d2194b24418d533e3297074d2c453117f78ec56db4da20f93973a039c committed_usage_bytes=0
summary requests=1 fresh_hits=0 stale_responses=0 misses=0 revalidation_starts=1 revalidation_waits=0 purges_applied=0 purges_ignored=0 recovery_rollbacks=0 recovery_commits=1 quota_evictions=1 unsafe_body_serves=0 committed_usage_bytes=0 quota_bytes=300
```

```console
$ cargo build --release --manifest-path developer-simulation/runs/2026-08-06--http-cache-revalidation/Cargo.toml
```

Passed.

```console
$ developer-simulation/runs/2026-08-06--http-cache-revalidation/target/release/http-cache-revalidation workload
workload objects=2000000 requests=1000000 purges=100000 request_hits=0 request_misses=1000000 committed_usage_bytes=8640000000 quota_bytes=68719476736 within_quota=true max_rss_bytes=65634304 memory_limit_bytes=268435456 within_memory=true
```

Observed: the compact harness completed in about 0.19 seconds on this run,
used a logical 8.64 GB committed footprint under the 64 GiB quota, and measured
65,634,304 bytes maximum resident set size under the 256 MiB limit. This is a
shape check, not a production benchmark: it does not allocate body bytes or
store the full semantic trace.

The requested external command was also tried:

```console
$ /usr/bin/time -l developer-simulation/runs/2026-08-06--http-cache-revalidation/target/release/http-cache-revalidation workload
```

Observed: the workload result matched the line above; the sandboxed `/usr/bin/time`
could not query `kern.clockrate`, so the prototype's macOS `getrusage` value is
the recorded memory measurement.

```console
$ if cargo run --quiet --manifest-path developer-simulation/runs/2026-08-06--http-cache-revalidation/Cargo.toml -- demo | rg -n 'cache\.test|tenant-[a-z]|body-[a-z]|worker-[0-9]|article|profile|origin_down|etag-[a-z]'; then exit 1; else echo privacy_check=PASS_no_raw_fixture_identifiers; fi
privacy_check=PASS_no_raw_fixture_identifiers
```

The demo's stable reason-code extraction was:

```text
FRESH_HIT
MISS_ORIGIN_ERROR
MISS_REVALIDATION_STARTED
PURGE_APPLIED
PURGE_DUPLICATE_IGNORED
PURGE_REORDERED_IGNORED
RECOVERY_RETAIN_COMMITTED
RECOVERY_ROLLBACK_UNCOMMITTED
REVALIDATION_COMMITTED_200
REVALIDATION_REJECTED_PURGE
REVALIDATION_STARTED
REVALIDATION_WAIT
STALE_IF_ERROR
```

## Skeptical review fixes

The reviewer reproduced the compact semantic claims and found one correctness
bug plus evidence-scope problems. Before archival, the prototype was corrected
and all affected checks were rerun:

- A delayed purge with sequence 1 is now ignored after sequence 2 has been
  accepted, even if the entry was repopulated in between. The regression keeps
  the repopulated entry and distinguishes `PURGE_REORDERED_IGNORED` from a
  duplicate purge.
- The trace parser now rejects surplus fields and more than 16 distinct tags;
  it no longer silently truncates invalid trace input.
- The 32-worker lease result is described as sequential serialized calls in one
  process. There is no lease expiry, worker-loss recovery, distributed
  concurrency, or fencing proof.
- Body safety is described as an in-memory digest/size/reference model only.
  No body bytes, file deletion, SQLite transaction, fsync, rename, or process
  restart is tested, so the prototype does not claim body-file deletion safety.
- The 2M/1M/100K run is explicitly a compact memory/quota shape harness. It
  does not model the semantic object, tag-posting, lease, body, or purge state
  at that scale.

The corrected prototype remains a no-fit result for the acceptance-critical
cache boundary. No BogKit defect was demonstrated.

## Categorized findings

| Finding | Severity | Confidence | Reproduction | Smallest improvement |
| --- | --- | --- | --- | --- |
| Method+URL key collides across Vary variants and tenants | High | High | `demo` baseline line reports `wrong_variant=true` and `cross_tenant_collision=true` | Include tenant, normalized method/URL, and Vary fingerprint in the canonical key |
| Expired requests stampede origin revalidation | High | High | The baseline model starts two revalidations; the reference model serializes 32 calls in one process, with one start and 31 waits | A production implementation still needs a durable/coordinated lease with expiry, fencing, and an explicit completion/failure path |
| Exact-URL invalidation cannot make tag-purged data unservable | High | High | `demo` baseline reports `purge_left_servable=true` | Maintain tenant-scoped tag postings and invalidate before processing later requests |
| Separate metadata/body commits can expose an unverified body or delete a referenced body | Critical | High | `baseline_reproduction` reports both modeled crash failures; reference tests cover `after_prepare`, `after_body`, and `after_metadata` in memory only | Use a durable journal, verify actual body bytes before metadata publication, then coordinate filesystem reachability-GC |
| No public BogKit component spans the acceptance-critical cache boundary | High design blocker | High | README/examples plus Fold source and 18 passing Fold tests; see decision audit | Add a first-class cross-store transaction protocol and lease/purge primitives, which is beyond this trial's scope |
| ESE-backed public examples require an unavailable model download | Medium | High | `cargo check --locked -p starter` and `-p search` fail in `ese/build.rs` on DNS | Vendor the model or provide an explicitly offline fixture/feature |
| Existing search example is not rustfmt-clean | Low | High | `cargo fmt --all -- --check` reports only `examples/search/src/main.rs` | Apply rustfmt in a separate, approved example-only change |

## Decision audit

1. The baseline was written down and reproduced before selecting a runtime
   component.
2. Fold was considered for metadata and tag indexes because its public
   `KeyedStream`, `Table`, and `InvertedIndex` surfaces are real fits for those
   isolated views.
3. Fold was rejected as the runtime for this prototype because the body is a
   separate content-addressed file store, `Retain` uses wall-clock processing
   time, and the public API does not provide per-key leases or sequence-aware
   purge fencing.
4. A custom Fold node was also rejected: implementing the missing cross-store
   journal and concurrency protocol inside the trial would be a new BogKit
   cache component, not a meaningful evaluation of an existing one.
5. The dependency-free model was kept in a new `trials/` subtree. No BogKit
   core file, existing example, root workspace manifest, GitHub state, or
   automation state changed.
6. The model makes no production latency, throughput, SQLite, filesystem, or
   crash-durability claim. It is a deterministic correctness model with a
   compact allocation/quota shape check.

## Acceptance mapping

| Acceptance criterion | Evidence |
| --- | --- |
| 1. Correct request decision | Logical-time unit tests plus the file-driven demo emit fresh hit, miss/revalidation, stale-if-error, wait, and completion reason codes |
| 2. Modeled single-flight behavior | 32 sequential calls in one process produce one start and 31 waits; lease expiry, worker loss, and distributed concurrency are not tested |
| 3. Purge visibility and convergence | Tag-index unit test applies sequence 2, repopulates the entry, then applies reordered 1 and duplicate 2; the repopulated entry survives and both later purges are ignored |
| 4. Modeled crash recovery | Three in-memory crash points; pre-metadata phases roll back, post-metadata phase retains verified labels, and every final entry has a matching modeled manifest |
| 5. Quota and shape memory | Quota trace evicts under 300 bytes; the compact shape harness reports 8.64 GB logical usage under 64 GiB and 65,634,304 bytes RSS under 256 MiB. This is not semantic-scale evidence |
| 6. Stable private output | SHA-256 test, stable reason-code extraction, and privacy grep pass |

## Prototype defects and limitations

These are limitations of the trial, not findings against BogKit:

- It does not parse HTTP, normalize percent-encoding, evaluate real `Vary`
  headers, or implement all HTTP cache directives.
- The trace parser expects blob manifests before their initial entries and is
  intentionally whitespace-delimited.
- Body verification is represented by a supplied bit and size/digest labels;
  no bytes, body-file deletion, fsync, rename, SQLite transaction, process
  restart, or power-loss behavior is run.
- The semantic engine is in-memory. The stated-count workload is a compact
  shape harness, not a two-million-entry semantic run and not a semantic purge
  workload.
- Lease calls are sequential within one process; there is no expiry or worker
  loss path.
- RSS measurement is implemented for macOS with `getrusage`; other platforms
  report an unavailable measurement rather than guessing.
- Purge identity is `(tenant, sequence, tag)`. A production contract should
  define what happens if one sequence is reused for different tags.
- The reference model conservatively fences a whole tenant's active
  revalidation when any accepted purge changes that tenant epoch.

## Prototype defects versus BogKit defects

The missing lease, sequence-aware purge, and cross-store journal are not
reported as bugs in BogKit: the public README and APIs do not claim to provide
those cache features. The only repository-level friction observed was the
network-dependent ESE build and the existing search-example formatting diff;
both were left unchanged.
