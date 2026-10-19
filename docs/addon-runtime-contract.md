# Client Addon Runtime Contract (Design Only)

This document defines the boundary for a future client-extension runtime. It is
not an implementation plan that enables execution: the current player verifies
and stages signed packages, but never loads or executes them.

## Current invariant

- Server entrypoints are rejected.
- Unsigned or incorrectly hashed client packages are rejected.
- The server and player verify the detached manifest signature independently.
- Staged JavaScript is inert data under the client addon directory.
- Existing layouts continue using trusted built-in renderers.
- Disabling or deleting an addon removes it from future inventory responses.

No runtime work may weaken these invariants.

## Package and manifest boundary

The existing package shape remains:

```text
manifest.json
manifest.sig
client/<declared-entrypoint>
```

The manifest must continue to declare:

- addon ID and version;
- `kind=client_extension`;
- `client_api_version`;
- one safe `client_entrypoint`;
- the SHA-256 digest of that entrypoint;
- explicit widget registrations and bounded permissions;
- optional `client_capabilities` from the closed allowlist:
  - `render_only`;
  - `camera_capture`;
  - `microphone_capture`;
  - `local_inference`;
  - `client_service`.

A `client_service` request may include inert service metadata with a fixed OCI
image reference plus exact digest, a local socket name, bounded CPU/memory/PID/
writable-storage limits, and optional model ID plus SHA-256 digest. The metadata
contains no command, host mount, port, credential, or arbitrary environment
field. `local_inference` requires the model identity and digest. These fields
are validated and signed as part of the manifest but are not pulled, started,
or exposed by the current release.

A server-side client status snapshot may include bounded service observations. Service
states are report-only and currently include `staged`, `starting`, `running`,
`degraded`, `stopped`, `failed`, and `rollback_ready`; they do not start or
stop anything. The pure client-service lifecycle module and its focused tests
validate only bounded metadata and explicit state transitions; they are contract
evidence, not runtime or process-execution evidence. Restart counts and diagnostic
details are bounded, and duplicate service IDs are rejected. Server admission also
rejects control characters and credential-, path-, or URL-shaped detail/error
strings before persisting snapshots. The server also revalidates stored snapshots
on readback, including the client identity, closed JSON field set, bounds, exact
second-precision UTC timestamps, regular single-link file type, and private `0600`
file permissions;
tampered or legacy-shaped files fail closed instead of being returned to callers.
The status path is derived from the checked status directory returned by the data-root
validator; callers do not reconstruct it from the raw `SIGNAGE_DATA_DIR` value after
validation. The containing status directory is created and required to remain private with
mode `0700`. Capability/service detail text is limited to 160 UTF-8 bytes,
add-on/report error text to 96 UTF-8 bytes, and the client and server enforce the
same limits.

The player heartbeat has a stricter, separate report boundary in
`screentra-client-status-report-admission.py`. Its closed top-level report shape
requires `at`, `addons`, `capabilities`, and `services` (with `error` optional); while
the trusted client supervisor is disabled, `services` must be `[]`. Running, healthy,
queued, or process observations cannot be represented by the heartbeat cache. Capability
observations are limited to the closed capability/state vocabulary, bounded
control-free details, and a bounded JSON envelope. Add-on entries likewise use a
closed `id`/version/state/entrypoint/error shape with verified-state vocabulary,
relative non-empty `client/` entrypoints (without traversal, backslashes, or control
characters, bounded to 160 UTF-8 bytes), duplicate-ID rejection, bounded versions, and
credential/path/URL-safe errors. Report timestamps must use the exact second-precision
RFC3339 UTC form `YYYY-MM-DDTHH:MM:SSZ`, not offsets, fractional seconds, or
shape-only strings. The heartbeat validates every status
write through this boundary, and invalid reports are rejected without replacing
the previous cache file. The server route also applies a 256 KiB body limit before
JSON extraction, matching the report persistence bound.

The command queue also carries three metadata-only client-service actions. The
`client_service_status` action returns the bounded disabled status without
starting a process. `client_service_rollback_request` preserves the service and
activation identity plus an optional bounded reason as a control request, then
returns `execution_disabled`; it never stops, updates, or rolls back anything.
`client_service_transcribe_request` is a push-to-talk speech-to-text request
contract only: its payload is exactly `service_id`, `request_id`, and the
boolean `push_to_talk`, with bounded 128-byte ASCII identifiers and a bounded
512-byte serialized envelope. It carries no audio bytes, paths, commands,
credentials, or arbitrary fields, and the actor returns `execution_disabled`
without touching a microphone, local IPC socket, model, or process. The server
and actor use the same closed action vocabulary, bind the command identity to
the client envelope, and reject arbitrary lifecycle fields. Service start, stop,
update, and rollback actions are not allowlisted and fail closed.

The heartbeat may report `available`, `unavailable`,
or `disabled` plus a bounded diagnostic detail. These observations are
health metadata only: `available` means the local prerequisite was detected, not
that an add-on has permission to use it. The current heartbeat reports camera
and ALSA microphone device presence, while `client_service` and
`local_inference` remain disabled until the signed runtime and capability broker
are separately implemented.

