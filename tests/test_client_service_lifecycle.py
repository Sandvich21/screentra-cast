#!/usr/bin/env python3
import importlib.util
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
MODULE_PATH = ROOT / 'player-client/scripts/screentra-client-service-lifecycle.py'
spec = importlib.util.spec_from_file_location('screentra_client_service_lifecycle', MODULE_PATH)
lifecycle = importlib.util.module_from_spec(spec)
assert spec.loader is not None
spec.loader.exec_module(lifecycle)


class ClientServiceLifecycleContractTests(unittest.TestCase):
    def record(self, **overrides):
        value = {
            'service_id': 'speech-to-text',
            'service_version': '1.2.3',
            'activation_id': 'activation-20261018-01',
            'state': 'staged',
        }
        value.update(overrides)
        return value

    def transition(self, record, target, **kwargs):
        return lifecycle.transition_service(record, target, **kwargs)

    def test_stage_start_running_is_metadata_only(self):
        starting = self.transition(self.record(), 'starting')
        self.assertEqual(starting['state'], 'starting')
        self.assertFalse(starting['execution_enabled'])
        self.assertEqual(starting['effect'], 'metadata_only')

        running = self.transition(starting, 'running')
        self.assertEqual(running['state'], 'running')
        self.assertEqual(running['service_id'], 'speech-to-text')
        self.assertEqual(running['activation_id'], 'activation-20261018-01')
        self.assertEqual(running['service_version'], '1.2.3')

    def test_degraded_and_failed_paths_are_explicit(self):
        running = self.record(state='running')
        degraded = self.transition(running, 'degraded')
        self.assertEqual(degraded['state'], 'degraded')
        failed = self.transition(degraded, 'failed')
        self.assertEqual(failed['state'], 'failed')
        self.assertFalse(failed['execution_enabled'])

    def test_failed_service_can_be_marked_rollback_ready_without_claiming_rollback(self):
        failed = self.record(state='failed')
        rollback = {
            'status': 'ready',
            'target_service_id': 'speech-to-text',
            'target_service_version': '1.2.2',
            'target_activation_id': 'activation-20261017-09',
            'reason': 'health check failed',
        }
        ready = self.transition(failed, 'rollback_ready', rollback=rollback)
        self.assertEqual(ready['state'], 'rollback_ready')
        self.assertEqual(ready['rollback'], {**rollback, 'applied': False})
        self.assertFalse(ready['rollback']['applied'])
        self.assertEqual(ready['service_version'], '1.2.3')
        self.assertEqual(ready['activation_id'], 'activation-20261018-01')

    def test_stopped_and_rollback_ready_are_terminal(self):
        stopped = self.transition(self.record(), 'stopped')
        self.assertEqual(stopped['state'], 'stopped')
        with self.assertRaises(lifecycle.LifecycleContractError):
            self.transition(stopped, 'starting')

        ready = self.transition(
            self.record(state='failed'),
            'rollback_ready',
            rollback={
                'status': 'ready',
                'target_service_id': 'speech-to-text',
                'target_service_version': '1.2.2',
                'target_activation_id': 'activation-20261017-09',
                'reason': 'failed health check',
            },
        )
        with self.assertRaises(lifecycle.LifecycleContractError):
            self.transition(ready, 'staged')

    def test_identity_binding_is_preserved_and_mismatch_is_rejected(self):
        record = self.record()
        transitioned = self.transition(
            record,
            'starting',
            expected_service_id='speech-to-text',
            expected_activation_id='activation-20261018-01',
        )
        self.assertEqual(transitioned['service_id'], record['service_id'])
        self.assertEqual(transitioned['activation_id'], record['activation_id'])
        for kwargs in (
            {'expected_service_id': 'other-service'},
            {'expected_activation_id': 'activation-other'},
        ):
            with self.subTest(kwargs=kwargs), self.assertRaises(lifecycle.LifecycleContractError):
                self.transition(record, 'starting', **kwargs)

    def test_unknown_states_and_invalid_transitions_are_rejected(self):
        with self.assertRaises(lifecycle.LifecycleContractError):
            self.transition(self.record(state='mystery'), 'starting')
        with self.assertRaises(lifecycle.LifecycleContractError):
            self.transition(self.record(), 'running')
        with self.assertRaises(lifecycle.LifecycleContractError):
            self.transition(self.record(state='running'), 'staged')
        with self.assertRaises(lifecycle.LifecycleContractError):
            self.transition(self.record(state='failed'), 'running')
        with self.assertRaises(lifecycle.LifecycleContractError):
            self.transition(self.record(), 'starting', execution_enabled=True)

    def test_malformed_identity_and_versions_are_rejected(self):
        for field, values in (
            ('service_id', ('', '../speech', 'speech/to-text', 'x' * 129)),
            ('activation_id', ('', 'activation other', 'x' * 129)),
            ('service_version', ('latest', '1.2', '1.2.3/evil', 'x' * 65)),
        ):
            for value in values:
                with self.subTest(field=field, value=repr(value)), self.assertRaises(lifecycle.LifecycleContractError):
                    lifecycle.validate_service_record(self.record(**{field: value}))

    def test_duplicate_and_oversized_service_metadata_are_rejected(self):
        with self.assertRaises(lifecycle.LifecycleContractError):
            lifecycle.validate_service_records([
                self.record(),
                self.record(activation_id='activation-20261018-02'),
            ])
        with self.assertRaises(lifecycle.LifecycleContractError):
            lifecycle.validate_service_record(
                self.record(metadata={'diagnostic': 'x' * lifecycle.MAX_METADATA_BYTES})
            )
        with self.assertRaises(lifecycle.LifecycleContractError):
            lifecycle.validate_service_record(self.record(extra='not-allowed'))

    def test_rollback_metadata_is_required_bounded_and_identity_bound(self):
        failed = self.record(state='failed')
        base = {
            'status': 'ready',
            'target_service_id': 'speech-to-text',
            'target_service_version': '1.2.2',
            'target_activation_id': 'activation-20261017-09',
            'reason': 'failed health check',
        }
        for update in (
            {'status': 'applied'},
            {'target_service_id': 'other-service'},
            {'target_service_version': 'latest'},
            {'target_activation_id': 'activation other'},
            {'reason': 'x' * (lifecycle.MAX_ROLLBACK_REASON_BYTES + 1)},
        ):
            with self.subTest(update=update), self.assertRaises(lifecycle.LifecycleContractError):
                self.transition(failed, 'rollback_ready', rollback={**base, **update})
        with self.assertRaises(lifecycle.LifecycleContractError):
            self.transition(failed, 'rollback_ready')


if __name__ == '__main__':
    unittest.main()
