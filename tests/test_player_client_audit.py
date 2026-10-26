#!/usr/bin/env python3
import importlib.util
import stat
import subprocess
import sys
import tempfile
import unittest
from unittest import mock
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def load_module(path: Path, name: str):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    spec.loader.exec_module(module)
    return module


actor = load_module(ROOT / 'player-client/scripts/screentra-player-actor.py', 'screentra_player_actor_audit')
builder = load_module(ROOT / 'player-client/scripts/build-installer.py', 'screentra_build_installer_audit')
renderer = load_module(ROOT / 'player-client/scripts/screentra-player-renderer.py', 'screentra_player_renderer_audit')


class PlayerClientAuditTests(unittest.TestCase):
    def test_actor_config_fetch_sends_enrolled_client_token(self):
        source = (ROOT / 'player-client/scripts/screentra-player-actor.py').read_text(encoding='utf-8')
        start = source.index('def fetch_config(')
        end = source.index('\ndef fetch_all_display_configs(', start)
        block = source[start:end]
        self.assertIn("token = _preview_token(cache_dir)", block)
        self.assertIn("'X-SCREENTRA-CLIENT-TOKEN': token", block)
        self.assertIn('urllib.request.Request(', block)

    def test_installer_source_reads_are_bounded(self):
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / 'oversized-source'
            path.write_bytes(b'x' * 33)
            with self.assertRaisesRegex(ValueError, 'exceeds 32 bytes'):
                builder.read_bounded_text(path, 32)

    def test_installer_output_drift_is_detectable(self):
        with tempfile.TemporaryDirectory() as temp:
            output = Path(temp) / 'player-client.sh'
            output.write_bytes(b'rendered')
            self.assertTrue(builder.output_matches('rendered', output))
            self.assertFalse(builder.output_matches('changed', output))

    def test_canonical_installer_bounds_saved_server_reads(self):
        canonical = (ROOT / 'player-client/install/player-client.sh').read_text()
        self.assertIn('MAX_SERVER_URL_BYTES=2048', canonical)
        self.assertIn('read_bounded_file', canonical)
        self.assertIn('CLIENT_ID_FILE" 256', canonical)
        self.assertIn('persisted client ID is missing or invalid', canonical)

    def test_embedded_updater_local_reads_are_bounded(self):
        canonical = (ROOT / 'player-client/install/player-client.sh').read_text()
        start = canonical.index("<<'PY_UPDATER'\n") + len("<<'PY_UPDATER'\n")
        end = canonical.index('\nPY_UPDATER\n', start)
        namespace = {'__name__': 'embedded_updater_test'}
        body = canonical[start:end]
        self.assertNotIn('capture_output=True', body)
        exec(compile(body, 'embedded-updater.py', 'exec'), namespace)
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / 'oversized'
            path.write_bytes(b'x' * 17)
            with self.assertRaisesRegex(RuntimeError, 'local update file'):
                namespace['bounded_read_text'](path, 16)
            with self.assertRaisesRegex(RuntimeError, 'local update file'):
                namespace['bounded_read_file'](path, 16)

    def test_kiosk_and_status_server_reads_are_bounded(self):
        installer = (ROOT / 'player-client/install/player-client.sh').read_text()
        kiosk = (ROOT / 'player-client/install/screentra-kiosk.sh').read_text()
        self.assertNotIn('cat "$INSTALL_DIR/server"', installer)
        self.assertNotIn("pathlib.Path(sys.argv[1]).read_text().strip()", installer)
        self.assertNotIn("pathlib.Path(sys.argv[1]).read_text().strip()", kiosk)
        self.assertNotIn('cfg = json.loads(path.read_text())', kiosk)
        self.assertNotIn('raw = path.read_bytes()', kiosk)
        self.assertNotIn('json.load(open(sys.argv[1]))', kiosk)
        self.assertNotIn('d=json.load(open("$CONFIG"))', installer)
        self.assertNotIn('json.load(open(sys.argv[1]))', installer)
        self.assertNotIn('python3 -c \'import sys; raise SystemExit(0 if "Screentra Cast Display" in sys.stdin.read()', kiosk)
        self.assertIn('head -c 1048577', kiosk)
        self.assertIn('head -c 65537', kiosk)
        self.assertNotIn('head -c 2049 "$CLIENT_DIR/server"', kiosk)
        self.assertIn('BROWSER_URL="http://127.0.0.1:${port}/"', kiosk)
        self.assertIn('capability exchange before redirecting Chromium', kiosk)
        self.assertIn('--max-filesize 65536 "$RENDERER_URL/health"', installer)
        self.assertNotIn('python3 -c \'import sys; raise SystemExit(0 if "Screentra Cast Live Player" in sys.stdin.read()', installer)
        self.assertIn('head -c 65537', installer)

    def test_embedded_heartbeat_reads_are_bounded(self):
        installer = (ROOT / 'player-client/install/player-client.sh').read_text()
        start = installer.index("cat > \"$HEARTBEAT\" <<'PY'\n") + len("cat > \"$HEARTBEAT\" <<'PY'\n")
        end = installer.index('\nPY\nchmod +x "$HEARTBEAT"', start)
        body = installer[start:end]
        compile(body, 'embedded-heartbeat.py', 'exec')
        self.assertIn('_read_bounded_response', body)
        self.assertNotIn('r.read().decode()', body)
        self.assertNotIn('Path(path).read_text()', body)
        self.assertNotIn("open('/proc/uptime').read()", body)
        self.assertNotIn("open('/proc/stat').readline()", body)

    def test_embedded_renderer_file_reads_are_bounded(self):
        installer = (ROOT / 'player-client/install/player-client.sh').read_text()
        start = installer.index("cat > \"$RENDERER\" <<'PY_RENDERER'\n") + len("cat > \"$RENDERER\" <<'PY_RENDERER'\n")
        end = installer.index('\nPY_RENDERER\nchmod +x "$RENDERER"', start)
        body = installer[start:end]
        compile(body, 'embedded-renderer.py', 'exec')
        self.assertIn('_read_bounded_path', body)
        self.assertIn('response.read(256 * 1024 + 1)', body)
        self.assertNotIn('.read_text()', body)
        self.assertNotIn('target.read_bytes()', body)
        self.assertNotIn('capture_output=True', body)
        self.assertNotIn('_ur.urlopen(url, context=ctx, timeout=10).read()', body)
        self.assertNotIn("sock.connect(('11.1.1.53', 3000))", body)
        self.assertIn('def _lan_ip_for_admin(server_url', body)
        namespace = {'__name__': 'embedded_renderer_test'}
        exec(compile(body, 'embedded-renderer.py', 'exec'), namespace)
        with self.assertRaisesRegex(ValueError, 'stdout exceeds its bound'):
            namespace['_bounded_run']([sys.executable, '-c', "print('x' * 13000)"], timeout=10)

    def test_embedded_renderer_browser_reads_are_bounded(self):
        installer = (ROOT / 'player-client/static/install/player-client.sh').read_text()
        marker = "PY_RENDERER'" + chr(10)
        start = installer.index(marker) + len(marker)
        end = installer.index(chr(10) + 'PY_RENDERER' + chr(10) + 'chmod +x "$RENDERER"', start)
        body = installer[start:end]
        html_start = body.index("PLAYER_HTML = r'''")
        html_end = body.index("'''", html_start + len("PLAYER_HTML = r'''"))
        browser = body[html_start:html_end]
        self.assertIn('readTextBounded', browser)
        self.assertIn('reader.cancel()', browser)
        self.assertNotIn('await r.text()', browser)
        self.assertNotIn('await r.json()', browser)
        self.assertNotIn('await c.json()', browser)

    def test_embedded_admin_login_browser_reads_are_bounded(self):
        source = (ROOT / 'player-client/scripts/screentra-player-renderer.py').read_text()
        self.assertIn('readJsonBounded', source)
        self.assertIn('await reader.cancel()', source)
        self.assertNotIn('await r.json()', source)

    def test_no_layout_admin_url_uses_local_origin_fallback(self):
        paths = [
            ROOT / 'static/js/no-layout.js',
            ROOT / 'player-client/static/no-layout.html',
            ROOT / 'player-client/scripts/no-layout.html',
        ]
        for path in paths:
            source = path.read_text()
            self.assertNotIn('11.1.1.54:3910', source)
            self.assertIn("new URL(window.location.href)", source)

    def test_local_admin_mutations_require_same_origin_when_origin_is_sent(self):
        same = {'Host': '127.0.0.1:3910', 'Origin': 'http://127.0.0.1:3910'}
        cross = {'Host': '127.0.0.1:3910', 'Origin': 'http://evil.example'}
        self.assertTrue(renderer._csrf_allows_mutation(same))
        self.assertFalse(renderer._csrf_allows_mutation(cross))
        self.assertTrue(renderer._csrf_allows_mutation({'Host': '127.0.0.1:3910'}))

    def test_player_capability_redirect_uses_fragment_not_query(self):
        html = renderer._live_player_redirect_html(
            'https://server.example:3000', 0, 'DP-2', 'client-1', 'A' * 128,
            'http://127.0.0.1:3900/'
        )
        self.assertIn('#player_capability=', html)
        self.assertIn('renderer_url=', html)
        self.assertNotIn('?player_capability=', html)

    def test_player_refreshes_expired_capability_through_local_renderer(self):
        source = (ROOT / 'static/js/player.js').read_text()
        self.assertIn("const PLAYER_RENDERER_URL = PLAYER_HASH_PARAMS.get('renderer_url')", source)
        self.assertIn("response.status === 401", source)
        self.assertIn("screentra-capability-refresh", source)
        self.assertIn("window.location.replace(PLAYER_RENDERER_URL)", source)

    def test_layout_change_marker_is_written_before_kiosk_restart(self):
        source = (ROOT / 'player-client/scripts/screentra-player-heartbeat.py').read_text()
        marker = source.index("if cfg and current_layout_id and current_layout_id != previous_layout_id:")
        block = source[marker:source.index('        elif cfg and current_layout_id:', marker)]
        self.assertLess(
            block.index('_atomic_write_text(prev_layout_path'),
            block.index('subprocess.run'),
        )
        self.assertIn('_read_real_file(prev_layout_path, 4096)', source)
        self.assertIn("cached_previous = _read_json_file(Path(args.cache_dir) / 'client-config.json', {})", source)
        self.assertNotIn('_read_bounded_text(prev_layout_path', source)

    def test_display_override_modal_escapes_connector_html(self):
        source = (ROOT / 'static/js/app.js').read_text()
        self.assertIn('Set Display Size: ${esc(connector)}', source)
        self.assertNotIn('Set Display Size: ${connector}', source)

    def test_installer_examples_do_not_use_private_deployment_addresses(self):
        for path in (
            ROOT / 'player-client/install/player-client.sh',
            ROOT / 'player-client/static/install/player-client.sh',
            ROOT / 'static/install/player-client.sh',
        ):
            source = path.read_text(encoding='utf-8')
            self.assertNotIn('10.1.1.76', source)
            self.assertIn('https://server.example:3000', source)

    def test_served_player_installer_matches_generated_installer(self):
        generated = (ROOT / 'player-client/static/install/player-client.sh').read_bytes()
        served = (ROOT / 'static/install/player-client.sh').read_bytes()
        self.assertEqual(served, generated)

    def test_renderer_admin_route_lookup_uses_configured_server(self):
        source = (ROOT / 'player-client/scripts/screentra-player-renderer.py').read_text()
        self.assertIn('def _lan_ip_for_admin(server_url', source)
        self.assertNotIn("sock.connect(('11.1.1.53', 3000))", source)

    def test_editor_json_reads_are_bounded(self):
        source = (ROOT / 'static/js/editor.js').read_text()
        self.assertIn('readEditorJsonBounded', source)
        self.assertNotIn('await response.json()', source)
        self.assertNotIn('await r.json()', source)

    def test_api_browser_json_reads_are_bounded(self):
        source = (ROOT / 'static/js/api.js').read_text()
        self.assertIn('_readApiJsonBounded', source)
        self.assertIn('reader.cancel()', source)
        self.assertNotIn('.json()', source)
        self.assertNotIn('.text()', source)

    def test_app_browser_reads_are_bounded(self):
        source = (ROOT / 'static/js/app.js').read_text()
        self.assertIn('readAppJsonBounded', source)
        self.assertIn('readAppTextBounded', source)
        self.assertIn('readAppBlobBounded', source)
        self.assertIn('reader.cancel()', source)
        self.assertNotRegex(source, r'\b(?:response|res|r|fileRes|assetRes)\.(?:json|text|blob)\(\)')

    def test_auth_browser_reads_are_bounded(self):
        source = (ROOT / 'static/js/auth.js').read_text()
        self.assertIn('readAuthJsonBounded', source)
        self.assertIn('reader.cancel()', source)
        self.assertNotRegex(source, r'\b(?:response|res)\.(?:json|text)\(\)')

    def test_news_template_browser_reads_are_bounded(self):
        source = (ROOT / 'static/js/news-template.js').read_text()
        self.assertIn('readNewsJsonBounded', source)
        self.assertIn('reader.cancel()', source)
        self.assertNotIn('.json()', source)

    def test_no_layout_js_browser_reads_are_bounded(self):
        source = (ROOT / 'static/js/no-layout.js').read_text()
        self.assertIn('readNoLayoutJsonBounded', source)
        self.assertIn('reader.cancel()', source)
        self.assertNotRegex(source, r'\br\.(?:json|text)\(\)')

    def test_legacy_no_layout_browser_reads_are_bounded(self):
        source = (ROOT / 'player-client/scripts/no-layout.html').read_text()
        self.assertIn('readNoLayoutJsonBounded', source)
        self.assertIn('reader.cancel()', source)
        self.assertNotIn('await r.json()', source)
        self.assertNotIn('await r.text()', source)

    def test_weather_browser_reads_are_bounded(self):
        source = (ROOT / 'static/js/weather.js').read_text()
        self.assertIn('readWeatherJsonBounded', source)
        self.assertIn('reader.cancel()', source)
        self.assertNotIn('response.json()', source)

    def test_calendar_browser_reads_are_bounded(self):
        source = (ROOT / 'static/js/calendar.js').read_text()
        self.assertIn('readCalendarTextBounded', source)
        self.assertIn('reader.cancel()', source)
        self.assertNotIn('.json()', source)
        self.assertNotIn('.text()', source)

    def test_calendar_error_messages_do_not_use_raw_html(self):
        source = (ROOT / 'static/js/calendar.js').read_text()
        self.assertIn('textContent', source)
        self.assertNotIn("' + e.message + '</div>'", source)

    def test_player_browser_reads_are_bounded(self):
        source = (ROOT / 'static/js/player.js').read_text()
        self.assertIn('readPlayerJsonBounded', source)
        self.assertIn('reader.cancel()', source)
        self.assertNotIn('.json()', source)
        self.assertNotIn('.text()', source)

    def test_bundle_install_commands_use_current_origin(self):
        source = (ROOT / 'static/js/app.js').read_text()
        self.assertIn('window.location.origin', source)
        self.assertNotIn('https://11.1.1.53:3000${r.bootstrap_url}', source)

    def test_player_slideshow_errors_do_not_use_raw_html(self):
        source = (ROOT / 'static/js/player.js').read_text()
        self.assertIn('error.textContent', source)
        self.assertIn('Slideshow error:', source)
        self.assertNotIn("'Slideshow error: '+e.message", source)

    def test_preview_browser_reads_are_bounded(self):
        source = (ROOT / 'static/js/preview.js').read_text()
        self.assertIn('readPreviewJsonBounded', source)
        self.assertIn('reader.cancel()', source)
        self.assertNotIn('.json()', source)
        self.assertNotIn('.text()', source)

    def test_font_picker_browser_reads_are_bounded(self):
        source = (ROOT / 'static/js/font-picker.js').read_text()
        self.assertIn('readJsonBounded', source)
        self.assertIn('reader.cancel()', source)
        self.assertNotIn('await r.text()', source)
        self.assertNotIn('await r.json()', source)

    def test_no_layout_browser_reads_are_bounded(self):
        source = (ROOT / 'player-client/static/no-layout.html').read_text()
        self.assertIn('readTextBounded', source)
        self.assertIn('reader.cancel()', source)
        self.assertNotIn('await r.json()', source)
        self.assertNotIn('await r.text()', source)

    def test_embedded_gpu_health_commands_are_bounded(self):
        installer = (ROOT / 'player-client/install/player-client.sh').read_text()
        start = installer.index("cat > \"$GPU_HEALTH\" <<'PY_GPU_HEALTH'\n") + len("cat > \"$GPU_HEALTH\" <<'PY_GPU_HEALTH'\n")
        end = installer.index('\nPY_GPU_HEALTH\nchmod +x "$GPU_HEALTH"', start)
        body = installer[start:end]
        namespace = {'__name__': 'embedded_gpu_health_test'}
        exec(compile(body, 'embedded-gpu-health.py', 'exec'), namespace)
        rc, out, err = namespace['_run']([sys.executable, '-c', "print('x' * 70000)"], timeout=10)
        self.assertEqual(rc, 127)
        self.assertEqual(out, '')
        self.assertIn('subprocess output exceeded its limit', err)

    def test_embedded_actor_responses_and_updates_are_bounded(self):
        installer = (ROOT / 'player-client/install/player-client.sh').read_text()
        start = installer.index("cat > \"$ACTOR\" <<'PY_ACTOR'\n") + len("cat > \"$ACTOR\" <<'PY_ACTOR'\n")
        end = installer.index('\nPY_ACTOR\nchmod +x "$ACTOR"', start)
        body = installer[start:end]
        compile(body, 'embedded-actor.py', 'exec')
        self.assertIn('_read_bounded_response', body)
        self.assertIn('MAX_UPDATE_INSTALLER_BYTES', body)
        self.assertNotIn('data = r.read()', body)
        self.assertNotIn('signature = r.read()', body)
        self.assertNotIn('json.loads(r.read().decode())', body)
        self.assertNotIn('pending_path.read_text', body)
        self.assertNotIn('subprocess.run(args, capture_output=True', body)
        namespace = {'__name__': 'embedded_actor_test'}
        exec(compile(body, 'embedded-actor.py', 'exec'), namespace)
        rc, out, err = namespace['_run_list'](
            [sys.executable, '-c', "print('x' * 70000)"],
            timeout=10,
        )
        self.assertEqual(rc, 1)
        self.assertEqual(out, '')
        self.assertIn('output exceeds its bound', err)

    def test_preview_frame_is_bounded(self):
        frame = b'x' * (actor.MAX_PREVIEW_FRAME_BYTES + 1)
        with self.assertRaises(ValueError):
            actor._validate_preview_frame(frame)
        self.assertEqual(actor._validate_preview_frame(b'png'), b'png')
        with self.assertRaises(ValueError):
            actor._require_bounded_text('x' * (actor.MAX_WLR_JSON_BYTES + 1), actor.MAX_WLR_JSON_BYTES)

    def test_run_list_bounds_both_streams(self):
        command = [actor.sys.executable, '-c', 'import sys; sys.stdout.write("x" * 100000)']
        rc, stdout, stderr = actor._run_list(command, timeout=3)
        self.assertEqual(rc, 1)
        self.assertEqual(stdout, '')
        self.assertIn('bound', stderr)

    def test_bounded_check_output_limits_process_output(self):
        command = [actor.sys.executable, '-c', 'import sys; sys.stdout.write("x" * 100)']
        with self.assertRaises(ValueError):
            actor._bounded_check_output(command, 32, 3)
        self.assertEqual(
            actor._bounded_check_output(
                [actor.sys.executable, '-c', 'print("ok")'], 32, 3
            ).strip(),
            'ok',
        )

    def test_bounded_capture_stdout_limits_preview_output(self):
        command = [actor.sys.executable, '-c', 'import sys; sys.stdout.buffer.write(b"x" * 100)']
        with self.assertRaises(ValueError):
            actor._bounded_capture_stdout(command, 32, 3)
        code, output = actor._bounded_capture_stdout(
            [actor.sys.executable, '-c', 'import sys; sys.stdout.buffer.write(b"ok")'], 32, 3
        )
        self.assertEqual((code, output), (0, b'ok'))

    def test_preview_pids_are_bounded(self):
        self.assertEqual(actor._validate_preview_pid('1234'), 1234)
        with self.assertRaises(ValueError):
            actor._validate_preview_pid('0')
        with self.assertRaises(ValueError):
            actor._validate_preview_pid('9' * 20)

    def test_probe_failure_cleanup_uses_bounded_waits(self):
        for relative in (
            'player-client/scripts/screentra-player-heartbeat.py',
            'player-client/scripts/screentra-player-preflight.py',
            'player-client/scripts/screentra-gpu-health.py',
        ):
            source = (ROOT / relative).read_text()
            self.assertNotIn('process.wait()\\n', source, relative)
            self.assertIn('process.wait(timeout=1)', source, relative)

    def test_preview_process_termination_reaps_child(self):
        process = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(30)'])
        try:
            self.assertTrue(actor._terminate_preview_process(process.pid, timeout=2))
            self.assertIsNotNone(process.poll())
            process.wait(timeout=1)
        finally:
            if process.poll() is None:
                process.kill()
                process.wait()

    def test_display_indexes_are_bounded(self):
        self.assertEqual(actor._validate_display_index(3), 3)
        with self.assertRaises(ValueError):
            actor._validate_display_index(-1)
        with self.assertRaises(ValueError):
            actor._validate_display_index(4)
        with self.assertRaises(ValueError):
            actor._validate_display_index(True)

    def test_bluetooth_scan_results_are_bounded(self):
        text = '\n'.join(f'Device AA:BB:CC:DD:{index // 256:02X}:{index % 256:02X} Device {index}' for index in range(130))
        self.assertLessEqual(len(actor._bluetoothctl_devices(text)), actor.MAX_BLUETOOTH_DEVICES)

    def test_audio_sink_result_count_is_bounded(self):
        text = 'Audio Sinks:\n' + '\n'.join(f'  {index}. Device {index}' for index in range(130))
        self.assertLessEqual(len(actor._wpctl_sinks(text)), actor.MAX_AUDIO_SINKS)

    def test_audio_sink_ids_are_bounded_in_status_parsing(self):
        sinks = actor._wpctl_sinks('Audio Sinks:\n  ' + ('9' * 100) + '. Device')
        self.assertEqual(sinks, [])

    def test_audio_sink_names_are_bounded(self):
        sinks = actor._wpctl_sinks('Audio Sinks:\n  1. ' + ('x' * 1000))
        self.assertEqual(len(sinks), 1)
        self.assertLessEqual(len(sinks[0]['name'].encode('utf-8')), 128)

    def test_hardware_numeric_inputs_reject_floats(self):
        with self.assertRaises(ValueError):
            actor._bounded_integer_input(5.5, 5, 30, 'duration')
        with self.assertRaises(ValueError):
            actor._bounded_integer_input(50.5, 0, 100, 'volume')

    def test_hardware_identifiers_reject_non_string_values(self):
        self.assertFalse(actor.bluetooth_connect({'address': ['AA:BB:CC:DD:EE:FF']})[0])
        self.assertFalse(actor.bluetooth_connect({'address': 'AA:BB:CC:DD:EE:FF', 'name': ['touch']})[0])
        self.assertFalse(actor.audio_set_default({'sink_id': ['1']})[0])

    def test_actor_labels_are_bounded(self):
        self.assertEqual(actor._validate_actor_label('server-command', 'requester'), 'server-command')
        with self.assertRaises(ValueError):
            actor._validate_actor_label('x' * 129, 'requester')

    def test_touch_binding_rejects_non_boolean_joined_flag(self):
        with tempfile.TemporaryDirectory() as temp:
            with self.assertRaises(ValueError):
                actor.apply_touch_binding(Path(temp) / 'cache', {'allow_joined_displays': 'yes'})

    def test_touch_device_names_are_bounded(self):
        self.assertEqual(actor._validate_touch_device_name('Panel Touch'), 'Panel Touch')
        with self.assertRaises(ValueError):
            actor._validate_touch_device_name('x' * 129)
        with self.assertRaises(ValueError):
            actor._validate_touch_device_name('touch\nname')

    def test_touch_device_paths_are_bounded(self):
        self.assertEqual(actor._validate_touch_device_path('/dev/input/event0'), '/dev/input/event0')
        with self.assertRaises(ValueError):
            actor._validate_touch_device_path('/dev/../../etc/passwd')
        with self.assertRaises(ValueError):
            actor._validate_touch_device_path('/dev/' + 'x' * 241)

    def test_display_count_rejects_malformed_values(self):
        with tempfile.TemporaryDirectory() as temp:
            ok, message, details = actor.apply_display_count(Path(temp) / 'cache', 'not-a-number')
            self.assertFalse(ok)
            self.assertIn('numeric', message)
            self.assertEqual(details, {})
            ok, message, details = actor.apply_display_count(Path(temp) / 'cache', 1.5)
        self.assertFalse(ok)
        self.assertIn('numeric', message)
        self.assertEqual(details, {})

    def test_display_preview_rejects_boolean_timing(self):
        with mock.patch.object(actor.shutil, 'which', return_value='/usr/bin/grim'), mock.patch.object(actor, '_preview_connector', return_value='HDMI-A-1'):
            ok, message, details = actor.start_display_preview({'cache_dir': '/tmp/cache', 'server': 'https://example.test', 'client_id': 'client-1'}, 0, {'interval_seconds': True, 'duration_seconds': 120})
        self.assertFalse(ok)
        self.assertIn('numeric', message)
        self.assertEqual(details, {})

    def test_display_connectors_are_bounded(self):
        self.assertEqual(actor._validate_display_connector('HDMI-A-1'), 'HDMI-A-1')
        with self.assertRaises(ValueError):
            actor._validate_display_connector('../../etc/passwd')
        with self.assertRaises(ValueError):
            actor._validate_display_connector('x' * 129)

    def test_display_power_rejects_non_string_state(self):
        result = actor.execute('display_power', {'display_index': 0, 'state': ['on']}, {'cache_dir': '/tmp/cache', 'client_id': 'client-1', 'server': 'https://example.test'})
        self.assertFalse(result['ok'])
        self.assertIn('state', result['message'].lower())

    def test_reload_layout_rejects_non_string_orientation(self):
        result = actor.execute('reload_layout', {'display_index': 0, 'orientation': []}, {'cache_dir': '/tmp/cache', 'client_id': 'client-1', 'server': 'https://example.test'})
        self.assertEqual(result['code'], 'invalid_payload')

    def test_execute_rejects_fractional_ddc_values(self):
        context = {'cache_dir': '/tmp/cache', 'client_id': 'client-1', 'server': 'https://example.test'}
        ddc_float = actor.execute('ddc_set', {'vcp': '10', 'value': 1.5}, context)
        self.assertEqual(ddc_float['code'], 'invalid_payload')

    def test_execute_rejects_malformed_hardware_payloads(self):
        context = {'cache_dir': '/tmp/cache', 'client_id': 'client-1', 'server': 'https://example.test'}
        ddc = actor.execute('ddc_set', {'vcp': '--bad', 'value': 'not-an-int'}, context)
        cec = actor.execute('cec_send', {'cmd': 'tx 0F:44:6D:00'}, context)
        self.assertEqual(ddc['code'], 'invalid_payload')
        self.assertEqual(cec['code'], 'invalid_payload')

    def test_execute_rejects_non_object_payloads(self):
        result = actor.execute('ddc_set', ['not-an-object'], {'cache_dir': '/tmp/cache', 'client_id': 'client-1', 'server': 'https://example.test'})
        self.assertFalse(result['ok'])
        self.assertEqual(result['code'], 'invalid_payload')

    def test_update_request_requires_object_payload(self):
        ok, message, details = actor.update_client('https://example.test', 'client-1', Path('/tmp/cache'), ['not-an-object'])
        self.assertFalse(ok)
        self.assertIn('payload', message.lower())
        self.assertEqual(details, {})

    def test_bluetooth_connect_requires_object_payload(self):
        ok, message, details = actor.bluetooth_connect(['not-an-object'])
        self.assertFalse(ok)
        self.assertIn('payload', message.lower())
        self.assertEqual(details, {})

    def test_bluetooth_scan_duration_is_bounded(self):
        ok, message, details = actor.bluetooth_scan({'duration_seconds': 'invalid'})
        self.assertFalse(ok)
        self.assertIn('duration', message.lower())
        self.assertEqual(details, {})

    def test_bluetooth_name_is_bounded(self):
        self.assertEqual(actor._validate_bluetooth_name('Living Room'), 'Living Room')
        with self.assertRaises(ValueError):
            actor._validate_bluetooth_name('x' * 256)
        with self.assertRaises(ValueError):
            actor._validate_bluetooth_name('bad\nname')

    def test_subprocess_output_is_bounded(self):
        completed = mock.Mock(returncode=0, stdout='x' * (actor.MAX_SUBPROCESS_OUTPUT_BYTES + 100), stderr='y' * (actor.MAX_SUBPROCESS_OUTPUT_BYTES + 100))
        with mock.patch.object(actor.subprocess, 'run', return_value=completed):
            _, stdout, stderr = actor._run_list(['fixture'])
        self.assertLessEqual(len(stdout.encode('utf-8')), actor.MAX_SUBPROCESS_OUTPUT_BYTES)
        self.assertLessEqual(len(stderr.encode('utf-8')), actor.MAX_SUBPROCESS_OUTPUT_BYTES)
        self.assertLessEqual(len(actor._bound_hardware_message('x' * 2048).encode('utf-8')), actor.MAX_HARDWARE_MESSAGE_BYTES)

    def test_audio_controls_require_object_payloads(self):
        ok, message, details = actor.audio_set_default(['not-an-object'])
        self.assertFalse(ok)
        self.assertIn('payload', message.lower())
        self.assertEqual(details, {})

    def test_audio_sink_identifiers_are_bounded(self):
        self.assertEqual(actor._validate_audio_sink_id('12'), '12')
        self.assertEqual(actor._validate_audio_sink_id('@DEFAULT_AUDIO_SINK@'), '@DEFAULT_AUDIO_SINK@')
        with self.assertRaises(ValueError):
            actor._validate_audio_sink_id('9' * 64)
        with self.assertRaises(ValueError):
            actor._validate_audio_sink_id('sink-name')

    def test_ddc_fields_are_bounded(self):
        self.assertEqual(actor._validate_ddc_vcp('D6'), 'D6')
        self.assertEqual(actor._validate_ddc_value(100), 100)
        self.assertEqual(actor._validate_ddc_bus('1'), '1')
        with self.assertRaises(ValueError):
            actor._validate_ddc_vcp('--output')
        with self.assertRaises(ValueError):
            actor._validate_ddc_value(70000)
        with self.assertRaises(ValueError):
            actor._validate_ddc_bus('9999')

    def test_cec_command_is_bounded_and_allowlisted(self):
        self.assertEqual(actor._validate_cec_command('pow 0'), 'pow 0')
        with self.assertRaises(ValueError):
            actor._validate_cec_command('tx 0F:44:6D:00')
        with self.assertRaises(ValueError):
            actor._validate_cec_command('x' * 128)

    def test_actor_requests_root_verified_update_and_rejects_insecure_remote_http(self):
        self.assertEqual(actor._server_origin('https://managed.example:3000/base/'), 'https://managed.example:3000/base')
        self.assertEqual(actor._server_origin('http://127.0.0.1:3000'), 'http://127.0.0.1:3000')
        with self.assertRaises(ValueError):
            actor._server_origin('http://managed.example:3000')
        with tempfile.TemporaryDirectory() as temp:
            ok, message, details = actor.update_client(
                'https://managed.example:3000',
                'client-id',
                Path(temp) / 'cache',
                {'server_url': 'https://attacker.example', 'dry_run': True},
            )
        self.assertTrue(ok)
        self.assertEqual(details['request_path'], '/var/lib/screentra-cast/update/request')
        self.assertNotIn('attacker.example', message)

    def test_actor_atomic_cache_writes_reject_symlinked_parents(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            outside = root / 'outside'
            outside.mkdir()
            redirected = root / 'cache'
            redirected.symlink_to(outside, target_is_directory=True)
            with self.assertRaises(ValueError):
                actor._atomic_write_text(redirected / 'status.json', '{"safe":true}')
            self.assertFalse((outside / 'status.json').exists())

    def test_update_request_marker_uses_private_atomic_publication(self):
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / 'request'
            actor._write_update_request_marker(
                path,
                {'client_id': 'client-id', 'reason': 'server-command'},
            )
            self.assertEqual(path.read_text(encoding='utf-8'),
                             '{"client_id":"client-id","reason":"server-command"}')
            self.assertEqual(path.stat().st_mode & 0o777, 0o600)
            self.assertEqual(list(Path(temp).iterdir()), [path])

    def test_actor_json_reader_rejects_symlinked_files(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            outside = root / 'outside.json'
            outside.write_text('{"unsafe":true}', encoding='utf-8')
            redirected = root / 'cache.json'
            redirected.symlink_to(outside)
            self.assertEqual(actor._read_json_file(redirected, {'safe': True}), {'safe': True})

    def test_pending_json_reader_is_bounded(self):
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / 'pending-command.json'
            path.write_bytes(b'{' + b'a' * actor.MAX_PENDING_BYTES + b'}')
            self.assertEqual(actor._read_json_file(path, {'safe': True}), {'safe': True})

    def test_actor_json_rejects_nonstandard_values(self):
        with self.assertRaises(ValueError):
            actor._parse_pending_command('{"value":NaN}')

        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / 'cache.json'
            path.write_text('{"safe":NaN}', encoding='utf-8')
            self.assertEqual(actor._read_json_file(path, {'fallback': True}), {'fallback': True})

    def test_installer_and_bundle_surfaces_are_hardened(self):
        canonical = (ROOT / 'player-client/install/player-client.sh').read_text()
        generated = (ROOT / 'player-client/static/install/player-client.sh').read_text()
        for installer in (canonical, generated):
            self.assertNotIn('_create_unverified_context', installer)
            self.assertNotIn('RIPLEY_INSECURE_SSL', installer)
            self.assertNotIn('RIPLEY_ADMIN_DEFAULT', installer)
            self.assertNotIn('--host 0.0.0.0', installer)
            self.assertIn('--host 127.0.0.1', installer)
            self.assertIn('RIPLEY_ADMIN_MODE', installer)
            self.assertNotIn('ripley-player-actor.service', installer)
            self.assertIn('screentra-client-actor.service', installer)
        deprecated = ROOT / 'static/install/screentra-cast-player-client.sh'
        mode = deprecated.stat().st_mode
        self.assertFalse(mode & (stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH))
        self.assertNotIn('sudo', deprecated.read_text())
        bootstrap = (ROOT / 'static/install/player-client-bundle-install.sh').read_text()
        self.assertIn('MAX_BUNDLE_BYTES', bootstrap)
        self.assertIn('tarfile.open', bootstrap)
        self.assertNotIn('extractall', bootstrap)
        self.assertIn('pkeyutl', bootstrap)
    def test_actor_bounded_text_reader_rejects_symlinks(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            outside = root / 'outside.txt'
            outside.write_text('secret', encoding='utf-8')
            redirected = root / 'token'
            redirected.symlink_to(outside)
            with self.assertRaises(ValueError):
                actor._read_bounded_text(redirected, 64)


if __name__ == '__main__':
    unittest.main()
