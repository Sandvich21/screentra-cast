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
import importlib.util
import json
import os
import re
import signal
import shutil
import ssl
import subprocess
import sys
import tempfile
import time
import urllib.parse
import urllib.request
from urllib.parse import urlparse
from pathlib import Path


def _load_transcription_request_admission():
    module_path = Path(__file__).with_name(
        'screentra-stt-transcription-request-admission.py'
    )
    spec = importlib.util.spec_from_file_location(
        'screentra_stt_transcription_request_admission', module_path
    )
    if spec is None or spec.loader is None:
        raise RuntimeError('transcription request admission contract is unavailable')
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def _load_rollback_request_admission():
    module_path = Path(__file__).with_name(
        'screentra-client-service-rollback-request-admission.py'
    )
    spec = importlib.util.spec_from_file_location(
        'screentra_client_service_rollback_request_admission', module_path
    )
    if spec is None or spec.loader is None:
        raise RuntimeError('rollback request admission contract is unavailable')
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def _load_status_request_admission():
    module_path = Path(__file__).with_name(
        'screentra-client-service-status-request-admission.py'
    )
    spec = importlib.util.spec_from_file_location(
        'screentra_client_service_status_request_admission', module_path
    )
    if spec is None or spec.loader is None:
        raise RuntimeError('status request admission contract is unavailable')
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


_TRANSCRIPTION_REQUEST_ADMISSION = _load_transcription_request_admission()
_ROLLBACK_REQUEST_ADMISSION = _load_rollback_request_admission()
_STATUS_REQUEST_ADMISSION = _load_status_request_admission()

MAX_PENDING_BYTES = 1 << 20
MAX_JSON_FILE_BYTES = 1 << 20
MAX_COMMAND_PAYLOAD_BYTES = 64 * 1024
MAX_CLIENT_ID_BYTES = 200
CLIENT_SERVICE_IDENTIFIER_MAX_BYTES = 128
CLIENT_SERVICE_REQUEST_ID_MAX_BYTES = 128
CLIENT_SERVICE_TRANSCRIBE_ENVELOPE_MAX_BYTES = 512
CLIENT_SERVICE_REASON_MAX_BYTES = 256

ALLOWED_CLIENT_COMMAND_ACTIONS = (
    'restart_renderer', 'restart_kiosk', 'restart_kiosk_display',
    'reload_layout', 'refresh_cache', 'update_client',
    'set_display_count', 'set_touch_binding', 'bluetooth_scan',
    'bluetooth_connect', 'audio_list', 'audio_set_default',
    'audio_set_volume', 'restart_client', 'start_display_preview',
    'stop_display_preview', 'ddc_set', 'display_settings', 'display_power', 'cec_send',
    'ir_send', 'client_service_status', 'client_service_rollback_request',
    'client_service_transcribe_request',
)
_ALLOWED_PENDING_COMMAND_FIELDS = frozenset({
    'client_id', 'action', 'payload', 'requested_at', 'requested_by',
})
MAX_REQUESTED_AT_BYTES = 64
MAX_REQUESTED_BY_BYTES = 128


def _validate_client_service_identifier(value, field):
    if (not isinstance(value, str) or not value or
            len(value.encode('utf-8')) > CLIENT_SERVICE_IDENTIFIER_MAX_BYTES or
            any(not (char.isascii() and (char.isalnum() or char in '._-')) for char in value)):
        raise ValueError(f'{field} is invalid or exceeds its bound')


