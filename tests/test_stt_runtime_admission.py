import copy
import dataclasses
import importlib.util
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
MODULE_PATH = ROOT / 'player-client/scripts/screentra-stt-runtime-admission.py'
APPROVAL_MODULE_PATH = ROOT / 'player-client/scripts/screentra-stt-approval-record.py'
RESOURCE_POLICY_MODULE_PATH = ROOT / 'player-client/scripts/screentra-stt-resource-policy-evidence.py'
IPC_POLICY_MODULE_PATH = ROOT / 'player-client/scripts/screentra-stt-ipc-policy-evidence.py'
IPC_READINESS_EVIDENCE_MODULE_PATH = ROOT / 'player-client/scripts/screentra-stt-ipc-readiness-evidence.py'
ROLLBACK_POLICY_MODULE_PATH = ROOT / 'player-client/scripts/screentra-stt-rollback-policy-evidence.py'
CAPABILITY_ADMISSION_MODULE_PATH = ROOT / 'player-client/scripts/screentra-capability-admission.py'
RUNTIME_INSTALLATION_EVIDENCE_MODULE_PATH = ROOT / 'player-client/scripts/screentra-stt-runtime-installation-evidence.py'
MODEL_LOAD_EVIDENCE_MODULE_PATH = ROOT / 'player-client/scripts/screentra-stt-model-load-evidence.py'
AUDIO_PERMISSION_EVIDENCE_MODULE_PATH = ROOT / 'player-client/scripts/screentra-stt-audio-permission-evidence.py'
RESOURCE_ENFORCEMENT_EVIDENCE_MODULE_PATH = ROOT / 'player-client/scripts/screentra-stt-resource-enforcement-evidence.py'


def load_approval_module():
    spec = importlib.util.spec_from_file_location(
        'screentra_stt_approval_record', APPROVAL_MODULE_PATH
    )
    module = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


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


def load_resource_policy_module():
    spec = importlib.util.spec_from_file_location(
        'screentra_stt_resource_policy_evidence', RESOURCE_POLICY_MODULE_PATH
    )
    module = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


def load_ipc_policy_module():
    spec = importlib.util.spec_from_file_location(
        'screentra_stt_ipc_policy_evidence', IPC_POLICY_MODULE_PATH
    )
    module = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


def load_ipc_readiness_evidence_module():
    module_name = 'screentra_stt_ipc_readiness_evidence'
    existing = sys.modules.get(module_name)
    if existing is not None:
        return existing
    spec = importlib.util.spec_from_file_location(
        module_name, IPC_READINESS_EVIDENCE_MODULE_PATH
    )
    module = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


def load_rollback_policy_module():
    spec = importlib.util.spec_from_file_location(
        'screentra_stt_rollback_policy_evidence', ROLLBACK_POLICY_MODULE_PATH
    )
    module = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


def load_capability_admission_module():
    spec = importlib.util.spec_from_file_location(
        'screentra_capability_admission', CAPABILITY_ADMISSION_MODULE_PATH
    )
    module = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


def load_runtime_installation_evidence_module():
    module_name = 'screentra_stt_runtime_installation_evidence'
    existing = sys.modules.get(module_name)
    if existing is not None:
        return existing
    spec = importlib.util.spec_from_file_location(
        module_name,
        RUNTIME_INSTALLATION_EVIDENCE_MODULE_PATH,
    )
    module = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


def load_model_load_evidence_module():
    if not MODEL_LOAD_EVIDENCE_MODULE_PATH.is_file():
        return None
    module_name = 'screentra_stt_model_load_evidence'
    existing = sys.modules.get(module_name)
    if existing is not None:
        return existing
    spec = importlib.util.spec_from_file_location(
        module_name,
        MODEL_LOAD_EVIDENCE_MODULE_PATH,
    )
    module = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


def load_audio_permission_evidence_module():
    module_name = 'screentra_stt_audio_permission_evidence'
    existing = sys.modules.get(module_name)
    if existing is not None:
        return existing
    spec = importlib.util.spec_from_file_location(
        module_name,
        AUDIO_PERMISSION_EVIDENCE_MODULE_PATH,
    )
    module = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


def load_resource_enforcement_evidence_module():
    module_name = 'screentra_stt_resource_enforcement_evidence'
    existing = sys.modules.get(module_name)
    if existing is not None:
        return existing
    spec = importlib.util.spec_from_file_location(
        module_name,
        RESOURCE_ENFORCEMENT_EVIDENCE_MODULE_PATH,
    )
    module = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


admission = load_module()
approval = load_approval_module()
resource_policy = load_resource_policy_module()
ipc_policy = load_ipc_policy_module()
ipc_readiness_evidence = load_ipc_readiness_evidence_module()
rollback_policy = load_rollback_policy_module()
capability_admission = load_capability_admission_module()
runtime_installation_evidence = load_runtime_installation_evidence_module()
model_load_evidence = load_model_load_evidence_module()
audio_permission_evidence = load_audio_permission_evidence_module()
resource_enforcement_evidence = load_resource_enforcement_evidence_module()


