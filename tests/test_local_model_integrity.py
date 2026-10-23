#!/usr/bin/env python3
import hashlib
import importlib.util
import os
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
MODULE_PATH = ROOT / 'player-client/scripts/screentra-local-model-integrity.py'
spec = importlib.util.spec_from_file_location('screentra_local_model_integrity', MODULE_PATH)
integrity = importlib.util.module_from_spec(spec)
assert spec.loader is not None
sys.modules[spec.name] = integrity
spec.loader.exec_module(integrity)


class LocalModelIntegrityTests(unittest.TestCase):
    def setUp(self):
        self.tempdir = tempfile.TemporaryDirectory()
        self.root = Path(self.tempdir.name) / 'models'
        self.root.mkdir()
        self.model = self.root / 'whisper-tiny.en.bin'
        self.model_bytes = b'local model fixture\x00\x01'
        self.model.write_bytes(self.model_bytes)
        self.digest = hashlib.sha256(self.model_bytes).hexdigest()

    def tearDown(self):
        self.tempdir.cleanup()

    def verify(self, **overrides):
        values = {
            'model_id': 'whisper-tiny.en',
            'expected_sha256': self.digest,
            'model_path': self.model,
            'approved_model_root': self.root,
        }
        values.update(overrides)
        return integrity.verify_model_file(**values)

    def test_valid_model_returns_verified_identity_and_size(self):
        verified = self.verify()

        self.assertEqual(verified.model_id, 'whisper-tiny.en')
        self.assertEqual(verified.sha256, self.digest)
        self.assertEqual(verified.path, self.model)
        self.assertEqual(verified.size_bytes, len(self.model_bytes))

    def test_model_id_is_bounded_and_path_safe(self):
        for model_id in (
            '',
            'x' * (integrity.MAX_MODEL_ID_BYTES + 1),
            '../whisper-tiny',
            'whisper/tiny',
            'whisper tiny',
            'whisper\x00tiny',
        ):
            with self.subTest(model_id=repr(model_id)), self.assertRaises(integrity.ModelIntegrityError):
                self.verify(model_id=model_id)

    def test_model_paths_are_bounded(self):
        with self.assertRaises(integrity.ModelIntegrityError):
            self.verify(model_path='x' * (integrity.MAX_PATH_BYTES + 1))
        with self.assertRaises(integrity.ModelIntegrityError):
            self.verify(approved_model_root='/' + ('x' * integrity.MAX_PATH_BYTES))

    def test_digest_must_be_exact_lowercase_sha256(self):
        for digest in (
            self.digest.upper(),
            'sha256:' + self.digest,
            self.digest[:-1],
            self.digest + '0',
            'g' * 64,
        ):
            with self.subTest(digest=repr(digest)), self.assertRaises(integrity.ModelIntegrityError):
                self.verify(expected_sha256=digest)

    def test_missing_model_and_digest_mismatch_fail_closed(self):
        with self.assertRaises(integrity.ModelIntegrityError):
            self.verify(model_path=self.root / 'missing.bin')
        with self.assertRaises(integrity.ModelIntegrityError):
            self.verify(expected_sha256='0' * 64)

    def test_model_must_be_regular_non_symlink_file(self):
        directory = self.root / 'directory.bin'
        directory.mkdir()
        with self.assertRaises(integrity.ModelIntegrityError):
            self.verify(model_path=directory)

        symlink = self.root / 'model-link.bin'
        symlink.symlink_to(self.model)
        with self.assertRaises(integrity.ModelIntegrityError):
            self.verify(model_path=symlink)

    def test_root_must_be_an_existing_non_symlink_directory(self):
        with tempfile.TemporaryDirectory() as other:
            outside_root = Path(other)
            root_link = Path(self.tempdir.name) / 'models-link'
            root_link.symlink_to(self.root, target_is_directory=True)
            with self.assertRaises(integrity.ModelIntegrityError):
                self.verify(approved_model_root=root_link)
            with self.assertRaises(integrity.ModelIntegrityError):
                self.verify(approved_model_root=self.root / 'missing')
            with self.assertRaises(integrity.ModelIntegrityError):
                self.verify(approved_model_root=self.model)
            with self.assertRaises(integrity.ModelIntegrityError):
                self.verify(approved_model_root=outside_root / 'missing')

    def test_traversal_and_outside_root_paths_are_rejected(self):
        outside = Path(self.tempdir.name) / 'outside.bin'
        outside.write_bytes(self.model_bytes)
        cases = (
            Path('..') / 'outside.bin',
            self.root.parent / 'outside.bin',
            outside,
            Path('/etc/passwd'),
        )
        for model_path in cases:
            with self.subTest(model_path=str(model_path)), self.assertRaises(integrity.ModelIntegrityError):
                self.verify(model_path=model_path)

    def test_oversized_model_is_rejected_before_hash_acceptance(self):
        with self.assertRaises(integrity.ModelIntegrityError):
            self.verify(max_model_bytes=len(self.model_bytes) - 1)

    def test_file_read_bound_and_size_limit_are_validated(self):
        with self.assertRaises(integrity.ModelIntegrityError):
            self.verify(max_model_bytes=0)
        with self.assertRaises(integrity.ModelIntegrityError):
            self.verify(max_model_bytes=True)
        with self.assertRaises(integrity.ModelIntegrityError):
            self.verify(max_model_bytes='100')

    def test_relative_file_path_is_resolved_only_inside_approved_root(self):
        verified = self.verify(model_path=self.model.name)

        self.assertEqual(verified.path, self.root / self.model.name)


if __name__ == '__main__':
    unittest.main()
