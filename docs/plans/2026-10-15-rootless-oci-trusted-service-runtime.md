# Rootless OCI Trusted-Service Runtime Implementation Plan

> **For Hermes:** Implement this plan task-by-task with fail-closed verification.

**Goal:** Add a separately reviewed rootless OCI execution tier for explicitly approved trusted server services without weakening the existing disabled runtime boundary.

**Architecture:** The current inert runtime records, activation-instance identity, capability binding, disabled credential broker, and controlled database adapters remain the policy boundary. A future OCI adapter must receive only an immutable activation plan, launch an exact digest-pinned image through a fixed-argument rootless runtime invocation, and expose no host port or unrestricted database handle. `EXECUTION_ENABLED` remains false until every gate, runtime probe, gateway, credential, resource-policy, and rollback criterion passes.

**Tech Stack:** Rust, Tokio child-process supervision, a rootless OCI runtime selected by explicit configuration, detached image-signature verification, Unix-domain gateway transport, typed storage/database operations, and disposable integration fixtures.

---

## Current prerequisite state

The development host has no `podman`, `docker`, `nerdctl`, `crun`, `runc`, or `buildah` executable available. No task in this plan may substitute a host process, shell command, or fake marker for OCI execution. The existing `FakeTrustedServiceRuntime` remains test-only and must not be promoted to production execution.

## Non-negotiable gates

- `EXECUTION_ENABLED` remains `false` until all acceptance criteria pass.
- The ordinary addon registry and declarative install path never execute server code.
- Only an approved runtime record may produce a launch request.
- The launch request binds the exact addon ID, activation-instance ID, image reference, lowercase `sha256:` digest, capability set, and validated namespaces.
- No server updater key, addon signing private key, TLS private key, client token, password hash, session data, database URL, database password, SQLite path, Unix database socket, raw SQL channel, or unrestricted connection handle enters the container.
- No host port is published. Gateway transport is controlled and authenticated; network egress is denied by default.
- The image is immutable, digest-pinned, and separately signed from addon and updater artifacts.
- The container uses a non-root user, read-only root filesystem, bounded writable scratch space, dropped capabilities, `no-new-privileges`, resource limits, and an explicit seccomp/cgroup policy.
- Stop, timeout, crash, gateway failure, credential failure, and database-boundary failure are fail-closed and auditable.
- Backup, restore, uninstall, and rollback never start or pull a service image.

---

## Task 1: Define the OCI launch contract

**Status:** Contract-only implementation complete in `src/trusted_service_oci.rs` and `src/trusted_service_runtime.rs`, including approval-record binding, propagation of the complete profile into the immutable runtime launch request, and construction restricted to validated approved records; execution remains disabled.

**Files:**
- Modify: `src/trusted_service_runtime.rs`
- Test: `src/trusted_service_runtime.rs`
- Modify: `docs/trusted-server-addon-operations.md`

Write failing tests for a launch specification that contains only validated immutable fields: addon ID, activation instance, image digest, capabilities, namespaces, rootless policy, network policy, resource limits, and controlled gateway endpoint. Reject mutable tags, non-lowercase digests, host paths, host ports, shell fragments, duplicate capabilities, and disabled/stopped identities before any process boundary is called.

Keep the contract independent of any installed OCI binary. The contract must remain inspectable in tests without starting processes.

Verify with focused runtime tests, then commit the contract separately.

## Task 2: Add an explicit runtime capability probe

**Status:** Preflight implementation complete; the dedicated test host has a verified runtime, but the application invokes no capability command and remains execution-disabled. The pure path-bound argv helper now accepts only an explicitly validated absolute runtime executable and preserves that exact path in the generated vector; it still performs no process execution.

**Files:**
- Create: `src/trusted_service_oci.rs`
- Modify: `src/main.rs`
- Test: `src/trusted_service_oci.rs`
- Modify: `docs/plans/2026-10-15-rootless-oci-trusted-service-runtime.md`

Write failing tests for runtime discovery and policy validation. Configuration must name one supported rootless runtime explicitly; never search arbitrary PATH entries and never invoke a shell. Probe version, rootless operation, user-namespace support, cgroup/resource support, network isolation, read-only rootfs support, and seccomp/no-new-privileges support. The configured executable path must be absolute, regular, non-symlink, and free of symlinked ancestor directories.

A missing executable or failed probe returns a blocked gate, not a fallback to host-process execution. Keep production wiring disabled and return a structured preflight report.

Verify with the host’s current absence of OCI tools: the probe must report blocked without attempting a host process or registry access.

## Task 3: Implement fixed-argument, no-network launch preparation

