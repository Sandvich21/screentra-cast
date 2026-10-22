#!/usr/bin/env python3
"""Screentra Cast cached player renderer.

Serves a local browser renderer from cached heartbeat config/layout files.
This lets a player continue displaying its assigned layout when the server or LAN is unavailable.
"""
import argparse
import json
import mimetypes
import os
import sys
import urllib.parse
import socket
import ssl
import stat
import tempfile
import time
import hashlib
import hmac
import secrets
import base64
import threading
import tarfile
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

MAX_REQUEST_BODY_BYTES = 1 << 20
MAX_JSON_FILE_BYTES = 1 << 20


def client_pairing_code(client_id):
    """Return the short physical-confirmation code for a client ID."""
    digest = hashlib.sha256(b'screentra-cast-pairing-v1\0' + str(client_id).encode('utf-8')).digest()
    return f'{int.from_bytes(digest[:4], "big") % 1_000_000:06d}'


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
            raise ValueError('renderer cache directory is not a real directory')
    return True


def _read_real_file(path: Path, maximum: int = MAX_JSON_FILE_BYTES) -> bytes:
    absolute = Path(os.path.abspath(os.fspath(path)))
    current = Path(absolute.anchor)
    for part in absolute.parts[1:]:
        current /= part
        metadata = os.lstat(current)
        if stat.S_ISLNK(metadata.st_mode):
            raise ValueError('renderer cache path contains a symlink')
    descriptor = os.open(absolute, os.O_RDONLY | getattr(os, 'O_NOFOLLOW', 0))
    try:
        metadata = os.fstat(descriptor)
        if not stat.S_ISREG(metadata.st_mode) or metadata.st_size > maximum:
            raise ValueError('renderer cache file is not bounded')
        raw = os.read(descriptor, maximum + 1)
    finally:
        os.close(descriptor)
    if len(raw) > maximum:
        raise ValueError('renderer cache file exceeds its bound')
    return raw


def _atomic_write_text(path: Path, text: str, mode: int = 0o600) -> None:
    _ensure_real_directory(path.parent, create=True)
    descriptor, temporary_name = tempfile.mkstemp(
        prefix=f'.{path.name}.', suffix='.tmp', dir=str(path.parent)
    )
    temporary = Path(temporary_name)
    try:
        with os.fdopen(descriptor, 'w', encoding='utf-8') as handle:
            handle.write(text)
        os.chmod(temporary, mode)
        os.replace(temporary, path)
        temporary = None
    finally:
        if temporary is not None:
            try:
                temporary.unlink()
            except FileNotFoundError:
                pass


def _read_json_file(path: Path, default):
    try:
        return json.loads(_read_real_file(path).decode('utf-8'))
    except Exception:
        return default


def _request_body(handler) -> str:
    raw_length = handler.headers.get('Content-Length')
    if raw_length is None:
        length = 0
    else:
        try:
            length = int(raw_length)
        except (TypeError, ValueError):
            raise ValueError('invalid Content-Length')
    if length < 0:
        raise ValueError('invalid Content-Length')
    if length > MAX_REQUEST_BODY_BYTES:
        raise OverflowError('request body too large')
    return handler.rfile.read(length).decode('utf-8') if length else '{}'


# === no-layout & touchscreen endpoints =====================================
import urllib.request as _ur
import subprocess as _sp
from datetime import datetime as _dt, timezone as _tz


def _iso_now() -> str:
    return _dt.now(_tz.utc).isoformat()

# Linux input.h ABS_* bit numbers. The EV_ABS bitmask in /proc/bus/input/devices
# (B: ABS=...) is a hex bitmask, so we test these by reading the bit at position
# N. These are the canonical "this is a touchscreen / multi-touch surface" bits.
_ABS_MT_POSITION_X = 53
_ABS_MT_POSITION_Y = 54
_ABS_MT_SLOT       = 47
_ABS_MT_TOUCH_MAJOR = 48
_ABS_X = 0
_ABS_Y = 1


def _udev_has_touchscreen_property(dev_path: str) -> bool:
    """Return True if udev marks dev_path as a touchscreen.

    udev sets ID_INPUT_TOUCHSCREEN=1 via libinput for any device that
    libinput classifies as a touchscreen. This is the most reliable
    detection: it works for HID-class, USB-class, and I2C-HID devices
    regardless of the device name.
    """
    try:
        out = _sp.check_output(
            ['udevadm', 'info', '--query=property', '--name=' + dev_path],
            stderr=_sp.DEVNULL, timeout=2, text=True,
        )
        for line in out.splitlines():
            if line.startswith('ID_INPUT_TOUCHSCREEN=1'):
                return True
    except Exception:
        return False
    return False


def _is_touch_bitmask(abs_bits: str) -> bool:
    """True if the ABS= hex bitmask indicates a touchscreen."""
    try:
        bits = int(abs_bits, 16)
    except (TypeError, ValueError):
        return False
    has_mt = (
        (bits >> _ABS_MT_POSITION_X) & 1
        and (bits >> _ABS_MT_POSITION_Y) & 1
        and (bits >> _ABS_MT_SLOT) & 1
    )
    has_single = (
        (bits >> _ABS_X) & 1
        and (bits >> _ABS_Y) & 1
    )
    return has_mt or has_single


def _is_touch_eligible(d: dict) -> bool:
    """Combine multiple signals to decide whether a device is a touchscreen.

    In order of trust:
      1. udev's ID_INPUT_TOUCHSCREEN — set by libinput, the project standard.
      2. ABS bitmask contains ABS_MT_POSITION_X/Y/_SLOT (multi-touch surface).
      3. The kernel name contains "Touch" or "touch" (some vendors put it in name).
    Each handler event path is checked independently so a multi-function device
    (e.g. USB audio + touch) is offered correctly.
    """
    hs = (d.get('handlers') or '').split()
    absbits = d.get('abs') or ''
    name = d.get('name') or ''
    has_touch_name = 'Touch' in name or 'touch' in name.lower()
    has_touch_abs  = _is_touch_bitmask(absbits)
    for h in hs:
        if not h.startswith('event'):
            continue
        if _udev_has_touchscreen_property('/dev/input/' + h):
            return True
        if has_touch_abs or has_touch_name:
            return True
    return False


def _nl_detect_touch_devices():
    devs = []
    p = Path('/proc/bus/input/devices')
    if not p.exists():
        return devs
    cur = {}
    for line in p.read_text().splitlines():
        if not line.strip():
            if cur:
                devs.append(cur); cur = {}
            continue
        if line.startswith('N:'):
            cur['name'] = line.split('N:', 1)[1].strip().lstrip('Name=').strip('"')
        elif line.startswith('H:'):
            cur.setdefault('handlers', '')
            cur['handlers'] += ' ' + line.split('H:', 1)[1].strip()
        elif line.startswith('B:') and 'EV=' in line:
            try:
                cur['ev'] = line.split('B:EV=', 1)[1].split()[0]
            except Exception:
                cur['ev'] = ''
        elif line.startswith('B:') and 'ABS=' in line:
            try:
                cur['abs'] = line.split('B:ABS=', 1)[1].split()[0]
            except Exception:
                cur['abs'] = ''
    if cur:
        devs.append(cur)
    out = []
    for d in devs:
        hs = (d.get('handlers') or '').split()
        absbits = d.get('abs') or ''
        ev = d.get('ev') or ''
        name = d.get('name') or '?'
        if not hs or not _is_touch_eligible(d):
            continue
        for h in hs:
            if not h.startswith('event'):
                continue
            dev_path = '/dev/input/' + h
            recommended = _udev_has_touchscreen_property(dev_path)
            reason = 'udev ID_INPUT_TOUCHSCREEN=1'
            if not recommended and (has_touch_abs := _is_touch_bitmask(absbits)):
                recommended = True
                reason = 'ABS_MT_POSITION_X/Y/_SLOT multi-touch surface'
            elif not recommended and ('Touch' in name or 'touch' in name.lower()):
                recommended = True
                reason = 'name contains "Touch"'
            out.append({
                'name': name,
                'path': dev_path,
                'type': 'touchscreen' if recommended else 'input',
                'recommended': recommended,
                'reason': reason,
            })
    out.sort(key=lambda x: (not x['recommended'], x['name']))
    return out

def _lan_ip_for_admin() -> str | None:
    try:
        # Prefer a UDP route lookup; it does not send packets but asks the kernel
        # which local source IP would reach the saved server/LAN.
        with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as sock:
            sock.settimeout(0.5)
            sock.connect(('11.1.1.53', 3000))
            ip = sock.getsockname()[0]
            if ip and not ip.startswith('127.'):
                return ip
    except Exception:
        pass
    try:
        out = _sp.check_output(['hostname', '-I'], text=True, timeout=2).split()
        for ip in out:
            if ip and not ip.startswith('127.') and ':' not in ip:
                return ip
    except Exception:
        pass
    return None

def _nl_status():
    cid_path = Path.home() / '.local/share/screentra-client/client-id'
    srv_path = Path.home() / '.local/share/screentra-client/server'
    client_id = cid_path.read_text().strip() if cid_path.exists() else None
    server_url = srv_path.read_text().strip() if srv_path.exists() else 'unknown'
    last_path = Handler.cache_dir / 'last-heartbeat.json'
    last_seen = connection = last_layout = None
    if last_path.exists():
        try:
            last = _read_json_file(last_path, {})
            last_seen = last.get('at')
            connection = last.get('connection', 'offline')
            last_layout = last.get('assigned_layout_id')
        except Exception:
            pass
    disp_path = Handler.cache_dir / f'display-{int(os.environ.get("RIPLEY_DISPLAY_INDEX", "0"))}.json'
    label = connector = resolution = None
    if disp_path.exists():
        try:
            d = _read_json_file(disp_path, {})
            label = d.get('label') or d.get('connector')
            connector = d.get('connector')
            w = d.get('width'); h = d.get('height')
            if w and h:
                resolution = f'{w}\u00d7{h}'
        except Exception:
            pass
    touch_pref = Handler.cache_dir / 'touch-device.json'
    selected = None
    if touch_pref.exists():
        try:
            selected = _read_json_file(touch_pref, {}).get('path')
        except Exception:
            pass
    devs = _nl_detect_touch_devices()
    touch_dev = None
    for d in devs:
        if d['path'] == selected:
            touch_dev = d['name']; break
    if not touch_dev and devs:
        touch_dev = devs[0]['name']
    display_index_for_layout = int(os.environ.get('RIPLEY_DISPLAY_INDEX', '0'))
    layout_status_file = Handler.cache_dir / f'assigned-layout-display-{display_index_for_layout}.json'
    if display_index_for_layout == 0 and not layout_status_file.exists():
        layout_status_file = Handler.cache_dir / 'assigned-layout.json'
    if layout_status_file.exists():
        try:
            last_layout = _read_json_file(layout_status_file, {}).get('id')
        except Exception:
            pass
    else:
        last_layout = None
    return {
        'screentra_client_client': True,
        'client_installed': True,
        'client_id': client_id,
        'identity_source': 'persistent_client_id',
        'server_url': server_url,
        'hostname': socket.gethostname() if 'socket' in dir() else os.uname().nodename,
        'lan_ip': _lan_ip_for_admin(),
        'admin_port': 3910,
        'admin_url': ('http://' + _lan_ip_for_admin() + ':3910/') if _lan_ip_for_admin() else None,
        'role': 'player',
        'port': int(os.environ.get('RIPLEY_RENDER_PORT', str(int(os.environ.get('RIPLEY_RENDER_BASE_PORT','3900')) + int(os.environ.get('RIPLEY_DISPLAY_INDEX','0'))))),
        'display_index': int(os.environ.get('RIPLEY_DISPLAY_INDEX', '0')),
        'display_label': label,
        'display_connector': connector,
        'display_resolution': resolution,
        'last_seen': last_seen,
        'connection': connection or 'offline',
        'assigned_layout_id': last_layout,
        'touch_device': touch_dev,
        'touch_path': selected,
        'build': os.environ.get('RIPLEY_BUILD', 'dev'),
    }


# === local admin / easy-setup page ===========================================

def _safe_run(args, timeout=5):
    try:
        cp = _sp.run(args, text=True, capture_output=True, timeout=timeout)
        return {'ok': cp.returncode == 0, 'code': cp.returncode, 'stdout': cp.stdout[-12000:], 'stderr': cp.stderr[-4000:]}
    except Exception as e:
        return {'ok': False, 'code': None, 'stdout': '', 'stderr': str(e)}

def _client_config_path() -> Path:
    return Handler.cache_dir.parent / 'config.json'

def _load_display_governance() -> dict:
    cfg = {'installer_detected_max_displays': 1, 'max_displays': None, 'last_detected_displays': []}
    try:
        path = _client_config_path()
        if path.exists():
            data = _read_json_file(path, {})
            if isinstance(data, dict):
                cfg.update({k: data.get(k) for k in cfg if k in data})
    except Exception as e:
        cfg['error'] = str(e)
    try:
        installer = max(int(cfg.get('installer_detected_max_displays') or 1), 1)
    except Exception:
        installer = 1
    override = cfg.get('max_displays')
    try:
        active = min(installer, max(int(override), 1)) if override is not None else installer
    except Exception:
        active = installer
    cfg['installer_detected_max_displays'] = installer
    cfg['active_max_displays'] = max(1, min(active, 4))
    cfg['config_path'] = str(_client_config_path())
    return cfg

def _load_client_config() -> dict:
    path = _client_config_path()
    try:
        if path.exists():
            data = _read_json_file(path, {})
            if isinstance(data, dict):
                return data
    except Exception:
        pass
    return {}

def _write_client_config(cfg: dict) -> None:
    path = _client_config_path()
    path.parent.mkdir(parents=True, exist_ok=True)
    _atomic_write_text(path, json.dumps(cfg, indent=2))

