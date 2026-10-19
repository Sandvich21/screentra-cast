#!/usr/bin/env python3
"""Pure admission for the client's metadata-only status snapshot."""
import json
import re
from collections.abc import Mapping
from datetime import datetime

MAX_REPORT_BYTES = 256 * 1024
MAX_CAPABILITIES = 8
MAX_ADDONS = 64
MAX_DETAIL_BYTES = 160
MAX_ERROR_BYTES = 96

_ALLOWED_FIELDS = frozenset({'at', 'addons', 'capabilities', 'services', 'error'})
_ALLOWED_CAPABILITIES = frozenset({
    'render_only',
    'camera_capture',
    'microphone_capture',
    'local_inference',
    'client_service',
})
_ALLOWED_STATES = frozenset({'available', 'unavailable', 'disabled'})
_ADDON_STATES = frozenset({'verified', 'verified_cached', 'failed'})
_ADDON_FIELDS = frozenset({'id', 'version', 'state', 'entrypoint', 'error'})
_ADDON_ID_RE = re.compile(r'^[a-z0-9][a-z0-9._-]{0,63}$')
_ADDON_VERSION_RE = re.compile(r'^[a-z0-9][a-z0-9._-]{0,63}$')
_TIMESTAMP_RE = re.compile(r'^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z$')
_FORBIDDEN_DETAIL_MARKERS = ('password', 'credential', 'secret', 'token', '://', '/')


class StatusReportError(ValueError):
    """Raised when a status snapshot crosses the inert report boundary."""


def _bounded_text(value, maximum, field):
    if not isinstance(value, str) or not value or len(value.encode('utf-8')) > maximum:
        raise StatusReportError(f'invalid status report {field}')
    if any(ord(character) < 0x20 or ord(character) == 0x7F for character in value):
        raise StatusReportError(f'invalid status report {field}')
    return value


def _validate_capabilities(value):
    if not isinstance(value, list) or len(value) > MAX_CAPABILITIES:
        raise StatusReportError('invalid status report capabilities')
    seen = set()
    for capability in value:
        if not isinstance(capability, Mapping) or set(capability) != {'id', 'state', 'detail'}:
            raise StatusReportError('invalid status report capability shape')
        identifier = capability['id']
        state = capability['state']
        detail = capability['detail']
        if identifier not in _ALLOWED_CAPABILITIES or identifier in seen:
            raise StatusReportError('invalid or duplicate status report capability')
        if state not in _ALLOWED_STATES:
            raise StatusReportError('invalid status report capability state')
        detail = _bounded_text(detail, MAX_DETAIL_BYTES, 'capability detail')
        lowered = detail.lower()
        if any(marker in lowered for marker in _FORBIDDEN_DETAIL_MARKERS):
            raise StatusReportError('unsafe status report capability detail')
        seen.add(identifier)


def _validate_addons(value):
    if not isinstance(value, list) or len(value) > MAX_ADDONS:
        raise StatusReportError('invalid status report addons')
    seen = set()
    for addon in value:
        if not isinstance(addon, Mapping) or set(addon) - _ADDON_FIELDS:
            raise StatusReportError('invalid status report addon shape')
        identifier = addon.get('id')
        state = addon.get('state')
        if not isinstance(identifier, str) or not _ADDON_ID_RE.fullmatch(identifier):
            raise StatusReportError('invalid status report addon id')
        if identifier in seen:
            raise StatusReportError('duplicate status report addon')
        if state not in _ADDON_STATES:
            raise StatusReportError('invalid status report addon state')
        version = addon.get('version')
        if version is not None and (
                not isinstance(version, str) or not _ADDON_VERSION_RE.fullmatch(version)):
            raise StatusReportError('invalid status report addon version')
        entrypoint = addon.get('entrypoint')
        if entrypoint is not None:
            if (not isinstance(entrypoint, str) or entrypoint == 'client/' or
                    not entrypoint.startswith('client/') or '\\' in entrypoint or
                    '..' in entrypoint.split('/') or
                    any(ord(character) < 0x20 or ord(character) == 0x7F for character in entrypoint) or
                    len(entrypoint.encode('utf-8')) > MAX_DETAIL_BYTES):
                raise StatusReportError('invalid status report addon entrypoint')
        error = addon.get('error')
        if error is not None:
            _validate_safe_text(error, MAX_ERROR_BYTES, 'addon error')
        seen.add(identifier)


def _validate_safe_text(value, maximum, field):
    _bounded_text(value, maximum, field)
    lowered = value.lower()
    if any(marker in lowered for marker in _FORBIDDEN_DETAIL_MARKERS):
        raise StatusReportError(f'unsafe status report {field}')


def validate_status_report(report):
    """Validate a report without probing devices or changing client state."""
    if not isinstance(report, Mapping) or set(report) - _ALLOWED_FIELDS:
        raise StatusReportError('invalid status report shape')
    timestamp = report.get('at')
    if not isinstance(timestamp, str) or not _TIMESTAMP_RE.fullmatch(timestamp):
        raise StatusReportError('invalid status report timestamp')
    try:
        datetime.strptime(timestamp, '%Y-%m-%dT%H:%M:%SZ')
    except ValueError as exc:
        raise StatusReportError('invalid status report timestamp') from exc
    _validate_addons(report.get('addons'))
    _validate_capabilities(report.get('capabilities'))
    # Runtime execution is disabled. No service observation may be represented
    # as running, healthy, queued, or otherwise active by this report path.
    if report.get('services') != []:
        raise StatusReportError('client service status reporting is disabled')
    error = report.get('error')
    if error is not None:
        _validate_safe_text(error, MAX_ERROR_BYTES, 'error')
    try:
        encoded = json.dumps(report, separators=(',', ':'), ensure_ascii=False).encode('utf-8')
    except (TypeError, ValueError, UnicodeError) as exc:
        raise StatusReportError('status report is not JSON serializable') from exc
    if len(encoded) > MAX_REPORT_BYTES:
        raise StatusReportError('status report exceeds its bound')
