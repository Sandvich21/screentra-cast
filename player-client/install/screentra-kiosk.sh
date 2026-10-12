#!/bin/bash
# screentra-kiosk.sh — Sway compositor plus one cached renderer/browser per display.
# The compositor is shared because one DRM device cannot be owned by multiple
# Sway instances. Each physical output gets its own local renderer and browser.
set -euo pipefail

LOG="${LOG:-/home/board/screentra-kiosk.log}"
BOARD_USER="${SCREENTRA_BOARD_USER:-board}"
BOARD_HOME="$(getent passwd "$BOARD_USER" | cut -d: -f6)"
: "${BOARD_HOME:=/home/$BOARD_USER}"
BOARD_UID="$(id -u "$BOARD_USER" 2>/dev/null || echo 1001)"
if [ -d "/run/user/${BOARD_UID}" ] && ! chattr -i "/run/user/${BOARD_UID}" 2>/dev/null; then
    XDG_RUNTIME_DIR="/run/user/${BOARD_UID}"
else
    XDG_RUNTIME_DIR="/run/${BOARD_USER}"
fi
BASE_PORT="${SCREENTRA_RENDER_BASE_PORT:-3900}"
CACHE_DIR="$BOARD_HOME/.local/share/screentra-client/cache"
CLIENT_DIR="$BOARD_HOME/.local/share/screentra-client"
CONFIG_JSON="$CLIENT_DIR/config.json"
SWAY_CONFIG_DIR="$BOARD_HOME/.config/sway"

log() { echo "[$(date)] $*" >> "$LOG"; }
log "screentra-kiosk.sh starting (PID=$$)"
log "Running as root, board=$BOARD_USER home=$BOARD_HOME"

mkdir -p "$XDG_RUNTIME_DIR" "$CACHE_DIR" "$SWAY_CONFIG_DIR"
chown "$BOARD_USER:$BOARD_USER" "$XDG_RUNTIME_DIR" "$CACHE_DIR" "$SWAY_CONFIG_DIR"
chmod 0700 "$XDG_RUNTIME_DIR"

# Kill stale children from an earlier compositor/browser stack.
for prog in sway chromium swaybg; do
    pkill -9 -u "$BOARD_USER" "$prog" 2>/dev/null || true
done
sleep 1
# Chromium profiles are runtime state on the board tmpfs. Repeated kiosk
# restarts must not leave timestamped profiles behind until the tmpfs is full.
for profile in "$XDG_RUNTIME_DIR"/chromium-*; do
    [ -e "$profile" ] || continue
    rm -rf -- "$profile"
done

# Refresh connector names and modes before building the Sway output rules.
# AMD/DisplayPort hotplug can change DP-2 into DP-1 (or vice versa); using the
# stale mapping makes the browser show "No valid display found".
DISPLAY_GOVERNANCE="$CLIENT_DIR/screentra-display-governance.py"
if [ -f "$DISPLAY_GOVERNANCE" ]; then
    log "Refreshing DRM display mapping..."
    if ! sudo -u "$BOARD_USER" env HOME="$BOARD_HOME" python3 "$DISPLAY_GOVERNANCE" \
        --config-dir "$CLIENT_DIR" >> "$LOG" 2>&1; then
        log "WARNING: display mapping refresh failed; retaining prior mapping"
    fi
fi

# Produce index|connector|port rows for active connected displays. The local
# admin display-count cap is respected; a missing cap means all connected
# outputs. During boot, DRM connectors can take several seconds to appear, so
# retry governance/config reads before refusing to start without a physical display.
read_display_spec() {
    python3 - "$CONFIG_JSON" "$BASE_PORT" <<'PY'
import json, sys
from pathlib import Path
path = Path(sys.argv[1])
base = int(sys.argv[2])
try:
    cfg = json.loads(path.read_text()) if path.exists() else {}
except Exception:
    cfg = {}
displays = [d for d in (cfg.get('last_detected_displays') or [])
            if d.get('connected') and d.get('enabled', True) and d.get('connector')]
displays.sort(key=lambda d: int(d.get('display_index', 0)))
override = cfg.get('max_displays')
try:
    cap = max(1, int(override)) if override is not None else len(displays)
except (TypeError, ValueError):
    cap = len(displays)
for d in displays[:cap]:
    idx = int(d.get('display_index', 0))
    connector = str(d['connector'])
    print(f'{idx}|{connector}|{base + idx}')
PY
}

