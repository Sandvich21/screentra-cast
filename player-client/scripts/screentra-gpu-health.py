#!/usr/bin/env python3
from __future__ import annotations
import json
import os
import re
import shutil
import subprocess
import sys
from pathlib import Path


MAX_FILE_READ_BYTES = 4096
MAX_SUBPROCESS_OUTPUT_BYTES = 64 * 1024
MAX_GPU_RECORDS = 16
MAX_GPU_NAME_BYTES = 256


def _read(path: Path) -> str | None:
    try:
        with path.open('rb') as handle:
            raw = handle.read(MAX_FILE_READ_BYTES)
        return raw.decode(errors='replace').strip()
    except Exception:
        return None


def _num(text: str | None) -> float | None:
    if text is None:
        return None
    try:
        return float(str(text).strip().replace('%', '').replace('MiB', '').replace('MHz', ''))
    except Exception:
        return None


def _temp_value(raw: str | None) -> float | None:
    v = _num(raw)
    if v is None:
        return None
    c = v / 1000.0 if abs(v) > 1000 else v
    if -30.0 < c < 140.0:
        return round(c, 1)
    return None


def _mhz_value(raw: str | None) -> float | None:
    v = _num(raw)
    if v is None or v <= 0:
        return None
    mhz = v / 1000000.0 if v > 1000000 else v / 1000.0 if v > 1000 else v
    return round(mhz, 0) if mhz > 0 else None




def _bound_subprocess_output(value: str) -> str:
    encoded = str(value).encode('utf-8', errors='replace')
    return encoded[:MAX_SUBPROCESS_OUTPUT_BYTES].decode('utf-8', errors='ignore')


def _bound_gpu_name(value: str) -> str:
    encoded = str(value).encode('utf-8', errors='replace')
    return encoded[:MAX_GPU_NAME_BYTES].decode('utf-8', errors='ignore').strip()


def _run(args: list[str], timeout: float = 2.0) -> tuple[int, str, str]:
    try:
        p = subprocess.run(args, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=timeout, check=False)
        return p.returncode, _bound_subprocess_output(p.stdout or '').strip(), _bound_subprocess_output(p.stderr or '').strip()
    except Exception as e:
        return 127, '', _bound_subprocess_output(str(e))


def pci_gpus() -> list[dict]:
    gpus = []
    lspci = shutil.which('lspci')
    if lspci:
        rc, out, err = _run([lspci, '-nn'], timeout=2.0)
        if rc == 0:
            for line in out.splitlines():
                low = line.lower()
                if 'vga compatible controller' in low or '3d controller' in low or 'display controller' in low:
                    vendor = 'unknown'
                    if 'nvidia' in low:
                        vendor = 'nvidia'
                    elif 'intel' in low:
                        vendor = 'intel'
                    elif 'amd' in low or 'ati' in low or 'advanced micro devices' in low:
                        vendor = 'amd'
                    gpus.append({'vendor': vendor, 'name': _bound_gpu_name(line), 'source': 'lspci'})
                    if len(gpus) >= MAX_GPU_RECORDS:
                        break
            if gpus:
                return gpus
    # Sysfs fallback without pciutils.
    for dev in sorted(Path('/sys/class/drm').glob('card*/device')):
        vendor_id = _read(dev / 'vendor') or ''
        device_id = _read(dev / 'device') or ''
        vendor = {'0x10de': 'nvidia', '0x8086': 'intel', '0x1002': 'amd', '0x1022': 'amd'}.get(vendor_id.lower(), 'unknown')
        if vendor != 'unknown' or device_id:
            gpus.append({'vendor': vendor, 'name': _bound_gpu_name(f'{vendor} GPU {vendor_id} {device_id}'.strip()), 'source': str(dev)})
            if len(gpus) >= MAX_GPU_RECORDS:
                break
    return gpus


def drm_usage() -> dict | None:
    for busy in sorted(Path('/sys/class/drm').glob('card*/device/gpu_busy_percent')):
        v = _num(_read(busy))
        if v is None:
            continue
        out = {'percent': v, 'device': busy.parent.parent.name, 'source': str(busy), 'method': 'drm-gpu_busy_percent'}
        try:
            total = int((_read(busy.parent / 'mem_info_vram_total') or '0').strip())
            used = int((_read(busy.parent / 'mem_info_vram_used') or '0').strip())
            if total > 0:
                out.update({'vram_total_bytes': total, 'vram_used_bytes': used, 'vram_percent': round((used / total) * 100.0, 1)})
        except Exception:
            pass
        return out
    return None


def hwmon_for(names: tuple[str, ...]) -> dict | None:
    for hw in sorted(Path('/sys/class/hwmon').glob('hwmon*')):
        name = (_read(hw / 'name') or '').lower()
        if not any(n in name for n in names):
            continue
        out = {'device': name or hw.name, 'source': str(hw), 'method': 'hwmon'}
        for temp in sorted(hw.glob('temp*_input')):
            c = _temp_value(_read(temp))
            if c is not None:
                out['temp_c'] = c
                break
        for freq in sorted(hw.glob('freq*_input')):
            mhz = _mhz_value(_read(freq))
            if mhz is not None:
                out['frequency_mhz'] = mhz
                break
        # Some Intel/AMD drivers use gt_*/pp_* files under DRM device.
        return out
    return None


