#!/usr/bin/env python3
"""Pure, bounded admission for one inert transcription request.

The request mapping is closed and immutable.  Admission returns metadata-only
state and never performs work outside this module.
"""
from __future__ import annotations

import json
import re
from collections.abc import Mapping
from types import MappingProxyType
from typing import Any, Final


MAX_SERVICE_ID_BYTES: Final = 64
MAX_REQUEST_ID_BYTES: Final = 128
MAX_REQUEST_BYTES: Final = 512
MAX_EVIDENCE_BYTES: Final = 2048

TRANSCRIPTION_REQUEST_FIELDS: Final = (
    'service_id',
    'request_id',
    'push_to_talk',
)
STT_TRANSCRIPTION_REQUEST_FIELDS: Final = frozenset(TRANSCRIPTION_REQUEST_FIELDS)
_TRANSCRIPTION_REQUEST_FIELD_SET: Final = STT_TRANSCRIPTION_REQUEST_FIELDS
_TRANSCRIPTION_EVIDENCE_FIELDS: Final = frozenset({
    'service_id',
    'request_id',
    'push_to_talk',
    'request_valid',
    'execution_enabled',
    'request_queued',
    'audio_accessed',
    'transcription_started',
    'status',
})
_SERVICE_ID_RE: Final = re.compile(r'^[a-z0-9][a-z0-9._-]{0,63}$')
_REQUEST_ID_RE: Final = re.compile(r'^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$')
_UNSAFE_IDENTIFIER_RE: Final = re.compile(
    r'(?:password|passwd|secret|token|credential|bearer|api[_-]?key|apikey)',
    re.IGNORECASE,
)


class STTTranscriptionRequestError(ValueError):
    """Raised when a transcription request is not the closed contract."""


def _fail(message: str = 'transcription request is invalid') -> STTTranscriptionRequestError:
    return STTTranscriptionRequestError(message)


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


def _request_values(value: Any) -> dict[str, Any]:
    if not isinstance(value, Mapping):
        raise _fail('transcription request must be a mapping')
    try:
        keys = list(value.keys())
        if len(keys) != len(set(keys)):
            raise _fail('transcription request contains duplicate keys')
        values = dict(value)
    except STTTranscriptionRequestError:
        raise
    except (TypeError, ValueError) as exc:
        raise _fail('transcription request is not a bounded mapping') from exc
    if set(values) != _TRANSCRIPTION_REQUEST_FIELD_SET:
        raise _fail('transcription request fields are not allowlisted')
    _bounded_json(values, MAX_REQUEST_BYTES, 'transcription request is oversized')
    _identifier(values['service_id'], 'service_id', _SERVICE_ID_RE, MAX_SERVICE_ID_BYTES)
    _identifier(values['request_id'], 'request_id', _REQUEST_ID_RE, MAX_REQUEST_ID_BYTES)
    if values['push_to_talk'] is not True:
        raise _fail('push_to_talk must be the real boolean true')
    return values


def validate_transcription_request(payload: Mapping[str, Any]) -> MappingProxyType:
    """Return the exact immutable request mapping after pure validation."""
    values = _request_values(payload)
    return MappingProxyType({field: values[field] for field in TRANSCRIPTION_REQUEST_FIELDS})


def build_transcription_request_evidence(request: Mapping[str, Any]) -> MappingProxyType:
    """Return only bounded metadata for the exact immutable validated request."""
    if type(request) is not MappingProxyType:
        raise _fail('transcription request must be the immutable validated result')
    values = _request_values(request)
    evidence = {
        'service_id': values['service_id'],
        'request_id': values['request_id'],
        'push_to_talk': values['push_to_talk'],
        'request_valid': True,
        'execution_enabled': False,
        'request_queued': False,
        'audio_accessed': False,
        'transcription_started': False,
        'status': 'disabled',
    }
    if set(evidence) != _TRANSCRIPTION_EVIDENCE_FIELDS:
        raise _fail('transcription evidence fields are not allowlisted')
    _bounded_json(evidence, MAX_EVIDENCE_BYTES, 'transcription evidence is oversized')
    return MappingProxyType(evidence)


admit_transcription_request = validate_transcription_request
transcription_request_evidence = build_transcription_request_evidence
validate_stt_transcription_request = validate_transcription_request
build_stt_transcription_request_evidence = build_transcription_request_evidence


__all__ = [
    'MAX_EVIDENCE_BYTES',
    'MAX_REQUEST_BYTES',
    'MAX_REQUEST_ID_BYTES',
    'MAX_SERVICE_ID_BYTES',
    'STTTranscriptionRequestError',
    'STT_TRANSCRIPTION_REQUEST_FIELDS',
    'TRANSCRIPTION_REQUEST_FIELDS',
    'admit_transcription_request',
    'build_stt_transcription_request_evidence',
    'build_transcription_request_evidence',
    'transcription_request_evidence',
    'validate_stt_transcription_request',
    'validate_transcription_request',
]
