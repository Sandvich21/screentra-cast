import dataclasses
import importlib.util
import sys
import unittest
from collections.abc import Mapping
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
MODULE_PATH = ROOT / 'player-client/scripts/screentra-stt-runtime-installation-evidence.py'


def load_module():
    if not MODULE_PATH.is_file():
        return None
    module_name = 'screentra_stt_runtime_installation_evidence'
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
SERVICE_VERSION = '0.1.0'
RUNTIME_FAMILY = 'whisper.cpp'
RUNTIME_VERSION = '1.9.5'
SOURCE_COMMIT = 'd1be6fde11ac6e0407606b4e42fe72d34add8037'
RUNTIME_SHA256 = 'd8b3d4f22d96f2a5c26cc34966927c69ab88406b52cdc7cee7c94e75bee16d97'
MODEL_ID = 'whisper-tiny.en'
MODEL_SHA256 = '921e4cf8686fdd993dcd081a5da5b6c365bfde1162e72b08d75ac75289920b1f'


def valid_evidence(**overrides):
    value = {
        'service_id': SERVICE_ID,
        'activation_id': ACTIVATION_ID,
        'service_version': SERVICE_VERSION,
        'runtime_family': RUNTIME_FAMILY,
        'runtime_version': RUNTIME_VERSION,
        'source_commit': SOURCE_COMMIT,
        'runtime_sha256': RUNTIME_SHA256,
        'model_id': MODEL_ID,
        'model_sha256': MODEL_SHA256,
        'runtime_verified': True,
        'model_verified': True,
        'non_starting_check': True,
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


class STTRuntimeInstallationEvidenceTests(unittest.TestCase):
    def require_module(self):
        self.assertIsNotNone(
            evidence_module,
            'production STT runtime-installation evidence module is not implemented',
        )
        return evidence_module

    def assert_invalid(self, evidence, **kwargs):
        module = self.require_module()
        with self.assertRaises(module.STTRuntimeInstallationEvidenceError, **kwargs):
            module.validate_stt_runtime_installation_evidence(evidence)

    def test_actual_installed_artifacts_are_frozen_metadata_only_evidence(self):
        module = self.require_module()
        result = module.validate_stt_runtime_installation_evidence(
            valid_evidence(),
            expected_service_id=SERVICE_ID,
            expected_activation_id=ACTIVATION_ID,
            expected_service_version=SERVICE_VERSION,
            expected_model_id=MODEL_ID,
            expected_model_sha256=MODEL_SHA256,
        )

        self.assertIsInstance(result, module.STTRuntimeInstallationEvidence)
        self.assertEqual(
            (result.service_id, result.activation_id, result.service_version),
            (SERVICE_ID, ACTIVATION_ID, SERVICE_VERSION),
        )
        self.assertEqual(
            (result.runtime_family, result.runtime_version, result.source_commit),
            (RUNTIME_FAMILY, RUNTIME_VERSION, SOURCE_COMMIT),
        )
        self.assertEqual(
            (result.runtime_sha256, result.model_id, result.model_sha256),
            (RUNTIME_SHA256, MODEL_ID, MODEL_SHA256),
        )
        self.assertTrue(result.policy_valid)
        self.assertTrue(result.artifacts_verified)
        for field in (
            'execution_enabled', 'runtime_activation_allowed', 'process_started',
            'model_loaded', 'service_started',
        ):
            self.assertFalse(getattr(result, field), field)
        with self.assertRaises(dataclasses.FrozenInstanceError):
            result.service_id = 'other-service'

        encoded = result.as_dict()
        self.assertEqual(set(encoded), {
            'plan_type', 'effect', 'service_id', 'activation_id', 'service_version',
            'runtime_family', 'runtime_version', 'source_commit', 'runtime_sha256',
            'model_id', 'model_sha256', 'runtime_verified', 'model_verified',
            'non_starting_check', 'policy_valid', 'artifacts_verified',
            'execution_enabled', 'runtime_activation_allowed', 'process_started',
            'model_loaded', 'service_started',
        })
        self.assertNotIn('listener_bound', encoded)
        self.assertNotIn('capture_active', encoded)
        self.assertNotIn('inference_active', encoded)

    def test_policy_and_artifacts_verified_require_both_artifacts_and_non_starting_check(self):
        module = self.require_module()
        for field in ('runtime_verified', 'model_verified', 'non_starting_check'):
            with self.subTest(field=field):
                result = module.validate_stt_runtime_installation_evidence(
                    valid_evidence(**{field: False})
                )
                self.assertFalse(result.policy_valid)
                self.assertFalse(result.artifacts_verified)
                for inert_field in (
                    'execution_enabled', 'runtime_activation_allowed',
                    'process_started', 'model_loaded', 'service_started',
                ):
                    self.assertFalse(getattr(result, inert_field), inert_field)

    def test_real_booleans_and_bounded_identity_values_are_required(self):
        module = self.require_module()
        for field in (
            'runtime_verified', 'model_verified', 'non_starting_check',
        ):
            for value in (1, 0, 'true', None, [], 1.0):
                with self.subTest(field=field, value=value):
                    self.assert_invalid(valid_evidence(**{field: value}))

        for overrides in (
            {'service_id': ''},
            {'service_id': 'other/service'},
            {'activation_id': '../activation'},
            {'service_version': '1.0'},
            {'service_version': 'v1.9.5'},
            {'service_version': '1.9.5+build'},
            {'runtime_family': 'whisper'},
            {'runtime_version': 'latest'},
            {'runtime_version': 'v1.9.5'},
            {'runtime_version': '1.9'},
            {'source_commit': SOURCE_COMMIT[:-1]},
            {'source_commit': SOURCE_COMMIT.upper()},
            {'model_id': 'whisper/tiny'},
            {'model_id': ''},
        ):
            with self.subTest(overrides=overrides):
                self.assert_invalid(valid_evidence(**overrides))

    def test_exact_lowercase_artifact_digests_and_non_placeholder_values_are_required(self):
        module = self.require_module()
        for field in ('runtime_sha256', 'model_sha256'):
            for value in (
                'not-a-digest',
                'A' * 64,
                '0' * 64,
                'a' * 64,
                'f' * 64,
                RUNTIME_SHA256[:-1],
            ):
                with self.subTest(field=field, value=value):
                    self.assert_invalid(valid_evidence(**{field: value}))

    def test_expected_identity_bindings_reject_service_activation_version_and_model_drift(self):
        module = self.require_module()
        for kwargs in (
            {'expected_service_id': 'other-service'},
            {'expected_activation_id': 'other-activation'},
            {'expected_service_version': '9.9.9'},
            {'expected_model_id': 'other-model'},
            {'expected_model_sha256': 'b' * 64},
        ):
            with self.subTest(kwargs=kwargs):
                with self.assertRaises(module.STTRuntimeInstallationEvidenceError):
                    module.validate_stt_runtime_installation_evidence(
                        valid_evidence(), **kwargs
                    )

    def test_closed_mapping_rejects_duplicates_arbitrary_fields_and_runtime_claims(self):
        module = self.require_module()
        self.assertEqual(module.RUNTIME_INSTALLATION_EVIDENCE_FIELDS, frozenset(valid_evidence()))
        self.assert_invalid(DuplicateMapping(valid_evidence()))
        for extra in (
            {'unexpected': True},
            {'path': '/opt/whisper'},
            {'url': 'https://example.invalid/runtime'},
            {'command': ['/bin/whisper-cli']},
            {'credentials': 'redacted'},
            {'token': 'redacted'},
            {'tag': 'latest'},
            {'raw_audio': 'base64'},
            {'listener': True},
            {'capture': True},
            {'inference': True},
        ):
            with self.subTest(extra=extra):
                self.assert_invalid(valid_evidence(**extra))

    def test_oversized_records_non_mappings_and_side_effect_apis_fail_closed(self):
        module = self.require_module()
        oversized = valid_evidence(runtime_version='x' * (module.MAX_RUNTIME_VERSION_BYTES + 1))
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
