#!/usr/bin/env bash
# Fetch and compact successive windows of one stream until disk runs low.
#
# Ingesting a 358-hour stream in one go needs ~110 GB and hours of decoding, so
# it goes in windows: fetch, probe, remux, then drop the source segments — the
# compacted assets and the PTS table are everything downstream needs, and the
# `.ts` files are only there for resumability during the fetch itself.
#
# Each window costs its segment count in GetObject calls against a bucket
# someone else pays for, so the count is explicit and the loop stops at a disk
# floor rather than filling the volume.
#
#   scripts/fetch-windows.sh 9422 387029 4        # 4 more windows from segment 387029
#   KEEP_CACHE=1 scripts/fetch-windows.sh 9422 387029 1
#
# Run `timeline`, then the analysis workers, once the windows you want are down:
# the timeline command merges every pts-*.json and assets-*.json it finds.

set -euo pipefail

STREAM="${1:?usage: fetch-windows.sh <streamId> <fromSegment> [windows]}"
FROM="${2:?usage: fetch-windows.sh <streamId> <fromSegment> [windows]}"
WINDOWS="${3:-1}"

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$HERE"

# 24 h at the ~1.989 s/segment this archive actually averages
COUNT="${WINDOW_SEGMENTS:-43085}"
# stop before the volume is full; a failed remux mid-window wastes the fetch
FLOOR_GB="${DISK_FLOOR_GB:-12}"

TOTAL=$(python3 -c "
import json; print(json.load(open('data/segments/stream-$STREAM-stream_4.json'))['objectCount'])")

free_gb() { df -g "$HOME" | tail -1 | awk '{print $4}'; }

for w in $(seq 1 "$WINDOWS"); do
  if [ "$FROM" -ge "$TOTAL" ]; then
    echo "reached the end of stream $STREAM at segment $TOTAL"
    break
  fi
  remaining=$((TOTAL - FROM))
  count=$(( remaining < COUNT ? remaining : COUNT ))

  avail=$(free_gb)
  if [ "$avail" -lt "$FLOOR_GB" ]; then
    echo "stopping: ${avail} GB free is below the ${FLOOR_GB} GB floor"
    break
  fi

  echo
  echo "=== window $w/$WINDOWS: segments $FROM..$((FROM + count - 1)) ($count GETs, ${avail} GB free) ==="
  node tools/compact.mjs "$STREAM" --from "$FROM" --count "$count" \
    --asset-minutes 30 --budget $((count + 100))

  if [ -z "${KEEP_CACHE:-}" ]; then
    # the assets and pts table are what downstream reads; the source segments
    # are re-fetchable and cost disk we need for the next window
    echo "dropping source cache for this window"
    rm -rf "data/cache/$STREAM"
  fi

  FROM=$((FROM + count))
done

echo
echo "fetched through segment $FROM of $TOTAL"
echo "next: cargo run -p tompkins-audio-search -- timeline $STREAM   (merges every window)"
echo "then: scripts/index-stream.sh $STREAM"
