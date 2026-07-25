#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"

if ! command -v railway >/dev/null 2>&1; then
  echo "Railway CLI is required: https://docs.railway.com/guides/cli" >&2
  exit 1
fi

cd "$repo_root"
exec railway up \
  --service "${RAILWAY_SERVICE:-mudgarden}" \
  --environment "${RAILWAY_ENVIRONMENT:-production}" \
  "$@"
