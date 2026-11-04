#!/usr/bin/env python3
"""Privileged, stdin-only SMB/NFS mount helper for Screentra backups.

This file is installed root-owned at /usr/local/libexec/screentra-cast-share-helper
and is callable only through a narrowly scoped sudoers rule. Secrets are read from
stdin, written to a root-only credentials file, and never printed or logged.
"""
from __future__ import annotations

import json
import os
import re
import shutil
import subprocess
import sys
from pathlib import Path

MOUNT_PATH = Path("/mnt/screentra-cast-backups/software")
UNIT_DIR = Path("/etc/systemd/system")
CREDENTIAL_DIR = Path("/etc/screentra-cast/credentials")
CREDENTIAL_PATH = CREDENTIAL_DIR / "software-backup"
UNIT_PREFIX = "screentra-cast-backup-share"
MAX_INPUT = 16 * 1024


def fail(message: str) -> None:
    print(json.dumps({"ok": False, "mounted": False, "mount_path": str(MOUNT_PATH), "error": message}))
    raise SystemExit(1)


def run(args: list[str], timeout: int = 45, check: bool = False) -> subprocess.CompletedProcess[str]:
    try:
        return subprocess.run(
            args,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            text=True,
            timeout=timeout,
            check=check,
            env={"PATH": "/usr/sbin:/usr/bin:/sbin:/bin"},
        )
    except (OSError, subprocess.SubprocessError):
        fail("server share operation failed")


def valid_text(value: object, name: str, maximum: int, allow_empty: bool = False) -> str:
    if not isinstance(value, str):
        fail(f"{name} is invalid")
    if not allow_empty and not value:
        fail(f"{name} is required")
    if len(value) > maximum or "\x00" in value or any(ord(ch) < 32 or ord(ch) == 127 for ch in value):
        fail(f"{name} is invalid")
    return value


def valid_endpoint(value: object, name: str) -> str:
    value = valid_text(value, name, 255)
    if any(ch.isspace() for ch in value) or "/" in value or "\\" in value or ":" in value:
        fail(f"{name} is invalid")
    if not re.fullmatch(r"[A-Za-z0-9._-]+", value):
        fail(f"{name} is invalid")
    return value


def valid_smb_share(value: object) -> str:
    value = valid_text(value, "share", 255)
    if any(ch.isspace() for ch in value) or "/" in value or "\\" in value:
        fail("share is invalid")
    return value


def valid_nfs_export(value: object) -> str:
    value = valid_text(value, "export", 1024)
    if not value.startswith("/") or ".." in Path(value).parts or "\n" in value:
        fail("export is invalid")
    return value


def unit_name() -> str:
    result = run(["/usr/bin/systemd-escape", "--path", "--suffix=mount", str(MOUNT_PATH)])
    name = result.stdout.strip()
    if not re.fullmatch(r"[A-Za-z0-9_.\\x-]+\.mount", name):
        fail("server share unit name is invalid")
    return name


def unit_path() -> Path:
    return UNIT_DIR / unit_name()


def is_mounted() -> bool:
    try:
        result = subprocess.run(
            ["/usr/bin/mountpoint", "-q", str(MOUNT_PATH)],
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            timeout=10,
            env={"PATH": "/usr/sbin:/usr/bin:/sbin:/bin"},
        )
        return result.returncode == 0
    except (OSError, subprocess.SubprocessError):
        return False


def stop_existing() -> None:
    path = unit_path()
    if path.exists():
        run(["/usr/bin/systemctl", "disable", "--now", path.name], timeout=60)
        run(["/usr/bin/systemctl", "daemon-reload"], timeout=30)
    if is_mounted():
        result = run(["/usr/bin/umount", "--", str(MOUNT_PATH)], timeout=60)
        if result.returncode != 0 or is_mounted():
            fail("existing server share is busy; remove it after active users stop")


