# OCR redaction remap trial

This is a standalone Rust CLI for remapping human-reviewed UTF-8 spans from old OCR text to revised grapheme rectangles. It processes one JSON page at a time, emits no OCR text, validates all geometry before marking a page publishable, and checkpoints paired rectangle/audit streams for deterministic resume.

It intentionally does **not** use a BogKit crate. Fold's durable incremental views solve a different problem from independent, authoritative page transformations; ESE and ANNy have no role in this mapping.

## Input and output

Each input JSONL line has:

- `page_id`, `old_text`, and `revised_text`
- `glyphs`: revised graphemes encoded as `[start_byte,end_byte,line,x,y,width,height]`; line-break graphemes have no rectangle
- `spans`: reviewed old-text `{start,end,reason}` byte ranges

Output JSONL contains only page IDs, status, rectangles, and a non-sensitive error code. The separate audit JSONL contains counts, allow-listed identifier-shaped reason codes, and decisions (`exact`, `removed`, `fallback_token`, or `fallback_line`), never source text or matched text.

## Run the verified fixture demo

Run from the BogKit repository root:

```sh
developer-simulation/runs/2026-08-04--ocr-redaction-remap/run_demo.sh
```

The script generates 240 systematic fixtures plus four hand cases; verifies
exact rectangle identity and sentinel absence; checks reversed/duplicated spans
for byte identity; and runs two interruption/resume comparisons. Generated
files use a temporary directory under `/private/tmp` and are removed on exit;
build output uses `/private/tmp/ocr-redaction-remap-target`.

## Individual commands

```sh
export CARGO_TARGET_DIR=/private/tmp/ocr-redaction-remap-target
DEMO_DIR="$(mktemp -d /private/tmp/ocr-redaction-remap-demo.XXXXXX)"
cargo run --offline --locked --manifest-path developer-simulation/Cargo.toml \
  -p ocr-redaction-remap -- generate --dir "$DEMO_DIR/fixtures" --pages 240
cargo run --offline --locked --manifest-path developer-simulation/Cargo.toml \
  -p ocr-redaction-remap -- remap \
  --input "$DEMO_DIR/fixtures/pages.jsonl" \
  --output "$DEMO_DIR/output.jsonl" \
  --audit "$DEMO_DIR/audit.jsonl"
cargo run --offline --locked --manifest-path developer-simulation/Cargo.toml \
  -p ocr-redaction-remap -- check \
  --input "$DEMO_DIR/fixtures/pages.jsonl" \
  --output "$DEMO_DIR/output.jsonl" \
  --audit "$DEMO_DIR/audit.jsonl" \
  --expected "$DEMO_DIR/fixtures/expected.jsonl" \
  --sentinels "$DEMO_DIR/fixtures/sentinels.json"
```

Use `--stop-after N` to simulate interruption and `--resume` to continue from
the last paired checkpoint. A completed run writes `<output>.complete`; it binds
the exact input, output, and audit paths, lengths, and SHA-256 values. Consumers
should verify through `--resume` and reject any page whose status is `blocked`.
This is controlled process-resume evidence, not a power-loss durability
guarantee.

Generate the exact acceptance workload with:

```sh
cargo run --release --offline --locked \
  --manifest-path developer-simulation/Cargo.toml \
  -p ocr-redaction-remap -- generate-workload \
  --output "$DEMO_DIR/workload.jsonl" \
  --pages 5000 --scalars-per-page 4000 --spans-per-page 30
```

The mapper caps non-trivial edit alignment at 256 edits per page. Pages over that limit are blocked rather than guessed.
