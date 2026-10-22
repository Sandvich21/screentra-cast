#!/usr/bin/env python3
"""Pure, inert activation-plan contract for the Magic Mirror local STT service.

The contract composes the existing lifecycle and health-observation identity
boundaries.  It returns bounded metadata plans only: it never opens a socket,
starts or stops a process, loads a model, reads audio, accesses credentials, or
changes lifecycle state.
"""
from __future__ import annotations

import importlib.util
import json
import re
import sys
from collections.abc import Mapping
from pathlib import Path
from typing import Any, Final


MAX_REQUEST_BYTES: Final = 16 * 1024
MAX_SERVICE_METADATA_BYTES: Final = 8 * 1024
MAX_SOCKET_NAME_BYTES: Final = 64

ALLOWED_INTENTS: Final = frozenset({
    'health', 'status', 'start', 'stop', 'update', 'rollback',
})
_METADATA_FIELDS: Final = frozenset({
    'id', 'image', 'image_digest', 'socket_name',
    'cpu_millis', 'memory_bytes', 'pids', 'writable_bytes',
    'model_id', 'model_sha256', 'service_version',
})
_REQUIRED_REQUEST_FIELDS: Final = frozenset({
    'service_id', 'service_version', 'activation_id', 'state',
    'intent', 'service_metadata',
})
_ALLOWED_REQUEST_FIELDS: Final = _REQUIRED_REQUEST_FIELDS | {'observation'}
_SOCKET_NAME_RE: Final = re.compile(r'^[a-z0-9][a-z0-9._-]{0,63}$')
_IMAGE_DIGEST_RE: Final = re.compile(r'^sha256:[0-9a-f]{64}$')


class SupervisorPlanError(ValueError):
    """Raised when a client-service activation-plan request is invalid."""


def _load_sibling(module_name: str, filename: str):
    existing = sys.modules.get(module_name)
    if existing is not None:
        return existing
    path = Path(__file__).with_name(filename)
    spec = importlib.util.spec_from_file_location(module_name, path)
    if spec is None or spec.loader is None:
        raise ImportError(f'cannot load contract dependency {filename}')
    module = importlib.util.module_from_spec(spec)
    sys.modules[module_name] = module
    spec.loader.exec_module(module)
    return module


_lifecycle = _load_sibling(
    'screentra_client_service_lifecycle',
    'screentra-client-service-lifecycle.py',
)
_health = _load_sibling(
    'screentra_local_ai_health_observation',
    'screentra-local-ai-health-observation.py',
)


def _fail(message: str) -> SupervisorPlanError:
    return SupervisorPlanError(message)


def _bounded_socket_name(value: Any) -> str:
    if (
        not isinstance(value, str)
        or not value
        or len(value.encode('utf-8')) > MAX_SOCKET_NAME_BYTES
        or _SOCKET_NAME_RE.fullmatch(value) is None
    ):
        raise _fail('socket_name is invalid or exceeds its bound')
    return value


def _validate_request_size(request: Mapping[str, Any]) -> None:
    try:
        encoded = json.dumps(
            request,
            ensure_ascii=True,
            allow_nan=False,
            separators=(',', ':'),
        ).encode('utf-8')
    except (TypeError, ValueError, UnicodeEncodeError) as exc:
        raise _fail('activation-plan request is not bounded JSON') from exc
    if len(encoded) > MAX_REQUEST_BYTES:
        raise _fail('activation-plan request exceeds its bound')


def _validate_service_metadata(value: Any) -> dict[str, Any]:
    if not isinstance(value, Mapping):
        raise _fail('service_metadata must be a JSON object')
    if set(value) - _METADATA_FIELDS:
        raise _fail('service_metadata contains an unsupported field')
    try:
        encoded = json.dumps(
            dict(value), ensure_ascii=True, allow_nan=False, separators=(',', ':')
        ).encode('utf-8')
    except (TypeError, ValueError, UnicodeEncodeError) as exc:
        raise _fail('service_metadata is not bounded JSON') from exc
    if len(encoded) > MAX_SERVICE_METADATA_BYTES:
        raise _fail('service_metadata exceeds its bound')

    required = {
        'id', 'image', 'image_digest', 'socket_name',
        'cpu_millis', 'memory_bytes', 'pids', 'writable_bytes',
        'model_id', 'model_sha256',
    }
    if not required.issubset(value):
        raise _fail('service_metadata is incomplete')
    metadata = dict(value)
    if not isinstance(metadata['id'], str):
        raise _fail('service metadata id is invalid')
    image = metadata['image']
    if (
        not isinstance(image, str)
        or not image
        or len(image) > 255
        or '..' in image
        or '://' in image
        or ':' in image
        or '@' in image
        or '//' in image
        or image.startswith('/')
        or image.endswith('/')
        or any(
            not (character.isascii() and (character.isalnum() or character in '._-/'))
            for character in image
        )
        or any(
            not component
            or component in ('.', '..')
            or not component[0].isascii()
            or not component[0].isalnum()
            or not component[-1].isascii()
            or not component[-1].isalnum()
            for component in image.split('/')
        )
    ):
        raise _fail('service image reference is invalid')
    if not isinstance(metadata['image_digest'], str) or _IMAGE_DIGEST_RE.fullmatch(
        metadata['image_digest']
    ) is None:
        raise _fail('service image_digest is invalid')
    socket_name = _bounded_socket_name(metadata['socket_name'])
    metadata['socket_name'] = socket_name

    resource_bounds = {
        'cpu_millis': (1, 4000),
        'memory_bytes': (64 * 1024 * 1024, 2 * 1024 * 1024 * 1024),
        'pids': (1, 256),
        'writable_bytes': (0, 512 * 1024 * 1024),
    }
    for field, (minimum, maximum) in resource_bounds.items():
        resource = metadata[field]
        if (
            isinstance(resource, bool)
            or not isinstance(resource, int)
            or not minimum <= resource <= maximum
        ):
            raise _fail(f'service metadata {field} is outside its bounded range')

    return metadata