DISPLAY_SPEC=""
for attempt in $(seq 1 20); do
    DISPLAY_SPEC="$(read_display_spec)"
    if [ -n "$DISPLAY_SPEC" ]; then
        break
    fi
    if [ "$attempt" -lt 20 ]; then
        log "Waiting for physical DRM outputs (attempt $attempt/20)..."
        sleep 1
    fi
done
if [ -z "$DISPLAY_SPEC" ]; then
    log "ERROR: no active display mapping; refusing to start an empty compositor"
    exit 1
fi
log "Active displays: $(printf '%s' "$DISPLAY_SPEC" | tr '\n' ' ')"

SWAY_TRANSFORM="normal"
ORIENTATION_FILE="$CACHE_DIR/client-config.json"
if [ -f "$ORIENTATION_FILE" ]; then
    ORIENTATION_VALUE="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1])).get("orientation","normal"))' "$ORIENTATION_FILE" 2>/dev/null || echo normal)"
    case "$ORIENTATION_VALUE" in
        normal) SWAY_TRANSFORM="normal" ;;
        flipped) SWAY_TRANSFORM="180" ;;
        rotated_left) SWAY_TRANSFORM="270" ;;
        rotated_right) SWAY_TRANSFORM="90" ;;
    esac
fi

# Generate one Sway config for the DRM device. Each Chromium gets a distinct
# Wayland app_id and is moved to its physical output by connector name.
{
    printf 'seat * {\n    hide_cursor 3000\n}\n'
    printf 'output * {\n    transform %s\n}\n' "$SWAY_TRANSFORM"
    while IFS='|' read -r idx connector port; do
        [ -n "$idx" ] || continue
        printf 'for_window [app_id="screentra-cast-display-%s"] move container to output %s\n' "$idx" "$connector"
        printf 'for_window [title="^Screentra Cast Display %s"] move container to output %s\n' "$idx" "$connector"
        printf 'for_window [app_id="screentra-cast-display-%s"] fullscreen enable\n' "$idx"
        printf 'for_window [app_id="screentra-cast-display-%s"] border none\n' "$idx"
        printf 'for_window [title="^Screentra Cast Display %s"] fullscreen enable\n' "$idx"
        printf 'for_window [title="^Screentra Cast Display %s"] border none\n' "$idx"
    done <<< "$DISPLAY_SPEC"
    if [ -n "${SCREENTRA_OUTPUT_MODE:-}" ]; then
        printf 'output * {\n    mode %s\n}\n' "$SCREENTRA_OUTPUT_MODE"
    fi
} > "$SWAY_CONFIG_DIR/config"
chown "$BOARD_USER:$BOARD_USER" "$SWAY_CONFIG_DIR/config"
log "Sway config written"

RENDERER_PIDS=()
BROWSER_PIDS=()
SWAY_PID=""
cleanup() {
    trap - EXIT INT TERM
    for pid in "${BROWSER_PIDS[@]}" "${SWAY_PID}" "${RENDERER_PIDS[@]}"; do
        [ -n "$pid" ] && kill "$pid" 2>/dev/null || true
    done
}
trap cleanup EXIT INT TERM

