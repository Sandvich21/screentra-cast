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

A client status report may also include bounded service observations. Service
states are report-only and currently include `staged`, `starting`, `running`,
`degraded`, `stopped`, `failed`, and `rollback_ready`; they do not start or
stop anything. The pure client-service lifecycle module and its focused tests
validate only bounded metadata and explicit state transitions; they are contract
evidence, not runtime or process-execution evidence. Restart counts and diagnostic
details are bounded, and duplicate service IDs are rejected.

The heartbeat currently sends an explicit empty service list because no trusted
client supervisor is enabled. A missing service report must not be interpreted
as a running service.

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
`disabled`, or `error` plus a bounded diagnostic detail. These observations are
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

A future active record should include the addon ID, version, package digest,
entrypoint, activation timestamp, and previous known-good record. The player
must report bounded states such as `staged`, `active`, `failed`, and
`rolled_back` only after the corresponding local transition has completed.

The current `verified` and `verified_cached` states mean verification/staging
only; they must not be renamed to `active`.

## Implementation gate

Do not add runtime loading until there is a concrete sandbox mechanism, a
renderer API test harness, a resource watchdog, atomic activation/rollback, and
an end-to-end test proving that an addon cannot access arbitrary DOM, network,
credentials, or host process capabilities. Until then, the signed-package
retrieval and status paths remain the complete executable-extension surface.
