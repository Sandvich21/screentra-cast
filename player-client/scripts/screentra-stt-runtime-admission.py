#!/usr/bin/env python3
"""Pure, fail-closed composition boundary for future STT runtime admission.

This module consumes already-produced preflight, model-verification, supervisor,
resource-policy, local-IPC-policy, and immutable approval-record
metadata. It performs no preflight, model
verification, process work, hardware access, audio access, filesystem mutation,
network work, or runtime activation. Even an approved future review result never
enables execution.
"""
from __future__ import annotations

import importlib.util
import json
import re
from collections.abc import Mapping
from pathlib import Path
import sys
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
_STT_CAPABILITIES: Final = frozenset({
    'microphone_capture',
    'local_inference',
    'client_service',
})
_CAPABILITY_RESULT_FIELDS: Final = frozenset({
    'client_id', 'addon_id', 'requested_capabilities', 'approved_capabilities',
    'capabilities', 'admitted', 'reason', 'execution_enabled', 'effect',
})

_REQUEST_FIELDS: Final = frozenset({
    'approval_record', 'capability_admission', 'preflight_evidence', 'model_staging_plan',
    'supervisor_plan', 'resource_policy_evidence', 'ipc_policy_evidence',
    'rollback_policy_evidence',
    'runtime_installation_evidence',
    'audio_permission_evidence',
    'resource_enforcement_evidence',
})
_REQUIRED_REQUEST_FIELDS: Final = _REQUEST_FIELDS
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


def _validate_supervisor(value: Any, expected: dict[str, str]) -> str:
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
    return plan['socket_name']


def _resource_policy_module() -> Any:
    module_name = 'screentra_stt_resource_policy_evidence'
    module = sys.modules.get(module_name)
    if module is not None:
        return module
    module_path = Path(__file__).with_name('screentra-stt-resource-policy-evidence.py')
    spec = importlib.util.spec_from_file_location(module_name, module_path)
    if spec is None or spec.loader is None:
        raise _fail('runtime admission resource policy is invalid')
    module = importlib.util.module_from_spec(spec)
    sys.modules[module_name] = module
    try:
        spec.loader.exec_module(module)
    except Exception as exc:
        sys.modules.pop(module_name, None)
        raise _fail('runtime admission resource policy is invalid') from exc
    return module


def _validated_resource_policy(value: Any, expected: dict[str, str]) -> Any:
    """Accept only the immutable result produced by the resource-policy validator."""
    module = _resource_policy_module()
    if type(value) is not module.STTResourcePolicyEvidence:
        raise _fail('runtime admission resource policy is invalid')
    try:
        raw = {
            'service_id': value.service_id,
            'activation_id': value.activation_id,
            'cpu_millis': value.cpu_millis,
            'memory_bytes': value.memory_bytes,
            'pids': value.pids,
            'writable_bytes': value.writable_bytes,
            'policy_version': value.policy_version,
        }
        validated = module.validate_resource_policy_evidence(
            raw,
            expected_service_id=expected['service_id'],
            expected_activation_id=expected['activation_id'],
        )
    except (AttributeError, TypeError, ValueError, KeyError) as exc:
        raise _fail('runtime admission resource policy is invalid') from exc
    if validated != value:
        raise _fail('runtime admission resource policy is invalid')
    if (
        value.policy_valid is not True
        or value.execution_enabled is not False
        or value.runtime_activation_allowed is not False
        or value.process_started is not False
        or value.resource_enforced is not False
        or value.effect != 'metadata_only'
    ):
        raise _fail('runtime admission resource policy is invalid')
    return value


def _ipc_policy_module() -> Any:
    module_name = 'screentra_stt_ipc_policy_evidence'
    module = sys.modules.get(module_name)
    if module is not None:
        return module
    module_path = Path(__file__).with_name('screentra-stt-ipc-policy-evidence.py')
    spec = importlib.util.spec_from_file_location(module_name, module_path)
    if spec is None or spec.loader is None:
        raise _fail('runtime admission IPC policy is invalid')
    module = importlib.util.module_from_spec(spec)
    sys.modules[module_name] = module
    try:
        spec.loader.exec_module(module)
    except Exception as exc:
        sys.modules.pop(module_name, None)
        raise _fail('runtime admission IPC policy is invalid') from exc
    return module


