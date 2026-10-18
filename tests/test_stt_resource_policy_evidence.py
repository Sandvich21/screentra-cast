import dataclasses
import importlib.util
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
MODULE_PATH = ROOT / 'player-client/scripts/screentra-stt-resource-policy-evidence.py'


def load_module():
    if not MODULE_PATH.is_file():
        return None
    spec = importlib.util.spec_from_file_location(
        'screentra_stt_resource_policy_evidence', MODULE_PATH
    )
    module = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


policy = load_module()

SERVICE_ID = 'magic-mirror-stt'
ACTIVATION_ID = 'activation-20261018-01'
POLICY_VERSION = 'resource-policy-v1'


def valid_evidence(**overrides):
    value = {
        'service_id': SERVICE_ID,
        'activation_id': ACTIVATION_ID,
        'cpu_millis': 500,
        'memory_bytes': 128 * 1024 * 1024,
        'pids': 32,
        'writable_bytes': 64 * 1024 * 1024,
        'policy_version': POLICY_VERSION,
    }
    value.update(overrides)
    return value


class STTResourcePolicyEvidenceTests(unittest.TestCase):
    def require_module(self):
        self.assertIsNotNone(
            policy,
            'production STT resource-policy evidence module is not implemented',
        )
        return policy

    def assert_invalid(self, evidence, **expected):
        module = self.require_module()
        with self.assertRaises(module.STTResourcePolicyEvidenceError, **expected):
            module.validate_resource_policy_evidence(evidence)

    def test_valid_policy_returns_frozen_metadata_only_plan(self):
        module = self.require_module()

        result = module.validate_resource_policy_evidence(valid_evidence())

        self.assertIsInstance(result, module.STTResourcePolicyEvidence)
        self.assertEqual(result.service_id, SERVICE_ID)
        self.assertEqual(result.activation_id, ACTIVATION_ID)
        self.assertEqual(result.policy_version, POLICY_VERSION)
        self.assertEqual(result.cpu_millis, 500)
        self.assertEqual(result.memory_bytes, 128 * 1024 * 1024)
        self.assertEqual(result.pids, 32)
        self.assertEqual(result.writable_bytes, 64 * 1024 * 1024)
        self.assertTrue(result.policy_valid)
        self.assertFalse(result.execution_enabled)
        self.assertFalse(result.runtime_activation_allowed)
        self.assertFalse(result.process_started)
        self.assertFalse(result.resource_enforced)
        self.assertEqual(result.effect, 'metadata_only')
        with self.assertRaises(dataclasses.FrozenInstanceError):
            result.cpu_millis = 1

        encoded = result.as_dict()
        self.assertEqual(encoded['plan_type'], 'stt_client_service_resource_policy_plan')
        self.assertTrue(encoded['policy_valid'])
        for field in (
            'execution_enabled', 'runtime_activation_allowed',
            'process_started', 'resource_enforced',
        ):
            self.assertFalse(encoded[field], field)
        for forbidden in (
            'process_running', 'container_running', 'container_id', 'command',
            'credentials', 'path', 'url', 'mounts', 'ports', 'environment',
            'audio', 'model', 'image', 'socket',
        ):
            self.assertNotIn(forbidden, encoded)

    def test_manifest_bounds_accept_exact_edges_and_zero_writable_storage(self):
        module = self.require_module()
        edges = (
            {'cpu_millis': 1, 'memory_bytes': 64 * 1024 * 1024, 'pids': 1, 'writable_bytes': 0},
            {
                'cpu_millis': 4000,
                'memory_bytes': 2 * 1024 * 1024 * 1024,
                'pids': 256,
                'writable_bytes': 512 * 1024 * 1024,
            },
        )
        for overrides in edges:
            with self.subTest(overrides=overrides):
                result = module.build_resource_policy_plan(valid_evidence(**overrides))
                self.assertTrue(result.policy_valid)

    def test_zero_or_oversized_positive_resources_are_rejected(self):
        module = self.require_module()
        cases = (
            {'cpu_millis': 0},
            {'cpu_millis': 4001},
            {'memory_bytes': 0},
            {'memory_bytes': 2 * 1024 * 1024 * 1024 + 1},
            {'pids': 0},
            {'pids': 257},
            {'writable_bytes': -1},
            {'writable_bytes': 512 * 1024 * 1024 + 1},
        )
        for overrides in cases:
            with self.subTest(overrides=overrides):
                with self.assertRaises(module.STTResourcePolicyEvidenceError):
                    module.validate_resource_policy_evidence(valid_evidence(**overrides))

    def test_resource_values_reject_booleans_and_non_integers(self):
        module = self.require_module()
        for field in ('cpu_millis', 'memory_bytes', 'pids', 'writable_bytes'):
            for value in (True, False, 1.5, '128', None):
                with self.subTest(field=field, value=value):
                    with self.assertRaises(module.STTResourcePolicyEvidenceError):
                        module.validate_resource_policy_evidence(valid_evidence(**{field: value}))

    def test_expected_service_and_activation_identities_are_bound(self):
        module = self.require_module()
        result = module.validate_resource_policy_evidence(
            valid_evidence(),
            expected_service_id=SERVICE_ID,
            expected_activation_id=ACTIVATION_ID,
        )
        self.assertEqual((result.service_id, result.activation_id), (SERVICE_ID, ACTIVATION_ID))

        for kwargs in (
            {'expected_service_id': 'other-service'},
            {'expected_activation_id': 'other-activation'},
        ):
            with self.subTest(kwargs=kwargs):
                with self.assertRaises(module.STTResourcePolicyEvidenceError):
                    module.validate_resource_policy_evidence(valid_evidence(), **kwargs)

    def test_only_the_closed_mapping_is_accepted(self):
        module = self.require_module()
        allowed = frozenset({
            'service_id', 'activation_id', 'cpu_millis', 'memory_bytes',
            'pids', 'writable_bytes', 'policy_version',
        })
        self.assertEqual(module.RESOURCE_POLICY_FIELDS, allowed)
        for extra in (
            {'unexpected': True},
            {'credentials': 'redacted'},
            {'path': '/tmp/service'},
            {'url': 'https://example.invalid'},
            {'command': ['/bin/sh']},
            {'mounts': []},
            {'ports': []},
            {'environment': {'SECRET': 'nope'}},
            {'audio': 'raw'},
            {'model_data': 'base64'},
        ):
            with self.subTest(extra=extra):
                self.assert_invalid(valid_evidence(**extra))

    def test_identities_policy_version_and_mapping_shape_are_bounded(self):
        module = self.require_module()
        cases = (
            {'service_id': 'other/service'},
            {'service_id': 'x' * 65},
            {'activation_id': 'activation/other'},
            {'activation_id': 'x' * 129},
            {'policy_version': ''},
            {'policy_version': 'policy version'},
            {'policy_version': 'x' * 65},
            {'service_id': True},
            {'activation_id': 1},
            {'policy_version': 1},
        )
        for overrides in cases:
            with self.subTest(overrides=overrides):
                self.assert_invalid(valid_evidence(**overrides))

        for value in (None, [], 'not-a-mapping'):
            with self.subTest(value=value):
                self.assert_invalid(value)
        self.assert_invalid({})

    def test_oversized_records_fail_closed_without_runtime_shaped_output(self):
        module = self.require_module()
        oversized = valid_evidence()
        oversized['padding'] = 'x' * module.MAX_EVIDENCE_BYTES
        self.assert_invalid(oversized)

        result = module.validate_resource_policy_evidence(valid_evidence())
        self.assertEqual(set(result.as_dict()), {
            'plan_type', 'effect', 'service_id', 'activation_id',
            'policy_version', 'cpu_millis', 'memory_bytes', 'pids',
            'writable_bytes', 'policy_valid', 'execution_enabled',
            'runtime_activation_allowed', 'process_started', 'resource_enforced',
        })


if __name__ == '__main__':
    unittest.main()
