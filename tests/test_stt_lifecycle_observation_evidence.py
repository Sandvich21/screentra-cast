import copy
import dataclasses
import importlib.util
import sys
import unittest
from collections.abc import Mapping
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
LIFECYCLE_PATH = ROOT / 'player-client/scripts/screentra-stt-lifecycle-observation-evidence.py'
ADMISSION_PATH = ROOT / 'player-client/scripts/screentra-stt-runtime-admission.py'


def load_module(name, path):
    if not path.is_file():
        return None
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


lifecycle = load_module('screentra_stt_lifecycle_observation_evidence', LIFECYCLE_PATH)
admission = load_module('screentra_stt_runtime_admission', ADMISSION_PATH)


SERVICE_ID = 'magic-mirror-stt'
SERVICE_VERSION = '0.1.0'
ACTIVATION_ID = 'activation-20261018-01'


def valid_observation(**overrides):
    value = {
        'service_id': SERVICE_ID,
        'activation_id': ACTIVATION_ID,
        'service_version': SERVICE_VERSION,
        'observed_state': 'staged',
        'identity_observed': True,
        'observation_non_mutating': True,
    }
    value.update(overrides)
    return value


class DuplicateKeyMapping(Mapping):
    def __init__(self, pairs):
        self._pairs = pairs

    def __getitem__(self, key):
        for candidate, value in reversed(self._pairs):
            if candidate == key:
                return value
        raise KeyError(key)

    def __iter__(self):
        return (key for key, _ in self._pairs)

    def __len__(self):
        return len(self._pairs)


