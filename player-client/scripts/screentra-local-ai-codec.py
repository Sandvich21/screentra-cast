#!/usr/bin/env python3
"""Contract-only codec for the future client-local AI service.

This module is deliberately local-only.  It provides bounded, typed JSON
frames for a future client-local speech/status service; it does not open a
socket, access a microphone, run a model, launch a process, or report frames
to a server.  Audio is accepted only as a bounded field in a local
``transcribe`` request and is never valid in a response.
"""
from __future__ import annotations

import base64
import binascii
import json
import re
import struct
from dataclasses import dataclass
from typing import Any, Final


LOCAL_ONLY: Final = True
PROTOCOL_VERSION: Final = 1
FRAME_LENGTH_BYTES: Final = 4
MAX_FRAME_BYTES: Final = 64 * 1024
MAX_AUDIO_BYTES: Final = 32 * 1024
MAX_REQUEST_ID_BYTES: Final = 64
MAX_OPERATION_BYTES: Final = 32
MAX_LANGUAGE_BYTES: Final = 32
MAX_TEXT_BYTES: Final = 16 * 1024
MAX_ERROR_CODE_BYTES: Final = 32

MAX_REQUEST_ENVELOPE_FIELDS: Final = 5
MAX_RESPONSE_ENVELOPE_FIELDS: Final = 7

_REQUEST_FIELDS = frozenset({'version', 'local_only', 'request_id', 'operation', 'payload'})
_RESPONSE_FIELDS = frozenset(
    {'version', 'local_only', 'request_id', 'operation', 'ok', 'result', 'error'}
)
_ALLOWED_OPERATIONS = frozenset({'health', 'transcribe'})
_ALLOWED_ERROR_CODES = frozenset(
    {'invalid_request', 'unsupported_operation', 'unavailable', 'internal_error'}
)
_REQUEST_ID_RE = re.compile(r'^[A-Za-z0-9._:-]+$')
_LANGUAGE_RE = re.compile(r'^[A-Za-z0-9-]+$')


class ProtocolError(ValueError):
    """Raised when a frame or typed message violates the local contract."""


@dataclass(frozen=True)
class HealthRequest:
    """Request a bounded local service health result."""


@dataclass(frozen=True)
class TranscribeRequest:
    """Submit bounded audio bytes to the local service only."""

    audio: bytes
    language: str | None = None


@dataclass(frozen=True)
class Request:
    request_id: str
    operation: str
    payload: HealthRequest | TranscribeRequest
    local_only: bool = LOCAL_ONLY


@dataclass(frozen=True)
class HealthResponse:
    status: str


@dataclass(frozen=True)
class TranscribeResponse:
    text: str


@dataclass(frozen=True)
class ErrorResponse:
    code: str


@dataclass(frozen=True)
class Response:
    request_id: str
    operation: str
    ok: bool
    result: HealthResponse | TranscribeResponse | None
    error: ErrorResponse | None
    local_only: bool = LOCAL_ONLY


def _bounded_text(value: Any, field: str, maximum: int, pattern: re.Pattern[str] | None = None) -> str:
    if not isinstance(value, str) or not value:
        raise ProtocolError(f'{field} is invalid or exceeds its bound')
    try:
        encoded = value.encode('utf-8')
    except UnicodeEncodeError as exc:
        raise ProtocolError(f'{field} is not valid UTF-8') from exc
    if len(encoded) > maximum:
        raise ProtocolError(f'{field} is invalid or exceeds its bound')
    if any(ord(char) < 32 or ord(char) == 127 for char in value):
        raise ProtocolError(f'{field} contains a control character')
    if pattern is not None and pattern.fullmatch(value) is None:
        raise ProtocolError(f'{field} contains unsupported characters')
    return value


def _validate_request_id(value: Any) -> str:
    return _bounded_text(value, 'request_id', MAX_REQUEST_ID_BYTES, _REQUEST_ID_RE)


def _validate_operation(value: Any) -> str:
    operation = _bounded_text(value, 'operation', MAX_OPERATION_BYTES)
    if operation not in _ALLOWED_OPERATIONS:
        raise ProtocolError(f'unsupported operation: {operation}')
    return operation


def _validate_language(value: Any) -> str:
    return _bounded_text(value, 'language', MAX_LANGUAGE_BYTES, _LANGUAGE_RE)


