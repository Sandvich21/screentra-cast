#!/usr/bin/env python3
import importlib.util
import unittest
from pathlib import Path
from unittest import mock

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location('screentra_gpu_health', ROOT / 'player-client/scripts/screentra-gpu-health.py')
gpu = importlib.util.module_from_spec(spec)
assert spec.loader is not None
spec.loader.exec_module(gpu)


class GpuHealthTests(unittest.TestCase):
    def test_subprocess_output_is_bounded(self):
        completed = mock.Mock(returncode=0, stdout='x' * (gpu.MAX_SUBPROCESS_OUTPUT_BYTES + 100), stderr='')
        with mock.patch.object(gpu.subprocess, 'run', return_value=completed):
            rc, stdout, stderr = gpu._run(['lspci', '-nn'])
        self.assertEqual(rc, 0)
        self.assertLessEqual(len(stdout.encode('utf-8')), gpu.MAX_SUBPROCESS_OUTPUT_BYTES)
        self.assertEqual(stderr, '')


if __name__ == '__main__':
    unittest.main()
