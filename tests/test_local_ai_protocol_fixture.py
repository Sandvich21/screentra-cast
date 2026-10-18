#!/usr/bin/env python3
import importlib.util
import json
import struct
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
MODULE_PATH = ROOT / 'player-client/scripts/screentra-local-ai-codec.py'
spec = importlib.util.spec_from_file_location('screentra_local_ai_codec', MODULE_PATH)
codec = importlib.util.module_from_spec(spec)
assert spec.loader is not None
sys.modules[spec.name] = codec
spec.loader.exec_module(codec)

from local_ai_protocol_fixture import InMemoryLocalAiProtocolFixture


class LocalAiProtocolFixtureTests(unittest.TestCase):
    def test_health_response_is_configurable_and_codec_encoded(self):
        fixture = InMemoryLocalAiProtocolFixture(health_status='degraded')
        request = codec.encode_request('health-1', codec.HealthRequest())

        response = codec.decode_response_frame(fixture.handle_frame(request))

        self.assertEqual(response.request_id, 'health-1')
        self.assertEqual(response.operation, 'health')
        self.assertTrue(response.ok)
        self.assertEqual(response.result.status, 'degraded')
        self.assertTrue(response.local_only)

    def test_transcription_is_deterministic_for_bounded_audio_and_language_only(self):
        fixture = InMemoryLocalAiProtocolFixture(
            transcriptions={(b'pcm-a', 'en-US'): 'fixed transcript'}
        )
        first = codec.encode_request(
            'transcribe-1', codec.TranscribeRequest(b'pcm-a', language='en-US')
        )
        second = codec.encode_request(
            'transcribe-2', codec.TranscribeRequest(b'pcm-a', language='en-US')
        )

        first_response = codec.decode_response_frame(fixture.handle_frame(first))
        second_response = codec.decode_response_frame(fixture.handle_frame(second))

        self.assertEqual(first_response.result.text, 'fixed transcript')
        self.assertEqual(second_response.result.text, 'fixed transcript')
        self.assertEqual(first_response.request_id, 'transcribe-1')
        self.assertEqual(second_response.request_id, 'transcribe-2')

    def test_unknown_bounded_transcription_input_returns_local_error_response(self):
        fixture = InMemoryLocalAiProtocolFixture(
            transcriptions={(b'pcm-a', 'en-US'): 'fixed transcript'}
        )
        request = codec.encode_request(
            'transcribe-3', codec.TranscribeRequest(b'other-pcm', language='en-US')
        )

        response = codec.decode_response_frame(fixture.handle_frame(request))

        self.assertFalse(response.ok)
        self.assertIsNone(response.result)
        self.assertEqual(response.error.code, 'unavailable')
        self.assertTrue(response.local_only)

    def test_malformed_and_unsupported_frames_keep_protocol_error_boundary(self):
        fixture = InMemoryLocalAiProtocolFixture()
        malformed = struct.pack('!I', 3) + b'{no'
        unsupported_body = json.dumps(
            {
                'local_only': True,
                'operation': 'shell',
                'payload': {},
                'request_id': 'bad-op',
                'version': codec.PROTOCOL_VERSION,
            },
            separators=(',', ':'),
        ).encode('utf-8')
        unsupported = struct.pack('!I', len(unsupported_body)) + unsupported_body

        with self.assertRaises(codec.ProtocolError):
            fixture.handle_frame(malformed)
        with self.assertRaises(codec.ProtocolError):
            fixture.handle_frame(unsupported)

    def test_fixture_outputs_remain_bounded_by_existing_codec(self):
        fixture = InMemoryLocalAiProtocolFixture(
            health_status='ok',
            transcriptions={(b'pcm', None): 'x' * (codec.MAX_TEXT_BYTES + 1)},
        )
        request = codec.encode_request(
            'bounded-1', codec.TranscribeRequest(b'pcm')
        )

        with self.assertRaises(codec.ProtocolError):
            fixture.handle_frame(request)


if __name__ == '__main__':
    unittest.main()
