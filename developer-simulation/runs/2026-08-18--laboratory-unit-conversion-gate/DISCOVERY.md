# Baseline-first discovery hypothesis

## Starting point

I am evaluating this as a healthcare-interface developer with eight years of Java and SQL integration work and four months of part-time Rust use. Before writing this note I read only the trial brief, the public root README, and the public examples. I have not inspected Fold, ESE, or ANNy implementation code, and I have not read any earlier lab run or the other trial.

The authoritative baseline is a frozen Java exact evaluator plus signed expected-output fixtures. The current advisory Node.js preflight is known to disagree on decimal half-way rounding and large negative values, can allow an unknown unit through unconverted, takes about eleven minutes for one million rows, and peaks above 1.4 GiB. The admission gate therefore needs proof of exactness and fail-closed behavior before speed matters.

## Problem-first hypothesis

A small standalone safe-Rust streaming CLI should be able to improve this baseline without a database runtime:

1. Parse the restricted decimal grammar directly into a signed `i128` coefficient and base-10 scale, never through binary floating point.
2. Represent each configured affine transform as bounded rational integers, combine source value, multiplier, and offset with checked arithmetic, and round exactly once with round-half-to-even.
3. Fully validate small reference tables in memory, then stream observation rows while retaining only mapping/analyte lookups, duplicate-ID state, and sortable report records.
4. Reject record-level value/mapping/dimension problems using stable codes, but fail the whole run for structural input/reference/publication faults.
5. Produce canonical output through a same-directory sibling temporary file, sync it, and atomically rename it only after the complete run succeeds.

The main technical risk is canonical sorting under a 256 MiB peak-memory limit. An in-memory million-row report may be too large. A production-shaped implementation would likely require bounded external sorting or exploit a fixture ordering guarantee, but no such guarantee is stated. For the smallest meaningful prototype I will first prove the error-prone core: exact decimal arithmetic, fail-closed admission, stable record rejections, deterministic canonical ordering on representative data, and preservation of a prior report on run failure. I will explicitly leave million-row/two-core/Java-comparison gates unclaimed unless they can actually be run on matching supplied inputs and hardware.

## BogKit component consideration before implementation inspection

- **Fold:** potentially relevant only if its persistent incremental materialization can simplify deterministic keyed admission/report state. The workload is immutable batch input with one final atomic report, so persistence and eager view maintenance may add storage, memory, recovery, and API cost without solving exact arithmetic or atomic publication. I will inspect its public API after this hypothesis and use it only if it materially reduces the prototype.
- **ESE:** text embeddings do not help exact numeric conversion, validation, canonical ordering, or atomic publication. Expected no fit.
- **ANNy:** approximate nearest-neighbor search is incompatible with exact mapping selection and fail-closed clinical preflight behavior. Expected no fit.

Grounded use/no-use decisions will follow public API inspection. A `no_fit` result is acceptable if a plain Rust solution best matches the brief.

## Finishing criteria for this bounded trial

- Tests are written first and observed failing for each retained behavior before implementation.
- A runnable safe-Rust crate demonstrates exact decimal parsing, checked rational affine conversion, round-half-to-even boundaries, reference validation, stable record-level rejection, duplicate-ID run failure, deterministic canonical output, slow independent reference comparison, and prior-report preservation.
- The crate has a fixed-seed generator or deterministic representative fixture path and a runnable demo/benchmark command.
- `cargo test`, formatting, strict Clippy (warnings plus all/pedantic denied), and the demo/benchmark pass.
- The report records exact commands/results, measurements, limitations, ordered friction, component fit, one-category findings, and cleanup proof without claiming unrun target gates.

## Post-hypothesis component decision

After recording the hypothesis, I inspected each component's public crate documentation and top-level API:

- Fold is a persistent, transactional, incrementally maintained dataflow over an embedded Fjall store. Its strengths are changing streams, retractions, and durable materialized views. This trial is a one-shot immutable batch whose hard parts are exact arithmetic, bounded canonical sorting, duplicate detection, and atomic file publication. Adding Fold would introduce a database and persistent runtime without removing those hard parts, conflicting with the compact no-database boundary.
- ESE converts text into floating-point embedding vectors. It cannot contribute to exact decimal conversion or exact mapping selection.
- ANNy is approximate vector-neighbor search. Approximation is specifically unsuitable for a fail-closed exact mapping key.

Final component result: **no_fit** for Fold, ESE, and ANNy. The prototype deliberately has no BogKit component dependency. This is a product-fit result, not a claim that the components are defective.

## Skeptical-review repair trail

The initial reviewer reproduced three prototype/evidence defects without challenging the component decision: an output could alias and replace an immutable input, a bare relative output reported a directory-sync error after the report had already appeared, and the benchmark printed acceptance `pass` despite missing authority and an externally measured memory-gate failure. The reviewer also identified two report-quality problems: missing supplied evidence was misclassified as a product finding, and 60 retained files duplicated one parser-error class.

Repair remained outside BogKit components. Test-first regressions now cover direct and resolved input aliases, an existing hard-link identity, the adjacent sibling-temporary alias, a bare relative output, and an explicitly labeled post-rename durability-uncertain state. Benchmark output separates completed execution from `unverified` or `failed` acceptance and has a known over-limit regression. The redundant fixture set is replaced by one named unknown-field example; the absent supplied corpus remains an evidence limit.

These repairs strengthen the standalone file-transform boundary and leave the grounded **no_fit** decision unchanged. They do not turn missing Java, signed-fixture, or target-Linux evidence into a component defect or adoption result.
