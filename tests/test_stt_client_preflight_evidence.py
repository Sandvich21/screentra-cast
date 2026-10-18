#!/usr/bin/env python3
import importlib.util
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
MODULE_PATH = ROOT / 'player-client/scripts/screentra-stt-client-preflight-evidence.py'


def load_module():
    if not MODULE_PATH.is_file():
        return None
    spec = importlib.util.spec_from_file_location(
        'screentra_stt_client_preflight_evidence', MODULE_PATH
    )
    module = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    spec.loader.exec_module(module)
    return module


preflight = load_module()


def valid_evidence(**overrides):
    evidence = {
        'host_id': 'client-56',
        'architecture': 'amd64',
        'cpu_count': 4,
        'memory_bytes': 8 * 1024 * 1024 * 1024,
        'free_bytes': 2 * 1024 * 1024 * 1024,
        'audio_capture_present': True,
        'pipewire_active': True,
        'wireplumber_active': True,
        'model_present': True,
        'model_id': 'whisper-tiny.en',
        'model_sha256': 'a' * 64,
        'local_runtime_present': True,
        'camera_present': True,
    }
    evidence.update(overrides)
    return evidence


class STTClientPreflightEvidenceTests(unittest.TestCase):
    def require_module(self):
        self.assertIsNotNone(
            preflight,
            'production STT preflight-evidence module is not implemented',
        )
        return preflight

    def test_valid_evidence_returns_metadata_only_non_readiness_decision(self):
        module = self.require_module()

        result = module.evaluate_preflight_evidence(valid_evidence())

        self.assertEqual(result['evidence_type'], 'stt_client_preflight_evidence')
        self.assertEqual(result['evaluation'], 'evidence_only')
        self.assertFalse(result['live_preflight'])
        self.assertFalse(result['readiness_claim'])
        self.assertEqual(result['blockers'], [])
        self.assertEqual(result['warnings'], [])
        self.assertFalse(result['execution_enabled'])
        self.assertFalse(result['runtime_ready'])
        self.assertFalse(result['hardware_accessed'])
        self.assertFalse(result['model_staged'])
        self.assertEqual(result['evidence']['host_id'], 'client-56')
        self.assertEqual(result['evidence']['model_sha256'], 'a' * 64)

    def test_blockers_are_deterministic_for_missing_audio_services_model_and_runtime(self):
        module = self.require_module()

        result = module.evaluate_preflight_evidence(valid_evidence(
            audio_capture_present=False,
            pipewire_active=False,
            wireplumber_active=False,
            model_present=False,
            model_id=None,
            model_sha256=None,
            local_runtime_present=False,
        ))

        self.assertEqual(result['blockers'], [
            'audio_capture_missing',
            'audio_service_inactive',
            'model_missing',
            'local_runtime_missing',
        ])
        self.assertEqual(result['warnings'], [])
        self.assertFalse(result['runtime_ready'])

    def test_model_identity_must_be_a_paired_lowercase_sha256_identity(self):
        module = self.require_module()

        for overrides in (
            {'model_id': None, 'model_sha256': 'a' * 64},
            {'model_id': 'whisper-tiny.en', 'model_sha256': None},
            {'model_id': 'whisper/tiny', 'model_sha256': 'a' * 64},
            {'model_id': 'whisper-tiny.en', 'model_sha256': 'A' * 64},
        ):
            with self.subTest(overrides=overrides):
                result = module.evaluate_preflight_evidence(valid_evidence(**overrides))
                self.assertEqual(result['blockers'], ['model_identity_invalid'])
                self.assertIsNone(result['evidence']['model_id'])
                self.assertIsNone(result['evidence']['model_sha256'])
                self.assertFalse(result['runtime_ready'])

    def test_model_identity_is_not_claimed_when_model_is_absent(self):
        module = self.require_module()

        result = module.evaluate_preflight_evidence(valid_evidence(
            model_present=False,
            model_id=None,
            model_sha256=None,
        ))

        self.assertEqual(result['blockers'], ['model_missing'])
        self.assertIsNone(result['evidence']['model_id'])
        self.assertIsNone(result['evidence']['model_sha256'])

        mismatched = module.evaluate_preflight_evidence(valid_evidence(
            model_present=False,
            model_id='whisper-tiny.en',
            model_sha256='a' * 64,
        ))
        self.assertEqual(mismatched['blockers'], [
            'model_missing',
            'model_identity_invalid',
        ])
        self.assertIsNone(mismatched['evidence']['model_id'])
        self.assertIsNone(mismatched['evidence']['model_sha256'])

    def test_optional_camera_evidence_missing_is_a_warning_not_a_blocker(self):
        module = self.require_module()
        evidence = valid_evidence()
        del evidence['camera_present']

        result = module.evaluate_preflight_evidence(evidence)

        self.assertEqual(result['blockers'], [])
        self.assertEqual(result['warnings'], ['camera_evidence_unavailable'])
        self.assertNotIn('camera_present', result['evidence'])

    def test_exact_fields_and_secret_or_execution_shaped_values_are_rejected(self):
        module = self.require_module()

        cases = (
            {'unexpected': True},
            {'url': 'https://example.invalid'},
            {'path': '/dev/snd/pcmC0D0c'},
            {'command': ['arecord']},
            {'credentials': 'redacted'},
            {'raw_audio': 'base64'},
            {'token': 'redacted'},
            {'host_id': 'client-secret-56'},
            {'host_id': 'client/56'},
            {'architecture': 'x86_64'},
            {'cpu_count': True},
            {'memory_bytes': -1},
            {'free_bytes': 9 * 1024 * 1024 * 1024},
            {'audio_capture_present': 1},
            {'camera_present': 'yes'},
        )
        for overrides in cases:
            with self.subTest(overrides=overrides):
                with self.assertRaises(module.STTPreflightEvidenceError):
                    module.evaluate_preflight_evidence(valid_evidence(**overrides))

    def test_non_mapping_missing_fields_and_oversized_evidence_are_rejected(self):
        module = self.require_module()

        with self.assertRaises(module.STTPreflightEvidenceError):
            module.evaluate_preflight_evidence([])
        with self.assertRaises(module.STTPreflightEvidenceError):
            module.evaluate_preflight_evidence({})
        with self.assertRaises(module.STTPreflightEvidenceError):
            module.evaluate_preflight_evidence(valid_evidence(host_id='x' * 65))
        with self.assertRaises(module.STTPreflightEvidenceError):
            module.evaluate_preflight_evidence(
                valid_evidence(host_id='x' * module.MAX_HOST_ID_BYTES)
                | {'padding': 'x' * module.MAX_EVIDENCE_BYTES}
            )


if __name__ == '__main__':
    unittest.main()