def atomic_write(path: Path, content: str, mode: int) -> None:
    path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    os.chmod(path.parent, 0o700)
    temp = path.with_name(f".{path.name}.tmp-{os.getpid()}")
    try:
        with open(temp, "w", encoding="utf-8", opener=lambda p, flags: os.open(p, flags, mode)) as handle:
            handle.write(content)
            handle.flush()
            os.fsync(handle.fileno())
        os.chmod(temp, mode)
        os.replace(temp, path)
    finally:
        try:
            temp.unlink()
        except FileNotFoundError:
            pass


def configure(payload: dict[str, object]) -> None:
    kind = valid_text(payload.get("kind"), "kind", 8)
    if kind not in {"smb", "nfs"}:
        fail("share type is invalid")
    server = valid_endpoint(payload.get("server"), "server")
    remote = valid_smb_share(payload.get("remote_path")) if kind == "smb" else valid_nfs_export(payload.get("remote_path"))
    username = valid_text(payload.get("username", ""), "username", 256, allow_empty=True)
    password = valid_text(payload.get("password", ""), "password", 4096, allow_empty=True)
    domain = valid_text(payload.get("domain", ""), "domain", 256, allow_empty=True)
    if kind == "smb" and (not username or not password):
        fail("SMB username and password are required")

    stop_existing()
    MOUNT_PATH.mkdir(mode=0o755, parents=True, exist_ok=True)
    if kind == "smb":
        credentials = f"username={username}\npassword={password}\n"
        if domain:
            credentials += f"domain={domain}\n"
        atomic_write(CREDENTIAL_PATH, credentials, 0o600)
        what = f"//{server}/{remote}"
        options = (
            f"credentials={CREDENTIAL_PATH},uid=board,gid=board,"
            "file_mode=0600,dir_mode=0700,_netdev,nofail,vers=3.0"
        )
        mount_type = "cifs"
    else:
        try:
            CREDENTIAL_PATH.unlink()
        except FileNotFoundError:
            pass
        what = f"{server}:{remote}"
        options = "rw,hard,timeo=600,retrans=2,_netdev,nofail,vers=4"
        mount_type = "nfs"

    unit = (
        "[Unit]\n"
        "Description=Screentra backup remote share\n"
        "Wants=network-online.target\n"
        "After=network-online.target\n\n"
        "[Mount]\n"
        f"What={what}\n"
        f"Where={MOUNT_PATH}\n"
        f"Type={mount_type}\n"
        f"Options={options}\n"
        "TimeoutSec=45\n\n"
        "[Install]\n"
        "WantedBy=multi-user.target\n"
    )
    atomic_write(unit_path(), unit, 0o644)
    run(["/usr/bin/systemctl", "daemon-reload"], timeout=30)
    result = run(["/usr/bin/systemctl", "enable", "--now", unit_path().name], timeout=75)
    if result.returncode != 0 or not is_mounted():
        run(["/usr/bin/systemctl", "disable", "--now", unit_path().name], timeout=30)
        fail("share mount failed; check the server, share name, credentials and permissions")
    print(json.dumps({"ok": True, "mounted": True, "mount_path": str(MOUNT_PATH), "error": None}))


def remove() -> None:
    stop_existing()
    try:
        unit_path().unlink()
    except FileNotFoundError:
        pass
    try:
        CREDENTIAL_PATH.unlink()
    except FileNotFoundError:
        pass
    run(["/usr/bin/systemctl", "daemon-reload"], timeout=30)
    try:
        MOUNT_PATH.rmdir()
    except OSError:
        pass
    print(json.dumps({"ok": True, "mounted": False, "mount_path": str(MOUNT_PATH), "error": None}))


def main() -> None:
    raw = sys.stdin.buffer.read(MAX_INPUT + 1)
    if len(raw) > MAX_INPUT:
        fail("share request is too large")
    try:
        payload = json.loads(raw.decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError):
        fail("share request is invalid")
    if not isinstance(payload, dict):
        fail("share request is invalid")
    operation = payload.get("operation")
    if operation == "configure":
        configure(payload)
    elif operation == "remove":
        remove()
    elif operation == "status":
        print(json.dumps({"ok": True, "mounted": is_mounted(), "mount_path": str(MOUNT_PATH), "error": None}))
    else:
        fail("share operation is invalid")


if __name__ == "__main__":
    main()
