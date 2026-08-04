# Trial notes — OCR redaction remapping

## Skeptical review and coordinator correction

The independent reviewer reproduced the fixture demo and no-fit decision, then
found three serious prototype defects before archival. A unique literal match in
the revised text could select the wrong repeated occurrence after an OCR change,
leaving the reviewed occurrence uncovered. A completed-run marker trusted
changed input or truncated output. A stale checkpoint could also extend a newly
truncated partial file with NUL bytes and report success.

The coordinator fixed all three classes. Literal candidates are now checked
against the source-position edit mapping and disagreements are covered
conservatively. Completion markers bind the exact input, output, and audit paths,
lengths, and SHA-256 values. Fresh runs invalidate stale partial/checkpoint state;
resume rejects short or changed partial prefixes instead of extending them.
Seven corrected unit tests cover the reviewer's exact repeated-occurrence,
changed-input, truncated-output, changed-audit, stale-checkpoint, and short-
partial reproductions. The 244-page demo, formatting, strict lint, and both
controlled resumes still pass.

The corrected nested-workspace 5,000-page, 20-million-ASCII-scalar,
150,000-span workload completed in 38.82 seconds with 5,947,392 bytes maximum
RSS, produced 5,000
output and audit lines, and matched a second full output byte-for-byte. This is
controlled process-resume and workload-specific evidence, not a general Unicode,
confidentiality, durability, crash, or power-loss guarantee. No BogKit defect was
demonstrated.

The remainder preserves the blind developer's trail. This reviewed correction
controls where an initial claim differs.

Run date: 2026-08-04 (America/New_York)

Persona: public-records processing engineer; production Python experience; Rust beginner.

## Outcome

Built a standalone Rust CLI under `trial/` that streams one page-delimited JSON record at a time, remaps reviewed old-text spans to revised grapheme rectangles, emits a content-free audit, blocks pages with invalid spans or geometry, and resumes paired output/audit files after controlled process interruption. The trial uses no BogKit crate because none fits this independent deterministic transformation.

Observed acceptance evidence:

- 240 systematic pages plus four hand pages: 243 exact pages, one explicit conservative repeated-token page, zero blocked pages, zero rectangle mismatches, and zero sentinel leaks in rectangle/audit output.
- Old/revised fixtures cover combining marks, multibyte text, compatibility ligatures, soft hyphens, line-break dehyphenation, whitespace normalization, an OCR character substitution, repeated phrases with and without distinguishing context, overlapping spans, duplicates, and deletion.
- Reversing span order and adding duplicates produced byte-identical rectangle and audit files.
- Two controlled interruptions after pages 73 and 191 resumed to byte-identical uninterrupted outputs. Broader crash and split-finalization claims were not retained as reviewed evidence.
- Corrupt offsets, an offset inside a multibyte character, missing geometry, and contradictory geometry each returned exit 2 with a deterministic blocked page and zero rectangles. Two runs had identical SHA-256 values. Invalid raw UTF-8 returned exit 2 with only `error code=invalid_utf8 line=1`.
- The final corrected nested-workspace workload of 5,000 pages, 20,000,000
  ASCII scalar values, and 150,000 reviewed spans completed in 38.82 seconds.
  macOS `/usr/bin/time -l` reported 5,947,392 bytes maximum resident set size, below
  the 64 MiB limit. It emitted 5,000 output lines, 5,000 audit lines, 600,000
  exact rectangles, zero blocked pages, and zero conservative pages. A second
  completed workload output was byte-identical.
- `cargo fmt -- --check`, seven unit tests, Clippy with warnings denied, a debug
  build, a release build, and the end-to-end demo all passed.

## Discovery order and friction

I behaved as if I had no prior BogKit knowledge and did not inspect any prior simulation material.

1. Ran `pwd`, `git status --short --branch`, `ls -la`, then read public `README.md`. The checkout was detached and clean. The README said the smallest start was `./scripts/new-project.sh`, described Fold, ESE, and ANNy, and listed examples in the order starter, timeseries, chat, search.
2. Listed public example files and read their public source in README order: `examples/starter/src/main.rs`, `examples/timeseries/src/main.rs`, `examples/chat/src/main.rs`, then `examples/search/src/main.rs`.
3. Tried to freeze the advertised smallest runnable BogKit baseline with:

   ```sh
   CARGO_TARGET_DIR=/private/tmp/bogkit-sim-2026-08-04-b/trial/.baseline-target cargo run -p starter
   ```

   It failed while building ESE. ESE attempted to download `model.safetensors`, then panicked after DNS resolution failed. This was before any trial design choice.
4. Only after that failure, read root `Cargo.toml`, `examples/starter/Cargo.toml`, and `scripts/new-project.sh`. The starter manifest declares `anny`, `ese`, and `fold` even though its source uses only Fold. The scaffold script also adds all three and the README says they may not all be used. That explains why the unrelated model download blocks the advertised starter offline.
5. Froze the existing-system baseline independently with `python3 trial/baseline_clamp.py`. It returned:

   ```json
   {"baseline": "stale_offset_clamp", "partial_exposure_reproduced": true}
   ```

   The reproducer asserts the exposure but prints no sensitive fixture text.

