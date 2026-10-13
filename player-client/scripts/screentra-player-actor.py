#!/usr/bin/env python3
"""Screentra Cast client command actor.

Reads a pending command file written by the heartbeat agent and executes it
locally on the player device. Actions are mapped to systemctl --user calls
or cache refreshes; results are written to command-result.json for the
heartbeat to surface back to the server.
"""
from __future__ import annotations

import argparse
import datetime
import datetime as _d
import json
import os
import re
import shutil
import ssl
import subprocess
import sys
import tempfile
import time
import urllib.parse
import urllib.request
from pathlib import Path


def _atomic_write_text(path: Path, text: str, mode: int = 0o600) -> None:
    """Replace a cache JSON file atomically and keep it owner-readable."""
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(f'.{path.name}.{os.getpid()}.tmp')
    temporary.write_text(text)
    os.chmod(temporary, mode)
    os.replace(temporary, path)


def _run_list(args: list[str], timeout: int = 20, input_data: str | None = None) -> tuple[int, str, str]:
    try:
        p = subprocess.run(args, capture_output=True, text=True, timeout=timeout, input=input_data)
        return p.returncode, (p.stdout or "").strip(), (p.stderr or "").strip()
    except Exception as e:
        return 1, "", str(e)


def ddcutil_detect(bus: str | None = None) -> tuple[bool, str]:
    """Run ddcutil detect to refresh display cache. Returns (ok, message)."""
    args = ['sudo', '-n', 'ddcutil', 'detect']
    if bus:
        args.extend(['--bus', bus])
    rc, out, err = _run_list(args)
    if rc == 0:
        return True, out.strip()
    return False, err.strip() or out.strip()


def apply_ddc_control(vcp_code: str, value: int, bus: str | None = None, has_layout: bool = False) -> tuple[bool, str]:
    """Set a DDC/CI VCP value via ddcutil. Returns (ok, message).
    If has_layout=True and the display is not initially detected (ddcutil_no_bus),
    runs ddcutil detect once and retries before returning failure."""
    args = ['sudo', '-n', 'ddcutil', 'setvcp']
    if bus:
        args.extend(['--bus', bus])
    args.extend([vcp_code, str(value)])

    def _do_set():
        rc, out, err = _run_list(args)
        if rc == 0:
            return True, f'DDC: {vcp_code}={value}' + (f' ({out.strip()})' if out.strip() else '')
        err_msg = err.strip() or out.strip()
        if 'not found' in err_msg.lower() or 'no such file' in err_msg.lower():
            return False, f'ddcutil_not_installed: {err_msg}'
        if 'unable to open' in err_msg.lower() or 'i2c' in err_msg.lower() or 'no bus' in err_msg.lower():
            return False, 'ddcutil_no_bus', err_msg
        if 'communication' in err_msg.lower() or 'timeout' in err_msg.lower():
            return False, f'ddcutil_no_support: {err_msg}'
        return False, f'DDC failed (rc={rc}): {err_msg}'

    ok, msg_or_tag, *extra = _do_set()
    if ok:
        return True, msg_or_tag

    # Retry once if has_layout and display was not found
    if has_layout and msg_or_tag == 'ddcutil_no_bus':
        err_msg = extra[0] if extra else ''
        ddcutil_detect(bus)
        ok2, msg2 = _do_set()
        if ok2:
            return True, f'{msg2} (re-detected)'
        return False, f'{msg_or_tag}: {err_msg} — retry after re-detection failed: {msg2}'

    return False, msg_or_tag


def apply_cec_command(cec_cmd: str) -> tuple[bool, str]:
    """Send a CEC command via cec-client. Returns (ok, message)."""
    rc, out, err = _run_list(
        ['sudo', '-n', 'cec-client', '-d', '1', '-s'],
        input_data=f'{cec_cmd}\n',
    )
    if rc == 0:
        return True, f'CEC: {cec_cmd}' + (f' — {out.strip()}' if out.strip() else '')
    return False, f'CEC failed (rc={rc}): {err.strip() or out.strip()}'


def _bluetoothctl_devices(text: str) -> list[dict]:
    devices = []
    seen = set()
    for line in (text or '').splitlines():
        match = re.match(r'^\s*Device\s+([0-9A-Fa-f:]{17})\s*(.*)$', line)
        if not match:
            continue
        address = match.group(1).upper()
        if address in seen:
            continue
        seen.add(address)
        devices.append({'address': address, 'name': match.group(2).strip() or address})
    return devices


def bluetooth_scan(payload: dict | None) -> tuple[bool, str, dict]:
    payload = payload or {}
    duration = max(5, min(int(payload.get('duration_seconds') or 10), 30))
    if shutil.which('bluetoothctl') is None:
        return False, 'bluetoothctl is not installed on this client', {'devices': []}
    _run_list(['bluetoothctl', 'power', 'on'], timeout=10)
    rc, out, err = _run_list(['bluetoothctl', '--timeout', str(duration), 'scan', 'on'], timeout=duration + 10)
    _, listed, list_err = _run_list(['bluetoothctl', 'devices'], timeout=10)
    devices = _bluetoothctl_devices(listed or out)
    if not devices and rc != 0:
        return False, f'Bluetooth scan failed: {err or out or list_err or "unknown error"}', {'devices': []}
    return True, f'Bluetooth scan found {len(devices)} device(s)', {'devices': devices, 'duration_seconds': duration}


