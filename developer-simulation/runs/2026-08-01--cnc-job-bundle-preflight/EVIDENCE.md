# Trial report: CNC job bundle preflight

Run completed: 2026-08-01 06:22 EDT

## Outcome

The result is a useful, runnable local prototype and a **no-fit conclusion for
BogKit**. The prototype correctly classified the required valid, truncated,
checksum-mismatched, undeclared, missing, case-colliding, absolute-path,
parent-traversal, and oversized fixtures within its documented ZIP subset. It
also rejected a missing tool, reported a multi-error manifest in stable order,
accepted exactly 1,000 declared files, staged only the valid bundle, and failed
closed when the selected staging root was a symlink. After skeptical review,
staging also uses a temporary directory, removes incomplete output after an
ordinary write failure, rechecks every copied member, and assigns the `ready`
name only after the whole copy succeeds.

The 2 GiB sparse member was actually read and hashed, not skipped from header
metadata. The final archived-workspace timed run reported 1,998,848 bytes
maximum resident set (about 1.91 MiB), below the 32 MiB acceptance limit, and
2,147,483,919 streamed bytes including the manifest. The fixture was rejected
only by the prototype's 1 GiB total-member policy and was never staged.

This is not production-ready. It supports only classic stored ZIPs, its 1 GiB
policy needs a product decision, and staging is not hardened against an active
local process racing path checks. It intentionally does not interpret or run
G-code.

## Brief used

The simulated developer maintains an on-premises machine controller and has
intermediate Rust experience. The existing Python preflight checks required
filenames and extensions in operator ZIP bundles. The recurring failures are
truncation, checksums, undeclared or missing files, case-colliding names,
unsafe paths, and missing tools. Bundles are untrusted, contain 1-1,000 files,
are usually 10 MB and occasionally 2 GB, must be checked offline by streaming,
must stay below 32 MB working memory, and must never write outside selected
staging. G-code semantics, malware detection, repair, authentication, and
production deployment are out of scope.

## Ordered discovery, friction, and debugging trail

1. I listed only the public root README and public examples. I then read the
   root README plus each public example manifest and main source. I did not
   inspect another checkout, a prior trial, the internet, or simulation state.
2. Before choosing any BogKit component, I froze a runnable Python baseline in
   `baseline.py` and its predicted comparison in `BASELINE.md`: exact
   `manifest.json` presence plus case-insensitive `.json`, `.nc`, or `.gcode`
   filename checks, with no extraction or content reads.
3. Only after freezing that baseline, I inspected the root Cargo workspace and
   local tool versions. The public examples showed Fold for persistent,
   incrementally maintained views, ESE for embeddings, and ANNy for nearest
   neighbors. None addresses archive parsing, trust-boundary path handling, or
   streaming checksums, so I chose no BogKit component.
4. I created an isolated nested Rust workspace under `trial-output` and made no
   change to the root workspace or BogKit core. The CLI uses only Serde and
   Serde JSON; the classic ZIP reader/writer, SHA-256, and CRC32 are local.
5. The initial formatter check failed with formatting diffs. After formatting,
   all three unit tests passed, but strict Clippy rejected ten issues. I moved
   the 64 KiB stream buffer from the stack to bounded heap storage, fixed style
   findings, and narrowly allowed length/name lints in the ZIP parser, fixture
   generator, manifest validator, and standard SHA-256 round variables.
6. The first fixture-generation run failed with `File exists (os error 17)`
   because the generator required the selected output directory not to exist.
   I changed it to accept an existing directory, matching safe temporary-folder
   practice, and regenerated successfully.
7. The normal demonstration produced the expected ten classifications. Only
   valid staged. The staged program hash was independently checked.
8. The measured Python baseline corrected one prediction: Python 3.14.6
   rejected the physically truncated ZIP on open. It still marked checksum
   mismatch, undeclared, missing, duplicate-case, absolute-path,
   parent-traversal, missing-tool, and multi-error bundles ready.
9. A 2 GiB logical sparse ZIP (20 KiB physical allocation) was generated. The
   first sandboxed `/usr/bin/time -l` run completed the scan in 17.09 seconds
   but could not query memory (`sysctl kern.clockrate: Operation not
   permitted`). A `ps` sampler was also blocked and correctly discarded as
   evidence. The approved read-only rerun reported full process accounting.
10. Two multi-error runs had identical whole-output hashes. The 1,000-file
    fixture passed. A valid bundle aimed at a symlinked staging root became
    invalid with `staging_failed`, and the symlink target stayed empty.
