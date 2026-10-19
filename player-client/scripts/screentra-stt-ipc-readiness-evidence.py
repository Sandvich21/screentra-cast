#!/usr/bin/env python3
"""Pure, bounded evidence for observed STT IPC transport readiness.

This module validates an already-produced, redacted local transport observation.
It never opens or binds a socket, starts a process, authenticates a peer, reads
credentials, accesses audio, executes a command, or reports runtime readiness.
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
PROTOCOL_VERSION: Final = 1

IPC_READINESS_FIELDS: Final = frozenset({
    'service_id',
    'activation_id',
    'socket_name',
    'protocol_version',
    'allowed_operations',
    'listener_observed',
    'peer_identity_observed',
    'hello_observed',
    'transport_non_mutating',
    'local_only',
})
ALLOWED_OPERATIONS: Final = frozenset({'health', 'transcribe'})

_SERVICE_ID_RE: Final = re.compile(r'^[a-z0-9][a-z0-9._-]{0,63}$')
_ACTIVATION_ID_RE: Final = re.compile(r'^[A-Za-z0-9][A-Za-z0-9_-]{0,127}$')
_SOCKET_NAME_RE: Final = re.compile(r'^[a-z0-9][a-z0-9_-]{0,63}$')
_SECRET_RE: Final = re.compile(
    r'(?:password|passwd|secret|token|credential|bearer|api[_-]?key)',
    re.IGNORECASE,
)


class STTIPCReadinessEvidenceError(ValueError):
    """Raised when STT IPC readiness evidence is malformed or unsafe."""


def _fail(message: str) -> STTIPCReadinessEvidenceError:
    return STTIPCReadinessEvidenceError(message)


def _bounded_json(value: Mapping[str, Any]) -> None:
    try:
        encoded = json.dumps(
            dict(value), ensure_ascii=True, allow_nan=False, separators=(',', ':')
        ).encode('utf-8')
    except (TypeError, ValueError, UnicodeEncodeError) as exc:
        raise _fail('STT IPC readiness evidence is not bounded JSON') from exc
    if len(encoded) > MAX_EVIDENCE_BYTES:
        raise _fail('STT IPC readiness evidence is oversized')


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


def _socket_name(value: Any, field: str = 'socket_name') -> str:
    return _identifier(value, field, _SOCKET_NAME_RE, MAX_SOCKET_NAME_BYTES)


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


def _boolean(value: Any, field: str) -> bool:
    if not isinstance(value, bool):
        raise _fail(f'{field} must be a boolean')
    return value


@dataclass(frozen=True, slots=True)
class STTIPCReadinessEvidence:
    """Immutable local transport observations, never activation state."""

    service_id: str
    activation_id: str
    socket_name: str
    protocol_version: int
    allowed_operations: tuple[str, ...]
    listener_observed: bool
    peer_identity_observed: bool
    hello_observed: bool
    transport_non_mutating: bool
    local_only: bool
    readiness_valid: bool
    execution_enabled: bool = False
    runtime_activation_allowed: bool = False
    socket_bound: bool = False
    ipc_enforced: bool = False
    listener_started: bool = False
    peer_authenticated: bool = False
    effect: str = 'metadata_only'

    def as_dict(self) -> dict[str, Any]:
        """Serialize only bounded observations and explicit inert flags."""
        return {
            'plan_type': 'stt_client_service_ipc_readiness_evidence',
            'effect': 'metadata_only',
            'service_id': self.service_id,
            'activation_id': self.activation_id,
            'socket_name': self.socket_name,
            'protocol_version': self.protocol_version,
            'allowed_operations': list(self.allowed_operations),
            'listener_observed': self.listener_observed,
            'peer_identity_observed': self.peer_identity_observed,
            'hello_observed': self.hello_observed,
            'transport_non_mutating': self.transport_non_mutating,
            'local_only': True,
            'readiness_valid': self.readiness_valid,
            'execution_enabled': False,
            'runtime_activation_allowed': False,
            'socket_bound': False,
            'ipc_enforced': False,
            'listener_started': False,
            'peer_authenticated': False,
        }

    as_plan = as_dict


def validate_stt_ipc_readiness_evidence(
    evidence: Mapping[str, Any],
    *,
    expected_service_id: str | None = None,
    expected_activation_id: str | None = None,
    expected_socket_name: str | None = None,
    expected_protocol_version: int | None = None,
    expected_allowed_operations: tuple[str, ...] | list[str] | None = None,
) -> STTIPCReadinessEvidence:
    """Validate one closed, identity-bound, metadata-only observation record."""
    if not isinstance(evidence, Mapping):
        raise _fail('STT IPC readiness evidence must be a mapping')
    try:
        keys = list(evidence.keys())
        if len(keys) != len(set(keys)):
            raise _fail('STT IPC readiness evidence contains duplicate fields')
        values = dict(evidence)
    except STTIPCReadinessEvidenceError:
        raise
    except (TypeError, ValueError) as exc:
        raise _fail('STT IPC readiness evidence must be a bounded mapping') from exc
    if set(values) != IPC_READINESS_FIELDS:
        raise _fail('STT IPC readiness evidence fields are not allowlisted')
    _bounded_json(values)

    service_id = _service_id(values['service_id'])
    activation_id = _activation_id(values['activation_id'])
    socket_name = _socket_name(values['socket_name'])
    if expected_service_id is not None and service_id != _service_id(expected_service_id, 'expected_service_id'):
        raise _fail('service identity mismatch')
    if expected_activation_id is not None and activation_id != _activation_id(expected_activation_id, 'expected_activation_id'):
        raise _fail('activation identity mismatch')
    if expected_socket_name is not None and socket_name != _socket_name(expected_socket_name, 'expected_socket_name'):
        raise _fail('socket identity mismatch')

    protocol_version = values['protocol_version']
    if (
        isinstance(protocol_version, bool)
        or not isinstance(protocol_version, int)
        or protocol_version != PROTOCOL_VERSION
    ):
        raise _fail('protocol_version is not the supported IPC version')
    if expected_protocol_version is not None:
        if isinstance(expected_protocol_version, bool) or not isinstance(expected_protocol_version, int):
            raise _fail('expected protocol version is invalid')
        if protocol_version != expected_protocol_version:
            raise _fail('protocol version mismatch')

    allowed_operations = _operations(values['allowed_operations'])
    if expected_allowed_operations is not None:
        expected_operations = _operations(expected_allowed_operations)
        if allowed_operations != expected_operations:
            raise _fail('allowed operation identity mismatch')

    observations = {
        field: _boolean(values[field], field)
        for field in (
            'listener_observed', 'peer_identity_observed', 'hello_observed',
            'transport_non_mutating',
        )
    }
    local_only = _boolean(values['local_only'], 'local_only')
    if local_only is not True:
        raise _fail('local_only must be true')
    readiness_valid = all(observations.values()) and local_only
    return STTIPCReadinessEvidence(
        service_id=service_id,
        activation_id=activation_id,
        socket_name=socket_name,
        protocol_version=protocol_version,
        allowed_operations=allowed_operations,
        **observations,
        local_only=True,
        readiness_valid=readiness_valid,
    )


validate_ipc_readiness_evidence = validate_stt_ipc_readiness_evidence
build_ipc_readiness_evidence = validate_stt_ipc_readiness_evidence
plan_ipc_readiness = validate_stt_ipc_readiness_evidence


__all__ = [
    'ALLOWED_OPERATIONS',
    'IPC_READINESS_FIELDS',
    'MAX_ACTIVATION_ID_BYTES',
    'MAX_EVIDENCE_BYTES',
    'MAX_SERVICE_ID_BYTES',
    'MAX_SOCKET_NAME_BYTES',
    'PROTOCOL_VERSION',
    'STTIPCReadinessEvidence',
    'STTIPCReadinessEvidenceError',
    'build_ipc_readiness_evidence',
    'plan_ipc_readiness',
    'validate_ipc_readiness_evidence',
    'validate_stt_ipc_readiness_evidence',
]
