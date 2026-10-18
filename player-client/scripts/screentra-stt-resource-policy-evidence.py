#!/usr/bin/env python3
"""Pure, inert evidence contract for a bounded STT client-service policy.

This module validates only already-provided resource-policy metadata.  It never
checks or applies a runtime limit, starts a process or container, opens a
socket, accesses audio or model data, reads credentials, or changes host state.
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
CPU_MILLIS_MIN: Final = 1
CPU_MILLIS_MAX: Final = 4000
MEMORY_BYTES_MIN: Final = 64 * 1024 * 1024
MEMORY_BYTES_MAX: Final = 2 * 1024 * 1024 * 1024
PIDS_MIN: Final = 1
PIDS_MAX: Final = 256
WRITABLE_BYTES_MIN: Final = 0
WRITABLE_BYTES_MAX: Final = 512 * 1024 * 1024

RESOURCE_POLICY_FIELDS: Final = frozenset({
    'service_id',
    'activation_id',
    'cpu_millis',
    'memory_bytes',
    'pids',
    'writable_bytes',
    'policy_version',
})
_SERVICE_ID_RE: Final = re.compile(r'^[a-z0-9][a-z0-9._-]{0,63}$')
_ACTIVATION_ID_RE: Final = re.compile(r'^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$')
_POLICY_VERSION_RE: Final = re.compile(r'^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$')
_SECRET_RE: Final = re.compile(
    r'(?:password|passwd|secret|token|credential|bearer|api[_-]?key)',
    re.IGNORECASE,
)


class STTResourcePolicyEvidenceError(ValueError):
    """Raised when resource-policy metadata is malformed or unsafe."""


def _fail(message: str) -> STTResourcePolicyEvidenceError:
    return STTResourcePolicyEvidenceError(message)


def _bounded_json(value: Mapping[str, Any]) -> None:
    try:
        encoded = json.dumps(
            dict(value), ensure_ascii=True, allow_nan=False, separators=(',', ':')
        ).encode('utf-8')
    except (TypeError, ValueError, UnicodeEncodeError) as exc:
        raise _fail('resource-policy evidence is not bounded JSON') from exc
    if len(encoded) > MAX_EVIDENCE_BYTES:
        raise _fail('resource-policy evidence is oversized')


def _service_id(value: Any, field: str = 'service_id') -> str:
    if (
        not isinstance(value, str)
        or _SERVICE_ID_RE.fullmatch(value) is None
        or _SECRET_RE.search(value) is not None
    ):
        raise _fail(f'{field} is invalid')
    return value


def _activation_id(value: Any, field: str = 'activation_id') -> str:
    if (
        not isinstance(value, str)
        or _ACTIVATION_ID_RE.fullmatch(value) is None
        or _SECRET_RE.search(value) is not None
    ):
        raise _fail(f'{field} is invalid')
    return value


def _policy_version(value: Any) -> str:
    if (
        not isinstance(value, str)
        or len(value.encode('ascii', errors='ignore')) != len(value)
        or len(value.encode('utf-8')) > MAX_POLICY_VERSION_BYTES
        or _POLICY_VERSION_RE.fullmatch(value) is None
        or _SECRET_RE.search(value) is not None
    ):
        raise _fail('policy_version is invalid')
    return value


def _bounded_integer(value: Any, field: str, minimum: int, maximum: int) -> int:
    if isinstance(value, bool) or not isinstance(value, int) or not minimum <= value <= maximum:
        raise _fail(f'{field} is outside the bounded client-service policy')
    return value


@dataclass(frozen=True, slots=True)
class STTResourcePolicyEvidence:
    """Immutable resource-policy metadata, never enforcement or runtime state."""

    service_id: str
    activation_id: str
    cpu_millis: int
    memory_bytes: int
    pids: int
    writable_bytes: int
    policy_version: str
    policy_valid: bool = True
    execution_enabled: bool = False
    runtime_activation_allowed: bool = False
    process_started: bool = False
    resource_enforced: bool = False
    effect: str = 'metadata_only'

    def as_dict(self) -> dict[str, Any]:
        """Serialize the fixed metadata-only plan shape."""
        return {
            'plan_type': 'stt_client_service_resource_policy_plan',
            'effect': 'metadata_only',
            'service_id': self.service_id,
            'activation_id': self.activation_id,
            'policy_version': self.policy_version,
            'cpu_millis': self.cpu_millis,
            'memory_bytes': self.memory_bytes,
            'pids': self.pids,
            'writable_bytes': self.writable_bytes,
            'policy_valid': True,
            'execution_enabled': False,
            'runtime_activation_allowed': False,
            'process_started': False,
            'resource_enforced': False,
        }

    as_plan = as_dict


def validate_resource_policy_evidence(
    evidence: Mapping[str, Any],
    *,
    expected_service_id: str | None = None,
    expected_activation_id: str | None = None,
) -> STTResourcePolicyEvidence:
    """Validate one closed, identity-bound, metadata-only resource policy."""
    if not isinstance(evidence, Mapping):
        raise _fail('resource-policy evidence must be a mapping')
    try:
        values = dict(evidence)
    except (TypeError, ValueError) as exc:
        raise _fail('resource-policy evidence must be a bounded mapping') from exc
    _bounded_json(values)
    if set(values) != RESOURCE_POLICY_FIELDS:
        raise _fail('resource-policy evidence fields are not allowlisted')

    service_id = _service_id(values['service_id'])
    activation_id = _activation_id(values['activation_id'])
    if expected_service_id is not None and service_id != _service_id(expected_service_id, 'expected_service_id'):
        raise _fail('service identity mismatch')
    if expected_activation_id is not None and activation_id != _activation_id(expected_activation_id, 'expected_activation_id'):
        raise _fail('activation identity mismatch')

    cpu_millis = _bounded_integer(values['cpu_millis'], 'cpu_millis', CPU_MILLIS_MIN, CPU_MILLIS_MAX)
    memory_bytes = _bounded_integer(
        values['memory_bytes'], 'memory_bytes', MEMORY_BYTES_MIN, MEMORY_BYTES_MAX
    )
    pids = _bounded_integer(values['pids'], 'pids', PIDS_MIN, PIDS_MAX)
    writable_bytes = _bounded_integer(
        values['writable_bytes'], 'writable_bytes', WRITABLE_BYTES_MIN, WRITABLE_BYTES_MAX
    )
    policy_version = _policy_version(values['policy_version'])

    return STTResourcePolicyEvidence(
        service_id=service_id,
        activation_id=activation_id,
        cpu_millis=cpu_millis,
        memory_bytes=memory_bytes,
        pids=pids,
        writable_bytes=writable_bytes,
        policy_version=policy_version,
    )


build_resource_policy_plan = validate_resource_policy_evidence
plan_resource_policy = validate_resource_policy_evidence
validate_stt_resource_policy = validate_resource_policy_evidence


__all__ = [
    'CPU_MILLIS_MAX',
    'CPU_MILLIS_MIN',
    'MEMORY_BYTES_MAX',
    'MEMORY_BYTES_MIN',
    'MAX_ACTIVATION_ID_BYTES',
    'MAX_EVIDENCE_BYTES',
    'MAX_POLICY_VERSION_BYTES',
    'MAX_SERVICE_ID_BYTES',
    'PIDS_MAX',
    'PIDS_MIN',
    'RESOURCE_POLICY_FIELDS',
    'STTResourcePolicyEvidence',
    'STTResourcePolicyEvidenceError',
    'WRITABLE_BYTES_MAX',
    'WRITABLE_BYTES_MIN',
    'build_resource_policy_plan',
    'plan_resource_policy',
    'validate_resource_policy_evidence',
    'validate_stt_resource_policy',
]
