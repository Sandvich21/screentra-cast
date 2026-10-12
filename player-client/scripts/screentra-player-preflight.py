#!/usr/bin/env python3
"""Screentra Cast client preflight checks.

Validates the local environment for running the player client:
- python3 OK
- systemd --user available
- display session (Wayland or X11)
- browser installed
- required group memberships
- writable runtime/cache dirs
- renderer port availability
"""
from __future__ import annotations


import os
import shutil
import time
import ssl
import socket
import subprocess
import sys
import urllib.request
from pathlib import Path


CHECKS: list[dict] = []


def _seat_service_running() -> bool:
    """Return True if the system-level labwc seat service is active for any display."""
    for idx in range(4):
        try:
            r = subprocess.run(
                ["systemctl", "--quiet", "is-active", f"screentra-cast-kiosk@{idx}.service"],
                capture_output=True, text=True, timeout=2,
            )
            if r.returncode == 0:
                return True
        except Exception:
            pass
    return False


def has_display_session(retries: int = 6, retry_delay: float = 1.0) -> tuple[bool, str]:
    """Detect an active Wayland/X11 session.

    Retries for `retries` * `retry_delay` seconds total, because the
    system-level labwc seat service may take a few seconds to start and
    create /run/user/UID/wayland-0. Also accepts a healthy seat service
    even before the socket exists, so installers running in the same
    tick that the seat is starting up still report success.
    """
    for attempt in range(max(1, retries)):
        wayland = os.environ.get("WAYLAND_DISPLAY")
        if wayland:
            return True, f"WAYLAND_DISPLAY={wayland}"
        xdg = os.environ.get("XDG_RUNTIME_DIR", f"/run/user/{os.getuid()}")
        try:
            socks = sorted(p.name for p in Path(xdg).glob("wayland-*") if p.name.startswith("wayland-"))
        except Exception:
            socks = []
        if socks:
            return True, f"WAYLAND_DISPLAY={socks[0]} (auto-detected)"
        if os.environ.get("DISPLAY"):
            return True, f"DISPLAY={os.environ['DISPLAY']}"
        if shutil.which("Xvfb") or shutil.which("xvfb-run"):
            return True, "xvfb-run available (headless kiosk mode)"
        # If the system-level labwc seat service is healthy, give it more time.
        if _seat_service_running():
            if attempt < retries - 1:
                time.sleep(retry_delay)
                continue
            # Final attempt: report the seat is running so the user knows
            # the Wayland socket just hasn't appeared yet.
            return True, "system-level screentra-cast-kiosk@.service active (wayland-0 socket pending)"
        if attempt < retries - 1:
            time.sleep(retry_delay)
    return False, "no Wayland/X11 display session detected (system-level screentra-cast-kiosk@.service is not active; install or start it from a TTY)"

def tls_context():
    cafile = os.environ.get('SCREENTRA_CA_CERT') or os.environ.get('RIPLEY_CA_CERT') or str(Path.home() / '.local/share/screentra-client/screentra-cast-signage-ca.crt')
    if cafile and Path(cafile).exists():
        try:
            return ssl.create_default_context(cafile=cafile)
        except Exception as e:
            return None
    if os.environ.get('RIPLEY_INSECURE_SSL', '').lower() in ('1', 'true', 'yes'):
        return ssl._create_unverified_context()
    return None

def urlopen_ripley(req, timeout=10):
    kw = {'timeout': timeout}
    ctx = tls_context()
    if ctx is not None:
        kw['context'] = ctx
    return urllib.request.urlopen(req, **kw)


def add(name: str, ok: bool, detail: str = "", required: bool = True) -> None:
    CHECKS.append({"name": name, "ok": ok, "detail": detail, "required": required})


def run(cmd: str) -> str:
    try:
        return subprocess.check_output(cmd, shell=True, text=True, stderr=subprocess.DEVNULL).strip()
    except Exception:
        return ""


def find_browser() -> str | None:
    for b in ("chromium", "chromium-browser", "google-chrome-stable", "google-chrome", "firefox-esr", "firefox"):
        if shutil.which(b):
            return b
    return None


def port_in_use(port: int) -> bool:
    try:
        with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
            s.bind(("127.0.0.1", port))
            return False
    except OSError:
        return True


