#!/usr/bin/env bash
# Signs, checks and zips the macOS convertbar-server binary for a release (ADR 0001).
#
#   scripts/package-server-macos.sh <binary> <signing identity> <zip out>
#
# KEYCHAIN, when set, names the keychain holding the identity (CI's temporary one). The
# signature must be Developer ID with hardened runtime and a fixed identifier: macOS keys the
# LaunchAgent's local-network grant on it, and an ad-hoc signature changes with every build.
# Notarization is CI's next step; a bare binary cannot be stapled.
set -euo pipefail

bin=${1:?binary}
identity=${2:?signing identity}
zip=${3:?zip output path}
identifier=com.convertbar.server

keychain_args=()
[ -n "${KEYCHAIN:-}" ] && keychain_args=(--keychain "$KEYCHAIN")

codesign --force --options runtime --timestamp --identifier "$identifier" \
  "${keychain_args[@]}" --sign "$identity" "$bin"

codesign --verify --strict --verbose=2 "$bin"
desc=$(codesign -dvv "$bin" 2>&1)
grep -q 'flags=.*runtime' <<<"$desc" || { echo "::error::hardened runtime missing" >&2; exit 1; }
grep -q 'Authority=Developer ID Application' <<<"$desc" \
  || { echo "::error::not signed with a Developer ID Application cert" >&2; exit 1; }
grep -q "^Identifier=$identifier\$" <<<"$desc" \
  || { echo "::error::identifier is not $identifier" >&2; exit 1; }
[ "$(lipo -archs "$bin")" = arm64 ] || { echo "::error::not an arm64-only binary" >&2; exit 1; }

# Smoke test: the signed binary starts under hardened runtime and serves the embedded web UI.
# Bash's /dev/tcp, so the check needs nothing beyond the shell.
data=$(mktemp -d)
port=$(( 20000 + RANDOM % 20000 ))
CONVERTBAR_NO_AUTH=1 CONVERTBAR_BIND=127.0.0.1 CONVERTBAR_PORT=$port CONVERTBAR_DATA_DIR=$data \
  "$bin" >"$data/server.log" 2>&1 &
pid=$!
trap 'kill "$pid" 2>/dev/null || true' EXIT
page=""
for _ in $(seq 1 50); do
  if { exec 3<>"/dev/tcp/127.0.0.1/$port"; } 2>/dev/null; then
    printf 'GET / HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n' >&3
    page=$(cat <&3)
    exec 3<&-
    break
  fi
  sleep 0.2
done
grep -q '<div id="root">' <<<"$page" || {
  echo "::error::the signed server did not serve the web UI" >&2
  cat "$data/server.log" >&2
  exit 1
}
kill "$pid"
wait "$pid" 2>/dev/null || true
trap - EXIT

# ditto, not zip: Apple's recommended archiver for notarization. The binary sits at the root.
ditto -c -k "$bin" "$zip"
echo "packaged $zip"
