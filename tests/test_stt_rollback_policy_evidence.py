import dataclasses
import importlib.util
import sys
import unittest
from collections.abc import Mapping
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
MODULE_PATH = ROOT / 'player-client/scripts/screentra-stt-rollback-policy-evidence.py'


def load_module():
    if not MODULE_PATH.is_file():
        return None
    spec = importlib.util.spec_from_file_location(
        'screentra_stt_rollback_policy_evidence', MODULE_PATH
    )
    module = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


policy = load_module()

SERVICE_ID = 'magic-mirror-stt'
ACTIVATION_ID = 'activation-20261018-01'
SERVICE_VERSION = '0.1.0'
POLICY_VERSION = 'rollback-policy-v1'


def valid_evidence(**overrides):
    value = {
        'service_id': SERVICE_ID,
        'activation_id': ACTIVATION_ID,
        'service_version': SERVICE_VERSION,
        'retention_count': 1,
        'rollback_artifact_verified': True,
        'rollback_identity_bound': True,
        'atomic_swap_supported': True,
        'state_preservation_supported': True,
        'cleanup_supported': True,
        'policy_version': POLICY_VERSION,
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


class STTRollbackPolicyEvidenceTests(unittest.TestCase):
    def require_module(self):
        self.assertIsNotNone(
            policy,
            'production STT rollback-policy evidence module is not implemented',
        )
        return policy

    def assert_invalid(self, evidence, **expected):
        module = self.require_module()
        with self.assertRaises(module.STTRollbackPolicyEvidenceError, **expected):
            module.validate_stt_rollback_policy_evidence(evidence)

    def test_valid_policy_is_frozen_identity_bound_metadata_only_evidence(self):
        module = self.require_module()
        result = module.validate_stt_rollback_policy_evidence(
            valid_evidence(),
            expected_service_id=SERVICE_ID,
            expected_activation_id=ACTIVATION_ID,
            expected_service_version=SERVICE_VERSION,
            expected_policy_version=POLICY_VERSION,
        )

        self.assertIsInstance(result, module.STTRollbackPolicyEvidence)
        self.assertEqual(
            (result.service_id, result.activation_id, result.service_version),
            (SERVICE_ID, ACTIVATION_ID, SERVICE_VERSION),
        )
        self.assertEqual(result.retention_count, 1)
        self.assertTrue(result.policy_valid)
        for field in (
            'execution_enabled', 'runtime_activation_allowed',
            'rollback_executed', 'process_started', 'state_changed',
        ):
            self.assertFalse(getattr(result, field), field)
        with self.assertRaises(dataclasses.FrozenInstanceError):
            result.service_id = 'other-service'

        encoded = result.as_dict()
        self.assertEqual(encoded['plan_type'], 'stt_client_service_rollback_policy_evidence')
        self.assertEqual(encoded['effect'], 'metadata_only')
        self.assertTrue(encoded['policy_valid'])
        self.assertEqual(set(encoded), {
            'plan_type', 'effect', 'policy_valid', 'service_id', 'activation_id',
            'service_version', 'retention_count', 'rollback_artifact_verified',
            'rollback_identity_bound', 'atomic_swap_supported',
            'state_preservation_supported', 'cleanup_supported', 'policy_version',
            'execution_enabled', 'runtime_activation_allowed', 'rollback_executed',
            'process_started', 'state_changed',
        })
        for field in (
            'execution_enabled', 'runtime_activation_allowed',
            'rollback_executed', 'process_started', 'state_changed',
        ):
            self.assertFalse(encoded[field], field)

    def test_policy_valid_is_false_when_any_required_rollback_control_is_false(self):
        module = self.require_module()
        controls = (
            'rollback_artifact_verified', 'rollback_identity_bound',
            'atomic_swap_supported', 'state_preservation_supported',
            'cleanup_supported',
        )
        for control in controls:
            with self.subTest(control=control):
                result = module.validate_stt_rollback_policy_evidence(
                    valid_evidence(**{control: False})
                )
                self.assertFalse(result.policy_valid)
                self.assertFalse(result.as_dict()['policy_valid'])
                for field in (
                    'execution_enabled', 'runtime_activation_allowed',
                    'rollback_executed', 'process_started', 'state_changed',
                ):
                    self.assertFalse(getattr(result, field), field)

    def test_retention_count_is_exactly_one_and_booleans_are_real_booleans(self):
        module = self.require_module()
        self.assertEqual(module.RETENTION_COUNT, 1)
        for value in (0, 2, -1, True, 1.0, '1', None):
            with self.subTest(retention_count=value):
                self.assert_invalid(valid_evidence(retention_count=value))

        for field in (
            'rollback_artifact_verified', 'rollback_identity_bound',
            'atomic_swap_supported', 'state_preservation_supported',
            'cleanup_supported',
        ):
            for value in (1, 0, 'true', None, [], 1.0):
                with self.subTest(field=field, value=value):
                    self.assert_invalid(valid_evidence(**{field: value}))

    def test_bounded_identities_versions_and_expected_bindings_reject_drift(self):
        module = self.require_module()
        cases = (
            {'service_id': ''},
            {'service_id': 'other/service'},
            {'service_id': 'x' * 65},
            {'activation_id': ''},
            {'activation_id': '../activation'},
            {'activation_id': 'x' * 129},
            {'service_version': '1.0'},
            {'service_version': '1.0.0+build'},
            {'service_version': 'x' * 65},
            {'policy_version': ''},
            {'policy_version': 'policy version'},
            {'policy_version': 'x' * 65},
            {'service_id': True},
            {'activation_id': 1},
            {'service_version': 1},
            {'policy_version': None},
        )
        for overrides in cases:
            with self.subTest(overrides=overrides):
                self.assert_invalid(valid_evidence(**overrides))

        for kwargs in (
            {'expected_service_id': 'other-service'},
            {'expected_activation_id': 'other-activation'},
            {'expected_service_version': '9.9.9'},
            {'expected_policy_version': 'rollback-policy-v2'},
        ):
            with self.subTest(kwargs=kwargs):
                with self.assertRaises(module.STTRollbackPolicyEvidenceError):
                    module.validate_stt_rollback_policy_evidence(
                        valid_evidence(), **kwargs
                    )

    def test_closed_mapping_rejects_duplicate_unknown_and_runtime_shaped_fields(self):
        module = self.require_module()
        self.assertEqual(module.ROLLBACK_POLICY_FIELDS, frozenset(valid_evidence()))
        self.assert_invalid(DuplicateMapping(valid_evidence()))
        for extra in (
            {'unexpected': True},
            {'credentials': 'redacted'},
            {'token': 'redacted'},
            {'path': '/var/lib/stt'},
            {'url': 'https://example.invalid'},
            {'command': ['/bin/sh']},
            {'mounts': []},
            {'ports': [9000]},
            {'audio': 'pcm'},
            {'raw_audio': 'base64'},
            {'model': 'whisper'},
            {'model_data': 'base64'},
            {'runtime_ready': True},
        ):
            with self.subTest(extra=extra):
                self.assert_invalid(valid_evidence(**extra))

    def test_oversized_records_and_non_mappings_fail_closed(self):
        module = self.require_module()
        oversized = valid_evidence()
        oversized['policy_version'] = 'x' * (module.MAX_POLICY_VERSION_BYTES + 1)
        self.assert_invalid(oversized)

        oversized = valid_evidence()
        oversized['unexpected_padding'] = 'x' * module.MAX_EVIDENCE_BYTES
        self.assert_invalid(oversized)

        for value in (None, [], 'not-a-mapping', 1):
            with self.subTest(value=value):
                self.assert_invalid(value)

    def test_module_is_pure_and_exposes_no_runtime_or_io_operations(self):
        module = self.require_module()
        source = MODULE_PATH.read_text(encoding='utf-8')
        for forbidden in (
            'subprocess', 'socket.socket', 'socket.bind', 'socket.listen',
            'os.system', 'create_subprocess', 'urlopen', 'open(',
        ):
            self.assertNotIn(forbidden, source)
        result = module.validate_stt_rollback_policy_evidence(valid_evidence())
        self.assertFalse(result.execution_enabled)
        self.assertFalse(result.runtime_activation_allowed)
        self.assertFalse(result.rollback_executed)
        self.assertFalse(result.process_started)
        self.assertFalse(result.state_changed)


if __name__ == '__main__':
    unittest.main()
