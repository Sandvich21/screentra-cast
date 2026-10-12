#!/usr/bin/env bash
set -Eeuo pipefail

ROLE_FILE=/etc/screentra-cast-iso/role
INSTALL_ROOT=/opt/screentra-cast/install

log() { printf '[screentra-cast-setup] %s\n' "$*"; }
die() { log "ERROR: $*" >&2; exit 1; }

mkdir -p "$(dirname "$ROLE_FILE")"
role=${SCREENTRA_ISO_ROLE:-}
if [[ -z "$role" && -t 0 && -t 1 && -x "$(command -v whiptail || true)" ]]; then
  role=$(whiptail --title 'Screentra Cast appliance setup' \
    --menu 'Choose what to install:' 14 72 3 \
    client 'Client only (Wayland/DRM signage player)' \
    server 'Server only (Screentra Cast server)' \
    combined 'Server + client on this machine' 3>&1 1>&2 2>&3) || exit 1
fi
role=${role:-client}
case "$role" in
  client|server|combined) ;;
  *) die "invalid role: $role" ;;
esac
printf '%s\n' "$role" > "$ROLE_FILE"
log "selected role: $role"

# The first ISO scaffold records the role and keeps payload execution explicit.
# The final installer wiring will call the architecture-matched payload wrappers
# after the Debian clean-install test validates networking and board ownership.
case "$role" in
  client)
    [[ -x "$INSTALL_ROOT/player-client.sh" ]] || die "client installer payload missing"
    log "client payload is available at $INSTALL_ROOT/player-client.sh"
    ;;
  server)
    [[ -x "$INSTALL_ROOT/server-install.sh" ]] || die "server installer payload missing"
    log "server payload is available at $INSTALL_ROOT/server-install.sh"
    ;;
  combined)
    [[ -x "$INSTALL_ROOT/appliance-install.sh" ]] || die "combined installer payload missing"
    log "combined payload is available at $INSTALL_ROOT/appliance-install.sh"
    ;;
esac

log "role recorded; installation payload is ready for the clean-box validation stage"