def _validated_ipc_policy(
    value: Any, expected: dict[str, str], expected_socket_name: str
) -> Any:
    """Accept only the immutable result produced by the IPC-policy validator."""
    module = _ipc_policy_module()
    if type(value) is not module.STTIPCPolicyEvidence:
        raise _fail('runtime admission IPC policy is invalid')
    try:
        raw = {
            'service_id': value.service_id,
            'activation_id': value.activation_id,
            'socket_name': value.socket_name,
            'protocol_version': value.protocol_version,
            'max_frame_bytes': value.max_frame_bytes,
            'max_request_bytes': value.max_request_bytes,
            'max_response_bytes': value.max_response_bytes,
            'allowed_operations': list(value.allowed_operations),
            'local_only': value.local_only,
        }
        validated = module.validate_stt_ipc_policy_evidence(
            raw,
            expected_service_id=expected['service_id'],
            expected_activation_id=expected['activation_id'],
            expected_socket_name=expected_socket_name,
        )
    except (AttributeError, TypeError, ValueError, KeyError) as exc:
        raise _fail('runtime admission IPC policy is invalid') from exc
    if validated != value:
        raise _fail('runtime admission IPC policy is invalid')
    if (
        value.policy_valid is not True
        or value.execution_enabled is not False
        or value.runtime_activation_allowed is not False
        or value.socket_bound is not False
        or value.ipc_enforced is not False
        or value.effect != 'metadata_only'
    ):
        raise _fail('runtime admission IPC policy is invalid')
    return value


def _rollback_policy_module() -> Any:
    module_name = 'screentra_stt_rollback_policy_evidence'
    module = sys.modules.get(module_name)
    if module is not None:
        return module
    module_path = Path(__file__).with_name('screentra-stt-rollback-policy-evidence.py')
    spec = importlib.util.spec_from_file_location(module_name, module_path)
    if spec is None or spec.loader is None:
        raise _fail('runtime admission rollback policy is invalid')
    module = importlib.util.module_from_spec(spec)
    sys.modules[module_name] = module
    try:
        spec.loader.exec_module(module)
    except Exception as exc:
        sys.modules.pop(module_name, None)
        raise _fail('runtime admission rollback policy is invalid') from exc
    return module


def _validated_rollback_policy(value: Any, expected: dict[str, str]) -> Any:
    """Accept only the immutable result produced by the rollback-policy validator."""
    module = _rollback_policy_module()
    if type(value) is not module.STTRollbackPolicyEvidence:
        raise _fail('runtime admission rollback policy is invalid')
    try:
        raw = {
            'service_id': value.service_id,
            'activation_id': value.activation_id,
            'service_version': value.service_version,
            'retention_count': value.retention_count,
            'rollback_artifact_verified': value.rollback_artifact_verified,
            'rollback_identity_bound': value.rollback_identity_bound,
            'atomic_swap_supported': value.atomic_swap_supported,
            'state_preservation_supported': value.state_preservation_supported,
            'cleanup_supported': value.cleanup_supported,
            'policy_version': value.policy_version,
        }
        validated = module.validate_stt_rollback_policy_evidence(
            raw,
            expected_service_id=expected['service_id'],
            expected_activation_id=expected['activation_id'],
            expected_service_version=expected['service_version'],
        )
    except (AttributeError, TypeError, ValueError, KeyError) as exc:
        raise _fail('runtime admission rollback policy is invalid') from exc
    if validated != value:
        raise _fail('runtime admission rollback policy is invalid')
    if (
        value.policy_valid is not True
        or value.execution_enabled is not False
        or value.runtime_activation_allowed is not False
        or value.rollback_executed is not False
        or value.process_started is not False
        or value.state_changed is not False
        or value.effect != 'metadata_only'
    ):
        raise _fail('runtime admission rollback policy is invalid')
    return value


def _runtime_installation_module() -> Any:
    module_name = 'screentra_stt_runtime_installation_evidence'
    module = sys.modules.get(module_name)
    if module is not None:
        return module
    module_path = Path(__file__).with_name('screentra-stt-runtime-installation-evidence.py')
    spec = importlib.util.spec_from_file_location(module_name, module_path)
    if spec is None or spec.loader is None:
        raise _fail('runtime installation evidence is invalid')
    module = importlib.util.module_from_spec(spec)
    sys.modules[module_name] = module
    try:
        spec.loader.exec_module(module)
    except Exception as exc:
        sys.modules.pop(module_name, None)
        raise _fail('runtime installation evidence is invalid') from exc
    return module