## Fit decision

No BogKit component was used.

- Fold maintains durable views as inserts and retractions arrive. Here every page is an authoritative, independent old/revised pair and the required result is a deterministic append-only transformation. Fold would introduce durable mutable state and a second recovery model without improving alignment, geometry validation, or ambiguity handling.
- ESE generates semantic embeddings; no semantic retrieval or similarity model is required.
- ANNy indexes nearest neighbors; pages and reviewed spans must never be matched approximately across records.
- The public surface inspected did not expose a Unicode sequence-alignment or glyph-geometry redaction primitive.

This is a poor product fit, not a demonstrated defect in Fold, ESE, or ANNy correctness. The starter's unrelated ESE build coupling is a separate observed setup defect.

## Prototype design

- Input spans remain authoritative UTF-8 byte ranges. Out-of-range, empty/reversed, or non-boundary offsets block the page.
- Unicode graphemes are preserved for rectangles. Matching tokens use NFKC, remove soft hyphens, collapse whitespace, expand compatibility ligatures, and remove line-break hyphenation only between alphanumeric characters.
- A unique normalized occurrence maps exactly. Repeated occurrences use up to 32 tokens of surrounding context; a unique best context remains exact, while a tie redacts every smallest matching token occurrence.
- When normalized text differs, a deterministic Myers edit script maps replacement blocks. Unique 4–8-token flanks make the local mapping exact. Without unique flanks, the mapper uses the implicated line as an explicit conservative fallback; if no safe line can be identified, it blocks publication.
- Non-trivial edit distance is capped at 256 edits per page. Larger rewrites return `edit_distance_limit`; they are never guessed.
- Every non-line-break revised grapheme must have exactly one positive-size rectangle. Missing, duplicate, extra, or contradictory geometry blocks the page and produces no rectangles.
- Reviewed spans are sorted and deduplicated before processing. Rectangles and reason codes are sorted and deduplicated. No offsets, source text, revised text, or matched token is present in the audit.
- Each checkpoint records completed input lines, paired lengths, rolling input/
  output/audit prefix hashes, and blocked-page count. Resume rejects short or
  changed partial prefixes, then truncates only uncheckpointed suffixes. A
  completion marker binds the exact paths, lengths, and SHA-256 values of the
  input and both final streams. Consumers must verify it and reject `blocked`
  pages. Directory-sync and power-loss durability remain outside the evidence.

## Exact verification commands and observed results

All commands ran from `/private/tmp/bogkit-sim-2026-08-04-b`.

### Formatting, tests, and lint

```sh
cargo fmt --manifest-path trial/Cargo.toml -- --check
CARGO_TARGET_DIR=/private/tmp/bogkit-sim-2026-08-04-b/trial/.target cargo test --offline --manifest-path trial/Cargo.toml
CARGO_TARGET_DIR=/private/tmp/bogkit-sim-2026-08-04-b/trial/.target cargo clippy --offline --all-targets --manifest-path trial/Cargo.toml -- -D warnings
```

Observed: formatting passed; four tests passed; Clippy finished with no warnings.

### Complete demo

```sh
trial/run_demo.sh
```

Observed final lines:

```text
checked pages=244 exact=243 conservative=1 sentinel_leaks=0 rectangle_mismatches=0
demo passed: exact rectangles, content-free output, duplicate identity, and two resumes
```

The script also compared normal versus reversed/duplicated spans and both resumed runs with `cmp`; every comparison returned 0.

### Malformed input

Representative command (repeated for all four structured malformed files):

```sh
trial/.target/debug/ocr-redaction-remap remap \
  --input trial/generated/malformed/corrupt_offset.jsonl \
  --output trial/generated/malformed/corrupt-output.jsonl \
  --audit trial/generated/malformed/corrupt-audit.jsonl
```

Observed structured errors and exit status:

| Case | Page error code | Exit | Rectangles |
|---|---|---:|---:|
| corrupt offset | `corrupt_offset` | 2 | 0 |
| byte inside multibyte character | `invalid_utf8_boundary` | 2 | 0 |
| missing rectangle | `missing_geometry` | 2 | 0 |
| duplicate/contradictory rectangle | `contradictory_geometry` | 2 | 0 |

Two-run SHA-256 values were unchanged:

```text
ad4fc132cfbe8b5db81cd98e49379be6d48078e8456bf9f16b81645ed05812ce  corrupt-output.jsonl
a02c3c9790d1220b2f9320db89dd3312a333e187430e9796489bd2870f7e0165  boundary-output.jsonl
4d0957a82e9b4db1cb620efb438fdbfd98702a5e76cdd541b716f6045c4c14ac  missing-output.jsonl
a87492b1c5151e1b70ebada56778f302ee3b8f79a38331a834cd3064b952cbaa  contradictory-output.jsonl
```

Invalid raw UTF-8 was run twice. Both runs returned exit 2 and exactly:

```text
error code=invalid_utf8 line=1
```

### Full workload

