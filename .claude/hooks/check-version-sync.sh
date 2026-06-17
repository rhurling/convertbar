#!/usr/bin/env bash
# Warn (non-blocking) if ConvertBar's three version manifests drift apart.
# Wired in as a Stop hook in .claude/settings.json.
set -u
d="${CLAUDE_PROJECT_DIR:-.}"
command -v jq >/dev/null 2>&1 || exit 0

tv=$(jq -r '.version // empty' "$d/src-tauri/tauri.conf.json" 2>/dev/null)
pv=$(jq -r '.version // empty' "$d/package.json" 2>/dev/null)
cv=$(grep -m1 -E '^version *=' "$d/src-tauri/Cargo.toml" 2>/dev/null | sed -E 's/.*"([^"]*)".*/\1/')

if [ -n "$tv" ] && { [ "$tv" != "$pv" ] || [ "$tv" != "$cv" ]; }; then
  echo "⚠️  Version mismatch — tauri.conf.json=$tv  package.json=$pv  Cargo.toml=$cv. Sync all three before committing/tagging (see /release)."
fi
exit 0
