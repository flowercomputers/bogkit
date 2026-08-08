# FASTQ barcode spill trial

This is a dependency-free Rust prototype for demultiplexing interleaved paired-end FASTQ from
standard input. It exact-matches the first 10 read-1 bases, corrects only a unique barcode at
Hamming distance one, sends ties to `ambiguous.fastq`, and sends other misses to
`unmatched.fastq`. Per-sample order is preserved.

It creates all sample files but keeps at most 24 FASTQ output writers open. Small per-destination
buffers avoid reopening a file for every pair when hundreds of samples are interleaved. A
deterministic `manifest.json` is atomically published only after all input and output validation
succeeds.

## Run the focused verification

From the BogKit repository root:

```console
export CARGO_TARGET_DIR=/private/tmp/fastq-barcode-spill-target
developer-simulation/runs/2026-08-05--fastq-barcode-spill/scripts/verify.sh
```

## Run the demo directly

```console
export CARGO_TARGET_DIR=/private/tmp/fastq-barcode-spill-target
cargo build --release --offline --locked --manifest-path developer-simulation/Cargo.toml \
  -p fastq-barcode-spill
demo_dir="$(mktemp -d /private/tmp/fastq-barcode-spill-demo-XXXXXX)"
CARGO_TARGET_DIR=/private/tmp/fastq-barcode-spill-target
/private/tmp/fastq-barcode-spill-target/release/fastq-barcode-spill \
  --barcodes fixtures/barcodes.tsv \
  --out "$demo_dir/output" \
  < fixtures/mixed.fastq
python3 -m json.tool "$demo_dir/output/manifest.json"
```

The TSV has exactly `10-base-barcode<TAB>safe-sample-name` per non-comment line. The output
directory must be new or empty. Existing data is never overwritten. Sample output names must be
unique under ASCII case folding and may not alias the reserved `ambiguous` or `unmatched` names;
this is checked before the output directory is created.

The supported paired identifiers are a non-empty printable first token with matching cores and
either `/1` plus `/2`, matching Illumina/CASAVA whitespace roles, or no explicit role. Conflicting
dual conventions and control bytes are rejected without echoing identifier content.

## Reproduce the clean Python-baseline comparison

```console
comparison_dir="$(mktemp -d /private/tmp/fastq-comparison-XXXXXX)"
trial=developer-simulation/runs/2026-08-05--fastq-barcode-spill
python3 "$trial/baseline.py" --barcodes "$trial/fixtures/barcodes.tsv" \
  --out "$comparison_dir/baseline" < "$trial/fixtures/clean.fastq"
/private/tmp/fastq-barcode-spill-target/release/fastq-barcode-spill \
  --barcodes "$trial/fixtures/barcodes.tsv" --out "$comparison_dir/rust" \
  < "$trial/fixtures/clean.fastq"
for f in alpha.fastq beta.fastq gamma.fastq delta.fastq unmatched.fastq; do
  cmp "$comparison_dir/baseline/$f" "$comparison_dir/rust/$f"
done
```

The baseline intentionally exact-matches only and opens all sample destinations. The comparison
therefore covers clean per-sample and unmatched FASTQ bytes, not the Rust-only ambiguity output
or completion manifest.

## Generate a numeric workload

This preserves non-seekable stdin by piping the generated data directly:

```console
trial=developer-simulation/runs/2026-08-05--fastq-barcode-spill
python3 "$trial/fixtures/generate.py" --samples 384 --pairs 0 --well-spaced \
  --barcodes /private/tmp/fastq-384.tsv
workload_dir="$(mktemp -d /private/tmp/fastq-million-XXXXXX)"
python3 "$trial/fixtures/generate.py" --samples 384 --pairs 1000000 --well-spaced --mixed --emit-only \
  --barcodes /private/tmp/fastq-384.tsv |
  /usr/bin/time -l /private/tmp/fastq-barcode-spill-target/release/fastq-barcode-spill \
    --barcodes /private/tmp/fastq-384.tsv --out "$workload_dir/output"
```

Use a new/empty output directory for every run. `TRIAL_REPORT.md` records the measurements made
on the test host and does not generalize them to other hosts or record sizes.

To independently observe the output descriptors with `lsof` during a throttled 200,000-pair
pipe (after the release build):

```console
FASTQ_BINARY=/private/tmp/fastq-barcode-spill-target/release/fastq-barcode-spill \
  python3 "$trial/scripts/measure_open_files.py"
```

The observer exits nonzero if `lsof` produces no successful positive sample; its result is
supporting evidence for the implementation's structural 24-writer bound.
