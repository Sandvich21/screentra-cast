#!/usr/bin/env python3
"""Pure, inert planning contract for a local model.

The contract validates model identity and safe declarative paths.  ``verify``
may perform the existing read-only integrity check for a caller-supplied local
file; stage, download, and load remain structured disabled results.  This
module never writes, downloads, loads, executes, starts a process, or uses a
network.
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
MAX_MODEL_PATH_BYTES: Final = 256
MAX_MODEL_ID_BYTES: Final = 64
ALLOWED_INTENTS: Final = frozenset({'verify', 'stage', 'download', 'load'})
_ALLOWED_REQUEST_FIELDS: Final = frozenset({
    'service_id',
    'activation_id',
    'model_id',
    'model_sha256',
    'approved_model_root',
    'model_path',
    'intent',
})
_IDENTIFIER_RE: Final = re.compile(r'^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$')
_MODEL_ID_RE: Final = re.compile(r'^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$')
_SHA256_RE: Final = re.compile(r'^[0-9a-f]{64}$')
_MODEL_PATH_PART_RE: Final = re.compile(r'^[A-Za-z0-9][A-Za-z0-9._-]*$')
_URI_SCHEME_RE: Final = re.compile(r'^[A-Za-z][A-Za-z0-9+.-]*://')


class LocalModelStagingPlanError(ValueError):
    """Raised when a local-model staging-plan request is not trusted."""


def _load_integrity_module():
    module_name = 'screentra_local_model_integrity'
    existing = sys.modules.get(module_name)
    if existing is not None:
        return existing
    path = Path(__file__).with_name('screentra-local-model-integrity.py')
    spec = importlib.util.spec_from_file_location(module_name, path)
    if spec is None or spec.loader is None:
        raise ImportError('cannot load local model integrity contract')
    module = importlib.util.module_from_spec(spec)
    sys.modules[module_name] = module
    spec.loader.exec_module(module)
    return module


_integrity = _load_integrity_module()


def _fail(message: str) -> LocalModelStagingPlanError:
    return LocalModelStagingPlanError(message)


def _validate_request_size(request: Mapping[str, Any]) -> None:
    try:
        encoded = json.dumps(
            request,
            ensure_ascii=True,
            allow_nan=False,
            separators=(',', ':'),
        ).encode('utf-8')
    except (TypeError, ValueError, UnicodeEncodeError) as exc:
        raise _fail('model staging request is not bounded JSON') from exc
    if len(encoded) > MAX_REQUEST_BYTES:
        raise _fail('model staging request exceeds its bound')


def _identifier(value: Any, field: str) -> str:
    if (
        not isinstance(value, str)
        or len(value.encode('ascii', errors='ignore')) != len(value)
        or not value
        or len(value.encode('utf-8')) > 128
        or _IDENTIFIER_RE.fullmatch(value) is None
    ):
        raise _fail(f'{field} is malformed or exceeds its bound')
    return value


def _model_id(value: Any) -> str:
    if (
        not isinstance(value, str)
        or len(value.encode('ascii', errors='ignore')) != len(value)
        or not value
        or len(value.encode('utf-8')) > MAX_MODEL_ID_BYTES
        or _MODEL_ID_RE.fullmatch(value) is None
    ):
        raise _fail('model_id is malformed or exceeds its bound')
    return value


def _model_sha256(value: Any) -> str:
    if not isinstance(value, str) or _SHA256_RE.fullmatch(value) is None:
        raise _fail('model_sha256 must be an exact lowercase SHA-256 digest')
    return value


def _approved_root(value: Any) -> str:
    if not isinstance(value, str) or not value or len(value.encode('utf-8')) > MAX_MODEL_PATH_BYTES:
        raise _fail('approved_model_root is malformed or exceeds its bound')
    if (
        '\x00' in value
        or '\\' in value
        or '//' in value
        or '~' in value
        or '?' in value
        or '#' in value
        or _URI_SCHEME_RE.match(value) is not None
    ):
        raise _fail('approved_model_root contains an unsafe path form')
    path = Path(value)
    if not path.is_absolute() or any(part in {'.', '..'} for part in path.parts):
        raise _fail('approved_model_root must be an approved absolute path')
    return value


def _relative_model_path(value: Any) -> str:
    if not isinstance(value, str) or not value or len(value.encode('utf-8')) > MAX_MODEL_PATH_BYTES:
        raise _fail('model_path is malformed or exceeds its bound')
    if (
        '\x00' in value
        or '\\' in value
        or '//' in value
        or '~' in value
        or '?' in value
        or '#' in value
        or _URI_SCHEME_RE.match(value) is not None
    ):
        raise _fail('model_path contains an unsafe path form')
    path = Path(value)
    raw_parts = value.split('/')
    if path.is_absolute() or any(
        part in {'', '.', '..'} or _MODEL_PATH_PART_RE.fullmatch(part) is None
        for part in raw_parts
    ) or any('registry' in part.lower() for part in raw_parts):
        raise _fail('model_path must be a bounded relative path')
    return value


def _verification_plan(
    model_id: str,
    model_sha256: str,
    model_path: str,
    approved_model_root: str,
) -> dict[str, Any]:
    try:
        verified = _integrity.verify_model_file(
            model_id=model_id,
            expected_sha256=model_sha256,
            model_path=model_path,
            approved_model_root=approved_model_root,
        )
    except (_integrity.ModelIntegrityError, OSError, TypeError, ValueError) as exc:
        raise _fail('local model verification failed') from exc
    return {
        'read_only': True,
        'verified': True,
        'model_path': model_path,
        'size_bytes': verified.size_bytes,
    }


def build_model_staging_plan(request: Mapping[str, Any]) -> dict[str, Any]:
    """Validate one inert local-model plan and return metadata-only evidence."""
    if not isinstance(request, Mapping):
        raise _fail('model staging request must be a JSON object')
    if set(request) != _ALLOWED_REQUEST_FIELDS:
        raise _fail('model staging request fields are not allowlisted')
    _validate_request_size(request)

    service_id = _identifier(request['service_id'], 'service_id')
    activation_id = _identifier(request['activation_id'], 'activation_id')
    model_id = _model_id(request['model_id'])
    model_sha256 = _model_sha256(request['model_sha256'])
    approved_model_root = _approved_root(request['approved_model_root'])
    model_path = _relative_model_path(request['model_path'])
    intent = request['intent']
    if not isinstance(intent, str) or intent not in ALLOWED_INTENTS:
        raise _fail('model staging intent is not allowlisted')

    verification = {
        'read_only': True,
        'verified': False,
        'model_path': model_path,
    }
    if intent == 'verify':
        verification = _verification_plan(
            model_id,
            model_sha256,
            model_path,
            approved_model_root,
        )

    disabled = intent != 'verify'
    return {
        'plan_type': 'local_model_staging_plan',
        'intent': intent,
        'service_id': service_id,
        'activation_id': activation_id,
        'model_id': model_id,
        'model_sha256': model_sha256,
        'approved_model_root': approved_model_root,
        'model_path': model_path,
        'ok': not disabled,
        'code': 'execution_disabled' if disabled else 'verified',
        'effect': 'metadata_only',
        'execution_enabled': False,
        'downloaded': False,
        'staged': False,
        'loaded': False,
        'process_started': False,
        'network_used': False,
        'verification_plan': verification,
        'stage_plan': {
            'effect': 'metadata_only',
            'execution_enabled': False,
            'requested_intent': intent,
            'filesystem_touched': False,
        },
    }


plan_model_staging = build_model_staging_plan


__all__ = [
    'ALLOWED_INTENTS',
    'MAX_MODEL_ID_BYTES',
    'MAX_MODEL_PATH_BYTES',
    'MAX_REQUEST_BYTES',
    'LocalModelStagingPlanError',
    'build_model_staging_plan',
    'plan_model_staging',
]
