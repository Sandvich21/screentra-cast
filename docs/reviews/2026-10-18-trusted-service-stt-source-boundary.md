# Trusted-service and client-STT source-boundary review

**Review date:** 2026-10-18
**Reviewed commit:** `8e9f9752e25acb8a6372d419373ad8a84c291e7e`
**Scope:** source/tests only; no client, server, listener, process, model, hardware, image, or deployment activation was attempted.

## Verdict

**PASS for the reviewed disabled contract boundary.** The source and focused tests keep
trusted-service execution and the client STT supervisor plan inert. This is **not** a
runtime-readiness, deployment, or physical-verification claim.

The preceding contract slices added inert source/test artifacts. This closeout added one
focused server-side request-admission fix and regression test described in the dated
closeout section below; it did not activate any runtime, deployment, or hardware path.

## Route registration and activation surface

- `src/api.rs:1286-1308` registers exactly six trusted-service routes: stage, list staged,
  remove staged, approve staged, list runtime records, and disable a runtime record.
- `src/api.rs:1670-1677` applies the global authentication middleware before the router
  reaches these routes; the handlers additionally require the administrator role at
  `src/api.rs:2053-2063` and `src/api.rs:2168-2241`.
- `src/api.rs:2168-2256` shows the handlers perform metadata/staging/record persistence,
  approval, disable, removal, and audit writes only. There is no route handler calling
  `TrustedServiceSupervisor::activate`, no runtime-start route, and the static check
  reported `api_activate_token_count=0`.
- `src/addons.rs:914-929` rejects `trusted_server_service` manifests after validation as
  contract-only, and rejects non-declarative executable addon kinds. Entrypoints,
  permissions, and client capabilities are rejected at `src/addons.rs:939-953`.

## Boundary evidence by path

### Execution flag and runtime activation

- `src/trusted_service_runtime.rs:25-27` defines
  `pub const EXECUTION_ENABLED: bool = false`.
- New approved records initialize all runtime evidence to false at
  `src/trusted_service_runtime.rs:532-560`, including
  `database_boundary_verified`, `credential_broker_verified`, gateway, transport,
  lifecycle, cleanup, and dedicated-host evidence.
- Record loading enforces the flag and contract at
  `src/trusted_service_runtime.rs:494-505` and `:734-766`; a persisted record with
  `execution_enabled=true` is rejected.
- `TrustedServiceSupervisor::activation_plan` collects every missing gate and forces
  `execution_backend` while the flag is false (`src/trusted_service_runtime.rs:1241-1288`).
  `activate` calls the runtime only after `can_start` is true
  (`src/trusted_service_runtime.rs:1337-1350`). Therefore the production activation
  path is blocked before the runtime adapter is reached.
- The real process adapter does contain a deferred `Command::new(...).spawn()` at
  `src/trusted_service_runtime.rs:966-1016`, but its first branch is the disabled gate
  at `:970-974`. The focused test
  `rootless_oci_runtime_never_spawns_while_execution_is_disabled` passed and asserted
  the running set remained empty (`src/trusted_service_runtime.rs:2791-2823`).

### OCI image pull/transport and process boundary

- `src/trusted_service_oci.rs:124-165` makes the runtime probe report
  `can_start: false` and records that the capability probe is not enabled.
- Approved-record launch validation returns `Trusted service execution is disabled`
  before checking the remaining activation details when the global or record flag is
  disabled (`src/trusted_service_oci.rs:232-266`).
- The fixed argv builder is contract/dry-run construction only
  (`src/trusted_service_oci.rs:276-306`). It includes `--pull=never`, `--network=none`,
  read-only/rootless restrictions, and a digest-pinned image reference; it does not
  pull an image. The actual `Command::spawn` remains behind the runtime gate above.
- The OCI tests passed for fixed argv, explicit runtime-path construction without
  execution, and disabled approved-record binding. These tests are source/contract
  evidence, not runtime or registry evidence.

### Socket listener and gateway

- The gateway constructor fixes the Unix socket path and validates its binding contract
  (`src/trusted_service_gateway.rs:788-804`).
