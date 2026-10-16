# Trusted Server-Service Runtime Phase 1 Implementation Plan

> **For Hermes:** Implement this plan task-by-task with fail-closed verification.

**Goal:** Add the first non-executing supervisor boundary for explicitly approved trusted server services without pulling images, starting containers, exposing a gateway, or granting database access.

**Architecture:** Trusted-service staging remains signed metadata only. A separate runtime-record store records administrator approval and lifecycle intent. The supervisor emits an activation plan whose execution gates remain false until image/runtime, credential, database, gateway, and resource-policy components are implemented. The records are not ordinary addon records and are never treated as running services.

**Tech Stack:** Rust, serde JSON, Axum, filesystem atomic replacement, existing detached-signature staging and audit paths.

---

## Phase 1 scope

- Add a separate persistent runtime-record namespace under `SIGNAGE_DATA_DIR/trusted-service-runtime/`.
- Add explicit admin approval and disable/revoke operations.
- Persist activation gates and an activation instance ID.
- Re-verify staged metadata before approval.
- Keep `execution_enabled: false` unconditionally.
- Add authenticated admin-only HTTP endpoints and audit events.
- Clean runtime records when staged metadata is removed.
- Add unit and HTTP coverage proving approval cannot execute, pull, or expose anything.

## Explicitly out of scope

- OCI registry access or image pulls.
- Container creation, process execution, supervisor daemon, or host ports.
- Service credentials or credential broker.
- Gateway implementation.
- SQLite mounts, unrestricted PostgreSQL access, or raw SQL APIs.
- Client extension activation.

## Acceptance criteria

1. Approval succeeds only for a currently staged, valid, separately signed service.
2. Runtime records are stored outside the ordinary addon registry.
3. Approval records contain no credential values or database connection strings.
4. Every execution gate is represented and runtime execution remains disabled.
5. Disable/revoke is idempotent and never attempts to stop a process.
6. Removing staged metadata also removes its inert runtime record.
7. Tampered or missing staged metadata fails closed.
8. Full Rust tests, check, build, and diff validation pass.

## Next phase after this plan

Implement the fake supervisor/runtime trait and disposable integration harness. Only after that should a real rootless OCI backend be considered.
