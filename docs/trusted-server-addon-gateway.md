# Trusted Server Addon Gateway Contract

This document defines the future server-owned gateway between a trusted addon
service and Screentra. It is a design contract only. The current server does
not expose this gateway, issue service credentials, start containers, or accept
`trusted_server_service` manifests.

## Boundary

A running service is identified by the installed addon ID and an activation
instance. The gateway derives the addon namespace from the authenticated
service credential; a request cannot choose another addon ID or storage
namespace in its body or query string.

The future route family is:

```text
/api/addon-services/{addon_id}/...
```

The route is internal to the service network or a server-owned Unix socket. It
must not be published as an arbitrary host port. A reverse proxy or gateway
process owns the public listener and forwards only the allowlisted operations.

## Service identity

Each active service receives a short-lived credential bound to:

- addon ID and installed version;
- activation instance;
- declared API namespace;
- declared capabilities;
- credential expiry as bounded RFC3339 metadata, validated against the current
  time before use;
- credential revocation state.

Credentials are minted and revoked only by the future supervisor. They are not
browser sessions, client authentication tokens, updater keys, addon signing
private keys, TLS private keys, or database credentials. Credential values are
never placed in manifests, logs, client payloads, backups, or error responses.

Disablement revokes the credential before or while stopping the service.
Restart does not broaden capabilities or change the bound namespace.

The current contract-only peer policy separately requires an exact configured
non-root UID/GID/PID tuple and rejects root, zero-PID, or mismatched
credentials. The PID is expected to come from the OS peer-credential record for
that activation's transport connection; it must be rebound after a restart.
The policy defines the validation rule only; no Unix socket listener or OS
peer-credential acquisition exists yet.

## Capability routing

Every operation is mapped to an explicit capability. Unknown operations and
unknown capabilities fail closed. The initial allowlist is intentionally small:

- `storage` — get, set, and delete bounded JSON records in the service's own
  core-owned namespace;
- `read:clients` — normalized enrolled-client metadata, excluding credentials,
  tokens, addresses that are not needed by the feature, and session data;
- `read:layouts` — approved layout metadata, excluding unrelated core data;
- `client_extension` — reference a separately signed client package already
  accepted by the client-extension delivery path.

Capabilities do not grant raw SQL, table creation in core schemas, filesystem
access, process execution, arbitrary HTTP proxying, secret retrieval, or access
to other addon namespaces.

## Request and response rules

The gateway must enforce all of the following before dispatching an operation:

1. authenticate and validate the service credential;
2. validate the OS-level Unix peer credentials against the exact configured
   non-root UID/GID policy;
3. verify the activation is enabled and not revoked;
4. match the requested operation to the manifest capability;
5. derive and validate the addon namespace from the credential;
6. enforce bounded request body, query, and response sizes;
7. apply an operation timeout and cancellation on disconnect;
8. reject unknown fields where the operation has a fixed schema;
9. redact credentials and secret-bearing fields from logs;
10. return stable error classes without backend, filesystem, or SQL details.

Payloads use JSON with explicit versioning. A future envelope may use this
shape:

```json
{
  "api_version": "1",
  "request_id": "server-generated-id",
  "operation": "storage.get",
  "payload": {}
}
```

Responses must include the same API version and request ID. They must not
contain raw database rows, SQL text, connection strings, internal paths, or
service credentials. Event delivery is at-least-once; consumers must treat
`request_id` as an idempotency key where the operation can mutate state.

## Storage operations

Storage operations use the core-owned namespaced storage contract already
implemented for SQLite and PostgreSQL. The service identity supplies the
namespace; the service supplies only a bounded key and JSON value.

The gateway must preserve these invariants:

- key maximum: 128 bytes;
- JSON value maximum: 64 KiB;
- no traversal, control characters, or cross-namespace access;
- upsert and delete results are explicit;
- backend errors are logged server-side and represented as generic internal
  failures to the service;
- storage cleanup is limited to the service's namespace and follows the
  uninstall/retention policy.

The service never receives the SQLite database path and never connects to core
PostgreSQL with a broad role. A future PostgreSQL-backed service uses a
service-specific role and schema, with grants reviewed against its declared
capabilities.

## Normalized core data

Read capabilities return deliberately small DTOs rather than ORM/database
models. DTOs must have field allowlists and bounded result counts. The gateway
must not expose:

- user password hashes or sessions;
- client authentication tokens;
- signing or updater keys;
- TLS private keys or certificate authorities;
- raw provider credentials;
- backup file paths or archive contents;
- unrelated addon storage;
- unrestricted core-table columns.

The service cannot use the gateway as a general-purpose network proxy. Any
external provider access must be declared, separately policy-checked, and
performed by a future server-owned egress component with bounded URLs and
credentials isolated from the service.

## Failure and audit behavior

The supervisor and gateway must record bounded audit events for installation,
activation, credential issuance/revocation, denied capability requests,
service errors, disablement, and uninstall. Audit details must not contain
credential values or full request bodies.

Suggested external error classes are:

- `unauthorized` — missing, expired, malformed, or revoked credential;
- `forbidden` — capability or namespace is not allowed;
- `invalid_request` — schema or size validation failed;
- `not_found` — requested namespaced object does not exist;
- `rate_limited` — service or operation budget exceeded;
- `unavailable` — service or gateway is unavailable;
- `internal` — backend failure, with details retained only in server logs.

Retries are safe only for operations documented as idempotent. Gateway request
IDs and storage upsert semantics provide the basis for retry-safe writes.

## Activation gates

No gateway implementation should be enabled until the following exist and are
covered by tests:

- separately signed manifest and immutable, signed image verification;
- explicit administrator approval and capability review;
- rootless supervisor with read-only filesystem and addon-only writable volume;
- default-deny network policy and bounded resources;
- credential minting, rotation, revocation, and audit;
- SQLite exclusion and PostgreSQL role/schema isolation;
- request/response limits, timeouts, rate limits, and health checks;
- backup, uninstall, rollback, and retention behavior;
- tests proving a service cannot read protected files, core credentials, or
  unrelated database namespaces.

Until those gates are implemented, the validator must continue rejecting
`trusted_server_service` manifests and no gateway route should be registered.