`camera_capture`, `microphone_capture`, `local_inference`, and `client_service`
are contract declarations only in the current release. They do not activate a
camera, microphone, model, container, or client process by themselves. A future
capability broker must bind them to an administrator-approved client target,
resource limits, and a signed service/runtime record.

A future runtime may reject a package with additional members even if its
signature is valid. The signature authenticates the package contract; it does
not grant capabilities that the manifest does not declare.

## Proposed module boundary

When execution is eventually authorized, the entrypoint should be treated as a
module, not as a same-origin script. The only supported export should be a
registration function receiving a capability-scoped host object:

```js
export function register(host) {
  return {
    widgets: [{
      type: "example.weather",
      render: (model, surface) => { /* sandbox-provided surface only */ }
    }]
  };
}
```

The exact JavaScript syntax is still open. The following must remain forbidden:

- direct access to `window`, `document`, storage, cookies, or arbitrary DOM nodes;
- network requests unless a separately approved capability exists;
- filesystem, process, shell, WebSocket, or native-module access;
- dynamic code loading, `eval`, `Function`, blob/script injection, and import of
  undeclared modules;
- access to client authentication tokens, server URLs containing credentials,
  raw API responses, or third-party provider keys.

The host must pass normalized widget data only. Third-party credentials remain
server-side.

## Activation gates

A staged package must pass every gate before a future runtime could activate it:

1. manifest schema and API compatibility;
2. server signature and entrypoint digest verification;
3. player signature, path, archive-size, member-size, and digest verification;
4. server enablement and client-targeting policy;
5. runtime permission allowlist;
6. sandbox availability and resource limits;
7. renderer registration validation;
8. persisted activation record and rollback copy;
9. a successful isolated startup probe without exposing the display surface.

If any gate fails, the package remains staged but inactive and the client reports
`failed`. No fallback may execute it as ordinary page JavaScript.

## Resource and lifecycle limits

The future runtime must define bounded limits before implementation, including:

- maximum module bytes and registration count;
- maximum widget instances per display;
- render time budget and watchdog behavior;
- memory and network quotas, if network capability is ever approved;
- maximum log/error size;
- startup timeout and crash-loop backoff;
- package version retention for rollback.

Activation, deactivation, replacement, and rollback must be atomic from the
renderer's perspective. A failed replacement must leave the last known-good
active package intact. A disabled or removed addon must not be loaded on the
next renderer start.

## Rollback and reporting

The current server-side signed-package lifecycle has a separate
`rollback_signed_client_package(id)` path. It retains one previous, validated
package under the checked addon root at `.rollback/<id>`. Rollback re-verifies
the retained manifest signature, regular-file entrypoint, entrypoint digest,
package identity, and persisted enabled/install state before atomically swapping
the current and retained package directories. Missing, malformed, tampered, or
symlinked rollback evidence fails closed and leaves the current package in
place. This is package evidence and storage recovery only: it never loads or
executes client JavaScript, starts a service, or changes `EXECUTION_ENABLED`.
Removing a signed client-extension treats the already path-validated installed
directory as opaque administrative state: it does not parse, verify, or execute
package contents merely to delete them. It separately validates the retained
rollback path and target under the same checked addon root, clears that evidence,
and fails closed without deleting the installed package if the rollback target is
symlinked or not a directory. Removal therefore leaves no inert rollback evidence
that could be restored after a later reinstall. The declarative `update()` path
and signed `update_signed_client_package()` semantics remain separate and are not
runtime activation or rollback.

A future active record should include the addon ID, version, package digest,
entrypoint, activation timestamp, and previous known-good record. A future
runtime rollback must be a distinct renderer/runtime operation with sandbox,
watchdog, activation-state, and display-atomicity guarantees; it must not be
inferred from or substituted by the current signed-package rollback evidence.
The player must report bounded states such as `staged`, `active`, `failed`, and
`rolled_back` only after the corresponding local transition has completed.

The current `verified` and `verified_cached` states mean verification/staging
only; they must not be renamed to `active`.

## Persisted cache readback verification

Before reporting `verified_cached`, the heartbeat treats the on-disk package as
untrusted persisted state and repeats the package checks. It requires regular,
non-symlink `manifest.json`, `manifest.sig`, and the declared
`client/<entrypoint>` file, bounds the manifest and signature reads, parses the
manifest, and revalidates its schema, kind, client API, addon identity/version,
server-entrypoint absence, entrypoint digest, closed client-capability list, and
optional client-service metadata against the current server inventory. It also
checks the active record's exact identity, version, digest, and entrypoint
fields, recomputes the entrypoint digest, and verifies the detached manifest
signature with the configured addon public key.

A readback failure reports the same bounded generic addon-cache error, leaves the
persisted cache in place for diagnosis or recovery, and does not load or execute
JavaScript. This readback gate is deliberately separate from any future runtime
activation gate: a package can be readback-verified and staged while remaining
inert and unavailable to a renderer runtime.

## Implementation gate

Do not add runtime loading until there is a concrete sandbox mechanism, a
renderer API test harness, a resource watchdog, atomic activation/rollback, and
an end-to-end test proving that an addon cannot access arbitrary DOM, network,
credentials, or host process capabilities. Until then, the signed-package
retrieval and status paths remain the complete executable-extension surface.
