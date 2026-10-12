#!/usr/bin/env python3
"""Import a Screentra Cast SQLite database into PostgreSQL.

This is an offline migration tool. It never opens the live server database for
writing; run it against a backup copy and a disposable PostgreSQL database.
The server binary must be PostgreSQL-backed before the imported database is
used by production.
"""
from __future__ import annotations

import argparse
import csv
import io
import os
import sqlite3
import subprocess
import sys
from pathlib import Path


def qident(value: str) -> str:
    return '"' + value.replace('"', '""') + '"'


def psql(dsn: str, sql: str, *, stdin: str | None = None) -> str:
    proc = subprocess.run(
        ["psql", dsn, "--no-psqlrc", "-v", "ON_ERROR_STOP=1", "-X", "-q", "-c", sql],
        input=stdin,
        text=True,
        capture_output=True,
    )
    if proc.returncode:
        raise RuntimeError(proc.stderr.strip() or proc.stdout.strip() or "psql failed")
    return proc.stdout


def psql_script(dsn: str, script: str) -> str:
    proc = subprocess.run(
        ["psql", dsn, "--no-psqlrc", "-v", "ON_ERROR_STOP=1", "-X", "-q", "-f", "-"],
        input=script,
        text=True,
        capture_output=True,
    )
    if proc.returncode:
        raise RuntimeError(proc.stderr.strip() or proc.stdout.strip() or "psql script failed")
    return proc.stdout


def psql_at(dsn: str, sql: str) -> str:
    proc = subprocess.run(
        ["psql", dsn, "--no-psqlrc", "-v", "ON_ERROR_STOP=1", "-X", "-q", "-At", "-c", sql],
        text=True,
        capture_output=True,
    )
    if proc.returncode:
        raise RuntimeError(proc.stderr.strip() or proc.stdout.strip() or "psql query failed")
    return proc.stdout


def pg_columns(dsn: str, table: str) -> dict[str, str]:
    proc = subprocess.run(
        [
            "psql", dsn, "--no-psqlrc", "-v", "ON_ERROR_STOP=1", "-X", "-q",
            "-At", "-F", "\t", "-c",
            "SELECT column_name, data_type FROM information_schema.columns "
            "WHERE table_schema='public' AND table_name='" + table.replace("'", "''") + "' "
            "ORDER BY ordinal_position",
        ],
        text=True,
        capture_output=True,
    )
    if proc.returncode:
        raise RuntimeError(proc.stderr.strip() or "could not inspect PostgreSQL table")
    result = {}
    for line in proc.stdout.splitlines():
        name, data_type = line.split("\t", 1)
        result[name] = data_type
    return result


def pg_count(dsn: str, table: str) -> int:
    """Return the row count for an already-validated public table."""
    output = psql(dsn, f"SELECT count(*) FROM public.{qident(table)}")
    for line in output.splitlines():
        value = line.strip()
        if value.isdigit():
            return int(value)
    raise RuntimeError(f"could not parse row count for {table}")


def synchronize_sequences(dsn: str) -> int:
    """Advance serial sequences beyond IDs copied from SQLite."""
    rows = psql_at(
        dsn,
        "SELECT table_name, column_name FROM information_schema.columns "
        "WHERE table_schema='public' AND column_default LIKE 'nextval(%' "
        "ORDER BY table_name, ordinal_position",
    )
    synchronized = 0
    for line in rows.splitlines():
        if not line.strip():
            continue
        table, column = line.split("|", 1)
        column_literal = column.replace("'", "''")
        psql(
            dsn,
            f"SELECT setval(pg_get_serial_sequence('public.{qident(table)}', '{column_literal}'), "
            f"COALESCE(MAX({qident(column)}), 1), MAX({qident(column)}) IS NOT NULL) FROM public.{qident(table)}",
        )
        synchronized += 1
    return synchronized


def pg_type(declared: str, pk: bool = False) -> str:
    t = (declared or "").upper()
    if "BLOB" in t:
        return "BYTEA"
    if any(x in t for x in ("REAL", "FLOA", "DOUB")):
        return "DOUBLE PRECISION"
    if "INT" in t:
        return "BIGINT"
    if any(x in t for x in ("CHAR", "CLOB", "TEXT")):
        return "TEXT"
    if "BOOL" in t:
        return "BOOLEAN"
    return "TEXT"


def sqlite_tables(conn: sqlite3.Connection) -> list[str]:
    return [
        row[0]
        for row in conn.execute(
            "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name"
        )
    ]


def table_info(conn: sqlite3.Connection, table: str):
    return list(conn.execute(f"PRAGMA table_info({qident(table)})"))


def create_tables(conn: sqlite3.Connection, dsn: str, tables: list[str]) -> None:
    for table in tables:
        columns = table_info(conn, table)
        if not columns:
            continue
        defs = []
        primary = []
        for _, name, declared, notnull, default, pk in columns:
            kind = pg_type(declared, bool(pk))
            defs.append(f"{qident(name)} {kind}" + (" NOT NULL" if notnull else ""))
            if pk:
                primary.append(name)
        if primary:
            defs.append("PRIMARY KEY (" + ", ".join(qident(x) for x in primary) + ")")
        sql = f"CREATE TABLE IF NOT EXISTS {qident(table)} ({', '.join(defs)})"
        psql(dsn, sql)


