#!/usr/bin/env python3
import importlib.util
import stat
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


class PlayerClientAuditTests(unittest.TestCase):
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
        exec(compile(canonical[start:end], 'embedded-updater.py', 'exec'), namespace)
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / 'oversized'
            path.write_bytes(b'x' * 17)
            with self.assertRaisesRegex(RuntimeError, 'local update file'):
                namespace['bounded_read_text'](path, 16)

    def test_kiosk_and_status_server_reads_are_bounded(self):
        installer = (ROOT / 'player-client/install/player-client.sh').read_text()
        kiosk = (ROOT / 'player-client/install/screentra-kiosk.sh').read_text()
        self.assertNotIn('cat "$INSTALL_DIR/server"', installer)
        self.assertNotIn("pathlib.Path(sys.argv[1]).read_text().strip()", installer)
        self.assertNotIn("pathlib.Path(sys.argv[1]).read_text().strip()", kiosk)
        self.assertIn('head -c 2049 "$CLIENT_DIR/server"', kiosk)
        self.assertIn('head -c 257 "$CLIENT_DIR/client-id"', kiosk)

    def test_embedded_heartbeat_reads_are_bounded(self):
        installer = (ROOT / 'player-client/install/player-client.sh').read_text()
        start = installer.index("cat > \"$HEARTBEAT\" <<'PY'\n") + len("cat > \"$HEARTBEAT\" <<'PY'\n")
        end = installer.index('\nPY\nchmod +x "$HEARTBEAT"', start)
        body = installer[start:end]
        compile(body, 'embedded-heartbeat.py', 'exec')
        self.assertIn('_read_bounded_response', body)
        self.assertNotIn('r.read().decode()', body)
        self.assertNotIn('Path(path).read_text()', body)

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
