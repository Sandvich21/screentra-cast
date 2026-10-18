#!/usr/bin/env python3
"""Pure, fail-closed composition boundary for future STT runtime admission.

This module consumes already-produced preflight, model-verification, and
supervisor-plan metadata. It performs no preflight, model verification,
process work, hardware access, audio access, filesystem mutation, network work,
or runtime activation. Even an approved future gate never enables execution.
"""
from __future__ import annotations

import json
import re
from collections.abc import Mapping
from typing import Any, Final


MAX_REQUEST_BYTES: Final = 32 * 1024
MAX_COMPONENT_BYTES: Final = 16 * 1024
MAX_IDENTIFIER_BYTES: Final = 128
MAX_VERSION_BYTES: Final = 64
MAX_MODEL_ID_BYTES: Final = 64
MAX_HOST_ID_BYTES: Final = 64
MAX_RESOURCE_BYTES: Final = 1 << 60
MAX_MODEL_SIZE_BYTES: Final = 1 << 40

_IDENTIFIER_RE: Final = re.compile(r'^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$')
_VERSION_RE: Final = re.compile(
    r'^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)'
    r'(?:-[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?$'
)
_MODEL_ID_RE: Final = re.compile(r'^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$')
_SHA256_RE: Final = re.compile(r'^[0-9a-f]{64}$')
_PATH_PART_RE: Final = re.compile(r'^[A-Za-z0-9][A-Za-z0-9._-]*$')
_SECRET_RE: Final = re.compile(
    r'(?:password|passwd|secret|token|credential|bearer|api[_-]?key)',
    re.IGNORECASE,
)

_REQUEST_FIELDS: Final = frozenset({
    'service_id', 'service_version', 'activation_id', 'host_id',
    'model_id', 'model_sha256', 'preflight_evidence',
    'model_staging_plan', 'supervisor_plan', 'approved_activation_gate',
})
_REQUIRED_REQUEST_FIELDS: Final = _REQUEST_FIELDS - {'service_version', 'approved_activation_gate'}
_PREFLIGHT_FIELDS: Final = frozenset({
    'evidence_type', 'evaluation', 'live_preflight', 'readiness_claim',
    'evidence', 'blockers', 'warnings', 'execution_enabled', 'runtime_ready',
    'hardware_accessed', 'model_staged',
})
_PREFLIGHT_EVIDENCE_FIELDS: Final = frozenset({
    'host_id', 'architecture', 'cpu_count', 'memory_bytes', 'free_bytes',
    'audio_capture_present', 'pipewire_active', 'wireplumber_active',
    'model_present', 'model_id', 'model_sha256', 'local_runtime_present',
    'camera_present',
})
_PREFLIGHT_REQUIRED_EVIDENCE_FIELDS: Final = _PREFLIGHT_EVIDENCE_FIELDS - {'camera_present'}
_MODEL_PLAN_FIELDS: Final = frozenset({
    'plan_type', 'intent', 'service_id', 'activation_id', 'model_id',
    'model_sha256', 'approved_model_root', 'model_path', 'ok', 'code',
    'effect', 'execution_enabled', 'downloaded', 'staged', 'loaded',
    'process_started', 'network_used', 'verification_plan', 'stage_plan',
})
_SUPERVISOR_FIELDS: Final = frozenset({
    'plan_type', 'intent', 'service_id', 'service_version', 'activation_id',
    'socket_name', 'model_id', 'model_sha256', 'lifecycle_state',
    'lifecycle_state_changed', 'execution_enabled', 'effect', 'process_started',
    'socket_bound', 'model_loaded', 'ok', 'code', 'observation', 'result',
})
_SUPERVISOR_RESULT_FIELDS: Final = frozenset({
    'ok', 'code', 'execution_enabled', 'effect', 'lifecycle_state',
    'lifecycle_state_changed', 'process_started', 'socket_bound', 'model_loaded',
})
_VERIFICATION_FIELDS: Final = frozenset({'read_only', 'verified', 'model_path', 'size_bytes'})
_STAGE_FIELDS: Final = frozenset({
    'effect', 'execution_enabled', 'requested_intent', 'filesystem_touched',
})
_GATE_FIELDS: Final = frozenset({
    'gate_type', 'approved', 'service_id', 'service_version', 'activation_id',
    'host_id', 'model_id', 'model_sha256', 'execution_enabled',
})
_GATE_REQUIRED_FIELDS: Final = _GATE_FIELDS - {'service_version'}
class RuntimeAdmissionError(ValueError):
    """Raised when composed evidence is malformed or not identity-bound."""


