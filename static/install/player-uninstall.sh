#!/usr/bin/env bash
set -euo pipefail

SERVER_URL="${RIPLEY_SERVER_URL:-}"
INSTALL_DIR="${SCREENTRA_CLIENT_DIR:-$HOME/.local/share/screentra-client-client}"
BIN_DIR="$HOME/.local/bin"
SYSTEMD_USER_DIR="$HOME/.config/systemd/user"
CLIENT_ID_FILE="$INSTALL_DIR/client-id"
UNREGISTER="${RIPLEY_UNREGISTER:-0}"

CLIENT_ID=""
if [ -r "$CLIENT_ID_FILE" ]; then
  CLIENT_ID="$(tr -d '\n\r' < "$CLIENT_ID_FILE")"
fi

echo "Stopping Ripley player services..."
if command -v systemctl >/dev/null 2>&1; then
  systemctl --user stop screentra-client-heartbeat.service 2>/dev/null || true
  for i in 0 1 2 3 4 5 6 7; do
    systemctl --user stop "ripley-kiosk-browser@$i.service" 2>/dev/null || true
    systemctl --user stop "screentra-client-renderer@$i.service" 2>/dev/null || true
    systemctl --user disable "ripley-kiosk-browser@$i.service" 2>/dev/null || true
    systemctl --user disable "screentra-client-renderer@$i.service" 2>/dev/null || true
  done
  systemctl --user disable screentra-client-heartbeat.service screentra-client-actor.service 2>/dev/null || true
fi

pkill -f screentra-client-renderer.py 2>/dev/null || true
pkill -f screentra-client-heartbeat.py 2>/dev/null || true
pkill -f ripley-kiosk-launcher.sh 2>/dev/null || true

rm -f "$SYSTEMD_USER_DIR"/screentra-client-heartbeat.service \
      "$SYSTEMD_USER_DIR"/screentra-client-renderer@.service \
      "$SYSTEMD_USER_DIR"/ripley-kiosk-browser@.service \
      "$SYSTEMD_USER_DIR"/screentra-client-actor.service
rm -f "$BIN_DIR"/screentra-client-preflight \
      "$BIN_DIR"/screentra-client-status \
      "$BIN_DIR"/screentra-client-heartbeat \
      "$BIN_DIR"/screentra-client-renderer \
      "$BIN_DIR"/screentra-client-actor \
      "$BIN_DIR"/ripley-kiosk-launcher
rm -rf "$INSTALL_DIR"

if command -v systemctl >/dev/null 2>&1; then
  systemctl --user daemon-reload 2>/dev/null || true
  systemctl --user reset-failed 2>/dev/null || true
fi

if [ "$UNREGISTER" = "1" ] && [ -n "$CLIENT_ID" ]; then
  python3 - <<PY || true
import urllib.request
server = "$SERVER_URL".rstrip('/')
client = "$CLIENT_ID"
try:
    req = urllib.request.Request(f"{server}/api/clients/{client}", method='DELETE')
    with urllib.request.urlopen(req, timeout=10) as r:
        print(f"Unregistered {client}: HTTP {r.status}")
except Exception as e:
    print(f"Unregister skipped/failed: {e}")
PY
fi

echo "Ripley player client uninstalled."
if [ -n "$CLIENT_ID" ]; then
  echo "Former Client ID: $CLIENT_ID"
fi
