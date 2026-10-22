#!/usr/bin/env python3
import importlib.util
import json
import unittest
from copy import deepcopy
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
MODULE_PATH = ROOT / 'player-client/scripts/screentra-client-service-supervisor-plan.py'
MANIFEST_PATH = ROOT / 'addon-packages/magic-mirror-stt/manifest.json'


def load_supervisor_module():
    if not MODULE_PATH.is_file():
        return None
    spec = importlib.util.spec_from_file_location(
        'screentra_client_service_supervisor_plan', MODULE_PATH
    )
    module = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    spec.loader.exec_module(module)
    return module


supervisor = load_supervisor_module()
manifest = json.loads(MANIFEST_PATH.read_text(encoding='utf-8'))


def valid_request(**overrides):
    value = {
        'service_id': manifest['id'],
        'service_version': manifest['version'],
        'activation_id': 'activation-20261018-01',
        'state': 'staged',
        'intent': 'health',
        'service_metadata': {
            **deepcopy(manifest['client_service']),
            'service_version': manifest['version'],
        },
    }
    value.update(overrides)
    return value


class ClientServiceSupervisorPlanTests(unittest.TestCase):
    def require_module(self):
        self.assertIsNotNone(
            supervisor,
            'production supervisor-plan module is not implemented',
        )
        return supervisor

    def test_health_plan_binds_manifest_identity_and_has_no_runtime_evidence(self):
        module = self.require_module()
        request = valid_request(
            observation={
                'service_id': manifest['id'],
                'service_version': manifest['version'],
                'activation_id': 'activation-20261018-01',
                'status': 'ok',
                'model_id': manifest['client_service']['model_id'],
                'model_sha256': manifest['client_service']['model_sha256'],
            }
        )

        plan = module.build_supervisor_activation_plan(request)

        self.assertEqual(plan['plan_type'], 'client_service_activation_plan')
        self.assertEqual(plan['service_id'], manifest['id'])
        self.assertEqual(plan['service_version'], manifest['version'])
        self.assertEqual(plan['activation_id'], 'activation-20261018-01')
        self.assertEqual(plan['socket_name'], manifest['client_service']['socket_name'])
        self.assertEqual(plan['model_id'], manifest['client_service']['model_id'])
        self.assertEqual(plan['model_sha256'], manifest['client_service']['model_sha256'])
        self.assertEqual(plan['lifecycle_state'], 'staged')
        self.assertTrue(plan['ok'])
        self.assertEqual(plan['code'], 'metadata_only')
        self.assertFalse(plan['execution_enabled'])
        self.assertEqual(plan['effect'], 'metadata_only')
        self.assertFalse(plan['process_started'])
        self.assertFalse(plan['socket_bound'])
        self.assertFalse(plan['model_loaded'])
        self.assertEqual(plan['observation']['status'], 'ok')

    def test_mutating_intents_are_structured_execution_disabled_results_without_state_change(self):
        module = self.require_module()
        for intent in ('start', 'stop', 'update', 'rollback'):
            with self.subTest(intent=intent):
                request = valid_request(intent=intent, state='running')
                original = deepcopy(request)
                plan = module.build_supervisor_activation_plan(request)

                self.assertFalse(plan['ok'])
                self.assertEqual(plan['code'], 'execution_disabled')
                self.assertFalse(plan['execution_enabled'])
                self.assertEqual(plan['effect'], 'metadata_only')
                self.assertEqual(plan['lifecycle_state'], 'running')
                self.assertFalse(plan['lifecycle_state_changed'])
                self.assertFalse(plan['process_started'])
                self.assertFalse(plan['socket_bound'])
                self.assertFalse(plan['model_loaded'])
                self.assertEqual(request, original)

    def test_status_may_bind_only_a_bounded_health_observation(self):
        module = self.require_module()
        request = valid_request(
            intent='status',
            observation={
                'service_id': manifest['id'],
                'service_version': manifest['version'],
                'activation_id': 'activation-20261018-01',
                'status': 'degraded',
                'model_id': manifest['client_service']['model_id'],
                'model_sha256': manifest['client_service']['model_sha256'],
            },
        )

        plan = module.build_supervisor_activation_plan(request)

        self.assertTrue(plan['ok'])
        self.assertEqual(plan['code'], 'metadata_only')
        self.assertEqual(plan['observation']['status'], 'degraded')
        self.assertNotIn('argv', plan)
        self.assertNotIn('env', plan)
        self.assertNotIn('mounts', plan)
        self.assertNotIn('ports', plan)

    def test_service_metadata_requires_valid_image_and_resource_policy(self):
        module = self.require_module()
        base = deepcopy(manifest['client_service'])
        cases = [
            ({key: value for key, value in base.items() if key != 'image'},),
            ({key: value for key, value in base.items() if key != 'image_digest'},),
            ({**base, 'image': '/tmp/host-image'},),
            ({**base, 'image_digest': 'sha256:' + ('A' * 64)},),
            ({**base, 'cpu_millis': 0},),
            ({**base, 'memory_bytes': 1},),
            ({**base, 'pids': 0},),
            ({**base, 'writable_bytes': -1},),
            ({**base, 'cpu_millis': 4001},),
            ({**base, 'memory_bytes': 2 * 1024 * 1024 * 1024 + 1},),
            ({**base, 'pids': 257},),
            ({**base, 'writable_bytes': 512 * 1024 * 1024 + 1},),
        ]
        for (service,) in cases:
            with self.subTest(service=service), self.assertRaises(module.SupervisorPlanError):
                module.build_supervisor_activation_plan(
                    valid_request(service_metadata={**service, 'service_version': manifest['version']})
                )

        module = self.require_module()
        cases = [
            valid_request(service_id='other-service'),
            valid_request(service_version='9.9.9'),
            valid_request(
                service_metadata={
                    **manifest['client_service'],
                    'service_version': '9.9.9',
                }
            ),
            valid_request(
                service_metadata={
                    **manifest['client_service'],
                    'model_sha256': manifest['client_service']['model_sha256'].upper(),
                }
            ),
            valid_request(
                service_metadata={
                    **manifest['client_service'],
                    'socket_name': '/tmp/stt.sock',
                }
            ),
            valid_request(**{'argv': ['/bin/true']}),
            valid_request(**{'env': {'SECRET': 'nope'}}),
            valid_request(**{'command': ['/bin/true']}),
            valid_request(
                service_metadata={**manifest['client_service'], 'path': '/tmp/nope'}
            ),
            valid_request(
                service_metadata={**manifest['client_service'], 'arbitrary': True}
            ),
            valid_request(
                service_metadata={**manifest['client_service'], 'mounts': []}
            ),
            valid_request(
                service_metadata={**manifest['client_service'], 'ports': []}
            ),
            valid_request(
                service_metadata={**manifest['client_service'], 'credential': 'secret'}
            ),
            valid_request(
                service_metadata={**manifest['client_service'], 'audio': 'raw'}
            ),
            valid_request(intent='unknown'),
            valid_request(service_id='x' * 129),
            valid_request(activation_id='x' * 129),
            valid_request(
                service_metadata={
                    **manifest['client_service'],
                    'model_id': 'x' * 65,
                }
            ),
            valid_request(
                service_metadata={
                    **manifest['client_service'],
                    'socket_name': 'x' * 65,
                }
            ),
        ]
        for request in cases:
            with self.subTest(request=request), self.assertRaises(module.SupervisorPlanError):
                module.build_supervisor_activation_plan(request)

    def test_health_observation_rejects_identity_arbitrary_and_oversized_fields(self):
        module = self.require_module()
        base = valid_request(intent='health')
        cases = [
            {**base, 'observation': {
                'service_id': 'other-service',
                'service_version': manifest['version'],
                'activation_id': base['activation_id'],
                'status': 'ok',
                'model_id': manifest['client_service']['model_id'],
                'model_sha256': manifest['client_service']['model_sha256'],
            }},
            {**base, 'observation': {
                'service_id': manifest['id'],
                'service_version': manifest['version'],
                'activation_id': base['activation_id'],
                'status': 'OK',
                'model_id': manifest['client_service']['model_id'],
                'model_sha256': manifest['client_service']['model_sha256'],
            }},
            {**base, 'observation': {
                'service_id': manifest['id'],
                'service_version': manifest['version'],
                'activation_id': base['activation_id'],
                'status': 'ok',
                'model_id': manifest['client_service']['model_id'],
                'model_sha256': 'A' * 64,
            }},
            {**base, 'observation': {
                'service_id': manifest['id'],
                'service_version': manifest['version'],
                'activation_id': base['activation_id'],
                'status': 'ok',
                'model_id': manifest['client_service']['model_id'],
                'model_sha256': manifest['client_service']['model_sha256'],
                'path': '/tmp/nope',
            }},
            {**base, 'observation': 'x' * (module.MAX_REQUEST_BYTES + 1)},
        ]
        for request in cases:
            with self.subTest(request=request), self.assertRaises(module.SupervisorPlanError):
                module.build_supervisor_activation_plan(request)


if __name__ == '__main__':
    unittest.main()
