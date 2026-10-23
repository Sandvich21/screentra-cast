#!/usr/bin/env python3
"""Screentra Cast player display governance.

Counts connected DRM connectors and writes `config.json` next to the player
runtime state.

Idempotent: safe to re-run after a hotplug or display reconfiguration.
"""
from __future__ import annotations

import argparse
import json
import os
import stat
import sys
import tempfile
from pathlib import Path
from typing import Any, Dict, List

# Local import without circular dep risk. The detect module ships as a
# hyphenated filename (`screentra-display-detect.py`) so it isn't a normal
# importable name; load it via importlib.
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import importlib.util as _ilu
_det_path = os.path.join(os.path.dirname(os.path.abspath(__file__)),
                         "screentra-display-detect.py")
_spec = _ilu.spec_from_file_location("screentra_display_detect", _det_path)
if _spec is None or _spec.loader is None:
    raise RuntimeError(f"cannot load detect module from {_det_path}")
_det = _ilu.module_from_spec(_spec)
sys.modules["screentra_display_detect"] = _det
_spec.loader.exec_module(_det)


CONFIG_DEFAULTS: Dict[str, Any] = {
    # Number of connected+enabled DRM connectors the installer found. The
    # renderer / kiosk-launcher uses this as the upper bound. Re-detected on
    # every run; never mutated by the actor.
    "installer_detected_max_displays": 0,
    # Optional operator-set cap (per-box). None means every connected physical
    # display is eligible, including displays with no touch/input device.
    "max_displays": None,
    # Cache of the most recent detection for offline debugging. Useful when
    # the user opens /admin and the display count looks wrong after a
    # hotplug or cable swap.
    "last_detected_displays": [],
}


def detect_count(max_scan: int = 16) -> int:
    """Return the count of connected physical DRM connectors, capped at max_scan.

    Count connected connectors even if currently disabled in the compositor, so
    an operator can re-enable DP-4 after touchscreen-only mode turned it off.
    """
    displays = _det.detect_displays(max_displays=max_scan)
    return sum(1 for d in displays if d.get("connected", d.get("enabled")))


def detect_full(max_scan: int = 16) -> List[Dict[str, Any]]:
    """Return the raw detection list (same shape as heartbeat 'displays' field)."""
    return _det.detect_displays(max_displays=max_scan)


def _ensure_real_directory(path: Path, create: bool = False) -> bool:
    absolute = Path(os.path.abspath(os.fspath(path)))
    current = Path(absolute.anchor)
    for part in absolute.parts[1:]:
        current /= part
        try:
            metadata = os.lstat(current)
        except FileNotFoundError:
            if not create:
                return False
            current.mkdir()
            metadata = os.lstat(current)
        if stat.S_ISLNK(metadata.st_mode) or not stat.S_ISDIR(metadata.st_mode):
            raise ValueError('display governance directory is not a real directory')
    return True


def _read_real_file(path: Path, maximum: int = 1 << 20) -> bytes:
    absolute = Path(os.path.abspath(os.fspath(path)))
    current = Path(absolute.anchor)
    for part in absolute.parts[1:]:
        current /= part
        metadata = os.lstat(current)
        if stat.S_ISLNK(metadata.st_mode):
            raise ValueError('display governance path contains a symlink')
    descriptor = os.open(absolute, os.O_RDONLY | getattr(os, 'O_NOFOLLOW', 0))
    try:
        metadata = os.fstat(descriptor)
        if not stat.S_ISREG(metadata.st_mode) or metadata.st_size > maximum:
            raise ValueError('display governance file is not bounded')
        raw = os.read(descriptor, maximum + 1)
    finally:
        os.close(descriptor)
    if len(raw) > maximum:
        raise ValueError('display governance file exceeds its bound')
    return raw


def _reject_duplicate_json_keys(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError('duplicate JSON object key')
        result[key] = value
    return result


def load_config(config_path: Path) -> Dict[str, Any]:
    """Load config.json (or return defaults) without writing."""
    if not config_path.exists():
        return dict(CONFIG_DEFAULTS)
    try:
        cfg = json.loads(
            _read_real_file(config_path).decode('utf-8'),
            object_pairs_hook=_reject_duplicate_json_keys,
            parse_constant=lambda _: (_ for _ in ()).throw(ValueError('non-standard JSON constant')),
        )
    except Exception:
        # Corrupt config.json → return defaults but don't clobber; caller decides.
        return dict(CONFIG_DEFAULTS)
    out = dict(CONFIG_DEFAULTS)
    if isinstance(cfg, dict):
        out.update(cfg)
    return out


def write_config(config_path: Path, *, installer_detected_max_displays: int,
                 max_displays, last_detected_displays: List[Dict[str, Any]]) -> None:
    """Atomically write config.json. max_displays=None means "no override".

    Preserve operator-managed keys such as touch_binding; display-governance owns
    only the display-count/detection fields.
    """
    cfg = load_config(config_path)
    cfg.update({
        "installer_detected_max_displays": int(installer_detected_max_displays),
        "max_displays": max_displays,
        "last_detected_displays": last_detected_displays,
    })
    _ensure_real_directory(config_path.parent, create=True)
    with tempfile.NamedTemporaryFile(
        mode="w",
        dir=config_path.parent,
        prefix=f".{config_path.name}.",
        suffix=".tmp",
        delete=False,
    ) as stream:
        json.dump(cfg, stream, indent=2)
        tmp = Path(stream.name)
    os.chmod(tmp, 0o600)
    os.replace(tmp, config_path)


def compute_active_max(config: Dict[str, Any]) -> int:
    """The value the renderer / kiosk-launcher should actually use."""
    installer = max(int(config.get("installer_detected_max_displays") or 0), 0)
    override = config.get("max_displays")
    if override is None:
        return installer
    try:
        override_i = max(int(override), 0)
    except (TypeError, ValueError):
        return installer
    # Operator cap cannot exceed what the hardware actually has.
    return min(installer, override_i)


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--config-dir", default=os.environ.get(
        os.environ.get("SCREENTRA_CLIENT_DIR") or os.environ.get("RIPLEY_CLIENT_DIR") or os.path.expanduser("~/.local/share/screentra-client")))
    ap.add_argument("--max-scan", type=int, default=16)
    ap.add_argument("--print-only", action="store_true",
                    help="Detect and print to stdout; do not write config.json")
    args = ap.parse_args()

    displays = detect_full(max_scan=args.max_scan)
    connected_count = sum(1 for d in displays if d.get("connected", d.get("enabled")))

    config_path = Path(args.config_dir) / "config.json"
    existing = load_config(config_path)
    max_displays_override = existing.get("max_displays")
    # None intentionally means all connected physical displays. Touch binding
    # is configured independently and must never decide whether a display can
    # render signage.

    if args.print_only:
        print(json.dumps({
            "installer_detected_max_displays": connected_count,
            "max_displays": max_displays_override,
            "active_max_displays": compute_active_max(
                {"installer_detected_max_displays": connected_count,
                 "max_displays": max_displays_override}),
            "displays": displays,
        }, indent=2))
        return 0

    write_config(config_path,
                 installer_detected_max_displays=connected_count,
                 max_displays=max_displays_override,
                 last_detected_displays=displays)
    print(f"display-governance: wrote {config_path} (installed_max={connected_count}, "
          f"max_displays={'unset' if max_displays_override is None else max_displays_override})",
          flush=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