def _find_bluetooth_sink(address: str, name: str | None) -> str | None:
    if shutil.which('wpctl') is None:
        return None
    _, output, _ = _run_list(['wpctl', 'status'], timeout=10)
    address_key = re.sub(r'[^0-9a-f]', '', address.lower())
    name_key = (name or '').strip().lower()
    for line in (output or '').splitlines():
        if not re.search(r'bluez_(?:output|sink)', line, re.IGNORECASE):
            continue
        normalized = re.sub(r'[^0-9a-f]', '', line.lower())
        if address_key and address_key not in normalized and name_key not in line.lower():
            continue
        match = re.match(r'^\s*[│*]*\s*(\d+)\.\s+', line)
        if match:
            return match.group(1)
    return None


def bluetooth_connect(payload: dict | None) -> tuple[bool, str, dict]:
    payload = payload or {}
    address = str(payload.get('address') or '').strip().upper()
    name = str(payload.get('name') or '').strip() or address
    if not re.fullmatch(r'[0-9A-F]{2}(?::[0-9A-F]{2}){5}', address):
        return False, 'A valid Bluetooth device address is required', {}
    if shutil.which('bluetoothctl') is None:
        return False, 'bluetoothctl is not installed on this client', {}
    _run_list(['bluetoothctl', 'power', 'on'], timeout=10)
    rc, out, err = _run_list(['bluetoothctl', 'connect', address], timeout=30)
    text = '\n'.join(v for v in (out, err) if v)
    connected = rc == 0 and not re.search(r'failed|not available|error', text, re.IGNORECASE)
    details = {'address': address, 'name': name, 'connected': connected, 'bluetooth_output': text[-1200:]}
    if not connected:
        return False, f'Bluetooth connection failed: {text or f"exit code {rc}"}', details
    time.sleep(1)
    sink_id = _find_bluetooth_sink(address, name)
    if sink_id and shutil.which('wpctl'):
        sink_rc, sink_out, sink_err = _run_list(['wpctl', 'set-default', sink_id], timeout=10)
        details['audio_sink_id'] = sink_id
        details['audio_device_selected'] = sink_rc == 0
        if sink_rc == 0:
            return True, f'Connected to {name}; Bluetooth audio output selected', details
        details['audio_selection_error'] = sink_err or sink_out
    else:
        details['audio_device_selected'] = False
        details['audio_selection_error'] = 'Bluetooth connected, but no PipeWire Bluetooth audio sink was found yet'
    return True, f'Connected to {name}; audio output selection may require a short PipeWire delay', details


def _wpctl_sinks(text: str) -> list[dict]:
    sinks = []
    in_sinks = False
    for line in (text or '').splitlines():
        stripped = line.strip()
        if re.search(r'\b(?:Audio )?Sinks:', stripped, re.IGNORECASE):
            in_sinks = True
            continue
        if in_sinks and re.search(r'\b(?:Devices|Sources|Filters|Streams|Audio Sources):', stripped, re.IGNORECASE):
            break
        if not in_sinks:
            continue
        match = re.search(r'(?<!\d)(\d+)\.\s+(.+?)\s*$', line)
        if not match:
            continue
        sink_id = match.group(1)
        name = re.sub(r'^\*\s*', '', match.group(2)).strip()
        name = re.sub(r'\s+\[vol:.*?\]\s*$', '', name).strip()
        if not any(item['id'] == sink_id for item in sinks):
            sinks.append({'id': sink_id, 'name': name, 'default': '*' in line[:match.start(2)]})
    return sinks


def audio_list() -> tuple[bool, str, dict]:
    if shutil.which('wpctl') is None:
        return False, 'wpctl is not installed on this client', {'devices': []}
    rc, out, err = _run_list(['wpctl', 'status'], timeout=10)
    devices = _wpctl_sinks(out)
    if rc != 0:
        return False, f'Audio device scan failed: {err or out or f"exit code {rc}"}', {'devices': devices}
    return True, f'Found {len(devices)} audio output device(s)', {'devices': devices}


def audio_set_default(payload: dict | None) -> tuple[bool, str, dict]:
    payload = payload or {}
    sink_id = str(payload.get('sink_id') or '').strip()
    if not sink_id.isdigit():
        return False, 'A valid audio output device is required', {}
    if shutil.which('wpctl') is None:
        return False, 'wpctl is not installed on this client', {}
    rc, out, err = _run_list(['wpctl', 'set-default', sink_id], timeout=10)
    if rc == 0:
        return True, f'Audio output selected: {sink_id}', {'sink_id': sink_id}
    return False, f'Audio output selection failed: {err or out or f"exit code {rc}"}', {'sink_id': sink_id}


