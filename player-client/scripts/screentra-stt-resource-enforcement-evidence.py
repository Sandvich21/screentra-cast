#!/usr/bin/env python3
"""Pure, bounded evidence for observed STT resource enforcement.

This module validates an already-produced, redacted observation only.  It never
applies limits, inspects a process or container, binds a cgroup, accesses audio
or model data, executes a command, reads credentials, or reports readiness.
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
MAX_POLICY_VERSION_BYTES: Final = 64

RESOURCE_ENFORCEMENT_FIELDS: Final = frozenset({
    'service_id',
    'activation_id',
    'policy_version',
    'cpu_enforced',
    'memory_enforced',
    'pids_enforced',
    'writable_enforced',
    'observation_non_mutating',
})

_SERVICE_ID_RE: Final = re.compile(r'^[a-z0-9][a-z0-9._-]{0,63}$')
_ACTIVATION_ID_RE: Final = re.compile(r'^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$')
_POLICY_VERSION_RE: Final = re.compile(r'^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$')
_SECRET_RE: Final = re.compile(
    r'(?:password|passwd|secret|token|credential|bearer|api[_-]?key)',
    re.IGNORECASE,
)


class STTResourceEnforcementEvidenceError(ValueError):
    """Raised when resource-enforcement evidence is malformed or unsafe."""


def _fail(message: str) -> STTResourceEnforcementEvidenceError:
    return STTResourceEnforcementEvidenceError(message)


def _bounded_json(value: Mapping[str, Any]) -> None:
    try:
        encoded = json.dumps(
            dict(value), ensure_ascii=True, allow_nan=False, separators=(',', ':')
        ).encode('utf-8')
    except (TypeError, ValueError, UnicodeEncodeError) as exc:
        raise _fail('STT resource-enforcement evidence is not bounded JSON') from exc
    if len(encoded) > MAX_EVIDENCE_BYTES:
        raise _fail('STT resource-enforcement evidence is oversized')


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


def _policy_version(value: Any, field: str = 'policy_version') -> str:
    return _identifier(value, field, _POLICY_VERSION_RE, MAX_POLICY_VERSION_BYTES)


def _boolean(value: Any, field: str) -> bool:
    if not isinstance(value, bool):
        raise _fail(f'{field} must be a boolean')
    return value


@dataclass(frozen=True, slots=True)
class STTResourceEnforcementEvidence:
    """Immutable enforcement observations, never enforcement or runtime state."""

    service_id: str
    activation_id: str
    policy_version: str
    cpu_enforced: bool
    memory_enforced: bool
    pids_enforced: bool
    writable_enforced: bool
    observation_non_mutating: bool
    enforcement_valid: bool
    policy_valid: bool
    execution_enabled: bool = False
    runtime_activation_allowed: bool = False
    process_started: bool = False
    resource_enforced: bool = False
    cgroup_bound: bool = False
    effect: str = 'metadata_only'

    def as_dict(self) -> dict[str, Any]:
        """Serialize only bounded observation metadata and inert status flags."""
        return {
            'plan_type': 'stt_client_service_resource_enforcement_evidence',
            'effect': 'metadata_only',
            'service_id': self.service_id,
            'activation_id': self.activation_id,
            'policy_version': self.policy_version,
            'cpu_enforced': self.cpu_enforced,
            'memory_enforced': self.memory_enforced,
            'pids_enforced': self.pids_enforced,
            'writable_enforced': self.writable_enforced,
            'observation_non_mutating': self.observation_non_mutating,
            'enforcement_valid': self.enforcement_valid,
            'policy_valid': self.policy_valid,
            'execution_enabled': False,
            'runtime_activation_allowed': False,
            'process_started': False,
            'resource_enforced': False,
            'cgroup_bound': False,
        }

    as_plan = as_dict


def validate_stt_resource_enforcement_evidence(
    evidence: Mapping[str, Any],
    *,
    expected_service_id: str | None = None,
    expected_activation_id: str | None = None,
    expected_policy_version: str | None = None,
) -> STTResourceEnforcementEvidence:
    """Validate one closed, identity-bound, metadata-only observation record."""
    if not isinstance(evidence, Mapping):
        raise _fail('STT resource-enforcement evidence must be a mapping')
    try:
        keys = list(evidence.keys())
        if len(keys) != len(set(keys)):
            raise _fail('STT resource-enforcement evidence contains duplicate fields')
        values = dict(evidence)
    except STTResourceEnforcementEvidenceError:
        raise
    except (TypeError, ValueError) as exc:
        raise _fail('STT resource-enforcement evidence must be a bounded mapping') from exc
    if set(values) != RESOURCE_ENFORCEMENT_FIELDS:
        raise _fail('STT resource-enforcement evidence fields are not allowlisted')
    _bounded_json(values)

    service_id = _service_id(values['service_id'])
    activation_id = _activation_id(values['activation_id'])
    policy_version = _policy_version(values['policy_version'])
    if expected_service_id is not None and service_id != _service_id(expected_service_id, 'expected_service_id'):
        raise _fail('service identity mismatch')
    if expected_activation_id is not None and activation_id != _activation_id(expected_activation_id, 'expected_activation_id'):
        raise _fail('activation identity mismatch')
    if expected_policy_version is not None and policy_version != _policy_version(expected_policy_version, 'expected_policy_version'):
        raise _fail('resource policy version mismatch')

    observations = {
        field: _boolean(values[field], field)
        for field in (
            'cpu_enforced', 'memory_enforced', 'pids_enforced',
            'writable_enforced', 'observation_non_mutating',
        )
    }
    enforcement_valid = all(observations.values())
    return STTResourceEnforcementEvidence(
        service_id=service_id,
        activation_id=activation_id,
        policy_version=policy_version,
        **observations,
        enforcement_valid=enforcement_valid,
        policy_valid=enforcement_valid,
    )


validate_resource_enforcement_evidence = validate_stt_resource_enforcement_evidence
build_resource_enforcement_evidence = validate_stt_resource_enforcement_evidence
validate_stt_resource_enforcement = validate_stt_resource_enforcement_evidence


__all__ = [
    'MAX_ACTIVATION_ID_BYTES',
    'MAX_EVIDENCE_BYTES',
    'MAX_POLICY_VERSION_BYTES',
    'MAX_SERVICE_ID_BYTES',
    'RESOURCE_ENFORCEMENT_FIELDS',
    'STTResourceEnforcementEvidence',
    'STTResourceEnforcementEvidenceError',
    'build_resource_enforcement_evidence',
    'validate_resource_enforcement_evidence',
    'validate_stt_resource_enforcement',
    'validate_stt_resource_enforcement_evidence',
]