11. Skeptical review found that a late ordinary write failure could leave an
    incomplete directory named `ready`, and that the second member read used
    for staging was not rechecked. The coordinator changed staging to use a
    temporary directory, remove incomplete output on failure, recheck byte
    count, CRC, and SHA-256 for every copy, and rename only after completion.
    Three regressions cover file/parent path collisions, incomplete-output
    cleanup, and content rechecking. Entry count and member-name length are now
    rejected before metadata allocation can grow beyond the trial policy.

## Baseline comparison

| Fixture | Python baseline measured | Rust prototype measured |
| --- | --- | --- |
| valid | ready | ready, staged |
| truncated | invalid (`zipfile` open failure) | invalid (`archive_invalid`) |
| checksum mismatch | **ready** | invalid (`checksum_mismatch`) |
| undeclared allowed-extension file | **ready** | invalid (`archive_file_undeclared`) |
| missing declared file | **ready** | invalid (`declared_file_missing`) |
| duplicate case | **ready** | invalid (archive and manifest case collision) |
| absolute path | **ready** | invalid (archive, manifest, and entry path unsafe) |
| parent traversal | **ready** | invalid (archive, manifest, and entry path unsafe) |
| missing tool | **ready** | invalid (`required_tool_missing`) |
| independent manifest errors | **ready** | invalid, 15 stable-ordered diagnostics |
| 2 GiB oversized sparse member | not run; baseline never reads members | invalid after full stream/hash |

The baseline's useful behavior is limited to central-directory readability,
the exact manifest filename, and extensions. The smallest safe improvement is
the prototype's sequence: inventory all names first, parse a bounded manifest,
collect independent errors, stream declared content for size/hash/CRC, and
only then create staging.

## Exact commands and observed evidence

### Public discovery

From `/private/tmp/bogkit-2026-08-01-trial-b.LaXksh`:

```console
pwd && rg --files -g 'README*' -g 'examples/**' -g '!target'
sed -n '1,240p' README.md; for f in examples/*/Cargo.toml examples/*/src/main.rs; do echo "FILE $f"; sed -n '1,260p' "$f"; done
```

Observed: one root README and four public examples (`starter`, `timeseries`,
`chat`, `search`). The first command confirmed the assigned checkout path.

### Build checks

From the prototype directory:

```console
cargo fmt --all -- --check
cargo test --offline
cargo clippy --offline --all-targets -- -D warnings
cargo build --offline --release
```

Observed after skeptical-review fixes: formatting passed; 6 tests passed, 0 failed; strict
Clippy passed with warnings denied; release build passed. Rust was
`rustc 1.95.0` and Cargo was `1.95.0`.

### Fixture generation and normal demonstration

```console
target/release/cnc-job-bundle-preflight generate-fixtures /private/tmp/cnc-preflight-fixtures.LTnBpG
target/release/cnc-job-bundle-preflight demo /private/tmp/cnc-preflight-fixtures.LTnBpG /private/tmp/cnc-preflight-staging.xRDZkg
```

Observed: generation succeeded. Demo exited 0 because valid was ready and the
nine normal adversarial fixtures were all invalid. Valid staged to
`/private/tmp/cnc-preflight-staging.xRDZkg/valid/ready`; each invalid report had
`ready: false` and `staged: null`.

```console
rg --files -uu /private/tmp/cnc-preflight-staging.xRDZkg
test ! -e /private/tmp/cnc-preflight-staging.xRDZkg/escape.nc
test ! -e /escape.nc
shasum -a 256 /private/tmp/cnc-preflight-staging.xRDZkg/valid/ready/programs/job.nc
```

Observed: the staging tree contained only valid's `manifest.json` and
`programs/job.nc`. Neither escape target existed. The staged program hash was
`b9cea1cced0aa93d046077443fe7ebfd0d5c217d0ba32a0dcc39fa3a18033861`,
matching the manifest and streamed diagnostic evidence.

### Measured Python baseline

```console
for f in valid truncated checksum-mismatch undeclared missing duplicate-case absolute-path parent-traversal missing-tool multi-error; do python3 baseline.py "/private/tmp/cnc-preflight-fixtures.LTnBpG/$f.zip"; done
```

Observed with Python 3.14.6: only truncated was invalid; all other bundles,
including all content/reference/path adversaries, were ready.

### Stable independent diagnostics

