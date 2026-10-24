#!/usr/bin/env python3
"""Screentra Cast client/client heartbeat agent.

Posts system metrics to the Screentra Cast signage server so the Connected Clients panel
can show enrolled player uptime and usage.
"""
import argparse
import ctypes
import hashlib
import io
import json
import os
import posixpath
import re
import shutil
import selectors
import ssl
import socket
import stat
import subprocess
import sys
import tarfile
import tempfile
import time
import urllib.request
from urllib.parse import quote, urlparse
import importlib.util
import sys as _sys
_display_detect_path = os.path.join(os.path.dirname(os.path.abspath(__file__)), 'screentra-display-detect.py')
_spec = importlib.util.spec_from_file_location('screentra_display_detect', _display_detect_path)
_mod = importlib.util.module_from_spec(_spec)
_sys.modules['screentra_display_detect'] = _mod
_spec.loader.exec_module(_mod)
detect_displays = _mod.detect_displays
_status_report_path = os.path.join(os.path.dirname(os.path.abspath(__file__)), 'screentra-client-status-report-admission.py')
_status_report_spec = importlib.util.spec_from_file_location('screentra_client_status_report_admission', _status_report_path)
_status_report_mod = importlib.util.module_from_spec(_status_report_spec)
_sys.modules['screentra_client_status_report_admission'] = _status_report_mod
_status_report_spec.loader.exec_module(_status_report_mod)
validate_status_report = _status_report_mod.validate_status_report

from pathlib import Path

MAX_JSON_FILE_BYTES = 1 << 20
MAX_COMMAND_RESPONSE_BYTES = 64 * 1024
MAX_TEXT_RESPONSE_BYTES = 64 * 1024
MAX_PROC_METRICS_BYTES = 64 * 1024


def _read_bounded_local_text(path: str, maximum: int = MAX_PROC_METRICS_BYTES) -> str:
    with open(path, 'rb') as handle:
        return handle.read(maximum).decode('utf-8', errors='replace')


def _read_bounded_response(response, maximum):
    if isinstance(maximum, bool) or not isinstance(maximum, int) or maximum < 1:
        raise ValueError('response bound is invalid')
    data = response.read(maximum + 1)
    if not isinstance(data, (bytes, bytearray)) or len(data) > maximum:
        raise ValueError('server response exceeds its bound')
    return bytes(data).decode('utf-8')


def client_pairing_code(client_id):
    """Return the short physical-confirmation code for a client ID."""
    digest = hashlib.sha256(b'screentra-cast-pairing-v1\0' + str(client_id).encode('utf-8')).digest()
    return f'{int.from_bytes(digest[:4], "big") % 1_000_000:06d}'


def _read_json_file(path: Path, default):
    try:
        raw = _read_real_file(path)
        if len(raw) > MAX_JSON_FILE_BYTES:
            return default
        return _parse_server_json(raw.decode('utf-8'))
    except Exception:
        return default


def _atomic_write_text(path: Path, text: str, mode: int = 0o600) -> None:
    """Replace a cache JSON file atomically and keep it owner-readable."""
    _ensure_real_directory(path.parent, create=True)
    os.chmod(path.parent, 0o700)
    fd, temporary_name = tempfile.mkstemp(
        prefix=f'.{path.name}.', suffix='.tmp', dir=str(path.parent)
    )
    temporary = Path(temporary_name)
    try:
        with os.fdopen(fd, 'w', encoding='utf-8') as handle:
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


def _write_status_report(path: Path, status):
    """Persist only a report admitted by the metadata-only status boundary."""
    validate_status_report(status)
    _atomic_write_text(path, json.dumps(status, indent=2))


def _display_governance_count(which):
    """Read config.json and return the display-count governance value.

    `which` is 'installer' (always-defined, what the installer detected) or
    'override' (operator-set, may be None). Missing/corrupt config.json is
    treated as "0" / None — the heartbeat will publish zero and the server
    will leave the existing row's value alone (operator_set is preserved by
    the server-side UPSERT).
    """
    try:
        cfg_path = Path(os.environ.get('RIPLEY_CLIENT_DIR',
                   str(Path.home() / '.local/share/screentra-client'))) / 'config.json'
        if not cfg_path.exists():
            return None if which == 'override' else 0
        cfg = _read_json_file(cfg_path, {})
    except Exception:
        return None if which == 'override' else 0
    if which == 'installer':
        v = cfg.get('installer_detected_max_displays')
        return int(v) if v is not None else 0
    # override
    v = cfg.get('max_displays')
    return int(v) if v is not None else None


def _read_theme():
    """Read theme from localStorage-style client config JSON (set by admin UI)."""
    try:
        cfg_path = Path(os.environ.get('RIPLEY_CLIENT_CACHE_DIR',
                   str(Path.home() / '.local/share/screentra-client/cache'))) / 'client-config.json'
        if not cfg_path.exists():
            return None
        cfg = _read_json_file(cfg_path, {})
        theme = cfg.get('theme')
        return theme if theme in ('midnight', 'light', 'classic') else None
    except Exception:
        return None


def _touch_binding():
    try:
        cfg_path = Path(os.environ.get('RIPLEY_CLIENT_DIR',
                   str(Path.home() / '.local/share/screentra-client'))) / 'config.json'
        if not cfg_path.exists():
            return None
        cfg = _read_json_file(cfg_path, {})
        binding = cfg.get('touch_binding')
        return binding if isinstance(binding, dict) else None
    except Exception:
        return None


MAX_LOCAL_PROBE_OUTPUT_BYTES = 4096


def _bound_local_probe_output(value: str) -> str:
    encoded = str(value).encode('utf-8', errors='replace')
    return encoded[:MAX_LOCAL_PROBE_OUTPUT_BYTES].decode('utf-8', errors='ignore')