def audio_set_volume(payload: dict | None) -> tuple[bool, str, dict]:
    payload = payload or {}
    sink_id = str(payload.get('sink_id') or '@DEFAULT_AUDIO_SINK@').strip()
    try:
        value = max(0, min(100, int(payload.get('value'))))
    except (TypeError, ValueError):
        return False, 'Volume must be a number from 0 to 100', {}
    if not sink_id.isdigit() and sink_id != '@DEFAULT_AUDIO_SINK@':
        return False, 'A valid audio output device is required', {}
    if shutil.which('wpctl') is None:
        return False, 'wpctl is not installed on this client', {}
    rc, out, err = _run_list(['wpctl', 'set-volume', sink_id, f'{value}%'], timeout=10)
    if rc == 0:
        return True, f'Client audio volume set to {value}%', {'sink_id': sink_id, 'volume': value}
    return False, f'Client audio volume failed: {err or out or f"exit code {rc}"}', {'sink_id': sink_id, 'volume': value}


def restart_client() -> tuple[bool, str, dict]:
    if shutil.which('systemctl') is None:
        return False, 'systemctl is not available on this client', {}
    rc, out, err = _run_list(['sudo', '-n', 'systemctl', 'reboot'], timeout=20)
    if rc == 0:
        return True, 'Client reboot requested', {'reboot_requested': True}
    return False, f'Client reboot failed: {err or out or f"exit code {rc}"}', {'reboot_requested': False}


def has_systemctl_user() -> bool:
    if shutil.which("systemctl") is None:
        return False
    rc, _, _ = _run_list(["systemctl", "--user", "status"])
    return rc == 0


def systemctl_user(action: str, unit: str) -> tuple[bool, str]:
    rc, out, err = _run_list(["systemctl", "--user", action, unit])
    ok = rc == 0
    return ok, (out or err or f"{action} {unit} rc={rc}").strip()


def restart_system_kiosk() -> tuple[bool, str]:
    """Restart the system kiosk wrapper used by the deployed player.

    Some installations run the renderer and Chromium as children of the
    system-level kiosk service rather than under a persistent systemd --user
    manager. In that layout there is no user D-Bus session for the actor, so
    the user-unit path cannot work. Restarting the kiosk wrapper is the
    supported fallback and brings the renderer/browser pair back together.
    """
    rc, out, err = _run_list(
        ['sudo', '-n', 'systemctl', 'restart', 'screentra-cast-kiosk@0.service'],
        timeout=30,
    )
    return rc == 0, (out or err or f'restart screentra-cast-kiosk@0.service rc={rc}').strip()


def apply_display_count(cache_dir: Path, max_displays) -> tuple[bool, str, dict]:
    """Persist active display count and apply renderer/browser services.

    One labwc compositor owns DRM (system screentra-cast-kiosk@0). Secondary
    displays are Chromium windows attached to wayland-0 via user
    screentra-kiosk-browser@N services; never start extra system labwc seats on a
    single-GPU player.
    """
    install_dir = cache_dir.parent
    config_path = install_dir / 'config.json'
    try:
        cfg = json.loads(config_path.read_text()) if config_path.exists() else {}
    except Exception:
        cfg = {}
    installer = max(int(cfg.get('installer_detected_max_displays') or 1), 1)
    if max_displays is None:
        override = None
        active = min(installer, 4)
    else:
        override = max(1, min(int(max_displays), installer, 4))
        active = override
    cfg['installer_detected_max_displays'] = installer
    cfg['max_displays'] = override
    cfg.setdefault('last_detected_displays', [])
    config_path.parent.mkdir(parents=True, exist_ok=True)
    tmp = config_path.with_suffix('.json.tmp')
    tmp.write_text(json.dumps(cfg, indent=2)); tmp.replace(config_path)

    details = {'config_path': str(config_path), 'installer_detected_max_displays': installer, 'max_displays': override, 'active_max_displays': active, 'steps': []}
    if has_systemctl_user():
        _run_list(['systemctl', '--user', 'daemon-reload'])
        for i in range(4):
            if i < active:
                ok1, msg1 = systemctl_user('enable', f'screentra-client-renderer@{i}.service')
                ok2, msg2 = systemctl_user('restart', f'screentra-client-renderer@{i}.service')
                details['steps'].append({'unit': f'screentra-client-renderer@{i}.service', 'enable': ok1, 'restart': ok2, 'message': msg2 or msg1})
                if i > 0:
                    ok3, msg3 = systemctl_user('enable', f'screentra-kiosk-browser@{i}.service')
                    ok4, msg4 = systemctl_user('restart', f'screentra-kiosk-browser@{i}.service')
                    details['steps'].append({'unit': f'screentra-kiosk-browser@{i}.service', 'enable': ok3, 'restart': ok4, 'message': msg4 or msg3})
            else:
                ok1, msg1 = systemctl_user('stop', f'screentra-client-renderer@{i}.service')
                ok2, msg2 = systemctl_user('disable', f'screentra-client-renderer@{i}.service')
                ok3, msg3 = systemctl_user('stop', f'screentra-kiosk-browser@{i}.service')
                ok4, msg4 = systemctl_user('disable', f'screentra-kiosk-browser@{i}.service')
                details['steps'].append({'unit': f'display-{i}', 'disabled': True, 'messages': [msg1,msg2,msg3,msg4]})
    # Prevent accidental second DRM-master labwc seats; display 1+ attach to wayland-0.
    for i in range(1,4):
        _run_list(['sudo', '-n', 'systemctl', 'disable', '--now', f'screentra-cast-kiosk@{i}.service'])
        details['steps'].append({'unit': f'screentra-cast-kiosk@{i}.service', 'disabled_system_seat': True})

    # Apply compositor output policy live when wayland-0 is available. Do not
    # restart labwc just to change display count; restarting can briefly drop DRM
    # and leave stale Chromium/orphan labwc processes. Boot-time policy remains in
    # screentra-kiosk.sh for reboots.
    try:
        env = os.environ.copy()
        env.setdefault('XDG_RUNTIME_DIR', f'/run/user/{os.getuid()}')
        env.setdefault('WAYLAND_DISPLAY', 'wayland-0')
        out = subprocess.check_output(['wlr-randr', '--json'], text=True, stderr=subprocess.DEVNULL, timeout=3, env=env)
        data = json.loads(out)
        outputs = [o for o in data if o.get('name')]
        outputs.sort(key=lambda o: o.get('name',''))
        x = 0
        for idx, o in enumerate(outputs):
            name = o.get('name')
            cur = next((m for m in o.get('modes', []) if m.get('current')), None)
            pref = next((m for m in o.get('modes', []) if m.get('preferred')), None)
            mode = cur or pref or {}
            width = int(mode.get('width') or 0)
            if idx < active:
                cp = subprocess.run(['wlr-randr', '--output', name, '--on', '--pos', f'{x},0'], text=True, capture_output=True, timeout=5, env=env)
                details['steps'].append({'output': name, 'action': 'on', 'x': x, 'code': cp.returncode, 'stderr': (cp.stderr or '')[-500:]})
                x += width
            elif o.get('enabled'):
                cp = subprocess.run(['wlr-randr', '--output', name, '--off'], text=True, capture_output=True, timeout=5, env=env)
                details['steps'].append({'output': name, 'action': 'off', 'code': cp.returncode, 'stderr': (cp.stderr or '')[-500:]})
    except Exception as e:
        details['steps'].append({'output_policy': 'skipped', 'error': str(e)})
        rc,out,err = _run_list(['sudo', '-n', 'systemctl', 'restart', 'screentra-cast-kiosk@0.service'])
        details['steps'].append({'unit': 'screentra-cast-kiosk@0.service', 'restart_code': rc, 'message': out or err})

    ok = has_systemctl_user()
    return ok, f'active display count set to {active}', details


