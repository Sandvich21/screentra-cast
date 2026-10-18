#!/usr/bin/env python3
"""Pure validator for a future client-local STT activation approval record.

The record is bounded, identity-bound metadata.  It records approval intent and
per-gate evidence only; it never grants permission, starts a process, loads a
model, accesses audio, binds IPC, or reports runtime readiness.
"""
from __future__ import annotations

import json
import re
import unicodedata
from collections.abc import Mapping
from dataclasses import dataclass
from datetime import datetime
from typing import Any, Final


MAX_RECORD_BYTES: Final = 16 * 1024
MAX_IDENTIFIER_BYTES: Final = 128
MAX_VERSION_BYTES: Final = 64
MAX_MODEL_ID_BYTES: Final = 64
MAX_HOST_ID_BYTES: Final = 64
MAX_APPROVER_BYTES: Final = 128
MAX_APPROVED_AT_BYTES: Final = 64
MAX_GATE_REASON_BYTES: Final = 512

REQUIRED_STT_APPROVAL_GATES: Final = (
    'capability_admission',
    'model_integrity',
    'audio_permission',
    'local_runtime',
    'resource_policy',
    'ipc_policy',
    'rollback_policy',
)
# Descriptive aliases for callers using activation terminology.
REQUIRED_STT_ACTIVATION_GATES: Final = REQUIRED_STT_APPROVAL_GATES
REQUIRED_GATES: Final = REQUIRED_STT_APPROVAL_GATES

_RECORD_FIELDS: Final = frozenset({
    'record_type',
    'service_id',
    'service_version',
    'activation_id',
    'host_id',
    'model_id',
    'model_sha256',
    'approver',
    'approved_at',
    'gates',
})
_GATE_FIELDS: Final = frozenset({'name', 'passed', 'reason'})
_IDENTIFIER_RE: Final = re.compile(r'^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$')
_MODEL_ID_RE: Final = re.compile(r'^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$')
_VERSION_RE: Final = re.compile(
    r'^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)'
    r'(?:-[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?$'
)
_SHA256_RE: Final = re.compile(r'^[0-9a-f]{64}$')
_RFC3339_RE: Final = re.compile(
    r'^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}'
    r'(?:\.\d{1,9})?(?:Z|[+-]\d{2}:\d{2})$'
)
_SECRET_RE: Final = re.compile(
    r'(?:password|passwd|secret|token|credential|bearer|api[_-]?key)',
    re.IGNORECASE,
)
_URL_RE: Final = re.compile(r'(?:[A-Za-z][A-Za-z0-9+.-]*://|\bwww\.)', re.IGNORECASE)
_COMMAND_RE: Final = re.compile(
    r'(?:`|\$\(|&&|\|\||;|'
    r'(?:^|\s)(?:sudo|sh|bash|zsh|python(?:3)?|arecord|ffmpeg|curl|wget|rm|chmod|systemctl)'
    r'(?:\s|$))',
    re.IGNORECASE,
)


class STTApprovalRecordError(ValueError):
    """Raised when an STT approval record is malformed or unsafe."""


@dataclass(frozen=True, slots=True)
class STTApprovalGate:
    """One immutable, metadata-only gate result."""

    name: str
    passed: bool
    reason: str


