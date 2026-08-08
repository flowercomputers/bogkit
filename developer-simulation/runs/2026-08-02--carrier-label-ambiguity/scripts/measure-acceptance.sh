#!/bin/sh
set -eu

if [ "$#" -ne 3 ]; then
  echo "usage: $0 BINARY RUN_DIRECTORY OUTPUT_LOG" >&2
  exit 2
fi

binary=$1
run_directory=$2
output_log=$3

"$binary" acceptance --dir "$run_directory" --shipments 20000 --seeds 30 >"$output_log" 2>&1 &
worker_pid=$!
peak_rss_kib=0
sample_count=0

while kill -0 "$worker_pid" 2>/dev/null; do
  current_rss_kib=$(ps -o rss= -p "$worker_pid" 2>/dev/null | tr -d ' ' || true)
  if [ -n "$current_rss_kib" ]; then
    sample_count=$((sample_count + 1))
    if [ "$current_rss_kib" -gt "$peak_rss_kib" ]; then
      peak_rss_kib=$current_rss_kib
    fi
  fi
  sleep 0.02
done

wait "$worker_pid"
cat "$output_log"
if [ "$sample_count" -eq 0 ]; then
  echo "MEASUREMENT FAILED: ps returned no valid RSS samples" >&2
  exit 1
fi
peak_rss_mib=$(awk "BEGIN { printf \"%.2f\", $peak_rss_kib / 1024 }")
echo "MEASURED PEAK RSS: ${peak_rss_kib} KiB (${peak_rss_mib} MiB)"
