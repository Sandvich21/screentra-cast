import dataclasses
import importlib.util
import sys
import unittest
from collections.abc import Mapping
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
MODULE_PATH = ROOT / 'player-client/scripts/screentra-stt-ipc-readiness-evidence.py'


def load_readiness_module():
    if not MODULE_PATH.is_file():
        return None
    spec = importlib.util.spec_from_file_location(
        'screentra_stt_ipc_readiness_evidence', MODULE_PATH
    )
    module = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


readiness = load_readiness_module()

SERVICE_ID = 'magic-mirror-stt'
ACTIVATION_ID = 'activation-20261018-01'
SOCKET_NAME = 'magic-mirror-stt'


def valid_evidence(**overrides):
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
    return value


class DuplicateMapping(Mapping):
    def __init__(self, pairs):
        self._pairs = list(pairs)

    def __getitem__(self, key):
        for candidate, value in self._pairs:
            if candidate == key:
                return value
        raise KeyError(key)

    def __iter__(self):
        return (key for key, _ in self._pairs)

    def __len__(self):
        return len(self._pairs)

    def keys(self):
        return [key for key, _ in self._pairs]


class STTIPCReadinessEvidenceTests(unittest.TestCase):
    def require_module(self):
        self.assertIsNotNone(
            readiness,
            'production STT IPC readiness-evidence module is not implemented',
        )
        return readiness

    def assert_invalid(self, evidence, **expected):
        module = self.require_module()
        with self.assertRaises(module.STTIPCReadinessEvidenceError, **expected):
            module.validate_stt_ipc_readiness_evidence(evidence)

    def test_valid_readiness_is_frozen_metadata_only_evidence(self):
        module = self.require_module()
        result = module.validate_stt_ipc_readiness_evidence(
            valid_evidence(),
            expected_service_id=SERVICE_ID,
            expected_activation_id=ACTIVATION_ID,
            expected_socket_name=SOCKET_NAME,
            expected_protocol_version=1,
            expected_allowed_operations=('health', 'transcribe'),
        )

        self.assertIsInstance(result, module.STTIPCReadinessEvidence)
        self.assertEqual(
            (result.service_id, result.activation_id, result.socket_name),
            (SERVICE_ID, ACTIVATION_ID, SOCKET_NAME),
        )
        self.assertEqual(result.protocol_version, 1)
        self.assertEqual(tuple(result.allowed_operations), ('health', 'transcribe'))
        self.assertTrue(result.readiness_valid)
        for field in (
            'execution_enabled', 'runtime_activation_allowed', 'socket_bound',
            'ipc_enforced', 'listener_started', 'peer_authenticated',
        ):
            self.assertFalse(getattr(result, field), field)
        with self.assertRaises(dataclasses.FrozenInstanceError):
            result.socket_name = 'other-socket'

        encoded = result.as_dict()
        self.assertEqual(encoded['plan_type'], 'stt_client_service_ipc_readiness_evidence')
        self.assertEqual(encoded['effect'], 'metadata_only')
        self.assertTrue(encoded['readiness_valid'])
        self.assertTrue(encoded['local_only'])
        for field in (
            'execution_enabled', 'runtime_activation_allowed', 'socket_bound',
            'ipc_enforced', 'listener_started', 'peer_authenticated',
        ):
            self.assertFalse(encoded[field], field)
        for forbidden in (
            'transport_ready', 'listener_ready', 'runtime_ready', 'process_started',
            'audio', 'raw_audio', 'network', 'url', 'host', 'path', 'command',
            'credentials', 'ports', 'listen', 'peer_uid', 'peer_gid', 'peer_pid',
            'socket_path',
        ):
            self.assertNotIn(forbidden, encoded)

    def test_readiness_valid_requires_all_five_observations_but_accepts_false_observations(self):
        module = self.require_module()
        for field in (
            'listener_observed', 'peer_identity_observed', 'hello_observed',
            'transport_non_mutating',
        ):
            result = module.validate_stt_ipc_readiness_evidence(
                valid_evidence(**{field: False})
            )
            self.assertFalse(result.readiness_valid, field)
        self.assert_invalid(valid_evidence(local_only=False))
        self.assert_invalid(valid_evidence(local_only=1))

        for field in (
            'listener_observed', 'peer_identity_observed', 'hello_observed',
            'transport_non_mutating', 'local_only',
        ):
            with self.subTest(field=field):
                self.assert_invalid(valid_evidence(**{field: 1}))
                self.assert_invalid(valid_evidence(**{field: 'true'}))

    def test_identity_protocol_and_operations_match_existing_ipc_policy(self):
        module = self.require_module()
        for kwargs in (
            {'expected_service_id': 'other-service'},
            {'expected_activation_id': 'other-activation'},
            {'expected_socket_name': 'other-socket'},
            {'expected_protocol_version': 2},
            {'expected_allowed_operations': ('health',)},
        ):
            with self.subTest(kwargs=kwargs):
                with self.assertRaises(module.STTIPCReadinessEvidenceError):
                    module.validate_stt_ipc_readiness_evidence(valid_evidence(), **kwargs)

        for operations in ([], ['health', 'health'], ['shell'], ['health', 'shell']):
            with self.subTest(operations=operations):
                self.assert_invalid(valid_evidence(allowed_operations=operations))
        for operations in ('health', {'health'}, [1], None):
            with self.subTest(operations=operations):
                self.assert_invalid(valid_evidence(allowed_operations=operations))

    def test_closed_mapping_rejects_duplicate_and_runtime_shaped_fields(self):
        module = self.require_module()
        pairs = list(valid_evidence().items())
        self.assert_invalid(DuplicateMapping(pairs + [('service_id', SERVICE_ID)]))
        for extra in (
            {'unexpected': True},
            {'path': '/tmp/stt.sock'},
            {'socket_path': '/tmp/stt.sock'},
            {'url': 'https://remote.invalid'},
            {'command': ['/bin/sh']},
            {'credentials': 'redacted'},
            {'peer_uid': 1000},
            {'peer_gid': 1000},
            {'peer_pid': 1234},
            {'raw_audio': 'base64'},
            {'audio': b'pcm'},
            {'port': 9000},
            {'ports': []},
            {'listener': True},
            {'bind': '0.0.0.0'},
        ):
            with self.subTest(extra=extra):
                self.assert_invalid({**valid_evidence(), **extra})

    def test_identifiers_reject_paths_urls_commands_credentials_and_oversized_records(self):
        module = self.require_module()
        for field in ('service_id', 'activation_id', 'socket_name'):
            for value in (
                '', '../escape', '/tmp/stt.sock', 'https://remote.invalid',
                'tcp://remote.invalid:9000', 'x' * 129,
            ):
                with self.subTest(field=field, value=value):
                    self.assert_invalid(valid_evidence(**{field: value}))
        oversized = valid_evidence()
        oversized['unexpected_padding'] = 'x' * module.MAX_EVIDENCE_BYTES
        self.assert_invalid(oversized)

    def test_module_is_pure_and_output_shape_has_no_activation_controls(self):
        module = self.require_module()
        source = MODULE_PATH.read_text(encoding='utf-8')
        for forbidden in (
            'socket.socket', 'socket.bind', 'socket.listen', 'subprocess',
            'create_subprocess', 'urlopen', 'os.system', 'open(',
        ):
            self.assertNotIn(forbidden, source)
        result = module.validate_stt_ipc_readiness_evidence(valid_evidence())
        self.assertEqual(set(result.as_dict()), {
            'plan_type', 'effect', 'service_id', 'activation_id', 'socket_name',
            'protocol_version', 'allowed_operations', 'listener_observed',
            'peer_identity_observed', 'hello_observed', 'transport_non_mutating',
            'local_only', 'readiness_valid', 'execution_enabled',
            'runtime_activation_allowed', 'socket_bound', 'ipc_enforced',
            'listener_started', 'peer_authenticated',
        })
        self.assertFalse(result.execution_enabled)
        self.assertFalse(result.socket_bound)
        self.assertFalse(result.ipc_enforced)



if __name__ == '__main__':
    unittest.main()
