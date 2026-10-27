#!/usr/bin/env python3
import importlib.util
import os
import socket
import tempfile
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
    def test_multi_display_config_fetch_uses_client_token(self):
        calls = []
        original_urlopen = actor.urlopen_ripley
        original_read = actor._read_bounded_response
        original_fetch_config = actor.fetch_config
        class Response:
            status = 200
            def __enter__(self):
                return self
            def __exit__(self, exc_type, exc_value, traceback):
                return False
        with tempfile.TemporaryDirectory() as tmp:
            cache_dir = Path(tmp) / 'cache'
            cache_dir.mkdir()
            token = 't' * 32
            (cache_dir.parent / 'client-auth-token').write_text(token)
            actor.urlopen_ripley = lambda request, **kwargs: (calls.append(request) or Response())
            actor._read_bounded_response = lambda response, maximum: '[{"display_index": 0}]'
            actor.fetch_config = lambda *args, **kwargs: True
            try:
                self.assertTrue(actor.fetch_all_display_configs('https://server.example', 'client-a', cache_dir))
            finally:
                actor.urlopen_ripley = original_urlopen
                actor._read_bounded_response = original_read
                actor.fetch_config = original_fetch_config
        self.assertEqual(len(calls), 1)
        headers = {key.lower(): value for key, value in calls[0].header_items()}
        self.assertEqual(headers.get('x-screentra-client-token'), token)

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

    def test_display_power_on_uses_sway_dpms_wake_for_selected_connector(self):
        calls = []
        original_connector = actor._preview_connector
        original_run_list = actor._run_list
        original_env = actor._preview_wayland_env
        original_which = actor.shutil.which
        actor._preview_connector = lambda cache_dir, display_index: 'DP-2'
        actor._preview_wayland_env = lambda: {
            'XDG_RUNTIME_DIR': '/run/board',
            'WAYLAND_DISPLAY': 'wayland-1',
            'SWAYSOCK': '/run/board/sway-ipc.1000.1820.sock',
        }
        actor.shutil.which = lambda name: '/usr/bin/swaymsg' if name == 'swaymsg' else None
        actor._run_list = lambda argv, **kwargs: (calls.append((argv, kwargs)) or (0, '', ''))
        try:
            result = actor.execute(
                'display_power',
                {'state': 'on', 'display_index': 0},
                {'cache_dir': '/tmp/non-hardware-test-cache', 'server': None, 'client_id': 'client-a'},
            )
        finally:
            actor._preview_connector = original_connector
            actor._run_list = original_run_list
            actor._preview_wayland_env = original_env
            actor.shutil.which = original_which
        self.assertTrue(result['ok'])
        self.assertEqual(result['details']['method'], 'wlr-randr+sway-dpms')
        self.assertEqual(calls[0][0], ['wlr-randr', '--output', 'DP-2', '--on'])
        self.assertEqual(calls[1][0], [
            'swaymsg', '-s', '/run/board/sway-ipc.1000.1820.sock',
            'output', 'DP-2', 'power', 'on',
        ])

    def test_output_scale_uses_selected_connector_and_restarts_kiosk(self):
        calls = []
        original_connector = actor._preview_connector
        original_env = actor._preview_wayland_env
        original_run_list = actor._run_list
        original_restart = actor.restart_system_kiosk
        actor._preview_connector = lambda cache_dir, display_index: 'HDMI-A-1'
        actor._preview_wayland_env = lambda: {
            'XDG_RUNTIME_DIR': '/run/board',
            'WAYLAND_DISPLAY': 'wayland-1',
        }
        actor._run_list = lambda argv, **kwargs: (calls.append((argv, kwargs)) or (0, '', ''))
        actor.restart_system_kiosk = lambda: (True, 'restarted')
        try:
            result = actor.apply_display_scale(Path('/tmp/cache'), 0, 1.25)
        finally:
            actor._preview_connector = original_connector
            actor._preview_wayland_env = original_env
            actor._run_list = original_run_list
            actor.restart_system_kiosk = original_restart
        self.assertTrue(result[0])
        self.assertEqual(calls[0][0], ['wlr-randr', '--output', 'HDMI-A-1', '--scale', '1.25'])
        self.assertTrue(result[2]['kiosk_restart'])

    def test_output_scale_rejects_values_outside_safe_range(self):
        for value in (0.49, 4.01, float('nan'), True, '1.25'):
            with self.subTest(value=value):
                ok, _message, details = actor.apply_display_scale(Path('/tmp/cache'), 0, value)
                self.assertFalse(ok)
                self.assertEqual(details, {})

    def test_wayland_env_ignores_stale_sway_ipc_socket(self):
        with tempfile.TemporaryDirectory() as runtime:
            stale_path = Path(runtime) / 'sway-ipc.1001.1067.sock'
            live_path = Path(runtime) / 'sway-ipc.1001.1820.sock'
            stale = socket.socket(socket.AF_UNIX)
            live = socket.socket(socket.AF_UNIX)
            stale.bind(str(stale_path))
            live.bind(str(live_path))
            os.utime(stale_path, ns=(2_000_000_000, 2_000_000_000))
            os.utime(live_path, ns=(1_000_000_000, 1_000_000_000))
            calls = []
            original_env = actor.os.environ.copy()
            original_run_list = actor._run_list
            original_which = actor.shutil.which
            try:
                actor.os.environ.clear()
                actor.os.environ.update({'XDG_RUNTIME_DIR': runtime})
                actor.shutil.which = lambda name: '/usr/bin/swaymsg' if name == 'swaymsg' else None
                actor._run_list = lambda argv, **kwargs: (
                    calls.append(argv) or (0 if argv[2] == str(live_path) else 1, '', '')
                )
                env = actor._preview_wayland_env()
            finally:
                actor.os.environ.clear()
                actor.os.environ.update(original_env)
                actor._run_list = original_run_list
                actor.shutil.which = original_which
                stale.close()
                live.close()
        self.assertEqual(env['SWAYSOCK'], str(live_path))
        self.assertEqual(len(calls), 2)

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