def validate_client_service_payload(action, payload):
    if action == 'client_service_status':
        _STATUS_REQUEST_ADMISSION.validate_status_request(payload)
        return
    if action == 'client_service_rollback_request':
        _ROLLBACK_REQUEST_ADMISSION.validate_rollback_request(payload)
        return
    if action == 'client_service_transcribe_request':
        _TRANSCRIPTION_REQUEST_ADMISSION.validate_transcription_request(payload)
        return
    if payload is None:
        return
    if not isinstance(payload, dict):
        raise ValueError('client service payload must be a JSON object')
    allowed = ({'service_id', 'activation_id', 'target_version', 'reason'}
               if action == 'client_service_rollback_request' else None)
    if allowed is None:
        return
    if set(payload) - allowed:
        raise ValueError('client service payload contains an unsupported field')
    required = {'service_id', 'activation_id'} if action == 'client_service_rollback_request' else set()
    missing = required - set(payload)
    if missing:
        raise ValueError(f'client service payload is missing {sorted(missing)[0]}')
    for field in ('service_id', 'activation_id', 'target_version'):
        if field in payload:
            _validate_client_service_identifier(payload[field], field)
    if 'reason' in payload:
        reason = payload['reason']
        if (not isinstance(reason, str) or not reason or
                len(reason.encode('utf-8')) > CLIENT_SERVICE_REASON_MAX_BYTES or
                any(ord(char) < 32 or ord(char) == 127 for char in reason)):
            raise ValueError('client service reason is invalid or exceeds its bound')


def validate_pending_command(spec, expected_client_id):
    if not isinstance(spec, dict):
        raise ValueError('pending command must be a JSON object')
    if set(spec) - _ALLOWED_PENDING_COMMAND_FIELDS:
        raise ValueError('pending command contains unsupported fields')
    if (not isinstance(expected_client_id, str) or not expected_client_id or
            len(expected_client_id.encode('utf-8')) > MAX_CLIENT_ID_BYTES):
        raise ValueError('client identity is invalid')
    if spec.get('client_id') != expected_client_id:
        raise ValueError('pending command client identity mismatch')
    if 'requested_at' in spec:
        requested_at = spec['requested_at']
        if (not isinstance(requested_at, str) or not requested_at or
                len(requested_at.encode('utf-8')) > MAX_REQUESTED_AT_BYTES or
                any(ord(char) < 32 or ord(char) == 127 for char in requested_at)):
            raise ValueError('pending command requested_at is invalid')
    if 'requested_by' in spec:
        requested_by = spec['requested_by']
        if requested_by is not None and (
                not isinstance(requested_by, str) or not requested_by or
                len(requested_by.encode('utf-8')) > MAX_REQUESTED_BY_BYTES or
                any(ord(char) < 32 or ord(char) == 127 for char in requested_by)):
            raise ValueError('pending command requested_by is invalid')
    action = spec.get('action')
    if action not in ALLOWED_CLIENT_COMMAND_ACTIONS:
        raise ValueError(f'unsupported client command action: {action}')
    payload = spec.get('payload')
    if payload is not None and not isinstance(payload, dict):
        raise ValueError('client command payload must be a JSON object')
    if payload is not None:
        try:
            encoded = json.dumps(
                payload,
                allow_nan=False,
                separators=(',', ':'),
            ).encode('utf-8')
        except (TypeError, ValueError, UnicodeError) as exc:
            raise ValueError('client command payload is not bounded JSON') from exc
        if len(encoded) > MAX_COMMAND_PAYLOAD_BYTES:
            raise ValueError(f'client command payload exceeds {MAX_COMMAND_PAYLOAD_BYTES} bytes')
        if 'client_id' in payload:
            raise ValueError('client identity is bound to the command envelope')
    if action == 'display_settings':
        if not isinstance(payload, dict):
            raise ValueError('display_settings payload must be a JSON object')
        allowed = {'display_index', 'brightness', 'contrast'}
        if set(payload) - allowed:
            raise ValueError('display_settings payload contains an unsupported field')
        if not isinstance(payload.get('display_index'), int) or isinstance(payload.get('display_index'), bool) or not 0 <= payload['display_index'] <= 3:
            raise ValueError('display_settings display_index must be between 0 and 3')
        supplied = [field for field in ('brightness', 'contrast') if field in payload]
        if not supplied:
            raise ValueError('display_settings requires brightness or contrast')
        for field in supplied:
            value = payload[field]
            if not isinstance(value, int) or isinstance(value, bool) or not 0 <= value <= 100:
                raise ValueError(f'display_settings {field} must be between 0 and 100')
    validate_client_service_payload(action, payload)
    return spec


