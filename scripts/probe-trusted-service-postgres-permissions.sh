#!/usr/bin/env bash
# Disposable developer probe for the future trusted-service PostgreSQL boundary.
# This is not production runtime functionality and is intentionally not installed.
set -euo pipefail

command -v psql >/dev/null || { echo "psql is required" >&2; exit 2; }
command -v sudo >/dev/null || { echo "sudo is required" >&2; exit 2; }

admin_host="${POSTGRES_ADMIN_HOST:-/var/run/postgresql}"
service_host="${POSTGRES_SERVICE_HOST:-127.0.0.1}"
db_owner="${POSTGRES_DB_OWNER:-$(id -un)}"
if [[ ! "$db_owner" =~ ^[a-z_][a-z0-9_]{0,62}$ ]]; then
    echo "POSTGRES_DB_OWNER must be a simple PostgreSQL identifier" >&2
    exit 2
fi

suffix="$(python3 -c 'import secrets; print(secrets.token_hex(8))')"
role="svc_${suffix}"
database="svcdb_${suffix}"
addon_schema="addon_${suffix}"
other_schema="other_${suffix}"
service_password="$(python3 -c 'import secrets; print(secrets.token_hex(24))')"

admin_psql() {
    sudo -n -u postgres psql -h "$admin_host" -v ON_ERROR_STOP=1 -X "$@"
}

service_psql() {
    PGPASSWORD="$service_password" \
    PGHOST="$service_host" \
    PGUSER="$role" \
    PGDATABASE="$database" \
    psql -v ON_ERROR_STOP=1 -X "$@"
}

cleanup() {
    admin_psql -d postgres -c "DROP DATABASE IF EXISTS \"$database\" WITH (FORCE);" >/dev/null 2>&1 || true
    admin_psql -d postgres -c "DROP ROLE IF EXISTS \"$role\";" >/dev/null 2>&1 || true
}
trap cleanup EXIT

admin_psql -d postgres >/dev/null <<SQL
CREATE ROLE "$role" LOGIN PASSWORD '$service_password'
    NOSUPERUSER NOCREATEDB NOCREATEROLE NOINHERIT;
CREATE DATABASE "$database" OWNER "$db_owner";
SQL

admin_psql -d "$database" >/dev/null <<SQL
CREATE SCHEMA "$addon_schema" AUTHORIZATION "$db_owner";
CREATE SCHEMA "$other_schema" AUTHORIZATION "$db_owner";
CREATE TABLE "$addon_schema".records (id integer PRIMARY KEY, value text NOT NULL);
CREATE TABLE "$other_schema".records (id integer PRIMARY KEY, value text NOT NULL);
CREATE TABLE public.core_secret (value text NOT NULL);
INSERT INTO public.core_secret VALUES ('core-only');
REVOKE ALL ON DATABASE "$database" FROM PUBLIC;
REVOKE ALL ON DATABASE "$database" FROM "$role";
GRANT CONNECT ON DATABASE "$database" TO "$role";
REVOKE ALL ON SCHEMA public FROM PUBLIC;
REVOKE ALL ON SCHEMA "$other_schema" FROM "$role";
GRANT USAGE ON SCHEMA "$addon_schema" TO "$role";
GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE "$addon_schema".records TO "$role";
SQL

service_psql -c "INSERT INTO \"$addon_schema\".records VALUES (1, 'ok');" >/dev/null
[[ "$(service_psql -Atqc "SELECT value FROM \"$addon_schema\".records WHERE id = 1;")" == "ok" ]]

if service_psql -c "SELECT value FROM public.core_secret;" >/dev/null 2>&1; then
    echo "service role unexpectedly read a core table" >&2
    exit 1
fi
if service_psql -c "SELECT value FROM \"$other_schema\".records;" >/dev/null 2>&1; then
    echo "service role unexpectedly read another addon schema" >&2
    exit 1
fi
if service_psql -c "CREATE TABLE \"$addon_schema\".arbitrary (value text);" >/dev/null 2>&1; then
    echo "service role unexpectedly created a table in the addon schema" >&2
    exit 1
fi
if service_psql -c "CREATE TABLE public.arbitrary (value text);" >/dev/null 2>&1; then
    echo "service role unexpectedly created a core-schema table" >&2
    exit 1
fi

admin_psql -d postgres \
    -c "REVOKE CONNECT ON DATABASE \"$database\" FROM PUBLIC; REVOKE CONNECT ON DATABASE \"$database\" FROM \"$role\";" \
    >/dev/null
if service_psql -c 'SELECT 1;' >/dev/null 2>&1; then
    echo "revoked service role still connected" >&2
    exit 1
fi

printf 'PASS: disposable PostgreSQL schema/role isolation probe\n'
