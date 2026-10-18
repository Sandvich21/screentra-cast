#!/usr/bin/env python3
import dataclasses
import importlib.util
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
MODULE_PATH = ROOT / 'player-client/scripts/screentra-stt-approval-record.py'
spec = importlib.util.spec_from_file_location('screentra_stt_approval_record', MODULE_PATH)
approval = importlib.util.module_from_spec(spec)
assert spec.loader is not None
sys.modules[spec.name] = approval
spec.loader.exec_module(approval)


SERVICE_ID = 'magic-mirror-stt'
SERVICE_VERSION = '0.1.0'
ACTIVATION_ID = 'activation-20261018-01'
HOST_ID = 'client-56'
MODEL_ID = 'whisper-tiny.en'
MODEL_SHA256 = 'a' * 64
APPROVED_AT = '2026-10-18T14:30:00Z'


def valid_gates():
    return [
        {'name': name, 'passed': True, 'reason': f'{name} evidence recorded'}
        for name in approval.REQUIRED_STT_APPROVAL_GATES
    ]


def valid_record(**overrides):
    value = {
        'record_type': 'stt_future_activation_approval',
        'service_id': SERVICE_ID,
        'service_version': SERVICE_VERSION,
        'activation_id': ACTIVATION_ID,
        'host_id': HOST_ID,
        'model_id': MODEL_ID,
        'model_sha256': MODEL_SHA256,
        'approver': 'admin-alice',
        'approved_at': APPROVED_AT,
        'gates': valid_gates(),
    }
    value.update(overrides)
    return value


