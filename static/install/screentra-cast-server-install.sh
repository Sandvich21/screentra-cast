#!/usr/bin/env bash
# Screentra Cast Server Installer — signed HTTPS release only.
# Requires root and an explicit SCREENTRA_CAST_RELEASE_BASE_URL.
set -eo pipefail

BOARD_USER="${BOARD_USER:-board}"
BOARD_HOME="${BOARD_HOME:-/home/board}"
BOARD_INSTALL_DIR="${BOARD_INSTALL_DIR:-/home/board/server}"
BOARD_DATA_DIR="${BOARD_DATA_DIR:-/home/board/data}"
BOARD_LISTEN_ADDR="${BOARD_LISTEN_ADDR:-0.0.0.0:3000}"
POSTGRES_DB="${SCREENTRA_CAST_POSTGRES_DB:-screentra_cast}"
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
TARGET_HOST="${SCREENTRA_CAST_TARGET_HOST:-$LOCAL_HOSTNAME}"
case "$TARGET_HOST" in
    ''|*[!a-zA-Z0-9.-]*) printf '%s\n' 'FATAL: SCREENTRA_CAST_TARGET_HOST must contain only hostname or IPv4 characters.' >&2; exit 1 ;;
esac
RELEASE_BASE_URL="${SCREENTRA_CAST_RELEASE_BASE_URL:-}"
case "$RELEASE_BASE_URL" in
    https://*/*) RELEASE_BASE_URL="${RELEASE_BASE_URL%/}" ;;
    *) printf '%s\n' 'FATAL: SCREENTRA_CAST_RELEASE_BASE_URL must be an explicit HTTPS release URL.' >&2; exit 1 ;;
esac
RELEASE_BINARY_NAME="screentra-cast-server-amd64"
SERVER_URL="$RELEASE_BASE_URL/$RELEASE_BINARY_NAME"
STATIC_URL="$RELEASE_BASE_URL/screentra-cast-static.tar.gz"
INSTALLERS_URL="$RELEASE_BASE_URL/screentra-cast-installers.tar.gz"
CHECKSUMS_URL="$RELEASE_BASE_URL/SHA256SUMS"

log() { printf '[screentra-cast-installer] %s\n' "$*"; }

if [ "$(id -u)" -ne 0 ]; then
    log 'FATAL: the release installer must run as root.'
    exit 1
fi
for required in curl python3 sha256sum; do
    command -v "$required" >/dev/null 2>&1 || { log "FATAL: required command is missing: $required"; exit 1; }
done

_dl() {
    local url=$1 dest=$2
    case "$url" in https://*) ;; *) log "FATAL: refusing non-HTTPS download"; exit 1 ;; esac
    curl --fail --silent --show-error --location --proto '=https' --tlsv1.2 \
        --connect-timeout 10 --max-time 120 --output "$dest" "$url"
}