def _json_loads(body: bytes) -> dict[str, Any]:
    def reject_duplicate_fields(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
        value = {}
        for key, item in pairs:
            if key in value:
                raise ValueError('duplicate JSON field')
            value[key] = item
        return value

    try:
        text = body.decode('utf-8')
        value = json.loads(
            text,
            object_pairs_hook=reject_duplicate_fields,
            parse_constant=lambda _: (_ for _ in ()).throw(ValueError()),
        )
    except (UnicodeDecodeError, json.JSONDecodeError, ValueError) as exc:
        raise ProtocolError('frame body is not valid JSON') from exc
    if not isinstance(value, dict):
        raise ProtocolError('frame JSON must be an object')
    return value


def _pack_envelope(envelope: dict[str, Any]) -> bytes:
    try:
        body = json.dumps(
            envelope,
            ensure_ascii=False,
            allow_nan=False,
            separators=(',', ':'),
            sort_keys=True,
        ).encode('utf-8')
    except (TypeError, ValueError, UnicodeEncodeError) as exc:
        raise ProtocolError('frame body cannot be encoded') from exc
    if len(body) > MAX_FRAME_BYTES:
        raise ProtocolError('frame exceeds its byte bound')
    return struct.pack('!I', len(body)) + body


def _unpack_frame(frame: bytes) -> dict[str, Any]:
    if not isinstance(frame, bytes) or len(frame) < FRAME_LENGTH_BYTES:
        raise ProtocolError('frame is truncated before its length prefix')
    declared = struct.unpack('!I', frame[:FRAME_LENGTH_BYTES])[0]
    if declared > MAX_FRAME_BYTES:
        raise ProtocolError('frame exceeds its byte bound')
    expected = FRAME_LENGTH_BYTES + declared
    if len(frame) != expected:
        raise ProtocolError('frame is truncated or contains trailing bytes')
    return _json_loads(frame[FRAME_LENGTH_BYTES:])


def _validate_common(envelope: dict[str, Any], fields: frozenset[str], maximum_fields: int) -> tuple[str, str]:
    if len(envelope) > maximum_fields or set(envelope) != fields:
        raise ProtocolError('envelope fields are not exactly allowlisted')
    if (not isinstance(envelope.get('version'), int) or
            isinstance(envelope.get('version'), bool) or
            envelope.get('version') != PROTOCOL_VERSION):
        raise ProtocolError('unsupported protocol version')
    if envelope.get('local_only') is not True:
        raise ProtocolError('frame is not marked local-only')
    return _validate_request_id(envelope.get('request_id')), _validate_operation(envelope.get('operation'))


def _decode_audio(audio_b64: Any) -> bytes:
    if not isinstance(audio_b64, str) or not audio_b64:
        raise ProtocolError('audio_b64 must be a non-empty base64 string')
    try:
        audio = base64.b64decode(audio_b64.encode('ascii'), validate=True)
    except (UnicodeEncodeError, binascii.Error) as exc:
        raise ProtocolError('audio_b64 is not valid base64') from exc
    if len(audio) > MAX_AUDIO_BYTES:
        raise ProtocolError('audio exceeds its byte bound')
    if base64.b64encode(audio).decode('ascii') != audio_b64:
        raise ProtocolError('audio_b64 is not canonical')
    return audio


def _decode_request(envelope: dict[str, Any]) -> Request:
    request_id, operation = _validate_common(
        envelope, _REQUEST_FIELDS, MAX_REQUEST_ENVELOPE_FIELDS
    )
    payload = envelope.get('payload')
    if not isinstance(payload, dict):
        raise ProtocolError('request payload must be an object')
    if operation == 'health':
        if payload:
            raise ProtocolError('health payload must be empty')
        typed_payload: HealthRequest | TranscribeRequest = HealthRequest()
    else:
        if set(payload) - {'audio_b64', 'language'} or 'audio_b64' not in payload:
            raise ProtocolError('transcribe payload fields are not allowlisted')
        language = payload.get('language')
        if language is not None:
            language = _validate_language(language)
        typed_payload = TranscribeRequest(_decode_audio(payload['audio_b64']), language)
    return Request(request_id, operation, typed_payload)


def encode_request(request_id: str, request: HealthRequest | TranscribeRequest) -> bytes:
    """Encode one typed local-only request into a 4-byte length-prefixed frame."""
    request_id = _validate_request_id(request_id)
    if isinstance(request, HealthRequest):
        operation = 'health'
        payload: dict[str, Any] = {}
    elif isinstance(request, TranscribeRequest):
        operation = 'transcribe'
        if not isinstance(request.audio, bytes):
            raise ProtocolError('audio must be bytes')
        if len(request.audio) > MAX_AUDIO_BYTES:
            raise ProtocolError('audio exceeds its byte bound')
        payload = {'audio_b64': base64.b64encode(request.audio).decode('ascii')}
        if request.language is not None:
            payload['language'] = _validate_language(request.language)
    else:
        raise ProtocolError('request type is not allowlisted')
    return _pack_envelope(
        {
            'version': PROTOCOL_VERSION,
            'local_only': LOCAL_ONLY,
            'request_id': request_id,
            'operation': operation,
            'payload': payload,
        }
    )


def decode_request_frame(frame: bytes) -> Request:
    """Decode and validate one complete local-only request frame."""
    return _decode_request(_unpack_frame(frame))


def _validate_status(status: Any) -> str:
    status = _bounded_text(status, 'status', 32)
    if status not in {'ok', 'degraded'}:
        raise ProtocolError('status is not allowlisted')
    return status


def _validate_error(error: Any) -> ErrorResponse:
    if not isinstance(error, dict) or set(error) != {'code'}:
        raise ProtocolError('error fields are not exactly allowlisted')
    code = _bounded_text(error.get('code'), 'error code', MAX_ERROR_CODE_BYTES)
    if code not in _ALLOWED_ERROR_CODES:
        raise ProtocolError('error code is not allowlisted')
    return ErrorResponse(code)


def _decode_response(envelope: dict[str, Any]) -> Response:
    request_id, operation = _validate_common(
        envelope, _RESPONSE_FIELDS, MAX_RESPONSE_ENVELOPE_FIELDS
    )
    if not isinstance(envelope.get('ok'), bool):
        raise ProtocolError('response ok must be boolean')
    result = envelope.get('result')
    error = envelope.get('error')
    if envelope['ok']:
        if error is not None or not isinstance(result, dict):
            raise ProtocolError('successful response must contain only a result')
        if operation == 'health':
            if set(result) != {'status'}:
                raise ProtocolError('health result fields are not allowlisted')
            typed_result: HealthResponse | TranscribeResponse = HealthResponse(
                _validate_status(result['status'])
            )
        else:
            if set(result) != {'text'}:
                raise ProtocolError('transcribe result fields are not allowlisted')
            typed_result = TranscribeResponse(
                _bounded_text(result['text'], 'text', MAX_TEXT_BYTES)
            )
        typed_error = None
    else:
        if result is not None or error is None:
            raise ProtocolError('failed response must contain only an error')
        typed_result = None
        typed_error = _validate_error(error)
    return Response(request_id, operation, envelope['ok'], typed_result, typed_error)


def encode_response(
    request_id: str,
    operation: str,
    response: HealthResponse | TranscribeResponse | ErrorResponse,
) -> bytes:
    """Encode one typed local-only response; response bodies cannot carry audio."""
    request_id = _validate_request_id(request_id)
    operation = _validate_operation(operation)
    if isinstance(response, ErrorResponse):
        error = {
            'code': _bounded_text(response.code, 'error code', MAX_ERROR_CODE_BYTES),
        }
        if error['code'] not in _ALLOWED_ERROR_CODES:
            raise ProtocolError('error code is not allowlisted')
        result = None
        ok = False
    elif isinstance(response, HealthResponse) and operation == 'health':
        result = {'status': _validate_status(response.status)}
        error = None
        ok = True
    elif isinstance(response, TranscribeResponse) and operation == 'transcribe':
        result = {'text': _bounded_text(response.text, 'text', MAX_TEXT_BYTES)}
        error = None
        ok = True
    else:
        raise ProtocolError('response type does not match operation')
    return _pack_envelope(
        {
            'version': PROTOCOL_VERSION,
            'local_only': LOCAL_ONLY,
            'request_id': request_id,
            'operation': operation,
            'ok': ok,
            'result': result,
            'error': error,
        }
    )


def decode_response_frame(frame: bytes) -> Response:
    """Decode and validate one complete local-only response frame."""
    return _decode_response(_unpack_frame(frame))


__all__ = [
    'ErrorResponse',
    'FRAME_LENGTH_BYTES',
    'HealthRequest',
    'HealthResponse',
    'LOCAL_ONLY',
    'MAX_AUDIO_BYTES',
    'MAX_FRAME_BYTES',
    'MAX_REQUEST_ID_BYTES',
    'PROTOCOL_VERSION',
    'ProtocolError',
    'TranscribeRequest',
    'TranscribeResponse',
    'decode_request_frame',
    'decode_response_frame',
    'encode_request',
    'encode_response',
]
