#!/usr/bin/env python3
import importlib.util
import json
import sys
import unittest
from collections.abc import Mapping
from pathlib import Path
from types import MappingProxyType

ROOT = Path(__file__).resolve().parents[1]
MODULE_PATH = ROOT / 'player-client/scripts/screentra-client-service-rollback-request-admission.py'


def load_module(path: Path, name: str):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


admission = load_module(
    MODULE_PATH,
    'screentra_client_service_rollback_request_admission',
)


class DuplicateKeyMapping(Mapping):
    def __init__(self, pairs):
        self._pairs = pairs

    def __getitem__(self, key):
        for candidate, value in reversed(self._pairs):
            if candidate == key:
                return value
        raise KeyError(key)

    def __iter__(self):
        return (key for key, _ in self._pairs)

    def __len__(self):
        return len(self._pairs)


def valid_payload(**overrides):
    payload = {
        'service_id': 'speech-to-text',
        'activation_id': 'activation-001',
    }
    payload.update(overrides)
    return payload


class ClientServiceRollbackRequestAdmissionTests(unittest.TestCase):
    def test_valid_request_is_exact_immutable_closed_mapping(self):
        result = admission.validate_rollback_request(valid_payload())

        self.assertIsInstance(result, MappingProxyType)
        self.assertEqual(tuple(result), ('service_id', 'activation_id'))
        self.assertEqual(set(result), {'service_id', 'activation_id'})
        self.assertEqual(dict(result), valid_payload())
        with self.assertRaises(TypeError):
            result['activation_id'] = 'tampered'

    def test_rejects_raw_derived_tampered_and_mismatched_payloads(self):
        validated = admission.validate_rollback_request(valid_payload())
        candidates = (
            valid_payload(),
            dict(validated),
            {**valid_payload(), 'activation_id': 'activation-002'},
            MappingProxyType(valid_payload(activation_id='../tampered')),
        )
        for candidate in candidates:
            with self.subTest(candidate=candidate), self.assertRaises(admission.RollbackRequestError):
                admission.build_rollback_request_evidence(candidate)

        with self.assertRaises(admission.RollbackRequestError):
            admission.validate_rollback_request(
                DuplicateKeyMapping([
                    ('service_id', 'speech-to-text'),
                    ('activation_id', 'activation-001'),
                    ('activation_id', 'tampered'),
                ])
            )

    def test_rejects_unsafe_fields_values_and_controls(self):
        invalid_payloads = (
            {'service_id': 'speech-to-text'},
            {'activation_id': 'activation-001'},
            valid_payload(extra='not allowed'),
            valid_payload(audio_bytes=b'raw audio'),
            valid_payload(audio_path='/tmp/audio.wav'),
            valid_payload(model='speech-model'),
            valid_payload(runtime='python'),
            valid_payload(port=9000),
            valid_payload(pid=42),
            valid_payload(command=['systemctl', 'stop']),
            valid_payload(process='service'),
            valid_payload(credentials='secret'),
            valid_payload(service_id='../speech-to-text'),
            valid_payload(service_id='speech/to-text'),
            valid_payload(service_id='https://example.invalid'),
            valid_payload(service_id='speech-to-text:9000'),
            valid_payload(service_id='speech-to-text;systemctl'),
            valid_payload(service_id='password-token'),
            valid_payload(activation_id='credential-001'),
            valid_payload(activation_id='activation with spaces'),
        )
        for payload in invalid_payloads:
            with self.subTest(payload=payload), self.assertRaises(admission.RollbackRequestError):
                admission.validate_rollback_request(payload)

        for field, maximum in (
            ('service_id', admission.MAX_SERVICE_ID_BYTES),
            ('activation_id', admission.MAX_ACTIVATION_ID_BYTES),
        ):
            with self.subTest(field=field), self.assertRaises(admission.RollbackRequestError):
                admission.validate_rollback_request(valid_payload(**{field: 'x' * (maximum + 1)}))

        oversized = valid_payload(
            service_id='s' * admission.MAX_SERVICE_ID_BYTES,
            activation_id='a' * admission.MAX_ACTIVATION_ID_BYTES,
        )
        with self.assertRaises(admission.RollbackRequestError):
            admission.validate_rollback_request(oversized)

    def test_evidence_is_metadata_only_exact_and_bounded(self):
        request = admission.validate_rollback_request(valid_payload())
        evidence = admission.build_rollback_request_evidence(request)

        self.assertIsInstance(evidence, MappingProxyType)
        self.assertEqual(
            set(evidence),
            {
                'service_id', 'activation_id', 'request_valid',
                'execution_enabled', 'request_queued',
                'rollback_executed', 'state_changed', 'status',
            },
        )
        self.assertEqual(evidence['status'], 'disabled')
        self.assertTrue(evidence['request_valid'])
        for field in ('execution_enabled', 'request_queued', 'rollback_executed', 'state_changed'):
            self.assertFalse(evidence[field])
        with self.assertRaises(TypeError):
            evidence['status'] = 'executed'
        encoded = json.dumps(dict(evidence), separators=(',', ':')).encode('utf-8')
        self.assertLessEqual(len(encoded), admission.MAX_EVIDENCE_BYTES)

    def test_contract_source_has_no_side_effect_primitives(self):
        source = MODULE_PATH.read_text(encoding='utf-8')
        for forbidden in (
            'socket', 'subprocess', 'Popen', 'run(', 'open(', 'Path(',
            'systemctl', 'rollback_executed = True', 'state_changed = True',
            'http://', 'https://',
        ):
            self.assertNotIn(forbidden, source)


if __name__ == '__main__':
    unittest.main()
