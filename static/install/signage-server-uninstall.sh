#!/usr/bin/env bash
# Signage server uninstaller.
#
# Removes the systemd user service, the binary, the data dir, and the
# install dir. Safe to re-run.
#
# Usage:
#   RIPLEY_SERVER_URL=http://server:3000 bash signage-server-uninstall.sh
#   RIPLEY_KEEP_DATA=1 bash signage-server-uninstall.sh  # keep DB/uploads
#
# Environment variables:
#   RIPLEY_SERVER_URL    Source server URL (for unregister notification, optional)
#   RIPLEY_CLIENT_DIR    Install dir (default: ~/.local/share/ripley-signage-server)
#   RIPLEY_KEEP_DATA     Default 1 (safe: never delete DB). Set 0 to also remove data dir.

set -euo pipefail

INSTALL_DIR="${RIPLEY_CLIENT_DIR:-$HOME/.local/share/ripley-signage-server}"
BIN_DIR="$HOME/.local/bin"
SYSTEMD_USER_DIR="$HOME/.config/systemd/user"
SERVICE_NAME="signage-server.service"
ENV_FILE="$INSTALL_DIR/server.env"

log() { printf '%s\n' "[ripley-uninstaller] $*" >&2; }

# Resolve data dir from env file (best-effort)
if [ -r "$ENV_FILE" ]; then
  DATA_DIR="$(grep -E '^SIGNAGE_DATA_DIR=' "$ENV_FILE" | head -1 | cut -d= -f2- | tr -d '\047')"
  DATA_DIR="${DATA_DIR:-}"
fi
DATA_DIR="${DATA_DIR:-$HOME/.local/share/signage}"

log "Stopping service..."
if command -v systemctl >/dev/null 2>&1; then
  systemctl --user stop "$SERVICE_NAME" 2>/dev/null || true
  systemctl --user disable "$SERVICE_NAME" 2>/dev/null || true
fi
pkill -f "$INSTALL_DIR/signage-server" 2>/dev/null || true

log "Removing service file and binaries..."
rm -f "$SYSTEMD_USER_DIR/$SERVICE_NAME"
rm -rf "$INSTALL_DIR" "$BIN_DIR/signage-server"

if command -v systemctl >/dev/null 2>&1; then
  systemctl --user daemon-reload 2>/dev/null || true
  systemctl --user reset-failed 2>/dev/null || true
fi

if [ "${RIPLEY_KEEP_DATA:-1}" = "0" ]; then
  log "Removing data dir: $DATA_DIR"
  rm -rf "$DATA_DIR"
else
  log "Keeping data dir: $DATA_DIR"
fi

log "Signage server uninstalled."
