#!/usr/bin/env python3
import importlib.util
import os
import tempfile
import unittest
from pathlib import Path
from unittest import mock

ROOT = Path(__file__).resolve().parents[1]
MODULE_PATH = ROOT / 'player-client/scripts/screentra-player-preflight.py'
spec = importlib.util.spec_from_file_location('screentra_player_preflight_files', MODULE_PATH)
preflight = importlib.util.module_from_spec(spec)
assert spec.loader is not None
spec.loader.exec_module(preflight)


class PlayerPreflightFileTests(unittest.TestCase):
    def test_command_runner_has_no_shell_fallback(self):
        self.assertEqual(preflight.run('echo attacker-controlled'), '')

    def test_tls_context_rejects_symlinked_ca(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            outside = root / 'outside.crt'
            outside.write_text('not a certificate', encoding='utf-8')
            redirected = root / 'ca.crt'
            redirected.symlink_to(outside)
            with mock.patch.dict(os.environ, {'RIPLEY_CA_CERT': str(redirected)}, clear=False):
                self.assertIsNone(preflight.tls_context())


if __name__ == '__main__':
    unittest.main()
