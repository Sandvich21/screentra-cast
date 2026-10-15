# Trusted Server Addon Operations

This document describes the current **disabled** staging boundary for trusted
server addon manifests. It does not authorize container execution.

## Current behavior

The addon manager accepts declarative manifests and signed client-extension
packages only. A manifest with `kind=trusted_server_service` is rejected before
installation, image pull, process creation, or network access.

Before returning the disabled-runtime error, the validator checks the service
metadata that would otherwise become an execution policy:

- image references are bounded, non-empty, and contain no whitespace, URL
  schemes, path traversal, absolute paths, or malformed path components;
- the image is paired with an exact lowercase `sha256:` digest containing 64
  hexadecimal characters;
- storage and API namespaces use the same conservative addon identifier rules;
- an optional client extension ID is validated as an addon identifier;
- capabilities are bounded to the documented allowlist:
  `storage`, `read:clients`, `read:layouts`, and `client_extension`.

These checks are defensive validation, not image verification. The current
server does not pull from a registry, verify an OCI signature, create a
container, mint a service credential, mount a volume, expose a gateway, or
start a process.

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