def _run_bounded_probe(args, maximum=MAX_LOCAL_PROBE_OUTPUT_BYTES, timeout=3):
    process = None
    try:
        process = subprocess.Popen(args, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
        selector = selectors.DefaultSelector()
        output = bytearray()
        selector.register(process.stdout, selectors.EVENT_READ)
        deadline = time.monotonic() + timeout
        try:
            while selector.get_map():
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise TimeoutError('probe timed out')
                for key, _ in selector.select(remaining):
                    chunk = os.read(key.fileobj.fileno(), 8192)
                    if not chunk:
                        selector.unregister(key.fileobj)
                        key.fileobj.close()
                        continue
                    if len(output) + len(chunk) > maximum:
                        raise ValueError('probe output exceeded its limit')
                    output.extend(chunk)
        finally:
            selector.close()
        return process.wait(timeout=max(0.1, deadline - time.monotonic())), bytes(output)
    except Exception:
        if process is not None and process.poll() is None:
            process.kill()
            process.wait(timeout=1)
        return None, b''
    finally:
        if process is not None and process.stdout is not None:
            process.stdout.close()


def run(cmd):
    commands = {
        "hostname -I | awk '{print $1}'": ['hostname', '-I'],
        'cat /etc/machine-id': ['cat', '/etc/machine-id'],
    }
    args = commands.get(cmd)
    if args is None:
        return ''
    try:
        returncode, raw = _run_bounded_probe(args)
        if returncode != 0:
            return ''
        output = _bound_local_probe_output(raw.decode('utf-8', errors='replace'))
        return output.strip().split()[0] if output.strip() else ''
    except Exception:
        return ''


def read_mem():
    values = {}
    try:
        for line in _read_bounded_local_text('/proc/meminfo').splitlines():
            k, v = line.split(':', 1)
            values[k] = int(v.strip().split()[0]) * 1024
    except Exception:
        pass
    total = values.get('MemTotal')
    available = values.get('MemAvailable')
    used = total - available if total and available else None
    return used, total


def read_disk(path='/'):
    try:
        st = os.statvfs(path)
        total = st.f_blocks * st.f_frsize
        avail = st.f_bavail * st.f_frsize
        return total - avail, total
    except Exception:
        return None, None


def read_uptime():
    try:
        return int(float(_read_bounded_local_text('/proc/uptime', 256).split()[0]))
    except Exception:
        return None


def read_cpu_usage(interval=0.25):
    def sample():
        fields = [int(x) for x in _read_bounded_local_text('/proc/stat').splitlines()[0].split()[1:]]
        idle = fields[3] + fields[4]
        total = sum(fields)
        return idle, total
    try:
        idle1, total1 = sample()
        time.sleep(interval)
        idle2, total2 = sample()
        total_delta = total2 - total1
        idle_delta = idle2 - idle1
        if total_delta <= 0:
            return None
        return round((1 - idle_delta / total_delta) * 100, 2)
    except Exception:
        return None


def local_ip():
    ip = run("hostname -I | awk '{print $1}'")
    if ip:
        return ip
    try:
        s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        s.connect(('8.8.8.8', 80))
        ip = s.getsockname()[0]
        s.close()
        return ip
    except Exception:
        return '127.0.0.1'


def payload(args):
    mem_used, mem_total = read_mem()
    disk_used, disk_total = read_disk('/')
    hostname = socket.gethostname()
    client_id = args.client_id or run('cat /etc/machine-id') or hostname
    result, result_path = read_command_result(args.cache_dir)
    last_command_result = None
    last_command_at = None
    if result:
        last_command_result = result
        last_command_at = result.get('finished_at') or result.get('started_at')
    displays = detect_displays(max_displays=getattr(args, 'max_displays', 4))
    active_layout_id = args.active_layout_id
    active_layout_revision = None
    try:
        cached_cfg = Path(args.cache_dir, 'client-config.json')
        if cached_cfg.exists():
            cached = _read_json_file(cached_cfg, {})
            active_layout_id = active_layout_id or cached.get('assigned_layout_id')
            value = cached.get('active_layout_revision')
            active_layout_revision = int(value) if value is not None else None
    except Exception:
        pass
    for display in displays:
        try:
            display_index = int(display.get('display_index', 0))
            display_cfg_path = Path(args.cache_dir, 'client-config.json' if display_index == 0 else f'display-config-{display_index}.json')
            display_cfg = _read_json_file(display_cfg_path, {})
            display['active_layout_id'] = display_cfg.get('assigned_layout_id') or active_layout_id
            value = display_cfg.get('active_layout_revision')
            display['active_layout_revision'] = int(value) if value is not None else active_layout_revision
        except Exception:
            display['active_layout_id'] = active_layout_id
            display['active_layout_revision'] = active_layout_revision
    return {
        'client_agent': True,
        'client_installed': True,
        'identity_source': 'persistent_client_id',
        'client_id': client_id,
        'pairing_code': client_pairing_code(client_id),
        'hostname': args.name or hostname,
        'address': local_ip(),
        'role': args.role,
        'version': args.version,
        'uptime_seconds': read_uptime(),
        'cpu_usage': read_cpu_usage(),
        'memory_used': mem_used,
        'memory_total': mem_total,
        'disk_used': disk_used,
        'disk_total': disk_total,
        'active_layout_id': active_layout_id,
        'active_layout_revision': active_layout_revision,
        'last_command_result': last_command_result,
        'last_command_at': last_command_at,
        'displays': displays,
        # Display count governance (Phase 1A, 2026-08-02). Read from
        # config.json which the installer writes after DRM detection, and the
        # admin /admin/display page can update. Heartbeat is the carrier;
        # the server stores both fields verbatim.
        'installer_detected_max_displays': _display_governance_count('installer'),
        'max_displays': _display_governance_count('override'),
        'touch_binding': _touch_binding(),
        'theme': _read_theme(),
    }




def tls_context():
    """Use the Screentra Cast CA bundle directly when present, independent of OS trust."""
    cafile = os.environ.get('RIPLEY_CA_CERT') or str(Path.home() / '.local/share/screentra-client/screentra-cast-signage-ca.crt')
    if not cafile or not Path(cafile).is_file():
        raise RuntimeError('Screentra Cast CA certificate is unavailable')
    try:
        return ssl.create_default_context(
            cadata=_read_real_file(Path(cafile), max_bytes=256 * 1024).decode('utf-8')
        )
    except Exception as e:
        raise RuntimeError('Screentra Cast CA certificate is invalid') from e


def validate_server_url(server):
    parsed = urlparse((server or '').strip())
    if parsed.scheme not in ('https', 'http') or not parsed.hostname:
        raise ValueError('server URL must include an https:// origin')
    if parsed.scheme == 'http' and parsed.hostname not in ('127.0.0.1', 'localhost', '::1'):
        raise ValueError('non-loopback server URLs must use HTTPS')
    return parsed

def urlopen_ripley(req, timeout=10):
    kw = {'timeout': timeout}
    parsed = validate_server_url(req.full_url)
    if parsed.scheme == 'https':
        ctx = tls_context()
        kw['context'] = ctx
    return urllib.request.urlopen(req, **kw)


def client_auth_token():
    """Read the per-client token without ever printing or logging it."""
    token = os.environ.get('RIPLEY_CLIENT_AUTH_TOKEN', '').strip()
    if token:
        return token
    path = os.environ.get(
        'RIPLEY_CLIENT_AUTH_TOKEN_FILE',
        str(Path.home() / '.local/share/screentra-client/client-auth-token'),
    )
    try:
        return _read_real_file(Path(path), max_bytes=4096).decode('utf-8').strip()
    except (OSError, UnicodeError, ValueError):
        return ''


def client_auth_headers(token=None):
    token = client_auth_token() if token is None else str(token).strip()
    return {'X-SCREENTRA-CLIENT-TOKEN': token} if token else {}


ADDON_API_VERSION = '1'
MAX_ADDON_PACKAGE_BYTES = 8 * 1024 * 1024
MAX_ADDON_EXTRACTED_BYTES = 8 * 1024 * 1024
MAX_ADDON_MEMBER_BYTES = 4 * 1024 * 1024
MAX_ADDON_MANIFEST_BYTES = 256 * 1024
MAX_ADDON_SIGNATURE_BYTES = 16 * 1024
ADDON_ID_RE = re.compile(r'^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$')
ADDON_VERSION_RE = re.compile(r'^[a-z0-9][a-z0-9._-]{0,63}$')
CLIENT_CAPABILITIES = frozenset({
    'render_only',
    'camera_capture',
    'microphone_capture',
    'local_inference',
    'client_service',
})
CLIENT_SERVICE_FIELDS = frozenset({
    'id',
    'image',
    'image_digest',
    'socket_name',
    'cpu_millis',
    'memory_bytes',
    'pids',
    'writable_bytes',
    'model_id',
    'model_sha256',
})
GENERIC_ADDON_ERROR = 'signed addon cache update failed'
GENERIC_INVENTORY_ERROR = 'addon inventory unavailable'


def _absolute_path(path):
    return Path(os.path.abspath(os.fspath(path)))


def _ensure_real_directory(path, create=False):
    """Validate or create a directory without following symlinked ancestors."""
    current = Path(_absolute_path(path).anchor)
    parts = _absolute_path(path).parts[1:]
    for part in parts:
        current /= part
        try:
            metadata = os.lstat(current)
        except FileNotFoundError:
            if not create:
                return False
            current.mkdir()
            metadata = os.lstat(current)
        if stat.S_ISLNK(metadata.st_mode) or not stat.S_ISDIR(metadata.st_mode):
            raise ValueError('addon cache directory is not a real directory')
    return True


def _path_exists(path):
    try:
        os.lstat(path)
        return True
    except FileNotFoundError:
        return False


def _reject_symlinked_ancestors(path):
    current = Path(_absolute_path(path).anchor)
    for part in _absolute_path(path).parts[1:]:
        current /= part
        try:
            metadata = os.lstat(current)
        except FileNotFoundError:
            break
        if stat.S_ISLNK(metadata.st_mode):
            raise ValueError('addon cache path contains a symlink')


def _require_real_file(path):
    """Reject symlinks and non-regular files before cache reads."""
    _reject_symlinked_ancestors(path)
    metadata = os.lstat(path)
    if stat.S_ISLNK(metadata.st_mode) or not stat.S_ISREG(metadata.st_mode):
        raise ValueError('addon cache file is not a regular file')
    return metadata


def _read_real_file(path, max_bytes=MAX_JSON_FILE_BYTES):
    _require_real_file(path)
    flags = os.O_RDONLY | getattr(os, 'O_NOFOLLOW', 0)
    descriptor = os.open(os.fspath(path), flags)
    try:
        metadata = os.fstat(descriptor)
        if not stat.S_ISREG(metadata.st_mode) or metadata.st_size > max_bytes:
            raise ValueError('addon cache file exceeds its bound')
        return os.read(descriptor, max_bytes + 1)
    finally:
        os.close(descriptor)


def _read_cache_json(path, default):
    if not _path_exists(path):
        return default
    try:
        return _parse_server_json(_read_real_file(path).decode('utf-8'))
    except (OSError, UnicodeError, ValueError, json.JSONDecodeError):
        raise ValueError('addon cache metadata is invalid')


def _validate_addon_version(version):
    if not isinstance(version, str) or not ADDON_VERSION_RE.fullmatch(version):
        raise ValueError('addon inventory contains an unsafe version')
    return version


def _validate_client_service_token(value, field):
    if not isinstance(value, str) or not 0 < len(value) <= 64 or not re.fullmatch(r'[a-z0-9._-]+', value):
        raise ValueError(f'invalid client service {field}')


def _validate_client_service_image(value):
    if (
        not isinstance(value, str)
        or not value
        or len(value) > 255
        or '..' in value
        or '://' in value
        or ':' in value
        or '@' in value
        or '//' in value
        or value.startswith('/')
        or value.endswith('/')
        or any(not (character.isascii() and (character.isalnum() or character in '._-/')) for character in value)
        or any(
            not component
            or component in ('.', '..')
            or not component[0].isascii()
            or not component[0].isalnum()
            or not component[-1].isascii()
            or not component[-1].isalnum()
            for component in value.split('/')
        )
    ):
        raise ValueError('invalid client service image reference')


def _validate_client_service_model_identity(service):
    model_id = service.get('model_id')
    model_sha256 = service.get('model_sha256')
    if (model_id is None) != (model_sha256 is None):
        raise ValueError('client service model_id and model_sha256 must be declared together')
    if model_id is not None:
        _validate_client_service_token(model_id, 'model id')
        if not isinstance(model_sha256, str) or not re.fullmatch(r'[0-9a-f]{64}', model_sha256):
            raise ValueError('client service model_sha256 must be a lowercase hexadecimal digest')


def _validate_client_extension_capabilities(manifest):
    capabilities = manifest.get('client_capabilities', [])
    if not isinstance(capabilities, list):
        raise ValueError('client_capabilities must be a list')
    seen = set()
    for capability in capabilities:
        if not isinstance(capability, str) or capability not in CLIENT_CAPABILITIES:
            raise ValueError('unsupported client capability')
        if capability in seen:
            raise ValueError('duplicate client capability')
        seen.add(capability)
    if 'local_inference' in seen and 'microphone_capture' not in seen:
        raise ValueError('local_inference requires microphone_capture')
    if 'local_inference' in seen and 'client_service' not in seen:
        raise ValueError('local_inference requires client_service')
    requests_service = 'client_service' in seen
    service = manifest.get('client_service')
    if requests_service != (service is not None):
        raise ValueError('client_service capability and metadata must be declared together')
    if service is None:
        return
    if not isinstance(service, dict) or set(service) - CLIENT_SERVICE_FIELDS:
        raise ValueError('client service metadata contains unsupported fields')
    required = {
        'id', 'image', 'image_digest', 'socket_name',
        'cpu_millis', 'memory_bytes', 'pids', 'writable_bytes',
    }
    if not required.issubset(service):
        raise ValueError('client service metadata is incomplete')
    _validate_client_service_token(service['id'], 'id')
    _validate_client_service_token(service['socket_name'], 'socket name')
    _validate_client_service_image(service['image'])
    if not isinstance(service['image_digest'], str) or not re.fullmatch(
            r'sha256:[0-9a-f]{64}', service['image_digest']):
        raise ValueError('invalid client service image digest')
    for field, minimum, maximum in (
        ('cpu_millis', 1, 4000),
        ('memory_bytes', 64 * 1024 * 1024, 2 * 1024 * 1024 * 1024),
        ('pids', 1, 256),
    ):
        value = service[field]
        if isinstance(value, bool) or not isinstance(value, int) or not minimum <= value <= maximum:
            raise ValueError(f'client service {field} is outside the bounded range')
    writable_bytes = service['writable_bytes']
    if isinstance(writable_bytes, bool) or not isinstance(writable_bytes, int) or not 0 <= writable_bytes <= 512 * 1024 * 1024:
        raise ValueError('client service writable_bytes is outside the bounded range')
    if 'local_inference' in seen and service.get('model_id') is None:
        raise ValueError('local_inference requires an explicit model identity and digest')
    _validate_client_service_model_identity(service)


def _validate_addon_manifest_metadata(manifest, expected):
    """Validate signed addon metadata against the server inventory contract."""
    if not isinstance(manifest, dict) or manifest.get('schema_version') != 1:
        raise ValueError('unsupported addon manifest schema')
    if manifest.get('kind') != 'client_extension' or manifest.get('client_api_version') != ADDON_API_VERSION:
        raise ValueError('unsupported client addon API version or kind')
    addon_id = manifest.get('id')
    if not isinstance(addon_id, str) or not ADDON_ID_RE.fullmatch(addon_id):
        raise ValueError('invalid addon id')
    if addon_id != expected.get('id') or manifest.get('version') != expected.get('version'):
        raise ValueError('addon package does not match the server inventory')
    if manifest.get('server_entrypoint') is not None:
        raise ValueError('client addon contains a server entrypoint')
    entrypoint = manifest.get('client_entrypoint')
    digest = manifest.get('client_sha256')
    if not isinstance(entrypoint, str) or not entrypoint.startswith('client/'):
        raise ValueError('invalid addon entrypoint')
    if not isinstance(digest, str) or not re.fullmatch(r'[0-9a-fA-F]{64}', digest):
        raise ValueError('invalid addon entrypoint digest')
    if digest.lower() != str(expected.get('client_sha256', '')).lower():
        raise ValueError('addon package digest does not match inventory')
    _validate_client_extension_capabilities(manifest)
    return entrypoint, digest


def _verify_addon_manifest_signature(manifest_path, signature_path):
    """Verify detached manifest bytes with the configured addon public key."""
    _read_real_file(signature_path, MAX_ADDON_SIGNATURE_BYTES)
    public_key = addon_public_key_path()
    try:
        public_key_bytes = _read_real_file(public_key, 64 * 1024)
    except (OSError, ValueError):
        raise ValueError('addon signing public key is unavailable')
    temporary_key = None
    try:
        descriptor, temporary_name = tempfile.mkstemp(prefix='.addon-key-', suffix='.pub')
        temporary_key = Path(temporary_name)
        with os.fdopen(descriptor, 'wb') as handle:
            handle.write(public_key_bytes)
        os.chmod(temporary_key, 0o600)
        result = subprocess.run(
            ['openssl', 'pkeyutl', '-verify', '-rawin', '-pubin', '-inkey', str(temporary_key),
             '-sigfile', str(signature_path), '-in', str(manifest_path)],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            timeout=10,
            check=False,
        )
    finally:
        if temporary_key is not None:
            try:
                temporary_key.unlink()
            except FileNotFoundError:
                pass
    if result.returncode != 0:
        raise ValueError('addon manifest signature verification failed')


def _verify_cached_addon(target, active, expected):
    """Revalidate persisted addon files before accepting verified_cached."""
    if set(active) != {'id', 'version', 'client_sha256', 'entrypoint'}:
        raise ValueError('addon cache metadata is invalid')
    manifest_path = target / 'manifest.json'
    signature_path = target / 'manifest.sig'
    manifest_bytes = _read_real_file(manifest_path, MAX_ADDON_MANIFEST_BYTES)
    try:
        manifest = _parse_addon_manifest(manifest_bytes)
    except (UnicodeError, ValueError, json.JSONDecodeError):
        raise ValueError('addon cache manifest is invalid')
    entrypoint, digest = _validate_addon_manifest_metadata(manifest, expected)
    if (active['id'] != expected['id'] or active['version'] != expected['version'] or
            str(active['client_sha256']).lower() != expected['client_sha256'].lower() or
            active['entrypoint'] != entrypoint):
        raise ValueError('addon cache metadata does not match the signed manifest')
    client_path = target / entrypoint
    client_bytes = _read_real_file(client_path, MAX_ADDON_MEMBER_BYTES)
    if hashlib.sha256(client_bytes).hexdigest().lower() != digest.lower():
        raise ValueError('addon entrypoint SHA-256 verification failed')
    _verify_addon_manifest_signature(manifest_path, signature_path)
    return entrypoint


def _atomic_exchange(left, right):
    """Atomically exchange two same-filesystem paths on Linux."""
    if sys.platform != 'linux':
        raise OSError('atomic directory exchange is unavailable')
    libc = ctypes.CDLL(None, use_errno=True)
    renameat2 = getattr(libc, 'renameat2', None)
    if renameat2 is None:
        raise OSError('atomic directory exchange is unavailable')
    renameat2.argtypes = [ctypes.c_int, ctypes.c_char_p, ctypes.c_int, ctypes.c_char_p, ctypes.c_uint]
    renameat2.restype = ctypes.c_int
    result = renameat2(-100, os.fsencode(left), -100, os.fsencode(right), 0x2)
    if result != 0:
        error = ctypes.get_errno()
        raise OSError(error, os.strerror(error))


def addon_public_key_path():
    return Path(os.environ.get(
        'SCREENTRA_CAST_ADDON_SIGNING_PUBLIC_KEY',
        '/etc/screentra-cast/addon-signing.pub',
    ))


def _safe_addon_member(name):
    normalized = posixpath.normpath(name)
    if normalized in ('.', '') or normalized.startswith('/') or normalized.startswith('../') or '/../' in normalized:
        raise ValueError('addon package contains an unsafe path')
    if normalized not in ('manifest.json', 'manifest.sig') and not normalized.startswith('client/'):
        raise ValueError('addon package contains an unexpected member')
    return normalized


def _parse_addon_manifest(manifest_bytes):
    def reject_duplicate_fields(pairs):
        result = {}
        for key, value in pairs:
            if key in result:
                raise ValueError('addon manifest contains duplicate JSON fields')
            result[key] = value
        return result

    try:
        value = json.loads(
            manifest_bytes.decode('utf-8'),
            object_pairs_hook=reject_duplicate_fields,
            parse_constant=lambda _: (_ for _ in ()).throw(ValueError()),
        )
    except (UnicodeDecodeError, json.JSONDecodeError, ValueError) as exc:
        raise ValueError('addon manifest is not valid JSON') from exc
    if not isinstance(value, dict):
        raise ValueError('addon manifest must be an object')
    return value


def _extract_verified_addon_package(package_bytes, expected, staging_parent=None):
    if len(package_bytes) > MAX_ADDON_PACKAGE_BYTES:
        raise ValueError('addon package exceeds the client limit')
    staging = Path(tempfile.mkdtemp(
        prefix='.addon-verify-',
        dir=os.fspath(staging_parent) if staging_parent is not None else None,
    ))
    try:
        with tarfile.open(fileobj=io.BytesIO(package_bytes), mode='r:gz') as archive:
            seen = set()
            extracted_bytes = 0
            for index, member in enumerate(archive):
                if index >= 64:
                    raise ValueError('addon package contains too many members')
                name = _safe_addon_member(member.name)
                if name in seen:
                    raise ValueError('addon package contains duplicate members')
                seen.add(name)
                if member.isdir():
                    continue
                if not member.isfile() or member.size > MAX_ADDON_MEMBER_BYTES:
                    raise ValueError('addon package contains an unsupported or oversized file')
                source = archive.extractfile(member)
                if source is None:
                    raise ValueError('addon package member cannot be read')
                target = staging / name
                target.parent.mkdir(parents=True, exist_ok=True)
                member_bytes = source.read(member.size + 1)
                if len(member_bytes) != member.size:
                    raise ValueError('addon package member size changed while reading')
                extracted_bytes += len(member_bytes)
                if extracted_bytes > MAX_ADDON_EXTRACTED_BYTES:
                    raise ValueError('addon package extracted data exceeds the client limit')
                target.write_bytes(member_bytes)
        manifest_path = staging / 'manifest.json'
        signature_path = staging / 'manifest.sig'
        try:
            manifest_bytes = _read_real_file(manifest_path, MAX_ADDON_MANIFEST_BYTES)
            _read_real_file(signature_path, MAX_ADDON_SIGNATURE_BYTES)
        except (OSError, ValueError) as exc:
            raise ValueError('addon package is missing or redirects its metadata') from exc
        manifest = _parse_addon_manifest(manifest_bytes)
        entrypoint, digest = _validate_addon_manifest_metadata(manifest, expected)
        client_path = staging / entrypoint
        try:
            client_bytes = _read_real_file(client_path, MAX_ADDON_MEMBER_BYTES)
        except (OSError, ValueError) as exc:
            raise ValueError('addon entrypoint is missing or redirects its content') from exc
        if hashlib.sha256(client_bytes).hexdigest().lower() != digest.lower():
            raise ValueError('addon entrypoint SHA-256 verification failed')
        _verify_addon_manifest_signature(manifest_path, signature_path)
        return staging, manifest, entrypoint
    except Exception:
        shutil.rmtree(staging, ignore_errors=True)
        raise


def _publish_verified_addon(target, staging, active_path, active_text):
    """Publish a verified directory with rollback-safe active metadata."""
    _ensure_real_directory(target.parent)
    _ensure_real_directory(staging)
    target_exists = _path_exists(target)
    if target_exists:
        _ensure_real_directory(target)
    old_active = _read_real_file(active_path) if _path_exists(active_path) else None
    try:
        if target_exists:
            _atomic_exchange(staging, target)
        else:
            os.replace(staging, target)
        _atomic_write_text(active_path, active_text)
    except Exception:
        try:
            if target_exists:
                _atomic_exchange(staging, target)
            elif _path_exists(target):
                _ensure_real_directory(target)
                shutil.rmtree(target)
            if old_active is None:
                if _path_exists(active_path):
                    _require_real_file(active_path)
                    Path(active_path).unlink()
            else:
                _atomic_write_text(active_path, old_active.decode('utf-8'))
        finally:
            if _path_exists(staging):
                _ensure_real_directory(staging)
                shutil.rmtree(staging)
        raise
    if _path_exists(staging):
        _ensure_real_directory(staging)
        shutil.rmtree(staging)


def fetch_addon_bytes(server, client_id, addon_id):
    url = (server.rstrip('/') + '/api/clients/' + quote(client_id, safe='') +
           '/addons/' + quote(addon_id, safe='') + '/package')
    req = urllib.request.Request(url, headers=client_auth_headers())
    with urlopen_ripley(req, timeout=20) as response:
        package = response.read(MAX_ADDON_PACKAGE_BYTES + 1)
    if len(package) > MAX_ADDON_PACKAGE_BYTES:
        raise ValueError('addon package exceeds the client limit')
    return package


def client_capability_status():
    """Report local capability observations without enabling any service."""
    camera_nodes = sorted(Path('/dev').glob('video*'))
    microphone_nodes = sorted(Path('/dev/snd').glob('pcm*C*D*c')) if Path('/dev/snd').is_dir() else []
    return [
        {
            'id': 'render_only',
            'state': 'available',
            'detail': 'Built-in declarative renderer is available',
        },
        {
            'id': 'camera_capture',
            'state': 'available' if camera_nodes else 'unavailable',
            'detail': ('Detected local video device' if camera_nodes
                       else 'No local video capture device detected'),
        },
        {
            'id': 'microphone_capture',
            'state': 'available' if microphone_nodes else 'unavailable',
            'detail': ('Detected local ALSA capture device' if microphone_nodes
                       else 'No local ALSA capture device detected'),
        },
        {
            'id': 'local_inference',
            'state': 'disabled',
            'detail': 'Signed client inference service is not enabled',
        },
        {
            'id': 'client_service',
            'state': 'disabled',
            'detail': 'Trusted client service runtime is not enabled',
        },
    ]


def sync_client_addons(server, client_id, cache_dir):
    """Fetch and verify enabled addon packages without executing their code."""
    status_path = Path(cache_dir) / 'addon-status.json'
    status = {
        'at': time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime()),
        'addons': [],
        'capabilities': client_capability_status(),
        # No trusted client supervisor is enabled yet. Keep this explicit so a
        # missing service report cannot be mistaken for a running service.
        'services': [],
    }
    try:
        url = server.rstrip('/') + '/api/clients/' + quote(client_id, safe='') + '/addons'
        req = urllib.request.Request(url, headers=client_auth_headers())
        with urlopen_ripley(req, timeout=10) as response:
            inventory = _parse_server_json(
                _read_bounded_response(response, MAX_JSON_FILE_BYTES)
            )
        addons = inventory.get('addons') if isinstance(inventory, dict) else None
        if not isinstance(addons, list):
            raise ValueError('server returned an invalid addon inventory')
    except Exception:
        status['error'] = GENERIC_INVENTORY_ERROR
        _write_status_report(status_path, status)
        return status

    validated = []
    try:
        for expected in addons:
            if not isinstance(expected, dict) or not ADDON_ID_RE.fullmatch(str(expected.get('id', ''))):
                raise ValueError('invalid addon inventory entry')
            version = _validate_addon_version(expected.get('version'))
            if expected.get('client_api_version') != ADDON_API_VERSION:
                raise ValueError('unsupported addon API version')
            if not isinstance(expected.get('client_sha256'), str) or not re.fullmatch(
                    r'[0-9a-fA-F]{64}', expected['client_sha256']):
                raise ValueError('invalid addon inventory digest')
            validated.append((expected, version))
    except Exception:
        for expected in addons:
            item_status = {}
            if isinstance(expected, dict) and ADDON_ID_RE.fullmatch(str(expected.get('id', ''))):
                item_status['id'] = expected['id']
            item_status.update({'state': 'failed', 'error': GENERIC_ADDON_ERROR})
            status['addons'].append(item_status)
        status['error'] = GENERIC_INVENTORY_ERROR
        _write_status_report(status_path, status)
        return status

    addon_root = Path(os.environ.get('RIPLEY_CLIENT_ADDONS_DIR', str(Path(cache_dir).parent / 'addons')))
    desired_ids = {expected['id'] for expected, _ in validated}
    try:
        _ensure_real_directory(addon_root, create=True)
    except Exception:
        for expected, version in validated:
            status['addons'].append({
                'id': expected['id'], 'version': version,
                'state': 'failed', 'error': GENERIC_ADDON_ERROR,
            })
        status['error'] = GENERIC_ADDON_ERROR
        _write_status_report(status_path, status)
        return status

    for expected, version in validated:
        item_status = {'id': expected['id'], 'version': version}
        try:
            id_dir = addon_root / expected['id']
            if _path_exists(id_dir):
                _ensure_real_directory(id_dir)
            target = id_dir / version
            if _path_exists(target):
                _ensure_real_directory(target)
            active_path = id_dir / 'active.json'
            active = _read_cache_json(active_path, {}) if _path_exists(id_dir) else {}
            target_exists = _path_exists(target)
            if target_exists:
                if (not isinstance(active, dict) or active.get('id') != expected['id'] or
                        active.get('version') != version or
                        str(active.get('client_sha256', '')).lower() != expected['client_sha256'].lower()):
                    item_status.update({'state': 'failed', 'error': GENERIC_ADDON_ERROR})
                    status['addons'].append(item_status)
                    continue
            cached_entrypoint = active.get('entrypoint')
            if cached_entrypoint is not None:
                if not isinstance(cached_entrypoint, str) or not cached_entrypoint.startswith('client/'):
                    raise ValueError('addon cache entrypoint is unsafe')
                if _safe_addon_member(cached_entrypoint) != cached_entrypoint:
                    raise ValueError('addon cache entrypoint is unsafe')
            cached_file = target / cached_entrypoint if cached_entrypoint and _path_exists(target) else None
            if (active.get('id') == expected['id'] and active.get('version') == version and
                    str(active.get('client_sha256', '')).lower() == expected['client_sha256'].lower() and
                    cached_file is not None):
                try:
                    _verify_cached_addon(target, active, expected)
                except Exception:
                    item_status.update({'state': 'failed', 'error': GENERIC_ADDON_ERROR})
                    status['addons'].append(item_status)
                    continue
                item_status.update({'state': 'verified_cached', 'entrypoint': cached_entrypoint})
                status['addons'].append(item_status)
                continue

            package = fetch_addon_bytes(server, client_id, expected['id'])
            staging, manifest, entrypoint = _extract_verified_addon_package(
                package, expected, staging_parent=addon_root)
            if manifest['version'] != version:
                raise ValueError('addon package version changed during verification')
            _validate_client_extension_capabilities(manifest)
            _ensure_real_directory(id_dir, create=True)
            if _path_exists(target):
                _ensure_real_directory(target)
            _publish_verified_addon(
                target,
                staging,
                active_path,
                json.dumps({
                    'id': manifest['id'], 'version': manifest['version'],
                    'client_sha256': manifest['client_sha256'],
                    'entrypoint': entrypoint,
                }, indent=2),
            )
            item_status.update({'state': 'verified', 'entrypoint': entrypoint})
        except Exception:
            item_status.update({'state': 'failed', 'error': GENERIC_ADDON_ERROR})
        status['addons'].append(item_status)

    if len(desired_ids) == len(validated):
        for child in addon_root.iterdir():
            if child.is_symlink() or not child.is_dir() or not ADDON_ID_RE.fullmatch(child.name) or child.name in desired_ids:
                continue
            active_path = child / 'active.json'
            if not _path_exists(active_path):
                continue
            try:
                _ensure_real_directory(child)
                _require_real_file(active_path)
                shutil.rmtree(child)
            except Exception:
                continue
    _write_status_report(status_path, status)
    return status


