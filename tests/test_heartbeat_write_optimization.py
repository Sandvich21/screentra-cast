#!/usr/bin/env python3
import importlib.util
import tempfile
import unittest
from pathlib import Path
from unittest import mock

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location(
    'screentra_player_heartbeat_write_optimization',
    ROOT / 'player-client/scripts/screentra-player-heartbeat.py',
)
heartbeat = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
SPEC.loader.exec_module(heartbeat)


class HeartbeatWriteOptimizationTests(unittest.TestCase):
    def test_atomic_write_if_changed_skips_identical_content(self):
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / 'cache.json'
            self.assertTrue(heartbeat._atomic_write_text_if_changed(path, '{"value":1}'))
            with mock.patch.object(heartbeat, '_atomic_write_text', wraps=heartbeat._atomic_write_text) as write:
                self.assertFalse(heartbeat._atomic_write_text_if_changed(path, '{"value":1}'))
            write.assert_not_called()
            self.assertEqual(path.read_text(), '{"value":1}')

    def test_fetch_all_displays_can_skip_primary_display(self):
        calls = []
        with mock.patch.object(heartbeat, 'fetch_config', side_effect=lambda *args, **kwargs: calls.append(kwargs.get('display_index'))):
            heartbeat.fetch_all_displays(
                'https://server.example', 'client-a', Path('/tmp/cache'), 3,
                'assigned-layout-display-{index}.json', start_index=1,
            )
        self.assertEqual(calls, [1, 2])


if __name__ == '__main__':
    unittest.main()
