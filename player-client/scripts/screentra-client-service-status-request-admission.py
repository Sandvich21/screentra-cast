#!/usr/bin/env python3
"""Pure, bounded admission for one inert client-service status request.

The contract returns immutable metadata only. It never starts, inspects, or
controls a local service and never accesses host resources.
"""
from __future__ import annotations

import json
import re
from collections.abc import Mapping
from types import MappingProxyType
from typing import Any, Final


MAX_SERVICE_ID_BYTES: Final = 64
MAX_REQUEST_BYTES: Final = 80
MAX_EVIDENCE_BYTES: Final = 1024

STATUS_REQUEST_FIELDS: Final = ('service_id',)
CLIENT_SERVICE_STATUS_REQUEST_FIELDS: Final = frozenset(STATUS_REQUEST_FIELDS)
_STATUS_EVIDENCE_FIELDS: Final = frozenset({
    'service_id',
    'request_valid',
    'execution_enabled',
    'request_queued',
    'health_requested',
    'process_started',
    'service_healthy',
})
_SERVICE_ID_RE: Final = re.compile(r'^[a-z0-9][a-z0-9._-]{0,63}$')
_UNSAFE_IDENTIFIER_RE: Final = re.compile(
    r'(?:password|passwd|secret|token|credential|bearer|api[_-]?key|apikey)',
    re.IGNORECASE,
)


class StatusRequestError(ValueError):
    """Raised when a status request is not the closed contract."""


ClientServiceStatusRequestError = StatusRequestError


def _fail(message: str = 'status request is invalid') -> StatusRequestError:
    return StatusRequestError(message)


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


def _service_id(value: Any) -> str:
    if (
        not isinstance(value, str)
        or not value
        or not value.isascii()
        or len(value.encode('utf-8')) > MAX_SERVICE_ID_BYTES
        or _SERVICE_ID_RE.fullmatch(value) is None
        or _UNSAFE_IDENTIFIER_RE.search(value) is not None
    ):
        raise _fail('service_id is invalid or exceeds its bound')
    return value


def _request_values(value: Any) -> dict[str, str]:
    if not isinstance(value, Mapping):
        raise _fail('status request must be a mapping')
    try:
        keys = list(value.keys())
        if len(keys) != len(set(keys)):
            raise _fail('status request contains duplicate keys')
        values = dict(value)
    except StatusRequestError:
        raise
    except (TypeError, ValueError) as exc:
        raise _fail('status request is not a bounded mapping') from exc
    if set(values) != CLIENT_SERVICE_STATUS_REQUEST_FIELDS:
        raise _fail('status request fields are not allowlisted')
    _bounded_json(values, MAX_REQUEST_BYTES, 'status request is oversized')
    return {'service_id': _service_id(values['service_id'])}


def validate_status_request(payload: Mapping[str, Any]) -> MappingProxyType:
    """Return the exact immutable request mapping after pure validation."""
    values = _request_values(payload)
    return MappingProxyType({'service_id': values['service_id']})


def build_status_request_evidence(request: Mapping[str, Any]) -> MappingProxyType:
    """Return bounded metadata for the exact immutable validated request."""
    if type(request) is not MappingProxyType:
        raise _fail('status request must be the immutable validated result')
    values = _request_values(request)
    evidence = {
        'service_id': values['service_id'],
        'request_valid': True,
        'execution_enabled': False,
        'request_queued': False,
        'health_requested': False,
        'process_started': False,
        'service_healthy': False,
    }
    if set(evidence) != _STATUS_EVIDENCE_FIELDS:
        raise _fail('status evidence fields are not allowlisted')
    _bounded_json(evidence, MAX_EVIDENCE_BYTES, 'status evidence is oversized')
    return MappingProxyType(evidence)


admit_status_request = validate_status_request
admit_client_service_status_request = validate_status_request
status_request_evidence = build_status_request_evidence
validate_client_service_status_request = validate_status_request
build_client_service_status_request_evidence = build_status_request_evidence


__all__ = [
    'CLIENT_SERVICE_STATUS_REQUEST_FIELDS',
    'ClientServiceStatusRequestError',
    'MAX_EVIDENCE_BYTES',
    'MAX_REQUEST_BYTES',
    'MAX_SERVICE_ID_BYTES',
    'STATUS_REQUEST_FIELDS',
    'StatusRequestError',
    'admit_client_service_status_request',
    'admit_status_request',
    'build_client_service_status_request_evidence',
    'build_status_request_evidence',
    'status_request_evidence',
    'validate_client_service_status_request',
    'validate_status_request',
]
