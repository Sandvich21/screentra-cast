# Screentra Cast

Screentra Cast is a self-hosted digital-signage platform built for physical
displays. It combines a Rust management server with Linux player clients so
teams can design, schedule, publish, and monitor signage layouts across a fleet
of screens. The platform supports reusable widgets, media, weather, news,
traffic, calendars, scheduling, remote client management, and signed client
updates.

**Physical displays only:** virtual-display deployments are not supported.

## Current support status

- **Validated server platform:** Debian 13 amd64
- **Expected server platform:** Ubuntu Server amd64 with `apt` and `systemd`;
  validate the selected Ubuntu release before production use
- **Current server release:** [`v0.1.3`](https://github.com/Sandvich21/screentra-cast/releases/tag/v0.1.3)
- **Appliance ISO:** does not work as of now. It is amd64/UEFI work in
  progress and is not included in releases until independent physical boot and
  installation validation is complete

## Repository layout

- `src/` — Rust server
- `static/` — dashboard, previews, player pages, installers, and shared assets
- `player-client/` — canonical Linux player-client scripts and installer source
- `iso/` — appliance ISO build materials; see [`iso/README.md`](iso/README.md)
- `tools/` — bundle and migration utilities
- `tests/` — integration and contract tests

## Install a new server

Use a fresh Debian 13 amd64 server, or an Ubuntu Server amd64 system with
`apt`, `systemd`, network access, `curl`, and `sudo`. The installer creates the
PostgreSQL database, dedicated `board` service account, server-local TLS
certificates, and `screentra-cast-server.service`.

The release installer verifies the SHA-256 checksums for the server binary,
static archive, and installer bundle before installing them.

```bash
RELEASE=v0.1.3
BASE="https://github.com/Sandvich21/screentra-cast/releases/download/$RELEASE"

curl -fsSLo /tmp/screentra-cast-server-install.sh \
  "$BASE/screentra-cast-server-install.sh"
chmod 0755 /tmp/screentra-cast-server-install.sh

sudo env \
  SCREENTRA_CAST_RELEASE_BASE_URL="$BASE" \
  SCREENTRA_CAST_SERVER_HOST=<server-ip-or-dns> \
  SCREENTRA_CAST_LOCAL_HOSTNAME=<server-ip-or-dns> \
  bash /tmp/screentra-cast-server-install.sh
```

After installation, open the displayed HTTPS dashboard URL and create the
first administrator. The installer does **not** create a default administrator
password. Keep the generated CA private to the server; distribute only the
published CA certificate to clients that need to trust it.

The installer creates a clean server and does not copy databases, layouts,
client assignments, runtime uploads, backups, certificates, or private keys
from another server. Migrate those items separately when replacing an
existing server. Uploaded media belongs in the server's runtime `uploads/`
directory (normally `/home/board/server/uploads`), not only in
`static/uploads/`.

## Install a physical display client

After the server is running:

1. Install the signed player-client installer supplied by the server or release
   on a supported Debian physical-display device.
2. Adopt the client in the dashboard.
3. Assign a layout to the physical display.
4. Verify the client's heartbeat and physical output before production use.

Client identity, cached layouts, credentials, and runtime uploads are created
on the client or server during installation and are not stored in this source
repository.

## Build and test from source

The following packages are sufficient for a Debian 13 amd64 build host:

```bash
sudo apt-get update
sudo apt-get install -y \
  git build-essential cmake pkg-config libssl-dev libsqlite3-dev libpq-dev \
  nodejs npm python3
```

Use a current stable Rust toolchain. Debian's packaged Rust version may be too
old for the versions locked in `Cargo.lock`:

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
  | sh -s -- -y --profile minimal --default-toolchain stable
. "$HOME/.cargo/env"
```

Clone the repository and run the build/test checks:

```bash
git clone https://github.com/Sandvich21/screentra-cast.git
cd screentra-cast

cargo build --release
cargo test --release
python3 -B -m py_compile tools/*.py player-client/scripts/*.py iso/tests/*.py

while IFS= read -r file; do node --check "$file"; done < <(git ls-files '*.js')
while IFS= read -r file; do bash -n "$file"; done < <(git ls-files '*.sh')
```

The release server binary is produced at:

```text
target/release/screentra-cast
```

The player-client bundle builder requires the client-update signing key to be
provisioned outside this repository:

```bash
SCREENTRA_CAST_PLAYER_SOURCE=player-client \
  python3 tools/build-player-client-bundle.py
```

Do not commit signing keys, server-local CA material, databases, credentials,
client identity, or runtime uploads.

## Build the appliance ISO

The appliance ISO does not work as of now. It is currently amd64/UEFI only and
is not a supported release or installation path. Its prerequisites and
validation requirements are documented in
[`iso/README.md`](iso/README.md). Do not describe an ISO as production-ready
until it has passed independent physical boot, installation, SSH, enrollment,
and native Wayland/DRM checks.

## GitHub source and release scope

The source repository intentionally excludes machine-local and runtime state,
including:

- `target/` and other build output
- databases, caches, logs, backups, and Hermes state
- runtime uploads and downloaded release output
- local certificates, private keys, and signing material

Large binaries, static release archives, installers, and future validated ISO
artifacts belong in GitHub Releases rather than the source tree. Release assets
are versioned software artifacts, not a backup of a running server's content.

## Server release updater

The System page can check the latest stable release from GitHub and apply the
signed `screentra-cast-server-update.tar.gz` asset. The updater preserves
runtime state, validates the detached manifest signature, swaps the binary and
static tree atomically, preserves the deployment-local CA, and restarts the
configured service. A failed static/CA step rolls back automatically.

Provision the release verification public key on each server at:

```text
/etc/screentra-cast/server-update-signing.pub
```

The private signing key must remain outside the repository and outside the
running server. The release workflow expects it in the GitHub Actions secret
`SCREENTRA_CAST_SERVER_UPDATE_SIGNING_KEY`. The checkbox on the System page is
off by default; enabling it opts the server into periodic stable-release checks
and automatic installation/restart.

Optional deployment overrides are:

```text
SCREENTRA_CAST_GITHUB_REPO=owner/repository
SCREENTRA_CAST_UPDATE_ASSET=screentra-cast-server-update.tar.gz
```

## Player client installation and LAN adoption

The player client release bundle is intended for users who do not have Rust,
Python source dependencies, or a checkout of this repository. The GitHub Release
workflow publishes a signed bundle containing the generated installer, its
signature, the pinned client-update public key, and a manifest. The bundle does
not contain client identity, credentials, layouts, uploads, or runtime state.

When the installer is run without `SCREENTRA_SERVER_URL`, it sends a bounded LAN
broadcast discovery request. A Screentra Cast server replies only with its HTTPS
URL and published CA certificate. The installer validates the CA hash, asks the
user to confirm the discovered server (or select one when several are present),
and stores the CA before making HTTPS requests. Remote HTTP discovery is
rejected.

The first heartbeat registers the new client as a pending candidate. An
administrator adopts it from **Connected Clients**. Repeating the installer on
additional displays follows the same flow; no server address needs to be typed.
For unattended or routed-network installs, the explicit override remains:

```bash
SCREENTRA_SERVER_URL=https://server.example:3000 \
  sudo bash payload/install/player-client.sh
```

The release workflow signs client installers with the separate GitHub Actions
secret `SCREENTRA_CAST_CLIENT_UPDATE_SIGNING_KEY`; it must match the pinned
client public key already provisioned on existing clients.

For an existing server replacement, preserve the original credentials, client
identities, TLS trust, database content, layouts, schedules, and runtime
media through a separate backed-up migration procedure.

## Project status

The `main` branch contains the current Screentra Cast source and the published
`v0.1.4` server installation path. Debian 13 amd64 is the validated platform;
Ubuntu Server amd64 and the appliance ISO require additional independent
validation before production rollout.