def _write_display_count(max_displays) -> dict:
    cfg = _load_client_config()
    installer = max(int(cfg.get('installer_detected_max_displays') or 1), 1)
    override = None if max_displays is None else max(1, min(int(max_displays), installer, 4))
    cfg['installer_detected_max_displays'] = installer
    cfg['max_displays'] = override
    cfg.setdefault('last_detected_displays', [])
    _write_client_config(cfg)
    pending = Handler.cache_dir / 'pending-command.json'
    pending.parent.mkdir(parents=True, exist_ok=True)
    _atomic_write_text(pending, json.dumps({'action':'set_display_count','payload':{'max_displays': override},'requested_at':_iso_now(),'requested_by':'local-admin'}, indent=2))
    _safe_run(['systemctl', '--user', 'restart', 'screentra-client-heartbeat.service'], timeout=6)
    actor = _safe_run(['systemctl', '--user', 'start', 'screentra-client-actor.service'], timeout=6)
    gov = _load_display_governance()
    return {'ok': True, 'max_displays': override, 'active_max_displays': gov['active_max_displays'], 'installer_detected_max_displays': installer, 'actor_start': actor}

def _resolve_touch_name(device_path: str | None, device_name: str | None = None) -> str | None:
    if device_name:
        return str(device_name)
    for dev in _nl_detect_touch_devices():
        if dev.get('path') == device_path:
            return dev.get('name')
    return None

def _display_connectors_for_indexes(indexes: list[int]) -> list[str]:
    cfg = _load_client_config()
    displays = cfg.get('last_detected_displays') or []
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
    if not out:
        wlr = _wlr_outputs()
        wlr.sort(key=lambda o: o.get('name') or '')
        for idx in indexes:
            if 0 <= idx < len(wlr):
                c = wlr[idx].get('name')
                if c and c not in out:
                    out.append(str(c))
    return out

def _current_touch_binding() -> dict:
    cfg = _load_client_config()
    binding = cfg.get('touch_binding') if isinstance(cfg.get('touch_binding'), dict) else {}
    legacy = Handler.cache_dir / 'touch-device.json'
    if not binding and legacy.exists():
        try:
            path = _read_json_file(legacy, {}).get('path')
            if path:
                binding = {'device_path': path, 'device_name': _resolve_touch_name(path), 'target_display_indexes': [0], 'target_connectors': _display_connectors_for_indexes([0]), 'allow_joined_displays': False}
        except Exception:
            pass
    return binding or {'device_path': None, 'device_name': None, 'target_display_indexes': [0], 'target_connectors': _display_connectors_for_indexes([0]), 'allow_joined_displays': False}

def _write_touch_binding(payload: dict) -> dict:
    cfg = _load_client_config()
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
    if not allow_joined:
        indexes = indexes[:1]
    else:
        indexes = indexes[:2]
    binding = {
        'device_path': dev_path,
        'device_name': dev_name,
        'target_display_indexes': indexes,
        'target_connectors': _display_connectors_for_indexes(indexes),
        'allow_joined_displays': allow_joined,
        'updated_at': _iso_now(),
        'updated_by': payload.get('requested_by') or 'local-admin',
    }
    cfg['touch_binding'] = binding
    _write_client_config(cfg)
    if dev_path:
        _atomic_write_text(Handler.cache_dir / 'touch-device.json', json.dumps({'path': dev_path, 'name': dev_name}, indent=2))
    pending = Handler.cache_dir / 'pending-command.json'
    pending.parent.mkdir(parents=True, exist_ok=True)
    _atomic_write_text(pending, json.dumps({'action':'set_touch_binding','payload': binding,'requested_at':_iso_now(),'requested_by':'local-admin'}, indent=2))
    _safe_run(['systemctl', '--user', 'restart', 'screentra-client-heartbeat.service'], timeout=6)
    actor = _safe_run(['systemctl', '--user', 'start', 'screentra-client-actor.service'], timeout=6)
    return {'ok': True, 'touch_binding': binding, 'actor_start': actor}

def _service_state(name: str) -> dict:
    active = _safe_run(['systemctl', '--user', 'is-active', name], timeout=2)
    enabled = _safe_run(['systemctl', '--user', 'is-enabled', name], timeout=2)
    return {'name': name, 'active': (active['stdout'] or active['stderr']).strip(), 'enabled': (enabled['stdout'] or enabled['stderr']).strip(), 'active_ok': active['ok']}

def _system_service_state(name: str) -> dict:
    active = _safe_run(['sudo', '-n', 'systemctl', 'is-active', name], timeout=2)
    enabled = _safe_run(['sudo', '-n', 'systemctl', 'is-enabled', name], timeout=2)
    return {'name': name, 'active': (active['stdout'] or active['stderr']).strip(), 'enabled': (enabled['stdout'] or enabled['stderr']).strip(), 'active_ok': active['ok']}

def _sysfs_outputs() -> list[dict]:
    out = []
    root = Path('/sys/class/drm')
    try:
        for d in sorted(root.iterdir()):
            if '-' not in d.name:
                continue
            status = d / 'status'
            if not status.exists():
                continue
            state = status.read_text().strip()
            if state != 'connected':
                continue
            name = d.name.split('-', 1)[1]
            mode = None
            modes = d / 'modes'
            if modes.exists():
                first = (modes.read_text().splitlines() or [''])[0]
                if 'x' in first:
                    try:
                        w, h = first.split('x', 1)[0], first.split('x', 1)[1].split()[0]
                        mode = {'width': int(w), 'height': int(h), 'source': 'sysfs'}
                    except Exception:
                        mode = {'label': first, 'source': 'sysfs'}
            out.append({'name': name, 'description': name, 'enabled': None, 'connected': True, 'position': {}, 'current_mode': mode, 'source': 'sysfs'})
    except Exception:
        return []
    return out

def _wlr_outputs() -> list[dict]:
    env = {**os.environ, 'XDG_RUNTIME_DIR': os.environ.get('XDG_RUNTIME_DIR', f'/run/user/{os.getuid()}'), 'WAYLAND_DISPLAY': os.environ.get('WAYLAND_DISPLAY', 'wayland-0')}
    try:
        out = _sp.check_output(['wlr-randr', '--json'], text=True, stderr=_sp.DEVNULL, timeout=3, env=env)
        data = json.loads(out)
        return [{'name': o.get('name'), 'description': o.get('description'), 'enabled': bool(o.get('enabled')), 'position': o.get('position') or {}, 'current_mode': next((m for m in o.get('modes', []) if m.get('current')), None)} for o in data]
    except Exception:
        return _sysfs_outputs()

def _read_tail(path: Path, max_bytes: int = 40000) -> str:
    try:
        if not path.exists() or not path.is_file():
            return ''
        with path.open('rb') as f:
            f.seek(0, os.SEEK_END); size = f.tell(); f.seek(max(0, size - max_bytes)); data = f.read()
        return data.decode('utf-8', 'replace')[-max_bytes:]
    except Exception as e:
        return f'error reading {path}: {e}'

def _journal_tail(unit: str, lines: int = 160, system: bool = False) -> str:
    cmd = ['journalctl', '--no-pager', '-n', str(lines), '-u', unit]
    if system: cmd = ['sudo', '-n'] + cmd
    else: cmd.insert(1, '--user')
    r = _safe_run(cmd, timeout=6)
    return (r.get('stdout') or r.get('stderr') or '')[-40000:]

def _admin_logs(target: str) -> dict:
    install = Path.home() / '.local/share/screentra-client'
    state = Path.home() / '.local/state/screentra-client'
    cache = install / 'cache'
    targets = {
        'heartbeat': lambda: _journal_tail('screentra-client-heartbeat.service'),
        'actor': lambda: _journal_tail('screentra-client-actor.service'),
        'renderer0': lambda: _journal_tail('screentra-client-renderer@0.service'),
        'kiosk0': lambda: _journal_tail('screentra-cast-kiosk-kiosk@0.service', system=True),
        'launcher': lambda: _read_tail(state / 'kiosk-launcher.log'),
        'touch': lambda: _read_tail(Path('/tmp/ripley-touch-events.log')),
        'install': lambda: '\n\n'.join(_read_tail(p) for p in sorted(Path('/tmp').glob('player-client-*-install.log'))[-4:]),
        'heartbeat-cache': lambda: _read_tail(cache / 'last-heartbeat.json'),
    }
    if target == 'all': return {'target': target, 'logs': {k: fn() for k, fn in targets.items()}}
    fn = targets.get(target)
    if not fn: return {'target': target, 'error': 'unknown log target', 'valid_targets': sorted(targets)}
    return {'target': target, 'text': fn()}

def _ca_file() -> Path:
    env = os.environ.get('RIPLEY_CA_CERT')
    return Path(env) if env else Path.home() / '.local/share/screentra-client/screentra-cast-signage-ca.crt'

def _sha256_file(path: Path) -> str | None:
    try:
        h = hashlib.sha256()
        with path.open('rb') as f:
            for chunk in iter(lambda: f.read(65536), b''): h.update(chunk)
        return h.hexdigest()
    except Exception:
        return None

def _cert_info() -> dict:
    srv_path = Path.home() / '.local/share/screentra-client/server'
    server_url = srv_path.read_text().strip().rstrip('/') if srv_path.exists() else ''
    ca = _ca_file(); ca_exists = ca.exists() and ca.stat().st_size > 0
    verify = {'ok': False, 'error': 'server or CA missing'}
    parsed = urllib.parse.urlparse(server_url) if server_url else None
    if parsed and parsed.hostname and ca_exists:
        port = parsed.port or (443 if parsed.scheme == 'https' else 80)
        try:
            if parsed.scheme == 'https':
                ctx = ssl.create_default_context(cafile=str(ca))
                with socket.create_connection((parsed.hostname, port), timeout=5) as sock:
                    with ctx.wrap_socket(sock, server_hostname=parsed.hostname) as ssock:
                        cert = ssock.getpeercert()
                verify = {'ok': True, 'host': parsed.hostname, 'port': port, 'subject': cert.get('subject'), 'issuer': cert.get('issuer'), 'not_after': cert.get('notAfter'), 'san': cert.get('subjectAltName')}
            else:
                verify = {'ok': True, 'warning': 'server URL is not HTTPS'}
        except Exception as e:
            verify = {'ok': False, 'error': str(e), 'host': parsed.hostname, 'port': port}
    return {'server_url': server_url, 'ca_path': str(ca), 'ca_exists': ca_exists, 'ca_size': ca.stat().st_size if ca.exists() else 0, 'ca_sha256': _sha256_file(ca), 'verify': verify, 'published_ca_url': (server_url + '/install/screentra-cast-signage-ca.crt') if server_url else ''}

def _refresh_ca_from_server() -> dict:
    srv_path = Path.home() / '.local/share/screentra-client/server'
    if not srv_path.exists(): return {'ok': False, 'error': 'server file missing'}
    server_url = srv_path.read_text().strip().rstrip('/')
    if not server_url: return {'ok': False, 'error': 'server URL empty'}
    url = server_url + '/install/screentra-cast-signage-ca.crt'; out = _ca_file()
    try:
        ca_path = _ca_file()
        if not ca_path.exists():
            return {'ok': False, 'error': 'existing CA certificate is required to refresh trust', 'url': url}
        ctx = _ssl.create_default_context(cafile=str(ca_path))
        data = _ur.urlopen(url, context=ctx, timeout=10).read()
        if len(data) < 200 or b'BEGIN CERTIFICATE' not in data:
            return {'ok': False, 'error': f'downloaded CA did not look like a certificate ({len(data)} bytes)', 'url': url}
        out.parent.mkdir(parents=True, exist_ok=True)
        tmp = out.with_suffix(out.suffix + '.tmp'); tmp.write_bytes(data); os.chmod(tmp, 0o644); tmp.replace(out)
        verify = _cert_info().get('verify')
        restart = _safe_run(['systemctl', '--user', 'restart', 'screentra-client-heartbeat.service', 'screentra-client-actor.service'], timeout=8)
        return {'ok': True, 'url': url, 'ca_path': str(out), 'bytes': len(data), 'sha256': _sha256_file(out), 'verify': verify, 'restart': restart}
    except Exception as e:
        return {'ok': False, 'error': str(e), 'url': url}



def _read_proc_stat_cpu() -> tuple[int, int] | None:
    try:
        parts = Path('/proc/stat').read_text().splitlines()[0].split()[1:]
        vals = [int(x) for x in parts]
        idle = vals[3] + (vals[4] if len(vals) > 4 else 0)
        total = sum(vals)
        return total, idle
    except Exception:
        return None

def _cpu_usage_percent(sample_secs: float = 0.12) -> float | None:
    a = _read_proc_stat_cpu()
    if not a:
        return None
    time.sleep(sample_secs)
    b = _read_proc_stat_cpu()
    if not b:
        return None
    total_delta = b[0] - a[0]
    idle_delta = b[1] - a[1]
    if total_delta <= 0:
        return None
    return round(max(0.0, min(100.0, (1.0 - (idle_delta / total_delta)) * 100.0)), 1)

def _cpu_temp_c() -> float | None:
    candidates = []
    try:
        for z in Path('/sys/class/thermal').glob('thermal_zone*/temp'):
            typ = z.parent.joinpath('type').read_text().strip().lower() if z.parent.joinpath('type').exists() else ''
            score = 0 if any(k in typ for k in ('cpu', 'x86_pkg_temp', 'k10temp', 'tctl', 'soc')) else 1
            candidates.append((score, z))
    except Exception:
        pass
    try:
        for z in Path('/sys/class/hwmon').glob('hwmon*/temp*_input'):
            label = ''
            label_path = z.with_name(z.name.replace('_input', '_label'))
            if label_path.exists():
                label = label_path.read_text().strip().lower()
            score = 0 if any(k in label for k in ('cpu', 'tctl', 'tdie', 'package', 'soc')) else 2
            candidates.append((score, z))
    except Exception:
        pass
    for _, path in sorted(candidates, key=lambda x: x[0]):
        try:
            raw = int(path.read_text().strip())
            c = raw / 1000.0 if raw > 1000 else float(raw)
            if -20.0 < c < 130.0:
                return round(c, 1)
        except Exception:
            continue
    return None