**Status:** Dry-run fixed-argument builder complete; it returns argv only and cannot start or pull anything. The vector includes non-root UID, default seccomp, bounded writable scratch via a fixed tmpfs path, and explicit `--pull=never` so a future launcher cannot implicitly fetch an image before separate signature/registry policy approval.

**Files:**
- Modify: `src/trusted_service_oci.rs`
- Modify: `src/trusted_service_runtime.rs`
- Test: `src/trusted_service_oci.rs`

Write failing tests proving the generated invocation uses an argument vector, never `sh -c`, never interpolates untrusted values into a command string, never publishes ports, and never mounts the core database or arbitrary host paths. The image argument must use the exact verified digest.

Build only a dry-run launch description until the runtime probe is available. Do not start a process, pull an image, or contact a registry in unit tests.

## Task 4: Add isolated storage and gateway boundaries

**Status:** Typed gateway framing and admission are complete, and a Unix transport adapter is now implemented behind the disabled execution gate. `TrustedServiceUnixGateway` uses the fixed socket path, validates the real parent/socket, acquires Linux `SO_PEERCRED` before decoding the hello, enforces the typed session admission state, bounds frames, active connections, and timeout budgets, and exposes readiness only while at least one active session has completed a valid peer-checked connection hello, resetting that state on listener shutdown or cancellation. It returns before binding when `EXECUTION_ENABLED` is false, and the gated listener has an explicit shutdown path that aborts active connection tasks and removes only the socket inode it created. Typed SQLite/PostgreSQL handler adapters preserve the audit-bearing database boundary, and a bounded metadata-only credential lease ledger now owns registration, expiry validation, and revocation state without storing or issuing secrets. Production selection, credential transport, and live socket verification remain disabled.

**Files:**
- Create: `src/trusted_service_gateway.rs`
- Modify: `src/trusted_service_database.rs`
- Modify: `src/trusted_service_credentials.rs`
- Test: `src/trusted_service_gateway.rs`
- Modify: `docs/trusted-server-service-gateway.md`

Define an authenticated Unix-domain gateway with bounded request size, request timeout, response size, operation count, and per-activation rate limits. Route only typed storage, approved read-only metadata, and future client-extension operations. Bind every request to addon ID plus activation instance and reject stopped, disabled, replaced, cross-namespace, unknown-capability, raw-SQL-shaped, and malformed requests.

Keep SQLite API-only and keep the current PostgreSQL controlled adapter non-service-facing until the gateway has complete audit and identity tests. Credential requests remain blocked until an opaque lease transport and revocation lifecycle are separately reviewed.

## Task 5: Add resource, identity, and failure lifecycle coverage

**Status:** Partial real-supervisor implementation is now present behind the disabled activation gate. `RootlessOciRuntime` accepts only an explicit validated runtime path and an allowlisted registry policy, derives the OCI spec from the private approved-record launch request, validates the exact immutable image digest and registry before spawning, uses the fixed `--pull=never` argv, keeps child stdout/stderr detached, exposes status/stop behavior, reaps crashed children through `supervise()`, and provides two-phase replacement rollback in `TrustedServiceSupervisor::replace()`. The typed Unix gateway, database-handler adapters, and bounded deterministic readiness-deadline contract now compile and are tested, but no production constructor, readiness handshake, credential path, database activation, or real container launch is enabled. Runtime readiness observation, resource enforcement evidence, and real-runtime integration remain pending.

**Files:**
- Modify: `src/trusted_service_oci.rs`
- Modify: `src/trusted_service_runtime.rs`
- Test: `src/trusted_service_oci.rs`
- Test: `src/trusted_service_runtime.rs`

Cover startup, readiness timeout, clean stop, crash, repeated stop, activation replacement, disabled records, gateway timeout, credential denial, audit failure, and database-boundary failure. Verify no success is returned when completion audit persistence fails. Verify cleanup removes only activation-owned temporary state and never another addon’s namespace.

Use a disposable fixture and a real rootless OCI runtime only when one is explicitly installed and the test environment opts in. A missing runtime must produce a skipped/blocked integration result, never a simulated success.

## Task 6: Add signed-image and registry transport policy

**Status:** Contract-only registry allowlist validation is complete. It rejects unqualified images, non-canonical registry entries, aliases, and traversal paths; trusted-service upload staging now creates an exclusive mode-`0700` directory and exclusive mode-`0600` regular files under a validated parent. No image pull, registry connection, credential handling, or transport is enabled.

