#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
debug_bind="${MUDGARDEN_DEBUG_BIND:-127.0.0.1:2223}"

if command -v security >/dev/null 2>&1; then
  keychain_api_key="$(
    security find-generic-password \
      -a "$(id -un)" \
      -s mudgarden-openai-api-key \
      -w 2>/dev/null || true
  )"
  if [ -n "$keychain_api_key" ]; then
    export OPENAI_API_KEY="$keychain_api_key"
  fi
fi

case "$debug_bind" in
  "" | off | disabled)
    echo "MUDGARDEN_DEBUG_BIND must be enabled to open the visualizer." >&2
    exit 1
    ;;
esac

debug_host="${debug_bind%:*}"
debug_port="${debug_bind##*:}"
if [ "$debug_host" = "$debug_bind" ] || ! [[ "$debug_port" =~ ^[0-9]+$ ]]; then
  echo "MUDGARDEN_DEBUG_BIND must use host:port format." >&2
  exit 1
fi

case "$debug_host" in
  0.0.0.0 | "::" | "[::]")
    browser_host="127.0.0.1"
    ;;
  *)
    browser_host="$debug_host"
    ;;
esac

visualizer_url="http://${browser_host}:${debug_port}"
health_url="${visualizer_url}/api/debug/health"

open_visualizer() {
  echo "Opening ${visualizer_url}"
  if [ "${MUDGARDEN_NO_OPEN:-0}" = "1" ]; then
    return
  fi
  if command -v open >/dev/null 2>&1; then
    open "$visualizer_url"
  elif command -v xdg-open >/dev/null 2>&1; then
    xdg-open "$visualizer_url"
  elif command -v cmd.exe >/dev/null 2>&1; then
    cmd.exe /C start "" "$visualizer_url"
  else
    echo "No browser opener found; visit ${visualizer_url}" >&2
  fi
}

if curl --silent --fail --max-time 1 "$health_url" >/dev/null 2>&1; then
  echo "MUDGarden is already running."
  open_visualizer
  exit 0
fi

cd "$repo_root/examples/mudgarden"
cargo run -p mudgarden &
server_pid=$!

stop_server() {
  if kill -0 "$server_pid" >/dev/null 2>&1; then
    kill -INT "$server_pid" >/dev/null 2>&1 || true
    wait "$server_pid" 2>/dev/null || true
  fi
}
trap stop_server EXIT INT TERM

for _ in {1..600}; do
  if curl --silent --fail --max-time 1 "$health_url" >/dev/null 2>&1; then
    open_visualizer
    break
  fi
  if ! kill -0 "$server_pid" >/dev/null 2>&1; then
    wait "$server_pid"
    exit $?
  fi
  sleep 0.25
done

if ! curl --silent --fail --max-time 1 "$health_url" >/dev/null 2>&1; then
  echo "MUDGarden did not become ready at ${visualizer_url}." >&2
  exit 1
fi

set +e
wait "$server_pid"
status=$?
set -e
trap - EXIT INT TERM
exit "$status"
