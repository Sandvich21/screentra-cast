#!/usr/bin/env bash
# Definitions only: safe to source in unprivileged regression tests.
nm_method() {
    case "$1" in dhcp) echo auto;; static) echo manual;; *) return 1;; esac
}
nm_dns() { local ip; for ip in $1; do printf '%s;' "$ip"; done; printf '\n'; }
validate_network() {
    python3 - "$@" <<'PY'
import ipaddress, re, sys
host, mode, address, gateway, dns = sys.argv[1:]
try:
    assert len(host) <= 63 and re.fullmatch(r'[a-zA-Z0-9](?:[a-zA-Z0-9-]*[a-zA-Z0-9])?', host)
    assert mode in ('dhcp', 'static')
    if mode == 'static':
        assert '/' in address
        interface = ipaddress.IPv4Interface(address)
        assert 1 <= interface.network.prefixlen <= 32
        ipaddress.IPv4Address(gateway)
        assert dns.split()
        for value in dns.split():
            ipaddress.IPv4Address(value)
except (AssertionError, ValueError):
    sys.exit('Invalid hostname or IPv4 configuration; nothing has been erased.')
PY
}
validate_url() {
    python3 - "$1" <<'PY'
import sys, urllib.parse
try:
    u = urllib.parse.urlsplit(sys.argv[1])
    assert u.scheme in ('http', 'https') and u.hostname and u.port != 0
    assert not (u.username or u.password or u.query or u.fragment)
    assert not any(c.isspace() or c in '\\"\x27' for c in sys.argv[1])
except (ValueError, AssertionError):
    sys.exit('Use an unauthenticated HTTP(S) URL without query or fragment.')
PY
}
install_resolver() {
    rm -f -- "$2"
    install -m 0644 "$1" "$2"
}
verify_boot_files() {
    local root=$1 file kernel found=0
    for kernel in "$root"/boot/vmlinuz-*; do
        [[ -s "$kernel" && -s "$root/boot/initrd.img-${kernel##*/vmlinuz-}" ]] && found=1
    done
    [[ "$found" == 1 ]] || { echo 'Missing matching kernel/initrd' >&2; return 1; }
    for file in boot/grub/grub.cfg boot/efi/EFI/Screentra Cast/grubx64.efi boot/efi/EFI/BOOT/BOOTX64.EFI; do
        [[ -s "$root/$file" ]] || { echo "Missing boot artifact: $file" >&2; return 1; }
    done
}
