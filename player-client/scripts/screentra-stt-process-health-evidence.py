#!/usr/bin/env python3
"""Pure, bounded evidence for an observed STT service-health check.

This module validates a redacted, non-mutating observation only. It never starts
or inspects a process/container, opens a listener, executes a command, accesses
audio or model data, or reports runtime readiness.
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
MAX_VERSION_BYTES: Final = 64
HEALTH_PROTOCOL: Final = 'local_ai_health_v1'

PROCESS_HEALTH_EVIDENCE_FIELDS: Final = frozenset({
    'service_id',
    'activation_id',
    'service_version',
    'health_protocol',
    'process_observed',
    'health_response_observed',
    'identity_observed',
    'health_observation_non_mutating',
})

_SERVICE_ID_RE: Final = re.compile(r'^[a-z0-9][a-z0-9._-]{0,63}$')
_ACTIVATION_ID_RE: Final = re.compile(r'^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$')
_VERSION_RE: Final = re.compile(
    r'^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)'
    r'(?:-[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?$'
)
_SECRET_RE: Final = re.compile(
    r'(?:password|passwd|secret|token|credential|bearer|api[_-]?key)',
    re.IGNORECASE,
)


class STTProcessHealthEvidenceError(ValueError):
    """Raised when process-health observation evidence is malformed or unsafe."""


def _fail(message: str) -> STTProcessHealthEvidenceError:
    return STTProcessHealthEvidenceError(message)


def _bounded_json(value: Mapping[str, Any]) -> None:
    try:
        encoded = json.dumps(
            dict(value), ensure_ascii=True, allow_nan=False, separators=(',', ':')
        ).encode('utf-8')
    except (TypeError, ValueError, UnicodeEncodeError) as exc:
        raise _fail('STT process-health evidence is not bounded JSON') from exc
    if len(encoded) > MAX_EVIDENCE_BYTES:
        raise _fail('STT process-health evidence is oversized')


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


def _version(value: Any, field: str = 'service_version') -> str:
    if (
        not isinstance(value, str)
        or not value
        or len(value.encode('ascii', errors='ignore')) != len(value)
        or len(value.encode('utf-8')) > MAX_VERSION_BYTES
        or _VERSION_RE.fullmatch(value) is None
    ):
        raise _fail(f'{field} is invalid')
    return value


def _boolean(value: Any, field: str) -> bool:
    if not isinstance(value, bool):
        raise _fail(f'{field} must be a boolean')
    return value


@dataclass(frozen=True, slots=True)
class STTProcessHealthEvidence:
    """Immutable service-health observations, never lifecycle evidence."""

    service_id: str
    activation_id: str
    service_version: str
    health_protocol: str
    process_observed: bool
    health_response_observed: bool
    identity_observed: bool
    health_observation_non_mutating: bool
    health_observation_valid: bool
    execution_enabled: bool = False
    runtime_activation_allowed: bool = False
    process_started: bool = False
    service_healthy: bool = False
    health_observed: bool = False
    effect: str = 'metadata_only'

    def as_dict(self) -> dict[str, Any]:
        """Serialize observations and explicit disabled execution fields."""
        return {
            'plan_type': 'stt_process_health_evidence',
            'effect': 'metadata_only',
            'service_id': self.service_id,
            'activation_id': self.activation_id,
            'service_version': self.service_version,
            'health_protocol': self.health_protocol,
            'process_observed': self.process_observed,
            'health_response_observed': self.health_response_observed,
            'identity_observed': self.identity_observed,
            'health_observation_non_mutating': self.health_observation_non_mutating,
            'health_observation_valid': self.health_observation_valid,
            'execution_enabled': False,
            'runtime_activation_allowed': False,
            'process_started': False,
            'service_healthy': False,
            'health_observed': False,
        }

    as_plan = as_dict


def validate_stt_process_health_evidence(
    evidence: Mapping[str, Any],
    *,
    expected_service_id: str | None = None,
    expected_activation_id: str | None = None,
    expected_service_version: str | None = None,
) -> STTProcessHealthEvidence:
    """Validate one closed, identity-bound, metadata-only health observation."""
    if not isinstance(evidence, Mapping):
        raise _fail('STT process-health evidence must be a mapping')
    try:
        keys = list(evidence.keys())
        if len(keys) != len(set(keys)):
            raise _fail('STT process-health evidence contains duplicate fields')
        values = dict(evidence)
    except STTProcessHealthEvidenceError:
        raise
    except (TypeError, ValueError) as exc:
        raise _fail('STT process-health evidence must be a bounded mapping') from exc
    if set(values) != PROCESS_HEALTH_EVIDENCE_FIELDS:
        raise _fail('STT process-health evidence fields are not allowlisted')
    _bounded_json(values)

    service_id = _service_id(values['service_id'])
    activation_id = _activation_id(values['activation_id'])
    service_version = _version(values['service_version'])
    if values['health_protocol'] != HEALTH_PROTOCOL:
        raise _fail('health_protocol is not the supported local health protocol')

    observations = {
        field: _boolean(values[field], field)
        for field in (
            'process_observed',
            'health_response_observed',
            'identity_observed',
            'health_observation_non_mutating',
        )
    }

    expected_values = (
        ('expected_service_id', expected_service_id, service_id, _service_id),
        ('expected_activation_id', expected_activation_id, activation_id, _activation_id),
        ('expected_service_version', expected_service_version, service_version, _version),
    )
    for field, expected, actual, validator in expected_values:
        if expected is not None and actual != validator(expected, field):
            raise _fail(f'{field} mismatch')

    return STTProcessHealthEvidence(
        service_id=service_id,
        activation_id=activation_id,
        service_version=service_version,
        health_protocol=HEALTH_PROTOCOL,
        **observations,
        health_observation_valid=all(observations.values()),
    )


validate_process_health_evidence = validate_stt_process_health_evidence
build_process_health_evidence = validate_stt_process_health_evidence


__all__ = [
    'HEALTH_PROTOCOL',
    'MAX_ACTIVATION_ID_BYTES',
    'MAX_EVIDENCE_BYTES',
    'MAX_SERVICE_ID_BYTES',
    'MAX_VERSION_BYTES',
    'PROCESS_HEALTH_EVIDENCE_FIELDS',
    'STTProcessHealthEvidence',
    'STTProcessHealthEvidenceError',
    'build_process_health_evidence',
    'validate_process_health_evidence',
    'validate_stt_process_health_evidence',
]
