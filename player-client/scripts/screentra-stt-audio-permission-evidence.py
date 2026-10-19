#!/usr/bin/env python3
"""Pure, bounded evidence for future STT audio-permission review.

This module validates already-observed permission metadata only.  It never
activates permission, accesses a device, captures audio, binds a listener,
starts a service, executes a command, reads credentials, or reports readiness.
"""
from __future__ import annotations

import json
import re
from collections.abc import Mapping
from dataclasses import dataclass
from typing import Any, Final


MAX_EVIDENCE_BYTES: Final = 4 * 1024
MAX_SERVICE_ID_BYTES: Final = 64
MAX_ACTIVATION_ID_BYTES: Final = 128
MAX_HOST_ID_BYTES: Final = 64

AUDIO_BACKENDS: Final = frozenset({'pipewire'})
PERMISSION_SOURCES: Final = frozenset({'group_membership', 'user_service'})
AUDIO_PERMISSION_FIELDS: Final = frozenset({
    'service_id',
    'activation_id',
    'host_id',
    'audio_backend',
    'permission_source',
    'permission_observed',
    'capture_device_observed',
    'user_service_observed',
    'non_capture_check',
})

_SERVICE_ID_RE: Final = re.compile(r'^[a-z0-9][a-z0-9._-]{0,63}$')
_ACTIVATION_ID_RE: Final = re.compile(r'^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$')
_HOST_ID_RE: Final = re.compile(r'^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$')
_SECRET_RE: Final = re.compile(
    r'(?:password|passwd|secret|token|credential|bearer|api[_-]?key)',
    re.IGNORECASE,
)


class STTAudioPermissionEvidenceError(ValueError):
    """Raised when audio-permission evidence is malformed or unsafe."""


def _fail(message: str) -> STTAudioPermissionEvidenceError:
    return STTAudioPermissionEvidenceError(message)


def _bounded_json(value: Mapping[str, Any]) -> None:
    try:
        encoded = json.dumps(
            dict(value), ensure_ascii=True, allow_nan=False, separators=(',', ':')
        ).encode('utf-8')
    except (TypeError, ValueError, UnicodeEncodeError) as exc:
        raise _fail('STT audio-permission evidence is not bounded JSON') from exc
    if len(encoded) > MAX_EVIDENCE_BYTES:
        raise _fail('STT audio-permission evidence is oversized')


def _identifier(
    value: Any,
    field: str,
    pattern: re.Pattern[str],
    maximum: int,
) -> str:
    if (
        not isinstance(value, str)
        or not value
        or len(value.encode('ascii', errors='ignore')) != len(value)
        or len(value.encode('utf-8')) > maximum
        or pattern.fullmatch(value) is None
        or _SECRET_RE.search(value) is not None
    ):
        raise _fail(f'{field} is invalid')
    return value


def _service_id(value: Any, field: str = 'service_id') -> str:
    return _identifier(value, field, _SERVICE_ID_RE, MAX_SERVICE_ID_BYTES)


def _activation_id(value: Any, field: str = 'activation_id') -> str:
    return _identifier(value, field, _ACTIVATION_ID_RE, MAX_ACTIVATION_ID_BYTES)


def _host_id(value: Any, field: str = 'host_id') -> str:
    return _identifier(value, field, _HOST_ID_RE, MAX_HOST_ID_BYTES)


def _boolean(value: Any, field: str) -> bool:
    if not isinstance(value, bool):
        raise _fail(f'{field} must be a boolean')
    return value


@dataclass(frozen=True, slots=True)
class STTAudioPermissionEvidence:
    """Immutable permission evidence, never permission activation or capture."""

    service_id: str
    activation_id: str
    host_id: str
    audio_backend: str
    permission_source: str
    permission_observed: bool
    capture_device_observed: bool
    user_service_observed: bool
    non_capture_check: bool
    policy_valid: bool
    execution_enabled: bool = False
    runtime_activation_allowed: bool = False
    hardware_permission_activated: bool = False
    microphone_accessed: bool = False
    audio_captured: bool = False
    listener_bound: bool = False
    effect: str = 'metadata_only'

    def as_dict(self) -> dict[str, Any]:
        """Serialize only bounded permission metadata and inert status flags."""
        return {
            'plan_type': 'stt_audio_permission_evidence',
            'effect': 'metadata_only',
            'service_id': self.service_id,
            'activation_id': self.activation_id,
            'host_id': self.host_id,
            'audio_backend': self.audio_backend,
            'permission_source': self.permission_source,
            'permission_observed': self.permission_observed,
            'capture_device_observed': self.capture_device_observed,
            'user_service_observed': self.user_service_observed,
            'non_capture_check': self.non_capture_check,
            'policy_valid': self.policy_valid,
            'execution_enabled': False,
            'runtime_activation_allowed': False,
            'hardware_permission_activated': False,
            'microphone_accessed': False,
            'audio_captured': False,
            'listener_bound': False,
        }

    as_plan = as_dict