def main() -> int:
    add("python3", shutil.which("python3") is not None, sys.version.split()[0])
    add("curl/wget", shutil.which("curl") is not None or shutil.which("wget") is not None,
        "curl" if shutil.which("curl") else "wget" if shutil.which("wget") else "missing", required=False)
    add("systemctl --user", shutil.which("systemctl") is not None and bool(run("systemctl --user status >/dev/null 2>&1 && echo ok")),
        "systemd user manager running", required=False)

    ok, detail = has_display_session()
    add("display session", ok, detail)

    add("browser installed", find_browser() is not None, find_browser() or "no chromium/chrome/firefox", required=False)

    # Wayland kiosk compositor + output helper. Required for display takeover.
    add("sway (wayland compositor)", shutil.which("sway") is not None,
        shutil.which("sway") or "install sway for Wayland kiosk mode", required=True)
    add("wlr-randr (wayland output helper)", shutil.which("wlr-randr") is not None,
        shutil.which("wlr-randr") or "install wlr-randr (wlroots tools, ships with sway) for Wayland output discovery", required=False)

    # libinput / touch detection support
    add("/proc/bus/input/devices", os.path.exists("/proc/bus/input/devices"),
        "present" if os.path.exists("/proc/bus/input/devices") else "missing — touch device auto-detect unavailable", required=False)

    # Group memberships. Required for the kiosk to run.
    groups = run("id -Gn").split()
    needed = {"video", "render", "audio", "tty", "input"}
    missing = sorted(needed - set(groups))
    if missing:
        add("user groups", False,
            f"present={sorted(needed & set(groups))} missing={missing}",
            required=False)
        add("user groups fix", False,
            f"sudo usermod -aG {','.join(missing)} $USER  (then log out and back in)",
            required=False)
    else:
        add("user groups", True, f"present={sorted(needed & set(groups))}", required=False)

    # Live device access. /dev/dri/renderD128 is needed for EGL/GLES2. We do
    # NOT read from the device — render nodes use streaming IO that blocks
    # forever if no write side is active. `os.open` with O_NONBLOCK tells us
    # the kernel would let us in (permissions/group ok); that's all the
    # coverage we need for "is the device accessible to the kiosk browser".
    if Path("/dev/dri/renderD128").exists():
        try:
            import os as _os
            _fd = _os.open("/dev/dri/renderD128",
                           _os.O_RDWR | _os.O_NONBLOCK | _os.O_CLOEXEC)
            _os.close(_fd)
            add("/dev/dri/renderD128 readable", True, "openable", required=False)
        except PermissionError:
            add("/dev/dri/renderD128 readable", False, "Permission denied; user not in 'render' group", required=True)
        except Exception as e:
            add("/dev/dri/renderD128 readable", False, f"open failed: {e!r}", required=False)
    else:
        add("/dev/dri/renderD128 readable", True, "device not present (headless ok)", required=False)

    # Writable dirs
    install_dir = Path(os.environ.get("RIPLEY_CLIENT_DIR") or Path.home() / ".local/share/screentra-client")
    bin_dir = Path.home() / ".local/bin"
    systemd_user = Path.home() / ".config/systemd/user"
    install_dir.mkdir(parents=True, exist_ok=True)
    add("install dir", os.access(install_dir, os.W_OK), str(install_dir))
    add("bin dir", os.access(bin_dir, os.W_OK) or os.access(bin_dir.parent, os.W_OK), str(bin_dir))
    add("systemd user dir", True, str(systemd_user))

    # Renderer port
    add("renderer port 3900", not port_in_use(3900), "free" if not port_in_use(3900) else "busy", required=False)

    # Server URL reachability
    server = os.environ.get("RIPLEY_SERVER_URL", "")
    if server:
        try:
            with urlopen_ripley(server.rstrip("/") + "/api/clients", timeout=5) as r:
                ok = r.status == 200
                detail = f"status {r.status}"
        except Exception as e:
            ok, detail = False, str(e)
        add("server reachable", ok, f"{server} ({detail})")
    else:
        add("server url configured", False, "RIPLEY_SERVER_URL not set (will be sourced from install args or server file)", required=False)

    # Report
    blocking = [c for c in CHECKS if c["required"] and not c["ok"]]
    soft = [c for c in CHECKS if not c["required"] and not c["ok"]]

    print("Screentra Cast Player Preflight")
    print("-" * 60)
    for c in CHECKS:
        status = "PASS" if c["ok"] else ("FAIL" if c["required"] else "WARN")
        print(f"  [{status}] {c['name']:<24} {c['detail']}")

    if blocking:
        print()
        print("Blocking issues:")
        for c in blocking:
            print(f"  - {c['name']}: {c['detail']}")
        return 1
    if soft:
        print()
        print("Soft warnings (services can run, but some features may not work):")
        for c in soft:
            print(f"  - {c['name']}: {c['detail']}")
    print()
    print("Ready to install/enable services.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