def _ram_usage() -> dict:
    info = {}
    try:
        for line in Path('/proc/meminfo').read_text().splitlines():
            k, v = line.split(':', 1)
            info[k] = int(v.strip().split()[0]) * 1024
        total = info.get('MemTotal') or 0
        avail = info.get('MemAvailable') or 0
        used = max(0, total - avail)
        pct = round((used / total) * 100.0, 1) if total else None
        return {'total_bytes': total, 'used_bytes': used, 'available_bytes': avail, 'percent': pct}
    except Exception as e:
        return {'error': str(e)}

def _gpu_usage() -> dict:
    helper = Path.home() / '.local/share/screentra-client/ripley-gpu-health.py'
    if helper.exists():
        try:
            r = _sp.run([sys.executable, str(helper)], stdout=_sp.PIPE, stderr=_sp.PIPE, text=True, timeout=4, check=False)
            if r.stdout.strip():
                data = json.loads(r.stdout.strip().splitlines()[-1])
                if r.stderr.strip():
                    data.setdefault('stderr', r.stderr.strip()[-500:])
                return data
            if r.stderr.strip():
                return {'percent': None, 'device': None, 'source': str(helper), 'error': r.stderr.strip()[-500:]}
        except Exception as e:
            return {'percent': None, 'device': None, 'source': str(helper), 'error': str(e)}
    # Fallback for partial/upgrading installs: generic DRM busy percent.
    out = {'percent': None, 'device': None, 'source': None, 'available': False, 'health': 'unknown', 'health_notes': ['GPU helper not installed yet']}
    try:
        for busy in sorted(Path('/sys/class/drm').glob('card*/device/gpu_busy_percent')):
            raw = busy.read_text().strip()
            out.update({'percent': float(raw), 'device': busy.parent.parent.name, 'source': str(busy), 'available': True, 'method': 'drm-gpu_busy_percent', 'health': 'ok'})
            return out
    except Exception as e:
        out['error'] = str(e)
    return out

def _system_metrics() -> dict:
    load = None
    try:
        load = os.getloadavg()
    except Exception:
        pass
    return {
        'cpu': {'percent': _cpu_usage_percent(), 'temp_c': _cpu_temp_c(), 'loadavg': load},
        'ram': _ram_usage(),
        'gpu': _gpu_usage(),
    }

def _client_backup_root() -> Path:
    p = Handler.cache_dir.parent / 'backups'
    p.mkdir(parents=True, exist_ok=True)
    return p

def _safe_tar_add(tar, path: Path, arcname: str, *, exclude_state: bool = True) -> int:
    """Add path to tar while excluding local identity/state/secrets.

    This is a software/config-shape backup, not a client clone. It intentionally
    skips cache/layouts, client_id, server URL, CA/certs/keys, auth sessions, and
    previously generated backups, and Chromium profiles so restoring it cannot impersonate the old box
    or overwrite target-server trust material.
    """
    added = 0
    deny_names = {'cache', 'admin-auth', 'backups', 'client-id', 'server', 'config.json'}
    deny_suffixes = ('.crt', '.key', '.pem', '.p12', '.pfx')
    try:
        if not path.exists():
            return 0
        if path.is_file():
            if exclude_state and (path.name in deny_names or path.name.endswith(deny_suffixes)):
                return 0
            tar.add(path, arcname=arcname, recursive=False)
            return 1
        for child in sorted(path.rglob('*')):
            rel = child.relative_to(path)
            parts = set(rel.parts)
            if exclude_state and (parts & deny_names):
                continue
            if exclude_state and any(str(part).startswith('chromium-profile') for part in rel.parts):
                continue
            if exclude_state and child.name.endswith(deny_suffixes):
                continue
            if child.is_file() or child.is_symlink():
                tar.add(child, arcname=str(Path(arcname) / rel), recursive=False)
                added += 1
    except Exception:
        return added
    return added

def _create_client_software_backup() -> dict:
    root = _client_backup_root()
    ts = _dt.now(_tz.utc).strftime('%Y%m%dT%H%M%SZ')
    host = socket.gethostname().replace('/', '-').replace(' ', '-')
    name = f'screentra-client-software-{host}-{ts}.tar.gz'
    out = root / name
    install = Path.home() / '.local/share/screentra-client'
    bindir = Path.home() / '.local/bin'
    user_units = Path.home() / '.config/systemd/user'
    manifest = {
        'created_at': _iso_now(),
        'hostname': socket.gethostname(),
        'kind': 'screentra-client-software-backup',
        'excluded_state': ['cache/', 'admin-auth/', 'backups/', 'chromium-profile*/', 'client-id', 'server', 'config.json', '*.crt', '*.key', '*.pem', '*.p12', '*.pfx'],
        'paths': [],
    }
    try:
        with tarfile.open(out, 'w:gz') as tar:
            n = _safe_tar_add(tar, install, 'screentra-client', exclude_state=True)
            manifest['paths'].append({'source': str(install), 'archive': 'screentra-client', 'files': n})
            helper_count = 0
            for helper in sorted(bindir.glob('ripley-*')):
                helper_count += _safe_tar_add(tar, helper, str(Path('local-bin') / helper.name), exclude_state=True)
            manifest['paths'].append({'source': str(bindir / 'ripley-*'), 'archive': 'local-bin/', 'files': helper_count})
            unit_count = 0
            for unit in sorted(user_units.glob('ripley-*.service')):
                unit_count += _safe_tar_add(tar, unit, str(Path('systemd-user') / unit.name), exclude_state=True)
            manifest['paths'].append({'source': str(user_units / 'ripley-*.service'), 'archive': 'systemd-user/', 'files': unit_count})
            for sys_unit in [Path('/etc/systemd/system/screentra-cast-kiosk-kiosk@.service')]:
                if sys_unit.exists():
                    manifest['paths'].append({'source': str(sys_unit), 'archive': str(Path('systemd-system') / sys_unit.name), 'files': _safe_tar_add(tar, sys_unit, str(Path('systemd-system') / sys_unit.name), exclude_state=True)})
            info = tarfile.TarInfo('MANIFEST.json')
            data = json.dumps(manifest, indent=2).encode()
            info.size = len(data)
            info.mtime = int(time.time())
            tar.addfile(info, fileobj=__import__('io').BytesIO(data))
        return {'ok': True, 'filename': name, 'path': str(out), 'bytes': out.stat().st_size, 'download_url': '/api/admin/client-backup/download?file=' + urllib.parse.quote(name), 'manifest': manifest}
    except Exception as e:
        try:
            if out.exists(): out.unlink()
        except Exception:
            pass
        return {'ok': False, 'error': str(e)}

def _reboot_device(confirm: bool = False) -> dict:
    """Schedule a device reboot after the HTTP response is sent.

    This endpoint is only reachable through the authenticated local admin port.
    Use sudo -n so a missing passwordless reboot rule fails immediately instead
    of hanging the renderer on a password prompt.
    """
    if not confirm:
        return {'ok': False, 'error': 'confirmation required'}
    def _run_reboot():
        try:
            _sp.Popen(['sudo', '-n', 'systemctl', 'reboot'], stdout=_sp.DEVNULL, stderr=_sp.DEVNULL)
        except Exception:
            pass
    # Quick permission probe that does not reboot.
    probe = _safe_run(['sudo', '-n', 'systemctl', '--help'], timeout=3)
    if not probe.get('ok'):
        return {'ok': False, 'error': 'sudo -n systemctl is not available for this user', 'probe': probe}
    threading.Timer(2.0, _run_reboot).start()
    return {'ok': True, 'scheduled': True, 'delay_seconds': 2, 'message': 'Device reboot scheduled.'}

def _admin_status() -> dict:
    base = _nl_status(); cache = Handler.cache_dir; install = cache.parent
    pending = install / 'pending-command.json'; result = cache / 'command-result.json'
    chromium = _safe_run(['pgrep', '-af', 'chromium.*127.0.0.1:390'], timeout=2)
    labwc = _safe_run(['pgrep', '-af', '^labwc'], timeout=2)
    services = [_system_service_state('screentra-client-heartbeat.service'), _system_service_state('screentra-client-actor.service'), _system_service_state('screentra-client-admin.service'), _system_service_state('screentra-cast-kiosk@0.service'), _system_service_state('screentra-cast-kiosk@1.service')]
    for svc in services:
        if svc.get('name') == 'screentra-client-actor.service' and svc.get('active') == 'inactive' and svc.get('enabled') == 'enabled':
            svc['active'] = 'ready'
            svc['active_ok'] = True
            svc['note'] = 'one-shot command actor; inactive means no pending command'
        if svc.get('name') == 'screentra-cast-kiosk-kiosk@1.service' and svc.get('active') == 'inactive' and svc.get('enabled') == 'disabled':
            svc['active'] = 'disabled'
            svc['active_ok'] = True
            svc['note'] = 'secondary kiosk output intentionally disabled while touchscreen-only mode is active'
    calibration = _read_json_file(cache / 'touch-calibration.json', [])
    pending_state = _read_json_file(pending, None)
    result_state = _read_json_file(result, None)
    return {'status': base, 'time': _iso_now(), 'uptime_seconds': time.monotonic(), 'auth': _auth_source(), 'metrics': _system_metrics(), 'display_governance': _load_display_governance(), 'services': services, 'processes': {'chromium': chromium.get('stdout', ''), 'labwc': labwc.get('stdout', '')}, 'outputs': _wlr_outputs(), 'touch': {'devices': _nl_detect_touch_devices(), 'binding': _current_touch_binding(), 'calibration': calibration}, 'ca': _cert_info(), 'command_state': {'pending_command': pending_state, 'last_result': result_state}}


# === local admin auth =========================================================

def _auth_dir() -> Path:
    p = Handler.cache_dir.parent / 'admin-auth'
    p.mkdir(parents=True, exist_ok=True)
    return p

def _auth_secret() -> bytes:
    p = _auth_dir() / 'session-secret'
    if not p.exists():
        p.write_text(secrets.token_urlsafe(32))
        try: os.chmod(p, 0o600)
        except Exception: pass
    return p.read_text().strip().encode()

def _password_hash(password: str, salt: str | None = None) -> str:
    salt = salt or secrets.token_hex(16)
    dk = hashlib.pbkdf2_hmac('sha256', password.encode(), salt.encode(), 120000)
    return 'pbkdf2_sha256$120000$' + salt + '$' + base64.b64encode(dk).decode()

def _verify_password(password: str, encoded: str) -> bool:
    try:
        if encoded.startswith('sha256:'):
            _prefix, salt, expected = encoded.split(':', 2)
            h = hashlib.sha256()
            h.update(salt.encode()); h.update(b':'); h.update(password.encode())
            return hmac.compare_digest(h.hexdigest(), expected)
        algo, rounds, salt, digest = encoded.split('$', 3)
        if algo != 'pbkdf2_sha256': return False
        dk = hashlib.pbkdf2_hmac('sha256', password.encode(), salt.encode(), int(rounds))
        return hmac.compare_digest(base64.b64encode(dk).decode(), digest)
    except Exception:
        return False

def _synced_users_path() -> Path:
    return Handler.cache_dir.parent / 'admin-users.json'

def _load_admin_users() -> list[dict]:
    path = _synced_users_path()
    if path.exists():
        try:
            data = _read_json_file(path, {})
            users = data.get('users') if isinstance(data, dict) else data
            if isinstance(users, list):
                return [u for u in users if isinstance(u, dict) and u.get('username') and (u.get('password_hash') or u.get('password'))]
        except Exception:
            pass
    return []

def _auth_source() -> dict:
    users = _load_admin_users()
    if users:
        try: synced_at = _read_json_file(_synced_users_path(), {}).get('synced_at')
        except Exception: synced_at = None
        return {'source': 'synced_server_users', 'fallback_default': False, 'user_count': len(users), 'synced_at': synced_at}
    return {'source': 'unprovisioned', 'fallback_default': False, 'user_count': 0, 'warning': 'No local admin users are provisioned; sync an operator account from the managed server.'}

def _check_login(username: str, password: str) -> tuple[bool, dict]:
    users = _load_admin_users()
    if users:
        for u in users:
            if hmac.compare_digest(str(u.get('username')), username):
                ph = u.get('password_hash')
                if ph and _verify_password(password, ph):
                    return True, {'username': username, 'source': 'synced_server_users'}
                if u.get('password') and hmac.compare_digest(str(u.get('password')), password):
                    return True, {'username': username, 'source': 'synced_server_users'}
        return False, {'error': 'invalid username or password', **_auth_source()}
    return False, {'error': 'local admin authentication is not provisioned', **_auth_source()}

def _make_session(username: str, source: str) -> str:
    exp = int(time.time()) + 12 * 3600
    nonce = secrets.token_urlsafe(12)
    body = f'{username}|{source}|{exp}|{nonce}'
    sig = hmac.new(_auth_secret(), body.encode(), hashlib.sha256).hexdigest()
    return base64.urlsafe_b64encode((body + '|' + sig).encode()).decode()

def _parse_cookie(header: str, name: str) -> str | None:
    for part in (header or '').split(';'):
        if '=' in part:
            k, v = part.strip().split('=', 1)
            if k == name: return v
    return None

def _session_user(cookie_header: str) -> dict | None:
    tok = _parse_cookie(cookie_header, 'ripley_admin_session')
    if not tok: return None
    try:
        raw = base64.urlsafe_b64decode(tok.encode()).decode()
        username, source, exp, nonce, sig = raw.rsplit('|', 4)
        body = f'{username}|{source}|{exp}|{nonce}'
        good = hmac.new(_auth_secret(), body.encode(), hashlib.sha256).hexdigest()
        if not hmac.compare_digest(sig, good): return None
        if int(exp) < int(time.time()): return None
        return {'username': username, 'source': source, 'expires_at': int(exp)}
    except Exception:
        return None

