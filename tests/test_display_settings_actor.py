#!/usr/bin/env python3
import importlib.util
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location(
    'screentra_player_actor_display_settings',
    ROOT / 'player-client/scripts/screentra-player-actor.py',
)
actor = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
SPEC.loader.exec_module(actor)


class DisplaySettingsActorTests(unittest.TestCase):
    def test_pending_display_settings_rejects_arbitrary_fields_and_vcp(self):
        valid = {
            'client_id': 'client-a',
            'action': 'display_settings',
            'payload': {'display_index': 1, 'brightness': 40, 'contrast': 60},
        }
        self.assertEqual(actor.validate_pending_command(valid, 'client-a'), valid)
        for payload in (
            {'display_index': 1, 'brightness': 40, 'vcp': 'D6'},
            {'display_index': 1, 'brightness': 40, 'shell': 'ddcutil setvcp D6 1'},
            {'display_index': 1, 'brightness': 40, 'contrast': 60, 'bus': '1'},
            {'display_index': 1},
            {'display_index': 1, 'brightness': 101},
        ):
            with self.assertRaises(ValueError):
                actor.validate_pending_command({**valid, 'payload': payload}, 'client-a')

    def test_display_settings_returns_structured_partial_failure_without_hardware(self):
        calls = []
        original = actor.apply_ddc_control
        actor.apply_ddc_control = lambda vcp, value, **kwargs: (calls.append((vcp, value)) or (value != 60, f'vcp {vcp}'))
        try:
            result = actor.execute(
                'display_settings',
                {'display_index': 1, 'brightness': 40, 'contrast': 60},
                {'cache_dir': '/tmp/non-hardware-test-cache', 'server': None, 'client_id': 'client-a'},
            )
        finally:
            actor.apply_ddc_control = original
        self.assertFalse(result['ok'])
        self.assertEqual(result['details']['brightness']['ok'], True)
        self.assertEqual(result['details']['contrast']['ok'], False)
        self.assertEqual(calls, [('10', 40), ('12', 60)])


if __name__ == '__main__':
    unittest.main()