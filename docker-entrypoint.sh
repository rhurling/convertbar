#!/bin/sh
# Start as root so entrypoint hooks that need it (e.g. Unraid's Tailscale
# integration) can run, then drop to PUID:PGID before exec'ing the server.
set -e

PUID="${PUID:-0}"
PGID="${PGID:-0}"
DATA_DIR="${CONVERTBAR_DATA_DIR:-/config}"

# Not root (docker run --user): can't chown or setpriv, so PUID/PGID are moot.
# Also taken when both stay at 0, i.e. privilege dropping is not requested.
if [ "$(id -u)" != "0" ] || { [ "$PUID" = "0" ] && [ "$PGID" = "0" ]; }; then
  exec /usr/local/bin/convertbar-server "$@"
fi

chown -R "$PUID:$PGID" "$DATA_DIR" || true

# --clear-groups: without it the process keeps root's supplementary groups,
# including GID 0.
exec setpriv --reuid="$PUID" --regid="$PGID" --clear-groups \
  env HOME="$DATA_DIR" /usr/local/bin/convertbar-server "$@"