def _admin_login_html() -> str:
    src = _auth_source()
    warn = 'No local admin account is provisioned yet; sync an operator account from the managed server.' if not src.get('user_count') else 'Using synced server users.'
    return '<!doctype html><html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>Screentra Cast Admin Sign In</title><style>body{margin:0;background:#07111f;color:#e5eefc;font-family:system-ui,-apple-system,Segoe UI,sans-serif;display:grid;place-items:center;min-height:100vh}.card{width:min(430px,92vw);background:#0f172a;border:1px solid #243247;border-radius:18px;padding:24px;box-shadow:0 20px 70px rgba(0,0,0,.4)}h1{margin:0 0 8px}.muted{color:#91a4bf}.warn{border:1px solid rgba(245,158,11,.45);background:rgba(245,158,11,.08);border-radius:12px;padding:10px;margin:14px 0;color:#facc15}label{display:block;margin-top:12px;color:#91a4bf;font-size:12px;text-transform:uppercase;letter-spacing:.08em}input{width:100%;margin-top:6px;padding:12px;border-radius:10px;border:1px solid #243247;background:#07111f;color:#e5eefc;font-size:16px}button{width:100%;margin-top:16px;padding:12px;border:0;border-radius:999px;background:#38bdf8;color:#03131f;font-weight:900;font-size:15px}.err{color:#fca5a5;min-height:20px;margin-top:10px}</style></head><body><form class="card" id="f"><h1>Screentra Cast Admin</h1><div class="muted">Sign in to manage this local player.</div><div class="warn">'+warn+'</div><label>Username</label><input id="u" autocomplete="username" value="admin"><label>Password</label><input id="p" type="password" autocomplete="current-password" autofocus><button>Sign in</button><div class="err" id="e"></div></form><script>document.getElementById("f").addEventListener("submit",async ev=>{ev.preventDefault();e.textContent="Signing in…";try{const r=await fetch("/api/admin/auth/login",{method:"POST",headers:{"Content-Type":"application/json"},body:JSON.stringify({username:u.value,password:p.value})});const j=await r.json();if(r.ok&&j.ok){location.href="/"}else{e.textContent=j.error||"Login failed"}}catch(x){e.textContent=String(x)}});</script></body></html>'

def _admin_auth_payload(handler) -> dict:
    user = _session_user(handler.headers.get('Cookie', ''))
    return {'authenticated': bool(user), 'user': user, **_auth_source()}

def _admin_requires_auth(path: str) -> bool:
    if os.environ.get('RIPLEY_ADMIN_MODE', '').lower() not in ('1', 'true', 'yes'):
        return False
    if path in ('/api/admin/auth/login', '/api/admin/auth/status', '/login', '/login.html'):
        return False
    if path.startswith('/api/admin') or path in ('/', '/admin.html'):
        return True
    return False

def _mutation_requires_auth(path: str) -> bool:
    if path in ('/api/admin/auth/login', '/api/admin/auth/logout', '/api/admin/auth/status'):
        return False
    return path in ('/api/touchscreen/select', '/api/touch-calibration.json', '/api/client-config.json') or path.startswith('/api/admin/')

ADMIN_HTML = """<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>Screentra Cast Player Admin</title><style>
:root{--bg:#07111f;--panel:#0f172a;--panel2:#111c31;--line:#26364f;--text:#e5eefc;--muted:#91a4bf;--ok:#22c55e;--warn:#f59e0b;--err:#ef4444;--accent:#38bdf8;--violet:#a78bfa}*{box-sizing:border-box}body{margin:0;background:radial-gradient(circle at top left,rgba(56,189,248,.14),transparent 34rem),linear-gradient(160deg,#07111f,#020617);color:var(--text);font:14px/1.45 system-ui,-apple-system,Segoe UI,sans-serif}.wrap{max-width:1380px;margin:0 auto;padding:24px}.top{display:flex;gap:16px;align-items:flex-start;justify-content:space-between;margin-bottom:18px}.title h1{margin:0;font-size:30px;letter-spacing:-.03em}.title p{margin:6px 0 0;color:var(--muted)}.pill{display:inline-flex;align-items:center;gap:6px;padding:5px 10px;border-radius:999px;border:1px solid var(--line);font-weight:900;font-size:12px;text-transform:uppercase;letter-spacing:.08em}.ok{color:var(--ok);border-color:rgba(34,197,94,.5)}.ready{color:#7dd3fc;border-color:rgba(125,211,252,.62);background:rgba(14,116,144,.12)}.disabled{color:#cbd5e1;border-color:rgba(203,213,225,.38);background:rgba(148,163,184,.10)}.warn{color:var(--warn);border-color:rgba(245,158,11,.5)}.err{color:var(--err);border-color:rgba(239,68,68,.5)}.muted{color:var(--muted)}.small{color:var(--muted);font-size:12px}.cards{display:grid;grid-template-columns:repeat(3,minmax(0,1fr));gap:12px;margin:14px 0 18px}.metric{background:linear-gradient(180deg,rgba(17,28,49,.96),rgba(15,23,42,.9));border:1px solid var(--line);border-radius:18px;padding:16px;box-shadow:0 14px 44px rgba(0,0,0,.24)}.metric .label{color:var(--muted);font-size:12px;text-transform:uppercase;letter-spacing:.08em}.metric .value{font-size:28px;font-weight:950;margin-top:7px;letter-spacing:-.04em}.metric .sub{color:var(--muted);margin-top:4px;min-height:20px}.bar{height:8px;background:#07111f;border-radius:999px;overflow:hidden;border:1px solid rgba(148,163,184,.18);margin-top:12px}.fill{height:100%;background:linear-gradient(90deg,var(--accent),var(--violet));width:0%}.sections{display:grid;gap:12px}.section,.panelcard{background:rgba(15,23,42,.9);border:1px solid var(--line);border-radius:16px;overflow:hidden;box-shadow:0 12px 38px rgba(0,0,0,.22)}.panelcard{padding:16px 18px 18px}.panelcard h2{margin:0 0 12px;font-size:16px}.fixed-panels{margin:14px 0 12px}details.section>summary{list-style:none;cursor:pointer;display:flex;align-items:center;justify-content:space-between;gap:12px;padding:16px 18px;font-weight:950;font-size:16px}details.section>summary::-webkit-details-marker{display:none}.chev{color:var(--muted);font-size:18px;transition:transform .16s ease}details[open] .chev{transform:rotate(90deg)}.body{border-top:1px solid var(--line);padding:16px 18px 18px;background:rgba(2,6,23,.16)}.kv{display:grid;grid-template-columns:170px 1fr;gap:8px 12px}.k{color:var(--muted);font-size:12px;text-transform:uppercase;letter-spacing:.06em}.v{word-break:break-word}.actions{display:flex;gap:8px;flex-wrap:wrap;margin-top:12px;align-items:center}button,select,a.btn{background:#17243a;color:var(--text);border:1px solid var(--line);border-radius:10px;padding:9px 11px;font-weight:800;text-decoration:none;cursor:pointer}button:hover,a.btn:hover,select:hover{border-color:var(--accent)}button.danger{background:rgba(127,29,29,.72);border-color:rgba(239,68,68,.65);color:#fecaca}.split{display:grid;grid-template-columns:1fr 1fr;gap:14px}.service{display:flex;align-items:center;justify-content:space-between;gap:12px;border-bottom:1px solid rgba(148,163,184,.12);padding:10px 0}.service:last-child{border-bottom:0}pre{margin:0;white-space:pre-wrap;word-break:break-word;max-height:420px;overflow:auto;background:#050b14;border:1px solid #182338;border-radius:10px;padding:12px;color:#cbd5e1}.logbar{display:flex;gap:8px;margin-bottom:10px;flex-wrap:wrap}.statusline{margin-top:10px;color:var(--muted)}label{display:inline-flex;gap:7px;align-items:center;color:var(--muted)}input[type=checkbox]{accent-color:var(--accent)}@media(max-width:1000px){.cards{grid-template-columns:repeat(2,minmax(0,1fr))}.split{grid-template-columns:1fr}}@media(max-width:640px){.wrap{padding:14px}.top{display:block}.cards{grid-template-columns:1fr}.kv{grid-template-columns:1fr}.actions{align-items:stretch}.actions>*{width:100%;justify-content:center}}
</style></head><body><div class="wrap"><div class="top"><div class="title"><h1>Screentra Cast Player Admin</h1><p>Easy setup / diagnostics for this local signage player. No layout editor here.</p></div><div id="overall" class="pill warn">LOADING</div></div>
<div class="cards"><section class="metric"><div class="label">CPU</div><div id="cpuValue" class="value">—</div><div id="cpuSub" class="sub">Temp —</div><div class="bar"><div id="cpuBar" class="fill"></div></div></section><section class="metric"><div class="label">RAM</div><div id="ramValue" class="value">—</div><div id="ramSub" class="sub">—</div><div class="bar"><div id="ramBar" class="fill"></div></div></section><section class="metric"><div class="label">GPU</div><div id="gpuValue" class="value">—</div><div id="gpuSub" class="sub">—</div><div class="bar"><div id="gpuBar" class="fill"></div></div></section></div>
<div class="sections">
<div class="split fixed-panels"><section class="panelcard"><h2>Device online / working</h2><div id="device" class="kv"></div><div class="actions"><button onclick="refreshAll()">Refresh</button><a class="btn" href="/">Open player</a><a class="btn" href="/touch-calibration.html">Touch calibration</a></div><div id="deviceMsg" class="statusline"></div></section><section class="panelcard"><h2>Services</h2><div id="services"></div><div class="actions"><button onclick="restartCore()">Restart heartbeat + actor</button><button class="danger" onclick="rebootDevice()">Reboot device</button></div></section></div>
<details class="section"><summary><span>Backup</span><span class="chev">›</span></summary><div class="body"><div class="kv"><div class="k">Client software backup</div><div class="v">Creates a portable software/config-shape archive for this player. It excludes client identity, server URL, config.json, CA/certs/keys, cached layouts, admin sessions, Chromium profiles, and previous backups.</div><div class="k">Update recommendation</div><div class="v">For client software updates, use the server-served installer/update_client path unless the client cannot reach the server. A separate upload-style update file would need package signing, CA preservation, rollback, and offline restore rules.</div></div><div class="actions"><button onclick="backupClient()">Create client backup</button></div><div id="backupMsg" class="statusline"></div></div></details>
<details class="section"><summary><span>Certificate / CA</span><span class="chev">›</span></summary><div class="body"><div id="ca" class="kv"></div><div class="actions"><button onclick="refreshCa()">Refresh CA from server</button><button onclick="loadCerts()">Re-check cert</button></div><div id="caMsg" class="statusline"></div></div></details>
<details class="section"><summary><span>Displays / touch</span><span class="chev">›</span></summary><div class="body"><div class="split"><div><h3>Displays</h3><div id="displays" class="kv"></div><div class="actions"><label>Active displays <select id="displayCount"><option value="1">1</option><option value="2">2</option><option value="3">3</option><option value="4">4</option></select></label><button onclick="saveDisplayCount()">Apply display count</button></div></div><div><h3>Touch binding</h3><div id="touchSummary" class="kv"></div><div class="actions"><label>Touch device <select id="touchDevice"></select></label><label>Bind to display <select id="touchDisplayA"></select></label><label><input id="touchJoined" type="checkbox" onchange="toggleTouchJoined()"> allow joined/tiled two-display touch surface</label><label id="touchDisplayBWrap" style="display:none">Second display <select id="touchDisplayB"></select></label><button onclick="saveTouchBinding()">Apply touch binding</button></div></div></div><div id="displayMsg" class="statusline"></div></div></details>
<details class="section"><summary><span>Logs</span><span class="chev">›</span></summary><div class="body"><div class="logbar"><select id="logTarget"><option value="heartbeat">heartbeat</option><option value="actor">actor</option><option value="renderer0">renderer0</option><option value="kiosk0">labwc kiosk</option><option value="launcher">launcher</option><option value="touch">touch</option><option value="install">install</option><option value="heartbeat-cache">heartbeat cache</option><option value="all">all</option></select><button onclick="loadLogs()">Load logs</button></div><pre id="logs">Select a log target.</pre></div></details>
</div></div><script>
const $=id=>document.getElementById(id); const esc=s=>String(s??'').replace(/[&<>\"']/g,c=>({'&':'&amp;','<':'&lt;','>':'&gt;','\"':'&quot;',"'":'&#39;'}[c]));
function kv(keys){return keys.map(([k,v])=>`<div class="k">${esc(k)}</div><div class="v">${esc(v??'—')}</div>`).join('')} function pill(ok,txt,cls){return `<span class="pill ${cls|| (ok?'ok':'err')}">${esc(txt)}</span>`} function servicePill(x){const active=String(x.active||'unknown').toLowerCase(); const enabled=String(x.enabled||'').toLowerCase(); if(active==='ready')return pill(true,'READY','ready'); if(active==='disabled'||enabled==='disabled')return pill(true,'DISABLED','disabled'); return pill(!!x.active_ok,x.active||'unknown');}
function pct(v){v=Number(v); return Number.isFinite(v)?Math.max(0,Math.min(100,v)):0} function fmtPct(v){return Number.isFinite(Number(v))?Number(v).toFixed(1)+'%':'—'} function bytes(n){n=Number(n); if(!Number.isFinite(n)||n<=0)return '—'; const u=['B','KB','MB','GB','TB']; let i=0; while(n>=1024&&i<u.length-1){n/=1024;i++} return n.toFixed(i?1:0)+' '+u[i]}
async function api(path,opts){const r=await fetch(path,{cache:'no-store',...(opts||{})}); const t=await r.text(); try{return JSON.parse(t)}catch{return {raw:t,status:r.status}}}
async function refreshAll(){const d=await api('/api/admin/status'); renderStatus(d)}
function renderCards(d){const m=d.metrics||{}, cpu=m.cpu||{}, ram=m.ram||{}, gpu=m.gpu||{}, s=d.status||{}; $('cpuValue').textContent=fmtPct(cpu.percent); $('cpuSub').textContent='Temp '+(cpu.temp_c!=null?cpu.temp_c+' °C':'—')+(cpu.loadavg?` · load ${Number(cpu.loadavg[0]).toFixed(2)}`:''); $('cpuBar').style.width=pct(cpu.percent)+'%'; $('ramValue').textContent=fmtPct(ram.percent); $('ramSub').textContent=`${bytes(ram.used_bytes)} / ${bytes(ram.total_bytes)}`; $('ramBar').style.width=pct(ram.percent)+'%'; $('gpuValue').textContent=gpu.percent!=null?fmtPct(gpu.percent):(gpu.temp_c!=null?gpu.temp_c+' °C':'—'); const gpuNotes=(gpu.health_notes||[]).join(' · '); $('gpuSub').textContent=(gpu.device||gpu.vendor||'GPU')+(gpu.health?` · ${gpu.health}`:'')+(gpu.percent==null?' · usage n/a':'')+(gpu.frequency_mhz!=null?` · ${gpu.frequency_mhz} MHz`:'')+(gpu.vram_percent!=null?` · VRAM ${fmtPct(gpu.vram_percent)}`:'')+(gpuNotes?` · ${gpuNotes}`:''); $('gpuBar').style.width=pct(gpu.percent)+'%';}
function renderStatus(d){const s=d.status||{}; $('overall').className='pill '+(s.connection==='online'?'ok':'err'); $('overall').textContent=s.connection==='online'?'ONLINE':'CHECK'; renderCards(d); $('device').innerHTML=kv([['Hostname',s.hostname],['LAN admin URL',s.admin_url],['Client ID',s.client_id],['Server',s.server_url],['Connection',s.connection],['Last seen',s.last_seen],['Display',`${s.display_label||'—'} · ${s.display_resolution||'—'} · idx ${s.display_index}`],['Layout',s.assigned_layout_id||'No layout assigned'],['Touch',s.touch_device||'not detected']]); const ca=d.ca||{}; $('ca').innerHTML=kv([['CA path',ca.ca_path],['CA SHA256',ca.ca_sha256],['Published CA',ca.published_ca_url],['Verify',ca.verify?.ok?'OK':(ca.verify?.error||'failed')],['Server cert expires',ca.verify?.not_after]]); $('services').innerHTML=(d.services||[]).map(x=>`<div class="service"><div><b>${esc(x.name)}</b><div class="small">enabled: ${esc(x.enabled)}${x.note?' · '+esc(x.note):''}</div></div>${servicePill(x)}</div>`).join(''); const g=d.display_governance||{}; const dc=$('displayCount'); if(dc) dc.value=String(g.active_max_displays||1); renderTouchControls(d); const tb=d.touch?.binding||{}; $('displays').innerHTML=kv([['Active display count',`${g.active_max_displays||1} / detected ${g.installer_detected_max_displays||1}`],['Outputs',(d.outputs||[]).map(o=>`${o.name}:${o.enabled===true?'on':(o.enabled===false?'off':'connected')} ${o.current_mode?(o.current_mode.width?o.current_mode.width+'x'+o.current_mode.height:(o.current_mode.label||'')):''}`).join(' | ')||'—'],['Calibration runs',(d.touch?.calibration||[]).length]]); $('touchSummary').innerHTML=kv([['Touch devices',(d.touch?.devices||[]).map(t=>`${t.name} ${t.path} ${t.reason||''}`).join(' | ')||'—'],['Touch binding',tb.device_name?`${tb.device_name} → ${(tb.target_connectors||tb.target_display_indexes||[]).join(', ')||'all active outputs'}${tb.allow_joined_displays?' (joined/tiled span)':''}`:'not set']]);}
async function loadCerts(){const c=await api('/api/admin/certificates'); $('ca').innerHTML=kv([['CA path',c.ca_path],['CA SHA256',c.ca_sha256],['Published CA',c.published_ca_url],['Verify',c.verify?.ok?'OK':(c.verify?.error||'failed')],['Server cert expires',c.verify?.not_after]]);}
async function refreshCa(){if(!confirm('Refresh the local CA from the saved server URL and restart heartbeat/actor?')) return; $('caMsg').textContent='Refreshing CA…'; const r=await api('/api/admin/ca/refresh',{method:'POST'}); $('caMsg').textContent=r.ok?'CA refreshed and services restarted.':('CA refresh failed: '+(r.error||JSON.stringify(r))); await loadCerts();}
async function restartCore(){if(!confirm('Restart heartbeat and actor services?'))return; const r=await api('/api/admin/services/restart',{method:'POST'}); alert(r.ok?'Restart requested.':'Restart failed: '+(r.error||JSON.stringify(r))); await refreshAll();}
async function rebootDevice(){if(!confirm('Reboot this player device now? The screen will go offline while it restarts.'))return; if(!confirm('Final confirmation: reboot the device?'))return; const msg=$('deviceMsg'); if(msg) msg.textContent='Scheduling reboot…'; const r=await api('/api/admin/device/reboot',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({confirm:true})}); if(msg) msg.textContent=r.ok?'Reboot scheduled. The admin page will disconnect shortly.':('Reboot failed: '+(r.error||JSON.stringify(r))); if(!r.ok) alert('Reboot failed: '+(r.error||JSON.stringify(r)));}
function renderTouchControls(d){const devSel=$('touchDevice'), a=$('touchDisplayA'), b=$('touchDisplayB'), joined=$('touchJoined'); if(!devSel||!a||!b||!joined)return; const tb=d.touch?.binding||{}; devSel.innerHTML=(d.touch?.devices||[]).map(t=>`<option value="${esc(t.path)}" data-name="${esc(t.name)}" ${t.path===tb.device_path?'selected':''}>${esc(t.name)} · ${esc(t.path)}</option>`).join('') || '<option value="">No touch device detected</option>'; const outs=(d.outputs||[]); const opts=outs.map((o,i)=>`<option value="${i}">${esc(o.name||('display '+i))}</option>`).join('') || '<option value="0">display 0</option>'; a.innerHTML=opts; b.innerHTML=opts; const idxs=tb.target_display_indexes||[0]; a.value=String(idxs[0]??0); b.value=String(idxs[1]??1); joined.checked=!!tb.allow_joined_displays; toggleTouchJoined();}
function toggleTouchJoined(){const wrap=$('touchDisplayBWrap'); if(wrap) wrap.style.display=$('touchJoined')?.checked?'inline-flex':'none';}
async function saveTouchBinding(){const dev=$('touchDevice'); const joined=!!$('touchJoined')?.checked; const idxs=[Number($('touchDisplayA')?.value||0)]; if(joined) idxs.push(Number($('touchDisplayB')?.value||1)); if(joined&&idxs[0]===idxs[1]){alert('Choose two different displays for joined/tiled touch mode.');return;} if(joined&&!confirm('Joined/tiled mode maps this touchscreen across the active desktop for two-display Surface/NVIDIA-style systems. Continue?'))return; const payload={device_path:dev?.value||null,device_name:dev?.selectedOptions?.[0]?.dataset?.name||null,target_display_indexes:idxs,allow_joined_displays:joined}; const msg=$('displayMsg'); if(msg) msg.textContent='Applying touch binding…'; const r=await api('/api/admin/touch-binding',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify(payload)}); if(msg) msg.textContent=r.ok?'Touch binding applied; labwc/kiosk services are restarting.':('Failed: '+(r.error||JSON.stringify(r))); setTimeout(refreshAll,4000);}
async function saveDisplayCount(){const v=Number($('displayCount').value||1); if(v>1&&!confirm('Enable '+v+' active displays? Touch coordinates may change until outputs settle.'))return; const msg=$('displayMsg'); if(msg) msg.textContent='Applying display count…'; const r=await api('/api/admin/display-count',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({max_displays:v})}); if(msg) msg.textContent=r.ok?'Display count applied; kiosk services are restarting.':('Failed: '+(r.error||JSON.stringify(r))); setTimeout(refreshAll,2500);}
async function backupClient(){if(!confirm('Create a client software backup archive? This excludes client identity, server URL, CA/certs, cached layouts, auth sessions, Chromium profiles, and previous backups.'))return; const msg=$('backupMsg'); if(msg) msg.textContent='Creating backup…'; const r=await api('/api/admin/client-backup',{method:'POST'}); if(r.ok){if(msg) msg.innerHTML='Backup created: <a class="btn" href="'+esc(r.download_url)+'">Download '+esc(r.filename)+'</a> · '+bytes(r.bytes);} else {if(msg) msg.textContent='Backup failed: '+(r.error||JSON.stringify(r));}}
async function loadLogs(){const target=$('logTarget').value; $('logs').textContent='Loading…'; const r=await api('/api/admin/logs?target='+encodeURIComponent(target)); $('logs').textContent=r.text || (r.logs?Object.entries(r.logs).map(([k,v])=>'## '+k+'\\n'+v).join('\\n\\n') : JSON.stringify(r,null,2));}
refreshAll(); setInterval(refreshAll, 8000);
</script></body></html>"""
from urllib.parse import unquote

