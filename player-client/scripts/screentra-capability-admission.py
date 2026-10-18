#!/usr/bin/env python3
"""Pure admission policy contract for a future client capability broker.

This module validates only declarative manifest and administrator policy
metadata.  It does not grant hardware permission, start a service, spawn a
process, access credentials, or enable execution.
"""
from __future__ import annotations

import re
from collections.abc import Mapping
from dataclasses import dataclass
from typing import Any, Final


CLIENT_ID_MAX_BYTES: Final = 64
ADDON_ID_MAX_BYTES: Final = 64
CLIENT_CAPABILITIES: Final = frozenset({
    'render_only',
    'camera_capture',
    'microphone_capture',
    'local_inference',
    'client_service',
})

_MANIFEST_FIELDS = frozenset({'client_id', 'addon_id', 'capabilities'})
_POLICY_FIELDS = frozenset({'client_id', 'addon_id', 'approved_capabilities'})
_FORBIDDEN_FIELDS = frozenset({
    'command',
    'commands',
    'credential',
    'credentials',
    'execution_enabled',
    'path',
    'paths',
    'process',
    'processes',
})
_IDENTITY_RE = re.compile(r'^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$')


class CapabilityAdmissionError(ValueError):
    """Raised when manifest or administrator policy metadata is malformed."""


@dataclass(frozen=True)
class CapabilityAdmission:
    """Metadata-only result; this object never represents activated access."""

    client_id: str
    addon_id: str
    requested_capabilities: tuple[str, ...]
    approved_capabilities: tuple[str, ...]
    capabilities: tuple[str, ...]
    admitted: bool
    reason: str | None
    execution_enabled: bool = False
    effect: str = 'metadata_only'

    def as_dict(self) -> dict[str, Any]:
        """Return bounded metadata without permission or runtime claims."""
        return {
            'client_id': self.client_id,
            'addon_id': self.addon_id,
            'requested_capabilities': list(self.requested_capabilities),
            'approved_capabilities': list(self.approved_capabilities),
            'capabilities': list(self.capabilities),
            'admitted': self.admitted,
            'reason': self.reason,
            'execution_enabled': False,
            'effect': 'metadata_only',
        }


def _reject_forbidden_fields(value: Any) -> None:
    if isinstance(value, Mapping):
        for key, nested in value.items():
            if key in _FORBIDDEN_FIELDS:
                raise CapabilityAdmissionError('execution fields are not allowed')
            _reject_forbidden_fields(nested)
    elif isinstance(value, list):
        for nested in value:
            _reject_forbidden_fields(nested)


def _require_fields(value: Any, fields: frozenset[str]) -> Mapping[str, Any]:
    if not isinstance(value, Mapping) or set(value) != fields:
        raise CapabilityAdmissionError('capability metadata fields are not allowlisted')
    _reject_forbidden_fields(value)
    return value


def _validate_identity(value: Any, field: str) -> str:
    if not isinstance(value, str):
        raise CapabilityAdmissionError('client and addon identities are malformed')
    try:
        size = len(value.encode('ascii'))
    except UnicodeEncodeError as exc:
        raise CapabilityAdmissionError('client and addon identities are malformed') from exc
    maximum = CLIENT_ID_MAX_BYTES if field == 'client_id' else ADDON_ID_MAX_BYTES
    if size == 0 or size > maximum or _IDENTITY_RE.fullmatch(value) is None:
        raise CapabilityAdmissionError('client and addon identities are malformed')
    return value


def _validate_capabilities(value: Any, field: str) -> tuple[str, ...]:
    if not isinstance(value, list):
        raise CapabilityAdmissionError(f'{field} must be a list')
    seen: set[str] = set()
    validated: list[str] = []
    for capability in value:
        if not isinstance(capability, str) or capability not in CLIENT_CAPABILITIES:
            raise CapabilityAdmissionError('unknown or malformed capability')
        if capability in seen:
            raise CapabilityAdmissionError('duplicate capability')
        seen.add(capability)
        validated.append(capability)
    capabilities = tuple(validated)
    if 'local_inference' in seen and not {
        'microphone_capture',
        'client_service',
    }.issubset(seen):
        raise CapabilityAdmissionError(
            f'{field} local_inference dependencies are incomplete'
        )
    return capabilities


def admit_capabilities(
    manifest: Mapping[str, Any],
    policy: Mapping[str, Any] | None = None,
) -> CapabilityAdmission:
    """Evaluate requested capabilities against an explicit admin policy.

    Omitting ``policy`` is equivalent to an empty approved set.  A supplied
    policy must bind to the same client and addon identities as the manifest.
    Admission is denied, rather than partially granted, when the requested
    set is not a subset of the approved set.
    """
    manifest = _require_fields(manifest, _MANIFEST_FIELDS)
    client_id = _validate_identity(manifest['client_id'], 'client_id')
    addon_id = _validate_identity(manifest['addon_id'], 'addon_id')
    requested = _validate_capabilities(manifest['capabilities'], 'requested')

    if policy is None:
        approved = ()
    else:
        policy = _require_fields(policy, _POLICY_FIELDS)
        policy_client_id = _validate_identity(policy['client_id'], 'client_id')
        policy_addon_id = _validate_identity(policy['addon_id'], 'addon_id')
        if policy_client_id != client_id or policy_addon_id != addon_id:
            raise CapabilityAdmissionError('manifest and policy identities do not match')
        approved = _validate_capabilities(policy['approved_capabilities'], 'approved')

    admitted = set(requested).issubset(approved)
    return CapabilityAdmission(
        client_id=client_id,
        addon_id=addon_id,
        requested_capabilities=requested,
        approved_capabilities=approved,
        capabilities=requested if admitted else (),
        admitted=admitted,
        reason=None if admitted else 'requested capabilities are not approved',
    )


__all__ = [
    'ADDON_ID_MAX_BYTES',
    'CapabilityAdmission',
    'CapabilityAdmissionError',
    'CLIENT_CAPABILITIES',
    'CLIENT_ID_MAX_BYTES',
    'admit_capabilities',
]
