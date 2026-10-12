#!/usr/bin/env python3
"""Screentra Cast client/client heartbeat agent.

Posts system metrics to the Screentra Cast signage server so the Connected Clients panel
can show enrolled player uptime and usage.
"""
import argparse
import json
import ssl
import os
import socket
import subprocess
import sys
import time
import urllib.request
import importlib.util
import sys as _sys
_display_detect_path = os.path.join(os.path.dirname(os.path.abspath(__file__)), 'screentra-display-detect.py')
_spec = importlib.util.spec_from_file_location('screentra_display_detect', _display_detect_path)
_mod = importlib.util.module_from_spec(_spec)
_sys.modules['screentra_display_detect'] = _mod
_spec.loader.exec_module(_mod)
detect_displays = _mod.detect_displays

from pathlib import Path


def _atomic_write_text(path: Path, text: str, mode: int = 0o600) -> None:
    """Replace a cache JSON file atomically and keep it owner-readable."""
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(f'.{path.name}.{os.getpid()}.tmp')
    temporary.write_text(text)
    os.chmod(temporary, mode)
    os.replace(temporary, path)


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
        cfg = json.loads(cfg_path.read_text())
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
        cfg = json.loads(cfg_path.read_text())
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
        cfg = json.loads(cfg_path.read_text())
        binding = cfg.get('touch_binding')
        return binding if isinstance(binding, dict) else None
    except Exception:
        return None


def run(cmd):
    try:
        return subprocess.check_output(cmd, shell=True, text=True, stderr=subprocess.DEVNULL).strip()
    except Exception:
        return ""


def read_mem():
    values = {}
    try:
        for line in open('/proc/meminfo'):
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
        return int(float(open('/proc/uptime').read().split()[0]))
    except Exception:
        return None


def read_cpu_usage(interval=0.25):
    def sample():
        fields = [int(x) for x in open('/proc/stat').readline().split()[1:]]
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
            cached = json.loads(cached_cfg.read_text())
            active_layout_id = active_layout_id or cached.get('assigned_layout_id')
            value = cached.get('active_layout_revision')
            active_layout_revision = int(value) if value is not None else None
    except Exception:
        pass
    for display in displays:
        try:
            display_index = int(display.get('display_index', 0))
            display_cfg_path = Path(args.cache_dir, 'client-config.json' if display_index == 0 else f'display-config-{display_index}.json')
            display_cfg = json.loads(display_cfg_path.read_text()) if display_cfg_path.exists() else {}
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
    if cafile and Path(cafile).exists():
        try:
            return ssl.create_default_context(cafile=cafile)
        except Exception as e:
            print(f'warning: failed to load Screentra Cast CA {cafile}: {e}', flush=True)
    if os.environ.get('RIPLEY_INSECURE_SSL', '').lower() in ('1', 'true', 'yes'):
        return ssl._create_unverified_context()
    return None

def urlopen_ripley(req, timeout=10):
    kw = {'timeout': timeout}
    ctx = tls_context()
    if ctx is not None:
        kw['context'] = ctx
    return urllib.request.urlopen(req, **kw)


def poll_command(server, client_id):
    url = server.rstrip('/') + '/api/clients/' + urllib.request.quote(client_id, safe='') + '/command'
    try:
        with urlopen_ripley(url, timeout=5) as r:
            data = r.read().decode()
            return json.loads(data)
    except urllib.error.HTTPError as e:
        if e.code == 404:
            return None
        raise
    except Exception:
        return None


def ack_command(server, client_id):
    url = server.rstrip('/') + '/api/clients/' + urllib.request.quote(client_id, safe='') + '/command'
    try:
        req = urllib.request.Request(url, method='DELETE')
        with urlopen_ripley(req, timeout=5) as r:
            return r.read().decode()
    except Exception as e:
        print(f'ack failed: {e}', flush=True)
        return None


def write_pending_command(command, cache_dir):
    pending_path = Path(cache_dir) / 'pending-command.json'
    pending_path.parent.mkdir(parents=True, exist_ok=True)
    _atomic_write_text(pending_path, json.dumps(command, indent=2))
    print(f'pending command written: {command.get("action")}', flush=True)
    try:
        import subprocess, shutil
        if shutil.which('systemctl'):
            subprocess.run(['systemctl', '--user', 'start', 'ripley-player-actor.service'], timeout=10)
    except Exception as e:
        print(f'actor start skipped/failed: {e}', flush=True)




def read_command_result(cache_dir):
    result_path = Path(cache_dir) / 'command-result.json'
    if not result_path.exists():
        return None, None
    try:
        return json.loads(result_path.read_text()), result_path
    except Exception as e:
        print(f'bad command-result.json: {e}', flush=True)
        try: result_path.unlink()
        except Exception: pass
        return None, None


