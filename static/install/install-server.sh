#!/usr/bin/env bash
set -euo pipefail
INSTALL_DIR="${RIPLEY_SERVER_DIR:-$HOME/ripley-kiosk}"
START_SERVICE="${RIPLEY_START_SERVER:-1}"
FORCE_BUILD=0
for arg in "$@"; do
  case "$arg" in
    --build)   FORCE_BUILD=1 ;;
    --no-start) START_SERVICE=0 ;;
  esac
done

log(){ printf '[ripley-server-install] %s\n' "$*"; }
have(){ command -v "$1" >/dev/null 2>&1; }
need_sudo(){
  if ! have sudo; then
    echo "This step requires root privileges. Please re-run with sudo:" >&2
    echo "  sudo bash $0 $*" >&2
    exit 1
  fi
}

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PAYLOAD_DIR="$SCRIPT_DIR/payload"
[ -d "$PAYLOAD_DIR" ] || { echo "payload directory missing: $PAYLOAD_DIR" >&2; exit 1; }

# Install required packages — needs sudo if packages are missing
install_pkg(){
  local pkg="$1"; shift
  have "$pkg" && return 0
  need_sudo "$@"
  log "Installing $pkg (sudo required)..."
  sudo apt-get update -qq && sudo DEBIAN_FRONTEND=noninteractive apt-get install -y -qq "$@"
}

install_pkg curl      curl wget
install_pkg rsync     rsync
install_pkg sqlite3   sqlite3

# Install or build binary
if [ -x "$INSTALL_DIR/target/release/signage" ]; then
  log "Using existing binary at target/release/signage"
elif [ -x "$PAYLOAD_DIR/target/release/signage" ]; then
  log "Installing bundled static binary..."
  mkdir -p "$INSTALL_DIR/target/release"
  cp -f "$PAYLOAD_DIR/target/release/signage" "$INSTALL_DIR/target/release/signage"
  chmod +x "$INSTALL_DIR/target/release/signage"
elif [ -x "$PAYLOAD_DIR/static/download/signage-server" ]; then
  log "Installing bundled static binary..."
  mkdir -p "$INSTALL_DIR/target/release"
  cp -f "$PAYLOAD_DIR/static/download/signage-server" "$INSTALL_DIR/target/release/signage"
  chmod +x "$INSTALL_DIR/target/release/signage"
elif [ "$FORCE_BUILD" = "1" ]; then
  log "Building from source (--build)..."
  install_pkg cargo    cargo
  install_pkg rustc   rustc
  install_pkg build-essential build-essential pkg-config libssl-dev python3 python3-venv
  [ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"
  cargo build --release
else
  echo "No binary found. Run with --build to compile from source." >&2
  exit 1
fi

# Write env — HTTP only, no TLS, no TLS cert paths
ENV_DIR="$HOME/.config/ripley-kiosk"
ENV_FILE="$ENV_DIR/server.env"
mkdir -p "$ENV_DIR"
if [ ! -f "$ENV_FILE" ]; then
  log "Writing server env at $ENV_FILE"
  cat > "$ENV_FILE" <<EOF
SIGNAGE_TLS=0
SIGNAGE_ADMIN_PASSWORD=${SIGNAGE_ADMIN_PASSWORD:-change-me-now}
RUST_LOG=info
EOF
  chmod 600 "$ENV_FILE"
else
  log "Keeping existing env at $ENV_FILE"
fi

# Auto-start via systemd user service (no root needed for user services)
if [ "$START_SERVICE" = "1" ]; then
  UNIT_DIR="$HOME/.config/systemd/user"
  mkdir -p "$UNIT_DIR"
  cat > "$UNIT_DIR/ripley-signage-server.service" <<EOF
[Unit]
Description=Ripley Signage Server
After=network-online.target
Wants=network-online.target

[Service]
WorkingDirectory=$INSTALL_DIR
EnvironmentFile=$ENV_FILE
ExecStart=$INSTALL_DIR/target/release/signage
Restart=always
RestartSec=3

[Install]
WantedBy=default.target
EOF

  # Ensure a systemd user session exists. On headless/SSH machines the user
  # instance may not be running. Start it as the current user (no sudo needed).
  if ! systemctl --user status >/dev/null 2>&1; then
    log "Starting systemd user instance..."
    nohup systemd --user > /dev/null 2>&1 &
    sleep 3
  fi

  if systemctl --user status >/dev/null 2>&1; then
    systemctl --user daemon-reload
    systemctl --user enable --now ripley-signage-server.service
    log "Server auto-started via systemd user service"
  else
    log "systemd user session unavailable — install complete but server not started."
    log "To run manually:"
    log "  cd $INSTALL_DIR"
    log "  set -a && . $ENV_FILE && set +a"
    log "  ./target/release/signage"
  fi
else
  log "Install complete."
  log "To run: cd $INSTALL_DIR && set -a && . $ENV_FILE && set +a && ./target/release/signage"
fi
