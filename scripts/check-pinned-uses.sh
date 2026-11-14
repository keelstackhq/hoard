#!/usr/bin/env bash
# Fail if any workflow or action step uses a remote action not pinned to a full 40-hex SHA.
# Local actions (`./`, `./path`) are fine.
set -euo pipefail
cd "$(dirname "$0")/.."
bad=$(grep -rnE '^\s*-?\s*uses:' .github/workflows action.yml \
  | grep -vE 'uses:\s*\./' \
  | grep -vE 'uses:\s*[^ #@]+@[0-9a-f]{40}(\s|$)' || true)
if [ -n "$bad" ]; then
  echo "::error::pin these 'uses:' to a full commit SHA with a version comment:"
  echo "$bad"
  exit 1
fi
echo "all uses: pinned"