```console
target/release/cnc-job-bundle-preflight check /private/tmp/cnc-preflight-fixtures.LTnBpG/multi-error.zip --tools /private/tmp/cnc-preflight-fixtures.LTnBpG/tools.json | shasum -a 256
target/release/cnc-job-bundle-preflight check /private/tmp/cnc-preflight-fixtures.LTnBpG/multi-error.zip --tools /private/tmp/cnc-preflight-fixtures.LTnBpG/tools.json | shasum -a 256
```

Observed: both complete JSON outputs hashed to
`48eab7f13f5d8badf732bd67030e4e3349279f2c0ff124b9811fd1cfb78235c8`.
The output contained 15 diagnostics sorted by code, path, and message,
including independent archive, checksum, size, missing-file, entry-program,
manifest-version, manifest-format, duplicate, and tool errors.

### 1,000-file boundary

```console
target/release/cnc-job-bundle-preflight check /private/tmp/cnc-preflight-fixtures.LTnBpG/thousand-files.zip --tools /private/tmp/cnc-preflight-fixtures.LTnBpG/tools.json
```

Observed: `ready: true`, no diagnostics, 1,001 archive members including the
manifest, and 172,118 streamed bytes.

### 2 GiB streaming and memory

```console
target/release/cnc-job-bundle-preflight generate-fixtures /private/tmp/cnc-preflight-fixtures.LTnBpG --include-huge
ls -lh /private/tmp/cnc-preflight-fixtures.LTnBpG/oversized-2gib-sparse.zip
du -h /private/tmp/cnc-preflight-fixtures.LTnBpG/oversized-2gib-sparse.zip
/usr/bin/time -l target/release/cnc-job-bundle-preflight check /private/tmp/cnc-preflight-fixtures.LTnBpG/oversized-2gib-sparse.zip --tools /private/tmp/cnc-preflight-fixtures.LTnBpG/tools.json --staging /private/tmp/cnc-preflight-staging.xRDZkg/oversized
```

Observed: 2.0 GiB logical length, 20 KiB physical allocation. The final
archived-workspace timed run took 17.00 seconds, streamed 2,147,483,919 bytes
with a reported 65,536-byte buffer, matched the declared SHA-256 and ZIP CRC
(no mismatch diagnostics), and reported 1,998,848 bytes maximum resident set. It returned
exit 1 with only `archive_oversized`, `ready: false`, and `staged: null`.
`/private/tmp/cnc-preflight-staging.xRDZkg/oversized` did not exist afterwards.

### Symlinked staging root

```console
ln -s /private/tmp/cnc-preflight-outside.OpDeMm /private/tmp/cnc-preflight-staging-symlink.LTnBpG
target/release/cnc-job-bundle-preflight check /private/tmp/cnc-preflight-fixtures.LTnBpG/valid.zip --tools /private/tmp/cnc-preflight-fixtures.LTnBpG/tools.json --staging /private/tmp/cnc-preflight-staging-symlink.LTnBpG
```

Observed: a content-valid archive became `ready: false` with `staging_failed:
selected staging root must be a real directory`; nothing was staged and the
symlink target remained empty.

## Consequential decision audit

| Decision | Consequence | Evidence/reversibility |
| --- | --- | --- |
| Use no BogKit crate | Avoids a database and lifecycle that do not help the trust boundary | No core/workspace edits; easy to revisit if requirements become incremental |
| Validate fully before staging | Invalid bundles create no staging output | Demonstrated across nine normal adversaries plus oversized; invalid reports always had `staged: null` |
| Stage through a temporary directory and recheck every copy | A late failure cannot leave incomplete output named `ready`; copied bytes must still match validated content | Two skeptical-review regressions require cleanup and content rechecking before final naming |
| Treat a staging failure as not ready | A valid bundle is never reported ready if its requested write did not complete | Symlink-root and incomplete-copy tests returned no `ready` output |
| Reject compressed, encrypted, multi-disk, and ZIP64 inputs | Safe false negatives; many normal production ZIPs are currently unsupported | Explicit deterministic diagnostics; replace parser behind the same validation interface |
| Set total member cap to 1 GiB | Provides an oversized classification but conflicts with the brief's occasional 2 GiB workload | Prototype-only constant; needs operator/product decision before deployment |
| Require exact UTF-8 names and reject case collisions | Avoids controller/filesystem disagreement | Demonstrated ASCII case collision; Unicode normalization remains uncertain |
| Rename a completed temporary directory to `ready` | Prevents partial output from carrying the final name and avoids overwriting existing work | Existing destinations fail closed; copied content is rechecked; broader filesystem coordination remains unresolved |
| Hash with streaming SHA-256 and verify ZIP CRC | Detects manifest mismatch and archive corruption without member-sized allocation | Known SHA-256 unit vector plus 2 GiB cross-check against precomputed standard digest |