SERVICE_ID = 'magic-mirror-stt'
SERVICE_VERSION = '0.1.0'
ACTIVATION_ID = 'activation-20261018-01'
SOCKET_NAME = 'magic-mirror-stt'
HOST_ID = 'client-56'
MODEL_ID = 'whisper-tiny.en'
RUNTIME_FAMILY = 'whisper.cpp'
RUNTIME_VERSION = '1.9.5'
SOURCE_COMMIT = 'd1be6fde11ac6e0407606b4e42fe72d34add8037'
RUNTIME_SHA256 = 'd8b3d4f22d96f2a5c26cc34966927c69ab88406b52cdc7cee7c94e75bee16d97'
MODEL_SHA256 = '921e4cf8686fdd993dcd081a5da5b6c365bfde1162e72b08d75ac75289920b1f'
ROLLBACK_POLICY_VERSION = 'rollback-policy-v1'
STT_CAPABILITIES = (
    'microphone_capture',
    'local_inference',
    'client_service',
)


def valid_approval_record(**overrides):
    value = {
        'record_type': 'stt_future_activation_approval',
        'service_id': SERVICE_ID,
        'service_version': SERVICE_VERSION,
        'activation_id': ACTIVATION_ID,
        'host_id': HOST_ID,
        'model_id': MODEL_ID,
        'model_sha256': MODEL_SHA256,
        'approver': 'admin-alice',
        'approved_at': '2026-10-18T14:30:00Z',
        'gates': [
            {'name': name, 'passed': True, 'reason': f'{name} evidence recorded'}
            for name in approval.REQUIRED_STT_APPROVAL_GATES
        ],
    }
    value.update(overrides)
    return approval.validate_stt_approval_record(value)


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


def valid_resource_policy(**overrides):
    value = {
        'service_id': SERVICE_ID,
        'activation_id': ACTIVATION_ID,
        'cpu_millis': 500,
        'memory_bytes': 128 * 1024 * 1024,
        'pids': 32,
        'writable_bytes': 64 * 1024 * 1024,
        'policy_version': 'resource-policy-v1',
    }
    value.update(overrides)
    return resource_policy.validate_resource_policy_evidence(value)


def valid_ipc_policy(**overrides):
    value = {
        'service_id': SERVICE_ID,
        'activation_id': ACTIVATION_ID,
        'socket_name': SOCKET_NAME,
        'protocol_version': 1,
        'max_frame_bytes': 64 * 1024,
        'max_request_bytes': 64 * 1024,
        'max_response_bytes': 64 * 1024,
        'allowed_operations': ['health', 'transcribe'],
        'local_only': True,
    }
    value.update(overrides)
    return ipc_policy.validate_stt_ipc_policy_evidence(
        value,
        expected_service_id=SERVICE_ID,
        expected_activation_id=ACTIVATION_ID,
        expected_socket_name=SOCKET_NAME,
    )


def valid_ipc_readiness_evidence(**overrides):
    value = {
        'service_id': SERVICE_ID,
        'activation_id': ACTIVATION_ID,
        'socket_name': SOCKET_NAME,
        'protocol_version': 1,
        'allowed_operations': ['health', 'transcribe'],
        'listener_observed': True,
        'peer_identity_observed': True,
        'hello_observed': True,
        'transport_non_mutating': True,
        'local_only': True,
    }
    value.update(overrides)
    return ipc_readiness_evidence.validate_stt_ipc_readiness_evidence(
        value,
        expected_service_id=SERVICE_ID,
        expected_activation_id=ACTIVATION_ID,
        expected_socket_name=SOCKET_NAME,
        expected_protocol_version=1,
        expected_allowed_operations=('health', 'transcribe'),
    )


def valid_rollback_policy(**overrides):
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
        'policy_version': ROLLBACK_POLICY_VERSION,
    }
    value.update(overrides)
    return rollback_policy.validate_stt_rollback_policy_evidence(
        value,
        expected_service_id=SERVICE_ID,
        expected_activation_id=ACTIVATION_ID,
        expected_service_version=SERVICE_VERSION,
        expected_policy_version=ROLLBACK_POLICY_VERSION,
    )


def valid_capability_admission(**overrides):
    manifest = {
        'client_id': HOST_ID,
        'addon_id': SERVICE_ID,
        'capabilities': list(STT_CAPABILITIES),
    }
    policy = {
        'client_id': HOST_ID,
        'addon_id': SERVICE_ID,
        'approved_capabilities': list(STT_CAPABILITIES),
    }
    result = capability_admission.admit_capabilities(manifest, policy)
    if overrides:
        result = dataclasses.replace(result, **overrides)
    return result


def valid_runtime_installation_evidence(**overrides):
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
    return runtime_installation_evidence.validate_stt_runtime_installation_evidence(value)


def valid_model_load_evidence(**overrides):
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
    return model_load_evidence.validate_stt_model_load_evidence(
        value,
        expected_service_id=SERVICE_ID,
        expected_activation_id=ACTIVATION_ID,
        expected_model_id=MODEL_ID,
        expected_model_sha256=MODEL_SHA256,
    )


def valid_audio_permission_evidence(**overrides):
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
    return audio_permission_evidence.validate_stt_audio_permission_evidence(
        value,
        expected_service_id=SERVICE_ID,
        expected_activation_id=ACTIVATION_ID,
        expected_host_id=HOST_ID,
    )


def valid_resource_enforcement_evidence(**overrides):
    value = {
        'service_id': SERVICE_ID,
        'activation_id': ACTIVATION_ID,
        'policy_version': 'resource-policy-v1',
        'cpu_enforced': True,
        'memory_enforced': True,
        'pids_enforced': True,
        'writable_enforced': True,
        'observation_non_mutating': True,
    }
    value.update(overrides)
    return resource_enforcement_evidence.validate_stt_resource_enforcement_evidence(
        value,
        expected_service_id=SERVICE_ID,
        expected_activation_id=ACTIVATION_ID,
        expected_policy_version='resource-policy-v1',
    )