- `run_until_shutdown` checks `EXECUTION_ENABLED` at
  `src/trusted_service_gateway.rs:820-830`, before parent inspection and before
  `UnixListener::bind` at `:831-844`. The focused
  `unix_gateway_never_binds_while_execution_is_disabled` test passed.
- The client STT supervisor plan has no socket API or listener. Its module-level contract
  says it never opens a socket, starts/stops a process, loads a model, reads audio,
  accesses credentials, or changes lifecycle state (`player-client/scripts/
  screentra-client-service-supervisor-plan.py:2-7`).

### Credential issuance and transport

- `DisabledCredentialBroker::issue` and `revoke` return blocked errors at
  `src/trusted_service_credentials.rs:248-264`. The credential lease ledger stores only
  opaque, bounded lease metadata; it does not contain a secret value
  (`src/trusted_service_credentials.rs:75-83` and `:179-181`).
- The focused `disabled_broker_fails_closed_without_issuing_a_credential` test passed.
- The client STT supervisor plan rejects credential-shaped metadata fields in its
  negative test cases (`tests/test_client_service_supervisor_plan.py:128-190`) and
  contains no subprocess/socket/model-loading API tokens in the static check.

### Database boundary

- The database module explicitly describes its adapters as non-service-facing and keeps
  future wiring behind an authenticated gateway and disabled supervisor gate
  (`src/trusted_service_database.rs:1-4`).
- The core storage contract disables raw SQL, core-table access, migrations, SQLite-file
  mounts, and requires auditing (`src/trusted_service_database.rs:115-134`), with
  forbidden grants rejected at `:163-173`.
- The runtime activation inventory marks `database_boundary` false
  (`src/trusted_service_runtime.rs:276-281`), and the default record does the same at
  `:539-543`. The focused `core_storage_contract_is_file_and_sql_free` and
  `supervisor_blocks_activation_until_every_execution_gate_is_ready` tests passed.
- `src/api.rs:164-252` contains internal construction helpers for typed database/gateway
  adapters, but the route inventory above contains no database-boundary activation route;
  the adapters are not evidence that the disabled production boundary is active.

### Client STT supervisor plan

- `build_supervisor_activation_plan` validates bounded fields and identity, accepts only
  the closed intents, and performs no runtime work
  (`player-client/scripts/screentra-client-service-supervisor-plan.py:159-230`).
- Both the top-level plan and nested result explicitly return
  `execution_enabled=False`, `lifecycle_state_changed=False`, `process_started=False`,
  `socket_bound=False`, and `model_loaded=False` (`:230-263`). Mutating intents become
  `execution_disabled`; health/status can bind only a bounded identity-matched
  observation.
- The exact focused test command ran five tests, all passed. Tests cover health/status
  metadata-only results, all four mutating intents, identity/digest/socket/runtime-field
  rejection, and oversized/arbitrary health observations
  (`tests/test_client_service_supervisor_plan.py:53-233`).

## Allowed or independent operations versus deferred runtime paths

The following are intentionally separate from trusted-service activation:

- **Metadata/filesystem:** bounded runtime-record staging, atomic JSON replacement,
  permissions, and audit persistence are administrative state operations
  (`src/trusted_service_runtime.rs:325-440`); they do not start a service.
- **Tests/build:** fake runtime markers, temporary directories, and focused unit tests
  are test-only. OCI argv construction is a dry-run contract operation, not a process
  launch (`src/trusted_service_oci.rs:276-323`).
- **Crypto/integrity:** manifest/image signature re-verification is approval evidence,
  and the OCI module uses SHA-256 only to derive a deterministic container name
  (`src/trusted_service_runtime.rs:250-260`; `src/trusted_service_oci.rs:326-336`).
  These do not authorize execution.
- **Backup/system administration:** PostgreSQL dump/restore and tar backup commands are
  separate backup paths (`src/backup_api.rs:158-215`, `:279-328`). Allowlisted
  `systemctl` operations are also a separate administrative path
  (`src/backup_api.rs:543-581`). They were not treated as trusted-service activation
  evidence and were not exercised by this review.

Deferred until a separately reviewed activation slice: OCI runtime spawning, image
transport/pull evidence, credential broker issuance/transport, database-boundary
activation, gateway listener binding, readiness/resource enforcement, lifecycle
rollback/cleanup, and live client STT/model/audio behavior. The runtime gate inventory
records these blockers at `src/trusted_service_runtime.rs:267-320`.

