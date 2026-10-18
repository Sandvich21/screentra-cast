#!/usr/bin/env python3
"""Pure binding for metadata-only client-local AI health observations.

This module accepts only a bounded health observation and verifies its service
and optional model identity against caller-supplied expectations. It does not
change lifecycle state, inspect model files, access credentials or audio, or
perform any runtime operation.
"""
from __future__ import annotations

import json
import re
from collections.abc import Mapping
from dataclasses import dataclass
from typing import Any, ClassVar, Final


MAX_IDENTIFIER_BYTES: Final = 128
MAX_VERSION_BYTES: Final = 64
MAX_MODEL_ID_BYTES: Final = 64
SHA256_HEX_BYTES: Final = 64
MAX_OBSERVATION_BYTES: Final = 8192
MAX_ERROR_BYTES: Final = 128

_IDENTIFIER_RE = re.compile(r'^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$')
_VERSION_RE = re.compile(
    r'^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)'
    r'(?:-[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?$'
)
_MODEL_ID_RE = re.compile(r'^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$')
_SHA256_RE = re.compile(r'^[0-9a-f]{64}$')
_ALLOWED_FIELDS = frozenset({
    'service_id', 'service_version', 'activation_id', 'status',
    'model_id', 'model_sha256',
})
_REQUIRED_FIELDS = frozenset({'service_id', 'service_version', 'activation_id', 'status'})


class HealthObservationError(ValueError):
    """Raised when a health observation violates the binding contract."""


@dataclass(frozen=True)
class HealthObservation:
    """Validated health metadata; never a claim about runtime or disk state."""

    service_id: str
    service_version: str
    activation_id: str
    status: str
    model_id: str | None = None
    model_sha256: str | None = None

    metadata_only: ClassVar[bool] = True
    effect: ClassVar[str] = 'metadata_only'

    def as_dict(self) -> dict[str, str]:
        """Return only the allowlisted observed metadata fields."""
        result: dict[str, str] = {
            'service_id': self.service_id,
            'service_version': self.service_version,
            'activation_id': self.activation_id,
            'status': self.status,
        }
        if self.model_id is not None:
            result['model_id'] = self.model_id
            assert self.model_sha256 is not None
            result['model_sha256'] = self.model_sha256
        return result

    def to_json(self) -> str:
        """Serialize the allowlisted observation deterministically."""
        return json.dumps(self.as_dict(), ensure_ascii=True, separators=(',', ':'), sort_keys=True)


def _fail(message: str) -> HealthObservationError:
    # Keep every externally visible error generic and bounded; never include
    # observed or expected identity values in an error.
    if len(message.encode('ascii')) > MAX_ERROR_BYTES:
        message = 'health observation is invalid'
    return HealthObservationError(message)


def _text(value: Any, field: str, maximum: int) -> str:
    if not isinstance(value, str) or not value:
        raise _fail(f'health observation {field} is invalid')
    try:
        size = len(value.encode('utf-8'))
    except UnicodeEncodeError as exc:
        raise _fail(f'health observation {field} is invalid') from exc
    if size > maximum or any(ord(char) < 32 or ord(char) == 127 for char in value):
        raise _fail(f'health observation {field} is invalid')
    return value


def _identifier(value: Any, field: str) -> str:
    value = _text(value, field, MAX_IDENTIFIER_BYTES)
    if _IDENTIFIER_RE.fullmatch(value) is None:
        raise _fail(f'health observation {field} is invalid')
    return value


def _version(value: Any) -> str:
    value = _text(value, 'service_version', MAX_VERSION_BYTES)
    if _VERSION_RE.fullmatch(value) is None:
        raise _fail('health observation service_version is invalid')
    return value


def _model_id(value: Any) -> str:
    if not isinstance(value, str):
        raise _fail('health observation model identity is invalid')
    try:
        size = len(value.encode('ascii'))
    except UnicodeEncodeError as exc:
        raise _fail('health observation model identity is invalid') from exc
    if size > MAX_MODEL_ID_BYTES or _MODEL_ID_RE.fullmatch(value) is None:
        raise _fail('health observation model identity is invalid')
    return value


def _model_sha256(value: Any) -> str:
    if not isinstance(value, str) or _SHA256_RE.fullmatch(value) is None:
        raise _fail('health observation model identity is invalid')
    return value


