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

    def test_command_runner_bounds_probe_output(self):
        completed = mock.Mock(stdout='x' * (preflight.MAX_LOCAL_PROBE_OUTPUT_BYTES + 100), returncode=0)
        with mock.patch.object(preflight.subprocess, 'run', return_value=completed):
            result = preflight.run('id -Gn')
        self.assertLessEqual(len(result.encode('utf-8')), preflight.MAX_LOCAL_PROBE_OUTPUT_BYTES)

    def test_error_details_are_bounded(self):
        self.assertLessEqual(len(preflight._bound_error('x' * (preflight.MAX_ERROR_BYTES + 1)).encode('utf-8')), preflight.MAX_ERROR_BYTES)

    def test_directory_check_rejects_symlinked_install_path(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            outside = root / 'outside'
            outside.mkdir()
            redirected = root / 'install'
            redirected.symlink_to(outside, target_is_directory=True)
            with self.assertRaises(ValueError):
                preflight._ensure_real_directory(redirected, create=True)

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
