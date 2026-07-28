#!/usr/bin/env bash
# Generates the GitHub release body from conventional-commit subjects.
# tauri-action copies this body verbatim into latest.json's "notes" field, which is what
# the in-app update panel displays — so this is user-facing text, not a changelog for us.
set -euo pipefail

PREV="${1:-}"
CURRENT="${2:-}"
REPO_URL="https://github.com/rhurling/convertbar"

if [ -z "$PREV" ]; then
  echo "Initial release."
  exit 0
fi

subjects=$(git log --pretty=format:%s "${PREV}..${CURRENT}" 2>/dev/null || true)

emit_group() {
  local heading="$1" pattern="$2" matched
  matched=$(printf '%s\n' "$subjects" | grep -E "$pattern" || true)
  [ -z "$matched" ] && return 0
  printf '### %s\n' "$heading"
  # Strip the type prefix and any scope: "feat(ui): x" -> "x"
  printf '%s\n' "$matched" | sed -E 's/^[a-z]+(\([^)]*\))?!?: *//' | sed 's/^/- /'
  printf '\n'
}

emit_group "Features"    '^feat(\([^)]*\))?!?: '
emit_group "Fixes"       '^fix(\([^)]*\))?!?: '
emit_group "Performance" '^perf(\([^)]*\))?!?: '

# Everything else is collapsed to a count: dependabot subjects would otherwise dominate
# the body and bury the changes a user actually cares about.
other=$(printf '%s\n' "$subjects" \
  | grep -vE '^(feat|fix|perf)(\([^)]*\))?!?: ' \
  | grep -c . || true)
if [ "${other:-0}" -gt 0 ]; then
  printf '%s maintenance changes (dependencies, docs, tests, internals).\n\n' "$other"
fi

printf '**Full changelog**: %s/compare/%s...%s\n' "$REPO_URL" "$PREV" "$CURRENT"
