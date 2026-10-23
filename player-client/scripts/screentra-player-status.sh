#!/usr/bin/env bash
set -euo pipefail
INSTALL_DIR="${RIPLEY_CLIENT_DIR:-$HOME/.local/share/screentra-client}"
CACHE="$INSTALL_DIR/cache"
CONFIG="$CACHE/client-config.json"
RENDERER_URL="${RIPLEY_RENDERER_URL:-http://127.0.0.1:3900}"
SERVER_URL="${RIPLEY_SERVER_URL:-$(head -c 4096 "$INSTALL_DIR/server" 2>/dev/null || true)}"

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
  curl -fsS --max-time 4 --max-filesize 65536 "$RENDERER_URL/health" || echo "  renderer not reachable"
else
  RENDERER_URL="$RENDERER_URL" python3 - <<'PY' 2>/dev/null || echo "  renderer not reachable"
import os
import urllib.request
url = os.environ['RENDERER_URL'].rstrip('/') + '/health'
with urllib.request.urlopen(url, timeout=4) as response:
    data = response.read(65537)
if len(data) > 65536:
    raise ValueError('renderer health response exceeds its bound')
print(data.decode('utf-8'))
PY
fi
echo

if [ -f "$CONFIG" ]; then
  echo "Assigned layout:"
  CONFIG_PATH="$CONFIG" python3 - <<'PY'
import json
import os
config_path = os.environ['CONFIG_PATH']

def reject_duplicate_fields(pairs):
    value = {}
    for key, item in pairs:
        if key in value:
            raise ValueError('duplicate JSON field')
        value[key] = item
    return value

with open(config_path, 'rb') as handle:
    raw = handle.read((1 << 20) + 1)
if len(raw) > (1 << 20):
    raise ValueError('client config exceeds its bound')
d = json.loads(
    raw.decode('utf-8'),
    object_pairs_hook=reject_duplicate_fields,
    parse_constant=lambda _: (_ for _ in ()).throw(ValueError('non-standard JSON constant')),
)
if not isinstance(d, dict):
    raise ValueError('client config must be an object')
print("  client_id:           ", d.get("client_id"))
print("  assigned_layout_id:  ", d.get("assigned_layout_id"))
print("  server_time:         ", d.get("server_time"))
layout=d.get("layout") or {}
if not isinstance(layout, dict):
    layout = {}
widgets = layout.get("widgets") or []
print("  layout name:         ", layout.get("name"))
print("  layout widgets:      ", min(len(widgets) if isinstance(widgets, list) else 0, 128))
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