verify_release_asset() {
    local asset=$1
    (cd "$TMP" && sha256sum -c "$CHECKSUMS_FILE" --ignore-missing 2>/dev/null) | grep -Fqx "$asset: OK" || {
        log "FATAL: SHA-256 verification failed for $asset"
        exit 1
    }
    log "SHA-256 verified: $asset"
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

ensure_ffmpeg() {
    if command -v ffmpeg >/dev/null 2>&1; then
        log "FFmpeg already installed: $(command -v ffmpeg)"
        return 0
    fi
    if ! command -v apt-get >/dev/null 2>&1; then
        log "FATAL: FFmpeg is required by the server runtime, but apt-get is unavailable."
        exit 1
    fi
    log "Installing FFmpeg server runtime dependency..."
    DEBIAN_FRONTEND=noninteractive apt-get update -qq
    DEBIAN_FRONTEND=noninteractive apt-get install -y -qq ffmpeg
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
            if [[ "$TARGET_HOST" =~ ^[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
                printf 'subjectAltName=DNS:%s,DNS:%s,DNS:signage-server,DNS:localhost,IP:127.0.0.1,IP:%s\n' "$LOCAL_HOSTNAME" "$TARGET_HOST" "$TARGET_HOST"
            else
                printf 'subjectAltName=DNS:%s,DNS:%s,DNS:signage-server,DNS:localhost,IP:127.0.0.1\n' "$LOCAL_HOSTNAME" "$TARGET_HOST"
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

# ── Pre-flight ───────────────────────────────────────────────────────────────
log "=== Screentra Cast Server Release Installer ==="
log "Release:          $RELEASE_BASE_URL"
log "Target user:      $BOARD_USER"
log "Install dir:      $BOARD_INSTALL_DIR"
log "Data dir:         $BOARD_DATA_DIR"

# ── Create board user if missing ──────────────────────────────────────────────
if ! id "$BOARD_USER" >/dev/null 2>&1; then
    log "Creating user '$BOARD_USER'..."
    useradd -r -m -d "$BOARD_HOME" -s /bin/bash "$BOARD_USER"
else
    log "User '$BOARD_USER' already exists."
fi

ensure_ffmpeg

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

# ── Download release assets ────────────────────────────────────────────────────
log "Downloading release checksums..."
mkdir -p "$TMP"
CHECKSUMS_FILE="$TMP/SHA256SUMS"
_dl "$CHECKSUMS_URL" "$CHECKSUMS_FILE"
_dl "$SERVER_URL" "$TMP/$RELEASE_BINARY_NAME"
verify_release_asset "$RELEASE_BINARY_NAME"
mv "$TMP/$RELEASE_BINARY_NAME" "$BIN"
chown "$BOARD_USER:$BOARD_USER" "$BIN"
chmod +x "$BIN"
log "Binary installed: $BIN"

_dl "$STATIC_URL" "$TMP/screentra-cast-static.tar.gz"
verify_release_asset "screentra-cast-static.tar.gz"

# Extract regular files only after validating every archive member.
log "Extracting static files..."
STAGING_DIR="$TMP/static-staging"
rm -rf "$STAGING_DIR"
mkdir -p "$STAGING_DIR" "$BOARD_INSTALL_DIR/static"
python3 - "$TMP/screentra-cast-static.tar.gz" "$STAGING_DIR" <<'PY'
import pathlib, sys, tarfile
archive, target = map(pathlib.Path, sys.argv[1:])
with tarfile.open(archive, 'r:gz') as tf:
    members = tf.getmembers()
    for m in members:
        p = pathlib.PurePosixPath(m.name)
        if p.is_absolute() or '..' in p.parts or not m.isfile():
            raise SystemExit(f'unsafe static archive member: {m.name}')
    for m in members:
        out = target.joinpath(*pathlib.PurePosixPath(m.name).parts)
        out.parent.mkdir(parents=True, exist_ok=True)
        with tf.extractfile(m) as src, out.open('wb') as dst:
            dst.write(src.read())
        out.chmod(m.mode & 0o777)
PY
cp -a "$STAGING_DIR/." "$BOARD_INSTALL_DIR/static/"
rm -rf "$STAGING_DIR"
chown -R "$BOARD_USER:$BOARD_USER" "$BOARD_INSTALL_DIR/static/"
[ -d "$BOARD_INSTALL_DIR/static/js" ] || { log "FATAL: static extraction did not produce js/."; exit 1; }
log "Static files extracted OK."

if [ "$FRESH_INSTALL" = "true" ]; then
    provision_local_tls
fi

# Make the signed installer archive available to clients.
mkdir -p "$BOARD_INSTALL_DIR/static/install"
_dl "$INSTALLERS_URL" "$TMP/screentra-cast-installers.tar.gz"
verify_release_asset "screentra-cast-installers.tar.gz"
cp "$TMP/screentra-cast-installers.tar.gz" "$BOARD_INSTALL_DIR/static/install/"
chown "$BOARD_USER:$BOARD_USER" "$BOARD_INSTALL_DIR/static/install/screentra-cast-installers.tar.gz"
log "Client installers tarball saved to static/install/."

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
    env_lines[SCREENTRA_CAST_SERVER_URL]="https://${TARGET_HOST}:3000"
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
    if curl --fail --silent --show-error --cacert "$CERT_DIR/screentra-cast-local-ca.crt" \
        "https://127.0.0.1:$port/api/version" > /dev/null 2>&1; then
        log "Server is up on HTTPS port $port."
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

Cleanup (run as root after stopping the service):
  systemctl stop $SERVICE_NAME
  rm -rf $BOARD_HOME
  userdel $BOARD_USER

EOF
