#!/usr/bin/env python3
import importlib.util
import json
import sys
import tempfile
import unittest
from collections.abc import Mapping
from pathlib import Path
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
MODULE_PATH = ROOT / 'player-client/scripts/screentra-client-status-report-admission.py'


def load_module(path: Path, name: str):
    spec = importlib.util.spec_from_file_location(name, path)
    if spec is None or spec.loader is None:
        raise AssertionError(f'cannot load {path}')
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


admission = load_module(MODULE_PATH, 'screentra_client_status_report_admission')


def valid_report(**overrides):
    report = {
        'at': '2026-10-19T12:00:00Z',
        'addons': [],
        'capabilities': [
            {'id': 'render_only', 'state': 'available', 'detail': 'Built-in renderer'},
            {'id': 'camera_capture', 'state': 'unavailable', 'detail': 'No local video device detected'},
            {'id': 'microphone_capture', 'state': 'unavailable', 'detail': 'No local ALSA capture device detected'},
            {'id': 'local_inference', 'state': 'disabled', 'detail': 'Signed inference service is not enabled'},
            {'id': 'client_service', 'state': 'disabled', 'detail': 'Trusted client service runtime is not enabled'},
        ],
        'services': [],
    }
    report.update(overrides)
    return report


class ClientStatusReportAdmissionTests(unittest.TestCase):
    def test_accepts_bounded_inert_report(self):
        self.assertIsNone(admission.validate_status_report(valid_report()))
        self.assertIsNone(admission.validate_status_report(valid_report(addons=[{
            'id': 'weather',
            'version': '1.0.0',
            'state': 'verified_cached',
            'entrypoint': 'client/widget.js',
        }])))

    def test_services_must_remain_empty_while_runtime_is_disabled(self):
        with self.assertRaises(admission.StatusReportError):
            admission.validate_status_report(valid_report(services=[{
                'id': 'speech-to-text',
                'state': 'running',
            }]))

    def test_rejects_unknown_fields_and_invalid_capabilities(self):
        invalid = (
            valid_report(extra='not allowed'),
            valid_report(capabilities='available'),
            valid_report(capabilities=[{'id': 'unknown', 'state': 'available', 'detail': 'x'}]),
            valid_report(capabilities=[{'id': 'render_only', 'state': 'running', 'detail': 'x'}]),
            valid_report(capabilities=[{'id': 'render_only', 'state': 'available'}]),
            valid_report(at='2026-99-99T12:00:00Z'),
            valid_report(addons=[{'id': '../secret', 'state': 'verified'}]),
            valid_report(addons=[{'id': 'weather', 'state': 'running'}]),
            valid_report(addons=[{'id': 'weather', 'state': 'verified', 'entrypoint': '../client.js'}]),
            valid_report(addons=[
                {'id': 'weather', 'state': 'verified'},
                {'id': 'weather', 'state': 'failed'},
            ]),
            valid_report(addons=[{'id': 'weather', 'state': 'failed', 'error': 'password=secret'}]),
            valid_report(error='https://example.invalid'),
            valid_report(capabilities=[{'id': 'render_only', 'state': 'available', 'detail': '/dev/video0'}]),
        )
        for candidate in invalid:
            with self.subTest(candidate=candidate), self.assertRaises(admission.StatusReportError):
                admission.validate_status_report(candidate)

    def test_rejects_credential_path_and_control_shaped_metadata(self):
        for detail in ('password=secret', 'credential-token', 'https://example.invalid', '/tmp/status', 'x\nstatus'):
            with self.subTest(detail=detail), self.assertRaises(admission.StatusReportError):
                admission.validate_status_report(valid_report(
                    capabilities=[{'id': 'render_only', 'state': 'available', 'detail': detail}]
                ))

    def test_rejects_oversized_report_and_non_mapping_input(self):
        with self.assertRaises(admission.StatusReportError):
            admission.validate_status_report([])
        oversized = valid_report(
            capabilities=[{'id': 'render_only', 'state': 'available', 'detail': 'x' * 256}]
        )
        with self.assertRaises(admission.StatusReportError):
            admission.validate_status_report(oversized)
        encoded = json.dumps(valid_report(), separators=(',', ':')).encode('utf-8')
        self.assertLessEqual(len(encoded), admission.MAX_REPORT_BYTES)

    def test_upload_revalidates_before_network(self):
        heartbeat = load_module(
            ROOT / 'player-client/scripts/screentra-player-heartbeat.py',
            'screentra_player_heartbeat_for_upload_validation_test',
        )
        with patch.object(heartbeat, 'urlopen_ripley', side_effect=AssertionError('network called')):
            with self.assertRaises(heartbeat._status_report_mod.StatusReportError):
                heartbeat.post_addon_status(
                    'https://example.invalid',
                    'client-1',
                    valid_report(services=[{'id': 'speech-to-text'}]),
                )

    def test_heartbeat_writes_only_admitted_reports(self):
        heartbeat = load_module(
            ROOT / 'player-client/scripts/screentra-player-heartbeat.py',
            'screentra_player_heartbeat_for_status_report_test',
        )
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / 'addon-status.json'
            heartbeat._write_status_report(path, valid_report())
            self.assertEqual(json.loads(path.read_text(encoding='utf-8')), valid_report())
            with self.assertRaises(heartbeat._status_report_mod.StatusReportError):
                heartbeat._write_status_report(path, valid_report(services=[{'id': 'speech-to-text'}]))
            self.assertEqual(json.loads(path.read_text(encoding='utf-8')), valid_report())

        source = MODULE_PATH.read_text(encoding='utf-8')
        for forbidden in ('socket', 'subprocess', 'Popen', 'run(', 'open(', 'systemctl', 'Path(', 'urlopen'):
            self.assertNotIn(forbidden, source)
        heartbeat_source = (
            ROOT / 'player-client/scripts/screentra-player-heartbeat.py'
        ).read_text(encoding='utf-8')
        self.assertIn(
            'def post_addon_status(server, client_id, status):\n'
            '    validate_status_report(status)\n',
            heartbeat_source,
        )


if __name__ == '__main__':
    unittest.main()
