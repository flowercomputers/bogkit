#!/usr/bin/env bash
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
repo_root="$(cd "$here/../../.." && pwd)"
demo="$(mktemp -d /private/tmp/ocr-redaction-remap-demo.XXXXXX)"
target="/private/tmp/ocr-redaction-remap-target"
binary="$target/debug/ocr-redaction-remap"
trap 'rm -rf "$demo"' EXIT

CARGO_TARGET_DIR="$target" cargo build --offline --locked \
  --manifest-path "$repo_root/developer-simulation/Cargo.toml" \
  -p ocr-redaction-remap
"$binary" generate --dir "$demo/fixtures" --pages 240
"$binary" remap --input "$demo/fixtures/pages.jsonl" --output "$demo/output.jsonl" --audit "$demo/audit.jsonl"
"$binary" check --input "$demo/fixtures/pages.jsonl" --output "$demo/output.jsonl" --audit "$demo/audit.jsonl" --expected "$demo/fixtures/expected.jsonl" --sentinels "$demo/fixtures/sentinels.json"

"$binary" remap --input "$demo/fixtures/pages_shuffled_duplicated.jsonl" --output "$demo/variant-output.jsonl" --audit "$demo/variant-audit.jsonl"
cmp "$demo/output.jsonl" "$demo/variant-output.jsonl"
cmp "$demo/audit.jsonl" "$demo/variant-audit.jsonl"

set +e
"$binary" remap --input "$demo/fixtures/pages.jsonl" --output "$demo/resume-a-output.jsonl" --audit "$demo/resume-a-audit.jsonl" --stop-after 73
interrupted_a=$?
set -e
test "$interrupted_a" -eq 75
"$binary" remap --input "$demo/fixtures/pages.jsonl" --output "$demo/resume-a-output.jsonl" --audit "$demo/resume-a-audit.jsonl" --resume
cmp "$demo/output.jsonl" "$demo/resume-a-output.jsonl"
cmp "$demo/audit.jsonl" "$demo/resume-a-audit.jsonl"

set +e
"$binary" remap --input "$demo/fixtures/pages.jsonl" --output "$demo/resume-b-output.jsonl" --audit "$demo/resume-b-audit.jsonl" --stop-after 191
interrupted_b=$?
set -e
test "$interrupted_b" -eq 75
"$binary" remap --input "$demo/fixtures/pages.jsonl" --output "$demo/resume-b-output.jsonl" --audit "$demo/resume-b-audit.jsonl" --resume
cmp "$demo/output.jsonl" "$demo/resume-b-output.jsonl"
cmp "$demo/audit.jsonl" "$demo/resume-b-audit.jsonl"

echo "demo passed: exact rectangles, content-free output, duplicate identity, and two resumes"
