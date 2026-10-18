#!/usr/bin/env python3
"""Pure metadata contract for a future client-local service supervisor.

This module validates service identity/version records and applies only the
closed, metadata-only lifecycle graph. It never starts, stops, updates, or
rolls back a process.
"""
from __future__ import annotations

import copy
import json
import re
from typing import Any


LIFECYCLE_STATES = frozenset({
    'staged', 'starting', 'running', 'degraded', 'stopped', 'failed',
    'rollback_ready',
})
TERMINAL_STATES = frozenset({'stopped', 'rollback_ready'})
MAX_IDENTIFIER_BYTES = 128
MAX_VERSION_BYTES = 64
MAX_METADATA_BYTES = 4096
MAX_ROLLBACK_REASON_BYTES = 256
MAX_SERVICE_RECORD_BYTES = 8192
MAX_SERVICE_RECORDS = 32

_IDENTIFIER = re.compile(r'^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$')
_VERSION = re.compile(
    r'^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)'
    r'(?:-[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?$'
)

_ALLOWED_RECORD_FIELDS = frozenset({
    'service_id', 'service_version', 'activation_id', 'state', 'metadata',
    'rollback', 'execution_enabled', 'effect',
})
_ALLOWED_METADATA_FIELDS = frozenset({'diagnostic'})
_ALLOWED_ROLLBACK_FIELDS = frozenset({
    'status', 'target_service_id', 'target_service_version',
    'target_activation_id', 'reason', 'applied',
})
_TRANSITIONS = {
    'staged': frozenset({'starting', 'failed', 'stopped'}),
    'starting': frozenset({'running', 'degraded', 'failed', 'stopped'}),
    'running': frozenset({'degraded', 'failed', 'stopped'}),
    'degraded': frozenset({'running', 'failed', 'stopped', 'rollback_ready'}),
    'failed': frozenset({'stopped', 'rollback_ready'}),
    'stopped': frozenset(),
    'rollback_ready': frozenset(),
}


class LifecycleContractError(ValueError):
    """Raised when service metadata or a lifecycle transition is invalid."""


def _require_identifier(value: Any, field: str) -> str:
    if (not isinstance(value, str) or not value or
            len(value.encode('utf-8')) > MAX_IDENTIFIER_BYTES or
            not _IDENTIFIER.fullmatch(value)):
        raise LifecycleContractError(f'{field} is malformed or exceeds its bound')
    return value


def _require_version(value: Any, field: str) -> str:
    if (not isinstance(value, str) or not value or
            len(value.encode('utf-8')) > MAX_VERSION_BYTES or
            not _VERSION.fullmatch(value)):
        raise LifecycleContractError(f'{field} is malformed or exceeds its bound')
    return value


def _bounded_text(value: Any, field: str, max_bytes: int) -> str:
    if not isinstance(value, str) or len(value.encode('utf-8')) > max_bytes:
        raise LifecycleContractError(f'{field} is malformed or exceeds its bound')
    if any(ord(char) < 32 or ord(char) == 127 for char in value):
        raise LifecycleContractError(f'{field} contains a control character')
    return value


def _validate_metadata(metadata: Any) -> dict[str, str]:
    if metadata is None:
        return {}
    if not isinstance(metadata, dict):
        raise LifecycleContractError('metadata must be a JSON object')
    if set(metadata) - _ALLOWED_METADATA_FIELDS:
        raise LifecycleContractError('metadata contains an unsupported field')
    result = {}
    if 'diagnostic' in metadata:
        result['diagnostic'] = _bounded_text(
            metadata['diagnostic'], 'metadata.diagnostic', MAX_METADATA_BYTES
        )
    encoded = json.dumps(result, separators=(',', ':'), ensure_ascii=True).encode('utf-8')
    if len(encoded) > MAX_METADATA_BYTES:
        raise LifecycleContractError('metadata exceeds its bound')
    return result


def _validate_rollback(rollback: Any, service_id: str) -> dict[str, Any]:
    if not isinstance(rollback, dict):
        raise LifecycleContractError('rollback metadata must be a JSON object')
    if set(rollback) - _ALLOWED_ROLLBACK_FIELDS:
        raise LifecycleContractError('rollback metadata contains an unsupported field')
    required = {
        'status', 'target_service_id', 'target_service_version',
        'target_activation_id', 'reason',
    }
    if not required.issubset(rollback):
        raise LifecycleContractError('rollback metadata is incomplete')
    if rollback['status'] != 'ready':
        raise LifecycleContractError('rollback metadata must remain ready')
    if rollback.get('applied', False) is not False:
        raise LifecycleContractError('rollback metadata cannot claim rollback applied')
    target_service_id = _require_identifier(
        rollback['target_service_id'], 'rollback.target_service_id'
    )
    if target_service_id != service_id:
        raise LifecycleContractError('rollback service identity mismatch')
    target_version = _require_version(
        rollback['target_service_version'], 'rollback.target_service_version'
    )
    target_activation_id = _require_identifier(
        rollback['target_activation_id'], 'rollback.target_activation_id'
    )
    reason = _bounded_text(
        rollback['reason'], 'rollback.reason', MAX_ROLLBACK_REASON_BYTES
    )
    result = {
        'status': 'ready',
        'target_service_id': target_service_id,
        'target_service_version': target_version,
        'target_activation_id': target_activation_id,
        'reason': reason,
        'applied': False,
    }
    encoded = json.dumps(result, separators=(',', ':'), ensure_ascii=True).encode('utf-8')
    if len(encoded) > MAX_METADATA_BYTES:
        raise LifecycleContractError('rollback metadata exceeds its bound')
    return result