def _fail(message: str = 'runtime admission evidence is invalid') -> RuntimeAdmissionError:
    return RuntimeAdmissionError(message)


def _bounded_json(value: Any, limit: int, message: str) -> None:
    try:
        encoded = json.dumps(
            value, ensure_ascii=True, allow_nan=False, separators=(',', ':')
        ).encode('utf-8')
    except (TypeError, ValueError, UnicodeEncodeError) as exc:
        raise _fail(message) from exc
    if len(encoded) > limit:
        raise _fail(message)


def _mapping(value: Any, fields: frozenset[str], message: str) -> dict[str, Any]:
    if not isinstance(value, Mapping):
        raise _fail(message)
    result = dict(value)
    if set(result) != fields:
        raise _fail(message)
    return result


def _identifier(value: Any, field: str, maximum: int = MAX_IDENTIFIER_BYTES) -> str:
    if (
        not isinstance(value, str)
        or not value
        or len(value.encode('ascii', errors='ignore')) != len(value)
        or len(value.encode('utf-8')) > maximum
        or _IDENTIFIER_RE.fullmatch(value) is None
        or _SECRET_RE.search(value) is not None
    ):
        raise _fail(f'{field} is invalid')
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


def _digest(value: Any) -> str:
    if not isinstance(value, str) or _SHA256_RE.fullmatch(value) is None:
        raise _fail('model_sha256 is invalid')
    return value


def _boolean(value: Any, field: str, expected: bool | None = None) -> bool:
    if not isinstance(value, bool) or (expected is not None and value is not expected):
        raise _fail(f'{field} is invalid')
    return value


def _integer(value: Any, field: str, minimum: int, maximum: int) -> int:
    if isinstance(value, bool) or not isinstance(value, int) or not minimum <= value <= maximum:
        raise _fail(f'{field} is invalid')
    return value


def _safe_root(value: Any) -> str:
    if not isinstance(value, str) or not value or len(value.encode('utf-8')) > 256:
        raise _fail('model verification path is invalid')
    if '\x00' in value or '\\' in value or '//' in value or '?' in value or '#' in value:
        raise _fail('model verification path is invalid')
    parts = value.split('/')
    if not value.startswith('/') or any(part in {'', '.', '..'} for part in parts[1:]):
        raise _fail('model verification path is invalid')
    return value


def _safe_relative_path(value: Any) -> str:
    if not isinstance(value, str) or not value or len(value.encode('utf-8')) > 256:
        raise _fail('model verification path is invalid')
    parts = value.split('/')
    if (
        '\x00' in value or '\\' in value or '//' in value or '?' in value or '#' in value
        or value.startswith('/')
        or any(part in {'', '.', '..'} or _PATH_PART_RE.fullmatch(part) is None for part in parts)
    ):
        raise _fail('model verification path is invalid')
    return value


