#!/usr/bin/env python3
"""Pure, bounded evidence for an observed STT model-load check.

This module validates a redacted, non-mutating observation only. It never loads
model data, accesses audio, starts or inspects a process/container, opens a
listener, executes a command, or reports runtime readiness.
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
MAX_MODEL_ID_BYTES: Final = 64
MAX_DIGEST_BYTES: Final = 64

MODEL_LOAD_EVIDENCE_FIELDS: Final = frozenset({
    'service_id',
    'activation_id',
    'model_id',
    'model_sha256',
    'model_file_observed',
    'load_observation_non_mutating',
    'load_identity_observed',
})

_SERVICE_ID_RE: Final = re.compile(r'^[a-z0-9][a-z0-9._-]{0,63}$')
_ACTIVATION_ID_RE: Final = re.compile(r'^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$')
_MODEL_ID_RE: Final = re.compile(r'^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$')
_SHA256_RE: Final = re.compile(r'^[0-9a-f]{64}$')
_SECRET_RE: Final = re.compile(
    r'(?:password|passwd|secret|token|credential|bearer|api[_-]?key)',
    re.IGNORECASE,
)


class STTModelLoadEvidenceError(ValueError):
    """Raised when model-load observation evidence is malformed or unsafe."""


def _fail(message: str) -> STTModelLoadEvidenceError:
    return STTModelLoadEvidenceError(message)


def _bounded_json(value: Mapping[str, Any]) -> None:
    try:
        encoded = json.dumps(
            dict(value), ensure_ascii=True, allow_nan=False, separators=(',', ':')
        ).encode('utf-8')
    except (TypeError, ValueError, UnicodeEncodeError) as exc:
        raise _fail('STT model-load evidence is not bounded JSON') from exc
    if len(encoded) > MAX_EVIDENCE_BYTES:
        raise _fail('STT model-load evidence is oversized')


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


def _model_id(value: Any, field: str = 'model_id') -> str:
    return _identifier(value, field, _MODEL_ID_RE, MAX_MODEL_ID_BYTES)


def _digest(value: Any, field: str = 'model_sha256') -> str:
    if (
        not isinstance(value, str)
        or len(value) != MAX_DIGEST_BYTES
        or _SHA256_RE.fullmatch(value) is None
        or len(set(value)) == 1
    ):
        raise _fail(f'{field} is invalid')
    return value


def _boolean(value: Any, field: str) -> bool:
    if not isinstance(value, bool):
        raise _fail(f'{field} must be a boolean')
    return value


@dataclass(frozen=True, slots=True)
class STTModelLoadEvidence:
    """Immutable model-load observations, never model activation evidence."""

    service_id: str
    activation_id: str
    model_id: str
    model_sha256: str
    model_file_observed: bool
    load_observation_non_mutating: bool
    load_identity_observed: bool
    load_observation_valid: bool
    execution_enabled: bool = False
    runtime_activation_allowed: bool = False
    model_loaded: bool = False
    inference_started: bool = False
    process_started: bool = False
    effect: str = 'metadata_only'

    def as_dict(self) -> dict[str, Any]:
        """Serialize observations and explicit disabled execution fields."""
        return {
            'plan_type': 'stt_model_load_evidence',
            'effect': 'metadata_only',
            'service_id': self.service_id,
            'activation_id': self.activation_id,
            'model_id': self.model_id,
            'model_sha256': self.model_sha256,
            'model_file_observed': self.model_file_observed,
            'load_observation_non_mutating': self.load_observation_non_mutating,
            'load_identity_observed': self.load_identity_observed,
            'load_observation_valid': self.load_observation_valid,
            'execution_enabled': False,
            'runtime_activation_allowed': False,
            'model_loaded': False,
            'inference_started': False,
            'process_started': False,
        }

    as_plan = as_dict


def validate_stt_model_load_evidence(
    evidence: Mapping[str, Any],
    *,
    expected_service_id: str | None = None,
    expected_activation_id: str | None = None,
    expected_model_id: str | None = None,
    expected_model_sha256: str | None = None,
) -> STTModelLoadEvidence:
    """Validate one closed, identity-bound, metadata-only load observation."""
    if not isinstance(evidence, Mapping):
        raise _fail('STT model-load evidence must be a mapping')
    try:
        keys = list(evidence.keys())
        if len(keys) != len(set(keys)):
            raise _fail('STT model-load evidence contains duplicate fields')
        values = dict(evidence)
    except STTModelLoadEvidenceError:
        raise
    except (TypeError, ValueError) as exc:
        raise _fail('STT model-load evidence must be a bounded mapping') from exc
    if set(values) != MODEL_LOAD_EVIDENCE_FIELDS:
        raise _fail('STT model-load evidence fields are not allowlisted')
    _bounded_json(values)

    service_id = _service_id(values['service_id'])
    activation_id = _activation_id(values['activation_id'])
    model_id = _model_id(values['model_id'])
    model_sha256 = _digest(values['model_sha256'])
    observations = {
        field: _boolean(values[field], field)
        for field in (
            'model_file_observed',
            'load_observation_non_mutating',
            'load_identity_observed',
        )
    }

    expected_values = (
        ('expected_service_id', expected_service_id, service_id, _service_id),
        ('expected_activation_id', expected_activation_id, activation_id, _activation_id),
        ('expected_model_id', expected_model_id, model_id, _model_id),
        ('expected_model_sha256', expected_model_sha256, model_sha256, _digest),
    )
    for field, expected, actual, validator in expected_values:
        if expected is not None and actual != validator(expected, field):
            raise _fail(f'{field} mismatch')

    return STTModelLoadEvidence(
        service_id=service_id,
        activation_id=activation_id,
        model_id=model_id,
        model_sha256=model_sha256,
        **observations,
        load_observation_valid=all(observations.values()),
    )


validate_model_load_evidence = validate_stt_model_load_evidence
build_model_load_evidence = validate_stt_model_load_evidence


__all__ = [
    'MAX_ACTIVATION_ID_BYTES',
    'MAX_DIGEST_BYTES',
    'MAX_EVIDENCE_BYTES',
    'MAX_MODEL_ID_BYTES',
    'MAX_SERVICE_ID_BYTES',
    'MODEL_LOAD_EVIDENCE_FIELDS',
    'STTModelLoadEvidence',
    'STTModelLoadEvidenceError',
    'build_model_load_evidence',
    'validate_model_load_evidence',
    'validate_stt_model_load_evidence',
]
