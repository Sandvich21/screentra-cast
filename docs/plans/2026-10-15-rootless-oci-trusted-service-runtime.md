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

**Status:** Contract-only implementation complete in `src/trusted_service_oci.rs`; execution remains disabled.

**Files:**
- Modify: `src/trusted_service_runtime.rs`
- Test: `src/trusted_service_runtime.rs`
- Modify: `docs/trusted-server-addon-operations.md`

Write failing tests for a launch specification that contains only validated immutable fields: addon ID, activation instance, image digest, capabilities, namespaces, rootless policy, network policy, resource limits, and controlled gateway endpoint. Reject mutable tags, non-lowercase digests, host paths, host ports, shell fragments, duplicate capabilities, and disabled/stopped identities before any process boundary is called.

Keep the contract independent of any installed OCI binary. The contract must remain inspectable in tests without starting processes.

Verify with focused runtime tests, then commit the contract separately.

## Task 2: Add an explicit runtime capability probe

**Status:** Blocked preflight implementation complete; no runtime executable is installed and no capability command is invoked.

**Files:**
- Create: `src/trusted_service_oci.rs`
- Modify: `src/main.rs`
- Test: `src/trusted_service_oci.rs`
- Modify: `docs/plans/2026-10-15-rootless-oci-trusted-service-runtime.md`

Write failing tests for runtime discovery and policy validation. Configuration must name one supported rootless runtime explicitly; never search arbitrary PATH entries and never invoke a shell. Probe version, rootless operation, user-namespace support, cgroup/resource support, network isolation, read-only rootfs support, and seccomp/no-new-privileges support.

A missing executable or failed probe returns a blocked gate, not a fallback to host-process execution. Keep production wiring disabled and return a structured preflight report.

Verify with the host’s current absence of OCI tools: the probe must report blocked without attempting a host process or registry access.

## Task 3: Implement fixed-argument, no-network launch preparation

**Status:** Dry-run fixed-argument builder complete; it returns argv only and cannot start or pull anything.

**Files:**
- Modify: `src/trusted_service_oci.rs`
- Modify: `src/trusted_service_runtime.rs`
- Test: `src/trusted_service_oci.rs`

Write failing tests proving the generated invocation uses an argument vector, never `sh -c`, never interpolates untrusted values into a command string, never publishes ports, and never mounts the core database or arbitrary host paths. The image argument must use the exact verified digest.

Build only a dry-run launch description until the runtime probe is available. Do not start a process, pull an image, or contact a registry in unit tests.

## Task 4: Add isolated storage and gateway boundaries

**Status:** Bounded typed gateway frame contract is complete; Unix listener, authentication transport, rate limiting, and runtime wiring remain disabled.

**Files:**
- Create: `src/trusted_service_gateway.rs`
- Modify: `src/trusted_service_database.rs`
- Modify: `src/trusted_service_credentials.rs`
- Test: `src/trusted_service_gateway.rs`
- Modify: `docs/trusted-server-service-gateway.md`

Define an authenticated Unix-domain gateway with bounded request size, request timeout, response size, operation count, and per-activation rate limits. Route only typed storage, approved read-only metadata, and future client-extension operations. Bind every request to addon ID plus activation instance and reject stopped, disabled, replaced, cross-namespace, unknown-capability, raw-SQL-shaped, and malformed requests.

Keep SQLite API-only and keep the current PostgreSQL controlled adapter non-service-facing until the gateway has complete audit and identity tests. Credential requests remain blocked until an opaque lease transport and revocation lifecycle are separately reviewed.

## Task 5: Add resource, identity, and failure lifecycle coverage

**Files:**
- Modify: `src/trusted_service_oci.rs`
- Modify: `src/trusted_service_runtime.rs`
- Test: `src/trusted_service_oci.rs`
- Test: `src/trusted_service_runtime.rs`

Cover startup, readiness timeout, clean stop, crash, repeated stop, activation replacement, disabled records, gateway timeout, credential denial, audit failure, and database-boundary failure. Verify no success is returned when completion audit persistence fails. Verify cleanup removes only activation-owned temporary state and never another addon’s namespace.

Use a disposable fixture and a real rootless OCI runtime only when one is explicitly installed and the test environment opts in. A missing runtime must produce a skipped/blocked integration result, never a simulated success.

## Task 6: Add signed-image and registry transport policy

**Files:**
- Modify: `src/addons.rs`
- Modify: `src/trusted_service_oci.rs`
- Test: `src/addons.rs`
- Test: `src/trusted_service_oci.rs`
- Modify: `docs/trusted-server-addon-operations.md`

Require detached image-signature verification over the exact digest before launch. Registry credentials, if ever supported, remain server-resident and are provided through a narrowly scoped broker; they are never written to arguments, logs, archives, environment snapshots, or container-visible files. Pull policy must reject mutable tags, unexpected registries, digest changes, and unsigned images.

No image pull is implemented until signature verification, registry allowlisting, credential handling, and cleanup tests exist.

## Task 7: Gate activation and perform the boundary review

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