def validate_service_record(record: Any) -> dict[str, Any]:
    """Validate and copy one bounded service lifecycle record."""
    if not isinstance(record, dict):
        raise LifecycleContractError('service record must be a JSON object')
    if set(record) - _ALLOWED_RECORD_FIELDS:
        raise LifecycleContractError('service record contains an unsupported field')
    required = {'service_id', 'service_version', 'activation_id', 'state'}
    if not required.issubset(record):
        raise LifecycleContractError('service record is incomplete')
    service_id = _require_identifier(record['service_id'], 'service_id')
    version = _require_version(record['service_version'], 'service_version')
    activation_id = _require_identifier(record['activation_id'], 'activation_id')
    state = record['state']
    if not isinstance(state, str) or state not in LIFECYCLE_STATES:
        raise LifecycleContractError('service state is unknown')
    if 'execution_enabled' in record and record['execution_enabled'] is not False:
        raise LifecycleContractError('service lifecycle execution must remain disabled')
    if 'effect' in record and record['effect'] != 'metadata_only':
        raise LifecycleContractError('service lifecycle effect must be metadata_only')

    result: dict[str, Any] = {
        'service_id': service_id,
        'service_version': version,
        'activation_id': activation_id,
        'state': state,
    }
    if 'metadata' in record:
        result['metadata'] = _validate_metadata(record['metadata'])
    if state == 'rollback_ready':
        if 'rollback' not in record:
            raise LifecycleContractError('rollback_ready requires rollback metadata')
        result['rollback'] = _validate_rollback(record['rollback'], service_id)
    elif 'rollback' in record:
        raise LifecycleContractError('rollback metadata is only valid for rollback_ready')
    if 'execution_enabled' in record:
        result['execution_enabled'] = False
    if 'effect' in record:
        result['effect'] = 'metadata_only'

    encoded = json.dumps(result, separators=(',', ':'), ensure_ascii=True).encode('utf-8')
    if len(encoded) > MAX_SERVICE_RECORD_BYTES:
        raise LifecycleContractError('service record exceeds its bound')
    return result


def validate_service_records(records: Any) -> list[dict[str, Any]]:
    """Validate a bounded service list and reject duplicate service identities."""
    if not isinstance(records, list):
        raise LifecycleContractError('service records must be a JSON array')
    if len(records) > MAX_SERVICE_RECORDS:
        raise LifecycleContractError('service records exceed their count bound')
    validated = [validate_service_record(record) for record in records]
    service_ids = [record['service_id'] for record in validated]
    if len(service_ids) != len(set(service_ids)):
        raise LifecycleContractError('duplicate service metadata is not allowed')
    encoded = json.dumps(validated, separators=(',', ':'), ensure_ascii=True).encode('utf-8')
    if len(encoded) > MAX_METADATA_BYTES * 2:
        raise LifecycleContractError('service metadata list exceeds its bound')
    return validated


def transition_service(
    record: Any,
    target_state: Any,
    *,
    expected_service_id: str | None = None,
    expected_activation_id: str | None = None,
    rollback: dict[str, Any] | None = None,
    execution_enabled: bool = False,
) -> dict[str, Any]:
    """Return a metadata-only next record without performing lifecycle work."""
    current = validate_service_record(record)
    if expected_service_id is not None:
        _require_identifier(expected_service_id, 'expected_service_id')
        if expected_service_id != current['service_id']:
            raise LifecycleContractError('service identity mismatch')
    if expected_activation_id is not None:
        _require_identifier(expected_activation_id, 'expected_activation_id')
        if expected_activation_id != current['activation_id']:
            raise LifecycleContractError('activation identity mismatch')
    if execution_enabled is not False:
        raise LifecycleContractError('lifecycle transitions cannot enable execution')
    if not isinstance(target_state, str) or target_state not in LIFECYCLE_STATES:
        raise LifecycleContractError('target state is unknown')
    if target_state not in _TRANSITIONS[current['state']]:
        raise LifecycleContractError(
            f"invalid lifecycle transition: {current['state']} -> {target_state}"
        )
    if target_state == 'rollback_ready':
        if rollback is None:
            raise LifecycleContractError('rollback_ready requires rollback metadata')
        next_rollback = _validate_rollback(rollback, current['service_id'])
    elif rollback is not None:
        raise LifecycleContractError('rollback metadata is only valid for rollback_ready')
    else:
        next_rollback = None

    result = copy.deepcopy(current)
    result['state'] = target_state
    result['execution_enabled'] = False
    result['effect'] = 'metadata_only'
    if next_rollback is not None:
        result['rollback'] = next_rollback
    else:
        result.pop('rollback', None)
    return validate_service_record(result)
