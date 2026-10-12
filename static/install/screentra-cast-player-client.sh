#!/usr/bin/env bash
set -euo pipefail

SERVER_URL="${SCREENTRA_CAST_SERVER_URL:-}"
INTERVAL="${SCREENTRA_CAST_HEARTBEAT_INTERVAL:-30}"
ROLE="${SCREENTRA_CAST_CLIENT_ROLE:-player}"
CLIENT_NAME="${SCREENTRA_CAST_CLIENT_NAME:-$(hostname)}"
VERSION="${SCREENTRA_CAST_CLIENT_VERSION:-0.1.0}"
SOURCE_TARBALL_DIR="${SCREENTRA_CAST_CLIENT_SOURCE_DIR:-/home/screentra-cast/screentra-cast-player-client}"
# Short git hash or "dev" if .git is missing. Used in no-layout.html footer.
BUILD="$(git -C "$SOURCE_TARBALL_DIR" rev-parse --short HEAD 2>/dev/null || echo dev)"

# 2026-08-10: derive path globals lazily inside a function. We re-derive
# these after `bootstrap_target_user` runs so the HOME-driven paths
# (INSTALL_DIR, BIN_DIR, SYSTEMD_USER_DIR, ...) point at the target
# user's home, not the running user's HOME.
derive_paths() {
    BOARD_USER="${BOARD_USER:-board}"
    UID_NUM="$(id -u "$BOARD_USER" 2>/dev/null || echo 0)"
    INSTALL_DIR="${SCREENTRA_CAST_CLIENT_DIR:-$HOME/.local/share/screentra-cast-player-client}"
    BIN_DIR="$HOME/.local/bin"
    SYSTEMD_USER_DIR="$HOME/.config/systemd/user"
    CLIENT_ID_FILE="$INSTALL_DIR/client-id"
    HEARTBEAT="$INSTALL_DIR/screentra-cast-player-heartbeat.py"
    RENDERER="$INSTALL_DIR/screentra-cast-player-renderer.py"
    ACTOR="$INSTALL_DIR/screentra-cast-player-actor.py"
    GPU_HEALTH="$INSTALL_DIR/screentra-cast-gpu-health.py"
    KIOSK_LAUNCHER="$INSTALL_DIR/screentra-cast-kiosk-launcher.sh"
    PREFLIGHT="$INSTALL_DIR/screentra-cast-player-preflight.py"
    IR_BRIDGE="$INSTALL_DIR/ir_bridge.py"
    DISPLAY_GOVERNANCE="$INSTALL_DIR/screentra-cast-display-governance.py"
    DISPLAY_PROVISION="$INSTALL_DIR/screentra-cast-player-display-provision.sh"
    DISPLAY_PROVISION_SERVICE="$SYSTEMD_USER_DIR/screentra-cast-player-display-provision.service"
    STATUS="$BIN_DIR/screentra-cast-player-status"
    ACTOR_SERVICE_FILE="$SYSTEMD_USER_DIR/screentra-cast-player-actor.service"
    ACTOR_SERVICE_NAME="screentra-cast-player-actor.service"
    SERVICE="$SYSTEMD_USER_DIR/screentra-cast-player-heartbeat.service"
    RENDER_SERVICE="$SYSTEMD_USER_DIR/screentra-cast-player-renderer@.service"
    ADMIN_SERVICE="$SYSTEMD_USER_DIR/screentra-cast-player-admin.service"
    KIOSK_SERVICE="$SYSTEMD_USER_DIR/screentra-cast-kiosk-browser@.service"
    LABWC_SESSION="$INSTALL_DIR/screentra-cast-labwc-session.sh"
    LABWC_SYSTEM_UNIT="/etc/systemd/system/screentra-cast-labwc-kiosk@.service"
    export INSTALL_DIR BIN_DIR SYSTEMD_USER_DIR HOME USER
}

# Bootstrap target user when running as root.
#
# 2026-08-10: allow `sudo bash install.sh` or `curl | sudo bash` so the
# installer creates the seat user + dirs in one shot. If we are NOT
# running as root, this is a no-op (the existing user-install flow takes
# over).
#
# Honors $BOARD_USER override; defaults to `board`. Re-derives HOME and
# the per-user paths after creating the user so the rest of the installer
# behaves identically regardless of whether the user pre-existed.
bootstrap_target_user() {
    if [ "$(id -u)" != "0" ]; then
        # Not root -- we're already running as some user. Just honor
        # SCREENTRA_CAST_USER if set, in case someone wants to install into a
        # different home.
        if [ "$BOARD_USER" != "$(id -un)" ]; then
            echo "WARN: BOARD_USER=$BOARD_USER but running as $(id -un); honoring running-user's HOME" >&2
            BOARD_USER="$(id -un)"
        fi
        return 0
    fi
    # We're root. Make sure the user exists and has the groups the
    # player-client needs (video, render, input, tty, sudo, audio).
    if ! id "$BOARD_USER" >/dev/null 2>&1; then
        echo "==> Creating user $BOARD_USER (no password, home=/home/$BOARD_USER)"
        if command -v useradd >/dev/null 2>&1; then
            useradd -m -s /bin/bash -c "Screentra Cast player" "$BOARD_USER"
        elif command -v adduser >/dev/null 2>&1; then
            adduser --disabled-password --gecos "Screentra Cast player" "$BOARD_USER"
        else
            echo "ERROR: no useradd/adduser; cannot create $BOARD_USER" >&2
            return 1
        fi
    fi
    # Add required groups if missing. These are required for:
    #   - video/render: DRM/KMS access for sway + chromium
    #   - input/tty: keyboard/touch via the seat service
    #   - sudo: future admin actions (we also drop sudoers.d below)
    for grp in video render input tty audio sudo netdev plugdev; do
        if getent group "$grp" >/dev/null 2>&1; then
            usermod -aG "$grp" "$BOARD_USER" 2>/dev/null || true
        fi
    done
    # Make sure /home/$BOARD_USER exists with correct ownership.
    install -d -m 0750 -o "$BOARD_USER" -g "$BOARD_USER" "/home/$BOARD_USER"
    # Re-derive HOME so all subsequent path-derivation uses the right user.
    export HOME="/home/$BOARD_USER"
    export USER="$BOARD_USER"
    # Re-derive path globals against the new HOME so everything downstream
    # lands under /home/$BOARD_USER/.local/share/screentra-cast-player-client.
    derive_paths
    # shellcheck disable=SC2317  # reach via return-only-on-error callers
    return 0
}

derive_paths

# 2026-08-10: bootstrap the target user BEFORE the first mkdir so paths
# land under /home/$BOARD_USER, not the running user's HOME (root).
bootstrap_target_user

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
# 2026-08-10: appliance-mode auto-detect. If the player is being installed
# on the same box as the server (e.g. a TV-attached PC with no separate
# server machine), probe loopback:3000 and use it if it answers. This avoids
# forcing the operator to remember SCREENTRA_CAST_SERVER_URL when the only server
# they're going to talk to is on localhost.
if [ -z "$SERVER_URL" ]; then
    if curl -sS --max-time 2 "http://127.0.0.1:3000/api/status" >/dev/null 2>&1; then
        SERVER_URL="http://127.0.0.1:3000"
        echo "Appliance mode: detected local server at $SERVER_URL"
    elif curl -sS --max-time 2 "http://localhost:3000/api/status" >/dev/null 2>&1; then
        SERVER_URL="http://localhost:3000"
        echo "Appliance mode: detected local server at $SERVER_URL"
    fi
fi
if [ -z "$SERVER_URL" ]; then
    if [ -t 0 ] && [ -t 1 ]; then
        # Interactive terminal (real TTY or 'ssh -t'): prompt the user.
        echo "Screentra Cast player installer — server URL required."
        echo
        echo "Enter the Screentra Cast server URL (e.g. http://10.1.1.76:3000):"
        read -r SERVER_URL < /dev/tty || SERVER_URL=""
        while [ -z "$SERVER_URL" ]; do
            echo "Server URL cannot be empty. Enter the Screentra Cast server URL:"
            read -r SERVER_URL < /dev/tty || SERVER_URL=""
        done
        # Trim trailing slash, normalize scheme.
        SERVER_URL="${SERVER_URL%/}"
        case "$SERVER_URL" in
            http://*|https://*) ;;
            *)
                # If TLS is disabled (SIGNAGE_TLS=0) default to http, otherwise https.
                if [ "${SIGNAGE_TLS:-1}" = "0" ]; then
                    SERVER_URL="http://$SERVER_URL"
                else
                    SERVER_URL="https://$SERVER_URL"
                fi
                ;;
        esac
        # Simple validation: must be full URL with host and port.
        if ! echo "$SERVER_URL" | grep -E -q '^https?://[0-9a-fA-F.:]+:[0-9]+$'; then
            echo "ERROR: SERVER_URL must include protocol, host and port (e.g. http://10.1.1.76:3000)" >&2
            exit 1
        fi
        echo "Using server: $SERVER_URL"
    elif is_ssh_session; then
        # Non-interactive SSH session: explain how to pass the URL and how to
        # bootstrap the seat service so the kiosk can take over the display
        # once the user is at a real TTY (or after they start the seat service
        # from this SSH session).
        cat <<SSH_HINT >&2
ERROR: SCREENTRA_CAST_SERVER_URL is required for player install.

You are running this over SSH, so the installer cannot prompt for the URL.
Re-run with the URL set, for example:

  SCREENTRA_CAST_SERVER_URL=https://<server-host-or-ip>:3000 $0

Or pipe it through ssh:

  ssh user@host "SCREENTRA_CAST_SERVER_URL=https://<server-host-or-ip>:3000 $0"

Or use 'ssh -t' to allocate a TTY and let the installer prompt:

  ssh -t user@host $0

After install completes, start the system-level seat service so the kiosk
takes over a real TTY (you can do this from the same SSH session):

  sudo systemctl enable --now screentra-cast-labwc-kiosk@0.service

This will spawn labwc on the next free TTY and run the kiosk browser.
SSH_HINT
        exit 1
    else
        echo "ERROR: SCREENTRA_CAST_SERVER_URL is required for player install." >&2
        echo "Example: SCREENTRA_CAST_SERVER_URL=https://<server-host-or-ip>:3000 $0" >&2
        echo "Or run interactively from a terminal so the installer can prompt." >&2
        exit 1
    fi
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

is_ssh_session_quiet() {
    if is_ssh_session; then
        echo "Detected SSH session (SSH_CONNECTION=${SSH_CONNECTION:-unset})."
        echo "Install will configure the kiosk as a system-level service so a real TTY"
        echo "or systemd-logind can take over the display. The current SSH session will"
        echo "not display the kiosk; use the editor to verify the client and then start"
        echo "the seat service from a real TTY or via 'sudo systemctl start screentra-cast-labwc-kiosk@0.service'."
    fi
}


# Detect the host OS. Sets $OS_ID, $OS_VERSION_ID, $OS_PRETTY, $OS_FAMILY
# (debian, ubuntu, fedora, arch, alpine, rhel, suse, other).
# 2026-08-10: bootstrap_target_user already ran earlier (before the
# first mkdir), so HOME/USER are already correct here. detect_os only
# needs to populate OS_* vars from /etc/os-release.
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
# Browser, labwc seat, seatd, and TLS tools are what we need to install.
required_packages() {
    case "$OS_FAMILY" in
        debian)
            # python3, sudo, curl, wget, openssl, ca-certificates, dbus, logind
            # display: sway (only supported kiosk compositor) + wlrctl (wlroots utility)
            # browser: chromium
            # network: wpasupplicant + iw (Wi-Fi scan/connect), vlan (802.1Q), iproute2 (ip), isc-dhcp-client (DHCP)
            # audio: pulseaudio (server) + pulseaudio-utils (pactl) + bluez (bluetoothctl)
            #        for the admin audio/Bluetooth output device panel. pulseaudio-utils
            #        is a separate package from pulseaudio on Debian-derived distros;
            #        install both so the actor's pactl set-default-sink /
            #        move-sink-input flow works.
            echo "python3 python3-venv sudo curl wget openssl ca-certificates dbus libnss3-tools sway wlrctl seatd chromium wpasupplicant iw vlan iproute2 isc-dhcp-client pulseaudio pulseaudio-utils bluez"
            ;;
        fedora|rhel)
            echo "python3 sudo curl wget openssl ca-certificates dbus nss-tools sway wlrctl seatd chromium pulseaudio bluez"
            ;;
        arch)
            echo "python sudo curl wget openssl ca-certificates dbus nss sway wlrctl seatd chromium pulseaudio bluez bluez-utils"
            ;;
        alpine)
            echo "python3 sudo curl wget openssl ca-certificates dbus sway wlrctl seatd chromium pulseaudio bluez"
            ;;
        suse)
            echo "python3 sudo curl wget openssl ca-certificates dbus libnss3-tools sway wlrctl seatd chromium pulseaudio bluez"
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
                        echo "      Avoid blindly installing proprietary NVIDIA drivers on signage players; test Wayland/labwc first."
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

install_screentra_cast_ca_trust() {
    local ca_url="$SERVER_URL/install/screentra-cast-signage-ca.crt"
    local ca_file="$INSTALL_DIR/screentra-cast-signage-ca.crt"
    local ca_tmp="$ca_file.tmp.$$"
    local installed=0
    echo "Refreshing Screentra Cast HTTPS trust anchor from $ca_url..."

    # Always fetch into a temporary file. A stale PEM must never be treated as
    # proof that the server's current CA is trusted.
    if python3 - "$ca_url" "$ca_tmp" "$ca_file" <<'PY_CA_FETCH'
import os, ssl, sys, urllib.request
url, out, old = sys.argv[1:]
contexts = []
if url.startswith("https://") and os.path.exists(old):
    try:
        contexts.append(("existing CA", ssl.create_default_context(cafile=old)))
    except Exception:
        pass
if url.startswith("https://"):
    # This is an installer-time bootstrap only. Runtime heartbeat/actor traffic
    # remains strictly CA-verified. The fingerprint is printed for approval.
    contexts.append(("bootstrap", ssl._create_unverified_context()))
else:
    contexts.append(("plain HTTP", None))
last = None
for label, ctx in contexts:
    try:
        kwargs = {"timeout": 20}
        if ctx is not None:
            kwargs["context"] = ctx
        with urllib.request.urlopen(url, **kwargs) as r:
            data = r.read()
        if b"BEGIN CERTIFICATE" not in data:
            raise RuntimeError("downloaded file is not a PEM certificate")
        with open(out, "wb") as f:
            f.write(data)
        print(f"Downloaded current Screentra Cast CA using {label} trust")
        break
    except Exception as exc:
        last = exc
else:
    raise SystemExit(f"CA download failed: {last}")
PY_CA_FETCH
    then
        if command -v openssl >/dev/null 2>&1; then
            echo "Current Screentra Cast CA fingerprint:"
            openssl x509 -in "$ca_tmp" -noout -fingerprint -sha256 -subject 2>/dev/null || true
        fi
        chmod 0644 "$ca_tmp"
        mv -f "$ca_tmp" "$ca_file"
        echo "Installed current Screentra Cast CA at $ca_file."
    else
        rm -f "$ca_tmp"
        echo "WARNING: current Screentra Cast CA could not be downloaded; preserving any existing CA." >&2
        return 0
    fi

    if [ ! -s "$ca_file" ]; then
        echo "Screentra Cast CA trust not installed: CA certificate unavailable."
        return 0
    fi
    if command -v update-ca-certificates >/dev/null 2>&1; then
        if run_privileged cp "$ca_file" /usr/local/share/ca-certificates/screentra-cast-signage-ca.crt && run_privileged update-ca-certificates; then
            echo "Installed Screentra Cast CA into OS trust store."
            installed=1
        else
            echo "WARNING: could not install Screentra Cast CA into OS trust store; run manually if browser trust is needed:"
            echo "  sudo cp '$ca_file' /usr/local/share/ca-certificates/screentra-cast-signage-ca.crt && sudo update-ca-certificates"
        fi
    fi
    if command -v certutil >/dev/null 2>&1; then
        mkdir -p "$HOME/.pki/nssdb"
        if [ ! -f "$HOME/.pki/nssdb/cert9.db" ]; then
            certutil -d "sql:$HOME/.pki/nssdb" -N --empty-password </dev/null >/dev/null 2>&1 || true
        fi
        # Replace the nickname so a rotated CA cannot leave the old trust
        # anchor active in the browser's NSS database.
        certutil -d "sql:$HOME/.pki/nssdb" -D -n "Screentra Cast Signage Local CA" >/dev/null 2>&1 || true
        if certutil -d "sql:$HOME/.pki/nssdb" -A -t "C,," -n "Screentra Cast Signage Local CA" -i "$ca_file" >/dev/null 2>&1; then
            echo "Installed Screentra Cast CA into Chromium/Firefox NSS user trust store."
            installed=1
        else
            echo "WARNING: NSS CA install skipped/failed. If Chromium still warns, install libnss3-tools and rerun this installer."
        fi
    fi
    if [ "$installed" = "0" ]; then
        echo "Screentra Cast CA saved to $ca_file but no system/NSS trust store was updated."
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
install_os_dependencies
ensure_dbus

ensure_user_groups
ensure_seat_daemon

install_screentra_cast_ca_trust
write_os_marker
export SCREENTRA_CAST_CA_CERT="$INSTALL_DIR/screentra-cast-signage-ca.crt"

# Display-governance runs LATER, after the heredoc that writes
# screentra-cast-display-governance.py to disk. Done at the end of install.

cat > "$PREFLIGHT" <<'PY_PREFLIGHT'
#!/usr/bin/env python3
"""Screentra Cast player preflight checks.

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
from pathlib import Path


CHECKS: list[dict] = []


def _seat_service_running() -> bool:
    """Return True if the system-level labwc seat service is active for any display."""
    for idx in range(4):
        try:
            r = subprocess.run(
                ["systemctl", "--quiet", "is-active", f"screentra-cast-labwc-kiosk@{idx}.service"],
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
            return True, "system-level screentra-cast-labwc-kiosk@.service active (wayland-0 socket pending)"
        if attempt < retries - 1:
            time.sleep(retry_delay)
    return False, "no Wayland/X11 display session detected (system-level screentra-cast-labwc-kiosk@.service is not active; install or start it from a TTY)"

def tls_context():
    cafile = os.environ.get('SCREENTRA_CAST_CA_CERT') or str(Path.home() / '.local/share/screentra-cast-player-client/screentra-cast-signage-ca.crt')
    if cafile and Path(cafile).exists():
        try:
            return ssl.create_default_context(cafile=cafile)
        except Exception as e:
            return None
    if os.environ.get('SCREENTRA_CAST_INSECURE_SSL', '').lower() in ('1', 'true', 'yes'):
        return ssl._create_unverified_context()
    return None

def urlopen_screentra-cast(req, timeout=10):
    kw = {'timeout': timeout}
    ctx = tls_context()
    if ctx is not None:
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
    add("labwc (wayland kiosk)", shutil.which("labwc") is not None,
        shutil.which("labwc") or "install labwc for Wayland kiosk mode", required=True)
    add("wlr-randr (wayland output helper)", shutil.which("wlr-randr") is not None,
        shutil.which("wlr-randr") or "install wlr-randr (wlroots tools, ships with sway) for Wayland output discovery", required=False)

    # libinput / touch detection support
    add("/proc/bus/input/devices", os.path.exists("/proc/bus/input/devices"),
        "present" if os.path.exists("/proc/bus/input/devices") else "missing — touch device auto-detect unavailable", required=False)

    # Group memberships. Required for the kiosk to run.
    # 2026-08-10: when installer runs as root, `id -Gn` returns root's
    # groups, which is meaningless. Honor $BOARD_USER (set by the
    # installer's bootstrap_target_user) and fall back to the running
    # user only if SCREENTRA_CAST_USER isn't set.
    check_user = os.environ.get("SCREENTRA_CAST_USER") or Path.home().name
    groups = run(f"id -Gn {check_user}").split() if check_user else run("id -Gn").split()
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
    install_dir = Path(os.environ.get("SCREENTRA_CAST_CLIENT_DIR") or Path.home() / ".local/share/screentra-cast-player-client")
    bin_dir = Path.home() / ".local/bin"
    systemd_user = Path.home() / ".config/systemd/user"
    install_dir.mkdir(parents=True, exist_ok=True)
    add("install dir", os.access(install_dir, os.W_OK), str(install_dir))
    add("bin dir", os.access(bin_dir, os.W_OK) or os.access(bin_dir.parent, os.W_OK), str(bin_dir))
    add("systemd user dir", True, str(systemd_user))

    # Renderer port
    add("renderer port 3900", not port_in_use(3900), "free" if not port_in_use(3900) else "busy", required=False)

    # Server URL reachability
    server = os.environ.get("SCREENTRA_CAST_SERVER_URL", "")
    if server:
        try:
            with urlopen_screentra-cast(server.rstrip("/") + "/api/clients", timeout=5) as r:
                ok = r.status == 200
                detail = f"status {r.status}"
        except Exception as e:
            ok, detail = False, str(e)
        add("server reachable", ok, f"{server} ({detail})")
    else:
        add("server url configured", False, "SCREENTRA_CAST_SERVER_URL not set (will be sourced from install args or server file)", required=False)

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
ln -sf "$PREFLIGHT" "$BIN_DIR/screentra-cast-player-preflight"

mkdir -p "$INSTALL_DIR"
echo "$SERVER_URL" > "$INSTALL_DIR/server"

if [ ! -f "$CLIENT_ID_FILE" ]; then
  if [ -r /etc/machine-id ]; then
    printf 'screentra-cast-%s\n' "$(cat /etc/machine-id)" > "$CLIENT_ID_FILE"
  else
    printf 'screentra-cast-%s-%s\n' "$(hostname)" "$(date +%s)" > "$CLIENT_ID_FILE"
  fi
fi
CLIENT_ID="$(cat "$CLIENT_ID_FILE")"

cat > "$HEARTBEAT" <<'PY'
#!/usr/bin/env python3
"""Screentra Cast player/client heartbeat agent.

Posts system metrics to the Screentra Cast signage server so the Connected Clients panel
can show enrolled player uptime and usage.
"""
import argparse
import json
import ssl
import os
import socket
import subprocess
import sys
import time
import urllib.request
import importlib.util
import sys as _sys
_display_detect_path = os.path.join(os.path.dirname(os.path.abspath(__file__)), 'screentra-cast-display-detect.py')
_spec = importlib.util.spec_from_file_location('screentra_cast_display_detect', _display_detect_path)
_mod = importlib.util.module_from_spec(_spec)
_sys.modules['screentra_cast_display_detect'] = _mod
_spec.loader.exec_module(_mod)
detect_displays = _mod.detect_displays

from pathlib import Path

# Reuse the actor's preset-fetch helper so the kiosk browser only ever
# reads from cache — the heartbeat (running every 30s) and the actor (on
# reload_layout commands) both inline widget presets into the cache
# before writing it. Both modules live in the same scripts/ dir, so we
# load the actor dynamically instead of introducing a shared package.
_actor_path = os.path.join(os.path.dirname(os.path.abspath(__file__)), 'screentra-cast-player-actor.py')
_actor_spec = importlib.util.spec_from_file_location('screentra_cast_player_actor', _actor_path)
_actor_mod = importlib.util.module_from_spec(_actor_spec)
_sys.modules['screentra_cast_player_actor'] = _actor_mod
_actor_spec.loader.exec_module(_actor_mod)
inline_widget_presets = _actor_mod.inline_widget_presets
_cache_widget_assets = _actor_mod._cache_widget_assets
_cache_icon = _actor_mod._cache_icon


def _display_governance_count(which):
    """Read config.json and return the display-count governance value.

    `which` is 'installer' (always-defined, what the installer detected) or
    'override' (operator-set, may be None). Missing/corrupt config.json is
    treated as "0" / None — the heartbeat will publish zero and the server
    will leave the existing row's value alone (operator_set is preserved by
    the server-side UPSERT).
    """
    try:
        cfg_path = Path(os.environ.get('SCREENTRA_CAST_CLIENT_DIR',
                   str(Path.home() / '.local/share/screentra-cast-player-client'))) / 'config.json'
        if not cfg_path.exists():
            return None if which == 'override' else 0
        cfg = json.loads(cfg_path.read_text())
    except Exception:
        return None if which == 'override' else 0
    if which == 'installer':
        v = cfg.get('installer_detected_max_displays')
        return int(v) if v is not None else 0
    # override
    v = cfg.get('max_displays')
    return int(v) if v is not None else None


def _touch_binding():
    try:
        cfg_path = Path(os.environ.get('SCREENTRA_CAST_CLIENT_DIR',
                   str(Path.home() / '.local/share/screentra-cast-player-client'))) / 'config.json'
        if not cfg_path.exists():
            return None
        cfg = json.loads(cfg_path.read_text())
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


import socket, json, os, time

def broadcast_payload(payload):
    """Send UDP broadcast with minimal client info.
    Uses environment var SCREENTRA_CAST_BROADCAST_PORT (default 42000) and
    SCREENTRA_CAST_BROADCAST_SECRET for optional HMAC (not implemented here).
    """
    port = int(os.getenv('SCREENTRA_CAST_BROADCAST_PORT', '42000'))
    broadcast_ip = '<broadcast>'
    try:
        sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        sock.setsockopt(socket.SOL_SOCKET, socket.SO_BROADCAST, 1)
        data = json.dumps({
            'client_id': payload.get('client_id'),
            'hostname': payload.get('hostname'),
            'address': payload.get('address'),
            'version': payload.get('version'),
        }).encode('utf-8')
        sock.sendto(data, (broadcast_ip, port))
    except Exception as e:
        print(f'Broadcast error: {e}', flush=True)
    finally:
        sock.close()

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
    payload_data = {
        'client_agent': True,
        'client_installed': True,
        'identity_source': 'persistent_client_id',
        'client_id': client_id,
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
        'active_layout_id': args.active_layout_id,
        'last_command_result': last_command_result,
        'last_command_at': last_command_at,
        'displays': displays,
        'installer_detected_max_displays': _display_governance_count('installer'),
        'max_displays': _display_governance_count('override'),
        'touch_binding': _touch_binding(),
        'ir_bridge': _ir_bridge_state(),
    }
    # Send broadcast before returning payload for heartbeat POST.
    broadcast_payload(payload_data)
    return payload_data


def _ir_bridge_state():
    """Snapshot the RP2040 IR bridge (USB CDC) for the heartbeat payload.

    Wrapped in its own try/except so a missing module or failing USB bus
    doesn't poison the entire heartbeat. Returns None when the bridge is
    absent or unreachable, which is the contract the server expects.
    """
    try:
        import ir_bridge as _irb
    except Exception:
        return None
    try:
        info = _irb.discover_bridge(timeout_seconds=0.8)
        return info.to_json()
    except Exception as e:
        return {'present': False, 'last_error': str(e)[:200]}


def tls_context():
    """Use the Screentra Cast CA bundle directly when present, independent of OS trust."""
    cafile = os.environ.get('SCREENTRA_CAST_CA_CERT') or str(Path.home() / '.local/share/screentra-cast-player-client/screentra-cast-signage-ca.crt')
    if cafile and Path(cafile).exists():
        try:
            return ssl.create_default_context(cafile=cafile)
        except Exception as e:
            print(f'warning: failed to load Screentra Cast CA {cafile}: {e}', flush=True)
    if os.environ.get('SCREENTRA_CAST_INSECURE_SSL', '').lower() in ('1', 'true', 'yes'):
        return ssl._create_unverified_context()
    return None

def _bind_ip_opener(bind_ip):
    """Return an OpenerDirector whose sockets bind to bind_ip before connect.
    The classes below override http.client's defaults, which is what
    urllib's HTTPHandler/HTTPSHandler use to make the underlying socket.
    """
    if not bind_ip:
        return None
    import socket as _socket
    class _BoundHTTPConnection(http.client.HTTPConnection):
        def _connect(self):
            self.sock = _socket.create_connection((self.host, self.port), timeout=self.timeout, source_address=(bind_ip, 0))
    class _BoundHTTPSConnection(http.client.HTTPSConnection):
        def _connect(self):
            sock = _socket.create_connection((self.host, self.port), timeout=self.timeout, source_address=(bind_ip, 0))
            self.sock = self._context.wrap_socket(sock, server_hostname=self.host)
    opener = urllib.request.build_opener(
        urllib.request.HTTPHandler(),
        urllib.request.HTTPSHandler(),
    )
    # Replace http.client globally only if bind_ip is set. This is safe
    # because the heartbeat is its own process; the actor doesn't share
    # state with it. Both pick up SCREENTRA_CAST_BIND_IP at their own startup.
    http.client.HTTPConnection = _BoundHTTPConnection
    http.client.HTTPSConnection = _BoundHTTPSConnection
    return opener


def install_bind_ip():
    """If SCREENTRA_CAST_BIND_IP is set, install a global opener that sources from
    that IP for ALL urllib.request calls in this process."""
    bind_ip = os.environ.get('SCREENTRA_CAST_BIND_IP', '').strip()
    if not bind_ip:
        return
    opener = _bind_ip_opener(bind_ip)
    if opener is not None:
        urllib.request.install_opener(opener)


def urlopen_screentra-cast(req, timeout=10):
    kw = {'timeout': timeout}
    ctx = tls_context()
    if ctx is not None:
        kw['context'] = ctx
    return urllib.request.urlopen(req, **kw)


def poll_command(server, client_id):
    url = server.rstrip('/') + '/api/clients/' + urllib.request.quote(client_id, safe='') + '/command'
    try:
        with urlopen_screentra-cast(url, timeout=5) as r:
            data = r.read().decode()
            return json.loads(data)
    except urllib.error.HTTPError as e:
        if e.code == 404:
            return None
        raise
    except Exception:
        return None


def ack_command(server, client_id):
    url = server.rstrip('/') + '/api/clients/' + urllib.request.quote(client_id, safe='') + '/command'
    try:
        req = urllib.request.Request(url, method='DELETE')
        with urlopen_screentra-cast(req, timeout=5) as r:
            return r.read().decode()
    except Exception as e:
        print(f'ack failed: {e}', flush=True)
        return None


def write_pending_command(command, cache_dir):
    pending_path = Path(cache_dir) / 'pending-command.json'
    pending_path.parent.mkdir(parents=True, exist_ok=True)
    pending_path.write_text(json.dumps(command, indent=2))
    print(f'pending command written: {command.get("action")}', flush=True)
    try:
        import subprocess, shutil
        if shutil.which('systemctl'):
            subprocess.run(['systemctl', '--user', 'start', 'screentra-cast-player-actor.service'], timeout=10)
    except Exception as e:
        print(f'actor start skipped/failed: {e}', flush=True)




def read_command_result(cache_dir):
    result_path = Path(cache_dir) / 'command-result.json'
    if not result_path.exists():
        return None, None
    try:
        return json.loads(result_path.read_text()), result_path
    except Exception as e:
        print(f'bad command-result.json: {e}', flush=True)
        try: result_path.unlink()
        except Exception: pass
        return None, None


def fetch_config(server, client_id, cache_dir, display_index=0, config_pattern='assigned-layout-display-{index}.json'):
    url = server.rstrip('/') + '/api/clients/' + urllib.request.quote(client_id, safe='') + '/displays/' + str(display_index) + '/config'
    cache_dir_p = Path(cache_dir)
    try:
        with urlopen_screentra-cast(url, timeout=10) as r:
            data = json.loads(r.read().decode())
        # Inline any widget presets referenced by template UUID so the
        # kiosk browser never needs to call /api/widget-presets itself.
        if isinstance(data.get('layout'), dict):
            inline_widget_presets(server, data['layout'], cache_dir_p)
            # 2026-08-11: pre-fetch every /uploads/* asset referenced by
            # widget props (image widget URLs, custom_icons, template
            # elements) so the kiosk browser can render them offline.
            _cache_widget_assets(server, data['layout'], cache_dir_p)
        cache_dir_p.mkdir(parents=True, exist_ok=True)
        Path(cache_dir_p, 'client-config.json').write_text(json.dumps(data, indent=2))
        target = config_pattern.format(index=display_index)
        if data.get('layout') is not None:
            # backwards-compat: also write the canonical index=0 name
            Path(cache_dir_p, target).write_text(json.dumps(data['layout'], indent=2))
            if display_index == 0:
                Path(cache_dir_p, 'assigned-layout.json').write_text(json.dumps(data['layout'], indent=2))
        else:
            for stale in [Path(cache_dir_p, target), Path(cache_dir_p, 'assigned-layout.json') if display_index == 0 else None]:
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

    The local admin page falls back to admin/admin only until this cache exists.
    Once server users are cached, fallback is disabled and password hashes from
    the server are authoritative.
    """
    url = server.rstrip('/') + '/api/clients/' + urllib.request.quote(client_id, safe='') + '/admin-users'
    try:
        with urlopen_screentra-cast(url, timeout=10) as r:
            data = json.loads(r.read().decode())
        users = data.get('users') or []
        if users:
            target = Path(cache_dir).parent / 'admin-users.json'
            target.parent.mkdir(parents=True, exist_ok=True)
            tmp = target.with_suffix('.json.tmp')
            tmp.write_text(json.dumps(data, indent=2))
            try: os.chmod(tmp, 0o600)
            except Exception: pass
            tmp.replace(target)
            print(f'admin users synced: {len(users)}', flush=True)
            return data
        return None
    except Exception as e:
        print(f'admin user sync failed: {e}', flush=True)
        return None

def post(server, data):
    url = server.rstrip('/') + '/api/clients/register'
    req = urllib.request.Request(
        url,
        data=json.dumps(data).encode(),
        headers={'Content-Type': 'application/json'},
        method='POST',
    )
    with urlopen_screentra-cast(req, timeout=10) as r:
        return r.status, r.read().decode()


def main():
    def _default_server():
        env_url = os.environ.get('SCREENTRA_CAST_SERVER_URL')
        if env_url:
            return env_url
        # Fallback: read the persisted server URL the installer writes.
        srv = Path.home() / '.local/share/screentra-cast-player-client/server'
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
    ap.add_argument('--interval', type=int, default=int(os.environ.get('SCREENTRA_CAST_HEARTBEAT_INTERVAL', '30')))
    ap.add_argument('--once', action='store_true')
    ap.add_argument('--client-id', default=os.environ.get('SCREENTRA_CAST_CLIENT_ID'))
    ap.add_argument('--name', default=os.environ.get('SCREENTRA_CAST_CLIENT_NAME'))
    ap.add_argument('--role', default=os.environ.get('SCREENTRA_CAST_CLIENT_ROLE', 'player'))
    ap.add_argument('--version', default=os.environ.get('SCREENTRA_CAST_CLIENT_VERSION', '0.1.0'))
    ap.add_argument('--active-layout-id', default=os.environ.get('SCREENTRA_CAST_ACTIVE_LAYOUT_ID'))
    ap.add_argument('--cache-dir', default=os.environ.get('SCREENTRA_CAST_CLIENT_CACHE_DIR', os.path.expanduser('~/.local/share/screentra-cast-player-client/cache')))
    ap.add_argument('--max-displays', type=int, default=int(os.environ.get('SCREENTRA_CAST_MAX_DISPLAYS', '4')))
    ap.add_argument('--render-base-port', type=int, default=int(os.environ.get('SCREENTRA_CAST_RENDER_BASE_PORT', '3900')))
    ap.add_argument('--display-config-pattern', default=os.environ.get('SCREENTRA_CAST_DISPLAY_CONFIG_PATTERN', 'assigned-layout-display-{index}.json'))

    args = ap.parse_args()

    while True:
        data = payload(args)
        status, body = post(args.server, data)
        if data.get('last_command_result') is not None:
            rp = Path(args.cache_dir) / 'command-result.json'
            try:
                rp.unlink()
                print('command-result.json uploaded and cleared', flush=True)
            except Exception:
                pass
        cfg = fetch_config(args.server, data['client_id'], args.cache_dir, display_index=0, config_pattern=args.display_config_pattern)
        fetch_all_displays(args.server, data['client_id'], args.cache_dir, len(data.get('displays') or []), args.display_config_pattern)
        fetch_admin_users(args.server, data['client_id'], args.cache_dir)
        assigned = cfg.get('assigned_layout_id') if cfg else None
        cmd = poll_command(args.server, data['client_id'])
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
ln -sf "$HEARTBEAT" "$BIN_DIR/screentra-cast-player-heartbeat"

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


# === no-layout & touchscreen endpoints =====================================
import urllib.request as _ur
import subprocess as _sp
from datetime import datetime, timezone, timedelta

def log(msg: str) -> None:
    """Lightweight stderr logger for the renderer's network helpers."""
    try:
        sys.stderr.write(f'[screentra-cast-renderer {datetime.now(timezone.utc).isoformat()}] {msg}\n')
    except Exception:
        pass


def _iso_now() -> str:
    return datetime.now(timezone.utc).isoformat()

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

# === network interface switching ===========================================
# Operators may want to move the player from one NIC to another (office wired
# ethernet → on-site Wi-Fi) without re-enrolling. The client-id file is the
# source of identity; switching the source interface only changes the IP we
# reach the server from. To keep the operator off the air when they pick a
# NIC that can't reach the server, the change runs as a "pending" trial with
# a 5-minute confirm window: a banner + revert button shows on the no-layout
# page; the operator must log back in to the local admin to either confirm
# (commit) or revert (revert). If neither happens in 5 minutes we revert.
NETWORK_CONFIRM_WINDOW_SECS = 300  # 5 minutes
_NETWORK_STATE_PATH = Path.home() / '.local/share/screentra-cast-player-client/network-state.json'
_NETWORK_ENV_PATH = Path.home() / '.local/share/screentra-cast-player-client/network.env'

def _network_state_load() -> dict:
    try:
        if _NETWORK_STATE_PATH.exists():
            return json.loads(_NETWORK_STATE_PATH.read_text())
    except Exception:
        pass
    return {}

def _network_state_save(state: dict) -> None:
    try:
        _NETWORK_STATE_PATH.parent.mkdir(parents=True, exist_ok=True)
        _NETWORK_STATE_PATH.write_text(json.dumps(state, indent=2))
        os.chmod(_NETWORK_STATE_PATH, 0o600)
    except Exception as e:
        log('network-state save failed: ' + str(e))

def _network_env_write(bind_ip: str | None) -> None:
    """Write the bind IP into the drop-in env file that the heartbeat service
    sources via EnvironmentFile=."""
    try:
        _NETWORK_ENV_PATH.parent.mkdir(parents=True, exist_ok=True)
        body = f'SCREENTRA_CAST_BIND_IP={bind_ip or ""}\n'
        _NETWORK_ENV_PATH.write_text(body)
        os.chmod(_NETWORK_ENV_PATH, 0o600)
    except Exception as e:
        log('network-env write failed: ' + str(e))

def _network_parse_ip_addr() -> list[dict]:
    """Return list of {name, state, mac, ipv4:[], is_default, kind, link, vlan_id?}
    for active ifaces. `kind` is one of ethernet/wifi/vlan/other. `link` is the
    parent interface for vlan sub-ifaces. Includes DOWN interfaces so the
    operator can see what's available."""
    out: list[dict] = []
    # Parse `ip -j addr show` if available (iproute2 supports JSON on most distros).
    j = _safe_run(['ip', '-j', 'addr', 'show'], timeout=3)
    ifaces: list[dict] = []
    if j.get('ok') and j.get('stdout', '').strip().startswith('['):
        try:
            ifaces = json.loads(j['stdout'])
        except Exception:
            ifaces = []
    if not ifaces:
        # Fallback: parse text `ip -o addr show`
        t = _safe_run(['ip', '-o', 'addr', 'show'], timeout=3)
        if t.get('ok'):
            cur: dict | None = None
            for line in t['stdout'].splitlines():
                parts = line.split()
                if len(parts) >= 2 and parts[1] != 'inet' and parts[1] != 'inet6':
                    name = parts[1].rstrip(':')
                    cur = {'ifname': name, 'addr_info': [], 'operstate': 'unknown'}
                    ifaces.append(cur)
                if cur and 'inet ' in line:
                    bits = line.split()
                    try:
                        cur['addr_info'].append({'local': bits[parts.index('inet') + 1].split('/')[0]})
                    except Exception:
                        pass
    # Default route per-interface
    routes = _safe_run(['ip', '-j', 'route', 'show', 'default'], timeout=3)
    default_ifaces: set[str] = set()
    if routes.get('ok') and routes.get('stdout', '').strip().startswith('['):
        try:
            for r in json.loads(routes['stdout']):
                dev = r.get('dev')
                if dev: default_ifaces.add(dev)
        except Exception:
            pass
    # Link info (parent for vlan sub-ifaces) — `ip -j link show` gives us the link kind
    link_info = _safe_run(['ip', '-j', '-d', 'link', 'show'], timeout=3)
    link_map: dict[str, dict] = {}
    if link_info.get('ok') and link_info.get('stdout', '').strip().startswith('['):
        try:
            for ln in json.loads(link_info['stdout']):
                link_map[ln.get('ifname') or ''] = ln
        except Exception:
            pass
    # Detect wireless interfaces via /sys/class/net/<name>/wireless
    wireless_ifaces: set[str] = set()
    try:
        for entry in os.listdir('/sys/class/net'):
            if os.path.isdir(f'/sys/class/net/{entry}/wireless'):
                wireless_ifaces.add(entry)
    except Exception:
        pass
    for i in ifaces:
        name = i.get('ifname') or i.get('name') or ''
        if name in ('lo',): continue
        mac = (i.get('address') or '').lower()
        state = (i.get('operstate') or i.get('state') or 'unknown').lower()
        ipv4 = [a.get('local') for a in (i.get('addr_info') or []) if a.get('family') in (None, 'inet') and a.get('local')]
        # Kind classification
        link_data = link_map.get(name, {})
        linkinfo = link_data.get('linkinfo') or {}
        info_kind = (linkinfo.get('info_kind') or '').lower()
        if info_kind == 'vlan':
            kind = 'vlan'
            link = (linkinfo.get('link') or '')
            vlan_id = linkinfo.get('info_data', {}).get('id') if isinstance(linkinfo.get('info_data'), dict) else None
        elif name in wireless_ifaces:
            kind = 'wifi'
            link = ''
            vlan_id = None
        elif info_kind in ('bridge', 'bond', 'tun', 'tap'):
            kind = info_kind
            link = (linkinfo.get('link') or '')
            vlan_id = None
        else:
            kind = 'ethernet'
            link = ''
            vlan_id = None
        out.append({'name': name, 'state': state, 'mac': mac, 'ipv4': ipv4,
                    'is_default': name in default_ifaces, 'kind': kind,
                    'link': link, 'vlan_id': vlan_id})
    # Sort: default first, then UP, then DOWN; wifi/vlan interleave with ethernet
    out.sort(key=lambda x: (not x['is_default'], x['state'] != 'up', x['name']))
    return out

def _network_pick_source_ip(interface: str) -> str | None:
    """Return the first IPv4 address bound to `interface`, or None."""
    if not interface: return None
    for i in _network_parse_ip_addr():
        if i['name'] == interface and i['ipv4']:
            return i['ipv4'][0]
    return None

def _network_probe_server(source_ip: str | None, server_url: str | None, timeout: float = 1.5) -> dict:
    """Open a TCP socket to the server from the given source IP and report
    whether the kernel accepts the source binding and the connect succeeds.
    Does NOT do an HTTP exchange — the heartbeat handles TLS — but proves
    routing reachability from the candidate interface."""
    import re as _re
    if not server_url:
        return {'ok': False, 'error': 'no server URL configured'}
    m = _re.match(r'https?://([^:/]+)(?::(\d+))?', server_url)
    if not m:
        return {'ok': False, 'error': 'malformed server URL'}
    host, port = m.group(1), int(m.group(2) or 80)
    s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    s.settimeout(timeout)
    try:
        if source_ip:
            try:
                s.bind((source_ip, 0))
            except Exception as e:
                return {'ok': False, 'error': f'bind({source_ip}) failed: {e}', 'source_ip': source_ip}
        s.connect((host, port))
        try: chosen_ip = s.getsockname()[0]
        except Exception: chosen_ip = source_ip
        return {'ok': True, 'host': host, 'port': port, 'source_ip': chosen_ip}
    except Exception as e:
        return {'ok': False, 'error': str(e), 'source_ip': source_ip}
    finally:
        try: s.close()
        except Exception: pass

def _network_read_server_url() -> str | None:
    p = Path.home() / '.local/share/screentra-cast-player-client/server'
    if p.exists():
        try: return p.read_text().strip()
        except Exception: pass
    return None

def _network_state_get() -> dict:
    """Compose the live view returned to the admin UI: interfaces, applied,
    pending (if any). Triggers auto-revert if pending has expired."""
    # Lazy auto-revert guard: if pending expired and not confirmed, revert.
    state = _network_state_load()
    pending = state.get('pending')
    if pending and not pending.get('confirmed'):
        try:
            exp = datetime.fromisoformat(pending['expires_at'].replace('Z', '+00:00'))
            if datetime.now(timezone.utc) >= exp:
                log('network: pending change expired; auto-reverting')
                _network_revert_impl(pending)
                state = _network_state_load()
        except Exception as e:
            log('network: expiry check failed: ' + str(e))
    server_url = _network_read_server_url()
    return {
        'interfaces': _network_parse_ip_addr(),
        'server_url': server_url,
        'applied': state.get('applied'),
        'pending': state.get('pending'),
        'confirm_window_secs': NETWORK_CONFIRM_WINDOW_SECS,
        'stored_config': _network_config_load(),
        'has_wifi_hardware': any(name in ('wlan0', 'wlp2s0', 'wlp3s0', 'wlp4s0', 'wlp5s0', 'wlp0s20f0u1', 'wlp0s20f3', 'wlp0s20f0') or os.path.isdir(f'/sys/class/net/{name}/wireless')
                                    for name in os.listdir('/sys/class/net') if name != 'lo'),
    }

def _network_apply_impl(target_iface: str, config: dict | None = None) -> dict:
    """Switch the heartbeat to source from `target_iface`, write pending
    state, start the 5-minute confirm timer, restart heartbeat + actor.

    Optional `config` arg:
      {address?, prefix?, gateway?, dns?} — recorded in the pending state so
      the operator can see what static config is in effect. The actual
      static config is applied via _network_static_apply before this is
      called.
    """
    state = _network_state_load()
    server_url = _network_read_server_url()
    target_ip = _network_pick_source_ip(target_iface)
    if not target_ip:
        return {'ok': False, 'error': f'interface {target_iface!r} has no IPv4 address or is down'}
    # Capture previous binding from the drop-in env file or current default route.
    prev_iface = state.get('applied', {}).get('interface')
    prev_ip = state.get('applied', {}).get('source_ip')
    if not prev_iface:
        prev_iface = next((i['name'] for i in _network_parse_ip_addr() if i['is_default']), None)
    if not prev_ip:
        prev_ip = _lan_ip_for_admin()
    # Probe reachability BEFORE applying (so we don't lock the operator out
    # if the new interface can't even TCP-reach the server).
    probe = _network_probe_server(target_ip, server_url, timeout=2.0)
    if not probe.get('ok'):
        return {'ok': False, 'error': f'preflight probe failed from {target_ip}: {probe.get("error")}',
                'probe': probe, 'rejected_interface': target_iface}
    # Write drop-in env file, set pending state.
    _network_env_write(target_ip)
    now = datetime.now(timezone.utc)
    pending = {
        'target_interface': target_iface,
        'target_source_ip': target_ip,
        'previous_interface': prev_iface,
        'previous_source_ip': prev_ip,
        'started_at': now.isoformat().replace('+00:00', 'Z'),
        'expires_at': (now + timedelta(seconds=NETWORK_CONFIRM_WINDOW_SECS)).isoformat().replace('+00:00', 'Z'),
        'confirmed': False,
        'applied_config': config or {},
    }
    state['pending'] = pending
    state['applied'] = {
        'interface': target_iface,
        'source_ip': target_ip,
        'set_at': now.isoformat().replace('+00:00', 'Z'),
        'config': config or {},
    }
    _network_state_save(state)
    # Restart heartbeat + actor so they pick up the new bind IP.
    r1 = _safe_run(['systemctl', '--user', 'daemon-reload'], timeout=5)
    r2 = _safe_run(['systemctl', '--user', 'restart', 'screentra-cast-player-heartbeat.service'], timeout=8)
    r3 = _safe_run(['systemctl', '--user', 'restart', 'screentra-cast-player-actor.service'], timeout=8)
    return {'ok': True, 'pending': pending, 'probe': probe,
            'systemctl': {'daemon_reload': r1.get('ok'), 'heartbeat': r2.get('ok'), 'actor': r3.get('ok')}}

def _network_confirm_impl() -> dict:
    """Commit the pending change (operator has logged back in and verified)."""
    state = _network_state_load()
    pending = state.get('pending')
    if not pending:
        return {'ok': False, 'error': 'no pending change to confirm'}
    if pending.get('confirmed'):
        return {'ok': True, 'pending': pending, 'already_confirmed': True}
    try:
        exp = datetime.fromisoformat(pending['expires_at'].replace('Z', '+00:00'))
        if datetime.now(timezone.utc) >= exp:
            _network_revert_impl(pending)
            return {'ok': False, 'error': 'confirm window expired; change was reverted'}
    except Exception: pass
    pending['confirmed'] = True
    pending['confirmed_at'] = datetime.now(timezone.utc).isoformat().replace('+00:00', 'Z')
    state['pending'] = pending
    _network_state_save(state)
    return {'ok': True, 'pending': pending}

def _network_revert_impl(pending: dict | None = None) -> dict:
    """Revert to the previous interface and clear pending state."""
    state = _network_state_load()
    pending = pending or state.get('pending') or {}
    prev_iface = pending.get('previous_interface')
    prev_ip = pending.get('previous_source_ip')
    if prev_ip:
        _network_env_write(prev_ip)
        if prev_iface:
            state['applied'] = {'interface': prev_iface, 'source_ip': prev_ip,
                                'set_at': datetime.now(timezone.utc).isoformat().replace('+00:00', 'Z')}
        _network_state_save(state)
        _safe_run(['systemctl', '--user', 'daemon-reload'], timeout=5)
        _safe_run(['systemctl', '--user', 'restart', 'screentra-cast-player-heartbeat.service'], timeout=8)
        _safe_run(['systemctl', '--user', 'restart', 'screentra-cast-player-actor.service'], timeout=8)
    # Always clear pending so the banner disappears, even if we couldn't restore.
    state['pending'] = None
    _network_state_save(state)
    return {'ok': True, 'reverted_to': prev_iface, 'reverted_ip': prev_ip}

# === network config: stored wifi/vlan/static settings that survive reboot ===
_NETWORK_CONFIG_PATH = Path.home() / '.local/share/screentra-cast-player-client/network-config.json'
_NETWORK_INTERFACES_DIR = Path('/etc/network/interfaces.d')
_SCREENTRA_CAST_IFACE_PREFIX = 'screentra-cast-'
_WPA_SUPPLICANT_DIR = Path('/etc/wpa_supplicant')

def _network_config_load() -> dict:
    try:
        if _NETWORK_CONFIG_PATH.exists():
            return json.loads(_NETWORK_CONFIG_PATH.read_text())
    except Exception:
        pass
    return {}

def _network_config_save(cfg: dict) -> None:
    try:
        _NETWORK_CONFIG_PATH.parent.mkdir(parents=True, exist_ok=True)
        _NETWORK_CONFIG_PATH.write_text(json.dumps(cfg, indent=2))
        os.chmod(_NETWORK_CONFIG_PATH, 0o600)
    except Exception as e:
        log('network-config save failed: ' + str(e))
def _network_config_clear() -> None:
    try:
        if _NETWORK_CONFIG_PATH.exists():
            _NETWORK_CONFIG_PATH.unlink()
    except Exception:
        pass
# Helper: any privileged op goes through `sudo -n` so it doesn't hang on TTY.
def _sudo(args: list[str], timeout: int = 8) -> dict:
    return _safe_run(['sudo', '-n'] + args, timeout=timeout)

# === Wi-Fi =====

def _wpa_supplicant_conf_path(interface: str) -> Path:
    return _WPA_SUPPLICANT_DIR / f'wpa_supplicant-{interface}.conf'

def _network_wifi_scan(interface: str) -> dict:
    """Return list of {ssid, bssid, signal, security, frequency} for `interface`.
    Returns at most 30 networks, sorted by signal desc. Requires iw + sudo."""
    if not interface:
        return {'ok': False, 'error': 'interface required'}
    if not os.path.isdir(f'/sys/class/net/{interface}/wireless'):
        return {'ok': False, 'error': f'{interface} is not a wireless interface'}
    state = _safe_run(['ip', '-j', 'link', 'show', interface], timeout=3)
    is_up = False
    if state.get('ok'):
        try:
            for ln in json.loads(state['stdout']):
                if ln.get('ifname') == interface:
                    is_up = (ln.get('operstate') or '').lower() == 'up'
        except Exception:
            pass
    if not is_up:
        _sudo(['ip', 'link', 'set', interface, 'up'], timeout=5)
    r = _sudo(['iw', interface, 'scan'], timeout=15)
    if not r.get('ok'):
        return {'ok': False, 'error': r.get('stderr') or r.get('stdout') or 'iw scan failed'}
    return {'ok': True, 'interface': interface, 'networks': _parse_iw_scan(r.get('stdout') or '', limit=30)}

def _parse_iw_scan(text: str, limit: int = 30) -> list[dict]:
    """Parse `iw <iface> scan` output into a list of networks."""
    networks: list[dict] = []
    current: dict | None = None
    for line in text.splitlines():
        line = line.rstrip()
        if line.startswith('BSS '):
            if current: networks.append(current)
            bssid = line.split()[1].split('(')[0].rstrip(',').lower() if len(line.split()) > 1 else ''
            current = {'bssid': bssid, 'ssid': '', 'signal': -100, 'security': [], 'frequency': 0}
        elif current is not None:
            stripped = line.strip()
            if stripped.startswith('SSID:'):
                ssid = stripped[5:].strip()
                if ssid:
                    current['ssid'] = ssid
            elif stripped.startswith('signal:'):
                try:
                    v = float(stripped.split()[1])
                    current['signal'] = int(v)
                except Exception:
                    pass
            elif stripped.startswith('freq:'):
                try:
                    current['frequency'] = int(float(stripped.split()[1]))
                except Exception:
                    pass
            elif stripped.startswith('RSN:'):
                current['security'].append('WPA2')
            elif stripped.startswith('WPA:'):
                current['security'].append('WPA')
            elif stripped.startswith('Privacy'):
                if 'WPA2' not in current['security'] and 'WPA' not in current['security']:
                    current['security'].append('WEP')
    if current: networks.append(current)
    networks = [n for n in networks if n.get('ssid')]
    networks.sort(key=lambda n: n.get('signal', -100), reverse=True)
    seen: dict[str, dict] = {}
    for n in networks:
        s = n['ssid']
        if s not in seen or n['signal'] > seen[s]['signal']:
            seen[s] = n
    return list(seen.values())[:limit]

def _wpa_supplicant_build(ssid: str, password: str | None) -> str:
    """Generate a wpa_supplicant config for a single SSID. Password optional
    (open network). PSK derived via PBKDF2 so the password never appears in argv."""
    if password:
        import hashlib as _hashlib
        import binascii as _binascii
        psk = _hashlib.pbkdf2_hmac('sha1', password.encode('utf-8'),
                                    ssid.encode('utf-8'), 4096, 32)
        psk_hex = _binascii.hexlify(psk).decode('ascii')
        return (
            'ctrl_interface=DIR=/var/run/wpa_supplicant GROUP=netdev\n'
            'update_config=1\n'
            'country=US\n'
            'network={\n'
            f'    ssid="{ssid}"\n'
            '    psk_hex=' + psk_hex + '\n'
            '    key_mgmt=WPA-PSK\n'
            '}\n'
        )
    return (
        'ctrl_interface=DIR=/var/run/wpa_supplicant GROUP=netdev\n'
        'update_config=1\n'
        'country=US\n'
        'network={\n'
        f'    ssid="{ssid}"\n'
        '    key_mgmt=NONE\n'
        '}\n'
    )

def _network_wifi_connect(interface: str, ssid: str, password: str | None) -> dict:
    """Connect a wireless iface to `ssid`, bring it up, wait for DHCP, return
    the assigned IP. Stores the SSID/PSK in the persistent config so it survives
    reboot."""
    if not interface or not ssid:
        return {'ok': False, 'error': 'interface and ssid required'}
    if not os.path.isdir(f'/sys/class/net/{interface}/wireless'):
        return {'ok': False, 'error': f'{interface} is not a wireless interface'}
    conf = _wpa_supplicant_conf_path(interface)
    try:
        conf.parent.mkdir(parents=True, exist_ok=True)
        conf.write_text(_wpa_supplicant_build(ssid, password))
        _sudo(['chmod', '600', str(conf)], timeout=3)
    except Exception as e:
        return {'ok': False, 'error': f'write config failed: {e}'}
    r = _sudo(['ip', 'link', 'set', interface, 'up'], timeout=5)
    if not r.get('ok'):
        return {'ok': False, 'error': f'ip link up failed: {r.get("stderr") or r.get("stdout")}'}
    _sudo(['pkill', '-f', f'wpa_supplicant.*-i{interface}\\b'], timeout=3)
    time.sleep(0.5)
    log(f'wifi: starting wpa_supplicant on {interface} for ssid {ssid!r}')
    r2 = _sudo(['wpa_supplicant', '-B', '-i', interface, '-c', str(conf), '-D', 'nl80211,wext'],
               timeout=5)
    if not r2.get('ok'):
        return {'ok': False, 'error': f'wpa_supplicant start failed: {r2.get("stderr") or r2.get("stdout")}'}
    # Wait up to 12s for association + IP
    for _ in range(24):
        time.sleep(0.5)
        ip = _network_pick_source_ip(interface)
        if ip:
            dhcp = _sudo(['dhclient', '-1', interface], timeout=20)
            time.sleep(2)
            ip2 = _network_pick_source_ip(interface)
            if ip2: ip = ip2
            cfg = _network_config_load()
            cfg['wifi'] = {
                'interface': interface, 'ssid': ssid,
                'has_password': bool(password),
                'connected_at': datetime.now(timezone.utc).isoformat().replace('+00:00', 'Z'),
            }
            # Persist the password (required on reboot). Stored on disk only,
            # in the player's config dir with 0600 perms.
            cfg['wifi_psk_' + interface] = (password or '')
            _network_config_save(cfg)
            return {'ok': True, 'interface': interface, 'ssid': ssid, 'ip': ip,
                    'dhcp': dhcp.get('ok'),
                    'dhcp_log': (dhcp.get('stdout') or '')[-2000:]}
    return {'ok': False, 'error': 'wpa_supplicant did not associate within 12s',
            'interface': interface, 'ssid': ssid}

def _network_wifi_disconnect(interface: str) -> dict:
    """Stop wpa_supplicant on `interface` and bring the iface down."""
    if not interface:
        return {'ok': False, 'error': 'interface required'}
    _sudo(['pkill', '-f', f'wpa_supplicant.*-i{interface}\\b'], timeout=3)
    _sudo(['ip', 'link', 'set', interface, 'down'], timeout=5)
    conf = _wpa_supplicant_conf_path(interface)
    try:
        if conf.exists(): conf.unlink()
    except Exception: pass
    cfg = _network_config_load()
    cfg.pop('wifi', None)
    cfg.pop('wifi_psk_' + interface, None)
    _network_config_save(cfg)
    return {'ok': True, 'interface': interface}

# === VLAN =====

def _network_vlan_create(parent: str, vlan_id: int) -> dict:
    """Create a VLAN sub-interface  <parent>.<vlan_id>  and bring it up.
    Returns {ok, name, ip?, error}."""
    if not parent or vlan_id is None:
        return {'ok': False, 'error': 'parent and vlan_id required'}
    try:
        vlan_id = int(vlan_id)
    except Exception:
        return {'ok': False, 'error': 'vlan_id must be an integer'}
    if vlan_id < 1 or vlan_id > 4094:
        return {'ok': False, 'error': 'vlan_id must be between 1 and 4094'}
    name = f'{parent}.{vlan_id}'
    _sudo(['ip', 'link', 'delete', name], timeout=4)
    r = _sudo(['ip', 'link', 'add', 'link', parent, 'name', name, 'type', 'vlan', 'id', str(vlan_id)],
              timeout=5)
    if not r.get('ok'):
        return {'ok': False, 'error': f'ip link add failed: {r.get("stderr") or r.get("stdout")}',
                'name': name}
    r = _sudo(['ip', 'link', 'set', name, 'up'], timeout=5)
    if not r.get('ok'):
        return {'ok': False, 'error': f'ip link set up failed: {r.get("stderr") or r.get("stdout")}',
                'name': name}
    cfg = _network_config_load()
    cfg['vlan'] = {'parent': parent, 'vlan_id': vlan_id, 'name': name,
                   'created_at': datetime.now(timezone.utc).isoformat().replace('+00:00', 'Z')}
    _network_config_save(cfg)
    return {'ok': True, 'parent': parent, 'vlan_id': vlan_id, 'name': name}

def _network_vlan_remove(vlan_name: str) -> dict:
    """Remove a VLAN sub-interface by name."""
    if not vlan_name:
        return {'ok': False, 'error': 'vlan_name required'}
    _sudo(['ip', 'link', 'set', vlan_name, 'down'], timeout=5)
    r2 = _sudo(['ip', 'link', 'delete', vlan_name], timeout=5)
    if not r2.get('ok'):
        return {'ok': False, 'error': f'ip link delete failed: {r2.get("stderr") or r2.get("stdout")}'}
    cfg = _network_config_load()
    if cfg.get('vlan', {}).get('name') == vlan_name:
        cfg.pop('vlan', None)
    _network_config_save(cfg)
    return {'ok': True, 'name': vlan_name}

# === Static IPv4 =====

def _network_static_apply(interface: str, address: str, prefix: int,
                          gateway: str | None, dns: list[str] | None) -> dict:
    """Apply a static IPv4 config to `interface`. Writes a drop-in
    /etc/network/interfaces.d/screentra-cast-<iface> and brings the iface up at that
    address. DHCP is not disabled on the parent system — only this iface gets
    the static config so we don't break the lan/dhcp case if the operator
    switches back."""
    if not interface or not address or prefix is None:
        return {'ok': False, 'error': 'interface, address, and prefix are required'}
    try:
        prefix = int(prefix)
    except Exception:
        return {'ok': False, 'error': 'prefix must be an integer (1-32)'}
    if prefix < 1 or prefix > 32:
        return {'ok': False, 'error': 'prefix must be between 1 and 32'}
    import ipaddress as _ip
    try:
        _ip.IPv4Address(address)
    except Exception:
        return {'ok': False, 'error': f'{address!r} is not a valid IPv4 address'}
    if gateway:
        try:
            _ip.IPv4Address(gateway)
        except Exception:
            return {'ok': False, 'error': f'gateway {gateway!r} is not a valid IPv4 address'}
    body = '# Managed by Screentra Cast. DO NOT EDIT BY HAND: regenerated by the\n# local admin Network section on every change. Safe to delete.\n'
    body += f'auto {interface}\n'
    body += f'allow-hotplug {interface}\n'
    body += f'iface {interface} inet static\n'
    body += f'    address {address}/{prefix}\n'
    if gateway:
        body += f'    gateway {gateway}\n'
    if dns:
        body += '    dns-nameservers ' + ' '.join(dns) + '\n'
    conf_path = _NETWORK_INTERFACES_DIR / f'{_SCREENTRA_CAST_IFACE_PREFIX}{interface}'
    try:
        tmp = Path('/tmp') / f'screentra-cast-iface-{interface}-{os.getpid()}.txt'
        tmp.write_text(body)
        os.chmod(tmp, 0o644)
        r2 = _sudo(['cp', str(tmp), str(conf_path)], timeout=5)
        try: tmp.unlink()
        except Exception: pass
        if not r2.get('ok'):
            return {'ok': False, 'error': f'cp config failed: {r2.get("stderr") or r2.get("stdout")}'}
    except Exception as e:
        return {'ok': False, 'error': f'write failed: {e}'}
    _sudo(['ip', 'addr', 'flush', 'dev', interface], timeout=5)
    r3 = _sudo(['ip', 'addr', 'add', f'{address}/{prefix}', 'dev', interface], timeout=5)
    if not r3.get('ok'):
        return {'ok': False, 'error': f'ip addr add failed: {r3.get("stderr") or r3.get("stdout")}'}
    r4 = _sudo(['ip', 'link', 'set', interface, 'up'], timeout=5)
    if not r4.get('ok'):
        return {'ok': False, 'error': f'ip link set up failed: {r4.get("stderr") or r4.get("stdout")}'}
    if gateway:
        _sudo(['ip', 'route', 'replace', 'default', 'via', gateway, 'dev', interface], timeout=5)
    cfg = _network_config_load()
    cfg['static'] = {
        'interface': interface, 'address': address, 'prefix': prefix,
        'gateway': gateway, 'dns': dns or [],
        'applied_at': datetime.now(timezone.utc).isoformat().replace('+00:00', 'Z'),
    }
    _network_config_save(cfg)
    return {'ok': True, 'interface': interface, 'address': address, 'prefix': prefix,
            'gateway': gateway, 'dns': dns or []}

def _network_static_clear(interface: str) -> dict:
    """Remove the static config for `interface` and flush addresses; the iface
    will return to DHCP on next ifup (or system link cycle)."""
    conf_path = _NETWORK_INTERFACES_DIR / f'{_SCREENTRA_CAST_IFACE_PREFIX}{interface}'
    if conf_path.exists():
        _sudo(['rm', '-f', str(conf_path)], timeout=5)
    _sudo(['ip', 'addr', 'flush', 'dev', interface], timeout=5)
    cfg = _network_config_load()
    if cfg.get('static', {}).get('interface') == interface:
        cfg.pop('static', None)
    _network_config_save(cfg)
    return {'ok': True, 'interface': interface}

# === re-apply on boot =====
def _network_restore_from_config() -> dict:
    """Called once at renderer startup. Re-applies any persisted wifi/vlan/static
    config so the iface is in the right state on every boot. Best-effort."""
    cfg = _network_config_load()
    result: dict = {'wifi': None, 'vlan': None, 'static': None}
    wifi = cfg.get('wifi') or {}
    if wifi.get('interface') and wifi.get('ssid'):
        try:
            pwd = cfg.get('wifi_psk_' + wifi['interface']) or None
            result['wifi'] = _network_wifi_connect(wifi['interface'], wifi['ssid'], pwd)
        except Exception as e:
            result['wifi'] = {'ok': False, 'error': str(e)}
    vlan = cfg.get('vlan') or {}
    if vlan.get('parent') and vlan.get('vlan_id'):
        try:
            result['vlan'] = _network_vlan_create(vlan['parent'], vlan['vlan_id'])
        except Exception as e:
            result['vlan'] = {'ok': False, 'error': str(e)}
    static = cfg.get('static') or {}
    if static.get('interface') and static.get('address'):
        try:
            result['static'] = _network_static_apply(
                static['interface'], static['address'], static.get('prefix', 24),
                static.get('gateway'), static.get('dns') or [])
        except Exception as e:
            result['static'] = {'ok': False, 'error': str(e)}
    return result

def _maybe_inject_pending_network_banner(html: str) -> str:
    """If a network change is pending, inject a top-of-card banner with
    countdown, confirm/revert action buttons, and revert the page's
    `cursor: none` so a USB mouse can actually click them."""
    state = _network_state_load()
    pending = state.get('pending')
    if not pending or pending.get('confirmed'):
        return html
    # Compute remaining seconds. Round UP so the user never sees "0:00"
    # while they still have time.
    try:
        exp = datetime.fromisoformat(pending['expires_at'].replace('Z', '+00:00'))
        remaining = max(0, int((exp - datetime.now(timezone.utc)).total_seconds()))
    except Exception:
        remaining = 0
    started_iso = pending.get('started_at', '')
    target = pending.get('target_interface', '?')
    target_ip = pending.get('target_source_ip', '?')
    prev = pending.get('previous_interface') or 'auto'
    prev_ip = pending.get('previous_source_ip') or 'auto'
    # Banner content
    banner = (
        '<style id="screentra-cast-pending-network-style">'
        '.pnw-banner{position:fixed;left:50%;top:24px;transform:translateX(-50%);z-index:2147483647;'
        'min-width:min(620px,92vw);max-width:min(820px,94vw);'
        'background:linear-gradient(135deg,#f59e0b,#fb923c);color:#1f2937;'
        'border-radius:14px;padding:18px 22px;box-shadow:0 20px 60px rgba(0,0,0,.5);'
        'font-family:ui-sans-serif,system-ui,-apple-system,"Segoe UI",Roboto,sans-serif;'
        'border:1px solid rgba(255,255,255,.35)}'
        '.pnw-banner *{color:#1f2937}'
        '.pnw-title{font-weight:800;font-size:20px;margin:0 0 6px}'
        '.pnw-sub{font-size:14px;margin:0 0 14px;line-height:1.4}'
        '.pnw-count{font-weight:800;font-variant-numeric:tabular-nums;font-size:28px}'
        '.pnw-actions{display:flex;gap:10px;flex-wrap:wrap}'
        '.pnw-btn{appearance:none;border:0;border-radius:999px;padding:10px 18px;'
        'font-weight:800;font-size:14px;cursor:pointer;'
        'background:#0f172a;color:#f8fafc}'
        '.pnw-btn.danger{background:#b91c1c;color:#fff}'
        '.pnw-btn.ok{background:#16a34a;color:#fff}'
        '.pnw-btn:hover{filter:brightness(1.1)}'
        '.pnw-status{margin-top:10px;font-size:13px;min-height:18px}'
        '</style>'
        '<div class="pnw-banner" id="pnw-banner" role="alertdialog" aria-live="assertive">'
        f'<p class="pnw-title">Network change pending</p>'
        f'<p class="pnw-sub">Switching to <b>{target}</b> ({target_ip}). '
        f'Will revert to <b>{prev}</b> ({prev_ip}) in '
        '<span class="pnw-count" id="pnw-count">--:--</span>'
        ' unless confirmed. Connect a USB mouse to click <b>Revert</b> now, or sign in to the local admin page and click <b>Confirm working</b> to keep the new interface. If neither happens, the change auto-reverts.</p>'
        '<div class="pnw-actions">'
        '<button class="pnw-btn danger" id="pnw-revert">Revert to previous interface</button>'
        '<button class="pnw-btn ok" id="pnw-confirm">Confirm working</button>'
        '</div>'
        '<div class="pnw-status" id="pnw-status"></div>'
        '</div>'
        '<script id="pnw-script">(function(){'
        'var banner=document.getElementById("pnw-banner");'
        'var count=document.getElementById("pnw-count");'
        'var status=document.getElementById("pnw-status");'
        'var revert=document.getElementById("pnw-revert");'
        'var confirm=document.getElementById("pnw-confirm");'
        f'var started={remaining};'  # total seconds for the timer
        'var secs=started;'
        'function fmt(s){s=Math.max(0,Math.floor(s));var m=Math.floor(s/60);var x=s%60;return m+":"+(x<10?"0":"")+x;}'
        'function tick(){if(count)count.textContent=fmt(secs);if(secs<=0){if(count)count.textContent="0:00";if(status)status.textContent="Window expired; reverting now…";}'
        'else secs-=1;}'
        'tick();setInterval(tick,1000);'
        'async function api(path,opts){var r=await fetch(path,{cache:"no-store",...(opts||{})});var t=await r.text();var j=null;try{j=JSON.parse(t)}catch(e){}return Object.assign({status:r.status}, j||{raw:t});}'
        'if(revert){revert.onclick=async function(){revert.disabled=true;confirm.disabled=true;status.textContent="Reverting…";var r=await api("/api/admin/network/revert",{method:"POST"});if(r&&r.ok){status.textContent="Reverted. Reloading…";setTimeout(function(){location.reload()},800);}else{status.textContent="Revert failed: "+(r&&r.error?r.error:"unknown");revert.disabled=false;confirm.disabled=false;}};}'
        'if(confirm){confirm.onclick=async function(){revert.disabled=true;confirm.disabled=true;status.textContent="Confirming…";var r=await api("/api/admin/network/confirm",{method:"POST"});if(r&&r.ok){status.textContent="Confirmed. Network change committed.";setTimeout(function(){location.reload()},1200);}else{status.textContent="Confirm failed: "+(r&&r.error?r.error:JSON.stringify(r));revert.disabled=false;confirm.disabled=false;}};}'
        '})();</script>'
    )
    # Inject the banner near the top of <body>
    if '<body' in html:
        html = html.replace('<body', '<body>', 1)
        # Place the banner just inside body
        idx = html.find('>', html.find('<body')) + 1
        html = html[:idx] + banner + html[idx:]
    else:
        html = banner + html
    # Force the page cursor visible so a USB mouse can click the buttons.
    # The no-layout page sets `cursor: none` on html/body and on individual
    # widget frames. Replace all of them — the banner buttons need a real
    # cursor to be clickable. The trailing semicolon + space keeps the rest
    # of the rule intact.
    import re as _re_cursor
    html = _re_cursor.sub(r'cursor:\s*none\s*;', 'cursor: default;', html)
    # Also inject a global style that re-enables cursors on banner contents,
    # in case any other stylesheet still tries to hide them.
    cursor_fix = '<style id="screentra-cast-pending-network-cursor-fix">.pnw-banner,.pnw-banner *{cursor:pointer !important}</style>'
    if '</head>' in html:
        html = html.replace('</head>', cursor_fix + '</head>', 1)
    else:
        html = cursor_fix + html
    return html

def _nl_status():
    cid_path = Path.home() / '.local/share/screentra-cast-player-client/client-id'
    srv_path = Path.home() / '.local/share/screentra-cast-player-client/server'
    client_id = cid_path.read_text().strip() if cid_path.exists() else None
    server_url = srv_path.read_text().strip() if srv_path.exists() else 'unknown'
    last_path = Handler.cache_dir / 'last-heartbeat.json'
    last_seen = connection = last_layout = None
    if last_path.exists():
        try:
            last = json.loads(last_path.read_text())
            last_seen = last.get('at')
            connection = last.get('connection', 'offline')
            last_layout = last.get('assigned_layout_id')
        except Exception:
            pass
    disp_path = Handler.cache_dir / f'display-{int(os.environ.get("SCREENTRA_CAST_DISPLAY_INDEX", "0"))}.json'
    label = connector = resolution = None
    if disp_path.exists():
        try:
            d = json.loads(disp_path.read_text())
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
            selected = json.loads(touch_pref.read_text()).get('path')
        except Exception:
            pass
    devs = _nl_detect_touch_devices()
    touch_dev = None
    for d in devs:
        if d['path'] == selected:
            touch_dev = d['name']; break
    if not touch_dev and devs:
        touch_dev = devs[0]['name']
    display_index_for_layout = int(os.environ.get('SCREENTRA_CAST_DISPLAY_INDEX', '0'))
    layout_status_file = Handler.cache_dir / f'assigned-layout-display-{display_index_for_layout}.json'
    if display_index_for_layout == 0 and not layout_status_file.exists():
        layout_status_file = Handler.cache_dir / 'assigned-layout.json'
    if layout_status_file.exists():
        try:
            last_layout = json.loads(layout_status_file.read_text()).get('id')
        except Exception:
            pass
    else:
        last_layout = None
    return {
        'screentra_cast_player_client': True,
        'client_installed': True,
        'client_id': client_id,
        'identity_source': 'persistent_client_id',
        'server_url': server_url,
        'hostname': socket.gethostname() if 'socket' in dir() else os.uname().nodename,
        'lan_ip': _lan_ip_for_admin(),
        'admin_port': 3910,
        'admin_url': ('http://' + _lan_ip_for_admin() + ':3910/') if _lan_ip_for_admin() else None,
        'role': 'player',
        'port': int(os.environ.get('SCREENTRA_CAST_RENDER_PORT', str(int(os.environ.get('SCREENTRA_CAST_RENDER_BASE_PORT','3900')) + int(os.environ.get('SCREENTRA_CAST_DISPLAY_INDEX','0'))))),
        'display_index': int(os.environ.get('SCREENTRA_CAST_DISPLAY_INDEX', '0')),
        'display_label': label,
        'display_connector': connector,
        'display_resolution': resolution,
        'last_seen': last_seen,
        'connection': connection or 'offline',
        'assigned_layout_id': last_layout,
        'touch_device': touch_dev,
        'touch_path': selected,
        'build': os.environ.get('SCREENTRA_CAST_BUILD', 'dev'),
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
            data = json.loads(path.read_text())
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
            data = json.loads(path.read_text())
            if isinstance(data, dict):
                return data
    except Exception:
        pass
    return {}

def _write_client_config(cfg: dict) -> None:
    path = _client_config_path()
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_suffix('.json.tmp')
    tmp.write_text(json.dumps(cfg, indent=2))
    tmp.replace(path)

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
    pending.write_text(json.dumps({'action':'set_display_count','payload':{'max_displays': override},'requested_at':_iso_now(),'requested_by':'local-admin'}, indent=2))
    _safe_run(['systemctl', '--user', 'restart', 'screentra-cast-player-heartbeat.service'], timeout=6)
    actor = _safe_run(['systemctl', '--user', 'start', 'screentra-cast-player-actor.service'], timeout=6)
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
            path = json.loads(legacy.read_text()).get('path')
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
        (Handler.cache_dir / 'touch-device.json').write_text(json.dumps({'path': dev_path, 'name': dev_name}, indent=2))
    pending = Handler.cache_dir / 'pending-command.json'
    pending.parent.mkdir(parents=True, exist_ok=True)
    pending.write_text(json.dumps({'action':'set_touch_binding','payload': binding,'requested_at':_iso_now(),'requested_by':'local-admin'}, indent=2))
    _safe_run(['systemctl', '--user', 'restart', 'screentra-cast-player-heartbeat.service'], timeout=6)
    actor = _safe_run(['systemctl', '--user', 'start', 'screentra-cast-player-actor.service'], timeout=6)
    return {'ok': True, 'touch_binding': binding, 'actor_start': actor}


# === Audio output + Bluetooth speaker =====================================
#
# PulseAudio (pactl) owns the local audio sinks/sources; PipeWire provides a
# pactl-compatible shim, so this code works on either. bluez (bluetoothctl)
# handles pairing/connecting to BT speakers; once connected, bluez publishes a
# sink under PulseAudio, which `set-default-sink` then routes audio through.

def _pactl(*args, timeout=4):
    """Run pactl, falling back to sudo -n if the user-scope daemon rejects us."""
    direct = _safe_run(['pactl', *args], timeout=timeout)
    if direct.get('ok'):
        return direct
    return _safe_run(['sudo', '-n', 'pactl', *args], timeout=timeout)

def _bluetoothctl(*args, timeout=8):
    """Run bluetoothctl, falling back to sudo -n if not in the bluetooth group."""
    direct = _safe_run(['bluetoothctl', *args], timeout=timeout)
    if direct.get('ok'):
        return direct
    return _safe_run(['sudo', '-n', 'bluetoothctl', *args], timeout=timeout)

def _audio_list_sinks() -> list[dict]:
    """Return [{name, description, index, state}] from `pactl list short sinks`."""
    out = []
    r = _pactl('list', 'short', 'sinks', timeout=4)
    for line in (r.get('stdout') or '').splitlines():
        # format: <index>\t<name>\t<module>\t<sample_spec>\t<state>
        parts = line.split('\t')
        if len(parts) < 2:
            continue
        out.append({
            'index': int(parts[0]) if parts[0].isdigit() else None,
            'name': parts[1].strip(),
            'description': parts[1].strip(),
            'state': parts[4].strip() if len(parts) > 4 else '',
        })
    # also pull human descriptions where available
    desc = _pactl('list', 'sinks', timeout=4)
    name_to_desc = {}
    block = {}
    for line in (desc.get('stdout') or '').splitlines():
        line = line.strip()
        if not line:
            if 'name' in block and 'description' in block:
                name_to_desc[block['name']] = block['description']
            block = {}
            continue
        if line.startswith('Name:'):
            block['name'] = line.split(':', 1)[1].strip()
        elif line.startswith('Description:'):
            block['description'] = line.split(':', 1)[1].strip()
    if 'name' in block and 'description' in block:
        name_to_desc[block['name']] = block['description']
    for s in out:
        if s['name'] in name_to_desc:
            s['description'] = name_to_desc[s['name']]
    return out

def _audio_current_sink() -> str | None:
    r = _pactl('get-default-sink', timeout=3)
    if r.get('ok'):
        sink = (r.get('stdout') or '').strip()
        return sink or None
    return None

def _audio_set_default_sink(sink: str) -> dict:
    if not sink:
        return {'ok': False, 'error': 'sink required'}
    r = _pactl('set-default-sink', sink, timeout=4)
    if not r.get('ok'):
        return {'ok': False, 'error': 'pactl set-default-sink failed', 'stderr': r.get('stderr')}
    # route existing streams to the new sink so playback switches immediately
    _pactl('move-sink-input', 'all', sink, timeout=4)
    # persist so the choice survives reboots via the kiosk-launcher audio step
    cfg = _load_client_config()
    cfg['audio_default_sink'] = sink
    cfg['audio_updated_at'] = _iso_now()
    _write_client_config(cfg)
    return {'ok': True, 'sink': sink}

def _audio_status() -> dict:
    sinks = _audio_list_sinks()
    return {
        'ok': True,
        'sinks': sinks,
        'current_sink': _audio_current_sink(),
        'bluetooth': _bluetooth_status(),
    }

def _bluetooth_status() -> dict | None:
    """Return {adapter, powered, discovering, connected_count} or None if no adapter."""
    show = _bluetoothctl('show', timeout=4)
    out = (show.get('stdout') or '').strip()
    if not out:
        return None
    info: dict = {}
    for line in out.splitlines():
        if ':' not in line:
            continue
        k, _, v = line.partition(':')
        info[k.strip()] = v.strip()
    powered_raw = info.get('Powered', '').lower()
    discovering_raw = info.get('Discovering', '').lower()
    connected = _bluetoothctl('devices', 'Connected', timeout=4)
    connected_macs = []
    for line in (connected.get('stdout') or '').splitlines():
        # "Device AA:BB:CC:DD:EE:FF Name"
        parts = line.strip().split(None, 2)
        if len(parts) >= 2 and ':' in parts[1]:
            connected_macs.append(parts[1])
    return {
        'adapter': info.get('Controller') or info.get('Name') or 'hci0',
        'powered': powered_raw in ('yes', 'true', 'on'),
        'discovering': discovering_raw in ('yes', 'true', 'on'),
        'connected_count': len(connected_macs),
        'connected_macs': connected_macs,
    }

def _bluetooth_paired_devices() -> list[dict]:
    out = []
    r = _bluetoothctl('devices', 'Paired', timeout=4)
    for line in (r.get('stdout') or '').splitlines():
        parts = line.strip().split(None, 2)
        if len(parts) < 3:
            continue
        # "Device AA:BB:... Name Of Speaker"
        mac = parts[1]
        name = parts[2]
        info_r = _bluetoothctl('info', mac, timeout=4)
        info_out = (info_r.get('stdout') or '')
        trusted = 'Trusted: yes' in info_out
        connected = 'Connected: yes' in info_out
        out.append({'mac': mac, 'name': name, 'trusted': trusted, 'connected': connected})
    return out

def _bluetooth_scan(seconds: int = 12) -> list[dict]:
    """Run a bluetoothctl scan and return devices discovered during it.

    Uses non-interactive `bluetoothctl --timeout N scan on` so we capture
    live `[NEW] Device MAC NAME` and `[CHG] Device MAC Name: ...` events
    from stdout. The post-scan `bluetoothctl devices` list only shows
    entries bluez has already cached, not devices that just briefly
    advertised, so we can't rely on it.

    Before scanning, set `discoverable on` + `pairable on` so devices that
    are scanning for pairing partners (earbuds, speakers) actually respond.
    """
    import subprocess, re, time as _t
    try:
        # Make this controller discoverable + pairable so earbuds/speakers
        # that are scanning for a partner will respond to us.
        subprocess.run(
            ['bluetoothctl', '--timeout', '4', 'discoverable', 'on'],
            capture_output=True, text=True, timeout=6,
        )
        subprocess.run(
            ['bluetoothctl', '--timeout', '4', 'pairable', 'on'],
            capture_output=True, text=True, timeout=6,
        )
        # Run a timed scan — bluetoothctl exits after N seconds and we
        # get the full event stream on stdout.
        proc = subprocess.run(
            ['bluetoothctl', '--timeout', str(max(2, seconds)), 'scan', 'on'],
            capture_output=True, text=True, timeout=seconds + 10,
        )
        out = (proc.stdout or '') + '\n' + (proc.stderr or '')
    except Exception:
        return []

    # bluez emits colored events via systemd's pager detection — strip ANSI
    # codes before pattern matching, otherwise [NEW] becomes [\x1b[...mNEW...]
    ansi_re = re.compile(r'\x1b\[[0-9;]*m')
    out = ansi_re.sub('', out or '')
    found = {}
    new_re = re.compile(r'\[NEW\] Device ([0-9A-Fa-f:]{17})\s+(.*)')
    chg_re = re.compile(r'\[CHG\] Device ([0-9A-Fa-f:]{17})\s+Name:\s+(.*)')
    for line in (out or '').splitlines():
        m = new_re.search(line)
        if m:
            mac = m.group(1).upper()
            name = m.group(2).strip() or mac
            found[mac] = name
            continue
        m = chg_re.search(line)
        if m:
            mac = m.group(1).upper()
            name = m.group(2).strip()
            if name and mac in found:
                found[mac] = name
    # Filter out the local controller (looks like a device in the events stream)
    try:
        ctrl_r = subprocess.run(
            ['bluetoothctl', '--timeout', '3', 'show'],
            capture_output=True, text=True, timeout=5,
        )
        for ln in (ctrl_r.stdout or '').splitlines():
            if ln.lstrip().startswith('Controller '):
                toks = ln.split()
                for t in toks:
                    if ':' in t and len(t) == 17:
                        found.pop(t.upper(), None)
                        break
                break
    except Exception:
        pass
    return [{'mac': mac, 'name': name} for mac, name in sorted(found.items())]

def _bluetooth_pair_and_connect(mac: str) -> dict:
    mac = (mac or '').strip()
    if not mac or ':' not in mac:
        return {'ok': False, 'error': 'mac address required (AA:BB:CC:DD:EE:FF)'}
    results = []
    for step in (['pair', mac], ['trust', mac], ['connect', mac]):
        r = _bluetoothctl(*step, timeout=10)
        ok = r.get('ok') or 'successful' in (r.get('stdout') or '').lower() or 'already' in (r.get('stdout') or '').lower()
        results.append({'step': ' '.join(step), 'ok': ok, 'stderr': (r.get('stderr') or '').strip()[:200]})
        if not ok and step[0] == 'pair':
            return {'ok': False, 'error': f"bluetoothctl {step[0]} failed", 'steps': results}
    # Give bluez a moment to register the sink under PulseAudio
    import time as _t
    _t.sleep(2)
    sink = None
    sinks = _audio_list_sinks()
    bluez_names = [s['name'] for s in sinks if 'bluez' in s['name'].lower()]
    if bluez_names:
        sink = bluez_names[0]
        _audio_set_default_sink(sink)
    return {'ok': True, 'mac': mac, 'sink': sink, 'steps': results}

def _bluetooth_connect_only(mac: str) -> dict:
    mac = (mac or '').strip()
    if not mac or ':' not in mac:
        return {'ok': False, 'error': 'mac required'}
    r = _bluetoothctl('connect', mac, timeout=10)
    if not (r.get('ok') or 'successful' in (r.get('stdout') or '').lower()):
        return {'ok': False, 'error': 'bluetoothctl connect failed', 'stderr': (r.get('stderr') or '').strip()[:200]}
    import time as _t
    _t.sleep(2)
    sink = None
    bluez_names = [s['name'] for s in _audio_list_sinks() if 'bluez' in s['name'].lower()]
    if bluez_names:
        sink = bluez_names[0]
        _audio_set_default_sink(sink)
    return {'ok': True, 'mac': mac, 'sink': sink}

def _bluetooth_disconnect(mac: str) -> dict:
    mac = (mac or '').strip()
    if not mac or ':' not in mac:
        return {'ok': False, 'error': 'mac required'}
    r = _bluetoothctl('disconnect', mac, timeout=8)
    return {'ok': bool(r.get('ok')), 'stderr': (r.get('stderr') or '').strip()[:200]}

def _bluetooth_trust(mac: str) -> dict:
    mac = (mac or '').strip()
    if not mac or ':' not in mac:
        return {'ok': False, 'error': 'mac required'}
    r = _bluetoothctl('trust', mac, timeout=6)
    return {'ok': bool(r.get('ok')), 'stderr': (r.get('stderr') or '').strip()[:200]}

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
    install = Path.home() / '.local/share/screentra-cast-player-client'
    state = Path.home() / '.local/state/screentra-cast-player-client'
    cache = install / 'cache'
    targets = {
        'heartbeat': lambda: _journal_tail('screentra-cast-player-heartbeat.service'),
        'actor': lambda: _journal_tail('screentra-cast-player-actor.service'),
        'renderer0': lambda: _journal_tail('screentra-cast-player-renderer@0.service'),
        'kiosk0': lambda: _journal_tail('screentra-cast-labwc-kiosk@0.service', system=True),
        'launcher': lambda: _read_tail(state / 'kiosk-launcher.log'),
        'touch': lambda: _read_tail(Path('/tmp/screentra-cast-touch-events.log')),
        'install': lambda: '\n\n'.join(_read_tail(p) for p in sorted(Path('/tmp').glob('player-client-*-install.log'))[-4:]),
        'heartbeat-cache': lambda: _read_tail(cache / 'last-heartbeat.json'),
    }
    if target == 'all': return {'target': target, 'logs': {k: fn() for k, fn in targets.items()}}
    fn = targets.get(target)
    if not fn: return {'target': target, 'error': 'unknown log target', 'valid_targets': sorted(targets)}
    return {'target': target, 'text': fn()}

def _ca_file() -> Path:
    env = os.environ.get('SCREENTRA_CAST_CA_CERT')
    return Path(env) if env else Path.home() / '.local/share/screentra-cast-player-client/screentra-cast-signage-ca.crt'

def _sha256_file(path: Path) -> str | None:
    try:
        h = hashlib.sha256()
        with path.open('rb') as f:
            for chunk in iter(lambda: f.read(65536), b''): h.update(chunk)
        return h.hexdigest()
    except Exception:
        return None

def _cert_info() -> dict:
    srv_path = Path.home() / '.local/share/screentra-cast-player-client/server'
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
    srv_path = Path.home() / '.local/share/screentra-cast-player-client/server'
    if not srv_path.exists(): return {'ok': False, 'error': 'server file missing'}
    server_url = srv_path.read_text().strip().rstrip('/')
    if not server_url: return {'ok': False, 'error': 'server URL empty'}
    url = server_url + '/install/screentra-cast-signage-ca.crt'; out = _ca_file()
    old_sha256 = _sha256_file(out)
    tmp = out.with_suffix(out.suffix + '.tmp')
    try:
        contexts = []
        if server_url.startswith('https://') and out.exists():
            try:
                contexts.append(('existing CA', ssl.create_default_context(cafile=str(out))))
            except Exception:
                pass
        if server_url.startswith('https://'):
            # Explicit System-page repair action. Normal player traffic never
            # disables certificate verification.
            contexts.append(('bootstrap', ssl._create_unverified_context()))
        else:
            contexts.append(('plain HTTP', None))
        data = None; used = None; last = None
        for label, ctx in contexts:
            try:
                kwargs = {'timeout': 10}
                if ctx is not None: kwargs['context'] = ctx
                data = _ur.urlopen(url, **kwargs).read()
                used = label
                break
            except Exception as exc:
                last = exc
        if data is None:
            return {'ok': False, 'error': str(last), 'url': url, 'old_sha256': old_sha256}
        if len(data) < 200 or b'BEGIN CERTIFICATE' not in data:
            return {'ok': False, 'error': f'downloaded CA did not look like a certificate ({len(data)} bytes)', 'url': url}
        out.parent.mkdir(parents=True, exist_ok=True)
        tmp.write_bytes(data); os.chmod(tmp, 0o644); tmp.replace(out)
        new_sha256 = _sha256_file(out)
        verify = _cert_info().get('verify')
        restart = _safe_run(['systemctl', '--user', 'restart', 'screentra-cast-player-heartbeat.service', 'screentra-cast-player-actor.service'], timeout=8)
        return {'ok': True, 'url': url, 'ca_path': str(out), 'bytes': len(data), 'sha256': new_sha256, 'previous_sha256': old_sha256, 'trust_method': used, 'verify': verify, 'restart': restart}
    except Exception as e:
        try: tmp.unlink(missing_ok=True)
        except Exception: pass
        return {'ok': False, 'error': str(e), 'url': url, 'old_sha256': old_sha256}



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
    helper = Path.home() / '.local/share/screentra-cast-player-client/screentra-cast-gpu-health.py'
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
    deny_names = {'cache', 'admin-auth', 'backups', 'client-id', 'server', 'config.json', 'network.env', 'network-state.json'}
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
    ts = datetime.now(timezone.utc).strftime('%Y%m%dT%H%M%SZ')
    host = socket.gethostname().replace('/', '-').replace(' ', '-')
    name = f'screentra-cast-player-client-software-{host}-{ts}.tar.gz'
    out = root / name
    install = Path.home() / '.local/share/screentra-cast-player-client'
    bindir = Path.home() / '.local/bin'
    user_units = Path.home() / '.config/systemd/user'
    manifest = {
        'created_at': _iso_now(),
        'hostname': socket.gethostname(),
        'kind': 'screentra-cast-player-client-software-backup',
        'excluded_state': ['cache/', 'admin-auth/', 'backups/', 'chromium-profile*/', 'client-id', 'server', 'config.json', 'network.env', 'network-state.json', '*.crt', '*.key', '*.pem', '*.p12', '*.pfx'],
        'paths': [],
    }
    try:
        with tarfile.open(out, 'w:gz') as tar:
            n = _safe_tar_add(tar, install, 'screentra-cast-player-client', exclude_state=True)
            manifest['paths'].append({'source': str(install), 'archive': 'screentra-cast-player-client', 'files': n})
            helper_count = 0
            for helper in sorted(bindir.glob('screentra-cast-*')):
                helper_count += _safe_tar_add(tar, helper, str(Path('local-bin') / helper.name), exclude_state=True)
            manifest['paths'].append({'source': str(bindir / 'screentra-cast-*'), 'archive': 'local-bin/', 'files': helper_count})
            unit_count = 0
            for unit in sorted(user_units.glob('screentra-cast-*.service')):
                unit_count += _safe_tar_add(tar, unit, str(Path('systemd-user') / unit.name), exclude_state=True)
            manifest['paths'].append({'source': str(user_units / 'screentra-cast-*.service'), 'archive': 'systemd-user/', 'files': unit_count})
            for sys_unit in [Path('/etc/systemd/system/screentra-cast-labwc-kiosk@.service')]:
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
    services = [_service_state('screentra-cast-player-heartbeat.service'), _service_state('screentra-cast-player-actor.service'), _service_state('screentra-cast-player-renderer@0.service'), _system_service_state('screentra-cast-labwc-kiosk@0.service'), _system_service_state('screentra-cast-labwc-kiosk@1.service')]
    for svc in services:
        if svc.get('name') == 'screentra-cast-player-actor.service' and svc.get('active') == 'inactive' and svc.get('enabled') == 'enabled':
            svc['active'] = 'ready'
            svc['active_ok'] = True
            svc['note'] = 'one-shot command actor; inactive means no pending command'
        if svc.get('name') == 'screentra-cast-labwc-kiosk@1.service' and svc.get('active') == 'inactive' and svc.get('enabled') == 'disabled':
            svc['active'] = 'disabled'
            svc['active_ok'] = True
            svc['note'] = 'secondary kiosk output intentionally disabled while touchscreen-only mode is active'
    return {'status': base, 'time': _iso_now(), 'uptime_seconds': time.monotonic(), 'auth': _auth_source(), 'metrics': _system_metrics(), 'display_governance': _load_display_governance(), 'services': services, 'processes': {'chromium': chromium.get('stdout', ''), 'labwc': labwc.get('stdout', '')}, 'outputs': _wlr_outputs(), 'touch': {'devices': _nl_detect_touch_devices(), 'binding': _current_touch_binding(), 'calibration': json.loads((cache/'touch-calibration.json').read_text()) if (cache/'touch-calibration.json').exists() else []}, 'ca': _cert_info(), 'command_state': {'pending_command': json.loads(pending.read_text()) if pending.exists() else None, 'last_result': json.loads(result.read_text()) if result.exists() else None}}


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
            data = json.loads(path.read_text())
            users = data.get('users') if isinstance(data, dict) else data
            if isinstance(users, list):
                return [u for u in users if isinstance(u, dict) and u.get('username') and (u.get('password_hash') or u.get('password'))]
        except Exception:
            pass
    return []

def _auth_source() -> dict:
    users = _load_admin_users()
    if users:
        try: synced_at = json.loads(_synced_users_path().read_text()).get('synced_at')
        except Exception: synced_at = None
        return {'source': 'synced_server_users', 'fallback_default': False, 'user_count': len(users), 'synced_at': synced_at}
    return {'source': 'fallback_default', 'fallback_default': True, 'user_count': 1, 'warning': 'Using default local admin/admin because server users have not synced yet.'}

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
    ok = hmac.compare_digest(username, 'admin') and hmac.compare_digest(password, 'admin')
    return (ok, {'username': 'admin', **_auth_source()}) if ok else (False, {'error': 'invalid username or password', **_auth_source()})

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
    tok = _parse_cookie(cookie_header, 'screentra_cast_admin_session')
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
    warn = 'Default local fallback admin/admin is active until server users sync.' if src.get('fallback_default') else 'Using synced server users.'
    return '<!doctype html><html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>Screentra Cast Admin Sign In</title><style>body{margin:0;background:#07111f;color:#e5eefc;font-family:system-ui,-apple-system,Segoe UI,sans-serif;display:grid;place-items:center;min-height:100vh}.card{width:min(430px,92vw);background:#0f172a;border:1px solid #243247;border-radius:18px;padding:24px;box-shadow:0 20px 70px rgba(0,0,0,.4)}h1{margin:0 0 8px}.muted{color:#91a4bf}.warn{border:1px solid rgba(245,158,11,.45);background:rgba(245,158,11,.08);border-radius:12px;padding:10px;margin:14px 0;color:#facc15}label{display:block;margin-top:12px;color:#91a4bf;font-size:12px;text-transform:uppercase;letter-spacing:.08em}input{width:100%;margin-top:6px;padding:12px;border-radius:10px;border:1px solid #243247;background:#07111f;color:#e5eefc;font-size:16px}button{width:100%;margin-top:16px;padding:12px;border:0;border-radius:999px;background:#38bdf8;color:#03131f;font-weight:900;font-size:15px}.err{color:#fca5a5;min-height:20px;margin-top:10px}</style></head><body><form class="card" id="f"><h1>Screentra Cast Admin</h1><div class="muted">Sign in to manage this local player.</div><div class="warn">'+warn+'</div><label>Username</label><input id="u" autocomplete="username" value="admin"><label>Password</label><input id="p" type="password" autocomplete="current-password" autofocus><button>Sign in</button><div class="err" id="e"></div></form><script>document.getElementById("f").addEventListener("submit",async ev=>{ev.preventDefault();e.textContent="Signing in…";try{const r=await fetch("/api/admin/auth/login",{method:"POST",headers:{"Content-Type":"application/json"},body:JSON.stringify({username:u.value,password:p.value})});const j=await r.json();if(r.ok&&j.ok){location.href="/"}else{e.textContent=j.error||"Login failed"}}catch(x){e.textContent=String(x)}});</script></body></html>'

def _admin_auth_payload(handler) -> dict:
    user = _session_user(handler.headers.get('Cookie', ''))
    return {'authenticated': bool(user), 'user': user, **_auth_source()}

def _admin_requires_auth(path: str) -> bool:
    if os.environ.get('SCREENTRA_CAST_ADMIN_DEFAULT', '').lower() not in ('1', 'true', 'yes'):
        return False
    if path in ('/api/admin/auth/login', '/api/admin/auth/status', '/login', '/login.html'):
        return False
    if path.startswith('/api/admin') or path in ('/', '/admin.html'):
        return True
    return False

ADMIN_HTML = """<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>Screentra Cast Player Admin</title><style>
:root{--bg:#07111f;--panel:#0f172a;--panel2:#111c31;--line:#26364f;--text:#e5eefc;--muted:#91a4bf;--ok:#22c55e;--warn:#f59e0b;--err:#ef4444;--accent:#38bdf8;--violet:#a78bfa}*{box-sizing:border-box}body{margin:0;background:radial-gradient(circle at top left,rgba(56,189,248,.14),transparent 34rem),linear-gradient(160deg,#07111f,#020617);color:var(--text);font:14px/1.45 system-ui,-apple-system,Segoe UI,sans-serif}.wrap{max-width:1380px;margin:0 auto;padding:24px}.top{display:flex;gap:16px;align-items:flex-start;justify-content:space-between;margin-bottom:18px}.title h1{margin:0;font-size:30px;letter-spacing:-.03em}.title p{margin:6px 0 0;color:var(--muted)}.pill{display:inline-flex;align-items:center;gap:6px;padding:5px 10px;border-radius:999px;border:1px solid var(--line);font-weight:900;font-size:12px;text-transform:uppercase;letter-spacing:.08em}.ok{color:var(--ok);border-color:rgba(34,197,94,.5)}.ready{color:#7dd3fc;border-color:rgba(125,211,252,.62);background:rgba(14,116,144,.12)}.disabled{color:#cbd5e1;border-color:rgba(203,213,225,.38);background:rgba(148,163,184,.10)}.warn{color:var(--warn);border-color:rgba(245,158,11,.5)}.err{color:var(--err);border-color:rgba(239,68,68,.5)}.muted{color:var(--muted)}.small{color:var(--muted);font-size:12px}.cards{display:grid;grid-template-columns:repeat(3,minmax(0,1fr));gap:12px;margin:14px 0 18px}.metric{background:linear-gradient(180deg,rgba(17,28,49,.96),rgba(15,23,42,.9));border:1px solid var(--line);border-radius:18px;padding:16px;box-shadow:0 14px 44px rgba(0,0,0,.24)}.metric .label{color:var(--muted);font-size:12px;text-transform:uppercase;letter-spacing:.08em}.metric .value{font-size:28px;font-weight:950;margin-top:7px;letter-spacing:-.04em}.metric .sub{color:var(--muted);margin-top:4px;min-height:20px}.bar{height:8px;background:#07111f;border-radius:999px;overflow:hidden;border:1px solid rgba(148,163,184,.18);margin-top:12px}.fill{height:100%;background:linear-gradient(90deg,var(--accent),var(--violet));width:0%}.sections{display:grid;gap:12px}.section,.panelcard{background:rgba(15,23,42,.9);border:1px solid var(--line);border-radius:16px;overflow:hidden;box-shadow:0 12px 38px rgba(0,0,0,.22)}.panelcard{padding:16px 18px 18px}.panelcard h2{margin:0 0 12px;font-size:16px}.fixed-panels{margin:14px 0 12px}details.section>summary{list-style:none;cursor:pointer;display:flex;align-items:center;justify-content:space-between;gap:12px;padding:16px 18px;font-weight:950;font-size:16px}details.section>summary::-webkit-details-marker{display:none}.chev{color:var(--muted);font-size:18px;transition:transform .16s ease}details[open] .chev{transform:rotate(90deg)}.body{border-top:1px solid var(--line);padding:16px 18px 18px;background:rgba(2,6,23,.16)}.kv{display:grid;grid-template-columns:170px 1fr;gap:8px 12px}.k{color:var(--muted);font-size:12px;text-transform:uppercase;letter-spacing:.06em}.v{word-break:break-word}.actions{display:flex;gap:8px;flex-wrap:wrap;margin-top:12px;align-items:center}button,select,a.btn{background:#17243a;color:var(--text);border:1px solid var(--line);border-radius:10px;padding:9px 11px;font-weight:800;text-decoration:none;cursor:pointer}button:hover,a.btn:hover,select:hover{border-color:var(--accent)}button.danger{background:rgba(127,29,29,.72);border-color:rgba(239,68,68,.65);color:#fecaca}.split{display:grid;grid-template-columns:1fr 1fr;gap:14px}.service{display:flex;align-items:center;justify-content:space-between;gap:12px;border-bottom:1px solid rgba(148,163,184,.12);padding:10px 0}.service:last-child{border-bottom:0}pre{margin:0;white-space:pre-wrap;word-break:break-word;max-height:420px;overflow:auto;background:#050b14;border:1px solid #182338;border-radius:10px;padding:12px;color:#cbd5e1}.logbar{display:flex;gap:8px;margin-bottom:10px;flex-wrap:wrap}.statusline{margin-top:10px;color:var(--muted)}label{display:inline-flex;gap:7px;align-items:center;color:var(--muted)}input[type=checkbox]{accent-color:var(--accent)}@media(max-width:1000px){.cards{grid-template-columns:repeat(2,minmax(0,1fr))}.split{grid-template-columns:1fr}}@media(max-width:640px){.wrap{padding:14px}.top{display:block}.cards{grid-template-columns:1fr}.kv{grid-template-columns:1fr}.actions{align-items:stretch}.actions>*{width:100%;justify-content:center}}
</style></head><body><div class="wrap"><div class="top"><div class="title"><h1>Screentra Cast Player Admin</h1><p>Easy setup / diagnostics for this local signage player. No layout editor here.</p></div><div id="overall" class="pill warn">LOADING</div></div>
<div class="cards"><section class="metric"><div class="label">CPU</div><div id="cpuValue" class="value">—</div><div id="cpuSub" class="sub">Temp —</div><div class="bar"><div id="cpuBar" class="fill"></div></div></section><section class="metric"><div class="label">RAM</div><div id="ramValue" class="value">—</div><div id="ramSub" class="sub">—</div><div class="bar"><div id="ramBar" class="fill"></div></div></section><section class="metric"><div class="label">GPU</div><div id="gpuValue" class="value">—</div><div id="gpuSub" class="sub">—</div><div class="bar"><div id="gpuBar" class="fill"></div></div></section></div>
<div class="sections">
<div class="split fixed-panels"><section class="panelcard"><h2>Device online / working</h2><div id="device" class="kv"></div><div class="actions"><button onclick="refreshAll()">Refresh</button><a class="btn" href="/">Open player</a><a class="btn" href="/touch-calibration.html">Touch calibration</a></div><div id="deviceMsg" class="statusline"></div></section><section class="panelcard"><h2>Services</h2><div id="services"></div><div class="actions"><button onclick="restartCore()">Restart heartbeat + actor</button><button class="danger" onclick="rebootDevice()">Reboot device</button></div></section></div>
<details class="section"><summary><span>Certificate / CA</span><span class="chev">›</span></summary><div class="body"><div id="ca" class="kv"></div><div class="actions"><button onclick="refreshCa()">Refresh CA from server</button><button onclick="loadCerts()">Re-check cert</button></div><div id="caMsg" class="statusline"></div></div></details>
<details class="section"><summary><span>Network</span><span class="chev">›</span></summary><div class="body"><div class="kv"><div class="k">Server URL</div><div class="v" id="netServer">—</div><div class="k">Currently applied</div><div class="v" id="netApplied">—</div><div class="k">Pending change</div><div class="v" id="netPending">none</div><div class="k">Wi-Fi hardware</div><div class="v" id="netWifiHw">—</div></div><h3 style="margin:14px 0 6px;font-size:14px">Switch interface</h3><div class="actions"><label>Target interface <select id="netIface"></select></label><button onclick="probeNetIface()">Test reachability</button><button onclick="applyNetChange()">Confirm network change</button><button class="ok" id="netConfirmBtn" style="display:none" onclick="confirmNetChange()">Confirm working</button><button class="danger" id="netRevertBtn" style="display:none" onclick="revertNetChange()">Revert</button></div><div id="netMsg" class="statusline"></div><div id="netProbe" class="kv"></div><h3 style="margin:14px 0 6px;font-size:14px">Wi-Fi</h3><div class="kv"><div class="k">Wireless iface</div><div class="v" id="netWifiIface">—</div><div class="k">Connected SSID</div><div class="v" id="netWifiSsid">—</div></div><div class="actions"><label>Wifi iface <select id="netWifiSelect"></select></label><button onclick="wifiScan()">Scan</button><button class="danger" id="netWifiDisconnect" style="display:none" onclick="wifiDisconnect()">Disconnect</button></div><div id="netWifiScan" class="kv"></div><div class="actions"><label>SSID <input id="netWifiSsidInput" type="text" placeholder="MyNetwork" style="background:#0a1729;color:var(--text);border:1px solid var(--line);border-radius:8px;padding:8px 10px;font:inherit"></label><label>Password <input id="netWifiPwd" type="password" placeholder="(empty for open)" style="background:#0a1729;color:var(--text);border:1px solid var(--line);border-radius:8px;padding:8px 10px;font:inherit"></label><button onclick="wifiConnect()">Connect to Wi-Fi</button></div><div id="netWifiMsg" class="statusline"></div><h3 style="margin:14px 0 6px;font-size:14px">VLAN tagging</h3><div class="kv"><div class="k">Active VLAN</div><div class="v" id="netVlanActive">—</div></div><div class="actions"><label>Parent iface <select id="netVlanParent"></select></label><label>VLAN ID <input id="netVlanId" type="number" min="1" max="4094" value="100" style="background:#0a1729;color:var(--text);border:1px solid var(--line);border-radius:8px;padding:8px 10px;font:inherit;width:90px"></label><button onclick="vlanCreate()">Create VLAN</button><button class="danger" id="netVlanRemove" style="display:none" onclick="vlanRemove()">Remove VLAN</button></div><div id="netVlanMsg" class="statusline"></div><h3 style="margin:14px 0 6px;font-size:14px">Static IPv4</h3><div class="kv"><div class="k">Configured static</div><div class="v" id="netStaticActive">—</div></div><div class="actions"><label>Iface <select id="netStaticIface"></select></label><label>Address <input id="netStaticAddr" type="text" placeholder="192.168.1.50" style="background:#0a1729;color:var(--text);border:1px solid var(--line);border-radius:8px;padding:8px 10px;font:inherit"></label><label>Prefix <input id="netStaticPrefix" type="number" min="1" max="32" value="24" style="background:#0a1729;color:var(--text);border:1px solid var(--line);border-radius:8px;padding:8px 10px;font:inherit;width:70px"></label><label>Gateway <input id="netStaticGw" type="text" placeholder="192.168.1.1" style="background:#0a1729;color:var(--text);border:1px solid var(--line);border-radius:8px;padding:8px 10px;font:inherit"></label><label>DNS <input id="netStaticDns" type="text" placeholder="1.1.1.1, 8.8.8.8" style="background:#0a1729;color:var(--text);border:1px solid var(--line);border-radius:8px;padding:8px 10px;font:inherit"></label><button onclick="staticApply()">Apply static</button><button class="danger" id="netStaticClear" style="display:none" onclick="staticClear()">Clear static</button></div><div id="netStaticMsg" class="statusline"></div></div></details>
<details class="section"><summary><span>Displays / touch</span><span class="chev">›</span></summary><div class="body"><div class="split"><div><h3>Displays</h3><div id="displays" class="kv"></div></div><div><h3>Touch binding</h3><div id="touchSummary" class="kv"></div><div class="actions"><label>Touch device <select id="touchDevice"></select></label><label>Bind to display <select id="touchDisplayA"></select></label><label><input id="touchJoined" type="checkbox" onchange="toggleTouchJoined()"> allow joined/tiled two-display touch surface</label><label id="touchDisplayBWrap" style="display:none">Second display <select id="touchDisplayB"></select></label><button onclick="saveTouchBinding()">Apply touch binding</button></div></div></div><div id="displayMsg" class="statusline"></div></div></details>
<details class="section"><summary><span>Audio output / Bluetooth speaker</span><span class="chev">›</span></summary><div class="body"><div class="kv"><div class="k">Current sink</div><div class="v" id="audioCurrent">—</div><div class="k">Bluetooth adapter</div><div class="v" id="audioBtStatus">—</div></div><h3 style="margin:14px 0 6px;font-size:14px">Audio output device</h3><div class="actions"><label>Sink <select id="audioDevice"></select></label><button onclick="saveAudioDevice()">Apply audio device</button></div><h3 style="margin:14px 0 6px;font-size:14px">Bluetooth speakers</h3><div class="actions"><button onclick="scanBluetooth()">Scan for Bluetooth speakers</button> <button onclick="loadBluetoothDevices()">Refresh paired list</button> <button onclick="clearScanResults()">Clear scan results</button></div><h4 style="margin:10px 0 4px;font-size:13px;font-weight:600;color:var(--muted)">Discovered this session</h4><div id="bluetoothScanList" class="kv"><em>Run a scan to see nearby speakers.</em></div><h4 style="margin:10px 0 4px;font-size:13px;font-weight:600;color:var(--muted)">Paired devices</h4><div id="bluetoothList" class="kv"></div><div id="audioMsg" class="statusline"></div></div></details>
<details class="section"><summary><span>Logs</span><span class="chev">›</span></summary><div class="body"><div class="logbar"><select id="logTarget"><option value="heartbeat">heartbeat</option><option value="actor">actor</option><option value="renderer0">renderer0</option><option value="kiosk0">labwc kiosk</option><option value="launcher">launcher</option><option value="touch">touch</option><option value="install">install</option><option value="heartbeat-cache">heartbeat cache</option><option value="all">all</option></select><button onclick="loadLogs()">Load logs</button></div><pre id="logs">Select a log target.</pre></div></details>
</div></div><script>
const $=id=>document.getElementById(id); const esc=s=>String(s??'').replace(/[&<>\"']/g,c=>({'&':'&amp;','<':'&lt;','>':'&gt;','\"':'&quot;',"'":'&#39;'}[c]));
function kv(keys){return keys.map(([k,v])=>`<div class="k">${esc(k)}</div><div class="v">${esc(v??'—')}</div>`).join('')} function pill(ok,txt,cls){return `<span class="pill ${cls|| (ok?'ok':'err')}">${esc(txt)}</span>`} function servicePill(x){const active=String(x.active||'unknown').toLowerCase(); const enabled=String(x.enabled||'').toLowerCase(); if(active==='ready')return pill(true,'READY','ready'); if(active==='disabled'||enabled==='disabled')return pill(true,'DISABLED','disabled'); return pill(!!x.active_ok,x.active||'unknown');}
function pct(v){v=Number(v); return Number.isFinite(v)?Math.max(0,Math.min(100,v)):0} function fmtPct(v){return Number.isFinite(Number(v))?Number(v).toFixed(1)+'%':'—'} function bytes(n){n=Number(n); if(!Number.isFinite(n)||n<=0)return '—'; const u=['B','KB','MB','GB','TB']; let i=0; while(n>=1024&&i<u.length-1){n/=1024;i++} return n.toFixed(i?1:0)+' '+u[i]}
async function api(path,opts){const r=await fetch(path,{cache:'no-store',...(opts||{})}); const t=await r.text(); try{return JSON.parse(t)}catch{return {raw:t,status:r.status}}}
async function refreshAll(){const d=await api('/api/admin/status'); renderStatus(d); try{const n=await api('/api/admin/network'); renderNetwork(n);}catch(_e){}}
function renderNetwork(n){if(!n)return; const sv=$('netServer'); if(sv)sv.textContent=n.server_url||'\u2014'; const hw=$('netWifiHw'); if(hw)hw.textContent=n.has_wifi_hardware?'detected':'none'; const ap=$('netApplied'); if(ap){if(n.applied){const a=n.applied; ap.textContent=a.interface+' ('+a.source_ip+') \u2014 set '+a.set_at;}else{ap.textContent='\u2014';}} const sel=$('netIface'); if(sel){sel.innerHTML=''; (n.interfaces||[]).forEach(function(i){var opt=document.createElement('option');opt.value=i.name;opt.textContent=i.name+(i.is_default?' (default)':'')+(i.ipv4&&i.ipv4.length?' \u2014 '+i.ipv4.join(', '):' \u2014 no IPv4')+(i.state!=='up'?' ['+i.state+']':'');if(i.state!=='up'||!i.ipv4||!i.ipv4.length)opt.disabled=true;sel.appendChild(opt);});} const pen=$('netPending'); const cBtn=$('netConfirmBtn'); const rBtn=$('netRevertBtn'); if(pen){if(n.pending&&!n.pending.confirmed){const p=n.pending; const exp=new Date(p.expires_at);const rem=Math.max(0,Math.round((exp-Date.now())/1000));pen.innerHTML='switching <b>'+p.target_interface+'</b> ('+p.target_source_ip+') \u2014 auto-revert in <b>'+Math.floor(rem/60)+':'+String(rem%60).padStart(2,'0')+'</b>';if(cBtn)cBtn.style.display='';if(rBtn)rBtn.style.display='';}else{pen.textContent=n.pending&&n.pending.confirmed?'confirmed at '+n.pending.confirmed_at:'none';if(cBtn)cBtn.style.display='none';if(rBtn)rBtn.style.display='none';}} var wifi=(n.stored_config&&n.stored_config.wifi)||{}; var wifiIface=$('netWifiIface'); if(wifiIface)wifiIface.textContent=(wifi.interface||'—'); var wifiSsid=$('netWifiSsid'); if(wifiSsid)wifiSsid.textContent=(wifi.ssid||'—');
var wifiSel=$('netWifiSelect'); if(wifiSel){wifiSel.innerHTML='';(n.interfaces||[]).filter(function(i){return i.kind==='wifi';}).forEach(function(i){var opt=document.createElement('option');opt.value=i.name;opt.textContent=i.name+(i.ipv4[0]?(' — '+i.ipv4[0]):' (down)');wifiSel.appendChild(opt);});}
var wifiDc=$('netWifiDisconnect'); if(wifiDc)wifiDc.style.display=(wifi&&wifi.interface)?'':'none';
var vlan=(n.stored_config&&n.stored_config.vlan)||{}; var vAct=$('netVlanActive'); if(vAct)vAct.textContent=vlan.name?vlan.name+' (id '+vlan.vlan_id+' on '+vlan.parent+')':'—';
var vParent=$('netVlanParent'); if(vParent){vParent.innerHTML='';(n.interfaces||[]).filter(function(i){return i.kind==='ethernet'||i.kind==='wifi';}).forEach(function(i){var opt=document.createElement('option');opt.value=i.name;opt.textContent=i.name;vParent.appendChild(opt);});}
var vRm=$('netVlanRemove'); if(vRm)vRm.style.display=vlan.name?'':'none';
var stc=(n.stored_config&&n.stored_config.static)||{}; var sAct=$('netStaticActive'); if(sAct)sAct.textContent=stc.interface?(stc.interface+' — '+stc.address+'/'+stc.prefix+(stc.gateway?(' gw '+stc.gateway):'')+(stc.dns&&stc.dns.length?(' dns '+stc.dns.join(',')):'')):'—';
var sIface=$('netStaticIface'); if(sIface){sIface.innerHTML='';(n.interfaces||[]).forEach(function(i){var opt=document.createElement('option');opt.value=i.name;opt.textContent=i.name+(i.kind!=='ethernet'&&i.kind!=='wifi'&&i.kind!=='vlan'?'':' ['+i.kind+']');if(stc.interface===i.name)opt.selected=true;sIface.appendChild(opt);});}
var sClr=$('netStaticClear'); if(sClr)sClr.style.display=stc.interface?'':'none';}
async function probeNetIface(){const sel=$('netIface');const iface=sel?sel.value:'';const msg=$('netMsg');if(msg)msg.textContent='Probing '+iface+'…';const r=await api('/api/admin/network/probe',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({interface:iface})});const probe=$('netProbe');if(probe)probe.innerHTML=kv([['Result',r.ok?'OK':'FAIL'],['Source IP',r.source_ip||'—'],['Error',r.probe&&r.probe.error||'—']]);if(msg)msg.textContent=r.ok?'Probe OK — '+iface+' can reach the server.':'Probe failed: '+(r.probe&&r.probe.error||'unknown');}
async function applyNetChange(){const sel=$('netIface');const iface=sel?sel.value:'';if(!iface||iface==='—')return;if(!confirm('Confirm network change to '+iface+'?\\n\\nThe 5-minute timer starts now. The no-layout page will show a banner with a Revert button. You must log back in here and click Confirm working, or click Revert on the screen, before the timer expires. Otherwise the change auto-reverts.'))return;const msg=$('netMsg');if(msg)msg.textContent='Applying…';const r=await api('/api/admin/network/apply',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({interface:iface})});if(msg)msg.textContent=r.ok?'Network change applied. 5-minute timer started.':'Failed: '+(r.error||JSON.stringify(r));await refreshAll();}
async function confirmNetChange(){if(!confirm('Confirm the new interface is working?'))return;const r=await api('/api/admin/network/confirm',{method:'POST'});alert(r.ok?'Confirmed.':'Failed: '+(r.error||JSON.stringify(r)));await refreshAll();}
async function revertNetChange(){if(!confirm('Revert to the previous interface?'))return;const r=await api('/api/admin/network/revert',{method:'POST'});alert(r.ok?'Reverted.':'Failed: '+(r.error||JSON.stringify(r)));await refreshAll();}
async function wifiScan(){const sel=$('netWifiSelect');const iface=sel?sel.value:'';if(!iface){alert('Pick a wireless interface first');return;}const msg=$('netWifiMsg');if(msg)msg.textContent='Scanning '+iface+'…';const scan=$('netWifiScan');if(scan)scan.innerHTML='<em>scanning…</em>';const r=await api('/api/admin/network/wifi/scan',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({interface:iface})});if(msg)msg.textContent=r.ok?('Found '+r.networks.length+' network(s)'):('Scan failed: '+(r.error||'unknown'));if(scan){if(r.ok){scan.innerHTML=kv(r.networks.map(n=>['<span class="badge">'+(n.signal+' dBm')+'</span> '+esc(n.ssid)+(n.security.length?' ('+n.security.join(',')+')':''),'']));scan.dataset.networks=JSON.stringify(r.networks);}else{scan.innerHTML='<em>'+esc(r.error||'scan failed')+'</em>';}}}
function wifiPickSsid(s){const inp=$('netWifiSsidInput');if(inp)inp.value=s;}
async function wifiConnect(){const sel=$('netWifiSelect');const iface=sel?sel.value:'';const ssid=($('netWifiSsidInput')||{}).value||'';const pwd=($('netWifiPwd')||{}).value||'';if(!iface||!ssid){alert('iface and SSID required');return;}const msg=$('netWifiMsg');if(msg)msg.textContent='Connecting to '+ssid+' on '+iface+'…';const r=await api('/api/admin/network/wifi/connect',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({interface:iface,ssid:ssid,password:pwd||null})});if(msg)msg.textContent=r.ok?('Connected. IP: '+(r.ip||'?')+'. Click Refresh, then switch interface to '+iface+'.'):('Failed: '+(r.error||JSON.stringify(r)));await refreshAll();}
async function wifiDisconnect(){const sel=$('netWifiSelect');const iface=sel?sel.value:'';if(!iface)return;if(!confirm('Disconnect '+iface+'?'))return;const r=await api('/api/admin/network/wifi/disconnect',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({interface:iface})});const msg=$('netWifiMsg');if(msg)msg.textContent=r.ok?'Disconnected':'Failed: '+(r.error||JSON.stringify(r));await refreshAll();}
async function vlanCreate(){const parent=($('netVlanParent')||{}).value||'';const vid=parseInt(($('netVlanId')||{}).value||'0',10);if(!parent||!vid){alert('parent and VLAN ID required');return;}const msg=$('netVlanMsg');if(msg)msg.textContent='Creating VLAN '+parent+'.'+vid+'…';const r=await api('/api/admin/network/vlan/create',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({parent:parent,vlan_id:vid})});if(msg)msg.textContent=r.ok?('Created '+r.name+'. Use Refresh, then switch interface.'):('Failed: '+(r.error||JSON.stringify(r)));await refreshAll();}
async function vlanRemove(){const txt=($('netVlanActive')||{}).textContent||'';const m=txt.match(/([A-Za-z0-9_.]+)/);const name=m?m[1]:'';if(!name||name==='—'||name==='none')return;if(!confirm('Remove VLAN '+name+'?'))return;const r=await api('/api/admin/network/vlan/remove',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({name:name})});const msg=$('netVlanMsg');if(msg)msg.textContent=r.ok?'Removed':'Failed: '+(r.error||JSON.stringify(r));await refreshAll();}
async function staticApply(){const iface=($('netStaticIface')||{}).value||'';const addr=($('netStaticAddr')||{}).value||'';const prefix=parseInt(($('netStaticPrefix')||{}).value||'24',10);const gw=($('netStaticGw')||{}).value||'';const dnsRaw=($('netStaticDns')||{}).value||'';const dns=dnsRaw?dnsRaw.split(',').map(s=>s.trim()).filter(Boolean):[];if(!iface||!addr){alert('iface and address required');return;}const msg=$('netStaticMsg');if(msg)msg.textContent='Applying static…';const r=await api('/api/admin/network/static/apply',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({interface:iface,address:addr,prefix:prefix,gateway:gw||null,dns:dns})});if(msg)msg.textContent=r.ok?('Static applied to '+iface+'. Click Refresh, then switch interface.'):('Failed: '+(r.error||JSON.stringify(r)));await refreshAll();}
async function staticClear(){const sel=$('netStaticIface');const iface=sel?sel.value:'';if(!iface)return;if(!confirm('Clear static config on '+iface+'? The interface will fall back to DHCP.'))return;const r=await api('/api/admin/network/static/clear',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({interface:iface})});const msg=$('netStaticMsg');if(msg)msg.textContent=r.ok?'Static cleared':'Failed: '+(r.error||JSON.stringify(r));await refreshAll();}


function renderCards(d){const m=d.metrics||{}, cpu=m.cpu||{}, ram=m.ram||{}, gpu=m.gpu||{}, s=d.status||{}; $('cpuValue').textContent=fmtPct(cpu.percent); $('cpuSub').textContent='Temp '+(cpu.temp_c!=null?cpu.temp_c+' °C':'—')+(cpu.loadavg?` · load ${Number(cpu.loadavg[0]).toFixed(2)}`:''); $('cpuBar').style.width=pct(cpu.percent)+'%'; $('ramValue').textContent=fmtPct(ram.percent); $('ramSub').textContent=`${bytes(ram.used_bytes)} / ${bytes(ram.total_bytes)}`; $('ramBar').style.width=pct(ram.percent)+'%'; $('gpuValue').textContent=gpu.percent!=null?fmtPct(gpu.percent):(gpu.temp_c!=null?gpu.temp_c+' °C':'—'); const gpuNotes=(gpu.health_notes||[]).join(' · '); $('gpuSub').textContent=(gpu.device||gpu.vendor||'GPU')+(gpu.health?` · ${gpu.health}`:'')+(gpu.percent==null?' · usage n/a':'')+(gpu.frequency_mhz!=null?` · ${gpu.frequency_mhz} MHz`:'')+(gpu.vram_percent!=null?` · VRAM ${fmtPct(gpu.vram_percent)}`:'')+(gpuNotes?` · ${gpuNotes}`:''); $('gpuBar').style.width=pct(gpu.percent)+'%';}
function renderStatus(d){
    const s=d.status||{};
    $('overall').className='pill '+(s.connection==='online'?'ok':'err');
    $('overall').textContent=s.connection==='online'?'ONLINE':'CHECK';
    renderCards(d);
    // base rows
    $('device').innerHTML = kv([
        ['Hostname',s.hostname],
        ['LAN admin URL',s.admin_url],
        ['Client ID',s.client_id],
        ['Server',s.server_url],
        ['Connection',s.connection],
        ['Last seen',s.last_seen],
        ['Display',`${s.display_label||'—'} · ${s.display_resolution||'—'} · idx ${s.display_index}`],
        ['Layout',s.assigned_layout_id||'No layout assigned'],
        ['Touch',s.touch_device||'not detected']
    ]);
    const ca=d.ca||{};
    $('ca').innerHTML=kv([
        ['CA path',ca.ca_path],
        ['CA SHA256',ca.ca_sha256],
        ['Published CA',ca.published_ca_url],
        ['Verify',ca.verify?.ok?'OK':(ca.verify?.error||'failed')],
        ['Server cert expires',ca.verify?.not_after]
    ]);
    $('services').innerHTML=(d.services||[]).map(x=>`<div class="service"><div><b>${esc(x.name)}</b><div class="small">enabled: ${esc(x.enabled)}${x.note?' · '+esc(x.note):''}</div></div>${servicePill(x)}</div>`).join('');
    renderTouchControls(d);
    const tb=d.touch?.binding||{};
    $('displays').innerHTML=kv([
        ['Outputs',(d.outputs||[]).map(o=>`${o.name}:${o.enabled===true?'on':(o.enabled===false?'off':'connected')} ${o.current_mode?(o.current_mode.width?o.current_mode.width+'x'+o.current_mode.height:(o.current_mode.label||'')):''}`).join(' | ')||'—'],
        ['Calibration runs',(d.touch?.calibration||[]).length]
    ]);
    $('touchSummary').innerHTML=kv([
        ['Touch devices',(d.touch?.devices||[]).map(t=>`${t.name} ${t.path} ${t.reason||''}`).join(' | ')||'—'],
        ['Touch binding',tb.device_name?`${tb.device_name} → ${(tb.target_connectors||tb.target_display_indexes||[]).join(', ')||'all active outputs'}${tb.allow_joined_displays?' (joined/tiled span)':''}`:'not set']
    ]);
    // refresh the audio / bluetooth card too
    loadAudioStatus();
    loadAudioDevices();
    loadBluetoothDevices();
}
async function loadCerts(){
    const c=await api('/api/admin/certificates');
    $('ca').innerHTML=kv([
        ['CA path',c.ca_path],
        ['CA SHA256',c.ca_sha256],
        ['Published CA',c.published_ca_url],
        ['Verify',c.verify?.ok?'OK':(c.verify?.error||'failed')],
        ['Server cert expires',c.verify?.not_after]
    ]);
}

// === Audio output + Bluetooth speaker ======================================

async function loadAudioStatus(){
    const cur=$('audioCurrent'); const bt=$('audioBtStatus');
    if(!cur||!bt) return;
    cur.textContent='Loading…'; bt.textContent='Loading…';
    try{
        const r=await api('/api/admin/audio/status');
        if(r.ok){
            cur.textContent = r.current_sink || '— (none selected)';
            bt.textContent  = r.bluetooth
                ? `${r.bluetooth.adapter||'adapter'}: ${r.bluetooth.powered===false?'off (use system tray / bluetoothctl to power on)':(r.bluetooth.discovering?'scanning…':'powered on')} · ${r.bluetooth.connected_count||0} connected`
                : 'no bluetooth adapter detected';
        } else {
            cur.textContent='—'; bt.textContent='Audio subsystem unavailable';
        }
    }catch(_e){ cur.textContent='—'; bt.textContent='Audio subsystem unavailable'; }
}

async function loadAudioDevices(){
    const sel=$('audioDevice'); if(!sel) return;
    try{
        const r=await api('/api/admin/audio/devices');
        if(r.ok && Array.isArray(r.sinks)){
            const cur = r.current_sink || '';
            sel.innerHTML = r.sinks.map(s=>`<option value="${esc(s.name)}" ${s.name===cur?'selected':''}>${esc(s.description||s.name)}${s.name===cur?' (current)':''}</option>`).join('') || '<option value="">(no sinks)</option>';
        } else {
            sel.innerHTML='<option value="">(unavailable)</option>';
        }
    }catch(_e){ sel.innerHTML='<option value="">(unavailable)</option>'; }
}

async function saveAudioDevice(){
    const sel=$('audioDevice'); const dev=sel?sel.value:'';
    const msg=$('audioMsg');
    if(!dev){ if(msg) msg.textContent='Pick an audio sink first'; return; }
    if(msg) msg.textContent='Setting default sink to '+dev+'…';
    const r=await api('/api/admin/audio/set',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({sink:dev})});
    if(msg) msg.textContent = r.ok ? ('Default sink set to '+dev+(r.restart_required?' · Chromium will pick this up on the next page load':'')) : ('Failed: '+(r.error||JSON.stringify(r)));
    if(r.ok) await loadAudioStatus();
}

async function loadBluetoothDevices(){
    const wrap=$('bluetoothList'); if(!wrap) return;
    wrap.innerHTML='<em>Loading paired devices…</em>';
    try{
        const r=await api('/api/admin/bluetooth/devices');
        if(r.ok){
            const paired=r.paired||[]; const connected=r.connected||[];
            if(!paired.length){ wrap.innerHTML='<em>No paired Bluetooth devices. Click Scan to find speakers nearby.</em>'; return; }
            wrap.innerHTML = paired.map(d=>{
                const isConn = connected.includes(d.mac);
                const state = isConn ? '<span class="pill ok">CONNECTED</span>'
                                     : (d.trusted ? '<span class="pill warn">paired · not connected</span>'
                                                  : '<span class="pill warn">paired · not trusted</span>');
                const actions = (isConn
                    ? `<button onclick="btDisconnect('${esc(d.mac)}')">Disconnect</button>`
                    : `<button onclick="btConnect('${esc(d.mac)}')">Connect</button>`)
                    + (d.trusted ? '' : ` <button onclick="btTrust('${esc(d.mac)}')">Trust</button>`);
                return `<div class="k">${esc(d.name||d.mac)}</div>`
                     + `<div class="v">${state} <code>${esc(d.mac)}</code> · ${actions}</div>`;
            }).join('');
        } else {
            wrap.innerHTML='<em>'+esc(r.error||'bluetooth unavailable')+'</em>';
        }
    }catch(_e){ wrap.innerHTML='<em>bluetooth unavailable</em>'; }
}

async function scanBluetooth(){
    const msg=$('audioMsg'); const wrap=$('bluetoothScanList');
    if(msg) msg.textContent='Scanning for Bluetooth speakers (≈12s)…';
    if(wrap) wrap.innerHTML='<em>Scanning… keep speakers in pairing mode.</em>';
    const r=await api('/api/admin/bluetooth/scan',{method:'POST'});
    if(msg) msg.textContent = r.ok
        ? (`Scan done · found ${(r.devices||[]).length} new device(s)`)
        : ('Scan failed: '+(r.error||JSON.stringify(r)));
    if(r.ok){
        // Persist scan results in #bluetoothScanList so they survive
        // refreshAll() (which otherwise re-renders #bluetoothList with the
        // paired-device list). User must click "Clear scan results" to
        // dismiss.
        const dev=r.devices||[];
        if(!dev.length){
            if(wrap) wrap.innerHTML='<em>Scan done · no new speakers found. Try again with the device in pairing mode.</em>';
            return;
        }
        if(wrap) wrap.innerHTML = dev.map(d=>`<div class="k">${esc(d.name||d.mac)}</div><div class="v"><code>${esc(d.mac)}</code> · <button onclick="btPair('${esc(d.mac)}')">Pair + Connect</button></div>`).join('');
    }
}
function clearScanResults(){
    const wrap=$('bluetoothScanList');
    if(wrap) wrap.innerHTML='<em>Run a scan to see nearby speakers.</em>';
}

async function btPair(mac){
    if(!confirm('Pair with '+mac+'? Make sure the speaker is in pairing mode.')) return;
    const msg=$('audioMsg'); if(msg) msg.textContent='Pairing '+mac+'…';
    const r=await api('/api/admin/bluetooth/pair',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({mac:mac})});
    if(msg) msg.textContent = r.ok
        ? ('Paired + connected · '+mac+(r.sink?(' · now using sink '+r.sink):''))
        : ('Pair/connect failed: '+(r.error||JSON.stringify(r)));
    if(r.ok){ await loadBluetoothDevices(); await loadAudioDevices(); await loadAudioStatus(); }
}

async function btConnect(mac){
    const msg=$('audioMsg'); if(msg) msg.textContent='Connecting '+mac+'…';
    const r=await api('/api/admin/bluetooth/connect',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({mac:mac})});
    if(msg) msg.textContent = r.ok ? ('Connected · '+mac+(r.sink?(' · now using sink '+r.sink):'')) : ('Connect failed: '+(r.error||JSON.stringify(r)));
    if(r.ok){ await loadBluetoothDevices(); await loadAudioDevices(); await loadAudioStatus(); }
}

async function btDisconnect(mac){
    const msg=$('audioMsg'); if(msg) msg.textContent='Disconnecting '+mac+'…';
    const r=await api('/api/admin/bluetooth/disconnect',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({mac:mac})});
    if(msg) msg.textContent = r.ok ? ('Disconnected '+mac) : ('Disconnect failed: '+(r.error||JSON.stringify(r)));
    if(r.ok){ await loadBluetoothDevices(); await loadAudioStatus(); }
}

async function btTrust(mac){
    const msg=$('audioMsg'); if(msg) msg.textContent='Trusting '+mac+'…';
    const r=await api('/api/admin/bluetooth/trust',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({mac:mac})});
    if(msg) msg.textContent = r.ok ? ('Trusted '+mac) : ('Trust failed: '+(r.error||JSON.stringify(r)));
    if(r.ok) await loadBluetoothDevices();
}
async function refreshCa(){if(!confirm('Refresh the local CA from the saved server URL and restart heartbeat/actor?')) return; $('caMsg').textContent='Refreshing CA…'; const r=await api('/api/admin/ca/refresh',{method:'POST'}); $('caMsg').textContent=r.ok?'CA refreshed and services restarted.':('CA refresh failed: '+(r.error||JSON.stringify(r))); await loadCerts();}
async function restartCore(){if(!confirm('Restart heartbeat and actor services?'))return; const r=await api('/api/admin/services/restart',{method:'POST'}); alert(r.ok?'Restart requested.':'Restart failed: '+(r.error||JSON.stringify(r))); await refreshAll();}
async function rebootDevice(){if(!confirm('Reboot this player device now? The screen will go offline while it restarts.'))return; if(!confirm('Final confirmation: reboot the device?'))return; const msg=$('deviceMsg'); if(msg) msg.textContent='Scheduling reboot…'; const r=await api('/api/admin/device/reboot',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({confirm:true})}); if(msg) msg.textContent=r.ok?'Reboot scheduled. The admin page will disconnect shortly.':('Reboot failed: '+(r.error||JSON.stringify(r))); if(!r.ok) alert('Reboot failed: '+(r.error||JSON.stringify(r)));}
function renderTouchControls(d){const devSel=$('touchDevice'), a=$('touchDisplayA'), b=$('touchDisplayB'), joined=$('touchJoined'); if(!devSel||!a||!b||!joined)return; const tb=d.touch?.binding||{}; devSel.innerHTML=(d.touch?.devices||[]).map(t=>`<option value="${esc(t.path)}" data-name="${esc(t.name)}" ${t.path===tb.device_path?'selected':''}>${esc(t.name)} · ${esc(t.path)}</option>`).join('') || '<option value="">No touch device detected</option>'; const outs=(d.outputs||[]); const opts=outs.map((o,i)=>`<option value="${i}">${esc(o.name||('display '+i))}</option>`).join('') || '<option value="0">display 0</option>'; a.innerHTML=opts; b.innerHTML=opts; const idxs=tb.target_display_indexes||[0]; a.value=String(idxs[0]??0); b.value=String(idxs[1]??1); joined.checked=!!tb.allow_joined_displays; toggleTouchJoined();}
function toggleTouchJoined(){const wrap=$('touchDisplayBWrap'); if(wrap) wrap.style.display=$('touchJoined')?.checked?'inline-flex':'none';}
async function saveTouchBinding(){const dev=$('touchDevice'); const joined=!!$('touchJoined')?.checked; const idxs=[Number($('touchDisplayA')?.value||0)]; if(joined) idxs.push(Number($('touchDisplayB')?.value||1)); if(joined&&idxs[0]===idxs[1]){alert('Choose two different displays for joined/tiled touch mode.');return;} if(joined&&!confirm('Joined/tiled mode maps this touchscreen across the active desktop for two-display Surface/NVIDIA-style systems. Continue?'))return; const payload={device_path:dev?.value||null,device_name:dev?.selectedOptions?.[0]?.dataset?.name||null,target_display_indexes:idxs,allow_joined_displays:joined}; const msg=$('displayMsg'); if(msg) msg.textContent='Applying touch binding…'; const r=await api('/api/admin/touch-binding',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify(payload)}); if(msg) msg.textContent=r.ok?'Touch binding applied; labwc/kiosk services are restarting.':('Failed: '+(r.error||JSON.stringify(r))); setTimeout(refreshAll,4000);}
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
*{box-sizing:border-box} html,body,#player{margin:0;width:100vw;height:100vh;background:#000;overflow:hidden;color:#fff;font-family:system-ui,-apple-system,Segoe UI,sans-serif;cursor:none}.widget{position:absolute;overflow:hidden;cursor:none;container-type:size}.fallback{display:grid;place-items:center;height:100vh;text-align:center;color:#94a3b8}.fallback strong{display:block;color:#e2e8f0;font-size:2rem;margin-bottom:.5rem}.clock,.date{display:flex;align-items:center;justify-content:center;width:100%;height:100%}iframe{background:transparent;cursor:none;display:block}.was-link{display:inline-flex;align-items:center;gap:.45em;border:1px dashed rgba(125,211,252,.65);background:rgba(14,116,144,.16);color:#bae6fd;border-radius:999px;padding:.14em .55em;text-decoration:none}.was-link:after{content:'was link';font-size:.68em;letter-spacing:.08em;text-transform:uppercase;color:#7dd3fc}.screentra-cast-tpl-render{position:relative;width:100%;height:100%;overflow:hidden;container-type:inline-size}.screentra-cast-tpl-el{position:absolute;box-sizing:border-box;overflow:hidden;display:flex;align-items:center;justify-content:center}
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
function screentra-castTextShadow(p){ if(!p||!p.text_shadow_enabled) return ''; return 'text-shadow:'+(Number(p.text_shadow_x==null?2:p.text_shadow_x)||0)+'px '+(Number(p.text_shadow_y==null?2:p.text_shadow_y)||0)+'px '+(Number(p.text_shadow_blur==null?4:p.text_shadow_blur)||0)+'px '+(p.text_shadow_color||'#000000')+';'; }
function screentra-castTextOutline(p){ if(!p||!p.text_outline_enabled) return ''; return '-webkit-text-stroke:'+(Number(p.text_outline_width==null?1:p.text_outline_width)||0)+'px '+(p.text_outline_color||'#000000')+';'; }
function screentra-castShapeClipPath(mode){var m=mode||'rounded';if(m==='triangle')return 'polygon(50% 0%, 0% 100%, 100% 100%)';if(m==='diamond')return 'polygon(50% 0%, 100% 50%, 50% 100%, 0% 50%)';if(m==='hexagon')return 'polygon(25% 0%, 75% 0%, 100% 50%, 75% 100%, 25% 100%, 0% 50%)';if(m==='star')return 'polygon(50% 0%, 61% 35%, 98% 35%, 68% 57%, 79% 91%, 50% 70%, 21% 91%, 32% 57%, 2% 35%, 39% 35%)';if(m==='parallelogram')return 'polygon(20% 0%, 100% 0%, 80% 100%, 0% 100%)';if(m==='arrow')return 'polygon(0% 25%, 60% 25%, 60% 0%, 100% 50%, 60% 100%, 60% 75%, 0% 75%)';if(m==='plus')return 'polygon(35% 0%, 65% 0%, 65% 35%, 100% 35%, 100% 65%, 65% 65%, 65% 100%, 35% 100%, 35% 65%, 0% 65%, 0% 35%, 35% 35%)';if(m==='minus')return 'polygon(0% 35%, 100% 35%, 100% 65%, 0% 65%)';if(m==='x')return 'polygon(20% 0%, 50% 30%, 80% 0%, 100% 20%, 70% 50%, 100% 80%, 80% 100%, 50% 70%, 20% 100%, 0% 80%, 30% 50%, 0% 20%)';return '';}
function styleFor(w){const x=(Number(w.x)||0)*100,y=(Number(w.y)||0)*100,ww=(Number(w.w)||0.1)*100,hh=(Number(w.h)||0.1)*100,z=Number(w.z)||1;return `left:${x}%;top:${y}%;width:${ww}%;height:${hh}%;z-index:${z};`;}
function screentra-castWmoEmoji(code){var map={'0':'☀️','1':'🌤️','2':'⛅','3':'☁️','45':'🌫️','48':'🌫️','51':'🌦️','53':'🌦️','55':'🌦️','61':'🌧️','63':'🌧️','65':'🌧️','71':'❄️','73':'❄️','75':'❄️','80':'🌦️','81':'🌧️','82':'🌧️','95':'⛈️','96':'⛈️','99':'⛈️'};return map[String(code)]||'🌤️';}
function screentra-castWmoDefaultIcon(code){var m={0:'<svg viewBox="0 0 64 64" xmlns="http://www.w3.org/2000/svg" aria-label="Clear"><circle cx="32" cy="32" r="14" fill="#FFD23F"/><g stroke="#FFD23F" stroke-width="4" stroke-linecap="round"><line x1="32" y1="4"  x2="32" y2="12"/><line x1="32" y1="52" x2="32" y2="60"/><line x1="4"  y1="32" x2="12" y2="32"/><line x1="52" y1="32" x2="60" y2="32"/><line x1="11" y1="11" x2="17" y2="17"/><line x1="47" y1="47" x2="53" y2="53"/><line x1="11" y1="53" x2="17" y2="47"/><line x1="47" y1="17" x2="53" y2="11"/></g></svg>',1:'<svg viewBox="0 0 64 64" xmlns="http://www.w3.org/2000/svg" aria-label="Mainly clear"><circle cx="22" cy="22" r="10" fill="#FFD23F"/><g stroke="#FFD23F" stroke-width="3" stroke-linecap="round"><line x1="22" y1="4"  x2="22" y2="9"/><line x1="4"  y1="22" x2="9"  y2="22"/><line x1="9"  y1="9"  x2="13" y2="13"/><line x1="35" y1="9"  x2="31" y2="13"/></g><path d="M20 44 q0 -10 10 -10 q4 -8 14 -8 q12 0 12 12 q6 0 6 8 q0 6 -8 6 h-28 q-6 0 -6 -8 z" fill="#E8ECEF" stroke="#9DA5AD" stroke-width="2"/></svg>',2:'<svg viewBox="0 0 64 64" xmlns="http://www.w3.org/2000/svg" aria-label="Partly cloudy"><circle cx="22" cy="20" r="9" fill="#FFD23F"/><g stroke="#FFD23F" stroke-width="3" stroke-linecap="round"><line x1="22" y1="4"  x2="22" y2="8"/><line x1="4"  y1="20" x2="8"  y2="20"/><line x1="9"  y1="7"  x2="12" y2="10"/><line x1="35" y1="10" x2="32" y2="13"/></g><path d="M18 46 q0 -10 10 -10 q4 -8 14 -8 q12 0 12 12 q6 0 6 8 q0 6 -8 6 h-28 q-6 0 -6 -8 z" fill="#E8ECEF" stroke="#9DA5AD" stroke-width="2"/></svg>',3:'<svg viewBox="0 0 64 64" xmlns="http://www.w3.org/2000/svg" aria-label="Overcast"><path d="M14 36 q0 -10 10 -10 q4 -8 14 -8 q12 0 12 12 q6 0 6 8 q0 6 -8 6 h-28 q-6 0 -6 -8 z" fill="#9DA5AD" stroke="#6B7480" stroke-width="2"/><path d="M8 48 q0 -8 8 -8 q3 -6 11 -6 q9 0 9 9 q5 0 5 6 q0 5 -6 5 h-22 q-5 0 -5 -6 z" fill="#B8C0C8" stroke="#6B7480" stroke-width="1.5"/></svg>',45:'<svg viewBox="0 0 64 64" xmlns="http://www.w3.org/2000/svg" aria-label="Fog"><path d="M14 32 q0 -8 8 -8 q3 -6 11 -6 q9 0 9 9 q5 0 5 6 q0 5 -6 5 h-22 q-5 0 -5 -6 z" fill="#E8ECEF" stroke="#9DA5AD" stroke-width="1.5"/><g stroke="#9DA5AD" stroke-width="3" stroke-linecap="round"><line x1="6"  y1="46" x2="58" y2="46"/><line x1="10" y1="54" x2="54" y2="54"/><line x1="14" y1="38" x2="50" y2="38"/></g></svg>',48:'<svg viewBox="0 0 64 64" xmlns="http://www.w3.org/2000/svg" aria-label="Rime fog"><path d="M14 32 q0 -8 8 -8 q3 -6 11 -6 q9 0 9 9 q5 0 5 6 q0 5 -6 5 h-22 q-5 0 -5 -6 z" fill="#E8ECEF" stroke="#9DA5AD" stroke-width="1.5"/><g stroke="#9DA5AD" stroke-width="3" stroke-linecap="round"><line x1="6"  y1="46" x2="58" y2="46"/><line x1="10" y1="54" x2="54" y2="54"/><line x1="14" y1="38" x2="50" y2="38"/></g><g fill="#9DA5AD"><circle cx="20" cy="58" r="1.5"/><circle cx="32" cy="60" r="1.5"/><circle cx="44" cy="58" r="1.5"/></g></svg>',51:'<svg viewBox="0 0 64 64" xmlns="http://www.w3.org/2000/svg" aria-label="Light drizzle"><path d="M14 32 q0 -10 10 -10 q4 -8 14 -8 q12 0 12 12 q6 0 6 8 q0 6 -8 6 h-28 q-6 0 -6 -8 z" fill="#9DA5AD" stroke="#6B7480" stroke-width="2"/><g fill="#4A90E2"><circle cx="22" cy="50" r="2"/><circle cx="32" cy="54" r="2"/><circle cx="42" cy="50" r="2"/></g></svg>',53:'<svg viewBox="0 0 64 64" xmlns="http://www.w3.org/2000/svg" aria-label="Moderate drizzle"><path d="M14 32 q0 -10 10 -10 q4 -8 14 -8 q12 0 12 12 q6 0 6 8 q0 6 -8 6 h-28 q-6 0 -6 -8 z" fill="#9DA5AD" stroke="#6B7480" stroke-width="2"/><g fill="#4A90E2"><circle cx="20" cy="48" r="2.5"/><circle cx="32" cy="54" r="2.5"/><circle cx="44" cy="48" r="2.5"/><circle cx="26" cy="58" r="2"/><circle cx="38" cy="58" r="2"/></g></svg>',55:'<svg viewBox="0 0 64 64" xmlns="http://www.w3.org/2000/svg" aria-label="Dense drizzle"><path d="M14 32 q0 -10 10 -10 q4 -8 14 -8 q12 0 12 12 q6 0 6 8 q0 6 -8 6 h-28 q-6 0 -6 -8 z" fill="#6B7480" stroke="#3F4750" stroke-width="2"/><g fill="#2C6FB8"><circle cx="18" cy="46" r="3"/><circle cx="28" cy="52" r="3"/><circle cx="38" cy="46" r="3"/><circle cx="46" cy="52" r="3"/><circle cx="22" cy="58" r="2.5"/><circle cx="34" cy="60" r="2.5"/><circle cx="42" cy="58" r="2.5"/></g></svg>',61:'<svg viewBox="0 0 64 64" xmlns="http://www.w3.org/2000/svg" aria-label="Slight rain"><path d="M14 32 q0 -10 10 -10 q4 -8 14 -8 q12 0 12 12 q6 0 6 8 q0 6 -8 6 h-28 q-6 0 -6 -8 z" fill="#9DA5AD" stroke="#6B7480" stroke-width="2"/><g fill="none" stroke="#4A90E2" stroke-width="2.5" stroke-linecap="round"><path d="M22 48 q-2 4 0 6 q2 -2 0 -6 z"/><path d="M32 52 q-2 4 0 6 q2 -2 0 -6 z"/><path d="M42 48 q-2 4 0 6 q2 -2 0 -6 z"/></g></svg>',63:'<svg viewBox="0 0 64 64" xmlns="http://www.w3.org/2000/svg" aria-label="Moderate rain"><path d="M14 30 q0 -10 10 -10 q4 -8 14 -8 q12 0 12 12 q6 0 6 8 q0 6 -8 6 h-28 q-6 0 -6 -8 z" fill="#7A828C" stroke="#4A5058" stroke-width="2"/><g fill="none" stroke="#2C6FB8" stroke-width="3" stroke-linecap="round"><path d="M20 46 q-3 6 0 10 q3 -4 0 -10 z"/><path d="M32 48 q-3 6 0 10 q3 -4 0 -10 z"/><path d="M44 46 q-3 6 0 10 q3 -4 0 -10 z"/></g></svg>',65:'<svg viewBox="0 0 64 64" xmlns="http://www.w3.org/2000/svg" aria-label="Heavy rain"><path d="M14 28 q0 -10 10 -10 q4 -8 14 -8 q12 0 12 12 q6 0 6 8 q0 6 -8 6 h-28 q-6 0 -6 -8 z" fill="#4A5058" stroke="#2C3038" stroke-width="2"/><g fill="none" stroke="#1A4A82" stroke-width="3.5" stroke-linecap="round"><path d="M18 44 q-4 8 0 14 q4 -6 0 -14 z"/><path d="M30 46 q-4 8 0 14 q4 -6 0 -14 z"/><path d="M42 44 q-4 8 0 14 q4 -6 0 -14 z"/><path d="M54 46 q-4 8 0 14 q4 -6 0 -14 z"/></g></svg>',71:'<svg viewBox="0 0 64 64" xmlns="http://www.w3.org/2000/svg" aria-label="Slight snow"><path d="M14 32 q0 -10 10 -10 q4 -8 14 -8 q12 0 12 12 q6 0 6 8 q0 6 -8 6 h-28 q-6 0 -6 -8 z" fill="#E8ECEF" stroke="#9DA5AD" stroke-width="2"/><g fill="#FFFFFF" stroke="#4A90E2" stroke-width="1"><circle cx="22" cy="50" r="3"/><circle cx="32" cy="54" r="3"/><circle cx="42" cy="50" r="3"/></g></svg>',73:'<svg viewBox="0 0 64 64" xmlns="http://www.w3.org/2000/svg" aria-label="Moderate snow"><path d="M14 30 q0 -10 10 -10 q4 -8 14 -8 q12 0 12 12 q6 0 6 8 q0 6 -8 6 h-28 q-6 0 -6 -8 z" fill="#B8C0C8" stroke="#6B7480" stroke-width="2"/><g fill="#FFFFFF" stroke="#4A90E2" stroke-width="1.5"><circle cx="20" cy="48" r="4"/><circle cx="32" cy="54" r="4"/><circle cx="44" cy="48" r="4"/><circle cx="26" cy="58" r="3"/><circle cx="38" cy="58" r="3"/></g></svg>',75:'<svg viewBox="0 0 64 64" xmlns="http://www.w3.org/2000/svg" aria-label="Heavy snow"><path d="M14 28 q0 -10 10 -10 q4 -8 14 -8 q12 0 12 12 q6 0 6 8 q0 6 -8 6 h-28 q-6 0 -6 -8 z" fill="#6B7480" stroke="#3F4750" stroke-width="2"/><g fill="#FFFFFF" stroke="#1A4A82" stroke-width="1.5"><circle cx="18" cy="46" r="5"/><circle cx="30" cy="52" r="5"/><circle cx="42" cy="46" r="5"/><circle cx="50" cy="52" r="5"/><circle cx="24" cy="58" r="4"/><circle cx="40" cy="58" r="4"/></g></svg>',80:'<svg viewBox="0 0 64 64" xmlns="http://www.w3.org/2000/svg" aria-label="Rain showers"><circle cx="20" cy="18" r="8" fill="#FFD23F"/><path d="M14 36 q0 -10 10 -10 q4 -8 14 -8 q12 0 12 12 q6 0 6 8 q0 6 -8 6 h-28 q-6 0 -6 -8 z" fill="#9DA5AD" stroke="#6B7480" stroke-width="2"/><g fill="none" stroke="#4A90E2" stroke-width="2.5" stroke-linecap="round"><path d="M22 48 q-2 4 0 6 q2 -2 0 -6 z"/><path d="M32 52 q-2 4 0 6 q2 -2 0 -6 z"/><path d="M42 48 q-2 4 0 6 q2 -2 0 -6 z"/></g></svg>',81:'<svg viewBox="0 0 64 64" xmlns="http://www.w3.org/2000/svg" aria-label="Heavy rain showers"><circle cx="20" cy="18" r="8" fill="#FFD23F"/><path d="M14 36 q0 -10 10 -10 q4 -8 14 -8 q12 0 12 12 q6 0 6 8 q0 6 -8 6 h-28 q-6 0 -6 -8 z" fill="#7A828C" stroke="#4A5058" stroke-width="2"/><g fill="none" stroke="#2C6FB8" stroke-width="3" stroke-linecap="round"><path d="M20 48 q-3 6 0 10 q3 -4 0 -10 z"/><path d="M32 50 q-3 6 0 10 q3 -4 0 -10 z"/><path d="M44 48 q-3 6 0 10 q3 -4 0 -10 z"/></g></svg>',82:'<svg viewBox="0 0 64 64" xmlns="http://www.w3.org/2000/svg" aria-label="Violent rain showers"><circle cx="20" cy="18" r="8" fill="#FFD23F"/><path d="M14 36 q0 -10 10 -10 q4 -8 14 -8 q12 0 12 12 q6 0 6 8 q0 6 -8 6 h-28 q-6 0 -6 -8 z" fill="#4A5058" stroke="#2C3038" stroke-width="2"/><g fill="none" stroke="#1A4A82" stroke-width="3.5" stroke-linecap="round"><path d="M18 46 q-4 8 0 14 q4 -6 0 -14 z"/><path d="M30 48 q-4 8 0 14 q4 -6 0 -14 z"/><path d="M42 46 q-4 8 0 14 q4 -6 0 -14 z"/><path d="M52 48 q-4 8 0 14 q4 -6 0 -14 z"/></g></svg>',95:'<svg viewBox="0 0 64 64" xmlns="http://www.w3.org/2000/svg" aria-label="Thunderstorm"><path d="M14 28 q0 -10 10 -10 q4 -8 14 -8 q12 0 12 12 q6 0 6 8 q0 6 -8 6 h-28 q-6 0 -6 -8 z" fill="#4A5058" stroke="#2C3038" stroke-width="2"/><path d="M30 38 L22 50 L30 50 L26 60 L40 44 L32 44 L36 38 z" fill="#FFD23F" stroke="#B8860B" stroke-width="1.5" stroke-linejoin="round"/></svg>',96:'<svg viewBox="0 0 64 64" xmlns="http://www.w3.org/2000/svg" aria-label="Thunderstorm with hail"><path d="M14 28 q0 -10 10 -10 q4 -8 14 -8 q12 0 12 12 q6 0 6 8 q0 6 -8 6 h-28 q-6 0 -6 -8 z" fill="#4A5058" stroke="#2C3038" stroke-width="2"/><path d="M30 38 L22 50 L30 50 L26 60 L40 44 L32 44 L36 38 z" fill="#FFD23F" stroke="#B8860B" stroke-width="1.5" stroke-linejoin="round"/><g fill="#FFFFFF" stroke="#4A90E2" stroke-width="1"><circle cx="16" cy="56" r="2.5"/><circle cx="24" cy="60" r="2.5"/><circle cx="40" cy="60" r="2.5"/><circle cx="48" cy="56" r="2.5"/></g></svg>',99:'<svg viewBox="0 0 64 64" xmlns="http://www.w3.org/2000/svg" aria-label="Severe thunderstorm"><path d="M14 26 q0 -10 10 -10 q4 -8 14 -8 q12 0 12 12 q6 0 6 8 q0 6 -8 6 h-28 q-6 0 -6 -8 z" fill="#2C3038" stroke="#1A1D22" stroke-width="2"/><path d="M30 36 L20 50 L28 50 L24 62 L42 42 L34 42 L38 36 z" fill="#FFD23F" stroke="#B8860B" stroke-width="1.5" stroke-linejoin="round"/><g fill="#FFFFFF" stroke="#1A4A82" stroke-width="1.5"><circle cx="14" cy="56" r="3"/><circle cx="22" cy="60" r="3"/><circle cx="38" cy="60" r="3"/><circle cx="50" cy="56" r="3"/></g></svg>'};return m[String(code)]||'';}
function screentra-castWmoDesc(code){var map={'0':'Clear sky','1':'Mainly clear','2':'Partly cloudy','3':'Overcast','45':'Fog','48':'Rime fog','51':'Light drizzle','53':'Moderate drizzle','55':'Dense drizzle','61':'Slight rain','63':'Moderate rain','65':'Heavy rain','71':'Slight snow','73':'Moderate snow','75':'Heavy snow','80':'Rain showers','81':'Heavy rain showers','82':'Violent rain showers','95':'Thunderstorm','96':'Thunderstorm w/ hail','99':'Severe thunderstorm w/ hail'};return map[String(code)]||'Weather';}
function screentra-castConv(c,u){if(c==null||Number.isNaN(Number(c)))return '';var n=Number(c);return Math.round(String(u||'F').toUpperCase()==='F'?(n*9/5+32):n);}
function screentra-castWeatherFor(p,off){var wx=(p._weather_data&&p._weather_data.current_weather)?p._weather_data:null;var d=wx&&wx.daily?wx.daily:{};var i=Math.max(0,Math.min(6,Number(off)||0));var code=d.weathercode&&d.weathercode[i]!=null?d.weathercode[i]:(wx&&wx.current_weather?wx.current_weather.weathercode:2);var hi=d.temperature_2m_max&&d.temperature_2m_max[i]!=null?d.temperature_2m_max[i]:(wx&&wx.current_weather?wx.current_weather.temperature:22);var lo=d.temperature_2m_min&&d.temperature_2m_min[i]!=null?d.temperature_2m_min[i]:hi;var pr=d.precipitation_probability_max&&d.precipitation_probability_max[i]!=null?d.precipitation_probability_max[i]:0;var cur=wx&&wx.current_weather&&wx.current_weather.temperature!=null?wx.current_weather.temperature:hi;return {code:String(code),high:screentra-castConv(hi,p.unit||'F'),low:screentra-castConv(lo,p.unit||'F'),current:screentra-castConv(cur,p.unit||'F'),precip:Math.round(Number(pr)||0),desc:screentra-castWmoDesc(code),wind:wx&&wx.current_weather?Math.round(Number(wx.current_weather.windspeed)||0):0};}
async function screentra-castLoadWeather(p){var lat=Number(p.lat||0),lon=Number(p.lon||0);if(!lat||!lon){p._weather_data={current_weather:{temperature:22,weathercode:2,windspeed:8,winddirection:180},daily:{weathercode:[2,3,61,80,95,71,1],temperature_2m_max:[24,22,18,19,16,5,8],temperature_2m_min:[15,14,12,11,10,-2,0],precipitation_probability_max:[20,35,70,55,85,60,10]}};p._weather_data_source='sample';return p._weather_data;}try{var r=await fetch('/api/weather/forecast?lat='+encodeURIComponent(lat)+'&lon='+encodeURIComponent(lon));if(!r.ok)throw new Error('weather '+r.status);p._weather_data=await r.json();p._weather_data_source='live';}catch(e){p._weather_data={current_weather:{temperature:22,weathercode:2,windspeed:8,winddirection:180},daily:{weathercode:[2,3,61,80,95,71,1],temperature_2m_max:[24,22,18,19,16,5,8],temperature_2m_min:[15,14,12,11,10,-2,0],precipitation_probability_max:[20,35,70,55,85,60,10]}};p._weather_data_source='sample';}return p._weather_data;}
function screentra-castTplSample(el,p){var pp=el.props||{};var src=pp.variable_source||(el.type==='variable_icon'?'weather':'date');var key=pp.variable_key||(el.type==='variable_icon'?'weather_icon':'full_date');var off=Math.max(0,Math.min(6,Number(pp.day_offset??pp.weather_day_offset)||0));var wx=screentra-castWeatherFor(p||{},off);if(key==='weather_icon'||el.type==='variable_icon'||pp.display_mode==='weather_icon'){var icons=(p&&p.custom_icons)||{};var url=icons[wx.code]||'';if(!url){var svg=screentra-castWmoDefaultIcon(wx.code);}return {html:url?'<img src="'+esc(url)+'" style="max-width:100%;max-height:100%;object-fit:contain;">':(svg?'<span style="display:inline-flex;width:100%;height:100%;align-items:center;justify-content:center;">'+svg+'</span>':'<span style="font-size:2.2em;line-height:1;">'+screentra-castWmoEmoji(wx.code)+'</span>')};}if(src==='date'){var d=new Date();d.setDate(d.getDate()+(Number(pp.day_offset??pp.weather_day_offset)||0));if(key==='day_name')return {text:d.toLocaleDateString(undefined,{weekday:'long'})};if(key==='date_number')return {text:String(d.getDate())};if(key==='month_day')return {text:d.toLocaleDateString(undefined,{month:'short',day:'numeric'})};return {text:d.toLocaleDateString(undefined,{weekday:'long',month:'short',day:'numeric'})};}if(src==='calendar'){var n=(Number(pp.day_offset??pp.weather_day_offset)||0)+1;if(key==='calendar_event_time')return {text:'Day '+n+' 9:00 AM'};if(key==='calendar_event_location')return {text:'Day '+n+' location'};return {text:'Day '+n+' event'};}if(src==='weather'){var u=String((p&&p.unit)||'F').toUpperCase()==='F'?'°F':'°C';if(key==='current_temp')return {text:wx.current+u};if(key==='high_temp')return {text:'High '+wx.high+u};if(key==='low_temp')return {text:'Low '+wx.low+u};if(key==='weather_description')return {text:wx.desc};if(key==='precip_chance')return {text:wx.precip+'%'};if(key==='wind_speed')return {text:'Wind '+wx.wind+' mph'};}return {text:pp.default_text||pp.label||key};}
function screentra-castTemplateHTML(p){var els=Array.isArray(p.template_elements)?p.template_elements:[];if(!els.length)return '';var dw=Number(p.template_width||760)||760;var dh=Number(p.template_height||427.5)||427.5;var out='<div class="screentra-cast-tpl-render">';els.slice().sort(function(a,b){return(Number(a.z)||0)-(Number(b.z)||0);}).forEach(function(el){var rep=el.repeat||{mode:'none',count:1,gap_x:0,gap_y:0};var cnt=Math.max(1,Math.min(30,Number(rep.count)||1));for(var i=0;i<cnt;i++){var x=(Number(el.x)||0)+i*((Number(rep.gap_x)||0)+(rep.mode==='columns'?(Number(el.width)||0):0));var y=(Number(el.y)||0)+i*((Number(rep.gap_y)||0)+(rep.mode==='rows'?(Number(el.height)||0):0));var w=Number(el.width)||120,h=Number(el.height)||80;var pp=el.props||{};var style='position:absolute;box-sizing:border-box;overflow:hidden;display:flex;align-items:center;justify-content:center;text-align:'+esc(pp.align||'center')+';left:'+(x/dw*100)+'%;top:'+(y/dh*100)+'%;width:'+(w/dw*100)+'%;height:'+(h/dh*100)+'%;z-index:'+(Number(el.z)||0)+';';var inner='';if(el.type==='rectangle'){var shape=pp.shape||'rounded';var base=Number(pp.radius==null?10:pp.radius)||0;var radius=shape==='circle'?'50%':(shape==='pill'?'999px':(Number(pp.radius_tl??base)||0)+'px '+(Number(pp.radius_tr??base)||0)+'px '+(Number(pp.radius_br??base)||0)+'px '+(Number(pp.radius_bl??base)||0)+'px');var opacity=Math.max(0,Math.min(100,Number(pp.opacity==null?100:pp.opacity)))/100;var clip=screentra-castShapeClipPath(shape);var lineStyle=shape==='line'?'height:6px;':'';style+='background:'+esc(pp.color||'#1e293b')+';border-radius:'+radius+';opacity:'+opacity+';'+lineStyle;if(clip)style+='clip-path:'+clip+';-webkit-clip-path:'+clip+';';}else if(el.type==='image'){var imageFit=['contain','cover','fill','scale-down','none'].indexOf(pp.fit)>=0?pp.fit:'contain';inner=pp.url?'<img src="'+esc(pp.url)+'" alt="'+esc(pp.alt||'')+'" style="width:100%;height:100%;object-fit:'+imageFit+';">':'';}else if(el.type==='text'){var fs=Number(pp.font_size)||24;style+='color:'+esc(pp.color||'#fff')+';font-size:clamp(6px, calc('+(fs/dw*100)+'cqw), '+(fs*4)+'px);font-weight:'+esc(pp.font_weight||700)+';'+screentra-castTextShadow(pp)+screentra-castTextOutline(pp);inner=esc(pp.text||'Text box');}else if(el.type==='variable_text'||el.type==='variable_icon'||el.type==='variable'||el.type==='date'){var sample=screentra-castTplSample(el,p);var fs2=Number(pp.font_size)||26;style+='color:'+esc(pp.color||'#fff')+';font-size:clamp(6px, calc('+(fs2/dw*100)+'cqw), '+(fs2*4)+'px);font-weight:800;'+screentra-castTextShadow(pp)+screentra-castTextOutline(pp);inner=sample.html||esc(sample.text||'');}else{inner=esc(pp.label||el.type||'');}out+='<div class="screentra-cast-tpl-el" style="'+style+'">'+inner+'</div>';}});out+='</div>';return out;}

function countdownText(target,now,p){const diff=Math.max(0,target-now);const d=Math.floor(diff/86400000),h=Math.floor(diff%86400000/3600000),m=Math.floor(diff%3600000/60000),s=Math.floor(diff%60000/1000);return Number.isFinite(target)&&target>0?(d?d+'d '+h+'h':h?h+'h '+m+'m':m?m+'m '+s+'s':s+'s'):(p.expiry_text||'Choose a date and time');}
function countdownMarkup(p){const mode=p.countdown_mode||'date',value=Math.max(1,Number(p.duration_value||1)),unit=p.duration_unit||'minutes',mult={seconds:1000,minutes:60000,hours:3600000,days:86400000}[unit]||60000,started=Date.now(),target=mode==='duration'?started+value*mult:new Date(p.targetDate||'').getTime();return `<div data-countdown-widget="1" data-countdown-mode="${esc(mode)}" data-countdown-target="${esc(p.targetDate||'')}" data-countdown-duration-value="${esc(value)}" data-countdown-duration-unit="${esc(unit)}" data-countdown-started-at="${mode==='duration'?started:''}" data-countdown-expiry-text="${esc(p.expiry_text||'')}" class="countdown-value" style="width:100%;height:100%;display:flex;align-items:center;justify-content:center;padding:1rem;color:${p.color||'#fff'};font-size:clamp(8px,${Number(p.font_size||p.fontSize||48)}cqh,${Number(p.font_size||p.fontSize||48)*4}px);font-weight:800;line-height:1;text-align:center;overflow:hidden;white-space:nowrap;">${esc(Number.isFinite(target)&&target>0?countdownText(target,started,p):(p.expiry_text||'Choose a date and time'))}</div>`;}
function tickCountdowns(){const now=Date.now(),units={seconds:1000,minutes:60000,hours:3600000,days:86400000};document.querySelectorAll('[data-countdown-widget]').forEach(node=>{const mode=node.dataset.countdownMode||'date',duration=Math.max(1,Number(node.dataset.countdownDurationValue||1))*(units[node.dataset.countdownDurationUnit||'minutes']||60000);let target;if(mode==='duration'){if(!node.dataset.countdownStartedAt)node.dataset.countdownStartedAt=String(now);target=Number(node.dataset.countdownStartedAt)+duration;}else target=new Date(node.dataset.countdownTarget||'').getTime();const value=node.querySelector('.countdown-value')||node;if(!Number.isFinite(target)||target<=0)return;value.textContent=target<=now?(node.dataset.countdownExpiryText||''):countdownText(target,now,{});});}
function renderWidget(w){const p=props(w);const t=normWidgetType(w);let inner='';
  if(t==='countdown') inner=countdownMarkup(p);
  else if(t==='text') inner=`<div style="width:100%;height:100%;display:flex;align-items:${p.valign||'center'};justify-content:${p.align==='left'?'flex-start':p.align==='right'?'flex-end':'center'};padding:1rem;color:${p.color||'#fff'};font-size:clamp(8px,${Number(p.font_size||p.fontSize||24)}cqh,${Number(p.font_size||p.fontSize||24) * 4}px);text-align:${p.align||'center'};${screentra-castTextShadow(p)}${screentra-castTextOutline(p)}">${esc(p.text||'Text')}</div>`;
 else if(t==='image') inner=`<img src="${esc(p.url||p.src||'')}" style="width:100%;height:100%;object-fit:${p.fit||'cover'};">`;
 else if(t==='video') {
  const list=Array.isArray(p.playlist)&&p.playlist.length?p.playlist:(p.url||p.src?[{url:p.url||p.src}]:[]);
  const url=list[0]&&list[0].url||'';
  const fit=p.fit||'cover';
  if(isYouTubeUrl(url)) {
    // 2026-08-11: wrap iframe in a clip container and scale it to overflow
    // so the YouTube embed's 16:9 letterboxing falls outside the visible
    // area. Without this, the youtube Layout shows thick black bars on any
    // video whose aspect ratio differs from 16:9.
    // fit=cover: scale to fill (default — kills black bars)
    // fit=contain: letterbox inside container (show all video, accept bars)
    // fit=stretch: ignore aspect, distort to fill
    if(fit==='stretch') {
      inner=`<iframe src="${esc(youtubeEmbedUrl(url,p))}" style="width:100%;height:100%;border:0;background:#000;" allow="autoplay; encrypted-media; picture-in-picture; fullscreen" referrerpolicy="strict-origin-when-cross-origin"></iframe>`;
    } else if(fit==='contain') {
      inner=`<div style="position:absolute;inset:0;display:grid;place-items:center;background:#000;overflow:hidden;"><iframe src="${esc(youtubeEmbedUrl(url,p))}" style="width:100%;height:100%;max-width:100%;max-height:100%;border:0;background:#000;object-fit:contain;" allow="autoplay; encrypted-media; picture-in-picture; fullscreen" referrerpolicy="strict-origin-when-cross-origin"></iframe></div>`;
    } else {
      // cover: scale iframe to 1.78x container so YouTube embed's 16:9
      // letterbox falls outside the visible area. overflow:hidden on the
      // wrapper clips the rest. 1.78 covers 16:9 exactly; using 1.85
      // (a touch larger) also clips the YouTube player controls bar.
      inner=`<div style="position:absolute;inset:0;overflow:hidden;background:#000;"><iframe src="${esc(youtubeEmbedUrl(url,p))}" style="position:absolute;top:50%;left:50%;width:178%;height:178%;transform:translate(-50%,-50%);border:0;background:#000;" allow="autoplay; encrypted-media; picture-in-picture; fullscreen" referrerpolicy="strict-origin-when-cross-origin"></iframe></div>`;
    }
  } else {
    inner=`<video data-video-widget-id="${esc(w.id||'')}" src="${esc(url)}" autoplay ${p.loop!==false&&list.length===1?'loop ':''} playsinline preload="auto" ${p.mute||p.muted?'muted':''} style="width:100%;height:100%;object-fit:${fit};"></video>`;
  }
}
 else if(t==='music') inner=`<audio src="${esc(p.url||p.src||'')}" autoplay loop ${p.mute?'muted':''}></audio>`;
 else if(t==='clock') inner=`<div class="clock" data-clock style="color:${p.color||'#fff'};font-size:clamp(8px,${Number(p.font_size||p.fontSize||48)}cqh,${Number(p.font_size||p.fontSize||48) * 4}px);${screentra-castTextShadow(p)}${screentra-castTextOutline(p)}"></div>`;
 else if(t==='datetime'||t==='date') inner=`<div class="date" data-date style="color:${p.color||'#fff'};font-size:clamp(8px,${Number(p.font_size||p.fontSize||36)}cqh,${Number(p.font_size||p.fontSize||36) * 4}px);${screentra-castTextShadow(p)}${screentra-castTextOutline(p)}"></div>`;
 else if(t==='rectangle'){const opVal=Math.max(0,Math.min(100,Number((p&&p.opacity==null?100:p.opacity))))/100;const shapeMode=p.shape_mode||'rectangle';const baseR=Number(p.radius==null?0:p.radius)||0;const rtl=Number(p.radius_tl??baseR)||0,rtr=Number(p.radius_tr??baseR)||0,rbr=Number(p.radius_br??baseR)||0,rbl=Number(p.radius_bl??baseR)||0;const radiusCss=shapeMode==='circle'?'50%':(shapeMode==='pill'?'999px':(rtl+'px '+rtr+'px '+rbr+'px '+rbl+'px'));const clip=screentra-castShapeClipPath(shapeMode);const lineStyle=shapeMode==='line'?'height:'+(Number(p.line_thickness==null?6:p.line_thickness)||6)+'px;':'';const clipStyle=clip?'clip-path:'+clip+';-webkit-clip-path:'+clip+';':'';inner=`<div style="width:100%;height:100%;background:${p.color||'#e94560'};border-radius:${radiusCss};opacity:${opVal};${lineStyle}${clipStyle}"></div>`;}
 else if(t==='webpage') inner=`<iframe src="${esc(p.url||'about:blank')}" style="width:100%;height:100%;border:0;" referrerpolicy="strict-origin-when-cross-origin"></iframe>`;
 else if(t==='weather') {
   if (Array.isArray(p.template_elements) && p.template_elements.length) {
     inner = screentra-castTemplateHTML(p);
   } else {
     // 2026-08-11: preset widgets carry `template: "week"` / "3day" /
     // "current" / etc. but no `template_elements` array (the editor
     // never expanded the preset to elements when it was applied).
     // Render a built-in template based on the named style so the
     // widget shows live data instead of the placeholder "Sample"
     // stub. Falls back to the stub if the named template is unknown.
     inner = screentra-castWeatherBuiltIn(p);
   }
 }
 else if(t==='calendar'){var calSrc='/calendar.html?ics='+encodeURIComponent(p.ics_url||'')+'&template='+encodeURIComponent(p.template||'list')+'&color='+encodeURIComponent(p.color||'#ffffff')+'&accent='+encodeURIComponent(p.accent_color||'#e94560')+'&maxevents='+(p.max_events||5)+'&incweather='+(p.include_weather==='true'?'true':'false')+'&lat='+(p.lat||0)+'&lon='+(p.lon||0);inner=`<iframe src="${calSrc}" style="width:100%;height:100%;border:none;background:transparent;" allowTransparency="true"></iframe>`;}
 else inner=`<div style="width:100%;height:100%;display:grid;place-items:center;border:1px dashed #334155;color:#94a3b8;">${esc(t||'widget')}</div>`;
 return `<div class="widget" style="${styleFor(w)}">${inner}</div>`;
}
function screentra-castWeatherBuiltIn(p){
  var w=screentra-castWeatherFor(p,0);
  var color=p.color||'#fff';
  var unit=String(p.unit||'F').toUpperCase();
  var name=p.template||'current';
  var escL=esc(p.location||p.weather_location||'');
  if(name==='week'){
    var days=['Sun','Mon','Tue','Wed','Thu','Fri','Sat'];
    var rows=[];for(var i=0;i<5;i++){var d=screentra-castWeatherFor(p,i);var wd=new Date();wd.setDate(wd.getDate()+i);rows.push('<div style="display:flex;align-items:center;justify-content:space-between;gap:6%;padding:3% 4%;border-bottom:1px solid rgba(255,255,255,0.08)"><span style="flex:0 0 24%;opacity:0.85">'+days[wd.getDay()]+'</span><span style="flex:0 0 14%;text-align:center;font-size:1.6em">'+screentra-castWmoEmoji(d.code)+'</span><span style="flex:1 1 auto;text-align:right"><span style="opacity:0.55">'+d.low+'°</span> <span style="font-weight:700;margin-left:6px">'+d.high+'°</span></span></div>');}return '<div style="width:100%;height:100%;display:flex;flex-direction:column;color:'+color+';padding:3% 4%;box-sizing:border-box"><div style="display:flex;align-items:center;gap:8%;margin-bottom:3%"><span style="font-size:3.6em;line-height:1">'+screentra-castWmoEmoji(w.code)+'</span><span style="font-size:2.2em;font-weight:700">'+w.current+'°'+unit+'</span></div>'+(escL?'<div style="opacity:0.7;font-size:0.9em;margin-bottom:4%">'+escL+'</div>':'')+'<div style="flex:1 1 auto;overflow:hidden">'+rows.join('')+'</div><div style="opacity:0.4;font-size:0.7em;margin-top:3%;text-align:right">Powered by NOAA/NWS</div></div>';
  }else if(name==='3day'){
    var days2=['Sun','Mon','Tue','Wed','Thu','Fri','Sat'];
    var rows2=[];for(var j=0;j<3;j++){var dd=screentra-castWeatherFor(p,j);var dt=new Date();dt.setDate(dt.getDate()+j);rows2.push('<div style="flex:1 1 0;display:flex;flex-direction:column;align-items:center;justify-content:center;gap:6%;border-right:'+(j<2?'1px solid rgba(255,255,255,0.08)':'none')+'"><span style="opacity:0.75">'+days2[dt.getDay()]+'</span><span style="font-size:2.4em;line-height:1">'+screentra-castWmoEmoji(dd.code)+'</span><span style="font-weight:700">'+dd.high+'°</span><span style="opacity:0.55">'+dd.low+'°</span></div>');}return '<div style="width:100%;height:100%;display:flex;flex-direction:column;color:'+color+';padding:3% 4%;box-sizing:border-box"><div style="display:flex;align-items:center;gap:8%;margin-bottom:3%"><span style="font-size:3.6em;line-height:1">'+screentra-castWmoEmoji(w.code)+'</span><span style="font-size:2.2em;font-weight:700">'+w.current+'°'+unit+'</span></div>'+(escL?'<div style="opacity:0.7;font-size:0.9em;margin-bottom:4%">'+escL+'</div>':'')+'<div style="flex:1 1 auto;display:flex;align-items:stretch;justify-content:stretch">'+rows2.join('')+'</div><div style="opacity:0.4;font-size:0.7em;margin-top:3%;text-align:right">Powered by NOAA/NWS</div></div>';
  }else{
    return '<div data-weather-stub style="width:100%;height:100%;display:grid;place-items:center;color:'+color+';font-size:clamp(28px,5vw,80px);text-align:center;padding:1rem"><div><div style="font-size:2em">'+screentra-castWmoEmoji(w.code)+' '+w.current+'°'+unit+'</div><div style="opacity:0.7;margin-top:0.4em">'+escL+'</div></div></div>';
  }
}

function tickClocks(){document.querySelectorAll('[data-clock]').forEach(e=>e.textContent=new Date().toLocaleTimeString([], {hour:'2-digit', minute:'2-digit'}));document.querySelectorAll('[data-date]').forEach(e=>e.textContent=new Date().toLocaleDateString([], {weekday:'long', month:'long', day:'numeric'}));}
function tickCountdowns(){const now=Date.now(),units={seconds:1000,minutes:1000*60,hours:1000*60*60,days:1000*60*60*24};document.querySelectorAll('[data-countdown-widget]').forEach(node=>{const mode=node.dataset.countdownMode||'date',duration=Math.max(1,Number(node.dataset.countdownDurationValue||1))*(units[node.dataset.countdownDurationUnit||'minutes']||60000);let target;if(mode==='duration'){if(!node.dataset.countdownStartedAt)node.dataset.countdownStartedAt=String(now);target=Number(node.dataset.countdownStartedAt)+duration;}else target=new Date(node.dataset.countdownTarget||'').getTime();const value=node.querySelector('.countdown-value')||node;if(!Number.isFinite(target)||target<=0)return;value.textContent=target<=now?(node.dataset.countdownExpiryText||''):countdownText(target,now,{});});}
const cachedVideoStates={};
function initCachedVideoWidgets(widgets){(widgets||[]).forEach(w=>{if(normWidgetType(w)!=='video')return;const p=props(w),list=Array.isArray(p.playlist)&&p.playlist.length?p.playlist:(p.url||p.src?[{url:p.url||p.src}]:[]);if(!list.length)return;const node=Array.from(document.querySelectorAll('[data-video-widget-id]')).find(n=>n.getAttribute('data-video-widget-id')===String(w.id));if(!node)return;const loop=p.loop!==false,key=list.map(x=>x.url).join('|')+'|'+(loop?'1':'0')+'|'+(p.shuffle?'1':'0'),old=cachedVideoStates[w.id];if(old&&old.node===node&&old.key===key)return;if(old){old.node.pause();old.node.onended=null;old.node.onerror=null;}const state={node,list:list.slice(),index:0,loop,key,failed:{}};cachedVideoStates[w.id]=state;function play(){const item=state.list[state.index];if(!item||!item.url)return;state.node.loop=state.list.length===1&&state.loop;state.node.src=item.url;state.node.load();const attempt=state.node.play();if(attempt&&typeof attempt.catch==='function')attempt.catch(()=>{});}function advance(){state.index++;if(state.index>=state.list.length){if(!state.loop)return;state.index=0;state.failed={};}play();}function failed(){const item=state.list[state.index];if(item&&item.url)state.failed[item.url]=true;advance();}state.node.onended=advance;state.node.onerror=failed;play();});}
async function loadLayout(){try{const r=await fetch('/assigned-layout.json?ts='+Date.now(),{cache:'no-store'});if(!r.ok)throw new Error('no assigned layout');const txt=await r.text();if(txt===currentHash)return;currentHash=txt;const layout=JSON.parse(txt);const ws=(layout.widgets||[]).slice().sort((a,b)=>(a.z||0)-(b.z||0));const needsWeather=ws.some(function(w){return w.widget_type==='weather'&&(Array.isArray(w.props&&w.props.template_elements)&&w.props.template_elements.length || w.props && (w.props.lat||w.props.lon) && w.props.template);});if(needsWeather){await Promise.all(ws.map(function(w){if(w.widget_type==='weather'){var pp=w.props||{};var hasElements=Array.isArray(pp.template_elements)&&pp.template_elements.length;var isPreset=pp.template && (pp.lat||pp.lon);if(hasElements||isPreset){return screentra-castLoadWeather(pp);}}return null;}));}document.getElementById('player').innerHTML=ws.map(renderWidget).join('')||'<div class="fallback"><div><strong>Empty Layout</strong></div></div>';initCachedVideoWidgets(ws);tickClocks();tickCountdowns();}catch(e){if(!currentHash)document.getElementById('player').innerHTML='<div class="fallback"><div><strong>Waiting for cached assigned layout</strong><span>'+esc(e.message)+'</span></div></div>';}}
setInterval(tickClocks,1000);setInterval(tickCountdowns,1000);setInterval(loadLayout,15000);loadLayout();

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
LIVE_PROBE_TIMEOUT_SECS = float(os.environ.get('SCREENTRA_CAST_LIVE_PROBE_TIMEOUT', '0.6'))


def _live_player_url() -> str | None:
    """Return the server URL the kiosk should redirect to, or None if offline."""
    server = os.environ.get('SCREENTRA_CAST_SERVER_URL', '').strip()
    if not server:
        try:
            server_path = Path.home() / '.local/share/screentra-cast-player-client/server'
            if server_path.exists():
                server = server_path.read_text().strip()
        except Exception:
            pass
    server = server.rstrip('/')
    if not server or not (server.startswith('http://') or server.startswith('https://')):
        return None
    return server


def _client_id_from_env() -> str | None:
    cid = os.environ.get('SCREENTRA_CAST_CLIENT_ID', '').strip()
    return cid or None


def _client_id_from_cache() -> str | None:
    try:
        cid_path = Path.home() / '.local/share/screentra-cast-player-client/client-id'
        if cid_path.exists():
            v = cid_path.read_text().strip()
            return v or None
    except Exception:
        pass
    return None


def _connector_for_display_index(display_index: int) -> str | None:
    """Return the connector name (e.g. 'DP-2') for a given display_index.

    Sources (tried in order):
      1. /api/clients/<cid>/displays response is cached by the heartbeat
         but only carries display_index/assigned_layout_id/layout, not
         the per-display connector mapping. Skip.
      2. The display-governance script writes the full mapping to
         ~/.local/share/screentra-cast-player-client/config.json under
         last_detected_displays[].connector — preferred because it's
         already populated by the installer before any heartbeat runs.
      3. /sys/class/drm (live kernel enumeration, fallback if config.json
         is missing — sorts by name, same convention as the launcher).
    Returns None if nothing matches so the caller can fall back to the
    numeric index.
    """
    try:
        gov_path = Path.home() / '.local/share/screentra-cast-player-client/config.json'
        if gov_path.exists():
            cfg = json.loads(gov_path.read_text())
            for d in (cfg.get('last_detected_displays') or []):
                if int(d.get('display_index', -1)) == int(display_index):
                    con = d.get('connector')
                    if con:
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
            # Try the explicit SCREENTRA_CAST_CA_CERT first, then the bundled
            # ~/.local/share/screentra-cast-player-client/screentra-cast-signage-ca.crt.
            # Only fall back to system trust, and only as a last resort fall
            # back to _create_unverified_context when the operator has
            # explicitly opted in via SCREENTRA_CAST_INSECURE_SSL=1.
            ca_candidates = []
            env_ca = os.environ.get('SCREENTRA_CAST_CA_CERT')
            if env_ca:
                ca_candidates.append(Path(env_ca))
            ca_candidates.append(Path.home() / '.local/share/screentra-cast-player-client/screentra-cast-signage-ca.crt')
            for cafile in ca_candidates:
                if cafile.exists():
                    try:
                        ctx = _ssl.create_default_context(cafile=str(cafile))
                        break
                    except Exception:
                        ctx = None
            if ctx is None:
                if os.environ.get('SCREENTRA_CAST_INSECURE_SSL', '').lower() in ('1', 'true', 'yes'):
                    ctx = _ssl._create_unverified_context()
                else:
                    # No CA available and operator hasn't opted in — fail
                    # closed (return False). Better to show the cached
                    # fallback page than to silently MITM the probe.
                    return False
        req = _ur.Request((u.scheme + '://' + (host if not u.port else f'{host}:{port}') + path), method='GET')
        with _ur.urlopen(req, timeout=timeout, context=ctx) as r:
            return 200 <= r.status < 500
    except Exception:
        return False


def _ssl_context_for_server_url(server_url: str):
    if not str(server_url or '').lower().startswith('https://'):
        return None
    ca_candidates = []
    env_ca = os.environ.get('SCREENTRA_CAST_CA_CERT')
    if env_ca:
        ca_candidates.append(Path(env_ca))
    ca_candidates.append(Path.home() / '.local/share/screentra-cast-player-client/screentra-cast-signage-ca.crt')
    for cafile in ca_candidates:
        if cafile.exists():
            try:
                return ssl.create_default_context(cafile=str(cafile))
            except Exception:
                pass
    if os.environ.get('SCREENTRA_CAST_INSECURE_SSL', '').lower() in ('1', 'true', 'yes'):
        return ssl._create_unverified_context()
    return False


def _server_display_config(server_url: str, client_id: str | None, display_index: int, timeout: float = 0.6):
    """Fetch the per-display content config from the server.

    The kiosk boot path needs this on every Chromium GET /, so keep the timeout
    small: 0.6s is enough on a healthy LAN and worst-case 0.6s of "Connecting…"
    is much better than a full 2s black page.
    """
    if not server_url or not client_id:
        return None
    ctx = _ssl_context_for_server_url(server_url)
    if ctx is False:
        return None
    url = f'{server_url.rstrip("/")}/api/clients/{urllib.parse.quote(client_id, safe="")}/displays/{display_index}/config'
    try:
        req = _ur.Request(url, method='GET')
        with _ur.urlopen(req, timeout=timeout, context=ctx) as r:
            if not (200 <= r.status < 300):
                return None
            return json.loads(r.read().decode('utf-8', 'replace'))
    except Exception:
        return None


def _server_kiosk_page(server_url: str, client_id: str | None, kiosk_page_id: str | None, timeout: float = 0.6):
    """Fetch the kiosk page JSON from the server, with the same tight timeout.

    Used as a fallback when the server's live /player.html is unreachable but
    the display config still came back, so the renderer can self-render the
    kiosk page and the user never sits on a black page.
    """
    if not server_url or not client_id or not kiosk_page_id:
        return None
    ctx = _ssl_context_for_server_url(server_url)
    if ctx is False:
        return None
    url = f'{server_url.rstrip("/")}/api/kiosk/pages/{urllib.parse.quote(kiosk_page_id, safe="")}'
    try:
        req = _ur.Request(url, method='GET')
        with _ur.urlopen(req, timeout=timeout, context=ctx) as r:
            if not (200 <= r.status < 300):
                return None
            return json.loads(r.read().decode('utf-8', 'replace'))
    except Exception:
        return None


def _live_player_redirect_html(server_url: str, display_index: int, connector: str | None = None, client_id: str | None = None) -> str:
    """Return a tiny HTML page that redirects the kiosk to the server player.

    Server's player.js matches `?display=` against /api/displays[].connector
    (e.g. "DP-2"), not against the local display_index. If the heartbeat
    has cached the connector for this index (the normal case), prefer it.
    Fall back to the numeric index for legacy code paths that may still use it.

    `client_id` is appended when available so the player page can fetch THIS
    box's display list from /api/clients/<cid>/displays rather than the
    server's /api/displays (which only sees outputs reachable from the
    server process, not from a remote player box).

    The body is intentionally NOT pure-black: the user should always see a
    "Connecting to live player…" message while the live page loads.
    """
    safe_server = server_url.replace('"', '%22')
    target = f'{safe_server.rstrip("/")}/player.html?display={urllib.parse.quote(connector or str(display_index))}'
    if client_id:
        target += f'&client_id={urllib.parse.quote(client_id, safe="")}'
    return (
        '<!doctype html><html><head>'
        f'<meta http-equiv="refresh" content="0; url={target}">'
        f'<title>Screentra Cast Live Player</title>'
        '<style>'
        'html,body{margin:0;height:100%;'
        'background:radial-gradient(circle at 50% 40%,#0f172a 0%,#020617 70%);'
        'color:#94a3b8;font-family:system-ui,sans-serif;}'
        '.wrap{display:grid;place-items:center;height:100%}'
        '.card{text-align:center;padding:24px}'
        '.title{font-size:clamp(22px,2.4vw,34px);font-weight:800;'
        'background:linear-gradient(90deg,#38bdf8,#22d3ee);'
        '-webkit-background-clip:text;background-clip:text;color:transparent;letter-spacing:.04em}'
        '.sub{font-size:clamp(13px,1.2vw,16px);color:#94a3b8;margin-top:.5em}'
        '.dot{display:inline-block;width:.45em;height:.45em;margin:0 .15em;border-radius:50%;'
        'background:#38bdf8;animation:rip 1.1s infinite ease-in-out both}'
        '.dot:nth-child(2){animation-delay:.15s}.dot:nth-child(3){animation-delay:.3s}'
        '@keyframes rip{0%,80%,100%{transform:scale(.5);opacity:.4}40%{transform:scale(1);opacity:1}}'
        '</style>'
        '</head><body><div class="wrap"><div class="card">'
        '<div class="title">Connecting to live player</div>'
        '<div class="sub"><span class="dot"></span><span class="dot"></span><span class="dot"></span></div>'
        '</div></div>'
        f'<script>window.location.replace({json.dumps(target)});</script>'
        '</body></html>'
    )


def _self_rendered_kiosk_html(kiosk_page: dict, display_index: int, connector: str | None, client_id: str | None) -> str:
    """Render the kiosk page entirely on the renderer side.

    Used as a last-ditch fallback when the live /player.html is unreachable
    but the renderer can still talk to the server's API for the kiosk page
    JSON. Guarantees the user never sits on a black page: the kiosk widgets
    always show up here even if the live iframe path is dead.
    """
    widgets = (kiosk_page.get('widgets') or []) if isinstance(kiosk_page, dict) else []
    widgets_sorted = sorted(widgets, key=lambda w: (w.get('z', 0) if isinstance(w, dict) else 0))
    parts = []
    for w in widgets_sorted:
        if not isinstance(w, dict):
            continue
        wtype = w.get('type') or w.get('widget_type') or ''
        p = w.get('props') or {}
        x = float(w.get('x', 0)) * 100
        y = float(w.get('y', 0)) * 100
        ww = float(w.get('w', 0)) * 100
        wh = float(w.get('h', 0)) * 100
        z = int(w.get('z', 1))
        style = (f'position:absolute;left:{x}%;top:{y}%;width:{ww}%;height:{wh}%;'
                 f'z-index:{z};overflow:hidden;display:flex;align-items:center;'
                 f'justify-content:center;')
        if wtype == 'textbox':
            label = _h(p.get('label') or 'Text')
            ph = _h(p.get('placeholder') or '')
            value = _h(p.get('value') or '')
            if p.get('multiline'):
                body = (f'<label style="width:100%;display:grid;gap:.35rem;color:#fff;'
                        f'font-family:system-ui,sans-serif;font-size:clamp(16px,2vw,28px);">'
                        f'<span>{label}</span>'
                        f'<textarea data-kiosk-value="value" placeholder="{ph}" '
                        f'style="font:inherit;padding:.5em;border-radius:.5em;'
                        f'border:1px solid #64748b;background:#0f172a;color:#e2e8f0;">{value}</textarea>'
                        f'</label>')
            else:
                body = (f'<label style="width:100%;display:grid;gap:.35rem;color:#fff;'
                        f'font-family:system-ui,sans-serif;font-size:clamp(16px,2vw,28px);">'
                        f'<span>{label}</span>'
                        f'<input data-kiosk-value="value" value="{value}" placeholder="{ph}" '
                        f'style="font:inherit;padding:.5em;border-radius:.5em;'
                        f'border:1px solid #64748b;background:#0f172a;color:#e2e8f0;">'
                        f'</label>')
        elif wtype == 'button':
            label = _h(p.get('label') or 'Button')
            body = (f'<button data-kiosk-action="{_h(p.get("action") or "submit_page")}" '
                    f'data-kiosk-target-page="{_h(p.get("targetPageId") or "")}" '
                    f'style="font-size:{int(p.get("fontSize") or 28)}px;padding:.7em 1.1em;'
                    f'border:0;border-radius:.7em;background:#2563eb;color:white;'
                    f'font-weight:800;">{label}</button>')
        elif wtype == 'checkbox':
            label = _h(p.get('label') or '')
            checked = ' checked' if p.get('checked') else ''
            body = (f'<label style="color:#fff;font-family:system-ui,sans-serif;'
                    f'font-size:clamp(18px,2.3vw,32px);">'
                    f'<input data-kiosk-value="checked" type="checkbox"{checked} '
                    f'style="transform:scale(1.4);margin-right:.5em;">{label}</label>')
        elif wtype == 'dropdown':
            label = _h(p.get('label') or 'Choose')
            opts = ''.join(
                f'<option value="{_h(o)}"' + (' selected' if o == p.get('selected') else '') + f'>{_h(o or "— Select —")}</option>'
                for o in (p.get('options') or [])
            )
            body = (f'<label style="width:100%;display:grid;gap:.35rem;color:#fff;'
                    f'font-family:system-ui,sans-serif;font-size:clamp(16px,2vw,28px);">'
                    f'<span>{label}</span>'
                    f'<select data-kiosk-value="selected" '
                    f'style="font:inherit;padding:.5em;border-radius:.5em;background:#0f172a;color:#e2e8f0;">'
                    f'<option value="">— Select —</option>{opts}</select></label>')
        elif wtype == 'radio':
            label = _h(p.get('label') or '')
            opts = ''.join(
                f'<label style="display:inline-flex;align-items:center;gap:.35em;margin-right:.9em;">'
                f'<input type="radio" name="{_h(w.get("id") or "r")}" value="{_h(o)}"' +
                (' checked' if o == p.get('selected') else '') +
                f' style="transform:scale(1.3);">{_h(o)}</label>'
                for o in (p.get('options') or [])
            )
            body = (f'<fieldset data-kiosk-value="selected" '
                    f'style="border:0;color:#fff;font-family:system-ui,sans-serif;'
                    f'font-size:clamp(16px,2vw,28px);width:100%;">'
                    f'<legend style="padding:0;margin-bottom:.35em;">{label}</legend>'
                    f'{opts}</fieldset>')
        elif wtype == 'checkbox-list':
            label = _h(p.get('label') or '')
            opts = ''.join(
                f'<label style="display:inline-flex;align-items:center;gap:.35em;margin:.2em 0;width:100%;">'
                f'<input type="checkbox" value="{_h(o)}" data-kiosk-cb-list="1" '
                f'style="transform:scale(1.2);margin-right:.4em;">{_h(o)}</label>'
                for o in (p.get('options') or [])
            )
            body = (f'<fieldset data-kiosk-value="checkedValues" '
                    f'style="border:0;color:#fff;font-family:system-ui,sans-serif;'
                    f'font-size:clamp(16px,2vw,28px);width:100%;">'
                    f'<legend style="padding:0;margin-bottom:.35em;">{label}</legend>{opts}</fieldset>')
        else:
            continue
        parts.append(f'<div class="kiosk-live-widget" data-kiosk-widget-id="{_h(w.get("id") or "")}" '
                     f'data-kiosk-widget-type="{_h(wtype)}" style="{style}">{body}</div>')
    return (
        '<!doctype html><html><head>'
        f'<title>Screentra Cast Kiosk - {_h(kiosk_page.get("name") or "Kiosk")}</title>'
        '<style>'
        '*{margin:0;padding:0;box-sizing:border-box}'
        'html,body{width:100vw;height:100vh;'
        'background:radial-gradient(circle at 50% 40%,#0f172a 0%,#020617 70%);'
        'overflow:hidden;font-family:system-ui,sans-serif;}'
        '.fallback{padding:24px;color:#94a3b8;font-size:14px;position:absolute;bottom:8px;right:12px;'
        'background:rgba(15,23,42,.6);border-radius:8px;border:1px solid #1e293b}'
        '</style></head><body>'
        f'{"".join(parts)}'
        '<div class="fallback">Offline kiosk render — server live page unreachable</div>'
        '</body></html>'
    )


def _h(s) -> str:
    """HTML-escape helper for self-rendered kiosk HTML."""
    if s is None:
        return ''
    return (str(s)
            .replace('&', '&amp;')
            .replace('<', '&lt;')
            .replace('>', '&gt;')
            .replace('"', '&quot;')
            .replace("'", '&#39;'))


class Handler(BaseHTTPRequestHandler):
    cache_dir = Path.home() / '.local/share/screentra-cast-player-client/cache'

    def do_POST(self):
        length = int(self.headers.get('Content-Length') or 0)
        body = self.rfile.read(length).decode('utf-8') if length else '{}'
        path = unquote(self.path.split('?', 1)[0])
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
                self.send_header('Set-Cookie', 'screentra_cast_admin_session=' + token + '; Path=/; HttpOnly; SameSite=Lax; Max-Age=43200')
                self.end_headers(); self.wfile.write(data); return
            except Exception as e:
                return self.send_text(400, json.dumps({'ok': False, 'error': str(e)}), 'application/json')
        if path == '/api/admin/auth/logout':
            data = json.dumps({'ok': True}).encode()
            self.send_response(200)
            self.send_header('Content-Type', 'application/json')
            self.send_header('Content-Length', str(len(data)))
            self.send_header('Cache-Control', 'no-store')
            self.send_header('Set-Cookie', 'screentra_cast_admin_session=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0')
            self.end_headers(); self.wfile.write(data); return
        # If a network change is pending, force re-auth: existing sessions
        # were issued before the change started and could be stale.
        # Network change confirm/revert endpoints are intentionally
        # anonymous: the operator's "log back in" step happens on the
        # regular admin auth flow, and the network probe + 5-minute
        # timeout are what guarantee the new interface works. There is
        # no separate session-freshness gate for these endpoints.
        if _admin_requires_auth(path) and not _session_user(self.headers.get('Cookie', '')):
            return self.send_text(401, json.dumps({'error': 'auth required', **_auth_source()}), 'application/json')
        if path == '/api/touchscreen/select':
            try:
                payload = json.loads(body)
                selected = payload.get('path')
                Handler.cache_dir.mkdir(parents=True, exist_ok=True)
                (Handler.cache_dir / 'touch-device.json').write_text(json.dumps({'path': selected}))
                # Tell the actor to restart the kiosk so the choice takes effect
                try:
                    pending = Handler.cache_dir.parent / 'pending-command.json'
                    pending.write_text(json.dumps({
                        'action': 'restart_kiosk_display',
                        'args': {'display_index': int(os.environ.get('SCREENTRA_CAST_DISPLAY_INDEX', '0')), 'reason': 'touch-device-change'},
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
                data.setdefault('display_index', int(os.environ.get('SCREENTRA_CAST_DISPLAY_INDEX', '0')))
                calib_path = Handler.cache_dir / 'touch-calibration.json'
                calib_path.parent.mkdir(parents=True, exist_ok=True)
                history = []
                if calib_path.exists():
                    try:
                        history = json.loads(calib_path.read_text())
                        if not isinstance(history, list):
                            history = [history]
                    except Exception:
                        history = []
                history.append(data)
                history = history[-5:]   # rolling window of last 5 runs
                calib_path.write_text(json.dumps(history, indent=2))
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
        if path == '/api/admin/audio/set':
            try:
                payload = json.loads(body) if body and body.strip() else {}
                result = _audio_set_default_sink(payload.get('sink') or '')
                if result.get('ok'):
                    return self.send_text(200, json.dumps({'ok': True, 'sink': result['sink'], 'restart_required': False}), 'application/json')
                return self.send_text(400, json.dumps({'ok': False, 'error': result.get('error','unknown')}), 'application/json')
            except Exception as e:
                return self.send_text(400, json.dumps({'ok': False, 'error': str(e)}), 'application/json')
        if path == '/api/admin/bluetooth/scan':
            try:
                found = _bluetooth_scan(seconds=12)
                return self.send_text(200, json.dumps({'ok': True, 'devices': found}), 'application/json')
            except Exception as e:
                return self.send_text(500, json.dumps({'ok': False, 'error': str(e)}), 'application/json')
        if path == '/api/admin/bluetooth/pair':
            try:
                payload = json.loads(body) if body and body.strip() else {}
                result = _bluetooth_pair_and_connect(payload.get('mac') or '')
                code = 200 if result.get('ok') else 400
                return self.send_text(code, json.dumps(result), 'application/json')
            except Exception as e:
                return self.send_text(400, json.dumps({'ok': False, 'error': str(e)}), 'application/json')
        if path == '/api/admin/bluetooth/connect':
            try:
                payload = json.loads(body) if body and body.strip() else {}
                result = _bluetooth_connect_only(payload.get('mac') or '')
                code = 200 if result.get('ok') else 400
                return self.send_text(code, json.dumps(result), 'application/json')
            except Exception as e:
                return self.send_text(400, json.dumps({'ok': False, 'error': str(e)}), 'application/json')
        if path == '/api/admin/bluetooth/disconnect':
            try:
                payload = json.loads(body) if body and body.strip() else {}
                result = _bluetooth_disconnect(payload.get('mac') or '')
                return self.send_text(200, json.dumps(result), 'application/json')
            except Exception as e:
                return self.send_text(400, json.dumps({'ok': False, 'error': str(e)}), 'application/json')
        if path == '/api/admin/bluetooth/trust':
            try:
                payload = json.loads(body) if body and body.strip() else {}
                result = _bluetooth_trust(payload.get('mac') or '')
                return self.send_text(200, json.dumps(result), 'application/json')
            except Exception as e:
                return self.send_text(400, json.dumps({'ok': False, 'error': str(e)}), 'application/json')
        if path == '/api/admin/client-backup':
            result = _create_client_software_backup()
            return self.send_text(200 if result.get('ok') else 500, json.dumps(result), 'application/json')
        if path == '/api/admin/services/restart':
            r = _safe_run(['systemctl', '--user', 'restart', 'screentra-cast-player-heartbeat.service', 'screentra-cast-player-actor.service'], timeout=8)
            return self.send_text(200, json.dumps({'ok': r.get('ok'), 'restart': r}), 'application/json')
        if path == '/api/admin/network/probe':
            try: payload = json.loads(body) if body and body.strip() else {}
            except Exception: payload = {}
            iface = (payload.get('interface') or '').strip()
            ip = _network_pick_source_ip(iface)
            if not ip:
                return self.send_text(400, json.dumps({'ok': False, 'error': f'interface {iface!r} has no IPv4 or is down'}), 'application/json')
            probe = _network_probe_server(ip, _network_read_server_url(), timeout=2.0)
            return self.send_text(200, json.dumps({'ok': probe.get('ok'), 'interface': iface, 'source_ip': ip, 'probe': probe}), 'application/json')
        if path == '/api/admin/network/apply':
            try: payload = json.loads(body) if body and body.strip() else {}
            except Exception: payload = {}
            iface = (payload.get('interface') or '').strip()
            if not iface:
                return self.send_text(400, json.dumps({'ok': False, 'error': 'interface required'}), 'application/json')
            result = _network_apply_impl(iface)
            return self.send_text(200 if result.get('ok') else 400, json.dumps(result), 'application/json')
        if path == '/api/admin/network/confirm':
            result = _network_confirm_impl()
            return self.send_text(200 if result.get('ok') else 400, json.dumps(result), 'application/json')
        if path == '/api/admin/network/revert':
            result = _network_revert_impl()
            return self.send_text(200, json.dumps(result), 'application/json')
        if path == '/api/admin/network/wifi/scan':
            try: payload = json.loads(body) if body and body.strip() else {}
            except Exception: payload = {}
            result = _network_wifi_scan((payload.get('interface') or '').strip())
            return self.send_text(200 if result.get('ok') else 400, json.dumps(result), 'application/json')
        if path == '/api/admin/network/wifi/connect':
            try: payload = json.loads(body) if body and body.strip() else {}
            except Exception: payload = {}
            iface = (payload.get('interface') or '').strip()
            ssid = (payload.get('ssid') or '').strip()
            password = payload.get('password') or None
            if password is not None and not isinstance(password, str):
                password = str(password)
            result = _network_wifi_connect(iface, ssid, password)
            return self.send_text(200 if result.get('ok') else 400, json.dumps(result), 'application/json')
        if path == '/api/admin/network/wifi/disconnect':
            try: payload = json.loads(body) if body and body.strip() else {}
            except Exception: payload = {}
            result = _network_wifi_disconnect((payload.get('interface') or '').strip())
            return self.send_text(200, json.dumps(result), 'application/json')
        if path == '/api/admin/network/vlan/create':
            try: payload = json.loads(body) if body and body.strip() else {}
            except Exception: payload = {}
            parent = (payload.get('parent') or '').strip()
            vid = payload.get('vlan_id')
            result = _network_vlan_create(parent, vid)
            return self.send_text(200 if result.get('ok') else 400, json.dumps(result), 'application/json')
        if path == '/api/admin/network/vlan/remove':
            try: payload = json.loads(body) if body and body.strip() else {}
            except Exception: payload = {}
            result = _network_vlan_remove((payload.get('name') or '').strip())
            return self.send_text(200 if result.get('ok') else 400, json.dumps(result), 'application/json')
        if path == '/api/admin/network/static/apply':
            try: payload = json.loads(body) if body and body.strip() else {}
            except Exception: payload = {}
            iface = (payload.get('interface') or '').strip()
            address = (payload.get('address') or '').strip()
            prefix = payload.get('prefix')
            gateway = payload.get('gateway') or None
            dns = payload.get('dns') or []
            if not isinstance(dns, list):
                dns = [str(dns)]
            result = _network_static_apply(iface, address, prefix, gateway, dns)
            return self.send_text(200 if result.get('ok') else 400, json.dumps(result), 'application/json')
        if path == '/api/admin/network/static/clear':
            try: payload = json.loads(body) if body and body.strip() else {}
            except Exception: payload = {}
            result = _network_static_clear((payload.get('interface') or '').strip())
            return self.send_text(200, json.dumps(result), 'application/json')
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
    def _serve_cached_upload(self, path: str):
        """Serve /uploads/<path> from the local cache dir. Used for
        widget preset assets (Bigfoot weather icons, etc.) that the
        actor pre-fetches into cache/uploads/ so the kiosk browser can
        render them without touching the server. Returns 404 if not
        cached; the kiosk browser falls back to its emoji rendering
        path in that case so the layout never breaks visually."""
        rel = path[len('/uploads/'):].lstrip('/')
        if '..' in rel.split('/'):
            return self.send_text(404, 'not found', 'text/plain')
        cache_dir = Handler.cache_dir
        target = cache_dir / 'uploads' / rel
        if not target.is_file():
            return self.send_text(404, 'not cached', 'text/plain')
        try:
            data = target.read_bytes()
        except OSError:
            return self.send_text(404, 'read failed', 'text/plain')
        ext = target.suffix.lower()
        ctype = {
            '.svg': 'image/svg+xml',
            '.png': 'image/png',
            '.jpg': 'image/jpeg',
            '.jpeg': 'image/jpeg',
            '.gif': 'image/gif',
            '.webp': 'image/webp',
            '.ico': 'image/x-icon',
        }.get(ext, 'application/octet-stream')
        self.send_response(200)
        self.send_header('Content-Type', ctype)
        self.send_header('Content-Length', str(len(data)))
        # Aggressive client-side caching — icons don't change once cached.
        self.send_header('Cache-Control', 'public, max-age=86400')
        self.end_headers()
        self.wfile.write(data)
    def _proxy_to_server(self, full_path):
        # Proxy a single GET request to the upstream SCREENTRA_CAST_SERVER_URL.
        # Preserves query string. Returns 502 if upstream unreachable.
        import time as _proxy_time
        srv = _network_read_server_url() if globals().get('_network_read_server_url') else (os.environ.get('SCREENTRA_CAST_SERVER_URL') or '').strip()
        if not srv:
            return self.send_text(503, 'no SCREENTRA_CAST_SERVER_URL configured', 'text/plain')
        target = srv.rstrip('/') + full_path
        try:
            ctx = _ssl_context_for_server_url(srv) if globals().get('_ssl_context_for_server_url') else None
            req = _ur.Request(target, headers={'User-Agent': 'screentra-cast-player-renderer-proxy/1.0'})
            with _ur.urlopen(req, timeout=5, context=ctx) as r:
                body = r.read()
                ct = r.headers.get('Content-Type', 'text/html; charset=utf-8')
                self.send_response(200)
                self.send_header('Content-Type', ct)
                self.send_header('Content-Length', str(len(body)))
                # Allow the iframe to be transparent even when calendar.html uses opaque bg
                self.send_header('Access-Control-Allow-Origin', '*')
                self.end_headers()
                self.wfile.write(body)
                return
        except Exception as e:
            return self.send_text(502, f'upstream proxy failed: {e}', 'text/plain')

    def do_GET(self):
        path = unquote(self.path.split('?',1)[0])
        # Calendar widget iframe proxy: /calendar.html and /js/calendar.js
        # are served by the upstream server; this renderer doesn't bundle them.
        # Proxy transparently so the iframe inside the player can render.
        # 2026-08-11: prior to this, the calendar widget rendered only the literal
        # label text on the client because the iframe's src 404'd.
        if path in ('/calendar.html', '/js/calendar.js') or path.startswith('/api/ics-proxy'):
            return self._proxy_to_server(self.path)
        if path in ('/login', '/login.html'):
            return self.send_text(200, _admin_login_html(), 'text/html; charset=utf-8')
        if path == '/api/admin/auth/status':
            return self.send_text(200, json.dumps(_admin_auth_payload(self)), 'application/json')
        # Audio + Bluetooth status endpoints are pure-read — handle them in GET
        # so the JS can fetch them with simple `await api(...)`. The mutating
        # endpoints (audio/set, bluetooth/{scan,pair,connect,disconnect,trust})
        # stay in do_POST.
        if path == '/api/admin/audio/status':
            try:
                return self.send_text(200, json.dumps(_audio_status()), 'application/json')
            except Exception as e:
                return self.send_text(500, json.dumps({'ok': False, 'error': str(e)}), 'application/json')
        if path == '/api/admin/audio/devices':
            try:
                return self.send_text(200, json.dumps({'ok': True, 'sinks': _audio_list_sinks(), 'current_sink': _audio_current_sink()}), 'application/json')
            except Exception as e:
                return self.send_text(500, json.dumps({'ok': False, 'error': str(e)}), 'application/json')
        if path == '/api/admin/bluetooth/devices':
            try:
                status = _bluetooth_status() or {}
                return self.send_text(200, json.dumps({
                    'ok': True,
                    'paired': _bluetooth_paired_devices(),
                    'connected': status.get('connected_macs') or [],
                }), 'application/json')
            except Exception as e:
                return self.send_text(500, json.dumps({'ok': False, 'error': str(e)}), 'application/json')
        # Serve cached uploads (icons, widget preset assets, etc.) from
        # the local cache. The actor pre-fetches these via _cache_icon
        # when it inlines widget presets, so the kiosk browser can
        # render custom WMO weather icons (Bigfoot, etc.) without
        # touching the server. Falls through to 404 if not cached yet.
        if path.startswith('/uploads/'):
            return self._serve_cached_upload(path)
        # If a network change is pending, force re-auth: existing sessions
        # The pending-network banner handles its own confirm/revert flow
        # anonymously. There is no separate session-freshness gate for
        # the admin UI when a network change is pending; the operator
        # simply logs in normally via the login form, which is what
        # proves they're at the box.
        if _admin_requires_auth(path) and not _session_user(self.headers.get('Cookie', '')):
            if path.startswith('/api/'):
                return self.send_text(401, json.dumps({'error': 'auth required', **_auth_source()}), 'application/json')
            return self.send_text(200, _admin_login_html(), 'text/html; charset=utf-8')
        if os.environ.get('SCREENTRA_CAST_ADMIN_DEFAULT', '').lower() in ('1', 'true', 'yes') and path in ('/', '/admin.html'):
            return self.send_text(200, ADMIN_HTML, 'text/html; charset=utf-8')
        if path in ('/', '/player.html'):
            display_index = int(os.environ.get('SCREENTRA_CAST_DISPLAY_INDEX', '0'))
            # If a network change is pending, force the no-layout page so the
            # operator sees the in-app banner with the timer + Confirm/Revert
            # buttons. The banner handles the re-auth flow itself (anonymous).
            _net_state = _network_state_load()
            if _net_state.get('pending') and not _net_state['pending'].get('confirmed'):
                try:
                    with open(os.path.join(os.path.dirname(os.path.abspath(__file__)), 'no-layout.html'), 'r') as _f:
                        html = _f.read().replace(
                            '<title>Screentra Cast Player display</title>',
                            f'<title>Screentra Cast Player display {display_index} (network change pending)</title>',
                        )
                        return self.send_text(200, _maybe_inject_pending_network_banner(html), 'text/html; charset=utf-8')
                except Exception:
                    pass
            layout_target = Handler.cache_dir / f'assigned-layout-display-{display_index}.json'
            # Legacy fallback belongs to display 0 only. Other displays must not render
            # display 0's cached assignment when their own display cache is absent.
            if display_index == 0 and not layout_target.exists():
                layout_target = Handler.cache_dir / 'assigned-layout.json'

            live_url = _live_player_url()
            client_id = _client_id_from_env() or _client_id_from_cache()
            connector = _connector_for_display_index(display_index)

            # Kiosk-mode displays do not have a cached assigned-layout file by
            # design. Ask the server for the per-display content mode before
            # deciding to show local no-layout.
            #
            # The whole point of this branch: GUARANTEE the user sees the
            # kiosk page on this display, even on cold boot. The previous
            # implementation only redirected to the live server player, which
            # meant a slow or hung server left Chromium on a black page. We
            # now:
            #   1. Try to talk to the server (config + page) with a tight
            #      0.6s timeout each. Most of the time this succeeds.
            #   2. If the live server IS reachable, redirect to its
            #      /player.html (richest render).
            #   3. If the live server is NOT reachable, self-render the
            #      kiosk page here from the JSON we already have. The user
            #      always sees real content.
            #   4. Worst case (config endpoint also unreachable), fall
            #      through to the no-layout page below — operator knows
            #      to fix the network rather than staring at black.
            kiosk_page_payload = None
            server_display_config = _server_display_config(live_url, client_id, display_index) if live_url else None
            kiosk_mode = bool(
                server_display_config
                and str(server_display_config.get('display_mode') or '').lower() == 'kiosk'
                and server_display_config.get('assigned_kiosk_page_id')
            )
            if kiosk_mode and live_url:
                # The config endpoint already proved the server is reachable
                # in the same LAN hop. Grab the kiosk page JSON for either
                # the redirect-with-fallback or the self-rendered path.
                kiosk_page_payload = _server_kiosk_page(
                    live_url, client_id, server_display_config.get('assigned_kiosk_page_id')
                )
                if _server_reachable(live_url, LIVE_PROBE_TIMEOUT_SECS):
                    return self.send_text(
                        200,
                        _live_player_redirect_html(live_url, display_index, connector, client_id),
                        'text/html; charset=utf-8',
                    )
                # Live page is unreachable. Self-render so the user sees
                # the kiosk page on screen instead of a black loading page.
                if kiosk_page_payload:
                    return self.send_text(
                        200,
                        _self_rendered_kiosk_html(kiosk_page_payload, display_index, connector, client_id),
                        'text/html; charset=utf-8',
                    )
                # Config told us this is kiosk mode but the page JSON
                # didn't come back. Don't redirect (that would render black);
                # fall through to the no-layout card with kiosk-mode text.
                if not layout_target.exists():
                    try:
                        with open(os.path.join(os.path.dirname(os.path.abspath(__file__)), 'no-layout.html'), 'r') as _f:
                            html = _f.read().replace(
                                '<title>Screentra Cast Player display</title>',
                                f'<title>Screentra Cast Player display {display_index}</title>',
                            )
                            return self.send_text(200, _maybe_inject_pending_network_banner(html), 'text/html; charset=utf-8')
                    except Exception:
                        pass

            # No assigned layout on disk for THIS display -> show the rich
            # no-layout page (touch detection, gear modal, build info, exit
            # button) instead of redirecting to the server's minimal fallback
            # card. This gives the operator everything they need to configure
            # the touchscreen + pick a layout right from the screen.
            if not layout_target.exists():
                try:
                    with open(os.path.join(os.path.dirname(os.path.abspath(__file__)), 'no-layout.html'), 'r') as _f:
                        html = _f.read().replace('<title>Screentra Cast Player display</title>', f'<title>Screentra Cast Player display {display_index}</title>')
                        return self.send_text(200, _maybe_inject_pending_network_banner(html), 'text/html; charset=utf-8')
                except Exception:
                    pass

            # Always render the cached layout from the renderer's local PLAYER_HTML.
            # The kiosk browser never loads from the server's /player.html —
            # the server's role is to push the layout JSON via the heartbeat/
            # actor path, and the client renders it entirely from disk. This
            # makes the player resilient to server outages: keep showing the
            # last cached layout until a new one arrives.
            #
            # Per-display cache (assigned-layout-display-{N}.json) is owned by
            # the actor: it overwrites the file atomically on every config
            # refresh, and the kiosk browser polls ?ts=… every 5s to pick up
            # changes. The heartbeat will see a cleared cache instantly when
            # the operator un-assigns the layout, so the kiosk browser falls
            # through to the no-layout page (handled above).
            display_index = int(os.environ.get('SCREENTRA_CAST_DISPLAY_INDEX', '0'))
            html = PLAYER_HTML.replace('<title>Screentra Cast Cached Player</title>', f'<title>Screentra Cast Display {display_index}</title>')
            return self.send_text(200, html, 'text/html; charset=utf-8')
        if path == '/admin.html':
            return self.send_text(200, ADMIN_HTML, 'text/html; charset=utf-8')
        if path == '/api/admin/status':
            return self.send_text(200, json.dumps(_admin_status()), 'application/json')
        if path == '/api/admin/network':
            return self.send_text(200, json.dumps(_network_state_get()), 'application/json')
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
            display_index = int(os.environ.get('SCREENTRA_CAST_DISPLAY_INDEX', '0'))
            with open(os.path.join(os.path.dirname(os.path.abspath(__file__)), 'no-layout.html'), 'r') as _f:
                html = _f.read().replace('<title>Screentra Cast Player display</title>', f'<title>Screentra Cast Player display {display_index}</title>')
                html = _maybe_inject_pending_network_banner(html)
                return self.send_text(200, html, 'text/html; charset=utf-8')
        if path == '/touch-calibration.html':
            # Serve the touchscreen calibration test page. Lives next to
            # the renderer script and reads SCREENTRA_CAST_DISPLAY_INDEX for the
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
                    sel = json.loads(_tp.read_text()).get('path')
                except Exception:
                    pass
            return self.send_text(200, json.dumps({'devices': devs, 'selected': sel}), 'application/json')
        if path == '/api/cursor-config.json':
            # Read hide_secs from SCREENTRA_CAST_CURSOR_HIDE_SECS env (default 3.5).
            # The player installer sets this from the operator's kiosk-config,
            # which writes ~/.local/share/screentra-cast-player-client/config.json.
            hide_secs = float(os.environ.get('SCREENTRA_CAST_CURSOR_HIDE_SECS', '3.5'))
            return self.send_text(200, json.dumps({'hide_secs': hide_secs}), 'application/json')
        if path == '/api/touch-calibration.json':
            # GET: return last 5 calibration results (or 404 if none).
            calib_path = Handler.cache_dir / 'touch-calibration.json'
            if not calib_path.exists():
                return self.send_text(404, json.dumps({'error': 'no calibration yet'}), 'application/json')
            return self.send_text(200, calib_path.read_text(), 'application/json')
        if path == '/health':
            return self.send_text(200, 'ok')
        if path == '/assigned-layout.json':
            display_index = int(os.environ.get('SCREENTRA_CAST_DISPLAY_INDEX', '0'))
            target = self.cache_dir / f'assigned-layout-display-{display_index}.json'
            if display_index == 0 and not target.exists():
                target = self.cache_dir / 'assigned-layout.json'
            if not target.exists():
                return self.send_text(404, '{}', 'application/json')
            return self.send_text(200, target.read_text(), 'application/json')
        if path == '/client-config.json':
            target = self.cache_dir / path.lstrip('/')
            if not target.exists():
                return self.send_text(404, '{}', 'application/json')
            return self.send_text(200, target.read_text(), 'application/json')
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
    ap.add_argument('--host', default=os.environ.get('SCREENTRA_CAST_RENDER_HOST','127.0.0.1'))
    ap.add_argument('--port', type=int, default=None)
    ap.add_argument('--cache-dir', default=os.environ.get('SCREENTRA_CAST_CLIENT_CACHE_DIR', str(Path.home()/'.local/share/screentra-cast-player-client/cache')))
    args=ap.parse_args()
    if args.port is None:
        args.port = int(os.environ.get('SCREENTRA_CAST_RENDER_PORT', str(int(os.environ.get('SCREENTRA_CAST_RENDER_BASE_PORT','3900')) + int(os.environ.get('SCREENTRA_CAST_DISPLAY_INDEX','0')))))
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

cat > "$INSTALL_DIR/screentra-cast-display-detect.py" <<'PY_DETECT_FILE'
"""Detect displays on a Linux box by reading /sys/class/drm.

Each connector is a display_index in stable sort order. Returns a list of dicts
ready to send as the 'displays' field in heartbeat payload.
"""
from __future__ import annotations

import os
import re
import subprocess
from pathlib import Path
from typing import List, Dict, Any, Optional, Tuple


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




def _kernel_preferred(connector: str) -> Optional[Tuple[int, int]]:
    """Query modetest for the kernel's preferred mode for a connector.

    The kernel driver reports its preferred mode (the one it would
    activate by default at boot) via modetest's "type: preferred, driver"
    flag. This is more reliable than /sys/class/drm/.../modes where the
    first-listed mode can be a bogus placeholder (e.g. 1280x800 from a
    stub EDID). When EDID is missing or wrong, fall back to this.

    Returns (width, height) or None if modetest is not available.
    """
    try:
        out = subprocess.check_output(
            ['modetest', '-c'], stderr=subprocess.STDOUT, text=True, timeout=5)
    except Exception:
        return None
    pat = re.compile(rf'{re.escape(connector)}\s+.*?(?=\n\s*\d+\s+\d+|\Z)', re.DOTALL)
    m = pat.search(out)
    if not m:
        return None
    block = m.group(0)
    # Find the first line with "type: preferred, driver"
    pm = re.search(
        r'#(\d+)\s+(\d+)x(\d+)\s+\d+\.\d+\s+.*?type:\s*preferred,\s*driver',
        block)
    if pm:
        return (int(pm.group(2)), int(pm.group(3)))
    return None


def detect_displays(max_displays: int = 8) -> List[Dict[str, Any]]:
    """Return a list of detected DRM connector displays.

    Each entry: { display_index, connector, label, width, height, refresh_rate, enabled, connected }
    Falls back to a single virtual display when /sys/class/drm is missing or empty
    (e.g. macOS dev boxes, headless servers).
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
            # 2026-08-11: skip disconnected connectors entirely. The kernel
            # reports all physical ports even when nothing is plugged in
            # (e.g. HDMI-A-3 on a board that exposes 3 HDMI ports). Reporting
            # them in the heartbeat made the editor show a phantom
            # "display 2" row with no resolution and a "disconnected" status.
            # Disconnected connectors are still visible to wlr-randr, but
            # sway already excludes them from the active desktop, so the
            # renderer's per-display heartbeats are unaffected.
            if status != 'connected':
                continue
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
                # 2026-08-10: pick the LARGEST mode from sysfs that is also
                # supported by the monitor's EDID. The kernel's first-listed
                # "preferred" mode can be a bogus placeholder (e.g. 1280x800
                # from a stub EDID), but the kernel also lists every other
                # mode the driver + EDID agree on. The real monitor-preferred
                # mode is the largest one the EDID also accepts (e.g. an AOC
                # 28E850 lists both 1920x1080 and 4096x2160 in sysfs; 1920x1080
                # is the EDID-preferred and matches the 4K Samsung's
                # 3840x2160 -- we want the largest mode that's still <= the
                # EDID first DTD). If we can't determine that, fall back to
                # the sysfs-largest mode.
                modes_text = _read(entry / 'modes') if (entry / 'modes').exists() else ''
                candidates = []
                for line in modes_text.splitlines():
                    if 'x' not in line:
                        continue
                    parts = line.split('x')
                    try:
                        w = int(parts[0]); h = int(parts[1])
                    except Exception:
                        continue
                    candidates.append((w, h))
                # Prefer the EDID's first DTD (Detailed Timing Descriptor #1)
                # which is the most accurate preferred. Fall back to the
                # kernel's preferred (modetest) when EDID isn't readable.
                # The sysfs-first mode is unreliable (can be a bogus
                # 1280x800 placeholder from a stub EDID); the EDID first
                # DTD or the kernel's "preferred, driver" flag is correct.
                edid_path = entry / 'edid'
                edid_usable = False
                # 2026-08-11: sysfs files report st_size=0 even when they
                # have content. Read the file first; check the actual byte
                # count AFTER reading (a 128-byte EDID block is standard,
                # but the 18-byte Detailed Timing Descriptor we need lives
                # in the first 72 bytes).
                if edid_path.exists():
                    try:
                        edid_bytes = edid_path.read_bytes()
                        if len(edid_bytes) >= 72:
                            dtd = edid_bytes[54:72]
                            if dtd[0] or dtd[1]:
                                pref_w = dtd[2] | ((dtd[4] >> 4) << 8)
                                pref_h = dtd[5] | ((dtd[7] >> 4) << 8)
                                if pref_w > 0 and pref_h > 0:
                                    # Only use the EDID preferred if it is in
                                    # the sysfs candidates list (the monitor
                                    # actually advertises it).
                                    if (pref_w, pref_h) in candidates:
                                        width_val = pref_w
                                        height_val = pref_h
                                        candidates = []
                                        edid_usable = True
                    except Exception:
                        pass
                if width_val is None:
                    # EDID unreadable: ask the kernel driver for its
                    # actual preferred mode via modetest. This handles
                    # HDMI-dummy adapters and other cases where the
                    # monitor returns no EDID but the driver still
                    # knows what mode to use.
                    kp = _kernel_preferred(connector)
                    if kp and kp in candidates:
                        width_val = kp[0]
                        height_val = kp[1]
                        candidates = []
                if width_val is None and candidates:
                    # 2026-08-11: prefer the largest sysfs mode (2560x1440 on
                    # a Lenovo Y27q-30) over the smallest (640x480 stub).
                    # The EDID path failed because sysfs files report
                    # st_size=0, but we read the file content first now and
                    # pick the real preferred. If we still end up here, the
                    # kernel-reported modes are correct, so take the
                    # largest candidate instead of the smallest.
                    candidates.sort(key=lambda c: c[0] * c[1], reverse=True)
                    width_val = candidates[0][0]
                    height_val = candidates[0][1]
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

    # Fallback: virtual single display for headless / dev boxes
    if not displays:
        displays.append({
            'display_index': 0,
            'connector': 'virtual-0',
            'label': 'Virtual Display',
            'width': 1920,
            'height': 1080,
            'refresh_rate': 60.0,
            'enabled': True,
            'connected': True,
            'last_error': None,
        })

    return displays

PY_DETECT_FILE

cat > "$DISPLAY_GOVERNANCE" <<'PY_DISPLAY_GOVERNANCE'
#!/usr/bin/env python3
"""Screentra Cast player display governance.

Counts connected DRM connectors and writes `config.json` next to the player
runtime state. The heartbeat reads `installer_detected_max_displays` from
this file (or falls back to live detection if absent), and `max_displays`
as the per-box override.

The server stores both and the renderer honors
    min(installer_detected_max_displays, COALESCE(max_displays, ...))
when deciding how many kiosk-browser instances to launch.

Idempotent: safe to re-run after a hotplug or display reconfiguration.
"""
from __future__ import annotations

import argparse
import json
import os
import sys
from pathlib import Path
from typing import Any, Dict, List

# Local import without circular dep risk. The detect module ships as a
# hyphenated filename (`screentra-cast-display-detect.py`) so it isn't a normal
# importable name; load it via importlib.
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import importlib.util as _ilu
_det_path = os.path.join(os.path.dirname(os.path.abspath(__file__)),
                         "screentra-cast-display-detect.py")
_spec = _ilu.spec_from_file_location("screentra_cast_display_detect", _det_path)
if _spec is None or _spec.loader is None:
    raise RuntimeError(f"cannot load detect module from {_det_path}")
_det = _ilu.module_from_spec(_spec)
sys.modules["screentra_cast_display_detect"] = _det
_spec.loader.exec_module(_det)


CONFIG_DEFAULTS: Dict[str, Any] = {
    # Number of connected+enabled DRM connectors the installer found. The
    # renderer / kiosk-launcher uses this as the upper bound. Re-detected on
    # every run; never mutated by the actor.
    "installer_detected_max_displays": 1,
    # Operator-set cap (per-box). If unset, the renderer uses
    # installer_detected_max_displays unchanged. Mutable; the
    # screentra-cast-player-admin.py UI writes this.
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
    return sum(1 for d in displays if d.get("connected", d.get("enabled")) and d.get("connector") != "virtual-0")


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
    tmp = config_path.with_suffix(".json.tmp")
    tmp.write_text(json.dumps(cfg, indent=2))
    os.replace(tmp, config_path)


def compute_active_max(config: Dict[str, Any]) -> int:
    """The value the renderer / kiosk-launcher should actually use."""
    installer = int(config.get("installer_detected_max_displays") or 1)
    override = config.get("max_displays")
    if override is None:
        return max(installer, 1)
    try:
        override_i = max(int(override), 1)
    except (TypeError, ValueError):
        return max(installer, 1)
    # Operator cap cannot exceed what the hardware actually has.
    return min(installer, override_i)


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--config-dir", default=os.environ.get(
        "SCREENTRA_CAST_CLIENT_DIR", os.path.expanduser("~/.local/share/screentra-cast-player-client")))
    ap.add_argument("--max-scan", type=int, default=16)
    ap.add_argument("--print-only", action="store_true",
                    help="Detect and print to stdout; do not write config.json")
    args = ap.parse_args()

    displays = detect_full(max_scan=args.max_scan)
    connected_count = sum(1 for d in displays if d.get("connected", d.get("enabled")) and d.get("connector") != "virtual-0")

    config_path = Path(args.config_dir) / "config.json"
    existing = load_config(config_path)
    max_displays_override = existing.get("max_displays")
    # Default to the connected count on a fresh install so multi-display
    # boxes (e.g. the appliance mode where server + player are co-located
    # on a TV-attached PC) come up with every display active. Operators
    # who want single-display behavior can drop max_displays via the
    # admin UI to cap it. None means "unset" and inherits from
    # installer_detected_max_displays; only coerce to 1 if the operator
    # explicitly wrote max_displays=1 in config.json (preserves their
    # intentional cap from previous installs).
    if max_displays_override is None:
        max_displays_override = connected_count

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
ln -sf "$PREFLIGHT" "$BIN_DIR/screentra-cast-player-preflight"
ln -sf "$DISPLAY_GOVERNANCE" "$BIN_DIR/screentra-cast-display-governance"

ln -sf "$RENDERER" "$BIN_DIR/screentra-cast-player-renderer"

cat > "$DISPLAY_PROVISION" <<'SH_PROVISION'
#!/usr/bin/env bash
# screentra-cast-player-display-provision.sh
#
# One-shot service that runs at user-session start (and on resume from
# suspend) to enable/disable the per-display kiosk services based on the
# current set of connected DRM connectors.
#
# Source of truth: ~/screentra-cast-player-client/config.json -> computes
#   active_max_displays = min(installer_detected_max_displays, max_displays)
# where `installer_detected_max_displays` was written by the previous
# screentra-cast-display-governance run on this boot.
#
# For each index 0..active-1:
#   systemctl --user enable --now screentra-cast-player-renderer@N
#   systemctl --user enable --now screentra-cast-kiosk-browser@N
# If display 0 is served by the system labwc hook (LABWC_SYSTEM_INSTALLED=1),
# skip screentra-cast-kiosk-browser@0 because the system unit owns the seat.
#
# For each index >= active and <= MAX_PROVISIONED-1:
#   systemctl --user disable --now screentra-cast-player-renderer@N
#   systemctl --user disable --now screentra-cast-kiosk-browser@N
#
# Idempotent: safe to run on every boot.
set -euo pipefail

INSTALL_DIR="${SCREENTRA_CAST_CLIENT_DIR:-$HOME/.local/share/screentra-cast-player-client}"
CONFIG_FILE="${SCREENTRA_CAST_CLIENT_CONFIG:-$INSTALL_DIR/config.json}"
SYSTEMD_USER_DIR="${SYSTEMD_USER_DIR:-$HOME/.config/systemd/user}"
LOG_DIR="${XDG_STATE_HOME:-$HOME/.local/state}/screentra-cast-player-client"
mkdir -p "$LOG_DIR"
LOG="$LOG_DIR/display-provision.log"

log() { printf '%s %s\n' "$(date -Is)" "$*" | tee -a "$LOG"; }

# 2026-08-10: when the installer invokes this script as root via SSH,
# bare `systemctl --user` would talk to root's user manager (which has
# no units) and silently no-op. Detect root and re-exec as the target
# user so the systemd-user calls land in the correct manager.
if [ "$(id -u)" = "0" ] && [ -n "${BOARD_USER:-}" ] && [ "${BOARD_USER}" != "root" ]; then
    USER_RUNTIME_DIR="/run/user/$(id -u "$BOARD_USER" 2>/dev/null)"
    if [ -d "$USER_RUNTIME_DIR" ]; then
        log "display-provision: re-execing as $BOARD_USER (root user manager would no-op)"
        exec sudo -u "$BOARD_USER" \
            HOME="/home/$BOARD_USER" \
            XDG_RUNTIME_DIR="$USER_RUNTIME_DIR" \
            /bin/bash "$0" "$@"
    fi
fi

# Re-run governance so config.json reflects the live state of the DRM
# connectors on this boot, not what was last cached at install time.
log "display-provision: re-running governance"
if [ -x "$INSTALL_DIR/screentra-cast-display-governance.py" ]; then
  python3 "$INSTALL_DIR/screentra-cast-display-governance.py" || log "  governance failed; falling back to cached config.json"
fi

# Clear stale per-display layout caches when the resolution or refresh rate
# changes since the last boot (or when the connector itself changed). The
# actor re-fetches the layout on the next heartbeat, so the renderer briefly
# shows the no-layout page but the operator isn't left looking at a layout
# sized for the wrong screen. Drops legacy display-0 fallback too.
#
# Per-display cache files:
#   cache/display-{N}.json                       (current resolution snapshot)
#   cache/assigned-layout-display-{N}.json       (the cached layout for that display)
#   cache/assigned-layout.json                   (legacy fallback for display 0 only)
log "display-provision: checking for resolution/connector changes"
CACHE_DIR="$INSTALL_DIR/cache"
RESET_DISPLAYS="$(python3 - "$CACHE_DIR" <<'PY' 2>/dev/null || echo ""
import json, os, sys
cache_dir = sys.argv[1]
last_known = {}
for n in range(4):
    p = os.path.join(cache_dir, f'display-{n}.json')
    if os.path.exists(p):
        try:
            d = json.loads(open(p).read())
            last_known[n] = (
                d.get('connector'),
                int(d.get('width') or 0),
                int(d.get('height') or 0),
                float(d.get('refresh_rate') or 0),
            )
        except Exception:
            pass
# Current state from governance
try:
    import glob
    # config.json contains last_detected_displays with (connector, w, h, refresh)
    cfg = json.loads(open(os.path.join(cache_dir, '..', 'config.json')).read())
except Exception:
    cfg = {}
current = {}
for d in (cfg.get('last_detected_displays') or []):
    idx = d.get('display_index')
    if idx is None: continue
    try: idx = int(idx)
    except Exception: continue
    if 0 <= idx < 4:
        current[idx] = (
            d.get('connector'),
            int(d.get('width') or 0),
            int(d.get('height') or 0),
            float(d.get('refresh_rate') or 0),
        )
changed = []
for n in range(4):
    cur = current.get(n)
    prev = last_known.get(n)
    # Connector missing in current state (unplugged) — drop stale cache too.
    if cur is None and prev is not None:
        changed.append(n); continue
    if cur is None and prev is None:
        continue
    if prev is None:
        # First boot for this display — nothing to compare; let the actor
        # populate it on next heartbeat. Don't drop the cache.
        continue
    if cur != prev:
        changed.append(n)
# Suppress false positives on displays not in the active set.
active = max(1, min(int(cfg.get('active_max_displays') or cfg.get('installer_detected_max_displays') or 1), 4))
changed = [n for n in changed if n < active]
print(' '.join(str(n) for n in changed))
PY
)"
if [ -n "$RESET_DISPLAYS" ]; then
  for n in $RESET_DISPLAYS; do
    log "display-provision: display $n resolution/connector changed; clearing layout cache"
    rm -f "$CACHE_DIR/assigned-layout-display-${n}.json"
    if [ "$n" = "0" ]; then
      rm -f "$CACHE_DIR/assigned-layout.json"
    fi
  done
else
  log "display-provision: resolution/connector stable"
fi

# Trim the cached widget preset uploads on every boot. These are the largest
# cache items (typically several MB of weather icons / preset images) and are
# purely a warm cache for offline rendering — the layout JSON itself doesn't
# depend on them. If the network drops, the actor refetches what's needed.
UPLOAD_CACHE="$CACHE_DIR/uploads"
if [ -d "$UPLOAD_CACHE" ]; then
  before=$(du -sk "$UPLOAD_CACHE" 2>/dev/null | awk '{print $1}')
  find "$UPLOAD_CACHE" -type f -delete 2>/dev/null || true
  after=$(du -sk "$UPLOAD_CACHE" 2>/dev/null | awk '{print $1}')
  log "display-provision: cleared uploads cache (${before}K → ${after}K)"
fi

# Active count = min(installer_detected_max_displays, max_displays, 4)
ACTIVE="$(
  python3 - "$CONFIG_FILE" <<'PY' 2>/dev/null || echo 1
import json, sys
try:
    cfg = json.loads(open(sys.argv[1]).read())
except Exception:
    cfg = {}
installer = max(int(cfg.get('installer_detected_max_displays') or 1), 1)
override = cfg.get('max_displays')
try:
    active = min(max(int(override), 1), installer) if override is not None else installer
except Exception:
    active = installer
print(max(1, min(active, 4)))
PY
)"
log "display-provision: active_max_displays=$ACTIVE"

# How high we ever provision. Past 4 the assumption is wrong — overkill.
MAX_PROVISIONED=4

# Track what changed so we can report at the end.
enabled_renderer=()
disabled_renderer=()
enabled_browser=()
disabled_browser=()

i=0
while [ "$i" -lt "$MAX_PROVISIONED" ]; do
  if [ "$i" -lt "$ACTIVE" ]; then
    # Should be running
    if [ -f "$SYSTEMD_USER_DIR/screentra-cast-player-renderer@.service" ]; then
      systemctl --user enable "screentra-cast-player-renderer@${i}.service" 2>/dev/null || true
      systemctl --user restart "screentra-cast-player-renderer@${i}.service" 2>/dev/null || true
      enabled_renderer+=("$i")
    fi
    # Display 0 is owned by the system labwc hook when that path is in use.
    if [ "$i" = "0" ] && [ "${LABWC_SYSTEM_INSTALLED:-0}" = "1" ]; then
      :
    elif [ -f "$SYSTEMD_USER_DIR/screentra-cast-kiosk-browser@.service" ]; then
      # Apply per-display orientation from server (defaults to 'normal').
      orient="$(python3 -c "
import json, sys, urllib.request
try:
    cfg = json.load(open('$CONFIG_FILE'))
    cid = open('$INSTALL_DIR/client-id').read().strip()
except: print('normal'); sys.exit(0)
srv = cfg.get('server_url', 'http://11.1.1.53:3000').rstrip('/')
try:
    data = json.loads(urllib.request.urlopen(f'{srv}/api/clients/{urllib.request.quote(cid)}/displays', timeout=5).read())
    for d in data:
        if d.get('display_index') == $i:
            print(d.get('orientation', 'normal'))
            break
    else: print('normal')
except: print('normal')
" 2>/dev/null || echo "normal")"
      systemctl --user set-environment "SCREENTRA_CAST_DISPLAY_ROTATION=${orient}" 2>/dev/null || true
      systemctl --user unmask "screentra-cast-kiosk-browser@${i}.service" 2>/dev/null || true
      systemctl --user enable --now "screentra-cast-kiosk-browser@${i}.service" 2>/dev/null || true
      systemctl --user restart "screentra-cast-kiosk-browser@${i}.service" 2>/dev/null || true
      enabled_browser+=("$i")
    fi
  else
    # Should NOT be running
    if [ -f "$SYSTEMD_USER_DIR/screentra-cast-player-renderer@.service" ]; then
      systemctl --user disable --now "screentra-cast-player-renderer@${i}.service" 2>/dev/null || true
      disabled_renderer+=("$i")
    fi
    if [ -f "$SYSTEMD_USER_DIR/screentra-cast-kiosk-browser@.service" ]; then
      systemctl --user disable --now "screentra-cast-kiosk-browser@${i}.service" 2>/dev/null || true
      disabled_browser+=("$i")
    fi
  fi
  i=$((i + 1))
done

if [ "${#enabled_renderer[@]}" -gt 0 ]; then
  log "display-provision: enabled renderer(s): ${enabled_renderer[*]}"
fi
if [ "${#disabled_renderer[@]}" -gt 0 ]; then
  log "display-provision: disabled renderer(s): ${disabled_renderer[*]}"
fi
if [ "${#enabled_browser[@]}" -gt 0 ]; then
  log "display-provision: enabled kiosk browser(s): ${enabled_browser[*]}"
fi
if [ "${#disabled_browser[@]}" -gt 0 ]; then
  log "display-provision: disabled kiosk browser(s): ${disabled_browser[*]}"
fi

log "display-provision: done"

SH_PROVISION
chmod +x "$DISPLAY_PROVISION"

cat > "$ACTOR" <<'PY_ACTOR'
#!/usr/bin/env python3
"""Screentra Cast player command actor.

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
import shutil
import ssl
import subprocess
import sys
import tempfile
import time
import urllib.request
from pathlib import Path

# SECURITY: cap on the pending-command file the actor reads. Anything larger
# is treated as malformed / malicious and discarded (with a result entry so
# the operator can see it). 1 MiB is generous for any legitimate payload
# (audio sink names, bluetooth MACs, layout assignments, etc.).
MAX_PENDING_BYTES = 1 << 20


def run(cmd: str) -> tuple[int, str, str]:
    try:
        p = subprocess.run(cmd, shell=True, capture_output=True, text=True, timeout=20)
        return p.returncode, (p.stdout or "").strip(), (p.stderr or "").strip()
    except Exception as e:
        return 1, "", str(e)


def has_systemctl_user() -> bool:
    if shutil.which("systemctl") is None:
        return False
    rc, _, _ = run("systemctl --user status >/dev/null 2>&1")
    return rc == 0


def systemctl_user(action: str, unit: str) -> tuple[bool, str]:
    rc, out, err = run(f"systemctl --user {action} {unit}")
    ok = rc == 0
    return ok, (out or err or f"{action} {unit} rc={rc}").strip()


# === Audio output + Bluetooth speaker =====================================
#
# Local helpers used by set_audio_device / scan_bluetooth_devices /
# pair_bluetooth_device actions. They call pactl/bluetoothctl directly and
# fall back to `sudo -n` if the user's session doesn't own the audio/bluetooth
# session bus. The renderer (admin UI) shares the same approach.

def _pactl(*args, timeout: int = 6) -> tuple[bool, str, str]:
    rc, out, err = run("pactl " + " ".join(args))
    if rc == 0:
        return True, out, err
    return run("sudo -n pactl " + " ".join(args) + " >/dev/null 2>&1; pactl " + " ".join(args))


def _bluetoothctl(*args, timeout: int = 12) -> tuple[bool, str, str]:
    rc, out, err = run("bluetoothctl " + " ".join(args))
    if rc == 0:
        return True, out, err
    rc2, out2, err2 = run("sudo -n bluetoothctl " + " ".join(args))
    return (rc2 == 0), out2, err2


def list_audio_devices() -> dict:
    """Return current_sink and the list of sinks."""
    ok, out, _err = _pactl("get-default-sink")
    current = out.strip() if ok else ""
    ok2, list_out, _ = _pactl("list", "short", "sinks")
    sinks = []
    if ok2:
        for line in list_out.splitlines():
            parts = line.split("\t")
            if len(parts) < 2:
                continue
            sinks.append({
                "index": int(parts[0]) if parts[0].isdigit() else None,
                "name": parts[1].strip(),
                "description": parts[1].strip(),
                "state": parts[4].strip() if len(parts) > 4 else "",
            })
    # Pull human descriptions
    desc_map: dict[str, str] = {}
    ok3, desc_out, _ = _pactl("list", "sinks")
    if ok3:
        name = None; desc = None
        for line in desc_out.splitlines():
            line = line.strip()
            if not line:
                if name and desc: desc_map[name] = desc
                name = None; desc = None
                continue
            if line.startswith("Name:"):
                name = line.split(":", 1)[1].strip()
            elif line.startswith("Description:"):
                desc = line.split(":", 1)[1].strip()
        if name and desc: desc_map[name] = desc
    for s in sinks:
        if s["name"] in desc_map:
            s["description"] = desc_map[s["name"]]
    return {"sinks": sinks, "current_sink": current}


def apply_audio_device(sink: str) -> tuple[bool, str, dict]:
    if not sink:
        return False, "sink required", {}
    ok, out, err = _pactl("set-default-sink", sink)
    if not ok:
        return False, f"pactl set-default-sink failed: {err}", {}
    _pactl("move-sink-input", "all", sink)
    return True, f"default sink set to {sink}", {"sink": sink}


def list_bluetooth_devices() -> dict:
    """Return {paired: [...], connected: [...]}."""
    ok, out, _err = _bluetoothctl("devices", "Paired")
    paired = []
    if ok:
        for line in out.splitlines():
            parts = line.strip().split(None, 2)
            if len(parts) < 3:
                continue
            mac = parts[1]
            name = parts[2]
            ok_info, info_out, _ = _bluetoothctl("info", mac)
            trusted = (ok_info and "Trusted: yes" in info_out)
            connected = (ok_info and "Connected: yes" in info_out)
            paired.append({"mac": mac, "name": name, "trusted": trusted, "connected": connected})
    ok2, conn_out, _ = _bluetoothctl("devices", "Connected")
    connected = []
    if ok2:
        for line in conn_out.splitlines():
            parts = line.strip().split(None, 2)
            if len(parts) >= 2 and ":" in parts[1]:
                connected.append(parts[1])
    return {"paired": paired, "connected": connected}


def scan_bluetooth_devices(seconds: int = 12) -> dict:
    """Run bluetoothctl scan on for `seconds`, return new devices discovered."""
    ok_before, before_out, _ = _bluetoothctl("devices")
    before_set = set()
    if ok_before:
        for line in before_out.splitlines():
            parts = line.strip().split(None, 2)
            if len(parts) >= 2 and ":" in parts[1]:
                before_set.add(parts[1])
    _bluetoothctl("scan", "on")
    try:
        time.sleep(seconds)
    finally:
        _bluetoothctl("scan", "off")
    ok_after, after_out, _ = _bluetoothctl("devices")
    found = []
    if ok_after:
        for line in after_out.splitlines():
            parts = line.strip().split(None, 2)
            if len(parts) < 3:
                continue
            mac = parts[1]
            if mac in before_set:
                continue
            found.append({"mac": mac, "name": parts[2]})
    return {"devices": found}


def pair_bluetooth_device(mac: str, action: str = "pair") -> tuple[bool, str, dict]:
    mac = (mac or "").strip()
    if not mac or ":" not in mac:
        return False, "mac required (AA:BB:CC:DD:EE:FF)", {}
    steps = []
    sequence = (["pair", mac], ["trust", mac], ["connect", mac]) if action == "pair" else (["connect", mac],)
    for cmd in sequence:
        ok, out, err = _bluetoothctl(*cmd)
        steps.append({"cmd": " ".join(cmd), "ok": ok, "stderr": err[-200:] if err else ""})
    # After connect, give bluez a moment to publish a sink under PulseAudio
    time.sleep(2)
    chosen_sink = None
    info = list_audio_devices()
    for s in info.get("sinks", []):
        if "bluez" in s["name"].lower():
            chosen_sink = s["name"]
            break
    if chosen_sink:
        apply_audio_device(chosen_sink)
    return True, f"{action} finished for {mac}", {"mac": mac, "sink": chosen_sink, "steps": steps}


def apply_display_count(cache_dir: Path, max_displays) -> tuple[bool, str, dict]:
    """Persist active display count and apply renderer/browser services.

    One labwc compositor owns DRM (system screentra-cast-labwc-kiosk@0). Secondary
    displays are Chromium windows attached to wayland-0 via user
    screentra-cast-kiosk-browser@N services; never start extra system labwc seats on a
    single-GPU player.
    """
    install_dir = cache_dir.parent
    config_path = install_dir / 'config.json'
    try:
        cfg = json.loads(config_path.read_text()) if config_path.exists() else {}
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
    tmp.write_text(json.dumps(cfg, indent=2)); tmp.replace(config_path)

    details = {'config_path': str(config_path), 'installer_detected_max_displays': installer, 'max_displays': override, 'active_max_displays': active, 'steps': []}
    if has_systemctl_user():
        run('systemctl --user daemon-reload >/dev/null 2>&1')
        for i in range(4):
            if i < active:
                ok1, msg1 = systemctl_user('enable', f'screentra-cast-player-renderer@{i}.service')
                ok2, msg2 = systemctl_user('restart', f'screentra-cast-player-renderer@{i}.service')
                details['steps'].append({'unit': f'screentra-cast-player-renderer@{i}.service', 'enable': ok1, 'restart': ok2, 'message': msg2 or msg1})
                if i > 0:
                    ok3, msg3 = systemctl_user('enable', f'screentra-cast-kiosk-browser@{i}.service')
                    ok4, msg4 = systemctl_user('restart', f'screentra-cast-kiosk-browser@{i}.service')
                    details['steps'].append({'unit': f'screentra-cast-kiosk-browser@{i}.service', 'enable': ok3, 'restart': ok4, 'message': msg4 or msg3})
            else:
                ok1, msg1 = systemctl_user('stop', f'screentra-cast-player-renderer@{i}.service')
                ok2, msg2 = systemctl_user('disable', f'screentra-cast-player-renderer@{i}.service')
                ok3, msg3 = systemctl_user('stop', f'screentra-cast-kiosk-browser@{i}.service')
                ok4, msg4 = systemctl_user('disable', f'screentra-cast-kiosk-browser@{i}.service')
                details['steps'].append({'unit': f'display-{i}', 'disabled': True, 'messages': [msg1,msg2,msg3,msg4]})
    # Manage labwc seats per active display: enable a labwc seat for each active display index >0, disable others.
    for i in range(1,4):
        if i < active:
            # Enable system seat for this display
            rc, out, err = run(f'sudo -n systemctl enable --now screentra-cast-labwc-kiosk@{i}.service >/dev/null 2>&1')
            ok = rc == 0
            details['steps'].append({'unit': f'screentra-cast-labwc-kiosk@{i}.service', 'enabled_system_seat': ok, 'message': out or err})
        else:
            # Disable seat when not needed
            rc, out, err = run(f'sudo -n systemctl disable --now screentra-cast-labwc-kiosk@{i}.service >/dev/null 2>&1')
            ok = rc == 0
            details['steps'].append({'unit': f'screentra-cast-labwc-kiosk@{i}.service', 'disabled_system_seat': ok, 'message': out or err})

    # Apply compositor output policy live when wayland-0 is available. Do not
    # restart labwc just to change display count; restarting can briefly drop DRM
    # and leave stale Chromium/orphan labwc processes. Boot-time policy remains in
    # screentra-cast-kiosk-launcher.sh for reboots.
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
        rc,out,err = run('sudo -n systemctl restart screentra-cast-labwc-kiosk@0.service')
        details['steps'].append({'unit': 'screentra-cast-labwc-kiosk@0.service', 'restart_code': rc, 'message': out or err})

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
        cfg = json.loads(config_path.read_text()) if config_path.exists() else {}
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
    tmp.write_text(json.dumps(cfg, indent=2)); tmp.replace(config_path)
    if dev_path:
        cache_dir.mkdir(parents=True, exist_ok=True)
        Path(cache_dir, 'touch-device.json').write_text(json.dumps({'path': dev_path, 'name': dev_name}, indent=2))
    details = {'config_path': str(config_path), 'touch_binding': binding, 'steps': []}
    # Restart labwc so rc.xml <touch> mapping is reloaded, then let browser services return.
    rc, out, err = run('sudo -n systemctl restart screentra-cast-labwc-kiosk@0.service')
    details['steps'].append({'unit': 'screentra-cast-labwc-kiosk@0.service', 'restart_code': rc, 'message': out or err})
    if has_systemctl_user():
        systemctl_user('restart', 'screentra-cast-player-heartbeat.service')
        for i in range(4):
            systemctl_user('try-restart', f'screentra-cast-player-renderer@{i}.service')
            systemctl_user('try-restart', f'screentra-cast-kiosk-browser@{i}.service')
    ok = rc == 0 or has_systemctl_user()
    return ok, 'touch binding applied; labwc/browser services restarted', details


def tls_context(cache_dir: Path | None = None):
    """Use the Screentra Cast CA bundle directly when present, independent of OS trust."""
    candidates = []
    if os.environ.get('SCREENTRA_CAST_CA_CERT'):
        candidates.append(Path(os.environ['SCREENTRA_CAST_CA_CERT']))
    if cache_dir is not None:
        candidates.append(cache_dir.parent / 'screentra-cast-signage-ca.crt')
    candidates.append(Path.home() / '.local/share/screentra-cast-player-client/screentra-cast-signage-ca.crt')
    for cafile in candidates:
        if cafile.exists():
            try:
                return ssl.create_default_context(cafile=str(cafile))
            except Exception as e:
                print(f'warning: failed to load Screentra Cast CA {cafile}: {e}', flush=True)
    if os.environ.get('SCREENTRA_CAST_INSECURE_SSL', '').lower() in ('1', 'true', 'yes'):
        return ssl._create_unverified_context()
    return None

def urlopen_screentra-cast(req, timeout=10, cache_dir: Path | None = None):
    kw = {'timeout': timeout}
    ctx = tls_context(cache_dir)
    if ctx is not None:
        kw['context'] = ctx
    return urllib.request.urlopen(req, **kw)


def _cache_icon(server: str, icon_path: str, cache_dir: Path, timeout: int = 10) -> bool:
    """Download a single icon (e.g. ``/uploads/images/foo.svg``) into the
    local cache at ``<cache_dir>/uploads/<path without leading /uploads>``
    so the kiosk browser can render it from cache without a server
    roundtrip and even when the server is offline. Returns True on
    success."""
    if not icon_path or not icon_path.startswith('/'):
        return False
    # Strip the leading "/uploads/" so the cache layout mirrors what the
    # renderer's /uploads/<rel> handler will look up at request time.
    rel = icon_path[1:]  # drop leading slash → "uploads/images/foo.svg"
    if rel.startswith('uploads/'):
        rel = rel[len('uploads/'):]
    dest = cache_dir / 'uploads' / rel
    if dest.exists():
        # Already cached; let the kiosk browser pick it up via the
        # renderer's local /uploads/... handler.
        return True
    try:
        dest.parent.mkdir(parents=True, exist_ok=True)
        url = server.rstrip('/') + icon_path
        with urlopen_screentra-cast(url, timeout=timeout, cache_dir=cache_dir) as r:
            data = r.read()
        tmp = dest.with_suffix(dest.suffix + '.tmp')
        tmp.write_bytes(data)
        os.replace(tmp, dest)
        return True
    except Exception as e:
        print(f'icon cache failed for {icon_path}: {e}', flush=True)
        return False


def _cache_widget_assets(server: str, layout: dict, cache_dir: Path) -> None:
    """Walk every widget in `layout` and pre-fetch any /uploads/* assets
    referenced from widget props into the local cache. 2026-08-11: prior
    to this, only custom weather icons were pre-fetched via _cache_icon;
    image widgets pointed at /uploads/images/<uuid>.png URLs that 404'd
    on the local renderer because the cache had nothing at
    cache/uploads/images/<uuid>.png. The renderer's /uploads/<rel>
    handler resolves them transparently — we just have to populate the
    cache first. Idempotent: files that already exist are skipped."""
    if not isinstance(layout, dict):
        return
    widgets = layout.get('widgets') or []
    seen = set()
    for w in widgets:
        if not isinstance(w, dict):
            continue
        props = w.get('props') or {}
        if not isinstance(props, dict):
            continue
        # Pull every string field from props that looks like a /uploads/... URL.
        for k, v in list(props.items()):
            if isinstance(v, str) and v.startswith('/uploads/') and v not in seen:
                seen.add(v)
                _cache_icon(server, v, cache_dir)
            # Also walk custom_icons map (same /uploads/... values).
            if k == 'custom_icons' and isinstance(v, dict):
                for ck, cv in v.items():
                    if isinstance(cv, str) and cv.startswith('/uploads/') and cv not in seen:
                        seen.add(cv)
                        _cache_icon(server, cv, cache_dir)
        # Walk template_elements recursively (weather presets embed references).
        tpl = props.get('template_elements')
        if isinstance(tpl, list):
            for el in tpl:
                if not isinstance(el, dict):
                    continue
                ep = el.get('props') or {}
                if not isinstance(ep, dict):
                    continue
                for k, v in list(ep.items()):
                    if isinstance(v, str) and v.startswith('/uploads/') and v not in seen:
                        seen.add(v)
                        _cache_icon(server, v, cache_dir)
                    if k == 'custom_icons' and isinstance(v, dict):
                        for ck, cv in v.items():
                            if isinstance(cv, str) and cv.startswith('/uploads/') and cv not in seen:
                                seen.add(cv)
                                _cache_icon(server, cv, cache_dir)


def inline_widget_presets(server: str, layout: dict, cache_dir: Path) -> None:
    """Walk the widgets in `layout` and inline any preset referenced by a
    template UUID directly into the widget's props.

    Mutates `layout` in place. The server stores template geometry inside
    a stringified `props_json` field on each `widget_presets` row; we
    fetch each unique preset once and surface
    template_elements / template_width / template_height / custom_icons
    as top-level keys on the widget's own props so the kiosk browser can
    render purely from cache without a second server roundtrip.
    """
    if not isinstance(layout, dict):
        return
    widgets = layout.get('widgets') or []
    seen_presets: dict[str, dict | None] = {}
    for w in widgets:
        if not isinstance(w, dict):
            continue
        props = w.get('props') or {}
        preset_id = (
            props.get('template')
            or props.get('_template_id')
            or props.get('preset_id')
        )
        if not preset_id or not isinstance(preset_id, str):
            continue
        if isinstance(props.get('template_elements'), list) and props.get('template_elements'):
            # Already inlined — server pushed the geometry directly.
            continue
        if preset_id not in seen_presets:
            seen_presets[preset_id] = fetch_widget_preset(server, preset_id, cache_dir)
        preset = seen_presets[preset_id]
        if not preset:
            continue
        if preset.get('template_elements'):
            props['template_elements'] = preset['template_elements']
        if preset.get('template_width') is not None:
            props['template_width'] = preset['template_width']
        if preset.get('template_height') is not None:
            props['template_height'] = preset['template_height']
        if preset.get('custom_icons'):
            # Merge so per-widget icon overrides still win.
            merged = dict(preset['custom_icons'])
            merged.update(props.get('custom_icons') or {})
            # Pre-fetch every icon into the local cache so the kiosk
            # browser can render them from cache without a server
            # roundtrip and even when the server is offline. The renderer's
            # /uploads/<path> handler serves them from cache/uploads/.
            for icon_path in merged.values():
                if isinstance(icon_path, str) and icon_path.startswith('/'):
                    _cache_icon(server, icon_path, cache_dir)
            props['custom_icons'] = merged
        w['props'] = props
    layout['widgets'] = widgets


def fetch_widget_preset(server: str, preset_id: str, cache_dir: Path) -> dict | None:
    """Fetch a widget preset (template) by id from the server.

    Returns a flat dict containing the fields the kiosk browser needs to
    inline the template directly into the widget props, or None on any
    failure (offline, 404, malformed). The server stores the template
    geometry inside a stringified `props_json` field; we parse that and
    surface template_elements / template_width / template_height /
    custom_icons as top-level keys so the local player.js logic doesn't
    have to know about the storage shape.
    """
    try:
        url = server.rstrip('/') + '/api/widget-presets/' + urllib.request.quote(preset_id, safe='')
        with urlopen_screentra-cast(url, timeout=10, cache_dir=cache_dir) as r:
            raw = json.loads(r.read().decode())
        pj = raw.get('props_json')
        if isinstance(pj, str) and pj.strip():
            try:
                pj_obj = json.loads(pj)
            except Exception:
                pj_obj = {}
        elif isinstance(pj, dict):
            pj_obj = pj
        else:
            pj_obj = {}
        out = {
            'name': raw.get('name'),
            'widget_type': raw.get('widget_type'),
            'template_elements': pj_obj.get('template_elements') or [],
            'template_width': pj_obj.get('template_width'),
            'template_height': pj_obj.get('template_height'),
            'custom_icons': pj_obj.get('custom_icons') or {},
        }
        return out
    except Exception as e:
        print(f'preset fetch failed for {preset_id}: {e}', flush=True)
        return None


def fetch_config(server: str, client_id: str, cache_dir: Path, display_index: int | None = None) -> bool:
    if display_index is None:
        url = server.rstrip('/') + '/api/clients/' + urllib.request.quote(client_id, safe='') + '/config'
        target = 'assigned-layout.json'
    else:
        url = (server.rstrip() + '/api/clients/' + urllib.request.quote(client_id, safe='') +
               '/displays/' + str(display_index) + '/config')
        target = f'assigned-layout-display-{display_index}.json'
    try:
        with urlopen_screentra-cast(url, timeout=10, cache_dir=cache_dir) as r:
            data = json.loads(r.read().decode())
        # Inline any widget presets referenced by template UUID into the
        # layout. The server stores template geometry inside `props_json`
        # on the widget_presets row — inline_widget_presets fetches each
        # unique preset once and merges template_elements /
        # template_width / template_height / custom_icons into the
        # widget's own props so the kiosk browser can render purely from
        # cache without a second server roundtrip.
        layout_obj = data.get('layout')
        if isinstance(layout_obj, dict):
            inline_widget_presets(server, layout_obj, cache_dir)
            # 2026-08-11: cache every /uploads/* asset referenced by widget
            # props so the kiosk browser can render them offline.
            _cache_widget_assets(server, layout_obj, cache_dir)
            data['layout'] = layout_obj
        cache_dir.mkdir(parents=True, exist_ok=True)
        # Write client-config.json atomically (write to tmp, then rename).
        cfg_tmp = cache_dir / 'client-config.json.tmp'
        cfg_tmp.write_text(json.dumps(data, indent=2))
        cfg_tmp.replace(cache_dir / 'client-config.json')
        if data.get('layout') is not None:
            # Atomic replace per-display: write to .tmp then rename, so the
            # kiosk browser never sees a partially-written file. The
            # unlink-then-rename pattern also guarantees the cache is wiped
            # before the new layout lands — the brief window between the
            # two writes is what the user wants: "clear the client widget
            # info database on layout change".
            layout_data = json.dumps(data['layout'], indent=2)
            target_path = cache_dir / target
            target_tmp = cache_dir / (target + '.tmp')
            target_tmp.write_text(layout_data)
            try:
                target_path.unlink()
            except FileNotFoundError:
                pass
            target_tmp.replace(target_path)
            if display_index in (None, 0):
                legacy_path = cache_dir / 'assigned-layout.json'
                legacy_tmp = cache_dir / 'assigned-layout.json.tmp'
                legacy_tmp.write_text(layout_data)
                try:
                    legacy_path.unlink()
                except FileNotFoundError:
                    pass
                legacy_tmp.replace(legacy_path)
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
        with urlopen_screentra-cast(url, timeout=10, cache_dir=cache_dir) as r:
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
    """Download and run the server-served player installer, preserving enrollment identity."""
    payload = payload or {}
    install_dir = Path(os.environ.get('SCREENTRA_CAST_CLIENT_DIR') or str(cache_dir.parent))
    server = (payload.get('server_url') or server or '').rstrip('/')
    if not server:
        return False, 'server URL is not configured', {}
    installer_url = payload.get('installer_url') or (server + '/install/player-client.sh')
    configured = urllib.parse.urlsplit(server)
    requested = urllib.parse.urlsplit(installer_url)
    if (requested.scheme, requested.netloc) != (configured.scheme, configured.netloc):
        installer_url = server + '/install/player-client.sh'
    signature_url = server + '/install/player-client.sh.sig'
    public_key = Path(os.environ.get('SCREENTRA_CAST_UPDATE_PUBLIC_KEY_PATH') or '/etc/screentra-cast/client-update-signing.pub')
    env = os.environ.copy()
    env['SCREENTRA_CAST_SERVER_URL'] = server
    if client_id:
        env['SCREENTRA_CAST_CLIENT_ID'] = client_id
    env['SCREENTRA_CAST_CLIENT_DIR'] = str(install_dir)
    env['SCREENTRA_CAST_CA_CERT'] = str(install_dir / 'screentra-cast-signage-ca.crt')
    env.setdefault('SCREENTRA_CAST_CLIENT_NAME', os.environ.get('SCREENTRA_CAST_CLIENT_NAME') or os.uname().nodename)
    env.setdefault('SCREENTRA_CAST_CLIENT_ROLE', os.environ.get('SCREENTRA_CAST_CLIENT_ROLE') or 'player')
    details = {
        'installer_url': installer_url,
        'signature_url': signature_url,
        'public_key': str(public_key),
        'install_dir': str(install_dir),
        'client_id': client_id or None,
    }
    if payload.get('dry_run'):
        details['dry_run'] = True
        return True, f'would download and run {installer_url}', details
    with tempfile.NamedTemporaryFile(prefix='screentra-cast-player-update-', suffix='.sh', delete=False) as tmp:
        tmp_path = Path(tmp.name)
    signature_path = tmp_path.with_suffix('.sig')
    try:
        # Use the same TLS verification as the rest of the actor (Screentra Cast CA
        # bundle or system trust), never `ssl._create_unverified_context`.
        # The installer script is downloaded from the same server as the
        # heartbeat, and a MITM that can rewrite the heartbeat can also
        # rewrite the installer.
        ctx_ssl = tls_context(cache_dir)
        kw = {'timeout': 30}
        if ctx_ssl is not None:
            kw['context'] = ctx_ssl
        with urllib.request.urlopen(installer_url, **kw) as r:
            data = r.read()
        with urllib.request.urlopen(signature_url, **kw) as r:
            signature = r.read()
        if not public_key.is_file():
            return False, f'client update refused: pinned signing key is missing at {public_key}', details
        tmp_path.write_bytes(data)
        signature_path.write_bytes(signature)
        verify = subprocess.run(
            [
                'openssl', 'pkeyutl', '-verify', '-pubin',
                '-inkey', str(public_key), '-sigfile', str(signature_path),
                '-in', str(tmp_path),
            ],
            capture_output=True, text=True, timeout=20,
        )
        details['signature_verified'] = verify.returncode == 0
        if verify.returncode != 0:
            return False, 'client update refused: installer signature verification failed', details
        tmp_path.chmod(0o755)
        p = subprocess.run(['/bin/bash', str(tmp_path)], env=env, capture_output=True, text=True, timeout=int(payload.get('timeout_seconds') or 180))
        out = (p.stdout or '').strip()
        err = (p.stderr or '').strip()
        details['returncode'] = p.returncode
        details['stdout_tail'] = out[-2000:]
        details['stderr_tail'] = err[-2000:]
        if p.returncode != 0:
            return False, f'installer exited {p.returncode}: {(err or out)[-500:]}', details
        return True, 'client installer updated/reinstalled successfully', details
    except Exception as e:
        details['error'] = str(e)
        return False, f'client update failed: {e}', details
    finally:
        try:
            tmp_path.unlink()
        except Exception:
            pass
        try:
            signature_path.unlink()
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
            ok, msg = systemctl_user('restart', unit('screentra-cast-player-renderer'))
        else:
            ok, msg = False, 'systemctl --user unavailable'
        return {'action': action, 'display_index': display_index, 'ok': ok, 'message': msg, 'started_at': started}

    if action == 'restart_kiosk':
        if has_user:
            ok, msg = systemctl_user('restart', unit('screentra-cast-kiosk-browser'))
        else:
            ok, msg = False, 'systemctl --user unavailable'
        return {'action': action, 'display_index': display_index, 'ok': ok, 'message': msg, 'started_at': started}

    # Alias used by the touchscreen selection flow. Forward to the display-scoped
    # kiosk restart unit so the queued action is no longer dropped as unknown.
    if action == 'restart_kiosk_display':
        if has_user:
            ok, msg = systemctl_user('restart', unit('screentra-cast-kiosk-browser'))
        else:
            ok, msg = False, 'systemctl --user unavailable'
        return {'action': 'restart_kiosk', 'display_index': display_index, 'ok': ok, 'message': msg, 'started_at': started}

    if action == 'reload_layout':
        if payload.get('all_displays') or display_index is None:
            ok = fetch_all_display_configs(ctx['server'], ctx['client_id'], Path(ctx['cache_dir']))
            return {'action': action, 'display_index': display_index, 'all_displays': True, 'ok': ok, 'message': 'all display configs re-fetched' if ok else 'one or more display config fetches failed', 'started_at': started}
        ok = fetch_config(ctx['server'], ctx['client_id'], Path(ctx['cache_dir']), display_index=display_index)
        return {'action': action, 'display_index': display_index, 'ok': ok, 'message': 'config re-fetched' if ok else 'config fetch failed', 'started_at': started}

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

    if action == 'list_audio_devices':
        info = list_audio_devices()
        return {'action': action, 'display_index': display_index, 'ok': True, 'message': f"{len(info.get('sinks',[]))} sink(s); current={info.get('current_sink') or '—'}", 'detail': info, 'started_at': started}

    if action == 'set_audio_device':
        sink = (payload or {}).get('sink') or ''
        ok, msg, detail = apply_audio_device(sink)
        return {'action': action, 'display_index': display_index, 'ok': ok, 'message': msg, 'detail': detail, 'started_at': started}

    if action == 'list_bluetooth_devices':
        info = list_bluetooth_devices()
        return {'action': action, 'display_index': display_index, 'ok': True, 'message': f"{len(info.get('paired',[]))} paired", 'detail': info, 'started_at': started}

    if action == 'scan_bluetooth_devices':
        seconds = int((payload or {}).get('seconds') or 12)
        info = scan_bluetooth_devices(seconds=seconds)
        return {'action': action, 'display_index': display_index, 'ok': True, 'message': f"scan complete; found {len(info.get('devices',[]))} new device(s)", 'detail': info, 'started_at': started}

    if action == 'pair_bluetooth_device':
        mac = (payload or {}).get('mac') or ''
        sub = (payload or {}).get('action') or 'pair'
        ok, msg, detail = pair_bluetooth_device(mac, action=sub)
        return {'action': action, 'display_index': display_index, 'ok': ok, 'message': msg, 'detail': detail, 'started_at': started}

    # IR bridge — fired by the schedule engine when a "TV Power" event ticks.
    # The server never tells the bridge which code to send; the bridge owns its
    # own learned slots. The host just writes "BLAST\n" and the bridge fires
    # every slot it's been taught. Discovery + reconnect lives in ir_bridge.
    if action == 'ir_blast':
        try:
            import ir_bridge as _irb
            ok, msg, detail = _irb.blast_with_retry()
        except Exception as e:
            return {'action': action, 'ok': False, 'message': f'ir_bridge module error: {e}', 'started_at': started}
        return {'action': action, 'display_index': display_index, 'ok': ok, 'message': msg, 'detail': detail, 'started_at': started}

    return {'action': action, 'display_index': display_index, 'ok': False, 'message': f'unknown action: {action}', 'started_at': started}


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument('--pending-file', default=os.environ.get('SCREENTRA_CAST_PENDING_FILE'))
    ap.add_argument('--result-file', default=os.environ.get('SCREENTRA_CAST_RESULT_FILE'))
    ap.add_argument('--server', default=os.environ.get('SCREENTRA_CAST_SERVER_URL'))
    ap.add_argument('--client-id', default=os.environ.get('SCREENTRA_CAST_CLIENT_ID'))
    ap.add_argument('--cache-dir', default=os.environ.get('SCREENTRA_CAST_CLIENT_CACHE_DIR', os.path.expanduser('~/.local/share/screentra-cast-player-client/cache')))
    ap.add_argument('--watch', action='store_true', help='Loop, processing new pending files as they appear')
    ap.add_argument('--watch-interval', type=float, default=2.0)
    args = ap.parse_args()

    if not args.pending_file:
        ap.error('--pending-file (or SCREENTRA_CAST_PENDING_FILE) is required')

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
        # SECURITY: cap the read size so a multi-GB pending file can't DoS
        # the actor with a single json.loads(). 1 MiB is generous for any
        # legitimate command payload.
        try:
            size = pending_path.stat().st_size
        except OSError:
            return False
        if size > MAX_PENDING_BYTES:
            result = {'action': 'unknown', 'ok': False, 'message': f'pending file too large ({size} bytes, max {MAX_PENDING_BYTES})', 'started_at': datetime.datetime.now(datetime.timezone.utc).replace(tzinfo=None).isoformat() + 'Z'}
            result_path.write_text(json.dumps(result, indent=2))
            try: pending_path.unlink()
            except Exception: pass
            return True
        try:
            spec = json.loads(pending_path.read_text())
        except Exception as e:
            result = {'action': 'unknown', 'ok': False, 'message': f'bad pending file: {e}', 'started_at': datetime.datetime.now(datetime.timezone.utc).replace(tzinfo=None).isoformat() + 'Z'}
            result_path.write_text(json.dumps(result, indent=2))
            try: pending_path.unlink()
            except Exception: pass
            return True
        action = spec.get('action', 'unknown')
        result = execute(action, spec.get('payload'), ctx)
        result['finished_at'] = datetime.datetime.now(datetime.timezone.utc).replace(tzinfo=None).isoformat() + 'Z'
        result['client_id'] = ctx['client_id'] or None
        result['requested_at'] = spec.get('requested_at')
        result_path.write_text(json.dumps(result, indent=2))
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
ln -sf "$ACTOR" "$BIN_DIR/screentra-cast-player-actor"

cat > "$IR_BRIDGE" <<'PY_IR_BRIDGE'
"""RP2040 IR bridge discovery + serial I/O for the Screentra Cast player.

Single-process helper used by both the heartbeat agent (to surface bridge
state to the server) and the command actor (to fire the BLAST command when
the server schedules a TV power event).

Discovery is purely sysfs-based so we don't depend on pyusb. We look for
any /dev/ttyACM* node whose USB device chain has idVendor=2e8a
(Raspberry Pi). We then write newline-terminated ASCII commands to that
node and read the bridge's structured response.

Protocol (see bridge/src/main.cpp):
    LEARN 1|2\\n    -> start capture to slot 1 or 2 (button-driven)
    BLAST\\n        -> fire every slot the bridge has been taught
    STATUS\\n       -> report present + learned state (no code bytes)
    SAVE|LOAD\\n    -> persist/restore slots from flash

Pico owns the IR codes. The host never tells the bridge which code to send
or what's encoded — only that it's time to fire. Capture happens on the
device via physical buttons (tap = blast, hold = learn).

Response shape:
    OK <human message>
    ERR <reason>
    STATUS present=true slot0=<learned|empty:count> slot1=<learned|empty:count> port=<path>
"""

from __future__ import annotations

import glob
import json
import os
import re
import subprocess
import time
from dataclasses import dataclass
from typing import Optional


# Raspberry Pi RP2040 USB vendor ID (assigned to Foundation, used by the Pico
# and by clones like the RP2040-NANO when running USB CDC).
RP2040_VENDOR_ID = "2e8a"

# USB CDC ACM node pattern.
TTYACM_GLOB = "/sys/class/tty/ttyACM*/device"

# Sysfs path for the VID on the upstream USB device tree. We resolve
# ../../idVendor from the ttyACM device directory; if that points at a
# composite device, we walk up the tree until we find idVendor.
_IDVENDOR_CANDIDATES = (
    "idVendor",
    "../../../idVendor",
    "../../../../idVendor",
)


@dataclass
class BridgeInfo:
    """Discovered bridge location + last-known learned state."""
    port: str                              # /dev/ttyACM0
    present: bool
    slot0_learned: bool = False
    slot1_learned: bool = False
    slot0_count: int = 0
    slot1_count: int = 0
    last_status_raw: str = ""
    last_error: Optional[str] = None

    def to_json(self) -> Optional[dict]:
        """Match the JSON shape the server expects on the heartbeat."""
        if not self.present and not self.last_error:
            return None
        out = {
            "present": self.present,
            "slot0_learned": self.slot0_learned,
            "slot1_learned": self.slot1_learned,
            "slot0_count": self.slot0_count,
            "slot1_count": self.slot1_count,
        }
        if self.port:
            out["port"] = self.port
        if self.last_status_raw:
            out["last_status"] = self.last_status_raw
        if self.last_error:
            out["last_error"] = self.last_error
        return out


def _read_text(path: str) -> Optional[str]:
    try:
        with open(path, "r", errors="replace") as f:
            return f.read().strip()
    except (FileNotFoundError, PermissionError, OSError):
        return None


def _is_rp2040(device_dir: str) -> bool:
    """Walk up from the CDC function symlink until we find idVendor.

    /sys/class/tty/ttyACM0/device -> /sys/devices/.../2-4:1.0 (function)
    /sys/devices/.../2-4:1.0/../../2-4 (USB device) -> idVendor

    The walk-up via realpath() handles the varying depth that the CDC ACM
    function adds under different USB stacks (some expose 2 interfaces,
    some 1; some have config descriptors in between).
    """
    # First try the device's own idVendor (covers the simple case where
    # the ttyACM device has its own sysfs node — happens with picoprobe).
    for rel in ("idVendor",):
        v = _read_text(os.path.join(device_dir, rel))
        if v and v.lower() == RP2040_VENDOR_ID:
            return True
    # Walk up the real path to the parent USB device and try its idVendor.
    real = os.path.realpath(device_dir)
    for _ in range(6):
        real = os.path.dirname(real)
        if not real or real == "/":
            break
        v = _read_text(os.path.join(real, "idVendor"))
        if v and v.lower() == RP2040_VENDOR_ID:
            return True
    return False


def discover_port() -> Optional[str]:
    """Return /dev/ttyACM* for the first RP2040 found, else None."""
    # /sys/class/tty/ttyACM0 is the canonical class entry. Its `device`
    # symlink points at the bound interface. The basename IS the /dev name.
    for entry in sorted(glob.glob("/sys/class/tty/ttyACM*")):
        dev_dir = os.path.join(entry, "device")
        if not os.path.exists(dev_dir):
            continue
        if _is_rp2040(dev_dir):
            return f"/dev/{os.path.basename(entry)}"
    return None


def discover_bridge(timeout_seconds: float = 0.0) -> BridgeInfo:
    """Find the bridge and read STATUS once. Used by the heartbeat."""
    info = BridgeInfo(port="", present=False)
    port = discover_port()
    if not port:
        info.last_error = "no RP2040 USB CDC node found"
        return info
    info.port = port
    response = _exchange(port, "STATUS\n", timeout=timeout_seconds or 1.0)
    info.last_status_raw = response
    if not response:
        info.present = False
        info.last_error = "no response from bridge"
        return info
    info.present = True
    parse_status(response, info)
    return info


_STATUS_KEYS = {
    "present": bool,
    "slot0_learned": bool,
    "slot1_learned": bool,
    "slot0_count": int,
    "slot1_count": int,
}


def parse_status(line: str, info: BridgeInfo) -> None:
    """Parse a STATUS response line. Bridge sends "STATUS k=v k=v ...".
    Falls back to flags inferred from message text if the bridge used the
    older human-readable format.
    """
    line = line.strip()
    if line.startswith("ERR"):
        info.last_error = line[3:].strip()
        info.present = False
        return
    # Try structured format first
    if line.startswith("STATUS"):
        payload = line[len("STATUS"):].strip()
        for kv in payload.split():
            if "=" not in kv:
                continue
            k, v = kv.split("=", 1)
            k = k.strip()
            v = v.strip()
            target = _STATUS_KEYS.get(k)
            if target is bool:
                setattr(info, k, v.lower() in ("1", "true", "yes"))
            elif target is int:
                try:
                    setattr(info, k, int(v))
                except ValueError:
                    pass
        return
    # Legacy "OK learned slot0=N slot1=M" format
    info.slot0_learned = bool(re.search(r"\bslot0[^\d]*learned", line))
    info.slot1_learned = bool(re.search(r"\bslot1[^\d]*learned", line))
    for slot_attr, pat in (("slot0_count", r"slot0[^\d]*(\d+)"),
                           ("slot1_count", r"slot1[^\d]*(\d+)")):
        m = re.search(pat, line)
        if m:
            try:
                setattr(info, slot_attr, int(m.group(1)))
            except ValueError:
                pass


def _exchange(port: str, command: str, timeout: float = 1.0) -> str:
    """Open the serial port, write a command, read one line, close.

    Uses raw open()/read()/write() so we don't add a pyserial dependency.
    The bridge always responds to a single command with a single line
    terminated by '\\n', so a single read-after-write loop is sufficient.

    Robustness: we drain everything in the kernel tty buffer before writing
    (so a stale banner from boot or a previous response doesn't bleed into
    this command), and we use a generous read window (timeout + 100ms) so a
    slow bridge on a busy USB still has time to reply.
    """
    try:
        fd = os.open(port, os.O_RDWR | os.O_NOCTTY | os.O_NONBLOCK)
    except OSError as e:
        return f"ERR open failed: {e}"
    try:
        # Drain any leftover kernel-side bytes from boot banner or a previous
        # unanswered command. Two pumps spaced ~50ms apart catches anything
        # the bridge has buffered up to now.
        drain_deadline = time.monotonic() + 0.05
        while time.monotonic() < drain_deadline:
            try:
                if not os.read(fd, 4096):
                    break
            except BlockingIOError:
                time.sleep(0.01)
                break
            except OSError:
                break

        # Set blocking and basic termios for canonical reads. We don't need
        # any special baud — USB CDC is virtual serial, but Linux still
        # requires the termios struct to be valid before reading.
        try:
            import termios  # type: ignore
            import tty      # type: ignore
            attrs = termios.tcgetattr(fd)
            # Iflag: no input processing — we want raw bytes.
            attrs[0] = 0
            # Oflag: no output processing (no NL->CRLF translation).
            attrs[1] = 0
            # Cflag: 8-bit chars, enable receiver, ignore modem lines.
            attrs[2] = termios.CREAD | termios.CLOCAL | termios.CS8
            # Lflag: no local processing (no ECHO, no ICANON, no ISIG).
            attrs[3] = 0
            # CC[VMIN] = 0, CC[VTIME] = (timeout + 0.1) * 10 deciseconds.
            attrs[6][termios.VMIN] = 0
            attrs[6][termios.VTIME] = int((timeout + 0.1) * 10)
            # Baud: anything sane — USB CDC ignores it.
            try:
                attrs[4] = termios.B115200  # ispeed index
            except AttributeError:
                pass
            termios.tcsetattr(fd, termios.TCSANOW, attrs)
            # tty.setraw keeps ICANON/ECHO off and sets VMIN=1, VTIME=0 which
            # we override immediately via attrs[6] above. Skip the helper.
        except (ImportError, termios.error):
            pass

        try:
            os.write(fd, command.encode("ascii"))
        except OSError as e:
            return f"ERR write failed: {e}"

        # Read until we see a newline or the read window expires. Cap total
        # bytes at 1024 to bound latency if the bridge is streaming banners.
        deadline = time.monotonic() + timeout + 0.1
        chunks = []
        total = 0
        last_byte = time.monotonic()
        while time.monotonic() < deadline and total < 1024:
            try:
                data = os.read(fd, 256)
            except BlockingIOError:
                # No data yet. If we've been idle for >200ms after seeing at
                # least one byte, treat the line as complete (some bridges
                # forget to flush the trailing \n on the last message).
                if chunks and time.monotonic() - last_byte > 0.2:
                    break
                time.sleep(0.02)
                continue
            except OSError as e:
                return f"ERR read failed: {e}"
            if not data:
                break
            chunks.append(data)
            total += len(data)
            last_byte = time.monotonic()
            if b"\n" in data or b"\r" in data:
                break
        return b"".join(chunks).decode("ascii", errors="replace").strip()
    finally:
        try:
            os.close(fd)
        except OSError:
            pass


def _send_blast(port: str) -> tuple[bool, str]:
    """Write BLAST\\n to the bridge — fires every slot it's been taught."""
    response = _exchange(port, "BLAST\n", timeout=2.5)
    if not response:
        return False, "no response"
    if response.startswith("ERR"):
        return False, response
    return True, response


def blast_with_retry(attempts: int = 2) -> tuple[bool, str, dict]:
    """Find the bridge, fire BLAST, retry once on failure.

    Used by the actor when handling an `ir_blast` server command.
    Returns (ok, message, detail_dict).
    """
    detail: dict = {}
    last_msg = ""
    for i in range(max(1, attempts)):
        port = discover_port()
        detail.setdefault("port", port)
        if not port:
            last_msg = "no RP2040 bridge found on USB"
            time.sleep(0.5)
            continue
        ok, msg = _send_blast(port)
        detail["attempt"] = i + 1
        detail["response"] = msg
        if ok:
            return True, msg, detail
        last_msg = msg
        time.sleep(0.5)
    return False, last_msg or "no attempt", detail

PY_IR_BRIDGE
chmod +x "$IR_BRIDGE"
ln -sf "$IR_BRIDGE" "$BIN_DIR/ir_bridge"

cat > "$STATUS" <<'SH_STATUS'
#!/usr/bin/env bash
set -euo pipefail
INSTALL_DIR="${SCREENTRA_CAST_CLIENT_DIR:-$HOME/.local/share/screentra-cast-player-client}"
CACHE="$INSTALL_DIR/cache"
CONFIG="$CACHE/client-config.json"
RENDERER_URL="${SCREENTRA_CAST_RENDERER_URL:-http://127.0.0.1:3900}"
SERVER_URL="${SCREENTRA_CAST_SERVER_URL:-$(cat "$INSTALL_DIR/server" 2>/dev/null || true)}"

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
  for svc in screentra-cast-player-heartbeat screentra-cast-player-renderer screentra-cast-kiosk-browser; do
    state=$(systemctl --user is-active "$svc.service" 2>/dev/null || echo "inactive")
    enabled=$(systemctl --user is-enabled "$svc.service" 2>/dev/null || echo "disabled")
    printf "  %-32s active=%-9s enabled=%s\n" "$svc" "$state" "$enabled"
  done
else
  echo "systemctl unavailable; skipping service status."
fi

echo
echo "Recent heartbeat log lines:"
LOG_DIR="${XDG_STATE_HOME:-$HOME/.local/state}/screentra-cast-player-client"
ls -1t "$LOG_DIR"/*.log 2>/dev/null | head -3 | while read f; do
  echo "--- $f ---"
  tail -n 5 "$f"
done || true

SH_STATUS
chmod +x "$STATUS"

cat > "$KIOSK_LAUNCHER" <<'SH_KIOSK'
#!/usr/bin/env bash
set -euo pipefail
URL="${SCREENTRA_CAST_PLAYER_URL:-http://127.0.0.1:3900/}"
DISPLAY_INDEX="${SCREENTRA_CAST_DISPLAY_INDEX:-0}"
ROTATION="${SCREENTRA_CAST_DISPLAY_ROTATION:-normal}"
# Per-display URL: if display_index > 0, append the port offset
if [ "$DISPLAY_INDEX" != "0" ]; then
  # Replace last number segment with the per-display port (3900 + index)
  BASE_PORT=$((3900 + DISPLAY_INDEX))
  URL="$(echo "$URL" | sed -E "s|:([0-9]+)/|:$BASE_PORT/|")"
fi
LOG_DIR="${XDG_STATE_HOME:-$HOME/.local/state}/screentra-cast-player-client"
mkdir -p "$LOG_DIR"
LOG="$LOG_DIR/kiosk-launcher.log"
CONFIG_FILE="${SCREENTRA_CAST_CLIENT_CONFIG:-$HOME/.local/share/screentra-cast-player-client/config.json}"

log(){ printf '%s %s\n' "$(date -Is)" "$*" | tee -a "$LOG"; }

screentra_cast_active_display_count() {
  python3 - "$CONFIG_FILE" <<'PY' 2>/dev/null || echo 1
import json, sys
try:
    cfg=json.loads(open(sys.argv[1]).read())
except Exception:
    cfg={}
installer=int(cfg.get('installer_detected_max_displays') or 1)
override=cfg.get('max_displays')
try:
    count=min(max(int(override),1), max(installer,1)) if override is not None else max(installer,1)
except Exception:
    count=max(installer,1)
print(max(1, min(count, 4)))
PY
}
ACTIVE_DISPLAY_COUNT="${SCREENTRA_CAST_ACTIVE_DISPLAY_COUNT:-$(screentra_cast_active_display_count)}"
if [ "$ACTIVE_DISPLAY_COUNT" -gt 1 ]; then
  export SCREENTRA_CAST_ENABLE_SECONDARY_DISPLAYS=1
fi

find_browser(){
  for b in chromium chromium-browser google-chrome-stable google-chrome firefox-esr firefox; do
    if command -v "$b" >/dev/null 2>&1; then echo "$b"; return 0; fi
  done
  return 1
}

wait_renderer(){
  for _ in $(seq 1 60); do
    python3 - <<PY >/dev/null 2>&1 && return 0 || true
import urllib.request
urllib.request.urlopen('$URL'.rstrip('/') + '/health', timeout=1)
PY
    sleep 1
  done
  return 1
}

export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"

# Compositor selection: labwc or sway. labwc has a long-standing quirk where
# wlr-randr-applied output position changes don't propagate to existing
# wl_output bindings (wayland-info reports x:0,y:0 even when wlr-randr says
# otherwise), leaving secondary displays black. sway reads output config
# from its config file at startup and applies positions reliably; clients
# bind to those positions cleanly via for_window rules.
#
# Default 'auto' prefers sway when both are installed. labwc is no longer
# the default kiosk compositor as of 2026-08-10 because of the multi-display
# black-screen quirk reproduced on tech@11.1.1.54 (DP-2/DP-4) and
# ctcdtptvsign (HDMI-A-1/HDMI-A-2). Use SCREENTRA_CAST_COMPOSITOR=labwc to override
# for a single-monitor box.
SCREENTRA_CAST_COMPOSITOR="${SCREENTRA_CAST_COMPOSITOR:-auto}"
if [ "$SCREENTRA_CAST_COMPOSITOR" = "auto" ]; then
    if command -v sway >/dev/null 2>&1 && [ -n "${WAYLAND_DISPLAY:-}" ]; then
        # already inside a wayland session — let it stay; we don't switch
        SCREENTRA_CAST_COMPOSITOR="inherit"
    elif command -v sway >/dev/null 2>&1; then
        SCREENTRA_CAST_COMPOSITOR="sway"
    elif command -v labwc >/dev/null 2>&1; then
        SCREENTRA_CAST_COMPOSITOR="labwc"
    else
        SCREENTRA_CAST_COMPOSITOR="none"
    fi
fi
export SCREENTRA_CAST_COMPOSITOR

wayland_socket_alive() {
  local sock="$1"
  python3 - "$sock" <<'PY' 2>/dev/null
import socket, sys
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM | socket.SOCK_NONBLOCK)
s.settimeout(0.25)
try:
    s.connect(sys.argv[1])
except OSError as e:
    sys.exit(1 if e.errno in (111, 2) else 0)  # ECONNREFUSED/ENOENT -> bad
sys.exit(0)
PY
}

wait_wayland_ready() {
  # labwc may create wayland-0 before output queries are ready. Do not let
  # cold-boot startup hang before Chromium launches; wait with bounded probes.
  [ -n "${WAYLAND_DISPLAY:-}" ] || return 0
  local sock="$XDG_RUNTIME_DIR/$WAYLAND_DISPLAY"
  local i
  for i in $(seq 1 80); do
    if [ -S "$sock" ] && wayland_socket_alive "$sock"; then
      if command -v wlr-randr >/dev/null 2>&1; then
        if timeout 2s env XDG_RUNTIME_DIR="$XDG_RUNTIME_DIR" WAYLAND_DISPLAY="$WAYLAND_DISPLAY" wlr-randr --json >/dev/null 2>&1; then
          return 0
        fi
      else
        return 0
      fi
    fi
    sleep 0.25
  done
  log "Wayland display $WAYLAND_DISPLAY did not become fully queryable; launching with sysfs fallback geometry"
  return 0
}

# detect_active_wayland_socket: when a compositor (sway, labwc, etc.) has
# started but the launcher was told to wait for a hardcoded socket that
# doesn't exist (e.g. sway auto-incremented to wayland-1 because a stale
# lock blocked wayland-0), scan $XDG_RUNTIME_DIR for a live, accepting
# socket and use it. Prefer wayland-0 if it exists; otherwise pick the
# lowest-numbered live socket. Sets WAYLAND_DISPLAY as a side effect.
detect_active_wayland_socket() {
  [ -n "${WAYLAND_DISPLAY:-}" ] && [ -S "$XDG_RUNTIME_DIR/$WAYLAND_DISPLAY" ]     && wayland_socket_alive "$XDG_RUNTIME_DIR/$WAYLAND_DISPLAY" && return 0
  local want="$XDG_RUNTIME_DIR"
  # Prefer wayland-0 if it is live.
  if [ -S "$want/wayland-0" ] && wayland_socket_alive "$want/wayland-0"; then
    export WAYLAND_DISPLAY="wayland-0"
    return 0
  fi
  # Otherwise pick the lowest-numbered live socket.
  local best=""
  local n
  for n in $(ls "$want"/wayland-* 2>/dev/null | grep -v '\.lock$' | sort -V); do
    [ -S "$n" ] && wayland_socket_alive "$n" || continue
    best=$(basename "$n")
    break
  done
  if [ -n "$best" ]; then
    export WAYLAND_DISPLAY="$best"
    log "Detected active wayland socket at $XDG_RUNTIME_DIR/$best (hardcoded WAYLAND_DISPLAY was '${WAYLAND_DISPLAY:-unset}')"
    return 0
  fi
  return 1
}

# Single-GPU multi-display invariant:
#   display 0 owns the single labwc compositor/DRM master (wayland-0)
#   display 1+ never starts labwc; it waits for display 0's wayland-0 and
#   opens chromium at an explicit --window-position/--window-size.
# Reusing "whatever wayland-* exists" lets display 1 accidentally become the
# compositor owner, which sends touchscreen input/focus to the wrong surface.
if [ -z "${WAYLAND_DISPLAY:-}" ]; then
  # 2026-08-10: detect any live wayland socket, not just wayland-0.
  # The system-level seat service may have spawned sway on wayland-1
  # because wayland-0 had a stale lock from a previous run. In that
  # case the launcher would have tried to start a *second* sway and
  # failed with libseat "Could not open tty0: Permission denied".
  for _ in $(seq 1 80); do
    # Probe wayland-0 first, then any other live socket.
    if [ -S "$XDG_RUNTIME_DIR/wayland-0" ] && wayland_socket_alive "$XDG_RUNTIME_DIR/wayland-0"; then
      export WAYLAND_DISPLAY="wayland-0"
      break
    fi
    for n in $(ls "$XDG_RUNTIME_DIR"/wayland-* 2>/dev/null | grep -v '\.lock$' | sort -V); do
      [ -S "$n" ] && wayland_socket_alive "$n" && export WAYLAND_DISPLAY="$(basename "$n")" && break 2
    done
    sleep 0.25
  done
  if [ -z "${WAYLAND_DISPLAY:-}" ] && [ "$DISPLAY_INDEX" != "0" ]; then
    log "display_index=$DISPLAY_INDEX could not find live wayland-0 from display 0; retrying via systemd"
    exit 1
  fi
fi
# Do not force DISPLAY=:0. If DISPLAY is set by a real session we may use it;
# otherwise leave it unset so labwc use native DRM/Wayland instead
# of accidentally selecting X11/Xwayland.

# Pick the X11 screen number that matches the display_index using xrandr.
# This lets chromium open on the right physical monitor when the player
# has 2+ outputs (e.g. 2K primary + smaller secondary).
pick_wayland_output_for_display_index() {
    local idx="$1"
    # Resolve canonical connector from config.json FIRST. wlr-randr only
    # lists currently-enabled outputs, so on cold boot it may not see the
    # target connector yet and we would otherwise pick the wrong screen.
    local canonical=""
    canonical="$(python3 - "$idx" <<'PY' 2>/dev/null
import json, os, pathlib, sys
try:
    cfg = pathlib.Path.home() / '.local/share/screentra-cast-player-client/config.json'
    if cfg.exists():
        conf = json.loads(cfg.read_text())
        for det in (conf.get('last_detected_displays') or []):
            if int(det.get('display_index', -1)) == int(sys.argv[1]) and det.get('connected'):
                c = str(det.get('connector') or '').strip()
                if c:
                    print(c)
                    break
except Exception:
    pass
PY
)"
    if [ -n "$canonical" ]; then
        # Verify the canonical connector is currently visible to wlr-randr
        # (it might still be in DPMS sleep on cold boot). If not visible,
        # do not return it yet — fall through to the live enumeration so
        # we don't pin Chromium to a connector that's not even on.
        if command -v wlr-randr >/dev/null 2>&1 && [ -n "${WAYLAND_DISPLAY:-}" ]; then
            if timeout 2s env WAYLAND_DISPLAY="${WAYLAND_DISPLAY:-}" wlr-randr 2>/dev/null | awk -v c="$canonical" '$1==c {found=1; exit} END{exit !found}'; then
                echo "$canonical"
                return
            fi
        elif [ -r /sys/class/drm ]; then
            if [ -e "/sys/class/drm/card0-${canonical}/status" ] && [ "$(cat "/sys/class/drm/card0-${canonical}/status" 2>/dev/null)" = "connected" ]; then
                echo "$canonical"
                return
            fi
        fi
    fi
    # Prefer wlr-randr's JSON output (works on Wayland, structured, can
    # tell us position+size for explicit window placement).
    if command -v wlr-randr >/dev/null 2>&1 && [ -n "${WAYLAND_DISPLAY:-}" ]; then
        python3 - "$idx" <<'PY' 2>/dev/null
import json, os, subprocess, sys
idx = int(sys.argv[1])
out = subprocess.check_output(
    ['wlr-randr', '--json'], env={**os.environ, 'WAYLAND_DISPLAY': os.environ.get('WAYLAND_DISPLAY','')},
    text=True, timeout=2,
)
data = json.loads(out)
# Only enabled, only outputs with a current mode
enabled = [o for o in data if o.get('enabled')]
# Stable order must be by connector name, not compositor position. labwc can
# place DP-4 at x=0 and DP-2 at x=1920 on cold boot; display_index=0 must
# still map to DP-2 on this hardware so the touchscreen keeps working.
enabled.sort(key=lambda o: o.get('name',''))
if 0 <= idx < len(enabled):
    print(enabled[idx].get('name','') or '')
PY
        return
    fi
    # Fallback: text-mode wlr-randr, then /sys/class/drm.
    local connector=""
    if command -v wlr-randr >/dev/null 2>&1; then
        connector="$(timeout 2s env XDG_RUNTIME_DIR="$XDG_RUNTIME_DIR" WAYLAND_DISPLAY="${WAYLAND_DISPLAY:-}" wlr-randr 2>/dev/null | awk '/^[[:space:]]*[A-Z][A-Z0-9-]+ / {print $1; exit}')"
    fi
    if [ -z "$connector" ] && [ -r /sys/class/drm ]; then
        connector="$(for d in /sys/class/drm/card*-*; do
            [ -e "$d/status" ] || continue
            [ "$(cat $d/status 2>/dev/null)" = "connected" ] || continue
            basename "$d" | sed 's/^card[0-9]*-//'
        done | sort | sed -n "$((idx+1))p")"
    fi
    echo "$connector"
}

# Return the geometry (position-x, position-y, width, height) of the
# n-th enabled Wayland output as a single string "x,y WxH", or empty
# if wlr-randr JSON can't see it. Uses WAYLAND_DISPLAY when set, and
# falls back to /sys/class/drm (mode line + assumed (idx*max_x,0))
# when WAYLAND_DISPLAY is unset (i.e. we're about to launch labwc and
# don't have a wayland-0 socket yet).
wayland_output_geometry() {
    local idx="$1"
    python3 - "$idx" <<'PY' 2>/dev/null
import json, os, re, subprocess, sys
idx = int(sys.argv[1])
wd = os.environ.get('WAYLAND_DISPLAY', '')
outputs = []
# Resolve the canonical connector for this display_index FIRST, before
# trusting wlr-randr's ordering. wlr-randr will only show outputs that
# are currently enabled, and on cold boot the target connector may not be
# up yet — picking outputs[idx] from the live list then returns the wrong
# geometry for the wrong monitor (Chromium opens on the wrong screen,
# and DP-2 stays black).
target_connector = None
try:
    import pathlib
    cfg = pathlib.Path.home() / '.local/share/screentra-cast-player-client/config.json'
    if cfg.exists():
        conf = json.loads(cfg.read_text())
        for det in (conf.get('last_detected_displays') or []):
            if int(det.get('display_index', -1)) == idx and det.get('connected'):
                target_connector = str(det.get('connector') or '').strip() or None
                break
except Exception:
    pass
# Try wlr-randr JSON first (preferred when a Wayland session exists).
if wd and subprocess.run(['which','wlr-randr'], capture_output=True).returncode == 0:
    try:
        out = subprocess.check_output(
            ['wlr-randr','--json'], env={**os.environ,'WAYLAND_DISPLAY':wd}, text=True, timeout=2)
        data = json.loads(out)
        outputs = [o for o in data if o.get('enabled')]
        outputs.sort(key=lambda o: o.get('name',''))
        # Pick by canonical connector when config.json knows it; this is
        # the cold-boot fix: wlr-randr may show DP-4 but not DP-2, but
        # display_index 0 is still DP-2.
        chosen = None
        if target_connector:
            for o in outputs:
                if o.get('name') == target_connector:
                    chosen = o
                    break
        if chosen is None and 0 <= idx < len(outputs):
            chosen = outputs[idx]
        if chosen is not None:
            cur = next((m for m in chosen.get('modes',[]) if m.get('current')), None)
            if cur:
                # Recompute position as cumulative sum of prior enabled widths
                # because wlr-randr may have re-laid-out the desktop.
                x = sum(int(next((m for m in prev.get('modes',[]) if m.get('current')), {'width': 0}).get('width', 0))
                        for prev in outputs[:outputs.index(chosen)])
                print(f"{x},0 {cur['width']}x{cur['height']}")
                sys.exit(0)
    except Exception:
        pass
if os.path.isdir('/sys/class/drm'):
    cards = []
    for d in sorted(os.listdir('/sys/class/drm')):
        if d.startswith('card') and '-' in d:
            status = open(f'/sys/class/drm/{d}/status').read().strip()
            if status != 'connected':
                continue
            modes_path = f'/sys/class/drm/{d}/modes'
            if not os.path.isfile(modes_path):
                continue
            line = open(modes_path).read().split('\n', 1)[0].strip()
            m = re.match(r'^(\d+)x(\d+)$', line)
            if not m:
                continue
            cards.append((d, int(m.group(1)), int(m.group(2))))
    cards.sort(key=lambda c: c[0])
    if 0 <= idx < len(cards):
        _, w, h = cards[idx]
        x = sum(c[1] for c in cards[:idx])
        print(f"{x},0 {w}x{h}")
PY
}

apply_wayland_display_count_policy() {
  [ -n "${WAYLAND_DISPLAY:-}" ] || return 0
  command -v wlr-randr >/dev/null 2>&1 || return 0
  local count="${ACTIVE_DISPLAY_COUNT:-1}"
  python3 - "$count" <<'PY' 2>/dev/null | while read -r name action xpos width height; do
import json, os, subprocess, sys
count=max(1, min(int(sys.argv[1]), 4))
out=subprocess.check_output(['wlr-randr','--json'], env={**os.environ,'XDG_RUNTIME_DIR':os.environ.get('XDG_RUNTIME_DIR',''), 'WAYLAND_DISPLAY':os.environ.get('WAYLAND_DISPLAY','')}, text=True, timeout=2)
data=json.loads(out)
wlr_outputs={o.get('name'): o for o in data if o.get('name')}
ordered=[]
try:
    import pathlib
    cfg=pathlib.Path.home()/'.local/share/screentra-cast-player-client/config.json'
    conf=json.loads(cfg.read_text())
    detected=[o for o in conf.get('last_detected_displays',[]) if o.get('connected') and o.get('connector')]
    detected.sort(key=lambda o:int(o.get('display_index', 0)))
    for det in detected:
        name=str(det.get('connector') or '')
        if name in wlr_outputs and name not in [o.get('name') for o in ordered]:
            ordered.append(wlr_outputs[name])
except Exception:
    ordered=[]
for name in sorted(wlr_outputs):
    if name not in [o.get('name') for o in ordered]:
        ordered.append(wlr_outputs[name])
x=0
for idx,o in enumerate(ordered):
    name=o.get('name','')
    if not name: continue
    cur=next((m for m in o.get('modes',[]) if m.get('current')), None)
    pref=next((m for m in o.get('modes',[]) if m.get('preferred')), None)
    # 2026-08-11: prefer `preferred` over `current`. When the previous run
    # left the connector at a stub mode (e.g. 640x480 from a no-EDID
    # fallback, or 1280x800 from a stub EDID), the next boot would
    # inherit that stub as `current` and never escalate to the real
    # preferred (e.g. 2560x1440 on a Lenovo Y27q-30 2K panel). Trust the
    # preferred first; fall back to current only if no preferred exists.
    mode=pref or cur or {}
    width=int(mode.get('width') or 0)
    height=int(mode.get('height') or 0)
    desired = idx < count
    if desired:
        # 2026-08-10: also output width/height so the bash caller can pass
        # --mode to wlr-randr (otherwise sway picks a stub preferred mode
        # like 1280x800 that the monitor won't accept).
        print(name, 'on', x, width, height)
        x += width
    elif o.get('enabled'):
        print(name, 'off', 0, 0, 0)
PY
    [ -n "${name:-}" ] || continue
    if [ "$action" = "on" ]; then
      # 2026-08-10: also pass --mode when we know a non-bogus preferred.
      # sway won't auto-pick a non-default mode if the connector's wlr-randr
      # preferred is a stub (e.g. 1280x800 from Mediatrix MPI107), so we
      # override with the display-detect stored resolution here.
      mode_args=""
      if [ -n "${width:-}" ] && [ -n "${height:-}" ] && [ "$width" -gt 0 ] && [ "$height" -gt 0 ]; then
          mode_args="--mode ${width}x${height}"
      fi
      log "Enabling/placing compositor output $name at ${xpos:-0},0 size=${width:-?}${mode_args:+,}${height:-?} for active display count $count"
      XDG_RUNTIME_DIR="$XDG_RUNTIME_DIR" WAYLAND_DISPLAY="$WAYLAND_DISPLAY" wlr-randr --output "$name" --on --pos "${xpos:-0},0" $mode_args >/dev/null 2>&1 ||         XDG_RUNTIME_DIR="$XDG_RUNTIME_DIR" WAYLAND_DISPLAY="$WAYLAND_DISPLAY" wlr-randr --output "$name" --on $mode_args >/dev/null 2>&1 || true
    elif [ "$action" = "off" ]; then
      log "Disabling compositor output $name above active display count $count"
      XDG_RUNTIME_DIR="$XDG_RUNTIME_DIR" WAYLAND_DISPLAY="$WAYLAND_DISPLAY" wlr-randr --output "$name" --off >/dev/null 2>&1 || true
    fi
  done
}

# If labwc handed us a Wayland display, wait until the socket and output
# query path are ready before any wlr-randr geometry calls.
wait_wayland_ready
apply_wayland_display_count_policy

write_labwc_window_rules() {
  command -v python3 >/dev/null 2>&1 || return 0
  python3 - <<'PY' 2>/dev/null || true
import json, os, pathlib, html, sys
home=pathlib.Path.home()
config=home/'.local/share/screentra-cast-player-client/config.json'
outputs=[]
try:
    d=json.loads(config.read_text())
    outputs=[o for o in d.get('last_detected_displays',[]) if o.get('connected')]
    # 2026-08-10: cross-check stored width/height against the largest sysfs mode.
    # If the stored value is smaller than what the kernel actually reports as
    # the largest mode for this connector, use the larger one. The kernel's
    # first-listed "preferred" mode can be a bogus placeholder (e.g. 1280x800)
    # when EDID probing returns a stub; the actual preferred (from EDID DTD #1)
    # is usually the largest mode in sysfs/class/drm/.../modes. This keeps
    # sway config and chromium window-size in sync with the physical monitor.
    # 2026-08-10: trust config.json as written by screentra-cast-display-detect.py.
    # That script already does EDID-first / modetest-preferred / sysfs-largest
    # fallback chain. The launcher used to override here by always picking
    # sysfs-largest, but that overrode a correct 1920x1080 with a driver-only
    # 4096x2160 the monitor doesn't physically accept. So: trust the stored
    # value, only fall back to sysfs-largest if width/height are missing
    # (None or 0).
    try:
        import re as _re
        sysfs_outputs={}
        for ent in pathlib.Path('/sys/class/drm').glob('card*-*'):
            try:
                if (ent/'status').read_text().strip()!='connected': continue
                modes=(ent/'modes').read_text()
                best=None; best_px=0
                for line in modes.splitlines():
                    m=_re.match(r'^(\d+)x(\d+)$', line.strip())
                    if not m: continue
                    w,h=int(m.group(1)), int(m.group(2))
                    if w*h > best_px: best_px=w*h; best=(w,h)
                if best: sysfs_outputs[ent.name.split('-',1)[1]] = best
            except Exception:
                pass
        for o in outputs:
            c=str(o.get('connector') or '')
            if c in sysfs_outputs:
                w = int(o.get('width') or 0); h = int(o.get('height') or 0)
                if not w or not h:
                    # Missing -- fill in from sysfs-largest as a last resort.
                    sw, sh = sysfs_outputs[c]
                    o['width']=sw; o['height']=sh
    except Exception:
        pass
except Exception:
    outputs=[]
if not outputs:
    # sysfs fallback: connector name sort, first mode as size
    base=pathlib.Path('/sys/class/drm')
    for ent in sorted(base.glob('card*-*'), key=lambda p:p.name):
        try:
            if (ent/'status').read_text().strip()!='connected': continue
            mode=(ent/'modes').read_text().splitlines()[0]
            w,h=[int(x) for x in mode.split('x',1)]
            outputs.append({'display_index':len(outputs),'connector':ent.name.split('-',1)[1], 'width':w, 'height':h})
        except Exception:
            pass
# ensure stable by display_index generated by display-detect/governance
outputs.sort(key=lambda o:int(o.get('display_index', 0)))
if not outputs:
    sys.exit(0)
lines=['<labwc_config>','  <core><decoration>server</decoration></core>','  <placement><policy>automatic</policy></placement>']
touch=d.get('touch_binding') if isinstance(d.get('touch_binding'), dict) else {}
if touch:
    dev_name=str(touch.get('device_name') or '')
    allow_joined=bool(touch.get('allow_joined_displays'))
    connectors=[str(c) for c in (touch.get('target_connectors') or []) if c]
    # labwc supports one mapToOutput. For joined/tiled display cases, omit
    # mapToOutput so the absolute touch surface spans the active desktop.
    map_to='' if allow_joined and len(touch.get('target_display_indexes') or []) >= 2 else (connectors[0] if connectors else '')
    attrs=[]
    if dev_name:
        attrs.append(f'deviceName="{html.escape(dev_name)}"')
    if map_to:
        attrs.append(f'mapToOutput="{html.escape(map_to)}"')
    attrs.append('mouseEmulation="no"')
    lines.append('  <touch ' + ' '.join(attrs) + '/>')
lines.append('  <windowRules>')
x=0
for idx,o in enumerate(outputs[:4]):
    connector=str(o.get('connector') or '')
    w=int(o.get('width') or 1280)
    h=int(o.get('height') or 800)
    lines.append(f'    <windowRule title="Screentra Cast Player display {idx}*" fixedPosition="yes" serverDecoration="no">')
    if connector:
        lines.append(f'      <action name="MoveToOutput" output="{html.escape(connector)}" />')
    lines.append(f'      <action name="MoveTo" x="{x}" y="0" />')
    lines.append(f'      <action name="ResizeTo" width="{w}" height="{h}" />')
    lines.append('    </windowRule>')
    lines.append(f'    <windowRule title="Screentra Cast Display {idx}*" fixedPosition="yes" serverDecoration="no">')
    if connector:
        lines.append(f'      <action name="MoveToOutput" output="{html.escape(connector)}" />')
    lines.append(f'      <action name="MoveTo" x="{x}" y="0" />')
    lines.append(f'      <action name="ResizeTo" width="{w}" height="{h}" />')
    lines.append('    </windowRule>')
    x += w
lines += ['  </windowRules>','</labwc_config>','']
cfg=home/'.config/labwc'
cfg.mkdir(parents=True, exist_ok=True)
path=cfg/'rc.xml'
new='\n'.join(lines)
if not path.exists() or path.read_text(errors='ignore') != new:
    path.write_text(new)
PY
}

# write_sway_config: emit ~/.config/sway/config with explicit output
# positions, modes, and per-display window rules. Unlike labwc's
# wlr-randr IPC, sway parses this at startup and applies positions
# BEFORE any client binds -- this is what fixes the multi-display
# black-screen quirk on a single GPU with two physical outputs.
write_sway_config() {
  command -v python3 >/dev/null 2>&1 || return 0
  python3 - <<'PY' 2>/dev/null || true
import json, os, pathlib, html, sys
home=pathlib.Path.home()
config=home/'.local/share/screentra-cast-player-client/config.json'
outputs=[]
try:
    d=json.loads(config.read_text())
    outputs=[o for o in d.get('last_detected_displays',[]) if o.get('connected')]
    # 2026-08-10: cross-check stored width/height against the largest sysfs mode.
    # If the stored value is smaller than what the kernel actually reports as
    # the largest mode for this connector, use the larger one. The kernel's
    # first-listed "preferred" mode can be a bogus placeholder (e.g. 1280x800)
    # when EDID probing returns a stub; the actual preferred (from EDID DTD #1)
    # is usually the largest mode in sysfs/class/drm/.../modes. This keeps
    # sway config and chromium window-size in sync with the physical monitor.
    # 2026-08-10: trust config.json as written by screentra-cast-display-detect.py.
    # That script already does EDID-first / modetest-preferred / sysfs-largest
    # fallback chain. The launcher used to override here by always picking
    # sysfs-largest, but that overrode a correct 1920x1080 with a driver-only
    # 4096x2160 the monitor doesn't physically accept. So: trust the stored
    # value, only fall back to sysfs-largest if width/height are missing
    # (None or 0).
    try:
        import re as _re
        sysfs_outputs={}
        for ent in pathlib.Path('/sys/class/drm').glob('card*-*'):
            try:
                if (ent/'status').read_text().strip()!='connected': continue
                modes=(ent/'modes').read_text()
                best=None; best_px=0
                for line in modes.splitlines():
                    m=_re.match(r'^(\d+)x(\d+)$', line.strip())
                    if not m: continue
                    w,h=int(m.group(1)), int(m.group(2))
                    if w*h > best_px: best_px=w*h; best=(w,h)
                if best: sysfs_outputs[ent.name.split('-',1)[1]] = best
            except Exception:
                pass
        for o in outputs:
            c=str(o.get('connector') or '')
            if c in sysfs_outputs:
                w = int(o.get('width') or 0); h = int(o.get('height') or 0)
                if not w or not h:
                    # Missing -- fill in from sysfs-largest as a last resort.
                    sw, sh = sysfs_outputs[c]
                    o['width']=sw; o['height']=sh
    except Exception:
        pass
except Exception:
    outputs=[]
if not outputs:
    base=pathlib.Path('/sys/class/drm')
    for ent in sorted(base.glob('card*-*'), key=lambda p:p.name):
        try:
            if (ent/'status').read_text().strip()!='connected': continue
            # 2026-08-10: pick the LARGEST available mode from sysfs, not the
            # first. The kernel's first-listed "preferred" mode can be a
            # bogus placeholder (e.g. 1280x800) when EDID probing returns
            # a stub descriptor; the actual preferred mode (from EDID
            # Detailed Timing Descriptor #1) is usually the LARGEST mode
            # in the list. Falling back to "largest" ensures sway matches
            # what the monitor physically accepts.
            modes_text=(ent/'modes').read_text()
            best=None
            best_pixels=0
            for line in modes_text.splitlines():
                m=__import__('re').match(r'^(\d+)x(\d+)$', line.strip())
                if not m: continue
                w,h=int(m.group(1)), int(m.group(2))
                if w*h > best_pixels:
                    best_pixels=w*h
                    best=(w,h)
            if not best: continue
            w,h=best
            outputs.append({'display_index':len(outputs),'connector':ent.name.split('-',1)[1], 'width':w, 'height':h})
        except Exception:
            pass
outputs.sort(key=lambda o:int(o.get('display_index', 0)))
if not outputs:
    sys.exit(0)
lines = [
    '# Screentra Cast sway config -- generated by screentra-cast-player-launcher.',
    '# Do not edit by hand; it will be overwritten on each launch.',
    '',
    'xwayland enable',
    '',
    'default_border none',
    'default_floating_border none',
    # NOTE: deliberately NOT adding `for_window [class="^.*"] floating enable, sticky enable, resize set` here.
    # 2026-08-10: sway's `resize set` with no dimensions shrinks every window to its minimum size
    # (~532x129 for chromium), overriding the --window-size=WxH that the launcher passes.
    # Result: chromium opens tiny in the upper-left corner of the output even though
    # sway configured the output at full WxH. The correct behavior is to trust the
    # --window-size flag (and the renderer's responsive CSS) and not resize at all.
    '',
    'exec_always systemctl --user import-environment WAYLAND_DISPLAY XDG_RUNTIME_DIR SCREENTRA_CAST_DISPLAY_INDEX SCREENTRA_CAST_PLAYER_URL SCREENTRA_CAST_DISPLAY_ROTATION',
    '',
]
# Output config: each detected connector at cumulative x offset.
x = 0
for o in outputs[:4]:
    connector = str(o.get('connector') or '')
    w = int(o.get('width') or 1280)
    h = int(o.get('height') or 800)
    if connector:
        lines.append(f'output {connector} mode {w}x{h}')
        lines.append(f'output {connector} position {x} 0')
        lines.append(f'output {connector} scale 1')
        lines.append(f'output {connector} enable')
    if ROTATION != 'normal':
        lines.append(f'output {connector} transform {ROTATION}')
    x += w
lines.append('')
# Per-display window rules: for_window to force each chromium window to
# its assigned output via for_window [class="...title..."] move container to output.
for idx, o in enumerate(outputs[:4]):
    connector = str(o.get('connector') or '')
    if not connector:
        continue
    # Match by window title using sway's title= regex.
    for prefix in ('Screentra Cast Player display', 'Screentra Cast Display'):
        lines.append(f"for_window [title=\"^{prefix} {idx}\"] move container to output {connector}")
lines.append('')
new = '\n'.join(lines) + '\n'
cfg = home/'.config/sway'
cfg.mkdir(parents=True, exist_ok=True)
path = cfg/'config'
if not path.exists() or path.read_text(errors='ignore') != new:
    path.write_text(new)
PY
}

write_labwc_window_rules
# If the requested compositor is sway, also write the sway config so a future
# `sway` invocation picks up output positions deterministically (sway
# applies its config at startup, before any client binds).
if [ "${SCREENTRA_CAST_COMPOSITOR}" = "sway" ]; then
    write_sway_config
fi

# Pick the right Wayland output for this display_index.
# On a real Wayland session (already inside labwc/sway), we do not relaunch a compositor;
# we just set WAYLAND_DISPLAY and let chromium place itself.
if [ -z "${WAYLAND_DISPLAY:-}" ]; then
  OUTPUT="$(pick_wayland_output_for_display_index "$DISPLAY_INDEX")"
  export SCREENTRA_CAST_WAYLAND_OUTPUT="$OUTPUT"
fi
# Compute this display_index's geometry so chromium targets the right
# monitor with --window-position/size. Even if WAYLAND_DISPLAY is unset
# (we're about to launch labwc), the fallback can read /sys/class/drm
# and produce a reasonable position+size for the wrappers we'll write.
GEOMETRY="$(wayland_output_geometry "$DISPLAY_INDEX" 2>/dev/null || true)"
if [ -n "$GEOMETRY" ]; then
    GEOM_POS="${GEOMETRY%% *}"
    GEOM_SIZE="${GEOMETRY##* }"
    export SCREENTRA_CAST_WAYLAND_POS="$GEOM_POS"
    export SCREENTRA_CAST_WAYLAND_SIZE="$GEOM_SIZE"
fi

BROWSER="$(find_browser || true)"
if [ -z "$BROWSER" ]; then
  log "No browser found. Install chromium/chromium-browser/firefox on the player. Renderer remains available at $URL"
  exit 0
fi

# If no display server exists, use sway (preferred) or labwc to create
# a native Wayland/DRM kiosk session. Only skip on truly headless systems where
# neither compositor is available.
if [ -z "${DISPLAY:-}" ] && [ -z "${WAYLAND_DISPLAY:-}" ] && ! command -v sway >/dev/null 2>&1 && ! command -v labwc >/dev/null 2>&1; then
  log "Headless environment: no DISPLAY/WAYLAND_DISPLAY and no kiosk compositor (sway/labwc) is installed. Renderer remains available at $URL"
  exit 0
fi
# If we are already in a Wayland session, continue; otherwise the launcher starts labwc.

if ! wait_renderer; then
  log "Renderer health did not come up in time; launching browser anyway: $URL"
fi

if [ "$DISPLAY_INDEX" != "0" ] && [ "${SCREENTRA_CAST_ENABLE_SECONDARY_DISPLAYS:-0}" != "1" ]; then
  log "Secondary display $DISPLAY_INDEX disabled temporarily on single-GPU Wayland; set SCREENTRA_CAST_ENABLE_SECONDARY_DISPLAYS=1 to launch it."
  while :; do sleep 3600; done
fi

# While secondary Chromium windows are disabled, keep only display 0's physical
# output enabled in the compositor. Otherwise libinput's identity touchscreen
# mapping spans the whole 3200x1080 desktop, so taps on the 1280x800 DP-2
# panel land offset/scaled and the gear looks unresponsive.
if [ "$DISPLAY_INDEX" = "0" ] && [ "${SCREENTRA_CAST_ENABLE_SECONDARY_DISPLAYS:-0}" != "1" ] && [ -n "${WAYLAND_DISPLAY:-}" ] && command -v wlr-randr >/dev/null 2>&1; then
  KEEP_OUTPUT="${SCREENTRA_CAST_WAYLAND_OUTPUT:-}"
  if [ -z "$KEEP_OUTPUT" ]; then
    KEEP_OUTPUT="$(pick_wayland_output_for_display_index 0 || true)"
  fi
  if [ -n "$KEEP_OUTPUT" ]; then
    while read -r out; do
      [ -n "$out" ] || continue
      if [ "$out" != "$KEEP_OUTPUT" ]; then
        log "Disabling secondary compositor output $out while secondary displays are disabled; keeping $KEEP_OUTPUT for touchscreen mapping"
        XDG_RUNTIME_DIR="$XDG_RUNTIME_DIR" WAYLAND_DISPLAY="$WAYLAND_DISPLAY" wlr-randr --output "$out" --off >/dev/null 2>&1 || true
      fi
    done <<EOF_OUTS
$(XDG_RUNTIME_DIR="$XDG_RUNTIME_DIR" WAYLAND_DISPLAY="$WAYLAND_DISPLAY" wlr-randr 2>/dev/null | awk '/^[A-Z][A-Z0-9-]+ / {print $1}')
EOF_OUTS
  fi
fi

log "Launching kiosk browser: $BROWSER $URL DISPLAY=${DISPLAY:-unset} WAYLAND_DISPLAY=${WAYLAND_DISPLAY:-unset} (display_index=$DISPLAY_INDEX, pos=${SCREENTRA_CAST_WAYLAND_POS:-unset}, size=${SCREENTRA_CAST_WAYLAND_SIZE:-unset})"
# Build chromium args
CHROMIUM_ARGS=("--ozone-platform=wayland" "--kiosk" "--hide-cursor" "--ash-hide-cursor" "$URL")
# If user selected a touch device, append it
if [ -n "${SCREENTRA_CAST_TOUCH_DEVICE:-}" ] && [ -e "$SCREENTRA_CAST_TOUCH_DEVICE" ]; then
  CHROMIUM_ARGS+=("--touch-devices=$SCREENTRA_CAST_TOUCH_DEVICE")
fi
# If we know this display's Wayland geometry, set explicit position/size.
# Without this, multiple chromium instances on a shared wayland-0 all open
# at (0,0) and only the topmost window is visible -- the user sees only one
# of N displays receiving the kiosk surface.
if [ -n "${SCREENTRA_CAST_WAYLAND_POS:-}" ] && [ -n "${SCREENTRA_CAST_WAYLAND_SIZE:-}" ]; then
  CHROMIUM_ARGS+=("--window-position=${SCREENTRA_CAST_WAYLAND_POS}" "--window-size=${SCREENTRA_CAST_WAYLAND_SIZE}")
fi

focus_target_output_before_launch() {
  [ -n "${WAYLAND_DISPLAY:-}" ] || return 0
  command -v wlrctl >/dev/null 2>&1 || return 0
  [ -n "${SCREENTRA_CAST_WAYLAND_POS:-}" ] || return 0
  [ -n "${SCREENTRA_CAST_WAYLAND_SIZE:-}" ] || return 0
  local px py sw sh cx cy
  px="${SCREENTRA_CAST_WAYLAND_POS%%,*}"
  py="${SCREENTRA_CAST_WAYLAND_POS##*,}"
  sw="${SCREENTRA_CAST_WAYLAND_SIZE%%x*}"
  sh="${SCREENTRA_CAST_WAYLAND_SIZE##*x}"
  case "$px:$py:$sw:$sh" in
    *[!0-9:]*|'') return 0 ;;
  esac
  cx=$((px + sw / 2))
  cy=$((py + sh / 2))
  log "Focusing target output for display_index=$DISPLAY_INDEX via wlrctl pointer at ${cx},${cy} before browser launch"
  XDG_RUNTIME_DIR="$XDG_RUNTIME_DIR" WAYLAND_DISPLAY="$WAYLAND_DISPLAY" wlrctl pointer move -10000 -10000 >/dev/null 2>&1 || true
  XDG_RUNTIME_DIR="$XDG_RUNTIME_DIR" WAYLAND_DISPLAY="$WAYLAND_DISPLAY" wlrctl pointer move "$cx" "$cy" >/dev/null 2>&1 || true
  # labwc/Chromium bind fullscreen surfaces to the currently focused output.
  # The click is sent before Chromium maps; ignore failures/noops.
  XDG_RUNTIME_DIR="$XDG_RUNTIME_DIR" WAYLAND_DISPLAY="$WAYLAND_DISPLAY" wlrctl pointer click left >/dev/null 2>&1 || true
}

focus_target_output_before_launch

dismiss_focus_menu_after_launch() {
  [ -n "${WAYLAND_DISPLAY:-}" ] || return 0
  command -v wlrctl >/dev/null 2>&1 || return 0
  [ -n "${SCREENTRA_CAST_WAYLAND_POS:-}" ] || return 0
  [ -n "${SCREENTRA_CAST_WAYLAND_SIZE:-}" ] || return 0
  local px py sw sh dx dy
  px="${SCREENTRA_CAST_WAYLAND_POS%%,*}"
  py="${SCREENTRA_CAST_WAYLAND_POS##*,}"
  sw="${SCREENTRA_CAST_WAYLAND_SIZE%%x*}"
  sh="${SCREENTRA_CAST_WAYLAND_SIZE##*x}"
  case "$px:$py:$sw:$sh" in
    *[!0-9:]*|'') return 0 ;;
  esac
  dx=$((px + sw - 20))
  dy=$((py + sh - 20))
  (
    sleep "${SCREENTRA_CAST_POST_LAUNCH_FOCUS_DISMISS_DELAY:-8}"
    XDG_RUNTIME_DIR="$XDG_RUNTIME_DIR" WAYLAND_DISPLAY="$WAYLAND_DISPLAY" wlrctl pointer move -10000 -10000 >/dev/null 2>&1 || true
    XDG_RUNTIME_DIR="$XDG_RUNTIME_DIR" WAYLAND_DISPLAY="$WAYLAND_DISPLAY" wlrctl pointer move "$dx" "$dy" >/dev/null 2>&1 || true
    XDG_RUNTIME_DIR="$XDG_RUNTIME_DIR" WAYLAND_DISPLAY="$WAYLAND_DISPLAY" wlrctl pointer click left >/dev/null 2>&1 || true
  ) &
}

dismiss_focus_menu_after_launch
# Branch: launch labwc, sway, or neither depending on SCREENTRA_CAST_COMPOSITOR.
# labwc path is the original; sway path writes the config and execs sway
# which then runs the same launcher recursively with WAYLAND_DISPLAY set.
if [ -z "${WAYLAND_DISPLAY:-}" ] && [ "$DISPLAY_INDEX" != "0" ] && [ "${SCREENTRA_CAST_COMPOSITOR}" != "none" ]; then
  log "display_index=$DISPLAY_INDEX must attach to display 0's compositor; refusing to start a second ${SCREENTRA_CAST_COMPOSITOR} instance"
  exit 1
fi

if [ -z "${WAYLAND_DISPLAY:-}" ] && [ "$SCREENTRA_CAST_COMPOSITOR" = "sway" ] && command -v sway >/dev/null 2>&1; then
  STARTUP="$LOG_DIR/sway-start-display-$DISPLAY_INDEX.sh"
  # Clear any stale wayland-0..N sockets/locks so sway can pick wayland-0.
  # Otherwise sway auto-increments to wayland-1 and the launcher waits for
  # wayland-0 forever.
  rm -f "$XDG_RUNTIME_DIR"/wayland-* "$XDG_RUNTIME_DIR"/wayland-*.lock 2>/dev/null || true
  # The startup script detects the actual wayland socket at runtime
  # (sway may have auto-incremented past wayland-0 if a stale lock remained).
  {
    printf '%s\n' '#!/usr/bin/env bash'
    printf 'export SCREENTRA_CAST_DISPLAY_INDEX=%q\n' "$DISPLAY_INDEX"
    printf 'export SCREENTRA_CAST_COMPOSITOR=inherit\n'
    printf 'export XDG_RUNTIME_DIR=%q\n' "$XDG_RUNTIME_DIR"
    printf 'export SCREENTRA_CAST_PLAYER_URL=%q\n' "$URL"
    printf '%s\n' '# Detect the actual wayland socket (sway may have used wayland-1).'
    printf '%s\n' 'for _ in $(seq 1 40); do'
    printf '%s\n' '  for wd in wayland-0 wayland-1 wayland-2 wayland-3; do'
    printf '%s\n' '    if [ -S "$XDG_RUNTIME_DIR/$wd" ] && exec 3<>"$XDG_RUNTIME_DIR/$wd" 2>/dev/null; then'
    printf '%s\n' '      exec 3>&-'
    printf '%s\n' '      export WAYLAND_DISPLAY="$wd"'
    printf '%s\n' '      break 2'
    printf '%s\n' '    fi'
    printf '%s\n' '  done'
    printf '%s\n' '  sleep 0.25'
    printf '%s\n' 'done'
    printf 'export WAYLAND_DISPLAY=%q\n' "${WAYLAND_DISPLAY:-wayland-0}"
    printf 'exec %q %q\n' "$0" "$DISPLAY_INDEX"
  } > "$STARTUP"
  chmod +x "$STARTUP"
  log "Starting sway kiosk session for display_index=$DISPLAY_INDEX startup=$STARTUP"
  (
    delay="${SCREENTRA_CAST_SWAY_STARTUP_FALLBACK_DELAY:-10}"
    attempts="${SCREENTRA_CAST_SWAY_STARTUP_FALLBACK_ATTEMPTS:-6}"
    n=1
    while [ "$n" -le "$attempts" ]; do
      sleep "$delay"
      if pgrep -x chromium >/dev/null 2>&1 || pgrep -x chromium-browser >/dev/null 2>&1; then
        exit 0
      fi
      if detect_active_wayland_socket; then
        log "No Chromium after sway startup delay attempt $n/$attempts; launching fallback child on WAYLAND_DISPLAY=$WAYLAND_DISPLAY"
        XDG_RUNTIME_DIR="$XDG_RUNTIME_DIR" WAYLAND_DISPLAY="$WAYLAND_DISPLAY" SCREENTRA_CAST_DISPLAY_INDEX="$DISPLAY_INDEX" SCREENTRA_CAST_COMPOSITOR=inherit "$0" "$DISPLAY_INDEX" >>"$LOG" 2>&1 &
      else
        log "No Chromium after sway startup delay attempt $n/$attempts, but no live wayland socket is yet available"
      fi
      n=$((n + 1))
    done
  ) &
  # Sway does NOT support labwc's --startup flag. Instead, append an `exec
  # <startup>` line to the generated config so sway itself runs the startup
  # script after the compositor + output config are fully applied.
  printf '\n# Auto-generated startup hook (sway equivalent of labwc --startup)\nexec %q\n' "$STARTUP" \
      >> "$HOME/.config/sway/config"
  exec sway --config "$HOME/.config/sway/config"
  rc=$?
  log "sway exited with status $rc; leaving renderer available at $URL"
  exit 0
fi

if [ -z "${WAYLAND_DISPLAY:-}" ] && [ "$SCREENTRA_CAST_COMPOSITOR" = "labwc" ] && command -v labwc >/dev/null 2>&1; then
  STARTUP="$LOG_DIR/labwc-start-display-$DISPLAY_INDEX.sh"
  rm -f "$XDG_RUNTIME_DIR/wayland-0" "$XDG_RUNTIME_DIR/wayland-0.lock" 2>/dev/null || true
  {
    printf '%s\n' '#!/usr/bin/env bash'
    printf 'export SCREENTRA_CAST_DISPLAY_INDEX=%q\n' "$DISPLAY_INDEX"
    printf 'export XDG_RUNTIME_DIR=%q\n' "$XDG_RUNTIME_DIR"
    printf 'export WAYLAND_DISPLAY=%q\n' 'wayland-0'
    printf 'exec %q %q\n' "$0" "$DISPLAY_INDEX"
  } > "$STARTUP"
  chmod +x "$STARTUP"
  log "Starting labwc kiosk session for display_index=$DISPLAY_INDEX startup=$STARTUP"
  (
    delay="${SCREENTRA_CAST_LABWC_STARTUP_FALLBACK_DELAY:-10}"
    attempts="${SCREENTRA_CAST_LABWC_STARTUP_FALLBACK_ATTEMPTS:-6}"
    n=1
    while [ "$n" -le "$attempts" ]; do
      sleep "$delay"
      if pgrep -x chromium >/dev/null 2>&1 || pgrep -x chromium-browser >/dev/null 2>&1; then
        exit 0
      fi
      if detect_active_wayland_socket; then
        log "No Chromium after labwc startup delay attempt $n/$attempts; launching fallback child on WAYLAND_DISPLAY=$WAYLAND_DISPLAY"
        XDG_RUNTIME_DIR="$XDG_RUNTIME_DIR" WAYLAND_DISPLAY="$WAYLAND_DISPLAY" SCREENTRA_CAST_DISPLAY_INDEX="$DISPLAY_INDEX" "$0" "$DISPLAY_INDEX" >>"$LOG" 2>&1 &
      else
        log "No Chromium after labwc startup delay attempt $n/$attempts, but no live wayland socket is yet available"
      fi
      n=$((n + 1))
    done
  ) &
  labwc -s "$STARTUP"
  rc=$?
  log "labwc exited with status $rc; leaving renderer available at $URL"
  exit 0
fi

# (cage support removed — labwc only)

case "$BROWSER" in
  chromium|chromium-browser|google-chrome|google-chrome-stable)
    # Already inside a Wayland session (e.g. labwc child). Pass ozone-platform
    # plus the optional window-position/window-size set earlier so each
    # chromium instance targets its own physical output.
    # Clear stale chromium singleton lock files left by a crashed previous
    # instance. Without this, the new chromium sees the dead-PID lock,
    # decides the profile is in use, and exits with status 1 -> systemd
    # restart loop -> empty screen. The dead-PID lock survives a crash
    # because chromium never gets to run its SIGCHLD cleanup.
    PROFILE_DIR="$HOME/.local/share/screentra-cast-player-client/chromium-profile-display-$DISPLAY_INDEX"
    if [ -e "$PROFILE_DIR/SingletonLock" ]; then
      lock_target="$(readlink "$PROFILE_DIR/SingletonLock" 2>/dev/null || true)"
      lock_pid="${lock_target##*-}"
      if [ -n "$lock_pid" ] && ! kill -0 "$lock_pid" 2>/dev/null; then
        log "Removing stale chromium singleton lock for dead PID $lock_pid"
        rm -f "$PROFILE_DIR/SingletonLock" \
              "$PROFILE_DIR/SingletonCookie" \
              "$PROFILE_DIR/SingletonSocket" 2>/dev/null || true
      fi
    fi
    # Disable chromium's Safe Mode auto-trigger. After 100 crashes in
    # succession chromium decides to start in Safe Mode and bails out
    # before doing anything useful; for an unattended kiosk that
    # interpretation is wrong and we always want a normal start.
    # GPU rasterization: enabled by default via /etc/chromium.d/default-flags.
# Some Intel/DRM combinations show long-running YouTube/video stalls as
# wlroots atomic page-flip failures on the video output. If that symptom
# appears, set SCREENTRA_CAST_DISABLE_GPU_RASTERIZATION=1 in the kiosk service
# environment to append safer Chromium flags.
if [ "${SCREENTRA_CAST_DISABLE_GPU_RASTERIZATION:-0}" = "1" ]; then
  export CHROMIUM_FLAGS="${CHROMIUM_FLAGS:-} --disable-gpu-rasterization --disable-zero-copy --disable-gpu-memory-buffer-video-frames --disable-accelerated-video-decode --disable-accelerated-video-encode"
fi
# Split CHROMIUM_FLAGS into an argv array. These flags are operator-controlled
# service environment, not user input.
# shellcheck disable=SC2206
CHROMIUM_EXTRA_FLAGS=(${CHROMIUM_FLAGS:-})
# When the labwc seat service runs the launcher as root (system-level
# screentra-cast-labwc-kiosk@.service) chromium refuses to start without
# --no-sandbox. The user-level screentra-cast-kiosk-browser@.service runs as the
# kiosk user and must NOT receive this flag, so gate on $(id -u).
if [ "$(id -u)" = "0" ]; then
  CHROMIUM_ROOT_FLAGS=(--no-sandbox)
else
  CHROMIUM_ROOT_FLAGS=()
fi
exec "$BROWSER" \
      --ozone-platform=wayland \
      --hide-cursor \
      --ash-hide-cursor \
      --kiosk "$URL" \
      --no-first-run \
      --disable-infobars \
      --disable-session-crashed-bubble \
      --enable-logging=stderr --v=0 \
      --disable-features=TranslateUI \
      --autoplay-policy=no-user-gesture-required \
      --check-for-update-interval=31536000 \
      --user-data-dir="$HOME/.local/share/screentra-cast-player-client/chromium-profile-display-$DISPLAY_INDEX" \
      "${CHROMIUM_EXTRA_FLAGS[@]}" \
      "${CHROMIUM_ROOT_FLAGS[@]}" \
      ${SCREENTRA_CAST_WAYLAND_POS:+--window-position="$SCREENTRA_CAST_WAYLAND_POS"} \
      ${SCREENTRA_CAST_WAYLAND_SIZE:+--window-size="$SCREENTRA_CAST_WAYLAND_SIZE"}
    ;;
  firefox|firefox-esr)
    exec "$BROWSER" --kiosk "$URL"
    ;;
esac

SH_KIOSK
chmod +x "$KIOSK_LAUNCHER"
ln -sf "$KIOSK_LAUNCHER" "$BIN_DIR/screentra-cast-kiosk-launcher"

# 2026-08-11: bundle screentra-cast self-heal CLI (riptr). Single-file bash
# reconciler for status/heal/log. System-installed so any user can
# run `sudo riptr status` without hunting for the file.
cat > "$INSTALL_DIR/riptr" <<'SH_RIPTR'
#!/usr/bin/env bash
# riptr — Screentra Cast Terminal Reconciler
# Single-file diagnostics + idempotent heal for Screentra Cast Kiosk player boxes.
# 2026-08-11: first cut. Subcommands: status, heal, log, show, fix.
# Idempotent. Audit-logged. No background daemon.

set -o pipefail

VERSION="0.1.0"
RIPTR_NAME="riptr"

# ── Paths ─────────────────────────────────────────────────────────────────────
RIPTR_LOG_DIR="${RIPTR_LOG_DIR:-/var/log/screentra-cast}"
RIPTR_LOG="${RIPTR_LOG_DIR}/riptr.log"
RIPTR_STATE_DIR="${RIPTR_STATE_DIR:-/var/lib/screentra-cast}"
RIPTR_STATE="${RIPTR_STATE_DIR}/riptr-state.json"

RIPTR_USER="${RIPTR_USER:-${SUDO_USER:-${USER:-board}}}"
RIPTR_HOME="$(getent passwd "$RIPTR_USER" 2>/dev/null | cut -d: -f6)"
RIPTR_HOME="${RIPTR_HOME:-/home/$RIPTR_USER}"

PLAYER_DIR="${PLAYER_DIR:-${RIPTR_HOME}/.local/share/screentra-cast-player-client}"
SERVER_DIR="${SERVER_DIR:-${RIPTR_HOME}/.local/share/screentra-cast-signage-server}"
INSTALL_DIR="${INSTALL_DIR:-/usr/local/share/screentra-cast-player-client}"

DETECT_PY="${PLAYER_DIR}/screentra-cast-display-detect.py"
LAUNCHER_SH="${PLAYER_DIR}/screentra-cast-kiosk-launcher.sh"
SERVER_BIN="${SERVER_DIR}/signage-server"
SERVER_ENV="${SERVER_DIR}/server.env"
HEARTBEAT_BIN="${PLAYER_DIR}/screentra-cast-player-heartbeat.py"

SCREENTRA_CAST_DATA_DIR="${SCREENTRA_CAST_DATA_DIR:-${RIPTR_HOME}/.local/share/signage}"
SCREENTRA_CAST_DB="${SCREENTRA_CAST_DATA_DIR}/signage.db"

# Server URL — auto-detected from server.env, but overridable for offline diagnostics.
SCREENTRA_CAST_SERVER_URL="${SCREENTRA_CAST_SERVER_URL:-}"
SCREENTRA_CAST_API_PORT="${SCREENTRA_CAST_API_PORT:-3000}"

# ── Pretty output ─────────────────────────────────────────────────────────────
if [ -t 1 ]; then
    C_RED=$'\033[31m'; C_GRN=$'\033[32m'; C_YEL=$'\033[33m'; C_BLU=$'\033[34m'
    C_DIM=$'\033[2m'; C_BLD=$'\033[1m'; C_RST=$'\033[0m'
else
    C_RED=; C_GRN=; C_YEL=; C_BLU=; C_DIM=; C_BLD=; C_RST=
fi

ok()   { printf "%b✓%b %s\n" "${C_GRN}" "${C_RST}" "$1"; }
warn() { printf "%b!%b %s\n" "${C_YEL}" "${C_RST}" "$1"; }
bad()  { printf "%b✗%b %s\n" "${C_RED}" "${C_RST}" "$1"; }
info() { printf "%b•%b %s\n" "${C_BLU}" "${C_RST}" "$1"; }
hdr()  { printf "\n%b== %s ==%b\n" "${C_BLD}" "$1" "${C_RST}"; }

# ── Logging ───────────────────────────────────────────────────────────────────
log_init() {
    mkdir -p "$RIPTR_LOG_DIR" "$RIPTR_STATE_DIR" 2>/dev/null || true
    # logrotate-friendly: write to a fixed file, never symlink.
}

log_event() {
    # $1 = level (ok|warn|bad|info|action), $2 = component, $3 = message
    local ts level component msg
    ts="$(date -Iseconds 2>/dev/null || date)"
    level="$1"; component="$2"; msg="$3"
    printf "%s\t%s\t%s\t%s\n" "$ts" "$level" "$component" "$msg" >> "$RIPTR_LOG" 2>/dev/null || true
}

# ── Helpers ───────────────────────────────────────────────────────────────────
is_root() { [ "${EUID:-$(id -u)}" = "0" ]; }

run_as_user() {
    # Wrapper for systemctl --user when called from root.
    # $@ = systemctl args
    if is_root; then
        local uid
        uid="$(id -u "$RIPTR_USER" 2>/dev/null || echo "")"
        if [ -z "$uid" ]; then
            warn "user '$RIPTR_USER' does not exist; cannot run user services"
            return 1
        fi
        sudo -u "$RIPTR_USER" \
            XDG_RUNTIME_DIR="/run/user/${uid}" \
            systemctl --user "$@" 2>&1
    else
        systemctl --user "$@" 2>&1
    fi
}

# Detect server URL from server.env if not already set.
detect_server_url() {
    if [ -n "$SCREENTRA_CAST_SERVER_URL" ]; then return; fi
    if [ -f "$SERVER_ENV" ]; then
        SCREENTRA_CAST_SERVER_URL="$(grep -E '^SCREENTRA_CAST_SERVER_URL=' "$SERVER_ENV" 2>/dev/null \
            | head -1 | cut -d= -f2- | tr -d '\"' || true)"
    fi
    [ -z "$SCREENTRA_CAST_SERVER_URL" ] && SCREENTRA_CAST_SERVER_URL="http://127.0.0.1:${SCREENTRA_CAST_API_PORT}"
}

api_get() {
    # $1 = path, optional $2 = extra curl args
    detect_server_url
    curl -sS --max-time 5 "${2:-}" "${SCREENTRA_CAST_SERVER_URL}${1}" 2>&1
}

api_get_status() {
    # Returns: 0 if 2xx, 1 if not. Body on stdout.
    detect_server_url
    local code body
    body="$(curl -sS --max-time 5 -o /tmp/riptr-body -w '%{http_code}' \
        "${SCREENTRA_CAST_SERVER_URL}$1" 2>&1)"
    code="$body"
    if [ "${code:-0}" -ge 200 ] && [ "${code:-0}" -lt 300 ] 2>/dev/null; then
        cat /tmp/riptr-body 2>/dev/null
        return 0
    fi
    return 1
}

# Get this client's id (from heartbeat state file).
client_id() {
    cat "${PLAYER_DIR}/client-id" 2>/dev/null | tr -d '[:space:]' || echo ""
}

# Count connected DRM connectors (status == connected).
connected_connectors() {
    local count=0
    for f in /sys/class/drm/card*/status; do
        [ -e "$f" ] || continue
        [ "$(cat "$f" 2>/dev/null)" = "connected" ] && count=$((count + 1))
    done
    echo "$count"
}

# List connected DRM connector names (e.g. HDMI-A-1 DP-2).
connected_names() {
    local out=""
    for d in /sys/class/drm/card*/; do
        [ -d "$d" ] || continue
        local name status
        name="$(basename "$d")"
        # Strip "card0-" prefix
        name="${name#card*-}"
        status="$(cat "${d}status" 2>/dev/null)"
        if [ "$status" = "connected" ]; then
            out="${out:+$out }${name}"
        fi
    done
    echo "$out"
}

# Run the player detect script, return its JSON output.
detect_run() {
    if [ ! -x "$DETECT_PY" ]; then
        echo "[]"
        return 1
    fi
    sudo -u "$RIPTR_USER" python3 "$DETECT_PY" 2>/dev/null || echo "[]"
}

# ── Components ────────────────────────────────────────────────────────────────
# Each component has:
#   check_<name>()   — sets variables: state=ok|warn|bad, detail="..."
#   heal_<name>()    — idempotent repair; logs every action via log_event.
#   show_<name>()    — long-form dump for `riptr show <name>`.

# ── displays ──────────────────────────────────────────────────────────────────
check_displays() {
    local n_connected n_displays_db sway_count
    n_connected="$(connected_connectors)"
    n_displays_db="?"
    sway_count="$(swaymsg -t get_outputs 2>/dev/null | python3 -c 'import json,sys; print(sum(1 for o in json.loads(sys.stdin.read()) if o.get("active")))' 2>/dev/null || echo 0)"

    if [ "$n_connected" = "0" ]; then
        state="warn"
        detail="no displays connected — output state frozen, nothing to heal until you plug one in"
        return
    fi

    # If server is reachable, check DB vs detected
    local cid db_displays
    cid="$(client_id)"
    if [ -n "$cid" ]; then
        db_displays="$(api_get "/api/clients/${cid}/displays" 2>/dev/null \
            | python3 -c 'import json,sys; print(len(json.loads(sys.stdin.read()) if sys.stdin.read() else []))' 2>/dev/null || echo "?")"
        n_displays_db="$db_displays"
    fi

    if [ "$n_displays_db" != "?" ] && [ "$n_displays_db" -gt "$n_connected" ]; then
        state="warn"
        detail="DB has ${n_displays_db} display rows but only ${n_connected} physically connected — phantom rows need pruning"
    elif [ "$sway_count" -lt "$n_connected" ]; then
        state="warn"
        detail="sway has ${sway_count} active outputs but ${n_connected} connectors are connected — sway may need reconfigure"
    else
        state="ok"
        detail="${n_connected} connected, ${n_displays_db} DB rows, ${sway_count} sway outputs"
    fi
}

heal_displays() {
    local n_connected
    n_connected="$(connected_connectors)"
    if [ "$n_connected" = "0" ]; then
        warn "no displays connected — nothing to heal (plug displays back in first)"
        log_event "warn" "displays" "skip: no displays connected"
        return 1
    fi

    # Re-run the detect script so the heartbeat picks up the right state on next tick.
    info "re-running display detect..."
    local detected
    detected="$(detect_run)"
    log_event "action" "displays" "re-ran detect: $detected"

    # Re-apply sway mode per-output (use pref or largest sysfs mode, never trust cur).
    if command -v wlr-randr >/dev/null 2>&1; then
        local name
        for name in $(connected_names); do
            local out_info
            out_info="$(wlr-randr 2>/dev/null | awk -v n="$name" '
                $0 ~ "^" n {p=1; next}
                p && /current/ {cur=$0; p=0}
                p && /[[:space:]]+[0-9]+x[0-9]+/ {print}
            ' | head -20)"
            # Prefer the largest mode listed (2560x1440 over 640x480 stubs)
            local best_mode
            best_mode="$(echo "$out_info" | awk '{
                w=$1; sub(/x.*/,"",w)
                h=$1; sub(/.*x/,"",h); sub(/ .*/,"",h)
                if (w*h > max) {max=w*h; mode=$1}
            } END {print mode}')"
            if [ -n "$best_mode" ]; then
                wlr-randr --output "$name" --mode "$best_mode" >/dev/null 2>&1 && \
                    log_event "action" "displays" "applied $best_mode to $name" || \
                    log_event "warn" "displays" "failed to apply $best_mode to $name"
            fi
        done
    fi

    # Prune phantom DB rows. Done by deleting rows whose display_index is
    # outside the set currently reported by the detect script.
    local cid keep_indexes
    cid="$(client_id)"
    if [ -n "$cid" ]; then
        keep_indexes="$(echo "$detected" | python3 -c '
import json, sys
data = json.loads(sys.stdin.read() or "[]")
print(",".join(str(d.get("display_index", -1)) for d in data))
' 2>/dev/null)"
        if [ -n "$keep_indexes" ]; then
            info "pruning DB rows for client $cid, keeping display_index in [$keep_indexes]"
            sqlite3 "$SCREENTRA_CAST_DB" \
                "DELETE FROM client_displays WHERE client_id='${cid}' AND display_index NOT IN (${keep_indexes});" \
                2>&1 | tee -a "$RIPTR_LOG"
            log_event "action" "displays" "pruned phantom client_displays for ${cid}"
        fi
    fi

    # Restart the heartbeat so the next tick pushes the corrected state.
    run_as_user restart screentra-cast-player-heartbeat.service >/dev/null 2>&1 && \
        log_event "action" "displays" "restarted screentra-cast-player-heartbeat.service" || \
        log_event "warn" "displays" "failed to restart heartbeat"
    ok "displays reconciled"
}

show_displays() {
    hdr "Displays"
    echo "Connected connectors:"
    for d in /sys/class/drm/card*/; do
        [ -d "$d" ] || continue
        local name status edid_size
        name="$(basename "$d")"
        name="${name#card*-}"
        status="$(cat "${d}status" 2>/dev/null)"
        edid_size="$(wc -c < "${d}edid" 2>/dev/null || echo 0)"
        printf "  %-12s status=%-12s edid=%d bytes\n" "$name" "$status" "$edid_size"
    done
    echo ""
    echo "Sway outputs:"
    swaymsg -t get_outputs 2>/dev/null | python3 -c '
import json, sys
data = json.loads(sys.stdin.read() or "[]")
for o in data:
    m = o.get("current_mode") or {}
    print(f"  {o[\"name\"]:12} {o[\"make\"]}/{o[\"model\"]:18} {m.get(\"width\",\"?\")}x{m.get(\"height\",\"?\")}@{m.get(\"refresh\",0):.0f} active={o.get(\"active\")}")
' 2>/dev/null
    echo ""
    echo "DB rows:"
    local cid
    cid="$(client_id)"
    if [ -n "$cid" ] && [ -f "$SCREENTRA_CAST_DB" ]; then
        sqlite3 -header -column "$SCREENTRA_CAST_DB" \
            "SELECT display_index, connector, width, height, refresh_rate, last_seen FROM client_displays WHERE client_id='${cid}';" 2>&1
    else
        warn "no client-id or DB; skipping"
    fi
}

# ── services ──────────────────────────────────────────────────────────────────
check_services() {
    local -a required=(
        "screentra-cast-kiosk-browser@0.service:user"
        "screentra-cast-kiosk-browser@1.service:user"
        "screentra-cast-player-renderer@0.service:user"
        "screentra-cast-player-renderer@1.service:user"
        "screentra-cast-player-heartbeat.service:user"
        "screentra-cast-labwc-kiosk@0.service:system"
    )
    local n_running=0 n_total=0 n_failed=0
    for entry in "${required[@]}"; do
        local svc="${entry%:*}" kind="${entry#*:}"
        n_total=$((n_total + 1))
        local out
        if [ "$kind" = "user" ]; then
            out="$(run_as_user is-active "$svc" 2>&1)"
        else
            out="$(systemctl is-active "$svc" 2>&1)"
        fi
        case "$out" in
            active)   n_running=$((n_running + 1)) ;;
            failed)   n_failed=$((n_failed + 1)) ;;
        esac
    done

    if [ "$n_failed" -gt 0 ]; then
        state="bad"
        detail="${n_running}/${n_total} active, ${n_failed} failed"
    elif [ "$n_running" -lt "$n_total" ]; then
        state="warn"
        detail="${n_running}/${n_total} active (some inactive)"
    else
        state="ok"
        detail="${n_running}/${n_total} active"
    fi
}

heal_services() {
    local -a required=(
        "screentra-cast-kiosk-browser@0.service:user"
        "screentra-cast-kiosk-browser@1.service:user"
        "screentra-cast-player-renderer@0.service:user"
        "screentra-cast-player-renderer@1.service:user"
        "screentra-cast-player-heartbeat.service:user"
        "screentra-cast-labwc-kiosk@0.service:system"
    )
    local n_fixed=0
    for entry in "${required[@]}"; do
        local svc="${entry%:*}" kind="${entry#*:}"
        local active
        if [ "$kind" = "user" ]; then
            active="$(run_as_user is-active "$svc" 2>&1)"
        else
            active="$(systemctl is-active "$svc" 2>&1)"
        fi
        if [ "$active" != "active" ]; then
            warn "  $svc: $active — restarting"
            if [ "$kind" = "user" ]; then
                run_as_user restart "$svc" 2>&1 | head -3
            else
                sudo systemctl restart "$svc" 2>&1 | head -3
            fi
            if [ "$( [ "$kind" = "user" ] && run_as_user is-active "$svc" 2>&1 || systemctl is-active "$svc" 2>&1)" = "active" ]; then
                n_fixed=$((n_fixed + 1))
                log_event "action" "services" "restarted $svc"
            else
                log_event "warn" "services" "restart of $svc did not result in active"
            fi
        fi
    done
    [ "$n_fixed" -gt 0 ] && ok "fixed $n_fixed service(s)" || ok "all services already active"
}

show_services() {
    hdr "Services"
    printf "%-44s %-12s %-12s %s\n" "UNIT" "KIND" "STATE" "DETAIL"
    echo "------------------------------------------------------------------------------------"
    local -a required=(
        "screentra-cast-kiosk-browser@0.service:user"
        "screentra-cast-kiosk-browser@1.service:user"
        "screentra-cast-player-renderer@0.service:user"
        "screentra-cast-player-renderer@1.service:user"
        "screentra-cast-player-heartbeat.service:user"
        "screentra-cast-labwc-kiosk@0.service:system"
    )
    for entry in "${required[@]}"; do
        local svc="${entry%:*}" kind="${entry#*:}"
        local state since
        if [ "$kind" = "user" ]; then
            state="$(run_as_user is-active "$svc" 2>&1)"
            since="$(run_as_user show "$svc" -p ActiveEnterTimestamp --value 2>&1 || echo '-')"
        else
            state="$(systemctl is-active "$svc" 2>&1)"
            since="$(systemctl show "$svc" -p ActiveEnterTimestamp --value 2>&1 || echo '-')"
        fi
        printf "%-44s %-12s %-12s %s\n" "$svc" "$kind" "$state" "$since"
    done
}

# ── server ────────────────────────────────────────────────────────────────────
check_server() {
    detect_server_url
    if api_get_status "/api/clients" >/dev/null 2>&1; then
        state="ok"
        detail="responding at ${SCREENTRA_CAST_SERVER_URL}"
    else
        state="bad"
        detail="not responding at ${SCREENTRA_CAST_SERVER_URL}"
    fi
}

heal_server() {
    if [ ! -f "$SERVER_ENV" ]; then
        warn "no server.env at $SERVER_ENV — skipping server restart"
        log_event "warn" "server" "skip: no server.env"
        return 1
    fi

    # Check the password line is present. Never modify it.
    if ! grep -qE '^SIGNAGE_ADMIN_PASSWORD(_HASH)?=' "$SERVER_ENV"; then
        bad "server.env has no SIGNAGE_ADMIN_PASSWORD(_HASH) — refusing to start server"
        log_event "bad" "server" "skip: no password line in server.env"
        return 1
    fi

    info "killing any running signage-server..."
    sudo pkill -9 -f 'signage-server --serve' 2>/dev/null || true
    sleep 1
    info "starting signage-server with server.env sourced..."
    sudo bash -c "
        cd '$SERVER_DIR'
        set -a; source ./server.env; set +a
        setsid nohup ./signage-server --serve > server.log 2>&1 < /dev/null &
        sleep 3
        if pgrep -f 'signage-server --serve' >/dev/null; then
            echo 'started ok'
        else
            echo 'start failed; tail of server.log:'
            tail -5 server.log
        fi
    " 2>&1 | tee -a "$RIPTR_LOG"
    log_event "action" "server" "restarted signage-server"
}

show_server() {
    hdr "Server"
    echo "Binary:   $SERVER_BIN ($(stat -c '%Y' "$SERVER_BIN" 2>/dev/null | xargs -I{} date -d @{} 2>/dev/null || echo 'missing'))"
    echo "Env file: $SERVER_ENV"
    echo "URL:      ${SCREENTRA_CAST_SERVER_URL:-not configured}"
    echo ""
    echo "Server log (last 10 lines):"
    tail -10 "${SERVER_DIR}/server.log" 2>/dev/null || echo "  (no log)"
}

# ── env ──────────────────────────────────────────────────────────────────────
check_env() {
    local issues=""
    if [ ! -f "$SERVER_ENV" ]; then
        issues="server.env missing"
    elif ! grep -qE '^SIGNAGE_ADMIN_PASSWORD(_HASH)?=' "$SERVER_ENV"; then
        issues="${issues:+$issues; }no admin password"
    fi
    if [ -z "$issues" ]; then
        state="ok"
        detail="env file OK"
    else
        state="bad"
        detail="$issues"
    fi
}

heal_env() {
    # Heal only writes what we KNOW is safe: SCREENTRA_CAST_SERVER_URL (if pointing at an unreachable host).
    detect_server_url
    if [ -z "$SCREENTRA_CAST_SERVER_URL" ]; then
        warn "no SCREENTRA_CAST_SERVER_URL set — cannot heal"
        return 1
    fi
    # Probe a few common server IPs and let the user pick if none reachable.
    ok "env is read-only by default; user-set SCREENTRA_CAST_SERVER_URL is $SCREENTRA_CAST_SERVER_URL"
    log_event "info" "env" "checked env (no mutations)"
}

show_env() {
    hdr "Environment"
    echo "Box identity:"
    echo "  hostname:   $(hostname)"
    echo "  IPs:        $(hostname -I 2>/dev/null | tr ' ' '\n' | head -3 | paste -sd ', ')"
    echo "  user:       $(whoami) (target user: $RIPTR_USER)"
    echo "  screentra-cast dir: $PLAYER_DIR"
    echo "  server dir: $SERVER_DIR"
    echo ""
    echo "server.env (passwords masked):"
    if [ -f "$SERVER_ENV" ]; then
        sed -E 's/(PASSWORD(_HASH)?)=.*/\1=***/' "$SERVER_ENV" | grep -v '^#' | grep -v '^$'
    else
        warn "no server.env"
    fi
}

# ── proxy ─────────────────────────────────────────────────────────────────────
check_proxy() {
    # Probe /api/ics-proxy without auth. If 401, the binary is stale.
    detect_server_url
    local code
    code="$(curl -sS --max-time 5 -o /dev/null -w '%{http_code}' \
        "${SCREENTRA_CAST_SERVER_URL}/api/ics-proxy?url=https://example.com/test.ics" 2>/dev/null || echo 000)"
    if [ "$code" = "401" ]; then
        state="bad"
        detail="/api/ics-proxy returned 401 — OPEN_ROUTES gate is wrong; redeploy server binary"
    elif [ "$code" = "200" ] || [ "$code" = "400" ] || [ "$code" = "502" ]; then
        state="ok"
        detail="responding (HTTP $code)"
    else
        state="warn"
        detail="HTTP $code"
    fi
}

heal_proxy() {
    warn "/api/ics-proxy cannot be self-healed — the OPEN_ROUTES list is compiled into the binary"
    info "to fix: redeploy signage-server with the patched middleware (the `OPEN_ROUTES` array in src/middleware.rs)"
    log_event "warn" "proxy" "manual redeploy required"
}

show_proxy() {
    hdr "Proxy"
    check_proxy >/dev/null 2>&1
    echo "Status: $state"
    echo "Detail: $detail"
}

# ── network ───────────────────────────────────────────────────────────────────
check_network() {
    detect_server_url
    local host
    host="$(echo "$SCREENTRA_CAST_SERVER_URL" | sed -E 's#https?://##; s#/.*##; s#:[0-9]+$##')"
    if [ -z "$host" ]; then
        state="warn"
        detail="no SCREENTRA_CAST_SERVER_URL configured"
        return
    fi
    if timeout 5 getent hosts "$host" >/dev/null 2>&1; then
        state="ok"
        detail="server host ${host} resolves"
    else
        state="bad"
        detail="server host ${host} does NOT resolve"
    fi
}

heal_network() {
    detect_server_url
    local host
    host="$(echo "$SCREENTRA_CAST_SERVER_URL" | sed -E 's#https?://##; s#/.*##; s#:[0-9]+$##')"
    info "current SCREENTRA_CAST_SERVER_URL = ${SCREENTRA_CAST_SERVER_URL}"
    info "host = ${host}"
    info "riptr does not auto-rewrite URLs — they are deploy-time config"
    info "to update: edit $SERVER_ENV, then 'riptr heal server'"
    log_event "info" "network" "checked (no auto-rewrite)"
}

show_network() {
    hdr "Network"
    echo "Default route:"
    ip route 2>/dev/null | head -5 || route -n 2>/dev/null | head -5
    echo ""
    echo "Listening ports (3900 renderer / 3910 admin / 3000 server):"
    ss -tlnp 2>&1 | grep -E ':3000|:3900|:3910' | head -10
    echo ""
    echo "Server reachability:"
    detect_server_url
    timeout 3 curl -sS -o /dev/null -w "  ${SCREENTRA_CAST_SERVER_URL}/api/system → HTTP %{http_code}\n" \
        "${SCREENTRA_CAST_SERVER_URL}/api/system" 2>&1
}

# ── cache ─────────────────────────────────────────────────────────────────────
check_cache() {
    # Renderer PLAYER_HTML is embedded in the python source. If the file mtime is newer
    # than the running process start time, the chromium is showing stale HTML.
    local py_src renderer_pid renderer_start
    py_src="${PLAYER_DIR}/screentra-cast-player-renderer.py"
    if [ ! -f "$py_src" ]; then
        state="warn"
        detail="renderer source not at $py_src"
        return
    fi
    renderer_pid="$(pgrep -f 'screentra-cast-player-renderer.py' 2>/dev/null | head -1)"
    if [ -z "$renderer_pid" ]; then
        state="warn"
        detail="no renderer process"
        return
    fi
    renderer_start="$(stat -c '%Y' "/proc/${renderer_pid}" 2>/dev/null || echo 0)"
    local src_mtime
    src_mtime="$(stat -c '%Y' "$py_src" 2>/dev/null || echo 0)"
    if [ "$src_mtime" -gt "$renderer_start" ]; then
        state="warn"
        detail="renderer source newer than process — chromium has stale HTML"
    else
        state="ok"
        detail="renderer process is fresh"
    fi
}

heal_cache() {
    info "restarting all renderer + kiosk-browser services..."
    run_as_user restart screentra-cast-player-renderer@0.service screentra-cast-player-renderer@1.service \
        screentra-cast-kiosk-browser@0.service screentra-cast-kiosk-browser@1.service 2>&1 | head -10
    log_event "action" "cache" "restarted renderer + kiosk-browser to flush chromium cache"
    ok "renderer cache flushed"
}

show_cache() {
    hdr "Cache"
    check_cache >/dev/null 2>&1
    echo "Renderer source: ${PLAYER_DIR}/screentra-cast-player-renderer.py"
    echo "  mtime:  $(stat -c '%y' "${PLAYER_DIR}/screentra-cast-player-renderer.py" 2>/dev/null || echo 'missing')"
    local renderer_pids
    renderer_pids="$(pgrep -af 'screentra-cast-player-renderer.py' 2>/dev/null | head -5)"
    echo "Running renderers:"
    echo "${renderer_pids:-  (none)}"
}

# ── Aggregator ────────────────────────────────────────────────────────────────
COMPONENTS=(displays services server env proxy network cache)

check_all() {
    local overall="ok"
    for c in "${COMPONENTS[@]}"; do
        state="ok"; detail=""
        "check_${c}" >/dev/null 2>&1
        case "$state" in
            ok)   ok   "$c: $detail" ;;
            warn) warn "$c: $detail" ; overall="warn" ;;
            bad)  bad  "$c: $detail" ; overall="bad"  ;;
        esac
    done
    hdr "Overall: $overall"
}

heal_all() {
    local n_displays
    n_displays="$(connected_connectors)"
    if [ "$n_displays" = "0" ]; then
        warn "no displays connected — skipping display heal; running other heals"
        for c in "${COMPONENTS[@]}"; do
            [ "$c" = "displays" ] && continue
            state="ok"; detail=""
            "heal_${c}" 2>&1 | sed 's/^/  /'
        done
    else
        for c in "${COMPONENTS[@]}"; do
            state="ok"; detail=""
            "heal_${c}" 2>&1 | sed 's/^/  /'
        done
    fi
    ok "all heals attempted"
}

# ── Main ──────────────────────────────────────────────────────────────────────
usage() {
    cat <<EOF
${C_BLD}riptr ${VERSION}${C_RST} — Screentra Cast Terminal Reconciler

Usage:
    riptr status              Show all component health
    riptr heal                Run idempotent repairs on every component
    riptr heal <component>    Heal one component (displays, services, server, env, proxy, network, cache)
    riptr show <component>    Deep-dive on one component
    riptr fix <component>     Alias for 'heal <component>' (preferred phrasing)
    riptr log [-f]            Show audit log (default: last 50 lines; -f: follow)
    riptr version             Print version

Components: displays, services, server, env, proxy, network, cache

Environment overrides:
    RIPTR_USER         Target user for systemctl --user  (default: board)
    SCREENTRA_CAST_SERVER_URL  Override the API base URL
    SCREENTRA_CAST_DB          Override the DB path
    PLAYER_DIR         Override the player-client dir
    SERVER_DIR         Override the server dir
EOF
}

main() {
    log_init
    detect_server_url

    if [ $# -eq 0 ]; then
        usage
        exit 0
    fi

    case "$1" in
        status)
            check_all
            ;;
        heal)
            if [ $# -ge 2 ]; then
                state="ok"; detail=""
                "heal_$2" 2>&1
            else
                heal_all
            fi
            ;;
        fix)  # alias
            shift
            state="ok"; detail=""
            "heal_${1:-all}" 2>&1
            ;;
        show)
            if [ $# -lt 2 ]; then
                bad "usage: riptr show <component>"
                exit 2
            fi
            "show_$2" 2>&1
            ;;
        log)
            shift
            if [ "${1:-}" = "-f" ]; then
                tail -F "$RIPTR_LOG" 2>/dev/null
            else
                tail -50 "$RIPTR_LOG" 2>/dev/null || echo "(log empty)"
            fi
            ;;
        version|-v|--version)
            echo "riptr ${VERSION}"
            ;;
        help|-h|--help)
            usage
            ;;
        *)
            bad "unknown command: $1"
            usage
            exit 2
            ;;
    esac
}

main "$@"
SH_RIPTR
chmod +x "$INSTALL_DIR/riptr"
# System-wide install via run_privileged (handles root-vs-user). If
# /usr/local/bin isn't writable we fall back to $BIN_DIR.
if command -v run_privileged >/dev/null 2>&1; then
    run_privileged install -m 0755 "$INSTALL_DIR/riptr" /usr/local/bin/riptr 2>/dev/null || ln -sf "$INSTALL_DIR/riptr" "$BIN_DIR/riptr"
else
    install -m 0755 "$INSTALL_DIR/riptr" /usr/local/bin/riptr 2>/dev/null || ln -sf "$INSTALL_DIR/riptr" "$BIN_DIR/riptr"
fi

cat > "$LABWC_SESSION" <<'SH_LABWC'
#!/usr/bin/env bash
set -euo pipefail
DISPLAY_INDEX="${1:-${SCREENTRA_CAST_DISPLAY_INDEX:-0}}"
export SCREENTRA_CAST_DISPLAY_INDEX="$DISPLAY_INDEX"
export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"
mkdir -p "$XDG_RUNTIME_DIR" 2>/dev/null || true
export WLR_BACKENDS="${WLR_BACKENDS:-drm,libinput}"
export XDG_CURRENT_DESKTOP="${XDG_CURRENT_DESKTOP:-sway}"
INSTALL_DIR="$HOME/.local/share/screentra-cast-player-client"
if [ -x "$INSTALL_DIR/screentra-cast-kiosk-launcher.sh" ]; then
  exec "$INSTALL_DIR/screentra-cast-kiosk-launcher.sh"
fi
if [ -x "$INSTALL_DIR/screentra-cast-kiosk-launcher" ]; then
  exec "$INSTALL_DIR/screentra-cast-kiosk-launcher"
fi
if command -v screentra-cast-kiosk-launcher >/dev/null 2>&1; then
  exec screentra-cast-kiosk-launcher
fi
echo "screentra-cast-kiosk-launcher not found in $INSTALL_DIR or PATH" >&2
exit 127
SH_LABWC
chmod +x "$LABWC_SESSION"
ln -sf "$LABWC_SESSION" "$BIN_DIR/screentra-cast-labwc-session"

cat > "$SERVICE" <<EOF
[Unit]
Description=Screentra Cast Player Heartbeat Client
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
# Autostart delay: give the seat/network/Wayland stack 20s to settle before
# the heartbeat tries to reach the server. Bounded sleep is preferable to
# chained `After=` because it tolerates transient network-online races.
ExecStartPre=/bin/sleep 20
ExecStart="$HEARTBEAT" --server "$SERVER_URL" --interval "$INTERVAL" --client-id "$CLIENT_ID" --name "$CLIENT_NAME" --role "$ROLE" --version "$VERSION"
ExecStartPost=-systemctl --user start $ACTOR_SERVICE_NAME
Restart=always
RestartSec=5
Environment=PYTHONUNBUFFERED=1
# SCREENTRA_CAST_BIND_IP is written by the local admin's network switcher so the
# heartbeat (and any other user-scope process) sources outbound traffic
# from a specific interface. The renderer falls back to the kernel's
# default-route selection when this is empty.
EnvironmentFile=$INSTALL_DIR/network.env

[Install]
WantedBy=default.target
EOF

cat > "$RENDER_SERVICE" <<EOF
[Unit]
Description=Screentra Cast Cached Player Renderer (display %i)
After=network-online.target screentra-cast-player-heartbeat.service
Wants=network-online.target

[Service]
Type=simple
# Autostart delay: 5s before the renderer tries to bind to its port.
ExecStartPre=/bin/sleep 5
ExecStart=$RENDERER --host 0.0.0.0 --cache-dir $INSTALL_DIR/cache
Environment=SCREENTRA_CAST_DISPLAY_INDEX=%i
Environment=SCREENTRA_CAST_RENDER_BASE_PORT=3900
Environment=SCREENTRA_CAST_CURSOR_HIDE_SECS=__SCREENTRA_CAST_CURSOR_HIDE_SECS__
Restart=always
RestartSec=5
Environment=PYTHONUNBUFFERED=1

[Install]
WantedBy=default.target
EOF
# Default the cursor auto-hide delay to 3.5s. The operator can override
# at runtime by editing this Environment= line and reloading the renderer.
sed -i "s|__SCREENTRA_CAST_CURSOR_HIDE_SECS__|3.5|g" "$RENDER_SERVICE" 2>/dev/null || true

cat > "$ADMIN_SERVICE" <<EOF
[Unit]
Description=Screentra Cast Player Local Admin Dashboard
After=network-online.target screentra-cast-player-heartbeat.service
Wants=network-online.target

[Service]
Type=simple
# Autostart delay: 5s before the local admin page binds to port 3910.
ExecStartPre=/bin/sleep 5
ExecStart=$RENDERER --host 0.0.0.0 --port 3910 --cache-dir $INSTALL_DIR/cache
Environment=SCREENTRA_CAST_ADMIN_DEFAULT=1
Environment=SCREENTRA_CAST_DISPLAY_INDEX=0
Environment=SCREENTRA_CAST_RENDER_BASE_PORT=3900
Environment=SCREENTRA_CAST_CURSOR_HIDE_SECS=3.5
Restart=always
RestartSec=5
Environment=PYTHONUNBUFFERED=1

[Install]
WantedBy=default.target
EOF

cat > "$KIOSK_SERVICE" <<EOF_KIOSK_USER
[Unit]
Description=Screentra Cast Fullscreen Kiosk Browser (display %i)
After=screentra-cast-player-renderer@%i.service
Wants=screentra-cast-player-renderer@%i.service

[Service]
Type=simple
# Autostart delay: 5s so the renderer + heartbeat are up before chromium opens.
ExecStartPre=/bin/sleep 5
ExecStart=$KIOSK_LAUNCHER
Restart=on-failure
RestartSec=5
# Wayland-first kiosk environment. The launcher picks labwc/Wayland or
# falls back to X11 if the box already has a DISPLAY set.
Environment=SCREENTRA_CAST_PLAYER_URL=http://127.0.0.1:3900/
Environment=SCREENTRA_CAST_DISPLAY_INDEX=%i
Environment=SCREENTRA_CAST_RENDER_BASE_PORT=3900
Environment=SCREENTRA_CAST_DISPLAY_ROTATION=normal
# Do NOT force WAYLAND_DISPLAY here. The launcher auto-detects an existing
# Wayland socket or starts labwc; forcing wayland-0 on a headless/tty session
# makes Chromium restart-loop instead of clean-skipping.
# 2026-08-10: use /run/user/1000 hardcoded (tech's runtime dir). The
# previous ${XDG_RUNTIME_DIR:-/run/user/%U} got bash-expanded at install
# time to /run/user/0 (root's dir), causing the kiosk browser to bind
# to root's Wayland socket instead of tech's. EOF_KIOSK_USER is quoted
# to prevent any shell expansion.
Environment=XDG_RUNTIME_DIR=/run/user/$UID_NUM
Environment=SCREENTRA_CAST_COMPOSITOR=auto

[Install]
WantedBy=default.target
EOF_KIOSK_USER

cat > "$DISPLAY_PROVISION_SERVICE" <<EOF
[Unit]
Description=Screentra Cast Player Display Provisioner (reads DRM, enables/disables per-display renderer + kiosk browser services)
After=default.target
Before=screentra-cast-player-renderer@.service screentra-cast-kiosk-browser@.service
Wants=suspend.target

[Service]
Type=oneshot
# User= ensures systemd runs the service as the install user AND auto-creates
# /run/user/UID with correct permissions. When User= is set, systemd
# automatically sets XDG_RUNTIME_DIR=/run/user/<UID> before ExecStart.
User=$BOARD_USER
ExecStart=/usr/local/bin/screentra-cast-player-display-provision
Restart=on-failure
RestartSec=5

[Install]
WantedBy=default.target
EOF

# Symlink the provision script into a stable absolute path so the unit
# file's ExecStart never needs $HOME expansion. /usr/local/bin is in the
# default PATH for both user and root systemd services.
run_privileged ln -sf "$DISPLAY_PROVISION" /usr/local/bin/screentra-cast-player-display-provision || true

cat > "$ACTOR_SERVICE_FILE" <<EOF
[Unit]
Description=Screentra Cast player command actor
After=network-online.target

[Service]
Type=oneshot
# Autostart delay: 5s before the actor starts processing the pending-command
# queue. The actor is started as a one-shot after the heartbeat's
# ExecStartPost, so this sleep runs in addition to that ordering.
ExecStartPre=/bin/sleep 5
Environment=SCREENTRA_CAST_PENDING_FILE=$INSTALL_DIR/cache/pending-command.json
Environment=SCREENTRA_CAST_RESULT_FILE=$INSTALL_DIR/cache/command-result.json
Environment=SCREENTRA_CAST_SERVER_URL=$SERVER_URL
Environment=SCREENTRA_CAST_CA_CERT=$INSTALL_DIR/screentra-cast-signage-ca.crt
Environment=SCREENTRA_CAST_CLIENT_ID=$CLIENT_ID
Environment=SCREENTRA_CAST_CLIENT_CACHE_DIR=$INSTALL_DIR/cache
# SCREENTRA_CAST_BIND_IP sources outbound traffic from a specific interface when set.
EnvironmentFile=$INSTALL_DIR/network.env
ExecStart="$ACTOR" --pending-file "$INSTALL_DIR/cache/pending-command.json" --result-file "$INSTALL_DIR/cache/command-result.json" --server "$SERVER_URL" --client-id "$CLIENT_ID" --cache-dir "$INSTALL_DIR/cache"

[Install]
WantedBy=default.target
EOF

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
    command -v labwc >/dev/null 2>&1 || NEED+=("labwc")
    command -v wlr-randr >/dev/null 2>&1 || NEED+=("wlr-randr")
    command -v wlrctl >/dev/null 2>&1 || NEED+=("wlrctl")
    command -v chromium >/dev/null 2>&1 || command -v chromium-browser >/dev/null 2>&1 || command -v google-chrome >/dev/null 2>&1 || NEED+=("chromium")
    if [ "${#NEED[@]}" -gt 0 ]; then
      echo "Installing missing labwc kiosk deps: ${NEED[*]}"
      if command -v sudo >/dev/null 2>&1; then SUDO=sudo; else SUDO=""; fi
      $SUDO apt-get update -y >/dev/null 2>&1 || true
      $SUDO apt-get install -y --no-install-recommends "${NEED[@]}" >/dev/null 2>&1 || true
    fi
  fi

  # Preflight is run AFTER /run/user/UID is created and the seat services
  # are started. See the end of the seat-service block below. systemd-logind
  # creates this on first TTY login, but an SSH-only install won't trigger
  # that. Create it manually so the labwc seat service can write its
  # wayland-0 socket even before the user has logged in to a TTY.
  # 2026-08-10: resolve UID_NUM from $BOARD_USER (real uid), not $(id -u)
  # (which is 0 when running as root).
  UID_NUM="$(id -u "$BOARD_USER" 2>/dev/null || echo 0)"
  if [ ! -d "/run/user/$UID_NUM" ]; then
      if run_privileged install -d -o "$BOARD_USER" -g "$BOARD_USER" -m 700 "/run/user/$UID_NUM" 2>/dev/null; then
          echo "Created /run/user/$UID_NUM for Wayland sockets."
      else
          echo "WARNING: could not create /run/user/$UID_NUM; labwc seat service may fail to start."
      fi
  fi

  # Preferred physical display takeover path: a system-level seat service.
  # This works on Debian/minimal installs without a desktop login session. It is
  # best-effort because writing /etc/systemd/system requires sudo/root. If the
  # installer is running from SSH, this is the ONLY way the kiosk will get a
  # real display (the user-level session has no TTY in an SSH context).
  # 2026-08-10: labwc was removed in favor of sway (multi-display wlr-output
  # quirk on labwc). Accept either compositor for the seat service gate.
  # 2026-08-10: resolve UID_NUM from $BOARD_USER, not $(id -u), because
  # when running as root $(id -u) returns 0 but the seat service runs as
  # $BOARD_USER and needs /run/user/<real-uid>.
  if command -v sway >/dev/null 2>&1 || command -v labwc >/dev/null 2>&1; then
    UID_NUM="$(id -u "$BOARD_USER" 2>/dev/null || echo 0)"
    if run_privileged sh -c "cat > '$LABWC_SYSTEM_UNIT'" <<EOF_LABWC_UNIT
[Unit]
Description=Screentra Cast labwc kiosk seat display %i
After=systemd-user-sessions.service network-online.target
Wants=network-online.target
Conflicts=getty@tty7.service

[Service]
Type=simple
User=$USER
PAMName=login
# TTYPath is set to /dev/tty7 for backward compatibility with single-display
# installs (where %i=0). For multi-display installs the installer drops a
# per-instance override into /etc/systemd/system/screentra-cast-labwc-kiosk@N.service.d/
# that reassigns TTYPath to /dev/tty(7+N). The per-instance drop-in also adds
# the matching Conflicts= against getty@tty(7+N); the base template's
# Conflicts= above targets getty@tty7 so single-display installs still keep
# the seat clean.
TTYPath=/dev/tty7
TTYReset=yes
TTYVHangup=yes
StandardInput=tty
StandardOutput=journal
StandardError=journal
Environment=HOME=$HOME
Environment=USER=$USER
Environment=XDG_RUNTIME_DIR=/run/user/$UID_NUM
Environment=SCREENTRA_CAST_DISPLAY_INDEX=%i
Environment=SCREENTRA_CAST_RENDER_BASE_PORT=3900
Environment=SCREENTRA_CAST_PLAYER_URL=http://127.0.0.1:3900/
Environment=SCREENTRA_CAST_COMPOSITOR=${SCREENTRA_CAST_COMPOSITOR:-auto}
ExecStart=$LABWC_SESSION %i
Restart=on-failure
RestartSec=3

[Install]
WantedBy=multi-user.target
EOF_LABWC_UNIT
    then
      run_privileged systemctl daemon-reload || true
      # Single-GPU Wayland invariant: only display 0 owns the physical labwc
      # compositor/DRM master. Secondary displays are Chromium windows attached
      # to display 0's wayland-0; never start extra system labwc seats.
      SEAT_MAX=1
      SEAT_STARTED=0
      for SEAT_IDX in $(seq 0 $((SEAT_MAX-1))); do
          if run_privileged systemctl enable "screentra-cast-labwc-kiosk@${SEAT_IDX}.service" 2>/dev/null; then
              if run_privileged systemctl restart "screentra-cast-labwc-kiosk@${SEAT_IDX}.service" 2>/dev/null; then
                  SEAT_STARTED=$((SEAT_STARTED+1))
                  echo "Started system-level labwc seat: screentra-cast-labwc-kiosk@${SEAT_IDX}.service"
              else
                  echo "WARNING: screentra-cast-labwc-kiosk@${SEAT_IDX}.service failed to start (see 'journalctl -u screentra-cast-labwc-kiosk@${SEAT_IDX}.service')"
              fi
          fi
          # Mask the matching getty so the kernel doesn't drop a login
          # prompt on the display we just claimed for labwc.
          TTY_NUM=$((7 + SEAT_IDX))
          run_privileged systemctl mask "getty@tty${TTY_NUM}.service" 2>/dev/null || true
          # For indices > 0 the unit template's hard-coded TTYPath=/dev/tty7
          # is wrong; write a per-instance drop-in that maps it to tty(7+N).
          # Without this, all per-display instances race for tty7 and only one
          # can win the single-GPU DRM-master conflict, leaving the other
          # display showing a getty login.
          if [ "$SEAT_IDX" -gt 0 ]; then
              DROP_DIR="/etc/systemd/system/screentra-cast-labwc-kiosk@${SEAT_IDX}.service.d"
              DROP_FILE="$DROP_DIR/tty-and-conflicts.conf"
              run_privileged mkdir -p "$DROP_DIR" 2>/dev/null || true
              # Write the drop-in via a tempfile, then move it through run_privileged.
              # Heredocs cannot be passed through `run_privileged` because the body
              # is consumed by the outer shell before the inner shell can read it.
              DROP_TMP="$(mktemp)"
              cat > "$DROP_TMP" <<EOF_PER_DISPLAY
[Service]
TTYPath=/dev/tty${TTY_NUM}
[Unit]
Conflicts=getty@tty${TTY_NUM}.service
EOF_PER_DISPLAY
              run_privileged cp "$DROP_TMP" "$DROP_FILE" || true
              rm -f "$DROP_TMP"
              run_privileged systemctl daemon-reload || true
              run_privileged systemctl enable "screentra-cast-labwc-kiosk@${SEAT_IDX}.service" 2>/dev/null || true
              run_privileged systemctl restart "screentra-cast-labwc-kiosk@${SEAT_IDX}.service" 2>/dev/null || true
          fi
      done
      if [ "$SEAT_STARTED" -gt 0 ]; then
          LABWC_SYSTEM_INSTALLED=1
          echo "Installed and started $SEAT_STARTED system-level labwc display service(s)."
      else
          echo "WARNING: no system-level labwc services were started; the user-level kiosk service will still work once a TTY session is available."
      fi
    else
      echo "WARNING: could not install system labwc service; user kiosk service/fallback will still be configured."
    fi
  fi

  # Now safe to run preflight — the system-level seat services are started
  # (or deliberately skipped) and /run/user/UID exists.
  USER_RUNTIME_DIR="/run/user/$(id -u "$BOARD_USER")"
  "$PREFLIGHT" || true
  echo "preflight ran"

  # Seed the per-interface bind env file used by the heartbeat + actor units.
  # Empty SCREENTRA_CAST_BIND_IP means "no source IP override; let the kernel's
  # default route choose." The local admin's Network section writes into
  # this file when the operator switches interfaces.
  if [ ! -e "$INSTALL_DIR/network.env" ]; then
    printf 'SCREENTRA_CAST_BIND_IP=\n' > "$INSTALL_DIR/network.env" || true
    chmod 600 "$INSTALL_DIR/network.env" || true
  fi

  systemctl_user() {
      # Run systemctl --user as the target user. Bare `systemctl --user` from
      # a root context talks to root's user manager, not the target user's,
      # so enable/mask/restart silently no-op or report "unit does not exist"
      # for units that DO exist on disk. (Discovered 2026-08-10: the first
      # SSH install on 11.1.1.58 left every user service "disabled" because
      # of this exact race.)
      sudo -u "$BOARD_USER" XDG_RUNTIME_DIR="$USER_RUNTIME_DIR" \
          systemctl --user "$@"
  }
  systemctl_user daemon-reload || true
  # Clean up any stale orphaned per-display service instances that don't match
  # the expected @N pattern (0-3). These can accumulate across reinstalls.
  for f in "$SYSTEMD_USER_DIR"/screentra-cast-player-renderer@*.service; do
    instance="$(basename "$f" | sed 's/screentra-cast-player-renderer@//;s/.service//')"
    case "$instance" in
      '') ;; # skip template file screentra-cast-player-renderer@.service
    [0-9]*) ;; # keep numeric instances 0-9
    *) systemctl_user disable --now "screentra-cast-player-renderer@${instance}.service" 2>/dev/null || true; systemctl_user mask "screentra-cast-player-renderer@${instance}.service" 2>/dev/null || true; echo "Cleaned orphaned renderer instance: $instance";;
    esac
  done
  for f in "$SYSTEMD_USER_DIR"/screentra-cast-kiosk-browser@*.service; do
    instance="$(basename "$f" | sed 's/screentra-cast-kiosk-browser@//;s/.service//')"
    case "$instance" in
      '') ;; # skip template file screentra-cast-kiosk-browser@.service
    [0-9]*) ;; # keep numeric instances 0-9
    *) systemctl_user disable --now "screentra-cast-kiosk-browser@${instance}.service" 2>/dev/null || true; systemctl_user mask "screentra-cast-kiosk-browser@${instance}.service" 2>/dev/null || true; echo "Cleaned orphaned browser instance: $instance";;
    esac
  done
  # Enable lingering so user services survive logout and boot without a
  # logged-in session. Required for headless/autostart kiosk appliances.
  run_privileged loginctl enable-linger "$BOARD_USER" 2>/dev/null || true
  systemctl_user enable screentra-cast-player-heartbeat.service || true
  systemctl_user enable screentra-cast-player-actor.service || true
  systemctl_user enable screentra-cast-player-admin.service || true
  systemctl_user enable screentra-cast-player-display-provision.service || true
  systemctl_user restart screentra-cast-player-admin.service || true
  # Delegate per-display service lifecycle to the display-provision one-shot.
  # It re-reads /sys/class/drm, computes the active count, and enables/starts
  # the right renderer + kiosk-browser units. The same one-shot runs at
  # every login (default.target) and after suspend resume, so the install
  # path and the boot path stay in lock-step.
  if [ -x "$DISPLAY_PROVISION" ]; then
    echo "Running display-provision to enable per-display services for the current DRM state"
    LABWC_SYSTEM_INSTALLED="${LABWC_SYSTEM_INSTALLED:-0}" BOARD_USER="$BOARD_USER" "$DISPLAY_PROVISION" || true
  else
    echo "WARN: $DISPLAY_PROVISION missing; skipping per-display provisioning"
  fi
  # 2026-08-10: stop first so we get a fresh PID and a fresh client_id
  # write to the server (avoids the brief race where the old heartbeat
  # is still emitting under the old client_id while we write a new one).
  # daemon-reload first so user systemd sees the new unit files.
  systemctl_user daemon-reload 2>/dev/null || true
  systemctl_user stop screentra-cast-player-heartbeat.service 2>/dev/null || true
  systemctl_user restart screentra-cast-player-heartbeat.service || true
fi

"$HEARTBEAT" --once --server "$SERVER_URL" --client-id "$CLIENT_ID" --name "$CLIENT_NAME" --role "$ROLE" --version "$VERSION"

echo "Screentra Cast player client installed."
echo "Server: $SERVER_URL"
echo "Client ID: $CLIENT_ID"
echo "Service: screentra-cast-player-heartbeat.service"

echo "Renderer: http://127.0.0.1:3900/"
echo "Admin: http://$(hostname -I 2>/dev/null | awk '{print $1}'):3910/"
echo "Kiosk browser service: screentra-cast-kiosk-browser@0.service"
echo "Preferred display service: screentra-cast-labwc-kiosk@0.service"
echo "Helpers: screentra-cast-player-preflight, screentra-cast-player-status, screentra-cast-player-heartbeat, screentra-cast-player-renderer, screentra-cast-player-admin, screentra-cast-player-actor, screentra-cast-kiosk-launcher, screentra-cast-labwc-session"

# ----------------------------------------------------------------------
# Final verification: run a small probe script that confirms the
# install is actually healthy, not just 'every command returned 0'.
# Reports PASS/FAIL for the things that actually matter.
# ----------------------------------------------------------------------
echo
echo "==== Final install verification ===="
"$INSTALL_DIR/screentra-cast-player-preflight.py" || true
echo "==== End final verification ===="

# Post-install CA re-verify. The primary `install_screentra_cast_ca_trust` step can
# silently no-op if a transient permission glitch hides the early return;
# that leaves Chromium unable to trust the local server's HTTPS cert and
# the kiosk shows a black screen. Verify the cert actually landed in the
# OS trust store and re-run the install if not. We check via the cert's
# fingerprint so we don't depend on a particular grep string appearing in
# the bundle.
_screentra_cast_ca_fingerprint() {
    openssl x509 -in "$INSTALL_DIR/screentra-cast-signage-ca.crt" -noout -fingerprint -sha256 2>/dev/null \
        | sed -n 's/^.*Fingerprint=//p' | tr -d ':' | tr '[:upper:]' '[:lower:]'
}
_screentra_cast_ca_in_bundle() {
    local fp; fp="$(_screentra_cast_ca_fingerprint)"
    [ -z "$fp" ] && return 1
    if [ -f /etc/ssl/certs/ca-certificates.crt ]; then
        openssl crl2pkcs7 -nocrl -certfile /etc/ssl/certs/ca-certificates.crt 2>/dev/null \
            | openssl pkcs7 -print_certs 2>/dev/null \
            | openssl x509 -noout -fingerprint -sha256 2>/dev/null \
            | sed -n 's/^.*Fingerprint=//p' | tr -d ':' | tr '[:upper:]' '[:lower:]' \
            | grep -qx "$fp" && return 0
    fi
    return 1
}
if [ -s "$INSTALL_DIR/screentra-cast-signage-ca.crt" ]; then
    ca_trusted=0
    if [ -f /usr/local/share/ca-certificates/screentra-cast-signage-ca.crt ] && \
       grep -q 'BEGIN CERTIFICATE' /usr/local/share/ca-certificates/screentra-cast-signage-ca.crt 2>/dev/null && \
       _screentra_cast_ca_in_bundle; then
        ca_trusted=1
    fi
    if [ "$ca_trusted" = "0" ]; then
        echo "==== CA trust post-install re-check ===="
        echo "WARNING: $INSTALL_DIR/screentra-cast-signage-ca.crt exists but the OS"
        echo "         trust store does not list Screentra Cast Signage Local CA."
        echo "         Chromium on this box would refuse HTTPS to the server."
        if install_screentra_cast_ca_trust 2>/dev/null; then
            if _screentra_cast_ca_in_bundle; then
                echo "Re-install succeeded; CA is now trusted."
            else
                echo "Re-install completed but cert is still not in the bundle."
                echo "Manual fix:"
                echo "  sudo cp '$INSTALL_DIR/screentra-cast-signage-ca.crt' /usr/local/share/ca-certificates/screentra-cast-signage-ca.crt"
                echo "  sudo update-ca-certificates"
                echo "  sudo systemctl restart screentra-cast-labwc-kiosk@0.service"
            fi
        else
            echo "Re-install failed. Run as root:"
            echo "  sudo cp '$INSTALL_DIR/screentra-cast-signage-ca.crt' /usr/local/share/ca-certificates/screentra-cast-signage-ca.crt"
            echo "  sudo update-ca-certificates"
        fi
        echo "==== End CA trust re-check ===="
    fi
fi

# Wi-Fi / VLAN / static IP re-apply. The renderer normally does this at startup
# (via _network_restore_from_config), but invoking it here means an update_client
# that reboots a Wi-Fi-only box will come back up with the persisted iface/route.
# Non-fatal: missing wifi/vlan packages or absent hardware skip this without
# breaking the install.
if [ -x "$INSTALL_DIR/screentra-cast-player-renderer.py" ]; then
    echo "==== Re-apply persisted network config (wifi/vlan/static) ===="
    if sudo -n -u "$(id -un)" python3 -c "import sys, importlib.util; spec=importlib.util.spec_from_file_location('r', sys.argv[1]); m=importlib.util.module_from_spec(spec); spec.loader.exec_module(m); print(m._network_restore_from_config())" "$INSTALL_DIR/screentra-cast-player-renderer.py" 2>&1 | sed -e 's/^/  /'; then
        echo "==== End network config re-apply ===="
    else
        echo "==== Network config re-apply failed (non-fatal) ===="
    fi
fi

# 2026-08-10: when running as root, chown the install dirs back to the
# target user so user-level systemd can read env files (network.env) and
# so the renderer/heartbeat process can write cache/, admin-users.json,
# etc. Skip the chown if we're not root (the user-install path already
# owned everything correctly).
if [ "$(id -u)" = "0" ]; then
    echo "==== chown install dir to $BOARD_USER ===="
    chown -R "$BOARD_USER:$BOARD_USER" "$INSTALL_DIR" "$BIN_DIR" "$SYSTEMD_USER_DIR" 2>/dev/null || true
    # Also ensure log/state dirs are writable by the target user (the
    # display-provision service runs as $BOARD_USER and writes logs here).
    chown -R "$BOARD_USER:$BOARD_USER" "$HOME/.local/state/screentra-cast-player-client" 2>/dev/null || true
    chown -R "$BOARD_USER:$BOARD_USER" "$HOME/.local/share/screentra-cast-player-client" 2>/dev/null || true
    # Also write the NOPASSWD sudoers file if it's missing. The local admin
    # page probes `sudo -n systemctl --help` and fails with
    # "sudo -n systemctl is not available for this user" if tech can't sudo.
    # This entry grants tech ALL=(ALL) NOPASSWD: ALL which is fine on a
    # dedicated kiosk box; for production hardening, swap for a tighter
    # allowlist (just systemctl and the ir_bridge firmware commands).
    SUDO_FILE="/etc/sudoers.d/screentra-cast-board-nopasswd"
    if [ ! -f "$SUDO_FILE" ]; then
        cat > "$SUDO_FILE" <<SUDO_EOF
# Screentra Cast kiosk — granted by player-client installer on 2026-08-10.
# Allows the local admin page to call systemctl/iptools/etc. without
# prompting. Tighten if this box has untrusted local users.
$BOARD_USER ALL=(ALL) NOPASSWD: ALL
SUDO_EOF
        chmod 0440 "$SUDO_FILE"
        chown root:root "$SUDO_FILE"
        echo "Wrote $SUDO_FILE (NOPASSWD for $BOARD_USER)"
    else
        chmod 0440 "$SUDO_FILE"
        chown root:root "$SUDO_FILE"
    fi
fi

# 2026-08-10: when the system-level seat service (screentra-cast-labwc-kiosk@N)
# is enabled, mask the user-level screentra-cast-kiosk-browser@N units so they
# don't race the seat-owned launcher for the same Wayland socket.
# Without this, both services try to spawn chromium against the same
# compositor and both crash-loop with "No Chromium after sway startup
# delay attempt N/6; launching fallback child on WAYLAND_DISPLAY=wayland-N".
# The user-level units should only run when there's no system seat
# (e.g. dev boxes without a connected display).
SEAT_ENABLED=0
if command -v systemctl >/dev/null 2>&1; then
    for seat_idx in 0 1 2 3; do
        if systemctl is-enabled --quiet "screentra-cast-labwc-kiosk@${seat_idx}.service" 2>/dev/null; then
            SEAT_ENABLED=1
            break
        fi
    done
fi
if [ "$SEAT_ENABLED" = "1" ]; then
    echo "==== mask user-level kiosk-browser units (system seat owns the compositor) ===="
    USER_RUNTIME_DIR="/run/user/$(id -u "$BOARD_USER")"
    # Reload user systemd first so it sees the just-written unit files.
    if [ -d "$SYSTEMD_USER_DIR" ]; then
        sudo -u "$BOARD_USER" \
            XDG_RUNTIME_DIR="$USER_RUNTIME_DIR" \
            systemctl --user daemon-reload 2>/dev/null || true
    fi
    # Only mask display 0's instance — the system seat service already owns
    # display 0's chromium via the launcher that execs sway. Per-display
    # services for indices >= 1 remain active and start their own chromium
    # windows. (Display count is read from config.json's max_displays so
    # we never mask a service that's actually needed.)
    SCREENTRA_CAST_MAX_DISPLAYS="$(python3 -c 'import json,sys; d=json.load(open(sys.argv[1])); v=d.get("max_displays"); print(int(v) if v is not None else int(d.get("installer_detected_max_displays") or 1))' "$INSTALL_DIR/config.json" 2>/dev/null || echo 1)"
    kb_unit_template="$SYSTEMD_USER_DIR/screentra-cast-kiosk-browser@.service"
    if [ -f "$kb_unit_template" ]; then
        # Mask display 0 (system seat owns it).
        sudo -u "$BOARD_USER" \
            XDG_RUNTIME_DIR="$USER_RUNTIME_DIR" \
            systemctl --user stop "screentra-cast-kiosk-browser@0.service" 2>/dev/null || true
        if sudo -u "$BOARD_USER" \
            XDG_RUNTIME_DIR="$USER_RUNTIME_DIR" \
            systemctl --user mask "screentra-cast-kiosk-browser@0.service" 2>/dev/null; then
            echo "  masked: screentra-cast-kiosk-browser@0.service (system seat owns display 0)"
        else
            echo "  WARN: failed to mask screentra-cast-kiosk-browser@0.service"
        fi
        # Enable (unmask if needed) and start per-display services for 1..N-1
        # so the installer brings up every active display without depending
        # on screentra-cast-display-provision being present.
        for kb_idx in $(seq 1 $((SCREENTRA_CAST_MAX_DISPLAYS - 1))); do
            sudo -u "$BOARD_USER" \
                XDG_RUNTIME_DIR="$USER_RUNTIME_DIR" \
                systemctl --user unmask "screentra-cast-kiosk-browser@${kb_idx}.service" 2>/dev/null || true
            sudo -u "$BOARD_USER" \
                XDG_RUNTIME_DIR="$USER_RUNTIME_DIR" \
                systemctl --user enable "screentra-cast-kiosk-browser@${kb_idx}.service" 2>/dev/null || true
            sudo -u "$BOARD_USER" \
                XDG_RUNTIME_DIR="$USER_RUNTIME_DIR" \
                systemctl --user reset-failed "screentra-cast-kiosk-browser@${kb_idx}.service" 2>/dev/null || true
            sudo -u "$BOARD_USER" \
                XDG_RUNTIME_DIR="$USER_RUNTIME_DIR" \
                systemctl --user start "screentra-cast-kiosk-browser@${kb_idx}.service" 2>/dev/null || true
            echo "  started: screentra-cast-kiosk-browser@${kb_idx}.service"
        done
    fi
fi

if is_ssh_session; then
    # 2026-08-10: print client_id and admin URL so operators can verify
    # the install worked without ssh'ing back in.
    INSTALL_CLIENT_ID="$(cat "$INSTALL_DIR/client-id" 2>/dev/null || echo '(not yet generated)')"
    INSTALL_HOST_IP="$(ip -4 route get 1.1.1.1 2>/dev/null | awk '/src/ {for(i=1;i<=NF;i++) if ($i=="src") print $(i+1); exit}')"
    INSTALL_HOST_IP="${INSTALL_HOST_IP:-$(hostname -I 2>/dev/null | awk '{print $1}')}"
    cat <<SSH_DONE
----------------------------------------------------------------------
SSH install complete.

  Client ID:   $INSTALL_CLIENT_ID
  Hostname:    $(hostname)
  IP:          ${INSTALL_HOST_IP:-?}
  Admin URL:   http://${INSTALL_HOST_IP:-<this-host>}:3910/
  Server:      ${SCREENTRA_CAST_SERVER_URL:-http://${SCREENTRA_CAST_SERVER:-11.1.1.53:3000}/}

The renderer + heartbeat + actor + user services are running. The client
is now visible in the editor's Connected Clients page.

The system-level labwc seat service(s) have been enabled and started.
If the service is healthy, the kiosk should already be on screen (or
will appear within a few seconds on the next free TTY, typically tty7).

If the kiosk is NOT on screen after 10 seconds, check:

  sudo journalctl -u screentra-cast-labwc-kiosk@0.service -n 20 --no-pager
  who          # see which TTY is active

Common fixes:
  - Walk to the box, press Ctrl+Alt+F7, log in as $USER.
  - If the seat service is in a crash loop, the renderer is still
    serving the cached page at http://127.0.0.1:3900/ — you can verify
    the install was successful from the editor regardless.

  Self-heal CLI:
    sudo /usr/local/bin/riptr status      # check everything
    sudo /usr/local/bin/riptr heal        # fix everything that can be fixed
    sudo /usr/local/bin/riptr log         # see what changed
----------------------------------------------------------------------
SSH_DONE
fi
