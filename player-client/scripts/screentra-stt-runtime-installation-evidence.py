#!/usr/bin/env python3
"""Pure, bounded evidence for an inert STT runtime/model installation.

This module validates already-recorded artifact identity and a non-starting
installation check.  It performs no installation, process, model, audio,
listener, network, credential, or lifecycle operation and never reports
runtime readiness.
"""
from __future__ import annotations

import json
import re
from collections.abc import Mapping
from dataclasses import dataclass
from typing import Any, Final


MAX_EVIDENCE_BYTES: Final = 8 * 1024
MAX_SERVICE_ID_BYTES: Final = 64
MAX_ACTIVATION_ID_BYTES: Final = 128
MAX_SERVICE_VERSION_BYTES: Final = 64
MAX_RUNTIME_VERSION_BYTES: Final = 64
MAX_SOURCE_COMMIT_BYTES: Final = 40
MAX_MODEL_ID_BYTES: Final = 64
MAX_DIGEST_BYTES: Final = 64

RUNTIME_FAMILY: Final = 'whisper.cpp'

RUNTIME_INSTALLATION_EVIDENCE_FIELDS: Final = frozenset({
    'service_id',
    'activation_id',
    'service_version',
    'runtime_family',
    'runtime_version',
    'source_commit',
    'runtime_sha256',
    'model_id',
    'model_sha256',
    'runtime_verified',
    'model_verified',
    'non_starting_check',
})

_SERVICE_ID_RE: Final = re.compile(r'^[a-z0-9][a-z0-9._-]{0,63}$')
_ACTIVATION_ID_RE: Final = re.compile(r'^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$')
_SEMVER_RE: Final = re.compile(
    r'^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)'
    r'(?:-[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?$'
)
_SOURCE_COMMIT_RE: Final = re.compile(r'^[0-9a-f]{40}$')
_SHA256_RE: Final = re.compile(r'^[0-9a-f]{64}$')
_MODEL_ID_RE: Final = re.compile(r'^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$')
_SECRET_RE: Final = re.compile(
    r'(?:password|passwd|secret|token|credential|bearer|api[_-]?key)',
    re.IGNORECASE,
)


class STTRuntimeInstallationEvidenceError(ValueError):
    """Raised when installation evidence is malformed or unsafe."""


def _fail(message: str) -> STTRuntimeInstallationEvidenceError:
    return STTRuntimeInstallationEvidenceError(message)


def _bounded_json(value: Mapping[str, Any]) -> None:
    try:
        encoded = json.dumps(
            dict(value), ensure_ascii=True, allow_nan=False, separators=(',', ':')
        ).encode('utf-8')
    except (TypeError, ValueError, UnicodeEncodeError) as exc:
        raise _fail('STT runtime installation evidence is not bounded JSON') from exc
    if len(encoded) > MAX_EVIDENCE_BYTES:
        raise _fail('STT runtime installation evidence is oversized')


