#!/usr/bin/env python3
import importlib.util
import json
import sys
import unittest
from collections.abc import Mapping
from pathlib import Path
from types import MappingProxyType

ROOT = Path(__file__).resolve().parents[1]


def load_module(path: Path, name: str):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


admission = load_module(
    ROOT / 'player-client/scripts/screentra-stt-transcription-request-admission.py',
    'screentra_stt_transcription_request_admission',
)
actor = load_module(
    ROOT / 'player-client/scripts/screentra-player-actor.py',
    'screentra_player_actor_transcription_admission',
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
        'request_id': 'request-001',
        'push_to_talk': True,
    }
    payload.update(overrides)
    return payload


class STTTranscriptionRequestAdmissionTests(unittest.TestCase):
    def test_valid_request_is_an_exact_immutable_closed_mapping(self):
        result = admission.validate_transcription_request(valid_payload())

        self.assertIsInstance(result, MappingProxyType)
        self.assertEqual(tuple(result), ('service_id', 'request_id', 'push_to_talk'))
        self.assertEqual(set(result), {'service_id', 'request_id', 'push_to_talk'})
        self.assertEqual(dict(result), valid_payload())
        with self.assertRaises(TypeError):
            result['request_id'] = 'tampered'

    def test_request_requires_true_push_to_talk_and_rejects_raw_derived_or_tampered_shapes(self):
        for payload in (
            valid_payload(push_to_talk=False),
            valid_payload(push_to_talk=1),
            valid_payload(push_to_talk='true'),
            {'service_id': 'speech-to-text', 'request_id': 'request-001'},
            valid_payload(audio_bytes=b'raw audio'),
            valid_payload(audio_path='/tmp/audio.wav'),
            valid_payload(model_data='weights'),
            valid_payload(port=9000),
            valid_payload(command=['arecord']),
            valid_payload(credentials='secret'),
        ):
            with self.subTest(payload=payload), self.assertRaises(admission.STTTranscriptionRequestError):
                admission.validate_transcription_request(payload)

        with self.assertRaises(admission.STTTranscriptionRequestError):
            admission.validate_transcription_request(
                DuplicateKeyMapping([
                    ('service_id', 'speech-to-text'),
                    ('request_id', 'request-001'),
                    ('request_id', 'tampered'),
                    ('push_to_talk', True),
                ])
            )

        validated = admission.validate_transcription_request(valid_payload())
        for candidate in (valid_payload(), dict(validated), {**dict(validated), 'request_id': 'tampered'}):
            with self.subTest(candidate=candidate), self.assertRaises(admission.STTTranscriptionRequestError):
                admission.build_transcription_request_evidence(candidate)

    def test_identifiers_are_canonical_bounded_and_not_paths_urls_commands_or_credentials(self):
        unsafe_values = (
            '',
            '../speech-to-text',
            'speech/to-text',
            'https://example.invalid',
            'speech-to-text:9000',
            'speech-to-text;arecord',
            'password-token',
            'request with spaces',
        )
        for field in ('service_id', 'request_id'):
            for value in unsafe_values:
                with self.subTest(field=field, value=value), self.assertRaises(admission.STTTranscriptionRequestError):
                    admission.validate_transcription_request(valid_payload(**{field: value}))
        for field, value in (
            ('service_id', 'x' * (admission.MAX_SERVICE_ID_BYTES + 1)),
            ('request_id', 'x' * (admission.MAX_REQUEST_ID_BYTES + 1)),
        ):
            with self.subTest(field=field), self.assertRaises(admission.STTTranscriptionRequestError):
                admission.validate_transcription_request(valid_payload(**{field: value}))

    def test_request_and_evidence_envelopes_are_bounded_and_metadata_only(self):
        oversized = valid_payload(request_id='x' * (admission.MAX_REQUEST_ID_BYTES + 1))
        with self.assertRaises(admission.STTTranscriptionRequestError):
            admission.validate_transcription_request(oversized)

        request = admission.validate_transcription_request(valid_payload())
        evidence = admission.build_transcription_request_evidence(request)
        self.assertIsInstance(evidence, MappingProxyType)
        self.assertEqual(
            set(evidence),
            {
                'service_id', 'request_id', 'push_to_talk', 'request_valid',
                'execution_enabled', 'request_queued', 'audio_accessed',
                'transcription_started', 'status',
            },
        )
        self.assertEqual(evidence['status'], 'disabled')
        self.assertTrue(evidence['request_valid'])
        for field in ('execution_enabled', 'request_queued', 'audio_accessed', 'transcription_started'):
            self.assertFalse(evidence[field])
        with self.assertRaises(TypeError):
            evidence['status'] = 'started'
        encoded = json.dumps(dict(evidence), separators=(',', ':')).encode('utf-8')
        self.assertLessEqual(len(encoded), admission.MAX_EVIDENCE_BYTES)

    def test_actor_uses_validated_contract_and_preserves_disabled_result(self):
        result = actor.execute(
            'client_service_transcribe_request',
            valid_payload(),
            {},
        )

        self.assertFalse(result['ok'])
        self.assertEqual(result['code'], 'execution_disabled')
        self.assertFalse(result['execution_enabled'])
        self.assertEqual(result['state'], 'disabled')
        self.assertEqual(result['status'], 'disabled')
        self.assertEqual(result['details'], {
            'service_id': 'speech-to-text',
            'request_id': 'request-001',
            'push_to_talk': True,
            'request_valid': True,
            'execution_enabled': False,
            'request_queued': False,
            'audio_accessed': False,
            'transcription_started': False,
            'status': 'disabled',
            'audio_bytes': 0,
            'transcript': None,
        })

        for payload in (
            valid_payload(audio=b'raw'),
            valid_payload(request_id='request/001'),
            valid_payload(push_to_talk=False),
        ):
            with self.subTest(payload=payload):
                rejected = actor.execute(
                    'client_service_transcribe_request', payload, {}
                )
                self.assertFalse(rejected['ok'])
                self.assertEqual(rejected['code'], 'invalid_payload')
                self.assertFalse(rejected['execution_enabled'])

    def test_contract_source_contains_no_runtime_activation_or_capture_path(self):
        source = (ROOT / 'player-client/scripts/screentra-stt-transcription-request-admission.py').read_text(encoding='utf-8')
        for forbidden in (
            'socket', 'subprocess', 'Popen', 'run(', 'getUserMedia', 'MediaRecorder',
            'arecord', 'ffmpeg', 'http://', 'https://',
        ):
            self.assertNotIn(forbidden, source)


if __name__ == '__main__':
    unittest.main()