def _bound_health(
    *,
    service_id: str,
    service_version: str,
    activation_id: str,
    model_id: str,
    model_sha256: str,
    observation: Any,
) -> dict[str, Any] | None:
    if observation is None:
        return None
    try:
        bound = _health.bind_health_observation(
            observation,
            expected_service_id=service_id,
            expected_service_version=service_version,
            expected_activation_id=activation_id,
            expected_model_id=model_id,
            expected_model_sha256=model_sha256,
        )
    except (TypeError, ValueError) as exc:
        raise _fail('health observation is invalid or identity-bound incorrectly') from exc
    return bound.as_dict()


def build_supervisor_activation_plan(request: Mapping[str, Any]) -> dict[str, Any]:
    """Validate one inert supervisor request and return a metadata-only plan.

    ``health`` and ``status`` may bind a supplied health observation.  The
    future mutating intents are accepted as named contract values, but remain
    structured ``execution_disabled`` results and preserve the supplied
    lifecycle state.  No branch performs runtime work.
    """
    if not isinstance(request, Mapping):
        raise _fail('activation-plan request must be a JSON object')
    if not _REQUIRED_REQUEST_FIELDS.issubset(request):
        raise _fail('activation-plan request is incomplete')
    if set(request) - _ALLOWED_REQUEST_FIELDS:
        raise _fail('activation-plan request contains an unsupported field')
    _validate_request_size(request)

    metadata = _validate_service_metadata(request['service_metadata'])
    service_id = request['service_id']
    service_version = request['service_version']
    activation_id = request['activation_id']
    state = request['state']
    intent = request['intent']

    try:
        record = _lifecycle.validate_service_record({
            'service_id': service_id,
            'service_version': service_version,
            'activation_id': activation_id,
            'state': state,
        })
    except (TypeError, ValueError) as exc:
        raise _fail('service lifecycle identity is invalid') from exc
    if metadata['id'] != record['service_id']:
        raise _fail('service identity mismatch')
    if 'service_version' in metadata and metadata['service_version'] != record['service_version']:
        raise _fail('service version mismatch')

    if not isinstance(intent, str) or intent not in ALLOWED_INTENTS:
        raise _fail('activation intent is not allowlisted')
    if intent not in {'health', 'status'} and 'observation' in request:
        raise _fail('health observations are only valid for health or status')

    model_id = metadata['model_id']
    model_sha256 = metadata['model_sha256']
    try:
        identity = _health.bind_health_observation(
            {
                'service_id': record['service_id'],
                'service_version': record['service_version'],
                'activation_id': record['activation_id'],
                'status': 'ok',
                'model_id': model_id,
                'model_sha256': model_sha256,
            },
            expected_service_id=record['service_id'],
            expected_service_version=record['service_version'],
            expected_activation_id=record['activation_id'],
            expected_model_id=model_id,
            expected_model_sha256=model_sha256,
        )
    except (TypeError, ValueError) as exc:
        raise _fail('service model identity is invalid') from exc

    observation = _bound_health(
        service_id=record['service_id'],
        service_version=record['service_version'],
        activation_id=record['activation_id'],
        model_id=identity.model_id,
        model_sha256=identity.model_sha256,
        observation=request.get('observation'),
    )
    metadata_only = intent in {'health', 'status'}
    code = 'metadata_only' if metadata_only else 'execution_disabled'
    result = {
        'ok': metadata_only,
        'code': code,
        'execution_enabled': False,
        'effect': 'metadata_only',
        'lifecycle_state': record['state'],
        'lifecycle_state_changed': False,
        'process_started': False,
        'socket_bound': False,
        'model_loaded': False,
    }
    return {
        'plan_type': 'client_service_activation_plan',
        'intent': intent,
        'service_id': record['service_id'],
        'service_version': record['service_version'],
        'activation_id': record['activation_id'],
        'socket_name': metadata['socket_name'],
        'model_id': identity.model_id,
        'model_sha256': identity.model_sha256,
        'lifecycle_state': record['state'],
        'lifecycle_state_changed': False,
        'execution_enabled': False,
        'effect': 'metadata_only',
        'process_started': False,
        'socket_bound': False,
        'model_loaded': False,
        'ok': metadata_only,
        'code': code,
        'observation': observation,
        'result': result,
    }


# Short alias for callers that prefer the contract's plan terminology.
plan_activation = build_supervisor_activation_plan


__all__ = [
    'ALLOWED_INTENTS',
    'MAX_REQUEST_BYTES',
    'MAX_SERVICE_METADATA_BYTES',
    'MAX_SOCKET_NAME_BYTES',
    'SupervisorPlanError',
    'build_supervisor_activation_plan',
    'plan_activation',
]