# Start one renderer per display. RIPLEY_DISPLAY_INDEX is what the renderer
# uses for its display cache, assignment, connector, and port.
while IFS='|' read -r idx connector port; do
    [ -n "$idx" ] || continue
    log "Starting renderer display $idx ($connector) on :$port..."
    sudo -u "$BOARD_USER" env \
        XDG_RUNTIME_DIR="$XDG_RUNTIME_DIR" HOME="$BOARD_HOME" \
        RIPLEY_DISPLAY_INDEX="$idx" RIPLEY_RENDER_PORT="$port" \
        RIPLEY_RENDER_BASE_PORT="$BASE_PORT" \
        python3 "$CLIENT_DIR/screentra-client-renderer.py" \
        --host 0.0.0.0 --port "$port" --cache-dir "$CACHE_DIR" \
        >> "$LOG" 2>&1 &
    RENDERER_PIDS+=("$!")
done <<< "$DISPLAY_SPEC"

# Sway must be the sole DRM owner. AMD APUs use GLES2 for reliable scan-out;
# callers can override this with WLR_RENDERER if a different GPU needs it.
log "Starting sway..."
sudo -u "$BOARD_USER" env \
    HOME="$BOARD_HOME" PATH="$PATH" XDG_RUNTIME_DIR="$XDG_RUNTIME_DIR" \
    env -u WAYLAND_DISPLAY \
    WLR_BACKENDS="${WLR_BACKENDS:-drm}" \
    WLR_RENDERER="${WLR_RENDERER:-gles2}" \
    sway -d -c "$SWAY_CONFIG_DIR/config" \
    >> "$LOG" 2>&1 &
SWAY_PID=$!
log "Sway PID=$SWAY_PID"

# Wait for the compositor socket selected by Sway; never hardcode wayland-0.
log "Waiting for Wayland socket in $XDG_RUNTIME_DIR..."
WAYLAND_DISPLAY_SOCK=""
for i in $(seq 1 30); do
    for sock in "$XDG_RUNTIME_DIR"/wayland-*; do
        [ -S "$sock" ] && WAYLAND_DISPLAY_SOCK="$sock" && break 2
    done
    sleep 0.5
done
if [ -z "$WAYLAND_DISPLAY_SOCK" ]; then
    log "ERROR: no Wayland socket found"
    exit 1
fi
WAYLAND_DISPLAY="$(basename "$WAYLAND_DISPLAY_SOCK")"
log "Wayland socket: $WAYLAND_DISPLAY"

