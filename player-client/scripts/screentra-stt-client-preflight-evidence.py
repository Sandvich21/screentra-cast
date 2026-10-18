#!/usr/bin/env python3
"""Pure evaluator for already-collected, redacted STT client evidence.

This is evidence evaluation only, not a live preflight or readiness claim.  It
never runs commands, opens sockets, probes hardware, reads files, contacts a
server, stages a model, or changes host state.
"""
from __future__ import annotations

import json
import re
from collections.abc import Mapping
from typing import Any, Final


MAX_EVIDENCE_BYTES: Final = 8 * 1024
MAX_HOST_ID_BYTES: Final = 64
MAX_MODEL_ID_BYTES: Final = 64
MAX_CPU_COUNT: Final = 256
MAX_RESOURCE_BYTES: Final = 1 << 60

_REQUIRED_FIELDS: Final = frozenset({
    'host_id',
    'architecture',
    'cpu_count',
    'memory_bytes',
    'free_bytes',
    'audio_capture_present',
    'pipewire_active',
    'wireplumber_active',
    'model_present',
    'local_runtime_present',
})
_OPTIONAL_FIELDS: Final = frozenset({
    'camera_present',
    'model_id',
    'model_sha256',
})
_ALLOWED_FIELDS: Final = _REQUIRED_FIELDS | _OPTIONAL_FIELDS
_ARCHITECTURES: Final = frozenset({'amd64', 'arm64'})
_LABEL_RE: Final = re.compile(r'^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$')
_SHA256_RE: Final = re.compile(r'^[0-9a-f]{64}$')
_SECRET_LIKE_RE: Final = re.compile(
    r'(?:password|passwd|secret|token|credential|bearer|api[_-]?key)',
    re.IGNORECASE,
)


class STTPreflightEvidenceError(ValueError):
    """Raised when an evidence mapping is not a bounded contract object."""


def _error(message: str) -> STTPreflightEvidenceError:
    return STTPreflightEvidenceError(message)


def _bounded_json_size(evidence: Mapping[str, Any]) -> None:
    try:
        encoded = json.dumps(
            dict(evidence),
            ensure_ascii=True,
            allow_nan=False,
            separators=(',', ':'),
        ).encode('utf-8')
    except (TypeError, ValueError, UnicodeEncodeError) as exc:
        raise _error('evidence is not bounded JSON') from exc
    if len(encoded) > MAX_EVIDENCE_BYTES:
        raise _error('evidence exceeds its bound')


def _label(value: Any, field: str, maximum: int) -> str:
    if not isinstance(value, str):
        raise _error(f'{field} must be a non-secret label')
    try:
        size = len(value.encode('ascii'))
    except UnicodeEncodeError as exc:
        raise _error(f'{field} must be a non-secret label') from exc
    if (
        size == 0
        or size > maximum
        or _LABEL_RE.fullmatch(value) is None
        or _SECRET_LIKE_RE.search(value) is not None
    ):
        raise _error(f'{field} must be a non-secret label')
    return value


def _bounded_integer(value: Any, field: str, *, minimum: int, maximum: int) -> int:
    if (
        isinstance(value, bool)
        or not isinstance(value, int)
        or value < minimum
        or value > maximum
    ):
        raise _error(f'{field} is outside its bound')
    return value


def _boolean(value: Any, field: str) -> bool:
    if not isinstance(value, bool):
        raise _error(f'{field} must be a boolean')
    return value


def _model_identity(evidence: Mapping[str, Any]) -> tuple[str | None, str | None, bool]:
    model_id = evidence.get('model_id')
    model_sha256 = evidence.get('model_sha256')
    if model_id is None and model_sha256 is None:
        return None, None, True
    if not isinstance(model_id, str) or not isinstance(model_sha256, str):
        return None, None, False
    try:
        checked_id = _label(model_id, 'model_id', MAX_MODEL_ID_BYTES)
    except STTPreflightEvidenceError:
        return None, None, False
    if _SHA256_RE.fullmatch(model_sha256) is None:
        return None, None, False
    return checked_id, model_sha256, True


