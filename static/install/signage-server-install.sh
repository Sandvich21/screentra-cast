#!/usr/bin/env bash
# Deprecated compatibility wrapper.
#
# This legacy installer used SQLite and a per-user systemd service. New server
# installs must use the canonical root/systemd installer, which provisions a
# local PostgreSQL database and runs the service as board.
set -euo pipefail

SERVER_HOST="${SERVER_HOST:-11.1.1.25}"
INSTALL_URL="http://${SERVER_HOST}:3000/install/screentra-cast-server-install.sh"

if [ "$(id -u)" -ne 0 ]; then
    if ! command -v sudo >/dev/null 2>&1; then
        printf '%s\n' "ERROR: sudo is required. Run the canonical installer as root:" >&2
        printf '  curl -fsSL %s | su -c "bash -s" --\n' "$INSTALL_URL" >&2
        exit 1
    fi
    tmp="$(mktemp)"
    trap 'rm -f "$tmp"' EXIT
    curl -fsSL "$INSTALL_URL" -o "$tmp"
    exec sudo bash "$tmp"
fi

printf '%s\n' "The legacy SQLite installer has been retired." >&2
printf '%s\n' "Use the PostgreSQL-backed canonical installer instead:" >&2
printf '  curl -fsSL %s | bash\n' "$INSTALL_URL" >&2
exit 1
