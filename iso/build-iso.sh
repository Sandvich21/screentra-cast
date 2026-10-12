#!/usr/bin/env bash
set -Eeuo pipefail
export PATH="/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"

ROOT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
PROJECT_DIR=$(cd -- "$ROOT_DIR/.." && pwd)
OUT_DIR=${OUT_DIR:-"$ROOT_DIR/out"}
WORK_DIR=${WORK_DIR:-"$ROOT_DIR/work"}
ARCH=${ARCH:-amd64}
MODE=${MODE:-menu}
DEBIAN_SUITE=${DEBIAN_SUITE:-trixie}
DEBIAN_MIRROR=${DEBIAN_MIRROR:-https://deb.debian.org/debian}
ALLOW_CROSS_BUILD=${ALLOW_CROSS_BUILD:-0}

log() { printf '[iso] %s\n' "$*"; }
die() { printf '[iso] ERROR: %s\n' "$*" >&2; exit 1; }

require_command() {
  command -v "$1" >/dev/null 2>&1 || die "missing build dependency: $1"
}

host_arch=$(dpkg --print-architecture 2>/dev/null || true)
if [[ "$ARCH" != "amd64" ]]; then
  die "Phase 1 only supports ARCH=amd64"
fi
if [[ "$host_arch" != "amd64" && "$ALLOW_CROSS_BUILD" != "1" ]]; then
  die "host architecture is $host_arch; set ALLOW_CROSS_BUILD=1 only after validating cross-build support"
fi

for cmd in live-build xorriso debootstrap mksquashfs; do
  require_command "$cmd"
done

[[ "$(id -u)" -eq 0 ]] || die "run the build as root or use a root-capable build wrapper"

mkdir -p "$OUT_DIR" "$WORK_DIR"

# Keep the build deterministic and isolated. The role selection is supplied
# through /etc/screentra-cast-iso/role during first boot; it is not a secret and
# is intentionally not baked into the client/server payloads.
export LB_MODE=debian
export LB_DISTRIBUTION="$DEBIAN_SUITE"
export LB_ARCHITECTURES="$ARCH"
export LB_MIRROR_BOOTSTRAP="$DEBIAN_MIRROR"
export LB_MIRROR_CHROOT="$DEBIAN_MIRROR"
export LB_PARENT_DIRECTORIES=true

cd "$WORK_DIR"

if [[ ! -f config/binary ]]; then
  log "initializing live-build configuration"
  lb config \
    --mode debian \
    --distribution "$DEBIAN_SUITE" \
    --architectures "$ARCH" \
    --archive-areas "main contrib non-free-firmware" \
    --mirror-bootstrap "$DEBIAN_MIRROR" \
    --mirror-chroot "$DEBIAN_MIRROR" \
    --apt-indices false \
    --bootappend-live "boot=live components username=board console=tty0 console=ttyS0,115200n8 systemd.debug_shell=ttyS0 systemd.getty_auto=no"
fi

mkdir -p config/package-lists config/hooks/normal config/includes.chroot/etc/screentra-cast-iso
mkdir -p config/includes.chroot/usr/local/sbin config/includes.chroot/etc/systemd/system config/includes.chroot/opt/screentra-cast/install
install -m 0755 "$ROOT_DIR/client-install.sh" config/includes.chroot/usr/local/sbin/screentra-cast-client-install
install -m 0644 "$ROOT_DIR/installer-contract.sh" config/includes.chroot/usr/local/sbin/installer-contract.sh
install -m 0755 "$ROOT_DIR/role-setup.sh" config/includes.chroot/usr/local/sbin/screentra-cast-iso-setup
install -m 0644 "$ROOT_DIR/screentra-cast-iso-setup.service" config/includes.chroot/etc/systemd/system/screentra-cast-iso-setup.service
install -m 0755 "$PROJECT_DIR/static/install/player-client.sh" config/includes.chroot/opt/screentra-cast/install/player-client.sh
install -m 0755 "$PROJECT_DIR/static/install/screentra-cast-server-install.sh" config/includes.chroot/opt/screentra-cast/install/server-install.sh
install -m 0755 "$PROJECT_DIR/static/install/screentra-cast-appliance-install.sh" config/includes.chroot/opt/screentra-cast/install/appliance-install.sh
mkdir -p config/includes.chroot/usr/share/screentra-cast config/includes.chroot/etc/systemd/system/getty@tty1.service.d
cat > config/includes.chroot/usr/share/screentra-cast/boot-status.html <<'HTML'
<!doctype html><meta charset="utf-8"><title>Screentra Cast ISO</title><style>body{background:#0d1117;color:#e6edf3;font:24px sans-serif;display:grid;place-items:center;height:100vh;margin:0}main{text-align:center}small{color:#8b949e}</style><main><h1>Screentra Cast Debian 13</h1><p>ISO booted successfully.</p></main>
HTML
cat > config/includes.chroot/etc/systemd/system/getty@tty1.service.d/autologin.conf <<'GETTY'
[Service]
ExecStart=
ExecStart=-/sbin/agetty --autologin board --noclear %I $TERM
GETTY

mkdir -p config/includes.binary/boot/grub
cat > config/includes.binary/boot/grub/config.cfg <<'GRUBCFG'
set default=0
set timeout=5
set timeout_style=menu
serial --unit=0 --speed=115200 --word=8 --parity=no --stop=1
terminal_input serial
terminal_output serial
if loadfont unicode ; then
    set gfxmode=800x600
    set gfxpayload=keep
    insmod efi_gop
    insmod efi_uga
    insmod video_bochs
    insmod video_cirrus
else
    set gfxmode=auto
    insmod all_video
fi
insmod gfxterm
insmod png
source /boot/grub/theme.cfg
terminal_output gfxterm
GRUBCFG

cat > config/hooks/normal/010-enable-screentra-cast-iso-setup.hook.chroot <<'HOOK'
#!/bin/sh
set -eu
systemctl enable screentra-cast-iso-setup.service
HOOK
chmod +x config/hooks/normal/010-enable-screentra-cast-iso-setup.hook.chroot
cat > config/hooks/normal/020-configure-client-session.hook.chroot <<'HOOK'
#!/bin/sh
set -eu
useradd --create-home --shell /bin/bash board 2>/dev/null || true
usermod -aG video,render,input,sudo board 2>/dev/null || true
cat > /etc/sudoers.d/screentra-cast-board <<'SUDOERS'
board ALL=(root) NOPASSWD: /usr/local/sbin/screentra-cast-client-install
SUDOERS
chmod 0440 /etc/sudoers.d/screentra-cast-board
usermod -aG video,render,input board 2>/dev/null || true
passwd --delete board >/dev/null 2>&1 || true
mkdir -p /home/board/.config/sway
cat > /home/board/.config/sway/config <<'SWAY'
output * bg #0d1117 solid_color
exec chromium --kiosk --noerrdialogs --disable-session-crashed-bubble file:///usr/share/screentra-cast/boot-status.html
SWAY
cat > /home/board/.bash_profile <<'PROFILE'
if [ "$(tty)" = /dev/tty1 ] && [ -z "${WAYLAND_DISPLAY:-}" ]; then
    while [ ! -e /etc/screentra-cast-client-installed ]; do
        if sudo -n /usr/local/sbin/screentra-cast-client-install; then
            echo 'Installation completed. Remove the ISO and reboot into the installed disk.'
            break
        else
            rc=$?
            if [ "$rc" = 2 ]; then
                echo 'Installation cancelled.'
            else
                echo "Installation FAILED (exit $rc). Logs: /var/log/screentra-cast-installer/"
            fi
        fi
        echo 'Live boot is not installation success.'
        read -r -p '[r]etry (requires fresh erase confirmation), [c]onsole: ' answer
        case "$answer" in r|R) ;; *) break;; esac
    done
    [ ! -e /etc/screentra-cast-client-installed ] || echo 'Installed disk is ready for reboot testing; this is still the live console.'
    # Remain at the shell for diagnostics; never hide failure behind Sway.