def validate_stt_audio_permission_evidence(
    evidence: Mapping[str, Any],
    *,
    expected_service_id: str | None = None,
    expected_activation_id: str | None = None,
    expected_host_id: str | None = None,
) -> STTAudioPermissionEvidence:
    """Validate one closed, identity-bound, metadata-only permission record."""
    if not isinstance(evidence, Mapping):
        raise _fail('STT audio-permission evidence must be a mapping')
    try:
        keys = list(evidence.keys())
        if len(keys) != len(set(keys)):
            raise _fail('STT audio-permission evidence contains duplicate fields')
        values = dict(evidence)
    except STTAudioPermissionEvidenceError:
        raise
    except (TypeError, ValueError) as exc:
        raise _fail('STT audio-permission evidence must be a bounded mapping') from exc
    if set(values) != AUDIO_PERMISSION_FIELDS:
        raise _fail('STT audio-permission evidence fields are not allowlisted')
    _bounded_json(values)

    service_id = _service_id(values['service_id'])
    activation_id = _activation_id(values['activation_id'])
    host_id = _host_id(values['host_id'])
    if expected_service_id is not None and service_id != _service_id(expected_service_id, 'expected_service_id'):
        raise _fail('service identity mismatch')
    if expected_activation_id is not None and activation_id != _activation_id(expected_activation_id, 'expected_activation_id'):
        raise _fail('activation identity mismatch')
    if expected_host_id is not None and host_id != _host_id(expected_host_id, 'expected_host_id'):
        raise _fail('host identity mismatch')

    audio_backend = values['audio_backend']
    if not isinstance(audio_backend, str) or audio_backend not in AUDIO_BACKENDS:
        raise _fail('audio_backend is not allowlisted')
    permission_source = values['permission_source']
    if not isinstance(permission_source, str) or permission_source not in PERMISSION_SOURCES:
        raise _fail('permission_source is not allowlisted')
    permission_observed = _boolean(values['permission_observed'], 'permission_observed')
    capture_device_observed = _boolean(
        values['capture_device_observed'], 'capture_device_observed'
    )
    user_service_observed = _boolean(
        values['user_service_observed'], 'user_service_observed'
    )
    non_capture_check = _boolean(values['non_capture_check'], 'non_capture_check')
    policy_valid = all(
        (
            permission_observed,
            capture_device_observed,
            user_service_observed,
            non_capture_check,
        )
    )

    return STTAudioPermissionEvidence(
        service_id=service_id,
        activation_id=activation_id,
        host_id=host_id,
        audio_backend=audio_backend,
        permission_source=permission_source,
        permission_observed=permission_observed,
        capture_device_observed=capture_device_observed,
        user_service_observed=user_service_observed,
        non_capture_check=non_capture_check,
        policy_valid=policy_valid,
    )


validate_audio_permission_evidence = validate_stt_audio_permission_evidence
build_audio_permission_evidence = validate_stt_audio_permission_evidence
validate_stt_audio_permission = validate_stt_audio_permission_evidence


__all__ = [
    'AUDIO_BACKENDS',
    'AUDIO_PERMISSION_FIELDS',
    'MAX_ACTIVATION_ID_BYTES',
    'MAX_EVIDENCE_BYTES',
    'MAX_HOST_ID_BYTES',
    'MAX_SERVICE_ID_BYTES',
    'PERMISSION_SOURCES',
    'STTAudioPermissionEvidence',
    'STTAudioPermissionEvidenceError',
    'build_audio_permission_evidence',
    'validate_audio_permission_evidence',
    'validate_stt_audio_permission',
    'validate_stt_audio_permission_evidence',
]
