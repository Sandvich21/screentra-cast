#!/usr/bin/env python3
import hashlib
import importlib.util
import io
import json
import os
import tarfile
import tempfile
import unittest
from pathlib import Path
from unittest import mock

ROOT = Path(__file__).resolve().parents[1]


def load_module(path: Path, name: str):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    spec.loader.exec_module(module)
    return module


heartbeat = load_module(
    ROOT / 'player-client/scripts/screentra-player-heartbeat.py',
    'screentra_player_heartbeat_addon_cache',
)


class InventoryResponse:
    def __init__(self, payload):
        self.payload = json.dumps(payload).encode('utf-8')

    def __enter__(self):
        return self

    def __exit__(self, exc_type, exc_value, traceback):
        return False

    def read(self, limit=None):
        return self.payload


class HeartbeatAddonCacheTests(unittest.TestCase):
    def inventory(self, version='1.0.0', digest=None):
        return {
            'addons': [{
                'id': 'demo.addon',
                'version': version,
                'client_api_version': '1',
                'client_sha256': digest or ('a' * 64),
            }],
        }

    def sync(self, temp, inventory):
        cache_dir = Path(temp) / 'cache'
        with mock.patch.object(heartbeat, 'urlopen_ripley', return_value=InventoryResponse(inventory)):
            return heartbeat.sync_client_addons('https://server.example', 'client-a', cache_dir)

    def test_inventory_response_body_is_bounded(self):
        with tempfile.TemporaryDirectory() as temp:
            result = self.sync(
                temp,
                {'addons': [], 'padding': 'x' * heartbeat.MAX_JSON_FILE_BYTES},
            )
            self.assertEqual(result.get('error'), heartbeat.GENERIC_INVENTORY_ERROR)

    def package_manifest(self, capabilities=None, service=None, payload=b'console.log("inert");\\n'):
        return {
            'schema_version': 1,
            'id': 'demo.addon',
            'name': 'Demo addon',
            'description': 'Signed client extension fixture',
            'version': '1.0.0',
            'kind': 'client_extension',
            'permissions': ['render_only'],
            'client_capabilities': [] if capabilities is None else capabilities,
            'client_entrypoint': 'client/index.js',
            'server_entrypoint': None,
            'client_sha256': hashlib.sha256(payload).hexdigest(),
            'client_api_version': '1',
            'client_service': service,
        }

    def signed_package(self, manifest, payload=b'console.log("inert");\\n'):
        output = io.BytesIO()
        with tarfile.open(fileobj=output, mode='w:gz') as archive:
            for name, content in (
                ('manifest.json', json.dumps(manifest).encode('utf-8')),
                ('manifest.sig', b'inert signature'),
                ('client/index.js', payload),
            ):
                info = tarfile.TarInfo(name)
                info.size = len(content)
                archive.addfile(info, io.BytesIO(content))
        return output.getvalue()

    def write_cached_addon(self, temp, manifest=None, active=None, payload=b'console.log("inert");\\n'):
        manifest = manifest or self.package_manifest(payload=payload)
        addon_root = Path(temp) / 'addons'
        target = addon_root / manifest['id'] / manifest['version']
        (target / 'client').mkdir(parents=True)
        (target / 'client/index.js').write_bytes(payload)
        (target / 'manifest.json').write_text(json.dumps(manifest), encoding='utf-8')
        (target / 'manifest.sig').write_bytes(b'cached signature')
        active = active or {
            'id': manifest['id'],
            'version': manifest['version'],
            'client_sha256': manifest['client_sha256'],
            'entrypoint': manifest['client_entrypoint'],
        }
        (target.parent / 'active.json').write_text(json.dumps(active), encoding='utf-8')
        return addon_root, target

    def cached_sync(self, temp, inventory, fetch=None, signature_result=0):
        cache_dir = Path(temp) / 'cache'
        public_key = Path(temp) / 'addon-signing.pub'
        public_key.write_text('test key', encoding='utf-8')
        patches = [
            mock.patch.dict(os.environ, {'RIPLEY_CLIENT_ADDONS_DIR': str(Path(temp) / 'addons')}, clear=False),
            mock.patch.object(heartbeat, 'urlopen_ripley', return_value=InventoryResponse(inventory)),
            mock.patch.object(heartbeat, 'addon_public_key_path', return_value=public_key),
            mock.patch.object(
                heartbeat.subprocess, 'run', return_value=mock.Mock(returncode=signature_result)
            ),
        ]
        if fetch is not None:
            patches.append(mock.patch.object(heartbeat, 'fetch_addon_bytes', side_effect=fetch))
        with patches[0], patches[1], patches[2], patches[3]:
            if fetch is None:
                return heartbeat.sync_client_addons('https://server.example', 'client-a', cache_dir)
            with patches[4]:
                return heartbeat.sync_client_addons('https://server.example', 'client-a', cache_dir)

    def test_cached_readback_revalidates_manifest_and_signature(self):
        with tempfile.TemporaryDirectory() as temp:
            payload = b'console.log("inert");\\n'
            manifest = self.package_manifest(payload=payload)
            self.write_cached_addon(temp, manifest=manifest, payload=payload)
            with mock.patch.object(heartbeat, 'fetch_addon_bytes', side_effect=AssertionError('cache should be accepted')):
                status = self.cached_sync(temp, self.inventory(digest=manifest['client_sha256']))
            self.assertEqual(status['addons'][0]['state'], 'verified_cached')

    def test_tampered_cached_manifest_fails_closed_and_preserves_cache(self):
        with tempfile.TemporaryDirectory() as temp:
            payload = b'console.log("inert");\\n'
            manifest = self.package_manifest(payload=payload)
            addon_root, target = self.write_cached_addon(temp, manifest=manifest, payload=payload)
            tampered = dict(manifest, kind='not-a-client-extension')
            (target / 'manifest.json').write_text(json.dumps(tampered), encoding='utf-8')
            with mock.patch.object(heartbeat, 'fetch_addon_bytes', side_effect=OSError('offline')) as fetch:
                status = self.cached_sync(temp, self.inventory(digest=manifest['client_sha256']))
            fetch.assert_not_called()
            self.assertEqual(status['addons'][0]['state'], 'failed')
            self.assertEqual((target / 'manifest.json').read_text(encoding='utf-8'), json.dumps(tampered))
            self.assertEqual((target / 'client/index.js').read_bytes(), payload)
            self.assertTrue((addon_root / 'demo.addon' / 'active.json').is_file())

    def test_tampered_cached_signature_fails_closed_and_preserves_cache(self):
        with tempfile.TemporaryDirectory() as temp:
            payload = b'console.log("inert");\\n'
            manifest = self.package_manifest(payload=payload)
            _, target = self.write_cached_addon(temp, manifest=manifest, payload=payload)
            (target / 'manifest.sig').write_bytes(b'tampered signature')
            with mock.patch.object(heartbeat, 'fetch_addon_bytes', side_effect=OSError('offline')) as fetch:
                status = self.cached_sync(
                    temp,
                    self.inventory(digest=manifest['client_sha256']),
                    signature_result=1,
                )
            fetch.assert_not_called()
            self.assertEqual(status['addons'][0]['state'], 'failed')
            self.assertEqual((target / 'manifest.sig').read_bytes(), b'tampered signature')
            self.assertEqual((target / 'client/index.js').read_bytes(), payload)

    def test_tampered_cached_active_metadata_fails_closed(self):
        with tempfile.TemporaryDirectory() as temp:
            payload = b'console.log("inert");\\n'
            manifest = self.package_manifest(payload=payload)
            _, target = self.write_cached_addon(
                temp,
                manifest=manifest,
                active={
                    'id': manifest['id'],
                    'version': manifest['version'],
                    'client_sha256': manifest['client_sha256'],
                    'entrypoint': manifest['client_entrypoint'],
                    'unexpected': 'tampered',
                },
                payload=payload,
            )
            with mock.patch.object(heartbeat, 'fetch_addon_bytes', side_effect=OSError('offline')) as fetch:
                status = self.cached_sync(temp, self.inventory(digest=manifest['client_sha256']))
            fetch.assert_not_called()
            self.assertEqual(status['addons'][0]['state'], 'failed')
            self.assertIn('unexpected', (target.parent / 'active.json').read_text(encoding='utf-8'))

    def test_cached_readback_rejects_symlinked_manifest_and_signature(self):
        for name in ('manifest.json', 'manifest.sig'):
            with self.subTest(name=name), tempfile.TemporaryDirectory() as temp:
                payload = b'console.log("inert");\\n'
                manifest = self.package_manifest(payload=payload)
                _, target = self.write_cached_addon(temp, manifest=manifest, payload=payload)
                outside = Path(temp) / name
                outside.write_bytes((target / name).read_bytes())
                (target / name).unlink()
                (target / name).symlink_to(outside)
                with mock.patch.object(heartbeat, 'fetch_addon_bytes', side_effect=OSError('offline')):
                    status = self.cached_sync(temp, self.inventory(digest=manifest['client_sha256']))
                self.assertEqual(status['addons'][0]['state'], 'failed')
                self.assertTrue((target / name).is_symlink())

    def assert_package_rejected_without_addon_cache_state(self, manifest):
        package = self.signed_package(manifest)
        with tempfile.TemporaryDirectory() as temp:
            staging_parent = Path(temp) / 'addons'
            staging_parent.mkdir()
            public_key = Path(temp) / 'addon-signing.pub'
            public_key.write_text('test key', encoding='utf-8')
            with mock.patch.object(heartbeat, 'addon_public_key_path', return_value=public_key), \
                    mock.patch.object(heartbeat.subprocess, 'run', return_value=mock.Mock(returncode=0)):
                with self.assertRaises(ValueError):
                    heartbeat._extract_verified_addon_package(
                        package,
                        self.inventory(digest=manifest['client_sha256'])['addons'][0],
                        staging_parent=staging_parent,
                    )
            self.assertEqual(list(staging_parent.iterdir()), [])

    def test_rejects_malformed_or_unapproved_client_capabilities_before_cache_state(self):
        invalid_capabilities = [
            'render_only',
            ['render_only', 'render_only'],
            ['host_shell'],
            ['local_inference', 'client_service'],
            ['local_inference', 'microphone_capture'],
            ['client_service'],
        ]
        for capabilities in invalid_capabilities:
            with self.subTest(capabilities=capabilities):
                self.assert_package_rejected_without_addon_cache_state(
                    self.package_manifest(capabilities=capabilities)
                )

    def test_accepts_bounded_client_capabilities_and_service_metadata(self):
        service = {
            'id': 'speech-to-text',
            'image': 'registry.example/whisper',
            'image_digest': 'sha256:' + ('b' * 64),
            'socket_name': 'speech-to-text',
            'cpu_millis': 500,
            'memory_bytes': 128 * 1024 * 1024,
            'pids': 32,
            'writable_bytes': 64 * 1024 * 1024,
            'model_id': 'whisper-tiny.en',
            'model_sha256': 'c' * 64,
        }
        manifest = self.package_manifest(
            capabilities=['microphone_capture', 'local_inference', 'client_service'],
            service=service,
        )
        package = self.signed_package(manifest)
        with tempfile.TemporaryDirectory() as temp:
            staging_parent = Path(temp) / 'addons'
            staging_parent.mkdir()
            public_key = Path(temp) / 'addon-signing.pub'
            public_key.write_text('test key', encoding='utf-8')
            with mock.patch.object(heartbeat, 'addon_public_key_path', return_value=public_key), \
                    mock.patch.object(heartbeat.subprocess, 'run', return_value=mock.Mock(returncode=0)):
                staging, verified, entrypoint = heartbeat._extract_verified_addon_package(
                    package,
                    self.inventory(digest=manifest['client_sha256'])['addons'][0],
                    staging_parent=staging_parent,
                )
            self.assertEqual(verified['client_capabilities'], manifest['client_capabilities'])
            self.assertEqual(verified['client_service']['model_sha256'], 'c' * 64)
            self.assertEqual(entrypoint, 'client/index.js')
            self.assertEqual(list(staging_parent.iterdir()), [staging])
            import shutil
            shutil.rmtree(staging)

    def test_rejects_unsafe_client_service_metadata_before_staging(self):
        base_service = {
            'id': 'speech-to-text',
            'image': 'registry.example/whisper',
            'image_digest': 'sha256:' + ('b' * 64),
            'socket_name': 'speech-to-text',
            'cpu_millis': 500,
            'memory_bytes': 128 * 1024 * 1024,
            'pids': 32,
            'writable_bytes': 64 * 1024 * 1024,
        }
        for field, value in (
            ('command', ['/bin/sh']),
            ('mounts', ['/host:/container']),
            ('ports', [8080]),
            ('credentials', {'token': 'secret'}),
            ('environment', {'PATH': '/tmp'}),
            ('socket_name', '../unsafe-socket'),
        ):
            with self.subTest(field=field):
                service = dict(base_service)
                service[field] = value
                manifest = self.package_manifest(
                    capabilities=['client_service'],
                    service=service,
                )
                self.assert_package_rejected_without_addon_cache_state(manifest)

    def test_rejects_unpaired_or_malformed_client_service_model_identity(self):
        base_service = {
            'id': 'speech-to-text',
            'image': 'registry.example/whisper',
            'image_digest': 'sha256:' + ('b' * 64),
            'socket_name': 'speech-to-text',
            'cpu_millis': 500,
            'memory_bytes': 128 * 1024 * 1024,
            'pids': 32,
            'writable_bytes': 64 * 1024 * 1024,
        }
        for update in (
            {'model_id': 'whisper-tiny.en'},
            {'model_sha256': 'c' * 63},
            {'model_sha256': 'C' * 64},
            {'model_id': '../unsafe', 'model_sha256': 'c' * 64},
        ):
            with self.subTest(update=update):
                service = dict(base_service)
                service.update(update)
                self.assert_package_rejected_without_addon_cache_state(
                    self.package_manifest(capabilities=['client_service'], service=service)
                )

    def test_rejects_unsafe_inventory_versions_before_creating_addon_cache(self):
        for version in ('../escape', '/absolute', 'nested/version', r'nested\\version', 'bad\n', 'x' * 65):
            with self.subTest(version=repr(version)), tempfile.TemporaryDirectory() as temp:
                addon_root = Path(temp) / 'addons'
                with mock.patch.dict(os.environ, {'RIPLEY_CLIENT_ADDONS_DIR': str(addon_root)}, clear=False):
                    status = self.sync(temp, self.inventory(version=version))
                self.assertEqual(status['addons'][0]['state'], 'failed')
                self.assertFalse(addon_root.exists(), version)

    def test_rejects_symlinked_addon_root_and_does_not_write_through_it(self):
        with tempfile.TemporaryDirectory() as temp:
            outside = Path(temp) / 'outside'
            outside.mkdir()
            addon_root = Path(temp) / 'addons'
            addon_root.symlink_to(outside, target_is_directory=True)
            with mock.patch.dict(os.environ, {'RIPLEY_CLIENT_ADDONS_DIR': str(addon_root)}, clear=False):
                status = self.sync(temp, self.inventory())
            self.assertEqual(status['addons'][0]['state'], 'failed')
            self.assertEqual(status['addons'][0]['error'], heartbeat.GENERIC_ADDON_ERROR)
            self.assertNotIn(str(outside), json.dumps(status))
            self.assertEqual(list(outside.iterdir()), [])

    def test_rejects_symlinked_addon_id_and_version_directories(self):
        for link_name in ('id', 'version'):
            with self.subTest(link_name=link_name), tempfile.TemporaryDirectory() as temp:
                addon_root = Path(temp) / 'addons'
                addon_root.mkdir()
                outside = Path(temp) / 'outside'
                outside.mkdir()
                if link_name == 'id':
                    (addon_root / 'demo.addon').symlink_to(outside, target_is_directory=True)
                else:
                    id_dir = addon_root / 'demo.addon'
                    id_dir.mkdir()
                    (id_dir / '1.0.0').symlink_to(outside, target_is_directory=True)
                with mock.patch.dict(os.environ, {'RIPLEY_CLIENT_ADDONS_DIR': str(addon_root)}, clear=False):
                    status = self.sync(temp, self.inventory())
                self.assertEqual(status['addons'][0]['state'], 'failed')
                self.assertEqual(list(outside.iterdir()), [])

    def test_rejects_symlinked_cache_parent_for_entrypoint(self):
        with tempfile.TemporaryDirectory() as temp:
            addon_root = Path(temp) / 'addons'
            version_dir = addon_root / 'demo.addon' / '1.0.0'
            version_dir.mkdir(parents=True)
            payload = b'console.log("inert");\n'
            digest = hashlib.sha256(payload).hexdigest()
            outside = Path(temp) / 'outside'
            outside.mkdir()
            (outside / 'index.js').write_bytes(payload)
            (version_dir.parent / 'active.json').write_text(json.dumps({
                'id': 'demo.addon', 'version': '1.0.0',
                'client_sha256': digest, 'entrypoint': 'client/index.js',
            }), encoding='utf-8')
            (version_dir / 'client').symlink_to(outside, target_is_directory=True)
            with mock.patch.dict(os.environ, {'RIPLEY_CLIENT_ADDONS_DIR': str(addon_root)}, clear=False):
                status = self.sync(temp, self.inventory(digest=digest))
            self.assertEqual(status['addons'][0]['state'], 'failed')
            self.assertTrue((version_dir / 'client').is_symlink())

    def test_rejects_symlinked_active_and_entrypoint_cache_targets(self):
        with tempfile.TemporaryDirectory() as temp:
            addon_root = Path(temp) / 'addons'
            version_dir = addon_root / 'demo.addon' / '1.0.0'
            version_dir.mkdir(parents=True)
            payload = b'console.log("inert");\n'
            digest = hashlib.sha256(payload).hexdigest()
            outside = Path(temp) / 'outside.js'
            outside.write_bytes(payload)
            active = {
                'id': 'demo.addon',
                'version': '1.0.0',
                'client_sha256': digest,
                'entrypoint': 'client/index.js',
            }
            active_target = version_dir.parent / 'active-target.json'
            active_target.write_text(json.dumps(active), encoding='utf-8')
            (version_dir.parent / 'active.json').symlink_to(active_target)
            (version_dir / 'client').mkdir()
            (version_dir / 'client/index.js').symlink_to(outside)
            with mock.patch.dict(os.environ, {'RIPLEY_CLIENT_ADDONS_DIR': str(addon_root)}, clear=False):
                status = self.sync(temp, self.inventory(digest=digest))
            self.assertEqual(status['addons'][0]['state'], 'failed')
            self.assertTrue((version_dir.parent / 'active.json').is_symlink())
            self.assertTrue((version_dir / 'client/index.js').is_symlink())

    def test_failed_active_metadata_write_preserves_previous_verified_version(self):
        with tempfile.TemporaryDirectory() as temp:
            addon_root = Path(temp) / 'addons'
            version_dir = addon_root / 'demo.addon' / '1.0.0'
            client_dir = version_dir / 'client'
            client_dir.mkdir(parents=True)
            old_payload = b'old verified payload\n'
            client_file = client_dir / 'index.js'
            client_file.write_bytes(old_payload)
            old_digest = hashlib.sha256(old_payload).hexdigest()
            (version_dir.parent / 'active.json').write_text(json.dumps({
                'id': 'demo.addon',
                'version': '1.0.0',
                'client_sha256': old_digest,
                'entrypoint': 'client/index.js',
            }), encoding='utf-8')
            new_payload = b'new verified payload\n'
            new_digest = hashlib.sha256(new_payload).hexdigest()
            staging = Path(temp) / 'verified-stage'
            (staging / 'client').mkdir(parents=True)
            (staging / 'client/index.js').write_bytes(new_payload)
            manifest = {
                'id': 'demo.addon',
                'version': '1.0.0',
                'client_sha256': new_digest,
            }
            real_atomic_write = heartbeat._atomic_write_text

            def fail_active_write(path, text, mode=0o600):
                if Path(path).name == 'active.json':
                    raise OSError('simulated metadata publication failure')
                return real_atomic_write(path, text, mode)

            with mock.patch.dict(os.environ, {'RIPLEY_CLIENT_ADDONS_DIR': str(addon_root)}, clear=False), \
                    mock.patch.object(heartbeat, 'fetch_addon_bytes', return_value=b'inert package'), \
                    mock.patch.object(heartbeat, '_extract_verified_addon_package', return_value=(
                        staging,
                        manifest,
                        'client/index.js',
                    )), \
                    mock.patch.object(heartbeat, '_atomic_write_text', side_effect=fail_active_write):
                status = self.sync(temp, self.inventory(digest=new_digest))
            self.assertEqual(status['addons'][0]['state'], 'failed')
            self.assertEqual(client_file.read_bytes(), old_payload)
            self.assertEqual(json.loads((version_dir.parent / 'active.json').read_text()), {
                'id': 'demo.addon',
                'version': '1.0.0',
                'client_sha256': old_digest,
                'entrypoint': 'client/index.js',
            })
    def test_heartbeat_command_runner_has_no_shell_fallback(self):
        self.assertEqual(heartbeat.run('echo attacker-controlled'), '')

    def test_json_cache_reader_rejects_symlinked_files(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            outside = root / 'outside.json'
            outside.write_text('{"unsafe":true}', encoding='utf-8')
            redirected = root / 'cache.json'
            redirected.symlink_to(outside)
            self.assertEqual(heartbeat._read_json_file(redirected, {'safe': True}), {'safe': True})

    def test_client_auth_token_reader_rejects_symlinked_files(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            outside = root / 'outside.token'
            outside.write_text('secret-token', encoding='utf-8')
            redirected = root / 'client-auth-token'
            redirected.symlink_to(outside)
            with mock.patch.dict(os.environ, {
                'RIPLEY_CLIENT_AUTH_TOKEN': '',
                'RIPLEY_CLIENT_AUTH_TOKEN_FILE': str(redirected),
            }, clear=False):
                self.assertEqual(heartbeat.client_auth_token(), '')


if __name__ == '__main__':
    unittest.main()
