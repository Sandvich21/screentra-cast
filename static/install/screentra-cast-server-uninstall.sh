#!/usr/bin/env bash
#
# Screentra Cast Server Uninstaller v0.1.0
#
# Removes systemd user service, binary, and data dir. Safe to re-run.
#
# Usage:
#   SCREENTRA_CAST_KEEP_DATA=1 bash screentra-cast-server-uninstall.sh
#   SCREENTRA_CAST_KEEP_DATA=0 bash screentra-cast-server-uninstall.sh  # also wipe data
#
set -euo pipefail

BOARD_USER="${BOARD_USER:-board}"
BOARD_HOME="/home/${BOARD_USER}"
BOARD_INSTALL_DIR="${BOARD_INSTALL_DIR:-${BOARD_HOME}/server}"
BOARD_DATA_DIR="${BOARD_DATA_DIR:-${BOARD_HOME}/data}"
ENV_FILE="${BOARD_INSTALL_DIR}/server.env"
SERVICE_NAME="screentra-cast-server.service"
SYSTEMD_USER_DIR="${BOARD_HOME}/.config/systemd/user"

log() { printf '[screentra-cast-uninstaller] %s\n' "$*" >&2; }

# Resolve actual data dir from env file if available
if [ -r "$ENV_FILE" ]; then
    DATA_FROM_ENV="$(grep -E '^SIGNAGE_DATA_DIR=' "$ENV_FILE" 2>/dev/null | head -1 | cut -d= -f2- | tr -d "'\"")"
    [ -n "$DATA_FROM_ENV" ] && BOARD_DATA_DIR="$DATA_FROM_ENV"
fi

systemctl_user() {
    sudo -u "$BOARD_USER" \
        XDG_RUNTIME_DIR="/run/user/$(id -u "$BOARD_USER")" \
        systemctl --user "$@"
}

log "Stopping service..."
systemctl_user stop "$SERVICE_NAME" 2>/dev/null || true
systemctl_user disable "$SERVICE_NAME" 2>/dev/null || true
pkill -f "${BOARD_INSTALL_DIR}/screentra-cast-server" 2>/dev/null || true

log "Removing service file..."
rm -f "$SYSTEMD_USER_DIR/$SERVICE_NAME"
systemctl_user daemon-reload 2>/dev/null || true
systemctl_user reset-failed 2>/dev/null || true

log "Removing install dir..."
rm -rf "$BOARD_INSTALL_DIR"

if [ "${SCREENTRA_CAST_KEEP_DATA:-1}" = "0" ]; then
    log "Removing data dir: $BOARD_DATA_DIR"
    rm -rf "$BOARD_DATA_DIR"
else
    log "Keeping data dir: $BOARD_DATA_DIR"
fi

log "Screentra Cast server uninstalled."
