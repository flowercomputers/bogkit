#!/bin/sh
set -eu

project_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
work_dir=$(mktemp -d /tmp/remittance-reconciliation.XXXXXX)
target_dir="$work_dir/target"
fixture_dir="$work_dir/fixture"
results_dir="$work_dir/results"
baseline_dir="$work_dir/baseline"

cleanup() {
  rm -rf -- "$work_dir"
}
trap cleanup EXIT HUP INT TERM

cd "$project_dir"
CARGO_TARGET_DIR="$target_dir" cargo build --release --locked
binary="$target_dir/release/remittance-reconciliation"

"$binary" generate \
  --out "$fixture_dir" \
  --claim-count 62000 \
  --remittance-count 50000
"$binary" reconcile \
  --claims "$fixture_dir/claims.jsonl" \
  --remittances "$fixture_dir/remittances.jsonl" \
  --out "$results_dir"
"$binary" verify \
  --claims "$fixture_dir/claims.jsonl" \
  --remittances "$fixture_dir/remittances.jsonl" \
  --ground-truth "$fixture_dir/ground-truth.jsonl" \
  --results "$results_dir"

"$binary" baseline \
  --claims "$fixture_dir/claims.jsonl" \
  --remittances "$fixture_dir/remittances.jsonl" \
  --out "$baseline_dir"
"$binary" verify \
  --claims "$fixture_dir/claims.jsonl" \
  --remittances "$fixture_dir/remittances.jsonl" \
  --ground-truth "$fixture_dir/ground-truth.jsonl" \
  --results "$baseline_dir" \
  --allow-failure

"$project_dir/scripts/check-shuffles.sh" \
  "$binary" \
  "$fixture_dir" \
  "$results_dir" \
  "$work_dir/shuffles"