**Files:**
- Modify: `src/addons.rs`
- Modify: `src/trusted_service_oci.rs`
- Test: `src/addons.rs`
- Test: `src/trusted_service_oci.rs`
- Modify: `docs/trusted-server-addon-operations.md`

Require detached image-signature verification over the exact digest before launch. Registry credentials, if ever supported, remain server-resident and are provided through a narrowly scoped broker; they are never written to arguments, logs, archives, environment snapshots, or container-visible files. Pull policy must reject mutable tags, unexpected registries, digest changes, and unsigned images.

No image pull is implemented until signature verification, registry allowlisting, credential handling, and cleanup tests exist.

## Task 7: Gate activation and perform the boundary review

**Status:** Boundary review remains closed, with the real process adapter and gated Unix transport now implemented but still unreachable from activation because the gates remain disabled. Contract validation, bounded dry-run argv construction, registry policy, gateway/session types, typed database adapters, lifecycle records, explicit-path OCI spawning code, crash reaping, and replacement rollback are present. Image-signature transport, credential broker, readiness/timeout enforcement, resource evidence, audit wiring into production selection, and production activation remain incomplete. `EXECUTION_ENABLED` and the `database_boundary` gate remain disabled.

**Boundary review checkpoint:** The current source review found no trusted-service activation route, no production constructor or runtime selection, no active Unix gateway listener, credential transport, or database-boundary activation path. The `RootlessOciRuntime` adapter contains the reviewed, gate-first process-spawn path, but `activation_plan()` still blocks on the disabled execution gate and incomplete required gates, so that path is unreachable from production activation. The approval endpoint only creates a persisted record. Backup/restore and addon removal paths perform inert metadata/storage cleanup and do not call runtime activation. This is source-level evidence only; it does not make the pending OCI gates pass.

**Files:**
- Modify: `src/trusted_service_runtime.rs`
- Modify: `docs/plans/2026-10-15-rootless-oci-trusted-service-runtime.md`
- Modify: `README.md`

Only after Tasks 1–6 pass should the activation gate be reconsidered. The review must confirm that no route, approval operation, backup/restore path, uninstall path, or ordinary addon path can activate execution accidentally. Keep `EXECUTION_ENABLED` false if any runtime, gateway, credential, database, resource, audit, or lifecycle criterion is incomplete.

Required verification:

```bash
RUST_TEST_THREADS=1 cargo test --locked
cargo check --locked
cargo build --release --locked
git diff --check
scripts/probe-trusted-service-postgres-permissions.sh
git status --short --branch
```

A real runtime test must report its runtime version, exact image digest, isolated fixture root, and cleanup result without exposing credentials or connection strings. No deployment or push is part of this plan.

## Task 8: Operational rollout and test-box verification

**Status:** Dedicated test host `11.1.1.26` is prepared and passed a read-only preflight: hostname `signary-client`, Debian 13 amd64, user `tech` UID/GID `1000:1000`, `/usr/bin/podman` `5.4.2`, OCI runtime `crun`, rootless mode, cgroup v2, subuid/subgid ranges `100000:65536`, user runtime directory `/run/user/1000`, non-interactive sudo available, and approximately 27 GB free on the 30 GB root filesystem. No trusted-service image has been pulled and no container has been started. Rollout remains blocked until Task 7's execution, gateway, credential, database, resource, audit, lifecycle, and rollback gates pass. Do not enable execution or deploy a trusted service as part of contract-only work.

When Task 7 is complete, rollout must proceed in this order:

1. Prepare a dedicated disposable test environment and record the exact runtime executable path, runtime version, image reference, image digest, approval record, isolated fixture root, and cleanup result. Never record credentials, connection strings, or secret values.
2. Build the release artifact with the locked toolchain, record its digest, and verify the target artifact matches before installation. Preserve a rollback backup before any state-changing deployment.
3. Run one explicitly approved trusted service on the dedicated test box only. Verify the Unix gateway socket, peer identity, typed storage operations, credential denial/expiry/revocation, resource limits, crash/timeout handling, replacement, rollback, and cleanup.
4. Confirm backup, restore, uninstall, and server restart do not activate the service, pull an image, issue credentials, or expose a gateway unexpectedly.
5. Perform physical/visual confirmation on test client `11.1.1.30` where the change affects client-visible behavior. Treat source tests and server health as insufficient evidence for display behavior.
6. Record service health, active listeners, runtime state, audit events, image digest, artifact digest, and rollback result. Keep `EXECUTION_ENABLED` and `database_boundary` disabled if any result is missing or ambiguous.
7. Promote only after the dedicated test-box evidence is complete and independently reviewed. No fleet rollout or push is part of the contract-only phase.
