# Screentra Cast

Screentra Cast is a self-hosted digital-signage platform built for physical
displays. It combines a Rust management server with Linux player clients so
teams can design, schedule, publish, and monitor signage layouts across a fleet
of screens. The platform supports reusable widgets, media, weather, news,
traffic, calendars, scheduling, remote client management, and signed client
updates.

## Repository layout

- `src/` — Rust server
- `static/` — dashboard, player, previews, installer artifacts, and shared assets
- `player-client/` — canonical player-client scripts and installer source
- `iso/` — appliance ISO build materials (WIP needs worked on also only amd64)
- `tools/` — bundle/build utilities
- `tests/` — integration and contract tests

## Install a new server

For a new server device, use a supported Debian 13 amd64 installation with
network access and `sudo`. The GitHub Release contains the compiled server,
static files, and client installers. The appliance ISO is still marked WIP and
is not part of this release until independent boot validation is complete.

1. Open the [Screentra Cast Releases](https://github.com/Sandvich21/screentra-cast/releases)
   page and note the release version.
2. Download the server installer from this repository:

   ```bash
   curl -fsSLo /tmp/screentra-cast-server-install.sh \\
     https://raw.githubusercontent.com/Sandvich21/screentra-cast/main/static/install/screentra-cast-server-install.sh
   chmod 0755 /tmp/screentra-cast-server-install.sh
   ```

3. Run it as root, selecting the matching release and the new server's address:

   ```bash
   sudo env \\
     SCREENTRA_CAST_RELEASE_BASE_URL=https://github.com/Sandvich21/screentra-cast/releases/download/v0.1.0 \\
     SCREENTRA_CAST_SERVER_HOST=<server-ip-or-dns> \\
     SCREENTRA_CAST_LOCAL_HOSTNAME=<server-ip-or-dns> \\
     bash /tmp/screentra-cast-server-install.sh
   ```

The installer installs PostgreSQL, creates the `board` service account, creates
server-local TLS certificates, installs the `screentra-cast-server.service`,
and starts the server. It does not create a default administrator password.
Open the displayed HTTPS dashboard URL and create the first administrator.
Keep the generated CA private to this server; only the published CA certificate
is distributed to clients.

The release assets are for amd64 servers. The installer does not copy runtime
state, databases, uploads, backups, certificates, or private keys from another
server.

## Install a physical display client

After the server is running, install the player client on a supported Debian
physical-display device using the signed installer supplied by the server or
release. The client must be adopted in the dashboard before assigning a layout.

## Build and test

```bash
cargo check --release
cargo test --release
python3 -B -m py_compile tools/*.py player-client/scripts/*.py iso/tests/*.py
```

The player-client bundle builder can be run only on a deployment workstation
with the client-update signing key provisioned outside this repository:

```bash
SCREENTRA_CAST_PLAYER_SOURCE=player-client \
  python3 tools/build-player-client-bundle.py
```

The player-client installer is signed separately for deployment. Signing keys and
server-local TLS/CA material are not part of this repository.

## GitHub upload scope

This source snapshot intentionally excludes runtime and machine-local data:

- build output (`target/`)
- user uploads and downloaded release output (`static/uploads/`, `static/download/`)
- local certificates and private keys (`certs/`)
- databases, caches, logs, and Hermes state

Release binaries, ISO images, and the large static download archive are
excluded from this source snapshot and should be published as GitHub Release
assets. The small installer bundles under `static/install/` remain because the
server serves them directly and their contents are source-controlled deployment
inputs.

Current source snapshot commits:

- `be1fa30` — complete Screentra branding rename
- `7238dfc` — fix signed client installer token handoff
