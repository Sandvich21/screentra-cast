import copy
import importlib.util
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
MODULE_PATH = ROOT / 'player-client/scripts/screentra-stt-runtime-admission.py'


def load_module():
    if not MODULE_PATH.is_file():
        return None
    spec = importlib.util.spec_from_file_location(
        'screentra_stt_runtime_admission', MODULE_PATH
    )
    module = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    spec.loader.exec_module(module)
    return module


admission = load_module()


SERVICE_ID = 'magic-mirror-stt'
SERVICE_VERSION = '0.1.0'
ACTIVATION_ID = 'activation-20261018-01'
HOST_ID = 'client-56'
MODEL_ID = 'whisper-tiny.en'
MODEL_SHA256 = 'a' * 64


def valid_preflight(**overrides):
    evidence = {
        'host_id': HOST_ID,
        'architecture': 'amd64',
        'cpu_count': 4,
        'memory_bytes': 8 * 1024 * 1024 * 1024,
        'free_bytes': 2 * 1024 * 1024 * 1024,
        'audio_capture_present': True,
        'pipewire_active': True,
        'wireplumber_active': True,
        'model_present': True,
        'model_id': MODEL_ID,
        'model_sha256': MODEL_SHA256,
        'local_runtime_present': True,
        'camera_present': True,
    }
    evidence.update(overrides)
    return {
        'evidence_type': 'stt_client_preflight_evidence',
        'evaluation': 'evidence_only',
        'live_preflight': False,
        'readiness_claim': False,
        'evidence': evidence,
        'blockers': [],
        'warnings': [],
        'execution_enabled': False,
        'runtime_ready': False,
        'hardware_accessed': False,
        'model_staged': False,
    }


def valid_model_plan(**overrides):
    value = {
        'plan_type': 'local_model_staging_plan',
        'intent': 'verify',
        'service_id': SERVICE_ID,
        'activation_id': ACTIVATION_ID,
        'model_id': MODEL_ID,
        'model_sha256': MODEL_SHA256,
        'approved_model_root': '/var/lib/screentra-cast/models',
        'model_path': 'whisper-tiny.en.bin',
        'ok': True,
        'code': 'verified',
        'effect': 'metadata_only',
        'execution_enabled': False,
        'downloaded': False,
        'staged': False,
        'loaded': False,
        'process_started': False,
        'network_used': False,
        'verification_plan': {
            'read_only': True,
            'verified': True,
            'model_path': 'whisper-tiny.en.bin',
            'size_bytes': 1234,
        },
        'stage_plan': {
            'effect': 'metadata_only',
            'execution_enabled': False,
            'requested_intent': 'verify',
            'filesystem_touched': False,
        },
    }
    value.update(overrides)
    return value


def valid_supervisor(**overrides):
    value = {
        'plan_type': 'client_service_activation_plan',
        'intent': 'health',
        'service_id': SERVICE_ID,
        'service_version': SERVICE_VERSION,
        'activation_id': ACTIVATION_ID,
        'socket_name': SERVICE_ID,
        'model_id': MODEL_ID,
        'model_sha256': MODEL_SHA256,
        'lifecycle_state': 'staged',
        'lifecycle_state_changed': False,
        'execution_enabled': False,
        'effect': 'metadata_only',
        'process_started': False,
        'socket_bound': False,
        'model_loaded': False,
        'ok': True,
        'code': 'metadata_only',
        'observation': None,
        'result': {
            'ok': True,
            'code': 'metadata_only',
            'execution_enabled': False,
            'effect': 'metadata_only',
            'lifecycle_state': 'staged',
            'lifecycle_state_changed': False,
            'process_started': False,
            'socket_bound': False,
            'model_loaded': False,
        },
    }
    value.update(overrides)
    return value


def valid_request(**overrides):
    value = {
        'service_id': SERVICE_ID,
        'service_version': SERVICE_VERSION,
        'activation_id': ACTIVATION_ID,
        'host_id': HOST_ID,
        'model_id': MODEL_ID,
        'model_sha256': MODEL_SHA256,
        'preflight_evidence': valid_preflight(),
        'model_staging_plan': valid_model_plan(),
        'supervisor_plan': valid_supervisor(),
    }
    value.update(overrides)
    return value


