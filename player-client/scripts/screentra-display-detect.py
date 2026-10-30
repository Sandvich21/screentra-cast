#!/usr/bin/env python3
"""Detect displays on a Linux box by reading /sys/class/drm.

Each connector is a display_index in stable sort order. Returns a list of dicts
ready to send as the 'displays' field in heartbeat payload.
"""
from __future__ import annotations

import os
import re
from pathlib import Path
from typing import List, Dict, Any, Optional


MAX_SYSFS_VALUE_BYTES = 4096
MAX_DISPLAYS = 8


def _read(path: Path) -> str:
    try:
        with path.open('rb') as handle:
            return handle.read(MAX_SYSFS_VALUE_BYTES).decode('utf-8', errors='replace').strip()
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


def _connector_sort_key(connector: Dict[str, Any]) -> tuple[int, str]:
    """Keep physical connected outputs ahead of disconnected connectors."""
    status = connector.get('status')
    enabled = bool(connector.get('enabled'))
    if status == 'connected' and enabled:
        priority = 0
    elif status == 'connected':
        priority = 1
    else:
        priority = 2
    return priority, str(connector.get('name') or '')


def _gpu_metadata(entry: Path) -> Dict[str, Optional[str]]:
    """Return stable, non-secret GPU identity for a DRM connector."""
    card = entry.name.split('-', 1)[0]
    drm_device = card
    try:
        for candidate in sorted(Path('/dev/dri/by-path').glob(f'pci-*-{card}')):
            if candidate.name.endswith('-card') and candidate.exists():
                drm_device = str(candidate)
                break
    except Exception:
        pass
    device = entry / 'device'
    driver = ''
    try:
        driver = os.path.basename(os.path.realpath(device / 'driver'))
    except Exception:
        pass
    vendor = _read(device / 'vendor') or None
    device_id = _read(device / 'device') or None
    pci_match = re.search(r'pci-[^-]+-[^-]+-[^-]+', drm_device)
    gpu_id = pci_match.group(0) if pci_match else drm_device
    gpu_name = {
        '0x1002': 'AMD', '0x10de': 'NVIDIA', '0x8086': 'Intel',
    }.get((vendor or '').lower(), vendor or 'Unknown GPU')
    return {
        'gpu': gpu_name,
        'gpu_driver': driver or None,
        'gpu_vendor': vendor,
        'gpu_device': device_id,
        'drm_device': drm_device,
        'gpu_id': gpu_id,
    }


def display_key(display: Dict[str, Any]) -> str:
    """Stable blacklist key: DRM device identity plus connector."""
    return str(display.get('display_key') or f"{display.get('drm_device') or display.get('connector') or ''}|{display.get('connector') or ''}")


def detect_displays(max_displays: int = 8, blacklisted_keys=None) -> List[Dict[str, Any]]:
    """Return a list of detected DRM connector displays.

    Each entry: { display_index, connector, label, width, height, refresh_rate, enabled, connected }
    Returns an empty list when no physical DRM connector is present. Virtual
    displays are not supported.
    """
    drm_root = Path('/sys/class/drm')
    displays: List[Dict[str, Any]] = []
    try:
        display_limit = max(0, min(int(max_displays), MAX_DISPLAYS))
    except (TypeError, ValueError):
        display_limit = MAX_DISPLAYS

    blacklist = {str(value) for value in (blacklisted_keys or []) if isinstance(value, str) and value}
    if drm_root.exists():
        connectors = []
        for entry in sorted(drm_root.iterdir()):
            name = entry.name
            # cardN-CONNECTOR-N
            if not name.startswith('card') or '-' not in name:
                continue
            status = _read(entry / 'status')
            enabled = _read(entry / 'enabled') == 'enabled'
            # Retain disconnected connectors for stable bounded indexing; the
            # sort below keeps physical connected outputs first.
            connectors.append({'name': name, 'entry': entry, 'status': status, 'enabled': enabled})

        # Prefer connected outputs, with enabled outputs first.
        connectors.sort(key=_connector_sort_key)

        for idx, conn in enumerate(connectors):
            if idx >= display_limit:
                break
            entry = conn['entry']
            connector = conn['name'].split('-', 1)[1] if '-' in conn['name'] else conn['name']
            gpu = _gpu_metadata(entry)
            key = f"{gpu['drm_device']}|{connector}"
            if key in blacklist:
                continue
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
                **gpu,
                'display_key': key,
            })

    # No physical connector means no supported display. Never synthesize a
    # virtual output: the player is a physical-display-only appliance.
    return displays
