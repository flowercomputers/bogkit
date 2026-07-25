#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
ssh_bind="${MUDGARDEN_BIND:-127.0.0.1:2222}"
debug_bind="${MUDGARDEN_DEBUG_BIND:-127.0.0.1:2223}"

port_from_bind() {
  local bind="$1"
  local port="${bind##*:}"

  if [ "$port" = "$bind" ] || ! [[ "$port" =~ ^[0-9]+$ ]]; then
    echo "Invalid bind address: ${bind}. Expected host:port." >&2
    exit 1
  fi

  printf '%s\n' "$port"
}

if ! command -v lsof >/dev/null 2>&1; then
  echo "lsof is required to find the running MUDGarden process." >&2
  exit 1
fi

ssh_port="$(port_from_bind "$ssh_bind")"
ports="$ssh_port"
case "$debug_bind" in
  "" | off | disabled)
    ;;
  *)
    debug_port="$(port_from_bind "$debug_bind")"
    ports="${ports} ${debug_port}"
    ;;
esac

listener_pids="$(
  for port in $ports; do
    lsof -nP -t -iTCP:"$port" -sTCP:LISTEN 2>/dev/null || true
  done | sort -u
)"

if [ -n "$listener_pids" ]; then
  for pid in $listener_pids; do
    command_name="$(ps -p "$pid" -o comm= | xargs)"
    if [ "${command_name##*/}" != "mudgarden" ]; then
      echo "Refusing to stop ${command_name:-unknown process} (PID ${pid}) on a MUDGarden port." >&2
      exit 1
    fi
  done

  echo "Stopping MUDGarden..."
  for pid in $listener_pids; do
    kill -INT "$pid"
  done

  for _ in {1..40}; do
    still_running=0
    for pid in $listener_pids; do
      if kill -0 "$pid" 2>/dev/null; then
        still_running=1
        break
      fi
    done
    if [ "$still_running" -eq 0 ]; then
      break
    fi
    sleep 0.25
  done

  for pid in $listener_pids; do
    if kill -0 "$pid" 2>/dev/null; then
      echo "MUDGarden did not stop within 10 seconds (PID ${pid})." >&2
      exit 1
    fi
  done
else
  echo "MUDGarden is not running; starting it now."
fi

exec "$repo_root/examples/mudgarden/scripts/run-mudgarden.sh" "$@"