def _validated_runtime_installation(value: Any, expected: dict[str, str]) -> Any:
    """Accept only the exact immutable, identity-bound install evidence type."""
    module = _runtime_installation_module()
    if type(value) is not module.STTRuntimeInstallationEvidence:
        raise _fail('runtime installation evidence is invalid')
    try:
        raw = {
            'service_id': value.service_id,
            'activation_id': value.activation_id,
            'service_version': value.service_version,
            'runtime_family': value.runtime_family,
            'runtime_version': value.runtime_version,
            'source_commit': value.source_commit,
            'runtime_sha256': value.runtime_sha256,
            'model_id': value.model_id,
            'model_sha256': value.model_sha256,
            'runtime_verified': value.runtime_verified,
            'model_verified': value.model_verified,
            'non_starting_check': value.non_starting_check,
        }
        validated = module.validate_stt_runtime_installation_evidence(
            raw,
            expected_service_id=expected['service_id'],
            expected_activation_id=expected['activation_id'],
            expected_service_version=expected['service_version'],
            expected_model_id=expected['model_id'],
            expected_model_sha256=expected['model_sha256'],
        )
    except (AttributeError, TypeError, ValueError, KeyError) as exc:
        raise _fail('runtime installation evidence is invalid') from exc
    if validated != value:
        raise _fail('runtime installation evidence is invalid')
    if (
        value.policy_valid is not True
        or value.artifacts_verified is not True
        or value.execution_enabled is not False
        or value.runtime_activation_allowed is not False
        or value.process_started is not False
        or value.model_loaded is not False
        or value.service_started is not False
        or value.effect != 'metadata_only'
    ):
        raise _fail('runtime installation evidence is invalid')
    return value


def _audio_permission_module() -> Any:
    module_name = 'screentra_stt_audio_permission_evidence'
    module = sys.modules.get(module_name)
    if module is not None:
        return module
    module_path = Path(__file__).with_name('screentra-stt-audio-permission-evidence.py')
    spec = importlib.util.spec_from_file_location(module_name, module_path)
    if spec is None or spec.loader is None:
        raise _fail('runtime admission audio-permission evidence is invalid')
    module = importlib.util.module_from_spec(spec)
    sys.modules[module_name] = module
    try:
        spec.loader.exec_module(module)
    except Exception as exc:
        sys.modules.pop(module_name, None)
        raise _fail('runtime admission audio-permission evidence is invalid') from exc
    return module


def _validated_audio_permission(value: Any, expected: dict[str, str]) -> Any:
    """Accept only the exact immutable, identity-bound permission result type."""
    module = _audio_permission_module()
    if type(value) is not module.STTAudioPermissionEvidence:
        raise _fail('runtime admission audio-permission evidence is invalid')
    try:
        raw = {
            'service_id': value.service_id,
            'activation_id': value.activation_id,
            'host_id': value.host_id,
            'audio_backend': value.audio_backend,
            'permission_source': value.permission_source,
            'permission_observed': value.permission_observed,
            'capture_device_observed': value.capture_device_observed,
            'user_service_observed': value.user_service_observed,
            'non_capture_check': value.non_capture_check,
        }
        validated = module.validate_stt_audio_permission_evidence(
            raw,
            expected_service_id=expected['service_id'],
            expected_activation_id=expected['activation_id'],
            expected_host_id=expected['host_id'],
        )
    except (AttributeError, TypeError, ValueError, KeyError) as exc:
        raise _fail('runtime admission audio-permission evidence is invalid') from exc
    if validated != value:
        raise _fail('runtime admission audio-permission evidence is invalid')
    if (
        value.policy_valid is not True
        or value.execution_enabled is not False
        or value.runtime_activation_allowed is not False
        or value.hardware_permission_activated is not False
        or value.microphone_accessed is not False
        or value.audio_captured is not False
        or value.listener_bound is not False
        or value.effect != 'metadata_only'
    ):
        raise _fail('runtime admission audio-permission evidence is invalid')
    return value