def post_addon_status(server, client_id, status):
    validate_status_report(status)
    url = server.rstrip('/') + '/api/clients/' + quote(client_id, safe='') + '/addons/status'
    request = urllib.request.Request(
        url,
        data=json.dumps(status).encode('utf-8'),
        headers={'Content-Type': 'application/json', **client_auth_headers()},
        method='POST',
    )
    try:
        with urlopen_ripley(request, timeout=10) as response:
            return response.status == 200
    except urllib.error.HTTPError as error:
        if error.code in (401, 404, 429):
            return False
        raise


def _reject_duplicate_json_keys(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError('server JSON contains duplicate fields')
        result[key] = value
    return result


def _parse_server_json(text):
    try:
        return json.loads(
            text,
            object_pairs_hook=_reject_duplicate_json_keys,
            parse_constant=lambda _: (_ for _ in ()).throw(ValueError()),
        )
    except (TypeError, ValueError, UnicodeError) as exc:
        raise ValueError('server response is not valid JSON') from exc


def _parse_command_response(text):
    if not isinstance(text, str) or len(text.encode('utf-8')) > MAX_COMMAND_RESPONSE_BYTES:
        raise ValueError('server command response exceeds its bound')
    result = _parse_server_json(text)
    if result is not None and not isinstance(result, dict):
        raise ValueError('server command response must be an object or null')
    return result


def poll_command(server, client_id, client_token):
    url = server.rstrip('/') + '/api/clients/' + quote(client_id, safe='') + '/command'
    try:
        req = urllib.request.Request(url, headers=client_auth_headers(client_token))
        with urlopen_ripley(req, timeout=5) as r:
            data = _read_bounded_response(r, MAX_COMMAND_RESPONSE_BYTES)
            return _parse_command_response(data)
    except urllib.error.HTTPError as e:
        if e.code in (401, 404, 429):
            return None
        raise
    except Exception:
        return None


def ack_command(server, client_id):
    url = server.rstrip('/') + '/api/clients/' + quote(client_id, safe='') + '/command/ack'
    try:
        req = urllib.request.Request(url, headers=client_auth_headers(), method='POST')
        with urlopen_ripley(req, timeout=5) as r:
            return _read_bounded_response(r, MAX_TEXT_RESPONSE_BYTES)
    except Exception as e:
        print(f'ack failed: {e}', flush=True)
        return None


def write_pending_command(command, cache_dir):
    pending_path = Path(cache_dir) / 'pending-command.json'
    _ensure_real_directory(pending_path.parent, create=True)
    _atomic_write_text(pending_path, json.dumps(command, indent=2))
    print(f'pending command written: {command.get("action")}', flush=True)
    try:
        import subprocess, shutil
        if shutil.which('systemctl'):
            subprocess.run(['systemctl', '--user', 'start', 'screentra-client-actor.service'], timeout=10)
    except Exception as e:
        print(f'actor start skipped/failed: {e}', flush=True)




def read_command_result(cache_dir):
    result_path = Path(cache_dir) / 'command-result.json'
    if not result_path.exists():
        return None, None
    try:
        return _read_json_file(result_path, None), result_path
    except Exception as e:
        print(f'bad command-result.json: {e}', flush=True)
        try: result_path.unlink()
        except Exception: pass
        return None, None


def fetch_config(server, client_id, cache_dir, display_index=0, config_pattern='assigned-layout-display-{index}.json'):
    url = server.rstrip('/') + '/api/clients/' + quote(client_id, safe='') + '/displays/' + str(display_index) + '/config'
    try:
        req = urllib.request.Request(url, headers=client_auth_headers())
        with urlopen_ripley(req, timeout=10) as r:
            data = _parse_server_json(_read_bounded_response(r, MAX_JSON_FILE_BYTES))
        # Preserve theme (set by heartbeat from local config, sent to server)
        # so it survives across config sync cycles.
        prev_path = Path(cache_dir, 'client-config.json')
        try:
            prev = _read_json_file(prev_path, {})
            if prev.get('theme'):
                data['theme'] = prev['theme']
        except Exception:
            pass
        _ensure_real_directory(Path(cache_dir), create=True)
        if display_index == 0:
            _atomic_write_text(Path(cache_dir, 'client-config.json'), json.dumps(data, indent=2))
        else:
            _atomic_write_text(Path(cache_dir, f'display-config-{display_index}.json'), json.dumps(data, indent=2))
        target = config_pattern.format(index=display_index)
        if data.get('layout') is not None:
            # backwards-compat: also write the canonical index=0 name
            _atomic_write_text(Path(cache_dir, target), json.dumps(data['layout'], indent=2))
            if display_index == 0:
                _atomic_write_text(Path(cache_dir, 'assigned-layout.json'), json.dumps(data['layout'], indent=2))
        else:
            for stale in [Path(cache_dir, target), Path(cache_dir, 'assigned-layout.json') if display_index == 0 else None]:
                if stale is None: continue
                try: stale.unlink()
                except FileNotFoundError: pass
        return data
    except Exception as e:
        print(f'config sync failed: {e}', flush=True)
        return None


def fetch_all_displays(server, client_id, cache_dir, display_count, config_pattern):
    """Fetch per-display config and populate cache/assigned-layout-display-N.json."""
    for idx in range(display_count):
        fetch_config(server, client_id, cache_dir, display_index=idx, config_pattern=config_pattern)


def fetch_admin_users(server, client_id, cache_dir):
    """Sync server users for the local admin login.

    Server users are cached for authenticated local administration; without
    that cache the local admin login remains unprovisioned.
    """
    url = server.rstrip('/') + '/api/clients/' + quote(client_id, safe='') + '/admin-users'
    try:
        req = urllib.request.Request(url, headers=client_auth_headers())
        with urlopen_ripley(req, timeout=10) as r:
            data = _parse_server_json(_read_bounded_response(r, MAX_JSON_FILE_BYTES))
        users = data.get('users') or []
        if users:
            target = Path(cache_dir).parent / 'admin-users.json'
            _ensure_real_directory(target.parent, create=True)
            _atomic_write_text(target, json.dumps(data, indent=2))
            print(f'admin users synced: {len(users)}', flush=True)
            return data
        return None
    except Exception as e:
        print(f'admin user sync failed: {e}', flush=True)
        return None

def post(server, data):
    url = server.rstrip('/') + '/api/clients/register'
    payload = dict(data)
    token = client_auth_token()
    if token:
        payload.setdefault('client_auth_token', token)
    req = urllib.request.Request(
        url,
        data=json.dumps(payload).encode(),
        headers={'Content-Type': 'application/json', **client_auth_headers()},
        method='POST',
    )
    with urlopen_ripley(req, timeout=10) as r:
        return r.status, _read_bounded_response(r, MAX_TEXT_RESPONSE_BYTES)


def main():
    def _default_server():
        env_url = os.environ.get('RIPLEY_SERVER_URL')
        if env_url:
            return env_url
        # Fallback: read the persisted server URL the installer writes.
        srv = Path.home() / '.local/share/screentra-client/server'
        if srv.exists():
            try:
                v = _read_real_file(srv, max_bytes=4096).decode('utf-8').strip()
                if v:
                    return v
            except Exception:
                pass
        return None

    ap = argparse.ArgumentParser()
    ap.add_argument('--server', default=_default_server())
    ap.add_argument('--interval', type=int, default=int(os.environ.get('RIPLEY_HEARTBEAT_INTERVAL', '30')))
    ap.add_argument('--once', action='store_true')
    ap.add_argument('--client-id', default=os.environ.get('RIPLEY_CLIENT_ID'))
    ap.add_argument('--name', default=os.environ.get('RIPLEY_CLIENT_NAME'))
    ap.add_argument('--role', default=os.environ.get('RIPLEY_CLIENT_ROLE', 'player'))
    ap.add_argument('--version', default=os.environ.get('RIPLEY_CLIENT_VERSION', '0.1.0'))
    ap.add_argument('--active-layout-id', default=os.environ.get('RIPLEY_ACTIVE_LAYOUT_ID'))
    ap.add_argument('--cache-dir', default=os.environ.get('RIPLEY_CLIENT_CACHE_DIR', os.path.expanduser('~/.local/share/screentra-client/cache')))
    ap.add_argument('--max-displays', type=int, default=int(os.environ.get('RIPLEY_MAX_DISPLAYS', '4')))
    ap.add_argument('--render-base-port', type=int, default=int(os.environ.get('RIPLEY_RENDER_BASE_PORT', '3900')))
    ap.add_argument('--display-config-pattern', default=os.environ.get('RIPLEY_DISPLAY_CONFIG_PATTERN', 'assigned-layout-display-{index}.json'))

    args = ap.parse_args()
    try:
        parsed_server = validate_server_url(args.server)
        if parsed_server.scheme == 'https':
            tls_context()
    except Exception as error:
        ap.error(str(error))

    while True:
        # Refresh configuration before building the registration payload so the
        # heartbeat acknowledges the revision the player has just fetched.
        probe = payload(args)
        try:
            fetch_config(args.server, probe['client_id'], args.cache_dir, display_index=0, config_pattern=args.display_config_pattern)
            fetch_all_displays(args.server, probe['client_id'], args.cache_dir, len(probe.get('displays') or []), args.display_config_pattern)
        except Exception as e:
            print(f'pre-heartbeat config refresh failed: {e}', flush=True)
        data = payload(args)
        try:
            status, body = post(args.server, data)
            try:
                response = _parse_server_json(body)
                server_theme = response.get('theme')
                if server_theme in ('midnight', 'light', 'classic'):
                    theme_path = Path(args.cache_dir) / 'client-config.json'
                    try:
                        cached = _read_json_file(theme_path, {})
                    except Exception:
                        cached = {}
                    cached['theme'] = server_theme
                    _ensure_real_directory(theme_path.parent, create=True)
                    _atomic_write_text(theme_path, json.dumps(cached, indent=2))
            except Exception:
                pass
        except Exception as e:
            # A temporary server outage must not kill the heartbeat service.
            status, body = 0, ''
            print(f'heartbeat post failed: {e}', flush=True)
        if status == 200 and data.get('last_command_result') is not None:
            rp = Path(args.cache_dir) / 'command-result.json'
            try:
                rp.unlink()
                print('command-result.json uploaded and cleared', flush=True)
            except Exception:
                pass
        previous_orientation = 'normal'
        try:
            previous_orientation = _read_json_file(Path(args.cache_dir) / 'client-config.json', {}).get('orientation', 'normal')
        except Exception:
            pass
        # Track previous layout assignment to detect changes.
        prev_layout_path = Path(args.cache_dir) / 'previous-layout-id.txt'
        try:
            previous_layout_id = _read_bounded_text(prev_layout_path, 4096).strip() or None
        except Exception:
            previous_layout_id = None
        cfg = fetch_config(args.server, data['client_id'], args.cache_dir, display_index=0, config_pattern=args.display_config_pattern)
        if cfg is None:
            # A transient 429/network failure must not turn a valid cached
            # assignment into "No layout assigned" on the renderer.
            cached_cfg = _read_json_file(Path(args.cache_dir) / 'client-config.json', {})
            current_layout_id = cached_cfg.get('assigned_layout_id') or previous_layout_id
            current_orientation = cached_cfg.get('orientation', previous_orientation)
        else:
            current_layout_id = cfg.get('assigned_layout_id')
            current_orientation = cfg.get('orientation', previous_orientation)
        # Restart kiosk on layout assignment change (e.g. after server nudge or editor assign).
        if cfg and current_layout_id and current_layout_id != previous_layout_id:
            try:
                subprocess.run(['sudo', '-n', 'systemctl', 'restart', 'screentra-cast-kiosk@0.service'], check=True, timeout=15)
                print(f'layout changed {previous_layout_id} -> {current_layout_id}; kiosk restarted', flush=True)
                _atomic_write_text(prev_layout_path, current_layout_id or '')
            except Exception as e:
                print(f'layout-change restart failed: {e}', flush=True)
        elif cfg and current_layout_id:
            _atomic_write_text(prev_layout_path, current_layout_id or '')
        if cfg and current_orientation != previous_orientation:
            try:
                subprocess.run(['sudo', '-n', 'systemctl', 'restart', 'screentra-cast-kiosk@0.service'], check=True, timeout=15)
                print(f'orientation changed {previous_orientation} -> {current_orientation}; kiosk restarted', flush=True)
            except Exception as e:
                print(f'orientation restart failed: {e}', flush=True)
        fetch_all_displays(args.server, data['client_id'], args.cache_dir, len(data.get('displays') or []), args.display_config_pattern)
        fetch_admin_users(args.server, data['client_id'], args.cache_dir)
        addon_status = sync_client_addons(args.server, data['client_id'], args.cache_dir)
        try:
            post_addon_status(args.server, data['client_id'], addon_status)
        except Exception as error:
            print(f'addon status report failed: {error}', flush=True)
        if addon_status.get('addons') or addon_status.get('error'):
            print(f'addon sync: {json.dumps(addon_status, separators=(",", ":"))}', flush=True)
        assigned = current_layout_id
        cmd = poll_command(args.server, data['client_id'], client_auth_token())
        if cmd:
            write_pending_command(cmd, args.cache_dir)
            ack = ack_command(args.server, data['client_id'])
            print(f'heartbeat status={status} client_id={data["client_id"]} assigned_layout={assigned} command={cmd.get("action")} ack={ack}', flush=True)
        else:
            print(f'heartbeat status={status} client_id={data["client_id"]} assigned_layout={assigned}', flush=True)

        # Persist a per-display cache so the renderer can show
        # the connector / resolution label on the no-layout page.
        try:
            for d in data.get('displays') or []:
                idx = d.get('display_index')
                if idx is None: continue
                _atomic_write_text(
                    Path(args.cache_dir, f'display-{idx}.json'),
                    __import__('json').dumps(d),
                )
        except Exception:
            pass

        # Persist a small status file so the renderer's no-layout page
        # can read live heartbeat info (last_seen, connection, assigned_layout_id)
        try:
            from datetime import datetime as _dt, timezone as _tz
            status_payload = {
                'at': _dt.now(_tz.utc).isoformat(),
                'connection': 'online' if status == 200 else 'offline',
                'assigned_layout_id': assigned,
                'pairing_code': client_pairing_code(data['client_id']),
            }
            _ensure_real_directory(Path(args.cache_dir), create=True)
            _atomic_write_text(
                Path(args.cache_dir) / 'last-heartbeat.json',
                __import__('json').dumps(status_payload),
            )
        except Exception:
            pass
        if args.once:
            print(body)
            return 0
        time.sleep(args.interval)


if __name__ == '__main__':
    raise SystemExit(main())
