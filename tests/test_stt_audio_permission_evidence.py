import dataclasses
import importlib.util
import sys
import unittest
from collections.abc import Mapping
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
MODULE_PATH = ROOT / 'player-client/scripts/screentra-stt-audio-permission-evidence.py'


def load_module():
    if not MODULE_PATH.is_file():
        return None
    module_name = 'screentra_stt_audio_permission_evidence'
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
HOST_ID = 'client-56'


def valid_evidence(**overrides):
    value = {
        'service_id': SERVICE_ID,
        'activation_id': ACTIVATION_ID,
        'host_id': HOST_ID,
        'audio_backend': 'pipewire',
        'permission_source': 'group_membership',
        'permission_observed': True,
        'capture_device_observed': True,
        'user_service_observed': True,
        'non_capture_check': True,
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


class STTAudioPermissionEvidenceTests(unittest.TestCase):
    def require_module(self):
        self.assertIsNotNone(
            evidence_module,
            'production STT audio-permission evidence module is not implemented',
        )
        return evidence_module

    def assert_invalid(self, evidence, **validator_kwargs):
        module = self.require_module()
        with self.assertRaises(module.STTAudioPermissionEvidenceError):
            module.validate_stt_audio_permission_evidence(evidence, **validator_kwargs)

    def test_valid_evidence_is_immutable_identity_bound_and_inert(self):
        module = self.require_module()
        result = module.validate_stt_audio_permission_evidence(
            valid_evidence(),
            expected_service_id=SERVICE_ID,
            expected_activation_id=ACTIVATION_ID,
            expected_host_id=HOST_ID,
        )

        self.assertIsInstance(result, module.STTAudioPermissionEvidence)
        self.assertTrue(result.policy_valid)
        self.assertEqual(
            (result.service_id, result.activation_id, result.host_id),
            (SERVICE_ID, ACTIVATION_ID, HOST_ID),
        )
        for field in (
            'execution_enabled', 'runtime_activation_allowed',
            'hardware_permission_activated', 'microphone_accessed',
            'audio_captured', 'listener_bound',
        ):
            self.assertFalse(getattr(result, field), field)
        with self.assertRaises(dataclasses.FrozenInstanceError):
            result.service_id = 'other-service'

        encoded = result.as_dict()
        self.assertEqual(set(encoded), {
            'plan_type', 'effect', 'service_id', 'activation_id', 'host_id',
            'audio_backend', 'permission_source', 'permission_observed',
            'capture_device_observed', 'user_service_observed', 'non_capture_check',
            'policy_valid', 'execution_enabled', 'runtime_activation_allowed',
            'hardware_permission_activated', 'microphone_accessed',
            'audio_captured', 'listener_bound',
        })
        self.assertNotIn('readiness_claim', encoded)
        self.assertNotIn('audio_bytes', encoded)

    def test_policy_valid_requires_all_four_observations_but_remains_inert(self):
        module = self.require_module()
        for field in (
            'permission_observed', 'capture_device_observed',
            'user_service_observed', 'non_capture_check',
        ):
            result = module.validate_stt_audio_permission_evidence(
                valid_evidence(**{field: False})
            )
            self.assertFalse(result.policy_valid, field)
            self.assertFalse(result.execution_enabled)
            self.assertFalse(result.runtime_activation_allowed)
            self.assertFalse(result.hardware_permission_activated)
            self.assertFalse(result.microphone_accessed)
            self.assertFalse(result.audio_captured)
            self.assertFalse(result.listener_bound)

    def test_exact_identities_and_allowlists_are_required(self):
        module = self.require_module()
        for kwargs in (
            {'expected_service_id': 'other-service'},
            {'expected_activation_id': 'other-activation'},
            {'expected_host_id': 'other-host'},
        ):
            with self.subTest(kwargs=kwargs):
                self.assert_invalid(valid_evidence(), **kwargs)

        for field, values in (
            ('audio_backend', ('alsa', 'pipewire/extra', 'https://audio.invalid')),
            ('permission_source', ('sudo', 'path/to/group', 'user_service --start')),
        ):
            for value in values:
                with self.subTest(field=field, value=value):
                    self.assert_invalid(valid_evidence(**{field: value}))

    def test_real_booleans_are_required(self):
        for field in (
            'permission_observed', 'capture_device_observed',
            'user_service_observed', 'non_capture_check',
        ):
            for value in (1, 0, 'true', None):
                with self.subTest(field=field, value=value):
                    self.assert_invalid(valid_evidence(**{field: value}))

    def test_closed_fields_reject_arbitrary_sensitive_or_runtime_claims(self):
        forbidden = (
            {'unexpected': True},
            {'path': '/tmp/audio'},
            {'url': 'https://example.invalid'},
            {'command': ['pw-record']},
            {'credentials': 'redacted'},
            {'raw_audio': 'redacted'},
            {'audio_bytes': 'AA=='},
            {'device_path': '/dev/snd'},
            {'hardware_capture_enabled': True},
            {'capture_started': True},
            {'permission_activated': True},
            {'readiness_claim': True},
            {'runtime_ready': True},
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

    def test_identifiers_are_bounded_and_non_path_non_secret_metadata(self):
        cases = (
            ('service_id', '../service'),
            ('activation_id', 'a' * 129),
            ('host_id', '/dev/audio'),
            ('service_id', 'audio-token'),
            ('activation_id', 'https://example.invalid'),
            ('host_id', 'client\\56'),
        )
        for field, value in cases:
            with self.subTest(field=field, value=value):
                self.assert_invalid(valid_evidence(**{field: value}))

    def test_non_mapping_and_non_json_values_fail_closed(self):
        self.assert_invalid([])
        self.assert_invalid(valid_evidence(host_id=object()))
        self.assert_invalid(valid_evidence(permission_observed=float('nan')))


if __name__ == '__main__':
    unittest.main()