class STTLifecycleObservationEvidenceTests(unittest.TestCase):
    def require_lifecycle(self):
        self.assertIsNotNone(
            lifecycle,
            'production STT lifecycle-observation evidence module is not implemented',
        )
        return lifecycle

    def test_valid_observation_is_immutable_closed_and_inert(self):
        module = self.require_lifecycle()

        result = module.validate_stt_lifecycle_observation_evidence(valid_observation())

        self.assertTrue(dataclasses.is_dataclass(result))
        self.assertTrue(getattr(type(result), '__dataclass_params__').frozen)
        self.assertEqual(
            result.as_dict(),
            {
                'evidence_type': 'stt_lifecycle_observation_evidence',
                'service_id': SERVICE_ID,
                'activation_id': ACTIVATION_ID,
                'service_version': SERVICE_VERSION,
                'observed_state': 'staged',
                'identity_observed': True,
                'observation_non_mutating': True,
                'observation_valid': True,
                'execution_enabled': False,
                'runtime_activation_allowed': False,
                'lifecycle_changed': False,
                'process_started': False,
                'service_started': False,
                'effect': 'metadata_only',
            },
        )
        with self.assertRaises(dataclasses.FrozenInstanceError):
            result.observed_state = 'stopped'

    def test_allowlisted_non_running_states_and_boolean_rule(self):
        module = self.require_lifecycle()
        for state in ('staged', 'stopped', 'rollback_ready'):
            with self.subTest(state=state):
                result = module.validate_stt_lifecycle_observation_evidence(
                    valid_observation(observed_state=state)
                )
                self.assertTrue(result.observation_valid)
        for state in ('running', 'starting', 'stopping', 'failed', 'transition'):
            with self.subTest(state=state):
                with self.assertRaises(module.STTLifecycleObservationEvidenceError):
                    module.validate_stt_lifecycle_observation_evidence(
                        valid_observation(observed_state=state)
                    )
        for field in ('identity_observed', 'observation_non_mutating'):
            with self.subTest(field=field):
                result = module.validate_stt_lifecycle_observation_evidence(
                    valid_observation(**{field: False})
                )
                self.assertFalse(result.observation_valid)
                with self.assertRaises(module.STTLifecycleObservationEvidenceError):
                    module.validate_stt_lifecycle_observation_evidence(
                        valid_observation(**{field: 1})
                    )

    def test_identity_binding_closed_fields_duplicate_fields_and_unsafe_payloads_fail(self):
        module = self.require_lifecycle()
        for field, value in (
            ('service_id', 'other-service'),
            ('activation_id', 'other-activation'),
            ('service_version', '9.9.9'),
            ('service_id', '/tmp/service'),
            ('activation_id', 'https://example.invalid'),
        ):
            with self.subTest(field=field, value=value):
                with self.assertRaises(module.STTLifecycleObservationEvidenceError):
                    module.validate_stt_lifecycle_observation_evidence(
                        valid_observation(**{field: value}),
                        expected_service_id=SERVICE_ID,
                        expected_activation_id=ACTIVATION_ID,
                        expected_service_version=SERVICE_VERSION,
                    )

        for extra in (
            {'path': '/run/stt.sock'},
            {'url': 'https://example.invalid'},
            {'command': ['/bin/true']},
            {'credentials': 'redacted'},
            {'raw_audio': 'AA=='},
            {'model_bytes': 'AA=='},
            {'pid': 1234},
            {'container_id': 'container-1'},
            {'socket_name': 'stt.sock'},
            {'transition': 'start'},
            {'start': True},
        ):
            with self.subTest(extra=extra):
                with self.assertRaises(module.STTLifecycleObservationEvidenceError):
                    module.validate_stt_lifecycle_observation_evidence(
                        {**valid_observation(), **extra}
                    )

        pairs = list(valid_observation().items())
        pairs.append(('service_id', SERVICE_ID))
        with self.assertRaises(module.STTLifecycleObservationEvidenceError):
            module.validate_stt_lifecycle_observation_evidence(DuplicateKeyMapping(pairs))

    def test_oversized_or_raw_non_mapping_evidence_fails_closed(self):
        module = self.require_lifecycle()
        oversized = valid_observation(service_id='a' * (module.MAX_SERVICE_ID_BYTES + 1))
        with self.assertRaises(module.STTLifecycleObservationEvidenceError):
            module.validate_stt_lifecycle_observation_evidence(oversized)
        for value in (None, [], 'evidence'):
            with self.subTest(value=value):
                with self.assertRaises(module.STTLifecycleObservationEvidenceError):
                    module.validate_stt_lifecycle_observation_evidence(value)

    def test_admission_composes_only_exact_lifecycle_type_and_preserves_inert_flags(self):
        self.assertIsNotNone(admission, 'runtime admission module is not available')
        self.require_lifecycle()
        from test_stt_runtime_admission import valid_request

        evidence = lifecycle.validate_stt_lifecycle_observation_evidence(valid_observation())
        request = valid_request(lifecycle_observation_evidence=evidence)
        result = admission.decide_runtime_admission(request)

        self.assertEqual(result['admission'], 'approved_for_future_review')
        self.assertTrue(result['evidence_composed']['lifecycle_observation'])
        self.assertEqual(
            tuple(result['lifecycle_observation_identity_tuple']),
            (SERVICE_ID, SERVICE_VERSION, ACTIVATION_ID, 'staged'),
        )
        for field in (
            'execution_enabled',
            'runtime_activation_allowed',
            'lifecycle_changed',
            'lifecycle_state_changed',
            'process_started',
            'service_started',
        ):
            self.assertFalse(result[field], field)
        for forbidden in ('runtime_ready', 'readiness_claim', 'activation_ready'):
            self.assertNotIn(forbidden, result)

    def test_admission_rejects_raw_derived_tampered_and_mismatched_lifecycle_evidence(self):
        self.assertIsNotNone(admission, 'runtime admission module is not available')
        self.require_lifecycle()
        from test_stt_runtime_admission import valid_request

        valid = lifecycle.validate_stt_lifecycle_observation_evidence(valid_observation())
        for value in (valid.as_dict(), {'service_id': SERVICE_ID}, None):
            with self.subTest(value=value):
                with self.assertRaises(admission.RuntimeAdmissionError):
                    admission.decide_runtime_admission(
                        valid_request(lifecycle_observation_evidence=value)
                    )

        class DerivedLifecycleEvidence(lifecycle.STTLifecycleObservationEvidence):
            pass

        with self.assertRaises(admission.RuntimeAdmissionError):
            admission.decide_runtime_admission(
                valid_request(
                    lifecycle_observation_evidence=DerivedLifecycleEvidence(
                        **dataclasses.asdict(valid)
                    )
                )
            )

        for tampered in (
            dataclasses.replace(valid, service_id='other-service'),
            dataclasses.replace(valid, activation_id='other-activation'),
            dataclasses.replace(valid, service_version='9.9.9'),
            dataclasses.replace(valid, observed_state='stopped'),
            dataclasses.replace(valid, observation_valid=False),
            dataclasses.replace(valid, execution_enabled=True),
            dataclasses.replace(valid, lifecycle_changed=True),
            dataclasses.replace(valid, process_started=True),
            dataclasses.replace(valid, service_started=True),
        ):
            with self.subTest(tampered=tampered):
                with self.assertRaises(admission.RuntimeAdmissionError):
                    admission.decide_runtime_admission(
                        valid_request(lifecycle_observation_evidence=tampered)
                    )

    def test_admission_binds_lifecycle_identity_to_supervisor_installation_and_health(self):
        self.assertIsNotNone(admission, 'runtime admission module is not available')
        self.require_lifecycle()
        from test_stt_runtime_admission import valid_request

        valid = lifecycle.validate_stt_lifecycle_observation_evidence(valid_observation())
        for component, change in (
            ('supervisor_plan', {'service_id': 'other-service'}),
            ('runtime_installation_evidence', {'activation_id': 'other-activation'}),
            ('process_health_evidence', {'service_version': '9.9.9'}),
        ):
            with self.subTest(component=component):
                request = valid_request(lifecycle_observation_evidence=valid)
                original = request[component]
                if dataclasses.is_dataclass(original):
                    request[component] = dataclasses.replace(original, **change)
                else:
                    request[component] = copy.deepcopy(original)
                    request[component].update(change)
                with self.assertRaises(admission.RuntimeAdmissionError):
                    admission.decide_runtime_admission(request)


if __name__ == '__main__':
    unittest.main()