## Categorized findings

| Category | Severity | Confidence | Reproduction | Smallest improvement |
| --- | --- | --- | --- | --- |
| Baseline accepts unsafe archive paths | Critical for controller staging | High | Run `baseline.py` on absolute and traversal fixtures | Reject unsafe/case-colliding names before any write |
| Baseline accepts checksum, declaration, size, and tool failures | High | High | Baseline loop above | Parse bounded manifest and stream SHA-256/size/CRC before ready |
| BogKit component fit | Informational: no fit | High | Compare public examples to acceptance criteria | Keep BogKit out unless incremental multi-bundle state becomes a requirement |
| Compressed and ZIP64 bundles rejected | Medium compatibility limitation | High | Create a compressed or ZIP64 input; it fails closed | Use a mature streaming ZIP parser without extraction APIs |
| Staging path checks have local race windows | Medium hardening gap | Medium | Not safely reproduced without a racing local process | Use directory file descriptors plus `openat`/`mkdirat` and no-follow flags |
| 1 GiB policy conflicts with occasional 2 GiB workload | Medium product-policy gap | High | 2 GiB fixture returns only `archive_oversized` | Make a reviewed local configuration or raise the policy with disk-budget checks |
| Unicode normalization aliases are not detected | Low-to-medium portability gap | Medium | Not covered; composed/decomposed names may alias on some filesystems | Normalize to a chosen Unicode form before collision checks |

## Rejected alternatives

- **Fold:** durable incremental counts/tables do not make a one-shot bundle
  safer, and would add persistent state to a stateless controller gate.
- **ESE and ANNy:** embeddings and nearest-neighbor search do not address any
  manifest, archive, memory, or staging criterion.
- **Extract first, validate later:** violates the central trust boundary and
  makes absolute/traversal paths consequential before classification.
- **Read each member into memory:** simple but cannot meet the 32 MiB target for
  ordinary 10 MB growth or 2 GiB inputs.
- **Shell out to an archive extractor:** harder to make portable and prove
  extraction-free; the prototype instead reads member byte ranges directly.
- **Repair malformed bundles:** explicitly outside scope and risks turning an
  operator error into silently changed machine input.

## Skeptical review and coordinator corrections

The reviewer reproduced the build checks, required fixture classifications,
stable diagnostics, 1,000-file boundary, sparse 2 GiB stream, and the no-fit
comparison. The no-fit label stood: none of the BogKit components supplies the
one-shot archive parsing, manifest validation, or bounded copy gate required by
this prototype.

The reviewer found two high-severity prototype-quality problems before
archival: incomplete output could retain the final `ready` name after a late
copy failure, and staged content was not compared with the content validated on
the first read. Both are fixed by temporary staging, cleanup, per-copy
byte/CRC/SHA-256 checks, and final rename, with regressions. The reviewer also
required early archive-entry and member-name bounds and narrower compatibility
language for the hand-written classic stored-ZIP subset.

These were prototype defects, not BogKit defects. No new feature or API
candidate meets the dashboard threshold.

## Unresolved uncertainty

- No compressed, ZIP64, data-descriptor, or multi-disk interoperability was
  implemented; these fail closed rather than receiving broad parser coverage.
- The parser has unit and generated-fixture coverage but no fuzzing or corpus
  testing against independently created ZIP implementations.
- The 2 GiB RSS number is one macOS run. The bounded 64 KiB member buffer and
  bounded 65,557-byte end-record scan explain the low result, but other
  allocators/platforms should be measured.
- Staging rejects pre-existing symlinks, uses temporary output, rechecks copied
  content, and cleans ordinary failures, but it does not provide coordinated
  filesystem access against another local process changing paths concurrently.
- Case folding is deterministic, but Unicode normalization and controller
  filesystem semantics need an explicit policy.
- The 1 GiB cap was selected to make “oversized” concrete. It is not justified
  as the correct operational limit given occasional 2 GiB jobs.
- G-code and tool usage inside program text are intentionally not interpreted;
  only manifest-declared tool identifiers are checked against inventory.

## Deliverable and workspace state

All prototype source, baseline material, lockfile, usage notes, and this report
are under
`/private/tmp/bogkit-2026-08-01-trial-b.LaXksh/trial-output/cnc-job-bundle-preflight`.
Generated fixtures and staging evidence were kept under separate `/private/tmp`
directories. The root Git status showed only untracked `trial-output/`; no
BogKit core or existing example was edited. The nested `[workspace]` avoided
any root Cargo workspace membership change.