fi
PROFILE
chown -R board:board /home/board
systemctl enable getty@tty1.service
HOOK
chmod +x config/hooks/normal/020-configure-client-session.hook.chroot

printf '%s\n' "$MODE" > config/includes.chroot/etc/screentra-cast-iso/default-role

cat > config/package-lists/screentra-cast.list.chroot <<'PACKAGES'
sudo
ca-certificates
curl
wget
python3
python3-venv
dbus
dbus-user-session
systemd
systemd-sysv
network-manager
openssh-client
whiptail
sway
chromium
seatd
libgl1-mesa-dri
mesa-vulkan-drivers
fonts-noto
fonts-noto-color-emoji
firmware-linux-free firmware-atheros
debootstrap
parted
dosfstools
e2fsprogs
rsync
wlr-randr
PACKAGES

log "building Debian $DEBIAN_SUITE $ARCH ISO"
lb build

iso=$(find "$WORK_DIR" -maxdepth 1 -type f -name '*.iso' -print -quit)
[[ -n "$iso" ]] || die "live-build completed without producing an ISO"
cp -f "$iso" "$OUT_DIR/screentra-cast-debian13-${ARCH}.iso"
sha256sum "$OUT_DIR/screentra-cast-debian13-${ARCH}.iso" > "$OUT_DIR/screentra-cast-debian13-${ARCH}.iso.sha256"
log "created $OUT_DIR/screentra-cast-debian13-${ARCH}.iso"