def _resource_enforcement_module() -> Any:
    module_name = 'screentra_stt_resource_enforcement_evidence'
    module = sys.modules.get(module_name)
    if module is not None:
        return module
    module_path = Path(__file__).with_name('screentra-stt-resource-enforcement-evidence.py')
    spec = importlib.util.spec_from_file_location(module_name, module_path)
    if spec is None or spec.loader is None:
        raise _fail('runtime admission resource-enforcement evidence is invalid')
    module = importlib.util.module_from_spec(spec)
    sys.modules[module_name] = module
    try:
        spec.loader.exec_module(module)
    except Exception as exc:
        sys.modules.pop(module_name, None)
        raise _fail('runtime admission resource-enforcement evidence is invalid') from exc
    return module


def _validated_resource_enforcement(
    value: Any, expected: dict[str, str], expected_policy_version: str
) -> Any:
    """Accept only the exact immutable, identity-bound enforcement observation."""
    module = _resource_enforcement_module()
    if type(value) is not module.STTResourceEnforcementEvidence:
        raise _fail('runtime admission resource-enforcement evidence is invalid')
    try:
        raw = {
            'service_id': value.service_id,
            'activation_id': value.activation_id,
            'policy_version': value.policy_version,
            'cpu_enforced': value.cpu_enforced,
            'memory_enforced': value.memory_enforced,
            'pids_enforced': value.pids_enforced,
            'writable_enforced': value.writable_enforced,
            'observation_non_mutating': value.observation_non_mutating,
        }
        validated = module.validate_stt_resource_enforcement_evidence(
            raw,
            expected_service_id=expected['service_id'],
            expected_activation_id=expected['activation_id'],
            expected_policy_version=expected_policy_version,
        )
    except (AttributeError, TypeError, ValueError, KeyError) as exc:
        raise _fail('runtime admission resource-enforcement evidence is invalid') from exc
    if validated != value:
        raise _fail('runtime admission resource-enforcement evidence is invalid')
    if (
        value.enforcement_valid is not True
        or value.policy_valid is not True
        or value.execution_enabled is not False
        or value.runtime_activation_allowed is not False
        or value.process_started is not False
        or value.resource_enforced is not False
        or value.cgroup_bound is not False
        or value.effect != 'metadata_only'
    ):
        raise _fail('runtime admission resource-enforcement evidence is invalid')
    return value


def _capability_admission_module() -> Any:
    module_name = 'screentra_capability_admission'
    module = sys.modules.get(module_name)
    if module is not None:
        return module
    module_path = Path(__file__).with_name('screentra-capability-admission.py')
    spec = importlib.util.spec_from_file_location(module_name, module_path)
    if spec is None or spec.loader is None:
        raise _fail('runtime capability admission is invalid')
    module = importlib.util.module_from_spec(spec)
    sys.modules[module_name] = module
    try:
        spec.loader.exec_module(module)
    except Exception as exc:
        sys.modules.pop(module_name, None)
        raise _fail('runtime capability admission is invalid') from exc
    return module


def _validated_capability_admission(value: Any, expected: dict[str, str]) -> Any:
    """Accept only the exact immutable, default-deny admission result type."""
    module = _capability_admission_module()
    if type(value) is not module.CapabilityAdmission:
        raise _fail('runtime capability admission is invalid')
    if set(vars(value)) != _CAPABILITY_RESULT_FIELDS:
        raise _fail('runtime capability admission is invalid')
    try:
        validated = module.admit_capabilities(
            {
                'client_id': value.client_id,
                'addon_id': value.addon_id,
                'capabilities': list(value.requested_capabilities),
            },
            {
                'client_id': value.client_id,
                'addon_id': value.addon_id,
                'approved_capabilities': list(value.approved_capabilities),
            },
        )
    except (AttributeError, TypeError, ValueError, KeyError) as exc:
        raise _fail('runtime capability admission is invalid') from exc
    if validated != value:
        raise _fail('runtime capability admission is invalid')
    if (
        value.client_id != expected['host_id']
        or value.addon_id != expected['service_id']
        or set(value.requested_capabilities) != _STT_CAPABILITIES
        or set(value.approved_capabilities) != _STT_CAPABILITIES
        or value.admitted is not True
        or value.execution_enabled is not False
        or value.effect != 'metadata_only'
        or value.reason is not None
        or tuple(value.capabilities) != tuple(value.requested_capabilities)
    ):
        raise _fail('runtime capability admission is invalid')
    return value


