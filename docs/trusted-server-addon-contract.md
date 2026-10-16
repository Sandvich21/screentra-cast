# Trusted Server Addon Service Contract

This is the contract for a future **trusted server addon service**. It is
currently documentation-only. `trusted_server_service` manifests are rejected
by the running addon manager until the container supervisor, capability model,
credential isolation, and database boundary are implemented.

## Trust tier

A trusted server service is different from a declarative addon and a signed
client renderer:

- it contains server-side executable code;
- installation requires an explicit administrator approval;
- its image must be immutable and separately signed;
- it runs outside the Screentra server process;
- it receives only declared capabilities;
- it never receives server updater keys, addon signing private keys, TLS private
  keys, client authentication tokens, user password hashes, or session data.

This tier is not a safe path for arbitrary public uploads. A future UI must show
that installation grants execution to a third-party container.

## Manifest shape

A future manifest uses `kind=trusted_server_service` and declares:

```json
{
  "schema_version": 1,
  "id": "example.air-quality",
  "name": "Example Air Quality",
  "description": "Normalizes air-quality readings for signage widgets.",
  "version": "1.0.0",
  "kind": "trusted_server_service",
  "permissions": ["storage", "read:clients", "client_extension"],
  "trusted_service": {
    "image": "registry.example.invalid/screentra/air-quality",
    "image_digest": "sha256:<64 lowercase hex characters>",
    "capabilities": ["storage", "read:clients"],
    "storage_namespace": "example.air-quality",
    "api_namespace": "example.air-quality",
    "launch_profile": {
      "command": ["/usr/local/bin/air-quality-service"],
      "gateway_socket_path": "/run/screentra/gateway.sock",
      "rootless": true,
      "non_root": true,
      "network_disabled": true,
      "read_only_rootfs": true,
      "no_new_privileges": true,
      "drop_all_capabilities": true,
      "seccomp_default": true,
      "memory_bytes": 268435456,
      "cpu_millis": 500,
      "pids": 64,
      "writable_bytes": 16777216
    },
    "client_extension_id": "example.air-quality.client"
  }
}
```

The manifest must never contain database URLs, passwords, bearer tokens, host
paths, shell fragments, arbitrary mounts, or unrestricted upstream proxy URLs.
The `command` field is an explicit bounded executable argv, not a shell command;
its isolation and resource fields are persisted into the approval record and
must match the eventual launch specification exactly. The image digest is part
of the signed contract; mutable tags are display metadata only and are not
sufficient for installation.

## Storage boundary

The service may eventually receive a dedicated storage namespace. Its database
role must be able to create and modify only its own schema, for example
`addon_example_air_quality`. It must not modify core tables. Relationships to
core objects use stable IDs and approved read APIs or read-only views.

The service must not mount Screentra's SQLite database. A container that needs
relational persistence requires PostgreSQL or a separate service-owned database.

## Capability examples

Capabilities are explicit and allowlisted. Examples include:

- `storage` — read/write only the service's namespace;
- `read:clients` — normalized enrolled-client metadata, without credentials;
- `read:layouts` — approved layout metadata, without unrelated secrets;
- `client_extension` — publish a separately signed client package reference.

Capabilities do not imply arbitrary SQL, filesystem access, process control, or
access to raw provider credentials.

## Lifecycle gates

Before a service can start, the future supervisor must verify:

1. detached manifest signature;
2. immutable image digest and image signature;
3. administrator approval and capability review;
4. rootless runtime policy;
5. read-only filesystem and addon-only writable volume;
6. default-deny network policy;
7. bounded CPU, memory, disk, log, and restart limits;
8. dedicated database role/schema grants;
9. health check and migration policy;
10. audit record for installation and activation.

Disablement must stop the service and revoke its gateway credential. Uninstall
must remove only the service-owned storage after an explicit confirmation and a
backup/retention decision.

## Current implementation status

The core manifest model now represents this contract, but validation deliberately
rejects `trusted_server_service` with a clear not-enabled error. No container is
pulled, started, or given database access by the current addon manager.
