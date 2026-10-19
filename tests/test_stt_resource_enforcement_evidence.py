import dataclasses
import importlib.util
import sys
import unittest
from collections.abc import Mapping
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
MODULE_PATH = ROOT / 'player-client/scripts/screentra-stt-resource-enforcement-evidence.py'


def load_module():
    if not MODULE_PATH.is_file():
        return None
    module_name = 'screentra_stt_resource_enforcement_evidence'
    existing = sys.modules.get(module_name)
    if existing is not None:
        return existing
    spec = importlib.util.spec_from_file_location(module_name, MODULE_PATH)
    module = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


evidence_module = load_module()

SERVICE_ID = 'magic-mirror-stt'
ACTIVATION_ID = 'activation-20261018-01'
POLICY_VERSION = 'resource-policy-v1'


def valid_evidence(**overrides):
    value = {
        'service_id': SERVICE_ID,
        'activation_id': ACTIVATION_ID,
        'policy_version': POLICY_VERSION,
        'cpu_enforced': True,
        'memory_enforced': True,
        'pids_enforced': True,
        'writable_enforced': True,
        'observation_non_mutating': True,
    }
    value.update(overrides)
    return value


class DuplicateMapping(Mapping):
    def __init__(self, value):
        self.value = value

    def __getitem__(self, key):
        return self.value[key]

    def __iter__(self):
        yield from self.value
        yield 'service_id'

    def __len__(self):
        return len(self.value) + 1


class STTResourceEnforcementEvidenceTests(unittest.TestCase):
    def require_module(self):
        self.assertIsNotNone(
            evidence_module,
            'production STT resource-enforcement evidence module is not implemented',
        )
        return evidence_module

    def assert_invalid(self, evidence, **validator_kwargs):
        module = self.require_module()
        with self.assertRaises(module.STTResourceEnforcementEvidenceError):
            module.validate_stt_resource_enforcement_evidence(
                evidence, **validator_kwargs
            )

    def test_valid_evidence_is_immutable_identity_bound_and_inert(self):
        module = self.require_module()
        result = module.validate_stt_resource_enforcement_evidence(
            valid_evidence(),
            expected_service_id=SERVICE_ID,
            expected_activation_id=ACTIVATION_ID,
            expected_policy_version=POLICY_VERSION,
        )

        self.assertIsInstance(result, module.STTResourceEnforcementEvidence)
        self.assertTrue(result.enforcement_valid)
        self.assertTrue(result.policy_valid)
        self.assertEqual(
            (result.service_id, result.activation_id, result.policy_version),
            (SERVICE_ID, ACTIVATION_ID, POLICY_VERSION),
        )
        for field in (
            'execution_enabled', 'runtime_activation_allowed', 'process_started',
            'resource_enforced', 'cgroup_bound',
        ):
            self.assertFalse(getattr(result, field), field)
        with self.assertRaises(dataclasses.FrozenInstanceError):
            result.service_id = 'other-service'

        encoded = result.as_dict()
        self.assertEqual(set(encoded), {
            'plan_type', 'effect', 'service_id', 'activation_id', 'policy_version',
            'cpu_enforced', 'memory_enforced', 'pids_enforced', 'writable_enforced',
            'observation_non_mutating', 'enforcement_valid', 'policy_valid',
            'execution_enabled', 'runtime_activation_allowed', 'process_started',
            'resource_enforced', 'cgroup_bound',
        })
        for forbidden in (
            'readiness_claim', 'runtime_ready', 'process_running', 'container_running',
            'container_id', 'command', 'credentials', 'path', 'url', 'audio_bytes',
            'model_bytes', 'device_path', 'mounts', 'ports',
        ):
            self.assertNotIn(forbidden, encoded)

    def test_enforcement_and_policy_valid_require_all_observations_but_remain_inert(self):
        module = self.require_module()
        for field in (
            'cpu_enforced', 'memory_enforced', 'pids_enforced',
            'writable_enforced', 'observation_non_mutating',
        ):
            with self.subTest(field=field):
                result = module.validate_stt_resource_enforcement_evidence(
                    valid_evidence(**{field: False})
                )
                self.assertFalse(result.enforcement_valid, field)
                self.assertFalse(result.policy_valid, field)
                for inert_field in (
                    'execution_enabled', 'runtime_activation_allowed',
                    'process_started', 'resource_enforced', 'cgroup_bound',
                ):
                    self.assertFalse(getattr(result, inert_field), inert_field)

    def test_exact_identities_and_policy_version_are_required(self):
        module = self.require_module()
        for kwargs in (
            {'expected_service_id': 'other-service'},
            {'expected_activation_id': 'other-activation'},
            {'expected_policy_version': 'other-policy-v2'},
        ):
            with self.subTest(kwargs=kwargs):
                self.assert_invalid(valid_evidence(), **kwargs)

        for field, value in (
            ('service_id', 'other/service'),
            ('activation_id', 'https://example.invalid'),
            ('policy_version', 'policy version'),
            ('service_id', 'x' * 65),
            ('activation_id', 'x' * 129),
            ('policy_version', 'x' * 65),
        ):
            with self.subTest(field=field, value=value):
                self.assert_invalid(valid_evidence(**{field: value}))

    def test_real_booleans_are_required(self):
        for field in (
            'cpu_enforced', 'memory_enforced', 'pids_enforced',
            'writable_enforced', 'observation_non_mutating',
        ):
            for value in (1, 0, 'true', None):
                with self.subTest(field=field, value=value):
                    self.assert_invalid(valid_evidence(**{field: value}))

    def test_closed_fields_reject_arbitrary_sensitive_or_runtime_claims(self):
        forbidden = (
            {'unexpected': True},
            {'path': '/tmp/cgroup'},
            {'url': 'https://example.invalid'},
            {'command': ['/bin/sh']},
            {'credentials': 'redacted'},
            {'raw_audio': 'redacted'},
            {'audio_bytes': 'AA=='},
            {'model_bytes': 'AA=='},
            {'container_id': 'abc'},
            {'process_id': 123},
            {'device_path': '/dev/cgroup'},
            {'device_control': 'write'},
            {'readiness_claim': True},
            {'resource_enforced': True},
            {'cgroup_bound': True},
        )
        for extra in forbidden:
            with self.subTest(extra=extra):
                value = valid_evidence()
                value.update(extra)
                self.assert_invalid(value)

    def test_duplicate_fields_and_oversized_records_fail_closed(self):
        self.assert_invalid(DuplicateMapping(valid_evidence()))
        module = self.require_module()
        oversized = valid_evidence()
        oversized['padding'] = 'x' * module.MAX_EVIDENCE_BYTES
        self.assert_invalid(oversized)

    def test_non_mapping_and_non_json_values_fail_closed(self):
        self.assert_invalid([])
        self.assert_invalid(valid_evidence(cpu_enforced=object()))
        self.assert_invalid(valid_evidence(memory_enforced=float('nan')))


if __name__ == '__main__':
    unittest.main()
