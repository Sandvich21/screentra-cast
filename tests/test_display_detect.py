#!/usr/bin/env python3
import importlib.util
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location('screentra_display_detect', ROOT / 'player-client/scripts/screentra-display-detect.py')
detect = importlib.util.module_from_spec(spec)
assert spec.loader is not None
spec.loader.exec_module(detect)


class DisplayDetectTests(unittest.TestCase):
    def test_sysfs_values_are_bounded(self):
        with tempfile.NamedTemporaryFile() as temp:
            temp.write(b'x' * (detect.MAX_SYSFS_VALUE_BYTES + 100))
            temp.flush()
            self.assertLessEqual(len(detect._read(Path(temp.name)).encode('utf-8')), detect.MAX_SYSFS_VALUE_BYTES)

    def test_connected_disabled_connector_precedes_disconnected_output(self):
        connectors = [
            {'name': 'DP-1', 'status': 'disconnected', 'enabled': False},
            {'name': 'DP-2', 'status': 'connected', 'enabled': False},
            {'name': 'DP-3', 'status': 'connected', 'enabled': True},
        ]
        ordered = sorted(connectors, key=detect._connector_sort_key)
        self.assertEqual([item['name'] for item in ordered], ['DP-3', 'DP-2', 'DP-1'])

    def test_display_limit_is_bounded(self):
        self.assertLessEqual(len(detect.detect_displays(max_displays=10_000)), detect.MAX_DISPLAYS)
        self.assertLessEqual(len(detect.detect_displays(max_displays='invalid')), detect.MAX_DISPLAYS)


if __name__ == '__main__':
    unittest.main()