def approved_gate(**overrides):
    value = {
        'gate_type': 'approved_stt_runtime_activation_gate',
        'approved': True,
        'service_id': SERVICE_ID,
        'service_version': SERVICE_VERSION,
        'activation_id': ACTIVATION_ID,
        'host_id': HOST_ID,
        'model_id': MODEL_ID,
        'model_sha256': MODEL_SHA256,
        'execution_enabled': False,
    }
    value.update(overrides)
    return value


class STTRuntimeAdmissionTests(unittest.TestCase):
    def require_module(self):
        self.assertIsNotNone(
            admission,
            'production STT runtime-admission module is not implemented',
        )
        return admission

    def test_complete_composition_is_denied_without_separate_approved_gate(self):
        module = self.require_module()

        result = module.decide_runtime_admission(valid_request())

        self.assertEqual(result['decision_type'], 'stt_runtime_admission')
        self.assertEqual(result['admission'], 'denied')
        self.assertEqual(result['reason'], 'approved_activation_gate_missing')
        self.assertFalse(result['runtime_activation_allowed'])
        self.assertFalse(result['execution_enabled'])
        self.assertEqual(result['service_id'], SERVICE_ID)
        self.assertEqual(result['service_version'], SERVICE_VERSION)
        self.assertEqual(result['activation_id'], ACTIVATION_ID)
        self.assertEqual(result['host_id'], HOST_ID)
        self.assertEqual(result['model_id'], MODEL_ID)
        self.assertEqual(result['model_sha256'], MODEL_SHA256)

    def test_future_approved_gate_can_change_admission_but_never_enable_execution(self):
        module = self.require_module()

        result = module.decide_runtime_admission(
            valid_request(approved_activation_gate=approved_gate())
        )

        self.assertEqual(result['admission'], 'approved')
        self.assertTrue(result['runtime_activation_allowed'])
        self.assertFalse(result['execution_enabled'])
        self.assertEqual(result['reason'], 'approved_activation_gate_present')

    def test_all_identity_tuple_fields_must_match_across_outputs(self):
        module = self.require_module()
        cases = (
            {'service_id': 'other-service'},
            {'service_version': '9.9.9'},
            {'activation_id': 'other-activation'},
            {'host_id': 'other-host'},
            {'model_id': 'other-model'},
            {'model_sha256': 'b' * 64},
        )
        for change in cases:
            with self.subTest(change=change):
                request = valid_request()
                request.update(change)
                with self.assertRaises(module.RuntimeAdmissionError):
                    module.decide_runtime_admission(request)

        for component in ('preflight_evidence', 'model_staging_plan', 'supervisor_plan'):
            with self.subTest(component=component):
                request = valid_request()
                request[component] = copy.deepcopy(request[component])
                field = 'host_id' if component == 'preflight_evidence' else 'service_id'
                target = request[component]
                if component == 'preflight_evidence':
                    target['evidence'][field] = 'other-host'
                else:
                    target[field] = 'other-service'
                with self.assertRaises(module.RuntimeAdmissionError):
                    module.decide_runtime_admission(request)

    def test_preflight_camera_evidence_is_optional_for_stt_admission(self):
        module = self.require_module()
        preflight = valid_preflight()
        del preflight['evidence']['camera_present']
        preflight['warnings'] = ['camera_evidence_unavailable']

        result = module.decide_runtime_admission(
            valid_request(preflight_evidence=preflight)
        )

        self.assertEqual(result['admission'], 'denied')
        self.assertFalse(result['runtime_activation_allowed'])

    def test_preflight_requires_empty_blockers_audio_and_local_runtime(self):
        module = self.require_module()
        for change in (
            {'blockers': ['audio_capture_missing']},
            {'evidence': {**valid_preflight()['evidence'], 'audio_capture_present': False}},
            {'evidence': {**valid_preflight()['evidence'], 'pipewire_active': False}},
            {'evidence': {**valid_preflight()['evidence'], 'wireplumber_active': False}},
            {'evidence': {**valid_preflight()['evidence'], 'local_runtime_present': False}},
        ):
            with self.subTest(change=change):
                preflight = valid_preflight()
                preflight.update(change)
                with self.assertRaises(module.RuntimeAdmissionError):
                    module.decide_runtime_admission(
                        valid_request(preflight_evidence=preflight)
                    )

    def test_model_requires_verified_read_only_plan_and_no_runtime_claims(self):
        module = self.require_module()
        cases = (
            {'intent': 'stage'},
            {'code': 'execution_disabled'},
            {'ok': False},
            {'verification_plan': {'read_only': False, 'verified': True}},
            {'verification_plan': {'read_only': True, 'verified': False}},
            {'loaded': True},
            {'execution_enabled': True},
        )
        for change in cases:
            with self.subTest(change=change):
                plan = valid_model_plan()
                plan.update(change)
                with self.assertRaises(module.RuntimeAdmissionError):
                    module.decide_runtime_admission(
                        valid_request(model_staging_plan=plan)
                    )

    def test_supervisor_must_be_identity_bound_and_metadata_only(self):
        module = self.require_module()
        for change in (
            {'service_version': '9.9.9'},
            {'activation_id': 'other-activation'},
            {'model_loaded': True},
            {'socket_bound': True},
            {'process_started': True},
            {'execution_enabled': True},
            {'result': {**valid_supervisor()['result'], 'execution_enabled': True}},
        ):
            with self.subTest(change=change):
                plan = valid_supervisor()
                plan.update(change)
                with self.assertRaises(module.RuntimeAdmissionError):
                    module.decide_runtime_admission(
                        valid_request(supervisor_plan=plan)
                    )

    def test_arbitrary_secret_path_url_command_audio_and_readiness_fields_are_rejected(self):
        module = self.require_module()
        for extra in (
            {'unexpected': True},
            {'token': 'redacted'},
            {'credentials': 'redacted'},
            {'path': '/tmp/model.bin'},
            {'url': 'https://example.invalid'},
            {'command': ['/bin/true']},
            {'audio': 'raw audio'},
            {'runtime_ready': True},
            {'hardware_ready': True},
            {'model_ready': True},
            {'execution_enabled': True},
        ):
            with self.subTest(extra=extra):
                with self.assertRaises(module.RuntimeAdmissionError):
                    module.decide_runtime_admission(valid_request(**extra))

        bad_gate = approved_gate(path='/tmp/approval')
        with self.assertRaises(module.RuntimeAdmissionError):
            module.decide_runtime_admission(
                valid_request(approved_activation_gate=bad_gate)
            )

    def test_malformed_component_objects_and_oversized_request_fail_closed(self):
        module = self.require_module()
        for field, value in (
            ('preflight_evidence', []),
            ('model_staging_plan', 'not-an-object'),
            ('supervisor_plan', {'service_id': SERVICE_ID}),
            ('model_id', None),
            ('model_sha256', 'not-a-digest'),
        ):
            with self.subTest(field=field):
                with self.assertRaises(module.RuntimeAdmissionError):
                    module.decide_runtime_admission(valid_request(**{field: value}))

        oversized = valid_request()
        oversized['padding'] = 'x' * module.MAX_REQUEST_BYTES
        with self.assertRaises(module.RuntimeAdmissionError):
            module.decide_runtime_admission(oversized)

    def test_module_is_pure_and_does_not_expose_runtime_apis(self):
        module = self.require_module()
        source = MODULE_PATH.read_text(encoding='utf-8')
        for forbidden in (
            'subprocess', 'socket.socket', 'socket.bind', 'socket.listen',
            'os.system', 'create_subprocess', 'urlopen', 'open(',
        ):
            self.assertNotIn(forbidden, source)
        result = module.decide_runtime_admission(valid_request())
        self.assertFalse(result['execution_enabled'])


if __name__ == '__main__':
    unittest.main()
