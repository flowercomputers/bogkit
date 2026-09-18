#!/bin/sh
# Run from any directory; no production credentials or services are needed.
set -eu
repo=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
cd "$repo"
cargo build --locked -p bog-cloud-records --bin bog-records-worker
cargo test --locked -p fold -p bog-serve -p bog-definition -p bog-runtime -p bog-cloud-records -p bog-cloud -p bog-cloud-mcp
cargo test --locked -p ese --test pinning
node --test cloud/tests/js/*.test.cjs
