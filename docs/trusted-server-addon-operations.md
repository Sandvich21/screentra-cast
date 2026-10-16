# Trusted Server Addon Operations

The current server also has a deliberately inert staging boundary for a
separately signed service manifest and image digest authorization. An
administrator must submit multipart fields `manifest`, `signature`, and
`image_signature` to `POST /api/addons/trusted-service/stage`. The manifest
signature is checked with the separate trusted-service public key at
`/etc/screentra-cast/trusted-service-signing.pub` or the
`SCREENTRA_CAST_TRUSTED_SERVICE_SIGNING_PUBKEY` override. The image signature
covers the exact `image_digest` string and is checked with the separate image
signing public key at `/etc/screentra-cast/trusted-service-image-signing.pub`
or the `SCREENTRA_CAST_TRUSTED_SERVICE_IMAGE_SIGNING_PUBKEY` override. A valid
submission is stored as `data/trusted-services/<addon-id>/` with state
`staged`; it is not added to the ordinary addon registry, enabled, pulled, or
executed.

The staged records can be inspected or removed by an administrator with
`GET /api/addons/trusted-service/staged` and
`DELETE /api/addons/trusted-service/staged/<addon-id>`. Successful stage and
removal operations are recorded as `trusted_service_stage_create` and
`trusted_service_stage_remove` audit events. Removal only deletes the inert
staged manifest and signature; it cannot stop or delete a running service
because no service can currently run.

The listing path re-verifies each staged manifest and its `image.sig` against
the trusted-service and image public keys before returning metadata. A tampered
or malformed staged directory fails closed instead of being reported as an
active service.

## Phase 1 runtime records

The first runtime phase adds a separate, inert record namespace at
`data/trusted-service-runtime/<addon-id>/runtime.json`. An administrator may
approve a currently staged service with
`POST /api/addons/trusted-service/staged/<addon-id>/approve`, inspect approval
records with `GET /api/addons/trusted-service/runtime`, and disable an approval
with `POST /api/addons/trusted-service/runtime/<addon-id>/disable`.

Approval is not activation. The record stores the signed manifest identity,
admin approver, activation instance, declared capabilities, and explicit
execution gates. The execution backend, credential broker, database boundary,
controlled gateway, and resource policy gates remain false. `execution_enabled`
is hard-coded false, and these endpoints do not pull images, create containers,
issue credentials, expose ports, or execute code. The credential-broker
interface exists separately but its default implementation rejects issuance and
revocation. Removing staged metadata also removes its inert runtime record.

The validator checks the service metadata that would otherwise become an
execution policy:

- image references are bounded, non-empty, and contain no whitespace, URL
  schemes, path traversal, absolute paths, or malformed path components;
- the image is paired with an exact lowercase `sha256:` digest containing 64
  hexadecimal characters;
- storage and API namespaces use the same conservative addon identifier rules;
- an optional client extension ID is validated as an addon identifier;
- capabilities are bounded to the documented allowlist:
  `storage`, `read:clients`, `read:layouts`, and `client_extension`.

The first rootless-OCI phase now has a contract-only validator in
`src/trusted_service_oci.rs`. It accepts only an exact lowercase image digest,
activation-bound namespaces, allowlisted capabilities, shell-free argv, no
environment or host mounts, no published ports, non-root UID, default seccomp,
read-only/no-new-privileges/default-deny-network policy, and bounded memory,
CPU, PID, and writable scratch limits. It does not
select a runtime, invoke a process, pull an image, or change the disabled
execution gate.

The launch contract also has an approval-binding validator. It requires the
approved state, approver, every named activation gate, exact record identity,
image, digest, capabilities, and namespaces before a launch spec can be used;
with the current `EXECUTION_ENABLED = false` and incomplete gates it always
rejects.

A runtime preflight report also exists for the explicitly selected executable.
It accepts only the named rootless runtime families (`podman` or `nerdctl`),
reports missing or non-executable paths as blocked, and always keeps
`can_start` false while OCI capability probing is disabled. It does not search
PATH, invoke a command, fall back to a host process, or contact a registry.