PLAYER_HTML = r'''<!doctype html>
<html>
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Screentra Cast Cached Player</title>
<style>
*{box-sizing:border-box} html,body,#player{margin:0;width:100vw;height:100vh;background:#000;overflow:hidden;color:#fff;font-family:system-ui,-apple-system,Segoe UI,sans-serif;cursor:none}.widget{position:absolute;overflow:hidden;cursor:none} iframe{background:#000;cursor:none}.fallback{display:grid;place-items:center;height:100vh;text-align:center;color:#94a3b8}.fallback strong{display:block;color:#e2e8f0;font-size:2rem;margin-bottom:.5rem}.clock,.date{display:flex;align-items:center;justify-content:center;width:100%;height:100%}iframe{background:#000;cursor:none}.was-link{display:inline-flex;align-items:center;gap:.45em;border:1px dashed rgba(125,211,252,.65);background:rgba(14,116,144,.16);color:#bae6fd;border-radius:999px;padding:.14em .55em;text-decoration:none}.was-link:after{content:'was link';font-size:.68em;letter-spacing:.08em;text-transform:uppercase;color:#7dd3fc}
</style>
</head>
<body><div id="player"><div class="fallback"><div><strong>Waiting for assigned layout</strong><span>Heartbeat will cache a layout when assigned.</span></div></div></div>
<script>
let currentHash='';
function esc(s){return String(s??'').replace(/[&<>"']/g,c=>({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));}
function normWidgetType(w){return (w.widget_type || w.type || '').toLowerCase().replace(/_/g,'-');}
function props(w){return w.props || w.properties || {};}
function isYouTubeUrl(u){return /(?:youtube\.com|youtu\.be)/i.test(String(u||''));}
function youtubeId(u){const s=String(u||'');let m=s.match(/[?&]v=([^&]+)/); if(m) return m[1]; m=s.match(/youtu\.be\/([^?&/]+)/); if(m) return m[1]; m=s.match(/youtube\.com\/(?:embed|shorts)\/([^?&/]+)/); return m?m[1]:'';}
function youtubeEmbedUrl(u,p){const id=youtubeId(u); const params=new URLSearchParams({autoplay:'1',mute:(p.mute||p.muted)?'1':'0',loop:'1',playsinline:'1',rel:'0'}); if(id) params.set('playlist',id); return id ? ('https://www.youtube.com/embed/'+encodeURIComponent(id)+'?'+params.toString()) : String(u||'');}
function styleFor(w){const x=(Number(w.x)||0)*100,y=(Number(w.y)||0)*100,ww=(Number(w.w??w.width)||0.1)*100,hh=(Number(w.h??w.height)||0.1)*100,z=Number(w.z)||1;return `left:${x}%;top:${y}%;width:${ww}%;height:${hh}%;z-index:${z};`;}
function wmoIcon(code){const m={'0':'☀️','1':'🌤️','2':'⛅','3':'☁️','45':'🌫️','48':'🌫️','51':'🌦️','53':'🌦️','55':'🌦️','61':'🌧️','63':'🌧️','65':'🌧️','71':'❄️','73':'❄️','75':'❄️','80':'🌦️','81':'🌧️','82':'🌧️','95':'⛈️','96':'⛈️','99':'⛈️'};return m[String(code)]||'🌤️';}
function wmoIconHtml(code,p){const key=String(code),custom=p&&p.custom_icons&&p.custom_icons[key];if(custom)return `<img src="${esc(custom)}" style="width:100%;height:100%;object-fit:contain;">`;const n=Number(code),sun=n<=1,cloud=n>=2,rain=(n>=51&&n<=67)||(n>=80&&n<=82),snow=n>=71&&n<=77,storm=n>=95;let body='';if(sun)body+='<circle cx="28" cy="28" r="13" fill="#facc15"/><g stroke="#facc15" stroke-width="4"> <path d="M28 5v9M28 42v9M5 28h9M42 28h9M12 12l7 7M37 37l7 7M44 12l-7 7M19 37l-7 7"/></g>';if(cloud)body+='<path d="M20 47h27a12 12 0 0 0 0-24 18 18 0 0 0-34 5 10 10 0 0 0 7 19z" fill="#cbd5e1" stroke="#64748b" stroke-width="2"/>';if(rain)body+='<g stroke="#38bdf8" stroke-width="4" stroke-linecap="round"><path d="M22 43l-4 9M34 43l-4 9M46 43l-4 9"/></g>';if(snow)body+='<g stroke="#e0f2fe" stroke-width="3" stroke-linecap="round"><path d="M20 42v14M13 49h14M15 44l10 10M25 44L15 54M37 42v14M30 49h14M32 44l10 10M42 44L32 54"/></g>';if(storm)body+='<path d="M30 32h12L29 57l3-16H21l9-19z" fill="#facc15"/>';if(!body)body='<circle cx="32" cy="32" r="20" fill="#facc15"/>';return `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64" style="width:100%;height:100%;padding:8px;">${body}</svg>`;}
function weatherFor(p,i){const d=p._weather_data||{},q=d.daily||{},codes=q.weather_code||q.weathercode||[],code=codes[i]??d.current_weather?.weather_code??d.current_weather?.weathercode??2,winds=q.wind_speed||q.windspeed||[],wind=winds[i]??d.current_weather?.windspeed??0;return {code,icon:wmoIcon(code),high:q.temperature_2m_max?.[i],low:q.temperature_2m_min?.[i],precip:q.precipitation_probability_max?.[i]??0,wind};}
function repeatCount(p,e){const r=e.repeat||{};return r.group?Math.max(1,Math.min(14,Number(p.shown_days)||Number(r.count)||4)):Math.max(1,Number(r.count)||1);}
function groupMetrics(es){const g={};es.forEach(e=>{const id=e.repeat&&e.repeat.group;if(!id)return;const r=e.repeat,x=Number(e.x)||0,y=Number(e.y)||0,w=Number(e.width)||120,h=Number(e.height)||80,m=g[id]||(g[id]={minX:x,minY:y,maxX:x+w,maxY:y+h,gapX:Number(r.gap_x)||0,gapY:Number(r.gap_y)||0,mode:r.mode||'rows'});m.minX=Math.min(m.minX,x);m.minY=Math.min(m.minY,y);m.maxX=Math.max(m.maxX,x+w);m.maxY=Math.max(m.maxY,y+h);});return g;}
function templateEl(e,p,i,g,dw,dh){const q=Object.assign({},e.props||{}),r=e.repeat||{},m=r.group&&g[r.group];if(i){const n=Number(q.day_offset??q.weather_day_offset)||0;q.day_offset=n+i;q.weather_day_offset=n+i;}let x=Number(e.x)||0,y=Number(e.y)||0,w=Number(e.width)||120,h=Number(e.height)||80;if(m&&i){if((m.mode||'rows')==='columns')x+=i*(m.maxX-m.minX+m.gapX);else y+=i*(m.maxY-m.minY+m.gapY);}const a=q.align||'center',j=a==='left'?'flex-start':a==='right'?'flex-end':'center';let st=`position:absolute;box-sizing:border-box;left:${x/dw*100}%;top:${y/dh*100}%;width:${w/dw*100}%;height:${h/dh*100}%;display:flex;align-items:center;justify-content:${j};text-align:${a};overflow:hidden;z-index:${Number(e.z)||0};color:${q.color||'#fff'};font-size:${Number(q.font_size)||24}px;`;let inner='';if(e.type==='rectangle')st+=`background:${q.color||'#1e293b'};opacity:${Number(q.opacity??100)/100};border-radius:${Number(q.radius)||0}px;`;else if(e.type==='text')inner=esc(q.text||'Text');else if(['variable_icon','variable_text','variable','date'].includes(e.type)){const k=q.variable_key||'',wx=weatherFor(p,Math.max(0,Math.min(6,Number(q.day_offset??q.weather_day_offset)||0)));if(k==='weather_icon'||e.type==='variable_icon'||q.display_mode==='weather_icon')inner=wmoIconHtml(wx.code,p);else if(k==='weather_description')inner=esc('Weather');else if(k==='high_temp')inner=esc('High '+(wx.high??'—'));else if(k==='low_temp')inner=esc('Low '+(wx.low??'—'));else if(k==='wind_speed')inner=esc('Wind '+Math.round(wx.wind)+' mph');else{const d=new Date();d.setDate(d.getDate()+(Number(q.day_offset??q.weather_day_offset)||0));inner=esc(k==='day_name'?d.toLocaleDateString(undefined,{weekday:'long'}):d.toLocaleDateString(undefined,{weekday:'long',month:'short',day:'numeric'}));}}else inner=esc(q.label||e.type||'');return `<div style="${st}">${inner}</div>`;}
function templateHtml(w){const p=props(w),es=Array.isArray(p.template_elements)?p.template_elements:[];if(!es.length)return '';const g=groupMetrics(es);let dw=Number(p.template_width)||760,dh=Number(p.template_height)||427.5;es.forEach(e=>{dw=Math.max(dw,(Number(e.x)||0)+(Number(e.width)||120));dh=Math.max(dh,(Number(e.y)||0)+(Number(e.height)||80));});return `<div style="position:relative;width:100%;height:100%;overflow:hidden;">${es.slice().sort((a,b)=>(Number(a.z)||0)-(Number(b.z)||0)).map(e=>Array.from({length:repeatCount(p,e)},(_,i)=>templateEl(e,p,i,g,dw,dh)).join('')).join('')}</div>`;}
async function loadWeather(p){const lat=Number(p.lat||0),lon=Number(p.lon||0);if(!lat||!lon)return;try{const r=await fetch(`/api/weather/forecast?lat=${encodeURIComponent(lat)}&lon=${encodeURIComponent(lon)}`,{cache:'no-store'});if(r.ok)p._weather_data=await r.json();}catch(_e){}}
function countdownFontSize(p){return Math.max(8,Math.min(160,Number(p.font_size||p.fontSize||48)||48))*2;}
function countdownText(node,now){const get=(name)=>node.getAttribute?node.getAttribute(name):(node.dataset||{})[name.slice(5).split('-').map((part,i)=>i?part[0].toUpperCase()+part.slice(1):part).join('')]||'',mode=get('data-countdown-mode')||'date',unit=get('data-countdown-duration-unit')||'minutes',value=Math.max(1,Number(get('data-countdown-duration-value'))||1),started=Number(get('data-countdown-started-at'))||now,target=mode==='duration'?started+value*({seconds:1000,minutes:60000,hours:3600000,days:86400000}[unit]||60000):new Date(get('data-countdown-target-date')||'').getTime(),diff=Math.max(0,target-now),expiry=get('data-countdown-expiry-text')||'Time\u2019s up!';if(!Number.isFinite(target)||target<=0)return get('data-countdown-invalid-text')||'Choose a date and time';if(diff<=0)return expiry;const d=Math.floor(diff/86400000),h=Math.floor(diff%86400000/3600000),m=Math.floor(diff%3600000/60000),s=Math.floor(diff%60000/1000);return d?d+'d '+h+'h':h?h+'h '+m+'m':m?m+'m '+s+'s':s+'s';}
function tickCountdowns(){const now=Date.now();document.querySelectorAll('[data-countdown-widget]').forEach(node=>{const value=node.querySelector('.countdown-value');if(value)value.textContent=countdownText(node,now);});}
function cachedCameraFilterName(value){return ['none','grayscale','sepia','halloween'].includes(value)?value:'none';}
function cachedCameraFilterCss(value){return ({none:'none',grayscale:'grayscale(1)',sepia:'sepia(1)',halloween:'saturate(1.35) contrast(1.15) hue-rotate(300deg)'})[cachedCameraFilterName(value)];}
function cachedCameraFilterOverlay(value,id){return cachedCameraFilterName(value)==='halloween'?`<span data-camera-filter-overlay="halloween" data-camera-filter-overlay-id="${esc(id||'')}" aria-hidden="true" style="position:absolute;inset:0;pointer-events:none;background:linear-gradient(135deg,rgba(249,115,22,.34),rgba(88,28,135,.42));mix-blend-mode:screen;opacity:.8;"></span>`:'';}
function initCachedCameraWidgets(widgets){const nodes=document.querySelectorAll('[data-camera-widget-id]');nodes.forEach(node=>{const id=node.getAttribute('data-camera-widget-id'),widget=(widgets||[]).find(item=>String(item.id)===String(id)),p=widget&&props(widget);if(!p||p.capture_enabled!==true||!navigator.mediaDevices||typeof navigator.mediaDevices.getUserMedia!=='function'||node.tagName!=='VIDEO')return;const facing=['user','environment','left','right'].includes(p.facing_mode)?p.facing_mode:'environment';const constraints={video:{facingMode:{ideal:facing},width:{ideal:1280,max:1280},height:{ideal:720,max:720},frameRate:{ideal:30,max:30}},audio:false};node.muted=true;node.autoplay=true;node.playsInline=true;navigator.mediaDevices.getUserMedia(constraints).then(stream=>{node.srcObject=stream;const result=typeof node.play==='function'?node.play():null;if(result&&typeof result.catch==='function')result.catch(()=>{});}).catch(()=>{});});}
function initCachedVideoWidgets(widgets){const nodes=document.querySelectorAll('[data-video-widget-id]');nodes.forEach(node=>{const id=node.getAttribute('data-video-widget-id'),widget=(widgets||[]).find(item=>String(item.id)===String(id)),p=widget&&props(widget),list=p&&Array.isArray(p.playlist)?p.playlist:[];if(!list.length)return;let index=0;const advance=()=>{index=(index+1)%list.length;node.src=String(list[index].url||'');if(typeof node.load==='function')node.load();const result=typeof node.play==='function'?node.play():null;if(result&&typeof result.catch==='function')result.catch(()=>{});};node.src=String(list[0].url||'');node.loop=list.length===1&&p.loop!==false;node.onended=list.length>1?advance:null;node.onerror=list.length>1?advance:null;});}
async function renderWidget(w){const p=props(w),t=normWidgetType(w);if(t==='weather')await loadWeather(p);let inner='';
 if(t==='weather'&&Array.isArray(p.template_elements)&&p.template_elements.length) inner=templateHtml(w);
 else if(t==='text') inner=`<div style="width:100%;height:100%;display:flex;align-items:${p.valign||'center'};justify-content:${p.align==='left'?'flex-start':p.align==='right'?'flex-end':'center'};padding:1rem;color:${p.color||'#fff'};font-size:${Number(p.font_size||p.fontSize||24)*2}px;text-align:${p.align||'center'};">${esc(p.text||'Text')}</div>`;
 else if(t==='countdown') { const target=p.countdown_mode==='duration' ? Date.now() + Math.max(1,Number(p.duration_value||1))*({seconds:1000,minutes:60000,hours:3600000,days:86400000}[p.duration_unit||'minutes']||60000) : new Date(p.targetDate||'').getTime(); const diff=Math.max(0,target-Date.now()); const d=Math.floor(diff/86400000),h=Math.floor(diff%86400000/3600000),m=Math.floor(diff%3600000/60000),s=Math.floor(diff%60000/1000); const text=Number.isFinite(target)&&target>0?(d?d+'d '+h+'h':h?h+'h '+m+'m':m?m+'m '+s+'s':s+'s'):(p.expiry_text||'Choose a date and time'); inner=`<div style="width:100%;height:100%;display:flex;align-items:center;justify-content:center;padding:1rem;color:${p.color||'#fff'};font-size:${countdownFontSize(p)}px;font-weight:800;line-height:1;text-align:center;overflow:hidden;white-space:nowrap;">${esc(text)}</div>`; }
 else if(t==='video') { const list=Array.isArray(p.playlist)&&p.playlist.length?p.playlist:(p.url?[{url:p.url}]:[]),u=String((list[0]||{}).url||''); const fit=['cover','contain','fill','none','scale-down'].includes(p.fit)?p.fit:'cover'; inner=`<video data-video-widget-id="${esc(w.id||'')}" src="${esc(u)}" autoplay playsinline preload="auto" ${p.loop !== false && list.length === 1 ? 'loop' : ''} ${p.muted||p.mute?'muted':''} style="width:100%;height:100%;object-fit:${fit};background:#000;"></video>`;}
 else if(t==='image') inner=`<img src="${esc(p.url||p.src||'')}" style="width:100%;height:100%;object-fit:${p.fit||'cover'};">`;
 else if(t==='camera') { const cameraFilter=cachedCameraFilterName(p.filter); const facing=['user','environment','left','right'].includes(p.facing_mode)?p.facing_mode:'environment'; if(p.capture_enabled===true) inner=`<div style="position:relative;width:100%;height:100%;background:#020617;color:#cbd5e1;"><video data-camera-widget-id="${esc(w.id||'')}" data-camera-filter="${cameraFilter}" autoplay playsinline muted style="width:100%;height:100%;object-fit:cover;background:#020617;filter:${cachedCameraFilterCss(cameraFilter)};"></video>${cachedCameraFilterOverlay(cameraFilter,w.id)}<span style="position:absolute;left:.5rem;bottom:.5rem;padding:.2rem .45rem;border-radius:.25rem;background:rgba(2,6,23,.75);font:600 12px system-ui;">Camera · ${esc(facing)}</span></div>`; else inner=`<div data-camera-widget-id="${esc(w.id||'')}" style="width:100%;height:100%;display:grid;place-items:center;border:1px dashed #334155;background:#020617;color:#94a3b8;font:600 14px system-ui;text-align:center;">📷 Camera preview<br><small style="font-weight:400;">capture disabled</small></div>`;}
 else if(t==='video') { const list=Array.isArray(p.playlist)&&p.playlist.length?p.playlist:(p.url?[{url:p.url}]:[]), u=String((list[0]||{}).url||''); if(isYouTubeUrl(u)){ inner=`<iframe src="${esc(youtubeEmbedUrl(u,Object.assign({},p,{muted:true})))}" allow="autoplay; encrypted-media; picture-in-picture" allowfullscreen style="width:100%;height:100%;border:0;background:#000;"></iframe>`; } else { const fit=['cover','contain','fill','none','scale-down'].includes(p.fit)?p.fit:'cover'; inner=`<video src="${esc(u)}" autoplay playsinline preload="auto" ${p.loop !== false && list.length === 1 ? 'loop' : ''} ${p.muted||p.mute?'muted':''} style="width:100%;height:100%;object-fit:${fit};background:#000;"></video>`; } }
 else if(t==='clock') inner=`<div class="clock" data-clock style="color:${p.color||'#fff'};font-size:${Number(p.font_size||p.fontSize||48)*2}px"></div>`;
 else if(t==='datetime'||t==='date') inner=`<div class="date" data-date style="color:${p.color||'#fff'};font-size:${Number(p.font_size||p.fontSize||36)*2}px"></div>`;
 else if(t==='rectangle') inner=`<div style="width:100%;height:100%;background:${p.color||'#e94560'};border-radius:${p.radius||0}px;"></div>`;
 else if(t==='webpage') inner=`<iframe src="${esc(p.url||'about:blank')}" style="width:100%;height:100%;border:0;"></iframe>`;
 else if(t==='weather') inner=`<div style="width:100%;height:100%;display:grid;place-items:center;color:${p.color||'#fff'};font-size:4rem;">${esc(p.location||'Weather')}</div>`;
 else if(t==='calendar') inner=`<div style="width:100%;height:100%;display:grid;place-items:center;color:${p.color||'#fff'};font-size:4rem;">${esc(p.label||'Calendar')}</div>`;
 else inner=`<div style="width:100%;height:100%;display:grid;place-items:center;border:1px dashed #334155;color:#94a3b8;">${esc(t||'widget')}</div>`;
 if(t==='countdown'){const mode=p.countdown_mode==='duration'?'duration':'date',started=Date.now(),durationValue=Math.max(1,Number(p.duration_value||1)),durationUnit=p.duration_unit||'minutes',target=p.targetDate||'',fake={getAttribute:(name)=>({'data-countdown-mode':mode,'data-countdown-duration-value':durationValue,'data-countdown-duration-unit':durationUnit,'data-countdown-started-at':started,'data-countdown-target-date':target,'data-countdown-expiry-text':p.expiry_text||'Time\u2019s up!','data-countdown-invalid-text':'Choose a date and time'}[name]||'')},text=countdownText(fake,started);inner=`<div data-countdown-widget data-countdown-mode="${esc(mode)}" data-countdown-duration-value="${durationValue}" data-countdown-duration-unit="${esc(durationUnit)}" data-countdown-started-at="${started}" data-countdown-target-date="${esc(target)}" data-countdown-expiry-text="${esc(p.expiry_text||'Time\u2019s up!')}" style="width:100%;height:100%;display:flex;align-items:center;justify-content:center;"><div class="countdown-value">${esc(text)}</div></div>`;}
 return `<div class="widget" style="${styleFor(w)}">${inner}</div>`;}
function tickClocks(){const now=new Date();document.querySelectorAll('[data-clock]').forEach(e=>e.textContent=now.toLocaleTimeString([], {hour:'2-digit', minute:'2-digit'}));document.querySelectorAll('[data-date]').forEach(e=>e.textContent=now.toLocaleDateString());tickCountdowns();}
function applyOrientation(value){const v=String(value||'normal');document.body.dataset.orientation=['normal','flipped','rotated_left','rotated_right'].includes(v)?v:'normal';}
async function loadLayout(){try{const r=await fetch('/assigned-layout.json?ts='+Date.now(),{cache:'no-store'});if(!r.ok)throw new Error('no assigned layout');const txt=await r.text();try{const c=await fetch('/api/client-config.json?ts='+Date.now(),{cache:'no-store'});if(c.ok)applyOrientation((await c.json()).orientation);}catch(_e){}if(txt===currentHash)return;currentHash=txt;const layout=JSON.parse(txt);const ws=(layout.widgets||[]).slice().sort((a,b)=>(a.z||0)-(b.z||0));const rendered=await Promise.all(ws.map(renderWidget));document.getElementById('player').innerHTML=rendered.join('')||'<div class="fallback"><div><strong>Empty Layout</strong></div></div>';tickClocks();initCachedVideoWidgets(ws);initCachedCameraWidgets(ws);}catch(e){if(!currentHash)document.getElementById('player').innerHTML='<div class="fallback"><div><strong>Waiting for cached assigned layout</strong><span>'+esc(e.message)+'</span></div></div>';}}
setInterval(tickClocks,1000);setInterval(loadLayout,5000);setInterval(()=>{currentHash='';loadLayout().catch(()=>{});},15*60*1000);loadLayout();

// === Cursor auto-hide (3.5s default, configurable via /api/cursor-config.json) ===
let __cursorIdle = Date.now();
const __cursorShow = () => { __cursorIdle = Date.now(); document.documentElement.style.cursor = 'default'; };
['pointermove','pointerdown','pointerup','mousemove','mousedown','mouseup','touchstart','touchmove','wheel','keydown','scroll'].forEach(ev =>
  window.addEventListener(ev, __cursorShow, { capture: true, passive: true }));
let __cursorHideSecs = 3.5;
async function __cursorLoadConfig() {
  try {
    const r = await fetch('/api/cursor-config.json', { cache: 'no-store' });
    if (r.ok) { const c = await r.json(); __cursorHideSecs = Number(c.hide_secs) || 3.5; }
  } catch (e) {}
}
setInterval(() => {
  if (Date.now() - __cursorIdle > __cursorHideSecs * 1000) {
    document.documentElement.style.cursor = 'none';
  }
}, 200);
__cursorLoadConfig();
</script></body></html>'''


