#!/usr/bin/env bash
# Screentra Cast Server Installer v0.5.0 — local HTTPS + client trust refresh
# One-line install (no sudo pre-installed):
#   curl -fsSL http://<server>:3000/install/screentra-cast-server-install.sh | su -c "bash -s" --
#
# One-line install (sudo pre-installed):
#   curl -fsSL http://<server>:3000/install/screentra-cast-server-install.sh | sudo bash
#
# Two-step (if you want to separate sudo setup from install):
#   Step 1: curl ... | sudo bash --setup-sudo
#   Step 2: curl ... | sudo bash
#
# Installs Screentra Cast as a systemd service under a dedicated "board" user.
# The calling user is granted NOPASSWD:ALL via sudoers for installation.
#
set -eo pipefail

# ── Who ran us ───────────────────────────────────────────────────────────────
# SUDO_USER is set when run via "sudo bash". Fall back to logname.
CALLER="${SUDO_USER:-$(logname 2>/dev/null || echo 'unknown')}"
MODE="${1:-install}"  # --setup-sudo or install

# ── Config ───────────────────────────────────────────────────────────────────
BOARD_USER="${BOARD_USER:-board}"
BOARD_HOME="${BOARD_HOME:-/home/board}"
BOARD_INSTALL_DIR="${BOARD_INSTALL_DIR:-/home/board/server}"
BOARD_DATA_DIR="${BOARD_DATA_DIR:-/home/board/data}"
BOARD_LISTEN_ADDR="${BOARD_LISTEN_ADDR:-0.0.0.0:3000}"
POSTGRES_DB="${SCREENTRA_CAST_POSTGRES_DB:-screentra-cast}"
LOCAL_HOSTNAME="${SCREENTRA_CAST_LOCAL_HOSTNAME:-screentra-cast.home.arpa}"
FRESH_INSTALL="false"
ADMIN_PASSWORD="${ADMIN_PASSWORD:-}"

BIN="$BOARD_INSTALL_DIR/screentra-cast-server"
ENV_FILE="$BOARD_INSTALL_DIR/server.env"
CERT_DIR="$BOARD_INSTALL_DIR/certs"
LOG_DIR="$BOARD_DATA_DIR/logs"
LOG_FILE="$LOG_DIR/screentra-cast.log"
SERVICE_NAME="screentra-cast-server.service"
TMP="$BOARD_HOME/tmp"

# Installer URLs — where to download the binary and static tarball from.
# Override SERVER_HOST if your source server runs elsewhere. Use SERVER_SCHEME=http
# only for a trusted local feed; HTTPS requires SCREENTRA_CAST_CA_CERT (or
# RIPLEY_CA_CERT) to point at the source server's public CA certificate.
SERVER_HOST="${SERVER_HOST:-11.1.1.26}"
SERVER_SCHEME="${SERVER_SCHEME:-https}"
SERVER_URL="${SERVER_SCHEME}://${SERVER_HOST}:3000/download/screentra-cast"
STATIC_URL="${SERVER_SCHEME}://${SERVER_HOST}:3000/download/screentra-cast-static.tar.gz"

log() { printf '[screentra-cast-installer] %s\n' "$*"; }

# ── Detect / install curl ────────────────────────────────────────────────────
ensure_curl() {
    if command -v curl >/dev/null 2>&1; then
        log "curl found: $(curl --version | head -1)"
        return 0
    fi
    log "curl not found — installing..."
    if command -v apt-get >/dev/null 2>&1; then
        apt-get update -qq && apt-get install -y curl
    elif command -v dnf >/dev/null 2>&1; then
        dnf install -y curl
    elif command -v pacman >/dev/null 2>&1; then
        pacman -Sy --noconfirm curl
    elif command -v apk >/dev/null 2>&1; then
        apk add curl
    else
        log "FATAL: Cannot install curl (no apt/dnf/pacman/apk found). Please install curl manually."
        exit 1
    fi
}