def _resolve_touch_name(device_path: str | None, device_name: str | None = None) -> str | None:
    if device_name:
        return str(device_name)
    if not device_path:
        return None
    try:
        blocks = Path('/proc/bus/input/devices').read_text().split('\n\n')
        target = Path(device_path).name
        for block in blocks:
            if target not in block:
                continue
            for line in block.splitlines():
                if line.startswith('N:') and 'Name=' in line:
                    return line.split('Name=', 1)[1].strip().strip('"')
    except Exception:
        pass
    return None


def _display_connectors_for_indexes(config: dict, indexes: list[int]) -> list[str]:
    displays = config.get('last_detected_displays') or []
    out = []
    for idx in indexes:
        for d in displays:
            try:
                if int(d.get('display_index', -1)) == int(idx):
                    c = d.get('connector') or d.get('label')
                    if c and c not in out:
                        out.append(str(c))
                    break
            except Exception:
                continue
    return out


def apply_touch_binding(cache_dir: Path, payload: dict) -> tuple[bool, str, dict]:
    install_dir = cache_dir.parent
    config_path = install_dir / 'config.json'
    try:
        cfg = json.loads(config_path.read_text()) if config_path.exists() else {}
    except Exception:
        cfg = {}
    dev_path = payload.get('device_path') or payload.get('path')
    dev_name = _resolve_touch_name(dev_path, payload.get('device_name'))
    allow_joined = bool(payload.get('allow_joined_displays'))
    raw_indexes = payload.get('target_display_indexes') or payload.get('display_indexes') or payload.get('display_index') or [0]
    if not isinstance(raw_indexes, list):
        raw_indexes = [raw_indexes]
    indexes = []
    for v in raw_indexes:
        try:
            i = max(0, min(int(v), 3))
            if i not in indexes:
                indexes.append(i)
        except Exception:
            pass
    if not indexes:
        indexes = [0]
    indexes = indexes[:2] if allow_joined else indexes[:1]
    binding = {
        'device_path': dev_path,
        'device_name': dev_name,
        'target_display_indexes': indexes,
        'target_connectors': _display_connectors_for_indexes(cfg, indexes),
        'allow_joined_displays': allow_joined,
        'updated_at': datetime.datetime.now(datetime.timezone.utc).replace(tzinfo=None).isoformat() + 'Z',
        'updated_by': payload.get('requested_by') or 'actor',
    }
    cfg['touch_binding'] = binding
    config_path.parent.mkdir(parents=True, exist_ok=True)
    tmp = config_path.with_suffix('.json.tmp')
    tmp.write_text(json.dumps(cfg, indent=2)); tmp.replace(config_path)
    if dev_path:
        cache_dir.mkdir(parents=True, exist_ok=True)
        Path(cache_dir, 'touch-device.json').write_text(json.dumps({'path': dev_path, 'name': dev_name}, indent=2))
    details = {'config_path': str(config_path), 'touch_binding': binding, 'steps': []}
    # Restart labwc so rc.xml <touch> mapping is reloaded, then let browser services return.
    rc, out, err = _run_list(['sudo', '-n', 'systemctl', 'restart', 'screentra-cast-kiosk@0.service'])
    details['steps'].append({'unit': 'screentra-cast-kiosk@0.service', 'restart_code': rc, 'message': out or err})
    if has_systemctl_user():
        systemctl_user('restart', 'screentra-client-heartbeat.service')
        for i in range(4):
            systemctl_user('try-restart', f'screentra-client-renderer@{i}.service')
            systemctl_user('try-restart', f'screentra-kiosk-browser@{i}.service')
    ok = rc == 0 or has_systemctl_user()
    return ok, 'touch binding applied; labwc/browser services restarted', details


