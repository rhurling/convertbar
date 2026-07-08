#!/usr/bin/env bash
# Fail the Stop if ConvertBar's three version manifests drift apart.
# Wired in as a Stop hook in .claude/settings.json.
#
# On mismatch it prints the warning to stderr and exits 2, which blocks the Stop and
# feeds the message back to Claude so the drift is actually acted on. Exit-0 stdout from
# a Stop hook is only surfaced in transcript mode — effectively invisible in normal use,
# which defeats the point of a drift check (see docs/fable-review/DECISIONS.md D7).
set -u
d="${CLAUDE_PROJECT_DIR:-.}"
command -v jq >/dev/null 2>&1 || exit 0

# Loop guard: when this Stop was already blocked once, let it through.
if jq -e '.stop_hook_active == true' >/dev/null 2>&1 <<<"$(cat 2>/dev/null || true)"; then
  exit 0
fi

tv=$(jq -r '.version // empty' "$d/src-tauri/tauri.conf.json" 2>/dev/null)
pv=$(jq -r '.version // empty' "$d/package.json" 2>/dev/null)
cv=$(grep -m1 -E '^version *=' "$d/src-tauri/Cargo.toml" 2>/dev/null | sed -E 's/.*"([^"]*)".*/\1/')

if [ -n "$tv" ] && { [ "$tv" != "$pv" ] || [ "$tv" != "$cv" ]; }; then
  echo "⚠️  Version mismatch — tauri.conf.json=$tv  package.json=$pv  Cargo.toml=$cv. Sync all three before committing/tagging (see /release)." >&2
  exit 2
fi
exit 0