def _validate_preflight(value: Any, expected: dict[str, str]) -> None:
    plan = _mapping(value, _PREFLIGHT_FIELDS, 'preflight evidence is malformed')
    _bounded_json(plan, MAX_COMPONENT_BYTES, 'preflight evidence is malformed')
    if plan['evidence_type'] != 'stt_client_preflight_evidence' or plan['evaluation'] != 'evidence_only':
        raise _fail('preflight evidence is not the required contract')
    for field in ('live_preflight', 'readiness_claim', 'execution_enabled', 'runtime_ready', 'hardware_accessed', 'model_staged'):
        _boolean(plan[field], f'preflight.{field}', False)
    if plan['blockers'] != [] or not isinstance(plan['warnings'], list):
        raise _fail('preflight blockers are not empty')
    if any(not isinstance(item, str) for item in plan['warnings']):
        raise _fail('preflight warnings are malformed')

    if not isinstance(plan['evidence'], Mapping):
        raise _fail('preflight evidence is malformed')
    evidence = dict(plan['evidence'])
    if (
        set(evidence) - _PREFLIGHT_EVIDENCE_FIELDS
        or not _PREFLIGHT_REQUIRED_EVIDENCE_FIELDS.issubset(evidence)
    ):
        raise _fail('preflight evidence is malformed')
    _bounded_json(evidence, MAX_COMPONENT_BYTES, 'preflight evidence is malformed')
    _identifier(evidence['host_id'], 'host_id', MAX_HOST_ID_BYTES)
    if evidence['architecture'] not in {'amd64', 'arm64'}:
        raise _fail('preflight architecture is invalid')
    _integer(evidence['cpu_count'], 'preflight.cpu_count', 1, 256)
    memory = _integer(evidence['memory_bytes'], 'preflight.memory_bytes', 1, MAX_RESOURCE_BYTES)
    free = _integer(evidence['free_bytes'], 'preflight.free_bytes', 0, MAX_RESOURCE_BYTES)
    if free > memory:
        raise _fail('preflight memory evidence is invalid')
    for field in (
        'audio_capture_present', 'pipewire_active', 'wireplumber_active',
        'model_present', 'local_runtime_present',
    ):
        _boolean(evidence[field], f'preflight.evidence.{field}', True)
    if 'camera_present' in evidence:
        _boolean(evidence['camera_present'], 'preflight.evidence.camera_present')
    _model_id(evidence['model_id'])
    _digest(evidence['model_sha256'])
    if evidence['host_id'] != expected['host_id']:
        raise _fail('preflight host identity mismatch')
    if evidence['model_id'] != expected['model_id'] or evidence['model_sha256'] != expected['model_sha256']:
        raise _fail('preflight model identity mismatch')


def _validate_model_plan(value: Any, expected: dict[str, str]) -> None:
    plan = _mapping(value, _MODEL_PLAN_FIELDS, 'model staging evidence is malformed')
    _bounded_json(plan, MAX_COMPONENT_BYTES, 'model staging evidence is malformed')
    if (
        plan['plan_type'] != 'local_model_staging_plan'
        or plan['intent'] != 'verify'
        or plan['service_id'] != expected['service_id']
        or plan['activation_id'] != expected['activation_id']
        or plan['model_id'] != expected['model_id']
        or plan['model_sha256'] != expected['model_sha256']
        or plan['ok'] is not True
        or plan['code'] != 'verified'
        or plan['effect'] != 'metadata_only'
    ):
        raise _fail('model staging identity or verification is invalid')
    _safe_root(plan['approved_model_root'])
    model_path = _safe_relative_path(plan['model_path'])
    _boolean(plan['execution_enabled'], 'model.execution_enabled', False)
    for field in ('downloaded', 'staged', 'loaded', 'process_started', 'network_used'):
        _boolean(plan[field], f'model.{field}', False)
    verification = _mapping(plan['verification_plan'], _VERIFICATION_FIELDS, 'model verification evidence is malformed')
    _boolean(verification['read_only'], 'model.verification.read_only', True)
    _boolean(verification['verified'], 'model.verification.verified', True)
    if _safe_relative_path(verification['model_path']) != model_path:
        raise _fail('model verification path mismatch')
    _integer(verification['size_bytes'], 'model.verification.size_bytes', 0, MAX_MODEL_SIZE_BYTES)
    stage = _mapping(plan['stage_plan'], _STAGE_FIELDS, 'model stage evidence is malformed')
    if stage['effect'] != 'metadata_only' or stage['requested_intent'] != 'verify':
        raise _fail('model stage evidence is not metadata-only')
    _boolean(stage['execution_enabled'], 'model.stage.execution_enabled', False)
    _boolean(stage['filesystem_touched'], 'model.stage.filesystem_touched', False)


