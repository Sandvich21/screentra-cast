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
ACTOR_SERVICE_FILE="$SYSTEMD_USER_DIR/screentra-client-actor.service"
ACTOR_SERVICE_NAME="screentra-client-actor.service"
SERVICE="$SYSTEMD_USER_DIR/screentra-client-heartbeat.service"
RENDER_SERVICE="$SYSTEMD_USER_DIR/screentra-client-renderer@.service"
ADMIN_SERVICE="$SYSTEMD_USER_DIR/screentra-client-admin.service"
KIOSK_SERVICE="$SYSTEMD_USER_DIR/screentra-kiosk-browser@.service"
SYSTEMD_KIOSK_UNIT="/etc/systemd/system/screentra-cast-kiosk@.service"
INSTALLER_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"

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


def sha256_bytes(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def validate_record(raw: object) -> dict[str, str] | None:
    if not isinstance(raw, dict) or raw.get("protocol") != DISCOVERY_PROTOCOL:
        return None
    name = raw.get("server_name")
    url = raw.get("url")
    ca_b64 = raw.get("ca_pem_b64")
    ca_hash = raw.get("ca_sha256")
    if not isinstance(name, str) or not name.strip() or len(name) > 128:
        return None
    if any(ord(char) < 32 for char in name):
        return None
    if not isinstance(url, str) or len(url) > 512:
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
                raw = json.loads(data.decode("utf-8"))
            except (UnicodeDecodeError, json.JSONDecodeError):
                continue
            record = validate_record(raw)
            if record:
                records[(record["url"], record["ca_sha256"])] = record
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
    SERVER_URL="$(cat "$INSTALL_DIR/server" 2>/dev/null || true)"
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
        echo "Enter the Screentra Cast server URL (e.g. https://10.1.1.76:3000):"
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
ExecStart=/usr/local/bin/screentra-kiosk.sh
Restart=always
RestartSec=5
Environment=HOME=/home/board
# AMD amdgpu APUs need the GLES2 renderer for reliable DRM scan-out.
# Pixman can start Sway but repeatedly fails to import the Chromium buffer,
# leaving the physical output black.
Environment=WLR_RENDERER=gles2
Environment=SCREENTRA_DISPLAY_INDEX=%i
Environment=SCREENTRA_RENDER_BASE_PORT=3900
Environment=SCREENTRA_RENDERER_URL=http://127.0.0.1:3900/
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
    marker = json.loads(request_file.read_text(encoding='utf-8')[:4096])
    if not isinstance(marker, dict) or set(marker) - {'requested_at', 'client_id', 'reason'}:
        raise RuntimeError('update request marker is invalid')
    token = auth_token_file.read_text(encoding='utf-8').strip()
    if not token or len(token) > 512:
        raise RuntimeError('client token is invalid')
    current = digest_file.read_text(encoding='ascii').strip() if digest_file.is_file() else ''
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
        actual_sha = hashlib.sha256(installer.read_bytes()).hexdigest()
        if actual_sha != expected_sha:
            raise RuntimeError('update digest verification failed')
        verify = subprocess.run(["openssl", "pkeyutl", "-verify", "-rawin", "-pubin", "-inkey", str(public_key_file), "-sigfile", str(signature), "-in", str(installer)], capture_output=True, timeout=20)
        if verify.returncode != 0:
            raise RuntimeError('update signature verification failed')
        installer.chmod(0o700)
        environment = os.environ.copy()
        environment.update({'SCREENTRA_SERVER_URL': server, 'RIPLEY_CLIENT_ID': client_id, 'SCREENTRA_CLIENT_DIR': str(install_dir)})
        completed = subprocess.run(['/bin/bash', str(installer)], env=environment, capture_output=True, text=True, timeout=300)
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
Cmnd_Alias SCREENTRA_DEVICE_CONTROL = /usr/bin/ddcutil, /usr/bin/cec-client, /usr/bin/systemctl restart screentra-cast-kiosk@*.service, /usr/bin/systemctl try-restart screentra-cast-kiosk@*.service, /usr/bin/systemctl disable --now screentra-cast-kiosk@*.service, /usr/bin/systemctl reboot, /usr/sbin/reboot
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
            echo "python3 python3-venv sudo curl wget openssl ca-certificates dbus libnss3-tools  sway wlrctl seatd chromium ddcutil cec-utils"
            ;;
        fedora|rhel)
            echo "python3 sudo curl wget openssl ca-certificates dbus nss-tools  seatd chromium"
            ;;
        arch)
            echo "python sudo curl wget openssl ca-certificates dbus nss  seatd chromium"
            ;;
        alpine)
            echo "python3 sudo curl wget openssl ca-certificates dbus  seatd chromium"
            ;;
        suse)
            echo "python3 sudo curl wget openssl ca-certificates dbus libnss3-tools  seatd chromium"
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
        case "$(cat "$vf" 2>/dev/null | tr '[:upper:]' '[:lower:]')" in
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
import shutil
import time
import ssl
import socket
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
                capture_output=True, text=True, timeout=2,
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

def tls_context():
    cafile = os.environ.get('SCREENTRA_CA_CERT') or os.environ.get('RIPLEY_CA_CERT') or str(Path.home() / '.local/share/screentra-client/screentra-cast-signage-ca.crt')
    if cafile and Path(cafile).exists():
        try:
            return ssl.create_default_context(cafile=cafile)
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


def run(cmd: str) -> str:
    try:
        return subprocess.check_output(cmd, shell=True, text=True, stderr=subprocess.DEVNULL).strip()
    except Exception:
        return ""


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
    install_dir.mkdir(parents=True, exist_ok=True)
    add("install dir", os.access(install_dir, os.W_OK), str(install_dir))
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
            ok, detail = False, str(e)
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
    printf 'ripley-%s\n' "$(cat /etc/machine-id)" > "$CLIENT_ID_FILE"
  else
    printf 'ripley-%s-%s\n' "$(hostname)" "$(date +%s)" > "$CLIENT_ID_FILE"
  fi
fi
CLIENT_ID="$(cat "$CLIENT_ID_FILE")"
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
import hashlib
import io
import json
import os
import posixpath
import re
import shutil
import ssl
import socket
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

from pathlib import Path

MAX_JSON_FILE_BYTES = 1 << 20


def client_pairing_code(client_id):
    """Return the short physical-confirmation code for a client ID."""
    digest = hashlib.sha256(b'screentra-cast-pairing-v1\0' + str(client_id).encode('utf-8')).digest()
    return f'{int.from_bytes(digest[:4], "big") % 1_000_000:06d}'


def _read_json_file(path: Path, default):
    try:
        if not path.is_file() or path.stat().st_size > MAX_JSON_FILE_BYTES:
            return default
        return json.loads(path.read_text(encoding='utf-8'))
    except Exception:
        return default


def _atomic_write_text(path: Path, text: str, mode: int = 0o600) -> None:
    """Replace a cache JSON file atomically and keep it owner-readable."""
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(f'.{path.name}.{os.getpid()}.tmp')
    temporary.write_text(text)
    os.chmod(temporary, mode)
    os.replace(temporary, path)


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


def run(cmd):
    try:
        return subprocess.check_output(cmd, shell=True, text=True, stderr=subprocess.DEVNULL).strip()
    except Exception:
        return ""


def read_mem():
    values = {}
    try:
        for line in open('/proc/meminfo'):
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
        return int(float(open('/proc/uptime').read().split()[0]))
    except Exception:
        return None


def read_cpu_usage(interval=0.25):
    def sample():
        fields = [int(x) for x in open('/proc/stat').readline().split()[1:]]
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
        return ssl.create_default_context(cafile=cafile)
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
        return Path(path).read_text().strip()
    except OSError:
        return ''


def client_auth_headers(token=None):
    token = client_auth_token() if token is None else str(token).strip()
    return {'X-SCREENTRA-CLIENT-TOKEN': token} if token else {}


ADDON_API_VERSION = '1'
MAX_ADDON_PACKAGE_BYTES = 8 * 1024 * 1024
MAX_ADDON_MEMBER_BYTES = 4 * 1024 * 1024
ADDON_ID_RE = re.compile(r'^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$')


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


def _extract_verified_addon_package(package_bytes, expected):
    if len(package_bytes) > MAX_ADDON_PACKAGE_BYTES:
        raise ValueError('addon package exceeds the client limit')
    staging = Path(tempfile.mkdtemp(prefix='.addon-verify-'))
    try:
        seen = set()
        with tarfile.open(fileobj=io.BytesIO(package_bytes), mode='r:gz') as archive:
            members = archive.getmembers()
            if len(members) > 64:
                raise ValueError('addon package contains too many members')
            for member in members:
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
                target.write_bytes(source.read(member.size + 1))
                if target.stat().st_size != member.size:
                    raise ValueError('addon package member size changed while reading')
        manifest_path = staging / 'manifest.json'
        signature_path = staging / 'manifest.sig'
        if not manifest_path.is_file() or not signature_path.is_file():
            raise ValueError('addon package is missing its manifest or signature')
        manifest_bytes = manifest_path.read_bytes()
        if len(manifest_bytes) > 256 * 1024:
            raise ValueError('addon manifest is too large')
        manifest = json.loads(manifest_bytes.decode('utf-8'))
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
        client_path = staging / entrypoint
        if not client_path.is_file() or hashlib.sha256(client_path.read_bytes()).hexdigest().lower() != digest.lower():
            raise ValueError('addon entrypoint SHA-256 verification failed')
        public_key = addon_public_key_path()
        if not public_key.is_file():
            raise ValueError('addon signing public key is unavailable')
        result = subprocess.run(
            ['openssl', 'pkeyutl', '-verify', '-rawin', '-pubin', '-inkey', str(public_key),
             '-sigfile', str(signature_path), '-in', str(manifest_path)],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            timeout=10,
            check=False,
        )
        if result.returncode != 0:
            raise ValueError('addon manifest signature verification failed')
        return staging, manifest, entrypoint
    except Exception:
        shutil.rmtree(staging, ignore_errors=True)
        raise


def fetch_addon_bytes(server, client_id, addon_id):
    url = (server.rstrip('/') + '/api/clients/' + quote(client_id, safe='') +
           '/addons/' + quote(addon_id, safe='') + '/package')
    req = urllib.request.Request(url, headers=client_auth_headers())
    with urlopen_ripley(req, timeout=20) as response:
        package = response.read(MAX_ADDON_PACKAGE_BYTES + 1)
    if len(package) > MAX_ADDON_PACKAGE_BYTES:
        raise ValueError('addon package exceeds the client limit')
    return package


