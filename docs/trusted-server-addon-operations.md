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

These checks are defensive validation plus detached image-digest signature
verification, not OCI image verification. The current server does not pull
from a registry, verify an OCI signature, create a container, mint a service
credential, mount a volume, expose a gateway, or start a process.

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

## Backup and restore scope

The filesystem backup archive is currently a **SQLite-only** path. When the
server uses SQLite, it captures the complete configured data directory,
excluding only the backup tarball directory itself. Therefore namespaced
`addon_storage` rows are preserved inside `data/signage.db`, while inert staged
trusted-service metadata is preserved under `data/trusted-services/`. Restore
dry-runs report whether `data/signage.db` and `data/trusted-services/` are
present and warn when either scope is absent. Restoring either scope does not
activate a service, pull an image, or execute addon code.

When the PostgreSQL repository backend is selected, create/upload/restore
filesystem backup operations fail closed with HTTP `501 Not Implemented`
until a database-native dump/restore path exists. The server must not present a
filesystem archive as a complete PostgreSQL backup.