# Live-server redirect: when the configured Screentra Cast server is reachable, the
# kiosk browser should hit the server's /player.html so it gets the real
# widget renderers (weather/calendar/clock/iframe/...). When the server is
# unreachable, fall back to the cached PLAYER_HTML so the player still shows
# the last cached layout offline.
LIVE_PROBE_TIMEOUT_SECS = float(os.environ.get('RIPLEY_LIVE_PROBE_TIMEOUT', '0.6'))


def _live_player_url() -> str | None:
    """Return the server URL the kiosk should redirect to, or None if offline."""
    server = os.environ.get('RIPLEY_SERVER_URL', '').strip()
    if not server:
        try:
            server_path = Path.home() / '.local/share/screentra-client/server'
            if server_path.exists():
                server = server_path.read_text().strip()
        except Exception:
            pass
    server = server.rstrip('/')
    if not server or not (server.startswith('http://') or server.startswith('https://')):
        return None
    return server


def _client_id_from_env() -> str | None:
    cid = os.environ.get('RIPLEY_CLIENT_ID', '').strip()
    return cid or None


def _client_id_from_cache() -> str | None:
    try:
        cid_path = Path.home() / '.local/share/screentra-client/client-id'
        if cid_path.exists():
            v = cid_path.read_text().strip()
            return v or None
    except Exception:
        pass
    return None


