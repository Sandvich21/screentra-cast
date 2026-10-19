#!/usr/bin/env python3
"""Pure, bounded evidence for observing a non-running STT lifecycle state.

This module validates an immutable, identity-bound observation only. It never
transitions lifecycle state, starts or inspects a process/container, opens a
socket, executes a command, accesses credentials, audio, or model bytes, or
reports readiness or activation.
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

LIFECYCLE_OBSERVATION_EVIDENCE_FIELDS: Final = frozenset({
    'service_id',
    'activation_id',
    'service_version',
    'observed_state',
    'identity_observed',
    'observation_non_mutating',
})
ALLOWLISTED_NON_RUNNING_STATES: Final = frozenset({
    'staged',
    'stopped',
    'rollback_ready',
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


class STTLifecycleObservationEvidenceError(ValueError):
    """Raised when lifecycle observation evidence is malformed or unsafe."""


def _fail(message: str) -> STTLifecycleObservationEvidenceError:
    return STTLifecycleObservationEvidenceError(message)


def _bounded_json(value: Mapping[str, Any]) -> None:
    try:
        encoded = json.dumps(
            dict(value), ensure_ascii=True, allow_nan=False, separators=(',', ':')
        ).encode('utf-8')
    except (TypeError, ValueError, UnicodeEncodeError) as exc:
        raise _fail('STT lifecycle observation evidence is not bounded JSON') from exc
    if len(encoded) > MAX_EVIDENCE_BYTES:
        raise _fail('STT lifecycle observation evidence is oversized')


def _identifier(value: Any, field: str, pattern: re.Pattern[str], maximum: int) -> str:
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
class STTLifecycleObservationEvidence:
    """Immutable non-running lifecycle observation, never a transition command."""

    service_id: str
    activation_id: str
    service_version: str
    observed_state: str
    identity_observed: bool
    observation_non_mutating: bool
    observation_valid: bool
    execution_enabled: bool = False
    runtime_activation_allowed: bool = False
    lifecycle_changed: bool = False
    process_started: bool = False
    service_started: bool = False
    effect: str = 'metadata_only'

    def as_dict(self) -> dict[str, Any]:
        """Serialize the fixed observation and explicit disabled flags."""
        return {
            'evidence_type': 'stt_lifecycle_observation_evidence',
            'service_id': self.service_id,
            'activation_id': self.activation_id,
            'service_version': self.service_version,
            'observed_state': self.observed_state,
            'identity_observed': self.identity_observed,
            'observation_non_mutating': self.observation_non_mutating,
            'observation_valid': self.observation_valid,
            'execution_enabled': False,
            'runtime_activation_allowed': False,
            'lifecycle_changed': False,
            'process_started': False,
            'service_started': False,
            'effect': 'metadata_only',
        }

    as_plan = as_dict


def validate_stt_lifecycle_observation_evidence(
    evidence: Mapping[str, Any],
    *,
    expected_service_id: str | None = None,
    expected_activation_id: str | None = None,
    expected_service_version: str | None = None,
) -> STTLifecycleObservationEvidence:
    """Validate one closed, identity-bound, metadata-only observation."""
    if not isinstance(evidence, Mapping):
        raise _fail('STT lifecycle observation evidence must be a mapping')
    try:
        keys = list(evidence.keys())
        if len(keys) != len(set(keys)):
            raise _fail('STT lifecycle observation evidence contains duplicate fields')
        values = dict(evidence)
    except STTLifecycleObservationEvidenceError:
        raise
    except (TypeError, ValueError) as exc:
        raise _fail('STT lifecycle observation evidence must be a bounded mapping') from exc
    if set(values) != LIFECYCLE_OBSERVATION_EVIDENCE_FIELDS:
        raise _fail('STT lifecycle observation evidence fields are not allowlisted')
    _bounded_json(values)

    service_id = _service_id(values['service_id'])
    activation_id = _activation_id(values['activation_id'])
    service_version = _version(values['service_version'])
    observed_state = values['observed_state']
    if (
        not isinstance(observed_state, str)
        or observed_state not in ALLOWLISTED_NON_RUNNING_STATES
    ):
        raise _fail('observed_state is not an allowlisted non-running state')
    identity_observed = _boolean(values['identity_observed'], 'identity_observed')
    observation_non_mutating = _boolean(
        values['observation_non_mutating'], 'observation_non_mutating'
    )

    expected_values = (
        ('expected_service_id', expected_service_id, service_id, _service_id),
        ('expected_activation_id', expected_activation_id, activation_id, _activation_id),
        ('expected_service_version', expected_service_version, service_version, _version),
    )
    for field, expected, actual, validator in expected_values:
        if expected is not None and actual != validator(expected, field):
            raise _fail(f'{field} mismatch')

    return STTLifecycleObservationEvidence(
        service_id=service_id,
        activation_id=activation_id,
        service_version=service_version,
        observed_state=observed_state,
        identity_observed=identity_observed,
        observation_non_mutating=observation_non_mutating,
        observation_valid=identity_observed and observation_non_mutating,
    )


validate_lifecycle_observation_evidence = validate_stt_lifecycle_observation_evidence
build_lifecycle_observation_evidence = validate_stt_lifecycle_observation_evidence


__all__ = [
    'ALLOWLISTED_NON_RUNNING_STATES',
    'LIFECYCLE_OBSERVATION_EVIDENCE_FIELDS',
    'MAX_ACTIVATION_ID_BYTES',
    'MAX_EVIDENCE_BYTES',
    'MAX_SERVICE_ID_BYTES',
    'MAX_VERSION_BYTES',
    'STTLifecycleObservationEvidence',
    'STTLifecycleObservationEvidenceError',
    'build_lifecycle_observation_evidence',
    'validate_lifecycle_observation_evidence',
    'validate_stt_lifecycle_observation_evidence',
]