def fetch_config(server, client_id, cache_dir, display_index=0, config_pattern='assigned-layout-display-{index}.json'):
    url = server.rstrip('/') + '/api/clients/' + urllib.request.quote(client_id, safe='') + '/displays/' + str(display_index) + '/config'
    try:
        with urlopen_ripley(url, timeout=10) as r:
            data = json.loads(r.read().decode())
        # Preserve theme (set by heartbeat from local config, sent to server)
        # so it survives across config sync cycles.
        prev_path = Path(cache_dir, 'client-config.json')
        try:
            prev = json.loads(prev_path.read_text())
            if prev.get('theme'):
                data['theme'] = prev['theme']
        except Exception:
            pass
        Path(cache_dir).mkdir(parents=True, exist_ok=True)
        if display_index == 0:
            Path(cache_dir, 'client-config.json').write_text(json.dumps(data, indent=2))
        else:
            Path(cache_dir, f'display-config-{display_index}.json').write_text(json.dumps(data, indent=2))
        target = config_pattern.format(index=display_index)
        if data.get('layout') is not None:
            # backwards-compat: also write the canonical index=0 name
            Path(cache_dir, target).write_text(json.dumps(data['layout'], indent=2))
            if display_index == 0:
                Path(cache_dir, 'assigned-layout.json').write_text(json.dumps(data['layout'], indent=2))
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

    The local admin page falls back to admin/admin only until this cache exists.
    Once server users are cached, fallback is disabled and password hashes from
    the server are authoritative.
    """
    url = server.rstrip('/') + '/api/clients/' + urllib.request.quote(client_id, safe='') + '/admin-users'
    try:
        with urlopen_ripley(url, timeout=10) as r:
            data = json.loads(r.read().decode())
        users = data.get('users') or []
        if users:
            target = Path(cache_dir).parent / 'admin-users.json'
            target.parent.mkdir(parents=True, exist_ok=True)
            tmp = target.with_suffix('.json.tmp')
            tmp.write_text(json.dumps(data, indent=2))
            try: os.chmod(tmp, 0o600)
            except Exception: pass
            tmp.replace(target)
            print(f'admin users synced: {len(users)}', flush=True)
            return data
        return None
    except Exception as e:
        print(f'admin user sync failed: {e}', flush=True)
        return None

def post(server, data):
    url = server.rstrip('/') + '/api/clients/register'
    req = urllib.request.Request(
        url,
        data=json.dumps(data).encode(),
        headers={'Content-Type': 'application/json'},
        method='POST',
    )
    with urlopen_ripley(req, timeout=10) as r:
        return r.status, r.read().decode()


def main():
    def _default_server():
        env_url = os.environ.get('RIPLEY_SERVER_URL')
        if env_url:
            return env_url
        # Fallback: read the persisted server URL the installer writes.
        srv = Path.home() / '.local/share/screentra-client/server'
        if srv.exists():
            try:
                v = srv.read_text().strip()
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
                response = json.loads(body)
                server_theme = response.get('theme')
                if server_theme in ('midnight', 'light', 'classic'):
                    theme_path = Path(args.cache_dir) / 'client-config.json'
                    try:
                        cached = json.loads(theme_path.read_text()) if theme_path.exists() else {}
                    except Exception:
                        cached = {}
                    cached['theme'] = server_theme
                    theme_path.parent.mkdir(parents=True, exist_ok=True)
                    theme_path.write_text(json.dumps(cached, indent=2))
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
            previous_orientation = json.loads((Path(args.cache_dir) / 'client-config.json').read_text()).get('orientation', 'normal')
        except Exception:
            pass
        # Track previous layout assignment to detect changes.
        prev_layout_path = Path(args.cache_dir) / 'previous-layout-id.txt'
        try:
            previous_layout_id = prev_layout_path.read_text().strip() or None
        except Exception:
            previous_layout_id = None
        cfg = fetch_config(args.server, data['client_id'], args.cache_dir, display_index=0, config_pattern=args.display_config_pattern)
        current_layout_id = (cfg or {}).get('assigned_layout_id')
        current_orientation = (cfg or {}).get('orientation', previous_orientation)
        # Restart kiosk on layout assignment change (e.g. after server nudge or editor assign).
        if cfg and current_layout_id and current_layout_id != previous_layout_id:
            try:
                subprocess.run(['sudo', '-n', 'systemctl', 'restart', 'screentra-cast-kiosk@0.service'], check=True, timeout=15)
                print(f'layout changed {previous_layout_id} -> {current_layout_id}; kiosk restarted', flush=True)
                prev_layout_path.write_text(current_layout_id or '')
            except Exception as e:
                print(f'layout-change restart failed: {e}', flush=True)
        elif cfg and current_layout_id:
            prev_layout_path.write_text(current_layout_id or '')
        if cfg and current_orientation != previous_orientation:
            try:
                subprocess.run(['sudo', '-n', 'systemctl', 'restart', 'screentra-cast-kiosk@0.service'], check=True, timeout=15)
                print(f'orientation changed {previous_orientation} -> {current_orientation}; kiosk restarted', flush=True)
            except Exception as e:
                print(f'orientation restart failed: {e}', flush=True)
        fetch_all_displays(args.server, data['client_id'], args.cache_dir, len(data.get('displays') or []), args.display_config_pattern)
        fetch_admin_users(args.server, data['client_id'], args.cache_dir)
        assigned = cfg.get('assigned_layout_id') if cfg else None
        cmd = poll_command(args.server, data['client_id'])
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
                Path(args.cache_dir, f'display-{idx}.json').write_text(__import__('json').dumps(d))
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
            }
            Path(args.cache_dir).mkdir(parents=True, exist_ok=True)
            (Path(args.cache_dir) / 'last-heartbeat.json').write_text(__import__('json').dumps(status_payload))
        except Exception:
            pass
        if args.once:
            print(body)
            return 0
        time.sleep(args.interval)


if __name__ == '__main__':
    raise SystemExit(main())