# Start one fullscreen Chromium window per display. Each window has a unique
# app_id so Sway can place it on the matching physical connector.
while IFS='|' read -r idx connector port; do
    [ -n "$idx" ] || continue
    LIVE_READY=0
    for i in $(seq 1 40); do
        if curl -fsS --max-time 2 "http://127.0.0.1:${port}/" | \
            python3 -c 'import sys; raise SystemExit(0 if "Screentra Cast Live Player" in sys.stdin.read() else 1)'; then
            LIVE_READY=1
            break
        fi
        sleep 0.5
    done
    BROWSER_URL="http://127.0.0.1:${port}/"
    if [ "$LIVE_READY" -eq 1 ]; then
        SERVER_URL="$(python3 -c 'import pathlib,sys; print(pathlib.Path(sys.argv[1]).read_text().strip())' "$CLIENT_DIR/server" 2>/dev/null || true)"
        CLIENT_ID="$(python3 -c 'import pathlib,sys; print(pathlib.Path(sys.argv[1]).read_text().strip())' "$CLIENT_DIR/client-id" 2>/dev/null || true)"
        if [ -n "$SERVER_URL" ] && [ -n "$CLIENT_ID" ]; then
            BROWSER_URL="${SERVER_URL%/}/player.html?display=${connector}&client_id=${CLIENT_ID}"
        fi
    fi
    CHROMIUM_USER_DATA="${XDG_RUNTIME_DIR}/chromium-display-${idx}"
    mkdir -p "$CHROMIUM_USER_DATA" "$BOARD_HOME/.config/chromium/Crash Reports"
    chown -R "$BOARD_USER:$BOARD_USER" "$CHROMIUM_USER_DATA" "$BOARD_HOME/.config/chromium"
    chmod 700 "$CHROMIUM_USER_DATA"
    log "Starting Chromium display $idx on $connector (port $port, url $BROWSER_URL)..."
    sudo -u "$BOARD_USER" env \
        HOME="$BOARD_HOME" XDG_RUNTIME_DIR="$XDG_RUNTIME_DIR" \
        WAYLAND_DISPLAY="$WAYLAND_DISPLAY" \
        chromium \
        --class="screentra-cast-display-${idx}" \
        --no-sandbox --ozone-platform=wayland --hide-cursor --ash-hide-cursor \
        --app="$BROWSER_URL" --kiosk --start-fullscreen \
        --no-first-run --disable-infobars --disable-session-crashed-bubble \
        --enable-logging=stderr --v=0 --disable-features=TranslateUI \
        --autoplay-policy=no-user-gesture-required --check-for-update-interval=31536000 \
        --disable-dev-shm-usage --disable-crash-reporter --disable-breakpad \
        --user-data-dir="$CHROMIUM_USER_DATA" \
        >> "$LOG" 2>&1 &
    BROWSER_WRAPPER_PID="$!"
    BROWSER_PIDS+=("$BROWSER_WRAPPER_PID")

    # Chromium app-mode uses a shared Wayland app_id, so app_id-based Sway
    # rules cannot distinguish the two windows. Route the concrete Chromium
    # PID after creation; this keeps DP-1 and HDMI-A-1 independent and makes
    # fullscreen deterministic on every restart.
    CHROMIUM_PID=""
    for i in $(seq 1 20); do
        CHROMIUM_PID="$( (pgrep -P "$BROWSER_WRAPPER_PID" -f '/usr/lib/chromium/chromium' || true) | python3 -c 'import sys; print(next((x.strip() for x in sys.stdin if x.strip()), ""))' )"
        [ -n "$CHROMIUM_PID" ] && break
        sleep 0.25
    done
    if [ -n "$CHROMIUM_PID" ]; then
        ROUTED=0
        for attempt in $(seq 1 20); do
            SWAYSOCK_PATH=""
            for sock in "$XDG_RUNTIME_DIR"/sway-ipc.*.sock; do
                [ -S "$sock" ] || continue
                if sudo -u "$BOARD_USER" env XDG_RUNTIME_DIR="$XDG_RUNTIME_DIR" SWAYSOCK="$sock" \
                    swaymsg -t get_version >/dev/null 2>&1; then
                    SWAYSOCK_PATH="$sock"
                    break
                fi
            done
            if [ -n "$SWAYSOCK_PATH" ]; then
                if sudo -u "$BOARD_USER" env XDG_RUNTIME_DIR="$XDG_RUNTIME_DIR" SWAYSOCK="$SWAYSOCK_PATH" \
                    swaymsg "[pid=$CHROMIUM_PID] move container to output $connector" >/dev/null 2>&1; then
                    log "Routing Chromium PID $CHROMIUM_PID to $connector"
                    if ! sudo -u "$BOARD_USER" env XDG_RUNTIME_DIR="$XDG_RUNTIME_DIR" SWAYSOCK="$SWAYSOCK_PATH" \
                        swaymsg "[pid=$CHROMIUM_PID] fullscreen enable" >/dev/null 2>&1; then
                        log "WARNING: failed to fullscreen Chromium PID $CHROMIUM_PID"
                    fi
                    sudo -u "$BOARD_USER" env XDG_RUNTIME_DIR="$XDG_RUNTIME_DIR" SWAYSOCK="$SWAYSOCK_PATH" \
                        swaymsg "[pid=$CHROMIUM_PID] border none" >/dev/null 2>&1 || true
                    ROUTED=1
                    break
                fi
            fi
            sleep 0.25
        done
        if [ "$ROUTED" -ne 1 ]; then
            log "WARNING: timed out routing Chromium PID $CHROMIUM_PID to $connector"
        fi
    fi
done <<< "$DISPLAY_SPEC"

# If Sway exits, let systemd restart the complete stack. This also cleans up
# both browser windows and all per-display renderer processes.
wait "$SWAY_PID"
exit $?