def sync_client_addons(server, client_id, cache_dir):
    """Fetch and verify enabled addon packages without executing their code."""
    status_path = Path(cache_dir) / 'addon-status.json'
    status = {'at': time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime()), 'addons': []}
    try:
        url = server.rstrip('/') + '/api/clients/' + quote(client_id, safe='') + '/addons'
        req = urllib.request.Request(url, headers=client_auth_headers())
        with urlopen_ripley(req, timeout=10) as response:
            inventory = json.loads(response.read().decode('utf-8'))
        addons = inventory.get('addons') if isinstance(inventory, dict) else None
        if not isinstance(addons, list):
            raise ValueError('server returned an invalid addon inventory')
    except Exception as error:
        status['error'] = str(error)[:256]
        _atomic_write_text(status_path, json.dumps(status, indent=2))
        return status
    addon_root = Path(os.environ.get('RIPLEY_CLIENT_ADDONS_DIR', str(Path(cache_dir).parent / 'addons')))
    addon_root.mkdir(parents=True, exist_ok=True)
    for expected in addons:
        item_status = {}
        try:
            if not isinstance(expected, dict) or not ADDON_ID_RE.fullmatch(str(expected.get('id', ''))):
                raise ValueError('invalid addon inventory entry')
            item_status.update({'id': expected.get('id'), 'version': expected.get('version')})
            if expected.get('client_api_version') != ADDON_API_VERSION:
                raise ValueError('unsupported addon API version')
            if not expected.get('client_sha256'):
                raise ValueError('addon inventory is missing its digest')
            version = expected.get('version')
            if not isinstance(version, str) or not version:
                raise ValueError('addon inventory is missing its version')
            target = addon_root / expected['id'] / version
            active = _read_json_file(target.parent / 'active.json', {})
            if not isinstance(active, dict):
                active = {}
            cached_entrypoint = active.get('entrypoint')
            try:
                if not isinstance(cached_entrypoint, str) or _safe_addon_member(cached_entrypoint) != cached_entrypoint or not cached_entrypoint.startswith('client/'):
                    cached_entrypoint = None
            except Exception:
                cached_entrypoint = None
            cached_file = target / cached_entrypoint if cached_entrypoint else None
            if (active.get('id') == expected['id'] and active.get('version') == version and
                    str(active.get('client_sha256', '')).lower() == str(expected['client_sha256']).lower() and
                    cached_file is not None and cached_file.is_file() and
                    hashlib.sha256(cached_file.read_bytes()).hexdigest().lower() == str(expected['client_sha256']).lower()):
                item_status.update({'state': 'verified_cached', 'entrypoint': cached_entrypoint})
                status['addons'].append(item_status)
                continue
            package = fetch_addon_bytes(server, client_id, expected['id'])
            staging, manifest, entrypoint = _extract_verified_addon_package(package, expected)
            target = addon_root / expected['id'] / manifest['version']
            target.parent.mkdir(parents=True, exist_ok=True)
            if target.exists():
                shutil.rmtree(target)
            os.replace(staging, target)
            _atomic_write_text(target.parent / 'active.json', json.dumps({
                'id': manifest['id'], 'version': manifest['version'],
                'client_sha256': manifest['client_sha256'],
                'entrypoint': entrypoint,
            }, indent=2))
            item_status.update({'state': 'verified', 'entrypoint': entrypoint})
        except Exception as error:
            item_status.update({'state': 'failed', 'error': str(error)[:256]})
        status['addons'].append(item_status)
    _atomic_write_text(status_path, json.dumps(status, indent=2))
    return status


def poll_command(server, client_id, client_token):
    url = server.rstrip('/') + '/api/clients/' + quote(client_id, safe='') + '/command'
    try:
        req = urllib.request.Request(url, headers=client_auth_headers(client_token))
        with urlopen_ripley(req, timeout=5) as r:
            data = r.read().decode()
            return json.loads(data)
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
            return r.read().decode()
    except Exception as e:
        print(f'ack failed: {e}', flush=True)
        return None


def write_pending_command(command, cache_dir):
    pending_path = Path(cache_dir) / 'pending-command.json'
    pending_path.parent.mkdir(parents=True, exist_ok=True)
    _atomic_write_text(pending_path, json.dumps(command, indent=2))
    print(f'pending command written: {command.get("action")}', flush=True)
    try:
        import subprocess, shutil
        if shutil.which('systemctl'):
            subprocess.run(['systemctl', '--user', 'start', 'ripley-player-actor.service'], timeout=10)
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


def fetch_config(server, client_id, cache_dir, display_index=0, config_pattern='assigned-layout-display-{index}.json'):
    url = server.rstrip('/') + '/api/clients/' + quote(client_id, safe='') + '/displays/' + str(display_index) + '/config'
    try:
        req = urllib.request.Request(url, headers=client_auth_headers())
        with urlopen_ripley(req, timeout=10) as r:
            data = json.loads(r.read().decode())
        # Preserve theme (set by heartbeat from local config, sent to server)
        # so it survives across config sync cycles.
        prev_path = Path(cache_dir, 'client-config.json')
        try:
            prev = _read_json_file(prev_path, {})
            if prev.get('theme'):
                data['theme'] = prev['theme']
        except Exception:
            pass
        Path(cache_dir).mkdir(parents=True, exist_ok=True)
        if display_index == 0:
            _atomic_write_text(Path(cache_dir, 'client-config.json'), json.dumps(data, indent=2))
        else:
            _atomic_write_text(Path(cache_dir, f'display-config-{display_index}.json'), json.dumps(data, indent=2))
        target = config_pattern.format(index=display_index)
        if data.get('layout') is not None:
            # backwards-compat: also write the canonical index=0 name
            _atomic_write_text(Path(cache_dir, target), json.dumps(data['layout'], indent=2))
            if display_index == 0:
                _atomic_write_text(Path(cache_dir, 'assigned-layout.json'), json.dumps(data['layout'], indent=2))
        else:
            for stale in [Path(cache_dir, target), Path(cache_dir, 'assigned-layout.json') if display_index == 0 else None]:
                if stale is None: continue
                try: stale.unlink()
                except FileNotFoundError: pass
        return data
    except Exception as e:
        print(f'config sync failed: {e}', flush=True)
        return None


def fetch_all_displays(server, client_id, cache_dir, display_count, config_pattern):
    """Fetch per-display config and populate cache/assigned-layout-display-N.json."""
    for idx in range(display_count):
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
            data = json.loads(r.read().decode())
        users = data.get('users') or []
        if users:
            target = Path(cache_dir).parent / 'admin-users.json'
            target.parent.mkdir(parents=True, exist_ok=True)
            _atomic_write_text(target, json.dumps(data, indent=2))
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
        return r.status, r.read().decode()


