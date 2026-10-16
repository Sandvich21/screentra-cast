# Trusted server-service database boundary

This document defines the database contract for the future trusted server-service
runtime. The repository contains non-service-facing SQLite and PostgreSQL
controlled adapters over core-owned storage methods, but no service gateway is
exposed and the `database_boundary` activation gate remains false.

## Identity and authorization

Every database request is bound to both:

- the approved addon ID; and
- the current activation instance ID.

The service cannot select either identity in a request. The supervisor must
construct the binding from the approved runtime record, and the gateway must
reject a request whose identity does not exactly match that binding. A stopped,
disabled, revoked, or replaced activation instance has no database authority.

Database access is capability-scoped. The service receives only the operations
explicitly granted to its contract; it never receives a database URL, password,
Unix socket path, SQLite file path, PostgreSQL superuser role, or unrestricted
connection handle.

## Boundary modes

### SQLite: core-owned storage API only

A SQLite-backed installation uses the controlled core storage API. The service
may read, write, delete, and list values inside its own validated namespace
through bounded typed operations. The service may not:

- mount or open `data/signage.db` or any SQLite journal/WAL file;
- issue SQL, select a table, inspect schema metadata, or use `PRAGMA`;
- create tables, indexes, triggers, migrations, or extensions;
- address another addon namespace or core-owned data;
- obtain a filesystem path or raw database connection.

The existing `addon_storage` repository is the persistence implementation for
this API boundary. Its SQLite and PostgreSQL implementations remain core-owned;
passing a storage namespace does not grant the service direct database access.

### PostgreSQL: isolated per-service schema or controlled API

A PostgreSQL-backed service must use one of these two forms, selected by the
server and recorded in the contract:

1. the same controlled core storage API used by SQLite; or
2. a dedicated schema and least-privilege role created for that addon.

The schema/role form is future work. It must be provisioned by the server, not
by the service, and its role must have no access to `public`, core schemas,
other addon schemas, system catalogs beyond what the driver requires, or
administrative functions. The service still uses an allowlisted gateway rather
than receiving the main database URL. It may not run arbitrary SQL, create
arbitrary objects, install extensions, alter core schemas, or perform
cross-schema joins.

A dedicated schema is not a license to bypass the API policy: operations,
request size, result size, row count, key count, timeouts, and audit events
remain bounded by the service contract. Any future SQL implementation must use
server-owned prepared statements or reviewed operation templates; raw SQL is
not part of the contract.

## Operation and resource limits

The contract currently defines only typed namespaced key/value operations:

- `Get(key)`;
- `Put(key, value_json)`;
- `Delete(key)`; and
- `List(prefix, limit)`.

The initial hard ceilings are:

- request: 64 KiB;
- value: 64 KiB;
- result rows: 1,000;
- keys in one request: 1,000;
- keys cannot contain `/`, `\\`, control characters, or traversal syntax.

A future service-specific operation must add a named capability, request schema,
limit, authorization rule, audit event, SQLite behavior, PostgreSQL behavior,
and disposable integration coverage before it can be accepted. There is no
fallback to an arbitrary SQL endpoint.

## Migrations and schema ownership

Trusted services do not run migrations against the core database. Core schema
changes remain reviewed Screentra server changes delivered through the signed
server update path. For a future isolated PostgreSQL schema, provisioning and
schema version changes remain server-supervised and must be restricted to the
service schema; service startup cannot execute an uploaded migration file.

SQLite has no service schema and never receives a service-created table. This
keeps the SQLite and PostgreSQL modes behaviorally aligned and prevents an
addon from turning storage access into an arbitrary schema extension mechanism.

## Audit and failure behavior

Every accepted, rejected, timed-out, and failed operation must produce bounded
audit metadata containing the addon ID, activation instance, operation name,
outcome, and timestamp. It must not contain values, credentials, SQL text,
connection strings, or complete request bodies.

The current adapters persist this metadata through the existing server audit
repositories. The lifecycle is explicit: a rejected request records `rejected`;
a valid request records `accepted`; an operation result records `succeeded` or
`failed`. If any audit write fails, the gateway returns a generic failure and
does not report the database operation as successful. This behavior is covered
for SQLite; PostgreSQL uses the same sink contract but remains non-service-facing.
The boundary fails closed when identity, capability, namespace, limits, backend
mode, schema/role binding, or audit persistence cannot be validated. A database
failure must not fall back to a broader role, another namespace, or direct
filesystem access.


## Backup, restore, uninstall, and rollback

Backups preserve service-owned data according to the selected boundary:

- SQLite filesystem backups preserve the core `addon_storage` rows inside
  `data/signage.db`; they never expose the SQLite file to a service.
- PostgreSQL native backups preserve the approved database state, including
  service-owned rows/schema according to the server's backup policy.
- Restore never starts a trusted service, runs service code, executes service
  migrations, or issues credentials.
- A failed restore must roll back the filesystem/database state as far as the
  existing backup contract permits and remove transient dump/staging files.
- Uninstall removes only the addon namespace/schema after the service is
  stopped and its identity is revoked; it must not remove another namespace or
  core tables. Cleanup is idempotent and auditable.

## Disposable PostgreSQL permission probe status

A local disposable probe has verified the minimum server-provisioned role
boundary without enabling it in Screentra. The probe used a temporary database,
non-superuser/non-createdb/non-createrole service role, and a server-owned addon
schema with explicit `USAGE` plus DML grants only. It verified that the role can
read/write its approved table but cannot read a core table, read another addon
schema, create tables in either schema, or connect after `CONNECT` is revoked.
A repeatable version of this probe is checked in at
`scripts/probe-trusted-service-postgres-permissions.sh`. It is a developer
probe only, is not installed or exposed as production CLI functionality, and
requires a local PostgreSQL administrator path through non-interactive `sudo`.
It removes the temporary database and role on exit; credentials are passed only
through child-process environment variables and are not retained.

This is permission evidence for the future PostgreSQL schema/role mode, not a
service gateway or activation path. The current controlled adapter still uses
server-owned `addon_storage`, and the `database_boundary` gate remains false.

## Required implementation gates

Before any real container can receive database capability, all of the
following are required:

1. a gateway that authenticates addon ID plus activation instance;
2. a SQLite adapter backed only by typed core-owned storage methods;
3. a PostgreSQL adapter using either the same API or a disposable per-addon
   schema/role with explicit grants and revokes;
4. operation, row, value, request, timeout, and disk limits;
5. audit persistence and fail-closed behavior for audit failure;
6. tests proving one addon cannot read/write another addon or core data;
7. PostgreSQL permission tests against a temporary role-owned database;
8. backup/restore and uninstall tests that never activate the service; and
9. supervisor, credential, network, filesystem, and resource gates enabled
   only after the preceding evidence is reviewed.

The contract is validation metadata only. Non-service-facing SQLite and
PostgreSQL adapters now exercise typed namespaced operations through core-owned
`addon_storage` methods. They are not wired to any runtime, HTTP route,
container, credential, or service gateway. PostgreSQL schema/role access remains
unimplemented, and the `database_boundary` activation gate remains false.
