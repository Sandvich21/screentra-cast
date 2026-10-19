#!/usr/bin/env python3
import importlib.util
import json
import sys
import unittest
from collections.abc import Mapping
from pathlib import Path
from types import MappingProxyType

ROOT = Path(__file__).resolve().parents[1]
MODULE_PATH = ROOT / 'player-client/scripts/screentra-client-service-status-request-admission.py'


def load_module(path: Path, name: str):
    spec = importlib.util.spec_from_file_location(name, path)
    if spec is None or spec.loader is None:
        raise AssertionError(f'cannot load {path}')
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


admission = load_module(
    MODULE_PATH,
    'screentra_client_service_status_request_admission',
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
    payload = {'service_id': 'speech-to-text'}
    payload.update(overrides)
    return payload


class ClientServiceStatusRequestAdmissionTests(unittest.TestCase):
    def test_valid_request_is_exact_immutable_closed_mapping(self):
        result = admission.validate_status_request(valid_payload())

        self.assertIsInstance(result, MappingProxyType)
        self.assertEqual(tuple(result), ('service_id',))
        self.assertEqual(dict(result), valid_payload())
        with self.assertRaises(TypeError):
            result['service_id'] = 'tampered'

    def test_rejects_raw_derived_tampered_and_duplicate_payloads(self):
        validated = admission.validate_status_request(valid_payload())
        for candidate in (
            valid_payload(),
            dict(validated),
            MappingProxyType(valid_payload(service_id='../tampered')),
        ):
            with self.subTest(candidate=candidate), self.assertRaises(admission.StatusRequestError):
                admission.build_status_request_evidence(candidate)

        with self.assertRaises(admission.StatusRequestError):
            admission.validate_status_request(
                DuplicateKeyMapping([
                    ('service_id', 'speech-to-text'),
                    ('service_id', 'tampered'),
                ])
            )

    def test_rejects_unsafe_fields_values_and_controls(self):
        invalid_payloads = (
            {},
            valid_payload(extra='not allowed'),
            valid_payload(health=True),
            valid_payload(health_requested=True),
            valid_payload(process='service'),
            valid_payload(pid=42),
            valid_payload(port=9000),
            valid_payload(socket='/tmp/service.sock'),
            valid_payload(command=['systemctl', 'status']),
            valid_payload(credentials='secret'),
            valid_payload(audio_path='/tmp/audio.wav'),
            valid_payload(model_path='/models/model.bin'),
            valid_payload(service_id='../speech-to-text'),
            valid_payload(service_id='speech/to-text'),
            valid_payload(service_id='https://example.invalid'),
            valid_payload(service_id='speech-to-text:9000'),
            valid_payload(service_id='speech-to-text;systemctl'),
            valid_payload(service_id='password-token'),
            valid_payload(service_id='credential-service'),
            valid_payload(service_id='speech to text'),
            valid_payload(service_id=''),
            valid_payload(service_id=42),
        )
        for payload in invalid_payloads:
            with self.subTest(payload=payload), self.assertRaises(admission.StatusRequestError):
                admission.validate_status_request(payload)

        with self.assertRaises(admission.StatusRequestError):
            admission.validate_status_request(
                valid_payload(service_id='x' * (admission.MAX_SERVICE_ID_BYTES + 1))
            )

        oversized = valid_payload(service_id='s' * admission.MAX_SERVICE_ID_BYTES)
        with self.assertRaises(admission.StatusRequestError):
            admission.validate_status_request(oversized)

    def test_evidence_is_metadata_only_exact_and_bounded(self):
        request = admission.validate_status_request(valid_payload())
        evidence = admission.build_status_request_evidence(request)

        self.assertIsInstance(evidence, MappingProxyType)
        self.assertEqual(
            set(evidence),
            {
                'service_id', 'request_valid', 'execution_enabled',
                'request_queued', 'health_requested', 'process_started',
                'service_healthy',
            },
        )
        self.assertTrue(evidence['request_valid'])
        for field in (
            'execution_enabled', 'request_queued', 'health_requested',
            'process_started', 'service_healthy',
        ):
            self.assertFalse(evidence[field], field)
        with self.assertRaises(TypeError):
            evidence['service_healthy'] = True
        encoded = json.dumps(dict(evidence), separators=(',', ':')).encode('utf-8')
        self.assertLessEqual(len(encoded), admission.MAX_EVIDENCE_BYTES)

    def test_contract_source_has_no_side_effect_primitives(self):
        source = MODULE_PATH.read_text(encoding='utf-8')
        for forbidden in (
            'socket', 'subprocess', 'Popen', 'run(', 'open(', 'Path(',
            'systemctl', 'service_healthy = True', 'process_started = True',
            'http://', 'https://',
        ):
            self.assertNotIn(forbidden, source)


if __name__ == '__main__':
    unittest.main()
