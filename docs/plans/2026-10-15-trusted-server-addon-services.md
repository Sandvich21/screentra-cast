# Trusted Server Addon Services Implementation Plan

> **For Hermes:** Execute this plan in small vertical slices. Keep ordinary declarative addons and trusted server services on separate capability paths.

**Goal:** Let open-source addon developers build substantial server/client features without granting ordinary addons unrestricted access to Screentra or its database.

**Architecture:** Keep the current declarative and signed-inert client-extension paths unchanged. Add a separate, explicitly operator-approved trusted-service path for signed container images. Give each service a namespaced storage boundary and controlled APIs; never grant direct access to core credentials or unrestricted core-table writes.

**Tech Stack:** Rust/Axum, SQLite and PostgreSQL backends, Docker/OCI service metadata, detached addon signatures, existing client-token authorization, integration tests with disposable PostgreSQL when available.

---

### Task 1: Define the trusted-service manifest contract

**Objective:** Add documentation and schema examples for a service addon without enabling execution.

**Files:**
- Create: `docs/trusted-server-addon-contract.md`
- Create: `examples/addons/trusted-service/manifest.json`
- Modify: `README.md`
- Test: manifest validation tests in `src/addons.rs`

The manifest should declare addon ID/version, `kind=trusted_server_service`, image reference plus immutable digest, requested capabilities, exposed API namespace, storage namespace, minimum server API, and client package identity. It must not contain arbitrary host paths, database URLs, commands, or secret values.

For this slice the server should reject installation with a clear message stating that trusted-service execution is not enabled yet.

### Task 2: Add core-owned addon storage primitives

**Objective:** Provide a namespaced persistence layer that future services can use without direct core-table access.

**Files:**
- Modify: `src/models.rs`
- Modify: `src/db.rs`
- Modify: `src/postgres.rs`
- Create: `migrations/postgres/0003_addon_storage.sql`
- Test: SQLite round-trip and namespace/size validation tests

Use an `addon_storage` table keyed by `(addon_id, key)` with bounded JSON values and timestamps. Add equivalent SQLite and PostgreSQL migrations. Do not expose raw SQL or allow an addon to select another addon’s namespace.

### Task 3: Add authenticated core storage API

**Objective:** Expose only the bounded namespace API needed by a future trusted service.

**Files:**
- Modify: `src/api.rs`
- Modify: `src/middleware.rs`
- Modify: `static/js/api.js` only if administrator tooling needs it
- Test: authorization and cross-namespace rejection tests

Use an addon-service credential or internal service identity, not a normal browser session. Expose bounded get/set/delete operations, with explicit addon ID and key validation. Do not expose users, sessions, client credentials, signing keys, or arbitrary database queries.

### Task 4: Add a service gateway contract

**Objective:** Define how a trusted service receives normalized events and returns bounded data.

**Files:**
- Create: `docs/trusted-server-addon-gateway.md`
- Modify: `src/api.rs`
- Test: request/response size, timeout, capability, and namespace checks

Use a fixed server-owned gateway namespace such as `/api/addon-services/{addon_id}/...`. Require the service credential, cap payloads, enforce timeouts, and prevent the service from selecting arbitrary upstream URLs or proxying core credentials.

### Task 5: Add signed OCI image staging, without starting containers

**Objective:** Verify and stage an operator-approved service image reference and digest without executing it.

**Files:**
- Modify: `src/addons.rs`
- Modify: `src/api.rs`
- Create: `docs/trusted-server-addon-operations.md`
- Test: image reference/digest validation, signature failure, and no-execution behavior

Require a separately signed service manifest and immutable image digest. Staging must not pull or run an image until the container policy, rootless runtime, filesystem mounts, network policy, and resource limits are implemented.

### Task 6: Implement rootless isolated service orchestration

**Objective:** Run trusted services only after explicit administrator approval and policy validation.

**Files:**
- Create: service supervisor module and unit templates
- Modify: `src/addons.rs`, `src/api.rs`, deployment documentation
- Test: disposable container lifecycle, denied mounts/network, resource limits, health checks

Use rootless containers, dedicated service users, read-only filesystem by default, an addon-only writable volume, no host secrets, no updater/addon private keys, and a default-deny network policy. Require Postgres for services needing database persistence; never mount the live SQLite database.

### Task 7: Add client delivery integration

**Objective:** Connect normalized service output to signed client extensions without coupling the client to the service database.

**Files:**
- Modify: existing client-extension manifest and inventory code
- Test: service output → normalized payload → client status and renderer contract

The client receives only normalized data and a separately signed renderer package. Service credentials and database credentials never cross the server/client boundary.

### Task 8: End-to-end test-client rollout

**Objective:** Verify the full trusted-service path on the dedicated test client before any fleet deployment.

**Verification:**
- full Rust tests and release build;
- disposable PostgreSQL migration twice;
- signed manifest/image verification;
- container cannot read protected core tables or files;
- service can use only its own schema/API namespace;
- client receives normalized data and stages, but does not execute, the client package;
- physical/visual confirmation on test client `11.1.1.30`;
- no production deployment or push until explicitly authorized.
