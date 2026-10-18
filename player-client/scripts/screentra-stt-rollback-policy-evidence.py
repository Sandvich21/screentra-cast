#!/usr/bin/env python3
"""Pure, inert evidence contract for bounded STT rollback policy.

This module validates rollback-policy metadata only. It does not execute a
rollback, start a process, change state, access credentials, use paths or URLs,
open a listener, access audio/model data, or report runtime readiness.
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
MAX_SERVICE_VERSION_BYTES: Final = 64
MAX_POLICY_VERSION_BYTES: Final = 64
RETENTION_COUNT: Final = 1

ROLLBACK_POLICY_FIELDS: Final = frozenset({
    'service_id',
    'activation_id',
    'service_version',
    'retention_count',
    'rollback_artifact_verified',
    'rollback_identity_bound',
    'atomic_swap_supported',
    'state_preservation_supported',
    'cleanup_supported',
    'policy_version',
})
_ROLLBACK_CONTROLS: Final = (
    'rollback_artifact_verified',
    'rollback_identity_bound',
    'atomic_swap_supported',
    'state_preservation_supported',
    'cleanup_supported',
)

_SERVICE_ID_RE: Final = re.compile(r'^[a-z0-9][a-z0-9._-]{0,63}$')
_ACTIVATION_ID_RE: Final = re.compile(r'^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$')
_SERVICE_VERSION_RE: Final = re.compile(
    r'^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)'
    r'(?:-[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?$'
)
_POLICY_VERSION_RE: Final = re.compile(r'^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$')
_SECRET_RE: Final = re.compile(
    r'(?:password|passwd|secret|token|credential|bearer|api[_-]?key)',
    re.IGNORECASE,
)


class STTRollbackPolicyEvidenceError(ValueError):
    """Raised when rollback-policy metadata is malformed or unsafe."""


def _fail(message: str) -> STTRollbackPolicyEvidenceError:
    return STTRollbackPolicyEvidenceError(message)


def _bounded_json(value: Mapping[str, Any]) -> None:
    try:
        encoded = json.dumps(
            dict(value), ensure_ascii=True, allow_nan=False, separators=(',', ':')
        ).encode('utf-8')
    except (TypeError, ValueError, UnicodeEncodeError) as exc:
        raise _fail('STT rollback-policy evidence is not bounded JSON') from exc
    if len(encoded) > MAX_EVIDENCE_BYTES:
        raise _fail('STT rollback-policy evidence is oversized')


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


def _service_version(value: Any, field: str = 'service_version') -> str:
    return _identifier(value, field, _SERVICE_VERSION_RE, MAX_SERVICE_VERSION_BYTES)


def _policy_version(value: Any) -> str:
    return _identifier(value, 'policy_version', _POLICY_VERSION_RE, MAX_POLICY_VERSION_BYTES)


def _strict_boolean(value: Any, field: str) -> bool:
    if not isinstance(value, bool):
        raise _fail(f'{field} must be a boolean')
    return value


@dataclass(frozen=True, slots=True)
class STTRollbackPolicyEvidence:
    """Immutable rollback-policy metadata, never rollback or runtime state."""

    service_id: str
    activation_id: str
    service_version: str
    retention_count: int
    rollback_artifact_verified: bool
    rollback_identity_bound: bool
    atomic_swap_supported: bool
    state_preservation_supported: bool
    cleanup_supported: bool
    policy_version: str
    policy_valid: bool
    execution_enabled: bool = False
    runtime_activation_allowed: bool = False
    rollback_executed: bool = False
    process_started: bool = False
    state_changed: bool = False
    effect: str = 'metadata_only'

    def as_dict(self) -> dict[str, Any]:
        """Serialize only bounded policy metadata and inert status flags."""
        return {
            'plan_type': 'stt_client_service_rollback_policy_evidence',
            'effect': 'metadata_only',
            'policy_valid': self.policy_valid,
            'service_id': self.service_id,
            'activation_id': self.activation_id,
            'service_version': self.service_version,
            'retention_count': self.retention_count,
            'rollback_artifact_verified': self.rollback_artifact_verified,
            'rollback_identity_bound': self.rollback_identity_bound,
            'atomic_swap_supported': self.atomic_swap_supported,
            'state_preservation_supported': self.state_preservation_supported,
            'cleanup_supported': self.cleanup_supported,
            'policy_version': self.policy_version,
            'execution_enabled': False,
            'runtime_activation_allowed': False,
            'rollback_executed': False,
            'process_started': False,
            'state_changed': False,
        }

    as_plan = as_dict


def validate_stt_rollback_policy_evidence(
    evidence: Mapping[str, Any],
    *,
    expected_service_id: str | None = None,
    expected_activation_id: str | None = None,
    expected_service_version: str | None = None,
    expected_policy_version: str | None = None,
) -> STTRollbackPolicyEvidence:
    """Validate one closed, identity-bound, metadata-only rollback policy."""
    if not isinstance(evidence, Mapping):
        raise _fail('STT rollback-policy evidence must be a mapping')
    try:
        keys = list(evidence.keys())
        if len(keys) != len(set(keys)):
            raise _fail('STT rollback-policy evidence contains duplicate fields')
        values = dict(evidence)
    except STTRollbackPolicyEvidenceError:
        raise
    except (TypeError, ValueError) as exc:
        raise _fail('STT rollback-policy evidence must be a bounded mapping') from exc
    if set(values) != ROLLBACK_POLICY_FIELDS:
        raise _fail('STT rollback-policy evidence fields are not allowlisted')
    _bounded_json(values)

    service_id = _service_id(values['service_id'])
    activation_id = _activation_id(values['activation_id'])
    service_version = _service_version(values['service_version'])
    policy_version = _policy_version(values['policy_version'])
    if expected_service_id is not None and service_id != _service_id(expected_service_id, 'expected_service_id'):
        raise _fail('service identity mismatch')
    if expected_activation_id is not None and activation_id != _activation_id(expected_activation_id, 'expected_activation_id'):
        raise _fail('activation identity mismatch')
    if expected_service_version is not None and service_version != _service_version(expected_service_version, 'expected_service_version'):
        raise _fail('service version mismatch')
    if expected_policy_version is not None and policy_version != _policy_version(expected_policy_version):
        raise _fail('policy version mismatch')

    retention_count = values['retention_count']
    if (
        isinstance(retention_count, bool)
        or not isinstance(retention_count, int)
        or retention_count != RETENTION_COUNT
    ):
        raise _fail('retention_count must be exactly one')

    controls = {
        field: _strict_boolean(values[field], field)
        for field in _ROLLBACK_CONTROLS
    }
    policy_valid = all(controls.values())

    return STTRollbackPolicyEvidence(
        service_id=service_id,
        activation_id=activation_id,
        service_version=service_version,
        retention_count=retention_count,
        policy_valid=policy_valid,
        policy_version=policy_version,
        **controls,
    )


validate_rollback_policy_evidence = validate_stt_rollback_policy_evidence
build_rollback_policy_evidence = validate_stt_rollback_policy_evidence
plan_rollback_policy = validate_stt_rollback_policy_evidence


__all__ = [
    'MAX_ACTIVATION_ID_BYTES',
    'MAX_EVIDENCE_BYTES',
    'MAX_POLICY_VERSION_BYTES',
    'MAX_SERVICE_ID_BYTES',
    'MAX_SERVICE_VERSION_BYTES',
    'RETENTION_COUNT',
    'ROLLBACK_POLICY_FIELDS',
    'STTRollbackPolicyEvidence',
    'STTRollbackPolicyEvidenceError',
    'build_rollback_policy_evidence',
    'plan_rollback_policy',
    'validate_rollback_policy_evidence',
    'validate_stt_rollback_policy_evidence',
]