class STTApprovalRecordTests(unittest.TestCase):
    def test_valid_record_returns_immutable_metadata_only_result(self):
        result = approval.validate_stt_approval_record(valid_record())

        self.assertIsInstance(result, approval.STTApprovalRecord)
        self.assertEqual(result.service_id, SERVICE_ID)
        self.assertEqual(result.service_version, SERVICE_VERSION)
        self.assertEqual(result.activation_id, ACTIVATION_ID)
        self.assertEqual(result.host_id, HOST_ID)
        self.assertEqual(result.model_id, MODEL_ID)
        self.assertEqual(result.model_sha256, MODEL_SHA256)
        self.assertEqual(result.approver, 'admin-alice')
        self.assertEqual(result.approved_at, APPROVED_AT)
        self.assertEqual(tuple(g.name for g in result.gates), approval.REQUIRED_STT_APPROVAL_GATES)
        self.assertTrue(all(g.passed for g in result.gates))
        self.assertFalse(result.execution_enabled)
        self.assertFalse(result.runtime_activation_allowed)
        self.assertEqual(result.effect, 'metadata_only')
        with self.assertRaises(dataclasses.FrozenInstanceError):
            result.service_id = 'other-service'
        with self.assertRaises(dataclasses.FrozenInstanceError):
            result.gates[0].name = 'other-gate'

    def test_result_serializes_only_bounded_non_readiness_metadata(self):
        result = approval.validate_stt_approval_record(valid_record())
        encoded = result.as_dict()

        self.assertFalse(encoded['execution_enabled'])
        self.assertFalse(encoded['runtime_activation_allowed'])
        self.assertEqual(encoded['effect'], 'metadata_only')
        self.assertEqual(encoded['gates'][0]['name'], 'capability_admission')
        for forbidden in (
            'process_started', 'model_loaded', 'audio_ready', 'socket_bound',
            'runtime_ready', 'hardware_ready', 'audio', 'path', 'url',
            'command', 'credentials', 'token',
        ):
            self.assertNotIn(forbidden, encoded)
        handoff = result.as_runtime_admission_input()
        self.assertEqual(handoff['approval_record']['activation_id'], ACTIVATION_ID)
        self.assertFalse(handoff['execution_enabled'])
        self.assertFalse(handoff['runtime_activation_allowed'])

    def test_required_gate_inventory_is_closed_and_exact(self):
        self.assertEqual(
            approval.REQUIRED_STT_APPROVAL_GATES,
            (
                'capability_admission', 'model_integrity', 'audio_permission',
                'local_runtime', 'resource_policy', 'ipc_policy',
                'rollback_policy',
            ),
        )
        result = approval.validate_stt_approval_record(valid_record())
        self.assertEqual(
            {gate.name for gate in result.gates},
            set(approval.REQUIRED_STT_APPROVAL_GATES),
        )

    def test_unknown_duplicate_and_missing_gates_are_rejected(self):
        cases = []
        unknown = valid_gates()
        unknown[0] = {'name': 'new_gate', 'passed': True, 'reason': 'evidence'}
        cases.append(unknown)
        duplicate = valid_gates()
        duplicate[1] = dict(duplicate[0])
        cases.append(duplicate)
        cases.append(valid_gates()[:-1])
        for gates in cases:
            with self.subTest(gates=gates):
                with self.assertRaises(approval.STTApprovalRecordError):
                    approval.validate_stt_approval_record(valid_record(gates=gates))

    def test_identity_fields_are_bound_into_the_immutable_result(self):
        result = approval.validate_stt_approval_record(valid_record())

        self.assertEqual(
            (result.service_id, result.service_version, result.activation_id, result.host_id),
            (SERVICE_ID, SERVICE_VERSION, ACTIVATION_ID, HOST_ID),
        )
        self.assertEqual((result.model_id, result.model_sha256), (MODEL_ID, MODEL_SHA256))
        self.assertEqual(result.as_dict()['activation_id'], ACTIVATION_ID)

    def test_gate_shape_requires_nonempty_bounded_reason_and_real_boolean(self):
        cases = []
        for reason in ('', 'x' * (approval.MAX_GATE_REASON_BYTES + 1), 'line\nfeed'):
            gates = valid_gates()
            gates[0]['reason'] = reason
            cases.append(gates)
        for passed in (1, 0, 'true', None):
            gates = valid_gates()
            gates[0]['passed'] = passed
            cases.append(gates)
        for gates in cases:
            with self.subTest(gates=gates):
                with self.assertRaises(approval.STTApprovalRecordError):
                    approval.validate_stt_approval_record(valid_record(gates=gates))

    def test_top_level_and_gate_arbitrary_fields_are_rejected(self):
        with self.assertRaises(approval.STTApprovalRecordError):
            approval.validate_stt_approval_record(valid_record(unexpected=True))
        gates = valid_gates()
        gates[0]['unexpected'] = True
        with self.assertRaises(approval.STTApprovalRecordError):
            approval.validate_stt_approval_record(valid_record(gates=gates))

    def test_secrets_credentials_paths_urls_commands_and_audio_are_rejected(self):
        for field, value in (
            ('approver', 'api-token-123'),
            ('approver', 'credential-admin'),
            ('approver', 'https://admin.example.invalid'),
            ('approver', '/etc/passwd'),
            ('approver', 'run; rm -rf /'),
        ):
            with self.subTest(field=field, value=value), self.assertRaises(approval.STTApprovalRecordError):
                approval.validate_stt_approval_record(valid_record(**{field: value}))

        for value in ('password leaked', 'https://example.invalid/evidence', '/tmp/model.bin', 'arecord --format raw'):
            gates = valid_gates()
            gates[0]['reason'] = value
            with self.subTest(value=value), self.assertRaises(approval.STTApprovalRecordError):
                approval.validate_stt_approval_record(valid_record(gates=gates))

        with self.assertRaises(approval.STTApprovalRecordError):
            approval.validate_stt_approval_record(valid_record(audio='raw audio'))

    def test_timestamp_must_be_bounded_rfc3339_with_timezone(self):
        for timestamp in (
            'expired',
            '2026-10-18 14:30:00Z',
            '2026-10-18T14:30:00',
            '2026-02-30T14:30:00Z',
            '2026-10-18T14:30:00+25:00',
            'x' * (approval.MAX_APPROVED_AT_BYTES + 1),
        ):
            with self.subTest(timestamp=timestamp), self.assertRaises(approval.STTApprovalRecordError):
                approval.validate_stt_approval_record(valid_record(approved_at=timestamp))

    def test_identifiers_model_digest_and_version_are_bounded(self):
        cases = (
            {'service_id': 'service/other'},
            {'service_version': 'latest'},
            {'activation_id': 'activation..other'},
            {'host_id': 'host secret'},
            {'model_id': 'model/path'},
            {'model_sha256': 'A' * 64},
            {'model_sha256': 'a' * 63},
            {'service_id': 'x' * (approval.MAX_IDENTIFIER_BYTES + 1)},
        )
        for overrides in cases:
            with self.subTest(overrides=overrides), self.assertRaises(approval.STTApprovalRecordError):
                approval.validate_stt_approval_record(valid_record(**overrides))

    def test_oversized_record_and_non_mapping_input_fail_closed(self):
        with self.assertRaises(approval.STTApprovalRecordError):
            approval.validate_stt_approval_record([])
        oversized = valid_record(approver='a' * approval.MAX_APPROVER_BYTES)
        oversized['padding'] = 'x' * approval.MAX_RECORD_BYTES
        with self.assertRaises(approval.STTApprovalRecordError):
            approval.validate_stt_approval_record(oversized)


if __name__ == '__main__':
    unittest.main()