def _approval_module() -> Any:
    module_name = 'screentra_stt_approval_record'
    module = sys.modules.get(module_name)
    if module is not None:
        return module
    module_path = Path(__file__).with_name('screentra-stt-approval-record.py')
    spec = importlib.util.spec_from_file_location(module_name, module_path)
    if spec is None or spec.loader is None:
        raise _fail('runtime admission approval record is invalid')
    module = importlib.util.module_from_spec(spec)
    sys.modules[module_name] = module
    try:
        spec.loader.exec_module(module)
    except Exception as exc:
        sys.modules.pop(module_name, None)
        raise _fail('runtime admission approval record is invalid') from exc
    return module


def _validated_approval_record(value: Any) -> Any:
    """Accept only the immutable result produced by the approval validator."""
    module = _approval_module()
    if type(value) is not module.STTApprovalRecord:
        raise _fail('runtime admission approval record is invalid')
    try:
        raw = {
            'record_type': value.record_type,
            'service_id': value.service_id,
            'service_version': value.service_version,
            'activation_id': value.activation_id,
            'host_id': value.host_id,
            'model_id': value.model_id,
            'model_sha256': value.model_sha256,
            'approver': value.approver,
            'approved_at': value.approved_at,
            'gates': [
                {'name': gate.name, 'passed': gate.passed, 'reason': gate.reason}
                for gate in value.gates
            ],
        }
        validated = module.validate_stt_approval_record(raw)
    except (AttributeError, TypeError, ValueError, KeyError) as exc:
        raise _fail('runtime admission approval record is invalid') from exc
    if validated != value:
        raise _fail('runtime admission approval record is invalid')
    if (
        value.approval_record_valid is not True
        or value.execution_enabled is not False
        or value.runtime_activation_allowed is not False
        or value.effect != 'metadata_only'
    ):
        raise _fail('runtime admission approval record is invalid')
    return value