## Checks actually run

All checks were run in `/home/tech/screentra-cast` with the source unchanged:

1. `git rev-parse HEAD`, `git status --short`, and `git log --oneline -5` — HEAD was
   `0c28e6061a575b642734a60780a637f10c412ec7`; the worktree was clean before this
   review artifact.
2. Static source-boundary script — six trusted route path registrations, zero
   `activate(` tokens in `src/api.rs`, `EXECUTION_ENABLED` remained a literal false,
   and all supervisor-plan side-effect token checks passed. The relevant plan scan
   returned zero for `subprocess`, `Popen`, `create_subprocess`, `os.system`,
   `socket.socket`, `socket.bind`, `socket.listen`, `model.load`, and `open(`.
3. `python3 -m unittest tests.test_client_service_supervisor_plan -v` — **5 passed,
   0 failed**.
4. Focused Rust tests with
   `RUSTFLAGS=-Awarnings cargo test --bin screentra-cast <test> -- --exact --quiet` —
   **7 commands, 7 passed, 0 failed**, one test per command:
   - `trusted_service_runtime::tests::rootless_oci_runtime_never_spawns_while_execution_is_disabled`
   - `trusted_service_gateway::tests::unix_gateway_never_binds_while_execution_is_disabled`
   - `trusted_service_oci::tests::approved_record_binding_still_respects_disabled_execution_gate`
   - `trusted_service_credentials::tests::disabled_broker_fails_closed_without_issuing_a_credential`
   - `trusted_service_database::tests::core_storage_contract_is_file_and_sql_free`
   - `trusted_service_runtime::tests::supervisor_blocks_activation_until_every_execution_gate_is_ready`
   - `trusted_service_runtime::tests::activation_plan_is_not_a_runtime_start_request`
5. An initial `cargo test --lib ...` attempt was invalid because this package has no
   library target; the same focused tests were rerun successfully with
   `--bin screentra-cast`.
6. `git diff --check` and final `git status --short`/diff inspection were run after
   writing this artifact.

## Post-install / contract closeout (2026-10-18)

### Installed artifact evidence is not runtime readiness

- The authorized `.56` installation identity is recorded as `whisper.cpp` 1.9.5,
  source commit `d1be6fde11ac6e0407606b4e42fe72d34add8037`, runtime SHA-256
  `d8b3d4f22d96f2a5c26cc34966927c69ab88406b52cdc7cee7c94e75bee16d97`, and
  `whisper-tiny.en` model SHA-256
  `921e4cf8686fdd993dcd081a5da5b6c365bfde1162e72b08d75ac75289920b1f`
  (`tests/test_stt_runtime_installation_evidence.py:29-37`). The evidence contract
  requires artifact checks plus a non-starting check and keeps execution, process,
  model-load, and service-start flags false (`player-client/scripts/
  screentra-stt-runtime-installation-evidence.py:137-186,245-260`).
- A read-only SSH probe of `.56` returned `host=third-floor`, no matching
  `whisper`/`stt`/`magic-mirror` process, no matching Unix listener, and no matching
  system or user unit. It observed only unrelated loopback listeners on `127.0.0.1:3900`
  and `:3910`. No file, service, process, listener, model, microphone, or camera
  operation was performed on `.56`; the probe is inactive-state evidence, not a fresh
  artifact hash or runtime-readiness result.
- The remaining blockers are runtime activation, process health, IPC binding and peer
  authorization, resource enforcement, microphone permission/capture, model loading and
  inference, lifecycle/rollback, and physical verification. Installation evidence and
  the plan's explicit non-readiness distinction remain separate
  (`docs/plans/2026-10-18-magic-mirror-addon.md:54-62`).

### Contract closeout and focused fix

- The latest client transcription contract requires `push_to_talk is True`, 64-byte
  service identifiers, and 128-byte request identifiers that reject credential-shaped
  identifiers (`player-client/scripts/screentra-stt-transcription-request-admission.py:69-101`).
  The server previously accepted any boolean, the broader 128-byte generic service
  identifier, and credential-shaped request IDs. Test-first regressions cover false
  push-to-talk, a 65-byte service ID, `password-token`, and `token-1`
  (`src/api.rs:9816-9834`): the tests failed before each fix and passed after it. The
  server now applies the matching checks at `src/api.rs:7619-7639,7687-7701`.
