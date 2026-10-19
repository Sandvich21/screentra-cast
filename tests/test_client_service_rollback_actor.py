#!/usr/bin/env python3
import importlib.util
import tempfile
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
    'screentra_player_actor_rollback_request',
)


class ClientServiceRollbackActorTests(unittest.TestCase):
    def test_actor_uses_exact_admission_and_returns_disabled_metadata_only_result(self):
        with tempfile.TemporaryDirectory() as temp:
            before = sorted(Path(temp).iterdir())
            result = actor.execute(
                'client_service_rollback_request',
                {
                    'service_id': 'speech-to-text',
                    'activation_id': 'activation-001',
                },
                {'cache_dir': temp},
            )
            after = sorted(Path(temp).iterdir())

        self.assertFalse(result['ok'])
        self.assertEqual(result['code'], 'execution_disabled')
        self.assertFalse(result['execution_enabled'])
        self.assertEqual(result['state'], 'disabled')
        self.assertEqual(result['status'], 'disabled')
        self.assertEqual(result['details'], {
            'service_id': 'speech-to-text',
            'activation_id': 'activation-001',
            'request_valid': True,
            'execution_enabled': False,
            'request_queued': False,
            'rollback_executed': False,
            'state_changed': False,
            'status': 'disabled',
        })
        self.assertEqual(before, after)

    def test_actor_rejects_raw_derived_tampered_mismatched_and_runtime_shaped_payloads(self):
        valid = {
            'service_id': 'speech-to-text',
            'activation_id': 'activation-001',
        }
        invalid_payloads = (
            {'service_id': 'speech-to-text'},
            {**valid, 'target_version': 'v2'},
            {**valid, 'audio': b'raw'},
            {**valid, 'model_path': '/models/stt'},
            {**valid, 'port': 9000},
            {**valid, 'command': ['systemctl', 'stop']},
            {**valid, 'activation_id': '../other'},
            {**valid, 'service_id': 'https://example.invalid'},
        )
        for payload in invalid_payloads:
            with self.subTest(payload=payload):
                result = actor.execute('client_service_rollback_request', payload, {})
                self.assertFalse(result['ok'])
                self.assertEqual(result['code'], 'invalid_payload')
                self.assertFalse(result['execution_enabled'])

    def test_actor_source_wires_dedicated_admission_and_does_not_rollback(self):
        source = (ROOT / 'player-client/scripts/screentra-player-actor.py').read_text(encoding='utf-8')
        self.assertIn('screentra-client-service-rollback-request-admission.py', source)
        self.assertIn('build_rollback_request_evidence', source)
        rollback_start = source.rfind("if action == 'client_service_rollback_request'")
        rollback_end = source.index("if action == 'client_service_transcribe_request'", rollback_start)
        rollback_branch = source[rollback_start:rollback_end]
        self.assertNotIn('subprocess', rollback_branch)
        self.assertNotIn('systemctl', rollback_branch)
        self.assertNotIn('open(', rollback_branch)


if __name__ == '__main__':
    unittest.main()
