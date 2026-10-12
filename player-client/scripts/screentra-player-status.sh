#!/usr/bin/env bash
set -euo pipefail
INSTALL_DIR="${RIPLEY_CLIENT_DIR:-$HOME/.local/share/screentra-client}"
CACHE="$INSTALL_DIR/cache"
CONFIG="$CACHE/client-config.json"
RENDERER_URL="${RIPLEY_RENDERER_URL:-http://127.0.0.1:3900}"
SERVER_URL="${RIPLEY_SERVER_URL:-$(cat "$INSTALL_DIR/server" 2>/dev/null || true)}"

echo "Screentra Cast Player Status"
echo "===================="
echo "Install dir: $INSTALL_DIR"
echo "Renderer:    $RENDERER_URL"
echo "Server:      ${SERVER_URL:-not configured}"
echo

echo "Cache files:"
ls -1 "$CACHE" 2>/dev/null || echo "  (none)"
echo

echo "Renderer health:"
if command -v curl >/dev/null 2>&1; then
  curl -fsS --max-time 4 "$RENDERER_URL/health" || echo "  renderer not reachable"
else
  python3 - <<PY 2>/dev/null || echo "  renderer not reachable"
import urllib.request
print(urllib.request.urlopen("$RENDERER_URL/health", timeout=4).read().decode())
PY
fi
echo

if [ -f "$CONFIG" ]; then
  echo "Assigned layout:"
  python3 - <<PY
import json
d=json.load(open("$CONFIG"))
print("  client_id:           ", d.get("client_id"))
print("  assigned_layout_id:  ", d.get("assigned_layout_id"))
print("  server_time:         ", d.get("server_time"))
layout=d.get("layout") or {}
print("  layout name:         ", layout.get("name"))
print("  layout widgets:      ", len(layout.get("widgets") or []))
PY
else
  echo "No cached client-config.json yet (heartbeat may not have run)."
fi
echo

if command -v systemctl >/dev/null 2>&1; then
  echo "Services:"
  for svc in screentra-client-heartbeat screentra-client-renderer screentra-kiosk-browser; do
    state=$(systemctl --user is-active "$svc.service" 2>/dev/null || echo "inactive")
    enabled=$(systemctl --user is-enabled "$svc.service" 2>/dev/null || echo "disabled")
    printf "  %-32s active=%-9s enabled=%s\n" "$svc" "$state" "$enabled"
  done
else
  echo "systemctl unavailable; skipping service status."
fi

echo
echo "Recent heartbeat log lines:"
LOG_DIR="${XDG_STATE_HOME:-$HOME/.local/state}/screentra-client"
ls -1t "$LOG_DIR"/*.log 2>/dev/null | head -3 | while read f; do
  echo "--- $f ---"
  tail -n 5 "$f"
done || true
