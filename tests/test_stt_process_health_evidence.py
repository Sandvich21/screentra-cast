import importlib.util
import sys
import unittest
from collections.abc import Mapping
from dataclasses import FrozenInstanceError
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
MODULE_PATH = ROOT / 'player-client/scripts/screentra-stt-process-health-evidence.py'


def load_module():
    if not MODULE_PATH.is_file():
        return None
    module_name = 'screentra_stt_process_health_evidence'
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
SERVICE_VERSION = '0.1.0'
ACTIVATION_ID = 'activation-20261018-01'


def valid_evidence(**overrides):
    value = {
        'service_id': SERVICE_ID,
        'activation_id': ACTIVATION_ID,
        'service_version': SERVICE_VERSION,
        'health_protocol': 'local_ai_health_v1',
        'process_observed': True,
        'health_response_observed': True,
        'identity_observed': True,
        'health_observation_non_mutating': True,
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


class STTProcessHealthEvidenceTests(unittest.TestCase):
    def require_module(self):
        self.assertIsNotNone(
            evidence_module,
            'production STT process-health evidence module is not implemented',
        )
        return evidence_module

    def assert_invalid(self, evidence, **validation_kwargs):
        module = self.require_module()
        with self.assertRaises(module.STTProcessHealthEvidenceError):
            module.validate_stt_process_health_evidence(evidence, **validation_kwargs)

    def test_valid_observation_is_immutable_metadata_only_and_not_readiness(self):
        module = self.require_module()
        result = module.validate_stt_process_health_evidence(
            valid_evidence(),
            expected_service_id=SERVICE_ID,
            expected_activation_id=ACTIVATION_ID,
            expected_service_version=SERVICE_VERSION,
        )

        self.assertIsInstance(result, module.STTProcessHealthEvidence)
        self.assertTrue(result.health_observation_valid)
        with self.assertRaises(FrozenInstanceError):
            result.service_id = 'other-service'
        for field in (
            'execution_enabled', 'runtime_activation_allowed', 'process_started',
            'service_healthy', 'health_observed',
        ):
            self.assertFalse(getattr(result, field), field)

        encoded = result.as_dict()
        self.assertEqual(set(encoded), {
            'plan_type', 'effect', 'service_id', 'activation_id', 'service_version',
            'health_protocol', 'process_observed', 'health_response_observed',
            'identity_observed', 'health_observation_non_mutating',
            'health_observation_valid', 'execution_enabled',
            'runtime_activation_allowed', 'process_started', 'service_healthy',
            'health_observed',
        })
        for forbidden in (
            'path', 'url', 'command', 'pid', 'port', 'credentials', 'raw_audio',
            'model_bytes', 'container_id', 'runtime_ready', 'readiness_claim',
            'lifecycle_state',
        ):
            self.assertNotIn(forbidden, encoded)

    def test_health_observation_valid_requires_all_four_real_true_observations(self):
        module = self.require_module()
        for field in (
            'process_observed', 'health_response_observed', 'identity_observed',
            'health_observation_non_mutating',
        ):
            result = module.validate_stt_process_health_evidence(
                valid_evidence(**{field: False})
            )
            self.assertFalse(result.health_observation_valid)
            for inert_field in (
                'execution_enabled', 'runtime_activation_allowed', 'process_started',
                'service_healthy', 'health_observed',
            ):
                self.assertFalse(getattr(result, inert_field), inert_field)

        for field in (
            'process_observed', 'health_response_observed', 'identity_observed',
            'health_observation_non_mutating',
        ):
            for value in (1, 0, 'true', None, [], 1.0):
                with self.subTest(field=field, value=value):
                    self.assert_invalid(valid_evidence(**{field: value}))

    def test_fixed_protocol_and_exact_identity_bindings_are_required(self):
        module = self.require_module()
        for kwargs in (
            {'expected_service_id': 'other-service'},
            {'expected_activation_id': 'other-activation'},
            {'expected_service_version': '9.9.9'},
        ):
            with self.subTest(kwargs=kwargs):
                self.assert_invalid(valid_evidence(), **kwargs)

        for overrides in (
            {'service_id': 'other-service'},
            {'activation_id': 'other-activation'},
            {'service_version': '9.9.9'},
            {'health_protocol': 'http'},
            {'health_protocol': 'local_ai_health_v2'},
        ):
            with self.subTest(overrides=overrides):
                self.assert_invalid(
                    valid_evidence(**overrides),
                    expected_service_id=SERVICE_ID,
                    expected_activation_id=ACTIVATION_ID,
                    expected_service_version=SERVICE_VERSION,
                )

    def test_closed_mapping_rejects_duplicates_arbitrary_fields_and_unsafe_claims(self):
        module = self.require_module()
        self.assertEqual(module.PROCESS_HEALTH_EVIDENCE_FIELDS, frozenset(valid_evidence()))
        self.assert_invalid(DuplicateMapping(valid_evidence()))
        for extra in (
            {'unexpected': True},
            {'path': '/run/stt.sock'},
            {'url': 'https://example.invalid/health'},
            {'command': ['/bin/whisper-cli']},
            {'credentials': 'redacted'},
            {'token': 'redacted'},
            {'raw_audio': 'AA=='},
            {'raw_model_bytes': 'AA=='},
            {'pid': 1234},
            {'port': 9000},
            {'listener_control': 'bind'},
            {'container_id': 'container-1'},
            {'service_started': True},
            {'runtime_ready': True},
            {'readiness_claim': True},
            {'lifecycle_state': 'running'},
        ):
            with self.subTest(extra=extra):
                self.assert_invalid(valid_evidence(**extra))

    def test_oversized_records_non_mappings_and_side_effect_apis_fail_closed(self):
        module = self.require_module()
        oversized = valid_evidence(service_version='x' * (module.MAX_VERSION_BYTES + 1))
        self.assert_invalid(oversized)
        oversized = valid_evidence()
        oversized['unexpected_padding'] = 'x' * module.MAX_EVIDENCE_BYTES
        self.assert_invalid(oversized)

        for value in (None, [], 'not-a-mapping', 1):
            with self.subTest(value=value):
                self.assert_invalid(value)

        source = MODULE_PATH.read_text(encoding='utf-8') if MODULE_PATH.is_file() else ''
        for forbidden in (
            'subprocess', 'socket.socket', 'socket.bind', 'socket.listen',
            'os.system', 'create_subprocess', 'urlopen', 'open(',
        ):
            self.assertNotIn(forbidden, source)


if __name__ == '__main__':
    unittest.main()