@dataclass(frozen=True, slots=True)
class STTApprovalRecord:
    """Validated approval intent, never runtime or readiness evidence."""

    record_type: str
    service_id: str
    service_version: str
    activation_id: str
    host_id: str
    model_id: str
    model_sha256: str
    approver: str
    approved_at: str
    gates: tuple[STTApprovalGate, ...]
    approval_record_valid: bool = True
    execution_enabled: bool = False
    runtime_activation_allowed: bool = False
    effect: str = 'metadata_only'

    def as_dict(self) -> dict[str, Any]:
        """Serialize only the fixed, bounded approval metadata shape."""
        return {
            'decision_type': 'stt_future_activation_approval',
            'approval_record_valid': True,
            'record_type': self.record_type,
            'service_id': self.service_id,
            'service_version': self.service_version,
            'activation_id': self.activation_id,
            'host_id': self.host_id,
            'model_id': self.model_id,
            'model_sha256': self.model_sha256,
            'approver': self.approver,
            'approved_at': self.approved_at,
            'gates': [
                {'name': gate.name, 'passed': gate.passed, 'reason': gate.reason}
                for gate in self.gates
            ],
            'execution_enabled': False,
            'runtime_activation_allowed': False,
            'effect': 'metadata_only',
        }

    def as_runtime_admission_input(self) -> dict[str, Any]:
        """Return inert evidence for a future runtime-admission boundary."""
        return {
            'approval_record': self.as_dict(),
            'execution_enabled': False,
            'runtime_activation_allowed': False,
            'effect': 'metadata_only',
        }

    as_runtime_admission_evidence = as_runtime_admission_input


def _fail(message: str) -> STTApprovalRecordError:
    return STTApprovalRecordError(message)


def _bounded_json(value: Any) -> None:
    try:
        encoded = json.dumps(
            value, ensure_ascii=True, allow_nan=False, separators=(',', ':')
        ).encode('utf-8')
    except (TypeError, ValueError, UnicodeEncodeError) as exc:
        raise _fail('STT approval record is not valid JSON metadata') from exc
    if len(encoded) > MAX_RECORD_BYTES:
        raise _fail('STT approval record is oversized')


def _safe_text(value: Any, field: str, maximum: int) -> str:
    if not isinstance(value, str) or not value or not value.strip():
        raise _fail(f'{field} is invalid')
    if len(value.encode('utf-8')) > maximum or any(character.isspace() and character not in {' '} for character in value):
        raise _fail(f'{field} is invalid')
    if any(unicodedata.category(character) == 'Cc' for character in value):
        raise _fail(f'{field} is invalid')
    if _SECRET_RE.search(value) or _URL_RE.search(value) or _COMMAND_RE.search(value):
        raise _fail(f'{field} contains forbidden metadata')
    if value.startswith(('/', '\\')) or '://' in value or '..' in value or '/' in value or '\\' in value:
        raise _fail(f'{field} contains a path')
    return value


def _identifier(value: Any, field: str, maximum: int = MAX_IDENTIFIER_BYTES) -> str:
    if (
        not isinstance(value, str)
        or not value
        or len(value.encode('ascii', errors='ignore')) != len(value)
        or len(value.encode('utf-8')) > maximum
        or _IDENTIFIER_RE.fullmatch(value) is None
        or '..' in value
        or _SECRET_RE.search(value) is not None
    ):
        raise _fail(f'{field} is invalid')
    return value


def _model_id(value: Any) -> str:
    if (
        not isinstance(value, str)
        or not value
        or len(value.encode('ascii', errors='ignore')) != len(value)
        or len(value.encode('utf-8')) > MAX_MODEL_ID_BYTES
        or _MODEL_ID_RE.fullmatch(value) is None
        or _SECRET_RE.search(value) is not None
    ):
        raise _fail('model_id is invalid')
    return value


def _version(value: Any) -> str:
    if (
        not isinstance(value, str)
        or not value
        or len(value.encode('ascii', errors='ignore')) != len(value)
        or len(value.encode('utf-8')) > MAX_VERSION_BYTES
        or _VERSION_RE.fullmatch(value) is None
    ):
        raise _fail('service_version is invalid')
    return value


def _digest(value: Any) -> str:
    if not isinstance(value, str) or _SHA256_RE.fullmatch(value) is None:
        raise _fail('model_sha256 is invalid')
    return value


