#!/usr/bin/env bash
# SessionStart hook: refresh docs/agents/org-decisions.md from keelstackhq/decisions.
# Works with gh or git credentials; without them the committed copy stays. Never fails a session.
set -u
dest="${CLAUDE_PROJECT_DIR:-.}/docs/agents/org-decisions.md"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
src=""
if command -v gh >/dev/null 2>&1 &&
  gh api -H "Accept: application/vnd.github.raw" repos/keelstackhq/decisions/contents/DECISIONS.md \
    >"$tmp/DECISIONS.md" 2>/dev/null && [ -s "$tmp/DECISIONS.md" ]; then
  src="$tmp/DECISIONS.md"
elif GIT_TERMINAL_PROMPT=0 git clone -q --depth 1 https://github.com/keelstackhq/decisions "$tmp/repo" \
  2>/dev/null && [ -s "$tmp/repo/DECISIONS.md" ]; then
  src="$tmp/repo/DECISIONS.md"
fi
if [ -z "$src" ]; then
  echo "org decisions: keelstackhq/decisions not reachable; using the committed copy"
elif cmp -s "$src" "$dest"; then
  echo "org decisions: up to date"
else
  mkdir -p "$(dirname "$dest")"
  cp "$src" "$dest"
  echo "org decisions: updated docs/agents/org-decisions.md; commit it alone as 'chore: sync org decisions'"
fi
exit 0
