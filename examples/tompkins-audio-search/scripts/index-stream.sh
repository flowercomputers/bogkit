#!/usr/bin/env bash
# Run every analysis track over an already-compacted stream, then commit.
#
# Assumes the media is already fetched and compacted — that step costs requests
# against a bucket someone else pays for, so it stays a separate, deliberate
# command (tools/compact.mjs). Everything here runs against the local copy and
# can be re-run freely: each worker skips assets whose batch is already complete
# and checksum-clean, and the Bog commit is idempotent by stable key.
#
#   scripts/index-stream.sh 9422 <pts-file> <assets-file>
#
# Ordering matters in one place: tag calibration must run after CLAP has seen
# every window, because a label's threshold is derived from its distribution
# over the whole corpus.

# Deliberately not `set -e`. The stages are independent — CLAP, BirdNET and
# speech share nothing but their input — so a failure in one must not cancel
# the ones behind it. A BirdNET crash once took the speech pass with it, which
# is a worse outcome than losing bird detections. Failures are collected and
# reported at the end instead.
set -uo pipefail

FAILED=()
stage() {
  local name="$1"; shift
  echo
  echo "== $name =="
  if ! "$@"; then
    echo "!! $name failed (exit $?); continuing with the remaining stages"
    FAILED+=("$name")
  fi
}

STREAM="${1:?usage: index-stream.sh <streamId> [pts-file] [assets-file]}"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$HERE"

PTS="${2:-$(ls -1 data/assets/$STREAM/pts-*.json | head -1)}"
ASSETS="${3:-$(ls -1 data/assets/$STREAM/assets-*.json | head -1)}"
PY=pipelines/.venv/bin/python
BIN=../../target/debug/tompkins-audio-search

echo "== stream $STREAM =="
echo "   pts    $PTS"
echo "   assets $ASSETS"

cd "$HERE"
stage "timeline" $BIN timeline "$STREAM"

stage "CLAP acoustic windows" \
  ./pipelines/.venv/bin/python pipelines/clap_windows.py "$STREAM" \
  --asset-dir "data/assets/$STREAM" --out "data/prepared/$STREAM" \
  --prompts pipelines/prompt_bank.json

stage "tag calibration" \
  ./pipelines/.venv/bin/python pipelines/tag_calibrate.py "data/prepared/$STREAM" \
  --prompts pipelines/prompt_bank.json

stage "BirdNET detections" \
  ./pipelines/.venv/bin/python pipelines/bird_frames.py "$STREAM" \
  --asset-dir "data/assets/$STREAM" --out "data/prepared/$STREAM" \
  --recording-date "${RECORDING_DATE:-2021-04-14}"

stage "speech" \
  ./pipelines/.venv/bin/python pipelines/speech_spans.py "$STREAM" \
  --asset-dir "data/assets/$STREAM" --out "data/prepared/$STREAM"

stage "commit" $BIN commit "$STREAM" --prepared data/prepared

echo
if [ ${#FAILED[@]} -eq 0 ]; then
  echo "all stages completed."
else
  echo "completed with ${#FAILED[@]} failed stage(s): ${FAILED[*]}"
fi
echo "the server picks up new batches at /api/ingest/$STREAM without a restart."
