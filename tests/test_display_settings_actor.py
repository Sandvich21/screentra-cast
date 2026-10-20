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
    def test_display_power_uses_wayland_dpms_for_selected_connector(self):
        calls = []
        original_connector = actor._preview_connector
        original_run_list = actor._run_list
        original_env = actor._preview_wayland_env
        actor._preview_connector = lambda cache_dir, display_index: 'DP-2'
        actor._preview_wayland_env = lambda: {'XDG_RUNTIME_DIR': '/run/board', 'WAYLAND_DISPLAY': 'wayland-1'}
        actor._run_list = lambda argv, **kwargs: (calls.append((argv, kwargs)) or (0, '', ''))
        try:
            result = actor.execute(
                'display_power',
                {'state': 'off', 'display_index': 0},
                {'cache_dir': '/tmp/non-hardware-test-cache', 'server': None, 'client_id': 'client-a'},
            )
        finally:
            actor._preview_connector = original_connector
            actor._run_list = original_run_list
            actor._preview_wayland_env = original_env
        self.assertTrue(result['ok'])
        self.assertEqual(result['details']['connector'], 'DP-2')
        wlr_calls = [call for call in calls if call[0][0] == 'wlr-randr']
        self.assertEqual(wlr_calls[0][0], ['wlr-randr', '--output', 'DP-2', '--off'])
        self.assertEqual(wlr_calls[0][1]['env']['WAYLAND_DISPLAY'], 'wayland-1')

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