def evaluate_preflight_evidence(evidence: Mapping[str, Any]) -> dict[str, Any]:
    """Evaluate bounded evidence without performing a live preflight.

    The returned flags are deliberately false even when the supplied evidence
    has no blockers.  A blocker-free result means only that this mapping had no
    listed evidence blockers; it does not establish runtime readiness.
    """
    if not isinstance(evidence, Mapping):
        raise _error('evidence must be a mapping')
    try:
        values = dict(evidence)
    except (TypeError, ValueError) as exc:
        raise _error('evidence must be a bounded mapping') from exc
    if set(values) - _ALLOWED_FIELDS:
        raise _error('evidence contains an unsupported field')
    if not _REQUIRED_FIELDS.issubset(values):
        raise _error('evidence is missing a required field')
    _bounded_json_size(values)

    host_id = _label(values['host_id'], 'host_id', MAX_HOST_ID_BYTES)
    architecture = values['architecture']
    if architecture not in _ARCHITECTURES:
        raise _error('architecture must be amd64 or arm64')
    cpu_count = _bounded_integer(
        values['cpu_count'], 'cpu_count', minimum=1, maximum=MAX_CPU_COUNT
    )
    memory_bytes = _bounded_integer(
        values['memory_bytes'],
        'memory_bytes',
        minimum=1,
        maximum=MAX_RESOURCE_BYTES,
    )
    free_bytes = _bounded_integer(
        values['free_bytes'],
        'free_bytes',
        minimum=0,
        maximum=MAX_RESOURCE_BYTES,
    )
    if free_bytes > memory_bytes:
        raise _error('free_bytes cannot exceed memory_bytes')

    audio_capture_present = _boolean(
        values['audio_capture_present'], 'audio_capture_present'
    )
    pipewire_active = _boolean(values['pipewire_active'], 'pipewire_active')
    wireplumber_active = _boolean(values['wireplumber_active'], 'wireplumber_active')
    model_present = _boolean(values['model_present'], 'model_present')
    local_runtime_present = _boolean(
        values['local_runtime_present'], 'local_runtime_present'
    )
    camera_present = None
    if 'camera_present' in values:
        camera_present = _boolean(values['camera_present'], 'camera_present')

    model_id, model_sha256, identity_valid = _model_identity(values)
    if model_present and (model_id is None or model_sha256 is None):
        identity_valid = False
    if not model_present and (model_id is not None or model_sha256 is not None):
        identity_valid = False
    if not identity_valid:
        model_id = None
        model_sha256 = None

    blockers: list[str] = []
    if not audio_capture_present:
        blockers.append('audio_capture_missing')
    if not pipewire_active or not wireplumber_active:
        blockers.append('audio_service_inactive')
    if not model_present:
        blockers.append('model_missing')
    if not identity_valid:
        blockers.append('model_identity_invalid')
    if not local_runtime_present:
        blockers.append('local_runtime_missing')

    warnings: list[str] = []
    if 'camera_present' not in values:
        warnings.append('camera_evidence_unavailable')

    output_evidence: dict[str, Any] = {
        'host_id': host_id,
        'architecture': architecture,
        'cpu_count': cpu_count,
        'memory_bytes': memory_bytes,
        'free_bytes': free_bytes,
        'audio_capture_present': audio_capture_present,
        'pipewire_active': pipewire_active,
        'wireplumber_active': wireplumber_active,
        'model_present': model_present,
        'model_id': model_id,
        'model_sha256': model_sha256,
        'local_runtime_present': local_runtime_present,
    }
    if camera_present is not None:
        output_evidence['camera_present'] = camera_present

    return {
        'evidence_type': 'stt_client_preflight_evidence',
        'evaluation': 'evidence_only',
        'live_preflight': False,
        'readiness_claim': False,
        'evidence': output_evidence,
        'blockers': blockers,
        'warnings': warnings,
        'execution_enabled': False,
        'runtime_ready': False,
        'hardware_accessed': False,
        'model_staged': False,
    }


validate_preflight_evidence = evaluate_preflight_evidence


__all__ = [
    'MAX_CPU_COUNT',
    'MAX_EVIDENCE_BYTES',
    'MAX_HOST_ID_BYTES',
    'MAX_MODEL_ID_BYTES',
    'MAX_RESOURCE_BYTES',
    'STTPreflightEvidenceError',
    'evaluate_preflight_evidence',
    'validate_preflight_evidence',
]