def _approved_at(value: Any) -> str:
    if (
        not isinstance(value, str)
        or len(value.encode('utf-8')) > MAX_APPROVED_AT_BYTES
        or _RFC3339_RE.fullmatch(value) is None
    ):
        raise _fail('approved_at is invalid')
    try:
        parsed = datetime.fromisoformat(value.replace('Z', '+00:00'))
    except ValueError as exc:
        raise _fail('approved_at is invalid') from exc
    if parsed.tzinfo is None or parsed.utcoffset() is None:
        raise _fail('approved_at is invalid')
    return value


def _validate_gate(value: Any) -> STTApprovalGate:
    if not isinstance(value, Mapping) or set(value) != _GATE_FIELDS:
        raise _fail('STT approval gate fields are not allowlisted')
    name = value['name']
    reason = value['reason']
    passed = value['passed']
    if not isinstance(name, str) or name not in REQUIRED_STT_APPROVAL_GATES:
        raise _fail('STT approval gate is unknown')
    if not isinstance(passed, bool):
        raise _fail('STT approval gate passed value is invalid')
    reason = _safe_text(reason, 'gate reason', MAX_GATE_REASON_BYTES)
    return STTApprovalGate(name=name, passed=passed, reason=reason)


def validate_stt_approval_record(record: Mapping[str, Any]) -> STTApprovalRecord:
    """Validate one bounded, identity-bound future STT approval record."""
    if not isinstance(record, Mapping) or set(record) != _RECORD_FIELDS:
        raise _fail('STT approval record fields are not allowlisted')
    _bounded_json(record)

    record_type = record['record_type']
    if record_type != 'stt_future_activation_approval':
        raise _fail('STT approval record type is invalid')
    service_id = _identifier(record['service_id'], 'service_id')
    service_version = _version(record['service_version'])
    activation_id = _identifier(record['activation_id'], 'activation_id')
    host_id = _identifier(record['host_id'], 'host_id', MAX_HOST_ID_BYTES)
    model_id = _model_id(record['model_id'])
    model_sha256 = _digest(record['model_sha256'])
    approver = _safe_text(record['approver'], 'approver', MAX_APPROVER_BYTES)
    approved_at = _approved_at(record['approved_at'])

    gates_value = record['gates']
    if not isinstance(gates_value, list) or len(gates_value) != len(REQUIRED_STT_APPROVAL_GATES):
        raise _fail('STT approval gates are incomplete')
    gates = tuple(_validate_gate(gate) for gate in gates_value)
    names = tuple(gate.name for gate in gates)
    if len(set(names)) != len(names) or set(names) != set(REQUIRED_STT_APPROVAL_GATES):
        raise _fail('STT approval gates are unknown, duplicated, or incomplete')
    gates_by_name = {gate.name: gate for gate in gates}
    ordered_gates = tuple(gates_by_name[name] for name in REQUIRED_STT_APPROVAL_GATES)

    return STTApprovalRecord(
        record_type=record_type,
        service_id=service_id,
        service_version=service_version,
        activation_id=activation_id,
        host_id=host_id,
        model_id=model_id,
        model_sha256=model_sha256,
        approver=approver,
        approved_at=approved_at,
        gates=ordered_gates,
    )


# Descriptive aliases for callers using validator/evaluator terminology.
validate_approval_record = validate_stt_approval_record
evaluate_stt_approval_record = validate_stt_approval_record


__all__ = [
    'MAX_APPROVED_AT_BYTES',
    'MAX_APPROVER_BYTES',
    'MAX_GATE_REASON_BYTES',
    'MAX_IDENTIFIER_BYTES',
    'MAX_RECORD_BYTES',
    'REQUIRED_GATES',
    'REQUIRED_STT_ACTIVATION_GATES',
    'REQUIRED_STT_APPROVAL_GATES',
    'STTApprovalGate',
    'STTApprovalRecord',
    'STTApprovalRecordError',
    'evaluate_stt_approval_record',
    'validate_approval_record',
    'validate_stt_approval_record',
]
