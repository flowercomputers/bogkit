#!/bin/sh
set -eu

binary="${CONTEXTFLYWHEEL_BIN:-contextflywheel}"
exec "$binary" --data "${CONTEXTFLYWHEEL_HOME:-.contextflywheel}" hook --event "$1"
