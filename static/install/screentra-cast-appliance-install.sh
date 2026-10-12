#!/usr/bin/env bash
#
# Screentra Cast Appliance Installer v0.1.0
#
# Single-shot install: server + player-client on the same box.
# Designed for "I have a TV, a PC, and zero servers" use case.
#
# Usage:
#   curl -fsSL http://<server>:3000/install/screentra-cast-appliance-install.sh | bash
#
set -euo pipefail

# ── defaults ────────────────────────────────────────────────────────────────
BOARD_USER="${BOARD_USER:-board}"
BOARD_HOME="/home/${BOARD_USER}"
BOARD_INSTALL_DIR="${BOARD_INSTALL_DIR:-${BOARD_HOME}/server}"
BOARD_DATA_DIR="${BOARD_DATA_DIR:-${BOARD_HOME}/data}"
BOARD_LISTEN_ADDR="${BOARD_LISTEN_ADDR:-0.0.0.0:3000}"
ADMIN_PASSWORD="${ADMIN_PASSWORD:-admin}"
SERVER_URL="http://127.0.0.1:3000"

# Installer fetch URL — set to the source server if different from local
INSTALL_SOURCE_URL="${INSTALL_SOURCE_URL:-http://127.0.0.1:3000}"

log() { printf '[screentra-cast-appliance] %s\n' "$*" >&2; }
fail() { log "FATAL: $*"; exit 1; }

# ── preflight ──────────────────────────────────────────────────────────────
command -v curl >/dev/null 2>&1 || fail "curl is required"
command -v systemctl >/dev/null 2>&1 || fail "systemd is required"

# ── create board user ──────────────────────────────────────────────────────
if ! id "$BOARD_USER" >/dev/null 2>&1; then
    log "Creating user '$BOARD_USER'..."
    useradd -m -s /bin/bash \
        -G video,render,input,netdev,plugdev,sudo,staff \
        "$BOARD_USER"
    log "Created user '$BOARD_USER'."
else
    log "User '$BOARD_USER' already exists."
fi

# ── install server ──────────────────────────────────────────────────────────
log "Step 1/2: installing Screentra Cast server..."

# Enable lingering for systemd --user services without a login session
loginctl enable-linger "$BOARD_USER" 2>/dev/null || true

# Run server installer as board user
exec sudo -E -u "$BOARD_USER" \
    HOME="$BOARD_HOME" \
    XDG_RUNTIME_DIR="/run/user/$(id -u "$BOARD_USER")" \
    SCREENTRA_CAST_SERVER_URL="$SERVER_URL" \
    BOARD_USER="$BOARD_USER" \
    BOARD_INSTALL_DIR="$BOARD_INSTALL_DIR" \
    BOARD_DATA_DIR="$BOARD_DATA_DIR" \
    BOARD_LISTEN_ADDR="$BOARD_LISTEN_ADDR" \
    ADMIN_PASSWORD="$ADMIN_PASSWORD" \
    bash "$INSTALL_SOURCE_URL/install/screentra-cast-server-install.sh"

log "Step 2/2: installing Screentra Cast player-client..."

# Run player-client installer pointing at the local server
exec sudo -E -u "$BOARD_USER" \
    HOME="$BOARD_HOME" \
    XDG_RUNTIME_DIR="/run/user/$(id -u "$BOARD_USER")" \
    SCREENTRA_CAST_SERVER_URL="$SERVER_URL" \
    BOARD_USER="$BOARD_USER" \
    BOARD_CLIENT_DIR="${BOARD_CLIENT_DIR:-${BOARD_HOME}/client}" \
    bash "$INSTALL_SOURCE_URL/install/player-client.sh"

log "Appliance installation complete."
log "Server:  http://127.0.0.1:3000"
log "Renderer: http://127.0.0.1:3900/"
log "Admin:    http://$(hostname -I 2>/dev/null | awk '{print $1}'):3910/"
