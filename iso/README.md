# Screentra Cast Debian 13 amd64 ISO

The persistent installer in this directory is **client-only, amd64, UEFI**.
Server/combined payload wrappers remain in the live foundation, but this installer
neither installs nor validates those roles. A successful live boot is not a
successful disk installation.

## Build prerequisites

The build host needs root privileges, network access, `live-build`, `xorriso`,
`debootstrap`, and `squashfs-tools`. On Debian 13:

```sh
sudo apt-get update
sudo apt-get install live-build xorriso debootstrap squashfs-tools syslinux-common isolinux
sudo ./iso/build-iso.sh
```

Artifacts go in `iso/out/`. Clean the previous live-build work tree before
rebuilding changed installers: cached stages can otherwise embed old payloads.
Do not embed passwords, authenticated URLs, tokens, private CAs or client state.
The builder includes `installer-contract.sh` next to the privileged installer.

## Client installation contract

- Boot the live ISO in UEFI mode. tty1 runs the privileged client installer.
  Failure or cancellation stays at the console; retry asks for a fresh disk
  selection/erase confirmation. It does not silently launch the live Sway page.
- Select an explicit **whole disk**. There is no first-disk default. The installer
  rejects the boot medium (by block ancestry), mounted targets, active swap,
  device-mapper/RAID descendants, an existing target mount, and concurrent runs.
  If the live boot medium cannot be identified, it fails closed.
- Supply an unauthenticated HTTP(S) server URL and hostname. Wired networking
  supports DHCP or static IPv4 address/prefix, gateway and space-separated IPv4
  DNS servers. Inputs are validated **before erase**. NetworkManager receives
  `auto`/`manual` and semicolon-terminated DNS values. Wi-Fi setup is out of scope.
- Supply and confirm a board SSH password interactively. It goes directly to
  `chpasswd` over stdin, never to a persistent plaintext password file. Tracing
  is disabled; ordinary installation uses umask 022, secret files use 0600.
- The target is a fresh Debian 13 debootstrap, not a clone of the live system.
  Resolver permissions are explicitly 0644. DNS is tested as `_apt`; apt update
  rejects partial mirror failures with `APT::Update::Error-Mode=any`.
- All target execution has explicit root HOME/USER/LOGNAME/PATH. The reusable
  player payload derives helpers and user units from **board's passwd home**,
  not root's HOME. Its `SCREENTRA_INSTALL_MODE=offline` installs and enables system
  services without contacting a running system/user bus, taking a seat,
  probing displays, running preflight, or enrolling the client. Normal online
  installation retains service start actions. Enrollment/health is a first-boot
  concern, not evidence from a live chroot.
- A temporary `policy-rc.d` blocks package service startup and preserves/restores
  any previous policy (including symlinks), also on failure. `/dev` is recursively
  bound once, `/proc` and `/sys` have slave propagation, and `/run` is a private
  tmpfs, never the live systemd bus. Cleanup reverses only mounts this run owns.
- Before completion: verify configured packages, `sshd -t`, offline SSH/client
  enablement, board helpers/user units and ownership, matching kernel/initrd,
  `grub.cfg`, `EFI/Screentra Cast/grubx64.efi`, and `EFI/BOOT/BOOTX64.EFI`. fstab uses
  filesystem UUIDs. The system kiosk wrapper remains privileged but launches
  the compositor, renderer and Chromium as board with native Wayland/DRM;
  Xwayland is disabled in generated Sway configuration.

## Disposable VM automation

Use a privileged shell **inside a disposable live VM**, never on the build host.
Noninteractive operation requires all of:

- `SCREENTRA_TARGET_DISK`: exact disposable whole-disk device
- `SCREENTRA_ERASE_CONFIRM`: exact `ERASE /dev/...` using the canonical device path
- `SCREENTRA_SERVER_URL`: unauthenticated server origin/base URL
- `SCREENTRA_SSH_PASSWORD`: supplied at runtime (do not put a value in source,
  shell history, process arguments, logs or ISO artifacts)

Optional inputs are `SCREENTRA_HOSTNAME`, `SCREENTRA_NETWORK_MODE` (`dhcp`/`static`),
`SCREENTRA_STATIC_ADDRESS`, `SCREENTRA_STATIC_GATEWAY`, `SCREENTRA_STATIC_DNS`,
`DEBIAN_SUITE`, and `DEBIAN_MIRROR`. The environment password is consumed and
unset before package/payload commands. Prefer the interactive password prompt.
The live GUI sudo entry intentionally does not forward arbitrary environment;
automation must arrange its own root context in the disposable VM.

## Logs, exit status and recovery

Live logs: `/var/log/screentra-cast-installer/install-*.log` (root-only), with stage
and exit status; payload failure logs are copied there before target cleanup.
Once a target exists, logs are also saved beneath target
`/var/log/screentra-cast-installer/`, plus `/var/log/screentra-cast-client-install.log`.
Copy live logs off the VM before shutting it down. A failure during bootstrap
may leave only a partial target, but the live log is retained.

Exit 0 means provisioning verification and cleanup succeeded; 2 means explicit
cancellation; other statuses mean failure. A failed cleanup is reported and the
remaining mount is left for inspection, not forcibly/lazily unmounted. A retry
refuses stale mounts until an operator resolves them. Successful target
provisioning writes `/etc/screentra-cast/install-complete`; successful live cleanup
also creates `/etc/screentra-cast-client-installed` to prevent repeat installation
in that live boot. Remove the ISO and reboot; no automatic reboot is performed.

## Local regression checks (non-destructive)

```sh
bash -n iso/client-install.sh
bash -n iso/installer-contract.sh
bash -n iso/build-iso.sh
bash -n static/install/player-client.sh
python3 -m unittest discover -s iso/tests -v
git diff --check
```

Tests execute only isolated definitions/initialization or a verification block
with mocked target operations. They do not source the full payload, partition,
mount, chroot, install packages, or start services.

The missing HOME abort was observed in the disposable VM debug-shell path.
The earlier resolver failure came from that harness's umask 077 and a 0600
resolver: `_apt` failed while root resolved. That does **not** establish a
physical-machine DNS defect. Both conditions now have explicit contracts.

These checks do not establish install readiness. Independent review, a fresh
ISO build, disposable UEFI installation, disk-only reboot (normal and fallback
EFI), SSH access, enrollment, native Wayland/DRM and persistent runtime checks
remain required before physical installation.
