#!/usr/bin/env python3
import importlib.util
import stat
import tempfile
import unittest
from unittest import mock
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def load_module(path: Path, name: str):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    spec.loader.exec_module(module)
    return module


actor = load_module(ROOT / 'player-client/scripts/screentra-player-actor.py', 'screentra_player_actor_audit')


class PlayerClientAuditTests(unittest.TestCase):
    def test_bluetooth_connect_requires_object_payload(self):
        ok, message, details = actor.bluetooth_connect(['not-an-object'])
        self.assertFalse(ok)
        self.assertIn('payload', message.lower())
        self.assertEqual(details, {})

    def test_bluetooth_scan_duration_is_bounded(self):
        ok, message, details = actor.bluetooth_scan({'duration_seconds': 'invalid'})
        self.assertFalse(ok)
        self.assertIn('duration', message.lower())
        self.assertEqual(details, {})

    def test_bluetooth_name_is_bounded(self):
        self.assertEqual(actor._validate_bluetooth_name('Living Room'), 'Living Room')
        with self.assertRaises(ValueError):
            actor._validate_bluetooth_name('x' * 256)
        with self.assertRaises(ValueError):
            actor._validate_bluetooth_name('bad\nname')

    def test_subprocess_output_is_bounded(self):
        completed = mock.Mock(returncode=0, stdout='x' * (actor.MAX_SUBPROCESS_OUTPUT_BYTES + 100), stderr='y' * (actor.MAX_SUBPROCESS_OUTPUT_BYTES + 100))
        with mock.patch.object(actor.subprocess, 'run', return_value=completed):
            _, stdout, stderr = actor._run_list(['fixture'])
        self.assertLessEqual(len(stdout.encode('utf-8')), actor.MAX_SUBPROCESS_OUTPUT_BYTES)
        self.assertLessEqual(len(stderr.encode('utf-8')), actor.MAX_SUBPROCESS_OUTPUT_BYTES)

    def test_audio_controls_require_object_payloads(self):
        ok, message, details = actor.audio_set_default(['not-an-object'])
        self.assertFalse(ok)
        self.assertIn('payload', message.lower())
        self.assertEqual(details, {})

    def test_audio_sink_identifiers_are_bounded(self):
        self.assertEqual(actor._validate_audio_sink_id('12'), '12')
        self.assertEqual(actor._validate_audio_sink_id('@DEFAULT_AUDIO_SINK@'), '@DEFAULT_AUDIO_SINK@')
        with self.assertRaises(ValueError):
            actor._validate_audio_sink_id('9' * 64)
        with self.assertRaises(ValueError):
            actor._validate_audio_sink_id('sink-name')

    def test_ddc_fields_are_bounded(self):
        self.assertEqual(actor._validate_ddc_vcp('D6'), 'D6')
        self.assertEqual(actor._validate_ddc_value(100), 100)
        self.assertEqual(actor._validate_ddc_bus('1'), '1')
        with self.assertRaises(ValueError):
            actor._validate_ddc_vcp('--output')
        with self.assertRaises(ValueError):
            actor._validate_ddc_value(70000)
        with self.assertRaises(ValueError):
            actor._validate_ddc_bus('9999')

    def test_cec_command_is_bounded_and_allowlisted(self):
        self.assertEqual(actor._validate_cec_command('pow 0'), 'pow 0')
        with self.assertRaises(ValueError):
            actor._validate_cec_command('tx 0F:44:6D:00')
        with self.assertRaises(ValueError):
            actor._validate_cec_command('x' * 128)

    def test_actor_requests_root_verified_update_and_rejects_insecure_remote_http(self):
        self.assertEqual(actor._server_origin('https://managed.example:3000/base/'), 'https://managed.example:3000/base')
        self.assertEqual(actor._server_origin('http://127.0.0.1:3000'), 'http://127.0.0.1:3000')
        with self.assertRaises(ValueError):
            actor._server_origin('http://managed.example:3000')
        with tempfile.TemporaryDirectory() as temp:
            ok, message, details = actor.update_client(
                'https://managed.example:3000',
                'client-id',
                Path(temp) / 'cache',
                {'server_url': 'https://attacker.example', 'dry_run': True},
            )
        self.assertTrue(ok)
        self.assertEqual(details['request_path'], '/var/lib/screentra-cast/update/request')
        self.assertNotIn('attacker.example', message)

    def test_actor_atomic_cache_writes_reject_symlinked_parents(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            outside = root / 'outside'
            outside.mkdir()
            redirected = root / 'cache'
            redirected.symlink_to(outside, target_is_directory=True)
            with self.assertRaises(ValueError):
                actor._atomic_write_text(redirected / 'status.json', '{"safe":true}')
            self.assertFalse((outside / 'status.json').exists())

    def test_update_request_marker_uses_private_atomic_publication(self):
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / 'request'
            actor._write_update_request_marker(
                path,
                {'client_id': 'client-id', 'reason': 'server-command'},
            )
            self.assertEqual(path.read_text(encoding='utf-8'),
                             '{"client_id":"client-id","reason":"server-command"}')
            self.assertEqual(path.stat().st_mode & 0o777, 0o600)
            self.assertEqual(list(Path(temp).iterdir()), [path])

    def test_actor_json_reader_rejects_symlinked_files(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            outside = root / 'outside.json'
            outside.write_text('{"unsafe":true}', encoding='utf-8')
            redirected = root / 'cache.json'
            redirected.symlink_to(outside)
            self.assertEqual(actor._read_json_file(redirected, {'safe': True}), {'safe': True})

    def test_pending_json_reader_is_bounded(self):
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / 'pending-command.json'
            path.write_bytes(b'{' + b'a' * actor.MAX_PENDING_BYTES + b'}')
            self.assertEqual(actor._read_json_file(path, {'safe': True}), {'safe': True})

    def test_installer_and_bundle_surfaces_are_hardened(self):
        canonical = (ROOT / 'player-client/install/player-client.sh').read_text()
        generated = (ROOT / 'player-client/static/install/player-client.sh').read_text()
        for installer in (canonical, generated):
            self.assertNotIn('_create_unverified_context', installer)
            self.assertNotIn('RIPLEY_INSECURE_SSL', installer)
            self.assertNotIn('RIPLEY_ADMIN_DEFAULT', installer)
            self.assertNotIn('--host 0.0.0.0', installer)
            self.assertIn('--host 127.0.0.1', installer)
            self.assertIn('RIPLEY_ADMIN_MODE', installer)
            self.assertNotIn('ripley-player-actor.service', installer)
            self.assertIn('screentra-client-actor.service', installer)
        deprecated = ROOT / 'static/install/screentra-cast-player-client.sh'
        mode = deprecated.stat().st_mode
        self.assertFalse(mode & (stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH))
        self.assertNotIn('sudo', deprecated.read_text())
        bootstrap = (ROOT / 'static/install/player-client-bundle-install.sh').read_text()
        self.assertIn('MAX_BUNDLE_BYTES', bootstrap)
        self.assertIn('tarfile.open', bootstrap)
        self.assertNotIn('extractall', bootstrap)
        self.assertIn('pkeyutl', bootstrap)
    def test_actor_bounded_text_reader_rejects_symlinks(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            outside = root / 'outside.txt'
            outside.write_text('secret', encoding='utf-8')
            redirected = root / 'token'
            redirected.symlink_to(outside)
            with self.assertRaises(ValueError):
                actor._read_bounded_text(redirected, 64)


if __name__ == '__main__':
    unittest.main()
