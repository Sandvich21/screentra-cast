#!/usr/bin/env python3
import importlib.util
import tempfile
import unittest
from pathlib import Path
from unittest import mock

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

    def test_login_payload_requires_bounded_strings(self):
        with self.assertRaises(ValueError):
            renderer._validate_login_payload({'username': ['admin'], 'password': 'secret'})
        with self.assertRaises(ValueError):
            renderer._validate_login_payload({'username': 'admin', 'password': 'x' * 257})

    def test_request_field_validation_rejects_unknown_fields(self):
        with self.assertRaises(ValueError):
            renderer._require_request_fields({'confirm': True, 'extra': 1}, {'confirm'})

    def test_request_json_rejects_duplicate_keys(self):
        with self.assertRaises(ValueError):
            renderer._parse_request_json('{"action":"one","action":"two"}')

    def test_safe_run_bounds_live_output(self):
        result = renderer._safe_run(
            [renderer.sys.executable, '-c', 'import sys; sys.stdout.write("x" * 20000)']
        )
        self.assertFalse(result['ok'])
        self.assertIn('stdout exceeds its bound', result['stderr'])

    def test_renderer_json_rejects_nonstandard_values_and_duplicate_cache_keys(self):
        with self.assertRaises(ValueError):
            renderer._parse_request_json('{"value":NaN}')
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / 'cache.json'
            path.write_text('{"safe":1,"safe":2}', encoding='utf-8')
            self.assertEqual(renderer._read_json_file(path, {'fallback': True}), {'fallback': True})

    def test_local_admin_routes_are_disabled(self):
        source = renderer.PLAYER_HTML
        self.assertFalse(renderer.LOCAL_ADMIN_ENABLED)
        self.assertIn("path.startswith('/api/admin')", Path(MODULE_PATH).read_text())

    def test_cached_calendar_uses_published_layout_widget_proxy(self):
        source = renderer.PLAYER_HTML
        self.assertIn("function loadCalendar(p,layoutId,widgetId)", source)
        self.assertIn("/api/ics-proxy?layout_id=", source)
        self.assertIn("loadCalendar(p,w._layout_id||'',w.id)", source)
        self.assertNotIn("/api/ics-proxy?url=", source)

    def test_renderer_starts_normal_player_without_a_startup_gate(self):
        source = Path(MODULE_PATH).read_text(encoding='utf-8')
        self.assertNotIn('STARTUP_STATUS_HTML', source)
        self.assertNotIn('STARTUP_LAYOUT_SECONDS', source)
        self.assertNotIn('/api/startup-status.json', source)
        self.assertNotIn('Starting display', source)
        self.assertNotIn('Normal signage starts in', source)

    def test_live_redirect_requires_online_assignment(self):
        with tempfile.TemporaryDirectory() as temp:
            cache = Path(temp)
            (cache / 'last-heartbeat.json').write_text(
                '{"connection":"offline","assigned_layout_id":"layout-1"}',
                encoding='utf-8',
            )
            self.assertFalse(renderer._heartbeat_has_live_assignment(cache))
            (cache / 'last-heartbeat.json').write_text(
                '{"connection":"online","assigned_layout_id":null}',
                encoding='utf-8',
            )
            self.assertFalse(renderer._heartbeat_has_live_assignment(cache))
            (cache / 'last-heartbeat.json').write_text(
                '{"connection":"online","assigned_layout_id":"layout-1"}',
                encoding='utf-8',
            )
            self.assertTrue(renderer._heartbeat_has_live_assignment(cache))

    def test_log_tail_rejects_symlinked_files(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            outside = root / 'outside.log'
            outside.write_text('secret log', encoding='utf-8')
            redirected = root / 'service.log'
            redirected.symlink_to(outside)
            self.assertNotIn('secret log', renderer._read_tail(redirected))

    def test_touch_binding_rejects_undetected_device_paths(self):
        with tempfile.TemporaryDirectory() as temp:
            cache = Path(temp) / 'cache'
            cache.mkdir()
            renderer.Handler.cache_dir = cache
            with mock.patch.object(renderer, '_safe_run', return_value={'ok': True}), \
                    mock.patch.object(renderer, '_display_connectors_for_indexes', return_value=[]):
                with self.assertRaises(ValueError):
                    renderer._write_touch_binding({'device_path': '/dev/not-a-detected-touchscreen'})

    def test_backup_root_rejects_symlinked_directory(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            outside = root / 'outside'
            outside.mkdir()
            cache = root / 'cache'
            cache.mkdir()
            (root / 'backups').symlink_to(outside, target_is_directory=True)
            renderer.Handler.cache_dir = cache
            with self.assertRaises(ValueError):
                renderer._client_backup_root()

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

    def test_cached_template_matches_server_text_and_weather_alert_semantics(self):
        source = renderer.PLAYER_HTML
        self.assertIn("font-family:${cachedFontFamily(q)};", source)
        self.assertIn("inner=esc(q.content||q.text||'Text')", source)
        self.assertIn("k==='weather-alert-code'", source)
        self.assertIn("weatherAlertColorCodeHtml(wx.alerts)", source)
        self.assertIn("if(!items.length)return ''", source)
        self.assertIn("alerts:alerts", source)
        self.assertIn("@font-face{font-family:'Creepster'", source)
        self.assertIn("@font-face{font-family:'Nosifer'", source)
        self.assertIn("container-type:size", source)
        self.assertIn("raw.includes('creepster')", source)
        self.assertIn("return \"'Creepster',sans-serif\"", source)
        self.assertIn("t==='traffic'", source)
        self.assertIn("cachedTrafficRelevantEvents", source)
        self.assertIn("const outputWidget=w;", source)
        self.assertIn("e.type==='image'", source)
        self.assertNotIn("Math.max(Number(w.z)||1,100)", source)
        self.assertIn("data-date-format", source)
        self.assertIn("ss:seconds", source)
        self.assertIn("/api/traffic/events", source)
        self.assertIn("async function loadTraffic", source)
        self.assertIn("cachedTrafficEvents(p)", source)
        self.assertIn("source==='traffic'", source)
        self.assertIn("cachedOpacity(p.opacity)", source)


if __name__ == '__main__':
    unittest.main()