def _validate_observation(value: Any, expected: dict[str, str]) -> None:
    if value is None:
        return
    fields = frozenset({
        'service_id', 'service_version', 'activation_id', 'status',
        'model_id', 'model_sha256',
    })
    observation = _mapping(value, fields, 'supervisor observation is malformed')
    if (
        observation['service_id'] != expected['service_id']
        or observation['service_version'] != expected['service_version']
        or observation['activation_id'] != expected['activation_id']
        or observation['status'] not in {'ok', 'degraded'}
        or observation['model_id'] != expected['model_id']
        or observation['model_sha256'] != expected['model_sha256']
    ):
        raise _fail('supervisor observation identity mismatch')
    _identifier(observation['service_id'], 'observation.service_id')
    _version(observation['service_version'])
    _identifier(observation['activation_id'], 'observation.activation_id')
    _model_id(observation['model_id'])
    _digest(observation['model_sha256'])


def _validate_supervisor(value: Any, expected: dict[str, str]) -> None:
    plan = _mapping(value, _SUPERVISOR_FIELDS, 'supervisor evidence is malformed')
    _bounded_json(plan, MAX_COMPONENT_BYTES, 'supervisor evidence is malformed')
    if (
        plan['plan_type'] != 'client_service_activation_plan'
        or plan['intent'] not in {'health', 'status'}
        or plan['service_id'] != expected['service_id']
        or plan['service_version'] != expected['service_version']
        or plan['activation_id'] != expected['activation_id']
        or plan['model_id'] != expected['model_id']
        or plan['model_sha256'] != expected['model_sha256']
        or plan['ok'] is not True
        or plan['code'] != 'metadata_only'
        or plan['effect'] != 'metadata_only'
    ):
        raise _fail('supervisor identity or metadata state is invalid')
    _identifier(plan['service_id'], 'supervisor.service_id')
    _version(plan['service_version'])
    _identifier(plan['activation_id'], 'supervisor.activation_id')
    _identifier(plan['socket_name'], 'supervisor.socket_name')
    _model_id(plan['model_id'])
    _digest(plan['model_sha256'])
    _boolean(plan['lifecycle_state_changed'], 'supervisor.lifecycle_state_changed', False)
    for field in ('execution_enabled', 'process_started', 'socket_bound', 'model_loaded'):
        _boolean(plan[field], f'supervisor.{field}', False)
    if not isinstance(plan['lifecycle_state'], str) or not plan['lifecycle_state']:
        raise _fail('supervisor lifecycle state is invalid')
    result = _mapping(plan['result'], _SUPERVISOR_RESULT_FIELDS, 'supervisor result is malformed')
    for field in ('execution_enabled', 'lifecycle_state_changed', 'process_started', 'socket_bound', 'model_loaded'):
        _boolean(result[field], f'supervisor.result.{field}', False)
    if result['ok'] is not True or result['code'] != 'metadata_only' or result['effect'] != 'metadata_only':
        raise _fail('supervisor result is not metadata-only')
    if result['lifecycle_state'] != plan['lifecycle_state']:
        raise _fail('supervisor lifecycle state mismatch')
    _validate_observation(plan['observation'], expected)


