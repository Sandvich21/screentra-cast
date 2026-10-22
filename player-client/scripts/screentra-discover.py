#!/usr/bin/env python3
"""Discover Screentra Cast servers on the local IPv4 broadcast domain."""
from __future__ import annotations

import argparse
import base64
import hashlib
import json
import os
import socket
import sys
import time
import urllib.parse

DISCOVERY_PROTOCOL = "screentra-cast-discovery-v1"
DISCOVERY_REQUEST = b"SCREENTRA_CAST_DISCOVER_V1"
MAX_RESPONSE_BYTES = 64 * 1024
_DISCOVERY_FIELDS = frozenset({
    "protocol", "server_name", "url", "ca_pem_b64", "ca_sha256",
})


def sha256_bytes(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def validate_record(raw: object) -> dict[str, str] | None:
    if not isinstance(raw, dict) or set(raw) != _DISCOVERY_FIELDS or raw.get("protocol") != DISCOVERY_PROTOCOL:
        return None
    name = raw.get("server_name")
    url = raw.get("url")
    ca_b64 = raw.get("ca_pem_b64")
    ca_hash = raw.get("ca_sha256")
    if not isinstance(name, str) or not name.strip() or len(name) > 128:
        return None
    if any(ord(char) < 32 for char in name):
        return None
    if not isinstance(url, str) or len(url) > 512:
        return None
    parsed = urllib.parse.urlparse(url)
    if parsed.scheme != "https" or not parsed.hostname or parsed.username or parsed.password:
        return None
    if parsed.path not in ("", "/") or parsed.query or parsed.fragment:
        return None
    try:
        port = parsed.port or 443
    except ValueError:
        return None
    if not 1 <= port <= 65535:
        return None
    if not isinstance(ca_b64, str) or len(ca_b64) > 128 * 1024:
        return None
    try:
        ca = base64.b64decode(ca_b64, validate=True)
    except Exception:
        return None
    if b"-----BEGIN CERTIFICATE-----" not in ca or len(ca) > 64 * 1024:
        return None
    if not isinstance(ca_hash, str) or ca_hash != sha256_bytes(ca):
        return None
    return {
        "server_name": name.strip(),
        "url": url.rstrip("/"),
        "ca_pem_b64": ca_b64,
        "ca_sha256": ca_hash,
    }


def discover(port: int, timeout: float) -> list[dict[str, str]]:
    request = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    request.setsockopt(socket.SOL_SOCKET, socket.SO_BROADCAST, 1)
    request.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    request.bind(("", 0))
    request.settimeout(0.25)
    try:
        request.sendto(DISCOVERY_REQUEST, ("255.255.255.255", port))
        deadline = time.monotonic() + timeout
        records: dict[tuple[str, str], dict[str, str]] = {}
        while time.monotonic() < deadline:
            try:
                data, _peer = request.recvfrom(MAX_RESPONSE_BYTES)
            except socket.timeout:
                continue
            try:
                raw = json.loads(data.decode("utf-8"))
            except (UnicodeDecodeError, json.JSONDecodeError):
                continue
            record = validate_record(raw)
            if record:
                records[(record["url"], record["ca_sha256"])] = record
        return sorted(records.values(), key=lambda value: (value["server_name"].lower(), value["url"]))
    finally:
        request.close()


def choose(records: list[dict[str, str]]) -> dict[str, str] | None:
    if not records:
        return None
    tty = None
    try:
        tty = open("/dev/tty", "r+", encoding="utf-8")
    except OSError:
        pass
    try:
        if len(records) == 1:
            record = records[0]
            print(
                f"Found Screentra Cast server '{record['server_name']}' at {record['url']}.",
                file=sys.stderr,
            )
            if os.environ.get("SCREENTRA_DISCOVERY_AUTO_ACCEPT") == "1":
                return record
            if tty is None:
                return None
            print("Use this server? [Y/n] ", end="", file=tty, flush=True)
            answer = tty.readline().strip().lower()
            return None if answer not in ("", "y", "yes") else record

        if tty is None:
            return None
        print("Discovered Screentra Cast servers:", file=tty)
        for index, record in enumerate(records, 1):
            print(f"  {index}) {record['server_name']} ({record['url']})", file=tty)
        print("Select a server number, or press Enter to cancel: ", end="", file=tty, flush=True)
        answer = tty.readline().strip()
        if not answer.isdigit() or not 1 <= int(answer) <= len(records):
            return None
        return records[int(answer) - 1]
    finally:
        if tty is not None:
            tty.close()


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--port", type=int, default=3911)
    parser.add_argument("--timeout", type=float, default=2.5)
    args = parser.parse_args()
    if not 1 <= args.port <= 65535 or not 0.2 <= args.timeout <= 10:
        return 2
    selected = choose(discover(args.port, args.timeout))
    if selected is None:
        return 1
    print(selected["url"])
    print(selected["ca_pem_b64"])
    print(selected["server_name"])
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