def _connector_for_display_index(display_index: int) -> str | None:
    """Return the connector name (e.g. 'DP-2') for a given display_index.

    Sources (tried in order):
      1. The display-governance mapping in config.json.
      2. /sys/class/drm (live kernel enumeration, fallback if config.json
         is missing or stale).
    Returns None when no physical connector matches.
    """
    try:
        gov_path = Path.home() / '.local/share/screentra-client/config.json'
        if gov_path.exists():
            cfg = _read_json_file(gov_path, {})
            for d in (cfg.get('last_detected_displays') or []):
                if int(d.get('display_index', -1)) == int(display_index):
                    con = d.get('connector')
                    if con and con != 'virtual-0':
                        return con
    except Exception:
        pass
    try:
        sysfs_root = Path('/sys/class/drm')
        if sysfs_root.is_dir():
            cards = []
            for d in sorted(sysfs_root.iterdir()):
                if d.name.startswith('card') and '-' in d.name:
                    status_path = d / 'status'
                    if not status_path.exists():
                        continue
                    if status_path.read_text().strip() != 'connected':
                        continue
                    mode_path = d / 'modes'
                    if mode_path.exists():
                        cards.append(d.name)
            cards.sort()
            if 0 <= display_index < len(cards):
                # card0-DP-2 → 'DP-2'
                name = cards[display_index]
                if '-' in name and name.split('-', 1)[1].startswith('card') is False:
                    return name.split('-', 1)[1]
                return name
    except Exception:
        return None
    return None


def _server_reachable(server_url: str, timeout: float) -> bool:
    """Probe the server with a short HTTPS GET on /api/version. Cheap and fast."""
    import ssl as _ssl
    import socket as _socket
    import urllib.request as _ur
    from urllib.parse import urlparse as _urlparse
    try:
        u = _urlparse(server_url)
        host = u.hostname
        port = u.port or (443 if u.scheme == 'https' else 80)
        path = u.path.rstrip('/') + '/api/version'
        # Cheap TCP reachability check first
        try:
            with _socket.create_connection((host, port), timeout=timeout):
                pass
        except Exception:
            return False
        ctx = None
        if u.scheme == 'https':
            ca_candidates = []
            env_ca = os.environ.get('RIPLEY_CA_CERT')
            if env_ca:
                ca_candidates.append(Path(env_ca))
            ca_candidates.append(Path.home() / '.local/share/screentra-client/screentra-cast-signage-ca.crt')
            for cafile in ca_candidates:
                if cafile.exists():
                    try:
                        ctx = _ssl.create_default_context(cafile=str(cafile))
                        break
                    except Exception:
                        ctx = None
            # No CA available: fail closed and keep the cached fallback page.
            if ctx is None:
                return False
        req = _ur.Request((u.scheme + '://' + (host if not u.port else f'{host}:{port}') + path), method='GET')
        with _ur.urlopen(req, timeout=timeout, context=ctx) as r:
            return 200 <= r.status < 500
    except Exception:
        return False


def _live_player_redirect_html(server_url: str, display_index: int, connector: str | None = None, client_id: str | None = None) -> str:
    """Return a redirect to the live server for a physical connector."""
    if not connector or connector == 'virtual-0':
        raise ValueError('physical display connector is required')
    safe_server = server_url.replace('"', '%22')
    target = f'{safe_server.rstrip("/")}/player.html?display={urllib.parse.quote(connector)}'
    if client_id:
        target += f'&client_id={urllib.parse.quote(client_id, safe="")}'
    return (
        '<!doctype html><html><head>'
        f'<meta http-equiv="refresh" content="0; url={target}">'
        f'<title>Screentra Cast Live Player</title>'
        '<style>html,body{margin:0;height:100%;background:#000;color:#94a3b8;font-family:system-ui,sans-serif;}'
        '.wrap{display:grid;place-items:center;height:100%}</style>'
        '</head><body><div class="wrap">'
        f'<div>Connecting to live player…</div>'
        f'<script>window.location.replace({json.dumps(target)});</script>'
        '</div></body></html>'
    )


