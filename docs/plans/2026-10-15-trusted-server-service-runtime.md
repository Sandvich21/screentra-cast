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

## Phase 1 implementation status

The approval boundary was implemented in commit `7dc5ec3`. The follow-up
trait and harness are the current working-tree changes:

- `TrustedServiceRuntime` defines the future start/stop/status boundary.
- `TrustedServiceSupervisor` produces activation plans and refuses activation
  while any gate is incomplete or execution is disabled.
- `FakeTrustedServiceRuntime` records lifecycle calls and writes only disposable
  marker files; it never starts a host process or contacts a registry.
- `DisposableRuntimeHarness` creates and removes an isolated temporary runtime
  root for integration tests.
- The supervisor binds the addon identity, declared capability allowlist, API
  namespace, and storage namespace into the future launch request; unknown or
  duplicate capabilities are rejected before any runtime call.
- A disabled-by-default credential-broker trait now defines opaque lease
  metadata without exposing, persisting, or transporting credential values.

- Non-service-facing SQLite and PostgreSQL adapters now exercise the same
  controlled API through core-owned `addon_storage` methods. SQLite has a
  namespaced round-trip and persisted audit coverage; PostgreSQL has a
  disposable round-trip probe with cross-addon rejection, activation binding,
  and cleanup. Both adapters have the same typed audit lifecycle boundary, but
  neither is wired to a service runtime.
- The contract binds every future request to addon ID plus activation instance,
  permits only typed namespaced operations, caps request/value/row/key sizes,
  forbids raw SQL/core tables/migrations/SQLite mounts, and persists only
  bounded audit metadata. Audit failure returns a generic gateway error and
  prevents the operation from being reported as successful.
- PostgreSQL per-addon schema/role mode remains future work. No service gateway
  or database capability is enabled.

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

Lifecycle coverage now verifies that HTTP backup/restore preserves controlled
addon storage, persisted service audit metadata, and inert runtime records
without restoring an activation marker. Runtime coverage also rejects disabled
activation records before planning or starting and rejects repeated stops for a
stopped activation handle.

Remaining work for this phase is the final fail-closed boundary review.
Keep `scripts/probe-trusted-service-postgres-permissions.sh` in the backend
gate. Keep the database gate and execution disabled until that review is
complete. Only then should a rootless OCI backend be considered.
