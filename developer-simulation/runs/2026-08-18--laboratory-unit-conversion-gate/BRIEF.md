# Trial 1: Exact laboratory-unit conversion admission gate

## Developer role and exact Rust experience

You are a healthcare-interface developer who has maintained Java and SQL integrations for eight years. You have used Rust part-time for exactly four months, completed the Rust Book, and shipped two internal command-line import validators; you have not shipped a Rust service or unsafe Rust code.

## Existing system and baseline

A regional laboratory network receives observation exports from instrument gateways. PostgreSQL reference tables and a Java 17 conversion service remain authoritative. The Java service parses decimal values, selects a mapping by `(source_system, analyte_code, source_unit)`, checks the declared physical dimension, applies a configured rational affine transform, and rounds to the analyte's declared decimal scale.

Before onboarding a new gateway, operators run a Node.js batch preflight against an immutable export. It is advisory: it emits normalized preview rows and review reasons but never changes the reference tables or clinical records. A frozen slow Java evaluator and signed expected-output fixtures define the required behavior for this trial.

## Concrete pain

The Node.js preflight uses binary floating point and has drifted from the Java service on half-way rounding and large negative values. It also lets an unknown unit fall through as an unconverted string in one code path. A million-row migration rehearsal takes about eleven minutes and peaks above 1.4 GiB, so teams sample the data instead of checking the entire file. The prototype must show whether a small Rust CLI can give operators a complete, exact, fail-closed report before a source is admitted.

## Workload and data shape

The self-contained benchmark contains:

- One immutable reference snapshot with exactly 12,000 unique mapping rows, 250 unit symbols, and 300 analyte definitions.
- Exactly 1,000,000 NDJSON observations: 950,000 ordinary valid conversions, 25,000 valid boundary conversions, and 25,000 expected record-level rejections. The three classes sum to 1,000,000.
- Observation fields `observation_id`, `source_system`, `analyte_code`, `source_unit`, `declared_dimension`, and `value`. IDs are nonempty ASCII and globally unique. A line is at most 1,024 bytes.
- Decimal values with at most 29 significant digits and scale 0 through 9. Scientific notation, `NaN`, and infinity are outside the accepted input grammar.
- A unique mapping key, target unit, expected dimension, target scale from 0 through 9, positive rational multiplier, and signed rational offset. Numerators and denominators are bounded in the fixture so checked `i128` arithmetic is sufficient; denominators are never zero.

For an admitted row, compute `target = source * multiplier + offset` as exact rational arithmetic, then round once to the target scale using round-half-to-even. Do not round an intermediate. An unknown mapping, incompatible observation-declared dimension, or invalid observation value becomes a record-level rejection. A malformed or internally inconsistent reference snapshot, duplicate observation ID, unreadable input, or structurally invalid NDJSON invalidates the whole run.

## Operational constraints

- Run offline on a declared two-core Linux machine with no network access.
- Treat the PostgreSQL export and signed fixtures as immutable inputs; the prototype must not connect to a database.
- Never emit patient names or raw free-text fields. Fixtures contain synthetic IDs only, and diagnostics may contain an observation ID, field name, and stable reason code.
- Validate the full reference snapshot before processing observations. A mapping-key duplicate, unknown unit symbol, mapping-to-analyte dimension mismatch, zero denominator, invalid scale, or transform coefficient outside the declared arithmetic bounds is a run-level failure.
- Produce one complete canonical report or leave the previous complete report untouched. Publication uses a sibling temporary file and an atomic rename on the same filesystem.
- Use safe Rust only. No subprocess, locale-sensitive number parsing, wall-clock decision, or environment-dependent ordering is permitted.

## Measurable acceptance criteria

### Correctness

1. On all signed fixtures, output exactly one normalized row or rejection row for every observation ID and match the frozen Java evaluator byte for byte after canonical sorting by `observation_id`.
2. On the million-row benchmark, report exactly 975,000 conversions and 25,000 rejections; `950,000 + 25,000 = 975,000`, and `975,000 + 25,000 = 1,000,000`.
3. Pass at least 20,000 generated arithmetic cases covering positive and negative values, zero, maximum accepted magnitude, every output scale, exact half-way values with even and odd retained digits, non-half remainders, nonzero offsets, and non-integer multipliers. Every result must equal the supplied slow rational reference evaluator.
4. Never produce a normalized row for an unknown mapping, incompatible dimension, invalid decimal, or checked-arithmetic overflow. Each must receive the exact stable reason code in the fixture.
5. Parse decimal input without an intermediate binary floating-point value. A source review check and tests around values such as `0.1`, `-2.5`, and long 29-digit coefficients must demonstrate this.

### Failure behavior

1. Pass all 60 supplied negative fixture files, including truncated NDJSON, invalid UTF-8, overlong lines, duplicate IDs, duplicate mapping keys, unknown unit symbols, zero denominators, invalid scales, inconsistent dimensions, out-of-range coefficients, and forced read or write errors.
2. For every run-level failure, exit nonzero, publish no new report, and preserve the byte digest of a pre-existing report.
3. For record-level failures, continue through the valid remainder of a structurally sound file and include the rejected ID and stable reason without echoing the raw value.
4. With injected exits after 1 percent and 50 percent of rows and after temporary-file sync but before rename, a rerun must produce the same complete report and leave no path that readers can mistake for a complete partial report.

### Deterministic and runnable evidence

1. Provide `cargo test` as a single command that runs parsing, exact arithmetic, rounding, mapping validation, negative fixtures, publication, and slow-reference comparison tests.
2. Provide a deterministic fixture generator with a fixed default seed and a release-mode benchmark command documented in the README. The generator must also write a manifest containing input counts and SHA-256 digests.
3. Shuffle the 1,000,000 observation lines with each of five fixed seeds. All five runs must produce byte-identical canonical reports. Three repeated runs of one shuffle must also have identical SHA-256 digests.
4. The benchmark command must print only machine-readable counts, elapsed time, peak resident memory as measured by the supplied harness, output digest, and pass/fail status. The evidence must be reproducible from a clean checkout with no services running.

### Performance and resources

1. Process the complete 1,000,000-observation benchmark in at most 30 seconds on the declared two-core machine.
2. Peak resident memory must not exceed 256 MiB; temporary storage excluding the final report must not exceed 512 MiB; no input line may cause more than 2 MiB additional live memory.
3. Report size must be at most the input observation-file size plus 25 percent. The harness measures file bytes and fails the run above that bound.
4. A 100,000-row warm run must be no more than 1.5 times slower than the supplied Java batch evaluator on the same machine. Record both times; do not substitute a different machine or extrapolate from a smaller input.

## Explicit non-goals

- Do not replace the Java service, PostgreSQL tables, or clinical interface engine.
- Do not make treatment decisions, interpret reference ranges, infer units, or repair rejected observations.
- Do not implement the full UCUM specification, arbitrary symbolic algebra, fuzzy matching, or machine learning.
- Do not add a web server, background worker, database, distributed execution, or live gateway integration.
- Do not process real patient data or publish the prototype into the production admission path.

## Compact self-contained prototype boundary

Build one safe-Rust CLI with a small library behind it. It reads `units.ndjson`, `analytes.ndjson`, `mappings.ndjson`, and `observations.ndjson`; validates the reference snapshot; streams observations; and atomically publishes canonical `report.ndjson`. Include a fixed-seed synthetic fixture generator, a deliberately slow exact rational evaluator used only by tests, the 60 negative fixtures, a benchmark harness, and a short README with the exact test and benchmark commands. Stop at this boundary even if a production service would need authentication, schema management, monitoring, or database access.