# ── Download helper (no heredoc — works with stdin exhaustion) ─────────────────
# Downloads $1 to $2 using Python stdlib, as the board user so files land
# with correct ownership without extra chown calls.
_dl() {
    local url=$1 dest=$2
    local ca_file="${SCREENTRA_CAST_CA_CERT:-${RIPLEY_CA_CERT:-}}"
    runuser -u "$BOARD_USER" -- env SCREENTRA_CAST_CA_CERT="$ca_file" python3 -c "
import os, sys, urllib.request, ssl
url, dest = sys.argv[1], sys.argv[2]
if url.startswith('https://'):
    cafile = os.environ.get('SCREENTRA_CAST_CA_CERT')
    if not cafile:
        raise SystemExit('HTTPS source requires SCREENTRA_CAST_CA_CERT or RIPLEY_CA_CERT')
    ctx = ssl.create_default_context(cafile=cafile)
else:
    ctx = None
try:
    request = urllib.request.urlopen(url, timeout=30, context=ctx) if ctx else urllib.request.urlopen(url, timeout=30)
    with request as r:
        data = r.read()
    with open(dest, 'wb') as f:
        f.write(data)
    print('Downloaded %d bytes to %s' % (len(data), dest), file=sys.stderr)
except Exception as e:
    print('Download error: ' + str(e), file=sys.stderr)
    sys.exit(1)
" "$url" "$dest"
}

# ── PostgreSQL provisioning ───────────────────────────────────────────────────
require_postgres_identifier() {
    case "$1" in
        ''|[0-9]*|*[!a-zA-Z0-9_]* )
            log "FATAL: PostgreSQL identifier '$1' is invalid. Use letters, numbers, and underscores; do not start with a number."
            exit 1
            ;;
    esac
}

ensure_postgresql() {
    if ! command -v psql >/dev/null 2>&1 || ! id postgres >/dev/null 2>&1 || ! command -v openssl >/dev/null 2>&1; then
        if ! command -v apt-get >/dev/null 2>&1; then
            log "FATAL: Fresh PostgreSQL installs currently require Debian/Ubuntu apt-get."
            exit 1
        fi
        log "Installing PostgreSQL..."
        DEBIAN_FRONTEND=noninteractive apt-get update -qq
        DEBIAN_FRONTEND=noninteractive apt-get install -y -qq postgresql postgresql-client openssl
    fi
    if command -v systemctl >/dev/null 2>&1; then
        systemctl enable --now postgresql
    else
        log "FATAL: systemctl is required to start PostgreSQL on a server install."
        exit 1
    fi
    if command -v pg_isready >/dev/null 2>&1 && ! pg_isready -q; then
        log "FATAL: PostgreSQL service did not become ready."
        exit 1
    fi
}

provision_postgresql() {
    require_postgres_identifier "$BOARD_USER"
    require_postgres_identifier "$POSTGRES_DB"
    ensure_postgresql

    if ! runuser -u postgres -- psql -Atqc "SELECT 1 FROM pg_roles WHERE rolname='$BOARD_USER'" | grep -qx '1'; then
        log "Creating PostgreSQL role '$BOARD_USER'..."
        runuser -u postgres -- psql -v ON_ERROR_STOP=1 -c "CREATE ROLE \"$BOARD_USER\" LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE;" >/dev/null
    fi

    if ! runuser -u postgres -- psql -Atqc "SELECT 1 FROM pg_database WHERE datname='$POSTGRES_DB'" | grep -qx '1'; then
        log "Creating PostgreSQL database '$POSTGRES_DB'..."
        runuser -u postgres -- createdb --owner="$BOARD_USER" "$POSTGRES_DB"
    fi

    runuser -u postgres -- psql -v ON_ERROR_STOP=1 -d "$POSTGRES_DB" \
        -c "GRANT ALL ON SCHEMA public TO \"$BOARD_USER\";" >/dev/null
    log "PostgreSQL is ready: local peer-authenticated database '$POSTGRES_DB'."
}

