#!/usr/bin/env python3
"""Pure, inert evidence contract for bounded STT local IPC policy.

This module validates local IPC policy metadata only.  It does not create a
listener, bind a socket, start a process, access audio, use credentials, or
perform network work.  A valid result describes a future transport policy; it
is not transport readiness or enforcement evidence.
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
MAX_SOCKET_NAME_BYTES: Final = 64
MAX_FRAME_BYTES: Final = 64 * 1024
MAX_REQUEST_BYTES: Final = MAX_FRAME_BYTES
MAX_RESPONSE_BYTES: Final = MAX_FRAME_BYTES
PROTOCOL_VERSION: Final = 1

IPC_POLICY_FIELDS: Final = frozenset({
    'service_id',
    'activation_id',
    'socket_name',
    'protocol_version',
    'max_frame_bytes',
    'max_request_bytes',
    'max_response_bytes',
    'allowed_operations',
    'local_only',
})
ALLOWED_OPERATIONS: Final = frozenset({'health', 'transcribe'})

_SERVICE_ID_RE: Final = re.compile(r'^[a-z0-9][a-z0-9._-]{0,63}$')
_ACTIVATION_ID_RE: Final = re.compile(r'^[A-Za-z0-9][A-Za-z0-9_-]{0,127}$')
# A socket identity is a local label, never a path, URL, address, or host.
_SOCKET_NAME_RE: Final = re.compile(r'^[a-z0-9][a-z0-9_-]{0,63}$')
_SECRET_RE: Final = re.compile(
    r'(?:password|passwd|secret|token|credential|bearer|api[_-]?key)',
    re.IGNORECASE,
)


class STTIPCPolicyEvidenceError(ValueError):
    """Raised when local IPC policy metadata is malformed or unsafe."""


def _fail(message: str) -> STTIPCPolicyEvidenceError:
    return STTIPCPolicyEvidenceError(message)


def _bounded_json(value: Mapping[str, Any]) -> None:
    try:
        encoded = json.dumps(
            dict(value), ensure_ascii=True, allow_nan=False, separators=(',', ':')
        ).encode('utf-8')
    except (TypeError, ValueError, UnicodeEncodeError) as exc:
        raise _fail('STT IPC policy evidence is not bounded JSON') from exc
    if len(encoded) > MAX_EVIDENCE_BYTES:
        raise _fail('STT IPC policy evidence is oversized')


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


def _service_id(value: Any) -> str:
    return _identifier(value, 'service_id', _SERVICE_ID_RE, MAX_SERVICE_ID_BYTES)


def _activation_id(value: Any) -> str:
    return _identifier(value, 'activation_id', _ACTIVATION_ID_RE, MAX_ACTIVATION_ID_BYTES)


def _socket_name(value: Any) -> str:
    return _identifier(value, 'socket_name', _SOCKET_NAME_RE, MAX_SOCKET_NAME_BYTES)


def _bounded_limit(value: Any, field: str) -> int:
    if (
        isinstance(value, bool)
        or not isinstance(value, int)
        or not 1 <= value <= MAX_FRAME_BYTES
    ):
        raise _fail(f'{field} is outside the codec-compatible bound')
    return value


def _operations(value: Any) -> tuple[str, ...]:
    if not isinstance(value, (list, tuple)) or not value:
        raise _fail('allowed_operations must be a non-empty sequence')
    if any(not isinstance(item, str) for item in value):
        raise _fail('allowed_operations contains a non-string operation')
    if len(set(value)) != len(value):
        raise _fail('allowed_operations contains duplicate operations')
    if any(item not in ALLOWED_OPERATIONS for item in value):
        raise _fail('allowed_operations contains an unknown operation')
    return tuple(value)


@dataclass(frozen=True, slots=True)
class STTIPCPolicyEvidence:
    """Immutable local IPC policy metadata, never listener or transport state."""

    service_id: str
    activation_id: str
    socket_name: str
    protocol_version: int
    max_frame_bytes: int
    max_request_bytes: int
    max_response_bytes: int
    allowed_operations: tuple[str, ...]
    local_only: bool = True
    policy_valid: bool = True
    execution_enabled: bool = False
    runtime_activation_allowed: bool = False
    socket_bound: bool = False
    ipc_enforced: bool = False
    effect: str = 'metadata_only'

    def as_dict(self) -> dict[str, Any]:
        """Serialize only bounded policy metadata and inert status flags."""
        return {
            'plan_type': 'stt_client_service_ipc_policy_evidence',
            'effect': 'metadata_only',
            'policy_valid': True,
            'service_id': self.service_id,
            'activation_id': self.activation_id,
            'socket_name': self.socket_name,
            'protocol_version': self.protocol_version,
            'max_frame_bytes': self.max_frame_bytes,
            'max_request_bytes': self.max_request_bytes,
            'max_response_bytes': self.max_response_bytes,
            'allowed_operations': list(self.allowed_operations),
            'local_only': True,
            'execution_enabled': False,
            'runtime_activation_allowed': False,
            'socket_bound': False,
            'ipc_enforced': False,
        }

    as_plan = as_dict


def validate_stt_ipc_policy_evidence(
    evidence: Mapping[str, Any],
    *,
    expected_service_id: str | None = None,
    expected_activation_id: str | None = None,
    expected_socket_name: str | None = None,
) -> STTIPCPolicyEvidence:
    """Validate one closed, identity-bound, metadata-only IPC policy."""
    if not isinstance(evidence, Mapping):
        raise _fail('STT IPC policy evidence must be a mapping')
    try:
        values = dict(evidence)
    except (TypeError, ValueError) as exc:
        raise _fail('STT IPC policy evidence must be a bounded mapping') from exc
    _bounded_json(values)
    if set(values) != IPC_POLICY_FIELDS:
        raise _fail('STT IPC policy evidence fields are not allowlisted')

    service_id = _service_id(values['service_id'])
    activation_id = _activation_id(values['activation_id'])
    socket_name = _socket_name(values['socket_name'])
    if expected_service_id is not None and service_id != _service_id(expected_service_id):
        raise _fail('service identity mismatch')
    if expected_activation_id is not None and activation_id != _activation_id(expected_activation_id):
        raise _fail('activation identity mismatch')
    if expected_socket_name is not None and socket_name != _socket_name(expected_socket_name):
        raise _fail('socket identity mismatch')

    protocol_version = values['protocol_version']
    if (
        isinstance(protocol_version, bool)
        or not isinstance(protocol_version, int)
        or protocol_version != PROTOCOL_VERSION
    ):
        raise _fail('protocol_version is not the supported codec version')
    max_frame_bytes = _bounded_limit(values['max_frame_bytes'], 'max_frame_bytes')
    max_request_bytes = _bounded_limit(values['max_request_bytes'], 'max_request_bytes')
    max_response_bytes = _bounded_limit(values['max_response_bytes'], 'max_response_bytes')
    if max_request_bytes > max_frame_bytes or max_response_bytes > max_frame_bytes:
        raise _fail('request and response limits exceed max_frame_bytes')
    allowed_operations = _operations(values['allowed_operations'])
    if values['local_only'] is not True:
        raise _fail('local_only must be true')

    return STTIPCPolicyEvidence(
        service_id=service_id,
        activation_id=activation_id,
        socket_name=socket_name,
        protocol_version=protocol_version,
        max_frame_bytes=max_frame_bytes,
        max_request_bytes=max_request_bytes,
        max_response_bytes=max_response_bytes,
        allowed_operations=allowed_operations,
    )


# Descriptive aliases for callers using policy/evidence terminology.
validate_ipc_policy_evidence = validate_stt_ipc_policy_evidence
build_ipc_policy_evidence = validate_stt_ipc_policy_evidence
plan_ipc_policy = validate_stt_ipc_policy_evidence


__all__ = [
    'ALLOWED_OPERATIONS',
    'IPC_POLICY_FIELDS',
    'MAX_ACTIVATION_ID_BYTES',
    'MAX_EVIDENCE_BYTES',
    'MAX_FRAME_BYTES',
    'MAX_REQUEST_BYTES',
    'MAX_RESPONSE_BYTES',
    'MAX_SERVICE_ID_BYTES',
    'MAX_SOCKET_NAME_BYTES',
    'PROTOCOL_VERSION',
    'STTIPCPolicyEvidence',
    'STTIPCPolicyEvidenceError',
    'build_ipc_policy_evidence',
    'plan_ipc_policy',
    'validate_ipc_policy_evidence',
    'validate_stt_ipc_policy_evidence',
]
