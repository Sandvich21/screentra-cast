#!/usr/bin/env bash
set -euo pipefail

SERVER_URL="${SCREENTRA_SERVER_URL:-${RIPLEY_SERVER_URL:-}}"
DISCOVERED_CA_B64=""
DISCOVERED_SERVER_NAME=""
INSTALL_DIR="${SCREENTRA_CLIENT_DIR:-/home/${SCREENTRA_BOARD_USER:-board}/.local/share/screentra-client}"
BIN_DIR="$HOME/.local/bin"
SYSTEMD_USER_DIR="$HOME/.config/systemd/user"
CLIENT_ID_FILE="$INSTALL_DIR/client-id"
HEARTBEAT="$INSTALL_DIR/screentra-client-heartbeat.py"
RENDERER="$INSTALL_DIR/screentra-client-renderer.py"
GPU_HEALTH="$INSTALL_DIR/screentra-gpu-health.py"
KIOSK_LAUNCHER="$INSTALL_DIR/screentra-kiosk.sh"
PREFLIGHT="$INSTALL_DIR/screentra-client-preflight.py"
DISPLAY_GOVERNANCE="$INSTALL_DIR/screentra-display-governance.py"
STATUS="$BIN_DIR/screentra-client-status"
ACTOR="$INSTALL_DIR/screentra-client-actor.py"
TRANSCRIPTION_ADMISSION="$INSTALL_DIR/screentra-stt-transcription-request-admission.py"
ROLLBACK_ADMISSION="$INSTALL_DIR/screentra-client-service-rollback-request-admission.py"
STATUS_ADMISSION="$INSTALL_DIR/screentra-client-service-status-request-admission.py"
STATUS_REPORT_ADMISSION="$INSTALL_DIR/screentra-client-status-report-admission.py"
ACTOR_SERVICE_FILE="$SYSTEMD_USER_DIR/screentra-client-actor.service"
ACTOR_SERVICE_NAME="screentra-client-actor.service"
SERVICE="$SYSTEMD_USER_DIR/screentra-client-heartbeat.service"
RENDER_SERVICE="$SYSTEMD_USER_DIR/screentra-client-renderer@.service"
KIOSK_SERVICE="$SYSTEMD_USER_DIR/screentra-kiosk-browser@.service"
SYSTEMD_KIOSK_UNIT="/etc/systemd/system/screentra-cast-kiosk@.service"
INSTALLER_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
MAX_SERVER_URL_BYTES=2048

read_bounded_file() {
    local path="$1" maximum="$2" value
    value="$(head -c "$((maximum + 1))" "$path" 2>/dev/null || true)"
    if [ "${#value}" -gt "$maximum" ]; then
        return 1
    fi
    printf '%s' "$value"
}

read_saved_server_url() {
    read_bounded_file "$1" "$MAX_SERVER_URL_BYTES"
}

discover_server() {
    local result
    if ! result="$(python3 - <<'PY_DISCOVERY'
#!/usr/bin/env python3
"""Discover Screentra Cast servers on the local IPv4 broadcast domain."""
from __future__ import annotations

import argparse
import base64
import hashlib
import json
import os
import socket
import sys
import time
import urllib.parse

DISCOVERY_PROTOCOL = "screentra-cast-discovery-v1"
DISCOVERY_REQUEST = b"SCREENTRA_CAST_DISCOVER_V1"
MAX_RESPONSE_BYTES = 64 * 1024
MAX_DISCOVERY_RECORDS = 128
_DISCOVERY_FIELDS = frozenset({
    "protocol", "server_name", "url", "ca_pem_b64", "ca_sha256",
})


def sha256_bytes(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def validate_record(raw: object) -> dict[str, str] | None:
    if not isinstance(raw, dict) or set(raw) != _DISCOVERY_FIELDS or raw.get("protocol") != DISCOVERY_PROTOCOL:
        return None
    name = raw.get("server_name")
    url = raw.get("url")
    ca_b64 = raw.get("ca_pem_b64")
    ca_hash = raw.get("ca_sha256")
    try:
        if not isinstance(name, str) or not name.strip() or len(name.encode('utf-8')) > 128:
            return None
        if not isinstance(url, str) or len(url.encode('utf-8')) > 512:
            return None
    except UnicodeEncodeError:
        return None
    if any(ord(char) < 32 or ord(char) == 127 for char in name):
        return None
    parsed = urllib.parse.urlparse(url)
    if parsed.scheme != "https" or not parsed.hostname or parsed.username or parsed.password:
        return None
    if parsed.path not in ("", "/") or parsed.query or parsed.fragment:
        return None
    try:
        port = parsed.port or 443
    except ValueError:
        return None
    if not 1 <= port <= 65535:
        return None
    if not isinstance(ca_b64, str) or len(ca_b64) > 128 * 1024:
        return None
    try:
        ca = base64.b64decode(ca_b64, validate=True)
    except Exception:
        return None
    if b"-----BEGIN CERTIFICATE-----" not in ca or len(ca) > 64 * 1024:
        return None
    if not isinstance(ca_hash, str) or ca_hash != sha256_bytes(ca):
        return None
    return {
        "server_name": name.strip(),
        "url": url.rstrip("/"),
        "ca_pem_b64": ca_b64,
        "ca_sha256": ca_hash,
    }


def _reject_duplicate_json_keys(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError("discovery JSON contains duplicate fields")
        result[key] = value
    return result


def _parse_discovery_json(text):
    try:
        return json.loads(
            text,
            object_pairs_hook=_reject_duplicate_json_keys,
            parse_constant=lambda _: (_ for _ in ()).throw(ValueError("non-standard JSON constant")),
        )
    except (TypeError, ValueError, UnicodeError) as exc:
        raise ValueError("discovery response is not valid JSON") from exc


def discover(port: int, timeout: float) -> list[dict[str, str]]:
    request = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    request.setsockopt(socket.SOL_SOCKET, socket.SO_BROADCAST, 1)
    request.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    request.bind(("", 0))
    request.settimeout(0.25)
    try:
        request.sendto(DISCOVERY_REQUEST, ("255.255.255.255", port))
        deadline = time.monotonic() + timeout
        records: dict[tuple[str, str], dict[str, str]] = {}
        while time.monotonic() < deadline:
            try:
                data, _peer = request.recvfrom(MAX_RESPONSE_BYTES)
            except socket.timeout:
                continue
            try:
                raw = _parse_discovery_json(data.decode("utf-8"))
            except (UnicodeDecodeError, ValueError):
                continue
            record = validate_record(raw)
            if record:
                records[(record["url"], record["ca_sha256"])] = record
                if len(records) >= MAX_DISCOVERY_RECORDS:
                    break
        return sorted(records.values(), key=lambda value: (value["server_name"].lower(), value["url"]))
    finally:
        request.close()


def choose(records: list[dict[str, str]]) -> dict[str, str] | None:
    if not records:
        return None
    tty = None
    try:
        tty = open("/dev/tty", "r+", encoding="utf-8")
    except OSError:
        pass
    try:
        if len(records) == 1:
            record = records[0]
            print(
                f"Found Screentra Cast server '{record['server_name']}' at {record['url']}.",
                file=sys.stderr,
            )
            if os.environ.get("SCREENTRA_DISCOVERY_AUTO_ACCEPT") == "1":
                return record
            if tty is None:
                return None
            print("Use this server? [Y/n] ", end="", file=tty, flush=True)
            answer = tty.readline().strip().lower()
            return None if answer not in ("", "y", "yes") else record

        if tty is None:
            return None
        print("Discovered Screentra Cast servers:", file=tty)
        for index, record in enumerate(records, 1):
            print(f"  {index}) {record['server_name']} ({record['url']})", file=tty)
        print("Select a server number, or press Enter to cancel: ", end="", file=tty, flush=True)
        answer = tty.readline().strip()
        if not answer.isdigit() or not 1 <= int(answer) <= len(records):
            return None
        return records[int(answer) - 1]
    finally:
        if tty is not None:
            tty.close()


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--port", type=int, default=3911)
    parser.add_argument("--timeout", type=float, default=2.5)
    args = parser.parse_args()
    if not 1 <= args.port <= 65535 or not 0.2 <= args.timeout <= 10:
        return 2
    selected = choose(discover(args.port, args.timeout))
    if selected is None:
        return 1
    print(selected["url"])
    print(selected["ca_pem_b64"])
    print(selected["server_name"])
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

PY_DISCOVERY
)"; then
        return 1
    fi
    local values=()
    mapfile -t values <<<"$result"
    if [ -z "${values[0]:-}" ] || [ -z "${values[1]:-}" ]; then
        return 1
    fi
    SERVER_URL="${values[0]}"
    DISCOVERED_CA_B64="${values[1]}"
    DISCOVERED_SERVER_NAME="${values[2]:-Screentra Cast}"
    return 0
}

is_ssh_session() {
    if [ -n "${SSH_CONNECTION:-}" ]; then return 0; fi
    if [ -n "${SSH_CLIENT:-}" ]; then return 0; fi
    if [ -n "${SSH_TTY:-}" ]; then return 0; fi
    local ppid=$$
    for _ in 1 2 3 4 5 6 7 8 9 10; do
        ppid="$(ps -o ppid= -p "$ppid" 2>/dev/null | tr -d ' ' || true)"
        [ -z "$ppid" ] && break
        local cmd
        cmd="$(ps -o comm= -p "$ppid" 2>/dev/null | tr -d ' ' || true)"
        case "$cmd" in
            sshd*|ssh-agent*) return 0 ;;
        esac
    done
    return 1
}

INTERVAL="${RIPLEY_HEARTBEAT_INTERVAL:-30}"
ROLE="${RIPLEY_CLIENT_ROLE:-player}"
CLIENT_NAME="${RIPLEY_CLIENT_NAME:-$(hostname)}"
VERSION="${RIPLEY_CLIENT_VERSION:-0.1.0}"
CLIENT_VERSION="$VERSION"
SOURCE_TARBALL_DIR="${RIPLEY_CLIENT_SOURCE_DIR:-/home/ripley/screentra-client-client}"
# Short git hash or "dev" if .git is missing. Used in no-layout.html footer.
BUILD="$(git -C "$SOURCE_TARBALL_DIR" rev-parse --short HEAD 2>/dev/null || echo dev)"

mkdir -p "$INSTALL_DIR" "$BIN_DIR" "$SYSTEMD_USER_DIR"
# Sanitize VERSION: must be a single shell-safe token. Anything with
# whitespace, parens, or shell metacharacters is replaced with the
# default. This prevents '13 (trixie)' or '0.1.0; rm -rf /' from
# corrupting the unit file's ExecStart line.
if printf '%s' "$VERSION" | grep -qE '[^A-Za-z0-9._-]'; then
    echo "WARN: VERSION value '$VERSION' contains unsafe characters; defaulting to 0.1.0"
    VERSION="0.1.0"
fi
printf "%s\n" "$VERSION" > "$INSTALL_DIR/VERSION"
if [ -z "$SERVER_URL" ] && [ -f "$INSTALL_DIR/server" ]; then
    SERVER_URL="$(read_saved_server_url "$INSTALL_DIR/server" 2>/dev/null || true)"
fi
if [ -z "$SERVER_URL" ]; then
    if discover_server; then
        echo "Using discovered Screentra Cast server: $DISCOVERED_SERVER_NAME ($SERVER_URL)"
    fi
fi
if [ -z "$SERVER_URL" ]; then
    if [ -t 0 ] && [ -t 1 ]; then
        # Interactive terminal (real TTY or 'ssh -t'): prompt the user.
        echo "Screentra player installer — server URL required."
        echo
        echo "Enter the Screentra Cast server URL (e.g. https://server.example:3000):"
        read -r SERVER_URL < /dev/tty || SERVER_URL=""
        while [ -z "$SERVER_URL" ]; do
            echo "Server URL cannot be empty. Enter the Screentra Cast server URL:"
            read -r SERVER_URL < /dev/tty || SERVER_URL=""
        done
        # Trim trailing slash, normalize scheme.
        SERVER_URL="${SERVER_URL%/}"
        case "$SERVER_URL" in
            http://*|https://*) ;;
            *) SERVER_URL="https://$SERVER_URL" ;;
        esac
        echo "Using server: $SERVER_URL"
    elif is_ssh_session; then
        # Non-interactive SSH session: explain how to pass the URL and how to
        # bootstrap the seat service so the kiosk can take over the display
        # once the user is at a real TTY (or after they start the seat service
        # from this SSH session).
        cat <<SSH_HINT >&2
ERROR: SCREENTRA_SERVER_URL is required for player install.

You are running this over SSH, so the installer cannot prompt for the URL.
Re-run with the URL set, for example:

  SCREENTRA_SERVER_URL=https://<server-host-or-ip>:3000 $0

Or pipe it through ssh:

  ssh user@host "SCREENTRA_SERVER_URL=https://<server-host-or-ip>:3000 $0"

Or use 'ssh -t' to allocate a TTY and let the installer prompt:

  ssh -t user@host $0

After install completes, start the system-level seat service so the kiosk
takes over a real TTY (you can do this from the same SSH session):

  sudo systemctl enable --now screentra-cast-kiosk@0.service

This will spawn  on the next free TTY and run the kiosk browser.
SSH_HINT
        exit 1
    else
        echo "ERROR: SCREENTRA_SERVER_URL is required for player install." >&2
        echo "Example: SCREENTRA_SERVER_URL=https://<server-host-or-ip>:3000 $0" >&2
        echo "Or run interactively from a terminal so the installer can prompt." >&2
        exit 1
    fi
fi

SERVER_URL="${SERVER_URL%/}"
if [[ "$SERVER_URL" != https://* && ! "$SERVER_URL" =~ ^http://(127\.0\.0\.1|localhost|::1)(:[0-9]+)?(/.*)?$ ]]; then
    echo "ERROR: remote player installs require an https:// server URL." >&2
    exit 1
fi

run_privileged() {
    if [ "$(id -u)" = "0" ]; then
        "$@"
    elif command -v sudo >/dev/null 2>&1; then
        if [ -t 0 ]; then sudo "$@"; else sudo -n "$@"; fi
    else
        return 1
    fi
}

install_client_update_public_key() {
    local source="${SCREENTRA_CLIENT_UPDATE_PUBLIC_KEY_SOURCE:-$INSTALLER_DIR/screentra-cast-client-update.pub}"
    local target="/etc/screentra-cast/client-update-signing.pub"
    if [ -s "$source" ] && grep -q 'BEGIN PUBLIC KEY' "$source"; then
        if [ -s "$target" ] && ! cmp -s "$source" "$target"; then
            echo "ERROR: bundled client-update public key does not match the existing pinned key at $target." >&2
            return 1
        fi
        if [ ! -s "$target" ]; then
            run_privileged install -D -o root -g root -m 0644 "$source" "$target"
            echo "Installed pinned client-update public key at $target."
        else
            echo "Verified existing pinned client-update public key at $target."
        fi
    elif [ ! -s "$target" ]; then
        echo "WARNING: client-update public key is missing; client self-updates will remain disabled." >&2
    fi
}


install_screentra_cast_service() {
    echo "Installing screentra-cast-kiosk@.service..."
    # Stage in a root-owned temporary directory; never use a predictable /tmp
    # pathname for a file that will be installed by root.
    local stage_dir
    stage_dir="$(run_privileged mktemp -d /run/screentra-cast-installer.XXXXXX)"
    run_privileged chmod 0700 "$stage_dir"
    local unit_src="$stage_dir/screentra-cast-kiosk@.service"
    run_privileged tee "$unit_src" > /dev/null <<'EOF_UNIT'
[Unit]
Description=Screentra Cast Kiosk Display %i
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=root
StandardInput=null
StandardOutput=journal
StandardError=journal
PrivateTmp=yes
ProtectSystem=full
ProtectKernelTunables=yes
ProtectKernelModules=yes
ProtectControlGroups=yes
ProtectKernelLogs=yes
LockPersonality=yes
RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6 AF_NETLINK
ExecStart=/usr/local/bin/screentra-kiosk.sh
Restart=always
RestartSec=5
# Actor-triggered kiosk refreshes terminate this wrapper with SIGTERM. Treat
# that controlled handoff as a successful stop rather than a crash.
SuccessExitStatus=143 SIGTERM
Environment=HOME=/home/board
# AMD amdgpu APUs need the GLES2 renderer for reliable DRM scan-out.
# Pixman can start Sway but repeatedly fails to import the Chromium buffer,
# leaving the physical output black.
Environment=WLR_RENDERER=gles2
Environment=SCREENTRA_DISPLAY_INDEX=%i
Environment=SCREENTRA_RENDER_BASE_PORT=3900
Environment=SCREENTRA_RENDERER_URL=http://127.0.0.1:3900/
# Use the local renderer capability exchange, then redirect to the canonical
# server player so physical displays use the same fonts and sizing as Preview.
# RTSP layouts remain on the local cached renderer path.
Environment=SCREENTRA_LIVE_PLAYER_REDIRECT=1
Environment=SCREENTRA_BOARD_USER=board

[Install]
WantedBy=multi-user.target
EOF_UNIT
    # Copy kiosk script from install dir (bundled by build-installer.py as static file)
    run_privileged install -m 0755 "$INSTALL_DIR/screentra-kiosk.sh" /usr/local/bin/screentra-kiosk.sh
    # Install and enable the systemd unit
    run_privileged install -m 0644 "$unit_src" /etc/systemd/system/screentra-cast-kiosk@.service
    run_privileged systemctl daemon-reload
    if ! run_privileged systemctl enable --now screentra-cast-kiosk@0.service; then
        echo "WARNING: could not start screentra-cast-kiosk@0.service from this session; it remains enabled for the next physical seat login."
    fi
    run_privileged rm -rf -- "$stage_dir"
    echo "screentra-cast-kiosk@.service installed and enabled."
}
# Install a root-owned update verifier. The unprivileged actor may only create
# a bounded request marker; it cannot choose the server, URL, signature key, or
# executable that root will run.
install_root_verified_updater() {
    local board_user="${SCREENTRA_BOARD_USER:-board}"
    local update_dir="/var/lib/screentra-cast/update"
    local request_file="$update_dir/request"
    local UPDATE_REQUEST_FILE="$request_file"
    local result_file="$update_dir/result.json"
    local UPDATER_SCRIPT="/usr/local/libexec/screentra-cast-client-updater.py"
    local updater_env="/etc/screentra-cast/client-update.env"
    local updater_unit="/etc/systemd/system/screentra-client-updater.service"
    local updater_path="/etc/systemd/system/screentra-client-updater.path"
    local digest_file="/etc/screentra-cast/client-update.sha256"
    run_privileged install -d -o root -g "$board_user" -m 0730 /var/lib/screentra-cast "$update_dir"
    run_privileged install -d -o root -g root -m 0755 /usr/local/libexec /etc/screentra-cast
    run_privileged touch "$result_file"
    run_privileged chown root:"$board_user" "$result_file"
    run_privileged chmod 0660 "$result_file"
    run_privileged tee "$updater_env" >/dev/null <<EOF_UPDATE_ENV
SERVER_URL=$SERVER_URL
CLIENT_ID=$CLIENT_ID
INSTALL_DIR=$INSTALL_DIR
CA_FILE=$INSTALL_DIR/screentra-cast-signage-ca.crt
AUTH_TOKEN_FILE=$CLIENT_AUTH_TOKEN_FILE
PUBLIC_KEY_FILE=/etc/screentra-cast/client-update-signing.pub
DIGEST_FILE=$digest_file
REQUEST_FILE=$request_file
RESULT_FILE=$result_file
EOF_UPDATE_ENV
    run_privileged chmod 0600 "$updater_env"
    run_privileged tee "$UPDATER_SCRIPT" >/dev/null <<'PY_UPDATER'
#!/usr/bin/env python3
import hashlib
import json
import os
import pathlib
import ssl
import subprocess
import tempfile
import time
import urllib.parse
import urllib.request

MAX_MANIFEST = 64 * 1024
MAX_INSTALLER = 16 * 1024 * 1024
MAX_SIGNATURE = 16 * 1024
EXPECTED_ARTIFACT_PATH = '/install/player-client.sh'
EXPECTED_SIGNATURE_PATH = '/install/player-client.sh.sig'


def same_origin(server: str, candidate: str) -> bool:
    base = urllib.parse.urlparse(server)
    parsed = urllib.parse.urlparse(candidate)
    return (parsed.scheme, parsed.hostname, parsed.port or (443 if parsed.scheme == 'https' else 80)) == (
        base.scheme, base.hostname, base.port or (443 if base.scheme == 'https' else 80)
    )


def bounded_read(response, limit: int) -> bytes:
    data = response.read(limit + 1)
    if len(data) > limit:
        raise RuntimeError('update response exceeded its size limit')
    return data


def bounded_read_text(path: pathlib.Path, limit: int, encoding: str = 'utf-8') -> str:
    with path.open('rb') as stream:
        data = stream.read(limit + 1)
    if len(data) > limit:
        raise RuntimeError('local update file exceeded its size limit')
    return data.decode(encoding)


def bounded_read_file(path: pathlib.Path, limit: int) -> bytes:
    with path.open('rb') as stream:
        data = stream.read(limit + 1)
    if len(data) > limit:
        raise RuntimeError('local update file exceeded its size limit')
    return data


def write_result(result_file: pathlib.Path, result: dict) -> None:
    temporary = result_file.with_name('.result.tmp')
    temporary.write_text(json.dumps(result, separators=(',', ':'))[:8192] + '\n', encoding='utf-8')
    temporary.chmod(0o660)
    os.replace(temporary, result_file)


def main() -> int:
    server = os.environ['SERVER_URL'].rstrip('/')
    client_id = os.environ['CLIENT_ID']
    install_dir = pathlib.Path(os.environ['INSTALL_DIR'])
    ca_file = pathlib.Path(os.environ['CA_FILE'])
    auth_token_file = pathlib.Path(os.environ['AUTH_TOKEN_FILE'])
    public_key_file = pathlib.Path(os.environ['PUBLIC_KEY_FILE'])
    digest_file = pathlib.Path(os.environ['DIGEST_FILE'])
    request_file = pathlib.Path(os.environ['REQUEST_FILE'])
    result_file = pathlib.Path(os.environ['RESULT_FILE'])
    if not server.startswith('https://'):
        raise RuntimeError('remote update requires HTTPS')
    if not ca_file.is_file() or not public_key_file.is_file() or not auth_token_file.is_file():
        raise RuntimeError('trusted update material is missing')
    if not request_file.is_file():
        raise RuntimeError('update request marker is missing')
    marker = json.loads(bounded_read_text(request_file, 4096))
    if not isinstance(marker, dict) or set(marker) - {'requested_at', 'client_id', 'reason'}:
        raise RuntimeError('update request marker is invalid')
    token = bounded_read_text(auth_token_file, 512).strip()
    if not token or len(token) > 512:
        raise RuntimeError('client token is invalid')
    current = bounded_read_text(digest_file, 128, 'ascii').strip() if digest_file.is_file() else ''
    if current and (len(current) != 64 or any(c not in '0123456789abcdef' for c in current.lower())):
        current = ''
    check_url = server + '/api/clients/' + urllib.parse.quote(client_id, safe='') + '/update-check?current_sha256=' + urllib.parse.quote(current, safe='')
    context = ssl.create_default_context(cafile=str(ca_file))
    headers = {'X-SCREENTRA-CLIENT-TOKEN': token, 'Accept': 'application/json'}
    request = urllib.request.Request(check_url, headers=headers)
    with urllib.request.urlopen(request, context=context, timeout=30) as response:
        manifest = json.loads(bounded_read(response, MAX_MANIFEST).decode('utf-8'))
    if not isinstance(manifest, dict) or not manifest.get('update_required'):
        request_file.unlink(missing_ok=True)
        write_result(result_file, {'ok': True, 'updated': False, 'checked_at': time.time()})
        return 0
    artifact_url = urllib.parse.urljoin(server + '/', str(manifest.get('artifact_url', '')))
    signature_url = urllib.parse.urljoin(server + '/', str(manifest.get('signature_url', '')))
    expected_path: str = '/install/player-client.sh'
    if not same_origin(server, str(manifest.get("artifact_url"))) or urllib.parse.urlparse(artifact_url).path != expected_path:
        raise RuntimeError('update artifact origin or path is invalid')
    if not same_origin(server, str(manifest.get('signature_url', ''))) or urllib.parse.urlparse(signature_url).path != EXPECTED_SIGNATURE_PATH:
        raise RuntimeError('update signature origin or path is invalid')
    expected_sha = str(manifest.get('sha256', '')).lower()
    if len(expected_sha) != 64 or any(c not in '0123456789abcdef' for c in expected_sha):
        raise RuntimeError('update digest is invalid')
    with tempfile.TemporaryDirectory(prefix='screentra-client-update-', dir='/var/lib/screentra-cast/update') as temp:
        installer = pathlib.Path(temp) / 'player-client.sh'
        signature = pathlib.Path(temp) / 'player-client.sh.sig'
        with urllib.request.urlopen(urllib.request.Request(artifact_url, headers=headers), context=context, timeout=30) as response:
            installer.write_bytes(bounded_read(response, MAX_INSTALLER))
        with urllib.request.urlopen(urllib.request.Request(signature_url, headers=headers), context=context, timeout=30) as response:
            signature.write_bytes(bounded_read(response, MAX_SIGNATURE))
        actual_sha = hashlib.sha256(bounded_read_file(installer, MAX_INSTALLER)).hexdigest()
        if actual_sha != expected_sha:
            raise RuntimeError('update digest verification failed')
        verify = subprocess.run(["openssl", "pkeyutl", "-verify", "-rawin", "-pubin", "-inkey", str(public_key_file), "-sigfile", str(signature), "-in", str(installer)], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=20)
        if verify.returncode != 0:
            raise RuntimeError('update signature verification failed')
        installer.chmod(0o700)
        environment = os.environ.copy()
        environment.update({'SCREENTRA_SERVER_URL': server, 'RIPLEY_CLIENT_ID': client_id, 'SCREENTRA_CLIENT_DIR': str(install_dir)})
        completed = subprocess.run(['/bin/bash', str(installer)], env=environment, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=300)
        if completed.returncode != 0:
            raise RuntimeError('verified installer failed')
        digest_file.parent.mkdir(parents=True, exist_ok=True)
        digest_file.write_text(actual_sha + '\n', encoding='ascii')
        digest_file.chmod(0o644)
    request_file.unlink(missing_ok=True)
    write_result(result_file, {'ok': True, 'updated': True, 'sha256': expected_sha, 'updated_at': time.time()})
    return 0


if __name__ == '__main__':
    try:
        raise SystemExit(main())
    except Exception as exc:
        try:
            write_result(pathlib.Path(os.environ['RESULT_FILE']), {'ok': False, 'error': str(exc)[:400]})
        finally:
            raise SystemExit(1)
PY_UPDATER
    run_privileged chmod 0750 "$UPDATER_SCRIPT"
    run_privileged chown root:root "$UPDATER_SCRIPT"
    run_privileged tee "$updater_unit" >/dev/null <<EOF_UPDATER_UNIT
[Unit]
Description=Screentra Cast root-verified client updater
After=network-online.target
Wants=network-online.target

[Service]
Type=oneshot
User=root
EnvironmentFile=$updater_env
ExecStart=$UPDATER_SCRIPT
EOF_UPDATER_UNIT
    run_privileged tee "$updater_path" >/dev/null <<EOF_UPDATER_PATH
[Unit]
Description=Watch for Screentra Cast client update requests

[Path]
PathExists=$UPDATE_REQUEST_FILE
Unit=screentra-client-updater.service

[Install]
WantedBy=multi-user.target
EOF_UPDATER_PATH
    run_privileged chmod 0644 "$updater_unit" "$updater_path"
    run_privileged systemctl daemon-reload
    run_privileged systemctl enable --now screentra-client-updater.path
}
# ── Create the appliance board user ───────────────────────────────────────────
# The board user runs the display compositor (sway) and renderer.
create_board_user() {
    local board_user="${SCREENTRA_BOARD_USER:-board}"
    local board_home="/home/$board_user"
    if id "$board_user" &>/dev/null; then
        echo "[board] user already exists"
    else
        echo "[board] creating board user..."
        run_privileged useradd --create-home --home-dir "$board_home" --shell /bin/bash "$board_user"
    fi
    if [ ! -d "$board_home" ]; then
        run_privileged mkdir -p "$board_home"
    fi
    run_privileged install -d -o "$board_user" -g "$board_user" -m 0755 "$board_home"
    run_privileged usermod -aG video,render,input,tty,audio "$board_user" 2>/dev/null || true
    run_privileged gpasswd -d "$board_user" sudo 2>/dev/null || true
    # Grant only the device-control commands used by the actor. Do not add the
    # player account to sudo or grant unrestricted NOPASSWD access.
    run_privileged tee /etc/sudoers.d/"$board_user" > /dev/null << 'SUDOERS'
Cmnd_Alias SCREENTRA_DEVICE_CONTROL = /usr/bin/ddcutil, /usr/bin/cec-client, /usr/bin/systemctl start screentra-cast-kiosk@*.service, /usr/bin/systemctl stop screentra-cast-kiosk@*.service, /usr/bin/systemctl restart screentra-cast-kiosk@*.service, /usr/bin/systemctl try-restart screentra-cast-kiosk@*.service, /usr/bin/systemctl disable --now screentra-cast-kiosk@*.service, /usr/bin/systemctl reboot, /usr/sbin/reboot
board ALL=(root) NOPASSWD: SCREENTRA_DEVICE_CONTROL
SUDOERS
    run_privileged chmod 0440 /etc/sudoers.d/"$board_user" 2>/dev/null || true
    run_privileged chown -R "$board_user:$board_user" "$INSTALL_DIR"
    echo "[board] board user ready at $board_home"
}



is_ssh_session_quiet() {
    if is_ssh_session; then
        echo "Detected SSH session (SSH_CONNECTION=${SSH_CONNECTION:-unset})."
        echo "Install will configure the kiosk as a system-level service so a real TTY"
        echo "or systemd-logind can take over the display. The current SSH session will"
        echo "not display the kiosk; use the editor to verify the client and then start"
        echo "the seat service from a real TTY or via 'sudo systemctl start screentra-cast-kiosk@0.service'."
    fi
}
# Detect the host OS. Sets $OS_ID, $OS_VERSION_ID, $OS_PRETTY, $OS_FAMILY
# (debian, ubuntu, fedora, arch, alpine, rhel, suse, other).
detect_os() {
    OS_ID=""
    OS_VERSION_ID=""
    OS_PRETTY=""
    OS_FAMILY="other"
    OS_PKG=""
    OS_REQUIRES_SUDO_UPDATE=0
    if [ -r /etc/os-release ]; then
        # shellcheck disable=SC1091
        . /etc/os-release
        OS_ID="${ID:-}"
        OS_VERSION_ID="${VERSION_ID:-}"
        OS_PRETTY="${PRETTY_NAME:-${ID:-}}"
    fi
    case "$OS_ID" in
        debian|raspbian|linuxmint|pop|elementary|zorin|kali|ubuntu)
            OS_FAMILY="debian"
            OS_PKG="apt-get"
            ;;
        fedora|nobara)
            OS_FAMILY="fedora"
            OS_PKG="dnf"
            ;;
        rhel|centos|rocky|almalinux|ol)
            OS_FAMILY="rhel"
            OS_PKG="yum"
            ;;
        arch|manjaro|endeavouros)
            OS_FAMILY="arch"
            OS_PKG="pacman"
            ;;
        alpine)
            OS_FAMILY="alpine"
            OS_PKG="apk"
            ;;
        opensuse*|sles)
            OS_FAMILY="suse"
            OS_PKG="zypper"
            ;;
        *)
            OS_FAMILY="other"
            ;;
    esac
    echo "Detected OS: ${OS_PRETTY:-unknown} (id=${OS_ID:-?}, version=${OS_VERSION_ID:-?}, family=${OS_FAMILY}, pkg=${OS_PKG:-none})"
}

# Map of additional packages required for the kiosk, by OS family.
# python3 is required by the heartbeat/renderer/actor — handled separately.
# Browser,  seat, seatd, and TLS tools are what we need to install.
required_packages() {
    case "$OS_FAMILY" in
        debian)
            # python3, sudo, curl, wget, openssl, ca-certificates, dbus, logind
            # display:  (only supported kiosk compositor) + sway (for wlr-randr)
            # browser: chromium
            echo "python3 python3-venv sudo curl wget openssl ca-certificates dbus libnss3-tools  sway wlrctl seatd chromium ddcutil cec-utils ffmpeg firmware-amd-graphics mesa-va-drivers vainfo"
            ;;
        fedora|rhel)
            echo "python3 sudo curl wget openssl ca-certificates dbus nss-tools  seatd chromium ffmpeg"
            ;;
        arch)
            echo "python sudo curl wget openssl ca-certificates dbus nss  seatd chromium ffmpeg"
            ;;
        alpine)
            echo "python3 sudo curl wget openssl ca-certificates dbus  seatd chromium ffmpeg"
            ;;
        suse)
            echo "python3 sudo curl wget openssl ca-certificates dbus libnss3-tools  seatd chromium ffmpeg"
            ;;
        *)
            echo ""
            ;;
    esac
}

# Install any missing required packages using the host package manager.
# Silent on success; prompts when interactive and requires elevation.
install_os_dependencies() {
    if [ -z "$OS_PKG" ]; then
        echo "Unknown OS family; skipping package install. Please install python3, a browser, and a Wayland seat manually."
        return 0
    fi
    local pkgs
    pkgs="$(required_packages)"
    [ -z "$pkgs" ] && return 0
    local missing=""
    local p
    for p in $pkgs; do
        if ! command -v "${p%%-*}" >/dev/null 2>&1 && ! dpkg -s "$p" >/dev/null 2>&1 && ! rpm -q "$p" >/dev/null 2>&1 && ! apk info -e "$p" >/dev/null 2>&1; then
            missing="$missing $p"
        fi
    done
    if [ -z "$missing" ]; then
        echo "All required packages already present."
        return 0
    fi
    echo "Missing packages:$missing"
    if [ -t 0 ] && [ -t 1 ]; then
        read -r -p "Install missing packages with $OS_PKG? [Y/n] " ans < /dev/tty || ans="y"
        case "$ans" in [nN]*) echo "Skipped. Install manually: $OS_PKG install$missing"; return 0;; esac
    fi
    case "$OS_PKG" in
        apt-get)
            run_privileged apt-get update
            run_privileged apt-get install -y $missing
            ;;
        dnf|yum)
            run_privileged $OS_PKG install -y $missing
            ;;
        pacman)
            run_privileged pacman -Sy --noconfirm $missing
            ;;
        apk)
            run_privileged apk add $missing
            ;;
        zypper)
            run_privileged zypper --non-interactive install $missing
            ;;
        *)
            echo "Unsupported package manager: $OS_PKG"
            return 1
            ;;
    esac
    echo "Installed:$missing"
}


install_gpu_telemetry_dependencies() {
    # Best effort telemetry support for the local admin GPU card. We install
    # safe user-space discovery tools automatically, but we do NOT blindly
    # switch GPU drivers (especially NVIDIA) because that can break Wayland.
    [ -z "${OS_PKG:-}" ] && return 0
    local gpu_lines vendors pkgs missing p
    gpu_lines="$( (command -v lspci >/dev/null 2>&1 && lspci -nn | grep -Ei 'VGA compatible controller|3D controller|Display controller') || true )"
    vendors="$(printf '%s\n' "$gpu_lines" | tr '[:upper:]' '[:lower:]')"
    # If pciutils/lspci is not installed yet, fall back to DRM PCI vendor IDs.
    for vf in /sys/class/drm/card*/device/vendor; do
        [ -r "$vf" ] || continue
        case "$(read_bounded_file "$vf" 128 2>/dev/null | tr '[:upper:]' '[:lower:]')" in
            0x10de) vendors="$vendors nvidia" ;;
            0x8086) vendors="$vendors intel" ;;
            0x1002|0x1022) vendors="$vendors amd" ;;
        esac
    done
    pkgs=""
    case "$OS_FAMILY" in
        debian)
            pkgs="pciutils mesa-utils"
            if printf '%s' "$vendors" | grep -q 'intel'; then pkgs="$pkgs intel-gpu-tools"; fi
            if printf '%s' "$vendors" | grep -Eq 'amd|ati|advanced micro devices'; then pkgs="$pkgs firmware-amd-graphics"; fi
            if printf '%s' "$vendors" | grep -q 'nvidia'; then
                if ! command -v nvidia-smi >/dev/null 2>&1; then
                    if apt-cache show nvidia-smi >/dev/null 2>&1; then
                        pkgs="$pkgs nvidia-smi"
                    else
                        echo "NOTE: NVIDIA GPU detected, but nvidia-smi package was not found in enabled APT repos."
                        echo "      GPU card will show basic/partial telemetry unless NVIDIA utilities are installed."
                        echo "      Avoid blindly installing proprietary NVIDIA drivers on signage players; test Wayland/ first."
                    fi
                fi
            fi
            ;;
        fedora|rhel)
            pkgs="pciutils mesa-utils"
            if printf '%s' "$vendors" | grep -q 'intel'; then pkgs="$pkgs intel-gpu-tools"; fi
            ;;
        arch)
            pkgs="pciutils mesa-utils"
            if printf '%s' "$vendors" | grep -q 'intel'; then pkgs="$pkgs intel-gpu-tools"; fi
            ;;
        suse)
            pkgs="pciutils Mesa-demo-x"
            ;;
        alpine)
            pkgs="pciutils mesa-demos"
            ;;
        *) return 0 ;;
    esac
    missing=""
    for p in $pkgs; do
        case "$OS_PKG" in
            apt-get) dpkg -s "$p" >/dev/null 2>&1 || missing="$missing $p" ;;
            dnf|yum) rpm -q "$p" >/dev/null 2>&1 || missing="$missing $p" ;;
            pacman) pacman -Q "$p" >/dev/null 2>&1 || missing="$missing $p" ;;
            apk) apk info -e "$p" >/dev/null 2>&1 || missing="$missing $p" ;;
            zypper) rpm -q "$p" >/dev/null 2>&1 || missing="$missing $p" ;;
        esac
    done
    [ -z "$missing" ] && { echo "GPU telemetry packages already present or not needed."; return 0; }
    echo "Installing GPU telemetry/helper packages:$missing"
    case "$OS_PKG" in
        apt-get) run_privileged apt-get update; run_privileged apt-get install -y $missing || echo "WARNING: some GPU telemetry packages failed to install: $missing" ;;
        dnf|yum) run_privileged $OS_PKG install -y $missing || echo "WARNING: some GPU telemetry packages failed to install: $missing" ;;
        pacman) run_privileged pacman -Sy --noconfirm $missing || echo "WARNING: some GPU telemetry packages failed to install: $missing" ;;
        apk) run_privileged apk add $missing || echo "WARNING: some GPU telemetry packages failed to install: $missing" ;;
        zypper) run_privileged zypper --non-interactive install $missing || echo "WARNING: some GPU telemetry packages failed to install: $missing" ;;
    esac
}

# Make sure dbus is up — required for logind and chromium.
ensure_dbus() {
    if pgrep -x dbus-daemon >/dev/null 2>&1; then return 0; fi
    if [ -x /run/dbus ]; then return 0; fi
    if command -v systemctl >/dev/null 2>&1; then
        run_privileged systemctl enable --now dbus.service >/dev/null 2>&1 || true
    fi
}

# Record detected OS for later diagnostics (preflight uses this).
write_os_marker() {
    cat > "$INSTALL_DIR/os-info" <<EOF
id=${OS_ID}
version_id=${OS_VERSION_ID}
pretty=${OS_PRETTY}
family=${OS_FAMILY}
pkg=${OS_PKG}
detected_at=$(date -u +%Y-%m-%dT%H:%M:%SZ)
EOF
}

install_screentra_ca_trust() {
    local ca_file="$INSTALL_DIR/screentra-cast-signage-ca.crt"
    local installed=0
    echo "Installing Screentra Cast HTTPS trust anchor (operator-provided CA required)..."
    # Skip if the file already exists and looks like a valid PEM.
    if [ -s "$ca_file" ] && grep -q 'BEGIN CERTIFICATE' "$ca_file" 2>/dev/null; then
        echo "Screentra CA already present at $ca_file; skipping download."
    else
        local ca_source="${RIPLEY_CA_CERT:-${SCREENTRA_CA_CERT_SOURCE:-}}"
        if [ -n "$ca_source" ] && [ -s "$ca_source" ]; then
            install -m 0644 "$ca_source" "$ca_file"
            echo "Copied operator-provided Screentra Cast CA certificate to $ca_file"
        else
            echo "WARNING: no operator-provided CA certificate is available; refusing unverified CA bootstrap."
            echo "Set RIPLEY_CA_CERT or SCREENTRA_CA_CERT_SOURCE to the server CA before installing."
        fi
    fi
    if [ ! -s "$ca_file" ]; then
        echo "Screentra CA trust not installed: CA certificate unavailable."
        return 0
    fi
    if command -v update-ca-certificates >/dev/null 2>&1; then
        # Verify the file exists before we try to install it (the python
        # block above is best-effort and silently fails on TLS errors).
        if [ ! -s "$ca_file" ]; then
            echo "WARNING: CA file $ca_file is empty or missing; OS trust store not updated."
        elif run_privileged cp "$ca_file" /usr/local/share/ca-certificates/screentra-cast-signage-ca.crt && run_privileged update-ca-certificates; then
            if [ -f /usr/local/share/ca-certificates/screentra-cast-signage-ca.crt ]; then
                echo "Installed Screentra Cast CA into OS trust store."
                installed=1
            else
                echo "WARNING: update-ca-certificates ran but /usr/local/share/ca-certificates/screentra-cast-signage-ca.crt is missing; trust not added."
            fi
        else
            echo "WARNING: could not install Screentra Cast CA into OS trust store; run manually if browser trust is needed:"
            echo "  sudo cp '$ca_file' /usr/local/share/ca-certificates/screentra-cast-signage-ca.crt && sudo update-ca-certificates"
        fi
    fi
    if command -v certutil >/dev/null 2>&1; then
        # Chromium uses the NSS database for the display user. When this
        # installer is invoked through sudo, HOME points at /root, so writing
        # only the invoking user's NSS database leaves the board browser with
        # ERR_CERT_AUTHORITY_INVALID even though curl trusts the system CA.
        local board_user="${SCREENTRA_BOARD_USER:-board}"
        local board_home="/home/$board_user"
        local nss_dir="$board_home/.pki/nssdb"
        if id "$board_user" >/dev/null 2>&1 && [ -d "$board_home" ]; then
            run_privileged install -d -o "$board_user" -g "$board_user" -m 0700 "$board_home/.pki" "$nss_dir"
            if [ ! -f "$nss_dir/cert9.db" ]; then
                run_privileged runuser -u "$board_user" -- env HOME="$board_home" certutil -d "sql:$nss_dir" -N --empty-password </dev/null >/dev/null 2>&1 || true
            fi
            run_privileged runuser -u "$board_user" -- env HOME="$board_home" certutil -d "sql:$nss_dir" -D -n "Screentra Cast Signage Local CA" >/dev/null 2>&1 || true
            if run_privileged runuser -u "$board_user" -- env HOME="$board_home" certutil -d "sql:$nss_dir" -A -t "C," -n "Screentra Cast Signage Local CA" -i "$ca_file" >/dev/null 2>&1; then
                echo "Installed Screentra Cast CA into the board Chromium/Firefox NSS trust store."
                installed=1
            else
                echo "WARNING: board-user NSS CA install failed; Chromium may warn about the local HTTPS certificate."
            fi
        else
            echo "WARNING: board user/home unavailable; NSS CA install skipped."
        fi
    fi
    if [ "$installed" = "0" ]; then
        echo "Screentra CA saved to $ca_file but no system/NSS trust store was updated."
    fi
}

# Best-effort: auto-fix groups and seat so the kiosk browser can take
# over the display without the user having to know about seatd/logind.
ensure_user_groups() {
    # Required groups for the kiosk browser:
    #   tty   - open /dev/ttyN (PAM login on the seat service)
    #   video - access /dev/dri/card0 (DRM master)
    #   render- access /dev/dri/renderD128 (EGL/GLES2 hardware accel)
    #   input - read touch/tablet input devices
    local need_fix=0
    local missing=""
    for g in tty video render input; do
        if getent group "$g" >/dev/null 2>&1; then
            if ! id -nG "$USER" 2>/dev/null | tr ' ' '\n' | grep -qx "$g"; then
                missing="$missing $g"
                need_fix=1
            fi
        fi
    done
    # Optional seatd group; harmless if it doesn't exist.
    if getent group seat >/dev/null 2>&1; then
        if ! id -nG "$USER" 2>/dev/null | tr ' ' '\n' | grep -qx seat; then
            missing="$missing seat"
            need_fix=1
        fi
    fi
    if [ "$need_fix" = "1" ]; then
        echo
        echo "Your user is missing these groups required for the kiosk browser:$missing"
        if [ -t 0 ] && [ -t 1 ]; then
            read -r -p "Add your user to these groups now? [Y/n] " ans < /dev/tty || ans="y"
            case "$ans" in [nN]*) echo "Skipped. Re-run as root or with sudo usermod -aG$missing $USER when ready."; return 0;; esac
        fi
        # usermod -aG takes a single comma-separated group list. The original
        # implementation passed `$missing "$USER"` which usermod interpreted
        # as `usermod -aG <group1> <group2> ... <user>` and printed usage;
        # fix by collapsing missing into a single comma-joined argument.
        local missing_csv
        missing_csv="$(echo "$missing" | tr ' ' ',')"
        missing_csv="${missing_csv#,}"
        missing_csv="${missing_csv%,}"
        if run_privileged usermod -aG "$missing_csv" "$USER" 2>/dev/null; then
            echo "Added $USER to:$missing"
            echo "NOTE: groups update on next login. The kiosk service may still fail until you re-login."
            echo "If the kiosk still cannot open /dev/dri/renderD128, log out and back in (or reboot) so the new groups apply."
        else
            echo "WARNING: could not add groups automatically. Run as root: usermod -aG$missing $USER"
        fi
    fi
}

ensure_seat_daemon() {
    # Prefer logind (Debian default). Fall back to seatd.
    # Probe logind via loginctl rather than systemctl, because on Debian
    # systemd-logind is a D-Bus-activated daemon and 'systemctl is-active'
    # can report 'inactive' even when logind is healthy and managing sessions.
    local seat_ok=0
    if command -v loginctl >/dev/null 2>&1 && loginctl 2>/dev/null | grep -q . ; then seat_ok=1; fi
    if [ "$seat_ok" = "0" ] && pgrep -x seatd >/dev/null 2>&1; then seat_ok=1; fi
    if [ "$seat_ok" = "1" ]; then return 0; fi
    # No seat daemon — try to start one. Prefer logind if installed.
    if [ -x /lib/systemd/systemd-logind ] || [ -x /usr/lib/systemd/systemd-logind ] || systemctl list-unit-files systemd-logind.service >/dev/null 2>&1; then
        if run_privileged systemctl enable --now systemd-logind.service >/dev/null 2>&1; then
            sleep 1
            if command -v loginctl >/dev/null 2>&1 && loginctl 2>/dev/null | grep -q . ; then
                echo "Started systemd-logind for kiosk display takeover."
                return 0
            fi
        fi
    fi
    if command -v seatd >/dev/null 2>&1; then
        if run_privileged systemctl enable --now seatd.service >/dev/null 2>&1; then
            sleep 1
            if pgrep -x seatd >/dev/null 2>&1; then
                echo "Started seatd for kiosk display takeover."
                return 0
            fi
        fi
    fi
    # Try installing seatd if apt is available.
    if command -v apt-get >/dev/null 2>&1; then
        if [ -t 0 ] && [ -t 1 ]; then
            read -r -p "No seat daemon running. Install seatd now? [Y/n] " ans < /dev/tty || ans="y"
            case "$ans" in [nN]*) echo "Skipped. Install logind or seatd before starting the kiosk browser."; return 0;; esac
        fi
        if run_privileged apt-get install -y seatd >/dev/null 2>&1 && run_privileged systemctl enable --now seatd.service >/dev/null 2>&1; then
            sleep 1
            if pgrep -x seatd >/dev/null 2>&1; then
                echo "Installed and started seatd."
                return 0
            fi
        fi
    fi
    echo "WARNING: no seat daemon (systemd-logind or seatd) running. The kiosk browser may fail to take over the display."
}

detect_os
VERSION="$CLIENT_VERSION"
install_os_dependencies
ensure_dbus

ensure_user_groups
ensure_seat_daemon
create_board_user

if [ -n "$DISCOVERED_CA_B64" ]; then
    discovered_ca_tmp="$(mktemp "$INSTALL_DIR/.discovered-ca.XXXXXX")"
    if printf '%s' "$DISCOVERED_CA_B64" | base64 --decode > "$discovered_ca_tmp" 2>/dev/null \
        && grep -q 'BEGIN CERTIFICATE' "$discovered_ca_tmp"; then
        install -m 0644 "$discovered_ca_tmp" "$INSTALL_DIR/screentra-cast-signage-ca.crt"
        echo "Saved discovered Screentra Cast CA trust anchor."
    fi
    rm -f "$discovered_ca_tmp"
fi
install_screentra_ca_trust
write_os_marker
export RIPLEY_CLIENT_DIR="$INSTALL_DIR"
export RIPLEY_SERVER_URL="$SERVER_URL"
export RIPLEY_CA_CERT="$INSTALL_DIR/screentra-cast-signage-ca.crt"

# Display-governance runs LATER, after the heredoc that writes
# ripley-display-governance.py to disk. Done at the end of install.

cat > "$PREFLIGHT" <<'PY_PREFLIGHT'
#!/usr/bin/env python3
"""Screentra Cast client preflight checks.

Validates the local environment for running the player client:
- python3 OK
- systemd --user available
- display session (Wayland or X11)
- browser installed
- required group memberships
- writable runtime/cache dirs
- renderer port availability
"""
from __future__ import annotations


import os
import selectors
import shutil
import time
import ssl
import socket
import stat
import subprocess
import sys
import urllib.request
from urllib.parse import urlparse
from pathlib import Path


CHECKS: list[dict] = []


def _seat_service_running() -> bool:
    """Return True if the system-level labwc seat service is active for any display."""
    for idx in range(4):
        try:
            r = subprocess.run(
                ["systemctl", "--quiet", "is-active", f"screentra-cast-kiosk@{idx}.service"],
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
                timeout=10,
            )
            if r.returncode == 0:
                return True
        except Exception:
            pass
    return False


def has_display_session(retries: int = 6, retry_delay: float = 1.0) -> tuple[bool, str]:
    """Detect an active Wayland/X11 session.

    Retries for `retries` * `retry_delay` seconds total, because the
    system-level labwc seat service may take a few seconds to start and
    create /run/user/UID/wayland-0. Also accepts a healthy seat service
    even before the socket exists, so installers running in the same
    tick that the seat is starting up still report success.
    """
    for attempt in range(max(1, retries)):
        wayland = os.environ.get("WAYLAND_DISPLAY")
        if wayland:
            return True, f"WAYLAND_DISPLAY={wayland}"
        xdg = os.environ.get("XDG_RUNTIME_DIR", f"/run/user/{os.getuid()}")
        try:
            socks = sorted(p.name for p in Path(xdg).glob("wayland-*") if p.name.startswith("wayland-"))
        except Exception:
            socks = []
        if socks:
            return True, f"WAYLAND_DISPLAY={socks[0]} (auto-detected)"
        if os.environ.get("DISPLAY"):
            return True, f"DISPLAY={os.environ['DISPLAY']}"
        if shutil.which("Xvfb") or shutil.which("xvfb-run"):
            return True, "xvfb-run available (headless kiosk mode)"
        # If the system-level labwc seat service is healthy, give it more time.
        if _seat_service_running():
            if attempt < retries - 1:
                time.sleep(retry_delay)
                continue
            # Final attempt: report the seat is running so the user knows
            # the Wayland socket just hasn't appeared yet.
            return True, "system-level screentra-cast-kiosk@.service active (wayland-0 socket pending)"
        if attempt < retries - 1:
            time.sleep(retry_delay)
    return False, "no Wayland/X11 display session detected (system-level screentra-cast-kiosk@.service is not active; install or start it from a TTY)"

def _ensure_real_directory(path: Path, create: bool = False) -> bool:
    absolute = Path(os.path.abspath(os.fspath(path)))
    current = Path(absolute.anchor)
    for part in absolute.parts[1:]:
        current /= part
        try:
            metadata = os.lstat(current)
        except FileNotFoundError:
            if not create:
                return False
            current.mkdir()
            metadata = os.lstat(current)
        if stat.S_ISLNK(metadata.st_mode) or not stat.S_ISDIR(metadata.st_mode):
            raise ValueError('install directory is not a real directory')
    return True


def _read_real_file(path: Path, maximum: int = 256 * 1024) -> bytes:
    absolute = Path(os.path.abspath(os.fspath(path)))
    current = Path(absolute.anchor)
    for part in absolute.parts[1:]:
        current /= part
        metadata = os.lstat(current)
        if stat.S_ISLNK(metadata.st_mode):
            raise ValueError('CA path contains a symlink')
    descriptor = os.open(absolute, os.O_RDONLY | getattr(os, 'O_NOFOLLOW', 0))
    try:
        metadata = os.fstat(descriptor)
        if not stat.S_ISREG(metadata.st_mode) or metadata.st_size > maximum:
            raise ValueError('CA file is not bounded')
        data = os.read(descriptor, maximum + 1)
    finally:
        os.close(descriptor)
    if len(data) > maximum:
        raise ValueError('CA file exceeds its bound')
    return data


def tls_context():
    cafile = os.environ.get('SCREENTRA_CA_CERT') or os.environ.get('RIPLEY_CA_CERT') or str(Path.home() / '.local/share/screentra-client/screentra-cast-signage-ca.crt')
    if cafile and Path(cafile).exists():
        try:
            return ssl.create_default_context(
                cadata=_read_real_file(Path(cafile)).decode('utf-8')
            )
        except Exception as e:
            return None
    return None


def validate_server_url(server):
    parsed = urlparse((server or '').strip())
    if parsed.scheme not in ('https', 'http') or not parsed.hostname:
        raise ValueError('server URL must include an https:// origin')
    if parsed.scheme == 'http' and parsed.hostname not in ('127.0.0.1', 'localhost', '::1'):
        raise ValueError('non-loopback server URLs must use HTTPS')
    return parsed

def urlopen_ripley(req, timeout=10):
    kw = {'timeout': timeout}
    parsed = validate_server_url(req if isinstance(req, str) else req.full_url)
    if parsed.scheme == 'https':
        ctx = tls_context()
        if ctx is None:
            raise RuntimeError('Screentra Cast CA certificate is unavailable or invalid')
        kw['context'] = ctx
    return urllib.request.urlopen(req, **kw)


def add(name: str, ok: bool, detail: str = "", required: bool = True) -> None:
    CHECKS.append({"name": name, "ok": ok, "detail": detail, "required": required})


MAX_LOCAL_PROBE_OUTPUT_BYTES = 4096
MAX_ERROR_BYTES = 512


def _bound_local_probe_output(value: str) -> str:
    encoded = str(value).encode('utf-8', errors='replace')
    return encoded[:MAX_LOCAL_PROBE_OUTPUT_BYTES].decode('utf-8', errors='ignore')


def _run_bounded_probe(args, maximum=MAX_LOCAL_PROBE_OUTPUT_BYTES, timeout=3):
    process = None
    try:
        process = subprocess.Popen(args, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
        selector = selectors.DefaultSelector()
        output = bytearray()
        selector.register(process.stdout, selectors.EVENT_READ)
        deadline = time.monotonic() + timeout
        try:
            while selector.get_map():
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise TimeoutError('probe timed out')
                for key, _ in selector.select(remaining):
                    chunk = os.read(key.fileobj.fileno(), 8192)
                    if not chunk:
                        selector.unregister(key.fileobj)
                        key.fileobj.close()
                        continue
                    if len(output) + len(chunk) > maximum:
                        raise ValueError('probe output exceeded its limit')
                    output.extend(chunk)
        finally:
            selector.close()
        return process.wait(timeout=max(0.1, deadline - time.monotonic())), bytes(output)
    except Exception:
        if process is not None and process.poll() is None:
            process.kill()
            process.wait(timeout=1)
        return None, b''
    finally:
        if process is not None and process.stdout is not None:
            process.stdout.close()


def _bound_error(value: str) -> str:
    encoded = str(value).encode('utf-8', errors='replace')
    return encoded[:MAX_ERROR_BYTES].decode('utf-8', errors='ignore')


def run(cmd: str) -> str:
    commands = {
        "systemctl --user status >/dev/null 2>&1 && echo ok":
            ['systemctl', '--user', 'status'],
        'id -Gn': ['id', '-Gn'],
    }
    args = commands.get(cmd)
    if args is None:
        return ''
    try:
        returncode, raw = _run_bounded_probe(args)
        if returncode != 0:
            return ''
        output = _bound_local_probe_output(raw.decode('utf-8', errors='replace'))
        return output.strip()
    except Exception:
        return ''


def find_browser() -> str | None:
    for b in ("chromium", "chromium-browser", "google-chrome-stable", "google-chrome", "firefox-esr", "firefox"):
        if shutil.which(b):
            return b
    return None


def port_in_use(port: int) -> bool:
    try:
        with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
            s.bind(("127.0.0.1", port))
            return False
    except OSError:
        return True


def main() -> int:
    add("python3", shutil.which("python3") is not None, sys.version.split()[0])
    add("curl/wget", shutil.which("curl") is not None or shutil.which("wget") is not None,
        "curl" if shutil.which("curl") else "wget" if shutil.which("wget") else "missing", required=False)
    add("systemctl --user", shutil.which("systemctl") is not None and bool(run("systemctl --user status >/dev/null 2>&1 && echo ok")),
        "systemd user manager running", required=False)

    ok, detail = has_display_session()
    add("display session", ok, detail)

    add("browser installed", find_browser() is not None, find_browser() or "no chromium/chrome/firefox", required=False)

    # Wayland kiosk compositor + output helper. Required for display takeover.
    add("sway (wayland compositor)", shutil.which("sway") is not None,
        shutil.which("sway") or "install sway for Wayland kiosk mode", required=True)
    add("wlr-randr (wayland output helper)", shutil.which("wlr-randr") is not None,
        shutil.which("wlr-randr") or "install wlr-randr (wlroots tools, ships with sway) for Wayland output discovery", required=False)

    # libinput / touch detection support
    add("/proc/bus/input/devices", os.path.exists("/proc/bus/input/devices"),
        "present" if os.path.exists("/proc/bus/input/devices") else "missing — touch device auto-detect unavailable", required=False)

    # Group memberships. Required for the kiosk to run.
    groups = run("id -Gn").split()
    needed = {"video", "render", "audio", "tty", "input"}
    missing = sorted(needed - set(groups))
    if missing:
        add("user groups", False,
            f"present={sorted(needed & set(groups))} missing={missing}",
            required=False)
        add("user groups fix", False,
            f"sudo usermod -aG {','.join(missing)} $USER  (then log out and back in)",
            required=False)
    else:
        add("user groups", True, f"present={sorted(needed & set(groups))}", required=False)

    # Live device access. /dev/dri/renderD128 is needed for EGL/GLES2. We do
    # NOT read from the device — render nodes use streaming IO that blocks
    # forever if no write side is active. `os.open` with O_NONBLOCK tells us
    # the kernel would let us in (permissions/group ok); that's all the
    # coverage we need for "is the device accessible to the kiosk browser".
    if Path("/dev/dri/renderD128").exists():
        try:
            import os as _os
            _fd = _os.open("/dev/dri/renderD128",
                           _os.O_RDWR | _os.O_NONBLOCK | _os.O_CLOEXEC)
            _os.close(_fd)
            add("/dev/dri/renderD128 readable", True, "openable", required=False)
        except PermissionError:
            add("/dev/dri/renderD128 readable", False, "Permission denied; user not in 'render' group", required=True)
        except Exception as e:
            add("/dev/dri/renderD128 readable", False, f"open failed: {e!r}", required=False)
    else:
        add("/dev/dri/renderD128 readable", True, "device not present (headless ok)", required=False)

    # Writable dirs
    install_dir = Path(os.environ.get("RIPLEY_CLIENT_DIR") or Path.home() / ".local/share/screentra-client")
    bin_dir = Path.home() / ".local/bin"
    systemd_user = Path.home() / ".config/systemd/user"
    try:
        _ensure_real_directory(install_dir, create=True)
        install_writable = os.access(install_dir, os.W_OK)
    except (OSError, ValueError):
        install_writable = False
    add("install dir", install_writable, str(install_dir))
    add("bin dir", os.access(bin_dir, os.W_OK) or os.access(bin_dir.parent, os.W_OK), str(bin_dir))
    add("systemd user dir", True, str(systemd_user))

    # Renderer port
    add("renderer port 3900", not port_in_use(3900), "free" if not port_in_use(3900) else "busy", required=False)

    # Server URL reachability
    server = os.environ.get("RIPLEY_SERVER_URL", "")
    if server:
        try:
            with urlopen_ripley(server.rstrip("/") + "/api/clients", timeout=5) as r:
                ok = r.status == 200
                detail = f"status {r.status}"
        except Exception as e:
            ok, detail = False, _bound_error(str(e))
        add("server reachable", ok, f"{server} ({detail})")
    else:
        add("server url configured", False, "RIPLEY_SERVER_URL not set (will be sourced from install args or server file)", required=False)

    # Report
    blocking = [c for c in CHECKS if c["required"] and not c["ok"]]
    soft = [c for c in CHECKS if not c["required"] and not c["ok"]]

    print("Screentra Cast Player Preflight")
    print("-" * 60)
    for c in CHECKS:
        status = "PASS" if c["ok"] else ("FAIL" if c["required"] else "WARN")
        print(f"  [{status}] {c['name']:<24} {c['detail']}")

    if blocking:
        print()
        print("Blocking issues:")
        for c in blocking:
            print(f"  - {c['name']}: {c['detail']}")
        return 1
    if soft:
        print()
        print("Soft warnings (services can run, but some features may not work):")
        for c in soft:
            print(f"  - {c['name']}: {c['detail']}")
    print()
    print("Ready to install/enable services.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

PY_PREFLIGHT
chmod +x "$PREFLIGHT"
ln -sf "$PREFLIGHT" "$BIN_DIR/screentra-client-preflight"

mkdir -p "$INSTALL_DIR"
echo "$SERVER_URL" > "$INSTALL_DIR/server"

if [ ! -f "$CLIENT_ID_FILE" ]; then
  if [ -r /etc/machine-id ]; then
    machine_id="$(read_bounded_file /etc/machine-id 256 2>/dev/null || true)"
    if [ -n "$machine_id" ]; then
      printf 'ripley-%s\n' "$machine_id" > "$CLIENT_ID_FILE"
    else
      printf 'ripley-%s-%s\n' "$(hostname)" "$(date +%s)" > "$CLIENT_ID_FILE"
    fi
  else
    printf 'ripley-%s-%s\n' "$(hostname)" "$(date +%s)" > "$CLIENT_ID_FILE"
  fi
fi
CLIENT_ID="$(read_bounded_file "$CLIENT_ID_FILE" 256 2>/dev/null || true)"
if [[ ! "$CLIENT_ID" =~ ^[A-Za-z0-9._-]{1,256}$ ]]; then
  echo "ERROR: persisted client ID is missing or invalid." >&2
  exit 1
fi
CLIENT_AUTH_TOKEN_FILE="$INSTALL_DIR/client-auth-token"
if [ ! -s "$CLIENT_AUTH_TOKEN_FILE" ]; then
  umask 077
  python3 - <<'PY_TOKEN' > "$CLIENT_AUTH_TOKEN_FILE"
import secrets
print(secrets.token_urlsafe(48))
PY_TOKEN
fi
chmod 0600 "$CLIENT_AUTH_TOKEN_FILE"
install_client_update_public_key
install_root_verified_updater

cat > "$HEARTBEAT" <<'PY'
#!/usr/bin/env python3
"""Screentra Cast client/client heartbeat agent.

Posts system metrics to the Screentra Cast signage server so the Connected Clients panel
can show enrolled player uptime and usage.
"""
import argparse
import ctypes
import hashlib
import io
import json
import os
import posixpath
import re
import shutil
import selectors
import ssl
import socket
import stat
import subprocess
import sys
import tarfile
import tempfile
import time
import urllib.request
from urllib.parse import quote, urlparse
import importlib.util
import sys as _sys
_display_detect_path = os.path.join(os.path.dirname(os.path.abspath(__file__)), 'screentra-display-detect.py')
_spec = importlib.util.spec_from_file_location('screentra_display_detect', _display_detect_path)
_mod = importlib.util.module_from_spec(_spec)
_sys.modules['screentra_display_detect'] = _mod
_spec.loader.exec_module(_mod)
detect_displays = _mod.detect_displays
_status_report_path = os.path.join(os.path.dirname(os.path.abspath(__file__)), 'screentra-client-status-report-admission.py')
_status_report_spec = importlib.util.spec_from_file_location('screentra_client_status_report_admission', _status_report_path)
_status_report_mod = importlib.util.module_from_spec(_status_report_spec)
_sys.modules['screentra_client_status_report_admission'] = _status_report_mod
_status_report_spec.loader.exec_module(_status_report_mod)
validate_status_report = _status_report_mod.validate_status_report

from pathlib import Path

MAX_JSON_FILE_BYTES = 1 << 20
MAX_COMMAND_RESPONSE_BYTES = 64 * 1024
MAX_TEXT_RESPONSE_BYTES = 64 * 1024
MAX_PROC_METRICS_BYTES = 64 * 1024
# Keep the last-known-good layout indefinitely when the server reports no
# assignment.  This is intentionally temporary: a transient server/config
# race must never replace working signage with a no-layout screen.  A later
# assignment still replaces the cached layout normally; restore an explicit
# unassignment policy here when operators want that behavior again.
PRESERVE_CACHED_LAYOUT_ON_UNASSIGNED = True
# Retained for compatibility with diagnostics/tests that report the old
# confirmation interval; the temporary preserve policy above takes precedence.
LAYOUT_UNASSIGNED_GRACE_SECONDS = 120


def _read_bounded_local_text(path: str, maximum: int = MAX_PROC_METRICS_BYTES) -> str:
    with open(path, 'rb') as handle:
        return handle.read(maximum).decode('utf-8', errors='replace')


def _read_bounded_response(response, maximum):
    if isinstance(maximum, bool) or not isinstance(maximum, int) or maximum < 1:
        raise ValueError('response bound is invalid')
    data = response.read(maximum + 1)
    if not isinstance(data, (bytes, bytearray)) or len(data) > maximum:
        raise ValueError('server response exceeds its bound')
    return bytes(data).decode('utf-8')


def client_pairing_code(client_id):
    """Return the short physical-confirmation code for a client ID."""
    digest = hashlib.sha256(b'screentra-cast-pairing-v1\0' + str(client_id).encode('utf-8')).digest()
    return f'{int.from_bytes(digest[:4], "big") % 1_000_000:06d}'


def _read_json_file(path: Path, default):
    try:
        raw = _read_real_file(path)
        if len(raw) > MAX_JSON_FILE_BYTES:
            return default
        return _parse_server_json(raw.decode('utf-8'))
    except Exception:
        return default


def _atomic_write_text(path: Path, text: str, mode: int = 0o600) -> None:
    """Replace a cache JSON file atomically and keep it owner-readable."""
    _ensure_real_directory(path.parent, create=True)
    os.chmod(path.parent, 0o700)
    fd, temporary_name = tempfile.mkstemp(
        prefix=f'.{path.name}.', suffix='.tmp', dir=str(path.parent)
    )
    temporary = Path(temporary_name)
    try:
        with os.fdopen(fd, 'w', encoding='utf-8') as handle:
            handle.write(text)
        os.chmod(temporary, mode)
        os.replace(temporary, path)
        temporary = None
    finally:
        if temporary is not None:
            try:
                temporary.unlink()
            except FileNotFoundError:
                pass


def _atomic_write_text_if_changed(path: Path, text: str, mode: int = 0o600) -> bool:
    """Atomically replace a cache file only when its contents changed."""
    try:
        current = _read_real_file(
            path,
            max_bytes=max(MAX_JSON_FILE_BYTES, len(text.encode('utf-8'))),
        )
        if current == text.encode('utf-8'):
            return False
    except (FileNotFoundError, OSError, ValueError):
        pass
    _atomic_write_text(path, text, mode=mode)
    return True


def _write_status_report(path: Path, status):
    """Persist only a report admitted by the metadata-only status boundary."""
    validate_status_report(status)
    comparable = dict(status)
    comparable.pop('at', None)
    try:
        previous = _read_cache_json(path, {})
        if isinstance(previous, dict):
            previous_comparable = dict(previous)
            previous_comparable.pop('at', None)
            if previous_comparable == comparable:
                return False
    except (OSError, UnicodeError, ValueError, json.JSONDecodeError):
        pass
    _atomic_write_text(path, json.dumps(status, indent=2))
    return True


def _display_governance_count(which):
    """Read config.json and return the display-count governance value.

    `which` is 'installer' (always-defined, what the installer detected) or
    'override' (operator-set, may be None). Missing/corrupt config.json is
    treated as "0" / None — the heartbeat will publish zero and the server
    will leave the existing row's value alone (operator_set is preserved by
    the server-side UPSERT).
    """
    try:
        cfg_path = Path(os.environ.get('RIPLEY_CLIENT_DIR',
                   str(Path.home() / '.local/share/screentra-client'))) / 'config.json'
        if not cfg_path.exists():
            return None if which == 'override' else 0
        cfg = _read_json_file(cfg_path, {})
    except Exception:
        return None if which == 'override' else 0
    if which == 'installer':
        v = cfg.get('installer_detected_max_displays')
        return int(v) if v is not None else 0
    # override
    v = cfg.get('max_displays')
    return int(v) if v is not None else None


def _read_theme():
    """Read theme from localStorage-style client config JSON (set by admin UI)."""
    try:
        cfg_path = Path(os.environ.get('RIPLEY_CLIENT_CACHE_DIR',
                   str(Path.home() / '.local/share/screentra-client/cache'))) / 'client-config.json'
        if not cfg_path.exists():
            return None
        cfg = _read_json_file(cfg_path, {})
        theme = cfg.get('theme')
        return theme if theme in ('midnight', 'light', 'classic') else None
    except Exception:
        return None


def _touch_binding():
    try:
        cfg_path = Path(os.environ.get('RIPLEY_CLIENT_DIR',
                   str(Path.home() / '.local/share/screentra-client'))) / 'config.json'
        if not cfg_path.exists():
            return None
        cfg = _read_json_file(cfg_path, {})
        binding = cfg.get('touch_binding')
        return binding if isinstance(binding, dict) else None
    except Exception:
        return None


MAX_LOCAL_PROBE_OUTPUT_BYTES = 4096


def _bound_local_probe_output(value: str) -> str:
    encoded = str(value).encode('utf-8', errors='replace')
    return encoded[:MAX_LOCAL_PROBE_OUTPUT_BYTES].decode('utf-8', errors='ignore')


def _run_bounded_probe(args, maximum=MAX_LOCAL_PROBE_OUTPUT_BYTES, timeout=3):
    process = None
    try:
        process = subprocess.Popen(args, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
        selector = selectors.DefaultSelector()
        output = bytearray()
        selector.register(process.stdout, selectors.EVENT_READ)
        deadline = time.monotonic() + timeout
        try:
            while selector.get_map():
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise TimeoutError('probe timed out')
                for key, _ in selector.select(remaining):
                    chunk = os.read(key.fileobj.fileno(), 8192)
                    if not chunk:
                        selector.unregister(key.fileobj)
                        key.fileobj.close()
                        continue
                    if len(output) + len(chunk) > maximum:
                        raise ValueError('probe output exceeded its limit')
                    output.extend(chunk)
        finally:
            selector.close()
        return process.wait(timeout=max(0.1, deadline - time.monotonic())), bytes(output)
    except Exception:
        if process is not None and process.poll() is None:
            process.kill()
            process.wait(timeout=1)
        return None, b''
    finally:
        if process is not None and process.stdout is not None:
            process.stdout.close()


def run(cmd):
    commands = {
        "hostname -I | awk '{print $1}'": ['hostname', '-I'],
        'cat /etc/machine-id': ['cat', '/etc/machine-id'],
    }
    args = commands.get(cmd)
    if args is None:
        return ''
    try:
        returncode, raw = _run_bounded_probe(args)
        if returncode != 0:
            return ''
        output = _bound_local_probe_output(raw.decode('utf-8', errors='replace'))
        return output.strip().split()[0] if output.strip() else ''
    except Exception:
        return ''


def read_mem():
    values = {}
    try:
        for line in _read_bounded_local_text('/proc/meminfo').splitlines():
            k, v = line.split(':', 1)
            values[k] = int(v.strip().split()[0]) * 1024
    except Exception:
        pass
    total = values.get('MemTotal')
    available = values.get('MemAvailable')
    used = total - available if total and available else None
    return used, total


def read_disk(path='/'):
    try:
        st = os.statvfs(path)
        total = st.f_blocks * st.f_frsize
        avail = st.f_bavail * st.f_frsize
        return total - avail, total
    except Exception:
        return None, None


def read_uptime():
    try:
        return int(float(_read_bounded_local_text('/proc/uptime', 256).split()[0]))
    except Exception:
        return None


def read_cpu_usage(interval=0.25):
    def sample():
        fields = [int(x) for x in _read_bounded_local_text('/proc/stat').splitlines()[0].split()[1:]]
        idle = fields[3] + fields[4]
        total = sum(fields)
        return idle, total
    try:
        idle1, total1 = sample()
        time.sleep(interval)
        idle2, total2 = sample()
        total_delta = total2 - total1
        idle_delta = idle2 - idle1
        if total_delta <= 0:
            return None
        return round((1 - idle_delta / total_delta) * 100, 2)
    except Exception:
        return None


def read_cpu_temp():
    candidates = []
    try:
        for path in Path('/sys/class/thermal').glob('thermal_zone*/temp'):
            type_path = path.parent / 'type'
            typ = _read_bounded_local_text(str(type_path), 256).strip().lower() if type_path.exists() else ''
            score = 0 if any(key in typ for key in ('cpu', 'x86_pkg_temp', 'k10temp', 'tctl', 'soc')) else 1
            candidates.append((score, path))
    except Exception:
        pass
    try:
        for path in Path('/sys/class/hwmon').glob('hwmon*/temp*_input'):
            label_path = path.with_name(path.name.replace('_input', '_label'))
            label = _read_bounded_local_text(str(label_path), 256).strip().lower() if label_path.exists() else ''
            score = 0 if any(key in label for key in ('cpu', 'tctl', 'tdie', 'package', 'soc')) else 2
            candidates.append((score, path))
    except Exception:
        pass
    for _, path in sorted(candidates, key=lambda item: item[0]):
        try:
            raw = int(_read_bounded_local_text(str(path), 128).strip())
            celsius = raw / 1000.0 if raw > 1000 else float(raw)
            if -20.0 < celsius < 130.0:
                return round(celsius, 1)
        except Exception:
            continue
    return None


def local_ip():
    ip = run("hostname -I | awk '{print $1}'")
    if ip:
        return ip
    try:
        s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        s.connect(('8.8.8.8', 80))
        ip = s.getsockname()[0]
        s.close()
        return ip
    except Exception:
        return '127.0.0.1'


def payload(args):
    mem_used, mem_total = read_mem()
    disk_used, disk_total = read_disk('/')
    hostname = socket.gethostname()
    client_id = args.client_id or run('cat /etc/machine-id') or hostname
    result, result_path = read_command_result(args.cache_dir)
    last_command_result = None
    last_command_at = None
    if result:
        last_command_result = result
        last_command_at = result.get('finished_at') or result.get('started_at')
    displays = detect_displays(max_displays=getattr(args, 'max_displays', 4))
    active_layout_id = args.active_layout_id
    active_layout_revision = None
    try:
        cached_cfg = Path(args.cache_dir, 'client-config.json')
        if cached_cfg.exists():
            cached = _read_json_file(cached_cfg, {})
            active_layout_id = active_layout_id or cached.get('assigned_layout_id')
            value = cached.get('active_layout_revision')
            active_layout_revision = int(value) if value is not None else None
    except Exception:
        pass
    for display in displays:
        try:
            display_index = int(display.get('display_index', 0))
            display_cfg_path = Path(args.cache_dir, 'client-config.json' if display_index == 0 else f'display-config-{display_index}.json')
            display_cfg = _read_json_file(display_cfg_path, {})
            display['active_layout_id'] = display_cfg.get('assigned_layout_id') or active_layout_id
            value = display_cfg.get('active_layout_revision')
            display['active_layout_revision'] = int(value) if value is not None else active_layout_revision
        except Exception:
            display['active_layout_id'] = active_layout_id
            display['active_layout_revision'] = active_layout_revision
    return {
        'client_agent': True,
        'client_installed': True,
        'identity_source': 'persistent_client_id',
        'client_id': client_id,
        'pairing_code': client_pairing_code(client_id),
        'hostname': args.name or hostname,
        'address': local_ip(),
        'role': args.role,
        'version': args.version,
        'uptime_seconds': read_uptime(),
        'cpu_usage': read_cpu_usage(),
        'cpu_temp': read_cpu_temp(),
        'memory_used': mem_used,
        'memory_total': mem_total,
        'disk_used': disk_used,
        'disk_total': disk_total,
        'active_layout_id': active_layout_id,
        'active_layout_revision': active_layout_revision,
        'last_command_result': last_command_result,
        'last_command_at': last_command_at,
        'displays': displays,
        # Display count governance (Phase 1A, 2026-08-02). Read from
        # config.json which the installer writes after DRM detection, and the
        # admin /admin/display page can update. Heartbeat is the carrier;
        # the server stores both fields verbatim.
        'installer_detected_max_displays': _display_governance_count('installer'),
        'max_displays': _display_governance_count('override'),
        'touch_binding': _touch_binding(),
        'theme': _read_theme(),
    }




def tls_context():
    """Use the Screentra Cast CA bundle directly when present, independent of OS trust."""
    cafile = os.environ.get('RIPLEY_CA_CERT') or str(Path.home() / '.local/share/screentra-client/screentra-cast-signage-ca.crt')
    if not cafile or not Path(cafile).is_file():
        raise RuntimeError('Screentra Cast CA certificate is unavailable')
    try:
        return ssl.create_default_context(
            cadata=_read_real_file(Path(cafile), max_bytes=256 * 1024).decode('utf-8')
        )
    except Exception as e:
        raise RuntimeError('Screentra Cast CA certificate is invalid') from e


def validate_server_url(server):
    parsed = urlparse((server or '').strip())
    if parsed.scheme not in ('https', 'http') or not parsed.hostname:
        raise ValueError('server URL must include an https:// origin')
    if parsed.scheme == 'http' and parsed.hostname not in ('127.0.0.1', 'localhost', '::1'):
        raise ValueError('non-loopback server URLs must use HTTPS')
    return parsed

def urlopen_ripley(req, timeout=10):
    kw = {'timeout': timeout}
    parsed = validate_server_url(req.full_url)
    if parsed.scheme == 'https':
        ctx = tls_context()
        kw['context'] = ctx
    return urllib.request.urlopen(req, **kw)


def client_auth_token():
    """Read the per-client token without ever printing or logging it."""
    token = os.environ.get('RIPLEY_CLIENT_AUTH_TOKEN', '').strip()
    if token:
        return token
    path = os.environ.get(
        'RIPLEY_CLIENT_AUTH_TOKEN_FILE',
        str(Path.home() / '.local/share/screentra-client/client-auth-token'),
    )
    try:
        return _read_real_file(Path(path), max_bytes=4096).decode('utf-8').strip()
    except (OSError, UnicodeError, ValueError):
        return ''


def client_auth_headers(token=None):
    token = client_auth_token() if token is None else str(token).strip()
    return {'X-SCREENTRA-CLIENT-TOKEN': token} if token else {}


ADDON_API_VERSION = '1'
MAX_ADDON_PACKAGE_BYTES = 8 * 1024 * 1024
MAX_ADDON_EXTRACTED_BYTES = 8 * 1024 * 1024
MAX_ADDON_MEMBER_BYTES = 4 * 1024 * 1024
MAX_ADDON_MANIFEST_BYTES = 256 * 1024
MAX_ADDON_SIGNATURE_BYTES = 16 * 1024
ADDON_ID_RE = re.compile(r'^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$')
ADDON_VERSION_RE = re.compile(r'^[a-z0-9][a-z0-9._-]{0,63}$')
CLIENT_CAPABILITIES = frozenset({
    'render_only',
    'camera_capture',
    'microphone_capture',
    'local_inference',
    'client_service',
})
CLIENT_SERVICE_FIELDS = frozenset({
    'id',
    'image',
    'image_digest',
    'socket_name',
    'cpu_millis',
    'memory_bytes',
    'pids',
    'writable_bytes',
    'model_id',
    'model_sha256',
})
GENERIC_ADDON_ERROR = 'signed addon cache update failed'
GENERIC_INVENTORY_ERROR = 'addon inventory unavailable'


def _absolute_path(path):
    return Path(os.path.abspath(os.fspath(path)))


def _ensure_real_directory(path, create=False):
    """Validate or create a directory without following symlinked ancestors."""
    current = Path(_absolute_path(path).anchor)
    parts = _absolute_path(path).parts[1:]
    for part in parts:
        current /= part
        try:
            metadata = os.lstat(current)
        except FileNotFoundError:
            if not create:
                return False
            current.mkdir()
            metadata = os.lstat(current)
        if stat.S_ISLNK(metadata.st_mode) or not stat.S_ISDIR(metadata.st_mode):
            raise ValueError('addon cache directory is not a real directory')
    return True


def _path_exists(path):
    try:
        os.lstat(path)
        return True
    except FileNotFoundError:
        return False


def _reject_symlinked_ancestors(path):
    current = Path(_absolute_path(path).anchor)
    for part in _absolute_path(path).parts[1:]:
        current /= part
        try:
            metadata = os.lstat(current)
        except FileNotFoundError:
            break
        if stat.S_ISLNK(metadata.st_mode):
            raise ValueError('addon cache path contains a symlink')


def _require_real_file(path):
    """Reject symlinks and non-regular files before cache reads."""
    _reject_symlinked_ancestors(path)
    metadata = os.lstat(path)
    if stat.S_ISLNK(metadata.st_mode) or not stat.S_ISREG(metadata.st_mode):
        raise ValueError('addon cache file is not a regular file')
    return metadata


def _read_real_file(path, max_bytes=MAX_JSON_FILE_BYTES):
    _require_real_file(path)
    flags = os.O_RDONLY | getattr(os, 'O_NOFOLLOW', 0)
    descriptor = os.open(os.fspath(path), flags)
    try:
        metadata = os.fstat(descriptor)
        if not stat.S_ISREG(metadata.st_mode) or metadata.st_size > max_bytes:
            raise ValueError('addon cache file exceeds its bound')
        return os.read(descriptor, max_bytes + 1)
    finally:
        os.close(descriptor)


def _read_cache_json(path, default):
    if not _path_exists(path):
        return default
    try:
        return _parse_server_json(_read_real_file(path).decode('utf-8'))
    except (OSError, UnicodeError, ValueError, json.JSONDecodeError):
        raise ValueError('addon cache metadata is invalid')


def _validate_addon_version(version):
    if not isinstance(version, str) or not ADDON_VERSION_RE.fullmatch(version):
        raise ValueError('addon inventory contains an unsafe version')
    return version


def _validate_client_service_token(value, field):
    if not isinstance(value, str) or not 0 < len(value) <= 64 or not re.fullmatch(r'[a-z0-9._-]+', value):
        raise ValueError(f'invalid client service {field}')


def _validate_client_service_image(value):
    if (
        not isinstance(value, str)
        or not value
        or len(value) > 255
        or '..' in value
        or '://' in value
        or ':' in value
        or '@' in value
        or '//' in value
        or value.startswith('/')
        or value.endswith('/')
        or any(not (character.isascii() and (character.isalnum() or character in '._-/')) for character in value)
        or any(
            not component
            or component in ('.', '..')
            or not component[0].isascii()
            or not component[0].isalnum()
            or not component[-1].isascii()
            or not component[-1].isalnum()
            for component in value.split('/')
        )
    ):
        raise ValueError('invalid client service image reference')


def _validate_client_service_model_identity(service):
    model_id = service.get('model_id')
    model_sha256 = service.get('model_sha256')
    if (model_id is None) != (model_sha256 is None):
        raise ValueError('client service model_id and model_sha256 must be declared together')
    if model_id is not None:
        _validate_client_service_token(model_id, 'model id')
        if not isinstance(model_sha256, str) or not re.fullmatch(r'[0-9a-f]{64}', model_sha256):
            raise ValueError('client service model_sha256 must be a lowercase hexadecimal digest')


def _validate_client_extension_capabilities(manifest):
    capabilities = manifest.get('client_capabilities', [])
    if not isinstance(capabilities, list):
        raise ValueError('client_capabilities must be a list')
    seen = set()
    for capability in capabilities:
        if not isinstance(capability, str) or capability not in CLIENT_CAPABILITIES:
            raise ValueError('unsupported client capability')
        if capability in seen:
            raise ValueError('duplicate client capability')
        seen.add(capability)
    if 'local_inference' in seen and 'microphone_capture' not in seen:
        raise ValueError('local_inference requires microphone_capture')
    if 'local_inference' in seen and 'client_service' not in seen:
        raise ValueError('local_inference requires client_service')
    requests_service = 'client_service' in seen
    service = manifest.get('client_service')
    if requests_service != (service is not None):
        raise ValueError('client_service capability and metadata must be declared together')
    if service is None:
        return
    if not isinstance(service, dict) or set(service) - CLIENT_SERVICE_FIELDS:
        raise ValueError('client service metadata contains unsupported fields')
    required = {
        'id', 'image', 'image_digest', 'socket_name',
        'cpu_millis', 'memory_bytes', 'pids', 'writable_bytes',
    }
    if not required.issubset(service):
        raise ValueError('client service metadata is incomplete')
    _validate_client_service_token(service['id'], 'id')
    _validate_client_service_token(service['socket_name'], 'socket name')
    _validate_client_service_image(service['image'])
    if not isinstance(service['image_digest'], str) or not re.fullmatch(
            r'sha256:[0-9a-f]{64}', service['image_digest']):
        raise ValueError('invalid client service image digest')
    for field, minimum, maximum in (
        ('cpu_millis', 1, 4000),
        ('memory_bytes', 64 * 1024 * 1024, 2 * 1024 * 1024 * 1024),
        ('pids', 1, 256),
    ):
        value = service[field]
        if isinstance(value, bool) or not isinstance(value, int) or not minimum <= value <= maximum:
            raise ValueError(f'client service {field} is outside the bounded range')
    writable_bytes = service['writable_bytes']
    if isinstance(writable_bytes, bool) or not isinstance(writable_bytes, int) or not 0 <= writable_bytes <= 512 * 1024 * 1024:
        raise ValueError('client service writable_bytes is outside the bounded range')
    if 'local_inference' in seen and service.get('model_id') is None:
        raise ValueError('local_inference requires an explicit model identity and digest')
    _validate_client_service_model_identity(service)


def _validate_addon_manifest_metadata(manifest, expected):
    """Validate signed addon metadata against the server inventory contract."""
    if not isinstance(manifest, dict) or manifest.get('schema_version') != 1:
        raise ValueError('unsupported addon manifest schema')
    if manifest.get('kind') != 'client_extension' or manifest.get('client_api_version') != ADDON_API_VERSION:
        raise ValueError('unsupported client addon API version or kind')
    addon_id = manifest.get('id')
    if not isinstance(addon_id, str) or not ADDON_ID_RE.fullmatch(addon_id):
        raise ValueError('invalid addon id')
    if addon_id != expected.get('id') or manifest.get('version') != expected.get('version'):
        raise ValueError('addon package does not match the server inventory')
    if manifest.get('server_entrypoint') is not None:
        raise ValueError('client addon contains a server entrypoint')
    entrypoint = manifest.get('client_entrypoint')
    digest = manifest.get('client_sha256')
    if not isinstance(entrypoint, str) or not entrypoint.startswith('client/'):
        raise ValueError('invalid addon entrypoint')
    if not isinstance(digest, str) or not re.fullmatch(r'[0-9a-fA-F]{64}', digest):
        raise ValueError('invalid addon entrypoint digest')
    if digest.lower() != str(expected.get('client_sha256', '')).lower():
        raise ValueError('addon package digest does not match inventory')
    _validate_client_extension_capabilities(manifest)
    return entrypoint, digest


def _verify_addon_manifest_signature(manifest_path, signature_path):
    """Verify detached manifest bytes with the configured addon public key."""
    _read_real_file(signature_path, MAX_ADDON_SIGNATURE_BYTES)
    public_key = addon_public_key_path()
    try:
        public_key_bytes = _read_real_file(public_key, 64 * 1024)
    except (OSError, ValueError):
        raise ValueError('addon signing public key is unavailable')
    temporary_key = None
    try:
        descriptor, temporary_name = tempfile.mkstemp(prefix='.addon-key-', suffix='.pub')
        temporary_key = Path(temporary_name)
        with os.fdopen(descriptor, 'wb') as handle:
            handle.write(public_key_bytes)
        os.chmod(temporary_key, 0o600)
        result = subprocess.run(
            ['openssl', 'pkeyutl', '-verify', '-rawin', '-pubin', '-inkey', str(temporary_key),
             '-sigfile', str(signature_path), '-in', str(manifest_path)],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            timeout=10,
            check=False,
        )
    finally:
        if temporary_key is not None:
            try:
                temporary_key.unlink()
            except FileNotFoundError:
                pass
    if result.returncode != 0:
        raise ValueError('addon manifest signature verification failed')


def _verify_cached_addon(target, active, expected):
    """Revalidate persisted addon files before accepting verified_cached."""
    if set(active) != {'id', 'version', 'client_sha256', 'entrypoint'}:
        raise ValueError('addon cache metadata is invalid')
    manifest_path = target / 'manifest.json'
    signature_path = target / 'manifest.sig'
    manifest_bytes = _read_real_file(manifest_path, MAX_ADDON_MANIFEST_BYTES)
    try:
        manifest = _parse_addon_manifest(manifest_bytes)
    except (UnicodeError, ValueError, json.JSONDecodeError):
        raise ValueError('addon cache manifest is invalid')
    entrypoint, digest = _validate_addon_manifest_metadata(manifest, expected)
    if (active['id'] != expected['id'] or active['version'] != expected['version'] or
            str(active['client_sha256']).lower() != expected['client_sha256'].lower() or
            active['entrypoint'] != entrypoint):
        raise ValueError('addon cache metadata does not match the signed manifest')
    client_path = target / entrypoint
    client_bytes = _read_real_file(client_path, MAX_ADDON_MEMBER_BYTES)
    if hashlib.sha256(client_bytes).hexdigest().lower() != digest.lower():
        raise ValueError('addon entrypoint SHA-256 verification failed')
    _verify_addon_manifest_signature(manifest_path, signature_path)
    return entrypoint


def _atomic_exchange(left, right):
    """Atomically exchange two same-filesystem paths on Linux."""
    if sys.platform != 'linux':
        raise OSError('atomic directory exchange is unavailable')
    libc = ctypes.CDLL(None, use_errno=True)
    renameat2 = getattr(libc, 'renameat2', None)
    if renameat2 is None:
        raise OSError('atomic directory exchange is unavailable')
    renameat2.argtypes = [ctypes.c_int, ctypes.c_char_p, ctypes.c_int, ctypes.c_char_p, ctypes.c_uint]
    renameat2.restype = ctypes.c_int
    result = renameat2(-100, os.fsencode(left), -100, os.fsencode(right), 0x2)
    if result != 0:
        error = ctypes.get_errno()
        raise OSError(error, os.strerror(error))


def addon_public_key_path():
    return Path(os.environ.get(
        'SCREENTRA_CAST_ADDON_SIGNING_PUBLIC_KEY',
        '/etc/screentra-cast/addon-signing.pub',
    ))


def _safe_addon_member(name):
    normalized = posixpath.normpath(name)
    if normalized in ('.', '') or normalized.startswith('/') or normalized.startswith('../') or '/../' in normalized:
        raise ValueError('addon package contains an unsafe path')
    if normalized not in ('manifest.json', 'manifest.sig') and not normalized.startswith('client/'):
        raise ValueError('addon package contains an unexpected member')
    return normalized


def _parse_addon_manifest(manifest_bytes):
    def reject_duplicate_fields(pairs):
        result = {}
        for key, value in pairs:
            if key in result:
                raise ValueError('addon manifest contains duplicate JSON fields')
            result[key] = value
        return result

    try:
        value = json.loads(
            manifest_bytes.decode('utf-8'),
            object_pairs_hook=reject_duplicate_fields,
            parse_constant=lambda _: (_ for _ in ()).throw(ValueError()),
        )
    except (UnicodeDecodeError, json.JSONDecodeError, ValueError) as exc:
        raise ValueError('addon manifest is not valid JSON') from exc
    if not isinstance(value, dict):
        raise ValueError('addon manifest must be an object')
    return value


def _extract_verified_addon_package(package_bytes, expected, staging_parent=None):
    if len(package_bytes) > MAX_ADDON_PACKAGE_BYTES:
        raise ValueError('addon package exceeds the client limit')
    staging = Path(tempfile.mkdtemp(
        prefix='.addon-verify-',
        dir=os.fspath(staging_parent) if staging_parent is not None else None,
    ))
    try:
        with tarfile.open(fileobj=io.BytesIO(package_bytes), mode='r:gz') as archive:
            seen = set()
            extracted_bytes = 0
            for index, member in enumerate(archive):
                if index >= 64:
                    raise ValueError('addon package contains too many members')
                name = _safe_addon_member(member.name)
                if name in seen:
                    raise ValueError('addon package contains duplicate members')
                seen.add(name)
                if member.isdir():
                    continue
                if not member.isfile() or member.size > MAX_ADDON_MEMBER_BYTES:
                    raise ValueError('addon package contains an unsupported or oversized file')
                source = archive.extractfile(member)
                if source is None:
                    raise ValueError('addon package member cannot be read')
                target = staging / name
                target.parent.mkdir(parents=True, exist_ok=True)
                member_bytes = source.read(member.size + 1)
                if len(member_bytes) != member.size:
                    raise ValueError('addon package member size changed while reading')
                extracted_bytes += len(member_bytes)
                if extracted_bytes > MAX_ADDON_EXTRACTED_BYTES:
                    raise ValueError('addon package extracted data exceeds the client limit')
                target.write_bytes(member_bytes)
        manifest_path = staging / 'manifest.json'
        signature_path = staging / 'manifest.sig'
        try:
            manifest_bytes = _read_real_file(manifest_path, MAX_ADDON_MANIFEST_BYTES)
            _read_real_file(signature_path, MAX_ADDON_SIGNATURE_BYTES)
        except (OSError, ValueError) as exc:
            raise ValueError('addon package is missing or redirects its metadata') from exc
        manifest = _parse_addon_manifest(manifest_bytes)
        entrypoint, digest = _validate_addon_manifest_metadata(manifest, expected)
        client_path = staging / entrypoint
        try:
            client_bytes = _read_real_file(client_path, MAX_ADDON_MEMBER_BYTES)
        except (OSError, ValueError) as exc:
            raise ValueError('addon entrypoint is missing or redirects its content') from exc
        if hashlib.sha256(client_bytes).hexdigest().lower() != digest.lower():
            raise ValueError('addon entrypoint SHA-256 verification failed')
        _verify_addon_manifest_signature(manifest_path, signature_path)
        return staging, manifest, entrypoint
    except Exception:
        shutil.rmtree(staging, ignore_errors=True)
        raise


def _publish_verified_addon(target, staging, active_path, active_text):
    """Publish a verified directory with rollback-safe active metadata."""
    _ensure_real_directory(target.parent)
    _ensure_real_directory(staging)
    target_exists = _path_exists(target)
    if target_exists:
        _ensure_real_directory(target)
    old_active = _read_real_file(active_path) if _path_exists(active_path) else None
    try:
        if target_exists:
            _atomic_exchange(staging, target)
        else:
            os.replace(staging, target)
        _atomic_write_text(active_path, active_text)
    except Exception:
        try:
            if target_exists:
                _atomic_exchange(staging, target)
            elif _path_exists(target):
                _ensure_real_directory(target)
                shutil.rmtree(target)
            if old_active is None:
                if _path_exists(active_path):
                    _require_real_file(active_path)
                    Path(active_path).unlink()
            else:
                _atomic_write_text(active_path, old_active.decode('utf-8'))
        finally:
            if _path_exists(staging):
                _ensure_real_directory(staging)
                shutil.rmtree(staging)
        raise
    if _path_exists(staging):
        _ensure_real_directory(staging)
        shutil.rmtree(staging)


def fetch_addon_bytes(server, client_id, addon_id):
    url = (server.rstrip('/') + '/api/clients/' + quote(client_id, safe='') +
           '/addons/' + quote(addon_id, safe='') + '/package')
    req = urllib.request.Request(url, headers=client_auth_headers())
    with urlopen_ripley(req, timeout=20) as response:
        package = response.read(MAX_ADDON_PACKAGE_BYTES + 1)
    if len(package) > MAX_ADDON_PACKAGE_BYTES:
        raise ValueError('addon package exceeds the client limit')
    return package


def client_capability_status():
    """Report local capability observations without enabling any service."""
    camera_nodes = sorted(Path('/dev').glob('video*'))
    microphone_nodes = sorted(Path('/dev/snd').glob('pcm*C*D*c')) if Path('/dev/snd').is_dir() else []
    return [
        {
            'id': 'render_only',
            'state': 'available',
            'detail': 'Built-in declarative renderer is available',
        },
        {
            'id': 'camera_capture',
            'state': 'available' if camera_nodes else 'unavailable',
            'detail': ('Detected local video device' if camera_nodes
                       else 'No local video capture device detected'),
        },
        {
            'id': 'microphone_capture',
            'state': 'available' if microphone_nodes else 'unavailable',
            'detail': ('Detected local ALSA capture device' if microphone_nodes
                       else 'No local ALSA capture device detected'),
        },
        {
            'id': 'local_inference',
            'state': 'disabled',
            'detail': 'Signed client inference service is not enabled',
        },
        {
            'id': 'client_service',
            'state': 'disabled',
            'detail': 'Trusted client service runtime is not enabled',
        },
    ]


def sync_client_addons(server, client_id, cache_dir):
    """Fetch and verify enabled addon packages without executing their code."""
    status_path = Path(cache_dir) / 'addon-status.json'
    status = {
        'at': time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime()),
        'addons': [],
        'capabilities': client_capability_status(),
        # No trusted client supervisor is enabled yet. Keep this explicit so a
        # missing service report cannot be mistaken for a running service.
        'services': [],
    }
    try:
        url = server.rstrip('/') + '/api/clients/' + quote(client_id, safe='') + '/addons'
        req = urllib.request.Request(url, headers=client_auth_headers())
        with urlopen_ripley(req, timeout=10) as response:
            inventory = _parse_server_json(
                _read_bounded_response(response, MAX_JSON_FILE_BYTES)
            )
        addons = inventory.get('addons') if isinstance(inventory, dict) else None
        if not isinstance(addons, list):
            raise ValueError('server returned an invalid addon inventory')
    except Exception:
        status['error'] = GENERIC_INVENTORY_ERROR
        _write_status_report(status_path, status)
        return status

    validated = []
    try:
        for expected in addons:
            if not isinstance(expected, dict) or not ADDON_ID_RE.fullmatch(str(expected.get('id', ''))):
                raise ValueError('invalid addon inventory entry')
            version = _validate_addon_version(expected.get('version'))
            if expected.get('client_api_version') != ADDON_API_VERSION:
                raise ValueError('unsupported addon API version')
            if not isinstance(expected.get('client_sha256'), str) or not re.fullmatch(
                    r'[0-9a-fA-F]{64}', expected['client_sha256']):
                raise ValueError('invalid addon inventory digest')
            validated.append((expected, version))
    except Exception:
        for expected in addons:
            item_status = {}
            if isinstance(expected, dict) and ADDON_ID_RE.fullmatch(str(expected.get('id', ''))):
                item_status['id'] = expected['id']
            item_status.update({'state': 'failed', 'error': GENERIC_ADDON_ERROR})
            status['addons'].append(item_status)
        status['error'] = GENERIC_INVENTORY_ERROR
        _write_status_report(status_path, status)
        return status

    addon_root = Path(os.environ.get('RIPLEY_CLIENT_ADDONS_DIR', str(Path(cache_dir).parent / 'addons')))
    desired_ids = {expected['id'] for expected, _ in validated}
    try:
        _ensure_real_directory(addon_root, create=True)
    except Exception:
        for expected, version in validated:
            status['addons'].append({
                'id': expected['id'], 'version': version,
                'state': 'failed', 'error': GENERIC_ADDON_ERROR,
            })
        status['error'] = GENERIC_ADDON_ERROR
        _write_status_report(status_path, status)
        return status

    for expected, version in validated:
        item_status = {'id': expected['id'], 'version': version}
        try:
            id_dir = addon_root / expected['id']
            if _path_exists(id_dir):
                _ensure_real_directory(id_dir)
            target = id_dir / version
            if _path_exists(target):
                _ensure_real_directory(target)
            active_path = id_dir / 'active.json'
            active = _read_cache_json(active_path, {}) if _path_exists(id_dir) else {}
            target_exists = _path_exists(target)
            if target_exists:
                if (not isinstance(active, dict) or active.get('id') != expected['id'] or
                        active.get('version') != version or
                        str(active.get('client_sha256', '')).lower() != expected['client_sha256'].lower()):
                    item_status.update({'state': 'failed', 'error': GENERIC_ADDON_ERROR})
                    status['addons'].append(item_status)
                    continue
            cached_entrypoint = active.get('entrypoint')
            if cached_entrypoint is not None:
                if not isinstance(cached_entrypoint, str) or not cached_entrypoint.startswith('client/'):
                    raise ValueError('addon cache entrypoint is unsafe')
                if _safe_addon_member(cached_entrypoint) != cached_entrypoint:
                    raise ValueError('addon cache entrypoint is unsafe')
            cached_file = target / cached_entrypoint if cached_entrypoint and _path_exists(target) else None
            if (active.get('id') == expected['id'] and active.get('version') == version and
                    str(active.get('client_sha256', '')).lower() == expected['client_sha256'].lower() and
                    cached_file is not None):
                try:
                    _verify_cached_addon(target, active, expected)
                except Exception:
                    item_status.update({'state': 'failed', 'error': GENERIC_ADDON_ERROR})
                    status['addons'].append(item_status)
                    continue
                item_status.update({'state': 'verified_cached', 'entrypoint': cached_entrypoint})
                status['addons'].append(item_status)
                continue

            package = fetch_addon_bytes(server, client_id, expected['id'])
            staging, manifest, entrypoint = _extract_verified_addon_package(
                package, expected, staging_parent=addon_root)
            if manifest['version'] != version:
                raise ValueError('addon package version changed during verification')
            _validate_client_extension_capabilities(manifest)
            _ensure_real_directory(id_dir, create=True)
            if _path_exists(target):
                _ensure_real_directory(target)
            _publish_verified_addon(
                target,
                staging,
                active_path,
                json.dumps({
                    'id': manifest['id'], 'version': manifest['version'],
                    'client_sha256': manifest['client_sha256'],
                    'entrypoint': entrypoint,
                }, indent=2),
            )
            item_status.update({'state': 'verified', 'entrypoint': entrypoint})
        except Exception:
            item_status.update({'state': 'failed', 'error': GENERIC_ADDON_ERROR})
        status['addons'].append(item_status)

    if len(desired_ids) == len(validated):
        for child in addon_root.iterdir():
            if child.is_symlink() or not child.is_dir() or not ADDON_ID_RE.fullmatch(child.name) or child.name in desired_ids:
                continue
            active_path = child / 'active.json'
            if not _path_exists(active_path):
                continue
            try:
                _ensure_real_directory(child)
                _require_real_file(active_path)
                shutil.rmtree(child)
            except Exception:
                continue
    _write_status_report(status_path, status)
    return status


def post_addon_status(server, client_id, status):
    validate_status_report(status)
    url = server.rstrip('/') + '/api/clients/' + quote(client_id, safe='') + '/addons/status'
    request = urllib.request.Request(
        url,
        data=json.dumps(status).encode('utf-8'),
        headers={'Content-Type': 'application/json', **client_auth_headers()},
        method='POST',
    )
    try:
        with urlopen_ripley(request, timeout=10) as response:
            return response.status == 200
    except urllib.error.HTTPError as error:
        if error.code in (401, 404, 429):
            return False
        raise


def _reject_duplicate_json_keys(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError('server JSON contains duplicate fields')
        result[key] = value
    return result


def _parse_server_json(text):
    try:
        return json.loads(
            text,
            object_pairs_hook=_reject_duplicate_json_keys,
            parse_constant=lambda _: (_ for _ in ()).throw(ValueError()),
        )
    except (TypeError, ValueError, UnicodeError) as exc:
        raise ValueError('server response is not valid JSON') from exc


def _parse_command_response(text):
    if not isinstance(text, str) or len(text.encode('utf-8')) > MAX_COMMAND_RESPONSE_BYTES:
        raise ValueError('server command response exceeds its bound')
    result = _parse_server_json(text)
    if result is not None and not isinstance(result, dict):
        raise ValueError('server command response must be an object or null')
    return result


def poll_command(server, client_id, client_token):
    url = server.rstrip('/') + '/api/clients/' + quote(client_id, safe='') + '/command'
    try:
        req = urllib.request.Request(url, headers=client_auth_headers(client_token))
        with urlopen_ripley(req, timeout=5) as r:
            data = _read_bounded_response(r, MAX_COMMAND_RESPONSE_BYTES)
            return _parse_command_response(data)
    except urllib.error.HTTPError as e:
        if e.code in (401, 404, 429):
            return None
        raise
    except Exception:
        return None


def ack_command(server, client_id):
    url = server.rstrip('/') + '/api/clients/' + quote(client_id, safe='') + '/command/ack'
    try:
        req = urllib.request.Request(url, headers=client_auth_headers(), method='POST')
        with urlopen_ripley(req, timeout=5) as r:
            return _read_bounded_response(r, MAX_TEXT_RESPONSE_BYTES)
    except Exception as e:
        print(f'ack failed: {e}', flush=True)
        return None


def write_pending_command(command, cache_dir):
    pending_path = Path(cache_dir) / 'pending-command.json'
    _ensure_real_directory(pending_path.parent, create=True)
    _atomic_write_text(pending_path, json.dumps(command, indent=2))
    print(f'pending command written: {command.get("action")}', flush=True)
    try:
        import subprocess, shutil
        if shutil.which('systemctl'):
            subprocess.run(['systemctl', '--user', 'start', 'screentra-client-actor.service'], timeout=10)
    except Exception as e:
        print(f'actor start skipped/failed: {e}', flush=True)




def read_command_result(cache_dir):
    result_path = Path(cache_dir) / 'command-result.json'
    if not result_path.exists():
        return None, None
    try:
        return _read_json_file(result_path, None), result_path
    except Exception as e:
        print(f'bad command-result.json: {e}', flush=True)
        try: result_path.unlink()
        except Exception: pass
        return None, None


def _preserve_cached_layout_on_empty_config(cache_dir, config_path, data):
    """Keep the last good assignment when the server reports no layout."""
    marker_path = Path(cache_dir) / 'unassigned-layout-confirmation.json'
    if data.get('layout') is not None:
        try:
            marker_path.unlink()
        except FileNotFoundError:
            pass
        return False
    previous = _read_cache_json(config_path, {})
    previous_layout_id = previous.get('assigned_layout_id') if isinstance(previous, dict) else None
    if not previous_layout_id:
        return False
    # If the server still reports an assignment but cannot return its layout,
    # never replace a usable cached layout with the no-layout screen.
    if data.get('assigned_layout_id') is not None:
        return True
    if PRESERVE_CACHED_LAYOUT_ON_UNASSIGNED:
        return True
    now = time.time()
    marker = _read_cache_json(marker_path, {})
    try:
        first_seen = float(marker.get('first_seen', 0))
    except (TypeError, ValueError):
        first_seen = 0
    if marker.get('layout_id') != previous_layout_id or first_seen <= 0:
        _atomic_write_text(marker_path, json.dumps({'layout_id': previous_layout_id, 'first_seen': now}))
        return True
    if now - first_seen < 120:
        return True
    try:
        marker_path.unlink()
    except FileNotFoundError:
        pass
    return False


def fetch_config(server, client_id, cache_dir, display_index=0, config_pattern='assigned-layout-display-{index}.json'):
    url = server.rstrip('/') + '/api/clients/' + quote(client_id, safe='') + '/displays/' + str(display_index) + '/config'
    try:
        req = urllib.request.Request(url, headers=client_auth_headers())
        with urlopen_ripley(req, timeout=10) as r:
            data = _parse_server_json(_read_bounded_response(r, MAX_JSON_FILE_BYTES))
        # Preserve theme (set by heartbeat from local config, sent to server)
        # so it survives across config sync cycles.
        prev_path = Path(cache_dir, 'client-config.json')
        try:
            prev = _read_json_file(prev_path, {})
            if prev.get('theme'):
                data['theme'] = prev['theme']
        except Exception:
            pass
        _ensure_real_directory(Path(cache_dir), create=True)
        config_path = Path(cache_dir, 'client-config.json' if display_index == 0 else f'display-config-{display_index}.json')
        preserve_cached_layout = _preserve_cached_layout_on_empty_config(cache_dir, config_path, data)
        config_text = None if preserve_cached_layout else json.dumps(data, indent=2)
        comparable = dict(data) if isinstance(data, dict) else data
        if isinstance(comparable, dict):
            comparable.pop('server_time', None)
        try:
            previous = _read_cache_json(config_path, {})
            if isinstance(previous, dict):
                previous_comparable = dict(previous)
                previous_comparable.pop('server_time', None)
                if previous_comparable == comparable:
                    config_text = None
        except (OSError, UnicodeError, ValueError, json.JSONDecodeError):
            pass
        if config_text is not None:
            _atomic_write_text_if_changed(config_path, config_text)
        target = config_pattern.format(index=display_index)
        if data.get('layout') is not None:
            # backwards-compat: also write the canonical index=0 name
            _atomic_write_text_if_changed(Path(cache_dir, target), json.dumps(data['layout'], indent=2))
            if display_index == 0:
                _atomic_write_text_if_changed(Path(cache_dir, 'assigned-layout.json'), json.dumps(data['layout'], indent=2))
        elif not preserve_cached_layout:
            for stale in [Path(cache_dir, target), Path(cache_dir, 'assigned-layout.json') if display_index == 0 else None]:
                if stale is None: continue
                try: stale.unlink()
                except FileNotFoundError: pass
        return data
    except Exception as e:
        print(f'config sync failed: {e}', flush=True)
        return None


def fetch_all_displays(server, client_id, cache_dir, display_count, config_pattern, start_index=0):
    """Fetch per-display config and populate cache/assigned-layout-display-N.json."""
    for idx in range(start_index, display_count):
        fetch_config(server, client_id, cache_dir, display_index=idx, config_pattern=config_pattern)


def fetch_admin_users(server, client_id, cache_dir):
    """Sync server users for the local admin login.

    Server users are cached for authenticated local administration; without
    that cache the local admin login remains unprovisioned.
    """
    url = server.rstrip('/') + '/api/clients/' + quote(client_id, safe='') + '/admin-users'
    try:
        req = urllib.request.Request(url, headers=client_auth_headers())
        with urlopen_ripley(req, timeout=10) as r:
            data = _parse_server_json(_read_bounded_response(r, MAX_JSON_FILE_BYTES))
        users = data.get('users') or []
        if users:
            target = Path(cache_dir).parent / 'admin-users.json'
            _ensure_real_directory(target.parent, create=True)
            admin_text = json.dumps(data, indent=2)
            comparable = dict(data)
            comparable.pop('synced_at', None)
            try:
                previous = _read_cache_json(target, {})
                if isinstance(previous, dict):
                    previous_comparable = dict(previous)
                    previous_comparable.pop('synced_at', None)
                    if previous_comparable == comparable:
                        admin_text = None
            except (OSError, UnicodeError, ValueError, json.JSONDecodeError):
                pass
            if admin_text is not None:
                _atomic_write_text_if_changed(target, admin_text)
            print(f'admin users synced: {len(users)}', flush=True)
            return data
        return None
    except Exception as e:
        print(f'admin user sync failed: {e}', flush=True)
        return None

def post(server, data):
    url = server.rstrip('/') + '/api/clients/register'
    payload = dict(data)
    token = client_auth_token()
    if token:
        payload.setdefault('client_auth_token', token)
    req = urllib.request.Request(
        url,
        data=json.dumps(payload).encode(),
        headers={'Content-Type': 'application/json', **client_auth_headers()},
        method='POST',
    )
    with urlopen_ripley(req, timeout=10) as r:
        return r.status, _read_bounded_response(r, MAX_TEXT_RESPONSE_BYTES)


def main():
    def _default_server():
        env_url = os.environ.get('RIPLEY_SERVER_URL')
        if env_url:
            return env_url
        # Fallback: read the persisted server URL the installer writes.
        srv = Path.home() / '.local/share/screentra-client/server'
        if srv.exists():
            try:
                v = _read_real_file(srv, max_bytes=4096).decode('utf-8').strip()
                if v:
                    return v
            except Exception:
                pass
        return None

    ap = argparse.ArgumentParser()
    ap.add_argument('--server', default=_default_server())
    ap.add_argument('--interval', type=int, default=int(os.environ.get('RIPLEY_HEARTBEAT_INTERVAL', '30')))
    ap.add_argument('--once', action='store_true')
    ap.add_argument('--client-id', default=os.environ.get('RIPLEY_CLIENT_ID'))
    ap.add_argument('--name', default=os.environ.get('RIPLEY_CLIENT_NAME'))
    ap.add_argument('--role', default=os.environ.get('RIPLEY_CLIENT_ROLE', 'player'))
    ap.add_argument('--version', default=os.environ.get('RIPLEY_CLIENT_VERSION', '0.1.0'))
    ap.add_argument('--active-layout-id', default=os.environ.get('RIPLEY_ACTIVE_LAYOUT_ID'))
    ap.add_argument('--cache-dir', default=os.environ.get('RIPLEY_CLIENT_CACHE_DIR', os.path.expanduser('~/.local/share/screentra-client/cache')))
    ap.add_argument('--max-displays', type=int, default=int(os.environ.get('RIPLEY_MAX_DISPLAYS', '4')))
    ap.add_argument('--render-base-port', type=int, default=int(os.environ.get('RIPLEY_RENDER_BASE_PORT', '3900')))
    ap.add_argument('--display-config-pattern', default=os.environ.get('RIPLEY_DISPLAY_CONFIG_PATTERN', 'assigned-layout-display-{index}.json'))

    args = ap.parse_args()
    try:
        parsed_server = validate_server_url(args.server)
        if parsed_server.scheme == 'https':
            tls_context()
    except Exception as error:
        ap.error(str(error))

    while True:
        previous_orientation = 'normal'
        try:
            previous_orientation = _read_json_file(Path(args.cache_dir) / 'client-config.json', {}).get('orientation', 'normal')
        except Exception:
            pass
        data = payload(args)
        try:
            status, body = post(args.server, data)
            try:
                response = _parse_server_json(body)
                server_theme = response.get('theme')
                if server_theme in ('midnight', 'light', 'classic'):
                    theme_path = Path(args.cache_dir) / 'client-config.json'
                    try:
                        cached = _read_json_file(theme_path, {})
                    except Exception:
                        cached = {}
                    cached['theme'] = server_theme
                    _ensure_real_directory(theme_path.parent, create=True)
                    _atomic_write_text_if_changed(theme_path, json.dumps(cached, indent=2))
            except Exception:
                pass
        except Exception as e:
            # A temporary server outage must not kill the heartbeat service.
            status, body = 0, ''
            print(f'heartbeat post failed: {e}', flush=True)
        if status == 200 and data.get('last_command_result') is not None:
            rp = Path(args.cache_dir) / 'command-result.json'
            try:
                rp.unlink()
                print('command-result.json uploaded and cleared', flush=True)
            except Exception:
                pass
        # Track previous layout assignment to detect changes.
        prev_layout_path = Path(args.cache_dir) / 'previous-layout-id.txt'
        try:
            previous_layout_id = _read_real_file(prev_layout_path, 4096).decode('utf-8').strip() or None
        except Exception:
            cached_previous = _read_json_file(Path(args.cache_dir) / 'client-config.json', {})
            previous_layout_id = cached_previous.get('assigned_layout_id') or None
        cfg = fetch_config(args.server, data['client_id'], args.cache_dir, display_index=0, config_pattern=args.display_config_pattern)
        if cfg is None:
            # A transient 429/network failure must not turn a valid cached
            # assignment into "No layout assigned" on the renderer.
            cached_cfg = _read_json_file(Path(args.cache_dir) / 'client-config.json', {})
            current_layout_id = cached_cfg.get('assigned_layout_id') or previous_layout_id
            current_orientation = cached_cfg.get('orientation', previous_orientation)
        else:
            current_layout_id = cfg.get('assigned_layout_id')
            current_orientation = cfg.get('orientation', previous_orientation)
        # Restart kiosk on layout assignment change (e.g. after server nudge or editor assign).
        if cfg and current_layout_id and current_layout_id != previous_layout_id:
            try:
                # Persist the observed assignment before restarting the kiosk.
                # The kiosk restart can restart this heartbeat service too; writing
                # afterward caused the same assignment to trigger repeatedly.
                _atomic_write_text_if_changed(prev_layout_path, current_layout_id)
                subprocess.run(['sudo', '-n', 'systemctl', 'restart', 'screentra-cast-kiosk@0.service'], check=True, timeout=15)
                print(f'layout changed {previous_layout_id} -> {current_layout_id}; kiosk restarted', flush=True)
            except Exception as e:
                print(f'layout-change restart failed: {e}', flush=True)
        elif cfg and current_layout_id:
            _atomic_write_text_if_changed(prev_layout_path, current_layout_id or '')
        if cfg and current_orientation != previous_orientation:
            try:
                subprocess.run(['sudo', '-n', 'systemctl', 'restart', 'screentra-cast-kiosk@0.service'], check=True, timeout=15)
                print(f'orientation changed {previous_orientation} -> {current_orientation}; kiosk restarted', flush=True)
            except Exception as e:
                print(f'orientation restart failed: {e}', flush=True)
        fetch_all_displays(args.server, data['client_id'], args.cache_dir, len(data.get('displays') or []), args.display_config_pattern, start_index=1)
        fetch_admin_users(args.server, data['client_id'], args.cache_dir)
        addon_status = sync_client_addons(args.server, data['client_id'], args.cache_dir)
        try:
            post_addon_status(args.server, data['client_id'], addon_status)
        except Exception as error:
            print(f'addon status report failed: {error}', flush=True)
        if addon_status.get('addons') or addon_status.get('error'):
            print(f'addon sync: {json.dumps(addon_status, separators=(",", ":"))}', flush=True)
        assigned = current_layout_id
        cmd = poll_command(args.server, data['client_id'], client_auth_token())
        if cmd:
            write_pending_command(cmd, args.cache_dir)
            ack = ack_command(args.server, data['client_id'])
            print(f'heartbeat status={status} client_id={data["client_id"]} assigned_layout={assigned} command={cmd.get("action")} ack={ack}', flush=True)
        else:
            print(f'heartbeat status={status} client_id={data["client_id"]} assigned_layout={assigned}', flush=True)

        # Persist a per-display cache so the renderer can show
        # the connector / resolution label on the no-layout page.
        try:
            for d in data.get('displays') or []:
                idx = d.get('display_index')
                if idx is None: continue
                _atomic_write_text_if_changed(
                    Path(args.cache_dir, f'display-{idx}.json'),
                    __import__('json').dumps(d),
                )
        except Exception:
            pass

        # Persist a small status file so the renderer's no-layout page
        # can read live heartbeat info (last_seen, connection, assigned_layout_id)
        try:
            from datetime import datetime as _dt, timezone as _tz
            status_payload = {
                'at': _dt.now(_tz.utc).isoformat(),
                'connection': 'online' if status == 200 else 'offline',
                'assigned_layout_id': assigned,
                'pairing_code': client_pairing_code(data['client_id']),
            }
            _ensure_real_directory(Path(args.cache_dir), create=True)
            _atomic_write_text(
                Path(args.cache_dir) / 'last-heartbeat.json',
                __import__('json').dumps(status_payload),
            )
        except Exception:
            pass
        if args.once:
            print(body)
            return 0
        time.sleep(args.interval)


if __name__ == '__main__':
    raise SystemExit(main())

PY
chmod +x "$HEARTBEAT"
ln -sf "$HEARTBEAT" "$BIN_DIR/screentra-client-heartbeat"

cat > "$INSTALL_DIR/no-layout.html" <<'PY_NOLAYOUT'
<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8" />
<meta name="viewport" content="width=device-width,initial-scale=1" />
<title>Screentra Cast Player display</title>
<style>
  :root {
    --bg: #0b1220;
    --bg2: #0f172a;
    --line: rgba(148,163,184,.18);
    --text: #e2e8f0;
    --muted: #94a3b8;
    --accent: #38bdf8;
    --accent2: #22d3ee;
    --ok: #22c55e;
    --warn: #f59e0b;
    --err: #ef4444;
    --card: rgba(15,23,42,.72);
  }
  * { box-sizing: border-box; }
  html, body {
    cursor: none; margin: 0; padding: 0; height: 100%; }
  body {
    background:
      radial-gradient(1200px 800px at 75% -10%, rgba(56,189,248,.18), transparent 60%),
      radial-gradient(900px 700px at 10% 110%, rgba(34,211,238,.14), transparent 55%),
      linear-gradient(160deg, var(--bg) 0%, var(--bg2) 100%);
    color: var(--text);
    font-family: ui-sans-serif, system-ui, -apple-system, "Segoe UI", Roboto, "Helvetica Neue", Arial, sans-serif;
    overflow: hidden;
  }
  .wrap {
    display: flex;
    align-items: center;
    justify-content: center;
    height: 100%;
    padding: 4vh 4vw;
  }
  .card {
    width: min(1100px, 92vw);
    background: var(--card);
    border: 1px solid var(--line);
    border-radius: 18px;
    padding: clamp(20px, 3vw, 40px);
    backdrop-filter: blur(12px);
    box-shadow: 0 30px 80px rgba(0,0,0,.45);
    position: relative;
  }
  .brand {
    display: flex;
    align-items: center;
    gap: 14px;
    margin-bottom: 14px;
  }
  .brand .dot {
    width: 14px; height: 14px;
    border-radius: 50%;
    background: var(--accent);
    box-shadow: 0 0 24px var(--accent);
  }
  .brand .name {
    font-size: clamp(22px, 2.4vw, 32px);
    font-weight: 700;
    letter-spacing: .5px;
  }
  .brand .role {
    margin-left: auto;
    margin-right: 60px;
    color: var(--muted);
    font-size: 12px;
    letter-spacing: .12em;
    text-transform: uppercase;
  }
  .title {
    font-size: clamp(28px, 3.4vw, 48px);
    font-weight: 800;
    margin: 6px 0 4px;
    line-height: 1.1;
    background: linear-gradient(90deg, var(--accent), var(--accent2));
    -webkit-background-clip: text;
    background-clip: text;
    color: transparent;
  }
  .subtitle {
    color: var(--muted);
    margin: 0 0 22px;
    font-size: clamp(14px, 1.4vw, 18px);
  }
  .grid {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: 14px;
  }
  .row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 14px 16px;
    border: 1px solid var(--line);
    border-radius: 12px;
    background: rgba(2,6,23,.4);
  }
  .row .k {
    color: var(--muted);
    font-size: 12px;
    text-transform: uppercase;
    letter-spacing: .14em;
  }
  .row .v {
    margin-top: 4px;
    font-size: clamp(14px, 1.3vw, 18px);
    font-weight: 600;
    word-break: break-word;
  }
  .pill {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    padding: 4px 10px;
    border-radius: 999px;
    font-size: 12px;
    font-weight: 700;
    letter-spacing: .08em;
    text-transform: uppercase;
    border: 1px solid var(--line);
    background: rgba(15,23,42,.6);
  }
  .pill.ok    { color: var(--ok);   border-color: rgba(34,197,94,.4); }
  .pill.warn  { color: var(--warn); border-color: rgba(245,158,11,.4); }
  .pill.err   { color: var(--err);  border-color: rgba(239,68,68,.4); }
  .pill.activating { color: var(--warn); border-color: rgba(245,158,11,.4); }

  .touchline {
    margin-top: 18px;
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 12px 16px;
    border: 1px dashed var(--line);
    border-radius: 12px;
    color: var(--muted);
    font-size: 14px;
  }
  .touchline strong { color: var(--text); font-weight: 600; }
  .admin-link {
    margin-top: 12px;
    display: inline-flex;
    align-items: center;
    gap: 8px;
    color: var(--accent);
    text-decoration: none;
    border: 1px solid var(--line);
    border-radius: 999px;
    padding: 8px 12px;
    background: rgba(2,6,23,.35);
    font-size: 13px;
    font-weight: 700;
  }
  .admin-link:hover { border-color: var(--accent); }
  .admin-card {
    margin-top: 14px;
    display: block;
    padding: 14px 16px;
    border: 1px solid rgba(56,189,248,.35);
    border-radius: 14px;
    background: rgba(14,116,144,.14);
  }
  .admin-card .label {
    color: var(--muted);
    font-size: 12px;
    text-transform: uppercase;
    letter-spacing: .14em;
  }
  .admin-card .url {
    margin-top: 4px;
    color: var(--text);
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
    font-size: clamp(16px, 1.8vw, 24px);
    font-weight: 800;
    word-break: break-all;
  }
  .admin-card .hint { margin-top: 4px; color: var(--muted); font-size: 13px; }
  .admin-card .button {
    color: #03131f;
    background: linear-gradient(90deg, var(--accent), var(--accent2));
    border-radius: 999px;
    padding: 11px 15px;
    text-decoration: none;
    font-weight: 900;
    white-space: nowrap;
    box-shadow: 0 10px 30px rgba(56,189,248,.22);
  }

  .gear {
    position: absolute;
    top: 18px;
    right: 18px;
    width: 64px;
    height: 64px;
    border-radius: 50%;
    background: rgba(2,6,23,.6);
    border: 1px solid var(--line);
    display: grid;
    place-items: center;
    cursor: pointer;
    transition: transform .15s ease, background .15s ease;
    z-index: 50;
    touch-action: manipulation;
    pointer-events: auto;
  }
  .gear:hover, .gear:active { background: rgba(56,189,248,.25); transform: rotate(30deg); }
  .gear svg { width: 28px; height: 28px; stroke: var(--accent); fill: none; stroke-width: 1.8; pointer-events: none; }
  .gear#calGear { right: 92px; }
  .gear#calGear:hover, .gear#calGear:active { transform: rotate(0deg); background: rgba(234,179,8,.25); }
  .gear#calGear svg { width: 24px; height: 24px; fill: var(--accent); stroke: none; }

  /* Modal */
  .modal-back {
    position: fixed;
    inset: 0;
    background: rgba(2,6,23,.72);
    backdrop-filter: blur(6px);
    display: none;
    align-items: center;
    justify-content: center;
    z-index: 100;
  }
  .modal-back.on { display: flex; }
  .modal {
    width: min(720px, 92vw);
    max-height: 86vh;
    overflow: auto;
    background: var(--bg2);
    border: 1px solid var(--line);
    border-radius: 16px;
    padding: 28px;
    box-shadow: 0 30px 80px rgba(0,0,0,.6);
  }
  .modal h2 {
    margin: 0 0 4px;
    font-size: 22px;
    font-weight: 700;
  }
  .modal .sub { color: var(--muted); margin-bottom: 18px; font-size: 14px; }
  .device {
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 12px 14px;
    border: 1px solid var(--line);
    border-radius: 10px;
    margin-bottom: 8px;
    cursor: none;
    transition: border-color .15s ease, background .15s ease;
  }
  .device:hover { background: rgba(56,189,248,.06); }
  .device.selected { border-color: var(--accent); background: rgba(56,189,248,.12); }
  .device .name { font-weight: 600; }
  .device .path { color: var(--muted); font-size: 12px; }
  .device .pick { margin-left: auto; }

  .testpad {
    margin: 16px 0;
    height: 110px;
    border: 1px dashed var(--accent);
    border-radius: 10px;
    display: grid;
    place-items: center;
    color: var(--accent);
    font-size: 14px;
    background: rgba(56,189,248,.05);
    user-select: none;
  }
  .actions {
    display: flex;
    justify-content: flex-end;
    gap: 10px;
    margin-top: 18px;
  }
  button {
    background: rgba(2,6,23,.6);
    color: var(--text);
    border: 1px solid var(--line);
    border-radius: 10px;
    padding: 10px 18px;
    font-size: 14px;
    font-weight: 600;
    cursor: none;
  }
  button.primary { background: linear-gradient(90deg, var(--accent), var(--accent2)); color: #002133; border: none; }
  button:hover { background: rgba(56,189,248,.12); }
  button.primary:hover { filter: brightness(1.05); }

  .pulse {
    animation: pulse 2.4s ease-in-out infinite;
  }
  @keyframes pulse {
    0%,100% { opacity: .55; }
    50% { opacity: 1; }
  }

  .foot {
    margin-top: 18px;
    color: var(--muted);
    font-size: 12px;
    text-align: right;
    letter-spacing: .04em;
  }
</style>
</head>
<body>
  <div class="wrap">
    <div class="card">
      <div class="brand">
        <span class="dot pulse"></span>
        <span class="name">Screentra Cast</span>
        <span class="role" id="role">Digital Signage Player</span>
      </div>

      <div class="title" id="title">No layout assigned</div>
      <p class="subtitle" id="subtitle">Waiting for the server to push a layout to this display.</p>

      <div class="grid">
        <div class="row" style="flex-wrap:wrap; gap:10px;">
          <div style="flex:0 0 auto;">
            <div class="k">Status</div>
            <div class="v"><span class="pill ok" id="statusPill">CONNECTED</span></div>
          </div>
          <div class="k" id="lastSeen" style="text-align:right; flex:1 1 auto;">Last seen —</div>
        </div>
        <div class="row">
          <div>
            <div class="k">Server</div>
            <div class="v" id="serverUrl">—</div>
          </div>
          <div class="k" id="serverLatency">— ms</div>
        </div>
        <div class="row">
          <div>
            <div class="k">Hostname</div>
            <div class="v" id="hostname">—</div>
          </div>
          <div class="k">Role · player</div>
        </div>
        <div class="row">
          <div>
            <div class="k">Display</div>
            <div class="v" id="displayInfo">—</div>
          </div>
          <div class="k" id="displayIdx">idx —</div>
        </div>
        <div class="row" style="grid-column: span 2">
          <div>
            <div class="k">Client ID</div>
            <div class="v" id="clientId" style="font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 14px;">—</div>

          </div>
          <div class="k" id="assignedLayout">No layout assigned</div>
        </div>
        <div class="row" style="grid-column: span 2">
          <div>
            <div class="k">Pairing code</div>
            <div class="v" id="pairingCode" style="font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 24px; letter-spacing: .18em;">—</div>
          </div>
          <div class="k">Confirm this code on the server before adoption</div>
        </div>
      </div>

      <div class="touchline" id="touchline">
        Touchscreen <strong id="touchState">— detecting —</strong>
      </div>
      <div class="admin-card" id="adminCard">
        <div>
          <div class="label">Local admin / easy setup</div>
          <div class="url" id="adminUrl">http://this-device:3910/</div>
          <div class="hint">Displayed for setup only. Type this URL from a phone or laptop on the same LAN for status, logs, touch calibration, and CA repair.</div>
        </div>
      </div>

      <div class="foot" id="foot">Screentra Cast · build __BUILD__ · client __CLIENT_ID__</div>
      <div id="__dbg" style="position:absolute;bottom:4px;left:8px;font-size:10px;color:rgba(148,163,184,.5);font-family:ui-monospace,monospace;">JS starting…</div>

      <div class="gear" id="calGear" title="Run touchscreen calibration test"><svg viewBox="0 0 24 24"><path d="M12 2L9 9H2l5 4-2 8 7-5 7 5-2-8 5-4h-7z"/></svg></div><div class="gear" id="gear" title="Configure touchscreen">
        <svg viewBox="0 0 24 24"><circle cx="12" cy="12" r="3"/><path d="M19.4 15a1.7 1.7 0 0 0 .3 1.8l.1.1a2 2 0 1 1-2.8 2.8l-.1-.1a1.7 1.7 0 0 0-1.8-.3 1.7 1.7 0 0 0-1 1.5V21a2 2 0 1 1-4 0v-.1a1.7 1.7 0 0 0-1.1-1.5 1.7 1.7 0 0 0-1.8.3l-.1.1a2 2 0 1 1-2.8-2.8l.1-.1a1.7 1.7 0 0 0 .3-1.8 1.7 1.7 0 0 0-1.5-1H3a2 2 0 1 1 0-4h.1a1.7 1.7 0 0 0 1.5-1.1 1.7 1.7 0 0 0-.3-1.8l-.1-.1a2 2 0 1 1 2.8-2.8l.1.1a1.7 1.7 0 0 0 1.8.3H9a1.7 1.7 0 0 0 1-1.5V3a2 2 0 1 1 4 0v.1a1.7 1.7 0 0 0 1 1.5 1.7 1.7 0 0 0 1.8-.3l.1-.1a2 2 0 1 1 2.8 2.8l-.1.1a1.7 1.7 0 0 0-.3 1.8V9a1.7 1.7 0 0 0 1.5 1H21a2 2 0 1 1 0 4h-.1a1.7 1.7 0 0 0-1.5 1z"/></svg>
      </div>
    </div>
  </div>

  <!-- Touchscreen configuration modal -->
  <div class="modal-back" id="modal">
    <div class="modal">
      <h2>Touchscreen configuration</h2>
      <p class="sub">Pick the device this display should use as its touch input. If auto-detect missed it, choose from the list below.</p>

      <div id="deviceList">Detecting…</div>

      <div class="testpad" id="testpad">Tap here to test</div>

      <div class="actions">
        <button id="cancelBtn">Cancel</button>
        <button id="rescanBtn">Rescan</button>
        <button class="primary" id="saveBtn">Save</button>
      </div>
    </div>
  </div>

  <script>
    // Bottom-left diagnostic: shows JS state so silent failures are visible.
    const __dbg = (m) => { const d = document.getElementById("__dbg"); if (d) d.textContent = m + " " + new Date().toISOString().slice(11,19); };
    window.addEventListener("error", e => __dbg("err " + (e.message || e)));
    window.addEventListener("unhandledrejection", e => __dbg("rej " + (e.reason && e.reason.message || e.reason || e)));
    const $ = (id) => document.getElementById(id);
    async function readTextBounded(response, maximum) {
      const reader = response.body && response.body.getReader ? response.body.getReader() : null;
      if (!reader) throw new Error('response body is unavailable');
      const decoder = new TextDecoder();
      let total = 0;
      let text = '';
      try {
        while (true) {
          const part = await reader.read();
          if (part.done) { text += decoder.decode(); return text; }
          const value = part.value || new Uint8Array();
          total += value.byteLength;
          if (total > maximum) {
            try { await reader.cancel(); } catch (_) {}
            throw new Error('response exceeds its size limit');
          }
          text += decoder.decode(value, { stream: true });
        }
      } finally {
        reader.releaseLock();
      }
    }
    let currentDevices = [];
    let selectedPath = null;

    async function refreshStatus() {
      try {
        const r = await fetch('/api/status.json', { cache: 'no-store' });
        if (!r.ok) return;
        const s = JSON.parse(await readTextBounded(r, 64 * 1024));
        if (s.display_index != null) document.title = 'Screentra Cast Player display ' + s.display_index;
        $('serverUrl').textContent = s.server_url || '—';
        $('hostname').textContent  = s.hostname || '—';
        $('clientId').textContent  = s.client_id || '—';
        $('pairingCode').textContent = s.pairing_code || '—';
        const localAdminUrl = (() => { try { const url = new URL(window.location.href); url.port = '3910'; url.pathname = '/'; url.search = ''; url.hash = ''; return url.href; } catch (_) { return 'http://localhost:3910/'; } })();
        const adminUrl = s.admin_url || (s.lan_ip ? ('http://' + s.lan_ip + ':3910/') : localAdminUrl);
        const adminUrlEl = $('adminUrl');
        if (adminUrlEl) adminUrlEl.textContent = adminUrl;
        $('lastSeen').textContent  = 'Last seen ' + (s.last_seen || '—');
        $('serverLatency').textContent = (s.server_latency_ms != null ? s.server_latency_ms + ' ms' : '— ms');
        $('displayIdx').textContent = 'idx ' + (s.display_index != null ? s.display_index : '—');
        $('displayInfo').textContent =
          (s.display_label || '—') +
          (s.display_resolution ? ' · ' + s.display_resolution : '') +
          (s.display_connector ? ' · ' + s.display_connector : '');
        $('role').textContent = s.role || 'Digital Signage Player';
        $('assignedLayout').textContent = s.assigned_layout_id
          ? 'Layout ' + s.assigned_layout_id.slice(0,8) + '…'
          : 'No layout assigned';

        const pill = $('statusPill');
        if (s.connection === 'online') { pill.textContent='CONNECTED'; pill.className='pill ok'; }
        else if (s.connection === 'stale') { pill.textContent='STALE'; pill.className='pill warn'; }
        else { pill.textContent='OFFLINE'; pill.className='pill err'; }

        const tl = $('touchline');
        if (s.touch_device) {
          tl.innerHTML = 'Touchscreen <strong>'+ s.touch_device +'</strong>' +
                         (s.touch_path ? ' <span style="color:var(--muted)">('+ s.touch_path +')</span>' : '') +
                         ' · <a style="color:var(--accent)" id="cfgLink" href="#">configure</a>';
          $('cfgLink').addEventListener('click', openModal);
        } else {
          tl.innerHTML = 'Touchscreen <strong>not detected</strong> · ' +
                         '<a style="color:var(--accent)" id="cfgLink" href="#">configure</a>';
          $('cfgLink').addEventListener('click', openModal);
        }

        $('foot').textContent = 'Screentra Cast · build ' + (s.build || '?') +
                                ' · display ' + (s.display_index != null ? s.display_index : '?') +
                                ' · port ' + (s.port || '?');
        __dbg('JS ok cid=' + (s.client_id || '-').slice(0, 12));
      } catch (e) {
        __dbg('JS err ' + (e && e.message || e));
      }
    }

    async function openModal() {
      $('modal').classList.add('on');
      await loadDevices();
    }
    function closeModal() { $('modal').classList.remove('on'); }

    async function loadDevices() {
      const list = $('deviceList');
      list.textContent = 'Detecting…';
      try {
        const r = await fetch('/api/touchscreen.json', { cache: 'no-store' });
        const data = JSON.parse(await readTextBounded(r, 64 * 1024));
        currentDevices = data.devices || [];
        selectedPath = data.selected || null;
        if (currentDevices.length === 0) {
          list.innerHTML = '<div class="sub">No input devices detected. Try Rescan.</div>';
          return;
        }
        list.innerHTML = '';
        for (const d of currentDevices) {
          const el = document.createElement('div');
          el.className = 'device' + ((selectedPath && d.path === selectedPath) ? ' selected' : (d.recommended ? ' selected' : ''));
          el.innerHTML = '<div><div class="name">' + d.name + '</div>' +
                         '<div class="path">' + d.path + (d.type ? ' · ' + d.type : '') + '</div></div>' +
                         '<div class="pick"><span class="pill ' + (d.recommended ? 'ok' : '') + '">' +
                         (d.recommended ? 'RECOMMENDED' : 'USE') + '</span></div>';
          el.addEventListener('click', () => {
            selectedPath = d.path;
            for (const c of list.children) c.classList.remove('selected');
            el.classList.add('selected');
          });
          list.appendChild(el);
          if (d.recommended && !selectedPath) selectedPath = d.path;
        }
      } catch (e) {
        list.innerHTML = '<div class="sub">Detection failed: ' + e + '</div>';
      }
    }

    async function saveSelection() {
      try {
        const r = await fetch('/api/touchscreen/select', {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ path: selectedPath }),
        });
        if (r.ok) {
          closeModal();
          refreshStatus();
        } else {
          alert('Save failed: ' + r.status);
        }
      } catch (e) {
        alert('Save failed: ' + e);
      }
    }

    // Touch test pad
    $('testpad').addEventListener('pointerdown', (ev) => {
      ev.preventDefault();
      const r = ev.target.getBoundingClientRect();
      const x = Math.round((ev.clientX - r.left) / r.width  * 100);
      const y = Math.round((ev.clientY - r.top ) / r.height * 100);
      ev.target.textContent = 'Tapped at ' + x + '%, ' + y + '%';
      ev.target.style.background = 'rgba(34,197,94,.18)';
      setTimeout(() => {
        ev.target.style.background = '';
        ev.target.textContent = 'Tap here to test';
      }, 1200);
    });

    document.addEventListener('DOMContentLoaded', () => {
      $('gear').addEventListener('click', openModal);
      $('cancelBtn').addEventListener('click', closeModal);
      $('rescanBtn').addEventListener('click', loadDevices);
      $('saveBtn').addEventListener('click', saveSelection);
    });
    const __calBtn = document.getElementById('calibrateBtn');
    if (__calBtn) __calBtn.addEventListener('click', () => {
      // Open the calibration page in the same tab. The renderer serves
      // /touch-calibration.html which posts back to /api/touch-calibration.json.
      window.location.href = '/touch-calibration.html';
    });
    const __calGear = document.getElementById('calGear');
    if (__calGear) {
      __calGear.addEventListener('click', () => {
        if (confirm('Run touchscreen calibration test? You\'ll tap 4 corners, a grid, edge bars, then drag a circle.')) {
          window.location.href = '/touch-calibration.html';
        }
      });
    }
    // Wire the gear icon to open the touchscreen configuration modal.
    // Without this, tapping the gear icon (top-right, SVG) does nothing
    // because the cfgLink anchor is inside the touchline, not the gear.
    const __gear = document.getElementById('gear');
    if (__gear) __gear.addEventListener('click', openModal);

    refreshStatus();



    async function checkAssignedLayoutAndReload() {
      try {
        const r = await fetch('/assigned-layout.json?ts=' + Date.now(), { cache: 'no-store' });
        if (r.ok) {
          const txt = await readTextBounded(r, 4 * 1024 * 1024);
          if (txt && txt.trim() && txt.trim() !== '{}') {
            window.location.reload();
          }
        }
      } catch (e) {}
    }
    setInterval(refreshStatus, 4000);
    setInterval(checkAssignedLayoutAndReload, 5000);
    setTimeout(checkAssignedLayoutAndReload, 1200);

    // === Cursor auto-hide (3.5s default, configurable via /api/cursor-config.json) ===
    let __cursorIdle = Date.now();
    let __cursorHideSecs = 3.5;
    const __cursorShow = () => { __cursorIdle = Date.now(); document.documentElement.style.cursor = 'default'; };
    ['pointermove','pointerdown','pointerup','mousemove','mousedown','mouseup','touchstart','touchmove','wheel','keydown','scroll'].forEach(ev =>
      window.addEventListener(ev, __cursorShow, { capture: true, passive: true }));
    async function __cursorLoadConfig() {
      try {
        const r = await fetch('/api/cursor-config.json', { cache: 'no-store' });
        if (r.ok) { const c = JSON.parse(await readTextBounded(r, 64 * 1024)); __cursorHideSecs = Number(c.hide_secs) || 3.5; }
      } catch (e) {}
    }
    setInterval(() => {
      if (Date.now() - __cursorIdle > __cursorHideSecs * 1000) {
        document.documentElement.style.cursor = 'none';
      }
    }, 200);
    __cursorLoadConfig();

    // === Watchdog: chrome tab/renderer-process crashes silently on this
    // labwc/wayland box and leaves the page black while still capturing
    // touch events. Force a full page reload every 5 min as a safety net
    // so the user recovers without manual intervention.
    setTimeout(() => { try { window.location.reload(); } catch (_e) {} }, 5 * 60 * 1000);
  </script>
</body>
</html>

PY_NOLAYOUT

# Substitute placeholder values into no-layout.html (build hash,
# client id). Done with sed so the heredoc above can stay single-quoted.
sed -i \
  -e "s/__BUILD__/$BUILD/g" \
  -e "s/__CLIENT_ID__/$CLIENT_ID/g" \
  "$INSTALL_DIR/no-layout.html"

cat > "$INSTALL_DIR/touch-calibration.html" <<'PY_CALIB'
<!doctype html>
<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1, user-scalable=no">
<title>Screentra Cast Touchscreen Calibration Test</title>
<style>
  :root {
    --bg: #020617;
    --panel: #0f172a;
    --text: #e2e8f0;
    --muted: #94a3b8;
    --accent: #38bdf8;
    --accent2: #22d3ee;
    --ok: #22c55e;
    --warn: #eab308;
    --err: #ef4444;
    --line: #1e293b;
  }
  * { box-sizing: border-box; margin: 0; padding: 0; -webkit-tap-highlight-color: transparent; user-select: none; }
  html, body { height: 100%; background: var(--bg); color: var(--text); font-family: system-ui, -apple-system, "Segoe UI", Roboto, sans-serif; overflow: hidden; }
  body { display: grid; grid-template-rows: auto 1fr auto; }
  header { padding: 12px 24px; border-bottom: 1px solid var(--line); display: flex; align-items: center; gap: 16px; background: rgba(2,6,23,.7); }
  header .stage { font-size: 14px; color: var(--muted); }
  header .stage b { color: var(--accent); }
  header h1 { font-size: 16px; font-weight: 700; }
  header .spacer { flex: 1; }
  header button {
    background: rgba(15,23,42,.9); color: var(--text); border: 1px solid var(--line);
    border-radius: 8px; padding: 8px 14px; font-size: 13px; cursor: pointer; font-weight: 600;
  }
  header button.primary { background: var(--accent); color: #001020; border-color: var(--accent); }
  header button:hover { border-color: var(--accent); }
  main { position: relative; overflow: hidden; }
  .stage-canvas { position: absolute; inset: 0; touch-action: none; }
  .stage-canvas.grid-test { background: linear-gradient(180deg, #0b1226 0%, #050d1f 100%); }
  .stage-canvas.target { background: #000; }
  .stage-canvas.summary { background: linear-gradient(180deg, #050d1f 0%, #020617 100%); }
  .grid-cell { position: absolute; border: 2px solid rgba(255,255,255,.15); border-radius: 12px; display: grid; place-items: center; color: #fff; font-weight: 800; font-size: 24px; transition: background 120ms; touch-action: none; }
  .grid-cell.idle { background: rgba(56,189,248,.18); }
  .grid-cell.armed { background: rgba(234,179,8,.85); }
  .grid-cell.hit { background: rgba(34,197,94,.85); }
  .grid-cell.missed { background: rgba(239,68,68,.85); }
  .edge-bar { position: absolute; background: rgba(56,189,248,.4); border: 2px solid var(--accent); border-radius: 8px; display: grid; place-items: center; color: #fff; font-weight: 700; }
  .edge-bar.hit { background: rgba(34,197,94,.7); border-color: var(--ok); }
  .edge-bar.missed { background: rgba(239,68,68,.7); border-color: var(--err); }
  .hint { position: absolute; left: 50%; top: 16px; transform: translateX(-50%); padding: 8px 14px; background: rgba(2,6,23,.8); border-radius: 999px; font-size: 14px; color: var(--text); pointer-events: none; z-index: 100; }
  .hint b { color: var(--accent); }
  .summary-wrap { position: absolute; inset: 0; display: grid; place-items: center; padding: 24px; overflow-y: auto; }
  .summary-card { background: var(--panel); border: 1px solid var(--line); border-radius: 14px; padding: 24px; max-width: 720px; width: 100%; }
  .summary-card h2 { font-size: 24px; margin-bottom: 16px; }
  .summary-card h2.pass { color: var(--ok); }
  .summary-card h2.fail { color: var(--err); }
  .summary-row { display: flex; justify-content: space-between; padding: 8px 0; border-bottom: 1px solid var(--line); font-size: 14px; }
  .summary-row:last-child { border-bottom: none; }
  .summary-row .label { color: var(--muted); }
  .summary-row .value { color: var(--text); font-weight: 600; }
  .summary-actions { display: flex; gap: 10px; margin-top: 16px; }
  .summary-actions button { background: rgba(15,23,42,.9); color: var(--text); border: 1px solid var(--line); border-radius: 8px; padding: 10px 16px; font-size: 14px; cursor: pointer; font-weight: 600; flex: 1; }
  .summary-actions button.primary { background: var(--accent); color: #001020; border-color: var(--accent); }
  .progress { display: flex; gap: 6px; }
  .progress .dot { width: 10px; height: 10px; border-radius: 50%; background: var(--line); }
  .progress .dot.active { background: var(--accent); }
  .progress .dot.done { background: var(--ok); }
  .progress .dot.failed { background: var(--err); }
  .failures { background: rgba(239,68,68,.08); border: 1px solid rgba(239,68,68,.3); border-radius: 8px; padding: 12px; margin-top: 12px; font-size: 13px; max-height: 220px; overflow-y: auto; }
  .failures li { padding: 4px 0; color: var(--muted); list-style: none; }
  .failures li b { color: var(--err); margin-right: 6px; }
  .drag-line { position: absolute; pointer-events: none; background: var(--accent); height: 4px; transform-origin: 0 50%; border-radius: 2px; opacity: .9; }
  .drag-handle { position: absolute; width: 60px; height: 60px; border-radius: 50%; background: var(--accent); border: 3px solid #fff; transform: translate(-50%,-50%); pointer-events: none; }
  .drag-handle.start { background: var(--ok); }
  .drag-handle.end { background: var(--warn); }
</style>
</head>
<body>
<header>
  <div class="progress" id="progress">
    <div class="dot" data-stage="0"></div>
    <div class="dot" data-stage="1"></div>
    <div class="dot" data-stage="2"></div>
    <div class="dot" data-stage="3"></div>
    <div class="dot" data-stage="4"></div>
  </div>
  <h1>Touchscreen calibration</h1>
  <div class="stage" id="stageLabel">—</div>
  <div class="spacer"></div>
  <button id="backBtn">Back</button>
</header>
<main>
  <div id="canvas" class="stage-canvas"></div>
</main>

<script>
"use strict";
const $ = (id) => document.getElementById(id);
const canvas = $('canvas');

const results = {
  started_at: new Date().toISOString(),
  stages: {},
  pixel_drift_max: 0,
  corners_total: 0,
  corners_hit: 0,
  edge_dead_strips: [],
  drag_max_distance: 0,
  summary: 'pending',
  finished_at: null,
};

let currentStage = 0;
const dragState = { active: false, startX: 0, startY: 0, currentX: 0, currentY: 0, targetX: 0, targetY: 0 };

const STAGE_DEFS = [
  { id: 'corner',  label: 'Corners' },
  { id: 'grid',    label: 'Grid' },
  { id: 'edges',   label: 'Edges' },
  { id: 'drag',    label: 'Drag' },
  { id: 'summary', label: 'Result' },
];

const STAGE_HANDLERS = {
  corner: {
    enter: () => cornerEnter(),
    onTap: (x, y) => cornerTap(x, y),
  },
  grid: {
    enter: () => gridEnter(),
    onTap: (x, y) => gridTap(x, y),
  },
  edges: {
    enter: () => edgesEnter(),
    onTap: (x, y) => edgesTap(x, y),
  },
  drag: {
    enter: () => dragEnter(),
    onTap: (x, y) => dragTap(x, y),
    onMove: (x, y) => dragMove(x, y),
  },
  summary: {
    enter: () => summaryEnter(),
    onTap: () => {},
  },
};

function rect() {
  const main = document.querySelector('main');
  return { w: main.clientWidth, h: main.clientHeight };
}
function rand(min, max) { return Math.random() * (max - min) + min; }
function clearChildren(el) { while (el.firstChild) el.removeChild(el.firstChild); }
function showHint(html) {
  let h = document.getElementById('hint');
  if (!h) { h = document.createElement('div'); h.id = 'hint'; h.className = 'hint'; canvas.appendChild(h); }
  h.innerHTML = html;
}
function hideHint() { const h = document.getElementById('hint'); if (h) h.remove(); }

function setStage(idx) {
  currentStage = idx;
  const dotEls = document.querySelectorAll('#progress .dot');
  dotEls.forEach((d, i) => {
    d.className = 'dot';
    if (i < idx) {
      const s = results.stages[STAGE_DEFS[i].id];
      d.classList.add(s && s.summary === 'fail' ? 'failed' : 'done');
    } else if (i === idx) {
      d.classList.add('active');
    }
  });
  $('stageLabel').innerHTML = '<b>' + (idx+1) + '/' + STAGE_DEFS.length + '</b> · ' + STAGE_DEFS[idx].label;
  STAGE_HANDLERS[STAGE_DEFS[idx].id].enter();
}

canvas.addEventListener('pointerdown', (ev) => {
  ev.preventDefault();
  const sid = STAGE_DEFS[currentStage].id;
  if (sid === 'drag') {
    if (Math.hypot(ev.clientX - dragState.targetX, ev.clientY - dragState.targetY) < 60) {
      dragState.active = true;
      dragState.startX = dragState.currentX = ev.clientX;
      dragState.startY = dragState.currentY = ev.clientY;
      STAGE_HANDLERS.drag.onMove(ev.clientX, ev.clientY);
    }
    return;
  }
  STAGE_HANDLERS[sid].onTap(ev.clientX, ev.clientY);
}, { passive: false });

canvas.addEventListener('pointermove', (ev) => {
  if (!dragState.active) return;
  dragState.currentX = ev.clientX;
  dragState.currentY = ev.clientY;
  STAGE_HANDLERS.drag.onMove(ev.clientX, ev.clientY);
}, { passive: false });

canvas.addEventListener('pointerup', (ev) => {
  if (STAGE_DEFS[currentStage].id !== 'drag' || !dragState.active) return;
  dragState.active = false;
  const dist = Math.hypot(ev.clientX - dragState.targetX, ev.clientY - dragState.targetY);
  results.stages.drag = {
    passes: dist < 80 ? 1 : 0,
    fails:  dist < 80 ? 0 : 1,
    drag_max_distance: dist,
  };
  results.drag_max_distance = dist;
  finishStage();
}, { passive: false });

canvas.addEventListener('pointercancel', () => { dragState.active = false; }, { passive: true });

// === STAGE: Corner ===
let cornerState = null;
function cornerEnter() {
  canvas.className = 'stage-canvas target';
  clearChildren(canvas);
  const r = rect();
  const corners = [
    { x: 0,   y: 0,   label: 'TL' },
    { x: r.w, y: 0,   label: 'TR' },
    { x: r.w, y: r.h, label: 'BR' },
    { x: 0,   y: r.h, label: 'BL' },
  ];
  results.stages.corner = { passes: 0, fails: 0, hits: [], misses: [], total: 4 };
  results.corners_total = 4;
  results.corners_hit = 0;
  cornerState = { i: 0, corners };
  showHint('Tap each <b>corner</b> as it lights up');
  cornerShowNext();
}
function cornerShowNext() {
  clearChildren(canvas);
  if (cornerState.i >= cornerState.corners.length) { finishStage(); return; }
  const c = cornerState.corners[cornerState.i];
  const bar = document.createElement('div');
  bar.className = 'edge-bar armed';
  bar.style.left = (c.x - 50) + 'px';
  bar.style.top  = (c.y - 50) + 'px';
  bar.style.width = '100px';
  bar.style.height = '100px';
  bar.textContent = c.label;
  canvas.appendChild(bar);
  cornerState.active = { el: bar, expected: c, idx: cornerState.i };
  cornerState.timeout = setTimeout(() => {
    if (cornerState.active && cornerState.active.idx === cornerState.i) {
      bar.className = 'edge-bar missed';
      results.stages.corner.misses.push({ expected: c, reason: 'timeout' });
      results.stages.corner.fails++;
      cornerState.active = null;
      cornerState.i++;
      setTimeout(cornerShowNext, 800);
    }
  }, 4000);
}
function cornerTap(x, y) {
  const a = cornerState.active;
  if (!a) return;
  const dx = x - a.expected.x;
  const dy = y - a.expected.y;
  if (Math.abs(dx) <= 60 && Math.abs(dy) <= 60) {
    a.el.className = 'edge-bar hit';
    results.stages.corner.passes++;
    results.corners_hit++;
    results.stages.corner.hits.push({ x, y, expected: a.expected, drift_px: Math.hypot(dx, dy) });
    cornerState.active = null;
    if (cornerState.timeout) clearTimeout(cornerState.timeout);
    cornerState.i++;
    setTimeout(cornerShowNext, 600);
  } else {
    const dist = Math.hypot(dx, dy);
    if (dist > 30) results.pixel_drift_max = Math.max(results.pixel_drift_max, dist);
  }
}

// === STAGE: Grid ===
let gridState = null;
function gridEnter() {
  canvas.className = 'stage-canvas grid-test';
  clearChildren(canvas);
  const r = rect();
  const cols = 5, rows = 4;
  const padding = 60;
  const cellW = (r.w - padding * 2) / cols;
  const cellH = (r.h - padding * 2) / rows;
  const targets = [];
  for (let row = 0; row < rows; row++) {
    for (let col = 0; col < cols; col++) {
      targets.push({
        col, row,
        x: padding + col * cellW,
        y: padding + row * cellH,
        w: cellW - 8,
        h: cellH - 8,
      });
    }
  }
  // Shuffle
  for (let i = targets.length - 1; i > 0; i--) {
    const j = Math.floor(Math.random() * (i + 1));
    [targets[i], targets[j]] = [targets[j], targets[i]];
  }
  results.stages.grid = { passes: 0, fails: 0, hits: [], misses: [], total: targets.length };
  gridState = { i: 0, targets, cells: [], timer: null };
  showHint('Tap each <b>highlighted</b> cell');
  gridShowNext();
}
function gridShowNext() {
  clearChildren(canvas);
  gridState.cells = [];
  gridState.targets.forEach((t, idx) => {
    const cell = document.createElement('div');
    cell.className = 'grid-cell idle';
    cell.style.left = t.x + 'px';
    cell.style.top  = t.y + 'px';
    cell.style.width  = t.w + 'px';
    cell.style.height = t.h + 'px';
    canvas.appendChild(cell);
    gridState.cells.push(cell);
  });
  if (gridState.i >= gridState.targets.length) { finishStage(); return; }
  const ac = gridState.cells[gridState.i];
  if (ac) {
    ac.className = 'grid-cell armed';
    ac.textContent = (gridState.i + 1);
  }
  gridState.active = { idx: gridState.i, el: ac, expected: gridState.targets[gridState.i] };
  if (gridState.timer) clearTimeout(gridState.timer);
  gridState.timer = setTimeout(() => {
    if (gridState.active && gridState.active.idx === gridState.i) {
      if (gridState.active.el) gridState.active.el.className = 'grid-cell missed';
      results.stages.grid.misses.push({ idx: gridState.i, reason: 'timeout' });
      results.stages.grid.fails++;
      gridState.active = null;
      gridState.i++;
      setTimeout(gridShowNext, 600);
    }
  }, 5000);
}
function gridTap(x, y) {
  if (!gridState.active) return;
  const e = gridState.active.expected;
  if (x >= e.x && x <= e.x + e.w && y >= e.y && y <= e.y + e.h) {
    gridState.active.el.className = 'grid-cell hit';
    results.stages.grid.passes++;
    results.stages.grid.hits.push({ x, y, expected: e });
    gridState.active = null;
    if (gridState.timer) clearTimeout(gridState.timer);
    gridState.i++;
    setTimeout(gridShowNext, 400);
  } else {
    const cdx = x - (e.x + e.w/2);
    const cdy = y - (e.y + e.h/2);
    const dist = Math.hypot(cdx, cdy);
    if (dist > 20) results.pixel_drift_max = Math.max(results.pixel_drift_max, dist);
  }
}

// === STAGE: Edges ===
let edgeState = null;
function edgesEnter() {
  canvas.className = 'stage-canvas target';
  clearChildren(canvas);
  const r = rect();
  const edges = [
    { id: 'top',    x: r.w/2 - 200, y: 8,            w: 400, h: 60 },
    { id: 'right',  x: r.w - 68,    y: r.h/2 - 100,   w: 60,  h: 200 },
    { id: 'bottom', x: r.w/2 - 200, y: r.h - 68,      w: 400, h: 60 },
    { id: 'left',   x: 8,           y: r.h/2 - 100,   w: 60,  h: 200 },
  ];
  results.stages.edges = { passes: 0, fails: 0, hits: [], misses: [], total: 4 };
  results.edge_dead_strips = [];
  edgeState = { i: 0, edges };
  showHint('Tap each edge bar to confirm the <b>edge sensors work</b>');
  edgeShowNext();
}
function edgeShowNext() {
  clearChildren(canvas);
  if (edgeState.i >= edgeState.edges.length) { finishStage(); return; }
  const e = edgeState.edges[edgeState.i];
  const bar = document.createElement('div');
  bar.className = 'edge-bar armed';
  bar.style.left = e.x + 'px';
  bar.style.top  = e.y + 'px';
  bar.style.width  = e.w + 'px';
  bar.style.height = e.h + 'px';
  bar.textContent = e.id.toUpperCase();
  canvas.appendChild(bar);
  edgeState.active = { el: bar, expected: e, idx: edgeState.i };
  edgeState.timeout = setTimeout(() => {
    if (edgeState.active && edgeState.active.idx === edgeState.i) {
      bar.className = 'edge-bar missed';
      results.stages.edges.misses.push({ edge: e.id, reason: 'timeout' });
      results.stages.edges.fails++;
      results.edge_dead_strips.push({ edge: e.id, max_offset: 'timeout' });
      edgeState.active = null;
      edgeState.i++;
      setTimeout(edgeShowNext, 700);
    }
  }, 4500);
}
function edgesTap(x, y) {
  const a = edgeState.active;
  if (!a) return;
  const e = a.expected;
  if (x >= e.x - 8 && x <= e.x + e.w + 8 && y >= e.y - 8 && y <= e.y + e.h + 8) {
    a.el.className = 'edge-bar hit';
    results.stages.edges.passes++;
    results.stages.edges.hits.push({ x, y, edge: e.id });
    edgeState.active = null;
    if (edgeState.timeout) clearTimeout(edgeState.timeout);
    edgeState.i++;
    setTimeout(edgeShowNext, 500);
  } else {
    const cdx = x - (e.x + e.w/2);
    const cdy = y - (e.y + e.h/2);
    const dist = Math.hypot(cdx, cdy);
    if (dist > 30) results.pixel_drift_max = Math.max(results.pixel_drift_max, dist);
  }
}

// === STAGE: Drag ===
function dragEnter() {
  canvas.className = 'stage-canvas target';
  clearChildren(canvas);
  const r = rect();
  dragState.startX   = r.w * 0.2;
  dragState.startY   = r.h * 0.5;
  dragState.targetX  = r.w * 0.8;
  dragState.targetY  = r.h * 0.5;
  dragState.currentX = dragState.startX;
  dragState.currentY = dragState.startY;
  const start = document.createElement('div');
  start.id = 'dragStart';
  start.className = 'drag-handle start';
  const end = document.createElement('div');
  end.id = 'dragEnd';
  end.className = 'drag-handle end';
  canvas.appendChild(start);
  canvas.appendChild(end);
  dragMove(0, 0);
  showHint('Press the <b>green</b> circle and drag to the <b>yellow</b> circle');
}
function dragTap(x, y) {
  // intentionally no-op; drag uses pointerdown/up for state
}
function dragMove(x, y) {
  const s = document.getElementById('dragStart');
  const e = document.getElementById('dragEnd');
  if (s) { s.style.left = dragState.startX + 'px'; s.style.top = dragState.startY + 'px'; }
  if (e) { e.style.left = dragState.targetX + 'px'; e.style.top = dragState.targetY + 'px'; }
  const existing = document.getElementById('dragLine');
  if (existing) existing.remove();
  if (dragState.active) {
    const line = document.createElement('div');
    line.id = 'dragLine';
    line.className = 'drag-line';
    const dx = dragState.currentX - dragState.startX;
    const dy = dragState.currentY - dragState.startY;
    const len = Math.hypot(dx, dy);
    const angle = Math.atan2(dy, dx) * 180 / Math.PI;
    line.style.left = dragState.startX + 'px';
    line.style.top  = dragState.startY + 'px';
    line.style.width = len + 'px';
    line.style.transform = 'rotate(' + angle + 'deg)';
    canvas.appendChild(line);
  }
}

// === STAGE: Summary ===
function summaryEnter() {
  canvas.className = 'stage-canvas summary';
  clearChildren(canvas);
  hideHint();
  results.finished_at = new Date().toISOString();

  const totalPass = Object.values(results.stages).reduce((a, s) => a + (s.passes || 0), 0);
  const totalFail = Object.values(results.stages).reduce((a, s) => a + (s.fails || 0), 0);
  const hit_rate = totalPass + totalFail > 0 ? (totalPass / (totalPass + totalFail) * 100).toFixed(1) : '0';

  const cornerPass = (results.stages.corner?.passes || 0) >= 4;
  const gridPass   = (results.stages.grid?.passes || 0)   >= 20;
  const edgePass   = (results.stages.edges?.passes || 0)  >= 4;
  const dragPass   = (results.stages.drag?.passes || 0)   >= 1;
  const driftPass  = results.pixel_drift_max < 50;
  results.summary = (cornerPass && gridPass && edgePass && dragPass && driftPass) ? 'pass' : 'fail';

  const wrap = document.createElement('div');
  wrap.className = 'summary-wrap';
  const card = document.createElement('div');
  card.className = 'summary-card';
  card.innerHTML = `
    <h2 class="${results.summary}">${results.summary === 'pass' ? '✓ Calibration PASS' : '✗ Calibration FAIL'}</h2>
    <div class="summary-row"><span class="label">Corners</span><span class="value">${results.corners_hit}/${results.corners_total}</span></div>
    <div class="summary-row"><span class="label">Grid cells</span><span class="value">${results.stages.grid?.passes || 0}/${results.stages.grid?.total || 20}</span></div>
    <div class="summary-row"><span class="label">Edge strips</span><span class="value">${results.stages.edges?.passes || 0}/4</span></div>
    <div class="summary-row"><span class="label">Drag accuracy</span><span class="value">${(results.drag_max_distance || 0).toFixed(1)} px</span></div>
    <div class="summary-row"><span class="label">Pixel drift (max)</span><span class="value">${results.pixel_drift_max.toFixed(1)} px</span></div>
    <div class="summary-row"><span class="label">Hit rate</span><span class="value">${hit_rate}%</span></div>
    <div class="summary-row"><span class="label">Edge dead strips</span><span class="value">${results.edge_dead_strips.length ? results.edge_dead_strips.map(d => d.edge).join(', ') : 'none'}</span></div>
    ${results.summary === 'fail' ? '<ul class="failures" id="failList"></ul>' : ''}
    <div class="summary-actions">
      <button id="rerunBtn">Run again</button>
      <button id="sendBtn">Save result</button>
      <button class="primary" id="doneBtn">Done</button>
    </div>
  `;
  wrap.appendChild(card);
  canvas.appendChild(wrap);

  if (results.summary === 'fail') {
    const list = $('failList');
    [
      ['corner', results.stages.corner?.misses, m => m.expected?.label],
      ['grid',   results.stages.grid?.misses,   m => m.idx + 1],
      ['edges',  results.stages.edges?.misses,  m => m.edge],
    ].forEach(([stage, misses, fmt]) => {
      if (!misses) return;
      misses.forEach(m => {
        const li = document.createElement('li');
        li.innerHTML = `<b>${stage} ${fmt(m)}</b> ${m.reason}`;
        list.appendChild(li);
      });
    });
  }
  $('rerunBtn').addEventListener('click', () => {
    results.stages = {};
    results.pixel_drift_max = 0;
    results.corners_total = 0;
    results.corners_hit = 0;
    results.edge_dead_strips = [];
    results.drag_max_distance = 0;
    results.summary = 'pending';
    results.started_at = new Date().toISOString();
    results.finished_at = null;
    setStage(0);
  });
  $('sendBtn').addEventListener('click', saveResult);
  $('doneBtn').addEventListener('click', () => {
    if (window.opener) window.close();
    else window.location.href = '/';
  });
}

function finishStage() {
  hideHint();
  document.querySelectorAll('#progress .dot').forEach((d, i) => {
    if (i <= currentStage) {
      d.classList.add('done');
      d.classList.remove('active');
    }
  });
  setTimeout(() => {
    const next = currentStage + 1;
    if (next < STAGE_DEFS.length) setStage(next);
    else setStage(STAGE_DEFS.length - 1);
  }, 800);
}

async function saveResult() {
  const sendBtn = $('sendBtn');
  if (sendBtn) { sendBtn.disabled = true; sendBtn.textContent = 'Saving…'; }
  try {
    const r = await fetch('/api/touch-calibration.json', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(results),
    });
    if (!r.ok) throw new Error('HTTP ' + r.status);
    if (sendBtn) { sendBtn.textContent = '✓ Saved'; }
  } catch (e) {
    if (sendBtn) { sendBtn.textContent = '✗ Save failed'; sendBtn.disabled = false; }
  }
}

$('backBtn').addEventListener('click', () => {
  if (window.history.length > 1) window.history.back();
  else window.location.href = '/';
});

results.max_touch_points = (navigator.maxTouchPoints || 1);
results.user_agent = navigator.userAgent;
results.viewport = { w: window.innerWidth, h: window.innerHeight };

setStage(0);
</script>
</body>
</html>
PY_CALIB


cat > "$RENDERER" <<'PY_RENDERER'
#!/usr/bin/env python3
"""Screentra Cast cached player renderer.

Serves a local browser renderer from cached heartbeat config/layout files.
This lets a player continue displaying its assigned layout when the server or LAN is unavailable.
"""
import argparse
import json
import mimetypes
import os
import sys
import urllib.parse
import urllib.request
import socket
import ssl
import stat
import tempfile
import time
import hashlib
import hmac
import secrets
import base64
import threading
import selectors
import tarfile
import shutil
import signal
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

MAX_REQUEST_BODY_BYTES = 1 << 20
MAX_JSON_FILE_BYTES = 1 << 20
MAX_DATA_PROXY_BYTES = 4 * 1024 * 1024
MAX_BACKUP_DOWNLOAD_BYTES = 256 * 1024 * 1024
MAX_CACHED_ASSET_BYTES = 16 * 1024 * 1024
MAX_INPUT_DEVICE_FILE_BYTES = 256 * 1024
MAX_TOUCH_DEVICES = 128
MAX_DISPLAY_OUTPUTS = 32
MAX_TOUCH_NAME_BYTES = 128
MAX_SYSTEM_METRICS_BYTES = 64 * 1024
MAX_GPU_HELPER_OUTPUT_BYTES = 64 * 1024
MAX_ERROR_BYTES = 512
MAX_STATIC_HTML_BYTES = 2 * 1024 * 1024
CAMERA_STREAM_URL_BYTES = 4096
CAMERA_WIDGET_ID_BYTES = 128
CAMERA_STREAM_FPS = 15
WEATHER_ICON_SLUGS = frozenset({
    'clear-day', 'clear-night', 'mostly-clear-day', 'mostly-clear-night',
    'partly-cloudy-day', 'partly-cloudy-night', 'overcast-day', 'overcast-night',
    'fog-day', 'fog-night', 'overcast-day-fog', 'overcast-night-fog',
    'drizzle', 'extreme-drizzle', 'rain', 'extreme-rain', 'snow', 'extreme-snow',
    'partly-cloudy-day-rain', 'partly-cloudy-night-rain',
    'overcast-day-rain', 'overcast-night-rain', 'extreme-day-rain',
    'extreme-night-rain', 'thunderstorms-day', 'thunderstorms-night',
    'thunderstorms-day-hail', 'thunderstorms-night-hail',
    'extreme-thunderstorms-day', 'extreme-thunderstorms-night',
})


def _bound_text(value: str | None, maximum: int) -> str:
    if value is None:
        return ''
    encoded = str(value).encode('utf-8', errors='replace')
    return encoded[:maximum].decode('utf-8', errors='ignore')


def _read_bounded_path(path: Path, maximum: int) -> str:
    with path.open('rb') as handle:
        return handle.read(maximum).decode('utf-8', errors='replace')


def client_pairing_code(client_id):
    """Return the short physical-confirmation code for a client ID."""
    digest = hashlib.sha256(b'screentra-cast-pairing-v1\0' + str(client_id).encode('utf-8')).digest()
    return f'{int.from_bytes(digest[:4], "big") % 1_000_000:06d}'


def _ensure_real_directory(path: Path, create: bool = False) -> bool:
    absolute = Path(os.path.abspath(os.fspath(path)))
    current = Path(absolute.anchor)
    for part in absolute.parts[1:]:
        current /= part
        try:
            metadata = os.lstat(current)
        except FileNotFoundError:
            if not create:
                return False
            current.mkdir()
            metadata = os.lstat(current)
        if stat.S_ISLNK(metadata.st_mode) or not stat.S_ISDIR(metadata.st_mode):
            raise ValueError('renderer cache directory is not a real directory')
    return True


def _read_real_file(path: Path, maximum: int = MAX_JSON_FILE_BYTES) -> bytes:
    absolute = Path(os.path.abspath(os.fspath(path)))
    current = Path(absolute.anchor)
    for part in absolute.parts[1:]:
        current /= part
        metadata = os.lstat(current)
        if stat.S_ISLNK(metadata.st_mode):
            raise ValueError('renderer cache path contains a symlink')
    descriptor = os.open(absolute, os.O_RDONLY | getattr(os, 'O_NOFOLLOW', 0))
    try:
        metadata = os.fstat(descriptor)
        if not stat.S_ISREG(metadata.st_mode) or metadata.st_size > maximum:
            raise ValueError('renderer cache file is not bounded')
        raw = os.read(descriptor, maximum + 1)
    finally:
        os.close(descriptor)
    if len(raw) > maximum:
        raise ValueError('renderer cache file exceeds its bound')
    return raw


def _atomic_write_text(path: Path, text: str, mode: int = 0o600) -> None:
    _ensure_real_directory(path.parent, create=True)
    descriptor, temporary_name = tempfile.mkstemp(
        prefix=f'.{path.name}.', suffix='.tmp', dir=str(path.parent)
    )
    temporary = Path(temporary_name)
    try:
        with os.fdopen(descriptor, 'w', encoding='utf-8') as handle:
            handle.write(text)
        os.chmod(temporary, mode)
        os.replace(temporary, path)
        temporary = None
    finally:
        if temporary is not None:
            try:
                temporary.unlink()
            except FileNotFoundError:
                pass


def _read_json_file(path: Path, default):
    try:
        return _parse_request_json(_read_real_file(path).decode('utf-8'))
    except Exception:
        return default


def _reject_duplicate_json_keys(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError('duplicate JSON object key')
        result[key] = value
    return result


def _parse_request_json(text: str) -> dict:
    value = json.loads(
        text,
        object_pairs_hook=_reject_duplicate_json_keys,
        parse_constant=lambda _: (_ for _ in ()).throw(ValueError('non-standard JSON constant')),
    )
    if not isinstance(value, dict):
        raise ValueError('request JSON must be an object')
    return value


def _require_request_fields(payload: dict, allowed: set[str]) -> None:
    unknown = set(payload) - allowed
    if unknown:
        raise ValueError('request contains unsupported fields')


def _validate_login_payload(payload: dict) -> tuple[str, str]:
    username = payload.get('username')
    password = payload.get('password')
    if not isinstance(username, str) or not 1 <= len(username) <= 128:
        raise ValueError('username must be a bounded string')
    if not isinstance(password, str) or not 1 <= len(password) <= 256:
        raise ValueError('password must be a bounded string')
    return username, password


def _request_body(handler) -> str:
    raw_length = handler.headers.get('Content-Length')
    if raw_length is None:
        length = 0
    else:
        try:
            length = int(raw_length)
        except (TypeError, ValueError):
            raise ValueError('invalid Content-Length')
    if length < 0:
        raise ValueError('invalid Content-Length')
    if length > MAX_REQUEST_BODY_BYTES:
        raise OverflowError('request body too large')
    return handler.rfile.read(length).decode('utf-8') if length else '{}'


# === no-layout & touchscreen endpoints =====================================
import urllib.request as _ur
import subprocess as _sp
from datetime import datetime as _dt, timezone as _tz


def _iso_now() -> str:
    return _dt.now(_tz.utc).isoformat()

# Linux input.h ABS_* bit numbers. The EV_ABS bitmask in /proc/bus/input/devices
# (B: ABS=...) is a hex bitmask, so we test these by reading the bit at position
# N. These are the canonical "this is a touchscreen / multi-touch surface" bits.
_ABS_MT_POSITION_X = 53
_ABS_MT_POSITION_Y = 54
_ABS_MT_SLOT       = 47
_ABS_MT_TOUCH_MAJOR = 48
_ABS_X = 0
_ABS_Y = 1


def _udev_has_touchscreen_property(dev_path: str) -> bool:
    """Return True if udev marks dev_path as a touchscreen.

    udev sets ID_INPUT_TOUCHSCREEN=1 via libinput for any device that
    libinput classifies as a touchscreen. This is the most reliable
    detection: it works for HID-class, USB-class, and I2C-HID devices
    regardless of the device name.
    """
    try:
        completed = _sp.run(
            ['udevadm', 'info', '--query=property', '--name=' + dev_path],
            stdout=_sp.PIPE, stderr=_sp.DEVNULL, timeout=2, text=True, check=False,
        )
        out = _bound_text(completed.stdout or '', MAX_INPUT_DEVICE_FILE_BYTES)
        for line in out.splitlines():
            if line.startswith('ID_INPUT_TOUCHSCREEN=1'):
                return True
    except Exception:
        return False
    return False


def _is_touch_bitmask(abs_bits: str) -> bool:
    """True if the ABS= hex bitmask indicates a touchscreen."""
    try:
        bits = int(abs_bits, 16)
    except (TypeError, ValueError):
        return False
    has_mt = (
        (bits >> _ABS_MT_POSITION_X) & 1
        and (bits >> _ABS_MT_POSITION_Y) & 1
        and (bits >> _ABS_MT_SLOT) & 1
    )
    has_single = (
        (bits >> _ABS_X) & 1
        and (bits >> _ABS_Y) & 1
    )
    return has_mt or has_single


def _is_touch_eligible(d: dict) -> bool:
    """Combine multiple signals to decide whether a device is a touchscreen.

    In order of trust:
      1. udev's ID_INPUT_TOUCHSCREEN — set by libinput, the project standard.
      2. ABS bitmask contains ABS_MT_POSITION_X/Y/_SLOT (multi-touch surface).
      3. The kernel name contains "Touch" or "touch" (some vendors put it in name).
    Each handler event path is checked independently so a multi-function device
    (e.g. USB audio + touch) is offered correctly.
    """
    hs = (d.get('handlers') or '').split()
    absbits = d.get('abs') or ''
    name = d.get('name') or ''
    has_touch_name = 'Touch' in name or 'touch' in name.lower()
    has_touch_abs  = _is_touch_bitmask(absbits)
    for h in hs:
        if not h.startswith('event'):
            continue
        if _udev_has_touchscreen_property('/dev/input/' + h):
            return True
        if has_touch_abs or has_touch_name:
            return True
    return False


def _nl_detect_touch_devices():
    devs = []
    p = Path('/proc/bus/input/devices')
    if not p.exists():
        return devs
    try:
        raw_devices = _read_real_file(p, MAX_INPUT_DEVICE_FILE_BYTES).decode('utf-8', errors='replace')
    except Exception:
        return devs
    cur = {}
    for line in raw_devices.splitlines():
        if not line.strip():
            if cur:
                devs.append(cur); cur = {}
            continue
        if line.startswith('N:'):
            cur['name'] = _bound_text(line.split('N:', 1)[1].strip().lstrip('Name=').strip('"'), MAX_TOUCH_NAME_BYTES)
        elif line.startswith('H:'):
            cur.setdefault('handlers', '')
            cur['handlers'] += ' ' + line.split('H:', 1)[1].strip()
        elif line.startswith('B:') and 'EV=' in line:
            try:
                cur['ev'] = line.split('B:EV=', 1)[1].split()[0]
            except Exception:
                cur['ev'] = ''
        elif line.startswith('B:') and 'ABS=' in line:
            try:
                cur['abs'] = line.split('B:ABS=', 1)[1].split()[0]
            except Exception:
                cur['abs'] = ''
    if cur:
        devs.append(cur)
    out = []
    for d in devs:
        hs = (d.get('handlers') or '').split()
        absbits = d.get('abs') or ''
        ev = d.get('ev') or ''
        name = d.get('name') or '?'
        if not hs or not _is_touch_eligible(d):
            continue
        for h in hs:
            if not h.startswith('event'):
                continue
            dev_path = '/dev/input/' + h
            recommended = _udev_has_touchscreen_property(dev_path)
            reason = 'udev ID_INPUT_TOUCHSCREEN=1'
            if not recommended and (has_touch_abs := _is_touch_bitmask(absbits)):
                recommended = True
                reason = 'ABS_MT_POSITION_X/Y/_SLOT multi-touch surface'
            elif not recommended and ('Touch' in name or 'touch' in name.lower()):
                recommended = True
                reason = 'name contains "Touch"'
            out.append({
                'name': name,
                'path': dev_path,
                'type': 'touchscreen' if recommended else 'input',
                'recommended': recommended,
                'reason': reason,
            })
            if len(out) >= MAX_TOUCH_DEVICES:
                return out
    out.sort(key=lambda x: (not x['recommended'], x['name']))
    return out

def _lan_ip_for_admin(server_url: str | None) -> str | None:
    try:
        parsed = urllib.parse.urlparse(server_url or '')
        host = parsed.hostname
        port = parsed.port or (443 if parsed.scheme == 'https' else 3000)
        if host:
            # Prefer a UDP route lookup; it does not send packets but asks the
            # kernel which local source IP would reach the configured server.
            with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as sock:
                sock.settimeout(0.5)
                sock.connect((host, port))
                ip = sock.getsockname()[0]
                if ip and not ip.startswith('127.'):
                    return ip
    except Exception:
        pass
    try:
        completed = _sp.run(['hostname', '-I'], stdout=_sp.PIPE, stderr=_sp.DEVNULL, text=True, timeout=2, check=False)
        out = _bound_text(completed.stdout or '', 4096).split()
        for ip in out:
            if ip and not ip.startswith('127.') and ':' not in ip:
                return ip
    except Exception:
        pass
    return None

def _nl_status():
    cid_path = Path.home() / '.local/share/screentra-client/client-id'
    srv_path = Path.home() / '.local/share/screentra-client/server'
    try:
        client_id = _read_real_file(cid_path, 4096).decode('utf-8').strip() if cid_path.exists() else None
    except Exception:
        client_id = None
    try:
        server_url = _read_real_file(srv_path, 4096).decode('utf-8').strip() if srv_path.exists() else 'unknown'
    except Exception:
        server_url = 'unknown'
    last_path = Handler.cache_dir / 'last-heartbeat.json'
    last_seen = connection = last_layout = None
    if last_path.exists():
        try:
            last = _read_json_file(last_path, {})
            last_seen = last.get('at')
            connection = last.get('connection', 'offline')
            last_layout = last.get('assigned_layout_id')
        except Exception:
            pass
    disp_path = Handler.cache_dir / f'display-{int(os.environ.get("RIPLEY_DISPLAY_INDEX", "0"))}.json'
    label = connector = resolution = None
    if disp_path.exists():
        try:
            d = _read_json_file(disp_path, {})
            label = d.get('label') or d.get('connector')
            connector = d.get('connector')
            w = d.get('width'); h = d.get('height')
            if w and h:
                resolution = f'{w}\u00d7{h}'
        except Exception:
            pass
    touch_pref = Handler.cache_dir / 'touch-device.json'
    selected = None
    if touch_pref.exists():
        try:
            selected = _read_json_file(touch_pref, {}).get('path')
        except Exception:
            pass
    devs = _nl_detect_touch_devices()
    touch_dev = None
    for d in devs:
        if d['path'] == selected:
            touch_dev = d['name']; break
    if not touch_dev and devs:
        touch_dev = devs[0]['name']
    display_index_for_layout = int(os.environ.get('RIPLEY_DISPLAY_INDEX', '0'))
    layout_status_file = Handler.cache_dir / f'assigned-layout-display-{display_index_for_layout}.json'
    if display_index_for_layout == 0 and not layout_status_file.exists():
        layout_status_file = Handler.cache_dir / 'assigned-layout.json'
    if layout_status_file.exists():
        try:
            last_layout = _read_json_file(layout_status_file, {}).get('id')
        except Exception:
            pass
    else:
        last_layout = None
    lan_ip = _lan_ip_for_admin(server_url)
    return {
        'screentra_client_client': True,
        'client_installed': True,
        'client_id': client_id,
        'identity_source': 'persistent_client_id',
        'server_url': server_url,
        'hostname': socket.gethostname() if 'socket' in dir() else os.uname().nodename,
        'lan_ip': lan_ip,
        'admin_port': 3910,
        'admin_url': ('http://' + lan_ip + ':3910/') if lan_ip else None,
        'role': 'player',
        'port': int(os.environ.get('RIPLEY_RENDER_PORT', str(int(os.environ.get('RIPLEY_RENDER_BASE_PORT','3900')) + int(os.environ.get('RIPLEY_DISPLAY_INDEX','0'))))),
        'display_index': int(os.environ.get('RIPLEY_DISPLAY_INDEX', '0')),
        'display_label': label,
        'display_connector': connector,
        'display_resolution': resolution,
        'last_seen': last_seen,
        'connection': connection or 'offline',
        'assigned_layout_id': last_layout,
        'touch_device': touch_dev,
        'touch_path': selected,
        'build': os.environ.get('RIPLEY_BUILD', 'dev'),
    }


# === local admin / easy-setup page ===========================================

def _bounded_run(args, timeout=5, stdout_max=12000, stderr_max=4000, env=None):
    process = _sp.Popen(args, stdout=_sp.PIPE, stderr=_sp.PIPE, env=env)
    selector = selectors.DefaultSelector()
    assert process.stdout is not None and process.stderr is not None
    selector.register(process.stdout, selectors.EVENT_READ, ('stdout', stdout_max))
    selector.register(process.stderr, selectors.EVENT_READ, ('stderr', stderr_max))
    buffers = {'stdout': bytearray(), 'stderr': bytearray()}
    deadline = time.monotonic() + timeout
    try:
        while selector.get_map():
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise _sp.TimeoutExpired(args, timeout)
            for key, _ in selector.select(remaining):
                stream_name, maximum = key.data
                chunk = os.read(key.fileobj.fileno(), maximum + 1)
                if not chunk:
                    selector.unregister(key.fileobj)
                    key.fileobj.close()
                    continue
                buffers[stream_name].extend(chunk)
                if len(buffers[stream_name]) > maximum:
                    raise ValueError(f'{stream_name} exceeds its bound')
        returncode = process.wait(timeout=max(deadline - time.monotonic(), 0.001))
        return returncode, {
            name: bytes(value).decode('utf-8', errors='replace')
            for name, value in buffers.items()
        }
    except BaseException:
        if process.poll() is None:
            process.kill()
        try:
            process.wait(timeout=1)
        except Exception:
            pass
        raise
    finally:
        selector.close()
        for stream in (process.stdin, process.stdout, process.stderr):
            if stream is not None:
                try:
                    stream.close()
                except Exception:
                    pass


def _safe_run(args, timeout=5, env=None):
    try:
        code, output = _bounded_run(args, timeout=timeout, env=env)
        return {'ok': code == 0, 'code': code, 'stdout': output['stdout'], 'stderr': output['stderr']}
    except Exception as e:
        return {'ok': False, 'code': None, 'stdout': '', 'stderr': _bound_text(str(e), MAX_ERROR_BYTES)}

def _client_config_path() -> Path:
    return Handler.cache_dir.parent / 'config.json'

def _load_display_governance() -> dict:
    cfg = {'installer_detected_max_displays': 1, 'max_displays': None, 'last_detected_displays': []}
    try:
        path = _client_config_path()
        if path.exists():
            data = _read_json_file(path, {})
            if isinstance(data, dict):
                cfg.update({k: data.get(k) for k in cfg if k in data})
    except Exception as e:
        cfg['error'] = _bound_text(str(e), MAX_ERROR_BYTES)
    try:
        installer = max(int(cfg.get('installer_detected_max_displays') or 1), 1)
    except Exception:
        installer = 1
    override = cfg.get('max_displays')
    try:
        active = min(installer, max(int(override), 1)) if override is not None else installer
    except Exception:
        active = installer
    cfg['installer_detected_max_displays'] = installer
    cfg['active_max_displays'] = max(1, min(active, 4))
    cfg['config_path'] = str(_client_config_path())
    return cfg

def _load_client_config() -> dict:
    path = _client_config_path()
    try:
        if path.exists():
            data = _read_json_file(path, {})
            if isinstance(data, dict):
                return data
    except Exception:
        pass
    return {}

def _write_client_config(cfg: dict) -> None:
    path = _client_config_path()
    _ensure_real_directory(path.parent, create=True)
    _atomic_write_text(path, json.dumps(cfg, indent=2))

def _write_display_count(max_displays) -> dict:
    cfg = _load_client_config()
    installer = max(int(cfg.get('installer_detected_max_displays') or 1), 1)
    override = None if max_displays is None else max(1, min(int(max_displays), installer, 4))
    cfg['installer_detected_max_displays'] = installer
    cfg['max_displays'] = override
    cfg.setdefault('last_detected_displays', [])
    _write_client_config(cfg)
    pending = Handler.cache_dir / 'pending-command.json'
    _ensure_real_directory(pending.parent, create=True)
    _atomic_write_text(pending, json.dumps({'action':'set_display_count','payload':{'max_displays': override},'requested_at':_iso_now(),'requested_by':'local-admin'}, indent=2))
    _safe_run(['systemctl', '--user', 'restart', 'screentra-client-heartbeat.service'], timeout=6)
    actor = _safe_run(['systemctl', '--user', 'start', 'screentra-client-actor.service'], timeout=6)
    gov = _load_display_governance()
    return {'ok': True, 'max_displays': override, 'active_max_displays': gov['active_max_displays'], 'installer_detected_max_displays': installer, 'actor_start': actor}

def _resolve_touch_name(device_path: str | None, device_name: str | None = None) -> str | None:
    if device_name:
        return str(device_name)
    for dev in _nl_detect_touch_devices():
        if dev.get('path') == device_path:
            return dev.get('name')
    return None

def _display_connectors_for_indexes(indexes: list[int]) -> list[str]:
    cfg = _load_client_config()
    displays = cfg.get('last_detected_displays') or []
    out = []
    for idx in indexes:
        for d in displays:
            try:
                if int(d.get('display_index', -1)) == int(idx):
                    c = d.get('connector') or d.get('label')
                    if c and c not in out:
                        out.append(str(c))
                    break
            except Exception:
                continue
    if not out:
        wlr = _wlr_outputs()
        wlr.sort(key=lambda o: o.get('name') or '')
        for idx in indexes:
            if 0 <= idx < len(wlr):
                c = wlr[idx].get('name')
                if c and c not in out:
                    out.append(str(c))
    return out

def _current_touch_binding() -> dict:
    cfg = _load_client_config()
    binding = cfg.get('touch_binding') if isinstance(cfg.get('touch_binding'), dict) else {}
    legacy = Handler.cache_dir / 'touch-device.json'
    if not binding and legacy.exists():
        try:
            path = _read_json_file(legacy, {}).get('path')
            if path:
                binding = {'device_path': path, 'device_name': _resolve_touch_name(path), 'target_display_indexes': [0], 'target_connectors': _display_connectors_for_indexes([0]), 'allow_joined_displays': False}
        except Exception:
            pass
    return binding or {'device_path': None, 'device_name': None, 'target_display_indexes': [0], 'target_connectors': _display_connectors_for_indexes([0]), 'allow_joined_displays': False}

def _validate_touch_device_path(dev_path: str | None) -> None:
    if dev_path is None:
        return
    if not isinstance(dev_path, str) or len(dev_path) > 256 or not dev_path.startswith('/dev/'):
        raise ValueError('touch device path is invalid')
    detected_paths = {
        device.get('path') for device in _nl_detect_touch_devices()
        if isinstance(device, dict) and isinstance(device.get('path'), str)
    }
    if dev_path not in detected_paths:
        raise ValueError('touch device path is not a detected touchscreen')


def _write_touch_binding(payload: dict) -> dict:
    cfg = _load_client_config()
    dev_path = payload.get('device_path') or payload.get('path')
    _validate_touch_device_path(dev_path)
    dev_name = _resolve_touch_name(dev_path)
    allow_joined = bool(payload.get('allow_joined_displays'))
    raw_indexes = payload.get('target_display_indexes') or payload.get('display_indexes') or payload.get('display_index') or [0]
    if not isinstance(raw_indexes, list):
        raw_indexes = [raw_indexes]
    indexes = []
    for v in raw_indexes:
        try:
            i = max(0, min(int(v), 3))
            if i not in indexes:
                indexes.append(i)
        except Exception:
            pass
    if not indexes:
        indexes = [0]
    if not allow_joined:
        indexes = indexes[:1]
    else:
        indexes = indexes[:2]
    binding = {
        'device_path': dev_path,
        'device_name': dev_name,
        'target_display_indexes': indexes,
        'target_connectors': _display_connectors_for_indexes(indexes),
        'allow_joined_displays': allow_joined,
        'updated_at': _iso_now(),
        'updated_by': payload.get('requested_by') or 'local-admin',
    }
    cfg['touch_binding'] = binding
    _write_client_config(cfg)
    if dev_path:
        _atomic_write_text(Handler.cache_dir / 'touch-device.json', json.dumps({'path': dev_path, 'name': dev_name}, indent=2))
    pending = Handler.cache_dir / 'pending-command.json'
    _ensure_real_directory(pending.parent, create=True)
    _atomic_write_text(pending, json.dumps({'action':'set_touch_binding','payload': binding,'requested_at':_iso_now(),'requested_by':'local-admin'}, indent=2))
    _safe_run(['systemctl', '--user', 'restart', 'screentra-client-heartbeat.service'], timeout=6)
    actor = _safe_run(['systemctl', '--user', 'start', 'screentra-client-actor.service'], timeout=6)
    return {'ok': True, 'touch_binding': binding, 'actor_start': actor}

def _service_state(name: str) -> dict:
    active = _safe_run(['systemctl', '--user', 'is-active', name], timeout=2)
    enabled = _safe_run(['systemctl', '--user', 'is-enabled', name], timeout=2)
    return {'name': name, 'active': (active['stdout'] or active['stderr']).strip(), 'enabled': (enabled['stdout'] or enabled['stderr']).strip(), 'active_ok': active['ok']}

def _system_service_state(name: str) -> dict:
    active = _safe_run(['sudo', '-n', 'systemctl', 'is-active', name], timeout=2)
    enabled = _safe_run(['sudo', '-n', 'systemctl', 'is-enabled', name], timeout=2)
    return {'name': name, 'active': (active['stdout'] or active['stderr']).strip(), 'enabled': (enabled['stdout'] or enabled['stderr']).strip(), 'active_ok': active['ok']}

def _sysfs_outputs() -> list[dict]:
    out = []
    root = Path('/sys/class/drm')
    try:
        for d in sorted(root.iterdir()):
            if '-' not in d.name:
                continue
            status = d / 'status'
            if not status.exists():
                continue
            state = _read_real_file(status, 64).decode('utf-8', errors='replace').strip()
            if state != 'connected':
                continue
            name = d.name.split('-', 1)[1]
            mode = None
            modes = d / 'modes'
            if modes.exists():
                first = (_read_real_file(modes, 4096).decode('utf-8', errors='replace').splitlines() or [''])[0]
                if 'x' in first:
                    try:
                        w, h = first.split('x', 1)[0], first.split('x', 1)[1].split()[0]
                        mode = {'width': int(w), 'height': int(h), 'source': 'sysfs'}
                    except Exception:
                        mode = {'label': first, 'source': 'sysfs'}
            out.append({'name': name, 'description': name, 'enabled': None, 'connected': True, 'position': {}, 'current_mode': mode, 'source': 'sysfs'})
            if len(out) >= MAX_DISPLAY_OUTPUTS:
                break
    except Exception:
        return []
    return out

def _wlr_outputs() -> list[dict]:
    env = {**os.environ, 'XDG_RUNTIME_DIR': os.environ.get('XDG_RUNTIME_DIR', f'/run/user/{os.getuid()}'), 'WAYLAND_DISPLAY': os.environ.get('WAYLAND_DISPLAY', 'wayland-0')}
    try:
        completed = _safe_run(['wlr-randr', '--json'], timeout=3, env=env)
        if completed.get('code') != 0:
            raise ValueError('wlr-randr failed')
        data = json.loads(
            _bound_text(completed.get('stdout') or '', MAX_JSON_FILE_BYTES),
            parse_constant=lambda _: (_ for _ in ()).throw(ValueError('non-standard JSON constant')),
        )
        if not isinstance(data, list):
            raise ValueError('wlr-randr output is not a list')
        return [{'name': _bound_text(o.get('name'), 128), 'description': _bound_text(o.get('description'), 256), 'enabled': bool(o.get('enabled')), 'position': o.get('position') or {}, 'current_mode': next((m for m in o.get('modes', []) if m.get('current')), None)} for o in data[:MAX_DISPLAY_OUTPUTS] if isinstance(o, dict)]
    except Exception:
        return _sysfs_outputs()

def _read_tail(path: Path, max_bytes: int = 40000) -> str:
    try:
        data = _read_real_file(path, maximum=max(max_bytes, 4 * 1024 * 1024))
        return data.decode('utf-8', 'replace')[-max_bytes:]
    except Exception as e:
        return f'error reading {path}: {e}'

def _journal_tail(unit: str, lines: int = 160, system: bool = False) -> str:
    cmd = ['journalctl', '--no-pager', '-n', str(lines), '-u', unit]
    if system: cmd = ['sudo', '-n'] + cmd
    else: cmd.insert(1, '--user')
    r = _safe_run(cmd, timeout=6)
    return (r.get('stdout') or r.get('stderr') or '')[-40000:]

def _admin_logs(target: str) -> dict:
    install = Path.home() / '.local/share/screentra-client'
    state = Path.home() / '.local/state/screentra-client'
    cache = install / 'cache'
    targets = {
        'heartbeat': lambda: _journal_tail('screentra-client-heartbeat.service'),
        'actor': lambda: _journal_tail('screentra-client-actor.service'),
        'renderer0': lambda: _journal_tail('screentra-client-renderer@0.service'),
        'kiosk0': lambda: _journal_tail('screentra-cast-kiosk-kiosk@0.service', system=True),
        'launcher': lambda: _read_tail(state / 'kiosk-launcher.log'),
        'touch': lambda: _read_tail(Path('/tmp/ripley-touch-events.log')),
        'install': lambda: '\n\n'.join(_read_tail(p) for p in sorted(Path('/tmp').glob('player-client-*-install.log'))[-4:]),
        'heartbeat-cache': lambda: _read_tail(cache / 'last-heartbeat.json'),
    }
    if target == 'all': return {'target': target, 'logs': {k: fn() for k, fn in targets.items()}}
    fn = targets.get(target)
    if not fn: return {'target': target, 'error': 'unknown log target', 'valid_targets': sorted(targets)}
    return {'target': target, 'text': fn()}

def _ca_file() -> Path:
    env = os.environ.get('RIPLEY_CA_CERT')
    return Path(env) if env else Path.home() / '.local/share/screentra-client/screentra-cast-signage-ca.crt'

def _sha256_file(path: Path) -> str | None:
    try:
        return hashlib.sha256(_read_real_file(path, maximum=256 * 1024)).hexdigest()
    except Exception:
        return None

def _cert_info() -> dict:
    srv_path = Path.home() / '.local/share/screentra-client/server'
    try:
        server_url = _read_real_file(srv_path, 4096).decode('utf-8').strip().rstrip('/')
    except Exception:
        server_url = ''
    ca = _ca_file()
    try:
        ca_data = _read_real_file(ca, maximum=256 * 1024)
    except Exception:
        ca_data = b''
    ca_exists = bool(ca_data)
    verify = {'ok': False, 'error': 'server or CA missing'}
    parsed = urllib.parse.urlparse(server_url) if server_url else None
    if parsed and parsed.hostname and ca_exists:
        port = parsed.port or (443 if parsed.scheme == 'https' else 80)
        try:
            if parsed.scheme == 'https':
                ctx = ssl.create_default_context(cadata=ca_data.decode('utf-8'))
                with socket.create_connection((parsed.hostname, port), timeout=5) as sock:
                    with ctx.wrap_socket(sock, server_hostname=parsed.hostname) as ssock:
                        cert = ssock.getpeercert()
                verify = {'ok': True, 'host': parsed.hostname, 'port': port, 'subject': cert.get('subject'), 'issuer': cert.get('issuer'), 'not_after': cert.get('notAfter'), 'san': cert.get('subjectAltName')}
            else:
                verify = {'ok': True, 'warning': 'server URL is not HTTPS'}
        except Exception as e:
            verify = {'ok': False, 'error': _bound_text(str(e), MAX_ERROR_BYTES), 'host': parsed.hostname, 'port': port}
    return {'server_url': server_url, 'ca_path': str(ca), 'ca_exists': ca_exists, 'ca_size': len(ca_data), 'ca_sha256': _sha256_file(ca), 'verify': verify, 'published_ca_url': (server_url + '/install/screentra-cast-signage-ca.crt') if server_url else ''}

def _refresh_ca_from_server() -> dict:
    srv_path = Path.home() / '.local/share/screentra-client/server'
    try:
        server_url = _read_real_file(srv_path, 4096).decode('utf-8').strip().rstrip('/')
    except Exception:
        return {'ok': False, 'error': 'server file missing'}
    if not server_url: return {'ok': False, 'error': 'server URL empty'}
    url = server_url + '/install/screentra-cast-signage-ca.crt'; out = _ca_file()
    try:
        ca_path = _ca_file()
        ca_data = _read_real_file(ca_path, maximum=256 * 1024)
        ctx = _ssl.create_default_context(cadata=ca_data.decode('utf-8'))
        with _ur.urlopen(url, context=ctx, timeout=10) as response:
            data = response.read(256 * 1024 + 1)
        if len(data) > 256 * 1024:
            return {'ok': False, 'error': 'downloaded CA exceeds the client limit', 'url': url}
        if len(data) < 200 or b'BEGIN CERTIFICATE' not in data:
            return {'ok': False, 'error': f'downloaded CA did not look like a certificate ({len(data)} bytes)', 'url': url}
        _ensure_real_directory(out.parent, create=True)
        _atomic_write_text(out, data.decode('utf-8'), mode=0o644)
        verify = _cert_info().get('verify')
        restart = _safe_run(['systemctl', '--user', 'restart', 'screentra-client-heartbeat.service', 'screentra-client-actor.service'], timeout=8)
        return {'ok': True, 'url': url, 'ca_path': str(out), 'bytes': len(data), 'sha256': _sha256_file(out), 'verify': verify, 'restart': restart}
    except Exception as e:
        return {'ok': False, 'error': _bound_text(str(e), MAX_ERROR_BYTES), 'url': url}



def _read_proc_stat_cpu() -> tuple[int, int] | None:
    try:
        parts = _read_bounded_path(Path('/proc/stat'), MAX_SYSTEM_METRICS_BYTES).splitlines()[0].split()[1:]
        vals = [int(x) for x in parts]
        idle = vals[3] + (vals[4] if len(vals) > 4 else 0)
        total = sum(vals)
        return total, idle
    except Exception:
        return None

def _cpu_usage_percent(sample_secs: float = 0.12) -> float | None:
    a = _read_proc_stat_cpu()
    if not a:
        return None
    time.sleep(sample_secs)
    b = _read_proc_stat_cpu()
    if not b:
        return None
    total_delta = b[0] - a[0]
    idle_delta = b[1] - a[1]
    if total_delta <= 0:
        return None
    return round(max(0.0, min(100.0, (1.0 - (idle_delta / total_delta)) * 100.0)), 1)

def _cpu_temp_c() -> float | None:
    candidates = []
    try:
        for z in Path('/sys/class/thermal').glob('thermal_zone*/temp'):
            typ = _read_bounded_path(z.parent.joinpath('type'), 256).strip().lower() if z.parent.joinpath('type').exists() else ''
            score = 0 if any(k in typ for k in ('cpu', 'x86_pkg_temp', 'k10temp', 'tctl', 'soc')) else 1
            candidates.append((score, z))
    except Exception:
        pass
    try:
        for z in Path('/sys/class/hwmon').glob('hwmon*/temp*_input'):
            label = ''
            label_path = z.with_name(z.name.replace('_input', '_label'))
            if label_path.exists():
                label = _read_bounded_path(label_path, 256).strip().lower()
            score = 0 if any(k in label for k in ('cpu', 'tctl', 'tdie', 'package', 'soc')) else 2
            candidates.append((score, z))
    except Exception:
        pass
    for _, path in sorted(candidates, key=lambda x: x[0]):
        try:
            raw = int(_read_bounded_path(path, 128).strip())
            c = raw / 1000.0 if raw > 1000 else float(raw)
            if -20.0 < c < 130.0:
                return round(c, 1)
        except Exception:
            continue
    return None

def _ram_usage() -> dict:
    info = {}
    try:
        for line in _read_bounded_path(Path('/proc/meminfo'), MAX_SYSTEM_METRICS_BYTES).splitlines():
            k, v = line.split(':', 1)
            info[k] = int(v.strip().split()[0]) * 1024
        total = info.get('MemTotal') or 0
        avail = info.get('MemAvailable') or 0
        used = max(0, total - avail)
        pct = round((used / total) * 100.0, 1) if total else None
        return {'total_bytes': total, 'used_bytes': used, 'available_bytes': avail, 'percent': pct}
    except Exception as e:
        return {'error': _bound_text(str(e), MAX_ERROR_BYTES)}

def _gpu_usage() -> dict:
    helper = Path.home() / '.local/share/screentra-client/ripley-gpu-health.py'
    if helper.exists():
        try:
            r = _sp.run([sys.executable, str(helper)], stdout=_sp.PIPE, stderr=_sp.PIPE, text=True, timeout=4, check=False)
            helper_stdout = _bound_text(r.stdout or '', MAX_GPU_HELPER_OUTPUT_BYTES)
            helper_stderr = _bound_text(r.stderr or '', MAX_GPU_HELPER_OUTPUT_BYTES)
            if helper_stdout.strip():
                data = json.loads(
                    helper_stdout.strip().splitlines()[-1],
                    parse_constant=lambda _: (_ for _ in ()).throw(ValueError('non-standard JSON constant')),
                )
                if helper_stderr.strip():
                    data.setdefault('stderr', helper_stderr.strip()[-500:])
                return data
            if helper_stderr.strip():
                return {'percent': None, 'device': None, 'source': str(helper), 'error': helper_stderr.strip()[-500:]}
        except Exception as e:
            return {'percent': None, 'device': None, 'source': str(helper), 'error': _bound_text(str(e), MAX_ERROR_BYTES)}
    # Fallback for partial/upgrading installs: generic DRM busy percent.
    out = {'percent': None, 'device': None, 'source': None, 'available': False, 'health': 'unknown', 'health_notes': ['GPU helper not installed yet']}
    try:
        for busy in sorted(Path('/sys/class/drm').glob('card*/device/gpu_busy_percent')):
            raw = _read_bounded_path(busy, 128).strip()
            out.update({'percent': float(raw), 'device': busy.parent.parent.name, 'source': str(busy), 'available': True, 'method': 'drm-gpu_busy_percent', 'health': 'ok'})
            return out
    except Exception as e:
        out['error'] = _bound_text(str(e), MAX_ERROR_BYTES)
    return out

def _system_metrics() -> dict:
    load = None
    try:
        load = os.getloadavg()
    except Exception:
        pass
    return {
        'cpu': {'percent': _cpu_usage_percent(), 'temp_c': _cpu_temp_c(), 'loadavg': load},
        'ram': _ram_usage(),
        'gpu': _gpu_usage(),
    }

def _client_backup_root() -> Path:
    p = Handler.cache_dir.parent / 'backups'
    _ensure_real_directory(p, create=True)
    return p

def _safe_tar_add(tar, path: Path, arcname: str, *, exclude_state: bool = True) -> int:
    """Add path to tar while excluding local identity/state/secrets.

    This is a software/config-shape backup, not a client clone. It intentionally
    skips cache/layouts, client_id, server URL, CA/certs/keys, auth sessions, and
    previously generated backups, and Chromium profiles so restoring it cannot impersonate the old box
    or overwrite target-server trust material.
    """
    added = 0
    deny_names = {'cache', 'admin-auth', 'backups', 'client-id', 'server', 'config.json'}
    deny_suffixes = ('.crt', '.key', '.pem', '.p12', '.pfx')
    try:
        if not path.exists():
            return 0
        if path.is_file():
            if exclude_state and (path.name in deny_names or path.name.endswith(deny_suffixes)):
                return 0
            tar.add(path, arcname=arcname, recursive=False)
            return 1
        for child in sorted(path.rglob('*')):
            rel = child.relative_to(path)
            parts = set(rel.parts)
            if exclude_state and (parts & deny_names):
                continue
            if exclude_state and any(str(part).startswith('chromium-profile') for part in rel.parts):
                continue
            if exclude_state and child.name.endswith(deny_suffixes):
                continue
            if child.is_file() or child.is_symlink():
                tar.add(child, arcname=str(Path(arcname) / rel), recursive=False)
                added += 1
    except Exception:
        return added
    return added

def _create_client_software_backup() -> dict:
    root = _client_backup_root()
    ts = _dt.now(_tz.utc).strftime('%Y%m%dT%H%M%SZ')
    host = socket.gethostname().replace('/', '-').replace(' ', '-')
    name = f'screentra-client-software-{host}-{ts}.tar.gz'
    out = root / name
    install = Path.home() / '.local/share/screentra-client'
    bindir = Path.home() / '.local/bin'
    user_units = Path.home() / '.config/systemd/user'
    manifest = {
        'created_at': _iso_now(),
        'hostname': socket.gethostname(),
        'kind': 'screentra-client-software-backup',
        'excluded_state': ['cache/', 'admin-auth/', 'backups/', 'chromium-profile*/', 'client-id', 'server', 'config.json', '*.crt', '*.key', '*.pem', '*.p12', '*.pfx'],
        'paths': [],
    }
    try:
        with tarfile.open(out, 'w:gz') as tar:
            n = _safe_tar_add(tar, install, 'screentra-client', exclude_state=True)
            manifest['paths'].append({'source': str(install), 'archive': 'screentra-client', 'files': n})
            helper_count = 0
            for helper in sorted(bindir.glob('ripley-*')):
                helper_count += _safe_tar_add(tar, helper, str(Path('local-bin') / helper.name), exclude_state=True)
            manifest['paths'].append({'source': str(bindir / 'ripley-*'), 'archive': 'local-bin/', 'files': helper_count})
            unit_count = 0
            for unit in sorted(user_units.glob('ripley-*.service')):
                unit_count += _safe_tar_add(tar, unit, str(Path('systemd-user') / unit.name), exclude_state=True)
            manifest['paths'].append({'source': str(user_units / 'ripley-*.service'), 'archive': 'systemd-user/', 'files': unit_count})
            for sys_unit in [Path('/etc/systemd/system/screentra-cast-kiosk-kiosk@.service')]:
                if sys_unit.exists():
                    manifest['paths'].append({'source': str(sys_unit), 'archive': str(Path('systemd-system') / sys_unit.name), 'files': _safe_tar_add(tar, sys_unit, str(Path('systemd-system') / sys_unit.name), exclude_state=True)})
            info = tarfile.TarInfo('MANIFEST.json')
            data = json.dumps(manifest, indent=2).encode()
            info.size = len(data)
            info.mtime = int(time.time())
            tar.addfile(info, fileobj=__import__('io').BytesIO(data))
        return {'ok': True, 'filename': name, 'path': str(out), 'bytes': out.stat().st_size, 'download_url': '/api/admin/client-backup/download?file=' + urllib.parse.quote(name), 'manifest': manifest}
    except Exception as e:
        try:
            if out.exists(): out.unlink()
        except Exception:
            pass
        return {'ok': False, 'error': _bound_text(str(e), MAX_ERROR_BYTES)}

def _reboot_device(confirm: bool = False) -> dict:
    """Schedule a device reboot after the HTTP response is sent.

    This endpoint is only reachable through the authenticated local admin port.
    Use sudo -n so a missing passwordless reboot rule fails immediately instead
    of hanging the renderer on a password prompt.
    """
    if not confirm:
        return {'ok': False, 'error': 'confirmation required'}
    def _run_reboot():
        try:
            _sp.Popen(['sudo', '-n', 'systemctl', 'reboot'], stdout=_sp.DEVNULL, stderr=_sp.DEVNULL)
        except Exception:
            pass
    # Quick permission probe that does not reboot.
    probe = _safe_run(['sudo', '-n', 'systemctl', '--help'], timeout=3)
    if not probe.get('ok'):
        return {'ok': False, 'error': 'sudo -n systemctl is not available for this user', 'probe': probe}
    threading.Timer(2.0, _run_reboot).start()
    return {'ok': True, 'scheduled': True, 'delay_seconds': 2, 'message': 'Device reboot scheduled.'}

def _admin_status() -> dict:
    base = _nl_status(); cache = Handler.cache_dir; install = cache.parent
    pending = install / 'pending-command.json'; result = cache / 'command-result.json'
    chromium = _safe_run(['pgrep', '-af', 'chromium.*127.0.0.1:390'], timeout=2)
    labwc = _safe_run(['pgrep', '-af', '^labwc'], timeout=2)
    services = [_system_service_state('screentra-client-heartbeat.service'), _system_service_state('screentra-client-actor.service'), _system_service_state('screentra-client-admin.service'), _system_service_state('screentra-cast-kiosk@0.service'), _system_service_state('screentra-cast-kiosk@1.service')]
    for svc in services:
        if svc.get('name') == 'screentra-client-actor.service' and svc.get('active') == 'inactive' and svc.get('enabled') == 'enabled':
            svc['active'] = 'ready'
            svc['active_ok'] = True
            svc['note'] = 'one-shot command actor; inactive means no pending command'
        if svc.get('name') == 'screentra-cast-kiosk-kiosk@1.service' and svc.get('active') == 'inactive' and svc.get('enabled') == 'disabled':
            svc['active'] = 'disabled'
            svc['active_ok'] = True
            svc['note'] = 'secondary kiosk output intentionally disabled while touchscreen-only mode is active'
    calibration = _read_json_file(cache / 'touch-calibration.json', [])
    pending_state = _read_json_file(pending, None)
    result_state = _read_json_file(result, None)
    return {'status': base, 'time': _iso_now(), 'uptime_seconds': time.monotonic(), 'auth': _auth_source(), 'metrics': _system_metrics(), 'display_governance': _load_display_governance(), 'services': services, 'processes': {'chromium': chromium.get('stdout', ''), 'labwc': labwc.get('stdout', '')}, 'outputs': _wlr_outputs(), 'touch': {'devices': _nl_detect_touch_devices(), 'binding': _current_touch_binding(), 'calibration': calibration}, 'ca': _cert_info(), 'command_state': {'pending_command': pending_state, 'last_result': result_state}}


# === local admin auth =========================================================

def _auth_dir() -> Path:
    p = Handler.cache_dir.parent / 'admin-auth'
    _ensure_real_directory(p, create=True)
    return p

def _auth_secret() -> bytes:
    p = _auth_dir() / 'session-secret'
    try:
        return _read_real_file(p, maximum=4096).strip()
    except FileNotFoundError:
        _atomic_write_text(p, secrets.token_urlsafe(32), mode=0o600)
        return _read_real_file(p, maximum=4096).strip()

def _password_hash(password: str, salt: str | None = None) -> str:
    salt = salt or secrets.token_hex(16)
    dk = hashlib.pbkdf2_hmac('sha256', password.encode(), salt.encode(), 120000)
    return 'pbkdf2_sha256$120000$' + salt + '$' + base64.b64encode(dk).decode()

def _verify_password(password: str, encoded: str) -> bool:
    try:
        if encoded.startswith('sha256:'):
            _prefix, salt, expected = encoded.split(':', 2)
            h = hashlib.sha256()
            h.update(salt.encode()); h.update(b':'); h.update(password.encode())
            return hmac.compare_digest(h.hexdigest(), expected)
        algo, rounds, salt, digest = encoded.split('$', 3)
        if algo != 'pbkdf2_sha256': return False
        dk = hashlib.pbkdf2_hmac('sha256', password.encode(), salt.encode(), int(rounds))
        return hmac.compare_digest(base64.b64encode(dk).decode(), digest)
    except Exception:
        return False

def _synced_users_path() -> Path:
    return Handler.cache_dir.parent / 'admin-users.json'

def _load_admin_users() -> list[dict]:
    path = _synced_users_path()
    if path.exists():
        try:
            data = _read_json_file(path, {})
            users = data.get('users') if isinstance(data, dict) else data
            if isinstance(users, list):
                return [u for u in users if isinstance(u, dict) and u.get('username') and (u.get('password_hash') or u.get('password'))]
        except Exception:
            pass
    return []

def _auth_source() -> dict:
    users = _load_admin_users()
    if users:
        try: synced_at = _read_json_file(_synced_users_path(), {}).get('synced_at')
        except Exception: synced_at = None
        return {'source': 'synced_server_users', 'fallback_default': False, 'user_count': len(users), 'synced_at': synced_at}
    return {'source': 'unprovisioned', 'fallback_default': False, 'user_count': 0, 'warning': 'No local admin users are provisioned; sync an operator account from the managed server.'}

def _check_login(username: str, password: str) -> tuple[bool, dict]:
    users = _load_admin_users()
    if users:
        for u in users:
            if hmac.compare_digest(str(u.get('username')), username):
                ph = u.get('password_hash')
                if ph and _verify_password(password, ph):
                    return True, {'username': username, 'source': 'synced_server_users'}
                if u.get('password') and hmac.compare_digest(str(u.get('password')), password):
                    return True, {'username': username, 'source': 'synced_server_users'}
        return False, {'error': 'invalid username or password', **_auth_source()}
    return False, {'error': 'local admin authentication is not provisioned', **_auth_source()}

def _make_session(username: str, source: str) -> str:
    exp = int(time.time()) + 12 * 3600
    nonce = secrets.token_urlsafe(12)
    body = f'{username}|{source}|{exp}|{nonce}'
    sig = hmac.new(_auth_secret(), body.encode(), hashlib.sha256).hexdigest()
    return base64.urlsafe_b64encode((body + '|' + sig).encode()).decode()

def _parse_cookie(header: str, name: str) -> str | None:
    for part in (header or '').split(';'):
        if '=' in part:
            k, v = part.strip().split('=', 1)
            if k == name: return v
    return None

def _session_user(cookie_header: str) -> dict | None:
    tok = _parse_cookie(cookie_header, 'ripley_admin_session')
    if not tok: return None
    try:
        raw = base64.urlsafe_b64decode(tok.encode()).decode()
        username, source, exp, nonce, sig = raw.rsplit('|', 4)
        body = f'{username}|{source}|{exp}|{nonce}'
        good = hmac.new(_auth_secret(), body.encode(), hashlib.sha256).hexdigest()
        if not hmac.compare_digest(sig, good): return None
        if int(exp) < int(time.time()): return None
        return {'username': username, 'source': source, 'expires_at': int(exp)}
    except Exception:
        return None

def _admin_login_html() -> str:
    src = _auth_source()
    warn = 'No local admin account is provisioned yet; sync an operator account from the managed server.' if not src.get('user_count') else 'Using synced server users.'
    return '<!doctype html><html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>Screentra Cast Admin Sign In</title><style>body{margin:0;background:#07111f;color:#e5eefc;font-family:system-ui,-apple-system,Segoe UI,sans-serif;display:grid;place-items:center;min-height:100vh}.card{width:min(430px,92vw);background:#0f172a;border:1px solid #243247;border-radius:18px;padding:24px;box-shadow:0 20px 70px rgba(0,0,0,.4)}h1{margin:0 0 8px}.muted{color:#91a4bf}.warn{border:1px solid rgba(245,158,11,.45);background:rgba(245,158,11,.08);border-radius:12px;padding:10px;margin:14px 0;color:#facc15}label{display:block;margin-top:12px;color:#91a4bf;font-size:12px;text-transform:uppercase;letter-spacing:.08em}input{width:100%;margin-top:6px;padding:12px;border-radius:10px;border:1px solid #243247;background:#07111f;color:#e5eefc;font-size:16px}button{width:100%;margin-top:16px;padding:12px;border:0;border-radius:999px;background:#38bdf8;color:#03131f;font-weight:900;font-size:15px}.err{color:#fca5a5;min-height:20px;margin-top:10px}</style></head><body><form class="card" id="f"><h1>Screentra Cast Admin</h1><div class="muted">Sign in to manage this local player.</div><div class="warn">'+warn+'</div><label>Username</label><input id="u" autocomplete="username" value="admin"><label>Password</label><input id="p" type="password" autocomplete="current-password" autofocus><button>Sign in</button><div class="err" id="e"></div></form><script>async function readJsonBounded(r,max=65536){const reader=r.body&&r.body.getReader?r.body.getReader():null;if(!reader)throw new Error("response body unavailable");const decoder=new TextDecoder();let total=0,text="";try{while(true){const p=await reader.read();if(p.done){text+=decoder.decode();return JSON.parse(text)}const v=p.value||new Uint8Array();total+=v.byteLength;if(total>max){try{await reader.cancel()}catch(_e){}throw new Error("response exceeds its size limit")}text+=decoder.decode(v,{stream:true})}}finally{reader.releaseLock()}}document.getElementById("f").addEventListener("submit",async ev=>{ev.preventDefault();e.textContent="Signing in…";try{const r=await fetch("/api/admin/auth/login",{method:"POST",headers:{"Content-Type":"application/json"},body:JSON.stringify({username:u.value,password:p.value})});const j=await readJsonBounded(r);if(r.ok&&j.ok){location.href="/"}else{e.textContent=j.error||"Login failed"}}catch(x){e.textContent=String(x)}});</script></body></html>'

def _admin_auth_payload(handler) -> dict:
    user = _session_user(handler.headers.get('Cookie', ''))
    return {'authenticated': bool(user), 'user': user, **_auth_source()}

def _csrf_allows_mutation(headers) -> bool:
    """Allow same-origin browser mutations; tolerate headerless local clients."""
    source = headers.get('Origin') or headers.get('Referer')
    if not source:
        return True
    try:
        parsed = urllib.parse.urlparse(source)
        host = str(headers.get('Host') or '').strip().lower()
        if not host or parsed.scheme != 'http' or parsed.netloc.lower() != host:
            return False
        return True
    except Exception:
        return False


def _admin_requires_auth(path: str) -> bool:
    if os.environ.get('RIPLEY_ADMIN_MODE', '').lower() not in ('1', 'true', 'yes'):
        return False
    if path in ('/api/admin/auth/login', '/api/admin/auth/status', '/login', '/login.html'):
        return False
    if path.startswith('/api/admin') or path in ('/', '/admin.html'):
        return True
    return False

def _mutation_requires_auth(path: str) -> bool:
    if path in ('/api/admin/auth/login', '/api/admin/auth/logout', '/api/admin/auth/status'):
        return False
    return path in ('/api/touchscreen/select', '/api/touch-calibration.json', '/api/client-config.json') or path.startswith('/api/admin/')

ADMIN_HTML = """<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>Screentra Cast Player Admin</title><style>
:root{--bg:#07111f;--panel:#0f172a;--panel2:#111c31;--line:#26364f;--text:#e5eefc;--muted:#91a4bf;--ok:#22c55e;--warn:#f59e0b;--err:#ef4444;--accent:#38bdf8;--violet:#a78bfa}*{box-sizing:border-box}body{margin:0;background:radial-gradient(circle at top left,rgba(56,189,248,.14),transparent 34rem),linear-gradient(160deg,#07111f,#020617);color:var(--text);font:14px/1.45 system-ui,-apple-system,Segoe UI,sans-serif}.wrap{max-width:1380px;margin:0 auto;padding:24px}.top{display:flex;gap:16px;align-items:flex-start;justify-content:space-between;margin-bottom:18px}.title h1{margin:0;font-size:30px;letter-spacing:-.03em}.title p{margin:6px 0 0;color:var(--muted)}.pill{display:inline-flex;align-items:center;gap:6px;padding:5px 10px;border-radius:999px;border:1px solid var(--line);font-weight:900;font-size:12px;text-transform:uppercase;letter-spacing:.08em}.ok{color:var(--ok);border-color:rgba(34,197,94,.5)}.ready{color:#7dd3fc;border-color:rgba(125,211,252,.62);background:rgba(14,116,144,.12)}.disabled{color:#cbd5e1;border-color:rgba(203,213,225,.38);background:rgba(148,163,184,.10)}.warn{color:var(--warn);border-color:rgba(245,158,11,.5)}.err{color:var(--err);border-color:rgba(239,68,68,.5)}.muted{color:var(--muted)}.small{color:var(--muted);font-size:12px}.cards{display:grid;grid-template-columns:repeat(3,minmax(0,1fr));gap:12px;margin:14px 0 18px}.metric{background:linear-gradient(180deg,rgba(17,28,49,.96),rgba(15,23,42,.9));border:1px solid var(--line);border-radius:18px;padding:16px;box-shadow:0 14px 44px rgba(0,0,0,.24)}.metric .label{color:var(--muted);font-size:12px;text-transform:uppercase;letter-spacing:.08em}.metric .value{font-size:28px;font-weight:950;margin-top:7px;letter-spacing:-.04em}.metric .sub{color:var(--muted);margin-top:4px;min-height:20px}.bar{height:8px;background:#07111f;border-radius:999px;overflow:hidden;border:1px solid rgba(148,163,184,.18);margin-top:12px}.fill{height:100%;background:linear-gradient(90deg,var(--accent),var(--violet));width:0%}.sections{display:grid;gap:12px}.section,.panelcard{background:rgba(15,23,42,.9);border:1px solid var(--line);border-radius:16px;overflow:hidden;box-shadow:0 12px 38px rgba(0,0,0,.22)}.panelcard{padding:16px 18px 18px}.panelcard h2{margin:0 0 12px;font-size:16px}.fixed-panels{margin:14px 0 12px}details.section>summary{list-style:none;cursor:pointer;display:flex;align-items:center;justify-content:space-between;gap:12px;padding:16px 18px;font-weight:950;font-size:16px}details.section>summary::-webkit-details-marker{display:none}.chev{color:var(--muted);font-size:18px;transition:transform .16s ease}details[open] .chev{transform:rotate(90deg)}.body{border-top:1px solid var(--line);padding:16px 18px 18px;background:rgba(2,6,23,.16)}.kv{display:grid;grid-template-columns:170px 1fr;gap:8px 12px}.k{color:var(--muted);font-size:12px;text-transform:uppercase;letter-spacing:.06em}.v{word-break:break-word}.actions{display:flex;gap:8px;flex-wrap:wrap;margin-top:12px;align-items:center}button,select,a.btn{background:#17243a;color:var(--text);border:1px solid var(--line);border-radius:10px;padding:9px 11px;font-weight:800;text-decoration:none;cursor:pointer}button:hover,a.btn:hover,select:hover{border-color:var(--accent)}button.danger{background:rgba(127,29,29,.72);border-color:rgba(239,68,68,.65);color:#fecaca}.split{display:grid;grid-template-columns:1fr 1fr;gap:14px}.service{display:flex;align-items:center;justify-content:space-between;gap:12px;border-bottom:1px solid rgba(148,163,184,.12);padding:10px 0}.service:last-child{border-bottom:0}pre{margin:0;white-space:pre-wrap;word-break:break-word;max-height:420px;overflow:auto;background:#050b14;border:1px solid #182338;border-radius:10px;padding:12px;color:#cbd5e1}.logbar{display:flex;gap:8px;margin-bottom:10px;flex-wrap:wrap}.statusline{margin-top:10px;color:var(--muted)}label{display:inline-flex;gap:7px;align-items:center;color:var(--muted)}input[type=checkbox]{accent-color:var(--accent)}@media(max-width:1000px){.cards{grid-template-columns:repeat(2,minmax(0,1fr))}.split{grid-template-columns:1fr}}@media(max-width:640px){.wrap{padding:14px}.top{display:block}.cards{grid-template-columns:1fr}.kv{grid-template-columns:1fr}.actions{align-items:stretch}.actions>*{width:100%;justify-content:center}}
</style></head><body><div class="wrap"><div class="top"><div class="title"><h1>Screentra Cast Player Admin</h1><p>Easy setup / diagnostics for this local signage player. No layout editor here.</p></div><div id="overall" class="pill warn">LOADING</div></div>
<div class="cards"><section class="metric"><div class="label">CPU</div><div id="cpuValue" class="value">—</div><div id="cpuSub" class="sub">Temp —</div><div class="bar"><div id="cpuBar" class="fill"></div></div></section><section class="metric"><div class="label">RAM</div><div id="ramValue" class="value">—</div><div id="ramSub" class="sub">—</div><div class="bar"><div id="ramBar" class="fill"></div></div></section><section class="metric"><div class="label">GPU</div><div id="gpuValue" class="value">—</div><div id="gpuSub" class="sub">—</div><div class="bar"><div id="gpuBar" class="fill"></div></div></section></div>
<div class="sections">
<div class="split fixed-panels"><section class="panelcard"><h2>Device online / working</h2><div id="device" class="kv"></div><div class="actions"><button onclick="refreshAll()">Refresh</button><a class="btn" href="/">Open player</a><a class="btn" href="/touch-calibration.html">Touch calibration</a></div><div id="deviceMsg" class="statusline"></div></section><section class="panelcard"><h2>Services</h2><div id="services"></div><div class="actions"><button onclick="restartCore()">Restart heartbeat + actor</button><button class="danger" onclick="rebootDevice()">Reboot device</button></div></section></div>
<details class="section"><summary><span>Backup</span><span class="chev">›</span></summary><div class="body"><div class="kv"><div class="k">Client software backup</div><div class="v">Creates a portable software/config-shape archive for this player. It excludes client identity, server URL, config.json, CA/certs/keys, cached layouts, admin sessions, Chromium profiles, and previous backups.</div><div class="k">Update recommendation</div><div class="v">For client software updates, use the server-served installer/update_client path unless the client cannot reach the server. A separate upload-style update file would need package signing, CA preservation, rollback, and offline restore rules.</div></div><div class="actions"><button onclick="backupClient()">Create client backup</button></div><div id="backupMsg" class="statusline"></div></div></details>
<details class="section"><summary><span>Certificate / CA</span><span class="chev">›</span></summary><div class="body"><div id="ca" class="kv"></div><div class="actions"><button onclick="refreshCa()">Refresh CA from server</button><button onclick="loadCerts()">Re-check cert</button></div><div id="caMsg" class="statusline"></div></div></details>
<details class="section"><summary><span>Displays / touch</span><span class="chev">›</span></summary><div class="body"><div class="split"><div><h3>Displays</h3><div id="displays" class="kv"></div><div class="actions"><label>Active displays <select id="displayCount"><option value="1">1</option><option value="2">2</option><option value="3">3</option><option value="4">4</option></select></label><button onclick="saveDisplayCount()">Apply display count</button></div></div><div><h3>Touch binding</h3><div id="touchSummary" class="kv"></div><div class="actions"><label>Touch device <select id="touchDevice"></select></label><label>Bind to display <select id="touchDisplayA"></select></label><label><input id="touchJoined" type="checkbox" onchange="toggleTouchJoined()"> allow joined/tiled two-display touch surface</label><label id="touchDisplayBWrap" style="display:none">Second display <select id="touchDisplayB"></select></label><button onclick="saveTouchBinding()">Apply touch binding</button></div></div></div><div id="displayMsg" class="statusline"></div></div></details>
<details class="section"><summary><span>Logs</span><span class="chev">›</span></summary><div class="body"><div class="logbar"><select id="logTarget"><option value="heartbeat">heartbeat</option><option value="actor">actor</option><option value="renderer0">renderer0</option><option value="kiosk0">labwc kiosk</option><option value="launcher">launcher</option><option value="touch">touch</option><option value="install">install</option><option value="heartbeat-cache">heartbeat cache</option><option value="all">all</option></select><button onclick="loadLogs()">Load logs</button></div><pre id="logs">Select a log target.</pre></div></details>
</div></div><script>
const $=id=>document.getElementById(id); const esc=s=>String(s??'').replace(/[&<>\"']/g,c=>({'&':'&amp;','<':'&lt;','>':'&gt;','\"':'&quot;',"'":'&#39;'}[c]));
function kv(keys){return keys.map(([k,v])=>`<div class="k">${esc(k)}</div><div class="v">${esc(v??'—')}</div>`).join('')} function pill(ok,txt,cls){return `<span class="pill ${cls|| (ok?'ok':'err')}">${esc(txt)}</span>`} function servicePill(x){const active=String(x.active||'unknown').toLowerCase(); const enabled=String(x.enabled||'').toLowerCase(); if(active==='ready')return pill(true,'READY','ready'); if(active==='disabled'||enabled==='disabled')return pill(true,'DISABLED','disabled'); return pill(!!x.active_ok,x.active||'unknown');}
function pct(v){v=Number(v); return Number.isFinite(v)?Math.max(0,Math.min(100,v)):0} function fmtPct(v){return Number.isFinite(Number(v))?Number(v).toFixed(1)+'%':'—'} function bytes(n){n=Number(n); if(!Number.isFinite(n)||n<=0)return '—'; const u=['B','KB','MB','GB','TB']; let i=0; while(n>=1024&&i<u.length-1){n/=1024;i++} return n.toFixed(i?1:0)+' '+u[i]}
async function readTextBounded(response,maximum){const reader=response.body&&response.body.getReader?response.body.getReader():null;if(!reader)throw new Error('response body is unavailable');const decoder=new TextDecoder();let total=0,text='';try{while(true){const part=await reader.read();if(part.done){text+=decoder.decode();return text;}const value=part.value||new Uint8Array();total+=value.byteLength;if(total>maximum){try{await reader.cancel();}catch(_e){}throw new Error('response exceeds its size limit');}text+=decoder.decode(value,{stream:true});}}finally{reader.releaseLock();}}
async function api(path,opts){const r=await fetch(path,{cache:'no-store',...(opts||{})}); const t=await readTextBounded(r,64*1024); try{return JSON.parse(t)}catch{return {raw:t,status:r.status}}}
async function refreshAll(){const d=await api('/api/admin/status'); renderStatus(d)}
function renderCards(d){const m=d.metrics||{}, cpu=m.cpu||{}, ram=m.ram||{}, gpu=m.gpu||{}, s=d.status||{}; $('cpuValue').textContent=fmtPct(cpu.percent); $('cpuSub').textContent='Temp '+(cpu.temp_c!=null?cpu.temp_c+' °C':'—')+(cpu.loadavg?` · load ${Number(cpu.loadavg[0]).toFixed(2)}`:''); $('cpuBar').style.width=pct(cpu.percent)+'%'; $('ramValue').textContent=fmtPct(ram.percent); $('ramSub').textContent=`${bytes(ram.used_bytes)} / ${bytes(ram.total_bytes)}`; $('ramBar').style.width=pct(ram.percent)+'%'; $('gpuValue').textContent=gpu.percent!=null?fmtPct(gpu.percent):(gpu.temp_c!=null?gpu.temp_c+' °C':'—'); const gpuNotes=(gpu.health_notes||[]).join(' · '); $('gpuSub').textContent=(gpu.device||gpu.vendor||'GPU')+(gpu.health?` · ${gpu.health}`:'')+(gpu.percent==null?' · usage n/a':'')+(gpu.frequency_mhz!=null?` · ${gpu.frequency_mhz} MHz`:'')+(gpu.vram_percent!=null?` · VRAM ${fmtPct(gpu.vram_percent)}`:'')+(gpuNotes?` · ${gpuNotes}`:''); $('gpuBar').style.width=pct(gpu.percent)+'%';}
function renderStatus(d){const s=d.status||{}; $('overall').className='pill '+(s.connection==='online'?'ok':'err'); $('overall').textContent=s.connection==='online'?'ONLINE':'CHECK'; renderCards(d); $('device').innerHTML=kv([['Hostname',s.hostname],['LAN admin URL',s.admin_url],['Client ID',s.client_id],['Server',s.server_url],['Connection',s.connection],['Last seen',s.last_seen],['Display',`${s.display_label||'—'} · ${s.display_resolution||'—'} · idx ${s.display_index}`],['Layout',s.assigned_layout_id||'No layout assigned'],['Touch',s.touch_device||'not detected']]); const ca=d.ca||{}; $('ca').innerHTML=kv([['CA path',ca.ca_path],['CA SHA256',ca.ca_sha256],['Published CA',ca.published_ca_url],['Verify',ca.verify?.ok?'OK':(ca.verify?.error||'failed')],['Server cert expires',ca.verify?.not_after]]); $('services').innerHTML=(d.services||[]).map(x=>`<div class="service"><div><b>${esc(x.name)}</b><div class="small">enabled: ${esc(x.enabled)}${x.note?' · '+esc(x.note):''}</div></div>${servicePill(x)}</div>`).join(''); const g=d.display_governance||{}; const dc=$('displayCount'); if(dc) dc.value=String(g.active_max_displays||1); renderTouchControls(d); const tb=d.touch?.binding||{}; $('displays').innerHTML=kv([['Active display count',`${g.active_max_displays||1} / detected ${g.installer_detected_max_displays||1}`],['Outputs',(d.outputs||[]).map(o=>`${o.name}:${o.enabled===true?'on':(o.enabled===false?'off':'connected')} ${o.current_mode?(o.current_mode.width?o.current_mode.width+'x'+o.current_mode.height:(o.current_mode.label||'')):''}`).join(' | ')||'—'],['Calibration runs',(d.touch?.calibration||[]).length]]); $('touchSummary').innerHTML=kv([['Touch devices',(d.touch?.devices||[]).map(t=>`${t.name} ${t.path} ${t.reason||''}`).join(' | ')||'—'],['Touch binding',tb.device_name?`${tb.device_name} → ${(tb.target_connectors||tb.target_display_indexes||[]).join(', ')||'all active outputs'}${tb.allow_joined_displays?' (joined/tiled span)':''}`:'not set']]);}
async function loadCerts(){const c=await api('/api/admin/certificates'); $('ca').innerHTML=kv([['CA path',c.ca_path],['CA SHA256',c.ca_sha256],['Published CA',c.published_ca_url],['Verify',c.verify?.ok?'OK':(c.verify?.error||'failed')],['Server cert expires',c.verify?.not_after]]);}
async function refreshCa(){if(!confirm('Refresh the local CA from the saved server URL and restart heartbeat/actor?')) return; $('caMsg').textContent='Refreshing CA…'; const r=await api('/api/admin/ca/refresh',{method:'POST'}); $('caMsg').textContent=r.ok?'CA refreshed and services restarted.':('CA refresh failed: '+(r.error||JSON.stringify(r))); await loadCerts();}
async function restartCore(){if(!confirm('Restart heartbeat and actor services?'))return; const r=await api('/api/admin/services/restart',{method:'POST'}); alert(r.ok?'Restart requested.':'Restart failed: '+(r.error||JSON.stringify(r))); await refreshAll();}
async function rebootDevice(){if(!confirm('Reboot this player device now? The screen will go offline while it restarts.'))return; if(!confirm('Final confirmation: reboot the device?'))return; const msg=$('deviceMsg'); if(msg) msg.textContent='Scheduling reboot…'; const r=await api('/api/admin/device/reboot',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({confirm:true})}); if(msg) msg.textContent=r.ok?'Reboot scheduled. The admin page will disconnect shortly.':('Reboot failed: '+(r.error||JSON.stringify(r))); if(!r.ok) alert('Reboot failed: '+(r.error||JSON.stringify(r)));}
function renderTouchControls(d){const devSel=$('touchDevice'), a=$('touchDisplayA'), b=$('touchDisplayB'), joined=$('touchJoined'); if(!devSel||!a||!b||!joined)return; const tb=d.touch?.binding||{}; devSel.innerHTML=(d.touch?.devices||[]).map(t=>`<option value="${esc(t.path)}" data-name="${esc(t.name)}" ${t.path===tb.device_path?'selected':''}>${esc(t.name)} · ${esc(t.path)}</option>`).join('') || '<option value="">No touch device detected</option>'; const outs=(d.outputs||[]); const opts=outs.map((o,i)=>`<option value="${i}">${esc(o.name||('display '+i))}</option>`).join('') || '<option value="0">display 0</option>'; a.innerHTML=opts; b.innerHTML=opts; const idxs=tb.target_display_indexes||[0]; a.value=String(idxs[0]??0); b.value=String(idxs[1]??1); joined.checked=!!tb.allow_joined_displays; toggleTouchJoined();}
function toggleTouchJoined(){const wrap=$('touchDisplayBWrap'); if(wrap) wrap.style.display=$('touchJoined')?.checked?'inline-flex':'none';}
async function saveTouchBinding(){const dev=$('touchDevice'); const joined=!!$('touchJoined')?.checked; const idxs=[Number($('touchDisplayA')?.value||0)]; if(joined) idxs.push(Number($('touchDisplayB')?.value||1)); if(joined&&idxs[0]===idxs[1]){alert('Choose two different displays for joined/tiled touch mode.');return;} if(joined&&!confirm('Joined/tiled mode maps this touchscreen across the active desktop for two-display Surface/NVIDIA-style systems. Continue?'))return; const payload={device_path:dev?.value||null,device_name:dev?.selectedOptions?.[0]?.dataset?.name||null,target_display_indexes:idxs,allow_joined_displays:joined}; const msg=$('displayMsg'); if(msg) msg.textContent='Applying touch binding…'; const r=await api('/api/admin/touch-binding',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify(payload)}); if(msg) msg.textContent=r.ok?'Touch binding applied; labwc/kiosk services are restarting.':('Failed: '+(r.error||JSON.stringify(r))); setTimeout(refreshAll,4000);}
async function saveDisplayCount(){const v=Number($('displayCount').value||1); if(v>1&&!confirm('Enable '+v+' active displays? Touch coordinates may change until outputs settle.'))return; const msg=$('displayMsg'); if(msg) msg.textContent='Applying display count…'; const r=await api('/api/admin/display-count',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({max_displays:v})}); if(msg) msg.textContent=r.ok?'Display count applied; kiosk services are restarting.':('Failed: '+(r.error||JSON.stringify(r))); setTimeout(refreshAll,2500);}
async function backupClient(){if(!confirm('Create a client software backup archive? This excludes client identity, server URL, CA/certs, cached layouts, auth sessions, Chromium profiles, and previous backups.'))return; const msg=$('backupMsg'); if(msg) msg.textContent='Creating backup…'; const r=await api('/api/admin/client-backup',{method:'POST'}); if(r.ok){if(msg) msg.innerHTML='Backup created: <a class="btn" href="'+esc(r.download_url)+'">Download '+esc(r.filename)+'</a> · '+bytes(r.bytes);} else {if(msg) msg.textContent='Backup failed: '+(r.error||JSON.stringify(r));}}
async function loadLogs(){const target=$('logTarget').value; $('logs').textContent='Loading…'; const r=await api('/api/admin/logs?target='+encodeURIComponent(target)); $('logs').textContent=r.text || (r.logs?Object.entries(r.logs).map(([k,v])=>'## '+k+'\\n'+v).join('\\n\\n') : JSON.stringify(r,null,2));}
refreshAll(); setInterval(refreshAll, 8000);
</script></body></html>"""
from urllib.parse import unquote

PLAYER_HTML = r'''<!doctype html>
<html>
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Screentra Cast Cached Player</title>
<style>
*{box-sizing:border-box}@font-face{font-family:'Berkshire Swash';font-style:normal;font-weight:400;font-display:swap;src:url('/assets/fonts/berkshire-swash-400.woff2') format('woff2')}@font-face{font-family:'Butcherman';font-style:normal;font-weight:400;font-display:swap;src:url('/assets/fonts/butcherman-400.woff2') format('woff2')}@font-face{font-family:'Creepster';font-style:normal;font-weight:400;font-display:swap;src:url('/assets/fonts/creepster-400.woff2') format('woff2')}@font-face{font-family:'Nosifer';font-style:normal;font-weight:400;font-display:swap;src:url('/assets/fonts/nosifer-400.woff2') format('woff2')} html,body,#player{margin:0;width:100vw;height:100vh;background:#000;overflow:hidden;color:#fff;font-family:system-ui,-apple-system,Segoe UI,sans-serif;cursor:none;container-type:size}.widget{position:absolute;overflow:hidden;cursor:none} iframe{background:#000;cursor:none}.fallback{display:grid;place-items:center;height:100vh;text-align:center;color:#94a3b8}.fallback strong{display:block;color:#e2e8f0;font-size:2rem;margin-bottom:.5rem}.clock,.date{display:flex;align-items:center;justify-content:center;width:100%;height:100%}iframe{background:#000;cursor:none}.was-link{display:inline-flex;align-items:center;gap:.45em;border:1px dashed rgba(125,211,252,.65);background:rgba(14,116,144,.16);color:#bae6fd;border-radius:999px;padding:.14em .55em;text-decoration:none}.was-link:after{content:'was link';font-size:.68em;letter-spacing:.08em;text-transform:uppercase;color:#7dd3fc}
</style>
</head>
<body><div id="player"><div class="fallback"><div><strong>Waiting for assigned layout</strong><span>Heartbeat will cache a layout when assigned.</span></div></div></div>
<script>
const cachedServerUrl=__SCREENTRA_SERVER_URL__;
let currentHash='';
async function readTextBounded(response,maximum){const reader=response.body&&response.body.getReader?response.body.getReader():null;if(!reader)throw new Error('response body is unavailable');const decoder=new TextDecoder();let total=0,text='';try{while(true){const part=await reader.read();if(part.done){text+=decoder.decode();return text;}const value=part.value||new Uint8Array();total+=value.byteLength;if(total>maximum){try{await reader.cancel();}catch(_e){}throw new Error('response exceeds its size limit');}text+=decoder.decode(value,{stream:true});}}finally{reader.releaseLock();}}
function esc(s){return String(s??'').replace(/[&<>"']/g,c=>({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));}
function normWidgetType(w){return (w.widget_type || w.type || '').toLowerCase().replace(/_/g,'-');}
function props(w){return w.props || w.properties || {};}
function cachedAssetUrl(u){const value=String(u||'');return value.startsWith('/uploads/')&&cachedServerUrl?cachedServerUrl+value:value;}
function isYouTubeUrl(u){return /(?:youtube\.com|youtu\.be)/i.test(String(u||''));}
function youtubeId(u){const s=String(u||'');let m=s.match(/[?&]v=([^&]+)/); if(m) return m[1]; m=s.match(/youtu\.be\/([^?&/]+)/); if(m) return m[1]; m=s.match(/youtube\.com\/(?:embed|shorts)\/([^?&/]+)/); return m?m[1]:'';}
function youtubeEmbedUrl(u,p){const id=youtubeId(u); const params=new URLSearchParams({autoplay:'1',mute:(p.mute||p.muted)?'1':'0',loop:'1',playsinline:'1',rel:'0'}); if(id) params.set('playlist',id); return id ? ('https://www.youtube.com/embed/'+encodeURIComponent(id)+'?'+params.toString()) : String(u||'');}
function cachedLayoutPercent(value){value=Number(value)||0;return Math.abs(value)>1?value/100:value;}
function cachedOpacity(value){const n=Number(value);return (Number.isFinite(n)?Math.max(0,Math.min(100,n))/100:1);}
function styleFor(w){const rx=Number(w.x)||0,ry=Number(w.y)||0,rw=Number(w.w??w.width)||0.1,rh=Number(w.h??w.height)||0.1,percentMode=[rx,ry,rw,rh].some(v=>Math.abs(v)>1),x=percentMode?rx/100:rx,y=percentMode?ry/100:ry,ww=percentMode?rw/100:rw,hh=percentMode?rh/100:rh,z=Number(w.z)||1;return `left:${x*100}%;top:${y*100}%;width:${ww*100}%;height:${hh*100}%;z-index:${z};`;}
function wmoIcon(code){const m={'0':'☀️','1':'🌤️','2':'⛅','3':'☁️','45':'🌫️','48':'🌫️','51':'🌦️','53':'🌦️','55':'🌦️','61':'🌧️','63':'🌧️','65':'🌧️','71':'❄️','73':'❄️','75':'❄️','80':'🌦️','81':'🌧️','82':'🌧️','95':'⛈️','96':'⛈️','99':'⛈️'};return m[String(code)]||'🌤️';}
function wmoIconHtml(code,p,staticOnly=false){const key=String(code),custom=p&&p.custom_icons&&p.custom_icons[key];if(custom)return `<img src="${esc(custom)}" style="width:100%;height:100%;object-fit:contain;">`;const slugs={'0':'clear-day','1':'mostly-clear-day','2':'partly-cloudy-day','3':'overcast-day','45':'fog-day','48':'overcast-day-fog','51':'drizzle','53':'drizzle','55':'extreme-drizzle','61':'rain','63':'rain','65':'extreme-rain','71':'snow','73':'snow','75':'extreme-snow','80':'partly-cloudy-day-rain','81':'overcast-day-rain','82':'extreme-day-rain','95':'thunderstorms-day','96':'thunderstorms-day-hail','99':'extreme-thunderstorms-day','0_night':'clear-night','1_night':'mostly-clear-night','2_night':'partly-cloudy-night','3_night':'overcast-night','45_night':'fog-night','48_night':'overcast-night-fog','80_night':'partly-cloudy-night-rain','81_night':'overcast-night-rain','82_night':'extreme-night-rain','95_night':'thunderstorms-night','96_night':'thunderstorms-night-hail','99_night':'extreme-thunderstorms-night-hail'},slug=slugs[key]||'partly-cloudy-day',prefix=staticOnly?'/assets/weather/meteocons/fill-static/':'/assets/weather/meteocons/fill/';return `<img class="ripley-weather-icon" src="${prefix}${slug}.svg?v=20261030100000-weather-animation-budget" alt="Weather" loading="eager" decoding="async" style="display:block;width:100%;height:100%;object-fit:contain;">`;}
function weatherAlertMarkup(slug,label){return '<img class="ripley-weather-alert-icon" src="/assets/weather/meteocons/fill-static/'+slug+'.svg" alt="'+esc(label)+'" loading="eager" decoding="async" style="display:block;width:100%;height:100%;object-fit:contain;">';}
function weatherAlertColorCodeHtml(alerts){const items=Array.isArray(alerts)?alerts.filter(Boolean).map(String):(alerts?[String(alerts)]:[]);if(!items.length)return '';const label=items.join(', '),lower=label.toLowerCase(),slug=/warning/.test(lower)?'code-red':/watch/.test(lower)?'code-orange':/advisory|statement|outlook/.test(lower)?'code-yellow':'code-purple';return weatherAlertMarkup(slug,label);}
function weatherAlertConditionHtml(alerts){const items=Array.isArray(alerts)?alerts.filter(Boolean).map(String):(alerts?[String(alerts)]:[]);if(!items.length)return '';const label=items.join(', '),lower=label.toLowerCase();let slug=null;if(/flood/.test(lower))slug='extreme-rain';else if(/fog|low visibility/.test(lower))slug='fog-day';else if(/thunder|lightning|severe/.test(lower))slug='extreme-thunderstorms-day';else if(/snow|blizzard|winter|ice|freez/.test(lower)&&!/squall/.test(lower))slug='extreme-snow';return slug?weatherAlertMarkup(slug,label):'';}
function weatherFor(p,i){const d=p._weather_data||{},q=d.daily||{},codes=q.weather_code_day||q.weather_code||q.weathercode||[],code=codes[i]??d.current_weather?.weather_code??d.current_weather?.weathercode??2,winds=q.wind_speed||q.windspeed||[],windRaw=winds[i]??d.current_weather?.windspeed??0,alertSeries=q.weather_alerts||q.weather_alert||[],alertValue=alertSeries[i],alerts=Array.isArray(alertValue)?alertValue.filter(Boolean).map(String):(alertValue?[String(alertValue)]:[]),metric=String(p.unit||(p.units==='metric'?'C':'F')).toUpperCase()!=='F',sourceF=d.source==='noaa_nws',display=v=>{if(v==null||Number.isNaN(Number(v)))return '';const n=Number(v);return Math.round(sourceF?(metric?(n-32)*5/9:n):(metric?n:n*9/5+32));},highRaw=q.temperature_2m_max?.[i]??d.current_weather?.temperature,lowRaw=q.temperature_2m_min?.[i]??highRaw,currentRaw=d.current_weather?.temperature??highRaw,descriptions={'0':'Clear','1':'Mainly clear','2':'Partly cloudy','3':'Overcast','45':'Fog','48':'Rime fog','51':'Light drizzle','53':'Moderate drizzle','55':'Dense drizzle','61':'Slight rain','63':'Moderate rain','65':'Heavy rain','71':'Slight snow','73':'Moderate snow','75':'Heavy snow','80':'Rain showers','81':'Heavy rain showers','82':'Violent rain showers','95':'Thunderstorm','96':'Thunderstorm with hail','99':'Severe thunderstorm'};return {code,icon:wmoIcon(code),current:display(currentRaw),high:display(highRaw),low:display(lowRaw),alerts:alerts,alerts_text:alerts.join(' / '),desc:d.source==='unavailable'?'':(descriptions[String(code)]||''),precip:q.precipitation_probability_max?.[i]??0,wind:Math.round(Number(windRaw||0)*(metric?1.60934:1)),unit:metric?'°C':'°F',windUnit:metric?'km/h':'mph'};}
function calendarOverrideMatches(event,override){const type=String(override&&(override.match_type||override.match)||(override&&override.uid?'uid':'title')).toLowerCase(),value=String(override&&(override.match_value||override.value||(type==='uid'?override.uid:override.title))||'').trim();if(!value)return false;if(override.source_date&&String(override.source_date)!==String(event.source_date||''))return false;if(type==='uid')return String(event.uid||'').trim()===value;return String(event.summary||'').trim().toLocaleLowerCase()===value.toLocaleLowerCase();}function applyCalendarOverrides(events,overrides,timeFormat){const list=Array.isArray(overrides)?overrides:[];return(Array.isArray(events)?events:[]).map(event=>{const o=list.find(item=>calendarOverrideMatches(event,item));if(!o)return event;const date=/^\d{4}-\d{2}-\d{2}$/.test(String(o.date||''))?String(o.date):event.date,corrected=String(o.time||''),match=corrected.match(/^(\d{1,2}):(\d{2})$/);let time=event.time;if(match){const hour=Number(match[1]),minute=Number(match[2]);if(hour>=0&&hour<=23&&minute>=0&&minute<=59)time=timeFormat==='24'?String(hour).padStart(2,'0')+':'+String(minute).padStart(2,'0'):(hour%12||12)+':'+String(minute).padStart(2,'0')+' '+(hour>=12?'PM':'AM');}return Object.assign({},event,{date,time,sort_time:match?Number(match[1])*60+Number(match[2]):event.sort_time,override_applied:true});});}function parseCalendarText(raw,maxEvents,timeFormat,overrides){const unfolded=String(raw||'').replace(/\r?\n[ \t]/g,''),out=[];const blocks=unfolded.match(/BEGIN:VEVENT[\s\S]*?END:VEVENT/g)||[];blocks.forEach(block=>{const start=(block.match(/(?:^|\n)DTSTART(?:;[^:]+)?:([^\n]+)/)||[])[1],uid=((block.match(/(?:^|\n)UID:(.*)/)||[])[1]||'').trim(),summary=((block.match(/(?:^|\n)SUMMARY:(.*)/)||[])[1]||'').trim(),location=((block.match(/(?:^|\n)LOCATION:(.*)/)||[])[1]||'').trim();if(!start||!summary)return;const date=start.match(/^(\d{4})(\d{2})(\d{2})/);if(!date)return;const hh=(start.match(/T(\d{2})(\d{2})/)||[])[1],mm=(start.match(/T\d{2}(\d{2})/)||[])[1];let time='All day';if(hh!=null){const n=Number(hh);time=timeFormat==='24'?String(n).padStart(2,'0')+':'+(mm||'00'):(n%12||12)+':'+(mm||'00')+' '+(n>=12?'PM':'AM');}out.push({uid,summary,location,time,date:date[1]+'-'+date[2]+'-'+date[3],source_date:date[1]+'-'+date[2]+'-'+date[3],source_start:start,sort_time:hh==null?0:Number(hh)*60+Number(mm||'0')});});const today=new Date().toISOString().slice(0,10);return applyCalendarOverrides(out,overrides,timeFormat).filter(e=>e.date>=today).sort((a,b)=>a.date.localeCompare(b.date)||(Number(a.sort_time)||0)-(Number(b.sort_time)||0)||a.summary.localeCompare(b.summary)).slice(0,Math.max(1,Math.min(30,Number(maxEvents)||7)));}
function calendarFor(p,i){return Array.isArray(p._calendar_events)?p._calendar_events[i]||null:null;}
async function loadCalendar(p,layoutId,widgetId){if(!layoutId||!widgetId)return;const url='/api/ics-proxy?layout_id='+encodeURIComponent(layoutId)+'&widget_id='+encodeURIComponent(widgetId);for(let attempt=0;attempt<3;attempt++){try{const r=await fetch(url,{cache:'no-store'});if(!r.ok){if([429,502,503,504].includes(r.status)&&attempt<2){await new Promise(resolve=>setTimeout(resolve,3500));continue;}throw new Error('calendar '+r.status);}const t=await readTextBounded(r,4*1024*1024);p._calendar_events=parseCalendarText(t,p.max_events,p.time_format==='24'?'24':'12',p.calendar_time_overrides);return;}catch(_e){if(attempt<2){await new Promise(resolve=>setTimeout(resolve,3500));continue;}p._calendar_events=[];}}}
function repeatCount(p,e){const r=e.repeat||{},t=String(p._widget_type||p.widget_type||'').toLowerCase();if(r.group&&t==='traffic'){const available=cachedTrafficRelevantEvents(p,p.traffic_variable).length,requested=Math.max(1,Math.min(30,Number(p.max_events)||Number(r.count)||1));return p.hide_if_empty===true?Math.min(requested,available):requested;}if(r.group&&t==='calendar'){const requested=Math.max(1,Math.min(30,Number(p.max_events)||Number(r.count)||5)),es=Array.isArray(p.template_elements)?p.template_elements:[],hasCalendarExemplar=es.some(item=>{const q=item&&item.props||{},source=q.variable_source||(item&&item.type==='variable_icon'?'weather':'date');return source==='calendar'&&!(item.repeat&&item.repeat.group);});return hasCalendarExemplar?Math.max(1,requested-1):requested;}return r.group?Math.max(1,Math.min(14,Number(p.shown_days)||Number(r.count)||4)):Math.max(1,Number(r.count)||1);}
function groupMetrics(es){const g={};es.forEach(e=>{const id=e.repeat&&e.repeat.group;if(!id)return;const r=e.repeat,x=Number(e.x)||0,y=Number(e.y)||0,w=Number(e.width)||120,h=Number(e.height)||80,m=g[id]||(g[id]={minX:x,minY:y,maxX:x+w,maxY:y+h,gapX:Number(r.gap_x)||0,gapY:Number(r.gap_y)||0,mode:r.mode||'rows'});m.minX=Math.min(m.minX,x);m.minY=Math.min(m.minY,y);m.maxX=Math.max(m.maxX,x+w);m.maxY=Math.max(m.maxY,y+h);});return g;}
function templateEl(e,p,i,g,dw,dh){const q=Object.assign({},e.props||{}),r=e.repeat||{},m=r.group&&g[r.group];if(i&&q.variable_source!=='calendar'){const n=Number(q.day_offset??q.weather_day_offset)||0;q.day_offset=n+i;q.weather_day_offset=n+i;}let x=Number(e.x)||0,y=Number(e.y)||0,w=Number(e.width)||120,h=Number(e.height)||80;if(m&&i){if((m.mode||'rows')==='columns')x+=i*(m.maxX-m.minX+m.gapX);else y+=i*(m.maxY-m.minY+m.gapY);}const a=q.align||'center',j=a==='left'?'flex-start':a==='right'?'flex-end':'center';let st=`position:absolute;box-sizing:border-box;left:${x/dw*100}%;top:${y/dh*100}%;width:${w/dw*100}%;height:${h/dh*100}%;display:flex;align-items:center;justify-content:${j};text-align:${a};overflow:hidden;z-index:${Number(e.z)||0};color:${q.color||'#fff'};font-family:${cachedFontFamily(q)};font-size:clamp(4px,min(calc(${(Number(q.font_size)||24)/dw*100}cqw),calc(${(Number(q.font_size)||24)/dh*100}cqh)),${(Number(q.font_size)||24)*4}px);font-weight:${['variable_icon','variable_text','variable','date'].includes(e.type)?800:(q.font_weight||700)};line-height:1.08;`;let inner='';if(e.type==='rectangle')st+=`background:${q.color||'#1e293b'};opacity:${cachedOpacity(q.opacity)};border-radius:${Number(q.radius)||0}px;`;else if(e.type==='text')inner=esc(q.content||q.text||'Text');else if(e.type==='image'){const fit=['cover','contain','fill','none','scale-down'].includes(q.fit)?q.fit:'contain',radius=Number(q.radius)||0;inner=`<img src="${esc(cachedAssetUrl(q.url||q.src||''))}" style="display:block;width:100%;height:100%;object-fit:${fit};border-radius:${radius}px;">`;}else if(['variable_icon','variable_text','variable','date'].includes(e.type)){const k=q.variable_key||'';if(q.variable_source==='calendar'){const base=Number(q.calendar_event_index??(r.group?0:(q.day_offset??q.weather_day_offset)))||0,idx=Math.max(0,Math.min(29,base+(r.group?i+1:0))),ev=calendarFor(p,idx);if(k==='calendar_event_title')inner=esc(ev?.summary||'');else if(k==='calendar_event_time')inner=esc(ev?.time||'');else if(k==='calendar_event_location')inner=esc(ev?.location||'');else if(k==='calendar_event_date'){const d=ev?new Date(ev.date+'T00:00:00'):null;inner=esc(d&&!isNaN(d)?d.toLocaleDateString(undefined,{month:'short',day:'numeric',year:'numeric'}):'');}}else {const source=q.variable_source||(e.type==='variable_icon'?'weather':'date');if(source==='weather'){const wx=weatherFor(p,Math.max(0,Math.min(6,Number(q.day_offset??q.weather_day_offset)||0))),alertDay=Math.max(0,Math.min(6,Number(q.day_offset??q.weather_day_offset)||0))===0;if(k==='weather_icon'&&q.weather_icon_alerts&&alertDay&&wx.alerts.length)inner=weatherAlertConditionHtml(wx.alerts);else if(k==='weather-alert-code'&&(e.type==='variable_icon'||q.display_mode==='weather_icon'))inner=weatherAlertColorCodeHtml(wx.alerts);else if((k==='weather_alerts'||k==='weather-alerts')&&(e.type==='variable_icon'||q.display_mode==='weather_icon'))inner=weatherAlertConditionHtml(wx.alerts);else if(k==='weather_icon'||e.type==='variable_icon'||q.display_mode==='weather_icon')inner=wmoIconHtml(wx.code,p,false);else if(k==='current_temp')inner=esc(wx.current===''?'':wx.current+wx.unit);else if(k==='high_temp')inner=esc(wx.high===''?'':'High '+wx.high+wx.unit);else if(k==='low_temp')inner=esc(wx.low===''?'':'Low '+wx.low+wx.unit);else if(k==='weather_description')inner=esc(wx.desc);else if(k==='weather_alerts'||k==='weather-alerts'||k==='weather-alert-code')inner=esc(wx.alerts_text);else if(k==='precip_chance')inner=esc(wx.precip+'%');else if(k==='wind_speed')inner=esc(wx.wind===''?'':'Wind '+wx.wind+' '+wx.windUnit);else inner=esc(q.default_text||q.label||k);}else if(source==='traffic'){inner=esc(cachedTrafficVariableText(p,k,i));}else if(source==='date'){const d=new Date();d.setDate(d.getDate()+(Number(q.day_offset??q.weather_day_offset)||0));if(k==='day_name')inner=esc(d.toLocaleDateString(undefined,{weekday:'long'}));else if(k==='date_number')inner=esc(String(d.getDate()));else if(k==='month_day')inner=esc(d.toLocaleDateString(undefined,{month:'short',day:'numeric'}));else inner=esc(d.toLocaleDateString(undefined,{weekday:'long',month:'short',day:'numeric'}));}else inner=esc(q.default_text||q.label||k);}}else inner=esc(q.label||e.type||'');return `<div style="${st}">${inner}</div>`;}
function templateHtml(w){const p=props(w),es=Array.isArray(p.template_elements)?p.template_elements:[];if(!es.length)return '';const g=groupMetrics(es);let dw=Number(p.template_width)||760,dh=Number(p.template_height)||427.5;es.forEach(e=>{dw=Math.max(dw,(Number(e.x)||0)+(Number(e.width)||120));dh=Math.max(dh,(Number(e.y)||0)+(Number(e.height)||80));});es.forEach(e=>{const r=e.repeat||{},m=r.group&&g[r.group],count=repeatCount(p,e);if(!m||!r.group)return;const stepX=(m.maxX-m.minX)+m.gapX,stepY=(m.maxY-m.minY)+m.gapY;if((m.mode||r.mode||'rows')==='columns')dw=Math.max(dw,m.maxX+Math.max(0,count-1)*stepX);else dh=Math.max(dh,m.maxY+Math.max(0,count-1)*stepY);});return `<div style="position:relative;width:100%;height:100%;overflow:hidden;container-type:size;">${es.slice().sort((a,b)=>(Number(a.z)||0)-(Number(b.z)||0)).map(e=>Array.from({length:repeatCount(p,e)},(_,i)=>templateEl(e,p,i,g,dw,dh)).join('')).join('')}</div>`;}
async function loadWeather(p){const lat=Number(p.lat||0),lon=Number(p.lon||0);if(!lat||!lon)return;try{const r=await fetch(`/api/weather/forecast?lat=${encodeURIComponent(lat)}&lon=${encodeURIComponent(lon)}`,{cache:'no-store'});if(r.ok)p._weather_data=JSON.parse(await readTextBounded(r,4*1024*1024));}catch(_e){}}
async function loadTraffic(p,widgetId){const lat=Number(p.lat||0),lon=Number(p.lon||0);if(!lat||!lon){p._traffic_data={configured:false,available:false,events:[]};return;}try{const radius=Math.max(1,Math.min(100,Number(p.radius_miles)||25)),r=await fetch(`/api/traffic/events?lat=${encodeURIComponent(lat)}&lon=${encodeURIComponent(lon)}&radius_miles=${encodeURIComponent(radius)}`,{cache:'no-store'});if(!r.ok)throw new Error('traffic '+r.status);p._traffic_data=JSON.parse(await readTextBounded(r,4*1024*1024));if(typeof filterTrafficEvents==='function'&&p._traffic_data.configured!==false&&p._traffic_data.available!==false)p._traffic_data.events=filterTrafficEvents(p._traffic_data.events,p,widgetId);}catch(_e){p._traffic_data={configured:false,available:false,events:[],message:'Traffic provider unavailable'};}}
const TRAFFIC_CLOSURE_STATE_KEY='screentra-cast.traffic-closure-tracking.v1',TRAFFIC_DAY_MS=86400000;function trafficReadState(){try{const raw=window.localStorage&&window.localStorage.getItem(TRAFFIC_CLOSURE_STATE_KEY),parsed=raw?JSON.parse(raw):{};return parsed&&typeof parsed==='object'&&!Array.isArray(parsed)?parsed:{};}catch(_e){return {};}}function trafficWriteState(state){try{if(window.localStorage)window.localStorage.setItem(TRAFFIC_CLOSURE_STATE_KEY,JSON.stringify(state));}catch(_e){}}function trafficEventKey(e){const id=String(e&&e.id||'').trim();if(id)return 'id:'+id;const fields=[e&&e.category,e&&e.event_type,e&&e.road,e&&e.direction,e&&e.description,e&&e.latitude,e&&e.longitude].map(v=>String(v==null?'':v).trim().toLowerCase()),fallback=fields.join('|');return fallback.replace(/^\|+|\|+$/g,'')?'fallback:'+fallback:'';}function trafficIsClosure(e){const category=String(e&&e.category||'').trim().toLowerCase(),type=String(e&&e.event_type||'').trim().toLowerCase(),description=String(e&&e.description||'').trim().toLowerCase(),lane=String(e&&e.lane_status||'').trim().toLowerCase();return category==='closure'||type.includes('closure')||/\b(?:closed|closure|closing)\b/.test(description+' '+lane);}function filterTrafficEvents(events,props,widgetId){const list=Array.isArray(events)?events:[],o=props||{},scope=String(widgetId||o._traffic_widget_id||'traffic-widget'),state=trafficReadState(),manual=o.manual_closure_blocking===true,active={};list.forEach(e=>{if(!trafficIsClosure(e))return;const key=trafficEventKey(e);if(key)active[key]=true;});let records=Array.isArray(o.blocked_closures)?o.blocked_closures:[];if(manual){const pruned=records.filter(r=>{const key=String(r&&r.key||trafficEventKey(r)||'');return key&&active[key];});if(pruned.length!==records.length)o.blocked_closures=pruned;records=pruned;}const blocked={};if(manual)records.forEach(r=>{const key=String(r&&r.key||trafficEventKey(r)||'');if(key)blocked[key]=true;});if(o.hide_long_term_closures!==true){if(state[scope]){delete state[scope];trafficWriteState(state);}return list.filter(e=>!trafficIsClosure(e)||!blocked[trafficEventKey(e)]);}const days=Math.max(1,Math.min(3650,Number(o.long_term_closure_days)||5)),cutoff=days*TRAFFIC_DAY_MS,now=Date.now(),tracked=state[scope]&&typeof state[scope]==='object'?state[scope]:{},visible=[];list.forEach(e=>{if(!trafficIsClosure(e)){visible.push(e);return;}const key=trafficEventKey(e);if(!key){visible.push(e);return;}if(blocked[key])return;let first=Number(tracked[key]);if(!Number.isFinite(first)||first>now)first=now;tracked[key]=first;if(now-first<cutoff)visible.push(e);});Object.keys(tracked).forEach(key=>{if(!active[key])delete tracked[key];});state[scope]=tracked;trafficWriteState(state);return visible;}
function cachedTrafficEvents(p){const live=p&&p._traffic_data&&Array.isArray(p._traffic_data.events)?p._traffic_data.events:null;return live||[];}
function cachedTrafficKind(e){return String(e&&((e.category||e.type||e.incident_type)||'event')).toLowerCase();}
function cachedTrafficDescription(e){return String(e&&((e.description||e.headline||e.message)||''));}
function cachedTrafficRelevantEvents(p,key){const events=cachedTrafficEvents(p),k=key||'traffic_summary',terms=k==='traffic_incidents'?['crash','incident']:k==='traffic_closures'?['closure']:k==='traffic_delays'?['delay','congestion']:k==='traffic_crashes_closures'?['crash','closure']:null;return terms?events.filter(e=>terms.some(term=>cachedTrafficKind(e).includes(term))):events;}
function cachedTrafficVariableText(p,key,index){const events=cachedTrafficRelevantEvents(p,key),k=key||'traffic_summary';if(k==='traffic_crashes_closures'){const event=events[Math.min(Math.max(0,Number(index)||0),Math.max(0,events.length-1))];return String(event?cachedTrafficDescription(event):'');}return String(events.length)+' traffic event'+(events.length===1?'':'s');}
function cachedDateText(d,fmt){const months=['January','February','March','April','May','June','July','August','September','October','November','December'],days=['Sunday','Monday','Tuesday','Wednesday','Thursday','Friday','Saturday'],shortMonths=months.map(x=>x.slice(0,3)),shortDays=days.map(x=>x.slice(0,3)),tokens={YYYY:String(d.getFullYear()),YY:String(d.getFullYear()).slice(-2),MMMM:months[d.getMonth()],MMM:shortMonths[d.getMonth()],MM:String(d.getMonth()+1).padStart(2,'0'),M:String(d.getMonth()+1),dddd:days[d.getDay()],ddd:shortDays[d.getDay()],DD:String(d.getDate()).padStart(2,'0'),D:String(d.getDate())};return String(fmt||'dddd, MMMM D').replace(/YYYY|MMMM|dddd|MMM|ddd|MM|DD|YY|M|D/g,m=>tokens[m]);}
function cachedClockText(d,fmt){const h24=d.getHours(),h12=h24%12||12,minutes=String(d.getMinutes()).padStart(2,'0'),seconds=String(d.getSeconds()).padStart(2,'0'),ampm=h24>=12?'PM':'AM';return String(fmt||'HH:mm').replace(/HH|hh|ss|H|h|mm|m|s|A/g,m=>({HH:String(h24).padStart(2,'0'),hh:String(h12).padStart(2,'0'),ss:seconds,H:String(h24),h:String(h12),mm:minutes,m:String(d.getMinutes()),s:String(d.getSeconds()),A:ampm}[m]));}
function cachedFontFamily(p){const raw=String((p&& (p.font_family||p.fontFamily))||'').toLowerCase();if(raw.includes('berkshire swash'))return "'Berkshire Swash',sans-serif";if(raw.includes('butcherman'))return "'Butcherman',sans-serif";if(raw.includes('creepster'))return "'Creepster',sans-serif";if(raw.includes('nosifer'))return "'Nosifer',sans-serif";return "system-ui,-apple-system,'Segoe UI',sans-serif";}
function cachedResponsiveFontSize(value){const size=Math.max(1,Number(value)||1);return `clamp(6px,calc(${size/960*100}cqw),${size*4}px)`;}
function countdownFontSize(p){return Math.max(8,Math.min(160,Number(p.font_size||p.fontSize||48)||48));}
function countdownText(node,now){const get=(name)=>node.getAttribute?node.getAttribute(name):(node.dataset||{})[name.slice(5).split('-').map((part,i)=>i?part[0].toUpperCase()+part.slice(1):part).join('')]||'',mode=get('data-countdown-mode')||'date',unit=get('data-countdown-duration-unit')||'minutes',value=Math.max(1,Number(get('data-countdown-duration-value'))||1),started=Number(get('data-countdown-started-at'))||now,target=mode==='duration'?started+value*({seconds:1000,minutes:60000,hours:3600000,days:86400000}[unit]||60000):new Date(get('data-countdown-target-date')||'').getTime(),diff=Math.max(0,target-now),expiry=get('data-countdown-expiry-text')||'Time\u2019s up!';if(!Number.isFinite(target)||target<=0)return get('data-countdown-invalid-text')||'Choose a date and time';if(diff<=0)return expiry;const d=Math.floor(diff/86400000),h=Math.floor(diff%86400000/3600000),m=Math.floor(diff%3600000/60000),s=Math.floor(diff%60000/1000);return d?d+'d '+h+'h':h?h+'h '+m+'m':m?m+'m '+s+'s':s+'s';}
function tickCountdowns(){const now=Date.now();document.querySelectorAll('[data-countdown-widget]').forEach(node=>{const value=node.querySelector('.countdown-value');if(value)value.textContent=countdownText(node,now);});}
function cachedCameraFilterName(value){return ['none','grayscale','sepia','halloween'].includes(value)?value:'none';}
function cachedCameraFilterCss(value){return ({none:'none',grayscale:'grayscale(1)',sepia:'sepia(1)',halloween:'saturate(1.35) contrast(1.15) hue-rotate(300deg)'})[cachedCameraFilterName(value)];}
function cachedCameraFilterOverlay(value,id){return cachedCameraFilterName(value)==='halloween'?`<span data-camera-filter-overlay="halloween" data-camera-filter-overlay-id="${esc(id||'')}" aria-hidden="true" style="position:absolute;inset:0;pointer-events:none;background:linear-gradient(135deg,rgba(249,115,22,.34),rgba(88,28,135,.42));mix-blend-mode:screen;opacity:.8;"></span>`:'';}
function initCachedCameraWidgets(widgets){const nodes=document.querySelectorAll('[data-camera-widget-id]');nodes.forEach(node=>{const id=node.getAttribute('data-camera-widget-id'),widget=(widgets||[]).find(item=>String(item.id)===String(id)),p=widget&&props(widget);if(!p||p.capture_enabled!==true||!navigator.mediaDevices||typeof navigator.mediaDevices.getUserMedia!=='function'||node.tagName!=='VIDEO')return;const facing=['user','environment','left','right'].includes(p.facing_mode)?p.facing_mode:'environment';const constraints={video:{facingMode:{ideal:facing},width:{ideal:1280,max:1280},height:{ideal:720,max:720},frameRate:{ideal:30,max:30}},audio:false};node.muted=true;node.autoplay=true;node.playsInline=true;navigator.mediaDevices.getUserMedia(constraints).then(stream=>{node.srcObject=stream;const result=typeof node.play==='function'?node.play():null;if(result&&typeof result.catch==='function')result.catch(()=>{});}).catch(()=>{});});}
function initCachedCameraStreamWidgets(widgets){const nodes=document.querySelectorAll('[data-camera-stream-widget-id]');nodes.forEach(node=>{const id=node.getAttribute('data-camera-stream-widget-id'),widget=(widgets||[]).find(item=>String(item.id)===String(id)),p=widget&&props(widget);if(!p||node.tagName!=='IMG')return;const wait=Math.max(1000,Math.min(60000,Number(p.reconnect_seconds)*1000||5000)),stallLimit=90000;let timer=null,reconnecting=false,lastSignature='',unchangedSince=Date.now();const base='/camera-stream.mjpeg?widget_id='+encodeURIComponent(id);const reconnect=()=>{if(reconnecting)return;reconnecting=true;if(timer){clearTimeout(timer);timer=null;}node.removeAttribute('src');node.src='about:blank';setTimeout(()=>{node.src=base+'&retry='+Date.now();reconnecting=false;},400);};node.onerror=()=>{if(timer||reconnecting)return;timer=setTimeout(()=>{timer=null;reconnect();},wait);};const canvas=document.createElement('canvas');canvas.width=16;canvas.height=9;const context=canvas.getContext('2d',{willReadFrequently:true});if(context){node.__cameraWatchdog=setInterval(()=>{if(!node.isConnected){clearInterval(node.__cameraWatchdog);return;}if(!node.complete||!node.naturalWidth||reconnecting)return;let signature='';try{context.drawImage(node,0,0,16,9);const pixels=context.getImageData(0,0,16,9).data;let sum=0;for(let i=0;i<pixels.length;i+=16)sum=(sum+pixels[i]*3+pixels[i+1]*5+pixels[i+2]*7)>>>0;signature=String(sum);}catch(_e){return;}if(signature!==lastSignature){lastSignature=signature;unchangedSince=Date.now();}else if(Date.now()-unchangedSince>=stallLimit)reconnect();},5000);}}); }
function parseVideoWaitAfter(value){var text=String(value==null?'':value).trim().toLowerCase().replace(/\s*(?:seconds?|secs?|s)\s*$/,'').trim(),range=text.match(/^(\d+(?:\.\d+)?)\s*[-–—]\s*(\d+(?:\.\d+)?)$/),min,max;if(range){min=Number(range[1]);max=Number(range[2]);}else if(/^\d+(?:\.\d+)?$/.test(text)){min=max=Number(text);}else{return{min:0,max:0};}if(!Number.isFinite(min)||!Number.isFinite(max))return{min:0,max:0};min=Math.max(0,Math.min(86400,min));max=Math.max(0,Math.min(86400,max));if(max<min){var swap=min;min=max;max=swap;}return{min:min,max:max};}
function videoWaitAfterSeconds(value){var parsed=parseVideoWaitAfter(value);if(parsed.max<=parsed.min)return parsed.min;var random=Math.max(0,Math.min(.999999999,Math.random()));return Number.isInteger(parsed.min)&&Number.isInteger(parsed.max)?parsed.min+Math.floor(random*(parsed.max-parsed.min+1)):parsed.min+(parsed.max-parsed.min)*random;}
function videoWaitAfterHasDelay(value){return parseVideoWaitAfter(value).max>0;}
function initCachedVideoWidgets(widgets){const nodes=document.querySelectorAll('[data-video-widget-id]');nodes.forEach(node=>{const id=node.getAttribute('data-video-widget-id'),widget=(widgets||[]).find(item=>String(item.id)===String(id)),p=widget&&props(widget),list=p&&Array.isArray(p.playlist)?p.playlist:[];if(!list.length)return;let index=0,waitTimer=null;const advance=()=>{const current=list[index]||{},wait=videoWaitAfterSeconds(current.wait_after);const go=()=>{waitTimer=null;index=(index+1)%list.length;node.src=cachedAssetUrl(String(list[index].url||''));if(typeof node.load==='function')node.load();const result=typeof node.play==='function'?node.play():null;if(result&&typeof result.catch==='function')result.catch(()=>{});};if(wait>0)waitTimer=setTimeout(go,Math.round(wait*1000));else go();};node.src=cachedAssetUrl(String(list[0].url||''));node.loop=list.length===1&&p.loop!==false&&!videoWaitAfterHasDelay(list[0]&&list[0].wait_after);node.onended=list.length>1||(list.length===1&&p.loop!==false&&videoWaitAfterHasDelay(list[0]&&list[0].wait_after))?advance:null;node.onerror=list.length>1?advance:null;});}
async function renderWidget(w){const t=normWidgetType(w),p=Object.assign({_widget_type:t},props(w));if(t==='weather')await loadWeather(p);else if(t==='calendar')await loadCalendar(p,w._layout_id||'',w.id);else if(t==='traffic')await loadTraffic(p,w.id);let inner='';
 if((t==='weather'||t==='calendar'||t==='traffic')&&Array.isArray(p.template_elements)&&p.template_elements.length) inner=templateHtml(Object.assign({},w,{props:p}));
 else if(t==='text') inner=`<div style="width:100%;height:100%;display:flex;align-items:${p.valign||'center'};justify-content:${p.align==='left'?'flex-start':p.align==='right'?'flex-end':'center'};color:${p.color||'#fff'};font-family:${cachedFontFamily(p)};font-size:${cachedResponsiveFontSize(Number(p.font_size||p.fontSize||24))};text-align:${p.align||'center'};">${esc(p.content||p.text||'Text')}</div>`;
 else if(t==='countdown') { const target=p.countdown_mode==='duration' ? Date.now() + Math.max(1,Number(p.duration_value||1))*({seconds:1000,minutes:60000,hours:3600000,days:86400000}[p.duration_unit||'minutes']||60000) : new Date(p.targetDate||'').getTime(); const diff=Math.max(0,target-Date.now()); const d=Math.floor(diff/86400000),h=Math.floor(diff%86400000/3600000),m=Math.floor(diff%3600000/60000),s=Math.floor(diff%60000/1000); const text=Number.isFinite(target)&&target>0?(d?d+'d '+h+'h':h?h+'h '+m+'m':m?m+'m '+s+'s':s+'s'):(p.expiry_text||'Choose a date and time'); inner=`<div style="width:100%;height:100%;display:flex;align-items:center;justify-content:center;color:${p.color||'#fff'};font-family:${cachedFontFamily(p)};text-align:center;overflow:hidden;"><div style="font-size:${cachedResponsiveFontSize(countdownFontSize(p))};font-weight:800;line-height:1;white-space:nowrap;">${esc(text)}</div></div>`; }
 else if(t==='video') { const list=Array.isArray(p.playlist)&&p.playlist.length?p.playlist:(p.url?[{url:p.url}]:[]),u=String((list[0]||{}).url||''); const fit=['cover','contain','fill','none','scale-down'].includes(p.fit)?p.fit:'cover'; inner=`<video data-video-widget-id="${esc(w.id||'')}" src="${esc(cachedAssetUrl(u))}" autoplay playsinline preload="auto" ${p.loop !== false && list.length === 1 ? 'loop' : ''} ${p.muted||p.mute?'muted':''} style="width:100%;height:100%;object-fit:${fit};background:#000;"></video>`;}
 else if(t==='image') inner=`<img src="${esc(cachedAssetUrl(p.url||p.src||''))}" style="width:100%;height:100%;object-fit:${p.fit||'cover'};">`;
 else if(t==='camera') { if(w._addon_type==='rtsp-stream'||p._addon_type==='rtsp-stream'){const fit=['cover','contain','fill'].includes(p.fit)?p.fit:'cover'; inner=`<div style="position:relative;width:100%;height:100%;background:#020617;overflow:hidden;"><img data-camera-stream-widget-id="${esc(w.id||'')}" src="/camera-stream.mjpeg?widget_id=${encodeURIComponent(String(w.id||''))}" style="width:100%;height:100%;object-fit:${fit};background:#020617;"></div>`;}else{const cameraFilter=cachedCameraFilterName(p.filter);const facing=['user','environment','left','right'].includes(p.facing_mode)?p.facing_mode:'environment';if(p.capture_enabled===true) inner=`<div style="position:relative;width:100%;height:100%;background:#020617;color:#cbd5e1;"><video data-camera-widget-id="${esc(w.id||'')}" data-camera-filter="${cameraFilter}" autoplay playsinline muted style="width:100%;height:100%;object-fit:cover;background:#020617;filter:${cachedCameraFilterCss(cameraFilter)};"></video>${cachedCameraFilterOverlay(cameraFilter,w.id)}<span style="position:absolute;left:.5rem;bottom:.5rem;padding:.2rem .45rem;border-radius:.25rem;background:rgba(2,6,23,.75);font:600 12px system-ui;">Camera · ${esc(facing)}</span></div>`;else inner=`<div data-camera-widget-id="${esc(w.id||'')}" style="width:100%;height:100%;display:grid;place-items:center;border:1px dashed #334155;background:#020617;color:#94a3b8;font:600 14px system-ui;text-align:center;">📷 Camera preview<br><small style="font-weight:400;">capture disabled</small></div>`;}}
 else if(t==='video') { const list=Array.isArray(p.playlist)&&p.playlist.length?p.playlist:(p.url?[{url:p.url}]:[]), u=String((list[0]||{}).url||''); if(isYouTubeUrl(u)){ inner=`<iframe src="${esc(youtubeEmbedUrl(u,Object.assign({},p,{muted:true})))}" allow="autoplay; encrypted-media; picture-in-picture" allowfullscreen style="width:100%;height:100%;border:0;background:#000;"></iframe>`; } else { const fit=['cover','contain','fill','none','scale-down'].includes(p.fit)?p.fit:'cover'; inner=`<video src="${esc(cachedAssetUrl(u))}" autoplay playsinline preload="auto" ${p.loop !== false && list.length === 1 ? 'loop' : ''} ${p.muted||p.mute?'muted':''} style="width:100%;height:100%;object-fit:${fit};background:#000;"></video>`; } }
 else if(t==='clock') inner=`<div class="clock" data-clock data-clock-format="${esc(p.format||'HH:mm')}" style="color:${p.color||'#fff'};font-family:${cachedFontFamily(p)};font-size:${cachedResponsiveFontSize(Number(p.font_size||p.fontSize||48))}"></div>`;
 else if(t==='datetime'||t==='date') inner=`<div class="date" data-date data-date-format="${esc(p.format||'dddd, MMMM D')}" style="color:${p.color||'#fff'};font-family:${cachedFontFamily(p)};font-size:${cachedResponsiveFontSize(Number(p.font_size||p.fontSize||36))}"></div>`;
 else if(t==='traffic'){const all=cachedTrafficEvents(p),key=p.traffic_variable||'traffic_summary',events=key==='traffic_crashes_closures'?all.filter(e=>{const kind=cachedTrafficKind(e);return kind.includes('crash')||kind.includes('closure');}):all,limit=Math.max(1,Math.min(10,Number(p.max_events)||3)),summary=events.length||p.hide_if_empty===false?cachedTrafficVariableText(p,key):'',rows=events.slice(0,limit).map(e=>`<div style="padding:.35rem 0;border-bottom:1px solid rgba(148,163,184,.25);"><b>${esc(e.category||'Event')}</b> · ${esc(e.road||'Road')} ${esc(e.direction||'')}<br><small>${esc(e.description||'')}</small></div>`).join(''),body=rows||(p._traffic_data&&p._traffic_data.available===false?'Traffic data unavailable':p.hide_if_empty===false?'<div>No active traffic events</div>':'');inner=`<div style="width:100%;height:100%;box-sizing:border-box;overflow:hidden;padding:.6rem;color:#fff;background:rgba(30,41,59,.55);font-family:system-ui,sans-serif;"><div style="font-weight:700;">🚧 ${esc(p.area_label||'Traffic')}</div>${summary?`<div style="font-size:1.15em;font-weight:700;margin-top:.35rem;">${esc(summary)}</div>`:''}<div style="font-size:.85em;margin-top:.4rem;">${body}</div></div>`;}
 else if(t==='rectangle') inner=`<div style="width:100%;height:100%;background:${p.color||'#e94560'};opacity:${cachedOpacity(p.opacity)};border-radius:${p.radius||0}px;"></div>`;
 else if(t==='webpage') inner=`<iframe src="${esc(p.url||'about:blank')}" style="width:100%;height:100%;border:0;"></iframe>`;
 else if(t==='weather') inner=`<div style="width:100%;height:100%;display:grid;place-items:center;color:${p.color||'#fff'};font-size:4rem;">${esc(p.location||'Weather')}</div>`;
 else if(t==='calendar') inner=`<div style="width:100%;height:100%;display:grid;place-items:center;color:${p.color||'#fff'};font-size:4rem;">${esc(p.label||'Calendar')}</div>`;
 else inner=`<div style="width:100%;height:100%;display:grid;place-items:center;border:1px dashed #334155;color:#94a3b8;">${esc(t||'widget')}</div>`;
 if(t==='countdown'){const mode=p.countdown_mode==='duration'?'duration':'date',started=Date.now(),durationValue=Math.max(1,Number(p.duration_value||1)),durationUnit=p.duration_unit||'minutes',target=p.targetDate||'',fake={getAttribute:(name)=>({'data-countdown-mode':mode,'data-countdown-duration-value':durationValue,'data-countdown-duration-unit':durationUnit,'data-countdown-started-at':started,'data-countdown-target-date':target,'data-countdown-expiry-text':p.expiry_text||'Time\u2019s up!','data-countdown-invalid-text':'Choose a date and time'}[name]||'')},text=countdownText(fake,started);inner=`<div data-countdown-widget data-countdown-mode="${esc(mode)}" data-countdown-duration-value="${durationValue}" data-countdown-duration-unit="${esc(durationUnit)}" data-countdown-started-at="${started}" data-countdown-target-date="${esc(target)}" data-countdown-expiry-text="${esc(p.expiry_text||'Time\u2019s up!')}" style="width:100%;height:100%;display:flex;align-items:center;justify-content:center;color:${p.color||'#fff'};font-family:${cachedFontFamily(p)};text-align:center;overflow:hidden;"><div class="countdown-value" style="font-size:${cachedResponsiveFontSize(countdownFontSize(p))};font-weight:800;line-height:1;white-space:nowrap;">${esc(text)}</div></div>`;}
 const outputWidget=w;
 return `<div class="widget" style="${styleFor(outputWidget)}">${inner}</div>`;}
function tickClocks(){const now=new Date();document.querySelectorAll('[data-clock]').forEach(e=>e.textContent=cachedClockText(now,e.getAttribute('data-clock-format')||'HH:mm'));document.querySelectorAll('[data-date]').forEach(e=>e.textContent=cachedDateText(now,e.getAttribute('data-date-format')||'dddd, MMMM D'));tickCountdowns();}
function applyOrientation(value){const v=String(value||'normal');document.body.dataset.orientation=['normal','flipped','rotated_left','rotated_right'].includes(v)?v:'normal';}
async function loadLayout(){try{const r=await fetch('/assigned-layout.json?ts='+Date.now(),{cache:'no-store'});if(!r.ok)throw new Error('no assigned layout');const txt=await readTextBounded(r,4*1024*1024);try{const c=await fetch('/api/client-config.json?ts='+Date.now(),{cache:'no-store'});if(c.ok)applyOrientation(JSON.parse(await readTextBounded(c,256*1024)).orientation);}catch(_e){}if(txt===currentHash)return;currentHash=txt;const layout=JSON.parse(txt);const layoutId=String(layout.id||layout.layout_id||'');const ws=(layout.widgets||[]).map(w=>Object.assign({_layout_id:layoutId},w)).slice().sort((a,b)=>(a.z||0)-(b.z||0));const rendered=await Promise.all(ws.map(renderWidget));document.getElementById('player').innerHTML=rendered.join('')||'<div class="fallback"><div><strong>Empty Layout</strong></div></div>';tickClocks();initCachedVideoWidgets(ws);initCachedCameraWidgets(ws);initCachedCameraStreamWidgets(ws);}catch(e){if(!currentHash)document.getElementById('player').innerHTML='<div class="fallback"><div><strong>Waiting for cached assigned layout</strong><span>'+esc(e.message)+'</span></div></div>';}}
setInterval(tickClocks,1000);setInterval(loadLayout,5000);setInterval(()=>{currentHash='';loadLayout().catch(()=>{});},15*60*1000);loadLayout();

// === Cursor auto-hide (3.5s default, configurable via /api/cursor-config.json) ===
let __cursorIdle = Date.now();
const __cursorShow = () => { __cursorIdle = Date.now(); document.documentElement.style.cursor = 'default'; };
['pointermove','pointerdown','pointerup','mousemove','mousedown','mouseup','touchstart','touchmove','wheel','keydown','scroll'].forEach(ev =>
  window.addEventListener(ev, __cursorShow, { capture: true, passive: true }));
let __cursorHideSecs = 3.5;
async function __cursorLoadConfig() {
  try {
    const r = await fetch('/api/cursor-config.json', { cache: 'no-store' });
    if (r.ok) { const c = JSON.parse(await readTextBounded(r, 64 * 1024)); __cursorHideSecs = Number(c.hide_secs) || 3.5; }
  } catch (e) {}
}
setInterval(() => {
  if (Date.now() - __cursorIdle > __cursorHideSecs * 1000) {
    document.documentElement.style.cursor = 'none';
  }
}, 200);
__cursorLoadConfig();
</script></body></html>'''


# Live-server redirect: when the configured Screentra Cast server is reachable, the
# kiosk browser should hit the server's /player.html so it gets the real
# widget renderers (weather/calendar/clock/iframe/...). When the server is
# unreachable, fall back to the cached PLAYER_HTML so the player still shows
# the last cached layout offline.
LIVE_PROBE_TIMEOUT_SECS = float(os.environ.get('RIPLEY_LIVE_PROBE_TIMEOUT', '0.6'))
ENABLE_LIVE_PLAYER_REDIRECT = os.environ.get('SCREENTRA_LIVE_PLAYER_REDIRECT', '').strip().lower() in ('1', 'true', 'yes')

# Keep the no-layout diagnostic page available at /no-layout.html for later
# re-enablement, but do not replace a working display with it during the
# temporary stale-assignment policy.
# The client-local admin webpage is retired. Keep the implementation in this
# source for rollback, but do not expose its routes from any renderer port.
LOCAL_ADMIN_ENABLED = False
SHOW_NO_LAYOUT_SCREEN = False


def _live_player_url() -> str | None:
    """Return the server URL the kiosk should redirect to, or None if offline."""
    server = os.environ.get('RIPLEY_SERVER_URL', '').strip()
    if not server:
        try:
            server_path = Path.home() / '.local/share/screentra-client/server'
            if server_path.exists():
                server = _read_real_file(server_path, 4096).decode('utf-8').strip()
        except Exception:
            pass
    server = server.rstrip('/')
    if not server or not (server.startswith('http://') or server.startswith('https://')):
        return None
    return server


def _client_id_from_env() -> str | None:
    cid = os.environ.get('RIPLEY_CLIENT_ID', '').strip()
    return cid or None


def _client_id_from_cache() -> str | None:
    try:
        cid_path = Path.home() / '.local/share/screentra-client/client-id'
        if cid_path.exists():
            v = _read_real_file(cid_path, 4096).decode('utf-8').strip()
            return v or None
    except Exception:
        pass
    return None


def _connector_for_display_index(display_index: int) -> str | None:
    """Return the connector name (e.g. 'DP-2') for a given display_index.

    Sources (tried in order):
      1. The display-governance mapping in config.json.
      2. /sys/class/drm (live kernel enumeration, fallback if config.json
         is missing or stale).
    Returns None when no physical connector matches.
    """
    try:
        gov_path = Path.home() / '.local/share/screentra-client/config.json'
        if gov_path.exists():
            cfg = _read_json_file(gov_path, {})
            for d in (cfg.get('last_detected_displays') or []):
                if int(d.get('display_index', -1)) == int(display_index):
                    con = d.get('connector')
                    if con and con != 'virtual-0':
                        return con
    except Exception:
        pass
    try:
        sysfs_root = Path('/sys/class/drm')
        if sysfs_root.is_dir():
            cards = []
            for d in sorted(sysfs_root.iterdir()):
                if d.name.startswith('card') and '-' in d.name:
                    status_path = d / 'status'
                    if not status_path.exists():
                        continue
                    if _read_bounded_path(status_path, 64).strip() != 'connected':
                        continue
                    mode_path = d / 'modes'
                    if mode_path.exists():
                        cards.append(d.name)
            cards.sort()
            if 0 <= display_index < len(cards):
                # card0-DP-2 → 'DP-2'
                name = cards[display_index]
                if '-' in name and name.split('-', 1)[1].startswith('card') is False:
                    return name.split('-', 1)[1]
                return name
    except Exception:
        return None
    return None


def _server_reachable(server_url: str, timeout: float) -> bool:
    """Probe the server with a short HTTPS GET on /api/version. Cheap and fast."""
    import ssl as _ssl
    import socket as _socket
    import urllib.request as _ur
    from urllib.parse import urlparse as _urlparse
    try:
        u = _urlparse(server_url)
        host = u.hostname
        port = u.port or (443 if u.scheme == 'https' else 80)
        path = u.path.rstrip('/') + '/api/version'
        # Cheap TCP reachability check first
        try:
            with _socket.create_connection((host, port), timeout=timeout):
                pass
        except Exception:
            return False
        ctx = None
        if u.scheme == 'https':
            ca_candidates = []
            env_ca = os.environ.get('RIPLEY_CA_CERT')
            if env_ca:
                ca_candidates.append(Path(env_ca))
            ca_candidates.append(Path.home() / '.local/share/screentra-client/screentra-cast-signage-ca.crt')
            for cafile in ca_candidates:
                if cafile.exists():
                    try:
                        ctx = _ssl.create_default_context(cafile=str(cafile))
                        break
                    except Exception:
                        ctx = None
            # No CA available: fail closed and keep the cached fallback page.
            if ctx is None:
                return False
        req = _ur.Request((u.scheme + '://' + (host if not u.port else f'{host}:{port}') + path), method='GET')
        with _ur.urlopen(req, timeout=timeout, context=ctx) as r:
            return 200 <= r.status < 500
    except _ur.HTTPError as exc:
        # /api/version is authenticated; a 401 still proves the server and
        # pinned TLS connection are reachable, so let the caller continue to
        # the client-token capability exchange.
        return 200 <= exc.code < 500
    except Exception:
        return False


def _client_auth_token(cache_dir: Path) -> str:
    try:
        return _read_real_file(cache_dir.parent / 'client-auth-token', 4096).decode('utf-8').strip()
    except Exception:
        return ''


def _heartbeat_has_live_assignment(cache_dir: Path) -> bool:
    """Allow live-player redirect only after a recent valid assignment sync."""
    try:
        status = _read_json_file(cache_dir / 'last-heartbeat.json', {})
        return status.get('connection') == 'online' and bool(status.get('assigned_layout_id'))
    except Exception:
        return False


def _fetch_player_capability(server_url: str, client_id: str, display_index: int, cache_dir: Path) -> str | None:
    """Exchange the local client credential for a short-lived browser token."""
    token = _client_auth_token(cache_dir)
    if not token:
        return None
    import ssl as _ssl
    import urllib.request as _ur
    try:
        parsed = urllib.parse.urlparse(server_url)
        if parsed.scheme not in ('http', 'https') or not parsed.hostname:
            return None
        base = server_url.rstrip('/')
        endpoint = f'{base}/api/clients/{urllib.parse.quote(client_id, safe="")}/player-capability/{display_index}'
        req = _ur.Request(endpoint, headers={'X-SCREENTRA-CLIENT-TOKEN': token}, method='GET')
        ctx = None
        if parsed.scheme == 'https':
            for cafile in (
                os.environ.get('RIPLEY_CA_CERT', ''),
                str(Path.home() / '.local/share/screentra-client/screentra-cast-signage-ca.crt'),
            ):
                if cafile and Path(cafile).exists():
                    try:
                        ctx = _ssl.create_default_context(cafile=cafile)
                        break
                    except Exception:
                        pass
            if ctx is None:
                return None
        with _ur.urlopen(req, timeout=LIVE_PROBE_TIMEOUT_SECS, context=ctx) as response:
            raw = response.read(32 * 1024 + 1)
            if len(raw) > 32 * 1024:
                return None
        value = json.loads(raw.decode('utf-8')).get('capability')
        return value if isinstance(value, str) and 64 <= len(value) <= 16 * 1024 else None
    except Exception:
        return None


def _live_player_redirect_html(server_url: str, display_index: int, connector: str | None = None, client_id: str | None = None, capability: str | None = None, renderer_url: str | None = None) -> str:
    """Return a redirect to the live server for a physical connector."""
    if not connector or connector == 'virtual-0':
        raise ValueError('physical display connector is required')
    safe_server = server_url.replace('"', '%22')
    target = f'{safe_server.rstrip("/")}/player.html?display={urllib.parse.quote(connector)}'
    if client_id:
        target += f'&client_id={urllib.parse.quote(client_id, safe="")}'
    if capability:
        target += f'#player_capability={urllib.parse.quote(capability, safe="")}'
        if renderer_url:
            target += f'&renderer_url={urllib.parse.quote(renderer_url, safe="")}'
    return (
        '<!doctype html><html><head>'
        f'<meta http-equiv="refresh" content="0; url={target}">'
        f'<title>Screentra Cast Live Player</title>'
        '<style>html,body{margin:0;height:100%;background:#000;color:#94a3b8;font-family:system-ui,sans-serif;}'
        '.wrap{display:grid;place-items:center;height:100%}</style>'
        '</head><body><div class="wrap">'
        f'<div>Connecting to live player…</div>'
        f'<script>window.location.replace({json.dumps(target)});</script>'
        '</div></body></html>'
    )


def _camera_layout_path(cache_dir: Path) -> Path:
    display_index = int(os.environ.get('RIPLEY_DISPLAY_INDEX', '0'))
    target = cache_dir / f'assigned-layout-display-{display_index}.json'
    if display_index == 0 and not target.exists():
        target = cache_dir / 'assigned-layout.json'
    return target


def _camera_stream_url(cache_dir: Path, widget_id: str) -> str | None:
    if not isinstance(widget_id, str) or not widget_id or len(widget_id.encode('utf-8')) > CAMERA_WIDGET_ID_BYTES:
        return None
    try:
        layout = _read_json_file(_camera_layout_path(cache_dir), {})
        widgets = layout.get('widgets', [])
        if not isinstance(widgets, list):
            return None
        widget = next((item for item in widgets if isinstance(item, dict) and str(item.get('id', '')) == widget_id), None)
        props = widget.get('props', {}) if isinstance(widget, dict) else {}
        raw_url = props.get('stream_url') if isinstance(props, dict) else None
        if not isinstance(raw_url, str) or not raw_url or len(raw_url.encode('utf-8')) > CAMERA_STREAM_URL_BYTES:
            return None
        if any(character in raw_url for character in '\r\n\x00'):
            return None
        parsed = urllib.parse.urlparse(raw_url)
        if parsed.scheme.lower() not in ('rtsp', 'rtsps') or not parsed.hostname:
            return None
        try:
            _ = parsed.port
        except ValueError:
            return None
        return raw_url
    except Exception:
        return None


def _camera_stream_props(cache_dir: Path, widget_id: str) -> dict:
    try:
        layout = _read_json_file(_camera_layout_path(cache_dir), {})
        widgets = layout.get('widgets', []) if isinstance(layout, dict) else []
        if not isinstance(widgets, list):
            return {}
        widget = next((item for item in widgets if isinstance(item, dict) and str(item.get('id', '')) == widget_id), None)
        props = widget.get('props', {}) if isinstance(widget, dict) else {}
        return dict(props) if isinstance(props, dict) else {}
    except Exception:
        return {}


def _camera_layout_has_rtsp_widget(cache_dir: Path) -> bool:
    try:
        layout = _read_json_file(_camera_layout_path(cache_dir), {})
        widgets = layout.get('widgets', [])
        return isinstance(widgets, list) and any(
            isinstance(item, dict)
            and (item.get('_addon_type') == 'rtsp-stream'
                 or isinstance(item.get('props'), dict)
                 and item['props'].get('_addon_type') == 'rtsp-stream')
            for item in widgets
        )
    except Exception:
        return False


def _camera_layout_has_stream(cache_dir: Path) -> bool:
    layout = _read_json_file(_camera_layout_path(cache_dir), {})
    widgets = layout.get('widgets', []) if isinstance(layout, dict) else []
    if not isinstance(widgets, list):
        return False
    return any(
        _camera_stream_url(cache_dir, str(item.get('id', '')))
        for item in widgets
        if isinstance(item, dict) and item.get('id')
    )


def _camera_public_layout(layout: dict) -> dict:
    """Remove RTSP/RTSPS source values before serving layout JSON to Chromium."""
    if not isinstance(layout, dict):
        return {}
    public = dict(layout)
    widgets = []
    for item in layout.get('widgets', []):
        if not isinstance(item, dict):
            widgets.append(item)
            continue
        widget = dict(item)
        props = widget.get('props')
        widget_type = str(widget.get('widget_type') or widget.get('type') or '').lower().replace('_', '-')
        addon_type = widget.get('_addon_type')
        if widget_type == 'camera' or addon_type == 'rtsp-stream' or (isinstance(props, dict) and props.get('_addon_type') == 'rtsp-stream'):
            if isinstance(props, dict):
                props = dict(props)
                props.pop('stream_url', None)
                widget['props'] = props
            widget.pop('stream_url', None)
        widgets.append(widget)
    public['widgets'] = widgets
    return public


def _proxy_live_data_request(path: str, query: str, cache_dir: Path):
    """Proxy safe live data APIs through the local renderer.

    RTSP layouts intentionally stay on the local cached renderer so the
    renderer can access the protected stream URL.  That renderer must still
    provide the server-backed weather and calendar APIs; relative browser
    fetches otherwise hit port 3900, where those routes do not exist.
    """
    if path not in ('/api/weather/forecast', '/api/ics-proxy', '/api/traffic/events'):
        return None
    server = _live_player_url()
    if not server:
        return None
    parsed = urllib.parse.urlparse(server)
    if parsed.scheme not in ('http', 'https') or not parsed.hostname:
        return None
    endpoint = server.rstrip('/') + path
    if query:
        endpoint += '?' + query
    headers = {'Cache-Control': 'no-cache'}
    token = _client_auth_token(cache_dir)
    if token:
        headers['X-SCREENTRA-CLIENT-TOKEN'] = token
    request = urllib.request.Request(endpoint, headers=headers, method='GET')
    context = None
    if parsed.scheme == 'https':
        for cafile in (
            os.environ.get('RIPLEY_CA_CERT', ''),
            str(Path.home() / '.local/share/screentra-client/screentra-cast-signage-ca.crt'),
        ):
            if cafile and Path(cafile).exists():
                try:
                    context = ssl.create_default_context(cafile=cafile)
                    break
                except Exception:
                    context = None
        if context is None:
            return None
    with urllib.request.urlopen(request, timeout=20, context=context) as response:
        body = response.read(MAX_DATA_PROXY_BYTES + 1)
        if len(body) > MAX_DATA_PROXY_BYTES:
            raise ValueError('proxied data response exceeds its size limit')
        content_type = response.headers.get('Content-Type', 'application/octet-stream')
        return response.status, content_type, body


def _proxy_weather_icon_asset(path: str, cache_dir: Path):
    """Proxy only the vendored weather icon assets needed by offline playback."""
    prefix = '/assets/weather/meteocons/'
    if not path.startswith(prefix):
        return None
    relative = path[len(prefix):]
    parts = relative.split('/')
    if len(parts) != 2 or parts[0] not in ('fill', 'fill-static') or not parts[1].endswith('.svg'):
        return None
    slug = parts[1][:-4]
    if slug not in WEATHER_ICON_SLUGS:
        return None
    server = _live_player_url()
    if not server:
        return None
    parsed = urllib.parse.urlparse(server)
    if parsed.scheme not in ('http', 'https') or not parsed.hostname:
        return None
    request = urllib.request.Request(server.rstrip('/') + path, headers={'Cache-Control': 'no-cache'}, method='GET')
    context = None
    if parsed.scheme == 'https':
        for cafile in (
            os.environ.get('RIPLEY_CA_CERT', ''),
            str(Path.home() / '.local/share/screentra-client/screentra-cast-signage-ca.crt'),
        ):
            if cafile and Path(cafile).exists():
                try:
                    context = ssl.create_default_context(cafile=cafile)
                    break
                except Exception:
                    context = None
        if context is None:
            return None
    with urllib.request.urlopen(request, timeout=10, context=context) as response:
        body = response.read(256 * 1024 + 1)
        if len(body) > 256 * 1024:
            raise ValueError('weather icon asset exceeds its size limit')
        return response.status, response.headers.get('Content-Type', 'image/svg+xml'), body


def _terminate_camera_process(process) -> None:
    if process is None:
        return
    if process.poll() is not None:
        try:
            process.wait(timeout=0.5)
        except Exception:
            pass
        return
    try:
        if os.name == 'posix':
            os.killpg(process.pid, signal.SIGTERM)
        else:
            process.terminate()
        process.wait(timeout=1.5)
    except Exception:
        try:
            if os.name == 'posix':
                os.killpg(process.pid, signal.SIGKILL)
            else:
                process.kill()
            process.wait(timeout=1)
        except Exception:
            pass


def _camera_stream_command(stream_url: str, props: dict | None = None) -> list[str]:
    # Chromium does not decode RTSP/RTSPS directly. FFmpeg converts the one
    # configured stream to a bounded-rate multipart JPEG response, while the
    # URL remains server-side in the cached layout and is never placed in DOM.
    # Per-widget settings are clamped here as a defense-in-depth boundary.
    props = props if isinstance(props, dict) else {}
    try:
        camera_max_fps = max(1, min(30, int(os.environ.get('SCREENTRA_CAMERA_MAX_FPS', '30'))))
    except (TypeError, ValueError):
        camera_max_fps = 30
    try:
        output_fps = max(1, min(camera_max_fps, int(props.get('output_fps', CAMERA_STREAM_FPS))))
    except (TypeError, ValueError):
        output_fps = min(CAMERA_STREAM_FPS, camera_max_fps)
    try:
        camera_max_width = max(320, min(1920, int(os.environ.get('SCREENTRA_CAMERA_MAX_WIDTH', '1920'))))
    except (TypeError, ValueError):
        camera_max_width = 1920
    try:
        output_width = max(320, min(camera_max_width, int(props.get('output_width', 960))))
    except (TypeError, ValueError):
        output_width = min(960, camera_max_width)
    # Synthetic interpolation is opt-in. With the VA-API path, minterpolate runs
    # after hwdownload and can stall the MJPEG relay under load; plain fps
    # output keeps the Radeon decode/scale path continuous and duplicates only as needed.
    interpolate = props.get('frame_interpolation', False) is True and output_fps > 1
    hwaccel = os.environ.get('SCREENTRA_FFMPEG_HWACCEL', '').strip().lower()
    vaapi_device = os.environ.get('SCREENTRA_VAAPI_DEVICE', '/dev/dri/renderD129').strip()
    use_vaapi = hwaccel == 'vaapi' and vaapi_device.startswith('/dev/dri/')
    if use_vaapi:
        # Keep interpolation in system memory because minterpolate is not a
        # VA-API filter. Decode and resize on the Radeon, then download once for
        # fps/interpolation and the CPU MJPEG encoder.
        gpu_scale = f'scale_vaapi=w={output_width}:h=-2:force_original_aspect_ratio=decrease,hwdownload,format=nv12'
        if interpolate:
            source_fps = max(1, (output_fps + 1) // 2)
            video_filter = (
                f'{gpu_scale},fps={source_fps},'
                f'minterpolate=fps={output_fps}:mi_mode=mci:mc_mode=aobmc:me_mode=bidir:vsbmc=1,format=yuv420p'
            )
        else:
            video_filter = f'{gpu_scale},fps={output_fps},format=yuv420p'
    elif interpolate:
        source_fps = max(1, (output_fps + 1) // 2)
        video_filter = (
            f'fps={source_fps},scale={output_width}:-2:force_original_aspect_ratio=decrease,'
            f'minterpolate=fps={output_fps}:mi_mode=mci:mc_mode=aobmc:me_mode=bidir:vsbmc=1,format=yuv420p'
        )
    else:
        video_filter = f'fps={output_fps},scale={output_width}:-2:force_original_aspect_ratio=decrease'
    ffmpeg_bin = os.environ.get('SCREENTRA_FFMPEG_BIN') or shutil.which('ffmpeg') or 'ffmpeg'
    command = [
        ffmpeg_bin,
        '-nostdin',
        '-hide_banner',
        '-loglevel', 'error',
        '-fflags', 'nobuffer',
        '-flags', 'low_delay',
        '-analyzeduration', '1000000',
        '-probesize', '1000000',
        '-rtsp_transport', 'tcp',
        '-timeout', '10000000',
    ]
    if use_vaapi:
        command.extend(['-hwaccel', 'vaapi', '-hwaccel_device', vaapi_device, '-hwaccel_output_format', 'vaapi'])
    command.extend([
        '-i', stream_url,
        '-an',
        '-vf', video_filter,
        '-q:v', '8',
        '-fps_mode', 'vfr',
        '-f', 'mpjpeg',
        'pipe:1',
    ])
    return command


def _serve_camera_stream(handler, widget_id: str) -> None:
    stream_url = _camera_stream_url(handler.cache_dir, widget_id)
    ffmpeg_bin = os.environ.get('SCREENTRA_FFMPEG_BIN') or shutil.which('ffmpeg')
    if not stream_url or not ffmpeg_bin:
        return handler.send_text(503, 'camera stream unavailable')
    stream_props = _camera_stream_props(handler.cache_dir, widget_id)
    process = None
    try:
        process = _sp.Popen(
            _camera_stream_command(stream_url, stream_props),
            stdin=_sp.DEVNULL,
            stdout=_sp.PIPE,
            stderr=_sp.DEVNULL,
            start_new_session=True,
            close_fds=True,
        )
        handler.send_response(200)
        handler.send_header('Content-Type', 'multipart/x-mixed-replace; boundary=ffmpeg')
        handler.send_header('Cache-Control', 'no-store, no-cache, must-revalidate')
        handler.send_header('Pragma', 'no-cache')
        handler.send_header('Connection', 'close')
        handler.end_headers()
        while True:
            chunk = process.stdout.read(64 * 1024) if process.stdout else b''
            if not chunk:
                break
            handler.wfile.write(chunk)
            handler.wfile.flush()
    except (BrokenPipeError, ConnectionResetError, TimeoutError):
        pass
    except Exception:
        # Never send the source URL or subprocess diagnostics to the browser
        # or renderer log. The next image reconnect will retry the stream.
        pass
    finally:
        if process is not None:
            _terminate_camera_process(process)
            try:
                if process.stdout:
                    process.stdout.close()
            except Exception:
                pass


class Handler(BaseHTTPRequestHandler):
    cache_dir = Path.home() / '.local/share/screentra-client/cache'

    def do_POST(self):
        path = unquote(self.path.split('?', 1)[0])
        if not LOCAL_ADMIN_ENABLED and (path == '/login' or path == '/login.html' or path.startswith('/api/admin')):
            return self.send_text(404, 'not found')
        if not _csrf_allows_mutation(self.headers):
            return self.send_text(403, json.dumps({'error': 'cross-origin mutation blocked'}), 'application/json')
        try:
            body = _request_body(self)
        except OverflowError:
            return self.send_text(413, json.dumps({'error': 'request body too large'}), 'application/json')
        except ValueError as exc:
            return self.send_text(400, json.dumps({'error': str(exc)}), 'application/json')
        if _mutation_requires_auth(path) and not _session_user(self.headers.get('Cookie', '')):
            return self.send_text(401, json.dumps({'error': 'auth required', **_auth_source()}), 'application/json')
        if path == '/api/admin/auth/login':
            try:
                payload = _parse_request_json(body) if body and body.strip() else {}
                _require_request_fields(payload, {'username', 'password'})
                username, password = _validate_login_payload(payload)
                ok, info = _check_login(username, password)
                if not ok:
                    return self.send_text(401, json.dumps({'ok': False, **info}), 'application/json')
                token = _make_session(info.get('username') or username, info.get('source') or 'fallback_default')
                data = json.dumps({'ok': True, **info}).encode()
                self.send_response(200)
                self.send_header('Content-Type', 'application/json')
                self.send_header('Content-Length', str(len(data)))
                self.send_header('Cache-Control', 'no-store')
                self.send_header('Set-Cookie', 'ripley_admin_session=' + token + '; Path=/; HttpOnly; SameSite=Lax; Max-Age=43200')
                self.end_headers(); self.wfile.write(data); return
            except Exception as e:
                return self.send_text(400, json.dumps({'ok': False, 'error': _bound_text(str(e), MAX_ERROR_BYTES)}), 'application/json')
        if path == '/api/admin/auth/logout':
            data = json.dumps({'ok': True}).encode()
            self.send_response(200)
            self.send_header('Content-Type', 'application/json')
            self.send_header('Content-Length', str(len(data)))
            self.send_header('Cache-Control', 'no-store')
            self.send_header('Set-Cookie', 'ripley_admin_session=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0')
            self.end_headers(); self.wfile.write(data); return
        if _admin_requires_auth(path) and not _session_user(self.headers.get('Cookie', '')):
            return self.send_text(401, json.dumps({'error': 'auth required', **_auth_source()}), 'application/json')
        if path == '/api/touchscreen/select':
            try:
                payload = _parse_request_json(body)
                _require_request_fields(payload, {'path'})
                selected = payload.get('path')
                _validate_touch_device_path(selected)
                _ensure_real_directory(Handler.cache_dir, create=True)
                _atomic_write_text(Handler.cache_dir / 'touch-device.json', json.dumps({'path': selected}))
                # Tell the actor to restart the kiosk so the choice takes effect
                try:
                    pending = Handler.cache_dir.parent / 'pending-command.json'
                    _atomic_write_text(pending, json.dumps({
                        'action': 'restart_kiosk_display',
                        'args': {'display_index': int(os.environ.get('RIPLEY_DISPLAY_INDEX', '0')), 'reason': 'touch-device-change'},
                        'queued_at': datetime.utcnow().isoformat() + 'Z',
                    }))
                except Exception:
                    pass
                return self.send_text(200, json.dumps({'saved': True, 'path': selected}), 'application/json')
            except Exception as e:
                return self.send_text(500, json.dumps({'error': _bound_text(str(e), MAX_ERROR_BYTES)}), 'application/json')
        if path == '/api/touch-calibration.json':
            try:
                data = _parse_request_json(body) if body and body.strip() else {}
                data.setdefault('received_at', _iso_now())
                # Tag per-display calibration results so the heartbeat
                # can attribute intermittent failures to a specific
                # screen when there are multiple on the same box.
                data.setdefault('display_index', int(os.environ.get('RIPLEY_DISPLAY_INDEX', '0')))
                calib_path = Handler.cache_dir / 'touch-calibration.json'
                _ensure_real_directory(calib_path.parent, create=True)
                history = []
                if calib_path.exists():
                    try:
                        history = _read_json_file(calib_path, [])
                        if not isinstance(history, list):
                            history = [history]
                    except Exception:
                        history = []
                history.append(data)
                history = history[-5:]   # rolling window of last 5 runs
                _atomic_write_text(calib_path, json.dumps(history, indent=2))
                return self.send_text(200, json.dumps({'ok': True, 'history_size': len(history)}), 'application/json')
            except Exception as e:
                return self.send_text(400, json.dumps({'error': _bound_text(str(e), MAX_ERROR_BYTES)}), 'application/json')
        if path == '/api/admin/ca/refresh':
            return self.send_text(200, json.dumps(_refresh_ca_from_server()), 'application/json')
        if path == '/api/admin/device/reboot':
            try:
                payload = _parse_request_json(body) if body and body.strip() else {}
                _require_request_fields(payload, {'confirm'})
                result = _reboot_device(bool(payload.get('confirm')))
            except Exception as e:
                return self.send_text(400, json.dumps({'ok': False, 'error': _bound_text(str(e), MAX_ERROR_BYTES)}), 'application/json')
            return self.send_text(200 if result.get('ok') else 400, json.dumps(result), 'application/json')
        if path == '/api/admin/display-count':
            try:
                payload = _parse_request_json(body) if body and body.strip() else {}
                _require_request_fields(payload, {'max_displays'})
                value = payload.get('max_displays')
                result = _write_display_count(None if value in (None, '', 'auto') else int(value))
                return self.send_text(200, json.dumps(result), 'application/json')
            except Exception as e:
                return self.send_text(400, json.dumps({'ok': False, 'error': _bound_text(str(e), MAX_ERROR_BYTES)}), 'application/json')
        if path == '/api/admin/touch-binding':
            try:
                payload = _parse_request_json(body) if body and body.strip() else {}
                _require_request_fields(payload, {
                    'device_path', 'path', 'device_name', 'target_display_indexes',
                    'display_indexes', 'display_index', 'allow_joined_displays', 'requested_by',
                })
                result = _write_touch_binding(payload)
                return self.send_text(200, json.dumps(result), 'application/json')
            except Exception as e:
                return self.send_text(400, json.dumps({'ok': False, 'error': _bound_text(str(e), MAX_ERROR_BYTES)}), 'application/json')
        if path == '/api/admin/client-backup':
            result = _create_client_software_backup()
            return self.send_text(200 if result.get('ok') else 500, json.dumps(result), 'application/json')
        if path == '/api/admin/services/restart':
            r = _safe_run(['systemctl', '--user', 'restart', 'screentra-client-heartbeat.service', 'screentra-client-actor.service'], timeout=8)
            return self.send_text(200, json.dumps({'ok': r.get('ok'), 'restart': r}), 'application/json')
        # GET /api/client-config.json — current cached config (heartbeat's view of server state)
        if path == '/api/client-config.json':
            cfg_path = Handler.cache_dir / 'client-config.json'
            try:
                data = _read_json_file(cfg_path, {})
                return self.send_text(200, json.dumps(data), 'application/json')
            except Exception:
                return self.send_text(404, json.dumps({'error': 'no config yet'}), 'application/json')
        # POST /api/client-config.json — save theme (or other fields) to local config.
        # Heartbeat reads this on next cycle and sends it to the server.
        if method == 'POST' and path == '/api/client-config.json':
            try:
                payload = _parse_request_json(body) if body and body.strip() else {}
            except Exception:
                return self.send_text(400, json.dumps({'ok': False, 'error': 'invalid json'}), 'application/json')
            cfg_path = Handler.cache_dir / 'client-config.json'
            try:
                try:
                    data = _read_json_file(cfg_path, {})
                except Exception:
                    data = {}
                for k in ['theme']:
                    if k in payload:
                        data[k] = payload[k]
                _ensure_real_directory(cfg_path.parent, create=True)
                _atomic_write_text(cfg_path, json.dumps(data, indent=2))
                return self.send_text(200, json.dumps({'ok': True}), 'application/json')
            except Exception as e:
                return self.send_text(500, json.dumps({'ok': False, 'error': _bound_text(str(e), MAX_ERROR_BYTES)}), 'application/json')
        return self.send_text(404, json.dumps({'error': 'not found'}), 'application/json')
    def log_message(self, fmt, *args):
        pass
    def send_text(self, status, text, ctype='text/plain'):
        data = text.encode()
        self.send_response(status)
        self.send_header('Content-Type', ctype)
        self.send_header('Content-Length', str(len(data)))
        self.send_header('Cache-Control', 'no-store')
        self.end_headers()
        self.wfile.write(data)
    def do_GET(self):
        path = unquote(self.path.split('?',1)[0])
        if not LOCAL_ADMIN_ENABLED and (path == '/login' or path == '/login.html' or path == '/admin.html' or path.startswith('/api/admin')):
            return self.send_text(404, 'not found')
        if path == '/camera-stream.mjpeg':
            query = urllib.parse.parse_qs(urllib.parse.urlparse(self.path).query, keep_blank_values=False)
            widget_ids = query.get('widget_id', [])
            if len(widget_ids) != 1:
                return self.send_text(400, 'invalid camera stream request')
            return _serve_camera_stream(self, widget_ids[0])
        if path.startswith('/assets/weather/meteocons/'):
            try:
                proxied = _proxy_weather_icon_asset(path, self.cache_dir)
                if proxied is None:
                    return self.send_text(404, 'weather icon not found')
                status, content_type, data = proxied
                self.send_response(status)
                self.send_header('Content-Type', content_type)
                self.send_header('Content-Length', str(len(data)))
                self.send_header('Cache-Control', 'public, max-age=86400')
                self.end_headers()
                self.wfile.write(data)
                return
            except Exception:
                return self.send_text(502, 'weather icon unavailable')
        if path in ('/api/weather/forecast', '/api/ics-proxy', '/api/traffic/events'):
            try:
                query = urllib.parse.urlparse(self.path).query
                proxied = _proxy_live_data_request(path, query, self.cache_dir)
                if proxied is None:
                    return self.send_text(503, json.dumps({'error': 'live server unavailable'}), 'application/json')
                status, content_type, data = proxied
                self.send_response(status)
                self.send_header('Content-Type', content_type)
                self.send_header('Content-Length', str(len(data)))
                self.send_header('Cache-Control', 'no-store')
                self.end_headers()
                self.wfile.write(data)
                return
            except Exception:
                return self.send_text(502, json.dumps({'error': 'live data proxy failed'}), 'application/json')
        if path in ('/login', '/login.html'):
            return self.send_text(200, _admin_login_html(), 'text/html; charset=utf-8')
        if path == '/api/admin/auth/status':
            return self.send_text(200, json.dumps(_admin_auth_payload(self)), 'application/json')
        if _admin_requires_auth(path) and not _session_user(self.headers.get('Cookie', '')):
            if path.startswith('/api/'):
                return self.send_text(401, json.dumps({'error': 'auth required', **_auth_source()}), 'application/json')
            return self.send_text(200, _admin_login_html(), 'text/html; charset=utf-8')
        if os.environ.get('RIPLEY_ADMIN_MODE', '').lower() in ('1', 'true', 'yes') and path in ('/', '/admin.html'):
            return self.send_text(200, ADMIN_HTML, 'text/html; charset=utf-8')
        # GET /api/client-config.json — current cached heartbeat config (layout + theme)
        if path == '/api/client-config.json':
            cfg_path = Handler.cache_dir / 'client-config.json'
            try:
                data = _read_json_file(cfg_path, {})
                return self.send_text(200, json.dumps(data), 'application/json')
            except Exception:
                return self.send_text(404, json.dumps({'error': 'no config yet'}), 'application/json')
        if path in ('/', '/player.html'):
            display_index = int(os.environ.get('RIPLEY_DISPLAY_INDEX', '0'))
            layout_target = Handler.cache_dir / f'assigned-layout-display-{display_index}.json'
            # Legacy fallback belongs to display 0 only. Other displays must not render
            # display 0's cached assignment when their own display cache is absent.
            if display_index == 0 and not layout_target.exists():
                layout_target = Handler.cache_dir / 'assigned-layout.json'

            # No assigned layout on disk for THIS display -> show the rich
            # no-layout page (touch detection, gear modal, build info, exit
            # button) instead of redirecting to the server's minimal fallback
            # card. This gives the operator everything they need to configure
            # the touchscreen + pick a layout right from the screen.
            if not layout_target.exists() and SHOW_NO_LAYOUT_SCREEN:
                try:
                    no_layout_path = Path(__file__).with_name('no-layout.html')
                    html = _read_real_file(no_layout_path, MAX_STATIC_HTML_BYTES).decode('utf-8')
                    html = html.replace('<title>Screentra Cast Player display</title>', f'<title>Screentra Cast Player display {display_index}</title>')
                    return self.send_text(200, html, 'text/html; charset=utf-8')
                except Exception:
                    pass

            # When a layout is assigned AND the configured server is reachable,
            # redirect the kiosk to live /player.html so weather/calendar/clock/
            # image widgets render with full data via the server-side iframes.
            # Server's player.html looks up the display by *connector*
            # (DP-2/DP-4/...) rather than by index, so we read the connector
            # from the heartbeat's cached client-config.json when available;
            # fall back to the index.
            live_url = _live_player_url()
            if (ENABLE_LIVE_PLAYER_REDIRECT
                    and _heartbeat_has_live_assignment(self.cache_dir)
                    and not _camera_layout_has_rtsp_widget(self.cache_dir)
                    and live_url
                    and _server_reachable(live_url, LIVE_PROBE_TIMEOUT_SECS)):
                connector = _connector_for_display_index(display_index)
                if connector:
                    client_id = _client_id_from_env() or _client_id_from_cache()
                    capability = _fetch_player_capability(live_url, client_id, display_index, self.cache_dir) if client_id else None
                    if capability:
                        return self.send_text(200, _live_player_redirect_html(live_url, display_index, connector, client_id, capability, f'http://127.0.0.1:{self.server.server_port}/'), 'text/html; charset=utf-8')

            # No live server reachable (offline mode). Render the offline
            # cached layout from the renderer script's PLAYER_HTML constant.
            display_index = int(os.environ.get('RIPLEY_DISPLAY_INDEX', '0'))
            html = PLAYER_HTML.replace('<title>Screentra Cast Cached Player</title>', f'<title>Screentra Cast Display {display_index}</title>')
            html = html.replace('__SCREENTRA_SERVER_URL__', json.dumps(_live_player_url() or ''))
            return self.send_text(200, html, 'text/html; charset=utf-8')
        if path == '/admin.html':
            return self.send_text(200, ADMIN_HTML, 'text/html; charset=utf-8')
        if path == '/api/admin/status':
            return self.send_text(200, json.dumps(_admin_status()), 'application/json')
        if path == '/api/admin/certificates':
            return self.send_text(200, json.dumps(_cert_info()), 'application/json')
        if path == '/api/admin/client-backup/download':
            query = urllib.parse.parse_qs(urllib.parse.urlparse(self.path).query)
            fname = (query.get('file') or [''])[0]
            safe_name = Path(fname).name
            root = _client_backup_root()
            candidate = root / safe_name
            metadata = os.lstat(candidate) if candidate.exists() or candidate.is_symlink() else None
            if metadata is None or stat.S_ISLNK(metadata.st_mode) or not stat.S_ISREG(metadata.st_mode):
                return self.send_text(404, 'backup not found')
            target = candidate.resolve()
            try:
                target.relative_to(root.resolve())
            except Exception:
                return self.send_text(403, 'forbidden')
            try:
                data = _read_real_file(candidate, maximum=MAX_BACKUP_DOWNLOAD_BYTES)
            except ValueError as error:
                if 'exceeds' in str(error) or 'bounded' in str(error):
                    return self.send_text(413, 'backup too large')
                return self.send_text(403, 'forbidden')
            self.send_response(200)
            self.send_header('Content-Type', 'application/gzip')
            self.send_header('Content-Length', str(len(data)))
            self.send_header('Content-Disposition', 'attachment; filename="' + safe_name.replace('"', '') + '"')
            self.send_header('Cache-Control', 'no-store')
            self.end_headers(); self.wfile.write(data); return
        if path == '/api/admin/logs':
            query = urllib.parse.parse_qs(urllib.parse.urlparse(self.path).query)
            target = (query.get('target') or ['heartbeat'])[0]
            return self.send_text(200, json.dumps(_admin_logs(target)), 'application/json')
        if path == '/no-layout.html':
            display_index = int(os.environ.get('RIPLEY_DISPLAY_INDEX', '0'))
            no_layout_path = Path(__file__).with_name('no-layout.html')
            html = _read_real_file(no_layout_path, MAX_STATIC_HTML_BYTES).decode('utf-8')
            html = html.replace('<title>Screentra Cast Player display</title>', f'<title>Screentra Cast Player display {display_index}</title>')
            return self.send_text(200, html, 'text/html; charset=utf-8')
        if path == '/touch-calibration.html':
            # Serve the touchscreen calibration test page. Lives next to
            # the renderer script and reads RIPLEY_DISPLAY_INDEX for the
            # results so per-display calibration is recorded separately.
            try:
                calibration_path = Path(__file__).with_name('touch-calibration.html')
                html = _read_real_file(calibration_path, MAX_STATIC_HTML_BYTES).decode('utf-8')
                return self.send_text(200, html, 'text/html; charset=utf-8')
            except FileNotFoundError:
                return self.send_text(404, 'calibration page not installed', 'text/plain')
        if path == '/api/status.json':
            # Debug: track JS heartbeat from the kiosk page (UA has
            # 'Chrome' in it). External curls (this script's debugging)
            # don't reset the timer. If the page goes black but chromium
            # is still alive, JS may have stopped running (renderer
            # process stalled). A gap >15s since the last chrome hit is
            # a strong signal of that.
            import sys as _sys, time as _time
            _ua = self.headers.get('User-Agent', '')
            _is_chrome = 'Chrome' in _ua or 'Chromium' in _ua
            _now = _time.time()
            if _is_chrome:
                _last = getattr(Handler, '_last_chrome_status_hit', 0.0)
                _gap = _now - _last if _last else 0.0
                if _last and _gap > 15:
                    print(f'[{_time.strftime("%H:%M:%S")}] /api/status.json GAP={_gap:.1f}s from chrome  (JS may be stalled on page; check chromium renderer process)', file=_sys.stderr, flush=True)
                Handler._last_chrome_status_hit = _now
            else:
                # external probe — log but don't reset chrome timer
                print(f'[{_time.strftime("%H:%M:%S")}] /api/status.json external probe (UA={_ua[:40]!r})', file=_sys.stderr, flush=True)
            try:
                payload = _nl_status()
            except Exception as _e:
                import traceback as _tb; _tb.print_exc()
                payload = {'error': str(_e)}
            return self.send_text(200, json.dumps(payload), 'application/json')
        if path == '/api/touchscreen.json':
            try:
                devs = _nl_detect_touch_devices()
            except Exception as _e:
                import traceback as _tb; _tb.print_exc()
                devs = []
            sel = None
            _tp = Handler.cache_dir / 'touch-device.json'
            if _tp.exists():
                try:
                    sel = _read_json_file(_tp, {}).get('path')
                except Exception:
                    pass
            return self.send_text(200, json.dumps({'devices': devs, 'selected': sel}), 'application/json')
        if path == '/api/cursor-config.json':
            # Read hide_secs from RIPLEY_CURSOR_HIDE_SECS env (default 3.5).
            # The player installer sets this from the operator's kiosk-config,
            # which writes ~/.local/share/screentra-client/config.json.
            hide_secs = float(os.environ.get('RIPLEY_CURSOR_HIDE_SECS', '3.5'))
            return self.send_text(200, json.dumps({'hide_secs': hide_secs}), 'application/json')
        if path == '/api/touch-calibration.json':
            # GET: return last 5 calibration results (or 404 if none).
            calib_path = Handler.cache_dir / 'touch-calibration.json'
            if not calib_path.exists():
                return self.send_text(404, json.dumps({'error': 'no calibration yet'}), 'application/json')
            return self.send_text(200, json.dumps(_read_json_file(calib_path, [])), 'application/json')
        if path == '/health':
            return self.send_text(200, 'ok')
        if path == '/assigned-layout.json':
            display_index = int(os.environ.get('RIPLEY_DISPLAY_INDEX', '0'))
            target = self.cache_dir / f'assigned-layout-display-{display_index}.json'
            if display_index == 0 and not target.exists():
                target = self.cache_dir / 'assigned-layout.json'
            if not target.exists():
                return self.send_text(404, '{}', 'application/json')
            return self.send_text(200, json.dumps(_camera_public_layout(_read_json_file(target, {}))), 'application/json')
        if path == '/client-config.json':
            target = self.cache_dir / path.lstrip('/')
            if not target.exists():
                return self.send_text(404, '{}', 'application/json')
            return self.send_text(200, json.dumps(_read_json_file(target, {})), 'application/json')
        # Optional cached asset serving under cache/assets
        safe = Path(path.lstrip('/'))
        candidate = self.cache_dir / safe
        try:
            candidate_metadata = os.lstat(candidate)
        except FileNotFoundError:
            return self.send_text(404, 'not found')
        if stat.S_ISLNK(candidate_metadata.st_mode) or not stat.S_ISREG(candidate_metadata.st_mode):
            return self.send_text(403, 'forbidden')
        target = candidate.resolve()
        try:
            target.relative_to(self.cache_dir.resolve())
        except Exception:
            return self.send_text(403, 'forbidden')
        try:
            data = _read_real_file(candidate, maximum=MAX_CACHED_ASSET_BYTES)
        except ValueError as error:
            if 'exceeds' in str(error) or 'bounded' in str(error):
                return self.send_text(413, 'asset too large')
            return self.send_text(403, 'forbidden')
        self.send_response(200)
        self.send_header('Content-Type', mimetypes.guess_type(str(target))[0] or 'application/octet-stream')
        self.send_header('Content-Length', str(len(data)))
        self.end_headers(); self.wfile.write(data); return
        return self.send_text(404, 'not found')

def main():
    ap=argparse.ArgumentParser()
    ap.add_argument('--host', default=os.environ.get('RIPLEY_RENDER_HOST','127.0.0.1'))
    ap.add_argument('--port', type=int, default=None)
    ap.add_argument('--cache-dir', default=os.environ.get('RIPLEY_CLIENT_CACHE_DIR', str(Path.home()/'.local/share/screentra-client/cache')))
    args=ap.parse_args()
    if args.port is None:
        args.port = int(os.environ.get('RIPLEY_RENDER_PORT', str(int(os.environ.get('RIPLEY_RENDER_BASE_PORT','3900')) + int(os.environ.get('RIPLEY_DISPLAY_INDEX','0')))))
    Handler.cache_dir=Path(args.cache_dir)
    _ensure_real_directory(Handler.cache_dir, create=True)
    print(f'Screentra Cast cached player renderer listening on http://{args.host}:{args.port}/ using {Handler.cache_dir}', flush=True)
    ThreadingHTTPServer((args.host,args.port), Handler).serve_forever()
if __name__=='__main__': main()

PY_RENDERER
chmod +x "$RENDERER"

cat > "$GPU_HEALTH" <<'PY_GPU_HEALTH'
#!/usr/bin/env python3
from __future__ import annotations
import json
import os
import re
import selectors
import shutil
import subprocess
import sys
import time
from pathlib import Path


MAX_FILE_READ_BYTES = 4096
MAX_SUBPROCESS_OUTPUT_BYTES = 64 * 1024
MAX_GPU_RECORDS = 16
MAX_GPU_NAME_BYTES = 256


def _read(path: Path) -> str | None:
    try:
        with path.open('rb') as handle:
            raw = handle.read(MAX_FILE_READ_BYTES)
        return raw.decode(errors='replace').strip()
    except Exception:
        return None


def _num(text: str | None) -> float | None:
    if text is None:
        return None
    try:
        return float(str(text).strip().replace('%', '').replace('MiB', '').replace('MHz', ''))
    except Exception:
        return None


def _temp_value(raw: str | None) -> float | None:
    v = _num(raw)
    if v is None:
        return None
    c = v / 1000.0 if abs(v) > 1000 else v
    if -30.0 < c < 140.0:
        return round(c, 1)
    return None


def _mhz_value(raw: str | None) -> float | None:
    v = _num(raw)
    if v is None or v <= 0:
        return None
    mhz = v / 1000000.0 if v > 1000000 else v / 1000.0 if v > 1000 else v
    return round(mhz, 0) if mhz > 0 else None




def _bound_subprocess_output(value: str) -> str:
    encoded = str(value).encode('utf-8', errors='replace')
    return encoded[:MAX_SUBPROCESS_OUTPUT_BYTES].decode('utf-8', errors='ignore')


def _bound_gpu_name(value: str) -> str:
    encoded = str(value).encode('utf-8', errors='replace')
    return encoded[:MAX_GPU_NAME_BYTES].decode('utf-8', errors='ignore').strip()


def _run(args: list[str], timeout: float = 2.0) -> tuple[int, str, str]:
    process = None
    try:
        process = subprocess.Popen(args, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        selector = selectors.DefaultSelector()
        stdout = bytearray()
        stderr = bytearray()
        for stream, buffer in ((process.stdout, stdout), (process.stderr, stderr)):
            selector.register(stream, selectors.EVENT_READ, buffer)
        deadline = time.monotonic() + timeout
        try:
            while selector.get_map():
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise TimeoutError('subprocess timed out')
                for key, _ in selector.select(remaining):
                    chunk = os.read(key.fileobj.fileno(), 8192)
                    if not chunk:
                        selector.unregister(key.fileobj)
                        key.fileobj.close()
                        continue
                    buffer = key.data
                    if len(buffer) + len(chunk) > MAX_SUBPROCESS_OUTPUT_BYTES:
                        raise ValueError('subprocess output exceeded its limit')
                    buffer.extend(chunk)
        finally:
            selector.close()
        returncode = process.wait(timeout=max(0.1, deadline - time.monotonic()))
        return returncode, _bound_subprocess_output(stdout.decode('utf-8', errors='replace')).strip(), _bound_subprocess_output(stderr.decode('utf-8', errors='replace')).strip()
    except Exception as e:
        if process is not None and process.poll() is None:
            process.kill()
            process.wait(timeout=1)
        return 127, '', _bound_subprocess_output(str(e))
    finally:
        if process is not None:
            for stream in (process.stdout, process.stderr):
                if stream is not None:
                    stream.close()


def pci_gpus() -> list[dict]:
    gpus = []
    lspci = shutil.which('lspci')
    if lspci:
        rc, out, err = _run([lspci, '-nn'], timeout=2.0)
        if rc == 0:
            for line in out.splitlines():
                low = line.lower()
                if 'vga compatible controller' in low or '3d controller' in low or 'display controller' in low:
                    vendor = 'unknown'
                    if 'nvidia' in low:
                        vendor = 'nvidia'
                    elif 'intel' in low:
                        vendor = 'intel'
                    elif 'amd' in low or 'ati' in low or 'advanced micro devices' in low:
                        vendor = 'amd'
                    gpus.append({'vendor': vendor, 'name': _bound_gpu_name(line), 'source': 'lspci'})
                    if len(gpus) >= MAX_GPU_RECORDS:
                        break
            if gpus:
                return gpus
    # Sysfs fallback without pciutils.
    for dev in sorted(Path('/sys/class/drm').glob('card*/device')):
        vendor_id = _read(dev / 'vendor') or ''
        device_id = _read(dev / 'device') or ''
        vendor = {'0x10de': 'nvidia', '0x8086': 'intel', '0x1002': 'amd', '0x1022': 'amd'}.get(vendor_id.lower(), 'unknown')
        if vendor != 'unknown' or device_id:
            gpus.append({'vendor': vendor, 'name': _bound_gpu_name(f'{vendor} GPU {vendor_id} {device_id}'.strip()), 'source': str(dev)})
            if len(gpus) >= MAX_GPU_RECORDS:
                break
    return gpus


def drm_usage() -> dict | None:
    for busy in sorted(Path('/sys/class/drm').glob('card*/device/gpu_busy_percent')):
        v = _num(_read(busy))
        if v is None:
            continue
        out = {'percent': v, 'device': busy.parent.parent.name, 'source': str(busy), 'method': 'drm-gpu_busy_percent'}
        try:
            total = int((_read(busy.parent / 'mem_info_vram_total') or '0').strip())
            used = int((_read(busy.parent / 'mem_info_vram_used') or '0').strip())
            if total > 0:
                out.update({'vram_total_bytes': total, 'vram_used_bytes': used, 'vram_percent': round((used / total) * 100.0, 1)})
        except Exception:
            pass
        return out
    return None


def hwmon_for(names: tuple[str, ...]) -> dict | None:
    for hw in sorted(Path('/sys/class/hwmon').glob('hwmon*')):
        name = (_read(hw / 'name') or '').lower()
        if not any(n in name for n in names):
            continue
        out = {'device': name or hw.name, 'source': str(hw), 'method': 'hwmon'}
        for temp in sorted(hw.glob('temp*_input')):
            c = _temp_value(_read(temp))
            if c is not None:
                out['temp_c'] = c
                break
        for freq in sorted(hw.glob('freq*_input')):
            mhz = _mhz_value(_read(freq))
            if mhz is not None:
                out['frequency_mhz'] = mhz
                break
        # Some Intel/AMD drivers use gt_*/pp_* files under DRM device.
        return out
    return None


def intel_extra(out: dict) -> dict:
    # i915/xe can expose RPS/GT clocks under DRM debug/sysfs depending on kernel.
    for dev in sorted(Path('/sys/class/drm').glob('card*/device')):
        driver = ''
        try:
            driver = dev.resolve().joinpath('driver').resolve().name.lower()
        except Exception:
            pass
        if driver not in ('i915', 'xe'):
            continue
        out.setdefault('device', driver)
        out.setdefault('source', str(dev))
        out.setdefault('method', 'intel-sysfs')
        for name in ('gt_cur_freq_mhz', 'gt_act_freq_mhz', 'rps_cur_freq_mhz'):
            mhz = _num(_read(dev / name))
            if mhz and mhz > 0:
                out['frequency_mhz'] = round(mhz, 0)
                break
        break
    return out


def nvidia_smi() -> dict | None:
    smi = shutil.which('nvidia-smi')
    if not smi:
        return None
    query = 'name,utilization.gpu,temperature.gpu,memory.used,memory.total,driver_version'
    rc, out, err = _run([smi, f'--query-gpu={query}', '--format=csv,noheader,nounits'], timeout=3.0)
    if rc != 0 or not out:
        return {'vendor': 'nvidia', 'method': 'nvidia-smi', 'source': smi, 'available': False, 'error': err or out or f'exit {rc}'}
    line = out.splitlines()[0]
    parts = [p.strip() for p in line.split(',')]
    while len(parts) < 6:
        parts.append('')
    name, util, temp, used, total, driver = parts[:6]
    used_f, total_f = _num(used), _num(total)
    result = {
        'vendor': 'nvidia', 'device': name or 'NVIDIA GPU', 'percent': _num(util), 'temp_c': _num(temp),
        'memory_used_mib': used_f, 'memory_total_mib': total_f, 'driver_version': driver or None,
        'source': smi, 'method': 'nvidia-smi', 'available': True,
    }
    if used_f is not None and total_f and total_f > 0:
        result['vram_percent'] = round((used_f / total_f) * 100.0, 1)
    return result


def classify_health(g: dict) -> dict:
    pct = g.get('percent')
    temp = g.get('temp_c')
    vram = g.get('vram_percent')
    level = 'unknown'
    notes: list[str] = []
    if pct is None:
        notes.append('usage telemetry unavailable')
    elif pct < 60:
        level = 'ok'
    elif pct < 85:
        level = 'busy'
        notes.append('GPU is working but has headroom pressure')
    else:
        level = 'hot'
        notes.append('GPU utilization is high for a signage player')
    if temp is not None:
        if temp >= 90:
            level = 'hot'; notes.append('GPU temperature is very high')
        elif temp >= 80 and level == 'ok':
            level = 'busy'; notes.append('GPU temperature is warm')
    if vram is not None and vram >= 85:
        level = 'busy' if level == 'ok' else level
        notes.append('GPU memory usage is high')
    if level == 'unknown' and (temp is not None or g.get('frequency_mhz') is not None):
        level = 'telemetry-partial'
    g['health'] = level
    g['health_notes'] = notes
    return g


def main() -> int:
    gpus = pci_gpus()
    vendors = {g.get('vendor') for g in gpus}
    out: dict = {'available': False, 'vendor': None, 'device': None, 'source': None, 'gpus': gpus, 'packages': {}}

    # NVIDIA proprietary path first: this is the only reliable NVIDIA utilization source.
    if 'nvidia' in vendors or shutil.which('nvidia-smi'):
        nv = nvidia_smi()
        if nv:
            out.update(nv)
            out['available'] = bool(nv.get('available', True))
            if not out['available']:
                out['packages']['debian'] = ['nvidia-smi or nvidia-utils package from Debian non-free-firmware/non-free repository']
            print(json.dumps(classify_health(out)))
            return 0

    drm = drm_usage()
    if drm:
        out.update(drm)
        # Guess vendor from card's PCI id if available.
        out['vendor'] = next((g.get('vendor') for g in gpus if g.get('vendor') != 'unknown'), out.get('vendor'))
        out['available'] = True
        print(json.dumps(classify_health(out)))
        return 0

    if 'amd' in vendors:
        h = hwmon_for(('amdgpu', 'radeon')) or {}
        out.update(h)
        out.update({'vendor': 'amd', 'available': bool(h), 'packages': {'debian': ['mesa-utils', 'libgl1-mesa-dri', 'firmware-amd-graphics']}})
        print(json.dumps(classify_health(out)))
        return 0

    if 'intel' in vendors:
        h = hwmon_for(('i915', 'xe', 'intel')) or {}
        h = intel_extra(h)
        out.update(h)
        out.update({'vendor': 'intel', 'available': bool(h.get('temp_c') is not None or h.get('frequency_mhz') is not None or h.get('source')), 'packages': {'debian': ['intel-gpu-tools', 'mesa-utils', 'libgl1-mesa-dri', 'firmware-misc-nonfree']}})
        if shutil.which('intel_gpu_top'):
            out['tool_available'] = 'intel_gpu_top'
            out['note'] = 'intel_gpu_top installed; live utilization may require elevated/capability access, so admin card uses safe sysfs telemetry.'
        print(json.dumps(classify_health(out)))
        return 0

    # Last chance: hwmon names can reveal vendor even without lspci.
    for vendor, names in [('amd', ('amdgpu', 'radeon')), ('intel', ('i915', 'xe', 'intel')), ('nvidia', ('nvidia',))]:
        h = hwmon_for(names)
        if h:
            out.update(h); out.update({'vendor': vendor, 'available': True})
            print(json.dumps(classify_health(out)))
            return 0

    out['error'] = 'No supported GPU telemetry source found. Install pciutils for GPU identification; NVIDIA needs nvidia-smi; Intel may need intel-gpu-tools/Mesa firmware.'
    out['packages']['debian'] = ['pciutils', 'mesa-utils', 'libgl1-mesa-dri', 'intel-gpu-tools', 'firmware-amd-graphics', 'firmware-misc-nonfree']
    print(json.dumps(classify_health(out)))
    return 0


if __name__ == '__main__':
    raise SystemExit(main())

PY_GPU_HEALTH
chmod +x "$GPU_HEALTH"

cat > "$INSTALL_DIR/screentra-display-detect.py" <<'PY_DETECT_FILE'
#!/usr/bin/env python3
"""Detect displays on a Linux box by reading /sys/class/drm.

Each connector is a display_index in stable sort order. Returns a list of dicts
ready to send as the 'displays' field in heartbeat payload.
"""
from __future__ import annotations

import os
import re
from pathlib import Path
from typing import List, Dict, Any, Optional


MAX_SYSFS_VALUE_BYTES = 4096
MAX_DISPLAYS = 8


def _read(path: Path) -> str:
    try:
        with path.open('rb') as handle:
            return handle.read(MAX_SYSFS_VALUE_BYTES).decode('utf-8', errors='replace').strip()
    except Exception:
        return ""


def _read_int(path: Path) -> Optional[int]:
    try:
        return int(_read(path))
    except Exception:
        return None


def _read_float(path: Path) -> Optional[float]:
    try:
        return float(_read(path))
    except Exception:
        return None


def _connector_sort_key(connector: Dict[str, Any]) -> tuple[int, str]:
    """Keep physical connected outputs ahead of disconnected connectors."""
    status = connector.get('status')
    enabled = bool(connector.get('enabled'))
    if status == 'connected' and enabled:
        priority = 0
    elif status == 'connected':
        priority = 1
    else:
        priority = 2
    return priority, str(connector.get('name') or '')


def _gpu_metadata(entry: Path) -> Dict[str, Optional[str]]:
    """Return stable, non-secret GPU identity for a DRM connector."""
    card = entry.name.split('-', 1)[0]
    drm_device = card
    try:
        for candidate in sorted(Path('/dev/dri/by-path').glob(f'pci-*-{card}')):
            if candidate.name.endswith('-card') and candidate.exists():
                drm_device = str(candidate)
                break
    except Exception:
        pass
    device = entry / 'device'
    driver = ''
    try:
        driver = os.path.basename(os.path.realpath(device / 'driver'))
    except Exception:
        pass
    vendor = _read(device / 'vendor') or None
    device_id = _read(device / 'device') or None
    pci_match = re.search(r'pci-[^-]+-[^-]+-[^-]+', drm_device)
    gpu_id = pci_match.group(0) if pci_match else drm_device
    gpu_name = {
        '0x1002': 'AMD', '0x10de': 'NVIDIA', '0x8086': 'Intel',
    }.get((vendor or '').lower(), vendor or 'Unknown GPU')
    return {
        'gpu': gpu_name,
        'gpu_driver': driver or None,
        'gpu_vendor': vendor,
        'gpu_device': device_id,
        'drm_device': drm_device,
        'gpu_id': gpu_id,
    }


def display_key(display: Dict[str, Any]) -> str:
    """Stable blacklist key: DRM device identity plus connector."""
    return str(display.get('display_key') or f"{display.get('drm_device') or display.get('connector') or ''}|{display.get('connector') or ''}")


def detect_displays(max_displays: int = 8, blacklisted_keys=None) -> List[Dict[str, Any]]:
    """Return a list of detected DRM connector displays.

    Each entry: { display_index, connector, label, width, height, refresh_rate, enabled, connected }
    Returns an empty list when no physical DRM connector is present. Virtual
    displays are not supported.
    """
    drm_root = Path('/sys/class/drm')
    displays: List[Dict[str, Any]] = []
    try:
        display_limit = max(0, min(int(max_displays), MAX_DISPLAYS))
    except (TypeError, ValueError):
        display_limit = MAX_DISPLAYS

    blacklist = {str(value) for value in (blacklisted_keys or []) if isinstance(value, str) and value}
    if drm_root.exists():
        connectors = []
        for entry in sorted(drm_root.iterdir()):
            name = entry.name
            # cardN-CONNECTOR-N
            if not name.startswith('card') or '-' not in name:
                continue
            status = _read(entry / 'status')
            enabled = _read(entry / 'enabled') == 'enabled'
            # Retain disconnected connectors for stable bounded indexing; the
            # sort below keeps physical connected outputs first.
            connectors.append({'name': name, 'entry': entry, 'status': status, 'enabled': enabled})

        # Prefer connected outputs, with enabled outputs first.
        connectors.sort(key=_connector_sort_key)

        for idx, conn in enumerate(connectors):
            if idx >= display_limit:
                break
            entry = conn['entry']
            connector = conn['name'].split('-', 1)[1] if '-' in conn['name'] else conn['name']
            gpu = _gpu_metadata(entry)
            key = f"{gpu['drm_device']}|{connector}"
            if key in blacklist:
                continue
            width = _read_int(entry / 'modes') and None
            # best-effort parse modes file (similar to display.rs parse_mode_line)
            width_val = None
            height_val = None
            try:
                first_mode = _read(entry / 'modes').splitlines()[0] if (entry / 'modes').exists() else ''
                if 'x' in first_mode:
                    parts = first_mode.split('x')
                    width_val = int(parts[0])
                    height_val = int(parts[1])
            except Exception:
                pass
            label = f"{connector} ({conn['status']})"
            displays.append({
                'display_index': idx,
                'connector': connector,
                'label': label,
                'width': width_val,
                'height': height_val,
                'refresh_rate': 60.0 if conn['enabled'] else None,
                'connected': conn['status'] == 'connected',
                'enabled': conn['enabled'] and conn['status'] == 'connected',
                'last_error': None,
                **gpu,
                'display_key': key,
            })

    # No physical connector means no supported display. Never synthesize a
    # virtual output: the player is a physical-display-only appliance.
    return displays

PY_DETECT_FILE

cat > "$DISPLAY_GOVERNANCE" <<'PY_DISPLAY_GOVERNANCE'
#!/usr/bin/env python3
"""Screentra Cast player display governance.

Counts connected DRM connectors and writes `config.json` next to the player
runtime state.

Idempotent: safe to re-run after a hotplug or display reconfiguration.
"""
from __future__ import annotations

import argparse
import json
import os
import stat
import sys
import tempfile
from pathlib import Path
from typing import Any, Dict, List

# Local import without circular dep risk. The detect module ships as a
# hyphenated filename (`screentra-display-detect.py`) so it isn't a normal
# importable name; load it via importlib.
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import importlib.util as _ilu
_det_path = os.path.join(os.path.dirname(os.path.abspath(__file__)),
                         "screentra-display-detect.py")
_spec = _ilu.spec_from_file_location("screentra_display_detect", _det_path)
if _spec is None or _spec.loader is None:
    raise RuntimeError(f"cannot load detect module from {_det_path}")
_det = _ilu.module_from_spec(_spec)
sys.modules["screentra_display_detect"] = _det
_spec.loader.exec_module(_det)


CONFIG_DEFAULTS: Dict[str, Any] = {
    # Number of connected+enabled DRM connectors the installer found. The
    # renderer / kiosk-launcher uses this as the upper bound. Re-detected on
    # every run; never mutated by the actor.
    "installer_detected_max_displays": 0,
    # Optional operator-set cap (per-box). None means every connected physical
    # display is eligible, including displays with no touch/input device.
    "max_displays": None,
    "display_blacklist": [],
    # Cache of the most recent detection for offline debugging. Useful when
    # the user opens /admin and the display count looks wrong after a
    # hotplug or cable swap.
    "last_detected_displays": [],
}


def detect_count(max_scan: int = 16) -> int:
    """Return the count of connected physical DRM connectors, capped at max_scan.

    Count connected connectors even if currently disabled in the compositor, so
    an operator can re-enable DP-4 after touchscreen-only mode turned it off.
    """
    config = load_config(Path(os.environ.get('SCREENTRA_CLIENT_DIR') or os.environ.get('RIPLEY_CLIENT_DIR') or os.path.expanduser('~/.local/share/screentra-client')) / 'config.json')
    displays = _det.detect_displays(max_displays=max_scan, blacklisted_keys=config.get('display_blacklist'))
    return sum(1 for d in displays if d.get("connected", d.get("enabled")))


def detect_full(max_scan: int = 16) -> List[Dict[str, Any]]:
    """Return the raw detection list (same shape as heartbeat 'displays' field)."""
    config_path = Path(os.environ.get('SCREENTRA_CLIENT_DIR') or os.environ.get('RIPLEY_CLIENT_DIR') or os.path.expanduser('~/.local/share/screentra-client')) / 'config.json'
    config = load_config(config_path)
    return _det.detect_displays(max_displays=max_scan, blacklisted_keys=config.get('display_blacklist'))


def _ensure_real_directory(path: Path, create: bool = False) -> bool:
    absolute = Path(os.path.abspath(os.fspath(path)))
    current = Path(absolute.anchor)
    for part in absolute.parts[1:]:
        current /= part
        try:
            metadata = os.lstat(current)
        except FileNotFoundError:
            if not create:
                return False
            current.mkdir()
            metadata = os.lstat(current)
        if stat.S_ISLNK(metadata.st_mode) or not stat.S_ISDIR(metadata.st_mode):
            raise ValueError('display governance directory is not a real directory')
    return True


def _read_real_file(path: Path, maximum: int = 1 << 20) -> bytes:
    absolute = Path(os.path.abspath(os.fspath(path)))
    current = Path(absolute.anchor)
    for part in absolute.parts[1:]:
        current /= part
        metadata = os.lstat(current)
        if stat.S_ISLNK(metadata.st_mode):
            raise ValueError('display governance path contains a symlink')
    descriptor = os.open(absolute, os.O_RDONLY | getattr(os, 'O_NOFOLLOW', 0))
    try:
        metadata = os.fstat(descriptor)
        if not stat.S_ISREG(metadata.st_mode) or metadata.st_size > maximum:
            raise ValueError('display governance file is not bounded')
        raw = os.read(descriptor, maximum + 1)
    finally:
        os.close(descriptor)
    if len(raw) > maximum:
        raise ValueError('display governance file exceeds its bound')
    return raw


def _reject_duplicate_json_keys(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError('duplicate JSON object key')
        result[key] = value
    return result


def load_config(config_path: Path) -> Dict[str, Any]:
    """Load config.json (or return defaults) without writing."""
    if not config_path.exists():
        return dict(CONFIG_DEFAULTS)
    try:
        cfg = json.loads(
            _read_real_file(config_path).decode('utf-8'),
            object_pairs_hook=_reject_duplicate_json_keys,
            parse_constant=lambda _: (_ for _ in ()).throw(ValueError('non-standard JSON constant')),
        )
    except Exception:
        # Corrupt config.json → return defaults but don't clobber; caller decides.
        return dict(CONFIG_DEFAULTS)
    out = dict(CONFIG_DEFAULTS)
    if isinstance(cfg, dict):
        out.update(cfg)
    return out


def write_config(config_path: Path, *, installer_detected_max_displays: int,
                 max_displays, last_detected_displays: List[Dict[str, Any]]) -> None:
    """Atomically write config.json. max_displays=None means "no override".

    Preserve operator-managed keys such as touch_binding; display-governance owns
    only the display-count/detection fields.
    """
    cfg = load_config(config_path)
    cfg.update({
        "installer_detected_max_displays": int(installer_detected_max_displays),
        "max_displays": max_displays,
        "last_detected_displays": last_detected_displays,
    })
    _ensure_real_directory(config_path.parent, create=True)
    with tempfile.NamedTemporaryFile(
        mode="w",
        dir=config_path.parent,
        prefix=f".{config_path.name}.",
        suffix=".tmp",
        delete=False,
    ) as stream:
        json.dump(cfg, stream, indent=2)
        tmp = Path(stream.name)
    os.chmod(tmp, 0o600)
    os.replace(tmp, config_path)


def compute_active_max(config: Dict[str, Any]) -> int:
    """The value the renderer / kiosk-launcher should actually use."""
    installer = max(int(config.get("installer_detected_max_displays") or 0), 0)
    override = config.get("max_displays")
    if override is None:
        return installer
    try:
        override_i = max(int(override), 0)
    except (TypeError, ValueError):
        return installer
    # Operator cap cannot exceed what the hardware actually has.
    return min(installer, override_i)


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--config-dir", default=os.environ.get(
        os.environ.get("SCREENTRA_CLIENT_DIR") or os.environ.get("RIPLEY_CLIENT_DIR") or os.path.expanduser("~/.local/share/screentra-client")))
    ap.add_argument("--max-scan", type=int, default=16)
    ap.add_argument("--print-only", action="store_true",
                    help="Detect and print to stdout; do not write config.json")
    args = ap.parse_args()

    displays = detect_full(max_scan=args.max_scan)
    connected_count = sum(1 for d in displays if d.get("connected", d.get("enabled")))

    config_path = Path(args.config_dir) / "config.json"
    existing = load_config(config_path)
    max_displays_override = existing.get("max_displays")
    # None intentionally means all connected physical displays. Touch binding
    # is configured independently and must never decide whether a display can
    # render signage.

    if args.print_only:
        print(json.dumps({
            "installer_detected_max_displays": connected_count,
            "max_displays": max_displays_override,
            "active_max_displays": compute_active_max(
                {"installer_detected_max_displays": connected_count,
                 "max_displays": max_displays_override}),
            "displays": displays,
        }, indent=2))
        return 0

    write_config(config_path,
                 installer_detected_max_displays=connected_count,
                 max_displays=max_displays_override,
                 last_detected_displays=displays)
    print(f"display-governance: wrote {config_path} (installed_max={connected_count}, "
          f"max_displays={'unset' if max_displays_override is None else max_displays_override})",
          flush=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

PY_DISPLAY_GOVERNANCE
chmod +x "$DISPLAY_GOVERNANCE"
ln -sf "$PREFLIGHT" "$BIN_DIR/screentra-client-preflight"
ln -sf "$DISPLAY_GOVERNANCE" "$BIN_DIR/ripley-display-governance"

ln -sf "$RENDERER" "$BIN_DIR/screentra-client-renderer"

# Embedded offline display fonts are inserted by build-installer.py.
# BEGIN_OFFLINE_FONT_BUNDLE
FONT_ASSET_DIR="$INSTALL_DIR/cache/assets/fonts"
mkdir -p "$FONT_ASSET_DIR"
base64 -d > "$FONT_ASSET_DIR/berkshire-swash-400.woff2" <<'OFFLINE_FONT_berkshire_swash_400_woff2'
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
OFFLINE_FONT_berkshire_swash_400_woff2
chmod 0644 "$FONT_ASSET_DIR/berkshire-swash-400.woff2"
base64 -d > "$FONT_ASSET_DIR/butcherman-400.woff2" <<'OFFLINE_FONT_butcherman_400_woff2'
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
OFFLINE_FONT_butcherman_400_woff2
chmod 0644 "$FONT_ASSET_DIR/butcherman-400.woff2"
base64 -d > "$FONT_ASSET_DIR/creepster-400.woff2" <<'OFFLINE_FONT_creepster_400_woff2'
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
OFFLINE_FONT_creepster_400_woff2
chmod 0644 "$FONT_ASSET_DIR/creepster-400.woff2"
base64 -d > "$FONT_ASSET_DIR/nosifer-400.woff2" <<'OFFLINE_FONT_nosifer_400_woff2'
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
OFFLINE_FONT_nosifer_400_woff2
chmod 0644 "$FONT_ASSET_DIR/nosifer-400.woff2"
# END_OFFLINE_FONT_BUNDLE

cat > "$TRANSCRIPTION_ADMISSION" <<'PY_TRANSCRIPTION_ADMISSION'
#!/usr/bin/env python3
"""Pure, bounded admission for one inert transcription request.

The request mapping is closed and immutable.  Admission returns metadata-only
state and never performs work outside this module.
"""
from __future__ import annotations

import json
import re
from collections.abc import Mapping
from types import MappingProxyType
from typing import Any, Final


MAX_SERVICE_ID_BYTES: Final = 64
MAX_REQUEST_ID_BYTES: Final = 128
MAX_REQUEST_BYTES: Final = 512
MAX_EVIDENCE_BYTES: Final = 2048

TRANSCRIPTION_REQUEST_FIELDS: Final = (
    'service_id',
    'request_id',
    'push_to_talk',
)
STT_TRANSCRIPTION_REQUEST_FIELDS: Final = frozenset(TRANSCRIPTION_REQUEST_FIELDS)
_TRANSCRIPTION_REQUEST_FIELD_SET: Final = STT_TRANSCRIPTION_REQUEST_FIELDS
_TRANSCRIPTION_EVIDENCE_FIELDS: Final = frozenset({
    'service_id',
    'request_id',
    'push_to_talk',
    'request_valid',
    'execution_enabled',
    'request_queued',
    'audio_accessed',
    'transcription_started',
    'status',
})
_SERVICE_ID_RE: Final = re.compile(r'^[a-z0-9][a-z0-9._-]{0,63}$')
_REQUEST_ID_RE: Final = re.compile(r'^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$')
_UNSAFE_IDENTIFIER_RE: Final = re.compile(
    r'(?:password|passwd|secret|token|credential|bearer|api[_-]?key|apikey)',
    re.IGNORECASE,
)


class STTTranscriptionRequestError(ValueError):
    """Raised when a transcription request is not the closed contract."""


def _fail(message: str = 'transcription request is invalid') -> STTTranscriptionRequestError:
    return STTTranscriptionRequestError(message)


def _bounded_json(value: Mapping[str, Any], limit: int, message: str) -> None:
    try:
        encoded = json.dumps(
            dict(value),
            ensure_ascii=True,
            allow_nan=False,
            separators=(',', ':'),
        ).encode('utf-8')
    except (TypeError, ValueError, UnicodeEncodeError) as exc:
        raise _fail(message) from exc
    if len(encoded) > limit:
        raise _fail(message)


def _identifier(value: Any, field: str, pattern: re.Pattern[str], maximum: int) -> str:
    if (
        not isinstance(value, str)
        or not value
        or not value.isascii()
        or len(value.encode('utf-8')) > maximum
        or pattern.fullmatch(value) is None
        or _UNSAFE_IDENTIFIER_RE.search(value) is not None
    ):
        raise _fail(f'{field} is invalid or exceeds its bound')
    return value


def _request_values(value: Any) -> dict[str, Any]:
    if not isinstance(value, Mapping):
        raise _fail('transcription request must be a mapping')
    try:
        keys = list(value.keys())
        if len(keys) != len(set(keys)):
            raise _fail('transcription request contains duplicate keys')
        values = dict(value)
    except STTTranscriptionRequestError:
        raise
    except (TypeError, ValueError) as exc:
        raise _fail('transcription request is not a bounded mapping') from exc
    if set(values) != _TRANSCRIPTION_REQUEST_FIELD_SET:
        raise _fail('transcription request fields are not allowlisted')
    _bounded_json(values, MAX_REQUEST_BYTES, 'transcription request is oversized')
    _identifier(values['service_id'], 'service_id', _SERVICE_ID_RE, MAX_SERVICE_ID_BYTES)
    _identifier(values['request_id'], 'request_id', _REQUEST_ID_RE, MAX_REQUEST_ID_BYTES)
    if values['push_to_talk'] is not True:
        raise _fail('push_to_talk must be the real boolean true')
    return values


def validate_transcription_request(payload: Mapping[str, Any]) -> MappingProxyType:
    """Return the exact immutable request mapping after pure validation."""
    values = _request_values(payload)
    return MappingProxyType({field: values[field] for field in TRANSCRIPTION_REQUEST_FIELDS})


def build_transcription_request_evidence(request: Mapping[str, Any]) -> MappingProxyType:
    """Return only bounded metadata for the exact immutable validated request."""
    if type(request) is not MappingProxyType:
        raise _fail('transcription request must be the immutable validated result')
    values = _request_values(request)
    evidence = {
        'service_id': values['service_id'],
        'request_id': values['request_id'],
        'push_to_talk': values['push_to_talk'],
        'request_valid': True,
        'execution_enabled': False,
        'request_queued': False,
        'audio_accessed': False,
        'transcription_started': False,
        'status': 'disabled',
    }
    if set(evidence) != _TRANSCRIPTION_EVIDENCE_FIELDS:
        raise _fail('transcription evidence fields are not allowlisted')
    _bounded_json(evidence, MAX_EVIDENCE_BYTES, 'transcription evidence is oversized')
    return MappingProxyType(evidence)


admit_transcription_request = validate_transcription_request
transcription_request_evidence = build_transcription_request_evidence
validate_stt_transcription_request = validate_transcription_request
build_stt_transcription_request_evidence = build_transcription_request_evidence


__all__ = [
    'MAX_EVIDENCE_BYTES',
    'MAX_REQUEST_BYTES',
    'MAX_REQUEST_ID_BYTES',
    'MAX_SERVICE_ID_BYTES',
    'STTTranscriptionRequestError',
    'STT_TRANSCRIPTION_REQUEST_FIELDS',
    'TRANSCRIPTION_REQUEST_FIELDS',
    'admit_transcription_request',
    'build_stt_transcription_request_evidence',
    'build_transcription_request_evidence',
    'transcription_request_evidence',
    'validate_stt_transcription_request',
    'validate_transcription_request',
]

PY_TRANSCRIPTION_ADMISSION
chmod +x "$TRANSCRIPTION_ADMISSION"

cat > "$ROLLBACK_ADMISSION" <<'PY_ROLLBACK_ADMISSION'
#!/usr/bin/env python3
"""Pure, bounded admission for one inert client-service rollback request.

Admission returns immutable metadata only.  It never performs rollback or any
other local, process, network, filesystem, or hardware operation.
"""
from __future__ import annotations

import json
import re
from collections.abc import Mapping
from types import MappingProxyType
from typing import Any, Final


MAX_SERVICE_ID_BYTES: Final = 64
MAX_ACTIVATION_ID_BYTES: Final = 128
MAX_REQUEST_BYTES: Final = 192
MAX_EVIDENCE_BYTES: Final = 2048

ROLLBACK_REQUEST_FIELDS: Final = ('service_id', 'activation_id')
CLIENT_SERVICE_ROLLBACK_REQUEST_FIELDS: Final = frozenset(ROLLBACK_REQUEST_FIELDS)
_ROLLBACK_EVIDENCE_FIELDS: Final = frozenset({
    'service_id',
    'activation_id',
    'request_valid',
    'execution_enabled',
    'request_queued',
    'rollback_executed',
    'state_changed',
    'status',
})
_SERVICE_ID_RE: Final = re.compile(r'^[a-z0-9][a-z0-9._-]{0,63}$')
_ACTIVATION_ID_RE: Final = re.compile(r'^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$')
_UNSAFE_IDENTIFIER_RE: Final = re.compile(
    r'(?:password|passwd|secret|token|credential|bearer|api[_-]?key|apikey)',
    re.IGNORECASE,
)


class RollbackRequestError(ValueError):
    """Raised when a rollback request is not the closed contract."""


ClientServiceRollbackRequestError = RollbackRequestError


def _fail(message: str = 'rollback request is invalid') -> RollbackRequestError:
    return RollbackRequestError(message)


def _bounded_json(value: Mapping[str, Any], limit: int, message: str) -> None:
    try:
        encoded = json.dumps(
            dict(value),
            ensure_ascii=True,
            allow_nan=False,
            separators=(',', ':'),
        ).encode('utf-8')
    except (TypeError, ValueError, UnicodeEncodeError) as exc:
        raise _fail(message) from exc
    if len(encoded) > limit:
        raise _fail(message)


def _identifier(value: Any, field: str, pattern: re.Pattern[str], maximum: int) -> str:
    if (
        not isinstance(value, str)
        or not value
        or not value.isascii()
        or len(value.encode('utf-8')) > maximum
        or pattern.fullmatch(value) is None
        or _UNSAFE_IDENTIFIER_RE.search(value) is not None
    ):
        raise _fail(f'{field} is invalid or exceeds its bound')
    return value


def _request_values(value: Any) -> dict[str, str]:
    if not isinstance(value, Mapping):
        raise _fail('rollback request must be a mapping')
    try:
        keys = list(value.keys())
        if len(keys) != len(set(keys)):
            raise _fail('rollback request contains duplicate keys')
        values = dict(value)
    except RollbackRequestError:
        raise
    except (TypeError, ValueError) as exc:
        raise _fail('rollback request is not a bounded mapping') from exc
    if set(values) != CLIENT_SERVICE_ROLLBACK_REQUEST_FIELDS:
        raise _fail('rollback request fields are not allowlisted')
    _bounded_json(values, MAX_REQUEST_BYTES, 'rollback request is oversized')
    return {
        'service_id': _identifier(
            values['service_id'], 'service_id', _SERVICE_ID_RE, MAX_SERVICE_ID_BYTES
        ),
        'activation_id': _identifier(
            values['activation_id'], 'activation_id', _ACTIVATION_ID_RE,
            MAX_ACTIVATION_ID_BYTES,
        ),
    }


def validate_rollback_request(payload: Mapping[str, Any]) -> MappingProxyType:
    """Return the exact immutable request mapping after pure validation."""
    values = _request_values(payload)
    return MappingProxyType({field: values[field] for field in ROLLBACK_REQUEST_FIELDS})


def build_rollback_request_evidence(request: Mapping[str, Any]) -> MappingProxyType:
    """Return bounded metadata for the exact immutable validated request."""
    if type(request) is not MappingProxyType:
        raise _fail('rollback request must be the immutable validated result')
    values = _request_values(request)
    evidence = {
        'service_id': values['service_id'],
        'activation_id': values['activation_id'],
        'request_valid': True,
        'execution_enabled': False,
        'request_queued': False,
        'rollback_executed': False,
        'state_changed': False,
        'status': 'disabled',
    }
    if set(evidence) != _ROLLBACK_EVIDENCE_FIELDS:
        raise _fail('rollback evidence fields are not allowlisted')
    _bounded_json(evidence, MAX_EVIDENCE_BYTES, 'rollback evidence is oversized')
    return MappingProxyType(evidence)


admit_rollback_request = validate_rollback_request
admit_client_service_rollback_request = validate_rollback_request
rollback_request_evidence = build_rollback_request_evidence
validate_client_service_rollback_request = validate_rollback_request
build_client_service_rollback_request_evidence = build_rollback_request_evidence


__all__ = [
    'CLIENT_SERVICE_ROLLBACK_REQUEST_FIELDS',
    'ClientServiceRollbackRequestError',
    'MAX_ACTIVATION_ID_BYTES',
    'MAX_EVIDENCE_BYTES',
    'MAX_REQUEST_BYTES',
    'MAX_SERVICE_ID_BYTES',
    'ROLLBACK_REQUEST_FIELDS',
    'RollbackRequestError',
    'admit_rollback_request',
    'build_client_service_rollback_request_evidence',
    'build_rollback_request_evidence',
    'rollback_request_evidence',
    'validate_client_service_rollback_request',
    'validate_rollback_request',
]

PY_ROLLBACK_ADMISSION
chmod +x "$ROLLBACK_ADMISSION"

cat > "$STATUS_ADMISSION" <<'PY_STATUS_ADMISSION'
#!/usr/bin/env python3
"""Pure, bounded admission for one inert client-service status request.

The contract returns immutable metadata only. It never starts, inspects, or
controls a local service and never accesses host resources.
"""
from __future__ import annotations

import json
import re
from collections.abc import Mapping
from types import MappingProxyType
from typing import Any, Final


MAX_SERVICE_ID_BYTES: Final = 64
MAX_REQUEST_BYTES: Final = 80
MAX_EVIDENCE_BYTES: Final = 1024

STATUS_REQUEST_FIELDS: Final = ('service_id',)
CLIENT_SERVICE_STATUS_REQUEST_FIELDS: Final = frozenset(STATUS_REQUEST_FIELDS)
_STATUS_EVIDENCE_FIELDS: Final = frozenset({
    'service_id',
    'request_valid',
    'execution_enabled',
    'request_queued',
    'health_requested',
    'process_started',
    'service_healthy',
})
_SERVICE_ID_RE: Final = re.compile(r'^[a-z0-9][a-z0-9._-]{0,63}$')
_UNSAFE_IDENTIFIER_RE: Final = re.compile(
    r'(?:password|passwd|secret|token|credential|bearer|api[_-]?key|apikey)',
    re.IGNORECASE,
)


class StatusRequestError(ValueError):
    """Raised when a status request is not the closed contract."""


ClientServiceStatusRequestError = StatusRequestError


def _fail(message: str = 'status request is invalid') -> StatusRequestError:
    return StatusRequestError(message)


def _bounded_json(value: Mapping[str, Any], limit: int, message: str) -> None:
    try:
        encoded = json.dumps(
            dict(value),
            ensure_ascii=True,
            allow_nan=False,
            separators=(',', ':'),
        ).encode('utf-8')
    except (TypeError, ValueError, UnicodeEncodeError) as exc:
        raise _fail(message) from exc
    if len(encoded) > limit:
        raise _fail(message)


def _service_id(value: Any) -> str:
    if (
        not isinstance(value, str)
        or not value
        or not value.isascii()
        or len(value.encode('utf-8')) > MAX_SERVICE_ID_BYTES
        or _SERVICE_ID_RE.fullmatch(value) is None
        or _UNSAFE_IDENTIFIER_RE.search(value) is not None
    ):
        raise _fail('service_id is invalid or exceeds its bound')
    return value


def _request_values(value: Any) -> dict[str, str]:
    if not isinstance(value, Mapping):
        raise _fail('status request must be a mapping')
    try:
        keys = list(value.keys())
        if len(keys) != len(set(keys)):
            raise _fail('status request contains duplicate keys')
        values = dict(value)
    except StatusRequestError:
        raise
    except (TypeError, ValueError) as exc:
        raise _fail('status request is not a bounded mapping') from exc
    if set(values) != CLIENT_SERVICE_STATUS_REQUEST_FIELDS:
        raise _fail('status request fields are not allowlisted')
    _bounded_json(values, MAX_REQUEST_BYTES, 'status request is oversized')
    return {'service_id': _service_id(values['service_id'])}


def validate_status_request(payload: Mapping[str, Any]) -> MappingProxyType:
    """Return the exact immutable request mapping after pure validation."""
    values = _request_values(payload)
    return MappingProxyType({'service_id': values['service_id']})


def build_status_request_evidence(request: Mapping[str, Any]) -> MappingProxyType:
    """Return bounded metadata for the exact immutable validated request."""
    if type(request) is not MappingProxyType:
        raise _fail('status request must be the immutable validated result')
    values = _request_values(request)
    evidence = {
        'service_id': values['service_id'],
        'request_valid': True,
        'execution_enabled': False,
        'request_queued': False,
        'health_requested': False,
        'process_started': False,
        'service_healthy': False,
    }
    if set(evidence) != _STATUS_EVIDENCE_FIELDS:
        raise _fail('status evidence fields are not allowlisted')
    _bounded_json(evidence, MAX_EVIDENCE_BYTES, 'status evidence is oversized')
    return MappingProxyType(evidence)


admit_status_request = validate_status_request
admit_client_service_status_request = validate_status_request
status_request_evidence = build_status_request_evidence
validate_client_service_status_request = validate_status_request
build_client_service_status_request_evidence = build_status_request_evidence


__all__ = [
    'CLIENT_SERVICE_STATUS_REQUEST_FIELDS',
    'ClientServiceStatusRequestError',
    'MAX_EVIDENCE_BYTES',
    'MAX_REQUEST_BYTES',
    'MAX_SERVICE_ID_BYTES',
    'STATUS_REQUEST_FIELDS',
    'StatusRequestError',
    'admit_client_service_status_request',
    'admit_status_request',
    'build_client_service_status_request_evidence',
    'build_status_request_evidence',
    'status_request_evidence',
    'validate_client_service_status_request',
    'validate_status_request',
]

PY_STATUS_ADMISSION
chmod +x "$STATUS_ADMISSION"

cat > "$STATUS_REPORT_ADMISSION" <<'PY_STATUS_REPORT_ADMISSION'
#!/usr/bin/env python3
"""Pure admission for the client's metadata-only status snapshot."""
import json
import re
from collections.abc import Mapping
from datetime import datetime

MAX_REPORT_BYTES = 256 * 1024
MAX_CAPABILITIES = 8
MAX_ADDONS = 64
MAX_DETAIL_BYTES = 160
MAX_ERROR_BYTES = 96

_ALLOWED_FIELDS = frozenset({'at', 'addons', 'capabilities', 'services', 'error'})
_ALLOWED_CAPABILITIES = frozenset({
    'render_only',
    'camera_capture',
    'microphone_capture',
    'local_inference',
    'client_service',
})
_ALLOWED_STATES = frozenset({'available', 'unavailable', 'disabled'})
_ADDON_STATES = frozenset({'verified', 'verified_cached', 'failed'})
_ADDON_FIELDS = frozenset({'id', 'version', 'state', 'entrypoint', 'error'})
_ADDON_ID_RE = re.compile(r'^[a-z0-9][a-z0-9._-]{0,63}$')
_ADDON_VERSION_RE = re.compile(r'^[a-z0-9][a-z0-9._-]{0,63}$')
_TIMESTAMP_RE = re.compile(r'^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z$')
_FORBIDDEN_DETAIL_MARKERS = ('password', 'credential', 'secret', 'token', '://', '/')


class StatusReportError(ValueError):
    """Raised when a status snapshot crosses the inert report boundary."""


def _bounded_text(value, maximum, field):
    if not isinstance(value, str) or not value or len(value.encode('utf-8')) > maximum:
        raise StatusReportError(f'invalid status report {field}')
    if any(ord(character) < 0x20 or ord(character) == 0x7F for character in value):
        raise StatusReportError(f'invalid status report {field}')
    return value


def _validate_capabilities(value):
    if not isinstance(value, list) or len(value) > MAX_CAPABILITIES:
        raise StatusReportError('invalid status report capabilities')
    seen = set()
    for capability in value:
        if not isinstance(capability, Mapping) or set(capability) != {'id', 'state', 'detail'}:
            raise StatusReportError('invalid status report capability shape')
        identifier = capability['id']
        state = capability['state']
        detail = capability['detail']
        if identifier not in _ALLOWED_CAPABILITIES or identifier in seen:
            raise StatusReportError('invalid or duplicate status report capability')
        if state not in _ALLOWED_STATES:
            raise StatusReportError('invalid status report capability state')
        detail = _bounded_text(detail, MAX_DETAIL_BYTES, 'capability detail')
        lowered = detail.lower()
        if any(marker in lowered for marker in _FORBIDDEN_DETAIL_MARKERS):
            raise StatusReportError('unsafe status report capability detail')
        seen.add(identifier)


def _validate_addons(value):
    if not isinstance(value, list) or len(value) > MAX_ADDONS:
        raise StatusReportError('invalid status report addons')
    seen = set()
    for addon in value:
        if not isinstance(addon, Mapping) or set(addon) - _ADDON_FIELDS:
            raise StatusReportError('invalid status report addon shape')
        identifier = addon.get('id')
        state = addon.get('state')
        if not isinstance(identifier, str) or not _ADDON_ID_RE.fullmatch(identifier):
            raise StatusReportError('invalid status report addon id')
        if identifier in seen:
            raise StatusReportError('duplicate status report addon')
        if state not in _ADDON_STATES:
            raise StatusReportError('invalid status report addon state')
        version = addon.get('version')
        if version is not None and (
                not isinstance(version, str) or not _ADDON_VERSION_RE.fullmatch(version)):
            raise StatusReportError('invalid status report addon version')
        entrypoint = addon.get('entrypoint')
        if entrypoint is not None:
            if (not isinstance(entrypoint, str) or entrypoint == 'client/' or
                    not entrypoint.startswith('client/') or '\\' in entrypoint or
                    '..' in entrypoint.split('/') or
                    any(ord(character) < 0x20 or ord(character) == 0x7F for character in entrypoint) or
                    len(entrypoint.encode('utf-8')) > MAX_DETAIL_BYTES):
                raise StatusReportError('invalid status report addon entrypoint')
        error = addon.get('error')
        if error is not None:
            _validate_safe_text(error, MAX_ERROR_BYTES, 'addon error')
        seen.add(identifier)


def _validate_safe_text(value, maximum, field):
    _bounded_text(value, maximum, field)
    lowered = value.lower()
    if any(marker in lowered for marker in _FORBIDDEN_DETAIL_MARKERS):
        raise StatusReportError(f'unsafe status report {field}')


def validate_status_report(report):
    """Validate a report without probing devices or changing client state."""
    if not isinstance(report, Mapping) or set(report) - _ALLOWED_FIELDS:
        raise StatusReportError('invalid status report shape')
    timestamp = report.get('at')
    if not isinstance(timestamp, str) or not _TIMESTAMP_RE.fullmatch(timestamp):
        raise StatusReportError('invalid status report timestamp')
    try:
        datetime.strptime(timestamp, '%Y-%m-%dT%H:%M:%SZ')
    except ValueError as exc:
        raise StatusReportError('invalid status report timestamp') from exc
    _validate_addons(report.get('addons'))
    _validate_capabilities(report.get('capabilities'))
    # Runtime execution is disabled. No service observation may be represented
    # as running, healthy, queued, or otherwise active by this report path.
    if report.get('services') != []:
        raise StatusReportError('client service status reporting is disabled')
    error = report.get('error')
    if error is not None:
        _validate_safe_text(error, MAX_ERROR_BYTES, 'error')
    try:
        encoded = json.dumps(report, separators=(',', ':'), ensure_ascii=False).encode('utf-8')
    except (TypeError, ValueError, UnicodeError) as exc:
        raise StatusReportError('status report is not JSON serializable') from exc
    if len(encoded) > MAX_REPORT_BYTES:
        raise StatusReportError('status report exceeds its bound')

PY_STATUS_REPORT_ADMISSION
chmod +x "$STATUS_REPORT_ADMISSION"

cat > "$ACTOR" <<'PY_ACTOR'
#!/usr/bin/env python3
"""Screentra Cast client command actor.

Reads a pending command file written by the heartbeat agent and executes it
locally on the player device. Actions are mapped to systemctl --user calls
or cache refreshes; results are written to command-result.json for the
heartbeat to surface back to the server.
"""
from __future__ import annotations

import argparse
import datetime
import datetime as _d
import importlib.util
import json
import os
import re
import select
import selectors
import signal
import shutil
import ssl
import stat
import subprocess
import sys
import tempfile
import time
import urllib.parse
import urllib.request
from urllib.parse import urlparse
from pathlib import Path


def _load_transcription_request_admission():
    module_path = Path(__file__).with_name(
        'screentra-stt-transcription-request-admission.py'
    )
    spec = importlib.util.spec_from_file_location(
        'screentra_stt_transcription_request_admission', module_path
    )
    if spec is None or spec.loader is None:
        raise RuntimeError('transcription request admission contract is unavailable')
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def _load_rollback_request_admission():
    module_path = Path(__file__).with_name(
        'screentra-client-service-rollback-request-admission.py'
    )
    spec = importlib.util.spec_from_file_location(
        'screentra_client_service_rollback_request_admission', module_path
    )
    if spec is None or spec.loader is None:
        raise RuntimeError('rollback request admission contract is unavailable')
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def _load_status_request_admission():
    module_path = Path(__file__).with_name(
        'screentra-client-service-status-request-admission.py'
    )
    spec = importlib.util.spec_from_file_location(
        'screentra_client_service_status_request_admission', module_path
    )
    if spec is None or spec.loader is None:
        raise RuntimeError('status request admission contract is unavailable')
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


_TRANSCRIPTION_REQUEST_ADMISSION = _load_transcription_request_admission()
_ROLLBACK_REQUEST_ADMISSION = _load_rollback_request_admission()
_STATUS_REQUEST_ADMISSION = _load_status_request_admission()

MAX_PENDING_BYTES = 1 << 20
MAX_JSON_FILE_BYTES = 1 << 20
MAX_SERVER_RESPONSE_BYTES = 1 << 20
MAX_COMMAND_PAYLOAD_BYTES = 64 * 1024
MAX_BLUETOOTH_DEVICES = 128
MAX_AUDIO_SINKS = 128
MAX_CLIENT_ID_BYTES = 200
CLIENT_SERVICE_IDENTIFIER_MAX_BYTES = 128
CLIENT_SERVICE_REQUEST_ID_MAX_BYTES = 128
CLIENT_SERVICE_TRANSCRIBE_ENVELOPE_MAX_BYTES = 512
CLIENT_SERVICE_REASON_MAX_BYTES = 256

ALLOWED_CLIENT_COMMAND_ACTIONS = (
    'restart_renderer', 'restart_kiosk', 'restart_kiosk_display',
    'reload_layout', 'refresh_cache', 'update_client',
    'set_display_count', 'set_touch_binding', 'bluetooth_scan',
    'bluetooth_connect', 'audio_list', 'audio_set_default',
    'audio_set_volume', 'restart_client', 'start_display_preview',
    'stop_display_preview', 'ddc_set', 'display_settings', 'display_power', 'cec_send',
    'ir_send', 'client_service_status', 'client_service_rollback_request',
    'client_service_transcribe_request',
)
_ALLOWED_PENDING_COMMAND_FIELDS = frozenset({
    'client_id', 'action', 'payload', 'requested_at', 'requested_by',
})
MAX_REQUESTED_AT_BYTES = 64
MAX_REQUESTED_BY_BYTES = 128


def _validate_client_service_identifier(value, field):
    if (not isinstance(value, str) or not value or
            len(value.encode('utf-8')) > CLIENT_SERVICE_IDENTIFIER_MAX_BYTES or
            any(not (char.isascii() and (char.isalnum() or char in '._-')) for char in value)):
        raise ValueError(f'{field} is invalid or exceeds its bound')


def validate_client_service_payload(action, payload):
    if action == 'client_service_status':
        _STATUS_REQUEST_ADMISSION.validate_status_request(payload)
        return
    if action == 'client_service_rollback_request':
        _ROLLBACK_REQUEST_ADMISSION.validate_rollback_request(payload)
        return
    if action == 'client_service_transcribe_request':
        _TRANSCRIPTION_REQUEST_ADMISSION.validate_transcription_request(payload)
        return
    if payload is None:
        return
    if not isinstance(payload, dict):
        raise ValueError('client service payload must be a JSON object')
    allowed = ({'service_id', 'activation_id', 'target_version', 'reason'}
               if action == 'client_service_rollback_request' else None)
    if allowed is None:
        return
    if set(payload) - allowed:
        raise ValueError('client service payload contains an unsupported field')
    required = {'service_id', 'activation_id'} if action == 'client_service_rollback_request' else set()
    missing = required - set(payload)
    if missing:
        raise ValueError(f'client service payload is missing {sorted(missing)[0]}')
    for field in ('service_id', 'activation_id', 'target_version'):
        if field in payload:
            _validate_client_service_identifier(payload[field], field)
    if 'reason' in payload:
        reason = payload['reason']
        if (not isinstance(reason, str) or not reason or
                len(reason.encode('utf-8')) > CLIENT_SERVICE_REASON_MAX_BYTES or
                any(ord(char) < 32 or ord(char) == 127 for char in reason)):
            raise ValueError('client service reason is invalid or exceeds its bound')


def validate_pending_command(spec, expected_client_id):
    if not isinstance(spec, dict):
        raise ValueError('pending command must be a JSON object')
    if set(spec) - _ALLOWED_PENDING_COMMAND_FIELDS:
        raise ValueError('pending command contains unsupported fields')
    if (not isinstance(expected_client_id, str) or not expected_client_id or
            len(expected_client_id.encode('utf-8')) > MAX_CLIENT_ID_BYTES):
        raise ValueError('client identity is invalid')
    if spec.get('client_id') != expected_client_id:
        raise ValueError('pending command client identity mismatch')
    if 'requested_at' in spec:
        requested_at = spec['requested_at']
        if (not isinstance(requested_at, str) or not requested_at or
                len(requested_at.encode('utf-8')) > MAX_REQUESTED_AT_BYTES or
                any(ord(char) < 32 or ord(char) == 127 for char in requested_at)):
            raise ValueError('pending command requested_at is invalid')
    if 'requested_by' in spec:
        requested_by = spec['requested_by']
        if requested_by is not None and (
                not isinstance(requested_by, str) or not requested_by or
                len(requested_by.encode('utf-8')) > MAX_REQUESTED_BY_BYTES or
                any(ord(char) < 32 or ord(char) == 127 for char in requested_by)):
            raise ValueError('pending command requested_by is invalid')
    action = spec.get('action')
    if action not in ALLOWED_CLIENT_COMMAND_ACTIONS:
        raise ValueError(f'unsupported client command action: {action}')
    payload = spec.get('payload')
    if payload is not None and not isinstance(payload, dict):
        raise ValueError('client command payload must be a JSON object')
    if payload is not None:
        try:
            encoded = json.dumps(
                payload,
                allow_nan=False,
                separators=(',', ':'),
            ).encode('utf-8')
        except (TypeError, ValueError, UnicodeError) as exc:
            raise ValueError('client command payload is not bounded JSON') from exc
        if len(encoded) > MAX_COMMAND_PAYLOAD_BYTES:
            raise ValueError(f'client command payload exceeds {MAX_COMMAND_PAYLOAD_BYTES} bytes')
        if 'client_id' in payload:
            raise ValueError('client identity is bound to the command envelope')
    if action == 'display_settings':
        if not isinstance(payload, dict):
            raise ValueError('display_settings payload must be a JSON object')
        allowed = {'display_index', 'brightness', 'contrast'}
        if set(payload) - allowed:
            raise ValueError('display_settings payload contains an unsupported field')
        if not isinstance(payload.get('display_index'), int) or isinstance(payload.get('display_index'), bool) or not 0 <= payload['display_index'] <= 3:
            raise ValueError('display_settings display_index must be between 0 and 3')
        supplied = [field for field in ('brightness', 'contrast') if field in payload]
        if not supplied:
            raise ValueError('display_settings requires brightness or contrast')
        for field in supplied:
            value = payload[field]
            if not isinstance(value, int) or isinstance(value, bool) or not 0 <= value <= 100:
                raise ValueError(f'display_settings {field} must be between 0 and 100')
    validate_client_service_payload(action, payload)
    return spec


def _read_bounded_response(response, maximum):
    if isinstance(maximum, bool) or not isinstance(maximum, int) or maximum < 1:
        raise ValueError('response bound is invalid')
    data = response.read(maximum + 1)
    if not isinstance(data, (bytes, bytearray)) or len(data) > maximum:
        raise ValueError('server response exceeds its bound')
    return bytes(data).decode('utf-8')


def _reject_duplicate_json_keys(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError('pending command contains duplicate JSON fields')
        result[key] = value
    return result


def _parse_pending_command(text):
    try:
        return json.loads(
            text,
            object_pairs_hook=_reject_duplicate_json_keys,
            parse_constant=lambda _: (_ for _ in ()).throw(ValueError('non-standard JSON constant')),
        )
    except (TypeError, ValueError, UnicodeError) as exc:
        raise ValueError('pending command JSON is invalid') from exc


def _read_bounded_text(path: Path, maximum: int) -> str:
    if isinstance(maximum, bool) or not isinstance(maximum, int) or maximum < 1:
        raise ValueError('file bound is invalid')
    absolute = Path(os.path.abspath(os.fspath(path)))
    current = Path(absolute.anchor)
    for part in absolute.parts[1:]:
        current /= part
        metadata = os.lstat(current)
        if stat.S_ISLNK(metadata.st_mode):
            raise ValueError('file path contains a symlink')
    descriptor = os.open(absolute, os.O_RDONLY | getattr(os, 'O_NOFOLLOW', 0))
    try:
        metadata = os.fstat(descriptor)
        if not stat.S_ISREG(metadata.st_mode) or metadata.st_size > maximum:
            raise ValueError('file is not a bounded regular file')
        raw = os.read(descriptor, maximum + 1)
    finally:
        os.close(descriptor)
    if len(raw) > maximum:
        raise ValueError('file exceeds its bound')
    return raw.decode('utf-8')


def _read_json_file(path: Path, default):
    try:
        return json.loads(
            _read_bounded_text(path, MAX_JSON_FILE_BYTES),
            object_pairs_hook=_reject_duplicate_json_keys,
            parse_constant=lambda _: (_ for _ in ()).throw(ValueError('non-standard JSON constant')),
        )
    except Exception:
        return default


def _server_origin(server: str) -> str:
    parsed = urllib.parse.urlsplit((server or '').strip())
    if parsed.scheme not in ('http', 'https') or not parsed.hostname or parsed.username or parsed.password or parsed.query or parsed.fragment:
        raise ValueError('configured server URL must be an HTTP(S) origin without credentials or query parameters')
    if parsed.scheme == 'http' and parsed.hostname not in {'127.0.0.1', '::1', 'localhost'} and os.environ.get('RIPLEY_ALLOW_INSECURE_HTTP', '').lower() not in ('1', 'true', 'yes'):
        raise ValueError('non-loopback HTTP server URLs require explicit RIPLEY_ALLOW_INSECURE_HTTP=1')
    return urllib.parse.urlunsplit((parsed.scheme, parsed.netloc, parsed.path.rstrip('/'), '', ''))


def _ensure_real_directory(path: Path, create: bool = False) -> bool:
    current = Path(os.path.abspath(os.fspath(path)).split(os.sep)[0] + os.sep)
    absolute = Path(os.path.abspath(os.fspath(path)))
    for part in absolute.parts[1:]:
        current /= part
        try:
            metadata = os.lstat(current)
        except FileNotFoundError:
            if not create:
                return False
            current.mkdir()
            metadata = os.lstat(current)
        if stat.S_ISLNK(metadata.st_mode) or not stat.S_ISDIR(metadata.st_mode):
            raise ValueError('client cache directory is not a real directory')
    return True


def _atomic_write_text(path: Path, text: str, mode: int = 0o600) -> None:
    """Replace a cache JSON file atomically without following symlinks."""
    _ensure_real_directory(path.parent, create=True)
    descriptor, temporary_name = tempfile.mkstemp(
        prefix=f'.{path.name}.', suffix='.tmp', dir=str(path.parent)
    )
    temporary = Path(temporary_name)
    try:
        with os.fdopen(descriptor, 'w', encoding='utf-8') as handle:
            handle.write(text)
        os.chmod(temporary, mode)
        os.replace(temporary, path)
        temporary = None
    finally:
        if temporary is not None:
            try:
                temporary.unlink()
            except FileNotFoundError:
                pass


MAX_SUBPROCESS_OUTPUT_BYTES = 64 * 1024
MAX_HARDWARE_MESSAGE_BYTES = 512


def _bound_subprocess_output(value: str) -> str:
    encoded = value.encode('utf-8', errors='replace')
    if len(encoded) <= MAX_SUBPROCESS_OUTPUT_BYTES:
        return value
    return encoded[:MAX_SUBPROCESS_OUTPUT_BYTES].decode('utf-8', errors='ignore')


def _bound_hardware_message(value: str) -> str:
    encoded = str(value).encode('utf-8', errors='replace')
    if len(encoded) <= MAX_HARDWARE_MESSAGE_BYTES:
        return str(value)
    return encoded[:MAX_HARDWARE_MESSAGE_BYTES].decode('utf-8', errors='ignore')


def _run_list(args: list[str], timeout: int = 20, input_data: str | None = None, env: dict[str, str] | None = None) -> tuple[int, str, str]:
    process = None
    selector = None
    try:
        process = subprocess.Popen(
            args,
            stdin=subprocess.PIPE if input_data is not None else subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=False,
            env=env,
        )
        if process.stdin is not None:
            process.stdin.write((input_data or '').encode('utf-8'))
            process.stdin.close()
        assert process.stdout is not None and process.stderr is not None
        selector = selectors.DefaultSelector()
        selector.register(process.stdout, selectors.EVENT_READ, 'stdout')
        selector.register(process.stderr, selectors.EVENT_READ, 'stderr')
        buffers = {'stdout': bytearray(), 'stderr': bytearray()}
        deadline = time.monotonic() + timeout
        while selector.get_map():
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise subprocess.TimeoutExpired(args, timeout)
            for key, _ in selector.select(remaining):
                chunk = os.read(key.fileobj.fileno(), MAX_SUBPROCESS_OUTPUT_BYTES + 1)
                if not chunk:
                    selector.unregister(key.fileobj)
                    key.fileobj.close()
                    continue
                buffer = buffers[key.data]
                buffer.extend(chunk)
                if len(buffer) > MAX_SUBPROCESS_OUTPUT_BYTES:
                    raise ValueError('subprocess output exceeds its bound')
        returncode = process.wait(timeout=max(deadline - time.monotonic(), 0.001))
        return (
            returncode,
            bytes(buffers['stdout']).decode('utf-8', errors='replace').strip(),
            bytes(buffers['stderr']).decode('utf-8', errors='replace').strip(),
        )
    except Exception as e:
        if process is not None and process.poll() is None:
            process.kill()
        if process is not None:
            try:
                process.wait(timeout=1)
            except Exception:
                pass
            for stream in (process.stdin, process.stdout, process.stderr):
                if stream is not None:
                    try:
                        stream.close()
                    except Exception:
                        pass
        if selector is not None:
            selector.close()
        return 1, '', _bound_hardware_message(str(e))
    finally:
        if selector is not None:
            selector.close()


def ddcutil_detect(bus: str | None = None) -> tuple[bool, str]:
    """Run ddcutil detect to refresh display cache. Returns (ok, message)."""
    bus = _validate_ddc_bus(bus)
    args = ['sudo', '-n', 'ddcutil', 'detect']
    if bus:
        args.extend(['--bus', bus])
    rc, out, err = _run_list(args)
    if rc == 0:
        return True, _bound_hardware_message(out.strip())
    return False, _bound_hardware_message(err.strip() or out.strip())


_DDC_VCP_RE = re.compile(r'^[0-9A-Fa-f]{1,2}$')
_DDC_BUS_RE = re.compile(r'^[1-9][0-9]{0,2}$')


def _validate_ddc_vcp(value: str) -> str:
    if not isinstance(value, str) or _DDC_VCP_RE.fullmatch(value) is None:
        raise ValueError('DDC VCP code is not allowlisted')
    return value


def _validate_ddc_value(value: int) -> int:
    if isinstance(value, bool) or not isinstance(value, int) or not 0 <= value <= 65535:
        raise ValueError('DDC value is outside its bound')
    return value


def _validate_ddc_bus(value: str | None) -> str | None:
    if value is None:
        return None
    if not isinstance(value, str) or _DDC_BUS_RE.fullmatch(value) is None or int(value) > 255:
        raise ValueError('DDC bus is not allowlisted')
    return value


def apply_ddc_control(vcp_code: str, value: int, bus: str | None = None, has_layout: bool = False) -> tuple[bool, str]:
    """Set a bounded DDC/CI VCP value via ddcutil."""
    vcp_code = _validate_ddc_vcp(vcp_code)
    value = _validate_ddc_value(value)
    bus = _validate_ddc_bus(bus)
    if not isinstance(has_layout, bool):
        raise ValueError('DDC layout flag is invalid')
    args = ['sudo', '-n', 'ddcutil', 'setvcp']
    if bus:
        args.extend(['--bus', bus])
    args.extend([vcp_code, str(value)])

    def _do_set():
        rc, out, err = _run_list(args)
        if rc == 0:
            return True, f'DDC: {vcp_code}={value}' + (f' ({_bound_hardware_message(out.strip())})' if out.strip() else '')
        err_msg = err.strip() or out.strip()
        if 'not found' in err_msg.lower() or 'no such file' in err_msg.lower():
            return False, f'ddcutil_not_installed: {_bound_hardware_message(err_msg)}'
        if 'unable to open' in err_msg.lower() or 'i2c' in err_msg.lower() or 'no bus' in err_msg.lower():
            return False, 'ddcutil_no_bus', err_msg
        if 'communication' in err_msg.lower() or 'timeout' in err_msg.lower():
            return False, f'ddcutil_no_support: {_bound_hardware_message(err_msg)}'
        return False, f'DDC failed (rc={rc}): {_bound_hardware_message(err_msg)}'

    ok, msg_or_tag, *extra = _do_set()
    if ok:
        return True, msg_or_tag

    # Retry once if has_layout and display was not found
    if has_layout and msg_or_tag == 'ddcutil_no_bus':
        err_msg = extra[0] if extra else ''
        ddcutil_detect(bus)
        ok2, msg2 = _do_set()
        if ok2:
            return True, f'{msg2} (re-detected)'
        return False, f'{msg_or_tag}: {_bound_hardware_message(err_msg)} — retry after re-detection failed: {_bound_hardware_message(msg2)}'

    return False, msg_or_tag


_CEC_COMMAND_RE = re.compile(r'^(?:pow|standby|is) [0-9A-Fa-f]$')


def _validate_cec_command(cec_cmd: str) -> str:
    if not isinstance(cec_cmd, str) or len(cec_cmd.encode('utf-8')) > 32 or _CEC_COMMAND_RE.fullmatch(cec_cmd) is None:
        raise ValueError('CEC command is not allowlisted')
    return cec_cmd


def apply_cec_command(cec_cmd: str) -> tuple[bool, str]:
    """Send a bounded allowlisted CEC command via cec-client."""
    cec_cmd = _validate_cec_command(cec_cmd)
    rc, out, err = _run_list(
        ['sudo', '-n', 'cec-client', '-d', '1', '-s'],
        input_data=f'{cec_cmd}\n',
    )
    if rc == 0:
        return True, f'CEC: {cec_cmd}' + (f' — {_bound_hardware_message(out.strip())}' if out.strip() else '')
    return False, f'CEC failed (rc={rc}): {_bound_hardware_message(err.strip() or out.strip())}'


def _bluetoothctl_devices(text: str) -> list[dict]:
    devices = []
    seen = set()
    for line in (text or '').splitlines():
        match = re.match(r'^\s*Device\s+([0-9A-Fa-f:]{17})\s*(.*)$', line)
        if not match:
            continue
        address = match.group(1).upper()
        if address in seen:
            continue
        seen.add(address)
        try:
            name = _validate_bluetooth_name(match.group(2).strip())
        except ValueError:
            name = address
        devices.append({'address': address, 'name': name or address})
        if len(devices) >= MAX_BLUETOOTH_DEVICES:
            break
    return devices


def _validate_bluetooth_name(value: str) -> str:
    if not isinstance(value, str) or not value or len(value.encode('utf-8')) > 128 or any(ord(char) < 32 or ord(char) == 127 for char in value):
        raise ValueError('Bluetooth device name is invalid')
    return value.strip()


def _bounded_integer_input(value, minimum: int, maximum: int, label: str) -> int:
    if isinstance(value, bool):
        raise ValueError(f'{label} must be an integer')
    if isinstance(value, int):
        parsed = value
    elif isinstance(value, str) and re.fullmatch(r'[0-9]+', value):
        parsed = int(value)
    else:
        raise ValueError(f'{label} must be an integer')
    return max(minimum, min(parsed, maximum))


def _validate_display_index(value) -> int:
    if isinstance(value, bool):
        raise ValueError('display index is invalid')
    if isinstance(value, int):
        index = value
    elif isinstance(value, str) and re.fullmatch(r'[0-9]+', value):
        index = int(value)
    else:
        raise ValueError('display index is invalid')
    if not 0 <= index <= 3:
        raise ValueError('display index is outside the supported range')
    return index


def _validate_optional_bool(value, label: str) -> bool:
    if not isinstance(value, bool):
        raise ValueError(f'{label} must be boolean')
    return value


def bluetooth_scan(payload: dict | None) -> tuple[bool, str, dict]:
    payload = payload or {}
    if not isinstance(payload, dict):
        return False, 'Bluetooth scan payload must be an object', {}
    try:
        duration = _bounded_integer_input(payload.get('duration_seconds', 10), 5, 30, 'duration')
    except ValueError:
        return False, 'Bluetooth scan duration must be a number from 5 to 30 seconds', {}
    if shutil.which('bluetoothctl') is None:
        return False, 'bluetoothctl is not installed on this client', {'devices': []}
    _run_list(['bluetoothctl', 'power', 'on'], timeout=10)
    rc, out, err = _run_list(['bluetoothctl', '--timeout', str(duration), 'scan', 'on'], timeout=duration + 10)
    _, listed, list_err = _run_list(['bluetoothctl', 'devices'], timeout=10)
    devices = _bluetoothctl_devices(listed or out)
    if not devices and rc != 0:
        return False, f'Bluetooth scan failed: {_bound_hardware_message(err or out or list_err or "unknown error")}', {'devices': []}
    return True, f'Bluetooth scan found {len(devices)} device(s)', {'devices': devices, 'duration_seconds': duration}


def _find_bluetooth_sink(address: str, name: str | None) -> str | None:
    if shutil.which('wpctl') is None:
        return None
    _, output, _ = _run_list(['wpctl', 'status'], timeout=10)
    address_key = re.sub(r'[^0-9a-f]', '', address.lower())
    name_key = (name or '').strip().lower()
    for line in (output or '').splitlines():
        if not re.search(r'bluez_(?:output|sink)', line, re.IGNORECASE):
            continue
        normalized = re.sub(r'[^0-9a-f]', '', line.lower())
        if address_key and address_key not in normalized and name_key not in line.lower():
            continue
        match = re.match(r'^\s*[│*]*\s*(\d+)\.\s+', line)
        if match:
            return match.group(1)
    return None


def bluetooth_connect(payload: dict | None) -> tuple[bool, str, dict]:
    payload = payload or {}
    if not isinstance(payload, dict):
        return False, 'Bluetooth connect payload must be an object', {}
    raw_address = payload.get('address')
    raw_name = payload.get('name')
    if not isinstance(raw_address, str) or (raw_name is not None and not isinstance(raw_name, str)):
        return False, 'Bluetooth address and name must be strings', {}
    address = raw_address.strip().upper()
    raw_name = raw_name.strip() if raw_name is not None else ''
    try:
        name = _validate_bluetooth_name(raw_name) if raw_name else address
    except ValueError:
        return False, 'Bluetooth device name is invalid', {}
    if not re.fullmatch(r'[0-9A-F]{2}(?::[0-9A-F]{2}){5}', address):
        return False, 'A valid Bluetooth device address is required', {}
    if shutil.which('bluetoothctl') is None:
        return False, 'bluetoothctl is not installed on this client', {}
    _run_list(['bluetoothctl', 'power', 'on'], timeout=10)
    rc, out, err = _run_list(['bluetoothctl', 'connect', address], timeout=30)
    text = '\n'.join(v for v in (out, err) if v)
    message_text = _bound_hardware_message(text)
    connected = rc == 0 and not re.search(r'failed|not available|error', text, re.IGNORECASE)
    details = {'address': address, 'name': name, 'connected': connected, 'bluetooth_output': _bound_hardware_message(text[-1200:])}
    if not connected:
        return False, f'Bluetooth connection failed: {message_text or f"exit code {rc}"}', details
    time.sleep(1)
    sink_id = _find_bluetooth_sink(address, name)
    if sink_id and shutil.which('wpctl'):
        sink_rc, sink_out, sink_err = _run_list(['wpctl', 'set-default', sink_id], timeout=10)
        details['audio_sink_id'] = sink_id
        details['audio_device_selected'] = sink_rc == 0
        if sink_rc == 0:
            return True, f'Connected to {name}; Bluetooth audio output selected', details
        details['audio_selection_error'] = _bound_hardware_message(sink_err or sink_out)
    else:
        details['audio_device_selected'] = False
        details['audio_selection_error'] = 'Bluetooth connected, but no PipeWire Bluetooth audio sink was found yet'
    return True, f'Connected to {name}; audio output selection may require a short PipeWire delay', details


def _wpctl_sinks(text: str) -> list[dict]:
    sinks = []
    in_sinks = False
    for line in (text or '').splitlines():
        stripped = line.strip()
        if re.search(r'\b(?:Audio )?Sinks:', stripped, re.IGNORECASE):
            in_sinks = True
            continue
        if in_sinks and re.search(r'\b(?:Devices|Sources|Filters|Streams|Audio Sources):', stripped, re.IGNORECASE):
            break
        if not in_sinks:
            continue
        match = re.search(r'(?<!\d)(\d+)\.\s+(.+?)\s*$', line)
        if not match:
            continue
        sink_id = match.group(1)
        if len(sink_id) > 8:
            continue
        name = re.sub(r'^\*\s*', '', match.group(2)).strip()
        name = re.sub(r'\s+\[vol:.*?\]\s*$', '', name).strip()
        name = _validate_audio_device_name(name)
        if not any(item['id'] == sink_id for item in sinks):
            sinks.append({'id': sink_id, 'name': name, 'default': '*' in line[:match.start(2)]})
            if len(sinks) >= MAX_AUDIO_SINKS:
                break
    return sinks


def _validate_audio_device_name(value: str) -> str:
    if not isinstance(value, str) or len(value.encode('utf-8')) > 128 or any(ord(char) < 32 or ord(char) == 127 for char in value):
        return 'unnamed audio device'
    return value.strip() or 'unnamed audio device'


def audio_list() -> tuple[bool, str, dict]:
    if shutil.which('wpctl') is None:
        return False, 'wpctl is not installed on this client', {'devices': []}
    rc, out, err = _run_list(['wpctl', 'status'], timeout=10)
    devices = _wpctl_sinks(out)
    if rc != 0:
        return False, f'Audio device scan failed: {_bound_hardware_message(err or out or f"exit code {rc}")}', {'devices': devices}
    return True, f'Found {len(devices)} audio output device(s)', {'devices': devices}


def _validate_audio_sink_id(value: str) -> str:
    if not isinstance(value, str) or value == '@DEFAULT_AUDIO_SINK@':
        if value == '@DEFAULT_AUDIO_SINK@':
            return value
        raise ValueError('audio sink identifier is invalid')
    if not re.fullmatch(r'[0-9]{1,8}', value):
        raise ValueError('audio sink identifier is invalid')
    return value


def audio_set_default(payload: dict | None) -> tuple[bool, str, dict]:
    payload = payload or {}
    if not isinstance(payload, dict):
        return False, 'Audio payload must be an object', {}
    raw_sink_id = payload.get('sink_id')
    if not isinstance(raw_sink_id, str):
        return False, 'A valid audio output device is required', {}
    try:
        sink_id = _validate_audio_sink_id(raw_sink_id.strip())
    except ValueError:
        return False, 'A valid audio output device is required', {}
    if shutil.which('wpctl') is None:
        return False, 'wpctl is not installed on this client', {}
    rc, out, err = _run_list(['wpctl', 'set-default', sink_id], timeout=10)
    if rc == 0:
        return True, f'Audio output selected: {sink_id}', {'sink_id': sink_id}
    return False, f'Audio output selection failed: {_bound_hardware_message(err or out or f"exit code {rc}")}', {'sink_id': sink_id}


def audio_set_volume(payload: dict | None) -> tuple[bool, str, dict]:
    payload = payload or {}
    if not isinstance(payload, dict):
        return False, 'Audio payload must be an object', {}
    raw_sink_id = payload.get('sink_id', '@DEFAULT_AUDIO_SINK@')
    raw_value = payload.get('value')
    if not isinstance(raw_sink_id, str):
        return False, 'Volume must be a number from 0 to 100', {}
    try:
        sink_id = _validate_audio_sink_id(raw_sink_id.strip())
        value = _bounded_integer_input(raw_value, 0, 100, 'volume')
    except (TypeError, ValueError):
        return False, 'Volume must be a number from 0 to 100', {}
    if not sink_id.isdigit() and sink_id != '@DEFAULT_AUDIO_SINK@':
        return False, 'A valid audio output device is required', {}
    if shutil.which('wpctl') is None:
        return False, 'wpctl is not installed on this client', {}
    rc, out, err = _run_list(['wpctl', 'set-volume', sink_id, f'{value}%'], timeout=10)
    if rc == 0:
        return True, f'Client audio volume set to {value}%', {'sink_id': sink_id, 'volume': value}
    return False, f'Client audio volume failed: {_bound_hardware_message(err or out or f"exit code {rc}")}', {'sink_id': sink_id, 'volume': value}


def restart_client() -> tuple[bool, str, dict]:
    if shutil.which('systemctl') is None:
        return False, 'systemctl is not available on this client', {}
    rc, out, err = _run_list(['sudo', '-n', 'systemctl', 'reboot'], timeout=20)
    if rc == 0:
        return True, 'Client reboot requested', {'reboot_requested': True}
    return False, f'Client reboot failed: {_bound_hardware_message(err or out or f"exit code {rc}")}', {'reboot_requested': False}


def has_systemctl_user() -> bool:
    if shutil.which("systemctl") is None:
        return False
    rc, _, _ = _run_list(["systemctl", "--user", "status"])
    return rc == 0


def systemctl_user(action: str, unit: str) -> tuple[bool, str]:
    rc, out, err = _run_list(["systemctl", "--user", action, unit])
    ok = rc == 0
    return ok, _bound_hardware_message((out or err or f"{action} {unit} rc={rc}").strip())


def restart_system_kiosk() -> tuple[bool, str]:
    """Restart the system kiosk wrapper used by the deployed player.

    Some installations run the renderer and Chromium as children of the
    system-level kiosk service rather than under a persistent systemd --user
    manager. In that layout there is no user D-Bus session for the actor, so
    the user-unit path cannot work. Restarting the kiosk wrapper is the
    supported fallback and brings the renderer/browser pair back together.
    """
    rc, out, err = _run_list(
        ['sudo', '-n', 'systemctl', 'restart', 'screentra-cast-kiosk@0.service'],
        timeout=30,
    )
    return rc == 0, _bound_hardware_message((out or err or f'restart screentra-cast-kiosk@0.service rc={rc}').strip())


def apply_display_count(cache_dir: Path, max_displays) -> tuple[bool, str, dict]:
    """Persist active display count and apply renderer/browser services.

    One labwc compositor owns DRM (system screentra-cast-kiosk@0). Secondary
    displays are Chromium windows attached to wayland-0 via user
    screentra-kiosk-browser@N services; never start extra system labwc seats on a
    single-GPU player.
    """
    install_dir = cache_dir.parent
    config_path = install_dir / 'config.json'
    try:
        cfg = _read_json_file(config_path, {})
    except Exception:
        cfg = {}
    if not isinstance(cfg, dict):
        cfg = {}
    try:
        raw_installer = cfg.get('installer_detected_max_displays', 1)
        installer = _bounded_integer_input(raw_installer, 1, 4, 'display count')
    except ValueError:
        installer = 1
    if max_displays is None:
        override = None
        active = installer
    else:
        try:
            override = _bounded_integer_input(max_displays, 1, min(installer, 4), 'display count')
        except ValueError:
            return False, 'display count must be numeric', {}
        active = override
    cfg['installer_detected_max_displays'] = installer
    cfg['max_displays'] = override
    cfg.setdefault('last_detected_displays', [])
    _ensure_real_directory(config_path.parent, create=True)
    _atomic_write_text(config_path, json.dumps(cfg, indent=2))

    details = {'config_path': str(config_path), 'installer_detected_max_displays': installer, 'max_displays': override, 'active_max_displays': active, 'steps': []}
    if has_systemctl_user():
        _run_list(['systemctl', '--user', 'daemon-reload'])
        for i in range(4):
            if i < active:
                ok1, msg1 = systemctl_user('enable', f'screentra-client-renderer@{i}.service')
                ok2, msg2 = systemctl_user('restart', f'screentra-client-renderer@{i}.service')
                details['steps'].append({'unit': f'screentra-client-renderer@{i}.service', 'enable': ok1, 'restart': ok2, 'message': msg2 or msg1})
                if i > 0:
                    ok3, msg3 = systemctl_user('enable', f'screentra-kiosk-browser@{i}.service')
                    ok4, msg4 = systemctl_user('restart', f'screentra-kiosk-browser@{i}.service')
                    details['steps'].append({'unit': f'screentra-kiosk-browser@{i}.service', 'enable': ok3, 'restart': ok4, 'message': msg4 or msg3})
            else:
                ok1, msg1 = systemctl_user('stop', f'screentra-client-renderer@{i}.service')
                ok2, msg2 = systemctl_user('disable', f'screentra-client-renderer@{i}.service')
                ok3, msg3 = systemctl_user('stop', f'screentra-kiosk-browser@{i}.service')
                ok4, msg4 = systemctl_user('disable', f'screentra-kiosk-browser@{i}.service')
                details['steps'].append({'unit': f'display-{i}', 'disabled': True, 'messages': [msg1,msg2,msg3,msg4]})
    # Prevent accidental second DRM-master labwc seats; display 1+ attach to wayland-0.
    for i in range(1,4):
        _run_list(['sudo', '-n', 'systemctl', 'disable', '--now', f'screentra-cast-kiosk@{i}.service'])
        details['steps'].append({'unit': f'screentra-cast-kiosk@{i}.service', 'disabled_system_seat': True})

    # Apply compositor output policy live when wayland-0 is available. Do not
    # restart labwc just to change display count; restarting can briefly drop DRM
    # and leave stale Chromium/orphan labwc processes. Boot-time policy remains in
    # screentra-kiosk.sh for reboots.
    try:
        env = os.environ.copy()
        env.setdefault('XDG_RUNTIME_DIR', f'/run/user/{os.getuid()}')
        env.setdefault('WAYLAND_DISPLAY', 'wayland-0')
        out = _bounded_check_output(['wlr-randr', '--json'], MAX_WLR_JSON_BYTES, 3, env=env)
        data = json.loads(
            out,
            parse_constant=lambda _: (_ for _ in ()).throw(ValueError('non-standard JSON constant')),
        )
        if not isinstance(data, list):
            raise ValueError('wlr-randr output is not a list')
        outputs = [o for o in data[:MAX_WLR_OUTPUTS] if isinstance(o, dict) and o.get('name')]
        outputs.sort(key=lambda o: o.get('name',''))
        x = 0
        for idx, o in enumerate(outputs):
            name = o.get('name')
            try:
                name = _validate_display_connector(name)
            except ValueError:
                continue
            cur = next((m for m in o.get('modes', []) if m.get('current')), None)
            pref = next((m for m in o.get('modes', []) if m.get('preferred')), None)
            mode = cur or pref or {}
            width = int(mode.get('width') or 0)
            if idx < active:
                rc, _, err = _run_list(
                    ['wlr-randr', '--output', name, '--on', '--pos', f'{x},0'],
                    timeout=5,
                    env=env,
                )
                details['steps'].append({'output': name, 'action': 'on', 'x': x, 'code': rc, 'stderr': _bound_hardware_message(err)})
                x += width
            elif o.get('enabled'):
                rc, _, err = _run_list(
                    ['wlr-randr', '--output', name, '--off'],
                    timeout=5,
                    env=env,
                )
                details['steps'].append({'output': name, 'action': 'off', 'code': rc, 'stderr': _bound_hardware_message(err)})
    except Exception as e:
        details['steps'].append({'output_policy': 'skipped', 'error': _bound_hardware_message(str(e))})
        rc,out,err = _run_list(['sudo', '-n', 'systemctl', 'restart', 'screentra-cast-kiosk@0.service'])
        details['steps'].append({'unit': 'screentra-cast-kiosk@0.service', 'restart_code': rc, 'message': _bound_hardware_message(out or err)})

    ok = has_systemctl_user()
    return ok, f'active display count set to {active}', details


_TOUCH_DEVICE_RE = re.compile(r'^/dev/[A-Za-z0-9._:+@/-]{1,240}$')


def _validate_touch_device_path(value: str | None) -> str | None:
    if value is None or value == '':
        return None
    if not isinstance(value, str) or '..' in value or _TOUCH_DEVICE_RE.fullmatch(value) is None:
        raise ValueError('touch device path is not allowlisted')
    return value


def _validate_touch_device_name(value: str | None) -> str | None:
    if value is None or value == '':
        return None
    if not isinstance(value, str) or len(value.encode('utf-8')) > 128 or any(ord(char) < 32 or ord(char) == 127 for char in value):
        raise ValueError('touch device name is not bounded')
    return value.strip() or None


def _validate_actor_label(value: str | None, field: str) -> str | None:
    if value is None or value == '':
        return None
    if not isinstance(value, str) or len(value.encode('utf-8')) > 128 or any(ord(char) < 32 or ord(char) == 127 for char in value):
        raise ValueError(f'{field} is not bounded')
    return value.strip() or None


def _resolve_touch_name(device_path: str | None, device_name: str | None = None) -> str | None:
    if device_name:
        return _validate_touch_device_name(device_name)
    if not device_path:
        return None
    try:
        blocks = _read_bounded_text(Path('/proc/bus/input/devices'), 256 * 1024).split('\n\n')
        target = Path(device_path).name
        for block in blocks:
            if target not in block:
                continue
            for line in block.splitlines():
                if line.startswith('N:') and 'Name=' in line:
                    return _validate_touch_device_name(line.split('Name=', 1)[1].strip().strip('"'))
    except Exception:
        pass
    return None


_DISPLAY_CONNECTOR_RE = re.compile(r'^[A-Za-z0-9_.:-]{1,128}$')


def _validate_display_connector(value: str) -> str:
    if not isinstance(value, str) or len(value.encode('utf-8')) > 128 or _DISPLAY_CONNECTOR_RE.fullmatch(value) is None:
        raise ValueError('display connector is not allowlisted')
    return value


def _display_connectors_for_indexes(config: dict, indexes: list[int]) -> list[str]:
    displays = config.get('last_detected_displays') or []
    out = []
    for idx in indexes:
        for d in displays:
            try:
                if int(d.get('display_index', -1)) == int(idx):
                    c = d.get('connector') or d.get('label')
                    try:
                        c = _validate_display_connector(c)
                    except ValueError:
                        continue
                    if c not in out:
                        out.append(c)
                    break
            except Exception:
                continue
    return out


def apply_touch_binding(cache_dir: Path, payload: dict) -> tuple[bool, str, dict]:
    install_dir = cache_dir.parent
    config_path = install_dir / 'config.json'
    try:
        cfg = _read_json_file(config_path, {})
    except Exception:
        cfg = {}
    if not isinstance(cfg, dict):
        cfg = {}
    dev_path = _validate_touch_device_path(payload.get('device_path') or payload.get('path'))
    dev_name = _resolve_touch_name(dev_path, payload.get('device_name'))
    requested_by = _validate_actor_label(payload.get('requested_by'), 'touch binding requester')
    allow_joined = payload.get('allow_joined_displays', False)
    if not isinstance(allow_joined, bool):
        raise ValueError('joined display flag is invalid')
    raw_indexes = payload.get('target_display_indexes') or payload.get('display_indexes') or payload.get('display_index') or [0]
    if not isinstance(raw_indexes, list):
        raw_indexes = [raw_indexes]
    indexes = []
    for v in raw_indexes:
        try:
            if isinstance(v, bool):
                raise ValueError
            i = max(0, min(int(v), 3))
            if i not in indexes:
                indexes.append(i)
        except Exception:
            pass
    if not indexes:
        indexes = [0]
    indexes = indexes[:2] if allow_joined else indexes[:1]
    binding = {
        'device_path': dev_path,
        'device_name': dev_name,
        'target_display_indexes': indexes,
        'target_connectors': _display_connectors_for_indexes(cfg, indexes),
        'allow_joined_displays': allow_joined,
        'updated_at': datetime.datetime.now(datetime.timezone.utc).replace(tzinfo=None).isoformat() + 'Z',
        'updated_by': requested_by or 'actor',
    }
    cfg['touch_binding'] = binding
    _ensure_real_directory(config_path.parent, create=True)
    _atomic_write_text(config_path, json.dumps(cfg, indent=2))
    if dev_path:
        _ensure_real_directory(cache_dir, create=True)
        _atomic_write_text(Path(cache_dir, 'touch-device.json'), json.dumps({'path': dev_path, 'name': dev_name}, indent=2))
    details = {'config_path': str(config_path), 'touch_binding': binding, 'steps': []}
    # Restart labwc so rc.xml <touch> mapping is reloaded, then let browser services return.
    rc, out, err = _run_list(['sudo', '-n', 'systemctl', 'restart', 'screentra-cast-kiosk@0.service'])
    details['steps'].append({'unit': 'screentra-cast-kiosk@0.service', 'restart_code': rc, 'message': _bound_hardware_message(out or err)})
    if has_systemctl_user():
        systemctl_user('restart', 'screentra-client-heartbeat.service')
        for i in range(4):
            systemctl_user('try-restart', f'screentra-client-renderer@{i}.service')
            systemctl_user('try-restart', f'screentra-kiosk-browser@{i}.service')
    ok = rc == 0 or has_systemctl_user()
    return ok, 'touch binding applied; labwc/browser services restarted', details


def tls_context(cache_dir: Path | None = None):
    """Use the Screentra Cast CA bundle directly when present, independent of OS trust."""
    candidates = []
    if os.environ.get('RIPLEY_CA_CERT'):
        candidates.append(Path(os.environ['RIPLEY_CA_CERT']))
    if cache_dir is not None:
        candidates.append(cache_dir.parent / 'screentra-cast-signage-ca.crt')
    candidates.append(Path.home() / '.local/share/ripley-player-client/screentra-cast-signage-ca.crt')
    for cafile in candidates:
        if cafile.exists():
            try:
                ca_data = _read_bounded_text(cafile, 256 * 1024)
                return ssl.create_default_context(cadata=ca_data)
            except Exception as e:
                print(f'warning: failed to load Screentra Cast CA {cafile}: {e}', flush=True)
    raise RuntimeError('Screentra Cast CA certificate is unavailable or invalid')


def validate_server_url(server: str):
    parsed = urlparse((server or '').strip())
    if parsed.scheme not in ('https', 'http') or not parsed.hostname:
        raise ValueError('server URL must include an https:// origin')
    if parsed.scheme == 'http' and parsed.hostname not in ('127.0.0.1', 'localhost', '::1'):
        raise ValueError('non-loopback server URLs must use HTTPS')
    return parsed

def urlopen_ripley(req, timeout=10, cache_dir: Path | None = None):
    kw = {'timeout': timeout}
    parsed = validate_server_url(req.full_url)
    if parsed.scheme == 'https':
        ctx = tls_context(cache_dir)
        kw['context'] = ctx
    return urllib.request.urlopen(req, **kw)


def _preview_pid_path(cache_dir: Path, display_index: int) -> Path:
    return cache_dir / f'display-preview-{display_index}.pid'


def _preview_state_path(cache_dir: Path, display_index: int) -> Path:
    return cache_dir / f'display-preview-{display_index}.json'


def _preview_token(cache_dir: Path) -> str:
    token_path = cache_dir.parent / 'client-auth-token'
    try:
        token = _read_bounded_text(token_path, 4096).strip()
    except Exception:
        return ''
    return token


def _write_preview_state(cache_dir: Path, display_index: int, state: dict) -> None:
    _atomic_write_text(_preview_state_path(cache_dir, display_index), json.dumps(state, indent=2))


def _preview_connector(cache_dir: Path, display_index: int) -> str | None:
    data = _read_json_file(cache_dir / f'display-{display_index}.json', {})
    connector = data.get('connector')
    try:
        return _validate_display_connector(connector) if connector else None
    except ValueError:
        return None


def _discover_sway_socket(runtime: str, env: dict[str, str]) -> str | None:
    """Return a live Sway IPC socket, ignoring stale sockets in the runtime dir."""
    if not shutil.which('swaymsg'):
        return None
    candidates = []
    configured = env.get('SWAYSOCK')
    if configured:
        candidates.append(Path(configured))
    try:
        discovered = sorted(
            (p for p in Path(runtime).glob('sway-ipc.*.sock') if p.is_socket()),
            key=lambda p: p.stat().st_mtime_ns,
            reverse=True,
        )
    except OSError:
        discovered = []
    candidates.extend(path for path in discovered if path not in candidates)
    for socket_path in candidates[:8]:
        rc, _, _ = _run_list(
            ['swaymsg', '-s', str(socket_path), '-t', 'get_version'],
            timeout=3,
            env=env,
        )
        if rc == 0:
            return str(socket_path)
    return None


def _preview_wayland_env() -> dict[str, str]:
    """Provide Wayland runtime variables when the actor is a system service."""
    env = os.environ.copy()
    runtime = env.get('XDG_RUNTIME_DIR')
    if not runtime:
        for candidate in ('/run/board', f"/run/user/{os.getuid()}"):
            if Path(candidate).is_dir():
                runtime = candidate
                env['XDG_RUNTIME_DIR'] = candidate
                break
    if runtime and not env.get('WAYLAND_DISPLAY'):
        sockets = sorted(
            p for p in Path(runtime).glob('wayland-*')
            if p.is_socket()
        )
        if sockets:
            env['WAYLAND_DISPLAY'] = sockets[0].name
    if runtime and not env.get('SWAYSOCK'):
        sway_socket = _discover_sway_socket(runtime, env)
        if sway_socket:
            env['SWAYSOCK'] = sway_socket
    return env


def _display_renderer_units(display_index: int) -> list[tuple[str, str]]:
    if display_index == 0:
        return [('system', 'screentra-cast-kiosk@0.service')]
    return [
        ('user', f'screentra-client-renderer@{display_index}.service'),
        ('user', f'screentra-kiosk-browser@{display_index}.service'),
    ]


def display_renderer_active(display_index: int) -> bool:
    """Return whether the display-specific renderer/browser is active."""
    units = _display_renderer_units(display_index)
    for scope, unit_name in units:
        if scope == 'system':
            rc, _, _ = _run_list(
                ['sudo', '-n', 'systemctl', 'is-active', '--quiet', unit_name],
                timeout=10,
            )
            if rc != 0:
                return False
        elif not systemctl_user('is-active', unit_name)[0]:
            return False
    return True


def _system_display_renderer_action(action: str, unit_name: str) -> tuple[bool, str]:
    rc, out, err = _run_list(
        ['sudo', '-n', 'systemctl', action, unit_name],
        timeout=30,
    )
    return rc == 0, _bound_hardware_message((out or err or f'{action} {unit_name} rc={rc}').strip())


def stop_display_renderer(display_index: int) -> tuple[bool, str]:
    """Stop rendering for one display while leaving the control plane alive."""
    units = list(reversed(_display_renderer_units(display_index)))
    messages = []
    for scope, unit_name in units:
        if scope == 'system':
            ok, message = _system_display_renderer_action('stop', unit_name)
        else:
            ok, message = systemctl_user('stop', unit_name)
        messages.append(message)
        if not ok:
            return False, _bound_hardware_message('; '.join(messages))
    return True, _bound_hardware_message('; '.join(messages))


def suspend_primary_display_renderer(env: dict[str, str]) -> tuple[bool, str]:
    """Stop display 0's browser/renderer without stopping its compositor.

    The primary system kiosk unit owns Sway as well as the browser and
    renderer. Stopping that unit destroys the compositor and therefore loses
    the DPMS state. Close only the display-0 Chromium surface and terminate
    only the display-0 renderer process; leave the unit/Sway alive so the
    compositor can keep the output powered off and receive the later wake
    command.
    """
    socket = env.get('SWAYSOCK')
    if not socket:
        return False, 'active Sway compositor socket is unavailable'
    messages = []
    rc, out, err = _run_list(
        [
            'swaymsg', '-s', socket,
            '[app_id="screentra-cast-display-0"] kill',
        ],
        timeout=10,
        env=env,
    )
    if rc == 0:
        messages.append('display-0 browser surface closed')
    elif 'No matching node' in (out or err):
        messages.append('display-0 browser surface was already closed')
    else:
        return False, _bound_hardware_message(
            out or err or 'failed to close display-0 browser surface'
        )

    def _terminate_matching(pattern: str, label: str) -> tuple[bool, int, str]:
        rc, output, error = _run_list(
            ['pgrep', '-u', str(os.getuid()), '-f', pattern],
            timeout=5,
        )
        if rc not in {0, 1}:
            return False, 0, _bound_hardware_message(
                error or output or f'failed to find {label} (rc={rc})'
            )
        pids = []
        for raw_pid in (output or '').splitlines()[:16]:
            try:
                pid = int(raw_pid.strip())
            except ValueError:
                continue
            if 1 <= pid <= 4194304 and pid != os.getpid():
                pids.append(pid)
        for pid in pids:
            try:
                os.kill(pid, signal.SIGTERM)
            except ProcessLookupError:
                continue
            except OSError as exc:
                return False, len(pids), _bound_hardware_message(
                    f'failed to stop {label} pid {pid}: {exc}'
                )
        return True, len(pids), ''

    browser_ok, browser_count, browser_message = _terminate_matching(
        r'chromium.*screentra-cast-display-0', 'display-0 browser'
    )
    if not browser_ok:
        return False, browser_message
    messages.append(f'display-0 browser process stopped ({browser_count})')

    renderer_ok, renderer_count, renderer_message = _terminate_matching(
        r'screentra-client-renderer\.py.*--port 3900', 'display-0 renderer'
    )
    if not renderer_ok:
        return False, renderer_message
    messages.append(f'display-0 renderer stopped ({renderer_count})')
    return True, _bound_hardware_message('; '.join(messages))


def start_display_renderer(display_index: int) -> tuple[bool, str]:
    """Start rendering for one display after a power-on transition."""
    messages = []
    for scope, unit_name in _display_renderer_units(display_index):
        if scope == 'system':
            ok, message = _system_display_renderer_action('start', unit_name)
        else:
            ok, message = systemctl_user('start', unit_name)
        messages.append(message)
        if not ok:
            return False, _bound_hardware_message('; '.join(messages))
    return True, _bound_hardware_message('; '.join(messages))


def restart_display_renderer(display_index: int) -> tuple[bool, str]:
    """Refresh one display's rendering after an in-place power transition."""
    if display_index == 0:
        return restart_system_kiosk()
    return systemctl_user('restart', f'screentra-kiosk-browser@{display_index}.service')


def refresh_or_start_display_renderer(display_index: int) -> tuple[bool, str, str]:
    """Refresh an active renderer or start one that was suspended."""
    if display_renderer_active(display_index):
        ok, message = restart_display_renderer(display_index)
        return ok, message, 'restarted'
    ok, message = start_display_renderer(display_index)
    return ok, message, 'started'


def apply_display_power(cache_dir: Path, display_index: int | None, state: str) -> tuple[bool, str, dict]:
    """Apply Linux DPMS/KMS power state and suspend/resume display rendering."""
    if display_index is None or display_index < 0 or display_index > 3:
        return False, 'a display index from 0 to 3 is required', {}
    if not isinstance(state, str) or state not in {'on', 'off'}:
        return False, 'display power state must be on or off', {}
    connector = _preview_connector(cache_dir, display_index)
    if not connector:
        return False, f'no connector is recorded for display {display_index}', {'display_index': display_index}
    # If the primary stack is not running there is no compositor IPC to wake;
    # start the complete stack first. During a normal suspend, the system kiosk
    # unit remains active and only its browser/renderer are stopped below.
    initial_env = _preview_wayland_env()
    if state == 'on' and (
        (display_index == 0 and not initial_env.get('SWAYSOCK'))
        or (display_index != 0 and not display_renderer_active(display_index))
    ):
        renderer_ok, renderer_message = start_display_renderer(display_index)
        details = {
            'display_index': display_index,
            'connector': connector,
            'state': state,
            'method': 'kiosk-stack-start',
            'renderer_started': renderer_ok,
            'renderer_start_message': renderer_message,
        }
        if not renderer_ok:
            return False, _bound_hardware_message(f'display renderer start failed: {renderer_message}'), details
        return True, _bound_hardware_message(f'display renderer started: {renderer_message}'), details

    env = initial_env
    if state == 'off' and display_index == 0:
        if not env.get('SWAYSOCK') or not shutil.which('swaymsg'):
            return False, 'active Sway compositor is required for Linux DPMS suspend', {
                'display_index': display_index,
                'connector': connector,
                'state': state,
                'method': 'sway-dpms',
                'stage': 'discover-compositor',
            }
        renderer_ok, renderer_message = suspend_primary_display_renderer(env)
        details = {
            'display_index': display_index,
            'connector': connector,
            'state': state,
            'method': 'sway-dpms',
            'renderer_suspended': renderer_ok,
            'renderer_suspend_message': renderer_message,
        }
        if not renderer_ok:
            return False, _bound_hardware_message(
                f'display renderer suspend failed: {renderer_message}'
            ), details
        rc, out, err = _run_list(
            ['swaymsg', '-s', env['SWAYSOCK'], 'output', connector, 'power', 'off'],
            timeout=10,
            env=env,
        )
        message = _bound_hardware_message(
            out or err or ('display powered off via Sway DPMS' if rc == 0 else 'Sway DPMS suspend failed')
        )
        details['return_code'] = rc
        if rc != 0:
            return False, message, details
        return True, _bound_hardware_message(f'{message}; renderer suspended'), details

    requested = '--on' if state == 'on' else '--off'
    if state == 'on' and env.get('SWAYSOCK') and shutil.which('swaymsg'):
        enable_rc, enable_out, enable_err = _run_list(
            ['wlr-randr', '--output', connector, '--on'],
            timeout=10,
            env=env,
        )
        if enable_rc != 0:
            message = _bound_hardware_message(
                enable_out or enable_err or 'failed to re-enable output before Sway DPMS wake'
            )
            return False, message, {
                'display_index': display_index,
                'connector': connector,
                'state': state,
                'method': 'wlr-randr+sway-dpms',
                'stage': 'enable-output',
                'enable_return_code': enable_rc,
            }
        rc, out, err = _run_list(
            ['swaymsg', '-s', env['SWAYSOCK'], 'output', connector, 'power', 'on'],
            timeout=10,
            env=env,
        )
        message = _bound_hardware_message(
            out or err or ('display powered on via Sway DPMS' if rc == 0 else 'Sway DPMS wake failed')
        )
        details = {
            'display_index': display_index,
            'connector': connector,
            'state': state,
            'method': 'wlr-randr+sway-dpms',
            'enable_return_code': enable_rc,
            'return_code': rc,
        }
        if rc != 0:
            return False, message, details
        if display_index == 0:
            renderer_ok, renderer_message = restart_display_renderer(display_index)
            renderer_action = 'restarted'
        else:
            renderer_ok, renderer_message, renderer_action = refresh_or_start_display_renderer(display_index)
        details[f'renderer_{renderer_action}'] = renderer_ok
        details[f'renderer_{renderer_action}_message'] = renderer_message
        if not renderer_ok:
            return False, _bound_hardware_message(
                f'{message}; renderer {renderer_action} failed: {renderer_message}'
            ), details
        return True, _bound_hardware_message(f'{message}; renderer {renderer_action}'), details
    rc, out, err = _run_list(
        ['wlr-randr', '--output', connector, requested],
        timeout=10,
        env=env,
    )
    message = _bound_hardware_message(out or err or ('display enabled' if state == 'on' else 'display disabled'))
    details = {
        'display_index': display_index,
        'connector': connector,
        'state': state,
        'method': 'wlr-randr',
        'return_code': rc,
    }
    if rc != 0:
        return False, message, details
    if state == 'off':
        renderer_ok, renderer_message = stop_display_renderer(display_index)
        details['renderer_stopped'] = renderer_ok
        details['renderer_stop_message'] = renderer_message
        if not renderer_ok:
            return False, _bound_hardware_message(
                f'{message}; renderer suspend failed: {renderer_message}'
            ), details
        return True, _bound_hardware_message(f'{message}; renderer suspended'), details
    renderer_ok, renderer_message, renderer_action = refresh_or_start_display_renderer(display_index)
    details[f'renderer_{renderer_action}'] = renderer_ok
    details[f'renderer_{renderer_action}_message'] = renderer_message
    if not renderer_ok:
        return False, _bound_hardware_message(
            f'{message}; renderer {renderer_action} failed: {renderer_message}'
        ), details
    return True, _bound_hardware_message(f'{message}; renderer {renderer_action}'), details


def sync_display_renderer_power_state(display_index: int | None, state: str) -> tuple[bool, str, dict]:
    """Match renderer state to a successful DDC/CEC power transition."""
    if display_index is None:
        return True, 'display renderer state unchanged because display index is unavailable', {}
    if state == 'off':
        ok, message = stop_display_renderer(display_index)
        return ok, message, {'renderer_stopped': ok, 'renderer_stop_message': message}
    if state == 'on' and not display_renderer_active(display_index):
        ok, message = start_display_renderer(display_index)
        return ok, message, {'renderer_started': ok, 'renderer_start_message': message}
    return True, 'display renderer already active', {'renderer_started': False}


def _upload_preview_frame(server: str, client_id: str, display_index: int, frame: bytes, cache_dir: Path) -> tuple[bool, str]:
    token = _preview_token(cache_dir)
    if not token:
        return False, 'client authentication token is unavailable'
    url = (server.rstrip('/') + '/clients/' + urllib.request.quote(client_id, safe='') +
           '/screenshot?display=' + str(display_index))
    request = urllib.request.Request(
        url,
        data=frame,
        method='POST',
        headers={
            'Content-Type': 'image/png',
            'X-SCREENTRA-CLIENT-TOKEN': token,
        },
    )
    try:
        with urlopen_ripley(request, timeout=20, cache_dir=cache_dir) as response:
            if response.status not in (200, 204):
                return False, f'upload returned HTTP {response.status}'
        return True, ''
    except Exception as exc:
        return False, f'upload failed: {exc}'


MAX_PREVIEW_FRAME_BYTES = 16 * 1024 * 1024
MAX_WLR_JSON_BYTES = 1 << 20
MAX_WLR_OUTPUTS = 32


def _require_bounded_text(value: str, maximum: int) -> str:
    if not isinstance(value, str) or len(value.encode('utf-8')) > maximum:
        raise ValueError('process text exceeds its bound')
    return value


def _bounded_check_output(args: list[str], maximum: int, timeout: float, env: dict[str, str] | None = None) -> str:
    process = subprocess.Popen(
        args,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        env=env,
    )
    assert process.stdout is not None
    chunks: list[bytes] = []
    total = 0
    deadline = time.monotonic() + timeout
    try:
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise subprocess.TimeoutExpired(args, timeout)
            ready, _, _ = select.select([process.stdout], [], [], remaining)
            if not ready:
                raise subprocess.TimeoutExpired(args, timeout)
            chunk = os.read(process.stdout.fileno(), min(65536, maximum + 1 - total))
            if not chunk:
                break
            total += len(chunk)
            if total > maximum:
                raise ValueError('process output exceeds its bound')
            chunks.append(chunk)
        remaining = deadline - time.monotonic()
        returncode = process.wait(timeout=max(remaining, 0.001))
        if returncode != 0:
            raise subprocess.CalledProcessError(returncode, args)
        return b''.join(chunks).decode('utf-8')
    except BaseException:
        if process.poll() is None:
            process.kill()
        try:
            process.wait(timeout=1)
        except Exception:
            pass
        raise
    finally:
        process.stdout.close()


def _bounded_capture_stdout(args: list[str], maximum: int, timeout: float, env: dict[str, str] | None = None) -> tuple[int, bytes]:
    process = subprocess.Popen(
        args,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        env=env,
    )
    assert process.stdout is not None
    chunks: list[bytes] = []
    total = 0
    deadline = time.monotonic() + timeout
    try:
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise subprocess.TimeoutExpired(args, timeout)
            ready, _, _ = select.select([process.stdout], [], [], remaining)
            if not ready:
                raise subprocess.TimeoutExpired(args, timeout)
            chunk = os.read(process.stdout.fileno(), min(65536, maximum + 1 - total))
            if not chunk:
                break
            total += len(chunk)
            if total > maximum:
                raise ValueError('process output exceeds its bound')
            chunks.append(chunk)
        remaining = deadline - time.monotonic()
        returncode = process.wait(timeout=max(remaining, 0.001))
        return returncode, b''.join(chunks)
    except BaseException:
        if process.poll() is None:
            process.kill()
        try:
            process.wait(timeout=1)
        except Exception:
            pass
        raise
    finally:
        process.stdout.close()


def _validate_preview_frame(frame: bytes) -> bytes:
    if not isinstance(frame, bytes) or not frame or len(frame) > MAX_PREVIEW_FRAME_BYTES:
        raise ValueError('preview frame is not bounded')
    return frame


def run_preview_loop(server: str, client_id: str, cache_dir: Path, display_index: int, interval: int, duration: int) -> int:
    """Capture one physical Wayland display until stopped or the session expires."""
    pid_path = _preview_pid_path(cache_dir, display_index)
    state_path = _preview_state_path(cache_dir, display_index)
    connector = _preview_connector(cache_dir, display_index)
    if shutil.which('grim') is None:
        _write_preview_state(cache_dir, display_index, {'status': 'unavailable', 'message': 'grim is not installed'})
        return 1
    if not connector:
        _write_preview_state(cache_dir, display_index, {'status': 'unavailable', 'message': 'physical display connector is unavailable'})
        return 1
    started = time.time()
    _write_preview_state(cache_dir, display_index, {'status': 'starting', 'connector': connector, 'started_at': started})
    try:
        while time.time() - started < duration:
            command = ['grim', '-o', connector, '-']
            try:
                returncode, frame = _bounded_capture_stdout(
                    command, MAX_PREVIEW_FRAME_BYTES, 20, env=_preview_wayland_env()
                )
            except ValueError:
                _write_preview_state(cache_dir, display_index, {'status': 'unavailable', 'connector': connector, 'message': 'grim capture exceeded the preview frame bound'})
                time.sleep(interval)
                continue
            except Exception as exc:
                _write_preview_state(cache_dir, display_index, {'status': 'error', 'connector': connector, 'message': f'capture failed: {_bound_hardware_message(str(exc))}'})
                time.sleep(interval)
                continue
            try:
                frame = _validate_preview_frame(frame)
            except ValueError:
                _write_preview_state(cache_dir, display_index, {'status': 'unavailable', 'connector': connector, 'message': 'grim capture exceeded the preview frame bound'})
                time.sleep(interval)
                continue
            if returncode != 0:
                _write_preview_state(cache_dir, display_index, {'status': 'unavailable', 'connector': connector, 'message': 'grim could not capture the display'})
                time.sleep(interval)
                continue
            ok, message = _upload_preview_frame(server, client_id, display_index, frame, cache_dir)
            if ok:
                _write_preview_state(cache_dir, display_index, {
                    'status': 'streaming', 'connector': connector, 'started_at': started,
                    'last_frame_at': time.time(), 'bytes': len(frame),
                })
            else:
                _write_preview_state(cache_dir, display_index, {'status': 'error', 'connector': connector, 'message': message})
            time.sleep(interval)
    finally:
        try:
            if _read_bounded_text(pid_path, 64).strip() == str(os.getpid()):
                pid_path.unlink()
        except Exception:
            pass
        try:
            state = _read_json_file(state_path, {})
            state.update({'status': 'stopped', 'stopped_at': time.time()})
            _write_preview_state(cache_dir, display_index, state)
        except Exception:
            pass
    return 0


def _validate_preview_pid(value: str) -> int:
    if not isinstance(value, str) or re.fullmatch(r'[1-9][0-9]{0,6}', value) is None:
        raise ValueError('preview process id is invalid')
    pid = int(value)
    if pid > 4_194_304:
        raise ValueError('preview process id is outside the supported range')
    return pid


def _preview_process_alive(pid_path: Path) -> bool:
    try:
        pid = _validate_preview_pid(_read_bounded_text(pid_path, 64).strip())
        os.kill(pid, 0)
        return True
    except Exception:
        return False


def start_display_preview(ctx: dict, display_index: int, payload: dict) -> tuple[bool, str, dict]:
    if not isinstance(payload, dict):
        return False, 'preview payload must be an object', {}
    cache_dir = Path(ctx['cache_dir'])
    _ensure_real_directory(cache_dir, create=True)
    if shutil.which('grim') is None:
        message = 'grim is not installed on this client'
        _write_preview_state(cache_dir, display_index, {'status': 'unavailable', 'message': message})
        return False, message, {'display_index': display_index}
    if _preview_connector(cache_dir, display_index) is None:
        message = 'physical display connector is unavailable'
        _write_preview_state(cache_dir, display_index, {'status': 'unavailable', 'message': message})
        return False, message, {'display_index': display_index}
    pid_path = _preview_pid_path(cache_dir, display_index)
    if _preview_process_alive(pid_path):
        return True, 'display preview is already running', {'display_index': display_index}
    try:
        raw_interval = payload.get('interval_seconds', 5)
        raw_duration = payload.get('duration_seconds', 120)
        if any(isinstance(value, bool) or not isinstance(value, int) for value in (raw_interval, raw_duration)):
            raise ValueError
        interval = max(1, min(raw_interval, 30))
        duration = max(10, min(raw_duration, 300))
    except (TypeError, ValueError, OverflowError):
        return False, 'preview interval and duration must be numeric', {}
    command = [
        sys.executable, str(Path(__file__).resolve()), '--preview-loop',
        '--server', ctx['server'], '--client-id', ctx['client_id'],
        '--cache-dir', str(cache_dir), '--display-index', str(display_index),
        '--preview-interval', str(interval), '--preview-duration', str(duration),
    ]
    try:
        process = subprocess.Popen(
            command, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL, start_new_session=True,
        )
        _atomic_write_text(pid_path, str(process.pid))
        time.sleep(0.05)
        if process.poll() is not None:
            state = _read_json_file(_preview_state_path(cache_dir, display_index), {})
            try:
                pid_path.unlink()
            except FileNotFoundError:
                pass
            return False, state.get('message') or 'display preview process exited immediately', state
        return True, 'display preview started', {'display_index': display_index, 'interval_seconds': interval, 'duration_seconds': duration}
    except Exception as exc:
        return False, f'display preview could not start: {_bound_hardware_message(str(exc))}', {}


def _terminate_preview_process(pid: int, timeout: float = 2.0) -> bool:
    """Terminate a preview child and reap it before reporting success."""
    deadline = time.monotonic() + max(0.1, min(timeout, 10.0))
    try:
        os.kill(pid, signal.SIGTERM)
    except ProcessLookupError:
        return True
    except OSError:
        return False
    while time.monotonic() < deadline:
        try:
            waited, _ = os.waitpid(pid, os.WNOHANG)
            if waited == pid:
                return True
        except ChildProcessError:
            try:
                os.kill(pid, 0)
            except ProcessLookupError:
                return True
            except OSError:
                return False
        except OSError:
            return False
        time.sleep(0.05)
    try:
        os.kill(pid, signal.SIGKILL)
    except ProcessLookupError:
        return True
    except OSError:
        return False
    while time.monotonic() < deadline + 1.0:
        try:
            waited, _ = os.waitpid(pid, os.WNOHANG)
            if waited == pid:
                return True
        except ChildProcessError:
            try:
                os.kill(pid, 0)
            except ProcessLookupError:
                return True
            except OSError:
                return False
        except OSError:
            return False
        time.sleep(0.05)
    return False


def stop_display_preview(ctx: dict, display_index: int) -> tuple[bool, str, dict]:
    cache_dir = Path(ctx['cache_dir'])
    pid_path = _preview_pid_path(cache_dir, display_index)
    try:
        pid = _validate_preview_pid(_read_bounded_text(pid_path, 64).strip())
    except Exception:
        pid = None
    if pid is not None and not _terminate_preview_process(pid):
        return False, 'display preview could not stop before its timeout', {}
    try:
        pid_path.unlink()
    except FileNotFoundError:
        pass
    _write_preview_state(cache_dir, display_index, {'status': 'stopped', 'stopped_at': time.time()})
    return True, 'display preview stopped', {'display_index': display_index}


def fetch_config(server: str, client_id: str, cache_dir: Path, display_index: int | None = None) -> bool:
    token = _preview_token(cache_dir)
    if not token:
        print('config refresh skipped: client authentication token is unavailable', flush=True)
        return False
    if display_index is None:
        url = server.rstrip('/') + '/api/clients/' + urllib.request.quote(client_id, safe='') + '/config'
        target = 'assigned-layout.json'
    else:
        url = (server.rstrip('/') + '/api/clients/' + urllib.request.quote(client_id, safe='') +
               '/displays/' + str(display_index) + '/config')
        target = f'assigned-layout-display-{display_index}.json'
    try:
        request = urllib.request.Request(
            url,
            headers={'X-SCREENTRA-CLIENT-TOKEN': token},
        )
        with urlopen_ripley(request, timeout=10, cache_dir=cache_dir) as r:
            data = json.loads(_read_bounded_response(r, MAX_SERVER_RESPONSE_BYTES))
        # The server display-config payload does not include local-admin theme.
        try:
            previous = _read_json_file(cache_dir / 'client-config.json', {})
            if previous.get('theme'):
                data['theme'] = previous['theme']
        except Exception:
            pass
        _ensure_real_directory(cache_dir, create=True)
        # Keep the complete response for heartbeat acknowledgement. Secondary
        # displays retain their own metadata; their layout cache remains layout-only
        # for the local renderer.
        if display_index is None or display_index == 0:
            _atomic_write_text(Path(cache_dir, 'client-config.json'), json.dumps(data, indent=2))
        else:
            _atomic_write_text(Path(cache_dir, f'display-config-{display_index}.json'), json.dumps(data, indent=2))
        if data.get('layout') is not None:
            _atomic_write_text(Path(cache_dir, target), json.dumps(data['layout'], indent=2))
            if display_index in (None, 0):
                _atomic_write_text(Path(cache_dir, 'assigned-layout.json'), json.dumps(data['layout'], indent=2))
        else:
            # Clear stale per-display and legacy display-0 layout caches when assignment is cleared.
            stale_files = [Path(cache_dir, target)]
            if display_index in (None, 0):
                stale_files.append(Path(cache_dir, 'assigned-layout.json'))
            for stale in stale_files:
                try:
                    stale.unlink()
                except FileNotFoundError:
                    pass
        return True
    except Exception as e:
        print(f'config refresh failed: {e}', flush=True)
        return False


def fetch_all_display_configs(server: str, client_id: str, cache_dir: Path) -> bool:
    """Refresh all known display layout caches after a scene/multi-display publish."""
    try:
        url = server.rstrip('/') + '/api/clients/' + urllib.request.quote(client_id, safe='') + '/displays'
        token = _preview_token(cache_dir)
        if not token:
            raise RuntimeError('client authentication token is unavailable')
        request = urllib.request.Request(
            url,
            headers={'X-SCREENTRA-CLIENT-TOKEN': token},
        )
        with urlopen_ripley(request, timeout=10, cache_dir=cache_dir) as r:
            displays = json.loads(_read_bounded_response(r, MAX_SERVER_RESPONSE_BYTES))
        ok = True
        seen = set()
        for d in displays:
            idx = d.get('display_index')
            if idx is None:
                continue
            try:
                idx = int(idx)
            except Exception:
                continue
            seen.add(idx)
            ok = fetch_config(server, client_id, cache_dir, display_index=idx) and ok
        # For known stale per-display caches not reported anymore, leave them alone;
        # heartbeat/display detection owns lifecycle. Missing assignment for a reported
        # display is cleared by fetch_config(display_index=idx).
        return ok
    except Exception as e:
        print(f'all-display config refresh failed: {e}', flush=True)
        return False


def _write_update_request_marker(path: Path, marker: dict) -> None:
    parent = path.parent
    metadata = os.lstat(parent)
    if stat.S_ISLNK(metadata.st_mode) or not stat.S_ISDIR(metadata.st_mode):
        raise ValueError('update request parent is not a real directory')
    raw = json.dumps(marker, separators=(',', ':'), allow_nan=False).encode('utf-8')
    if len(raw) > 4096:
        raise ValueError('client update refused: request marker is too large')
    descriptor, temporary_name = tempfile.mkstemp(
        prefix=f'.{path.name}.', suffix='.tmp', dir=str(parent)
    )
    temporary = Path(temporary_name)
    try:
        with os.fdopen(descriptor, 'wb') as handle:
            handle.write(raw)
        os.chmod(temporary, 0o600)
        os.replace(temporary, path)
        temporary = None
    finally:
        if temporary is not None:
            try:
                temporary.unlink()
            except FileNotFoundError:
                pass


def update_client(server: str, client_id: str, cache_dir: Path, payload: dict | None) -> tuple[bool, str, dict]:
    """Request the root-owned verifier; never download or execute as the player."""
    payload = payload or {}
    if not isinstance(payload, dict):
        return False, 'Client update payload must be an object', {}
    request_path = Path('/var/lib/screentra-cast/update/request')
    details = {'request_path': str(request_path), 'client_id': client_id or None}
    if payload.get('dry_run'):
        details['dry_run'] = True
        return True, 'would request the root-owned signed updater', details
    if not request_path.parent.is_dir():
        return False, 'client update request failed: root updater directory is unavailable', details
    marker = {
        'requested_at': datetime.datetime.now(datetime.timezone.utc).isoformat().replace('+00:00', 'Z'),
        'client_id': client_id or '',
        'reason': 'server-command',
    }
    try:
        _write_update_request_marker(request_path, marker)
        return True, 'verified root client updater requested', details
    except Exception as e:
        details['error'] = _bound_hardware_message(str(e))
        return False, f'client update request failed: {_bound_hardware_message(str(e))}', details


def execute(action: str, payload: dict | None, ctx: dict) -> dict:
    """Run a single command and return a result dict."""
    started = datetime.datetime.now(datetime.timezone.utc).replace(tzinfo=None).isoformat() + 'Z'
    if action == 'client_service_status':
        try:
            request = _STATUS_REQUEST_ADMISSION.validate_status_request(payload)
            evidence = _STATUS_REQUEST_ADMISSION.build_status_request_evidence(request)
        except ValueError as exc:
            return {
                'action': action, 'ok': False, 'code': 'invalid_payload',
                'execution_enabled': False, 'message': str(exc), 'started_at': started,
            }
        return {
            'action': action, 'ok': True, 'code': 'execution_disabled',
            'execution_enabled': False, 'state': 'disabled',
            'message': 'client service runtime execution is disabled',
            'details': dict(evidence),
            'started_at': started,
        }
    if action == 'client_service_rollback_request':
        try:
            request = _ROLLBACK_REQUEST_ADMISSION.validate_rollback_request(payload)
            evidence = _ROLLBACK_REQUEST_ADMISSION.build_rollback_request_evidence(request)
        except ValueError as exc:
            return {
                'action': action, 'ok': False, 'code': 'invalid_payload',
                'execution_enabled': False, 'message': str(exc), 'started_at': started,
            }
        return {
            'action': action, 'ok': False, 'code': 'execution_disabled',
            'execution_enabled': False, 'state': 'disabled',
            'status': evidence['status'],
            'message': 'client service rollback is disabled',
            'details': dict(evidence),
            'started_at': started,
        }
    if action == 'client_service_transcribe_request':
        try:
            request = _TRANSCRIPTION_REQUEST_ADMISSION.validate_transcription_request(payload)
            evidence = _TRANSCRIPTION_REQUEST_ADMISSION.build_transcription_request_evidence(request)
        except ValueError as exc:
            return {
                'action': action, 'ok': False, 'code': 'invalid_payload',
                'execution_enabled': False, 'message': str(exc), 'started_at': started,
            }
        details = dict(evidence)
        details.update({'audio_bytes': 0, 'transcript': None})
        return {
            'action': action, 'ok': False, 'code': 'execution_disabled',
            'execution_enabled': False, 'state': 'disabled', 'status': evidence['status'],
            'message': 'client service transcription is disabled',
            'details': details,
            'started_at': started,
        }
    payload = {} if payload is None else payload
    if not isinstance(payload, dict):
        return {'action': action, 'ok': False, 'code': 'invalid_payload', 'message': 'client command payload must be an object', 'started_at': started}
    if action not in ALLOWED_CLIENT_COMMAND_ACTIONS:
        return {
            'action': action, 'ok': False, 'code': 'unsupported_action',
            'execution_enabled': False, 'message': f'unknown action: {action}',
            'started_at': started,
        }
    raw_display_index = payload.get('display_index')
    if raw_display_index is None:
        display_index = None
    else:
        try:
            display_index = _validate_display_index(raw_display_index)
        except ValueError as exc:
            return {'action': action, 'ok': False, 'code': 'invalid_payload', 'message': str(exc), 'started_at': started}
    has_user = has_systemctl_user()

    def unit(base: str) -> str:
        return f'{base}@{display_index}.service' if display_index is not None else f'{base}.service'

    if action == 'restart_renderer':
        if has_user:
            ok, msg = systemctl_user('restart', unit('screentra-client-renderer'))
            if ok:
                return {'action': action, 'display_index': display_index, 'ok': True, 'message': msg, 'started_at': started}
        ok, msg = restart_system_kiosk()
        return {'action': action, 'display_index': display_index, 'ok': ok,
                'message': (f'kiosk service restarted; renderer display {display_index if display_index is not None else 0} refreshed'
                            if ok else f'renderer restart failed: {msg}'), 'details': {'scope': 'client_kiosk_service', 'systemd_message': msg}, 'started_at': started}

    if action == 'restart_kiosk':
        if has_user:
            ok, msg = systemctl_user('restart', unit('screentra-kiosk-browser'))
            if ok:
                return {'action': action, 'display_index': display_index, 'ok': True, 'message': msg, 'started_at': started}
        ok, msg = restart_system_kiosk()
        return {'action': action, 'display_index': display_index, 'ok': ok,
                'message': 'kiosk service restarted' if ok else f'kiosk restart failed: {msg}',
                'details': {'scope': 'client_kiosk_service', 'systemd_message': msg}, 'started_at': started}

    # Alias used by the touchscreen selection flow. Forward to the display-scoped
    # kiosk restart unit so the queued action is no longer dropped as unknown.
    if action == 'restart_kiosk_display':
        if has_user:
            ok, msg = systemctl_user('restart', unit('screentra-kiosk-browser'))
            if ok:
                return {'action': 'restart_kiosk', 'display_index': display_index, 'ok': True, 'message': msg, 'started_at': started}
        ok, msg = restart_system_kiosk()
        return {'action': 'restart_kiosk', 'display_index': display_index, 'ok': ok,
                'message': 'kiosk service restarted' if ok else f'kiosk restart failed: {msg}',
                'details': {'scope': 'client_kiosk_service', 'systemd_message': msg}, 'started_at': started}

    if action == 'reload_layout':
        cache_path = Path(ctx['cache_dir']) / 'client-config.json'
        requested_orientation = payload.get('orientation')
        try:
            if requested_orientation is not None and not isinstance(requested_orientation, str):
                raise ValueError('orientation must be a string')
            all_displays = _validate_optional_bool(payload.get('all_displays', False), 'all_displays')
        except ValueError as exc:
            return {'action': action, 'display_index': display_index, 'ok': False, 'code': 'invalid_payload', 'message': str(exc), 'started_at': started}
        if requested_orientation is not None and requested_orientation not in {'normal', 'flipped', 'rotated_left', 'rotated_right'}:
            return {'action': action, 'display_index': display_index, 'ok': False, 'message': 'invalid orientation', 'started_at': started}
        try:
            before_orientation = _read_json_file(cache_path, {}).get('orientation', 'normal')
        except Exception:
            before_orientation = 'normal'
        if all_displays or display_index is None:
            ok = fetch_all_display_configs(ctx['server'], ctx['client_id'], Path(ctx['cache_dir']))
            after_orientation = before_orientation
            try:
                after_orientation = _read_json_file(cache_path, {}).get('orientation', 'normal')
            except Exception:
                pass
            restarted = False
            restart_message = ''
            if ok and (after_orientation != before_orientation or (requested_orientation is not None and after_orientation == requested_orientation)):
                rc, out, err = _run_list(['sudo', '-n', 'systemctl', 'restart', 'screentra-cast-kiosk@0.service'])
                restarted = rc == 0
                restart_message = _bound_hardware_message(out or err)
            return {'action': action, 'display_index': display_index, 'all_displays': True, 'ok': ok and (restarted or (after_orientation == before_orientation and requested_orientation is None)), 'orientation_changed': after_orientation != before_orientation or requested_orientation is not None, 'kiosk_restarted': restarted, 'message': 'all display configs re-fetched' + ('; kiosk restarted for orientation change' if restarted else (': ' + restart_message if restart_message else '')), 'started_at': started}
        ok = fetch_config(ctx['server'], ctx['client_id'], Path(ctx['cache_dir']), display_index=display_index)
        after_orientation = before_orientation
        try:
            after_orientation = _read_json_file(cache_path, {}).get('orientation', 'normal')
        except Exception:
            pass
        restarted = False
        restart_message = ''
        if ok and (after_orientation != before_orientation or (requested_orientation is not None and after_orientation == requested_orientation)):
            restart_index = display_index if display_index is not None else 0
            rc, out, err = _run_list(['sudo', '-n', 'systemctl', 'restart', f'screentra-cast-kiosk@{restart_index}.service'])
            restarted = rc == 0
            restart_message = _bound_hardware_message(out or err)
        return {'action': action, 'display_index': display_index, 'ok': ok and (restarted or (after_orientation == before_orientation and requested_orientation is None)), 'orientation_changed': after_orientation != before_orientation or requested_orientation is not None, 'kiosk_restarted': restarted, 'message': 'config re-fetched' + ('; kiosk restarted for orientation change' if restarted else (': ' + restart_message if restart_message else '')), 'started_at': started}

    if action == 'refresh_cache':
        try:
            all_displays = _validate_optional_bool(payload.get('all_displays', False), 'all_displays')
        except ValueError as exc:
            return {'action': action, 'display_index': display_index, 'ok': False, 'code': 'invalid_payload', 'message': str(exc), 'started_at': started}
        if all_displays or display_index is None:
            ok = fetch_all_display_configs(ctx['server'], ctx['client_id'], Path(ctx['cache_dir']))
            return {'action': action, 'display_index': display_index, 'all_displays': True, 'ok': ok, 'message': 'all display caches refreshed' if ok else 'one or more display cache refreshes failed', 'started_at': started}
        ok = fetch_config(ctx['server'], ctx['client_id'], Path(ctx['cache_dir']), display_index=display_index)
        return {'action': action, 'display_index': display_index, 'ok': ok, 'message': 'cache refreshed' if ok else 'cache refresh failed', 'started_at': started}

    if action == 'update_client':
        ok, msg, details = update_client(ctx['server'], ctx['client_id'], Path(ctx['cache_dir']), payload)
        return {'action': action, 'display_index': display_index, 'ok': ok, 'message': msg, 'details': details, 'started_at': started}

    if action == 'set_display_count':
        ok, msg, details = apply_display_count(Path(ctx['cache_dir']), payload.get('max_displays'))
        return {'action': action, 'display_index': display_index, 'ok': ok, 'message': msg, 'details': details, 'started_at': started}

    if action == 'set_touch_binding':
        try:
            ok, msg, details = apply_touch_binding(Path(ctx['cache_dir']), payload)
        except (TypeError, ValueError, OverflowError) as exc:
            return {'action': action, 'display_index': display_index, 'ok': False, 'code': 'invalid_payload', 'message': str(exc), 'started_at': started}
        return {'action': action, 'display_index': display_index, 'ok': ok, 'message': msg, 'details': details, 'started_at': started}

    if action == 'bluetooth_scan':
        ok, msg, details = bluetooth_scan(payload)
        return {'action': action, 'ok': ok, 'message': msg, 'details': details, 'started_at': started}

    if action == 'bluetooth_connect':
        ok, msg, details = bluetooth_connect(payload)
        return {'action': action, 'ok': ok, 'message': msg, 'details': details, 'started_at': started}

    if action == 'audio_list':
        ok, msg, details = audio_list()
        return {'action': action, 'ok': ok, 'message': msg, 'details': details, 'started_at': started}

    if action == 'audio_set_default':
        ok, msg, details = audio_set_default(payload)
        return {'action': action, 'ok': ok, 'message': msg, 'details': details, 'started_at': started}

    if action == 'audio_set_volume':
        ok, msg, details = audio_set_volume(payload)
        return {'action': action, 'ok': ok, 'message': msg, 'details': details, 'started_at': started}

    if action == 'restart_client':
        ok, msg, details = restart_client()
        return {'action': action, 'ok': ok, 'message': msg, 'details': details, 'started_at': started}

    if action == 'start_display_preview':
        if display_index is None or display_index < 0 or display_index > 3:
            return {'action': action, 'ok': False, 'message': 'a display index from 0 to 3 is required', 'started_at': started}
        ok, msg, details = start_display_preview(ctx, display_index, payload)
        return {'action': action, 'display_index': display_index, 'ok': ok, 'message': msg, 'details': details, 'started_at': started}

    if action == 'stop_display_preview':
        if display_index is None or display_index < 0 or display_index > 3:
            return {'action': action, 'ok': False, 'message': 'a display index from 0 to 3 is required', 'started_at': started}
        ok, msg, details = stop_display_preview(ctx, display_index)
        return {'action': action, 'display_index': display_index, 'ok': ok, 'message': msg, 'details': details, 'started_at': started}

    # ── Scheduled DDC display settings ────────────────────────────────────────
    if action == 'display_settings':
        try:
            validate_pending_command({'client_id': ctx.get('client_id', ''), 'action': action, 'payload': payload}, ctx.get('client_id', ''))
        except ValueError as exc:
            return {'action': action, 'display_index': display_index, 'ok': False, 'code': 'invalid_payload', 'message': str(exc), 'started_at': started}
        results = []
        for field, vcp in (('brightness', '10'), ('contrast', '12')):
            if field not in payload:
                continue
            ok, msg = apply_ddc_control(vcp, payload[field])
            results.append({'setting': field, 'vcp': vcp, 'value': payload[field], 'ok': ok, 'message': msg})
        succeeded = sum(1 for result in results if result['ok'])
        ok = succeeded == len(results)
        return {
            'action': action, 'display_index': display_index, 'ok': ok,
            'partial': 0 < succeeded < len(results), 'message': 'DDC display settings applied' if ok else ('DDC display settings partially applied' if succeeded else 'DDC display settings failed'),
            'details': {'settings': results, **{result['setting']: result for result in results}}, 'started_at': started,
        }

    if action == 'display_power':
        state = payload.get('state')
        ok, message, details = apply_display_power(Path(ctx['cache_dir']), display_index, state)
        return {'action': action, 'display_index': display_index, 'ok': ok, 'message': message, 'details': details, 'started_at': started}

    # ── Display control (DDC/CI + CEC) ────────────────────────────────────────
    if action == 'ddc_set':
        # payload: { "vcp": "10", "value": 50 }  brightness
        # or { "vcp": "D6", "value": 1 }          power on OSD addr 0
        # optional: { "bus": "1" }                specific display bus
        # optional: { "has_layout": true }        retry detect if display not found
        try:
            raw_vcp = payload.get('vcp', '')
            if isinstance(raw_vcp, int) and not isinstance(raw_vcp, bool):
                vcp = 'D6' if raw_vcp == 6 else str(raw_vcp)
            else:
                vcp = raw_vcp
            value = _bounded_integer_input(payload.get('value', 0), 0, 65535, 'DDC value')
            bus = payload.get('bus')
            has_layout = payload.get('has_layout', False)
            if not isinstance(vcp, str) or (bus is not None and not isinstance(bus, str)) or not isinstance(has_layout, bool):
                raise ValueError('invalid DDC payload')
            ok, msg = apply_ddc_control(vcp, value, bus=bus, has_layout=has_layout)
        except (TypeError, ValueError, OverflowError) as exc:
            return {'action': action, 'display_index': display_index, 'ok': False, 'code': 'invalid_payload', 'message': str(exc), 'started_at': started}
        details = {}
        if ok and vcp.lower() in {'6', 'd6'} and value in {1, 2, 3, 4}:
            render_state = 'on' if value == 1 else 'off'
            render_ok, render_message, render_details = sync_display_renderer_power_state(display_index, render_state)
            details.update(render_details)
            details['render_state'] = render_state
            if not render_ok:
                return {
                    'action': action, 'display_index': display_index, 'ok': False,
                    'partial': True, 'message': f'{msg}; renderer transition failed: {render_message}',
                    'details': details, 'started_at': started,
                }
            msg = f'{msg}; {render_message}'
        return {'action': action, 'display_index': display_index, 'ok': ok, 'message': msg, 'details': details, 'started_at': started}

    if action == 'cec_send':
        # payload: { "cmd": "pow 0" }   power on OSD addr 0
        # or { "cmd": "standby 0" }    standby/off OSD addr 0
        # or { "cmd": "is 1" }          input HDMI-2
        try:
            cmd = payload.get('cmd', '')
            ok, msg = apply_cec_command(cmd)
        except (TypeError, ValueError, OverflowError) as exc:
            return {'action': action, 'display_index': display_index, 'ok': False, 'code': 'invalid_payload', 'message': str(exc), 'started_at': started}
        details = {}
        if ok and isinstance(cmd, str) and (cmd.startswith('pow ') or cmd.startswith('standby ')):
            render_state = 'on' if cmd.startswith('pow ') else 'off'
            render_ok, render_message, render_details = sync_display_renderer_power_state(display_index, render_state)
            details.update(render_details)
            details['render_state'] = render_state
            if not render_ok:
                return {
                    'action': action, 'display_index': display_index, 'ok': False,
                    'partial': True, 'message': f'{msg}; renderer transition failed: {render_message}',
                    'details': details, 'started_at': started,
                }
            msg = f'{msg}; {render_message}'
        return {'action': action, 'display_index': display_index, 'ok': ok, 'message': msg, 'details': details, 'started_at': started}

    if action == 'ir_send':
        # IR control — stub; hardware integration for later
        return {'action': action, 'display_index': display_index, 'ok': False,
                'message': 'IR control not yet implemented. Set method to DDC or CEC.',
                'started_at': started}

    return {'action': action, 'display_index': display_index, 'ok': False,
            'code': 'unsupported_action', 'execution_enabled': False,
            'message': f'unknown action: {action}', 'started_at': started}


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument('--pending-file', default=os.environ.get('RIPLEY_PENDING_FILE'))
    ap.add_argument('--result-file', default=os.environ.get('RIPLEY_RESULT_FILE'))
    ap.add_argument('--server', default=os.environ.get('RIPLEY_SERVER_URL'))
    ap.add_argument('--client-id', default=os.environ.get('RIPLEY_CLIENT_ID'))
    ap.add_argument('--cache-dir', default=os.environ.get('RIPLEY_CLIENT_CACHE_DIR', os.path.expanduser('~/.local/share/ripley-player-client/cache')))
    ap.add_argument('--watch', action='store_true', help='Loop, processing new pending files as they appear')
    ap.add_argument('--watch-interval', type=float, default=2.0)
    ap.add_argument('--preview-loop', action='store_true', help=argparse.SUPPRESS)
    ap.add_argument('--display-index', type=int)
    ap.add_argument('--preview-interval', type=int, default=5)
    ap.add_argument('--preview-duration', type=int, default=120)
    args = ap.parse_args()

    try:
        args.server = _server_origin(args.server)
    except ValueError as exc:
        ap.error(str(exc))

    if args.preview_loop:
        if not args.client_id or not args.cache_dir or args.display_index is None:
            ap.error('--preview-loop requires --client-id, --cache-dir, and --display-index')
        return run_preview_loop(
            args.server, args.client_id, Path(args.cache_dir), args.display_index,
            max(2, min(args.preview_interval, 30)), max(10, min(args.preview_duration, 300)),
        )

    if not args.pending_file:
        ap.error('--pending-file (or RIPLEY_PENDING_FILE) is required')

    pending_path = Path(args.pending_file)
    result_path = Path(args.result_file) if args.result_file else pending_path.with_name('command-result.json')
    _ensure_real_directory(result_path.parent, create=True)

    ctx = {
        'server': args.server,
        'client_id': args.client_id or '',
        'cache_dir': args.cache_dir,
    }

    def process_one():
        if not pending_path.exists():
            return False
        try:
            spec = _parse_pending_command(
                _read_bounded_text(pending_path, MAX_PENDING_BYTES)
            )
        except Exception as e:
            result = {'action': 'unknown', 'ok': False, 'message': f'bad pending file: {e}', 'started_at': datetime.datetime.now(datetime.timezone.utc).replace(tzinfo=None).isoformat() + 'Z'}
            _atomic_write_text(result_path, json.dumps(result, indent=2))
            try: pending_path.unlink()
            except Exception: pass
            return True
        try:
            validate_pending_command(spec, ctx['client_id'])
        except Exception as e:
            action = spec.get('action', 'unknown') if isinstance(spec, dict) else 'unknown'
            result = {
                'action': action, 'ok': False, 'code': 'invalid_command',
                'execution_enabled': False, 'message': str(e),
                'started_at': datetime.datetime.now(datetime.timezone.utc).replace(tzinfo=None).isoformat() + 'Z',
                'client_id': ctx['client_id'] or None,
                'requested_at': spec.get('requested_at') if isinstance(spec, dict) else None,
            }
            _atomic_write_text(result_path, json.dumps(result, indent=2))
            try: pending_path.unlink()
            except Exception: pass
            return True
        action = spec.get('action', 'unknown')
        result = execute(action, spec.get('payload'), ctx)
        result['finished_at'] = datetime.datetime.now(datetime.timezone.utc).replace(tzinfo=None).isoformat() + 'Z'
        result['client_id'] = ctx['client_id'] or None
        result['requested_at'] = spec.get('requested_at')
        _atomic_write_text(result_path, json.dumps(result, indent=2))
        try: pending_path.unlink()
        except Exception: pass
        print(f'actor: {action} -> ok={result["ok"]} {result["message"]}', flush=True)
        return True

    if args.watch:
        while True:
            if process_one():
                continue
            time.sleep(args.watch_interval)
    else:
        processed = process_one()
        return 0 if processed else 0


if __name__ == '__main__':
    raise SystemExit(main())

PY_ACTOR
chmod +x "$ACTOR"
ln -sf "$ACTOR" "$BIN_DIR/screentra-client-actor"

cat > "$STATUS" <<'SH_STATUS'
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
import stat
config_path = os.environ['CONFIG_PATH']

def reject_duplicate_fields(pairs):
    value = {}
    for key, item in pairs:
        if key in value:
            raise ValueError('duplicate JSON field')
        value[key] = item
    return value

flags = os.O_RDONLY | getattr(os, 'O_NOFOLLOW', 0)
fd = os.open(config_path, flags)
try:
    metadata = os.fstat(fd)
    if not stat.S_ISREG(metadata.st_mode) or metadata.st_size > (1 << 20):
        raise ValueError('client config is not a bounded regular file')
    raw = os.read(fd, (1 << 20) + 1)
finally:
    os.close(fd)
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

SH_STATUS
chmod +x "$STATUS"

cat > "$KIOSK_LAUNCHER" <<'SH_KIOSK'
#!/bin/bash
# screentra-kiosk.sh — Sway compositor plus one cached renderer/browser per display.
# The compositor is shared because one DRM device cannot be owned by multiple
# Sway instances. Each physical output gets its own local renderer and browser.
set -euo pipefail

LOG="/var/log/screentra-cast/kiosk.log"
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
SWAY_CONFIG_DIR="/run/screentra-cast/sway"

install -d -o root -g root -m 0755 /var/log/screentra-cast
install -d -o root -g root -m 0755 /run/screentra-cast
log() { echo "[$(date)] $*" >> "$LOG"; }
log "screentra-kiosk.sh starting (PID=$$)"
log "Running as root, board=$BOARD_USER home=$BOARD_HOME"

mkdir -p "$XDG_RUNTIME_DIR" "$CACHE_DIR" "$SWAY_CONFIG_DIR"
chown "$BOARD_USER:$BOARD_USER" "$XDG_RUNTIME_DIR" "$CACHE_DIR"
chown root:root /run/screentra-cast "$SWAY_CONFIG_DIR"
chmod 0700 "$XDG_RUNTIME_DIR"
chown root:root "$SWAY_CONFIG_DIR"
chmod 0755 "$SWAY_CONFIG_DIR"

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
    if path.exists():
        with path.open('rb') as stream:
            raw = stream.read((1 << 20) + 1)
        if len(raw) > (1 << 20):
            raise ValueError('display config too large')
        cfg = json.loads(raw.decode('utf-8'))
    else:
        cfg = {}
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
    ORIENTATION_VALUE="$(head -c 1048577 "$ORIENTATION_FILE" | python3 -c 'import json,sys; raw=sys.stdin.buffer.read(1048577)
if len(raw) > 1048576: raise SystemExit(2)
print(json.loads(raw.decode("utf-8")).get("orientation","normal"))' 2>/dev/null || echo normal)"
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
chown root:root "$SWAY_CONFIG_DIR/config"
chmod 0644 "$SWAY_CONFIG_DIR/config"
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
        SCREENTRA_FFMPEG_BIN="${SCREENTRA_FFMPEG_BIN:-}" \
        SCREENTRA_FFMPEG_HWACCEL="${SCREENTRA_FFMPEG_HWACCEL:-}" \
        SCREENTRA_VAAPI_DEVICE="${SCREENTRA_VAAPI_DEVICE:-}" \
        SCREENTRA_CAMERA_MAX_FPS="${SCREENTRA_CAMERA_MAX_FPS:-}" \
        SCREENTRA_CAMERA_MAX_WIDTH="${SCREENTRA_CAMERA_MAX_WIDTH:-}" \
        SCREENTRA_LIVE_PLAYER_REDIRECT="${SCREENTRA_LIVE_PLAYER_REDIRECT:-}" \
        python3 "$CLIENT_DIR/screentra-client-renderer.py" \
        --host 127.0.0.1 --port "$port" --cache-dir "$CACHE_DIR" \
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

# The Wayland socket can exist briefly before Sway is accepting IPC. Probe a
# live Sway socket before launching Chromium; otherwise a DPMS wake/restart can
# race the browser into "Connection refused" and leave only the compositor.
LIVE_SWAYSOCK=""
for i in $(seq 1 40); do
    for sock in "$XDG_RUNTIME_DIR"/sway-ipc.*.sock; do
        [ -S "$sock" ] || continue
        if sudo -u "$BOARD_USER" env XDG_RUNTIME_DIR="$XDG_RUNTIME_DIR" SWAYSOCK="$sock" \
            swaymsg -t get_version >/dev/null 2>&1; then
            LIVE_SWAYSOCK="$sock"
            break 2
        fi
    done
    sleep 0.25
done
if [ -z "$LIVE_SWAYSOCK" ]; then
    log "ERROR: Sway IPC was not ready"
    exit 1
fi
log "Sway IPC ready: $LIVE_SWAYSOCK"

# Start one fullscreen Chromium window per display. Each Chromium gets a unique
# app_id so Sway can place it on the matching physical connector.
while IFS='|' read -r idx connector port; do
    [ -n "$idx" ] || continue
    LIVE_READY=0
    for i in $(seq 1 40); do
        if curl -fsS --max-time 2 "http://127.0.0.1:${port}/" | head -c 65537 | \
            python3 -c 'import sys; data=sys.stdin.buffer.read(65537); raise SystemExit(0 if (b"Screentra Cast Display" in data or b"Connecting to live player" in data) else 1)'; then
            LIVE_READY=1
            break
        fi
        sleep 0.5
    done
    # Always open the local renderer. It performs the client-credential
    # capability exchange before redirecting Chromium to the live server;
    # launching /player.html directly would bypass that exchange.
    BROWSER_URL="http://127.0.0.1:${port}/"
    CHROMIUM_USER_DATA="${XDG_RUNTIME_DIR}/chromium-display-${idx}"
    mkdir -p "$CHROMIUM_USER_DATA" "$BOARD_HOME/.config/chromium/Crash Reports"
    chown -R "$BOARD_USER:$BOARD_USER" "$CHROMIUM_USER_DATA" "$BOARD_HOME/.config/chromium"
    chmod 700 "$CHROMIUM_USER_DATA"
    log "Starting Chromium display $idx on $connector (port $port, url $BROWSER_URL)..."
    GPU_ENV_ARGS=()
    GPU_CHROMIUM_ARGS=()
    if [ "${SCREENTRA_GPU_ACCEL:-0}" = "1" ]; then
        # Keep GPU selection hardware-neutral. The active DRM device and VA-API
        # driver vary by client (for example Intel iHD versus AMD radeonsi),
        # so only pass an explicit override when the client unit supplies one.
        if [ -n "${SCREENTRA_DRI_PRIME:-}" ]; then
            GPU_ENV_ARGS+=("DRI_PRIME=${SCREENTRA_DRI_PRIME}")
        fi
        if [ -n "${SCREENTRA_LIBVA_DRIVER_NAME:-}" ]; then
            GPU_ENV_ARGS+=("LIBVA_DRIVER_NAME=${SCREENTRA_LIBVA_DRIVER_NAME}")
        fi
        if [ -n "${SCREENTRA_LIBVA_DRIVERS_PATH:-}" ]; then
            GPU_ENV_ARGS+=("LIBVA_DRIVERS_PATH=${SCREENTRA_LIBVA_DRIVERS_PATH}")
        fi
        GPU_CHROMIUM_ARGS=(
            --use-gl=egl
            --use-angle=gl
            --ignore-gpu-blocklist
            --enable-gpu-rasterization
            --enable-zero-copy
            --enable-native-gpu-memory-buffers
            --enable-accelerated-2d-canvas
            --enable-features=UseOzonePlatform,VaapiVideoDecoder,VaapiVideoEncoder
        )
        log "Chromium GPU acceleration enabled for display $idx (driver selection: automatic or explicit client override)"
    fi
    sudo -u "$BOARD_USER" env \
        SCREENTRA_GPU_ACCEL="${SCREENTRA_GPU_ACCEL:-0}" \
        "${GPU_ENV_ARGS[@]}" \
        HOME="$BOARD_HOME" XDG_RUNTIME_DIR="$XDG_RUNTIME_DIR" \
        WAYLAND_DISPLAY="$WAYLAND_DISPLAY" \
        chromium \
        "${GPU_CHROMIUM_ARGS[@]}" \
        --class="screentra-cast-display-${idx}" \
        --ozone-platform=wayland --hide-cursor --ash-hide-cursor \
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

SH_KIOSK
chmod +x "$KIOSK_LAUNCHER"
ln -sf "$KIOSK_LAUNCHER" "$BIN_DIR/screentra-kiosk-launcher"

install_screentra_cast_service

cat > "$SERVICE" <<EOF
[Unit]
Description=Screentra Cast Player Heartbeat Client
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
ExecStart="$HEARTBEAT" --server "$SERVER_URL" --interval "$INTERVAL" --client-id "$CLIENT_ID" --name "$CLIENT_NAME" --role "$ROLE" --version "$VERSION"
ExecStartPost=-systemctl --user start $ACTOR_SERVICE_NAME
Restart=always
RestartSec=5
Environment=PYTHONUNBUFFERED=1

[Install]
WantedBy=default.target
EOF

cat > "$RENDER_SERVICE" <<EOF
[Unit]
Description=Screentra Cast Cached Player Renderer (display %i)
After=network-online.target screentra-client-heartbeat.service
Wants=network-online.target

[Service]
Type=simple
NoNewPrivileges=yes
PrivateTmp=yes
ProtectSystem=full
ProtectKernelTunables=yes
ProtectKernelModules=yes
ProtectControlGroups=yes
ProtectKernelLogs=yes
RestrictSUIDSGID=yes
LockPersonality=yes
RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6
ExecStart=$RENDERER --host 127.0.0.1 --cache-dir $INSTALL_DIR/cache
Environment=RIPLEY_DISPLAY_INDEX=%i
Environment=RIPLEY_RENDER_BASE_PORT=3900
Environment=RIPLEY_CURSOR_HIDE_SECS=__RIPLEY_CURSOR_HIDE_SECS__
Restart=always
RestartSec=5
Environment=PYTHONUNBUFFERED=1

[Install]
WantedBy=default.target
EOF
# Default the cursor auto-hide delay to 3.5s. The operator can override
# at runtime by editing this Environment= line and reloading the renderer.
sed -i "s|__RIPLEY_CURSOR_HIDE_SECS__|3.5|g" "$RENDER_SERVICE" 2>/dev/null || true

cat > "$KIOSK_SERVICE" <<EOF
[Unit]
Description=Screentra Cast Fullscreen Kiosk Browser (display %i)
After=screentra-client-renderer@%i.service
Wants=screentra-client-renderer@%i.service

[Service]
Type=simple
NoNewPrivileges=yes
PrivateTmp=yes
ProtectSystem=full
ProtectKernelTunables=yes
ProtectKernelModules=yes
ProtectControlGroups=yes
ProtectKernelLogs=yes
RestrictSUIDSGID=yes
LockPersonality=yes
RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6
ExecStart=$KIOSK_LAUNCHER
Restart=on-failure
RestartSec=5
# Wayland-first kiosk environment. The launcher picks /Wayland or
# falls back to X11 if the box already has a DISPLAY set.
Environment=RIPLEY_PLAYER_URL=http://127.0.0.1:3900/
Environment=RIPLEY_DISPLAY_INDEX=%i
Environment=RIPLEY_RENDER_BASE_PORT=3900
# Do NOT force WAYLAND_DISPLAY here. The launcher auto-detects an existing
# Wayland socket or starts ; forcing wayland-0 on a headless/tty session
# makes Chromium restart-loop instead of clean-skipping.
Environment=XDG_RUNTIME_DIR=${XDG_RUNTIME_DIR:-/run/user/%U}

[Install]
WantedBy=default.target
EOF

cat > "$ACTOR_SERVICE_FILE" <<EOF
[Unit]
Description=Screentra Cast player command actor
After=network-online.target

[Service]
Type=oneshot
NoNewPrivileges=yes
PrivateTmp=yes
ProtectSystem=full
ProtectKernelTunables=yes
ProtectKernelModules=yes
ProtectControlGroups=yes
ProtectKernelLogs=yes
RestrictSUIDSGID=yes
LockPersonality=yes
RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6
Environment=RIPLEY_PENDING_FILE=$INSTALL_DIR/cache/pending-command.json
Environment=RIPLEY_RESULT_FILE=$INSTALL_DIR/cache/command-result.json
Environment=SCREENTRA_SERVER_URL=$SERVER_URL
Environment=RIPLEY_CA_CERT=$INSTALL_DIR/screentra-cast-signage-ca.crt
Environment=RIPLEY_CLIENT_AUTH_TOKEN_FILE=$INSTALL_DIR/client-auth-token
Environment=RIPLEY_CLIENT_ID=$CLIENT_ID
Environment=RIPLEY_CLIENT_CACHE_DIR=$INSTALL_DIR/cache
ExecStart="$ACTOR" --pending-file "$INSTALL_DIR/cache/pending-command.json" --result-file "$INSTALL_DIR/cache/command-result.json" --server "$SERVER_URL" --client-id "$CLIENT_ID" --cache-dir "$INSTALL_DIR/cache"

[Install]
WantedBy=default.target
EOF

# SSH/root installs do not have a usable systemd --user bus. Run the
# heartbeat as a system service under the appliance user so online status,
# config refresh, and reconnect recovery survive reboot.
BOARD_USER="${SCREENTRA_BOARD_USER:-board}"
BOARD_HOME="$(getent passwd "$BOARD_USER" | cut -d: -f6)"
: "${BOARD_HOME:=/home/$BOARD_USER}"
SYSTEM_HEARTBEAT_UNIT="/etc/systemd/system/screentra-client-heartbeat.service"
run_privileged tee "$SYSTEM_HEARTBEAT_UNIT" >/dev/null <<EOF_SYSTEM_HEARTBEAT
[Unit]
Description=Screentra Cast Player Heartbeat Client
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=$BOARD_USER
Group=$BOARD_USER
NoNewPrivileges=yes
PrivateTmp=yes
ProtectSystem=full
ProtectKernelTunables=yes
ProtectKernelModules=yes
ProtectControlGroups=yes
ProtectKernelLogs=yes
RestrictSUIDSGID=yes
LockPersonality=yes
RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6
WorkingDirectory=$INSTALL_DIR
Environment=HOME=$BOARD_HOME
Environment=RIPLEY_CLIENT_DIR=$INSTALL_DIR
Environment=RIPLEY_CA_CERT=$INSTALL_DIR/screentra-cast-signage-ca.crt
Environment=PYTHONUNBUFFERED=1
ExecStart=$HEARTBEAT --server "$SERVER_URL" --interval "$INTERVAL" --client-id "$CLIENT_ID" --name "$CLIENT_NAME" --role "$ROLE" --version "$VERSION"
Restart=always
RestartSec=5

[Install]
WantedBy=multi-user.target
EOF_SYSTEM_HEARTBEAT
run_privileged systemctl daemon-reload
run_privileged systemctl enable screentra-client-heartbeat.service
run_privileged systemctl restart screentra-client-heartbeat.service

SYSTEM_ACTOR_UNIT="/etc/systemd/system/screentra-client-actor.service"
run_privileged tee "$SYSTEM_ACTOR_UNIT" >/dev/null <<EOF_SYSTEM_ACTOR
[Unit]
Description=Screentra Cast Player Command Actor
After=network-online.target screentra-client-heartbeat.service

[Service]
Type=simple
User=$BOARD_USER
Group=$BOARD_USER
NoNewPrivileges=yes
PrivateTmp=yes
ProtectSystem=full
ProtectKernelTunables=yes
ProtectKernelModules=yes
ProtectControlGroups=yes
ProtectKernelLogs=yes
RestrictSUIDSGID=yes
LockPersonality=yes
RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6
WorkingDirectory=$INSTALL_DIR
Environment=HOME=$BOARD_HOME
Environment=RIPLEY_PENDING_FILE=$INSTALL_DIR/cache/pending-command.json
Environment=RIPLEY_RESULT_FILE=$INSTALL_DIR/cache/command-result.json
Environment=RIPLEY_SERVER_URL=$SERVER_URL
Environment=RIPLEY_CLIENT_ID=$CLIENT_ID
Environment=RIPLEY_CLIENT_CACHE_DIR=$INSTALL_DIR/cache
Environment=RIPLEY_CA_CERT=$INSTALL_DIR/screentra-cast-signage-ca.crt
Environment=RIPLEY_CLIENT_AUTH_TOKEN_FILE=$INSTALL_DIR/client-auth-token
Environment=PYTHONUNBUFFERED=1
ExecStart=$ACTOR --pending-file $INSTALL_DIR/cache/pending-command.json --result-file $INSTALL_DIR/cache/command-result.json --server "$SERVER_URL" --client-id "$CLIENT_ID" --cache-dir $INSTALL_DIR/cache --watch
Restart=always
RestartSec=2

[Install]
WantedBy=multi-user.target
EOF_SYSTEM_ACTOR

# Remove the retired local admin dashboard from upgrades as well as fresh installs.
run_privileged systemctl disable --now screentra-client-admin.service >/dev/null 2>&1 || true
run_privileged rm -f /etc/systemd/system/screentra-client-admin.service
run_privileged systemctl daemon-reload
run_privileged systemctl enable --now screentra-client-actor.service

# Final ownership pass: generated helpers and service metadata must be
# readable by the appliance runtime user.
run_privileged chown -R "${SCREENTRA_BOARD_USER:-board}:${SCREENTRA_BOARD_USER:-board}" "$INSTALL_DIR"

# Display governance now that the script is on disk.
if [ -x "$DISPLAY_GOVERNANCE" ]; then
    python3 "$DISPLAY_GOVERNANCE" --config-dir "$INSTALL_DIR" || \
        echo "warning: display-governance failed (non-fatal)"
fi

if command -v systemctl >/dev/null 2>&1; then
  # Best-effort: install Wayland kiosk deps before preflight so a fresh
  # Debian/minimal install does not report a false "no display session" blocker.
  if ( command -v apt-get >/dev/null 2>&1 || command -v apt >/dev/null 2>&1 ); then
    NEED=()
    command -v sway >/dev/null 2>&1 || NEED+=("sway")
    command -v wlr-randr >/dev/null 2>&1 || NEED+=("wlr-randr")
    command -v bluetoothctl >/dev/null 2>&1 || NEED+=("bluez")
    command -v wpctl >/dev/null 2>&1 || NEED+=("pipewire" "wireplumber" "libspa-0.2-bluetooth")
    command -v wlrctl >/dev/null 2>&1 || NEED+=("wlrctl")
    command -v grim >/dev/null 2>&1 || NEED+=("grim")
    command -v chromium >/dev/null 2>&1 || command -v chromium-browser >/dev/null 2>&1 || command -v google-chrome >/dev/null 2>&1 || NEED+=("chromium")
    if [ "${#NEED[@]}" -gt 0 ]; then
      echo "Installing missing  kiosk deps: ${NEED[*]}"
      if command -v sudo >/dev/null 2>&1; then SUDO=sudo; else SUDO=""; fi
      $SUDO apt-get update -y >/dev/null 2>&1 || true
      $SUDO apt-get install -y --no-install-recommends "${NEED[@]}" >/dev/null 2>&1 || true
    fi
  fi

  # Preflight is run AFTER /run/user/UID is created and the seat services
  # are started. See the end of the seat-service block below. systemd-logind
  # creates this on first TTY login, but an SSH-only install won't trigger
  # that. Create it manually so the  seat service can write its
  # wayland-0 socket even before the user has logged in to a TTY.
  UID_NUM="$(id -u)"
  if [ ! -d "/run/user/$UID_NUM" ]; then
      if run_privileged install -d -o "$USER" -g "$USER" -m 700 "/run/user/$UID_NUM" 2>/dev/null; then
          echo "Created /run/user/$UID_NUM for Wayland sockets."
      else
          echo "WARNING: could not create /run/user/$UID_NUM;  seat service may fail to start."
      fi
  fi

  # Now safe to run preflight — the system-level seat services are started
  # (or deliberately skipped) and /run/user/UID exists.
  "$PREFLIGHT" || true
  echo "preflight ran"

  systemctl --user daemon-reload || true
  systemctl --user enable screentra-client-heartbeat.service || true
  systemctl --user enable screentra-client-actor.service || true
  # Start renderer/browser services for the governed active count. The
  # display-governance config defaults to the detected enabled connectors, but
  # the local/server admin pages can cap it. Display 0 is served by the system
  #  startup hook; display 1+ are user browser services on wayland-0.
  DISPLAY_COUNT="$(python3 - "$INSTALL_DIR/config.json" <<'PY_DETECT'
import json, sys
from pathlib import Path
try:
    path = Path(sys.argv[1])
    with path.open('rb') as stream:
        raw = stream.read((1 << 20) + 1)
    if len(raw) > (1 << 20):
        raise ValueError('config too large')
    cfg=json.loads(raw.decode('utf-8'))
except Exception:
    cfg={}
installer=max(int(cfg.get('installer_detected_max_displays') or 1),1)
override=cfg.get('max_displays')
try:
    active=min(max(int(override),1),installer) if override is not None else installer
except Exception:
    active=installer
print(max(1,min(active,4)))
PY_DETECT
)"
  echo "Active display count $DISPLAY_COUNT; enabling renderer(s) and secondary kiosk browser(s)"
  i=0
  while [ "$i" -lt "$DISPLAY_COUNT" ]; do
    systemctl --user enable "screentra-client-renderer@${i}.service" || true
    systemctl --user restart "screentra-client-renderer@${i}.service" || true
    if [ "$i" = "0" ] && [ "${LABWC_SYSTEM_INSTALLED:-0}" = "1" ]; then
      systemctl --user disable "screentra-kiosk-browser@${i}.service" >/dev/null 2>&1 || true
      systemctl --user stop "screentra-kiosk-browser@${i}.service" >/dev/null 2>&1 || true
    else
      systemctl --user enable "screentra-kiosk-browser@${i}.service" || true
      systemctl --user restart "screentra-kiosk-browser@${i}.service" || true
    fi
    i=$((i + 1))
  done
  systemctl --user restart screentra-client-heartbeat.service || true
fi

# display-governance and the final heartbeat may create/update config files;
# leave the complete appliance tree owned by the runtime user.
run_privileged chown -R "${SCREENTRA_BOARD_USER:-board}:${SCREENTRA_BOARD_USER:-board}" "$INSTALL_DIR"

"$HEARTBEAT" --once --server "$SERVER_URL" --client-id "$CLIENT_ID" --name "$CLIENT_NAME" --role "$ROLE" --version "$VERSION"

echo "Screentra player client installed."
echo "Server: $SERVER_URL"
echo "Client ID: $CLIENT_ID"
echo "Service: screentra-client-heartbeat.service"

echo "Renderer: http://127.0.0.1:3900/"
echo "Kiosk browser service: screentra-kiosk-browser@0.service"
echo "Preferred display service: screentra-cast-kiosk@.service"
echo "Helpers: screentra-client-preflight, screentra-client-status, screentra-client-heartbeat, screentra-client-renderer, screentra-client-actor, screentra-kiosk-launcher"

# ----------------------------------------------------------------------
# Final verification: run a small probe script that confirms the
# install is actually healthy, not just 'every command returned 0'.
# Reports PASS/FAIL for the things that actually matter.
# ----------------------------------------------------------------------
echo
echo "==== Final install verification ===="
"$INSTALL_DIR/screentra-client-preflight.py" || true
echo "==== End final verification ===="

if is_ssh_session; then
    cat <<SSH_DONE
----------------------------------------------------------------------
SSH install complete.

The renderer + heartbeat + actor + user services are running. The client
is now visible in the editor's Connected Clients page.

The system-level  seat service(s) have been enabled and started.
If the service is healthy, the kiosk should already be on screen (or
will appear within a few seconds on the next free TTY, typically tty7).

If the kiosk is NOT on screen after 10 seconds, check:

  sudo journalctl -u screentra-cast-kiosk@0.service -n 20 --no-pager
  who          # see which TTY is active

Common fixes:
  - Walk to the box, press Ctrl+Alt+F7, log in as $USER.
  - If the seat service is in a crash loop, the renderer is still
    serving the cached page at http://127.0.0.1:3900/ — you can verify
    the install was successful from the editor regardless.
----------------------------------------------------------------------
SSH_DONE
fi
