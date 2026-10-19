import importlib.util
import sys
import unittest
from collections.abc import Mapping
from dataclasses import FrozenInstanceError
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
MODULE_PATH = ROOT / 'player-client/scripts/screentra-stt-model-load-evidence.py'


def load_module():
    if not MODULE_PATH.is_file():
        return None
    module_name = 'screentra_stt_model_load_evidence'
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
MODEL_ID = 'whisper-tiny.en'
MODEL_SHA256 = '921e4cf8686fdd993dcd081a5da5b6c365bfde1162e72b08d75ac75289920b1f'


def valid_evidence(**overrides):
    value = {
        'service_id': SERVICE_ID,
        'activation_id': ACTIVATION_ID,
        'model_id': MODEL_ID,
        'model_sha256': MODEL_SHA256,
        'model_file_observed': True,
        'load_observation_non_mutating': True,
        'load_identity_observed': True,
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


class STTModelLoadEvidenceTests(unittest.TestCase):
    def require_module(self):
        self.assertIsNotNone(
            evidence_module,
            'production STT model-load evidence module is not implemented',
        )
        return evidence_module

    def assert_invalid(self, evidence, **validation_kwargs):
        module = self.require_module()
        with self.assertRaises(module.STTModelLoadEvidenceError):
            module.validate_stt_model_load_evidence(evidence, **validation_kwargs)

    def test_valid_observation_is_immutable_metadata_only_and_not_readiness(self):
        module = self.require_module()
        result = module.validate_stt_model_load_evidence(
            valid_evidence(),
            expected_service_id=SERVICE_ID,
            expected_activation_id=ACTIVATION_ID,
            expected_model_id=MODEL_ID,
            expected_model_sha256=MODEL_SHA256,
        )

        self.assertIsInstance(result, module.STTModelLoadEvidence)
        self.assertTrue(result.load_observation_valid)
        for field in (
            'execution_enabled', 'runtime_activation_allowed', 'model_loaded',
            'inference_started', 'process_started',
        ):
            self.assertFalse(getattr(result, field), field)
        with self.assertRaises(FrozenInstanceError):
            result.service_id = 'other-service'

        encoded = result.as_dict()
        self.assertEqual(set(encoded), {
            'plan_type', 'effect', 'service_id', 'activation_id', 'model_id',
            'model_sha256', 'model_file_observed', 'load_observation_non_mutating',
            'load_identity_observed', 'load_observation_valid', 'execution_enabled',
            'runtime_activation_allowed', 'model_loaded', 'inference_started',
            'process_started',
        })
        for forbidden in (
            'path', 'model_path', 'audio', 'raw_audio', 'command', 'process_id',
            'container_id', 'runtime_ready', 'readiness_claim',
        ):
            self.assertNotIn(forbidden, encoded)

    def test_load_observation_valid_requires_all_three_real_true_observations(self):
        module = self.require_module()
        for field in (
            'model_file_observed', 'load_observation_non_mutating',
            'load_identity_observed',
        ):
            result = module.validate_stt_model_load_evidence(
                valid_evidence(**{field: False})
            )
            self.assertFalse(result.load_observation_valid)
            for inert_field in (
                'execution_enabled', 'runtime_activation_allowed', 'model_loaded',
                'inference_started', 'process_started',
            ):
                self.assertFalse(getattr(result, inert_field), inert_field)

        for field in (
            'model_file_observed', 'load_observation_non_mutating',
            'load_identity_observed',
        ):
            for value in (1, 0, 'true', None, [], 1.0):
                with self.subTest(field=field, value=value):
                    self.assert_invalid(valid_evidence(**{field: value}))

    def test_exact_identity_and_digest_bindings_are_required(self):
        module = self.require_module()
        for kwargs in (
            {'expected_service_id': 'other-service'},
            {'expected_activation_id': 'other-activation'},
            {'expected_model_id': 'other-model'},
            {'expected_model_sha256': 'b' * 64},
        ):
            with self.subTest(kwargs=kwargs):
                with self.assertRaises(module.STTModelLoadEvidenceError):
                    module.validate_stt_model_load_evidence(
                        valid_evidence(), **kwargs
                    )

        for overrides in (
            {'service_id': 'other-service'},
            {'activation_id': 'other-activation'},
            {'model_id': 'other-model'},
            {'model_sha256': 'b' * 64},
            {'model_sha256': 'A' * 64},
            {'model_sha256': '0' * 64},
            {'model_sha256': 'not-a-digest'},
        ):
            with self.subTest(overrides=overrides):
                self.assert_invalid(
                    valid_evidence(**overrides),
                    expected_service_id=SERVICE_ID,
                    expected_activation_id=ACTIVATION_ID,
                    expected_model_id=MODEL_ID,
                    expected_model_sha256=MODEL_SHA256,
                )

    def test_closed_mapping_rejects_duplicates_arbitrary_fields_and_unsafe_claims(self):
        module = self.require_module()
        self.assertEqual(module.MODEL_LOAD_EVIDENCE_FIELDS, frozenset(valid_evidence()))
        self.assert_invalid(DuplicateMapping(valid_evidence()))
        for extra in (
            {'unexpected': True},
            {'path': '/opt/models/whisper.bin'},
            {'url': 'https://example.invalid/model'},
            {'command': ['/bin/whisper-cli']},
            {'credentials': 'redacted'},
            {'token': 'redacted'},
            {'raw_model_bytes': 'AA=='},
            {'audio_bytes': 'AA=='},
            {'process_started': True},
            {'container_id': 'container-1'},
            {'runtime_ready': True},
            {'readiness_claim': True},
        ):
            with self.subTest(extra=extra):
                self.assert_invalid(valid_evidence(**extra))

    def test_oversized_records_non_mappings_and_side_effect_apis_fail_closed(self):
        module = self.require_module()
        oversized = valid_evidence(model_id='x' * (module.MAX_MODEL_ID_BYTES + 1))
        self.assert_invalid(oversized)
        oversized = valid_evidence()
        oversized['unexpected_padding'] = 'x' * module.MAX_EVIDENCE_BYTES
        self.assert_invalid(oversized)

        for value in (None, [], 'not-a-mapping', 1):
            with self.subTest(value=value):
                self.assert_invalid(value)

        source = MODULE_PATH.read_text(encoding='utf-8')
        for forbidden in (
            'subprocess', 'socket.socket', 'socket.bind', 'socket.listen',
            'os.system', 'create_subprocess', 'urlopen', 'open(',
        ):
            self.assertNotIn(forbidden, source)


if __name__ == '__main__':
    unittest.main()