def _reject_duplicate_json_keys(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError('pending command contains duplicate JSON fields')
        result[key] = value
    return result


def _parse_pending_command(text):
    try:
        return json.loads(text, object_pairs_hook=_reject_duplicate_json_keys)
    except (TypeError, ValueError, UnicodeError) as exc:
        raise ValueError('pending command JSON is invalid') from exc


def _read_json_file(path: Path, default):
    try:
        if not path.is_file() or path.stat().st_size > MAX_JSON_FILE_BYTES:
            return default
        return json.loads(path.read_text(encoding='utf-8'))
    except Exception:
        return default


def _server_origin(server: str) -> str:
    parsed = urllib.parse.urlsplit((server or '').strip())
    if parsed.scheme not in ('http', 'https') or not parsed.hostname or parsed.username or parsed.password or parsed.query or parsed.fragment:
        raise ValueError('configured server URL must be an HTTP(S) origin without credentials or query parameters')
    if parsed.scheme == 'http' and parsed.hostname not in {'127.0.0.1', '::1', 'localhost'} and os.environ.get('RIPLEY_ALLOW_INSECURE_HTTP', '').lower() not in ('1', 'true', 'yes'):
        raise ValueError('non-loopback HTTP server URLs require explicit RIPLEY_ALLOW_INSECURE_HTTP=1')
    return urllib.parse.urlunsplit((parsed.scheme, parsed.netloc, parsed.path.rstrip('/'), '', ''))


def _atomic_write_text(path: Path, text: str, mode: int = 0o600) -> None:
    """Replace a cache JSON file atomically and keep it owner-readable."""
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(f'.{path.name}.{os.getpid()}.tmp')
    temporary.write_text(text)
    os.chmod(temporary, mode)
    os.replace(temporary, path)


def _run_list(args: list[str], timeout: int = 20, input_data: str | None = None, env: dict[str, str] | None = None) -> tuple[int, str, str]:
    try:
        p = subprocess.run(args, capture_output=True, text=True, timeout=timeout, input=input_data, env=env)
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
        cfg = _read_json_file(config_path, {})
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
    _atomic_write_text(config_path, json.dumps(cfg, indent=2))

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
        cfg = _read_json_file(config_path, {})
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
    _atomic_write_text(config_path, json.dumps(cfg, indent=2))
    if dev_path:
        cache_dir.mkdir(parents=True, exist_ok=True)
        _atomic_write_text(Path(cache_dir, 'touch-device.json'), json.dumps({'path': dev_path, 'name': dev_name}, indent=2))
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
    raise RuntimeError('Screentra Cast CA certificate is unavailable or invalid')


def validate_server_url(server: str):
    parsed = urlparse((server or '').strip())
    if parsed.scheme not in ('https', 'http') or not parsed.hostname:
        raise ValueError('server URL must include an https:// origin')
    if parsed.scheme == 'http' and parsed.hostname not in ('127.0.0.1', 'localhost', '::1'):
        raise ValueError('non-loopback server URLs must use HTTPS')
    return parsed

def urlopen_ripley(req, timeout=10, cache_dir: Path | None = None):
    kw = {'timeout': timeout}
    parsed = validate_server_url(req.full_url)
    if parsed.scheme == 'https':
        ctx = tls_context(cache_dir)
        kw['context'] = ctx
    return urllib.request.urlopen(req, **kw)


def _preview_pid_path(cache_dir: Path, display_index: int) -> Path:
    return cache_dir / f'display-preview-{display_index}.pid'


def _preview_state_path(cache_dir: Path, display_index: int) -> Path:
    return cache_dir / f'display-preview-{display_index}.json'


def _preview_token(cache_dir: Path) -> str:
    token_path = cache_dir.parent / 'client-auth-token'
    try:
        token = token_path.read_text(encoding='utf-8').strip()
    except Exception:
        return ''
    return token


def _write_preview_state(cache_dir: Path, display_index: int, state: dict) -> None:
    _atomic_write_text(_preview_state_path(cache_dir, display_index), json.dumps(state, indent=2))


def _preview_connector(cache_dir: Path, display_index: int) -> str | None:
    data = _read_json_file(cache_dir / f'display-{display_index}.json', {})
    connector = str(data.get('connector') or '').strip()
    return connector or None


def _preview_wayland_env() -> dict[str, str]:
    """Provide Wayland runtime variables when the actor is a system service."""
    env = os.environ.copy()
    runtime = env.get('XDG_RUNTIME_DIR')
    if not runtime:
        for candidate in ('/run/board', f"/run/user/{os.getuid()}"):
            if Path(candidate).is_dir():
                runtime = candidate
                env['XDG_RUNTIME_DIR'] = candidate
                break
    if runtime and not env.get('WAYLAND_DISPLAY'):
        sockets = sorted(
            p for p in Path(runtime).glob('wayland-*')
            if p.is_socket()
        )
        if sockets:
            env['WAYLAND_DISPLAY'] = sockets[0].name
    return env


def apply_display_power(cache_dir: Path, display_index: int | None, state: str) -> tuple[bool, str, dict]:
    """Apply Linux DPMS/KMS power state to one validated Wayland output."""
    if display_index is None or display_index < 0 or display_index > 3:
        return False, 'a display index from 0 to 3 is required', {}
    if state not in {'on', 'off'}:
        return False, 'display power state must be on or off', {}
    connector = _preview_connector(cache_dir, display_index)
    if not connector:
        return False, f'no connector is recorded for display {display_index}', {'display_index': display_index}
    env = _preview_wayland_env()
    requested = '--on' if state == 'on' else '--off'
    rc, out, err = _run_list(
        ['wlr-randr', '--output', connector, requested],
        timeout=10,
        env=env,
    )
    message = out or err or ('display enabled' if state == 'on' else 'display disabled')
    return rc == 0, message, {
        'display_index': display_index,
        'connector': connector,
        'state': state,
        'return_code': rc,
    }


def _upload_preview_frame(server: str, client_id: str, display_index: int, frame: bytes, cache_dir: Path) -> tuple[bool, str]:
    token = _preview_token(cache_dir)
    if not token:
        return False, 'client authentication token is unavailable'
    url = (server.rstrip('/') + '/clients/' + urllib.request.quote(client_id, safe='') +
           '/screenshot?display=' + str(display_index))
    request = urllib.request.Request(
        url,
        data=frame,
        method='POST',
        headers={
            'Content-Type': 'image/png',
            'X-SCREENTRA-CLIENT-TOKEN': token,
        },
    )
    try:
        with urlopen_ripley(request, timeout=20, cache_dir=cache_dir) as response:
            if response.status not in (200, 204):
                return False, f'upload returned HTTP {response.status}'
        return True, ''
    except Exception as exc:
        return False, f'upload failed: {exc}'


def run_preview_loop(server: str, client_id: str, cache_dir: Path, display_index: int, interval: int, duration: int) -> int:
    """Capture one physical Wayland display until stopped or the session expires."""
    pid_path = _preview_pid_path(cache_dir, display_index)
    state_path = _preview_state_path(cache_dir, display_index)
    connector = _preview_connector(cache_dir, display_index)
    if shutil.which('grim') is None:
        _write_preview_state(cache_dir, display_index, {'status': 'unavailable', 'message': 'grim is not installed'})
        return 1
    if not connector:
        _write_preview_state(cache_dir, display_index, {'status': 'unavailable', 'message': 'physical display connector is unavailable'})
        return 1
    started = time.time()
    _write_preview_state(cache_dir, display_index, {'status': 'starting', 'connector': connector, 'started_at': started})
    try:
        while time.time() - started < duration:
            command = ['grim', '-o', connector, '-']
            try:
                capture = subprocess.run(
                    command, capture_output=True, timeout=20, check=False,
                    env=_preview_wayland_env(),
                )
            except Exception as exc:
                _write_preview_state(cache_dir, display_index, {'status': 'error', 'connector': connector, 'message': f'capture failed: {exc}'})
                time.sleep(interval)
                continue
            if capture.returncode != 0 or not capture.stdout:
                _write_preview_state(cache_dir, display_index, {'status': 'unavailable', 'connector': connector, 'message': 'grim could not capture the display'})
                time.sleep(interval)
                continue
            ok, message = _upload_preview_frame(server, client_id, display_index, capture.stdout, cache_dir)
            if ok:
                _write_preview_state(cache_dir, display_index, {
                    'status': 'streaming', 'connector': connector, 'started_at': started,
                    'last_frame_at': time.time(), 'bytes': len(capture.stdout),
                })
            else:
                _write_preview_state(cache_dir, display_index, {'status': 'error', 'connector': connector, 'message': message})
            time.sleep(interval)
    finally:
        try:
            if pid_path.read_text(encoding='utf-8').strip() == str(os.getpid()):
                pid_path.unlink()
        except Exception:
            pass
        try:
            state = _read_json_file(state_path, {})
            state.update({'status': 'stopped', 'stopped_at': time.time()})
            _write_preview_state(cache_dir, display_index, state)
        except Exception:
            pass
    return 0


def _preview_process_alive(pid_path: Path) -> bool:
    try:
        pid = int(pid_path.read_text(encoding='utf-8').strip())
        os.kill(pid, 0)
        return True
    except Exception:
        return False


def start_display_preview(ctx: dict, display_index: int, payload: dict) -> tuple[bool, str, dict]:
    cache_dir = Path(ctx['cache_dir'])
    cache_dir.mkdir(parents=True, exist_ok=True)
    if shutil.which('grim') is None:
        message = 'grim is not installed on this client'
        _write_preview_state(cache_dir, display_index, {'status': 'unavailable', 'message': message})
        return False, message, {'display_index': display_index}
    if _preview_connector(cache_dir, display_index) is None:
        message = 'physical display connector is unavailable'
        _write_preview_state(cache_dir, display_index, {'status': 'unavailable', 'message': message})
        return False, message, {'display_index': display_index}
    pid_path = _preview_pid_path(cache_dir, display_index)
    if _preview_process_alive(pid_path):
        return True, 'display preview is already running', {'display_index': display_index}
    try:
        interval = max(1, min(int(payload.get('interval_seconds') or 5), 30))
        duration = max(10, min(int(payload.get('duration_seconds') or 120), 300))
    except (TypeError, ValueError):
        return False, 'preview interval and duration must be numeric', {}
    command = [
        sys.executable, str(Path(__file__).resolve()), '--preview-loop',
        '--server', ctx['server'], '--client-id', ctx['client_id'],
        '--cache-dir', str(cache_dir), '--display-index', str(display_index),
        '--preview-interval', str(interval), '--preview-duration', str(duration),
    ]
    try:
        process = subprocess.Popen(
            command, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL, start_new_session=True,
        )
        _atomic_write_text(pid_path, str(process.pid))
        time.sleep(0.05)
        if process.poll() is not None:
            state = _read_json_file(_preview_state_path(cache_dir, display_index), {})
            try:
                pid_path.unlink()
            except FileNotFoundError:
                pass
            return False, state.get('message') or 'display preview process exited immediately', state
        return True, 'display preview started', {'display_index': display_index, 'interval_seconds': interval, 'duration_seconds': duration}
    except Exception as exc:
        return False, f'display preview could not start: {exc}', {}


def stop_display_preview(ctx: dict, display_index: int) -> tuple[bool, str, dict]:
    cache_dir = Path(ctx['cache_dir'])
    pid_path = _preview_pid_path(cache_dir, display_index)
    try:
        pid = int(pid_path.read_text(encoding='utf-8').strip())
    except Exception:
        pid = None
    if pid is not None:
        try:
            os.kill(pid, signal.SIGTERM)
        except ProcessLookupError:
            pass
        except Exception as exc:
            return False, f'display preview could not stop: {exc}', {}
    try:
        pid_path.unlink()
    except FileNotFoundError:
        pass
    _write_preview_state(cache_dir, display_index, {'status': 'stopped', 'stopped_at': time.time()})
    return True, 'display preview stopped', {'display_index': display_index}


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
            previous = _read_json_file(cache_dir / 'client-config.json', {})
            if previous.get('theme'):
                data['theme'] = previous['theme']
        except Exception:
            pass
        cache_dir.mkdir(parents=True, exist_ok=True)
        # Keep the complete response for heartbeat acknowledgement. Secondary
        # displays retain their own metadata; their layout cache remains layout-only
        # for the local renderer.
        if display_index is None or display_index == 0:
            _atomic_write_text(Path(cache_dir, 'client-config.json'), json.dumps(data, indent=2))
        else:
            _atomic_write_text(Path(cache_dir, f'display-config-{display_index}.json'), json.dumps(data, indent=2))
        if data.get('layout') is not None:
            _atomic_write_text(Path(cache_dir, target), json.dumps(data['layout'], indent=2))
            if display_index in (None, 0):
                _atomic_write_text(Path(cache_dir, 'assigned-layout.json'), json.dumps(data['layout'], indent=2))
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
    """Request the root-owned verifier; never download or execute as the player."""
    payload = payload or {}
    request_path = Path('/var/lib/screentra-cast/update/request')
    details = {'request_path': str(request_path), 'client_id': client_id or None}
    if payload.get('dry_run'):
        details['dry_run'] = True
        return True, 'would request the root-owned signed updater', details
    if not request_path.parent.is_dir():
        return False, 'client update request failed: root updater directory is unavailable', details
    marker = {
        'requested_at': datetime.datetime.now(datetime.timezone.utc).isoformat().replace('+00:00', 'Z'),
        'client_id': client_id or '',
        'reason': 'server-command',
    }
    temporary = request_path.with_name('.request.tmp')
    try:
        raw = json.dumps(marker, separators=(',', ':')).encode('utf-8')
        if len(raw) > 4096:
            return False, 'client update refused: request marker is too large', details
        temporary.write_bytes(raw)
        temporary.chmod(0o600)
        os.replace(temporary, request_path)
        return True, 'verified root client updater requested', details
    except Exception as e:
        details['error'] = str(e)
        return False, f'client update request failed: {e}', details
    finally:
        try:
            temporary.unlink()
        except Exception:
            pass


def execute(action: str, payload: dict | None, ctx: dict) -> dict:
    """Run a single command and return a result dict."""
    started = datetime.datetime.now(datetime.timezone.utc).replace(tzinfo=None).isoformat() + 'Z'
    if action == 'client_service_status':
        try:
            request = _STATUS_REQUEST_ADMISSION.validate_status_request(payload)
            evidence = _STATUS_REQUEST_ADMISSION.build_status_request_evidence(request)
        except ValueError as exc:
            return {
                'action': action, 'ok': False, 'code': 'invalid_payload',
                'execution_enabled': False, 'message': str(exc), 'started_at': started,
            }
        return {
            'action': action, 'ok': True, 'code': 'execution_disabled',
            'execution_enabled': False, 'state': 'disabled',
            'message': 'client service runtime execution is disabled',
            'details': dict(evidence),
            'started_at': started,
        }
    if action == 'client_service_rollback_request':
        try:
            request = _ROLLBACK_REQUEST_ADMISSION.validate_rollback_request(payload)
            evidence = _ROLLBACK_REQUEST_ADMISSION.build_rollback_request_evidence(request)
        except ValueError as exc:
            return {
                'action': action, 'ok': False, 'code': 'invalid_payload',
                'execution_enabled': False, 'message': str(exc), 'started_at': started,
            }
        return {
            'action': action, 'ok': False, 'code': 'execution_disabled',
            'execution_enabled': False, 'state': 'disabled',
            'status': evidence['status'],
            'message': 'client service rollback is disabled',
            'details': dict(evidence),
            'started_at': started,
        }
    if action == 'client_service_transcribe_request':
        try:
            request = _TRANSCRIPTION_REQUEST_ADMISSION.validate_transcription_request(payload)
            evidence = _TRANSCRIPTION_REQUEST_ADMISSION.build_transcription_request_evidence(request)
        except ValueError as exc:
            return {
                'action': action, 'ok': False, 'code': 'invalid_payload',
                'execution_enabled': False, 'message': str(exc), 'started_at': started,
            }
        details = dict(evidence)
        details.update({'audio_bytes': 0, 'transcript': None})
        return {
            'action': action, 'ok': False, 'code': 'execution_disabled',
            'execution_enabled': False, 'state': 'disabled', 'status': evidence['status'],
            'message': 'client service transcription is disabled',
            'details': details,
            'started_at': started,
        }
    payload = payload or {}
    if action not in ALLOWED_CLIENT_COMMAND_ACTIONS:
        return {
            'action': action, 'ok': False, 'code': 'unsupported_action',
            'execution_enabled': False, 'message': f'unknown action: {action}',
            'started_at': started,
        }
    has_user = has_systemctl_user()
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
        requested_orientation = payload.get('orientation')
        if requested_orientation is not None and requested_orientation not in {'normal', 'flipped', 'rotated_left', 'rotated_right'}:
            return {'action': action, 'display_index': display_index, 'ok': False, 'message': 'invalid orientation', 'started_at': started}
        try:
            before_orientation = _read_json_file(cache_path, {}).get('orientation', 'normal')
        except Exception:
            before_orientation = 'normal'
        if payload.get('all_displays') or display_index is None:
            ok = fetch_all_display_configs(ctx['server'], ctx['client_id'], Path(ctx['cache_dir']))
            after_orientation = before_orientation
            try:
                after_orientation = _read_json_file(cache_path, {}).get('orientation', 'normal')
            except Exception:
                pass
            restarted = False
            restart_message = ''
            if ok and (after_orientation != before_orientation or (requested_orientation is not None and after_orientation == requested_orientation)):
                rc, out, err = _run_list(['sudo', '-n', 'systemctl', 'restart', 'screentra-cast-kiosk@0.service'])
                restarted = rc == 0
                restart_message = out or err
            return {'action': action, 'display_index': display_index, 'all_displays': True, 'ok': ok and (restarted or (after_orientation == before_orientation and requested_orientation is None)), 'orientation_changed': after_orientation != before_orientation or requested_orientation is not None, 'kiosk_restarted': restarted, 'message': 'all display configs re-fetched' + ('; kiosk restarted for orientation change' if restarted else (': ' + restart_message if restart_message else '')), 'started_at': started}
        ok = fetch_config(ctx['server'], ctx['client_id'], Path(ctx['cache_dir']), display_index=display_index)
        after_orientation = before_orientation
        try:
            after_orientation = _read_json_file(cache_path, {}).get('orientation', 'normal')
        except Exception:
            pass
        restarted = False
        restart_message = ''
        if ok and (after_orientation != before_orientation or (requested_orientation is not None and after_orientation == requested_orientation)):
            restart_index = display_index if display_index is not None else 0
            rc, out, err = _run_list(['sudo', '-n', 'systemctl', 'restart', f'screentra-cast-kiosk@{restart_index}.service'])
            restarted = rc == 0
            restart_message = out or err
        return {'action': action, 'display_index': display_index, 'ok': ok and (restarted or (after_orientation == before_orientation and requested_orientation is None)), 'orientation_changed': after_orientation != before_orientation or requested_orientation is not None, 'kiosk_restarted': restarted, 'message': 'config re-fetched' + ('; kiosk restarted for orientation change' if restarted else (': ' + restart_message if restart_message else '')), 'started_at': started}

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

    if action == 'start_display_preview':
        if display_index is None or display_index < 0 or display_index > 3:
            return {'action': action, 'ok': False, 'message': 'a display index from 0 to 3 is required', 'started_at': started}
        ok, msg, details = start_display_preview(ctx, display_index, payload)
        return {'action': action, 'display_index': display_index, 'ok': ok, 'message': msg, 'details': details, 'started_at': started}

    if action == 'stop_display_preview':
        if display_index is None or display_index < 0 or display_index > 3:
            return {'action': action, 'ok': False, 'message': 'a display index from 0 to 3 is required', 'started_at': started}
        ok, msg, details = stop_display_preview(ctx, display_index)
        return {'action': action, 'display_index': display_index, 'ok': ok, 'message': msg, 'details': details, 'started_at': started}

    # ── Scheduled DDC display settings ────────────────────────────────────────
    if action == 'display_settings':
        try:
            validate_pending_command({'client_id': ctx.get('client_id', ''), 'action': action, 'payload': payload}, ctx.get('client_id', ''))
        except ValueError as exc:
            return {'action': action, 'display_index': display_index, 'ok': False, 'code': 'invalid_payload', 'message': str(exc), 'started_at': started}
        results = []
        for field, vcp in (('brightness', '10'), ('contrast', '12')):
            if field not in payload:
                continue
            ok, msg = apply_ddc_control(vcp, payload[field])
            results.append({'setting': field, 'vcp': vcp, 'value': payload[field], 'ok': ok, 'message': msg})
        succeeded = sum(1 for result in results if result['ok'])
        ok = succeeded == len(results)
        return {
            'action': action, 'display_index': display_index, 'ok': ok,
            'partial': 0 < succeeded < len(results), 'message': 'DDC display settings applied' if ok else ('DDC display settings partially applied' if succeeded else 'DDC display settings failed'),
            'details': {'settings': results, **{result['setting']: result for result in results}}, 'started_at': started,
        }

    if action == 'display_power':
        state = payload.get('state')
        ok, message, details = apply_display_power(Path(ctx['cache_dir']), display_index, state)
        return {'action': action, 'display_index': display_index, 'ok': ok, 'message': message, 'details': details, 'started_at': started}

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

    return {'action': action, 'display_index': display_index, 'ok': False,
            'code': 'unsupported_action', 'execution_enabled': False,
            'message': f'unknown action: {action}', 'started_at': started}


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument('--pending-file', default=os.environ.get('RIPLEY_PENDING_FILE'))
    ap.add_argument('--result-file', default=os.environ.get('RIPLEY_RESULT_FILE'))
    ap.add_argument('--server', default=os.environ.get('RIPLEY_SERVER_URL'))
    ap.add_argument('--client-id', default=os.environ.get('RIPLEY_CLIENT_ID'))
    ap.add_argument('--cache-dir', default=os.environ.get('RIPLEY_CLIENT_CACHE_DIR', os.path.expanduser('~/.local/share/ripley-player-client/cache')))
    ap.add_argument('--watch', action='store_true', help='Loop, processing new pending files as they appear')
    ap.add_argument('--watch-interval', type=float, default=2.0)
    ap.add_argument('--preview-loop', action='store_true', help=argparse.SUPPRESS)
    ap.add_argument('--display-index', type=int)
    ap.add_argument('--preview-interval', type=int, default=5)
    ap.add_argument('--preview-duration', type=int, default=120)
    args = ap.parse_args()

    try:
        args.server = _server_origin(args.server)
    except ValueError as exc:
        ap.error(str(exc))

    if args.preview_loop:
        if not args.client_id or not args.cache_dir or args.display_index is None:
            ap.error('--preview-loop requires --client-id, --cache-dir, and --display-index')
        return run_preview_loop(
            args.server, args.client_id, Path(args.cache_dir), args.display_index,
            max(2, min(args.preview_interval, 30)), max(10, min(args.preview_duration, 300)),
        )

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
            if pending_path.stat().st_size > MAX_PENDING_BYTES:
                raise ValueError(f'pending command exceeds {MAX_PENDING_BYTES} bytes')
            spec = _parse_pending_command(pending_path.read_text(encoding='utf-8'))
        except Exception as e:
            result = {'action': 'unknown', 'ok': False, 'message': f'bad pending file: {e}', 'started_at': datetime.datetime.now(datetime.timezone.utc).replace(tzinfo=None).isoformat() + 'Z'}
            _atomic_write_text(result_path, json.dumps(result, indent=2))
            try: pending_path.unlink()
            except Exception: pass
            return True
        try:
            validate_pending_command(spec, ctx['client_id'])
        except Exception as e:
            action = spec.get('action', 'unknown') if isinstance(spec, dict) else 'unknown'
            result = {
                'action': action, 'ok': False, 'code': 'invalid_command',
                'execution_enabled': False, 'message': str(e),
                'started_at': datetime.datetime.now(datetime.timezone.utc).replace(tzinfo=None).isoformat() + 'Z',
                'client_id': ctx['client_id'] or None,
                'requested_at': spec.get('requested_at') if isinstance(spec, dict) else None,
            }
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
