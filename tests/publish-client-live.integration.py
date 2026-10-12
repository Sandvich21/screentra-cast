#!/usr/bin/env python3
"""Verify one completed Publish reached the live client without a reboot.

This is intentionally an explicit live probe, not part of the default test loop:
it reads the server's display configuration over verified TLS, the player's local
cache, the served player assets, the browser process, and recent server access
logs. It never publishes, edits a layout, restarts a service, or changes the
client.

Run this on the client after the Publish response, supplying the revision from
that response. A successful result proves the server -> actor/heartbeat cache
-> running player revision path. It does not claim physical-pixel confirmation.
"""
from __future__ import annotations

import argparse
import datetime as dt
import json
import os
import re
import ssl
import subprocess
import sys
from pathlib import Path
from urllib.parse import quote, urljoin
from urllib.request import Request, urlopen


class VerificationError(RuntimeError):
    pass


def run_command(args: list[str]) -> tuple[int, str]:
    result = subprocess.run(args, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    return result.returncode, result.stdout


def get_json(url: str, context: ssl.SSLContext) -> dict:
    request = Request(url, headers={"Cache-Control": "no-cache"})
    with urlopen(request, context=context, timeout=10) as response:
        if response.status != 200:
            raise VerificationError(f"HTTP {response.status} from {url.rsplit('/', 1)[-1]}")
        return json.loads(response.read().decode("utf-8"))


def get_text(url: str, context: ssl.SSLContext) -> str:
    request = Request(url, headers={"Cache-Control": "no-cache"})
    with urlopen(request, context=context, timeout=10) as response:
        if response.status != 200:
            raise VerificationError(f"HTTP {response.status} from {url.rsplit('/', 1)[-1]}")
        return response.read().decode("utf-8")


def read_json(path: Path) -> dict:
    try:
        return json.loads(path.read_text())
    except Exception as exc:
        raise VerificationError(f"cannot read {path}: {exc}") from exc


def check(label: str, function, failures: list[str]) -> None:
    try:
        detail = function()
        print(f"PASS {label}: {detail}")
    except Exception as exc:
        failures.append(f"{label}: {exc}")
        print(f"FAIL {label}: {exc}")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--server-url", default=os.environ.get("PUBLISH_TEST_SERVER_URL", "https://127.0.0.1:3000"))
    parser.add_argument("--ca-cert", default=os.environ.get("PUBLISH_TEST_CA_CERT"))
    parser.add_argument("--client-id", required=True, help="enrolled client ID; it is never printed")
    parser.add_argument("--display-index", type=int, required=True)
    parser.add_argument("--expected-revision", type=int, required=True)
    parser.add_argument("--before-revision", type=int)
    parser.add_argument("--cache-dir", default=os.environ.get("PUBLISH_TEST_CACHE_DIR", str(Path.home() / ".local/share/screentra-client/cache")))
    parser.add_argument("--since", help="journal time boundary, e.g. '2026-09-29 12:47:00'")
    parser.add_argument("--skip-journal", action="store_true")
    args = parser.parse_args()
    if not args.client_id.strip():
        raise SystemExit("--client-id must not be empty")
    if args.expected_revision < 0:
        raise SystemExit("--expected-revision must be non-negative")

    base = args.server_url.rstrip("/")
    ca_cert = args.ca_cert or os.environ.get("RIPLEY_CA_CERT")
    if not ca_cert:
        raise SystemExit("--ca-cert or PUBLISH_TEST_CA_CERT is required; TLS verification is not disabled")
    context = ssl.create_default_context(cafile=ca_cert)
    cache_dir = Path(args.cache_dir)
    client_path = quote(args.client_id, safe="")
    config_url = f"{base}/api/clients/{client_path}/displays/{args.display_index}/config"
    failures: list[str] = []
    state: dict = {}

    def server_config():
        state["server_config"] = get_json(config_url, context)
        payload = state["server_config"]
        revision = payload.get("active_layout_revision")
        if payload.get("client_id") != args.client_id:
            raise VerificationError("server returned a different client identity")
        if int(payload.get("display_index", -1)) != args.display_index:
            raise VerificationError("server returned a different display index")
        if revision != args.expected_revision:
            raise VerificationError(f"expected revision {args.expected_revision}, observed {revision}")
        if args.before_revision is not None and revision <= args.before_revision:
            raise VerificationError(f"revision {revision} did not advance beyond {args.before_revision}")
        layout = payload.get("layout") or {}
        if not layout.get("id"):
            raise VerificationError("server returned no assigned layout")
        return f"revision={revision}, layout_id_present"

    def display_metadata():
        path = cache_dir / f"display-config-{args.display_index}.json"
        payload = read_json(path)
        revision = payload.get("active_layout_revision")
        if revision != args.expected_revision:
            raise VerificationError(f"{path.name} has revision {revision}, expected {args.expected_revision}")
        return f"{path.name} revision={revision}"

    def layout_cache():
        path = cache_dir / f"assigned-layout-display-{args.display_index}.json"
        payload = read_json(path)
        expected = state["server_config"].get("layout") or {}
        if payload.get("id") != expected.get("id"):
            raise VerificationError(f"layout cache ID {payload.get('id')} differs from server config")
        if payload.get("updated_at") != expected.get("updated_at"):
            raise VerificationError("layout cache updated_at differs from server config")
        return f"{path.name} matches assigned layout"

    def served_player():
        html = get_text(f"{base}/player.html", context)
        match = re.search(r"/js/player\.js(?:\?v=([^\"']+))?", html)
        if not match:
            raise VerificationError("player.html does not reference player.js")
        script_url = urljoin(f"{base}/", match.group(0).lstrip("/"))
        script = get_text(script_url, context)
        for marker in ("active_layout_revision", ":revision:"):
            if marker not in script:
                raise VerificationError(f"served player.js lacks {marker}")
        return f"player.js cache-bust={match.group(1) or 'none'} revision-aware"

    def browser_process():
        rc, output = run_command(["ps", "-eo", "args"])
        if rc != 0:
            raise VerificationError("ps failed")
        marker = f"screentra-cast-display-{args.display_index}"
        rows = [line for line in output.splitlines() if marker in line and "player.html" in line]
        if not rows:
            raise VerificationError("no running kiosk browser for this display")
        if not any(args.client_id in line for line in rows):
            raise VerificationError("running kiosk browser belongs to a different client")
        return "kiosk browser uses player.html"

    def services():
        units = [
            "screentra-client-actor.service",
            "screentra-client-heartbeat.service",
            "screentra-cast-kiosk@0.service",
            "screentra-cast-endurance.service",
        ]
        for unit in units:
            rc, output = run_command(["systemctl", "is-active", unit])
            if rc != 0 or output.strip() != "active":
                raise VerificationError(f"{unit} is {output.strip() or 'unavailable'}")
        return "actor, heartbeat, kiosk, and endurance active"

    def pending_command():
        path = cache_dir / "pending-command.json"
        if path.exists():
            payload = read_json(path)
            raise VerificationError(f"pending command remains: {payload.get('action', 'unknown')}")
        return "pending-command.json absent"

    def journal_probe():
        if args.skip_journal:
            return "skipped by request"
        if not args.since:
            return "skipped (pass --since to verify post-publish config polling)"
        command = ["journalctl", "-u", "screentra-cast-server.service", "--since", args.since, "--no-pager"]
        rc, output = run_command(command)
        if rc != 0:
            rc, output = run_command(["sudo", "-n", *command])
        if rc != 0:
            raise VerificationError("journalctl unavailable; rerun with --skip-journal if access is intentionally restricted")
        needle = f"/api/clients/{args.client_id}/displays/{args.display_index}/config"
        if needle not in output or "status=200" not in output:
            raise VerificationError("no successful post-publish display-config request in server journal")
        return "successful display-config request observed after publish"

    check("server display configuration", server_config, failures)
    check("display metadata cache", display_metadata, failures)
    if "server_config" in state:
        check("display layout cache", layout_cache, failures)
    check("served player assets", served_player, failures)
    check("running player process", browser_process, failures)
    check("client services", services, failures)
    check("pending command", pending_command, failures)
    check("post-publish config polling", journal_probe, failures)

    if failures:
        print("\nPUBLISH LIVE VERIFICATION FAILED")
        for failure in failures:
            print(f"- {failure}")
        print("This test does not claim physical-pixel confirmation; it identifies the first stale transport/runtime stage.")
        return 1
    print("\nPUBLISH LIVE VERIFICATION PASSED")
    print("Server revision, client caches, served player code, browser process, and service path agree.")
    print("Physical-pixel output still requires visual confirmation.")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except VerificationError as exc:
        print(f"FAIL setup: {exc}", file=sys.stderr)
        raise SystemExit(1)