def tls_context(cache_dir: Path | None = None):
    """Use the Screentra Cast CA bundle directly when present, independent of OS trust."""
    candidates = []
    if os.environ.get('RIPLEY_CA_CERT'):
        candidates.append(Path(os.environ['RIPLEY_CA_CERT']))
    if cache_dir is not None:
        candidates.append(cache_dir.parent / 'screentra-cast-signage-ca.crt')
    candidates.append(Path.home() / '.local/share/ripley-player-client/screentra-cast-signage-ca.crt')
    for cafile in candidates:
        if cafile.exists():
            try:
                return ssl.create_default_context(cafile=str(cafile))
            except Exception as e:
                print(f'warning: failed to load Screentra Cast CA {cafile}: {e}', flush=True)
    return None

def urlopen_ripley(req, timeout=10, cache_dir: Path | None = None):
    kw = {'timeout': timeout}
    ctx = tls_context(cache_dir)
    if ctx is not None:
        kw['context'] = ctx
    return urllib.request.urlopen(req, **kw)


def fetch_config(server: str, client_id: str, cache_dir: Path, display_index: int | None = None) -> bool:
    if display_index is None:
        url = server.rstrip('/') + '/api/clients/' + urllib.request.quote(client_id, safe='') + '/config'
        target = 'assigned-layout.json'
    else:
        url = (server.rstrip() + '/api/clients/' + urllib.request.quote(client_id, safe='') +
               '/displays/' + str(display_index) + '/config')
        target = f'assigned-layout-display-{display_index}.json'
    try:
        with urlopen_ripley(url, timeout=10, cache_dir=cache_dir) as r:
            data = json.loads(r.read().decode())
        # The server display-config payload does not include local-admin theme.
        try:
            previous = json.loads((cache_dir / 'client-config.json').read_text())
            if previous.get('theme'):
                data['theme'] = previous['theme']
        except Exception:
            pass
        cache_dir.mkdir(parents=True, exist_ok=True)
        # Keep the complete response for heartbeat acknowledgement. Secondary
        # displays retain their own metadata; their layout cache remains layout-only
        # for the local renderer.
        if display_index is None or display_index == 0:
            Path(cache_dir, 'client-config.json').write_text(json.dumps(data, indent=2))
        else:
            Path(cache_dir, f'display-config-{display_index}.json').write_text(json.dumps(data, indent=2))
        if data.get('layout') is not None:
            Path(cache_dir, target).write_text(json.dumps(data['layout'], indent=2))
            if display_index in (None, 0):
                Path(cache_dir, 'assigned-layout.json').write_text(json.dumps(data['layout'], indent=2))
        else:
            # Clear stale per-display and legacy display-0 layout caches when assignment is cleared.
            stale_files = [Path(cache_dir, target)]
            if display_index in (None, 0):
                stale_files.append(Path(cache_dir, 'assigned-layout.json'))
            for stale in stale_files:
                try:
                    stale.unlink()
                except FileNotFoundError:
                    pass
        return True
    except Exception as e:
        print(f'config refresh failed: {e}', flush=True)
        return False


def fetch_all_display_configs(server: str, client_id: str, cache_dir: Path) -> bool:
    """Refresh all known display layout caches after a scene/multi-display publish."""
    try:
        url = server.rstrip('/') + '/api/clients/' + urllib.request.quote(client_id, safe='') + '/displays'
        with urlopen_ripley(url, timeout=10, cache_dir=cache_dir) as r:
            displays = json.loads(r.read().decode())
        ok = True
        seen = set()
        for d in displays:
            idx = d.get('display_index')
            if idx is None:
                continue
            try:
                idx = int(idx)
            except Exception:
                continue
            seen.add(idx)
            ok = fetch_config(server, client_id, cache_dir, display_index=idx) and ok
        # For known stale per-display caches not reported anymore, leave them alone;
        # heartbeat/display detection owns lifecycle. Missing assignment for a reported
        # display is cleared by fetch_config(display_index=idx).
        return ok
    except Exception as e:
        print(f'all-display config refresh failed: {e}', flush=True)
        return False