def decide_runtime_admission(request: Mapping[str, Any]) -> dict[str, Any]:
    """Compose independent evidence into a non-executing admission decision."""
    if not isinstance(request, Mapping):
        raise _fail('runtime admission request must be an object')
    request = dict(request)
    if set(request) - _REQUEST_FIELDS or not _REQUIRED_REQUEST_FIELDS.issubset(request):
        raise _fail('runtime admission request fields are not allowlisted')
    approval = _validated_approval_record(request['approval_record'])
    expected = {
        'service_id': approval.service_id,
        'service_version': approval.service_version,
        'activation_id': approval.activation_id,
        'host_id': approval.host_id,
        'model_id': approval.model_id,
        'model_sha256': approval.model_sha256,
    }
    capability = _validated_capability_admission(
        request['capability_admission'], expected
    )
    resource_policy = _validated_resource_policy(
        request['resource_policy_evidence'], expected
    )
    resource_enforcement = _validated_resource_enforcement(
        request['resource_enforcement_evidence'],
        expected,
        resource_policy.policy_version,
    )
    rollback_policy = _validated_rollback_policy(
        request['rollback_policy_evidence'], expected
    )
    runtime_installation = _validated_runtime_installation(
        request['runtime_installation_evidence'], expected
    )
    audio_permission = _validated_audio_permission(
        request['audio_permission_evidence'], expected
    )
    try:
        _validate_preflight(request['preflight_evidence'], expected)
        _validate_model_plan(request['model_staging_plan'], expected)
        socket_name = _validate_supervisor(request['supervisor_plan'], expected)
    except RuntimeAdmissionError as exc:
        raise _fail('runtime admission evidence is invalid') from exc
    ipc_policy = _validated_ipc_policy(
        request['ipc_policy_evidence'], expected, socket_name
    )
    bounded_request = dict(request)
    bounded_request['approval_record'] = approval.as_dict()
    bounded_request['capability_admission'] = capability.as_dict()
    bounded_request['resource_policy_evidence'] = resource_policy.as_dict()
    bounded_request['resource_enforcement_evidence'] = resource_enforcement.as_dict()
    bounded_request['ipc_policy_evidence'] = ipc_policy.as_dict()
    bounded_request['rollback_policy_evidence'] = rollback_policy.as_dict()
    bounded_request['runtime_installation_evidence'] = runtime_installation.as_dict()
    bounded_request['audio_permission_evidence'] = audio_permission.as_dict()
    _bounded_json(bounded_request, MAX_REQUEST_BYTES, 'runtime admission request is not bounded')

    all_gates_passed = all(gate.passed for gate in approval.gates)
    return {
        'decision_type': 'stt_runtime_admission',
        'admission': 'approved_for_future_review' if all_gates_passed else 'denied',
        'reason': 'approval_record_validated' if all_gates_passed else 'approval_gate_not_passed',
        'runtime_activation_allowed': False,
        'execution_enabled': False,
        'hardware_permission_activated': False,
        'microphone_access': False,
        'microphone_accessed': False,
        'audio_captured': False,
        'camera_access': False,
        'process_started': False,
        'socket_bound': False,
        'listener_bound': False,
        'model_loaded': False,
        'service_started': False,
        'resource_enforced': False,
        'cgroup_bound': False,
        'ipc_enforced': False,
        'rollback_executed': False,
        'state_changed': False,
        'lifecycle_state_changed': False,
        'service_id': expected['service_id'],
        'service_version': expected['service_version'],
        'activation_id': expected['activation_id'],
        'host_id': expected['host_id'],
        'audio_backend': audio_permission.audio_backend,
        'permission_source': audio_permission.permission_source,
        'client_id': capability.client_id,
        'addon_id': capability.addon_id,
        'requested_capabilities': tuple(capability.requested_capabilities),
        'approved_capabilities': tuple(capability.approved_capabilities),
        'capabilities': tuple(capability.capabilities),
        'model_id': expected['model_id'],
        'model_sha256': expected['model_sha256'],
        'runtime_family': runtime_installation.runtime_family,
        'runtime_version': runtime_installation.runtime_version,
        'source_commit': runtime_installation.source_commit,
        'runtime_sha256': runtime_installation.runtime_sha256,
        'runtime_installation_identity_tuple': (
            runtime_installation.service_id,
            runtime_installation.service_version,
            runtime_installation.activation_id,
            runtime_installation.runtime_family,
            runtime_installation.runtime_version,
            runtime_installation.source_commit,
            runtime_installation.runtime_sha256,
            runtime_installation.model_id,
            runtime_installation.model_sha256,
        ),
        'audio_permission_identity_tuple': (
            audio_permission.service_id,
            audio_permission.activation_id,
            audio_permission.host_id,
            audio_permission.audio_backend,
            audio_permission.permission_source,
        ),
        'resource_enforcement_policy_version': resource_enforcement.policy_version,
        'resource_enforcement_identity_tuple': (
            resource_enforcement.service_id,
            resource_enforcement.activation_id,
            resource_enforcement.policy_version,
        ),
        'socket_name': ipc_policy.socket_name,
        'protocol_version': ipc_policy.protocol_version,
        'allowed_operations': tuple(ipc_policy.allowed_operations),
        'identity_tuple': (
            expected['service_id'], expected['service_version'],
            expected['activation_id'], expected['host_id'],
            expected['model_id'], expected['model_sha256'],
        ),
        'ipc_identity_tuple': (
            ipc_policy.service_id, ipc_policy.activation_id,
            ipc_policy.socket_name, ipc_policy.protocol_version,
            tuple(ipc_policy.allowed_operations),
        ),
        'capability_identity_tuple': (
            capability.client_id, capability.addon_id,
            tuple(capability.requested_capabilities),
            tuple(capability.approved_capabilities),
        ),
        'rollback_identity_tuple': (
            rollback_policy.service_id, rollback_policy.service_version,
            rollback_policy.activation_id,
        ),
        'approval_gates': tuple(gate.name for gate in approval.gates),
        'evidence_composed': {
            'preflight': True,
            'model_verification': True,
            'supervisor': True,
            'approval_record': True,
            'capability_admission': True,
            'resource_policy': True,
            'resource_enforcement': True,
            'ipc_policy': True,
            'rollback_policy': True,
            'runtime_installation': True,
            'audio_permission': True,
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
