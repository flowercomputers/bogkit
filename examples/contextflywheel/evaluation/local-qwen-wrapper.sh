#!/bin/sh
set -eu

mode="$1"
fixture="$2"
data="$3"

project_root="/Users/alhinai/Documents/Codex/2026-08-16/event-details-event-links-partiful-luma/work/bogkit"
binary="$project_root/target/debug/contextflywheel"
plugin="$project_root/examples/contextflywheel/claude-plugin"
claude_bin="/Users/alhinai/.local/bin/claude"

api_key="$(security find-generic-password -a "$USER" -s "claude-local-llm-api-key" -w)"
export ANTHROPIC_BASE_URL="http://llm.alhinai.dev"
export ANTHROPIC_API_KEY="$api_key"
export ANTHROPIC_MODEL="sonnet"
export ANTHROPIC_DEFAULT_SONNET_MODEL="claude-sonnet-4-6"
export CONTEXTFLYWHEEL_BIN="$binary"
export CONTEXTFLYWHEEL_HOME="$data"

prompt='Diagnose and fix the failing timeout fixture in this directory. Do not modify tests or public function signatures. Begin by running the tests and profiler. The README suspects database latency, but treat that only as a hypothesis and use evidence. Make the smallest correct source change, run the full tests, and finish with a concise evidence-backed explanation.'

if [ "$mode" != "raw" ]; then
  "$binary" --data "$data" init \
    --objective "Diagnose and fix the isolated timeout fixture" \
    --constraint "Work only inside the copied fixture" \
    --constraint "Do not modify tests or public function signatures" \
    --permission "Read and edit the disposable fixture; run local tests"
  prompt="$prompt Record evidence and revise the current ContextFlywheel belief before changing investigation direction."
fi

if [ "$mode" = "hybrid" ]; then
  export CONTEXTFLYWHEEL_MODE="hybrid"
  export CONTEXTFLYWHEEL_RANK="0"
fi

if [ "$mode" = "raw" ]; then
  exec "$claude_bin" -p --model sonnet --effort high --dangerously-skip-permissions \
    --no-session-persistence --output-format json "$prompt"
else
  exec "$claude_bin" -p --model sonnet --effort high --dangerously-skip-permissions \
    --no-session-persistence --output-format json --plugin-dir "$plugin" "$prompt"
fi