def _validate_gate(value: Any, expected: dict[str, str]) -> None:
    if not isinstance(value, Mapping):
        raise _fail('approved activation gate is malformed')
    gate = dict(value)
    if set(gate) - _GATE_FIELDS or not _GATE_REQUIRED_FIELDS.issubset(gate):
        raise _fail('approved activation gate is malformed')
    _bounded_json(gate, MAX_COMPONENT_BYTES, 'approved activation gate is malformed')
    if (
        gate['gate_type'] != 'approved_stt_runtime_activation_gate'
        or gate['approved'] is not True
        or gate['service_id'] != expected['service_id']
        or gate['activation_id'] != expected['activation_id']
        or gate['host_id'] != expected['host_id']
        or gate['model_id'] != expected['model_id']
        or gate['model_sha256'] != expected['model_sha256']
    ):
        raise _fail('approved activation gate identity is invalid')
    if 'service_version' in gate and gate['service_version'] != expected['service_version']:
        raise _fail('approved activation gate version mismatch')
    _identifier(gate['service_id'], 'gate.service_id')
    if 'service_version' in gate:
        _version(gate['service_version'])
    _identifier(gate['activation_id'], 'gate.activation_id')
    _identifier(gate['host_id'], 'gate.host_id', MAX_HOST_ID_BYTES)
    _model_id(gate['model_id'])
    _digest(gate['model_sha256'])
    _boolean(gate['execution_enabled'], 'gate.execution_enabled', False)


def decide_runtime_admission(request: Mapping[str, Any]) -> dict[str, Any]:
    """Compose independent evidence into a non-executing admission decision."""
    if not isinstance(request, Mapping):
        raise _fail('runtime admission request must be an object')
    request = dict(request)
    if set(request) - _REQUEST_FIELDS or not _REQUIRED_REQUEST_FIELDS.issubset(request):
        raise _fail('runtime admission request fields are not allowlisted')
    _bounded_json(request, MAX_REQUEST_BYTES, 'runtime admission request is not bounded')

    expected = {
        'service_id': _identifier(request['service_id'], 'service_id'),
        'activation_id': _identifier(request['activation_id'], 'activation_id'),
        'host_id': _identifier(request['host_id'], 'host_id', MAX_HOST_ID_BYTES),
        'model_id': _model_id(request['model_id']),
        'model_sha256': _digest(request['model_sha256']),
    }
    if 'service_version' in request:
        expected['service_version'] = _version(request['service_version'])
    else:
        expected['service_version'] = None

    _validate_preflight(request['preflight_evidence'], expected)
    _validate_model_plan(request['model_staging_plan'], expected)

    supervisor = request['supervisor_plan']
    if not isinstance(supervisor, Mapping) or 'service_version' not in supervisor:
        raise _fail('supervisor service version is required')
    supervisor_version = _version(supervisor['service_version'])
    if expected['service_version'] is None:
        expected['service_version'] = supervisor_version
    elif expected['service_version'] != supervisor_version:
        raise _fail('service version mismatch')
    _validate_supervisor(supervisor, expected)

    gate_present = 'approved_activation_gate' in request
    if gate_present:
        _validate_gate(request['approved_activation_gate'], expected)

    approved = gate_present
    return {
        'decision_type': 'stt_runtime_admission',
        'admission': 'approved' if approved else 'denied',
        'reason': 'approved_activation_gate_present' if approved else 'approved_activation_gate_missing',
        'runtime_activation_allowed': approved,
        'execution_enabled': False,
        'service_id': expected['service_id'],
        'service_version': expected['service_version'],
        'activation_id': expected['activation_id'],
        'host_id': expected['host_id'],
        'model_id': expected['model_id'],
        'model_sha256': expected['model_sha256'],
        'evidence_composed': {
            'preflight': True,
            'model_verification': True,
            'supervisor': True,
            'approved_activation_gate': approved,
        },
    }


# Descriptive aliases for callers using evaluator/admission terminology.
evaluate_runtime_admission = decide_runtime_admission
evaluate_stt_runtime_admission = decide_runtime_admission
decide_stt_runtime_admission = decide_runtime_admission
admit_stt_runtime = decide_runtime_admission


__all__ = [
    'MAX_COMPONENT_BYTES',
    'MAX_REQUEST_BYTES',
    'RuntimeAdmissionError',
    'admit_stt_runtime',
    'decide_runtime_admission',
    'decide_stt_runtime_admission',
    'evaluate_runtime_admission',
    'evaluate_stt_runtime_admission',
]