class Handler(BaseHTTPRequestHandler):
    cache_dir = Path.home() / '.local/share/screentra-client/cache'

    def do_POST(self):
        path = unquote(self.path.split('?', 1)[0])
        try:
            body = _request_body(self)
        except OverflowError:
            return self.send_text(413, json.dumps({'error': 'request body too large'}), 'application/json')
        except ValueError as exc:
            return self.send_text(400, json.dumps({'error': str(exc)}), 'application/json')
        if _mutation_requires_auth(path) and not _session_user(self.headers.get('Cookie', '')):
            return self.send_text(401, json.dumps({'error': 'auth required', **_auth_source()}), 'application/json')
        if path == '/api/admin/auth/login':
            try:
                payload = json.loads(body) if body and body.strip() else {}
                username = str(payload.get('username') or '')
                password = str(payload.get('password') or '')
                ok, info = _check_login(username, password)
                if not ok:
                    return self.send_text(401, json.dumps({'ok': False, **info}), 'application/json')
                token = _make_session(info.get('username') or username, info.get('source') or 'fallback_default')
                data = json.dumps({'ok': True, **info}).encode()
                self.send_response(200)
                self.send_header('Content-Type', 'application/json')
                self.send_header('Content-Length', str(len(data)))
                self.send_header('Cache-Control', 'no-store')
                self.send_header('Set-Cookie', 'ripley_admin_session=' + token + '; Path=/; HttpOnly; SameSite=Lax; Max-Age=43200')
                self.end_headers(); self.wfile.write(data); return
            except Exception as e:
                return self.send_text(400, json.dumps({'ok': False, 'error': str(e)}), 'application/json')
        if path == '/api/admin/auth/logout':
            data = json.dumps({'ok': True}).encode()
            self.send_response(200)
            self.send_header('Content-Type', 'application/json')
            self.send_header('Content-Length', str(len(data)))
            self.send_header('Cache-Control', 'no-store')
            self.send_header('Set-Cookie', 'ripley_admin_session=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0')
            self.end_headers(); self.wfile.write(data); return
        if _admin_requires_auth(path) and not _session_user(self.headers.get('Cookie', '')):
            return self.send_text(401, json.dumps({'error': 'auth required', **_auth_source()}), 'application/json')
        if path == '/api/touchscreen/select':
            try:
                payload = json.loads(body)
                selected = payload.get('path')
                Handler.cache_dir.mkdir(parents=True, exist_ok=True)
                _atomic_write_text(Handler.cache_dir / 'touch-device.json', json.dumps({'path': selected}))
                # Tell the actor to restart the kiosk so the choice takes effect
                try:
                    pending = Handler.cache_dir.parent / 'pending-command.json'
                    _atomic_write_text(pending, json.dumps({
                        'action': 'restart_kiosk_display',
                        'args': {'display_index': int(os.environ.get('RIPLEY_DISPLAY_INDEX', '0')), 'reason': 'touch-device-change'},
                        'queued_at': datetime.utcnow().isoformat() + 'Z',
                    }))
                except Exception:
                    pass
                return self.send_text(200, json.dumps({'saved': True, 'path': selected}), 'application/json')
            except Exception as e:
                return self.send_text(500, json.dumps({'error': str(e)}), 'application/json')
        if path == '/api/touch-calibration.json':
            try:
                data = json.loads(body) if body and body.strip() else {}
                data.setdefault('received_at', _iso_now())
                # Tag per-display calibration results so the heartbeat
                # can attribute intermittent failures to a specific
                # screen when there are multiple on the same box.
                data.setdefault('display_index', int(os.environ.get('RIPLEY_DISPLAY_INDEX', '0')))
                calib_path = Handler.cache_dir / 'touch-calibration.json'
                calib_path.parent.mkdir(parents=True, exist_ok=True)
                history = []
                if calib_path.exists():
                    try:
                        history = _read_json_file(calib_path, [])
                        if not isinstance(history, list):
                            history = [history]
                    except Exception:
                        history = []
                history.append(data)
                history = history[-5:]   # rolling window of last 5 runs
                _atomic_write_text(calib_path, json.dumps(history, indent=2))
                return self.send_text(200, json.dumps({'ok': True, 'history_size': len(history)}), 'application/json')
            except Exception as e:
                return self.send_text(400, json.dumps({'error': str(e)}), 'application/json')
        if path == '/api/admin/ca/refresh':
            return self.send_text(200, json.dumps(_refresh_ca_from_server()), 'application/json')
        if path == '/api/admin/device/reboot':
            try:
                payload = json.loads(body) if body and body.strip() else {}
            except Exception:
                payload = {}
            result = _reboot_device(bool(payload.get('confirm')))
            return self.send_text(200 if result.get('ok') else 400, json.dumps(result), 'application/json')
        if path == '/api/admin/display-count':
            try:
                payload = json.loads(body) if body and body.strip() else {}
                value = payload.get('max_displays')
                result = _write_display_count(None if value in (None, '', 'auto') else int(value))
                return self.send_text(200, json.dumps(result), 'application/json')
            except Exception as e:
                return self.send_text(400, json.dumps({'ok': False, 'error': str(e)}), 'application/json')
        if path == '/api/admin/touch-binding':
            try:
                payload = json.loads(body) if body and body.strip() else {}
                result = _write_touch_binding(payload)
                return self.send_text(200, json.dumps(result), 'application/json')
            except Exception as e:
                return self.send_text(400, json.dumps({'ok': False, 'error': str(e)}), 'application/json')
        if path == '/api/admin/client-backup':
            result = _create_client_software_backup()
            return self.send_text(200 if result.get('ok') else 500, json.dumps(result), 'application/json')
        if path == '/api/admin/services/restart':
            r = _safe_run(['systemctl', '--user', 'restart', 'screentra-client-heartbeat.service', 'screentra-client-actor.service'], timeout=8)
            return self.send_text(200, json.dumps({'ok': r.get('ok'), 'restart': r}), 'application/json')
        # GET /api/client-config.json — current cached config (heartbeat's view of server state)
        if path == '/api/client-config.json':
            cfg_path = Handler.cache_dir / 'client-config.json'
            try:
                data = _read_json_file(cfg_path, {})
                return self.send_text(200, json.dumps(data), 'application/json')
            except Exception:
                return self.send_text(404, json.dumps({'error': 'no config yet'}), 'application/json')
        # POST /api/client-config.json — save theme (or other fields) to local config.
        # Heartbeat reads this on next cycle and sends it to the server.
        if method == 'POST' and path == '/api/client-config.json':
            try:
                payload = json.loads(body) if body and body.strip() else {}
            except Exception:
                return self.send_text(400, json.dumps({'ok': False, 'error': 'invalid json'}), 'application/json')
            cfg_path = Handler.cache_dir / 'client-config.json'
            try:
                try:
                    data = _read_json_file(cfg_path, {})
                except Exception:
                    data = {}
                for k in ['theme']:
                    if k in payload:
                        data[k] = payload[k]
                cfg_path.parent.mkdir(parents=True, exist_ok=True)
                _atomic_write_text(cfg_path, json.dumps(data, indent=2))
                return self.send_text(200, json.dumps({'ok': True}), 'application/json')
            except Exception as e:
                return self.send_text(500, json.dumps({'ok': False, 'error': str(e)}), 'application/json')
        return self.send_text(404, json.dumps({'error': 'not found'}), 'application/json')
    def log_message(self, fmt, *args):
        pass
    def send_text(self, status, text, ctype='text/plain'):
        data = text.encode()
        self.send_response(status)
        self.send_header('Content-Type', ctype)
        self.send_header('Content-Length', str(len(data)))
        self.send_header('Cache-Control', 'no-store')
        self.end_headers()
        self.wfile.write(data)
    def do_GET(self):
        path = unquote(self.path.split('?',1)[0])
        if path in ('/login', '/login.html'):
            return self.send_text(200, _admin_login_html(), 'text/html; charset=utf-8')
        if path == '/api/admin/auth/status':
            return self.send_text(200, json.dumps(_admin_auth_payload(self)), 'application/json')
        if _admin_requires_auth(path) and not _session_user(self.headers.get('Cookie', '')):
            if path.startswith('/api/'):
                return self.send_text(401, json.dumps({'error': 'auth required', **_auth_source()}), 'application/json')
            return self.send_text(200, _admin_login_html(), 'text/html; charset=utf-8')
        if os.environ.get('RIPLEY_ADMIN_MODE', '').lower() in ('1', 'true', 'yes') and path in ('/', '/admin.html'):
            return self.send_text(200, ADMIN_HTML, 'text/html; charset=utf-8')
        # GET /api/client-config.json — current cached heartbeat config (layout + theme)
        if path == '/api/client-config.json':
            cfg_path = Handler.cache_dir / 'client-config.json'
            try:
                data = _read_json_file(cfg_path, {})
                return self.send_text(200, json.dumps(data), 'application/json')
            except Exception:
                return self.send_text(404, json.dumps({'error': 'no config yet'}), 'application/json')
        if path in ('/', '/player.html'):
            display_index = int(os.environ.get('RIPLEY_DISPLAY_INDEX', '0'))
            layout_target = Handler.cache_dir / f'assigned-layout-display-{display_index}.json'
            # Legacy fallback belongs to display 0 only. Other displays must not render
            # display 0's cached assignment when their own display cache is absent.
            if display_index == 0 and not layout_target.exists():
                layout_target = Handler.cache_dir / 'assigned-layout.json'

            # No assigned layout on disk for THIS display -> show the rich
            # no-layout page (touch detection, gear modal, build info, exit
            # button) instead of redirecting to the server's minimal fallback
            # card. This gives the operator everything they need to configure
            # the touchscreen + pick a layout right from the screen.
            if not layout_target.exists():
                try:
                    with open(os.path.join(os.path.dirname(os.path.abspath(__file__)), 'no-layout.html'), 'r') as _f:
                        html = _f.read().replace('<title>Screentra Cast Player display</title>', f'<title>Screentra Cast Player display {display_index}</title>')
                        return self.send_text(200, html, 'text/html; charset=utf-8')
                except Exception:
                    pass

            # When a layout is assigned AND the configured server is reachable,
            # redirect the kiosk to live /player.html so weather/calendar/clock/
            # image widgets render with full data via the server-side iframes.
            # Server's player.html looks up the display by *connector*
            # (DP-2/DP-4/...) rather than by index, so we read the connector
            # from the heartbeat's cached client-config.json when available;
            # fall back to the index.
            live_url = _live_player_url()
            if live_url and _server_reachable(live_url, LIVE_PROBE_TIMEOUT_SECS):
                connector = _connector_for_display_index(display_index)
                if connector:
                    return self.send_text(200, _live_player_redirect_html(live_url, display_index, connector, _client_id_from_env() or _client_id_from_cache()), 'text/html; charset=utf-8')

            # No live server reachable (offline mode). Render the offline
            # cached layout from the renderer script's PLAYER_HTML constant.
            display_index = int(os.environ.get('RIPLEY_DISPLAY_INDEX', '0'))
            html = PLAYER_HTML.replace('<title>Screentra Cast Cached Player</title>', f'<title>Screentra Cast Display {display_index}</title>')
            return self.send_text(200, html, 'text/html; charset=utf-8')
        if path == '/admin.html':
            return self.send_text(200, ADMIN_HTML, 'text/html; charset=utf-8')
        if path == '/api/admin/status':
            return self.send_text(200, json.dumps(_admin_status()), 'application/json')
        if path == '/api/admin/certificates':
            return self.send_text(200, json.dumps(_cert_info()), 'application/json')
        if path == '/api/admin/client-backup/download':
            query = urllib.parse.parse_qs(urllib.parse.urlparse(self.path).query)
            fname = (query.get('file') or [''])[0]
            safe_name = Path(fname).name
            target = (_client_backup_root() / safe_name).resolve()
            try:
                target.relative_to(_client_backup_root().resolve())
            except Exception:
                return self.send_text(403, 'forbidden')
            if not target.exists() or not target.is_file():
                return self.send_text(404, 'backup not found')
            data = target.read_bytes()
            self.send_response(200)
            self.send_header('Content-Type', 'application/gzip')
            self.send_header('Content-Length', str(len(data)))
            self.send_header('Content-Disposition', 'attachment; filename="' + safe_name.replace('"', '') + '"')
            self.send_header('Cache-Control', 'no-store')
            self.end_headers(); self.wfile.write(data); return
        if path == '/api/admin/logs':
            query = urllib.parse.parse_qs(urllib.parse.urlparse(self.path).query)
            target = (query.get('target') or ['heartbeat'])[0]
            return self.send_text(200, json.dumps(_admin_logs(target)), 'application/json')
        if path == '/no-layout.html':
            display_index = int(os.environ.get('RIPLEY_DISPLAY_INDEX', '0'))
            with open(os.path.join(os.path.dirname(os.path.abspath(__file__)), 'no-layout.html'), 'r') as _f:
                html = _f.read().replace('<title>Screentra Cast Player display</title>', f'<title>Screentra Cast Player display {display_index}</title>')
                return self.send_text(200, html, 'text/html; charset=utf-8')
        if path == '/touch-calibration.html':
            # Serve the touchscreen calibration test page. Lives next to
            # the renderer script and reads RIPLEY_DISPLAY_INDEX for the
            # results so per-display calibration is recorded separately.
            try:
                with open(os.path.join(os.path.dirname(os.path.abspath(__file__)), 'touch-calibration.html'), 'r') as _f:
                    return self.send_text(200, _f.read(), 'text/html; charset=utf-8')
            except FileNotFoundError:
                return self.send_text(404, 'calibration page not installed', 'text/plain')
        if path == '/api/status.json':
            # Debug: track JS heartbeat from the kiosk page (UA has
            # 'Chrome' in it). External curls (this script's debugging)
            # don't reset the timer. If the page goes black but chromium
            # is still alive, JS may have stopped running (renderer
            # process stalled). A gap >15s since the last chrome hit is
            # a strong signal of that.
            import sys as _sys, time as _time
            _ua = self.headers.get('User-Agent', '')
            _is_chrome = 'Chrome' in _ua or 'Chromium' in _ua
            _now = _time.time()
            if _is_chrome:
                _last = getattr(Handler, '_last_chrome_status_hit', 0.0)
                _gap = _now - _last if _last else 0.0
                if _last and _gap > 15:
                    print(f'[{_time.strftime("%H:%M:%S")}] /api/status.json GAP={_gap:.1f}s from chrome  (JS may be stalled on page; check chromium renderer process)', file=_sys.stderr, flush=True)
                Handler._last_chrome_status_hit = _now
            else:
                # external probe — log but don't reset chrome timer
                print(f'[{_time.strftime("%H:%M:%S")}] /api/status.json external probe (UA={_ua[:40]!r})', file=_sys.stderr, flush=True)
            try:
                payload = _nl_status()
            except Exception as _e:
                import traceback as _tb; _tb.print_exc()
                payload = {'error': str(_e)}
            return self.send_text(200, json.dumps(payload), 'application/json')
        if path == '/api/touchscreen.json':
            try:
                devs = _nl_detect_touch_devices()
            except Exception as _e:
                import traceback as _tb; _tb.print_exc()
                devs = []
            sel = None
            _tp = Handler.cache_dir / 'touch-device.json'
            if _tp.exists():
                try:
                    sel = _read_json_file(_tp, {}).get('path')
                except Exception:
                    pass
            return self.send_text(200, json.dumps({'devices': devs, 'selected': sel}), 'application/json')
        if path == '/api/cursor-config.json':
            # Read hide_secs from RIPLEY_CURSOR_HIDE_SECS env (default 3.5).
            # The player installer sets this from the operator's kiosk-config,
            # which writes ~/.local/share/screentra-client/config.json.
            hide_secs = float(os.environ.get('RIPLEY_CURSOR_HIDE_SECS', '3.5'))
            return self.send_text(200, json.dumps({'hide_secs': hide_secs}), 'application/json')
        if path == '/api/touch-calibration.json':
            # GET: return last 5 calibration results (or 404 if none).
            calib_path = Handler.cache_dir / 'touch-calibration.json'
            if not calib_path.exists():
                return self.send_text(404, json.dumps({'error': 'no calibration yet'}), 'application/json')
            return self.send_text(200, json.dumps(_read_json_file(calib_path, [])), 'application/json')
        if path == '/health':
            return self.send_text(200, 'ok')
        if path == '/assigned-layout.json':
            display_index = int(os.environ.get('RIPLEY_DISPLAY_INDEX', '0'))
            target = self.cache_dir / f'assigned-layout-display-{display_index}.json'
            if display_index == 0 and not target.exists():
                target = self.cache_dir / 'assigned-layout.json'
            if not target.exists():
                return self.send_text(404, '{}', 'application/json')
            return self.send_text(200, json.dumps(_read_json_file(target, {})), 'application/json')
        if path == '/client-config.json':
            target = self.cache_dir / path.lstrip('/')
            if not target.exists():
                return self.send_text(404, '{}', 'application/json')
            return self.send_text(200, json.dumps(_read_json_file(target, {})), 'application/json')
        # Optional cached asset serving under cache/assets
        safe = Path(path.lstrip('/'))
        target = (self.cache_dir / safe).resolve()
        try:
            target.relative_to(self.cache_dir.resolve())
        except Exception:
            return self.send_text(403, 'forbidden')
        if target.exists() and target.is_file():
            data = target.read_bytes()
            self.send_response(200)
            self.send_header('Content-Type', mimetypes.guess_type(str(target))[0] or 'application/octet-stream')
            self.send_header('Content-Length', str(len(data)))
            self.end_headers(); self.wfile.write(data); return
        return self.send_text(404, 'not found')

def main():
    ap=argparse.ArgumentParser()
    ap.add_argument('--host', default=os.environ.get('RIPLEY_RENDER_HOST','127.0.0.1'))
    ap.add_argument('--port', type=int, default=None)
    ap.add_argument('--cache-dir', default=os.environ.get('RIPLEY_CLIENT_CACHE_DIR', str(Path.home()/'.local/share/screentra-client/cache')))
    args=ap.parse_args()
    if args.port is None:
        args.port = int(os.environ.get('RIPLEY_RENDER_PORT', str(int(os.environ.get('RIPLEY_RENDER_BASE_PORT','3900')) + int(os.environ.get('RIPLEY_DISPLAY_INDEX','0')))))
    Handler.cache_dir=Path(args.cache_dir)
    Handler.cache_dir.mkdir(parents=True, exist_ok=True)
    print(f'Screentra Cast cached player renderer listening on http://{args.host}:{args.port}/ using {Handler.cache_dir}', flush=True)
    ThreadingHTTPServer((args.host,args.port), Handler).serve_forever()
if __name__=='__main__': main()
