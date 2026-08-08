#!/bin/sh
set -eu

if [ "$#" -ne 4 ]; then
  echo "usage: $0 BINARY FIXTURE_DIR REFERENCE_RESULTS WORK_DIR" >&2
  exit 2
fi

binary=$1
fixture_dir=$2
reference_results=$3
work_dir=$4
seeds="1 2 3 5 8 13 21 34 55 89"

mkdir -p "$work_dir"
for seed in $seeds; do
  shuffled="$work_dir/seed-$seed/input"
  results="$work_dir/seed-$seed/results"
  "$binary" shuffle \
    --claims "$fixture_dir/claims.jsonl" \
    --remittances "$fixture_dir/remittances.jsonl" \
    --seed "$seed" \
    --out "$shuffled"
  "$binary" reconcile \
    --claims "$shuffled/claims.jsonl" \
    --remittances "$shuffled/remittances.jsonl" \
    --out "$results"
  cmp "$reference_results/accepted.jsonl" "$results/accepted.jsonl"
  cmp "$reference_results/review.jsonl" "$results/review.jsonl"
  cmp "$reference_results/summary.json" "$results/summary.json"
  echo "seed $seed: byte-identical"
done