def _text(
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
    return _text(value, field, _SERVICE_ID_RE, MAX_SERVICE_ID_BYTES)


def _activation_id(value: Any, field: str = 'activation_id') -> str:
    return _text(value, field, _ACTIVATION_ID_RE, MAX_ACTIVATION_ID_BYTES)


def _service_version(value: Any, field: str = 'service_version') -> str:
    return _text(value, field, _SEMVER_RE, MAX_SERVICE_VERSION_BYTES)


def _runtime_version(value: Any) -> str:
    return _text(value, 'runtime_version', _SEMVER_RE, MAX_RUNTIME_VERSION_BYTES)


def _source_commit(value: Any) -> str:
    return _text(value, 'source_commit', _SOURCE_COMMIT_RE, MAX_SOURCE_COMMIT_BYTES)


def _model_id(value: Any, field: str = 'model_id') -> str:
    return _text(value, field, _MODEL_ID_RE, MAX_MODEL_ID_BYTES)


def _digest(value: Any, field: str) -> str:
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
class STTRuntimeInstallationEvidence:
    """Immutable installation metadata, never runtime readiness evidence."""

    service_id: str
    activation_id: str
    service_version: str
    runtime_family: str
    runtime_version: str
    source_commit: str
    runtime_sha256: str
    model_id: str
    model_sha256: str
    runtime_verified: bool
    model_verified: bool
    non_starting_check: bool
    policy_valid: bool
    artifacts_verified: bool
    execution_enabled: bool = False
    runtime_activation_allowed: bool = False
    process_started: bool = False
    model_loaded: bool = False
    service_started: bool = False
    effect: str = 'metadata_only'

    def as_dict(self) -> dict[str, Any]:
        """Serialize the closed installation-evidence result."""
        return {
            'plan_type': 'stt_runtime_installation_evidence',
            'effect': 'metadata_only',
            'service_id': self.service_id,
            'activation_id': self.activation_id,
            'service_version': self.service_version,
            'runtime_family': self.runtime_family,
            'runtime_version': self.runtime_version,
            'source_commit': self.source_commit,
            'runtime_sha256': self.runtime_sha256,
            'model_id': self.model_id,
            'model_sha256': self.model_sha256,
            'runtime_verified': self.runtime_verified,
            'model_verified': self.model_verified,
            'non_starting_check': self.non_starting_check,
            'policy_valid': self.policy_valid,
            'artifacts_verified': self.artifacts_verified,
            'execution_enabled': False,
            'runtime_activation_allowed': False,
            'process_started': False,
            'model_loaded': False,
            'service_started': False,
        }

    as_plan = as_dict


def validate_stt_runtime_installation_evidence(
    evidence: Mapping[str, Any],
    *,
    expected_service_id: str | None = None,
    expected_activation_id: str | None = None,
    expected_service_version: str | None = None,
    expected_model_id: str | None = None,
    expected_model_sha256: str | None = None,
) -> STTRuntimeInstallationEvidence:
    """Validate one closed, identity-bound, non-starting installation record."""
    if not isinstance(evidence, Mapping):
        raise _fail('STT runtime installation evidence must be a mapping')
    try:
        keys = list(evidence.keys())
        if len(keys) != len(set(keys)):
            raise _fail('STT runtime installation evidence contains duplicate fields')
        values = dict(evidence)
    except STTRuntimeInstallationEvidenceError:
        raise
    except (TypeError, ValueError) as exc:
        raise _fail('STT runtime installation evidence must be a bounded mapping') from exc
    if set(values) != RUNTIME_INSTALLATION_EVIDENCE_FIELDS:
        raise _fail('STT runtime installation evidence fields are not allowlisted')
    _bounded_json(values)

    service_id = _service_id(values['service_id'])
    activation_id = _activation_id(values['activation_id'])
    service_version = _service_version(values['service_version'])
    runtime_family = values['runtime_family']
    if runtime_family != RUNTIME_FAMILY:
        raise _fail('runtime_family is not allowlisted')
    runtime_version = _runtime_version(values['runtime_version'])
    source_commit = _source_commit(values['source_commit'])
    runtime_sha256 = _digest(values['runtime_sha256'], 'runtime_sha256')
    model_id = _model_id(values['model_id'])
    model_sha256 = _digest(values['model_sha256'], 'model_sha256')
    runtime_verified = _boolean(values['runtime_verified'], 'runtime_verified')
    model_verified = _boolean(values['model_verified'], 'model_verified')
    non_starting_check = _boolean(values['non_starting_check'], 'non_starting_check')

    expected_values = (
        ('expected_service_id', expected_service_id, service_id, _service_id),
        ('expected_activation_id', expected_activation_id, activation_id, _activation_id),
        ('expected_service_version', expected_service_version, service_version, _service_version),
        ('expected_model_id', expected_model_id, model_id, _model_id),
        (
            'expected_model_sha256', expected_model_sha256, model_sha256,
            lambda value, field: _digest(value, field),
        ),
    )
    for field, expected, actual, validator in expected_values:
        if expected is not None and actual != validator(expected, field):
            raise _fail(f'{field} mismatch')

    artifacts_verified = runtime_verified and model_verified and non_starting_check
    return STTRuntimeInstallationEvidence(
        service_id=service_id,
        activation_id=activation_id,
        service_version=service_version,
        runtime_family=runtime_family,
        runtime_version=runtime_version,
        source_commit=source_commit,
        runtime_sha256=runtime_sha256,
        model_id=model_id,
        model_sha256=model_sha256,
        runtime_verified=runtime_verified,
        model_verified=model_verified,
        non_starting_check=non_starting_check,
        policy_valid=artifacts_verified,
        artifacts_verified=artifacts_verified,
    )


validate_runtime_installation_evidence = validate_stt_runtime_installation_evidence
build_runtime_installation_evidence = validate_stt_runtime_installation_evidence


__all__ = [
    'MAX_ACTIVATION_ID_BYTES',
    'MAX_EVIDENCE_BYTES',
    'MAX_MODEL_ID_BYTES',
    'MAX_RUNTIME_VERSION_BYTES',
    'MAX_SERVICE_ID_BYTES',
    'MAX_SERVICE_VERSION_BYTES',
    'RUNTIME_FAMILY',
    'RUNTIME_INSTALLATION_EVIDENCE_FIELDS',
    'STTRuntimeInstallationEvidence',
    'STTRuntimeInstallationEvidenceError',
    'build_runtime_installation_evidence',
    'validate_runtime_installation_evidence',
    'validate_stt_runtime_installation_evidence',
]
