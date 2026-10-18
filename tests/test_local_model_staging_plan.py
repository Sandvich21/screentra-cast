#!/usr/bin/env python3
import hashlib
import importlib.util
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
MODULE_PATH = ROOT / 'player-client/scripts/screentra-local-model-staging-plan.py'
plan_module = None
if MODULE_PATH.is_file():
    spec = importlib.util.spec_from_file_location(
        'screentra_local_model_staging_plan', MODULE_PATH
    )
    plan_module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = plan_module
    assert spec.loader is not None
    spec.loader.exec_module(plan_module)


class LocalModelStagingPlanTests(unittest.TestCase):
    def setUp(self):
        self.tempdir = tempfile.TemporaryDirectory()
        self.base = Path(self.tempdir.name)
        self.root = self.base / 'models'
        self.root.mkdir()
        self.model_path = self.root / 'whisper-tiny.en.bin'
        self.model_bytes = b'bounded local model fixture'
        self.model_path.write_bytes(self.model_bytes)
        self.digest = hashlib.sha256(self.model_bytes).hexdigest()

    def tearDown(self):
        self.tempdir.cleanup()

    def require_module(self):
        self.assertIsNotNone(
            plan_module,
            'production local-model staging-plan module is not implemented',
        )
        return plan_module

    def request(self, **overrides):
        value = {
            'service_id': 'magic-mirror-stt',
            'activation_id': 'activation-20261018-01',
            'model_id': 'whisper-tiny.en',
            'model_sha256': self.digest,
            'approved_model_root': str(self.root),
            'model_path': self.model_path.name,
            'intent': 'verify',
        }
        value.update(overrides)
        return value

    def assert_invalid(self, request):
        module = self.require_module()
        with self.assertRaises(module.LocalModelStagingPlanError):
            module.build_model_staging_plan(request)

    def test_verify_composes_read_only_integrity_and_binds_evidence(self):
        module = self.require_module()
        plan = module.build_model_staging_plan(self.request())

        self.assertEqual(plan['plan_type'], 'local_model_staging_plan')
        self.assertEqual(plan['service_id'], 'magic-mirror-stt')
        self.assertEqual(plan['activation_id'], 'activation-20261018-01')
        self.assertEqual(plan['model_id'], 'whisper-tiny.en')
        self.assertEqual(plan['model_sha256'], self.digest)
        self.assertEqual(plan['approved_model_root'], str(self.root))
        self.assertEqual(plan['model_path'], self.model_path.name)
        self.assertEqual(plan['code'], 'verified')
        self.assertEqual(plan['verification_plan']['size_bytes'], len(self.model_bytes))
        self.assertTrue(plan['verification_plan']['verified'])
        self.assertTrue(plan['verification_plan']['read_only'])
        for field in (
            'execution_enabled', 'downloaded', 'staged', 'loaded',
            'process_started', 'network_used',
        ):
            self.assertFalse(plan[field], field)

    def test_stage_download_and_load_are_disabled_without_filesystem_touch(self):
        module = self.require_module()
        root = self.base / 'not-created-model-root'
        for intent in ('stage', 'download', 'load'):
            with self.subTest(intent=intent):
                plan = module.build_model_staging_plan(self.request(
                    approved_model_root=str(root),
                    model_path='nested/model.bin',
                    intent=intent,
                ))

                self.assertFalse(root.exists())
                self.assertFalse(plan['ok'])
                self.assertEqual(plan['code'], 'execution_disabled')
                self.assertFalse(plan['execution_enabled'])
                self.assertEqual(plan['stage_plan']['effect'], 'metadata_only')
                for field in (
                    'downloaded', 'staged', 'loaded', 'process_started',
                    'network_used',
                ):
                    self.assertFalse(plan[field], field)

    def test_verify_rejects_symlinked_model_without_following_it(self):
        link = self.root / 'model-link.bin'
        link.symlink_to(self.model_path)
        self.assert_invalid(self.request(model_path=link.name))

    def test_relative_model_path_and_root_forms_are_strictly_bounded(self):
        unsafe_paths = (
            '', '.', '..', '../model.bin', 'nested/../../model.bin',
            '/tmp/model.bin', 'C:\\model.bin', 'https://example/model.bin',
            'registry.example/model.bin', 'nested//model.bin',
            'nested/./model.bin', 'nested/model bin', 'nested/~model.bin',
            'nested/model.bin?download=1',
        )
        for model_path in unsafe_paths:
            with self.subTest(model_path=repr(model_path)):
                self.assert_invalid(self.request(model_path=model_path, intent='stage'))

        for root in ('models', 'https://example/models', '/tmp/root/../models'):
            with self.subTest(root=repr(root)):
                self.assert_invalid(self.request(approved_model_root=root))

    def test_identity_digest_and_metadata_mismatches_fail_closed(self):
        cases = (
            self.request(service_id='other/service'),
            self.request(activation_id='activation/other'),
            self.request(model_id='whisper/tiny'),
            self.request(model_sha256=self.digest.upper()),
            self.request(model_sha256='0' * 64),
            self.request(model_sha256='sha256:' + self.digest),
            self.request(model_sha256=self.digest[:-1]),
            self.request(**{'expected_model_id': 'other-model'}),
        )
        for value in cases:
            with self.subTest(value=value):
                self.assert_invalid(value)

    def test_urls_registries_images_commands_credentials_audio_and_runtime_controls_are_rejected(self):
        forbidden = (
            {'url': 'https://example/model.bin'},
            {'registry': 'registry.example'},
            {'image': 'registry.example/model:latest'},
            {'command': ['/bin/sh']},
            {'credentials': {'token': 'secret'}},
            {'audio': 'raw audio'},
            {'runtime_controls': {'cpu': 1}},
            {'execution_enabled': True},
            {'max_model_bytes': 1},
            {'timeout_seconds': 1},
            {'arbitrary': True},
        )
        for extra in forbidden:
            with self.subTest(extra=extra):
                self.assert_invalid(self.request(**extra))

    def test_oversized_metadata_is_rejected(self):
        module = self.require_module()
        self.assert_invalid(
            self.request(model_id='m' * module.MAX_MODEL_ID_BYTES)
            | {'padding': 'x' * module.MAX_REQUEST_BYTES}
        )

    def test_verify_requires_a_local_path_and_never_claims_runtime_evidence(self):
        module = self.require_module()
        for model_path in (self.root / self.model_path.name, 'https://example/model.bin'):
            with self.subTest(model_path=str(model_path)):
                self.assert_invalid(self.request(model_path=str(model_path)))

        source = MODULE_PATH.read_text(encoding='utf-8')
        for forbidden in ('subprocess', 'socket.socket', 'socket.bind', 'os.system', 'open('):
            self.assertNotIn(forbidden, source)


if __name__ == '__main__':
    unittest.main()
