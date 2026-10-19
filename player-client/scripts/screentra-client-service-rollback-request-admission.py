#!/usr/bin/env python3
"""Pure, bounded admission for one inert client-service rollback request.

Admission returns immutable metadata only.  It never performs rollback or any
other local, process, network, filesystem, or hardware operation.
"""
from __future__ import annotations

import json
import re
from collections.abc import Mapping
from types import MappingProxyType
from typing import Any, Final


MAX_SERVICE_ID_BYTES: Final = 64
MAX_ACTIVATION_ID_BYTES: Final = 128
MAX_REQUEST_BYTES: Final = 192
MAX_EVIDENCE_BYTES: Final = 2048

ROLLBACK_REQUEST_FIELDS: Final = ('service_id', 'activation_id')
CLIENT_SERVICE_ROLLBACK_REQUEST_FIELDS: Final = frozenset(ROLLBACK_REQUEST_FIELDS)
_ROLLBACK_EVIDENCE_FIELDS: Final = frozenset({
    'service_id',
    'activation_id',
    'request_valid',
    'execution_enabled',
    'request_queued',
    'rollback_executed',
    'state_changed',
    'status',
})
_SERVICE_ID_RE: Final = re.compile(r'^[a-z0-9][a-z0-9._-]{0,63}$')
_ACTIVATION_ID_RE: Final = re.compile(r'^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$')
_UNSAFE_IDENTIFIER_RE: Final = re.compile(
    r'(?:password|passwd|secret|token|credential|bearer|api[_-]?key|apikey)',
    re.IGNORECASE,
)


class RollbackRequestError(ValueError):
    """Raised when a rollback request is not the closed contract."""


ClientServiceRollbackRequestError = RollbackRequestError


def _fail(message: str = 'rollback request is invalid') -> RollbackRequestError:
    return RollbackRequestError(message)


def _bounded_json(value: Mapping[str, Any], limit: int, message: str) -> None:
    try:
        encoded = json.dumps(
            dict(value),
            ensure_ascii=True,
            allow_nan=False,
            separators=(',', ':'),
        ).encode('utf-8')
    except (TypeError, ValueError, UnicodeEncodeError) as exc:
        raise _fail(message) from exc
    if len(encoded) > limit:
        raise _fail(message)


def _identifier(value: Any, field: str, pattern: re.Pattern[str], maximum: int) -> str:
    if (
        not isinstance(value, str)
        or not value
        or not value.isascii()
        or len(value.encode('utf-8')) > maximum
        or pattern.fullmatch(value) is None
        or _UNSAFE_IDENTIFIER_RE.search(value) is not None
    ):
        raise _fail(f'{field} is invalid or exceeds its bound')
    return value


def _request_values(value: Any) -> dict[str, str]:
    if not isinstance(value, Mapping):
        raise _fail('rollback request must be a mapping')
    try:
        keys = list(value.keys())
        if len(keys) != len(set(keys)):
            raise _fail('rollback request contains duplicate keys')
        values = dict(value)
    except RollbackRequestError:
        raise
    except (TypeError, ValueError) as exc:
        raise _fail('rollback request is not a bounded mapping') from exc
    if set(values) != CLIENT_SERVICE_ROLLBACK_REQUEST_FIELDS:
        raise _fail('rollback request fields are not allowlisted')
    _bounded_json(values, MAX_REQUEST_BYTES, 'rollback request is oversized')
    return {
        'service_id': _identifier(
            values['service_id'], 'service_id', _SERVICE_ID_RE, MAX_SERVICE_ID_BYTES
        ),
        'activation_id': _identifier(
            values['activation_id'], 'activation_id', _ACTIVATION_ID_RE,
            MAX_ACTIVATION_ID_BYTES,
        ),
    }


def validate_rollback_request(payload: Mapping[str, Any]) -> MappingProxyType:
    """Return the exact immutable request mapping after pure validation."""
    values = _request_values(payload)
    return MappingProxyType({field: values[field] for field in ROLLBACK_REQUEST_FIELDS})


def build_rollback_request_evidence(request: Mapping[str, Any]) -> MappingProxyType:
    """Return bounded metadata for the exact immutable validated request."""
    if type(request) is not MappingProxyType:
        raise _fail('rollback request must be the immutable validated result')
    values = _request_values(request)
    evidence = {
        'service_id': values['service_id'],
        'activation_id': values['activation_id'],
        'request_valid': True,
        'execution_enabled': False,
        'request_queued': False,
        'rollback_executed': False,
        'state_changed': False,
        'status': 'disabled',
    }
    if set(evidence) != _ROLLBACK_EVIDENCE_FIELDS:
        raise _fail('rollback evidence fields are not allowlisted')
    _bounded_json(evidence, MAX_EVIDENCE_BYTES, 'rollback evidence is oversized')
    return MappingProxyType(evidence)


admit_rollback_request = validate_rollback_request
admit_client_service_rollback_request = validate_rollback_request
rollback_request_evidence = build_rollback_request_evidence
validate_client_service_rollback_request = validate_rollback_request
build_client_service_rollback_request_evidence = build_rollback_request_evidence


__all__ = [
    'CLIENT_SERVICE_ROLLBACK_REQUEST_FIELDS',
    'ClientServiceRollbackRequestError',
    'MAX_ACTIVATION_ID_BYTES',
    'MAX_EVIDENCE_BYTES',
    'MAX_REQUEST_BYTES',
    'MAX_SERVICE_ID_BYTES',
    'ROLLBACK_REQUEST_FIELDS',
    'RollbackRequestError',
    'admit_rollback_request',
    'build_client_service_rollback_request_evidence',
    'build_rollback_request_evidence',
    'rollback_request_evidence',
    'validate_client_service_rollback_request',
    'validate_rollback_request',
]
