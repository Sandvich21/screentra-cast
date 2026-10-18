#!/usr/bin/env python3
import importlib.util
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
MODULE_PATH = ROOT / 'player-client/scripts/screentra-capability-admission.py'
spec = importlib.util.spec_from_file_location('screentra_capability_admission', MODULE_PATH)
admission = importlib.util.module_from_spec(spec)
assert spec.loader is not None
sys.modules[spec.name] = admission
spec.loader.exec_module(admission)


CLIENT_ID = 'client-a'
ADDON_ID = 'magic-mirror'


def manifest(**overrides):
    value = {
        'client_id': CLIENT_ID,
        'addon_id': ADDON_ID,
        'capabilities': ['render_only'],
    }
    value.update(overrides)
    return value


def policy(**overrides):
    value = {
        'client_id': CLIENT_ID,
        'addon_id': ADDON_ID,
        'approved_capabilities': ['render_only'],
    }
    value.update(overrides)
    return value


class CapabilityAdmissionTests(unittest.TestCase):
    def test_default_policy_denies_capture_and_inference(self):
        result = admission.admit_capabilities(
            manifest(capabilities=[
                'camera_capture',
                'microphone_capture',
                'local_inference',
                'client_service',
            ]),
        )

        self.assertFalse(result.admitted)
        self.assertEqual(result.capabilities, ())
        self.assertEqual(result.reason, 'requested capabilities are not approved')
        self.assertFalse(result.execution_enabled)

    def test_render_only_is_admitted_by_explicit_policy(self):
        result = admission.admit_capabilities(manifest(), policy())

        self.assertTrue(result.admitted)
        self.assertEqual(result.capabilities, ('render_only',))
        self.assertEqual(result.client_id, CLIENT_ID)
        self.assertEqual(result.addon_id, ADDON_ID)
        self.assertEqual(result.effect, 'metadata_only')

    def test_requested_capabilities_must_be_a_subset_of_approval(self):
        result = admission.admit_capabilities(
            manifest(capabilities=['render_only', 'camera_capture']),
            policy(approved_capabilities=['render_only']),
        )

        self.assertFalse(result.admitted)
        self.assertEqual(result.capabilities, ())
        self.assertEqual(result.approved_capabilities, ('render_only',))

    def test_local_inference_requires_dependencies_in_requested_and_approved_sets(self):
        with self.assertRaises(admission.CapabilityAdmissionError):
            admission.admit_capabilities(
                manifest(capabilities=['local_inference', 'client_service']),
                policy(approved_capabilities=['local_inference', 'client_service', 'microphone_capture']),
            )

        with self.assertRaises(admission.CapabilityAdmissionError):
            admission.admit_capabilities(
                manifest(capabilities=['local_inference', 'microphone_capture', 'client_service']),
                policy(approved_capabilities=['local_inference', 'microphone_capture']),
            )

    def test_client_and_addon_identity_must_match_policy(self):
        for field in ('client_id', 'addon_id'):
            with self.subTest(field=field), self.assertRaises(admission.CapabilityAdmissionError):
                admission.admit_capabilities(
                    manifest(),
                    policy(**{field: 'other-identity'}),
                )

    def test_unknown_duplicate_and_malformed_fields_are_rejected(self):
        cases = (
            manifest(capabilities=['unknown_capability']),
            manifest(capabilities=['render_only', 'render_only']),
            manifest(capabilities='render_only'),
            {**manifest(), 'unexpected': True},
            {**manifest(), 'command': ['/bin/true']},
            {**manifest(), 'path': '/tmp/service'},
            {**manifest(), 'credential': 'secret'},
            {**manifest(), 'process': {'name': 'service'}},
            {**manifest(), 'execution_enabled': True},
        )
        for value in cases:
            with self.subTest(value=value), self.assertRaises(admission.CapabilityAdmissionError):
                admission.admit_capabilities(value, policy())

    def test_admission_result_is_metadata_only_and_never_execution_enabled(self):
        result = admission.admit_capabilities(manifest(), policy())
        encoded = result.as_dict()

        self.assertFalse(result.execution_enabled)
        self.assertFalse(encoded['execution_enabled'])
        self.assertEqual(encoded['effect'], 'metadata_only')
        self.assertNotIn('hardware_permission_active', encoded)
        self.assertNotIn('service_runtime_active', encoded)
        self.assertNotIn('process', encoded)
        self.assertNotIn('credential', encoded)
        self.assertNotIn('path', encoded)
        self.assertNotIn('command', encoded)


if __name__ == '__main__':
    unittest.main()