A contract-only gateway transport is defined in
`src/trusted_service_gateway.rs`. It uses bounded JSON frames, binds every
request to addon and activation identities, accepts only the `storage`
capability, and converts only the typed storage operations into the existing
database boundary. The OCI contract permits only the container-side endpoint
`/run/screentra/gateway.sock`; the host-side socket mapping and listener are
not implemented. It has no listener, Unix socket, host port, credential
transport, or runtime caller; decoding validates before any backend adapter
could be reached.
Gateway requests are now bound to an explicit activation state. Only `Active`
bindings may decode a request; stopped, disabled, and replaced activations are
rejected before parsing can reach a backend adapter. The binding still carries
the validated addon/activation database contract, so identity and lifecycle
checks cannot be omitted by a future socket loop.

Response encoding is bounded to 64 KiB, the contract-only operation budget
allows at most 100 operations per future connection, and the activation
rate limiter allows 60 requests per 60-second window with bounded tracked
activation state. These are pure limits; there is still no socket listener or
production request loop.

A fixed-argument builder is also present for the future supervisor. It emits
only the selected supported runtime, `run`, immutable `image@digest`,
`--network=none`, read-only root filesystem, dropped capabilities,
no-new-privileges, bounded memory/CPU/PIDs, a deterministic container name,
and the validated service argv. The builder returns an argument vector and
never invokes it; gateway mounts, credentials, image pulls, and process
execution remain unavailable.


## Required future activation gates

A future implementation must add all of the following before changing the
validator to accept the service kind:

1. separately signed manifest verification;
2. immutable, digest-pinned OCI image verification and trusted registry policy;
3. explicit administrator approval and capability review;
4. rootless supervisor and dedicated service identity;
5. read-only filesystem plus addon-only writable storage;
6. default-deny network policy and no arbitrary host ports;
7. bounded CPU, memory, disk, log, timeout, and restart budgets;
8. service-specific PostgreSQL role/schema or the controlled core storage API;
9. short-lived gateway credentials with revocation and audit events;
10. health checks, rollback, uninstall cleanup, backup/retention behavior, and
    tests proving protected files, credentials, core tables, and other addon
    namespaces are inaccessible.

Until every gate exists, an apparently valid image reference remains metadata
only. A successful manifest parse must never cause a registry request or code
execution.

The SQLite side has a non-service-facing adapter in
`src/trusted_service_database.rs`. It calls only core-owned namespaced storage
methods. PostgreSQL has the same controlled adapter over the server-owned
`PostgresStore`; the configured database URL never leaves the server boundary.
The disposable SQLite and PostgreSQL tests cover round trips, cross-addon and
activation-identity rejection, and cleanup. Neither adapter is wired to a
runtime, HTTP route, container, credential, or service gateway; the
`database_boundary` activation gate remains false. The per-addon PostgreSQL
schema/role mode is still rejected until its separate grants and permission
probe are implemented.

## Backup and restore scope

The filesystem archive path is currently a **SQLite-only restore/upload** path.
When the server uses SQLite, it captures the complete configured data directory,
excluding only the backup tarball directory itself. Therefore namespaced
`addon_storage` rows are preserved inside `data/signage.db`, while inert staged
trusted-service metadata is preserved under `data/trusted-services/`.
Dry-run inspection reports whether `data/signage.db`, `data/postgresql.sql`, and
`data/trusted-services/` are present and warns when either database scope is
absent. Restoring either scope does not activate a service, pull an image, or
execute addon code.

When the PostgreSQL repository backend is selected, backup creation uses a
`pg_dump` plain SQL dump stored in the archive as `data/postgresql.sql`. The dump
is generated with credentials supplied only through the child-process
environment and is removed after archive creation. PostgreSQL uploads are accepted
only when that exact dump member is present and passes native dump validation;
filesystem-only archives are rejected with HTTP `400`. Confirmed PostgreSQL
restore uses the validated dump, creates a pre-restore rollback dump, and restores
filesystem state and database state with rollback attempts on failure. The server
must not present a filesystem-only archive as a complete PostgreSQL backup.
