import dataclasses
import importlib.util
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
MODULE_PATH = ROOT / 'player-client/scripts/screentra-stt-ipc-policy-evidence.py'


def load_module():
    if not MODULE_PATH.is_file():
        return None
    spec = importlib.util.spec_from_file_location(
        'screentra_stt_ipc_policy_evidence', MODULE_PATH
    )
    module = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


policy = load_module()

SERVICE_ID = 'magic-mirror-stt'
ACTIVATION_ID = 'activation-20261018-01'
SOCKET_NAME = 'magic-mirror-stt'


def valid_evidence(**overrides):
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
    return value


class STTIPCPolicyEvidenceTests(unittest.TestCase):
    def require_module(self):
        self.assertIsNotNone(
            policy,
            'production STT IPC policy-evidence module is not implemented',
        )
        return policy

    def assert_invalid(self, evidence, **expected):
        module = self.require_module()
        with self.assertRaises(module.STTIPCPolicyEvidenceError, **expected):
            module.validate_stt_ipc_policy_evidence(evidence)

    def test_valid_policy_is_frozen_identity_bound_metadata_only_evidence(self):
        module = self.require_module()

        result = module.validate_stt_ipc_policy_evidence(
            valid_evidence(),
            expected_service_id=SERVICE_ID,
            expected_activation_id=ACTIVATION_ID,
            expected_socket_name=SOCKET_NAME,
        )

        self.assertIsInstance(result, module.STTIPCPolicyEvidence)
        self.assertEqual(
            (result.service_id, result.activation_id, result.socket_name),
            (SERVICE_ID, ACTIVATION_ID, SOCKET_NAME),
        )
        self.assertEqual(result.protocol_version, module.PROTOCOL_VERSION)
        self.assertEqual(tuple(result.allowed_operations), ('health', 'transcribe'))
        self.assertTrue(result.policy_valid)
        for field in (
            'execution_enabled', 'runtime_activation_allowed',
            'socket_bound', 'ipc_enforced',
        ):
            self.assertFalse(getattr(result, field), field)
        with self.assertRaises(dataclasses.FrozenInstanceError):
            result.socket_name = 'other-socket'

        encoded = result.as_dict()
        self.assertEqual(encoded['plan_type'], 'stt_client_service_ipc_policy_evidence')
        self.assertEqual(encoded['effect'], 'metadata_only')
        self.assertEqual(tuple(encoded['allowed_operations']), ('health', 'transcribe'))
        self.assertTrue(encoded['policy_valid'])
        for field in (
            'execution_enabled', 'runtime_activation_allowed',
            'socket_bound', 'ipc_enforced',
        ):
            self.assertFalse(encoded[field], field)
        for forbidden in (
            'listener_started', 'listener_ready', 'process_started', 'process_running',
            'audio', 'raw_audio', 'network', 'network_used', 'url', 'host',
            'path', 'command', 'credentials', 'ports', 'listen',
        ):
            self.assertNotIn(forbidden, encoded)

    def test_operations_are_nonempty_unique_and_closed_to_health_and_transcribe(self):
        module = self.require_module()
        for operations in (['health'], ['transcribe'], ['transcribe', 'health']):
            with self.subTest(operations=operations):
                result = module.validate_stt_ipc_policy_evidence(
                    valid_evidence(allowed_operations=operations)
                )
                self.assertTrue(set(result.allowed_operations) <= {'health', 'transcribe'})

        for operations in ([], ['health', 'health'], ['transcribe', 'unknown'], ['shell']):
            with self.subTest(operations=operations):
                self.assert_invalid(valid_evidence(allowed_operations=operations))

        for operations in ('health', {'health'}, [1], None):
            with self.subTest(operations=operations):
                self.assert_invalid(valid_evidence(allowed_operations=operations))

    def test_codec_compatible_numeric_limits_and_relationships_are_enforced(self):
        module = self.require_module()
        self.assertEqual(module.MAX_FRAME_BYTES, 64 * 1024)
        self.assertEqual(module.MAX_FRAME_BYTES, 4 * 1024 * 16)
        result = module.validate_stt_ipc_policy_evidence(valid_evidence())
        self.assertLessEqual(result.max_request_bytes, result.max_frame_bytes)
        self.assertLessEqual(result.max_response_bytes, result.max_frame_bytes)

        for field in ('max_frame_bytes', 'max_request_bytes', 'max_response_bytes'):
            for value in (0, -1, module.MAX_FRAME_BYTES + 1, True, 1.5, '65536', None):
                with self.subTest(field=field, value=value):
                    self.assert_invalid(valid_evidence(**{field: value}))

        for change in (
            {'max_request_bytes': 64 * 1024 + 1},
            {'max_response_bytes': 64 * 1024 + 1},
            {'max_frame_bytes': 1024, 'max_request_bytes': 1025},
            {'max_frame_bytes': 1024, 'max_response_bytes': 1025},
        ):
            with self.subTest(change=change):
                self.assert_invalid(valid_evidence(**change))

    def test_exact_socket_and_identity_bindings_reject_drift_and_remote_shapes(self):
        module = self.require_module()
        for kwargs in (
            {'expected_service_id': 'other-service'},
            {'expected_activation_id': 'other-activation'},
            {'expected_socket_name': 'other-socket'},
        ):
            with self.subTest(kwargs=kwargs):
                module = self.require_module()
                with self.assertRaises(module.STTIPCPolicyEvidenceError):
                    module.validate_stt_ipc_policy_evidence(valid_evidence(), **kwargs)

        for socket_name in (
            '', 'other/socket', '/tmp/stt.sock', '../stt', 'https://remote.invalid',
            'tcp://remote.invalid:9000', '127.0.0.1:9000', 'remote.example',
            'stt.sock', 'x' * 65,
        ):
            with self.subTest(socket_name=socket_name):
                self.assert_invalid(valid_evidence(socket_name=socket_name))

        for field, value in (
            ('service_id', 'other/service'),
            ('activation_id', 'other/activation'),
            ('service_id', 'https://remote.invalid'),
            ('activation_id', '../activation'),
        ):
            with self.subTest(field=field, value=value):
                self.assert_invalid(valid_evidence(**{field: value}))

    def test_protocol_version_and_local_only_are_exact(self):
        module = self.require_module()
        self.assertEqual(
            module.validate_stt_ipc_policy_evidence(valid_evidence()).protocol_version,
            module.PROTOCOL_VERSION,
        )
        for version in (0, 2, True, 1.0, '1', None):
            with self.subTest(version=version):
                self.assert_invalid(valid_evidence(protocol_version=version))
        self.assert_invalid(valid_evidence(local_only=False))
        self.assert_invalid(valid_evidence(local_only=1))

    def test_closed_mapping_rejects_sensitive_runtime_shaped_fields(self):
        module = self.require_module()
        for extra in (
            {'unexpected': True},
            {'url': 'https://remote.invalid'},
            {'host': 'remote.invalid'},
            {'path': '/tmp/stt.sock'},
            {'command': ['/bin/sh']},
            {'credentials': 'redacted'},
            {'token': 'redacted'},
            {'raw_audio': 'base64'},
            {'audio': b'pcm'},
            {'port': 9000},
            {'ports': []},
            {'listener': True},
            {'bind': '0.0.0.0'},
        ):
            with self.subTest(extra=extra):
                self.assert_invalid(valid_evidence(**extra))

    def test_oversized_metadata_fails_closed_and_output_shape_stays_closed(self):
        module = self.require_module()
        oversized = valid_evidence()
        oversized['service_id'] = 'x' * module.MAX_SERVICE_ID_BYTES
        oversized['unexpected_padding'] = 'x' * module.MAX_EVIDENCE_BYTES
        self.assert_invalid(oversized)

        result = module.validate_stt_ipc_policy_evidence(valid_evidence())
        self.assertEqual(set(result.as_dict()), {
            'plan_type', 'effect', 'policy_valid', 'service_id', 'activation_id',
            'socket_name', 'protocol_version', 'max_frame_bytes',
            'max_request_bytes', 'max_response_bytes', 'allowed_operations',
            'local_only', 'execution_enabled', 'runtime_activation_allowed',
            'socket_bound', 'ipc_enforced',
        })
        self.assertTrue(result.as_dict()['local_only'])

    def test_module_is_pure_and_exposes_no_socket_or_runtime_operations(self):
        module = self.require_module()
        source = MODULE_PATH.read_text(encoding='utf-8')
        for forbidden in (
            'socket.socket', 'socket.bind', 'socket.listen', 'subprocess',
            'create_subprocess', 'urlopen', 'os.system', 'open(',
        ):
            self.assertNotIn(forbidden, source)
        result = module.validate_stt_ipc_policy_evidence(valid_evidence())
        self.assertFalse(result.execution_enabled)
        self.assertFalse(result.socket_bound)
        self.assertFalse(result.ipc_enforced)


if __name__ == '__main__':
    unittest.main()