def main():
    def _default_server():
        env_url = os.environ.get('RIPLEY_SERVER_URL')
        if env_url:
            return env_url
        # Fallback: read the persisted server URL the installer writes.
        srv = Path.home() / '.local/share/screentra-client/server'
        if srv.exists():
            try:
                v = srv.read_text().strip()
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
        # Refresh configuration before building the registration payload so the
        # heartbeat acknowledges the revision the player has just fetched.
        probe = payload(args)
        try:
            fetch_config(args.server, probe['client_id'], args.cache_dir, display_index=0, config_pattern=args.display_config_pattern)
            fetch_all_displays(args.server, probe['client_id'], args.cache_dir, len(probe.get('displays') or []), args.display_config_pattern)
        except Exception as e:
            print(f'pre-heartbeat config refresh failed: {e}', flush=True)
        data = payload(args)
        try:
            status, body = post(args.server, data)
            try:
                response = json.loads(body)
                server_theme = response.get('theme')
                if server_theme in ('midnight', 'light', 'classic'):
                    theme_path = Path(args.cache_dir) / 'client-config.json'
                    try:
                        cached = _read_json_file(theme_path, {})
                    except Exception:
                        cached = {}
                    cached['theme'] = server_theme
                    theme_path.parent.mkdir(parents=True, exist_ok=True)
                    _atomic_write_text(theme_path, json.dumps(cached, indent=2))
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
        previous_orientation = 'normal'
        try:
            previous_orientation = _read_json_file(Path(args.cache_dir) / 'client-config.json', {}).get('orientation', 'normal')
        except Exception:
            pass
        # Track previous layout assignment to detect changes.
        prev_layout_path = Path(args.cache_dir) / 'previous-layout-id.txt'
        try:
            previous_layout_id = prev_layout_path.read_text().strip() or None
        except Exception:
            previous_layout_id = None
        cfg = fetch_config(args.server, data['client_id'], args.cache_dir, display_index=0, config_pattern=args.display_config_pattern)
        current_layout_id = (cfg or {}).get('assigned_layout_id')
        current_orientation = (cfg or {}).get('orientation', previous_orientation)
        # Restart kiosk on layout assignment change (e.g. after server nudge or editor assign).
        if cfg and current_layout_id and current_layout_id != previous_layout_id:
            try:
                subprocess.run(['sudo', '-n', 'systemctl', 'restart', 'screentra-cast-kiosk@0.service'], check=True, timeout=15)
                print(f'layout changed {previous_layout_id} -> {current_layout_id}; kiosk restarted', flush=True)
                prev_layout_path.write_text(current_layout_id or '')
            except Exception as e:
                print(f'layout-change restart failed: {e}', flush=True)
        elif cfg and current_layout_id:
            prev_layout_path.write_text(current_layout_id or '')
        if cfg and current_orientation != previous_orientation:
            try:
                subprocess.run(['sudo', '-n', 'systemctl', 'restart', 'screentra-cast-kiosk@0.service'], check=True, timeout=15)
                print(f'orientation changed {previous_orientation} -> {current_orientation}; kiosk restarted', flush=True)
            except Exception as e:
                print(f'orientation restart failed: {e}', flush=True)
        fetch_all_displays(args.server, data['client_id'], args.cache_dir, len(data.get('displays') or []), args.display_config_pattern)
        fetch_admin_users(args.server, data['client_id'], args.cache_dir)
        addon_status = sync_client_addons(args.server, data['client_id'], args.cache_dir)
        if addon_status.get('addons') or addon_status.get('error'):
            print(f'addon sync: {json.dumps(addon_status, separators=(",", ":"))}', flush=True)
        assigned = cfg.get('assigned_layout_id') if cfg else None
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
                Path(args.cache_dir, f'display-{idx}.json').write_text(__import__('json').dumps(d))
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
            Path(args.cache_dir).mkdir(parents=True, exist_ok=True)
            (Path(args.cache_dir) / 'last-heartbeat.json').write_text(__import__('json').dumps(status_payload))
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
    let currentDevices = [];
    let selectedPath = null;

    async function refreshStatus() {
      try {
        const r = await fetch('/api/status.json', { cache: 'no-store' });
        if (!r.ok) return;
        const s = await r.json();
        if (s.display_index != null) document.title = 'Screentra Cast Player display ' + s.display_index;
        $('serverUrl').textContent = s.server_url || '—';
        $('hostname').textContent  = s.hostname || '—';
        $('clientId').textContent  = s.client_id || '—';
        $('pairingCode').textContent = s.pairing_code || '—';
        const adminUrl = s.admin_url || (s.lan_ip ? ('http://' + s.lan_ip + ':3910/') : 'http://11.1.1.54:3910/');
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
        const data = await r.json();
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
          const txt = await r.text();
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
        if (r.ok) { const c = await r.json(); __cursorHideSecs = Number(c.hide_secs) || 3.5; }
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
import socket
import ssl
import time
import hashlib
import hmac
import secrets
import base64
import threading
import tarfile
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

MAX_REQUEST_BODY_BYTES = 1 << 20
MAX_JSON_FILE_BYTES = 1 << 20


def client_pairing_code(client_id):
    """Return the short physical-confirmation code for a client ID."""
    digest = hashlib.sha256(b'screentra-cast-pairing-v1\0' + str(client_id).encode('utf-8')).digest()
    return f'{int.from_bytes(digest[:4], "big") % 1_000_000:06d}'


def _atomic_write_text(path: Path, text: str, mode: int = 0o600) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(f'.{path.name}.{os.getpid()}.tmp')
    temporary.write_text(text, encoding='utf-8')
    os.chmod(temporary, mode)
    os.replace(temporary, path)


def _read_json_file(path: Path, default):
    try:
        if not path.is_file() or path.stat().st_size > MAX_JSON_FILE_BYTES:
            return default
        return json.loads(path.read_text(encoding='utf-8'))
    except Exception:
        return default


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
        out = _sp.check_output(
            ['udevadm', 'info', '--query=property', '--name=' + dev_path],
            stderr=_sp.DEVNULL, timeout=2, text=True,
        )
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
    cur = {}
    for line in p.read_text().splitlines():
        if not line.strip():
            if cur:
                devs.append(cur); cur = {}
            continue
        if line.startswith('N:'):
            cur['name'] = line.split('N:', 1)[1].strip().lstrip('Name=').strip('"')
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
    out.sort(key=lambda x: (not x['recommended'], x['name']))
    return out

def _lan_ip_for_admin() -> str | None:
    try:
        # Prefer a UDP route lookup; it does not send packets but asks the kernel
        # which local source IP would reach the saved server/LAN.
        with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as sock:
            sock.settimeout(0.5)
            sock.connect(('11.1.1.53', 3000))
            ip = sock.getsockname()[0]
            if ip and not ip.startswith('127.'):
                return ip
    except Exception:
        pass
    try:
        out = _sp.check_output(['hostname', '-I'], text=True, timeout=2).split()
        for ip in out:
            if ip and not ip.startswith('127.') and ':' not in ip:
                return ip
    except Exception:
        pass
    return None

def _nl_status():
    cid_path = Path.home() / '.local/share/screentra-client/client-id'
    srv_path = Path.home() / '.local/share/screentra-client/server'
    client_id = cid_path.read_text().strip() if cid_path.exists() else None
    server_url = srv_path.read_text().strip() if srv_path.exists() else 'unknown'
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
    return {
        'screentra_client_client': True,
        'client_installed': True,
        'client_id': client_id,
        'identity_source': 'persistent_client_id',
        'server_url': server_url,
        'hostname': socket.gethostname() if 'socket' in dir() else os.uname().nodename,
        'lan_ip': _lan_ip_for_admin(),
        'admin_port': 3910,
        'admin_url': ('http://' + _lan_ip_for_admin() + ':3910/') if _lan_ip_for_admin() else None,
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

def _safe_run(args, timeout=5):
    try:
        cp = _sp.run(args, text=True, capture_output=True, timeout=timeout)
        return {'ok': cp.returncode == 0, 'code': cp.returncode, 'stdout': cp.stdout[-12000:], 'stderr': cp.stderr[-4000:]}
    except Exception as e:
        return {'ok': False, 'code': None, 'stdout': '', 'stderr': str(e)}

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
        cfg['error'] = str(e)
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
    path.parent.mkdir(parents=True, exist_ok=True)
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
    pending.parent.mkdir(parents=True, exist_ok=True)
    _atomic_write_text(pending, json.dumps({'action':'set_display_count','payload':{'max_displays': override},'requested_at':_iso_now(),'requested_by':'local-admin'}, indent=2))
    _safe_run(['systemctl', '--user', 'restart', 'screentra-client-heartbeat.service'], timeout=6)
    actor = _safe_run(['systemctl', '--user', 'start', 'ripley-player-actor.service'], timeout=6)
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

def _write_touch_binding(payload: dict) -> dict:
    cfg = _load_client_config()
    dev_path = payload.get('device_path') or payload.get('path')
    dev_name = _resolve_touch_name(dev_path, payload.get('device_name'))
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
    pending.parent.mkdir(parents=True, exist_ok=True)
    _atomic_write_text(pending, json.dumps({'action':'set_touch_binding','payload': binding,'requested_at':_iso_now(),'requested_by':'local-admin'}, indent=2))
    _safe_run(['systemctl', '--user', 'restart', 'screentra-client-heartbeat.service'], timeout=6)
    actor = _safe_run(['systemctl', '--user', 'start', 'ripley-player-actor.service'], timeout=6)
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
            state = status.read_text().strip()
            if state != 'connected':
                continue
            name = d.name.split('-', 1)[1]
            mode = None
            modes = d / 'modes'
            if modes.exists():
                first = (modes.read_text().splitlines() or [''])[0]
                if 'x' in first:
                    try:
                        w, h = first.split('x', 1)[0], first.split('x', 1)[1].split()[0]
                        mode = {'width': int(w), 'height': int(h), 'source': 'sysfs'}
                    except Exception:
                        mode = {'label': first, 'source': 'sysfs'}
            out.append({'name': name, 'description': name, 'enabled': None, 'connected': True, 'position': {}, 'current_mode': mode, 'source': 'sysfs'})
    except Exception:
        return []
    return out

def _wlr_outputs() -> list[dict]:
    env = {**os.environ, 'XDG_RUNTIME_DIR': os.environ.get('XDG_RUNTIME_DIR', f'/run/user/{os.getuid()}'), 'WAYLAND_DISPLAY': os.environ.get('WAYLAND_DISPLAY', 'wayland-0')}
    try:
        out = _sp.check_output(['wlr-randr', '--json'], text=True, stderr=_sp.DEVNULL, timeout=3, env=env)
        data = json.loads(out)
        return [{'name': o.get('name'), 'description': o.get('description'), 'enabled': bool(o.get('enabled')), 'position': o.get('position') or {}, 'current_mode': next((m for m in o.get('modes', []) if m.get('current')), None)} for o in data]
    except Exception:
        return _sysfs_outputs()

def _read_tail(path: Path, max_bytes: int = 40000) -> str:
    try:
        if not path.exists() or not path.is_file():
            return ''
        with path.open('rb') as f:
            f.seek(0, os.SEEK_END); size = f.tell(); f.seek(max(0, size - max_bytes)); data = f.read()
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
        'actor': lambda: _journal_tail('ripley-player-actor.service'),
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
        h = hashlib.sha256()
        with path.open('rb') as f:
            for chunk in iter(lambda: f.read(65536), b''): h.update(chunk)
        return h.hexdigest()
    except Exception:
        return None

def _cert_info() -> dict:
    srv_path = Path.home() / '.local/share/screentra-client/server'
    server_url = srv_path.read_text().strip().rstrip('/') if srv_path.exists() else ''
    ca = _ca_file(); ca_exists = ca.exists() and ca.stat().st_size > 0
    verify = {'ok': False, 'error': 'server or CA missing'}
    parsed = urllib.parse.urlparse(server_url) if server_url else None
    if parsed and parsed.hostname and ca_exists:
        port = parsed.port or (443 if parsed.scheme == 'https' else 80)
        try:
            if parsed.scheme == 'https':
                ctx = ssl.create_default_context(cafile=str(ca))
                with socket.create_connection((parsed.hostname, port), timeout=5) as sock:
                    with ctx.wrap_socket(sock, server_hostname=parsed.hostname) as ssock:
                        cert = ssock.getpeercert()
                verify = {'ok': True, 'host': parsed.hostname, 'port': port, 'subject': cert.get('subject'), 'issuer': cert.get('issuer'), 'not_after': cert.get('notAfter'), 'san': cert.get('subjectAltName')}
            else:
                verify = {'ok': True, 'warning': 'server URL is not HTTPS'}
        except Exception as e:
            verify = {'ok': False, 'error': str(e), 'host': parsed.hostname, 'port': port}
    return {'server_url': server_url, 'ca_path': str(ca), 'ca_exists': ca_exists, 'ca_size': ca.stat().st_size if ca.exists() else 0, 'ca_sha256': _sha256_file(ca), 'verify': verify, 'published_ca_url': (server_url + '/install/screentra-cast-signage-ca.crt') if server_url else ''}

def _refresh_ca_from_server() -> dict:
    srv_path = Path.home() / '.local/share/screentra-client/server'
    if not srv_path.exists(): return {'ok': False, 'error': 'server file missing'}
    server_url = srv_path.read_text().strip().rstrip('/')
    if not server_url: return {'ok': False, 'error': 'server URL empty'}
    url = server_url + '/install/screentra-cast-signage-ca.crt'; out = _ca_file()
    try:
        ca_path = _ca_file()
        if not ca_path.exists():
            return {'ok': False, 'error': 'existing CA certificate is required to refresh trust', 'url': url}
        ctx = _ssl.create_default_context(cafile=str(ca_path))
        data = _ur.urlopen(url, context=ctx, timeout=10).read()
        if len(data) < 200 or b'BEGIN CERTIFICATE' not in data:
            return {'ok': False, 'error': f'downloaded CA did not look like a certificate ({len(data)} bytes)', 'url': url}
        out.parent.mkdir(parents=True, exist_ok=True)
        tmp = out.with_suffix(out.suffix + '.tmp'); tmp.write_bytes(data); os.chmod(tmp, 0o644); tmp.replace(out)
        verify = _cert_info().get('verify')
        restart = _safe_run(['systemctl', '--user', 'restart', 'screentra-client-heartbeat.service', 'ripley-player-actor.service'], timeout=8)
        return {'ok': True, 'url': url, 'ca_path': str(out), 'bytes': len(data), 'sha256': _sha256_file(out), 'verify': verify, 'restart': restart}
    except Exception as e:
        return {'ok': False, 'error': str(e), 'url': url}



def _read_proc_stat_cpu() -> tuple[int, int] | None:
    try:
        parts = Path('/proc/stat').read_text().splitlines()[0].split()[1:]
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
            typ = z.parent.joinpath('type').read_text().strip().lower() if z.parent.joinpath('type').exists() else ''
            score = 0 if any(k in typ for k in ('cpu', 'x86_pkg_temp', 'k10temp', 'tctl', 'soc')) else 1
            candidates.append((score, z))
    except Exception:
        pass
    try:
        for z in Path('/sys/class/hwmon').glob('hwmon*/temp*_input'):
            label = ''
            label_path = z.with_name(z.name.replace('_input', '_label'))
            if label_path.exists():
                label = label_path.read_text().strip().lower()
            score = 0 if any(k in label for k in ('cpu', 'tctl', 'tdie', 'package', 'soc')) else 2
            candidates.append((score, z))
    except Exception:
        pass
    for _, path in sorted(candidates, key=lambda x: x[0]):
        try:
            raw = int(path.read_text().strip())
            c = raw / 1000.0 if raw > 1000 else float(raw)
            if -20.0 < c < 130.0:
                return round(c, 1)
        except Exception:
            continue
    return None

def _ram_usage() -> dict:
    info = {}
    try:
        for line in Path('/proc/meminfo').read_text().splitlines():
            k, v = line.split(':', 1)
            info[k] = int(v.strip().split()[0]) * 1024
        total = info.get('MemTotal') or 0
        avail = info.get('MemAvailable') or 0
        used = max(0, total - avail)
        pct = round((used / total) * 100.0, 1) if total else None
        return {'total_bytes': total, 'used_bytes': used, 'available_bytes': avail, 'percent': pct}
    except Exception as e:
        return {'error': str(e)}

def _gpu_usage() -> dict:
    helper = Path.home() / '.local/share/screentra-client/ripley-gpu-health.py'
    if helper.exists():
        try:
            r = _sp.run([sys.executable, str(helper)], stdout=_sp.PIPE, stderr=_sp.PIPE, text=True, timeout=4, check=False)
            if r.stdout.strip():
                data = json.loads(r.stdout.strip().splitlines()[-1])
                if r.stderr.strip():
                    data.setdefault('stderr', r.stderr.strip()[-500:])
                return data
            if r.stderr.strip():
                return {'percent': None, 'device': None, 'source': str(helper), 'error': r.stderr.strip()[-500:]}
        except Exception as e:
            return {'percent': None, 'device': None, 'source': str(helper), 'error': str(e)}
    # Fallback for partial/upgrading installs: generic DRM busy percent.
    out = {'percent': None, 'device': None, 'source': None, 'available': False, 'health': 'unknown', 'health_notes': ['GPU helper not installed yet']}
    try:
        for busy in sorted(Path('/sys/class/drm').glob('card*/device/gpu_busy_percent')):
            raw = busy.read_text().strip()
            out.update({'percent': float(raw), 'device': busy.parent.parent.name, 'source': str(busy), 'available': True, 'method': 'drm-gpu_busy_percent', 'health': 'ok'})
            return out
    except Exception as e:
        out['error'] = str(e)
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
    p.mkdir(parents=True, exist_ok=True)
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
        return {'ok': False, 'error': str(e)}

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
        if svc.get('name') == 'ripley-player-actor.service' and svc.get('active') == 'inactive' and svc.get('enabled') == 'enabled':
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
    p.mkdir(parents=True, exist_ok=True)
    return p

def _auth_secret() -> bytes:
    p = _auth_dir() / 'session-secret'
    if not p.exists():
        p.write_text(secrets.token_urlsafe(32))
        try: os.chmod(p, 0o600)
        except Exception: pass
    return p.read_text().strip().encode()

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
    return '<!doctype html><html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>Screentra Cast Admin Sign In</title><style>body{margin:0;background:#07111f;color:#e5eefc;font-family:system-ui,-apple-system,Segoe UI,sans-serif;display:grid;place-items:center;min-height:100vh}.card{width:min(430px,92vw);background:#0f172a;border:1px solid #243247;border-radius:18px;padding:24px;box-shadow:0 20px 70px rgba(0,0,0,.4)}h1{margin:0 0 8px}.muted{color:#91a4bf}.warn{border:1px solid rgba(245,158,11,.45);background:rgba(245,158,11,.08);border-radius:12px;padding:10px;margin:14px 0;color:#facc15}label{display:block;margin-top:12px;color:#91a4bf;font-size:12px;text-transform:uppercase;letter-spacing:.08em}input{width:100%;margin-top:6px;padding:12px;border-radius:10px;border:1px solid #243247;background:#07111f;color:#e5eefc;font-size:16px}button{width:100%;margin-top:16px;padding:12px;border:0;border-radius:999px;background:#38bdf8;color:#03131f;font-weight:900;font-size:15px}.err{color:#fca5a5;min-height:20px;margin-top:10px}</style></head><body><form class="card" id="f"><h1>Screentra Cast Admin</h1><div class="muted">Sign in to manage this local player.</div><div class="warn">'+warn+'</div><label>Username</label><input id="u" autocomplete="username" value="admin"><label>Password</label><input id="p" type="password" autocomplete="current-password" autofocus><button>Sign in</button><div class="err" id="e"></div></form><script>document.getElementById("f").addEventListener("submit",async ev=>{ev.preventDefault();e.textContent="Signing in…";try{const r=await fetch("/api/admin/auth/login",{method:"POST",headers:{"Content-Type":"application/json"},body:JSON.stringify({username:u.value,password:p.value})});const j=await r.json();if(r.ok&&j.ok){location.href="/"}else{e.textContent=j.error||"Login failed"}}catch(x){e.textContent=String(x)}});</script></body></html>'

def _admin_auth_payload(handler) -> dict:
    user = _session_user(handler.headers.get('Cookie', ''))
    return {'authenticated': bool(user), 'user': user, **_auth_source()}

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
async function api(path,opts){const r=await fetch(path,{cache:'no-store',...(opts||{})}); const t=await r.text(); try{return JSON.parse(t)}catch{return {raw:t,status:r.status}}}
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
*{box-sizing:border-box} html,body,#player{margin:0;width:100vw;height:100vh;background:#000;overflow:hidden;color:#fff;font-family:system-ui,-apple-system,Segoe UI,sans-serif;cursor:none}.widget{position:absolute;overflow:hidden;cursor:none} iframe{background:#000;cursor:none}.fallback{display:grid;place-items:center;height:100vh;text-align:center;color:#94a3b8}.fallback strong{display:block;color:#e2e8f0;font-size:2rem;margin-bottom:.5rem}.clock,.date{display:flex;align-items:center;justify-content:center;width:100%;height:100%}iframe{background:#000;cursor:none}.was-link{display:inline-flex;align-items:center;gap:.45em;border:1px dashed rgba(125,211,252,.65);background:rgba(14,116,144,.16);color:#bae6fd;border-radius:999px;padding:.14em .55em;text-decoration:none}.was-link:after{content:'was link';font-size:.68em;letter-spacing:.08em;text-transform:uppercase;color:#7dd3fc}
</style>
</head>
<body><div id="player"><div class="fallback"><div><strong>Waiting for assigned layout</strong><span>Heartbeat will cache a layout when assigned.</span></div></div></div>
<script>
let currentHash='';
function esc(s){return String(s??'').replace(/[&<>"']/g,c=>({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));}
function normWidgetType(w){return (w.widget_type || w.type || '').toLowerCase().replace(/_/g,'-');}
function props(w){return w.props || w.properties || {};}
function isYouTubeUrl(u){return /(?:youtube\.com|youtu\.be)/i.test(String(u||''));}
function youtubeId(u){const s=String(u||'');let m=s.match(/[?&]v=([^&]+)/); if(m) return m[1]; m=s.match(/youtu\.be\/([^?&/]+)/); if(m) return m[1]; m=s.match(/youtube\.com\/(?:embed|shorts)\/([^?&/]+)/); return m?m[1]:'';}
function youtubeEmbedUrl(u,p){const id=youtubeId(u); const params=new URLSearchParams({autoplay:'1',mute:(p.mute||p.muted)?'1':'0',loop:'1',playsinline:'1',rel:'0'}); if(id) params.set('playlist',id); return id ? ('https://www.youtube.com/embed/'+encodeURIComponent(id)+'?'+params.toString()) : String(u||'');}
function styleFor(w){const x=(Number(w.x)||0)*100,y=(Number(w.y)||0)*100,ww=(Number(w.w??w.width)||0.1)*100,hh=(Number(w.h??w.height)||0.1)*100,z=Number(w.z)||1;return `left:${x}%;top:${y}%;width:${ww}%;height:${hh}%;z-index:${z};`;}
function wmoIcon(code){const m={'0':'☀️','1':'🌤️','2':'⛅','3':'☁️','45':'🌫️','48':'🌫️','51':'🌦️','53':'🌦️','55':'🌦️','61':'🌧️','63':'🌧️','65':'🌧️','71':'❄️','73':'❄️','75':'❄️','80':'🌦️','81':'🌧️','82':'🌧️','95':'⛈️','96':'⛈️','99':'⛈️'};return m[String(code)]||'🌤️';}
function wmoIconHtml(code,p){const key=String(code),custom=p&&p.custom_icons&&p.custom_icons[key];if(custom)return `<img src="${esc(custom)}" style="width:100%;height:100%;object-fit:contain;">`;const n=Number(code),sun=n<=1,cloud=n>=2,rain=(n>=51&&n<=67)||(n>=80&&n<=82),snow=n>=71&&n<=77,storm=n>=95;let body='';if(sun)body+='<circle cx="28" cy="28" r="13" fill="#facc15"/><g stroke="#facc15" stroke-width="4"> <path d="M28 5v9M28 42v9M5 28h9M42 28h9M12 12l7 7M37 37l7 7M44 12l-7 7M19 37l-7 7"/></g>';if(cloud)body+='<path d="M20 47h27a12 12 0 0 0 0-24 18 18 0 0 0-34 5 10 10 0 0 0 7 19z" fill="#cbd5e1" stroke="#64748b" stroke-width="2"/>';if(rain)body+='<g stroke="#38bdf8" stroke-width="4" stroke-linecap="round"><path d="M22 43l-4 9M34 43l-4 9M46 43l-4 9"/></g>';if(snow)body+='<g stroke="#e0f2fe" stroke-width="3" stroke-linecap="round"><path d="M20 42v14M13 49h14M15 44l10 10M25 44L15 54M37 42v14M30 49h14M32 44l10 10M42 44L32 54"/></g>';if(storm)body+='<path d="M30 32h12L29 57l3-16H21l9-19z" fill="#facc15"/>';if(!body)body='<circle cx="32" cy="32" r="20" fill="#facc15"/>';return `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64" style="width:100%;height:100%;padding:8px;">${body}</svg>`;}
function weatherFor(p,i){const d=p._weather_data||{},q=d.daily||{},codes=q.weather_code||q.weathercode||[],code=codes[i]??d.current_weather?.weather_code??d.current_weather?.weathercode??2,winds=q.wind_speed||q.windspeed||[],wind=winds[i]??d.current_weather?.windspeed??0;return {code,icon:wmoIcon(code),high:q.temperature_2m_max?.[i],low:q.temperature_2m_min?.[i],precip:q.precipitation_probability_max?.[i]??0,wind};}
function repeatCount(p,e){const r=e.repeat||{};return r.group?Math.max(1,Math.min(14,Number(p.shown_days)||Number(r.count)||4)):Math.max(1,Number(r.count)||1);}
function groupMetrics(es){const g={};es.forEach(e=>{const id=e.repeat&&e.repeat.group;if(!id)return;const r=e.repeat,x=Number(e.x)||0,y=Number(e.y)||0,w=Number(e.width)||120,h=Number(e.height)||80,m=g[id]||(g[id]={minX:x,minY:y,maxX:x+w,maxY:y+h,gapX:Number(r.gap_x)||0,gapY:Number(r.gap_y)||0,mode:r.mode||'rows'});m.minX=Math.min(m.minX,x);m.minY=Math.min(m.minY,y);m.maxX=Math.max(m.maxX,x+w);m.maxY=Math.max(m.maxY,y+h);});return g;}
function templateEl(e,p,i,g,dw,dh){const q=Object.assign({},e.props||{}),r=e.repeat||{},m=r.group&&g[r.group];if(i){const n=Number(q.day_offset??q.weather_day_offset)||0;q.day_offset=n+i;q.weather_day_offset=n+i;}let x=Number(e.x)||0,y=Number(e.y)||0,w=Number(e.width)||120,h=Number(e.height)||80;if(m&&i){if((m.mode||'rows')==='columns')x+=i*(m.maxX-m.minX+m.gapX);else y+=i*(m.maxY-m.minY+m.gapY);}const a=q.align||'center',j=a==='left'?'flex-start':a==='right'?'flex-end':'center';let st=`position:absolute;box-sizing:border-box;left:${x/dw*100}%;top:${y/dh*100}%;width:${w/dw*100}%;height:${h/dh*100}%;display:flex;align-items:center;justify-content:${j};text-align:${a};overflow:hidden;z-index:${Number(e.z)||0};color:${q.color||'#fff'};font-size:${Number(q.font_size)||24}px;`;let inner='';if(e.type==='rectangle')st+=`background:${q.color||'#1e293b'};opacity:${Number(q.opacity??100)/100};border-radius:${Number(q.radius)||0}px;`;else if(e.type==='text')inner=esc(q.text||'Text');else if(['variable_icon','variable_text','variable','date'].includes(e.type)){const k=q.variable_key||'',wx=weatherFor(p,Math.max(0,Math.min(6,Number(q.day_offset??q.weather_day_offset)||0)));if(k==='weather_icon'||e.type==='variable_icon'||q.display_mode==='weather_icon')inner=wmoIconHtml(wx.code,p);else if(k==='weather_description')inner=esc('Weather');else if(k==='high_temp')inner=esc('High '+(wx.high??'—'));else if(k==='low_temp')inner=esc('Low '+(wx.low??'—'));else if(k==='wind_speed')inner=esc('Wind '+Math.round(wx.wind)+' mph');else{const d=new Date();d.setDate(d.getDate()+(Number(q.day_offset??q.weather_day_offset)||0));inner=esc(k==='day_name'?d.toLocaleDateString(undefined,{weekday:'long'}):d.toLocaleDateString(undefined,{weekday:'long',month:'short',day:'numeric'}));}}else inner=esc(q.label||e.type||'');return `<div style="${st}">${inner}</div>`;}
function templateHtml(w){const p=props(w),es=Array.isArray(p.template_elements)?p.template_elements:[];if(!es.length)return '';const g=groupMetrics(es);let dw=Number(p.template_width)||760,dh=Number(p.template_height)||427.5;es.forEach(e=>{dw=Math.max(dw,(Number(e.x)||0)+(Number(e.width)||120));dh=Math.max(dh,(Number(e.y)||0)+(Number(e.height)||80));});return `<div style="position:relative;width:100%;height:100%;overflow:hidden;">${es.slice().sort((a,b)=>(Number(a.z)||0)-(Number(b.z)||0)).map(e=>Array.from({length:repeatCount(p,e)},(_,i)=>templateEl(e,p,i,g,dw,dh)).join('')).join('')}</div>`;}
async function loadWeather(p){const lat=Number(p.lat||0),lon=Number(p.lon||0);if(!lat||!lon)return;try{const r=await fetch(`/api/weather/forecast?lat=${encodeURIComponent(lat)}&lon=${encodeURIComponent(lon)}`,{cache:'no-store'});if(r.ok)p._weather_data=await r.json();}catch(_e){}}
function countdownFontSize(p){return Math.max(8,Math.min(160,Number(p.font_size||p.fontSize||48)||48))*2;}
function countdownText(node,now){const get=(name)=>node.getAttribute?node.getAttribute(name):(node.dataset||{})[name.slice(5).split('-').map((part,i)=>i?part[0].toUpperCase()+part.slice(1):part).join('')]||'',mode=get('data-countdown-mode')||'date',unit=get('data-countdown-duration-unit')||'minutes',value=Math.max(1,Number(get('data-countdown-duration-value'))||1),started=Number(get('data-countdown-started-at'))||now,target=mode==='duration'?started+value*({seconds:1000,minutes:60000,hours:3600000,days:86400000}[unit]||60000):new Date(get('data-countdown-target-date')||'').getTime(),diff=Math.max(0,target-now),expiry=get('data-countdown-expiry-text')||'Time\u2019s up!';if(!Number.isFinite(target)||target<=0)return get('data-countdown-invalid-text')||'Choose a date and time';if(diff<=0)return expiry;const d=Math.floor(diff/86400000),h=Math.floor(diff%86400000/3600000),m=Math.floor(diff%3600000/60000),s=Math.floor(diff%60000/1000);return d?d+'d '+h+'h':h?h+'h '+m+'m':m?m+'m '+s+'s':s+'s';}
function tickCountdowns(){const now=Date.now();document.querySelectorAll('[data-countdown-widget]').forEach(node=>{const value=node.querySelector('.countdown-value');if(value)value.textContent=countdownText(node,now);});}
function initCachedVideoWidgets(widgets){const nodes=document.querySelectorAll('[data-video-widget-id]');nodes.forEach(node=>{const id=node.getAttribute('data-video-widget-id'),widget=(widgets||[]).find(item=>String(item.id)===String(id)),p=widget&&props(widget),list=p&&Array.isArray(p.playlist)?p.playlist:[];if(!list.length)return;let index=0;const advance=()=>{index=(index+1)%list.length;node.src=String(list[index].url||'');if(typeof node.load==='function')node.load();const result=typeof node.play==='function'?node.play():null;if(result&&typeof result.catch==='function')result.catch(()=>{});};node.src=String(list[0].url||'');node.loop=list.length===1&&p.loop!==false;node.onended=list.length>1?advance:null;node.onerror=list.length>1?advance:null;});}
async function renderWidget(w){const p=props(w),t=normWidgetType(w);if(t==='weather')await loadWeather(p);let inner='';
 if(t==='weather'&&Array.isArray(p.template_elements)&&p.template_elements.length) inner=templateHtml(w);
 else if(t==='text') inner=`<div style="width:100%;height:100%;display:flex;align-items:${p.valign||'center'};justify-content:${p.align==='left'?'flex-start':p.align==='right'?'flex-end':'center'};padding:1rem;color:${p.color||'#fff'};font-size:${Number(p.font_size||p.fontSize||24)*2}px;text-align:${p.align||'center'};">${esc(p.text||'Text')}</div>`;
 else if(t==='countdown') { const target=p.countdown_mode==='duration' ? Date.now() + Math.max(1,Number(p.duration_value||1))*({seconds:1000,minutes:60000,hours:3600000,days:86400000}[p.duration_unit||'minutes']||60000) : new Date(p.targetDate||'').getTime(); const diff=Math.max(0,target-Date.now()); const d=Math.floor(diff/86400000),h=Math.floor(diff%86400000/3600000),m=Math.floor(diff%3600000/60000),s=Math.floor(diff%60000/1000); const text=Number.isFinite(target)&&target>0?(d?d+'d '+h+'h':h?h+'h '+m+'m':m?m+'m '+s+'s':s+'s'):(p.expiry_text||'Choose a date and time'); inner=`<div style="width:100%;height:100%;display:flex;align-items:center;justify-content:center;padding:1rem;color:${p.color||'#fff'};font-size:${countdownFontSize(p)}px;font-weight:800;line-height:1;text-align:center;overflow:hidden;white-space:nowrap;">${esc(text)}</div>`; }
 else if(t==='video') { const list=Array.isArray(p.playlist)&&p.playlist.length?p.playlist:(p.url?[{url:p.url}]:[]),u=String((list[0]||{}).url||''); const fit=['cover','contain','fill','none','scale-down'].includes(p.fit)?p.fit:'cover'; inner=`<video data-video-widget-id="${esc(w.id||'')}" src="${esc(u)}" autoplay playsinline preload="auto" ${p.loop !== false && list.length === 1 ? 'loop' : ''} ${p.muted||p.mute?'muted':''} style="width:100%;height:100%;object-fit:${fit};background:#000;"></video>`;}
 else if(t==='image') inner=`<img src="${esc(p.url||p.src||'')}" style="width:100%;height:100%;object-fit:${p.fit||'cover'};">`;
 else if(t==='video') { const list=Array.isArray(p.playlist)&&p.playlist.length?p.playlist:(p.url?[{url:p.url}]:[]), u=String((list[0]||{}).url||''); if(isYouTubeUrl(u)){ inner=`<iframe src="${esc(youtubeEmbedUrl(u,Object.assign({},p,{muted:true})))}" allow="autoplay; encrypted-media; picture-in-picture" allowfullscreen style="width:100%;height:100%;border:0;background:#000;"></iframe>`; } else { const fit=['cover','contain','fill','none','scale-down'].includes(p.fit)?p.fit:'cover'; inner=`<video src="${esc(u)}" autoplay playsinline preload="auto" ${p.loop !== false && list.length === 1 ? 'loop' : ''} ${p.muted||p.mute?'muted':''} style="width:100%;height:100%;object-fit:${fit};background:#000;"></video>`; } }
 else if(t==='clock') inner=`<div class="clock" data-clock style="color:${p.color||'#fff'};font-size:${Number(p.font_size||p.fontSize||48)*2}px"></div>`;
 else if(t==='datetime'||t==='date') inner=`<div class="date" data-date style="color:${p.color||'#fff'};font-size:${Number(p.font_size||p.fontSize||36)*2}px"></div>`;
 else if(t==='rectangle') inner=`<div style="width:100%;height:100%;background:${p.color||'#e94560'};border-radius:${p.radius||0}px;"></div>`;
 else if(t==='webpage') inner=`<iframe src="${esc(p.url||'about:blank')}" style="width:100%;height:100%;border:0;"></iframe>`;
 else if(t==='weather') inner=`<div style="width:100%;height:100%;display:grid;place-items:center;color:${p.color||'#fff'};font-size:4rem;">${esc(p.location||'Weather')}</div>`;
 else if(t==='calendar') inner=`<div style="width:100%;height:100%;display:grid;place-items:center;color:${p.color||'#fff'};font-size:4rem;">${esc(p.label||'Calendar')}</div>`;
 else inner=`<div style="width:100%;height:100%;display:grid;place-items:center;border:1px dashed #334155;color:#94a3b8;">${esc(t||'widget')}</div>`;
 if(t==='countdown'){const mode=p.countdown_mode==='duration'?'duration':'date',started=Date.now(),durationValue=Math.max(1,Number(p.duration_value||1)),durationUnit=p.duration_unit||'minutes',target=p.targetDate||'',fake={getAttribute:(name)=>({'data-countdown-mode':mode,'data-countdown-duration-value':durationValue,'data-countdown-duration-unit':durationUnit,'data-countdown-started-at':started,'data-countdown-target-date':target,'data-countdown-expiry-text':p.expiry_text||'Time\u2019s up!','data-countdown-invalid-text':'Choose a date and time'}[name]||'')},text=countdownText(fake,started);inner=`<div data-countdown-widget data-countdown-mode="${esc(mode)}" data-countdown-duration-value="${durationValue}" data-countdown-duration-unit="${esc(durationUnit)}" data-countdown-started-at="${started}" data-countdown-target-date="${esc(target)}" data-countdown-expiry-text="${esc(p.expiry_text||'Time\u2019s up!')}" style="width:100%;height:100%;display:flex;align-items:center;justify-content:center;"><div class="countdown-value">${esc(text)}</div></div>`;}
 return `<div class="widget" style="${styleFor(w)}">${inner}</div>`;}
function tickClocks(){const now=new Date();document.querySelectorAll('[data-clock]').forEach(e=>e.textContent=now.toLocaleTimeString([], {hour:'2-digit', minute:'2-digit'}));document.querySelectorAll('[data-date]').forEach(e=>e.textContent=now.toLocaleDateString());tickCountdowns();}
function applyOrientation(value){const v=String(value||'normal');document.body.dataset.orientation=['normal','flipped','rotated_left','rotated_right'].includes(v)?v:'normal';}
async function loadLayout(){try{const r=await fetch('/assigned-layout.json?ts='+Date.now(),{cache:'no-store'});if(!r.ok)throw new Error('no assigned layout');const txt=await r.text();try{const c=await fetch('/api/client-config.json?ts='+Date.now(),{cache:'no-store'});if(c.ok)applyOrientation((await c.json()).orientation);}catch(_e){}if(txt===currentHash)return;currentHash=txt;const layout=JSON.parse(txt);const ws=(layout.widgets||[]).slice().sort((a,b)=>(a.z||0)-(b.z||0));const rendered=await Promise.all(ws.map(renderWidget));document.getElementById('player').innerHTML=rendered.join('')||'<div class="fallback"><div><strong>Empty Layout</strong></div></div>';tickClocks();initCachedVideoWidgets(ws);}catch(e){if(!currentHash)document.getElementById('player').innerHTML='<div class="fallback"><div><strong>Waiting for cached assigned layout</strong><span>'+esc(e.message)+'</span></div></div>';}}
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
    if (r.ok) { const c = await r.json(); __cursorHideSecs = Number(c.hide_secs) || 3.5; }
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


def _live_player_url() -> str | None:
    """Return the server URL the kiosk should redirect to, or None if offline."""
    server = os.environ.get('RIPLEY_SERVER_URL', '').strip()
    if not server:
        try:
            server_path = Path.home() / '.local/share/screentra-client/server'
            if server_path.exists():
                server = server_path.read_text().strip()
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
            v = cid_path.read_text().strip()
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
                    if status_path.read_text().strip() != 'connected':
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
    except Exception:
        return False


def _live_player_redirect_html(server_url: str, display_index: int, connector: str | None = None, client_id: str | None = None) -> str:
    """Return a redirect to the live server for a physical connector."""
    if not connector or connector == 'virtual-0':
        raise ValueError('physical display connector is required')
    safe_server = server_url.replace('"', '%22')
    target = f'{safe_server.rstrip("/")}/player.html?display={urllib.parse.quote(connector)}'
    if client_id:
        target += f'&client_id={urllib.parse.quote(client_id, safe="")}'
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


class Handler(BaseHTTPRequestHandler):
    cache_dir = Path.home() / '.local/share/screentra-client/cache'

    def do_POST(self):
        path = unquote(self.path.split('?', 1)[0])
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
                payload = json.loads(body) if body and body.strip() else {}
                username = str(payload.get('username') or '')
                password = str(payload.get('password') or '')
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
                return self.send_text(400, json.dumps({'ok': False, 'error': str(e)}), 'application/json')
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
                payload = json.loads(body)
                selected = payload.get('path')
                Handler.cache_dir.mkdir(parents=True, exist_ok=True)
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
                return self.send_text(500, json.dumps({'error': str(e)}), 'application/json')
        if path == '/api/touch-calibration.json':
            try:
                data = json.loads(body) if body and body.strip() else {}
                data.setdefault('received_at', _iso_now())
                # Tag per-display calibration results so the heartbeat
                # can attribute intermittent failures to a specific
                # screen when there are multiple on the same box.
                data.setdefault('display_index', int(os.environ.get('RIPLEY_DISPLAY_INDEX', '0')))
                calib_path = Handler.cache_dir / 'touch-calibration.json'
                calib_path.parent.mkdir(parents=True, exist_ok=True)
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
                return self.send_text(400, json.dumps({'error': str(e)}), 'application/json')
        if path == '/api/admin/ca/refresh':
            return self.send_text(200, json.dumps(_refresh_ca_from_server()), 'application/json')
        if path == '/api/admin/device/reboot':
            try:
                payload = json.loads(body) if body and body.strip() else {}
            except Exception:
                payload = {}
            result = _reboot_device(bool(payload.get('confirm')))
            return self.send_text(200 if result.get('ok') else 400, json.dumps(result), 'application/json')
        if path == '/api/admin/display-count':
            try:
                payload = json.loads(body) if body and body.strip() else {}
                value = payload.get('max_displays')
                result = _write_display_count(None if value in (None, '', 'auto') else int(value))
                return self.send_text(200, json.dumps(result), 'application/json')
            except Exception as e:
                return self.send_text(400, json.dumps({'ok': False, 'error': str(e)}), 'application/json')
        if path == '/api/admin/touch-binding':
            try:
                payload = json.loads(body) if body and body.strip() else {}
                result = _write_touch_binding(payload)
                return self.send_text(200, json.dumps(result), 'application/json')
            except Exception as e:
                return self.send_text(400, json.dumps({'ok': False, 'error': str(e)}), 'application/json')
        if path == '/api/admin/client-backup':
            result = _create_client_software_backup()
            return self.send_text(200 if result.get('ok') else 500, json.dumps(result), 'application/json')
        if path == '/api/admin/services/restart':
            r = _safe_run(['systemctl', '--user', 'restart', 'screentra-client-heartbeat.service', 'ripley-player-actor.service'], timeout=8)
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
                payload = json.loads(body) if body and body.strip() else {}
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
                cfg_path.parent.mkdir(parents=True, exist_ok=True)
                _atomic_write_text(cfg_path, json.dumps(data, indent=2))
                return self.send_text(200, json.dumps({'ok': True}), 'application/json')
            except Exception as e:
                return self.send_text(500, json.dumps({'ok': False, 'error': str(e)}), 'application/json')
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
            if not layout_target.exists():
                try:
                    with open(os.path.join(os.path.dirname(os.path.abspath(__file__)), 'no-layout.html'), 'r') as _f:
                        html = _f.read().replace('<title>Screentra Cast Player display</title>', f'<title>Screentra Cast Player display {display_index}</title>')
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
            if live_url and _server_reachable(live_url, LIVE_PROBE_TIMEOUT_SECS):
                connector = _connector_for_display_index(display_index)
                if connector:
                    return self.send_text(200, _live_player_redirect_html(live_url, display_index, connector, _client_id_from_env() or _client_id_from_cache()), 'text/html; charset=utf-8')

            # No live server reachable (offline mode). Render the offline
            # cached layout from the renderer script's PLAYER_HTML constant.
            display_index = int(os.environ.get('RIPLEY_DISPLAY_INDEX', '0'))
            html = PLAYER_HTML.replace('<title>Screentra Cast Cached Player</title>', f'<title>Screentra Cast Display {display_index}</title>')
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
            target = (_client_backup_root() / safe_name).resolve()
            try:
                target.relative_to(_client_backup_root().resolve())
            except Exception:
                return self.send_text(403, 'forbidden')
            if not target.exists() or not target.is_file():
                return self.send_text(404, 'backup not found')
            data = target.read_bytes()
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
            with open(os.path.join(os.path.dirname(os.path.abspath(__file__)), 'no-layout.html'), 'r') as _f:
                html = _f.read().replace('<title>Screentra Cast Player display</title>', f'<title>Screentra Cast Player display {display_index}</title>')
                return self.send_text(200, html, 'text/html; charset=utf-8')
        if path == '/touch-calibration.html':
            # Serve the touchscreen calibration test page. Lives next to
            # the renderer script and reads RIPLEY_DISPLAY_INDEX for the
            # results so per-display calibration is recorded separately.
            try:
                with open(os.path.join(os.path.dirname(os.path.abspath(__file__)), 'touch-calibration.html'), 'r') as _f:
                    return self.send_text(200, _f.read(), 'text/html; charset=utf-8')
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
            return self.send_text(200, json.dumps(_read_json_file(target, {})), 'application/json')
        if path == '/client-config.json':
            target = self.cache_dir / path.lstrip('/')
            if not target.exists():
                return self.send_text(404, '{}', 'application/json')
            return self.send_text(200, json.dumps(_read_json_file(target, {})), 'application/json')
        # Optional cached asset serving under cache/assets
        safe = Path(path.lstrip('/'))
        target = (self.cache_dir / safe).resolve()
        try:
            target.relative_to(self.cache_dir.resolve())
        except Exception:
            return self.send_text(403, 'forbidden')
        if target.exists() and target.is_file():
            data = target.read_bytes()
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
    Handler.cache_dir.mkdir(parents=True, exist_ok=True)
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
import shutil
import subprocess
import sys
from pathlib import Path


def _read(path: Path) -> str | None:
    try:
        return path.read_text(errors='replace').strip()
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


def _run(args: list[str], timeout: float = 2.0) -> tuple[int, str, str]:
    try:
        p = subprocess.run(args, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=timeout, check=False)
        return p.returncode, p.stdout.strip(), p.stderr.strip()
    except Exception as e:
        return 127, '', str(e)


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
                    gpus.append({'vendor': vendor, 'name': line, 'source': 'lspci'})
            if gpus:
                return gpus
    # Sysfs fallback without pciutils.
    for dev in sorted(Path('/sys/class/drm').glob('card*/device')):
        vendor_id = _read(dev / 'vendor') or ''
        device_id = _read(dev / 'device') or ''
        vendor = {'0x10de': 'nvidia', '0x8086': 'intel', '0x1002': 'amd', '0x1022': 'amd'}.get(vendor_id.lower(), 'unknown')
        if vendor != 'unknown' or device_id:
            gpus.append({'vendor': vendor, 'name': f'{vendor} GPU {vendor_id} {device_id}'.strip(), 'source': str(dev)})
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
from pathlib import Path
from typing import List, Dict, Any, Optional


def _read(path: Path) -> str:
    try:
        return path.read_text().strip()
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


def detect_displays(max_displays: int = 8) -> List[Dict[str, Any]]:
    """Return a list of detected DRM connector displays.

    Each entry: { display_index, connector, label, width, height, refresh_rate, enabled, connected }
    Returns an empty list when no physical DRM connector is present. Virtual
    displays are not supported.
    """
    drm_root = Path('/sys/class/drm')
    displays: List[Dict[str, Any]] = []

    if drm_root.exists():
        connectors = []
        for entry in sorted(drm_root.iterdir()):
            name = entry.name
            # cardN-CONNECTOR-N
            if not name.startswith('card') or '-' not in name:
                continue
            status = _read(entry / 'status')
            enabled = _read(entry / 'enabled') == 'enabled'
            # Skip disconnected or disabled unless no other connectors exist
            connectors.append({'name': name, 'entry': entry, 'status': status, 'enabled': enabled})

        # Prefer connected+enabled connectors first
        connectors.sort(key=lambda c: (0 if (c['status'] == 'connected' and c['enabled']) else 1, c['name']))

        for idx, conn in enumerate(connectors):
            if idx >= max_displays:
                break
            entry = conn['entry']
            connector = conn['name'].split('-', 1)[1] if '-' in conn['name'] else conn['name']
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
    displays = _det.detect_displays(max_displays=max_scan)
    return sum(1 for d in displays if d.get("connected", d.get("enabled")))


def detect_full(max_scan: int = 16) -> List[Dict[str, Any]]:
    """Return the raw detection list (same shape as heartbeat 'displays' field)."""
    return _det.detect_displays(max_displays=max_scan)


def load_config(config_path: Path) -> Dict[str, Any]:
    """Load config.json (or return defaults) without writing."""
    if not config_path.exists():
        return dict(CONFIG_DEFAULTS)
    try:
        cfg = json.loads(config_path.read_text())
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
    config_path.parent.mkdir(parents=True, exist_ok=True)
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
import json
import os
import re
import signal
import shutil
import ssl
import subprocess
import sys
import tempfile
import time
import urllib.parse
import urllib.request
from urllib.parse import urlparse
from pathlib import Path

MAX_PENDING_BYTES = 1 << 20
MAX_JSON_FILE_BYTES = 1 << 20


def _read_json_file(path: Path, default):
    try:
        if not path.is_file() or path.stat().st_size > MAX_JSON_FILE_BYTES:
            return default
        return json.loads(path.read_text(encoding='utf-8'))
    except Exception:
        return default


def _server_origin(server: str) -> str:
    parsed = urllib.parse.urlsplit((server or '').strip())
    if parsed.scheme not in ('http', 'https') or not parsed.hostname or parsed.username or parsed.password or parsed.query or parsed.fragment:
        raise ValueError('configured server URL must be an HTTP(S) origin without credentials or query parameters')
    if parsed.scheme == 'http' and parsed.hostname not in {'127.0.0.1', '::1', 'localhost'} and os.environ.get('RIPLEY_ALLOW_INSECURE_HTTP', '').lower() not in ('1', 'true', 'yes'):
        raise ValueError('non-loopback HTTP server URLs require explicit RIPLEY_ALLOW_INSECURE_HTTP=1')
    return urllib.parse.urlunsplit((parsed.scheme, parsed.netloc, parsed.path.rstrip('/'), '', ''))


def _atomic_write_text(path: Path, text: str, mode: int = 0o600) -> None:
    """Replace a cache JSON file atomically and keep it owner-readable."""
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(f'.{path.name}.{os.getpid()}.tmp')
    temporary.write_text(text)
    os.chmod(temporary, mode)
    os.replace(temporary, path)


def _run_list(args: list[str], timeout: int = 20, input_data: str | None = None) -> tuple[int, str, str]:
    try:
        p = subprocess.run(args, capture_output=True, text=True, timeout=timeout, input=input_data)
        return p.returncode, (p.stdout or "").strip(), (p.stderr or "").strip()
    except Exception as e:
        return 1, "", str(e)


def ddcutil_detect(bus: str | None = None) -> tuple[bool, str]:
    """Run ddcutil detect to refresh display cache. Returns (ok, message)."""
    args = ['sudo', '-n', 'ddcutil', 'detect']
    if bus:
        args.extend(['--bus', bus])
    rc, out, err = _run_list(args)
    if rc == 0:
        return True, out.strip()
    return False, err.strip() or out.strip()


def apply_ddc_control(vcp_code: str, value: int, bus: str | None = None, has_layout: bool = False) -> tuple[bool, str]:
    """Set a DDC/CI VCP value via ddcutil. Returns (ok, message).
    If has_layout=True and the display is not initially detected (ddcutil_no_bus),
    runs ddcutil detect once and retries before returning failure."""
    args = ['sudo', '-n', 'ddcutil', 'setvcp']
    if bus:
        args.extend(['--bus', bus])
    args.extend([vcp_code, str(value)])

    def _do_set():
        rc, out, err = _run_list(args)
        if rc == 0:
            return True, f'DDC: {vcp_code}={value}' + (f' ({out.strip()})' if out.strip() else '')
        err_msg = err.strip() or out.strip()
        if 'not found' in err_msg.lower() or 'no such file' in err_msg.lower():
            return False, f'ddcutil_not_installed: {err_msg}'
        if 'unable to open' in err_msg.lower() or 'i2c' in err_msg.lower() or 'no bus' in err_msg.lower():
            return False, 'ddcutil_no_bus', err_msg
        if 'communication' in err_msg.lower() or 'timeout' in err_msg.lower():
            return False, f'ddcutil_no_support: {err_msg}'
        return False, f'DDC failed (rc={rc}): {err_msg}'

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
        return False, f'{msg_or_tag}: {err_msg} — retry after re-detection failed: {msg2}'

    return False, msg_or_tag


def apply_cec_command(cec_cmd: str) -> tuple[bool, str]:
    """Send a CEC command via cec-client. Returns (ok, message)."""
    rc, out, err = _run_list(
        ['sudo', '-n', 'cec-client', '-d', '1', '-s'],
        input_data=f'{cec_cmd}\n',
    )
    if rc == 0:
        return True, f'CEC: {cec_cmd}' + (f' — {out.strip()}' if out.strip() else '')
    return False, f'CEC failed (rc={rc}): {err.strip() or out.strip()}'


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
        devices.append({'address': address, 'name': match.group(2).strip() or address})
    return devices


def bluetooth_scan(payload: dict | None) -> tuple[bool, str, dict]:
    payload = payload or {}
    duration = max(5, min(int(payload.get('duration_seconds') or 10), 30))
    if shutil.which('bluetoothctl') is None:
        return False, 'bluetoothctl is not installed on this client', {'devices': []}
    _run_list(['bluetoothctl', 'power', 'on'], timeout=10)
    rc, out, err = _run_list(['bluetoothctl', '--timeout', str(duration), 'scan', 'on'], timeout=duration + 10)
    _, listed, list_err = _run_list(['bluetoothctl', 'devices'], timeout=10)
    devices = _bluetoothctl_devices(listed or out)
    if not devices and rc != 0:
        return False, f'Bluetooth scan failed: {err or out or list_err or "unknown error"}', {'devices': []}
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
    address = str(payload.get('address') or '').strip().upper()
    name = str(payload.get('name') or '').strip() or address
    if not re.fullmatch(r'[0-9A-F]{2}(?::[0-9A-F]{2}){5}', address):
        return False, 'A valid Bluetooth device address is required', {}
    if shutil.which('bluetoothctl') is None:
        return False, 'bluetoothctl is not installed on this client', {}
    _run_list(['bluetoothctl', 'power', 'on'], timeout=10)
    rc, out, err = _run_list(['bluetoothctl', 'connect', address], timeout=30)
    text = '\n'.join(v for v in (out, err) if v)
    connected = rc == 0 and not re.search(r'failed|not available|error', text, re.IGNORECASE)
    details = {'address': address, 'name': name, 'connected': connected, 'bluetooth_output': text[-1200:]}
    if not connected:
        return False, f'Bluetooth connection failed: {text or f"exit code {rc}"}', details
    time.sleep(1)
    sink_id = _find_bluetooth_sink(address, name)
    if sink_id and shutil.which('wpctl'):
        sink_rc, sink_out, sink_err = _run_list(['wpctl', 'set-default', sink_id], timeout=10)
        details['audio_sink_id'] = sink_id
        details['audio_device_selected'] = sink_rc == 0
        if sink_rc == 0:
            return True, f'Connected to {name}; Bluetooth audio output selected', details
        details['audio_selection_error'] = sink_err or sink_out
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
        name = re.sub(r'^\*\s*', '', match.group(2)).strip()
        name = re.sub(r'\s+\[vol:.*?\]\s*$', '', name).strip()
        if not any(item['id'] == sink_id for item in sinks):
            sinks.append({'id': sink_id, 'name': name, 'default': '*' in line[:match.start(2)]})
    return sinks


def audio_list() -> tuple[bool, str, dict]:
    if shutil.which('wpctl') is None:
        return False, 'wpctl is not installed on this client', {'devices': []}
    rc, out, err = _run_list(['wpctl', 'status'], timeout=10)
    devices = _wpctl_sinks(out)
    if rc != 0:
        return False, f'Audio device scan failed: {err or out or f"exit code {rc}"}', {'devices': devices}
    return True, f'Found {len(devices)} audio output device(s)', {'devices': devices}


def audio_set_default(payload: dict | None) -> tuple[bool, str, dict]:
    payload = payload or {}
    sink_id = str(payload.get('sink_id') or '').strip()
    if not sink_id.isdigit():
        return False, 'A valid audio output device is required', {}
    if shutil.which('wpctl') is None:
        return False, 'wpctl is not installed on this client', {}
    rc, out, err = _run_list(['wpctl', 'set-default', sink_id], timeout=10)
    if rc == 0:
        return True, f'Audio output selected: {sink_id}', {'sink_id': sink_id}
    return False, f'Audio output selection failed: {err or out or f"exit code {rc}"}', {'sink_id': sink_id}


def audio_set_volume(payload: dict | None) -> tuple[bool, str, dict]:
    payload = payload or {}
    sink_id = str(payload.get('sink_id') or '@DEFAULT_AUDIO_SINK@').strip()
    try:
        value = max(0, min(100, int(payload.get('value'))))
    except (TypeError, ValueError):
        return False, 'Volume must be a number from 0 to 100', {}
    if not sink_id.isdigit() and sink_id != '@DEFAULT_AUDIO_SINK@':
        return False, 'A valid audio output device is required', {}
    if shutil.which('wpctl') is None:
        return False, 'wpctl is not installed on this client', {}
    rc, out, err = _run_list(['wpctl', 'set-volume', sink_id, f'{value}%'], timeout=10)
    if rc == 0:
        return True, f'Client audio volume set to {value}%', {'sink_id': sink_id, 'volume': value}
    return False, f'Client audio volume failed: {err or out or f"exit code {rc}"}', {'sink_id': sink_id, 'volume': value}


def restart_client() -> tuple[bool, str, dict]:
    if shutil.which('systemctl') is None:
        return False, 'systemctl is not available on this client', {}
    rc, out, err = _run_list(['sudo', '-n', 'systemctl', 'reboot'], timeout=20)
    if rc == 0:
        return True, 'Client reboot requested', {'reboot_requested': True}
    return False, f'Client reboot failed: {err or out or f"exit code {rc}"}', {'reboot_requested': False}


def has_systemctl_user() -> bool:
    if shutil.which("systemctl") is None:
        return False
    rc, _, _ = _run_list(["systemctl", "--user", "status"])
    return rc == 0


def systemctl_user(action: str, unit: str) -> tuple[bool, str]:
    rc, out, err = _run_list(["systemctl", "--user", action, unit])
    ok = rc == 0
    return ok, (out or err or f"{action} {unit} rc={rc}").strip()


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
    return rc == 0, (out or err or f'restart screentra-cast-kiosk@0.service rc={rc}').strip()


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
    installer = max(int(cfg.get('installer_detected_max_displays') or 1), 1)
    if max_displays is None:
        override = None
        active = min(installer, 4)
    else:
        override = max(1, min(int(max_displays), installer, 4))
        active = override
    cfg['installer_detected_max_displays'] = installer
    cfg['max_displays'] = override
    cfg.setdefault('last_detected_displays', [])
    config_path.parent.mkdir(parents=True, exist_ok=True)
    tmp = config_path.with_suffix('.json.tmp')
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
        out = subprocess.check_output(['wlr-randr', '--json'], text=True, stderr=subprocess.DEVNULL, timeout=3, env=env)
        data = json.loads(out)
        outputs = [o for o in data if o.get('name')]
        outputs.sort(key=lambda o: o.get('name',''))
        x = 0
        for idx, o in enumerate(outputs):
            name = o.get('name')
            cur = next((m for m in o.get('modes', []) if m.get('current')), None)
            pref = next((m for m in o.get('modes', []) if m.get('preferred')), None)
            mode = cur or pref or {}
            width = int(mode.get('width') or 0)
            if idx < active:
                cp = subprocess.run(['wlr-randr', '--output', name, '--on', '--pos', f'{x},0'], text=True, capture_output=True, timeout=5, env=env)
                details['steps'].append({'output': name, 'action': 'on', 'x': x, 'code': cp.returncode, 'stderr': (cp.stderr or '')[-500:]})
                x += width
            elif o.get('enabled'):
                cp = subprocess.run(['wlr-randr', '--output', name, '--off'], text=True, capture_output=True, timeout=5, env=env)
                details['steps'].append({'output': name, 'action': 'off', 'code': cp.returncode, 'stderr': (cp.stderr or '')[-500:]})
    except Exception as e:
        details['steps'].append({'output_policy': 'skipped', 'error': str(e)})
        rc,out,err = _run_list(['sudo', '-n', 'systemctl', 'restart', 'screentra-cast-kiosk@0.service'])
        details['steps'].append({'unit': 'screentra-cast-kiosk@0.service', 'restart_code': rc, 'message': out or err})

    ok = has_systemctl_user()
    return ok, f'active display count set to {active}', details


def _resolve_touch_name(device_path: str | None, device_name: str | None = None) -> str | None:
    if device_name:
        return str(device_name)
    if not device_path:
        return None
    try:
        blocks = Path('/proc/bus/input/devices').read_text().split('\n\n')
        target = Path(device_path).name
        for block in blocks:
            if target not in block:
                continue
            for line in block.splitlines():
                if line.startswith('N:') and 'Name=' in line:
                    return line.split('Name=', 1)[1].strip().strip('"')
    except Exception:
        pass
    return None


def _display_connectors_for_indexes(config: dict, indexes: list[int]) -> list[str]:
    displays = config.get('last_detected_displays') or []
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
    return out


def apply_touch_binding(cache_dir: Path, payload: dict) -> tuple[bool, str, dict]:
    install_dir = cache_dir.parent
    config_path = install_dir / 'config.json'
    try:
        cfg = _read_json_file(config_path, {})
    except Exception:
        cfg = {}
    dev_path = payload.get('device_path') or payload.get('path')
    dev_name = _resolve_touch_name(dev_path, payload.get('device_name'))
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
    indexes = indexes[:2] if allow_joined else indexes[:1]
    binding = {
        'device_path': dev_path,
        'device_name': dev_name,
        'target_display_indexes': indexes,
        'target_connectors': _display_connectors_for_indexes(cfg, indexes),
        'allow_joined_displays': allow_joined,
        'updated_at': datetime.datetime.now(datetime.timezone.utc).replace(tzinfo=None).isoformat() + 'Z',
        'updated_by': payload.get('requested_by') or 'actor',
    }
    cfg['touch_binding'] = binding
    config_path.parent.mkdir(parents=True, exist_ok=True)
    tmp = config_path.with_suffix('.json.tmp')
    _atomic_write_text(config_path, json.dumps(cfg, indent=2))
    if dev_path:
        cache_dir.mkdir(parents=True, exist_ok=True)
        _atomic_write_text(Path(cache_dir, 'touch-device.json'), json.dumps({'path': dev_path, 'name': dev_name}, indent=2))
    details = {'config_path': str(config_path), 'touch_binding': binding, 'steps': []}
    # Restart labwc so rc.xml <touch> mapping is reloaded, then let browser services return.
    rc, out, err = _run_list(['sudo', '-n', 'systemctl', 'restart', 'screentra-cast-kiosk@0.service'])
    details['steps'].append({'unit': 'screentra-cast-kiosk@0.service', 'restart_code': rc, 'message': out or err})
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
                return ssl.create_default_context(cafile=str(cafile))
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
        token = token_path.read_text(encoding='utf-8').strip()
    except Exception:
        return ''
    return token


def _write_preview_state(cache_dir: Path, display_index: int, state: dict) -> None:
    _atomic_write_text(_preview_state_path(cache_dir, display_index), json.dumps(state, indent=2))


def _preview_connector(cache_dir: Path, display_index: int) -> str | None:
    data = _read_json_file(cache_dir / f'display-{display_index}.json', {})
    connector = str(data.get('connector') or '').strip()
    return connector or None


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
    return env


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
                capture = subprocess.run(
                    command, capture_output=True, timeout=20, check=False,
                    env=_preview_wayland_env(),
                )
            except Exception as exc:
                _write_preview_state(cache_dir, display_index, {'status': 'error', 'connector': connector, 'message': f'capture failed: {exc}'})
                time.sleep(interval)
                continue
            if capture.returncode != 0 or not capture.stdout:
                _write_preview_state(cache_dir, display_index, {'status': 'unavailable', 'connector': connector, 'message': 'grim could not capture the display'})
                time.sleep(interval)
                continue
            ok, message = _upload_preview_frame(server, client_id, display_index, capture.stdout, cache_dir)
            if ok:
                _write_preview_state(cache_dir, display_index, {
                    'status': 'streaming', 'connector': connector, 'started_at': started,
                    'last_frame_at': time.time(), 'bytes': len(capture.stdout),
                })
            else:
                _write_preview_state(cache_dir, display_index, {'status': 'error', 'connector': connector, 'message': message})
            time.sleep(interval)
    finally:
        try:
            if pid_path.read_text(encoding='utf-8').strip() == str(os.getpid()):
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


def _preview_process_alive(pid_path: Path) -> bool:
    try:
        pid = int(pid_path.read_text(encoding='utf-8').strip())
        os.kill(pid, 0)
        return True
    except Exception:
        return False


def start_display_preview(ctx: dict, display_index: int, payload: dict) -> tuple[bool, str, dict]:
    cache_dir = Path(ctx['cache_dir'])
    cache_dir.mkdir(parents=True, exist_ok=True)
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
        interval = max(1, min(int(payload.get('interval_seconds') or 5), 30))
        duration = max(10, min(int(payload.get('duration_seconds') or 120), 300))
    except (TypeError, ValueError):
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
        return False, f'display preview could not start: {exc}', {}


def stop_display_preview(ctx: dict, display_index: int) -> tuple[bool, str, dict]:
    cache_dir = Path(ctx['cache_dir'])
    pid_path = _preview_pid_path(cache_dir, display_index)
    try:
        pid = int(pid_path.read_text(encoding='utf-8').strip())
    except Exception:
        pid = None
    if pid is not None:
        try:
            os.kill(pid, signal.SIGTERM)
        except ProcessLookupError:
            pass
        except Exception as exc:
            return False, f'display preview could not stop: {exc}', {}
    try:
        pid_path.unlink()
    except FileNotFoundError:
        pass
    _write_preview_state(cache_dir, display_index, {'status': 'stopped', 'stopped_at': time.time()})
    return True, 'display preview stopped', {'display_index': display_index}


def fetch_config(server: str, client_id: str, cache_dir: Path, display_index: int | None = None) -> bool:
    if display_index is None:
        url = server.rstrip('/') + '/api/clients/' + urllib.request.quote(client_id, safe='') + '/config'
        target = 'assigned-layout.json'
    else:
        url = (server.rstrip() + '/api/clients/' + urllib.request.quote(client_id, safe='') +
               '/displays/' + str(display_index) + '/config')
        target = f'assigned-layout-display-{display_index}.json'
    try:
        with urlopen_ripley(url, timeout=10, cache_dir=cache_dir) as r:
            data = json.loads(r.read().decode())
        # The server display-config payload does not include local-admin theme.
        try:
            previous = _read_json_file(cache_dir / 'client-config.json', {})
            if previous.get('theme'):
                data['theme'] = previous['theme']
        except Exception:
            pass
        cache_dir.mkdir(parents=True, exist_ok=True)
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
        with urlopen_ripley(url, timeout=10, cache_dir=cache_dir) as r:
            displays = json.loads(r.read().decode())
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


def update_client(server: str, client_id: str, cache_dir: Path, payload: dict | None) -> tuple[bool, str, dict]:
    """Request the root-owned verifier; never download or execute as the player."""
    payload = payload or {}
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
    temporary = request_path.with_name('.request.tmp')
    try:
        raw = json.dumps(marker, separators=(',', ':')).encode('utf-8')
        if len(raw) > 4096:
            return False, 'client update refused: request marker is too large', details
        temporary.write_bytes(raw)
        temporary.chmod(0o600)
        os.replace(temporary, request_path)
        return True, 'verified root client updater requested', details
    except Exception as e:
        details['error'] = str(e)
        return False, f'client update request failed: {e}', details
    finally:
        try:
            temporary.unlink()
        except Exception:
            pass


def execute(action: str, payload: dict | None, ctx: dict) -> dict:
    """Run a single command and return a result dict."""
    started = datetime.datetime.now(datetime.timezone.utc).replace(tzinfo=None).isoformat() + 'Z'
    has_user = has_systemctl_user()
    payload = payload or {}
    display_index = payload.get('display_index')
    try:
        display_index = int(display_index) if display_index is not None else None
    except Exception:
        display_index = None

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
        if requested_orientation is not None and requested_orientation not in {'normal', 'flipped', 'rotated_left', 'rotated_right'}:
            return {'action': action, 'display_index': display_index, 'ok': False, 'message': 'invalid orientation', 'started_at': started}
        try:
            before_orientation = _read_json_file(cache_path, {}).get('orientation', 'normal')
        except Exception:
            before_orientation = 'normal'
        if payload.get('all_displays') or display_index is None:
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
                restart_message = out or err
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
            restart_message = out or err
        return {'action': action, 'display_index': display_index, 'ok': ok and (restarted or (after_orientation == before_orientation and requested_orientation is None)), 'orientation_changed': after_orientation != before_orientation or requested_orientation is not None, 'kiosk_restarted': restarted, 'message': 'config re-fetched' + ('; kiosk restarted for orientation change' if restarted else (': ' + restart_message if restart_message else '')), 'started_at': started}

    if action == 'refresh_cache':
        if payload.get('all_displays') or display_index is None:
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
        ok, msg, details = apply_touch_binding(Path(ctx['cache_dir']), payload)
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

    # ── Display control (DDC/CI + CEC) ────────────────────────────────────────
    if action == 'ddc_set':
        # payload: { "vcp": "10", "value": 50 }  brightness
        # or { "vcp": "D6", "value": 1 }          power on
        # optional: { "bus": "1" }                specific display bus
        # optional: { "has_layout": true }        retry detect if display not found
        vcp = payload.get('vcp', '')
        value = payload.get('value', 0)
        bus = payload.get('bus')
        has_layout = payload.get('has_layout', False)
        ok, msg = apply_ddc_control(str(vcp), int(value), bus=bus, has_layout=has_layout)
        return {'action': action, 'display_index': display_index, 'ok': ok, 'message': msg, 'started_at': started}

    if action == 'cec_send':
        # payload: { "cmd": "pow 0" }   power on OSD addr 0
        # or { "cmd": "is 1" }          input HDMI-2
        cmd = payload.get('cmd', '')
        ok, msg = apply_cec_command(cmd)
        return {'action': action, 'display_index': display_index, 'ok': ok, 'message': msg, 'started_at': started}

    if action == 'ir_send':
        # IR control — stub; hardware integration for later
        return {'action': action, 'display_index': display_index, 'ok': False,
                'message': 'IR control not yet implemented. Set method to DDC or CEC.',
                'started_at': started}

    return {'action': action, 'display_index': display_index, 'ok': False, 'message': f'unknown action: {action}', 'started_at': started}


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
    result_path.parent.mkdir(parents=True, exist_ok=True)

    ctx = {
        'server': args.server,
        'client_id': args.client_id or '',
        'cache_dir': args.cache_dir,
    }

    def process_one():
        if not pending_path.exists():
            return False
        try:
            if pending_path.stat().st_size > MAX_PENDING_BYTES:
                raise ValueError(f'pending command exceeds {MAX_PENDING_BYTES} bytes')
            spec = json.loads(pending_path.read_text(encoding='utf-8'))
        except Exception as e:
            result = {'action': 'unknown', 'ok': False, 'message': f'bad pending file: {e}', 'started_at': datetime.datetime.now(datetime.timezone.utc).replace(tzinfo=None).isoformat() + 'Z'}
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
SERVER_URL="${RIPLEY_SERVER_URL:-$(cat "$INSTALL_DIR/server" 2>/dev/null || true)}"

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
  curl -fsS --max-time 4 "$RENDERER_URL/health" || echo "  renderer not reachable"
else
  python3 - <<PY 2>/dev/null || echo "  renderer not reachable"
import urllib.request
print(urllib.request.urlopen("$RENDERER_URL/health", timeout=4).read().decode())
PY
fi
echo

if [ -f "$CONFIG" ]; then
  echo "Assigned layout:"
  python3 - <<PY
import json
d=json.load(open("$CONFIG"))
print("  client_id:           ", d.get("client_id"))
print("  assigned_layout_id:  ", d.get("assigned_layout_id"))
print("  server_time:         ", d.get("server_time"))
layout=d.get("layout") or {}
print("  layout name:         ", layout.get("name"))
print("  layout widgets:      ", len(layout.get("widgets") or []))
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

# Start one fullscreen Chromium window per display. Each window has a unique
# app_id so Sway can place it on the matching physical connector.
while IFS='|' read -r idx connector port; do
    [ -n "$idx" ] || continue
    LIVE_READY=0
    for i in $(seq 1 40); do
        if curl -fsS --max-time 2 "http://127.0.0.1:${port}/" | \
            python3 -c 'import sys; raise SystemExit(0 if "Screentra Cast Display" in sys.stdin.read() else 1)'; then
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

cat > "$ADMIN_SERVICE" <<EOF
[Unit]
Description=Screentra Cast Player Local Admin Dashboard
After=network-online.target screentra-client-heartbeat.service
Wants=network-online.target

[Service]
Type=simple
ExecStart=$RENDERER --host 127.0.0.1 --port 3910 --cache-dir $INSTALL_DIR/cache
Environment=RIPLEY_ADMIN_MODE=1
Environment=RIPLEY_DISPLAY_INDEX=0
Environment=RIPLEY_RENDER_BASE_PORT=3900
Environment=RIPLEY_CURSOR_HIDE_SECS=3.5
Restart=always
RestartSec=5
Environment=PYTHONUNBUFFERED=1

[Install]
WantedBy=default.target
EOF

cat > "$KIOSK_SERVICE" <<EOF
[Unit]
Description=Screentra Cast Fullscreen Kiosk Browser (display %i)
After=screentra-client-renderer@%i.service
Wants=screentra-client-renderer@%i.service

[Service]
Type=simple
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

SYSTEM_ADMIN_UNIT="/etc/systemd/system/screentra-client-admin.service"
run_privileged tee "$SYSTEM_ADMIN_UNIT" >/dev/null <<EOF_SYSTEM_ADMIN
[Unit]
Description=Screentra Cast Player Local Admin Dashboard
After=network-online.target screentra-client-heartbeat.service
Wants=network-online.target

[Service]
Type=simple
User=$BOARD_USER
Group=$BOARD_USER
WorkingDirectory=$INSTALL_DIR
Environment=HOME=$BOARD_HOME
Environment=RIPLEY_CLIENT_DIR=$INSTALL_DIR
Environment=RIPLEY_SERVER_URL=$SERVER_URL
Environment=RIPLEY_CLIENT_ID=$CLIENT_ID
Environment=RIPLEY_RENDER_BASE_PORT=3900
Environment=RIPLEY_ADMIN_MODE=1
Environment=RIPLEY_DISPLAY_INDEX=0
Environment=PYTHONUNBUFFERED=1
ExecStart=$RENDERER --host 127.0.0.1 --port 3910 --cache-dir $INSTALL_DIR/cache
Restart=always
RestartSec=2

[Install]
WantedBy=multi-user.target
EOF_SYSTEM_ADMIN
run_privileged systemctl daemon-reload
run_privileged systemctl enable --now screentra-client-actor.service
run_privileged systemctl enable --now screentra-client-admin.service

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
  systemctl --user enable screentra-client-admin.service || true
  systemctl --user restart screentra-client-admin.service || true
  # Start renderer/browser services for the governed active count. The
  # display-governance config defaults to the detected enabled connectors, but
  # the local/server admin pages can cap it. Display 0 is served by the system
  #  startup hook; display 1+ are user browser services on wayland-0.
  DISPLAY_COUNT="$(python3 - "$INSTALL_DIR/config.json" <<'PY_DETECT'
import json, sys
try:
    cfg=json.loads(open(sys.argv[1]).read())
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
echo "Admin: http://$(hostname -I 2>/dev/null | awk '{print $1}'):3910/"
echo "Kiosk browser service: screentra-kiosk-browser@0.service"
echo "Preferred display service: screentra-cast-kiosk@.service"
echo "Helpers: screentra-client-preflight, screentra-client-status, screentra-client-heartbeat, screentra-client-renderer, screentra-client-admin, screentra-client-actor, screentra-kiosk-launcher"

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