def update_client(server: str, client_id: str, cache_dir: Path, payload: dict | None) -> tuple[bool, str, dict]:
    """Download and run the server-served player installer, preserving enrollment identity."""
    payload = payload or {}
    install_dir = Path(os.environ.get('RIPLEY_CLIENT_DIR') or str(cache_dir.parent))
    server = (payload.get('server_url') or server or '').rstrip('/')
    if not server:
        return False, 'server URL is not configured', {}
    installer_url = payload.get('installer_url') or (server + '/install/player-client.sh')
    configured = urllib.parse.urlsplit(server)
    requested = urllib.parse.urlsplit(installer_url)
    if (requested.scheme, requested.netloc) != (configured.scheme, configured.netloc):
        installer_url = server + '/install/player-client.sh'
    env = os.environ.copy()
    env['RIPLEY_SERVER_URL'] = server
    if client_id:
        env['RIPLEY_CLIENT_ID'] = client_id
    env['RIPLEY_CLIENT_DIR'] = str(install_dir)
    env['RIPLEY_CA_CERT'] = str(install_dir / 'screentra-cast-signage-ca.crt')
    env.setdefault('RIPLEY_CLIENT_NAME', os.environ.get('RIPLEY_CLIENT_NAME') or os.uname().nodename)
    env.setdefault('RIPLEY_CLIENT_ROLE', os.environ.get('RIPLEY_CLIENT_ROLE') or 'player')
    details = {'installer_url': installer_url, 'install_dir': str(install_dir), 'client_id': client_id or None}
    if payload.get('dry_run'):
        details['dry_run'] = True
        return True, f'would download and run {installer_url}', details
    with tempfile.NamedTemporaryFile(prefix='ripley-player-update-', suffix='.sh', delete=False) as tmp:
        tmp_path = Path(tmp.name)
    signature_path = tmp_path.with_suffix('.sig')
    try:
        # Use the same TLS verification as the rest of the actor (Screentra Cast CA
        # bundle or system trust; never bypass certificate verification.
        # The installer script is downloaded from the same server as the
        # heartbeat, and a MITM that can rewrite the heartbeat can also
        # rewrite the installer.
        ctx_ssl = tls_context(cache_dir)
        kw = {'timeout': 30}
        if ctx_ssl is not None:
            kw['context'] = ctx_ssl
        with urllib.request.urlopen(installer_url, **kw) as r:
            data = r.read()
        signature_url = server + '/install/player-client.sh.sig'
        with urllib.request.urlopen(signature_url, **kw) as r:
            signature = r.read()
        public_key = Path(
            os.environ.get('SCREENTRA_CAST_UPDATE_PUBLIC_KEY_PATH')
            or '/etc/screentra-cast/client-update-signing.pub'
        )
        if not public_key.is_file():
            return False, f'client update refused: pinned signing key is missing at {public_key}', details
        tmp_path.write_bytes(data)
        tmp_path.chmod(0o755)
        signature_path.write_bytes(signature)
        verify = subprocess.run(
            [
                'openssl', 'pkeyutl', '-verify', '-rawin', '-pubin',
                '-inkey', str(public_key), '-sigfile', str(signature_path),
                '-in', str(tmp_path),
            ],
            capture_output=True, text=True, timeout=20,
        )
        details['signature_verified'] = verify.returncode == 0
        if verify.returncode != 0:
            return False, 'client update refused: installer signature verification failed', details
        p = subprocess.run(['/bin/bash', str(tmp_path)], env=env, capture_output=True, text=True, timeout=int(payload.get('timeout_seconds') or 180))
        out = (p.stdout or '').strip()
        err = (p.stderr or '').strip()
        details['returncode'] = p.returncode
        details['stdout_tail'] = out[-2000:]
        details['stderr_tail'] = err[-2000:]
        if p.returncode != 0:
            return False, f'installer exited {p.returncode}: {(err or out)[-500:]}', details
        return True, 'client installer updated/reinstalled successfully', details
    except Exception as e:
        details['error'] = str(e)
        return False, f'client update failed: {e}', details
    finally:
        try:
            tmp_path.unlink()
        except Exception:
            pass
        try:
            signature_path.unlink()
        except Exception:
            pass


