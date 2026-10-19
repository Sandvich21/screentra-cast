#!/usr/bin/env python3
import importlib.util
import re
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def load_module(path: Path, name: str):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    spec.loader.exec_module(module)
    return module


actor = load_module(
    ROOT / 'player-client/scripts/screentra-player-actor.py',
    'screentra_player_actor_client_service_contract',
)


def server_actions():
    source = (ROOT / 'src/api.rs').read_text(encoding='utf-8')
    match = re.search(
        r'const ALLOWED_CLIENT_COMMAND_ACTIONS: &\[&str\] = &\[(.*?)\];',
        source,
        re.DOTALL,
    )
    if not match:
        raise AssertionError('server client command allowlist is missing')
    return set(re.findall(r'"([^"\\]+)"', match.group(1)))


class ClientServiceContractTests(unittest.TestCase):
    def test_server_and_actor_action_allowlists_are_symmetric(self):
        self.assertEqual(server_actions(), set(actor.ALLOWED_CLIENT_COMMAND_ACTIONS))
        generated = (ROOT / 'player-client/static/install/player-client.sh').read_text(encoding='utf-8')
        self.assertIn("'client_service_status'", generated)
        self.assertIn("'client_service_rollback_request'", generated)
        self.assertIn("'client_service_transcribe_request'", generated)
        self.assertIn("'execution_disabled'", generated)
        self.assertIn('client_service_status', server_actions())
        self.assertIn('client_service_rollback_request', server_actions())
        self.assertIn('client_service_transcribe_request', server_actions())
        for action in (
            'client_service_start',
            'client_service_stop',
            'client_service_update',
            'client_service_rollback',
        ):
            self.assertNotIn(action, server_actions())

    def test_pending_command_identity_and_payload_are_bounded(self):
        valid = {
            'client_id': 'client-a',
            'action': 'client_service_status',
            'payload': {'service_id': 'speech-to-text'},
            'requested_at': '2026-10-18T18:00:00Z',
        }
        self.assertEqual(actor.validate_pending_command(valid, 'client-a'), valid)
        with self.assertRaises(ValueError):
            actor.validate_pending_command({**valid, 'client_id': 'client-b'}, 'client-a')
        with self.assertRaises(ValueError):
            actor.validate_pending_command({**valid, 'payload': 'not-an-object'}, 'client-a')
        with self.assertRaises(ValueError):
            actor.validate_pending_command(
                {**valid, 'payload': {'service_id': 'x' * 129}}, 'client-a'
            )
        with self.assertRaises(ValueError):
            actor.validate_pending_command(
                {**valid, 'payload': {'service_id': 'x', 'extra': 'not allowed'}},
                'client-a',
            )
        with self.assertRaises(ValueError):
            actor.validate_pending_command(
                {**valid, 'payload': {'service_id': 'x' * (actor.MAX_COMMAND_PAYLOAD_BYTES + 1)}},
                'client-a',
            )

    def test_transcribe_request_payload_is_strictly_bounded(self):
        valid = {
            'client_id': 'client-a',
            'action': 'client_service_transcribe_request',
            'payload': {
                'service_id': 'speech-to-text',
                'request_id': 'request-001',
                'push_to_talk': True,
            },
            'requested_at': '2026-10-18T18:00:00Z',
        }
        self.assertEqual(actor.validate_pending_command(valid, 'client-a'), valid)
        for payload in (
            {'service_id': 'speech-to-text', 'request_id': 'request-001'},
            {'service_id': 'speech-to-text', 'request_id': 'request-001', 'push_to_talk': 'true'},
            {'service_id': 'speech/to-text', 'request_id': 'request-001', 'push_to_talk': True},
            {'service_id': 'speech-to-text', 'request_id': 'request 001', 'push_to_talk': True},
            {'service_id': 'speech-to-text', 'request_id': 'request-001', 'push_to_talk': True, 'audio': 'raw'},
        ):
            with self.assertRaises(ValueError):
                actor.validate_pending_command({**valid, 'payload': payload}, 'client-a')
        with self.assertRaises(ValueError):
            actor.validate_pending_command(
                {**valid, 'payload': {**valid['payload'], 'request_id': 'x' * (actor.CLIENT_SERVICE_REQUEST_ID_MAX_BYTES + 1)}},
                'client-a',
            )

    def test_transcribe_request_execution_is_disabled_without_local_side_effects(self):
        result = actor.execute(
            'client_service_transcribe_request',
            {
                'service_id': 'speech-to-text',
                'request_id': 'request-001',
                'push_to_talk': True,
            },
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

    def test_client_service_execution_is_disabled_and_structured(self):
        status = actor.execute(
            'client_service_status',
            {'service_id': 'speech-to-text'},
            {'client_id': 'client-a', 'cache_dir': '/tmp/screentra-contract-test'},
        )
        self.assertTrue(status['ok'])
        self.assertFalse(status['execution_enabled'])
        self.assertEqual(status['state'], 'disabled')

        rollback = actor.execute(
            'client_service_rollback_request',
            {
                'service_id': 'speech-to-text',
                'activation_id': 'activation-1',
            },
            {'client_id': 'client-a', 'cache_dir': '/tmp/screentra-contract-test'},
        )
        self.assertFalse(rollback['ok'])
        self.assertFalse(rollback['execution_enabled'])
        self.assertEqual(rollback['code'], 'execution_disabled')

        for action in (
            'client_service_start',
            'client_service_stop',
            'client_service_update',
            'client_service_rollback',
        ):
            result = actor.execute(action, None, {'client_id': 'client-a', 'cache_dir': '/tmp'})
            self.assertFalse(result['ok'])
            self.assertEqual(result['code'], 'unsupported_action')


if __name__ == '__main__':
    unittest.main()
