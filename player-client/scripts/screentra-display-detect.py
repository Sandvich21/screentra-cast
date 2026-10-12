#!/usr/bin/env python3
"""Detect displays on a Linux box by reading /sys/class/drm.

Each connector is a display_index in stable sort order. Returns a list of dicts
ready to send as the 'displays' field in heartbeat payload.
"""
from __future__ import annotations

import os
from pathlib import Path
from typing import List, Dict, Any, Optional


def _read(path: Path) -> str:
    try:
        return path.read_text().strip()
    except Exception:
        return ""


def _read_int(path: Path) -> Optional[int]:
    try:
        return int(_read(path))
    except Exception:
        return None


def _read_float(path: Path) -> Optional[float]:
    try:
        return float(_read(path))
    except Exception:
        return None


def detect_displays(max_displays: int = 8) -> List[Dict[str, Any]]:
    """Return a list of detected DRM connector displays.

    Each entry: { display_index, connector, label, width, height, refresh_rate, enabled, connected }
    Returns an empty list when no physical DRM connector is present. Virtual
    displays are not supported.
    """
    drm_root = Path('/sys/class/drm')
    displays: List[Dict[str, Any]] = []

    if drm_root.exists():
        connectors = []
        for entry in sorted(drm_root.iterdir()):
            name = entry.name
            # cardN-CONNECTOR-N
            if not name.startswith('card') or '-' not in name:
                continue
            status = _read(entry / 'status')
            enabled = _read(entry / 'enabled') == 'enabled'
            # Skip disconnected or disabled unless no other connectors exist
            connectors.append({'name': name, 'entry': entry, 'status': status, 'enabled': enabled})

        # Prefer connected+enabled connectors first
        connectors.sort(key=lambda c: (0 if (c['status'] == 'connected' and c['enabled']) else 1, c['name']))

        for idx, conn in enumerate(connectors):
            if idx >= max_displays:
                break
            entry = conn['entry']
            connector = conn['name'].split('-', 1)[1] if '-' in conn['name'] else conn['name']
            width = _read_int(entry / 'modes') and None
            # best-effort parse modes file (similar to display.rs parse_mode_line)
            width_val = None
            height_val = None
            try:
                first_mode = _read(entry / 'modes').splitlines()[0] if (entry / 'modes').exists() else ''
                if 'x' in first_mode:
                    parts = first_mode.split('x')
                    width_val = int(parts[0])
                    height_val = int(parts[1])
            except Exception:
                pass
            label = f"{connector} ({conn['status']})"
            displays.append({
                'display_index': idx,
                'connector': connector,
                'label': label,
                'width': width_val,
                'height': height_val,
                'refresh_rate': 60.0 if conn['enabled'] else None,
                'connected': conn['status'] == 'connected',
                'enabled': conn['enabled'] and conn['status'] == 'connected',
                'last_error': None,
            })

    # No physical connector means no supported display. Never synthesize a
    # virtual output: the player is a physical-display-only appliance.
    return displays