def execute(action: str, payload: dict | None, ctx: dict) -> dict:
    """Run a single command and return a result dict."""
    started = datetime.datetime.now(datetime.timezone.utc).replace(tzinfo=None).isoformat() + 'Z'
    has_user = has_systemctl_user()
    payload = payload or {}
    display_index = payload.get('display_index')
    try:
        display_index = int(display_index) if display_index is not None else None
    except Exception:
        display_index = None

    def unit(base: str) -> str:
        return f'{base}@{display_index}.service' if display_index is not None else f'{base}.service'

    if action == 'restart_renderer':
        if has_user:
            ok, msg = systemctl_user('restart', unit('screentra-client-renderer'))
            if ok:
                return {'action': action, 'display_index': display_index, 'ok': True, 'message': msg, 'started_at': started}
        ok, msg = restart_system_kiosk()
        return {'action': action, 'display_index': display_index, 'ok': ok,
                'message': (f'kiosk service restarted; renderer display {display_index if display_index is not None else 0} refreshed'
                            if ok else f'renderer restart failed: {msg}'), 'details': {'scope': 'client_kiosk_service', 'systemd_message': msg}, 'started_at': started}

    if action == 'restart_kiosk':
        if has_user:
            ok, msg = systemctl_user('restart', unit('screentra-kiosk-browser'))
            if ok:
                return {'action': action, 'display_index': display_index, 'ok': True, 'message': msg, 'started_at': started}
        ok, msg = restart_system_kiosk()
        return {'action': action, 'display_index': display_index, 'ok': ok,
                'message': 'kiosk service restarted' if ok else f'kiosk restart failed: {msg}',
                'details': {'scope': 'client_kiosk_service', 'systemd_message': msg}, 'started_at': started}

    # Alias used by the touchscreen selection flow. Forward to the display-scoped
    # kiosk restart unit so the queued action is no longer dropped as unknown.
    if action == 'restart_kiosk_display':
        if has_user:
            ok, msg = systemctl_user('restart', unit('screentra-kiosk-browser'))
            if ok:
                return {'action': 'restart_kiosk', 'display_index': display_index, 'ok': True, 'message': msg, 'started_at': started}
        ok, msg = restart_system_kiosk()
        return {'action': 'restart_kiosk', 'display_index': display_index, 'ok': ok,
                'message': 'kiosk service restarted' if ok else f'kiosk restart failed: {msg}',
                'details': {'scope': 'client_kiosk_service', 'systemd_message': msg}, 'started_at': started}

    if action == 'reload_layout':
        cache_path = Path(ctx['cache_dir']) / 'client-config.json'
        try:
            before_orientation = json.loads(cache_path.read_text()).get('orientation', 'normal')
        except Exception:
            before_orientation = 'normal'
        if payload.get('all_displays') or display_index is None:
            ok = fetch_all_display_configs(ctx['server'], ctx['client_id'], Path(ctx['cache_dir']))
            after_orientation = before_orientation
            try:
                after_orientation = json.loads(cache_path.read_text()).get('orientation', 'normal')
            except Exception:
                pass
            restarted = False
            restart_message = ''
            if ok and after_orientation != before_orientation:
                rc, out, err = _run_list(['sudo', '-n', 'systemctl', 'restart', 'screentra-cast-kiosk@0.service'])
                restarted = rc == 0
                restart_message = out or err
            return {'action': action, 'display_index': display_index, 'all_displays': True, 'ok': ok and (restarted or after_orientation == before_orientation), 'orientation_changed': after_orientation != before_orientation, 'kiosk_restarted': restarted, 'message': 'all display configs re-fetched' + ('; kiosk restarted for orientation change' if restarted else (': ' + restart_message if restart_message else '')), 'started_at': started}
        ok = fetch_config(ctx['server'], ctx['client_id'], Path(ctx['cache_dir']), display_index=display_index)
        after_orientation = before_orientation
        try:
            after_orientation = json.loads(cache_path.read_text()).get('orientation', 'normal')
        except Exception:
            pass
        restarted = False
        restart_message = ''
        if ok and after_orientation != before_orientation:
            restart_index = display_index if display_index is not None else 0
            rc, out, err = _run_list(['sudo', '-n', 'systemctl', 'restart', f'screentra-cast-kiosk@{restart_index}.service'])
            restarted = rc == 0
            restart_message = out or err
        return {'action': action, 'display_index': display_index, 'ok': ok and (restarted or after_orientation == before_orientation), 'orientation_changed': after_orientation != before_orientation, 'kiosk_restarted': restarted, 'message': 'config re-fetched' + ('; kiosk restarted for orientation change' if restarted else (': ' + restart_message if restart_message else '')), 'started_at': started}

    if action == 'refresh_cache':
        if payload.get('all_displays') or display_index is None:
            ok = fetch_all_display_configs(ctx['server'], ctx['client_id'], Path(ctx['cache_dir']))
            return {'action': action, 'display_index': display_index, 'all_displays': True, 'ok': ok, 'message': 'all display caches refreshed' if ok else 'one or more display cache refreshes failed', 'started_at': started}
        ok = fetch_config(ctx['server'], ctx['client_id'], Path(ctx['cache_dir']), display_index=display_index)
        return {'action': action, 'display_index': display_index, 'ok': ok, 'message': 'cache refreshed' if ok else 'cache refresh failed', 'started_at': started}

    if action == 'update_client':
        ok, msg, details = update_client(ctx['server'], ctx['client_id'], Path(ctx['cache_dir']), payload)
        return {'action': action, 'display_index': display_index, 'ok': ok, 'message': msg, 'details': details, 'started_at': started}

    if action == 'set_display_count':
        ok, msg, details = apply_display_count(Path(ctx['cache_dir']), payload.get('max_displays'))
        return {'action': action, 'display_index': display_index, 'ok': ok, 'message': msg, 'details': details, 'started_at': started}

    if action == 'set_touch_binding':
        ok, msg, details = apply_touch_binding(Path(ctx['cache_dir']), payload)
        return {'action': action, 'display_index': display_index, 'ok': ok, 'message': msg, 'details': details, 'started_at': started}

    if action == 'bluetooth_scan':
        ok, msg, details = bluetooth_scan(payload)
        return {'action': action, 'ok': ok, 'message': msg, 'details': details, 'started_at': started}

    if action == 'bluetooth_connect':
        ok, msg, details = bluetooth_connect(payload)
        return {'action': action, 'ok': ok, 'message': msg, 'details': details, 'started_at': started}

    if action == 'audio_list':
        ok, msg, details = audio_list()
        return {'action': action, 'ok': ok, 'message': msg, 'details': details, 'started_at': started}

    if action == 'audio_set_default':
        ok, msg, details = audio_set_default(payload)
        return {'action': action, 'ok': ok, 'message': msg, 'details': details, 'started_at': started}

    if action == 'audio_set_volume':
        ok, msg, details = audio_set_volume(payload)
        return {'action': action, 'ok': ok, 'message': msg, 'details': details, 'started_at': started}

    if action == 'restart_client':
        ok, msg, details = restart_client()
        return {'action': action, 'ok': ok, 'message': msg, 'details': details, 'started_at': started}

    # ── Display control (DDC/CI + CEC) ────────────────────────────────────────
    if action == 'ddc_set':
        # payload: { "vcp": "10", "value": 50 }  brightness
        # or { "vcp": "D6", "value": 1 }          power on
        # optional: { "bus": "1" }                specific display bus
        # optional: { "has_layout": true }        retry detect if display not found
        vcp = payload.get('vcp', '')
        value = payload.get('value', 0)
        bus = payload.get('bus')
        has_layout = payload.get('has_layout', False)
        ok, msg = apply_ddc_control(str(vcp), int(value), bus=bus, has_layout=has_layout)
        return {'action': action, 'display_index': display_index, 'ok': ok, 'message': msg, 'started_at': started}

    if action == 'cec_send':
        # payload: { "cmd": "pow 0" }   power on OSD addr 0
        # or { "cmd": "is 1" }          input HDMI-2
        cmd = payload.get('cmd', '')
        ok, msg = apply_cec_command(cmd)
        return {'action': action, 'display_index': display_index, 'ok': ok, 'message': msg, 'started_at': started}

    if action == 'ir_send':
        # IR control — stub; hardware integration for later
        return {'action': action, 'display_index': display_index, 'ok': False,
                'message': 'IR control not yet implemented. Set method to DDC or CEC.',
                'started_at': started}

    return {'action': action, 'display_index': display_index, 'ok': False, 'message': f'unknown action: {action}', 'started_at': started}


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument('--pending-file', default=os.environ.get('RIPLEY_PENDING_FILE'))
    ap.add_argument('--result-file', default=os.environ.get('RIPLEY_RESULT_FILE'))
    ap.add_argument('--server', default=os.environ.get('RIPLEY_SERVER_URL'))
    ap.add_argument('--client-id', default=os.environ.get('RIPLEY_CLIENT_ID'))
    ap.add_argument('--cache-dir', default=os.environ.get('RIPLEY_CLIENT_CACHE_DIR', os.path.expanduser('~/.local/share/ripley-player-client/cache')))
    ap.add_argument('--watch', action='store_true', help='Loop, processing new pending files as they appear')
    ap.add_argument('--watch-interval', type=float, default=2.0)
    args = ap.parse_args()

    if not args.pending_file:
        ap.error('--pending-file (or RIPLEY_PENDING_FILE) is required')

    pending_path = Path(args.pending_file)
    result_path = Path(args.result_file) if args.result_file else pending_path.with_name('command-result.json')
    result_path.parent.mkdir(parents=True, exist_ok=True)

    ctx = {
        'server': args.server,
        'client_id': args.client_id or '',
        'cache_dir': args.cache_dir,
    }

    def process_one():
        if not pending_path.exists():
            return False
        try:
            spec = json.loads(pending_path.read_text())
        except Exception as e:
            result = {'action': 'unknown', 'ok': False, 'message': f'bad pending file: {e}', 'started_at': datetime.datetime.now(datetime.timezone.utc).replace(tzinfo=None).isoformat() + 'Z'}
            _atomic_write_text(result_path, json.dumps(result, indent=2))
            try: pending_path.unlink()
            except Exception: pass
            return True
        action = spec.get('action', 'unknown')
        result = execute(action, spec.get('payload'), ctx)
        result['finished_at'] = datetime.datetime.now(datetime.timezone.utc).replace(tzinfo=None).isoformat() + 'Z'
        result['client_id'] = ctx['client_id'] or None
        result['requested_at'] = spec.get('requested_at')
        _atomic_write_text(result_path, json.dumps(result, indent=2))
        try: pending_path.unlink()
        except Exception: pass
        print(f'actor: {action} -> ok={result["ok"]} {result["message"]}', flush=True)
        return True

    if args.watch:
        while True:
            if process_one():
                continue
            time.sleep(args.watch_interval)
    else:
        processed = process_one()
        return 0 if processed else 0


if __name__ == '__main__':
    raise SystemExit(main())
