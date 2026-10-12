#!/usr/bin/env bash
set +x # Never trace passwords, even when invoked with bash -x.
set -Eeuo pipefail
umask 022
export PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
export HOME=${HOME:-/root} USER=root LOGNAME=root

# Client-only, destructive UEFI installer. No server installation path here.
source "$(dirname -- "${BASH_SOURCE[0]}")/installer-contract.sh"
[[ "$(id -u)" -eq 0 ]] || { echo 'Run as root.' >&2; exit 1; }
TARGET=/mnt/screentra-cast-target
INSTALLER=/opt/screentra-cast/install/player-client.sh
DEBIAN_SUITE=${DEBIAN_SUITE:-trixie}
DEBIAN_MIRROR=${DEBIAN_MIRROR:-https://deb.debian.org/debian}
STAGE=preflight
OWNED_MOUNTS=()
POLICY_ACTIVE=0
TARGET_MOUNTED=0
install -d -m 0700 /var/log/screentra-cast-installer
LOG=$(mktemp /var/log/screentra-cast-installer/install-XXXXXXXX.log)
exec 3>&1 4>&2
exec > >(tee -a "$LOG" >&3) 2>&1
LOG_PID=$!

stage() { STAGE=$1; printf '\n[installer] stage=%s\n' "$STAGE"; }
die() { echo "[installer] ERROR: $*" >&2; exit 1; }
cancel() { echo '[installer] Cancelled; no further actions.'; exit 2; }
target_exec() {
    chroot "$TARGET" /usr/bin/env -i HOME=/root USER=root LOGNAME=root \
        PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin \
        DEBIAN_FRONTEND=noninteractive "$@"
}
restore_policy() {
    local rc
    if [[ "$POLICY_ACTIVE" == 1 ]]; then
        rm -f "$TARGET/usr/sbin/policy-rc.d" || {
            rc=$?
            echo "Failed to remove temporary policy-rc.d (exit=$rc); policy backup retained." >&2
            return "$rc"
        }
        if [[ -e "$TARGET/usr/sbin/policy-rc.d.screentra-original" || -L "$TARGET/usr/sbin/policy-rc.d.screentra-original" ]]; then
            mv "$TARGET/usr/sbin/policy-rc.d.screentra-original" "$TARGET/usr/sbin/policy-rc.d" || {
                rc=$?
                echo "Failed to restore policy-rc.d (exit=$rc); policy backup retained." >&2
                return "$rc"
            }
        fi
        POLICY_ACTIVE=0
    fi
}
finish() {
    local rc=$? i
    trap - EXIT INT TERM
    set +e
    restore_policy || rc=1
    unset ssh_password ssh_password_confirm SCREENTRA_SSH_PASSWORD
    printf '[installer] stage=%s exit=%s log=%s\n' "$STAGE" "$rc" "$LOG"
    # Drain tee before saving a complete log into the target.
    exec 1>&3 2>&4
    wait "$LOG_PID"
    if [[ "$TARGET_MOUNTED" == 1 ]]; then
        install -d -m 0755 "$TARGET/var/log/screentra-cast-installer"
        install -m 0600 "$LOG" "$TARGET/var/log/screentra-cast-installer/$(basename "$LOG")" || rc=1
        [[ ! -f "$TARGET/var/log/screentra-cast-client-install.log" ]] || \
            install -m 0600 "$TARGET/var/log/screentra-cast-client-install.log" "${LOG%.log}-payload.log"
        if [[ "$rc" != 0 ]]; then rm -f "$TARGET/etc/screentra-cast/install-complete"; fi
    fi
    for ((i=${#OWNED_MOUNTS[@]}-1; i>=0; i--)); do
        if ! umount -R "${OWNED_MOUNTS[i]}"; then
            echo "Cleanup failed: ${OWNED_MOUNTS[i]}; retained for inspection." | tee -a "$LOG" >&2
            rc=1
        fi
    done
    if [[ "$rc" == 0 ]]; then
        touch /etc/screentra-cast-client-installed
        echo "Installation completed on $DISK. Remove the ISO and reboot."
    elif [[ "$rc" == 2 ]]; then
        echo "Installation cancelled. Log: $LOG"
    else
        echo "Installation FAILED at $STAGE. Log: $LOG. Do not treat live boot as install success." >&2
    fi
    exit "$rc"
}
trap finish EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
for cmd in debootstrap parted partprobe udevadm wipefs mkfs.fat mkfs.ext4 python3 findmnt lsblk blkid chroot flock swapon; do
    command -v "$cmd" >/dev/null || die "Missing dependency: $cmd"
done
exec 9>/run/screentra-cast-installer.lock
flock -n 9 || die 'Another installer is running.'
[[ ! -e /etc/screentra-cast-client-installed ]] || die 'Already completed in this live boot; reboot before another installation.'
[[ -d /sys/firmware/efi ]] || die 'UEFI boot is required.'
[[ -s "$INSTALLER" ]] || die 'Player payload missing.'
mountpoint -q "$TARGET" && die 'Target mount already exists; inspect and unmount it before retry.'
[[ ! -L "$TARGET" ]] || die 'Target path must not be a symlink.'
validate_url "$DEBIAN_MIRROR"
[[ "$DEBIAN_SUITE" =~ ^[a-z0-9-]+$ ]] || die 'Invalid Debian suite.'

server_url=${SCREENTRA_SERVER_URL:-}
if [[ -z "$server_url" && -t 0 ]]; then
    read -r -p 'Screentra Cast server URL: ' server_url || cancel
fi
server_url=${server_url%/}
validate_url "$server_url"
network_mode=${SCREENTRA_NETWORK_MODE:-dhcp}
static_address=${SCREENTRA_STATIC_ADDRESS:-}
static_gateway=${SCREENTRA_STATIC_GATEWAY:-}
static_dns=${SCREENTRA_STATIC_DNS:-}
hostname=${SCREENTRA_HOSTNAME:-screentra-cast-client}
if command -v whiptail >/dev/null && [[ -t 0 ]]; then
    network_mode=$(whiptail --title 'Network configuration' --menu 'Installed wired network:' 13 72 2 dhcp DHCP static 'Static IPv4' 3>&1 1>&2 2>&3) || cancel
    hostname=$(whiptail --inputbox 'Client hostname:' 10 72 "$hostname" 3>&1 1>&2 2>&3) || cancel
    if [[ "$network_mode" == static ]]; then
        static_address=$(whiptail --inputbox 'IPv4 address/prefix:' 10 72 "$static_address" 3>&1 1>&2 2>&3) || cancel
        static_gateway=$(whiptail --inputbox 'IPv4 gateway:' 10 72 "$static_gateway" 3>&1 1>&2 2>&3) || cancel
        static_dns=$(whiptail --inputbox 'DNS IPv4 addresses (space-separated):' 10 72 "$static_dns" 3>&1 1>&2 2>&3) || cancel
    fi
fi
validate_network "$hostname" "$network_mode" "$static_address" "$static_gateway" "$static_dns"

ssh_password=${SCREENTRA_SSH_PASSWORD:-}
unset SCREENTRA_SSH_PASSWORD
if command -v whiptail >/dev/null && [[ -t 0 ]]; then
    ssh_password=$(whiptail --passwordbox 'Set board SSH password:' 10 72 3>&1 1>&2 2>&3) || cancel
    ssh_password_confirm=$(whiptail --passwordbox 'Confirm board SSH password:' 10 72 3>&1 1>&2 2>&3) || cancel
    [[ "$ssh_password" == "$ssh_password_confirm" ]] || die 'Passwords do not match.'
elif [[ -z "$ssh_password" && -t 0 ]]; then
    read -r -s -p 'Set board SSH password: ' ssh_password || cancel; printf '\n'
    read -r -s -p 'Confirm board SSH password: ' ssh_password_confirm || cancel; printf '\n'
    [[ "$ssh_password" == "$ssh_password_confirm" ]] || die 'Passwords do not match.'
fi
[[ -n "$ssh_password" && "$ssh_password" != *$'\n'* && "$ssh_password" != *$'\r'* ]] || die 'A nonempty single-line SSH password is required.'

DISK=${SCREENTRA_TARGET_DISK:-}
if [[ -z "$DISK" ]]; then
    [[ -t 0 ]] || die 'Noninteractive installation requires SCREENTRA_TARGET_DISK and SCREENTRA_ERASE_CONFIRM.'
    lsblk -dpno NAME,SIZE,TYPE
    read -r -p 'Exact disk path to erase (no default): ' DISK || cancel
fi
[[ -b "$DISK" && "$(lsblk -dnro TYPE "$DISK")" == disk ]] || die 'Select a whole block disk.'
DISK=$(readlink -f "$DISK")
# Fail closed if live medium cannot be identified; compare ancestry, not prefixes.
live_source=$(findmnt -nro SOURCE --mountpoint /run/live/medium) || die 'Cannot identify live boot medium.'
live_source=$(readlink -f "$live_source")
[[ -b "$live_source" ]] || die 'Live source is not an identifiable block device.'
live_ancestry=$(lsblk -srnpo NAME "$live_source") || die 'Cannot resolve live medium ancestry.'
[[ -n "$live_ancestry" ]] || die 'Empty live medium ancestry.'
while read -r parent; do
    [[ "$(readlink -f "$parent")" != "$DISK" ]] || die 'Refusing to erase the live medium.'
done <<< "$live_ancestry"
# Also reject mounted partitions, swap, RAID/LVM descendants (not just the ISO).
while read -r child type; do
    [[ "$child" == "$DISK" || "$type" == part ]] || die 'Target has active device-mapper/RAID descendants.'
    if findmnt -rn -S "$child" >/dev/null; then die 'Target has mounted filesystems.'; fi
    while read -r swap; do [[ "$swap" != "$child" ]] || die 'Target has active swap.'; done < <(swapon --show=NAME --noheadings)
done < <(lsblk -rnpo NAME,TYPE "$DISK")
if [[ -t 0 ]]; then
    read -r -p "Type ERASE $DISK to destroy all data: " confirm || cancel
    [[ "$confirm" == "ERASE $DISK" ]] || cancel
else
    [[ "${SCREENTRA_ERASE_CONFIRM:-}" == "ERASE $DISK" ]] || die 'Exact SCREENTRA_ERASE_CONFIRM required.'
fi

stage partition
wipefs -a "$DISK"
parted -s "$DISK" mklabel gpt
parted -s "$DISK" mkpart ESP fat32 1MiB 513MiB
parted -s "$DISK" set 1 esp on
parted -s "$DISK" mkpart root ext4 513MiB 100%
partprobe "$DISK"
udevadm settle
case "$DISK" in *[0-9]) ESP="${DISK}p1"; ROOT_PART="${DISK}p2";; *) ESP="${DISK}1"; ROOT_PART="${DISK}2";; esac
mkfs.fat -F32 -n SCREENTRA_EFI "$ESP"
mkfs.ext4 -F -L SCREENTRA_ROOT "$ROOT_PART"
mkdir -p "$TARGET"
mount "$ROOT_PART" "$TARGET"
OWNED_MOUNTS+=("$TARGET")
TARGET_MOUNTED=1
mkdir -p "$TARGET/boot/efi"
mount "$ESP" "$TARGET/boot/efi"
OWNED_MOUNTS+=("$TARGET/boot/efi")
stage bootstrap
debootstrap --arch=amd64 "$DEBIAN_SUITE" "$TARGET" "$DEBIAN_MIRROR"
printf 'deb %s %s main contrib non-free-firmware\n' "$DEBIAN_MIRROR" "$DEBIAN_SUITE" > "$TARGET/etc/apt/sources.list"
printf 'deb %s %s-updates main contrib non-free-firmware\n' "$DEBIAN_MIRROR" "$DEBIAN_SUITE" >> "$TARGET/etc/apt/sources.list"
printf 'deb https://security.debian.org/debian-security %s-security main contrib non-free-firmware\n' "$DEBIAN_SUITE" >> "$TARGET/etc/apt/sources.list"
for fs in dev proc sys; do
    mount --rbind "/$fs" "$TARGET/$fs"
    OWNED_MOUNTS+=("$TARGET/$fs")
    mount --make-rslave "$TARGET/$fs"
done
# Private target runtime: never expose the live systemd bus.
mount -t tmpfs -o mode=0755,nosuid,nodev tmpfs "$TARGET/run"
OWNED_MOUNTS+=("$TARGET/run")
install_resolver /etc/resolv.conf "$TARGET/etc/resolv.conf"
root_uuid=$(blkid -s UUID -o value "$ROOT_PART")
esp_uuid=$(blkid -s UUID -o value "$ESP")
[[ -n "$root_uuid" && -n "$esp_uuid" ]] || die 'Missing filesystem UUIDs.'
cat > "$TARGET/etc/fstab" <<FSTAB
UUID=$root_uuid / ext4 defaults,noatime 0 1
UUID=$esp_uuid /boot/efi vfat umask=0077 0 1
FSTAB
printf '%s\n' "$hostname" > "$TARGET/etc/hostname"
printf '127.0.0.1 localhost\n127.0.1.1 %s\n::1 localhost ip6-localhost ip6-loopback\n' "$hostname" > "$TARGET/etc/hosts"
install -d -m 0700 "$TARGET/etc/NetworkManager/system-connections"
(umask 077
cat > "$TARGET/etc/NetworkManager/system-connections/Screentra Cast Ethernet.nmconnection" <<NM
[connection]
id=Screentra Cast Ethernet
type=ethernet
autoconnect=true

[ethernet]

[ipv4]
method=$(nm_method "$network_mode")
NM
if [[ "$network_mode" == static ]]; then
    printf 'address1=%s,%s\ndns=%s\n' "$static_address" "$static_gateway" "$(nm_dns "$static_dns")" >> "$TARGET/etc/NetworkManager/system-connections/Screentra Cast Ethernet.nmconnection"
fi
printf '\n[ipv6]\nmethod=auto\n' >> "$TARGET/etc/NetworkManager/system-connections/Screentra Cast Ethernet.nmconnection"
)
chmod 0600 "$TARGET/etc/NetworkManager/system-connections/Screentra Cast Ethernet.nmconnection"
install -d -m 0755 "$TARGET/etc/modules-load.d"
printf 'ath10k_pci\n' > "$TARGET/etc/modules-load.d/ath10k_pci.conf"
# Preserve a preexisting policy, including a symlink, until all packaging ends.
[[ ! -e "$TARGET/usr/sbin/policy-rc.d.screentra-original" && ! -L "$TARGET/usr/sbin/policy-rc.d.screentra-original" ]] || die 'Policy backup already exists.'
if [[ -e "$TARGET/usr/sbin/policy-rc.d" || -L "$TARGET/usr/sbin/policy-rc.d" ]]; then
    mv "$TARGET/usr/sbin/policy-rc.d" "$TARGET/usr/sbin/policy-rc.d.screentra-original"
fi
POLICY_ACTIVE=1
printf '#!/bin/sh\nexit 101\n' > "$TARGET/usr/sbin/policy-rc.d"
chmod 0755 "$TARGET/usr/sbin/policy-rc.d"
stage packages
mirror_host=$(python3 -c 'import urllib.parse,sys; print(urllib.parse.urlsplit(sys.argv[1]).hostname)' "$DEBIAN_MIRROR")
target_exec runuser -u _apt -- getent ahosts "$mirror_host" >/dev/null
target_exec runuser -u _apt -- getent ahosts security.debian.org >/dev/null
target_exec apt-get -o APT::Update::Error-Mode=any update
PACKAGES=(linux-image-amd64 initramfs-tools systemd systemd-sysv dbus dbus-user-session
    network-manager sudo curl wget ca-certificates python3 python3-venv
    whiptail sway chromium seatd libgl1-mesa-dri mesa-vulkan-drivers
    fonts-noto fonts-noto-color-emoji firmware-linux-free firmware-atheros grub-efi-amd64
    openssh-server parted dosfstools e2fsprogs rsync openssh-client wlr-randr wlrctl
    libnss3-tools openssl efibootmgr ddcutil cec-utils)
target_exec apt-get install -y --no-install-recommends "${PACKAGES[@]}"
stage payload
install -d -m 0755 "$TARGET/opt/screentra-cast" "$TARGET/var/log"
install -m 0755 "$INSTALLER" "$TARGET/opt/screentra-cast/player-client.sh"
(umask 077; target_exec env SCREENTRA_INSTALL_MODE=offline SCREENTRA_SERVER_URL="$server_url" \
    SCREENTRA_BOARD_USER=board bash /opt/screentra-cast/player-client.sh > "$TARGET/var/log/screentra-cast-client-install.log" 2>&1)
printf 'board:%s\n' "$ssh_password" | target_exec chpasswd
unset ssh_password ssh_password_confirm
stage bootloader
target_exec grub-install --target=x86_64-efi --efi-directory=/boot/efi --bootloader-id=Screentra Cast --recheck || \
    target_exec grub-install --target=x86_64-efi --efi-directory=/boot/efi --bootloader-id=Screentra Cast --no-nvram --recheck
target_exec grub-install --target=x86_64-efi --efi-directory=/boot/efi --no-nvram --removable --recheck
target_exec update-grub
target_exec systemctl --root=/ enable NetworkManager seatd ssh
restore_policy
stage verification
for package in "${PACKAGES[@]}"; do
    [[ "$(target_exec dpkg-query -W -f='${Status}' "$package")" == 'install ok installed' ]] || die "Package not configured: $package"
done
install -d -m 0755 "$TARGET/run/sshd"
target_exec /usr/sbin/sshd -t
for unit in ssh NetworkManager seatd screentra-cast-kiosk@0 screentra-client-heartbeat screentra-client-actor screentra-client-admin; do
    target_exec systemctl --root=/ is-enabled "$unit.service"
done
board_home=$(target_exec getent passwd board | cut -d: -f6)
[[ "$board_home" == /home/board ]] || die 'Unexpected board home.'
for helper in screentra-client-heartbeat.py screentra-client-renderer.py screentra-client-actor.py screentra-kiosk.sh screentra-display-detect.py; do
    target_exec runuser -u board -- test -s "$board_home/.local/share/screentra-client/$helper"
done
for unit in screentra-client-heartbeat.service screentra-client-actor.service screentra-client-admin.service screentra-client-renderer@.service screentra-kiosk-browser@.service; do
    target_exec runuser -u board -- test -r "$board_home/.config/systemd/user/$unit"
done
target_exec runuser -u board -- test -x "$board_home/.local/bin/screentra-client-status"
ownership_mismatch=$(target_exec find "$board_home/.local/share/screentra-client" "$board_home/.local/bin" "$board_home/.config/systemd/user" ! -user board -print -quit) || die 'Unable to verify board file ownership.'
[[ -z "$ownership_mismatch" ]] || die 'Incorrect board file ownership.'
verify_boot_files "$TARGET"
install -d -m 0755 "$TARGET/etc/screentra-cast"
(umask 077; printf '%s\n' "$server_url" > "$TARGET/etc/screentra-cast/server-url")
printf 'client\n' > "$TARGET/etc/screentra-cast/installed-role"
printf '%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" > "$TARGET/etc/screentra-cast/install-complete"
sync
stage completed
