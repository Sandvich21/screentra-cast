#!/usr/bin/env bash
# Ripley client installer (wrapper).
#
# Downloads the player-client.sh from the source server and runs it with
# RIPLEY_SERVER_URL forced to the configured value. Designed for use on
# a fresh test PC where the server is also being installed.
#
# Usage:
#   RIPLEY_SERVER_URL=http://localhost:3000 bash ripley-client-install.sh
#   RIPLEY_SERVER_URL=http://10.0.0.5:3000 \
#     curl -fsSL http://10.0.0.5:3000/install/ripley-client-install.sh | bash
#
# Environment variables (all forwarded to player-client.sh):
#   RIPLEY_SERVER_URL        Server URL (default: http://localhost:3000)
#   RIPLEY_CLIENT_ROLE       client role (default: player)
#   RIPLEY_CLIENT_NAME       client name (default: hostname)
#   RIPLEY_CLIENT_VERSION    client version string (default: empty)
#   RIPLEY_HEARTBEAT_INTERVAL  heartbeat seconds (default: 30)

set -euo pipefail

RIPLEY_SERVER_URL="${RIPLEY_SERVER_URL:-http://localhost:3000}"

log() { printf '%s\n' "[ripley-client-install] $*" >&2; }

if command -v curl >/dev/null 2>&1; then
  DOWNLOADER="curl"
elif command -v wget >/dev/null 2>&1; then
  DOWNLOADER="wget"
elif command -v python3 >/dev/null 2>&1; then
  DOWNLOADER="python3"
else
  log "ERROR: one of curl, wget, or python3 is required"
  exit 1
fi
command -v bash >/dev/null || { log "ERROR: bash is required"; exit 1; }

log "Fetching player-client.sh from ${RIPLEY_SERVER_URL}/install/player-client.sh (via $DOWNLOADER)"
tmp="$(mktemp)"
trap 'rm -f "$tmp"' EXIT
case "$DOWNLOADER" in
  curl)
    curl -fSL --retry 3 --connect-timeout 10 -o "$tmp" "${RIPLEY_SERVER_URL}/install/player-client.sh" || fail "curl download failed"
    ;;
  wget)
    wget -q --tries=3 --timeout=10 -O "$tmp" "${RIPLEY_SERVER_URL}/install/player-client.sh" || fail "wget download failed"
    ;;
  python3)
    python3 - "${RIPLEY_SERVER_URL}/install/player-client.sh" "$tmp" <<'PY' || fail "python download failed"
import sys, urllib.request
with urllib.request.urlopen(sys.argv[1], timeout=10) as r:
    with open(sys.argv[2], 'wb') as f:
        f.write(r.read())
PY
    ;;
esac
[ -s "$tmp" ] || { log "ERROR: downloaded file is empty"; exit 1; }

# Sanity check: first line should be a shebang
if ! head -1 "$tmp" | grep -q '^#!'; then
  log "ERROR: Downloaded file is not a bash script"
  exit 1
fi

log "Running player-client.sh with RIPLEY_SERVER_URL=${RIPLEY_SERVER_URL}"
exec "/bin/bash" "$tmp"