provision_local_tls() {
    case "$LOCAL_HOSTNAME" in
        ''|*[!a-zA-Z0-9.-]*)
            log "FATAL: SCREENTRA_CAST_LOCAL_HOSTNAME must contain only DNS hostname characters."
            exit 1
            ;;
    esac

    local ca_key="$CERT_DIR/screentra-cast-local-ca.key"
    local ca_cert="$CERT_DIR/screentra-cast-local-ca.crt"
    local server_key="$CERT_DIR/signage-server.key"
    local server_csr="$TMP/signage-server.csr"
    local server_cert="$CERT_DIR/signage-server.crt"
    local ext_file="$TMP/signage-server-openssl.cnf"

    mkdir -p "$CERT_DIR" "$TMP" "$BOARD_INSTALL_DIR/static/install"
    chmod 700 "$CERT_DIR"

    if [ ! -s "$ca_key" ] || [ ! -s "$ca_cert" ] || [ ! -s "$server_key" ] || [ ! -s "$server_cert" ]; then
        log "Generating persistent local TLS CA and server certificate..."
        umask 077
        openssl genrsa -out "$ca_key" 4096 >/dev/null 2>&1
        openssl req -x509 -new -sha256 -days 3650 -key "$ca_key" -out "$ca_cert" \
            -subj "/C=US/O=Screentra Cast/CN=Screentra Cast Local CA" \
            -addext "basicConstraints=critical,CA:TRUE,pathlen:1" \
            -addext "keyUsage=critical,keyCertSign,cRLSign" >/dev/null 2>&1
        openssl genrsa -out "$server_key" 2048 >/dev/null 2>&1

        {
            printf '%s\n' '[v3_server]' 'basicConstraints=critical,CA:FALSE' 'keyUsage=critical,digitalSignature,keyEncipherment' 'extendedKeyUsage=serverAuth' "subjectAltName=DNS:$LOCAL_HOSTNAME,DNS:signage-server,DNS:localhost,IP:127.0.0.1"
            if [[ "$SERVER_HOST" =~ ^[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
                printf 'subjectAltName=DNS:%s,DNS:%s,DNS:signage-server,DNS:localhost,IP:127.0.0.1,IP:%s\n' "$LOCAL_HOSTNAME" "$SERVER_HOST" "$SERVER_HOST"
            else
                printf 'subjectAltName=DNS:%s,DNS:%s,DNS:signage-server,DNS:localhost,IP:127.0.0.1\n' "$LOCAL_HOSTNAME" "$SERVER_HOST"
            fi
        } > "$ext_file"
        # Keep exactly one subjectAltName directive in the OpenSSL extension file.
        python3 - "$ext_file" <<'PY'
from pathlib import Path
import sys
p = Path(sys.argv[1])
lines = p.read_text().splitlines()
san = [line for line in lines if line.startswith('subjectAltName=')][-1]
p.write_text('\n'.join(line for line in lines if not line.startswith('subjectAltName=')) + '\n' + san + '\n')
PY
        openssl req -new -sha256 -key "$server_key" -out "$server_csr" \
            -subj "/C=US/O=Screentra Cast/CN=$LOCAL_HOSTNAME" >/dev/null 2>&1
        openssl x509 -req -sha256 -days 825 -in "$server_csr" -CA "$ca_cert" -CAkey "$ca_key" \
            -CAcreateserial -out "$server_cert" -extfile "$ext_file" -extensions v3_server >/dev/null 2>&1
        rm -f "$server_csr" "$ext_file" "$CERT_DIR/screentra-cast-local-ca.srl" "${ca_cert}.srl"
    else
        log "Preserving existing local TLS CA and server certificate."
    fi

    chown root:"$BOARD_USER" "$CERT_DIR"
    chmod 0750 "$CERT_DIR"
    chown root:root "$ca_key" "$ca_cert"
    chmod 600 "$ca_key"
    chmod 644 "$ca_cert"
    chown "$BOARD_USER:$BOARD_USER" "$server_key" "$server_cert"
    chmod 600 "$server_key"
    chmod 644 "$server_cert"

    # Publish only the public CA certificate. Never publish the CA private key.
    install -o "$BOARD_USER" -g "$BOARD_USER" -m 0644 "$ca_cert" "$BOARD_INSTALL_DIR/static/install/screentra-cast-signage-ca.crt"
    log "Local HTTPS ready for https://$LOCAL_HOSTNAME:3000"
}

# ── Bootstrap sudo if missing ───────────────────────────────────────────────
bootstrap_sudo() {
    # If already running as root, nothing to do
    [ "$(id -u)" = "0" ] && return 0
    # If sudo is available and works, nothing to do
    command -v sudo >/dev/null 2>&1 && sudo -n true 2>/dev/null && return 0
    # Need to install sudo via su
    log "sudo not found — installing via su (will prompt for root password)..."
    if ! command -v su >/dev/null 2>&1; then
        log "FATAL: neither sudo nor su found. Cannot bootstrap."
        exit 1
    fi
    su -c "apt-get update -qq && apt-get install -y -qq sudo curl 2>&1" || {
        log "FATAL: su -c apt-get install failed."
        log "Try running manually: su -c 'apt-get install -y sudo curl'"
        exit 1
    }
    log "sudo and curl installed."
}

# ── Setup sudo phase ───────────────────────────────────────────────────────────
if [ "$MODE" = "--setup-sudo" ] || [ "$MODE" = "setup-sudo" ]; then
    log "=== Sudo Setup Phase ==="
    log "Configuring passwordless sudo for '$CALLER'..."
    bootstrap_sudo

    # Grant full NOPASSWD so the installer can run without further prompts
    echo "$CALLER ALL=(ALL) NOPASSWD:ALL" | sudo tee "/etc/sudoers.d/screentra-cast-$CALLER" > /dev/null
    sudo chmod 440 "/etc/sudoers.d/screentra-cast-$CALLER"
    sudo visudo -c -f "/etc/sudoers.d/screentra-cast-$CALLER" 2>/dev/null || true
    log "Done. '$CALLER' can now run: curl ... | sudo bash"
    exit 0
fi

# ── Pre-flight ───────────────────────────────────────────────────────────────
log "=== Screentra Cast Server Installer ==="
log "Installer run as: root (via sudo)"
log "Original caller:  $CALLER"
log "Target user:     $BOARD_USER"
log "Install dir:     $BOARD_INSTALL_DIR"
log "Data dir:        $BOARD_DATA_DIR"

if [ "$(id -u)" -ne 0 ]; then
    log "FATAL: The install phase must run as root. Use: sudo bash"
    exit 1
fi
ensure_curl

# ── Create board user if missing ──────────────────────────────────────────────
if ! id "$BOARD_USER" >/dev/null 2>&1; then
    log "Creating user '$BOARD_USER'..."
    useradd -r -m -d "$BOARD_HOME" -s /bin/bash "$BOARD_USER"
else
    log "User '$BOARD_USER' already exists."
fi

if [ ! -f "$ENV_FILE" ]; then
    FRESH_INSTALL="true"
    log "Fresh install detected; PostgreSQL will be provisioned."
    provision_postgresql
fi


# ── Create directories ────────────────────────────────────────────────────────
log "Creating directories..."
mkdir -p "$BOARD_INSTALL_DIR/static" \
         "$BOARD_DATA_DIR/uploads" \
         "$BOARD_DATA_DIR/backups" \
         "$LOG_DIR" \
         "$TMP" \
         "$BOARD_HOME/.config/systemd/user"
chown -R "$BOARD_USER:$BOARD_USER" "$BOARD_HOME"
log "Directories created."

# ── Download binary ───────────────────────────────────────────────────────────
log "Downloading screentra-cast-server..."
mkdir -p "$TMP"
if ! _dl "$SERVER_URL" "$TMP/screentra-cast-server" 2>&1; then
    log "ERROR: Failed to download binary from $SERVER_URL"
    log "Make sure the Screentra Cast server is running on $SERVER_HOST"
    exit 1
fi
mv "$TMP/screentra-cast-server" "$BIN"
chown "$BOARD_USER:$BOARD_USER" "$BIN"
chmod +x "$BIN"
log "Binary installed: $BIN"

# ── Download static tarball ───────────────────────────────────────────────────
static_ok=false
log "Downloading static tarball..."
if _dl "$STATIC_URL" "$TMP/screentra-cast-static.tar.gz" 2>&1; then
    static_ok=true
    log "Static tarball downloaded."
else
    log "WARNING: Failed to download static tarball (server may not have it yet)."
fi

# ── Extract static ────────────────────────────────────────────────────────────
if $static_ok && [ -s "$TMP/screentra-cast-static.tar.gz" ]; then
    log "Extracting static files..."
    STAGING_DIR="$TMP/static-staging"
    rm -rf "$STAGING_DIR"
    mkdir -p "$STAGING_DIR" "$BOARD_INSTALL_DIR/static"
    tar -xzf "$TMP/screentra-cast-static.tar.gz" -C "$STAGING_DIR"
    # Copy only the archive contents from the isolated staging directory.
    cp -a "$STAGING_DIR/." "$BOARD_INSTALL_DIR/static/"
    rm -rf "$STAGING_DIR"
    chown -R "$BOARD_USER:$BOARD_USER" "$BOARD_INSTALL_DIR/static/"
    if [ -d "$BOARD_INSTALL_DIR/static/js" ]; then
        log "Static files extracted OK."
    else
        log "WARNING: static extraction may have failed (no js/ dir)."
    fi
fi

if [ "$FRESH_INSTALL" = "true" ]; then
    provision_local_tls
fi

# ── Download client installers tarball into static ────────────────────────────
# This makes the tarball available at /install/screentra-cast-installers.tar.gz
# so other devices on the network can pull it after the server is running.
INSTALLERS_URL="http://${SERVER_HOST}:3000/install/screentra-cast-installers.tar.gz"
mkdir -p "$BOARD_INSTALL_DIR/static/install"
log "Downloading client installers tarball..."
if _dl "$INSTALLERS_URL" "$TMP/screentra-cast-installers.tar.gz" 2>&1; then
    cp "$TMP/screentra-cast-installers.tar.gz" "$BOARD_INSTALL_DIR/static/install/"
    chown "$BOARD_USER:$BOARD_USER" "$BOARD_INSTALL_DIR/static/install/screentra-cast-installers.tar.gz"
    log "Client installers tarball saved to static/install/."
else
    log "WARNING: Could not download client installers tarball (server may still be starting)."
fi

# ── Write env file ────────────────────────────────────────────────────────────
log "Writing env file..."
declare -A env_lines

if [ -f "$ENV_FILE" ]; then
    while IFS='=' read -r k v; do
        [ -n "$k" ] && env_lines["$k"]="$v"
    done < "$ENV_FILE"
fi

if [ "$FRESH_INSTALL" = "true" ]; then
    env_lines[SIGNAGE_REPOSITORY_BACKEND]="postgres"
    env_lines[SIGNAGE_DATABASE_URL]="host=/var/run/postgresql dbname=$POSTGRES_DB user=$BOARD_USER"
elif [ "${env_lines[SIGNAGE_REPOSITORY_BACKEND]:-}" != "postgres" ]; then
    log "FATAL: Existing installation is not PostgreSQL-backed. Refusing to silently abandon its SQLite database. Migrate it first, then rerun the installer."
    exit 1
elif [ -z "${env_lines[SIGNAGE_DATABASE_URL]:-}" ]; then
    log "FATAL: Existing PostgreSQL installation is missing SIGNAGE_DATABASE_URL."
    exit 1
fi

if [ -n "$ADMIN_PASSWORD" ]; then
    env_lines[SIGNAGE_ADMIN_PASSWORD]="$ADMIN_PASSWORD"
    unset 'env_lines[SIGNAGE_ADMIN_PASSWORD_HASH]'
elif [ -z "${env_lines[SIGNAGE_ADMIN_PASSWORD_HASH]:-}" ] && [ -z "${env_lines[SIGNAGE_ADMIN_PASSWORD]:-}" ]; then
    # Fresh installs use an unreachable bootstrap secret. The end user creates
    # the first real admin account through the web setup screen.
    env_lines[SIGNAGE_ADMIN_PASSWORD_HASH]="$(head -c 32 /dev/urandom | sha256sum | cut -d' ' -f1)"
fi

env_lines[SIGNAGE_LISTEN_ADDR]="$BOARD_LISTEN_ADDR"
env_lines[SIGNAGE_DATA_DIR]="$BOARD_DATA_DIR"
if [ "$FRESH_INSTALL" = "true" ]; then
    env_lines[SCREENTRA_CAST_SERVER_URL]="https://${SERVER_HOST}:3000"
    env_lines[SIGNAGE_TLS]="1"
    env_lines[SIGNAGE_SECONDARY_TLS]="1"
    env_lines[SIGNAGE_TLS_CERT]="$CERT_DIR/signage-server.crt"
    env_lines[SIGNAGE_TLS_KEY]="$CERT_DIR/signage-server.key"
fi
env_lines[SIGNAGE_STATIC_DIR]="$BOARD_INSTALL_DIR/static"

if [ -z "${env_lines[SIGNAGE_AUTH_SECRET]:-}" ]; then
    env_lines[SIGNAGE_AUTH_SECRET]="$(head -c 32 /dev/urandom | base64)"
fi

{
    printf '# Generated by screentra-cast-server-install.sh\n'
    for k in "${!env_lines[@]}"; do
        printf '%s=%q\n' "$k" "${env_lines[$k]}"
    done
} > "$ENV_FILE.tmp"
mv "$ENV_FILE.tmp" "$ENV_FILE"
chown "$BOARD_USER:$BOARD_USER" "$ENV_FILE"
chmod 600 "$ENV_FILE"
log "Env file written: $ENV_FILE"

if [ "$FRESH_INSTALL" = "true" ]; then
    POSTGRES_URL="host=/var/run/postgresql dbname=$POSTGRES_DB user=$BOARD_USER"
    log "Applying PostgreSQL schema..."
    runuser -u "$BOARD_USER" -- env SIGNAGE_DATABASE_URL="$POSTGRES_URL" "$BIN" --postgres-schema
    runuser -u "$BOARD_USER" -- env PGHOST=/var/run/postgresql PGDATABASE="$POSTGRES_DB" PGUSER="$BOARD_USER" psql -Atqc "SELECT 1" >/dev/null
    log "PostgreSQL schema and peer authentication verified."
fi

# ── Write systemd system service ──────────────────────────────────────────────
log "Writing systemd service..."
mkdir -p /etc/systemd/system
cat > /etc/systemd/system/screentra-cast-server.service << 'EOF'
[Unit]
Description=Screentra Cast Digital Signage Server
After=network.target

[Service]
Type=simple
User=board
EnvironmentFile=/home/board/server/server.env
ExecStart=/home/board/server/screentra-cast-server
WorkingDirectory=/home/board/server
Restart=on-failure
RestartSec=5s
StandardOutput=journal
StandardError=journal

[Install]
WantedBy=multi-user.target
EOF
log "Service file written."

# ── Enable and start service ─────────────────────────────────────────────────
log "Starting service..."
systemctl daemon-reload
systemctl enable screentra-cast-server.service
systemctl start screentra-cast-server.service

# ── Wait for server ──────────────────────────────────────────────────────────
log "Waiting for server to start..."
port="${BOARD_LISTEN_ADDR#*:}"
startup_timeout="${SCREENTRA_CAST_STARTUP_TIMEOUT:-90}"
for i in $(seq 1 "$startup_timeout"); do
    if [ "${env_lines[SIGNAGE_TLS]:-0}" = "1" ]; then
        if curl -k -sf "https://127.0.0.1:$port/api/version" > /dev/null 2>&1; then
            log "Server is up on HTTPS port $port."
            break
        fi
    elif curl -sf "http://127.0.0.1:$port/api/version" > /dev/null 2>&1; then
        log "Server is up on HTTP port $port."
        break
    fi
    if [ "$i" -eq "$startup_timeout" ]; then
        log "WARNING: Server did not respond within ${startup_timeout} seconds."
        log "Check: systemctl status $SERVICE_NAME"
        log "Check: journalctl -u $SERVICE_NAME"
    fi
    sleep 1
done

# ── Done ─────────────────────────────────────────────────────────────────────
if [ "$FRESH_INSTALL" = "true" ]; then
    DATABASE_SUMMARY="PostgreSQL database '$POSTGRES_DB' (local peer auth)"
else
    DATABASE_SUMMARY="PostgreSQL database configured in server.env"
fi
cat << EOF

Screentra Cast server installed.

  Binary:      $BIN
  Data dir:    $BOARD_DATA_DIR (uploads/backups)
  Database:    $DATABASE_SUMMARY
  Config:      $ENV_FILE
  Bind:        $BOARD_LISTEN_ADDR
  Service:     $SERVICE_NAME (systemd, as $BOARD_USER)
  Log:         $LOG_FILE

Useful commands:
  sudo systemctl status $SERVICE_NAME
  sudo systemctl restart $SERVICE_NAME
  sudo journalctl -u $SERVICE_NAME -f

Open https://$LOCAL_HOSTNAME:${BOARD_LISTEN_ADDR#*:} in a browser after
mapping that hostname to this server on the local network.
Install the local CA from:
  https://$LOCAL_HOSTNAME:${BOARD_LISTEN_ADDR#*:}/install/screentra-cast-signage-ca.crt

No default admin password is created on a fresh install. If you supplied
ADMIN_PASSWORD, it remains available as a temporary bootstrap credential.

Cleanup (run these as $CALLER with sudo):
  sudo systemctl stop $SERVICE_NAME
  sudo rm -rf $BOARD_HOME
  sudo userdel $BOARD_USER
  sudo rm -f $SUDOERS_FILE

EOF