def _model_pair(model_id: Any, model_sha256: Any, *, present: bool) -> tuple[str | None, str | None]:
    if not present:
        return None, None
    if model_id is None or model_sha256 is None:
        raise _fail('health observation model identity is incomplete')
    return _model_id(model_id), _model_sha256(model_sha256)


def _decode_observation(value: Any) -> dict[str, Any]:
    if isinstance(value, Mapping):
        candidate = dict(value)
    elif isinstance(value, bytes):
        if len(value) > MAX_OBSERVATION_BYTES:
            raise _fail('health observation exceeds its bound')
        try:
            text = value.decode('utf-8')
        except UnicodeDecodeError as exc:
            raise _fail('health observation is invalid') from exc
        candidate = _load_json(text)
    elif isinstance(value, str):
        try:
            if len(value.encode('utf-8')) > MAX_OBSERVATION_BYTES:
                raise _fail('health observation exceeds its bound')
        except UnicodeEncodeError as exc:
            raise _fail('health observation is invalid') from exc
        candidate = _load_json(value)
    else:
        raise _fail('health observation must be an object')
    if not isinstance(candidate, dict):
        raise _fail('health observation must be an object')
    return candidate


def _load_json(text: str) -> dict[str, Any]:
    def reject_duplicate_fields(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
        result: dict[str, Any] = {}
        for key, value in pairs:
            if key in result:
                raise ValueError
            result[key] = value
        return result

    try:
        value = json.loads(
            text,
            object_pairs_hook=reject_duplicate_fields,
            parse_constant=lambda _: (_ for _ in ()).throw(ValueError()),
        )
    except (TypeError, ValueError, json.JSONDecodeError) as exc:
        raise _fail('health observation is invalid') from exc
    if not isinstance(value, dict):
        raise _fail('health observation must be an object')
    return value


def _validate_expected_model(
    expected_model_id: str | None, expected_model_sha256: str | None,
) -> tuple[str | None, str | None]:
    if (expected_model_id is None) != (expected_model_sha256 is None):
        raise _fail('expected model identity is incomplete')
    if expected_model_id is None:
        return None, None
    return _model_id(expected_model_id), _model_sha256(expected_model_sha256)


def bind_health_observation(
    observation: Mapping[str, Any] | bytes | str,
    *,
    expected_service_id: str,
    expected_service_version: str,
    expected_activation_id: str,
    expected_model_id: str | None = None,
    expected_model_sha256: str | None = None,
) -> HealthObservation:
    """Validate and bind one decoded or JSON health observation.

    Identity mismatches fail before a :class:`HealthObservation` is returned;
    this helper has no lifecycle state to mutate and makes no runtime claims.
    """
    expected_service_id = _identifier(expected_service_id, 'expected service identity')
    expected_service_version = _version(expected_service_version)
    expected_activation_id = _identifier(expected_activation_id, 'expected activation identity')
    expected_model_id, expected_model_sha256 = _validate_expected_model(
        expected_model_id, expected_model_sha256
    )

    observed = _decode_observation(observation)
    if not _REQUIRED_FIELDS.issubset(observed) or set(observed) - _ALLOWED_FIELDS:
        raise _fail('health observation fields are not allowlisted')
    service_id = _identifier(observed['service_id'], 'service identity')
    service_version = _version(observed['service_version'])
    activation_id = _identifier(observed['activation_id'], 'activation identity')
    status = observed['status']
    if not isinstance(status, str) or status not in {'ok', 'degraded'}:
        raise _fail('health observation status is not allowlisted')

    model_fields_present = 'model_id' in observed or 'model_sha256' in observed
    model_id, model_sha256 = _model_pair(
        observed.get('model_id'), observed.get('model_sha256'), present=model_fields_present
    )

    if (
        service_id != expected_service_id
        or service_version != expected_service_version
        or activation_id != expected_activation_id
    ):
        raise _fail('health observation service identity mismatch')
    if expected_model_id is not None and (
        model_id != expected_model_id or model_sha256 != expected_model_sha256
    ):
        raise _fail('health observation model identity mismatch')

    return HealthObservation(
        service_id, service_version, activation_id, status, model_id, model_sha256
    )


__all__ = [
    'HealthObservation',
    'HealthObservationError',
    'MAX_ERROR_BYTES',
    'MAX_MODEL_ID_BYTES',
    'MAX_OBSERVATION_BYTES',
    'SHA256_HEX_BYTES',
    'bind_health_observation',
]
