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
- `iso/` — appliance ISO build materials
- `tools/` — bundle/build utilities
- `tests/` — integration and contract tests

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