- The actor validates the same request through the pure admission module and returns
  `execution_disabled` with zero audio and no transcript (`player-client/scripts/
  screentra-player-actor.py:75-127,956-973`). The runtime-admission result likewise
  forces activation, microphone/camera access, process, listener, model-load,
  inference, health, resource, rollback, and lifecycle flags false
  (`player-client/scripts/screentra-stt-runtime-admission.py:1147-1183`).

### Boundary and packaging checks

- The 14 new `screentra-stt-*.py` modules have zero AST calls to subprocess/process,
  socket bind/listen, model-load, capture, network, or mutating filesystem APIs. Their
  inert flag counts were nonzero: `execution_enabled_false=15`,
  `runtime_activation_allowed_false=13`, `process_started_false=8`,
  `socket_bound_false=3`, `model_loaded_false=3`.
- The current route inventory still has exactly six trusted-service registrations at
  `src/api.rs:1286-1308`; `activate(` occurs zero times in `src/api.rs`. The latest
  contract-slice diff added zero source route, activation, spawn, or listener-bind
  lines. The focused server fix also added zero route, activation, spawn, or bind
  lines. `EXECUTION_ENABLED` remains the literal false at
  `src/trusted_service_runtime.rs:25-27`, and `database_boundary` remains false in
  the gate inventory at `:276-281` and default record at `:539-543`.
- The generated installer is source-derived: the generator maps the actor and
  transcription module at `player-client/scripts/build-installer.py:7-24`, and its
  in-memory output matched `player-client/static/install/player-client.sh` byte-for-byte
  across all 14 sections. The generator's optional `--deploy-server` path at
  `:42-67` was not invoked; no deploy, SSH copy, service enable/start/restart, or
  `.56` mutation was introduced.
- Deferred trusted-service boundaries remain fail-closed: OCI capability probing reports
  `can_start: false` (`src/trusted_service_oci.rs:157-164`), process start returns before
  spawn (`src/trusted_service_runtime.rs:966-974`), the gateway returns before
  `UnixListener::bind` (`src/trusted_service_gateway.rs:820-844`), the credential broker
  blocks issue/revoke (`src/trusted_service_credentials.rs:248-264`), and the database
  contract rejects raw SQL/core-table/migration/file-mount grants
  (`src/trusted_service_database.rs:115-173`).

### Closeout commands and results

- `python3 -m unittest discover -s tests -p 'test_stt_*.py' -v` — **134 tests,
  0 failures**; `python3 -m unittest discover -s tests -p 'test_client_service*.py' -v`
  — **19 tests, 0 failures**; `python3 -m unittest discover -s tests -p
  'test_local_model_staging_plan.py' -v` — **8 tests, 0 failures**.
- `PYTHONPYCACHEPREFIX=<temporary> python3 -m py_compile player-client/scripts/
  screentra-stt-*.py player-client/scripts/screentra-player-actor.py
  player-client/scripts/build-installer.py` — **exit 0**; temporary bytecode was
  removed. `git diff --check` — **exit 0**.
- `RUSTFLAGS=-Awarnings cargo test --bin screentra-cast --quiet` — **419 passed,
  0 failed, 6 ignored**. The focused regression command was **RED: 0 passed, 1
  failed** before the fix and **GREEN: 1 passed, 0 failed** after it.

## Residual blockers and scope limits

- `EXECUTION_ENABLED` must remain false until every required gate has independently
  reviewed evidence.
- `database_boundary`, credential broker, controlled gateway, image transport,
  readiness, resource policy, audit, lifecycle rollback, cleanup, and dedicated-host
  evidence remain false by default.
- No live client/service process, Unix socket, model file, microphone/audio path,
  registry transport, database role/schema activation, hardware, or deployment was
  verified. The review therefore makes no runtime-readiness or physical-verification
  claim.
- The OCI process adapter and Unix gateway are present as deferred code paths. Their
  existence is not activation evidence; both were verified to return before spawning
  or binding while disabled.
- No credentials, tokens, connection strings, or secret values are recorded here.