def valid_request(**overrides):
    value = {
        'approval_record': valid_approval_record(),
        'preflight_evidence': valid_preflight(),
        'model_staging_plan': valid_model_plan(),
        'supervisor_plan': valid_supervisor(),
        'resource_policy_evidence': valid_resource_policy(),
        'ipc_policy_evidence': valid_ipc_policy(),
        'ipc_readiness_evidence': valid_ipc_readiness_evidence(),
        'rollback_policy_evidence': valid_rollback_policy(),
        'capability_admission': valid_capability_admission(),
        'runtime_installation_evidence': valid_runtime_installation_evidence(),
        'model_load_evidence': valid_model_load_evidence(),
        'audio_permission_evidence': valid_audio_permission_evidence(),
        'resource_enforcement_evidence': valid_resource_enforcement_evidence(),
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

    def test_validated_approval_record_is_approved_for_future_review_and_inert(self):
        module = self.require_module()

        result = module.decide_runtime_admission(valid_request())

        self.assertEqual(result['decision_type'], 'stt_runtime_admission')
        self.assertEqual(result['admission'], 'approved_for_future_review')
        self.assertEqual(result['reason'], 'approval_record_validated')
        self.assertFalse(result['runtime_activation_allowed'])
        self.assertFalse(result['execution_enabled'])
        self.assertFalse(result['process_started'])
        self.assertFalse(result['socket_bound'])
        self.assertFalse(result['model_loaded'])
        self.assertFalse(result['resource_enforced'])
        self.assertFalse(result['ipc_enforced'])
        self.assertFalse(result['lifecycle_state_changed'])
        self.assertEqual(result['service_id'], SERVICE_ID)
        self.assertEqual(result['service_version'], SERVICE_VERSION)
        self.assertEqual(result['activation_id'], ACTIVATION_ID)
        self.assertEqual(result['host_id'], HOST_ID)
        self.assertEqual(result['model_id'], MODEL_ID)
        self.assertEqual(result['model_sha256'], MODEL_SHA256)
        self.assertTrue(result['evidence_composed']['resource_policy'])
        self.assertTrue(result['evidence_composed']['ipc_policy'])
        self.assertTrue(result['evidence_composed']['capability_admission'])
        self.assertEqual(result['client_id'], HOST_ID)
        self.assertEqual(result['addon_id'], SERVICE_ID)
        self.assertEqual(tuple(result['requested_capabilities']), STT_CAPABILITIES)
        self.assertEqual(tuple(result['approved_capabilities']), STT_CAPABILITIES)
        self.assertEqual(tuple(result['capabilities']), STT_CAPABILITIES)
        self.assertEqual(
            tuple(result['capability_identity_tuple']),
            (HOST_ID, SERVICE_ID, STT_CAPABILITIES, STT_CAPABILITIES),
        )

    def test_runtime_installation_evidence_is_composed_as_exact_inert_identity_bound_evidence(self):
        module = self.require_module()

        result = module.decide_runtime_admission(valid_request())

        self.assertEqual(result['admission'], 'approved_for_future_review')
        self.assertTrue(result['evidence_composed']['runtime_installation'])
        self.assertEqual(result['runtime_family'], RUNTIME_FAMILY)
        self.assertEqual(result['runtime_version'], RUNTIME_VERSION)
        self.assertEqual(result['source_commit'], SOURCE_COMMIT)
        self.assertEqual(result['runtime_sha256'], RUNTIME_SHA256)
        self.assertEqual(result['model_id'], MODEL_ID)
        self.assertEqual(result['model_sha256'], MODEL_SHA256)
        for field in (
            'execution_enabled', 'runtime_activation_allowed', 'process_started',
            'model_loaded', 'service_started',
        ):
            self.assertFalse(result[field], field)

    def test_runtime_installation_evidence_requires_exact_validated_type_and_full_identity(self):
        module = self.require_module()
        valid = valid_runtime_installation_evidence()

        for value in (
            valid.as_dict(),
            {'service_id': SERVICE_ID},
            None,
        ):
            with self.subTest(value=value):
                with self.assertRaises(module.RuntimeAdmissionError):
                    module.decide_runtime_admission(
                        valid_request(runtime_installation_evidence=value)
                    )

        class DerivedRuntimeInstallationEvidence(runtime_installation_evidence.STTRuntimeInstallationEvidence):
            pass

        with self.assertRaises(module.RuntimeAdmissionError):
            module.decide_runtime_admission(
                valid_request(
                    runtime_installation_evidence=DerivedRuntimeInstallationEvidence(
                        **dataclasses.asdict(valid)
                    )
                )
            )

        for tampered in (
            dataclasses.replace(valid, service_id='other-service'),
            dataclasses.replace(valid, activation_id='other-activation'),
            dataclasses.replace(valid, service_version='9.9.9'),
            dataclasses.replace(valid, model_id='other-model'),
            dataclasses.replace(valid, model_sha256=RUNTIME_SHA256),
            dataclasses.replace(valid, policy_valid=False),
            dataclasses.replace(valid, artifacts_verified=False),
            dataclasses.replace(valid, execution_enabled=True),
            dataclasses.replace(valid, service_started=True),
        ):
            with self.subTest(tampered=tampered):
                with self.assertRaises(module.RuntimeAdmissionError):
                    module.decide_runtime_admission(
                        valid_request(runtime_installation_evidence=tampered)
                    )

    def test_runtime_installation_evidence_rejects_failed_artifact_or_non_starting_checks(self):
        module = self.require_module()
        for field in ('runtime_verified', 'model_verified', 'non_starting_check'):
            with self.subTest(field=field):
                evidence = runtime_installation_evidence.validate_stt_runtime_installation_evidence(
                    {
                        **{
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
                        },
                        field: False,
                    }
                )
                with self.assertRaises(module.RuntimeAdmissionError):
                    module.decide_runtime_admission(
                        valid_request(runtime_installation_evidence=evidence)
                    )

    def test_model_load_observation_is_composed_as_exact_inert_identity_bound_evidence(self):
        module = self.require_module()
        result = module.decide_runtime_admission(valid_request())

        self.assertEqual(result['admission'], 'approved_for_future_review')
        self.assertTrue(result['evidence_composed']['model_load'])
        self.assertTrue(result['model_load_observation_valid'])
        self.assertEqual(
            tuple(result['model_load_identity_tuple']),
            (SERVICE_ID, ACTIVATION_ID, MODEL_ID, MODEL_SHA256),
        )
        for field in (
            'execution_enabled', 'runtime_activation_allowed', 'model_loaded',
            'inference_started', 'process_started',
        ):
            self.assertFalse(result[field], field)
        for forbidden in ('runtime_ready', 'readiness_claim', 'load_ready'):
            self.assertNotIn(forbidden, result)

    def test_model_load_observation_requires_exact_type_identity_and_no_runtime_claims(self):
        module = self.require_module()
        valid = valid_model_load_evidence()
        for value in (valid.as_dict(), {'service_id': SERVICE_ID}, None):
            with self.subTest(value=value):
                with self.assertRaises(module.RuntimeAdmissionError):
                    module.decide_runtime_admission(
                        valid_request(model_load_evidence=value)
                    )

        class DerivedModelLoadEvidence(model_load_evidence.STTModelLoadEvidence):
            pass

        with self.assertRaises(module.RuntimeAdmissionError):
            module.decide_runtime_admission(
                valid_request(
                    model_load_evidence=DerivedModelLoadEvidence(
                        **dataclasses.asdict(valid)
                    )
                )
            )

        for tampered in (
            dataclasses.replace(valid, service_id='other-service'),
            dataclasses.replace(valid, activation_id='other-activation'),
            dataclasses.replace(valid, model_id='other-model'),
            dataclasses.replace(valid, model_sha256=RUNTIME_SHA256),
            dataclasses.replace(valid, load_observation_valid=False),
            dataclasses.replace(valid, execution_enabled=True),
            dataclasses.replace(valid, model_loaded=True),
            dataclasses.replace(valid, inference_started=True),
            dataclasses.replace(valid, process_started=True),
        ):
            with self.subTest(tampered=tampered):
                with self.assertRaises(module.RuntimeAdmissionError):
                    module.decide_runtime_admission(
                        valid_request(model_load_evidence=tampered)
                    )

    def test_model_load_identity_must_match_model_plan_and_installation_identity(self):
        module = self.require_module()
        for field, value in (
            ('service_id', 'other-service'),
            ('activation_id', 'other-activation'),
            ('model_id', 'other-model'),
            ('model_sha256', RUNTIME_SHA256),
        ):
            with self.subTest(field=field):
                evidence = dataclasses.replace(valid_model_load_evidence(), **{field: value})
                with self.assertRaises(module.RuntimeAdmissionError):
                    module.decide_runtime_admission(
                        valid_request(model_load_evidence=evidence)
                    )

    def test_audio_permission_evidence_is_composed_as_exact_inert_identity_bound_type(self):
        module = self.require_module()
        result = module.decide_runtime_admission(valid_request())

        self.assertEqual(result['admission'], 'approved_for_future_review')
        self.assertTrue(result['evidence_composed']['audio_permission'])
        self.assertEqual(result['audio_backend'], 'pipewire')
        self.assertEqual(result['permission_source'], 'group_membership')
        self.assertEqual(
            tuple(result['audio_permission_identity_tuple']),
            (SERVICE_ID, ACTIVATION_ID, HOST_ID, 'pipewire', 'group_membership'),
        )
        for field in (
            'execution_enabled', 'runtime_activation_allowed',
            'hardware_permission_activated', 'microphone_accessed',
            'audio_captured', 'listener_bound', 'process_started',
            'socket_bound', 'model_loaded', 'resource_enforced',
            'ipc_enforced', 'rollback_executed', 'lifecycle_state_changed',
        ):
            self.assertFalse(result[field], field)
        self.assertNotIn('runtime_ready', result)

    def test_audio_permission_evidence_requires_exact_validated_type_and_no_claims(self):
        module = self.require_module()
        valid = valid_audio_permission_evidence()
        for value in (valid.as_dict(), {'service_id': SERVICE_ID}, None):
            with self.subTest(value=value):
                with self.assertRaises(module.RuntimeAdmissionError):
                    module.decide_runtime_admission(
                        valid_request(audio_permission_evidence=value)
                    )

        class DerivedAudioPermissionEvidence(audio_permission_evidence.STTAudioPermissionEvidence):
            pass

        with self.assertRaises(module.RuntimeAdmissionError):
            module.decide_runtime_admission(
                valid_request(
                    audio_permission_evidence=DerivedAudioPermissionEvidence(
                        **dataclasses.asdict(valid)
                    )
                )
            )

        for tampered in (
            dataclasses.replace(valid, service_id='other-service'),
            dataclasses.replace(valid, activation_id='other-activation'),
            dataclasses.replace(valid, host_id='other-host'),
            dataclasses.replace(valid, policy_valid=False),
            dataclasses.replace(valid, execution_enabled=True),
            dataclasses.replace(valid, runtime_activation_allowed=True),
            dataclasses.replace(valid, hardware_permission_activated=True),
            dataclasses.replace(valid, microphone_accessed=True),
            dataclasses.replace(valid, audio_captured=True),
            dataclasses.replace(valid, listener_bound=True),
        ):
            with self.subTest(tampered=tampered):
                with self.assertRaises(module.RuntimeAdmissionError):
                    module.decide_runtime_admission(
                        valid_request(audio_permission_evidence=tampered)
                    )

    def test_audio_permission_evidence_rejects_failed_policy(self):
        module = self.require_module()
        failed = audio_permission_evidence.validate_stt_audio_permission_evidence(
            {
                'service_id': SERVICE_ID,
                'activation_id': ACTIVATION_ID,
                'host_id': HOST_ID,
                'audio_backend': 'pipewire',
                'permission_source': 'user_service',
                'permission_observed': False,
                'capture_device_observed': True,
                'user_service_observed': True,
                'non_capture_check': True,
            },
            expected_service_id=SERVICE_ID,
            expected_activation_id=ACTIVATION_ID,
            expected_host_id=HOST_ID,
        )
        self.assertFalse(failed.policy_valid)
        with self.assertRaises(module.RuntimeAdmissionError):
            module.decide_runtime_admission(
                valid_request(audio_permission_evidence=failed)
            )

    def test_audio_permission_raw_claims_are_not_request_fields(self):
        module = self.require_module()
        for extra in (
            {'permission_activated': True},
            {'capture_started': True},
            {'audio_bytes': 'AA=='},
            {'device_path': '/dev/snd'},
            {'readiness_claim': True},
        ):
            with self.subTest(extra=extra):
                request = valid_request()
                request.update(extra)
                with self.assertRaises(module.RuntimeAdmissionError):
                    module.decide_runtime_admission(request)

    def test_capability_admission_requires_exact_validated_immutable_result_type(self):
        module = self.require_module()
        valid = valid_capability_admission()

        for value in (
            valid.as_dict(),
            {'client_id': HOST_ID, 'addon_id': SERVICE_ID},
            None,
        ):
            with self.subTest(value=value):
                with self.assertRaises(module.RuntimeAdmissionError):
                    module.decide_runtime_admission(
                        valid_request(capability_admission=value)
                    )

        class DerivedCapabilityAdmission(capability_admission.CapabilityAdmission):
            pass

        with self.assertRaises(module.RuntimeAdmissionError):
            module.decide_runtime_admission(
                valid_request(capability_admission=DerivedCapabilityAdmission(**dataclasses.asdict(valid)))
            )

    def test_capability_admission_rejects_missing_approval_and_dependency_drift(self):
        module = self.require_module()
        manifest = {
            'client_id': HOST_ID,
            'addon_id': SERVICE_ID,
            'capabilities': list(STT_CAPABILITIES),
        }
        default_denied = capability_admission.admit_capabilities(manifest)
        with self.assertRaises(module.RuntimeAdmissionError):
            module.decide_runtime_admission(
                valid_request(capability_admission=default_denied)
            )

        for tampered in (
            dataclasses.replace(valid_capability_admission(), admitted=False),
            dataclasses.replace(valid_capability_admission(), requested_capabilities=(
                'local_inference', 'client_service',
            )),
            dataclasses.replace(valid_capability_admission(), approved_capabilities=(
                'local_inference', 'client_service',
            )),
            dataclasses.replace(valid_capability_admission(), capabilities=(
                'local_inference', 'client_service',
            )),
        ):
            with self.subTest(tampered=tampered):
                with self.assertRaises(module.RuntimeAdmissionError):
                    module.decide_runtime_admission(
                        valid_request(capability_admission=tampered)
                    )

    def test_capability_admission_rejects_identity_drift_tampering_and_runtime_claims(self):
        module = self.require_module()
        for tampered in (
            dataclasses.replace(valid_capability_admission(), client_id='other-client'),
            dataclasses.replace(valid_capability_admission(), addon_id='other-addon'),
            dataclasses.replace(valid_capability_admission(), execution_enabled=True),
            dataclasses.replace(valid_capability_admission(), effect='runtime_active'),
        ):
            with self.subTest(tampered=tampered):
                with self.assertRaises(module.RuntimeAdmissionError):
                    module.decide_runtime_admission(
                        valid_request(capability_admission=tampered)
                    )

        readiness_claim = valid_capability_admission()
        object.__setattr__(readiness_claim, 'readiness_claim', True)
        with self.assertRaises(module.RuntimeAdmissionError):
            module.decide_runtime_admission(
                valid_request(capability_admission=readiness_claim)
            )

    def test_capability_admission_preserves_fail_closed_metadata_only_flags(self):
        module = self.require_module()
        result = module.decide_runtime_admission(valid_request())

        self.assertEqual(result['admission'], 'approved_for_future_review')
        for field in (
            'execution_enabled', 'runtime_activation_allowed',
            'hardware_permission_activated', 'microphone_access', 'camera_access',
            'process_started', 'socket_bound', 'model_loaded', 'resource_enforced',
            'ipc_enforced', 'rollback_executed', 'state_changed',
            'lifecycle_state_changed',
        ):
            self.assertFalse(result[field], field)

    def test_raw_or_arbitrary_approval_mappings_are_rejected(self):
        module = self.require_module()

        for record in (
            valid_approval_record().as_dict(),
            valid_approval_record().as_runtime_admission_input(),
            {'record_type': 'stt_future_activation_approval'},
            {'approval_record_valid': True},
        ):
            with self.subTest(record=record):
                with self.assertRaises(module.RuntimeAdmissionError):
                    module.decide_runtime_admission(valid_request(approval_record=record))

    def test_rejected_tampered_and_identity_drift_approval_remain_denied(self):
        module = self.require_module()

        rejected_gates = [
            {'name': gate.name, 'passed': gate.passed, 'reason': gate.reason}
            for gate in valid_approval_record().gates
        ]
        rejected_gates[0]['passed'] = False
        rejected = valid_approval_record(gates=rejected_gates)
        result = module.decide_runtime_admission(valid_request(approval_record=rejected))
        self.assertEqual(result['admission'], 'denied')
        self.assertFalse(result['runtime_activation_allowed'])
        self.assertFalse(result['execution_enabled'])

        tampered = dataclasses.replace(valid_approval_record(), execution_enabled=True)
        with self.assertRaises(module.RuntimeAdmissionError):
            module.decide_runtime_admission(valid_request(approval_record=tampered))

        drifted = dataclasses.replace(valid_approval_record(), activation_id='other-activation')
        with self.assertRaises(module.RuntimeAdmissionError):
            module.decide_runtime_admission(valid_request(approval_record=drifted))

    def test_approval_binds_full_identity_tuple_and_exact_gate_inventory(self):
        module = self.require_module()
        result = module.decide_runtime_admission(valid_request())

        self.assertEqual(
            tuple(result['identity_tuple']),
            (SERVICE_ID, SERVICE_VERSION, ACTIVATION_ID, HOST_ID, MODEL_ID, MODEL_SHA256),
        )
        self.assertEqual(
            tuple(result['approval_gates']), approval.REQUIRED_STT_APPROVAL_GATES
        )

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

    def test_resource_policy_accepts_only_validated_immutable_identity_bound_result(self):
        module = self.require_module()

        result = module.decide_runtime_admission(valid_request())

        self.assertEqual(result['admission'], 'approved_for_future_review')
        self.assertEqual(result['service_id'], SERVICE_ID)
        self.assertEqual(result['activation_id'], ACTIVATION_ID)
        for field in (
            'execution_enabled', 'runtime_activation_allowed', 'process_started',
            'socket_bound', 'model_loaded', 'resource_enforced',
            'lifecycle_state_changed',
        ):
            self.assertFalse(result[field], field)

        for value in (
            valid_resource_policy().as_dict(),
            {'service_id': SERVICE_ID},
        ):
            with self.subTest(value=value):
                with self.assertRaises(module.RuntimeAdmissionError):
                    module.decide_runtime_admission(
                        valid_request(resource_policy_evidence=value)
                    )

        for tampered in (
            dataclasses.replace(valid_resource_policy(), policy_valid=False),
            dataclasses.replace(valid_resource_policy(), resource_enforced=True),
            dataclasses.replace(valid_resource_policy(), execution_enabled=True),
            dataclasses.replace(valid_resource_policy(), runtime_activation_allowed=True),
        ):
            with self.subTest(tampered=tampered):
                with self.assertRaises(module.RuntimeAdmissionError):
                    module.decide_runtime_admission(
                        valid_request(resource_policy_evidence=tampered)
                    )

    def test_resource_policy_service_and_activation_must_match_complete_identity_tuple(self):
        module = self.require_module()
        for overrides in (
            {'service_id': 'other-service'},
            {'activation_id': 'other-activation'},
        ):
            with self.subTest(overrides=overrides):
                policy = valid_resource_policy(**overrides)
                with self.assertRaises(module.RuntimeAdmissionError):
                    module.decide_runtime_admission(
                        valid_request(resource_policy_evidence=policy)
                    )

    def test_resource_enforcement_evidence_is_composed_as_exact_inert_identity_bound_type(self):
        module = self.require_module()
        result = module.decide_runtime_admission(valid_request())

        self.assertEqual(result['admission'], 'approved_for_future_review')
        self.assertTrue(result['evidence_composed']['resource_enforcement'])
        self.assertEqual(
            tuple(result['resource_enforcement_identity_tuple']),
            (SERVICE_ID, ACTIVATION_ID, 'resource-policy-v1'),
        )
        self.assertEqual(result['resource_enforcement_policy_version'], 'resource-policy-v1')
        for field in (
            'execution_enabled', 'runtime_activation_allowed', 'process_started',
            'model_loaded', 'service_started', 'resource_enforced', 'cgroup_bound',
            'ipc_enforced', 'rollback_executed', 'lifecycle_state_changed',
        ):
            self.assertFalse(result[field], field)
        self.assertNotIn('runtime_ready', result)
        self.assertNotIn('readiness_claim', result)

    def test_resource_enforcement_requires_exact_validated_type_and_no_runtime_claims(self):
        module = self.require_module()
        valid = valid_resource_enforcement_evidence()
        for value in (valid.as_dict(), {'service_id': SERVICE_ID}, None):
            with self.subTest(value=value):
                with self.assertRaises(module.RuntimeAdmissionError):
                    module.decide_runtime_admission(
                        valid_request(resource_enforcement_evidence=value)
                    )

        class DerivedResourceEnforcementEvidence(resource_enforcement_evidence.STTResourceEnforcementEvidence):
            pass

        with self.assertRaises(module.RuntimeAdmissionError):
            module.decide_runtime_admission(
                valid_request(
                    resource_enforcement_evidence=DerivedResourceEnforcementEvidence(
                        **dataclasses.asdict(valid)
                    )
                )
            )

        for tampered in (
            dataclasses.replace(valid, service_id='other-service'),
            dataclasses.replace(valid, activation_id='other-activation'),
            dataclasses.replace(valid, policy_version='other-policy-v2'),
            dataclasses.replace(valid, cpu_enforced=False),
            dataclasses.replace(valid, policy_valid=False),
            dataclasses.replace(valid, enforcement_valid=False),
            dataclasses.replace(valid, resource_enforced=True),
            dataclasses.replace(valid, cgroup_bound=True),
            dataclasses.replace(valid, execution_enabled=True),
        ):
            with self.subTest(tampered=tampered):
                with self.assertRaises(module.RuntimeAdmissionError):
                    module.decide_runtime_admission(
                        valid_request(resource_enforcement_evidence=tampered)
                    )

    def test_resource_enforcement_policy_version_must_match_resource_policy(self):
        module = self.require_module()
        drifted_policy = dataclasses.replace(
            valid_resource_policy(), policy_version='other-policy-v2'
        )
        with self.assertRaises(module.RuntimeAdmissionError):
            module.decide_runtime_admission(
                valid_request(resource_policy_evidence=drifted_policy)
            )

        drifted_enforcement = dataclasses.replace(
            valid_resource_enforcement_evidence(), policy_version='other-policy-v2'
        )
        with self.assertRaises(module.RuntimeAdmissionError):
            module.decide_runtime_admission(
                valid_request(resource_enforcement_evidence=drifted_enforcement)
            )

    def test_resource_enforcement_raw_claims_are_not_request_fields(self):
        module = self.require_module()
        for extra in (
            {'readiness_claim': True},
            {'resource_enforced': True},
            {'cgroup_bound': True},
            {'process_started': True},
            {'container_id': 'abc'},
            {'model_bytes': 'AA=='},
        ):
            with self.subTest(extra=extra):
                request = valid_request()
                request['resource_enforcement_evidence'] = {
                    **valid_resource_enforcement_evidence().as_dict(),
                    **extra,
                }
                with self.assertRaises(module.RuntimeAdmissionError):
                    module.decide_runtime_admission(request)

    def test_rollback_policy_is_composed_as_validated_inert_identity_bound_evidence(self):
        module = self.require_module()

        result = module.decide_runtime_admission(valid_request())

        self.assertEqual(result['admission'], 'approved_for_future_review')
        self.assertTrue(result['evidence_composed']['rollback_policy'])
        self.assertEqual(
            tuple(result['rollback_identity_tuple']),
            (SERVICE_ID, SERVICE_VERSION, ACTIVATION_ID),
        )
        for field in (
            'execution_enabled', 'runtime_activation_allowed', 'process_started',
            'socket_bound', 'model_loaded', 'resource_enforced', 'ipc_enforced',
            'rollback_executed', 'state_changed', 'lifecycle_state_changed',
        ):
            self.assertFalse(result[field], field)

    def test_rollback_policy_requires_exact_validated_type_and_full_identity(self):
        module = self.require_module()

        for value in (
            valid_rollback_policy().as_dict(),
            {'service_id': SERVICE_ID},
            None,
        ):
            with self.subTest(value=value):
                with self.assertRaises(module.RuntimeAdmissionError):
                    module.decide_runtime_admission(
                        valid_request(rollback_policy_evidence=value)
                    )

        for tampered in (
            valid_rollback_policy(cleanup_supported=False),
            dataclasses.replace(valid_rollback_policy(), policy_valid=False),
            dataclasses.replace(valid_rollback_policy(), execution_enabled=True),
            dataclasses.replace(valid_rollback_policy(), runtime_activation_allowed=True),
            dataclasses.replace(valid_rollback_policy(), rollback_executed=True),
            dataclasses.replace(valid_rollback_policy(), state_changed=True),
            dataclasses.replace(valid_rollback_policy(), service_id='other-service'),
            dataclasses.replace(valid_rollback_policy(), activation_id='other-activation'),
            dataclasses.replace(valid_rollback_policy(), service_version='9.9.9'),
        ):
            with self.subTest(tampered=tampered):
                with self.assertRaises(module.RuntimeAdmissionError):
                    module.decide_runtime_admission(
                        valid_request(rollback_policy_evidence=tampered)
                    )

    def test_ipc_policy_is_composed_only_from_validated_immutable_metadata(self):
        module = self.require_module()
        result = module.decide_runtime_admission(valid_request())

        self.assertEqual(result['admission'], 'approved_for_future_review')
        self.assertEqual(
            tuple(result['ipc_identity_tuple']),
            (SERVICE_ID, ACTIVATION_ID, SOCKET_NAME, 1, ('health', 'transcribe')),
        )
        self.assertFalse(result['ipc_enforced'])
        self.assertFalse(result['socket_bound'])

        for value in (
            valid_ipc_policy().as_dict(),
            {'service_id': SERVICE_ID},
        ):
            with self.subTest(value=value):
                with self.assertRaises(module.RuntimeAdmissionError):
                    module.decide_runtime_admission(
                        valid_request(ipc_policy_evidence=value)
                    )

        for tampered in (
            dataclasses.replace(valid_ipc_policy(), policy_valid=False),
            dataclasses.replace(valid_ipc_policy(), execution_enabled=True),
            dataclasses.replace(valid_ipc_policy(), socket_bound=True),
            dataclasses.replace(valid_ipc_policy(), ipc_enforced=True),
        ):
            with self.subTest(tampered=tampered):
                with self.assertRaises(module.RuntimeAdmissionError):
                    module.decide_runtime_admission(
                        valid_request(ipc_policy_evidence=tampered)
                    )

    def test_ipc_policy_binds_service_activation_socket_protocol_and_operations(self):
        module = self.require_module()
        for tampered in (
            dataclasses.replace(valid_ipc_policy(), service_id='other-service'),
            dataclasses.replace(valid_ipc_policy(), activation_id='other-activation'),
            dataclasses.replace(valid_ipc_policy(), socket_name='other-socket'),
            dataclasses.replace(valid_ipc_policy(), protocol_version=0),
            dataclasses.replace(valid_ipc_policy(), allowed_operations=('health', 'shell')),
        ):
            with self.subTest(tampered=tampered):
                with self.assertRaises(module.RuntimeAdmissionError):
                    module.decide_runtime_admission(
                        valid_request(ipc_policy_evidence=tampered)
                    )

        supervisor = valid_supervisor(socket_name='other-socket')
        with self.assertRaises(module.RuntimeAdmissionError):
            module.decide_runtime_admission(
                valid_request(supervisor_plan=supervisor)
            )

    def test_ipc_readiness_is_composed_as_exact_inert_evidence_bound_to_policy(self):
        module = self.require_module()
        result = module.decide_runtime_admission(valid_request())

        self.assertEqual(result['admission'], 'approved_for_future_review')
        self.assertTrue(result['evidence_composed']['ipc_readiness'])
        self.assertTrue(result['ipc_readiness_valid'])
        self.assertEqual(
            tuple(result['ipc_readiness_identity_tuple']),
            (SERVICE_ID, ACTIVATION_ID, SOCKET_NAME, 1, ('health', 'transcribe')),
        )
        for field in (
            'execution_enabled', 'runtime_activation_allowed', 'socket_bound',
            'ipc_enforced', 'listener_started', 'peer_authenticated',
        ):
            self.assertFalse(result[field], field)
        for forbidden in ('transport_ready', 'listener_ready', 'runtime_ready'):
            self.assertNotIn(forbidden, result)

    def test_ipc_readiness_rejects_raw_tampered_and_mismatched_evidence(self):
        module = self.require_module()
        valid = valid_ipc_readiness_evidence()
        for value in (valid.as_dict(), {'service_id': SERVICE_ID}, None):
            with self.subTest(value=value):
                with self.assertRaises(module.RuntimeAdmissionError):
                    module.decide_runtime_admission(
                        valid_request(ipc_readiness_evidence=value)
                    )

        class DerivedReadinessEvidence(ipc_readiness_evidence.STTIPCReadinessEvidence):
            pass

        with self.assertRaises(module.RuntimeAdmissionError):
            module.decide_runtime_admission(
                valid_request(
                    ipc_readiness_evidence=DerivedReadinessEvidence(
                        **dataclasses.asdict(valid)
                    )
                )
            )

        for tampered in (
            dataclasses.replace(valid, service_id='other-service'),
            dataclasses.replace(valid, activation_id='other-activation'),
            dataclasses.replace(valid, socket_name='other-socket'),
            dataclasses.replace(valid, protocol_version=2),
            dataclasses.replace(valid, allowed_operations=('health',)),
            dataclasses.replace(valid, readiness_valid=False),
            dataclasses.replace(valid, execution_enabled=True),
            dataclasses.replace(valid, socket_bound=True),
            dataclasses.replace(valid, listener_started=True),
            dataclasses.replace(valid, peer_authenticated=True),
        ):
            with self.subTest(tampered=tampered):
                with self.assertRaises(module.RuntimeAdmissionError):
                    module.decide_runtime_admission(
                        valid_request(ipc_readiness_evidence=tampered)
                    )

    def test_preflight_camera_evidence_is_optional_for_stt_admission(self):
        module = self.require_module()
        preflight = valid_preflight()
        del preflight['evidence']['camera_present']
        preflight['warnings'] = ['camera_evidence_unavailable']

        result = module.decide_runtime_admission(
            valid_request(preflight_evidence=preflight)
        )

        self.assertEqual(result['admission'], 'approved_for_future_review')
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

        bad_approval = valid_approval_record().as_dict()
        bad_approval['path'] = '/tmp/approval'
        with self.assertRaises(module.RuntimeAdmissionError):
            module.decide_runtime_admission(
                valid_request(approval_record=bad_approval)
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
