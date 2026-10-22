#!/usr/bin/env python3
import importlib.util
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
MODULE_PATH = ROOT / 'player-client/scripts/screentra-player-renderer.py'
spec = importlib.util.spec_from_file_location('screentra_player_renderer_files', MODULE_PATH)
renderer = importlib.util.module_from_spec(spec)
assert spec.loader is not None
spec.loader.exec_module(renderer)


class PlayerRendererFileTests(unittest.TestCase):
    def test_json_reader_rejects_symlinked_files(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            outside = root / 'outside.json'
            outside.write_text('{"unsafe":true}', encoding='utf-8')
            redirected = root / 'cache.json'
            redirected.symlink_to(outside)
            self.assertEqual(renderer._read_json_file(redirected, {'safe': True}), {'safe': True})

    def test_auth_secret_rejects_symlinked_auth_directory(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            outside = root / 'outside'
            outside.mkdir()
            cache = root / 'cache'
            cache.mkdir()
            (root / 'admin-auth').symlink_to(outside, target_is_directory=True)
            renderer.Handler.cache_dir = cache
            with self.assertRaises(ValueError):
                renderer._auth_secret()

    def test_atomic_writer_rejects_symlinked_parents(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            outside = root / 'outside'
            outside.mkdir()
            redirected = root / 'cache'
            redirected.symlink_to(outside, target_is_directory=True)
            with self.assertRaises(ValueError):
                renderer._atomic_write_text(redirected / 'status.json', '{"safe":true}')
            self.assertFalse((outside / 'status.json').exists())


if __name__ == '__main__':
    unittest.main()
