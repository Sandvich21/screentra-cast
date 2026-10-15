# PostgreSQL Backup and Restore Implementation Plan

> **For Hermes:** Execute this plan in small vertical slices. Keep PostgreSQL credentials out of command-line arguments, logs, archives, and error responses.

**Goal:** Add a database-native PostgreSQL backup/restore path that preserves core data, namespaced addon storage, and inert trusted-service metadata without presenting a filesystem-only archive as a complete backup.

**Architecture:** Keep the current SQLite tar archive path unchanged. PostgreSQL backups will contain the application files, inert staged metadata, and a database-native dump produced from the configured PostgreSQL connection. Restore will validate the dump before swapping files, create a pre-restore database dump for rollback, restore the database transactionally, and retain filesystem rollback state until the operator removes it.

**Tech Stack:** Rust/Axum, `pg_dump`/`psql` subprocesses with credentials supplied only through child-process environment variables, safe gzip/tar inspection, PostgreSQL disposable integration tests.

---

### Task 1: Define the PostgreSQL archive contract

**Objective:** Add one explicit archive member and dry-run field for a PostgreSQL database dump.

**Files:**
- Modify: `src/backup_api.rs`
- Modify: `src/safe_archive.rs` only if archive inspection needs a new exact-path helper
- Modify: `docs/trusted-server-addon-operations.md`
- Test: dry-run archive inspection tests

Use the exact member `data/postgresql.sql` for a plain SQL dump. Add `contains_postgresql_dump` to `RestoreDryRunReport`. PostgreSQL confirmed restore must reject archives without this member; SQLite archives must not require it.

### Task 2: Add safe PostgreSQL command configuration

**Objective:** Run database-native tools without placing the connection string in process arguments or logs.

**Files:**
- Modify: `src/backup_api.rs`
- Test: URL parsing and redaction tests

Parse `SIGNAGE_DATABASE_URL` into child environment variables (`PGHOST`, `PGPORT`, `PGUSER`, `PGDATABASE`, and temporary `PGPASSWORD` when present). Reject unsupported URL forms such as missing host/database or non-PostgreSQL schemes. Never return the raw URL, password, or command stderr in an HTTP response or log.

The application must verify `pg_dump` and `psql` are available before beginning a PostgreSQL operation and must remove temporary dump files on every failure path.

### Task 3: Create PostgreSQL-native backups

**Objective:** Include a complete PostgreSQL dump in newly created backup archives.

**Files:**
- Modify: `src/backup_api.rs`
- Test: disposable PostgreSQL create-backup integration test

Run `pg_dump --format=plain --no-owner` into a temporary sibling file, add it to the tar archive as `data/postgresql.sql`, then remove the temporary source. Preserve the existing exclusion rules and trusted-service metadata. Insert the backup record only after both tar creation and dump completion succeed.

### Task 4: Validate PostgreSQL dumps before filesystem mutation

**Objective:** Ensure confirmed restore cannot partially replace the filesystem with an unusable database dump.

**Files:**
- Modify: `src/backup_api.rs`
- Test: malformed SQL dump and missing-member tests

Extract only into the existing staging directory. Require `data/postgresql.sql` for PostgreSQL restore, enforce archive member limits, and run a validation step before renaming the live install/data directories. Validation must not execute arbitrary addon code or activate trusted services.

### Task 5: Add database rollback around restore

**Objective:** Restore PostgreSQL and filesystem state together with a rollback path.

**Files:**
- Modify: `src/backup_api.rs`
- Test: disposable PostgreSQL restore success and forced-failure rollback tests

After moving the live data directory to its pre-restore location, create a pre-restore PostgreSQL dump there. Restore the staged SQL with `psql --set ON_ERROR_STOP=1` and a single transaction where supported. If database restore, filesystem placement, or service preparation fails, restore the pre-restore database dump and filesystem directories. Keep credentials out of the archive and response payload.

After success, remove `data/postgresql.sql` from the live runtime data directory; keep only the rollback copy under the existing pre-restore retention directory.

### Task 6: PostgreSQL addon-storage verification

**Objective:** Prove that the database-native dump preserves addon state.

**Files:**
- Modify: `src/backup_api.rs` tests
- Test: disposable PostgreSQL integration test

Seed `addon_storage` and staged trusted-service metadata, create a backup, mutate both, restore the backup, and verify the namespaced value and inert metadata return. Verify no activation marker, image pull, process start, or gateway exposure occurs.

### Task 7: Documentation and operational verification

**Objective:** Document the supported PostgreSQL workflow and its safety boundaries.

**Files:**
- Modify: `docs/trusted-server-addon-operations.md`
- Modify: `README.md` if backup behavior is documented there

Document required `pg_dump`/`psql` tooling, the exact archive member, rollback retention, credential handling, failure behavior, and the fact that backup/restore never activates trusted services.

Run:

```bash
RUST_TEST_THREADS=1 cargo test --locked
cargo check --locked
cargo build --release --locked
git diff --check
```

Also run the disposable PostgreSQL migration and create/restore probes twice with cleanup traps. Do not deploy or push until the PostgreSQL path and rollback behavior are verified.
