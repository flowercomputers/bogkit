#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"
workspace_manifest="$(cd "$root/../.." && pwd)/Cargo.toml"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-/private/tmp/fastq-barcode-spill-target}"

cargo test --offline --locked --manifest-path "$workspace_manifest" -p fastq-barcode-spill
cargo fmt --manifest-path "$workspace_manifest" -p fastq-barcode-spill -- --check
cargo clippy --offline --locked --manifest-path "$workspace_manifest" \
  -p fastq-barcode-spill --all-targets -- -D warnings
cargo build --release --offline --locked --manifest-path "$workspace_manifest" \
  -p fastq-barcode-spill
python3 tests/integration.py "$CARGO_TARGET_DIR/release/fastq-barcode-spill"

if FASTQ_BINARY="$CARGO_TARGET_DIR/release/fastq-barcode-spill" \
  FASTQ_MEASURE_PAIRS=2000 LSOF_COMMAND=/usr/bin/false \
  python3 scripts/measure_open_files.py; then
  echo "failed lsof observer was accepted" >&2
  exit 1
fi

demo_dir="$(mktemp -d "${TMPDIR:-/tmp}/fastq-barcode-spill-demo-XXXXXX")"
"$CARGO_TARGET_DIR/release/fastq-barcode-spill" \
  --barcodes fixtures/barcodes.tsv \
  --out "$demo_dir" \
  < fixtures/mixed.fastq
python3 -m json.tool "$demo_dir/manifest.json"
