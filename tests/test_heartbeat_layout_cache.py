#!/usr/bin/env python3
import importlib.util
import json
import tempfile
import unittest
from pathlib import Path
from unittest import mock

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location(
    'screentra_player_heartbeat_layout_cache',
    ROOT / 'player-client/scripts/screentra-player-heartbeat.py',
)
heartbeat = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
SPEC.loader.exec_module(heartbeat)


class _Response:
    def __init__(self, payload):
        self.payload = json.dumps(payload).encode('utf-8')

    def __enter__(self):
        return self

    def __exit__(self, *_args):
        return False

    def read(self, _limit):
        return self.payload


class HeartbeatLayoutCacheTests(unittest.TestCase):
    def test_single_empty_config_response_preserves_last_good_layout(self):
        with tempfile.TemporaryDirectory() as temp:
            cache = Path(temp)
            good = {
                'client_id': 'client-1',
                'display_index': 0,
                'assigned_layout_id': 'layout-1',
                'layout': {'id': 'layout-1', 'widgets': []},
                'orientation': 'normal',
            }
            empty = {
                'client_id': 'client-1',
                'display_index': 0,
                'assigned_layout_id': None,
                'layout': None,
                'orientation': 'normal',
            }
            responses = [_Response(good), _Response(empty)]
            with mock.patch.object(heartbeat, 'urlopen_ripley', side_effect=responses):
                heartbeat.fetch_config('https://server.example:3000', 'client-1', cache)
                heartbeat.fetch_config('https://server.example:3000', 'client-1', cache)
            layout_path = cache / 'assigned-layout-display-0.json'
            self.assertTrue(layout_path.exists())
            self.assertEqual(json.loads(layout_path.read_text())['id'], 'layout-1')
            self.assertEqual(
                json.loads((cache / 'client-config.json').read_text())['assigned_layout_id'],
                'layout-1',
            )

    def test_confirmed_unassignment_clears_after_grace_period(self):
        with tempfile.TemporaryDirectory() as temp:
            cache = Path(temp)
            good = {
                'client_id': 'client-1',
                'display_index': 0,
                'assigned_layout_id': 'layout-1',
                'layout': {'id': 'layout-1', 'widgets': []},
            }
            empty = {
                'client_id': 'client-1',
                'display_index': 0,
                'assigned_layout_id': None,
                'layout': None,
            }
            responses = [_Response(good), _Response(empty), _Response(empty)]
            with mock.patch.object(heartbeat, 'urlopen_ripley', side_effect=responses):
                heartbeat.fetch_config('https://server.example:3000', 'client-1', cache)
                with mock.patch.object(heartbeat.time, 'time', return_value=1000):
                    heartbeat.fetch_config('https://server.example:3000', 'client-1', cache)
                with mock.patch.object(heartbeat.time, 'time', return_value=1000 + heartbeat.LAYOUT_UNASSIGNED_GRACE_SECONDS + 1):
                    heartbeat.fetch_config('https://server.example:3000', 'client-1', cache)
            self.assertFalse((cache / 'assigned-layout-display-0.json').exists())


if __name__ == '__main__':
    unittest.main()