def intel_extra(out: dict) -> dict:
    # i915/xe can expose RPS/GT clocks under DRM debug/sysfs depending on kernel.
    for dev in sorted(Path('/sys/class/drm').glob('card*/device')):
        driver = ''
        try:
            driver = dev.resolve().joinpath('driver').resolve().name.lower()
        except Exception:
            pass
        if driver not in ('i915', 'xe'):
            continue
        out.setdefault('device', driver)
        out.setdefault('source', str(dev))
        out.setdefault('method', 'intel-sysfs')
        for name in ('gt_cur_freq_mhz', 'gt_act_freq_mhz', 'rps_cur_freq_mhz'):
            mhz = _num(_read(dev / name))
            if mhz and mhz > 0:
                out['frequency_mhz'] = round(mhz, 0)
                break
        break
    return out


def nvidia_smi() -> dict | None:
    smi = shutil.which('nvidia-smi')
    if not smi:
        return None
    query = 'name,utilization.gpu,temperature.gpu,memory.used,memory.total,driver_version'
    rc, out, err = _run([smi, f'--query-gpu={query}', '--format=csv,noheader,nounits'], timeout=3.0)
    if rc != 0 or not out:
        return {'vendor': 'nvidia', 'method': 'nvidia-smi', 'source': smi, 'available': False, 'error': err or out or f'exit {rc}'}
    line = out.splitlines()[0]
    parts = [p.strip() for p in line.split(',')]
    while len(parts) < 6:
        parts.append('')
    name, util, temp, used, total, driver = parts[:6]
    used_f, total_f = _num(used), _num(total)
    result = {
        'vendor': 'nvidia', 'device': name or 'NVIDIA GPU', 'percent': _num(util), 'temp_c': _num(temp),
        'memory_used_mib': used_f, 'memory_total_mib': total_f, 'driver_version': driver or None,
        'source': smi, 'method': 'nvidia-smi', 'available': True,
    }
    if used_f is not None and total_f and total_f > 0:
        result['vram_percent'] = round((used_f / total_f) * 100.0, 1)
    return result


def classify_health(g: dict) -> dict:
    pct = g.get('percent')
    temp = g.get('temp_c')
    vram = g.get('vram_percent')
    level = 'unknown'
    notes: list[str] = []
    if pct is None:
        notes.append('usage telemetry unavailable')
    elif pct < 60:
        level = 'ok'
    elif pct < 85:
        level = 'busy'
        notes.append('GPU is working but has headroom pressure')
    else:
        level = 'hot'
        notes.append('GPU utilization is high for a signage player')
    if temp is not None:
        if temp >= 90:
            level = 'hot'; notes.append('GPU temperature is very high')
        elif temp >= 80 and level == 'ok':
            level = 'busy'; notes.append('GPU temperature is warm')
    if vram is not None and vram >= 85:
        level = 'busy' if level == 'ok' else level
        notes.append('GPU memory usage is high')
    if level == 'unknown' and (temp is not None or g.get('frequency_mhz') is not None):
        level = 'telemetry-partial'
    g['health'] = level
    g['health_notes'] = notes
    return g


def main() -> int:
    gpus = pci_gpus()
    vendors = {g.get('vendor') for g in gpus}
    out: dict = {'available': False, 'vendor': None, 'device': None, 'source': None, 'gpus': gpus, 'packages': {}}

    # NVIDIA proprietary path first: this is the only reliable NVIDIA utilization source.
    if 'nvidia' in vendors or shutil.which('nvidia-smi'):
        nv = nvidia_smi()
        if nv:
            out.update(nv)
            out['available'] = bool(nv.get('available', True))
            if not out['available']:
                out['packages']['debian'] = ['nvidia-smi or nvidia-utils package from Debian non-free-firmware/non-free repository']
            print(json.dumps(classify_health(out)))
            return 0

    drm = drm_usage()
    if drm:
        out.update(drm)
        # Guess vendor from card's PCI id if available.
        out['vendor'] = next((g.get('vendor') for g in gpus if g.get('vendor') != 'unknown'), out.get('vendor'))
        out['available'] = True
        print(json.dumps(classify_health(out)))
        return 0

    if 'amd' in vendors:
        h = hwmon_for(('amdgpu', 'radeon')) or {}
        out.update(h)
        out.update({'vendor': 'amd', 'available': bool(h), 'packages': {'debian': ['mesa-utils', 'libgl1-mesa-dri', 'firmware-amd-graphics']}})
        print(json.dumps(classify_health(out)))
        return 0

    if 'intel' in vendors:
        h = hwmon_for(('i915', 'xe', 'intel')) or {}
        h = intel_extra(h)
        out.update(h)
        out.update({'vendor': 'intel', 'available': bool(h.get('temp_c') is not None or h.get('frequency_mhz') is not None or h.get('source')), 'packages': {'debian': ['intel-gpu-tools', 'mesa-utils', 'libgl1-mesa-dri', 'firmware-misc-nonfree']}})
        if shutil.which('intel_gpu_top'):
            out['tool_available'] = 'intel_gpu_top'
            out['note'] = 'intel_gpu_top installed; live utilization may require elevated/capability access, so admin card uses safe sysfs telemetry.'
        print(json.dumps(classify_health(out)))
        return 0

    # Last chance: hwmon names can reveal vendor even without lspci.
    for vendor, names in [('amd', ('amdgpu', 'radeon')), ('intel', ('i915', 'xe', 'intel')), ('nvidia', ('nvidia',))]:
        h = hwmon_for(names)
        if h:
            out.update(h); out.update({'vendor': vendor, 'available': True})
            print(json.dumps(classify_health(out)))
            return 0

    out['error'] = 'No supported GPU telemetry source found. Install pciutils for GPU identification; NVIDIA needs nvidia-smi; Intel may need intel-gpu-tools/Mesa firmware.'
    out['packages']['debian'] = ['pciutils', 'mesa-utils', 'libgl1-mesa-dri', 'intel-gpu-tools', 'firmware-amd-graphics', 'firmware-misc-nonfree']
    print(json.dumps(classify_health(out)))
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
