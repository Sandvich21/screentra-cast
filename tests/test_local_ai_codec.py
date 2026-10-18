#!/usr/bin/env python3
import base64
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


class LocalAiCodecTests(unittest.TestCase):
    def test_health_request_round_trips_as_local_only(self):
        frame = codec.encode_request('req-1', codec.HealthRequest())

        request = codec.decode_request_frame(frame)

        self.assertTrue(codec.LOCAL_ONLY)
        self.assertEqual(request.request_id, 'req-1')
        self.assertIsInstance(request.payload, codec.HealthRequest)

    def test_transcribe_request_allows_only_bounded_local_audio(self):
        audio = b'\x00\x01pcm'
        frame = codec.encode_request(
            'req-2', codec.TranscribeRequest(audio=audio, language='en-US')
        )

        request = codec.decode_request_frame(frame)

        self.assertEqual(request.payload.audio, audio)
        self.assertEqual(request.payload.language, 'en-US')
        self.assertLessEqual(len(frame) - codec.FRAME_LENGTH_BYTES, codec.MAX_FRAME_BYTES)

    def test_transcribe_audio_bound_is_enforced(self):
        with self.assertRaises(codec.ProtocolError):
            codec.encode_request(
                'req-3', codec.TranscribeRequest(audio=b'x' * (codec.MAX_AUDIO_BYTES + 1))
            )

    def test_fixed_width_frame_rejects_oversized_and_truncated_input(self):
        frame = codec.encode_request('req-4', codec.HealthRequest())
        declared = struct.unpack('!I', frame[:codec.FRAME_LENGTH_BYTES])[0]

        with self.assertRaises(codec.ProtocolError):
            codec.decode_request_frame(
                struct.pack('!I', codec.MAX_FRAME_BYTES + 1) + b'x'
            )
        with self.assertRaises(codec.ProtocolError):
            codec.decode_request_frame(frame[:-1])
        with self.assertRaises(codec.ProtocolError):
            codec.decode_request_frame(
                struct.pack('!I', declared + 1) + frame[codec.FRAME_LENGTH_BYTES:]
            )

    def test_malformed_json_and_unknown_operation_are_rejected(self):
        malformed = struct.pack('!I', 3) + b'{no'
        with self.assertRaises(codec.ProtocolError):
            codec.decode_request_frame(malformed)

        body = json.dumps(
            {
                'local_only': True,
                'operation': 'shell',
                'payload': {},
                'request_id': 'req-5',
                'version': codec.PROTOCOL_VERSION,
            },
            separators=(',', ':'),
        ).encode('utf-8')
        with self.assertRaises(codec.ProtocolError):
            codec.decode_request_frame(struct.pack('!I', len(body)) + body)

    def test_request_envelope_and_payload_reject_paths_commands_credentials_and_raw_audio(self):
        base = {
            'local_only': True,
            'operation': 'health',
            'payload': {},
            'request_id': 'req-6',
            'version': codec.PROTOCOL_VERSION,
        }
        for forbidden in ('path', 'command', 'password', 'token', 'credential', 'audio'):
            envelope = dict(base)
            envelope[forbidden] = 'forbidden'
            body = json.dumps(envelope, separators=(',', ':')).encode('utf-8')
            with self.subTest(forbidden=forbidden), self.assertRaises(codec.ProtocolError):
                codec.decode_request_frame(struct.pack('!I', len(body)) + body)

        payload = dict(base)
        payload['operation'] = 'transcribe'
        payload['payload'] = {'audio': base64.b64encode(b'raw').decode('ascii')}
        body = json.dumps(payload, separators=(',', ':')).encode('utf-8')
        with self.assertRaises(codec.ProtocolError):
            codec.decode_request_frame(struct.pack('!I', len(body)) + body)

    def test_request_id_operation_and_envelope_limits_are_enforced(self):
        with self.assertRaises(codec.ProtocolError):
            codec.encode_request('x' * (codec.MAX_REQUEST_ID_BYTES + 1), codec.HealthRequest())
        with self.assertRaises(codec.ProtocolError):
            codec.encode_request('bad id', codec.HealthRequest())

        frame = codec.encode_request('req-7', codec.HealthRequest())
        body = json.loads(frame[codec.FRAME_LENGTH_BYTES:])
        body['request_id'] = 'x' * (codec.MAX_REQUEST_ID_BYTES + 1)
        encoded = json.dumps(body, separators=(',', ':')).encode('utf-8')
        with self.assertRaises(codec.ProtocolError):
            codec.decode_request_frame(struct.pack('!I', len(encoded)) + encoded)

    def test_typed_responses_round_trip_without_audio_or_server_reporting_fields(self):
        health = codec.decode_response_frame(
            codec.encode_response('req-8', 'health', codec.HealthResponse(status='ok'))
        )
        self.assertEqual(health.result.status, 'ok')
        self.assertTrue(health.local_only)

        transcribe = codec.decode_response_frame(
            codec.encode_response(
                'req-9', 'transcribe', codec.TranscribeResponse(text='hello')
            )
        )
        self.assertEqual(transcribe.result.text, 'hello')

        frame = codec.encode_response(
            'req-10', 'transcribe', codec.TranscribeResponse(text='x')
        )
        body = json.loads(frame[codec.FRAME_LENGTH_BYTES:])
        body['result']['audio_b64'] = base64.b64encode(b'bad').decode('ascii')
        encoded = json.dumps(body, separators=(',', ':')).encode('utf-8')
        with self.assertRaises(codec.ProtocolError):
            codec.decode_response_frame(struct.pack('!I', len(encoded)) + encoded)

    def test_response_rejects_unknown_operation_and_malformed_result(self):
        with self.assertRaises(codec.ProtocolError):
            codec.encode_response('req-11', 'shell', codec.HealthResponse(status='ok'))

        frame = codec.encode_response(
            'req-12', 'health', codec.HealthResponse(status='ok')
        )
        body = json.loads(frame[codec.FRAME_LENGTH_BYTES:])
        body['result'] = {'status': 'ok', 'command': 'uname'}
        encoded = json.dumps(body, separators=(',', ':')).encode('utf-8')
        with self.assertRaises(codec.ProtocolError):
            codec.decode_response_frame(struct.pack('!I', len(encoded)) + encoded)

    def test_error_response_is_code_only_and_local(self):
        response = codec.decode_response_frame(
            codec.encode_response('req-13', 'health', codec.ErrorResponse('unavailable'))
        )
        self.assertFalse(response.ok)
        self.assertEqual(response.error.code, 'unavailable')
        self.assertIsNone(response.result)

    def test_duplicate_json_fields_and_boolean_version_are_rejected(self):
        duplicate = (
            b'{"local_only":true,"operation":"health","payload":{},'
            b'"request_id":"req-14","request_id":"other","version":1}'
        )
        with self.assertRaises(codec.ProtocolError):
            codec.decode_request_frame(struct.pack('!I', len(duplicate)) + duplicate)

        body = json.dumps(
            {
                'local_only': True,
                'operation': 'health',
                'payload': {},
                'request_id': 'req-15',
                'version': True,
            },
            separators=(',', ':'),
        ).encode('utf-8')
        with self.assertRaises(codec.ProtocolError):
            codec.decode_request_frame(struct.pack('!I', len(body)) + body)


if __name__ == '__main__':
    unittest.main()