```sh
trial/.target/release/ocr-redaction-remap generate-workload \
  --output trial/.workload/pages.jsonl \
  --pages 5000 --scalars-per-page 4000 --spans-per-page 30

/usr/bin/time -l -o trial/.workload/time.txt \
  trial/.target/release/ocr-redaction-remap remap \
  --input trial/.workload/pages.jsonl \
  --output trial/.workload/timed-output.jsonl \
  --audit trial/.workload/timed-audit.jsonl
```

Observed:

```text
generated workload_pages=5000 scalars=20000000 spans=150000
38.82 real  13.40 user  2.13 sys
5947392 maximum resident set size
5000 output lines; 5000 audit lines
0 blocked; 0 conservative; 600000 rectangles
```

`cmp` confirmed the timed output and audit were byte-identical to the earlier uninterrupted workload run.

## Findings

### 1. Stale-offset clamp can expose sensitive text

- Category: correctness defect in the existing Python baseline, not BogKit.
- Severity: critical.
- Confidence: high; deterministic assertion-backed reproducer.
- Reproduction: `python3 trial/baseline_clamp.py`.
- Smallest plausible improvement: stop clamping revised offsets; require a validated old-to-revised mapping and block publication on unresolved mapping or geometry.

### 2. Advertised starter is coupled to an unrelated model download

- Category: API friction and documentation gap in the current BogKit public starter.
- Severity: medium for first-run/offline use.
- Confidence: high for this detached current-main checkout; observed command and manifests explain the failure.
- Reproduction: the `cargo run -p starter` command above in a fresh target directory.
- Smallest plausible improvement: remove unused ESE/ANNy dependencies from `examples/starter/Cargo.toml`; make the scaffold add components on demand, or clearly disclose the ESE model download.

### 3. BogKit is a poor fit for this mapper

- Category: poor product fit; public-surface missing capability.
- Severity: informational.
- Confidence: high for Fold/ESE/ANNy roles described by the public README and examples; medium for the absence of any unpublished/internal primitive because this trial intentionally began and stayed on the public surface.
- Reproduction: compare the page-local transformation requirements with the starter/timeseries/chat/search examples.
- Smallest plausible improvement: none recommended for this task. A standalone streaming transformer is smaller and has a clearer recovery boundary.

### 4. Broad rewrites are intentionally blocked

- Category: prototype missing capability, not BogKit.
- Severity: moderate availability limitation; safe for confidentiality.
- Confidence: high; explicit `MAX_EDIT_DISTANCE = 256` behavior.
- Reproduction: provide a page whose normalized old/revised edit distance exceeds 256.
- Smallest plausible improvement: add a linear-space anchor partitioner with a separately tested memory bound before increasing the cap.

### 5. Measured performance meets the stated bound

- Category: performance result, no observed performance problem.
- Severity: informational.
- Confidence: high for the exact generated workload executed on this machine.
- Reproduction: full-workload commands above.
- Smallest plausible improvement: none required for the stated 64 MiB target; production inputs should still be profiled because geometry number formatting and page size can differ.

## Consequential-choice audit

| Choice | Decision | Evidence | Consequence / unresolved uncertainty |
|---|---|---|---|
| Use BogKit | No | Public components maintain views, embeddings, or nearest-neighbor indexes; none performs authoritative Unicode remapping. | Avoids unrelated state and model dependencies. Does not assess unpublished APIs. |
| Offset model | Validate UTF-8 bytes, map normalized graphemes | Invalid boundaries blocked deterministically; Unicode fixtures exact. | A reviewed span that normalizes to no token blocks rather than guessing. |
| Repeated sensitive text | Redact every tied smallest token candidate | Hand ambiguity case redacted both occurrences and nothing between them. | Can reduce utility but does not select an arbitrary occurrence. |
| Changed text | Bounded deterministic diff plus unique flanks | OCR substitution and deletion fixtures mapped exactly. | More than 256 edits blocks the page. Production normalization rules may need additional explicit transforms. |
| Geometry | Require complete one-to-one revised grapheme geometry | Missing and contradictory fixtures blocked with zero rectangles. | No bidirectional or arbitrary layout handling, consistent with non-goals. |
| Duplicate spans | Sort/deduplicate before processing | Reversed and duplicated input was byte-identical. | Same offsets with different valid reason codes remain two audit decisions but one rectangle union. |
| Recovery | Paired lengths and prefix hashes + SHA-256-bound completion marker | Two controlled resumes and the reviewer regressions pass. | This is process-resume evidence only; no exhaustive crash or power-loss injection was performed. |
| Diagnostics | Static error codes and counts only | Leak checker found zero fixture sentinels; invalid UTF-8 error was content-free. | Page IDs and reason codes are required to be non-sensitive identifier-shaped metadata. |

## Scope and cleanup

No BogKit/core/example/root file was modified. There was no commit, GitHub write, automation write, PDF/OCR/database/network dependency in the prototype, or archive into another checkout. Network was used only once to resolve the trial's four small Rust library dependencies after the sandboxed index lookup failed; subsequent verification ran offline from `trial/Cargo.lock`.

Generated build targets, demo outputs, and the 493 MiB workload were removed after recording the measurements. The retained handoff is source, lockfile, README, runnable demo/generators/checker, baseline reproducer, and these notes.