def csv_value(value, declared: str, target_type: str | None = None):
    if value is None:
        return r"\N"
    if target_type == "boolean":
        if value in (0, "0", False):
            return "false"
        if value in (1, "1", True):
            return "true"
    if "BLOB" in (declared or "").upper() or target_type == "bytea":
        return "\\x" + bytes(value).hex()
    return str(value)


def copy_table(
    conn: sqlite3.Connection,
    dsn: str,
    table: str,
    target_types: dict[str, str] | None = None,
) -> int:
    columns = table_info(conn, table)
    if target_types is not None:
        columns = [row for row in columns if row[1] in target_types]
    if not columns:
        return 0
    names = [row[1] for row in columns]
    declared = {row[1]: row[2] for row in columns}
    rows = conn.execute(f"SELECT {', '.join(qident(x) for x in names)} FROM {qident(table)}")
    buf = io.StringIO()
    writer = csv.writer(buf, lineterminator="\n")
    count = 0
    for row in rows:
        writer.writerow([
            csv_value(value, declared[name], target_types.get(name) if target_types else None)
            for name, value in zip(names, row)
        ])
        count += 1
    if not count:
        return 0
    copy_sql = (
        f"\\copy {qident(table)} ({', '.join(qident(x) for x in names)}) "
        "FROM STDIN WITH (FORMAT csv, NULL '\\N', ESCAPE '\\')"
    )
    psql(dsn, copy_sql, stdin=buf.getvalue())
    return count


def create_indexes(conn: sqlite3.Connection, dsn: str, tables: list[str]) -> None:
    for table in tables:
        for _, name, unique, *_ in conn.execute(f"PRAGMA index_list({qident(table)})"):
            if not name or name.startswith("sqlite_autoindex"):
                continue
            fields = [row[2] for row in conn.execute(f"PRAGMA index_info({qident(name)})")]
            if fields:
                unique_sql = "UNIQUE " if unique else ""
                psql(
                    dsn,
                    f"CREATE {unique_sql}INDEX IF NOT EXISTS {qident(name)} ON {qident(table)} ({', '.join(qident(x) for x in fields)})",
                )


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("sqlite_backup", type=Path)
    parser.add_argument("--dsn", default=os.environ.get("SIGNAGE_DATABASE_URL"), help="PostgreSQL DSN; not needed with --dry-run")
    parser.add_argument("--drop-schema", action="store_true", help="drop and recreate public schema in the target database")
    parser.add_argument("--dry-run", action="store_true", help="inspect the SQLite backup without connecting to PostgreSQL or writing anything")
    parser.add_argument(
        "--schema-file",
        type=Path,
        help="apply an existing PostgreSQL schema before copying matching SQLite columns",
    )
    args = parser.parse_args()
    if not args.dry_run and not args.dsn:
        parser.error("--dsn or SIGNAGE_DATABASE_URL is required unless --dry-run is used")
    if not args.sqlite_backup.is_file():
        parser.error(f"SQLite backup does not exist: {args.sqlite_backup}")
    if args.drop_schema:
        psql(args.dsn, "DROP SCHEMA public CASCADE; CREATE SCHEMA public;")
    schema_mode = args.schema_file is not None
    if schema_mode:
        if not args.schema_file.is_file():
            parser.error(f"PostgreSQL schema file does not exist: {args.schema_file}")
        psql_script(args.dsn, args.schema_file.read_text())
    source = sqlite3.connect(f"file:{args.sqlite_backup}?mode=ro", uri=True)
    tables = sqlite_tables(source)
    print(f"source={args.sqlite_backup} tables={len(tables)}", flush=True)
    if args.dry_run:
        total = 0
        for table in tables:
            count = source.execute(f"SELECT count(*) FROM {qident(table)}").fetchone()[0]
            total += count
            print(f"{table}: {count} row(s)", flush=True)
        print(f"dry_run=true source_rows={total}", flush=True)
        source.close()
        return 0
    if not schema_mode:
        create_tables(source, args.dsn, tables)
    total = 0
    imported_counts: dict[str, int] = {}
    for table in tables:
        target_types = pg_columns(args.dsn, table) if schema_mode else None
        count = copy_table(source, args.dsn, table, target_types)
        imported_counts[table] = count
        total += count
        print(f"{table}: {count} row(s)", flush=True)
    if not schema_mode:
        create_indexes(source, args.dsn, tables)
    synchronized = synchronize_sequences(args.dsn)
    print(f"synchronized_sequences={synchronized}", flush=True)
    if args.drop_schema:
        verified_total = 0
        for table, expected in imported_counts.items():
            if schema_mode and not pg_columns(args.dsn, table):
                continue
            actual = pg_count(args.dsn, table)
            if actual != expected:
                raise RuntimeError(f"row-count verification failed for {table}: expected {expected}, found {actual}")
            verified_total += actual
        print(f"verified_rows={verified_total}", flush=True)
    print(f"imported_rows={total}", flush=True)
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, sqlite3.Error, RuntimeError) as exc:
        print(f"migration failed: {exc}", file=sys.stderr)
        raise SystemExit(1)
