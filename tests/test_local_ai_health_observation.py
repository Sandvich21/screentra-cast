#!/usr/bin/env python3
import importlib.util
import json
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
MODULE_PATH = ROOT / 'player-client/scripts/screentra-local-ai-health-observation.py'
spec = importlib.util.spec_from_file_location('screentra_local_ai_health_observation', MODULE_PATH)
observation = importlib.util.module_from_spec(spec)
assert spec.loader is not None
sys.modules[spec.name] = observation
spec.loader.exec_module(observation)


SERVICE_ID = 'magic-mirror-stt'
SERVICE_VERSION = '1.2.3'
ACTIVATION_ID = 'activation-7'
MODEL_ID = 'whisper-tiny.en'
MODEL_SHA256 = 'a' * 64


def valid_observation(**overrides):
    value = {
        'service_id': SERVICE_ID,
        'service_version': SERVICE_VERSION,
        'activation_id': ACTIVATION_ID,
        'status': 'ok',
    }
    value.update(overrides)
    return value


class LocalAiHealthObservationTests(unittest.TestCase):
    def test_matching_mapping_returns_metadata_only_bound_observation(self):
        observed = valid_observation(model_id=MODEL_ID, model_sha256=MODEL_SHA256)

        result = observation.bind_health_observation(
            observed,
            expected_service_id=SERVICE_ID,
            expected_service_version=SERVICE_VERSION,
            expected_activation_id=ACTIVATION_ID,
            expected_model_id=MODEL_ID,
            expected_model_sha256=MODEL_SHA256,
        )

        self.assertIsInstance(result, observation.HealthObservation)
        self.assertEqual(result.as_dict(), observed)
        self.assertTrue(result.metadata_only)
        self.assertEqual(result.effect, 'metadata_only')
        self.assertNotIn('state', result.as_dict())
        self.assertNotIn('staged', result.as_dict())
        self.assertNotIn('loaded', result.as_dict())
        self.assertNotIn('running', result.as_dict())
        self.assertNotIn('verified_on_disk', result.as_dict())

    def test_serialized_json_is_decoded_and_preserved_without_runtime_claims(self):
        observed = valid_observation(status='degraded')

        result = observation.bind_health_observation(
            json.dumps(observed, separators=(',', ':')).encode('utf-8'),
            expected_service_id=SERVICE_ID,
            expected_service_version=SERVICE_VERSION,
            expected_activation_id=ACTIVATION_ID,
        )

        self.assertEqual(result.status, 'degraded')
        self.assertEqual(result.to_json(), json.dumps(observed, separators=(',', ':'), sort_keys=True))
        self.assertEqual(result.effect, 'metadata_only')

    def test_only_bounded_health_statuses_are_accepted(self):
        for status in ('ok', 'degraded'):
            with self.subTest(status=status):
                result = observation.bind_health_observation(
                    valid_observation(status=status),
                    expected_service_id=SERVICE_ID,
                    expected_service_version=SERVICE_VERSION,
                    expected_activation_id=ACTIVATION_ID,
                )
                self.assertEqual(result.status, status)

        for status in ('running', 'staged', 'loaded', 'healthy', '', 'OK'):
            with self.subTest(status=status), self.assertRaises(observation.HealthObservationError):
                observation.bind_health_observation(
                    valid_observation(status=status),
                    expected_service_id=SERVICE_ID,
                    expected_service_version=SERVICE_VERSION,
                    expected_activation_id=ACTIVATION_ID,
                )

    def test_service_identity_mismatches_are_rejected_without_mutating_observation(self):
        for field, expected in (
            ('service_id', 'other-service'),
            ('service_version', '9.9.9'),
            ('activation_id', 'other-activation'),
        ):
            observed = valid_observation()
            with self.subTest(field=field), self.assertRaises(observation.HealthObservationError):
                observation.bind_health_observation(
                    observed,
                    expected_service_id=expected if field == 'service_id' else SERVICE_ID,
                    expected_service_version=expected if field == 'service_version' else SERVICE_VERSION,
                    expected_activation_id=expected if field == 'activation_id' else ACTIVATION_ID,
                )
            self.assertEqual(observed, valid_observation())

    def test_expected_model_identity_mismatch_is_rejected(self):
        observed = valid_observation(model_id=MODEL_ID, model_sha256=MODEL_SHA256)

        with self.assertRaises(observation.HealthObservationError):
            observation.bind_health_observation(
                observed,
                expected_service_id=SERVICE_ID,
                expected_service_version=SERVICE_VERSION,
                expected_activation_id=ACTIVATION_ID,
                expected_model_id='other-model',
                expected_model_sha256=MODEL_SHA256,
            )
        with self.assertRaises(observation.HealthObservationError):
            observation.bind_health_observation(
                observed,
                expected_service_id=SERVICE_ID,
                expected_service_version=SERVICE_VERSION,
                expected_activation_id=ACTIVATION_ID,
                expected_model_id=MODEL_ID,
                expected_model_sha256='b' * 64,
            )

    def test_model_identity_is_optional_but_observed_pair_is_required(self):
        result = observation.bind_health_observation(
            valid_observation(),
            expected_service_id=SERVICE_ID,
            expected_service_version=SERVICE_VERSION,
            expected_activation_id=ACTIVATION_ID,
        )
        self.assertIsNone(result.model_id)
        self.assertIsNone(result.model_sha256)

        for partial in (
            {'model_id': MODEL_ID},
            {'model_sha256': MODEL_SHA256},
        ):
            with self.subTest(partial=partial), self.assertRaises(observation.HealthObservationError):
                observation.bind_health_observation(
                    valid_observation(**partial),
                    expected_service_id=SERVICE_ID,
                    expected_service_version=SERVICE_VERSION,
                    expected_activation_id=ACTIVATION_ID,
                )

    def test_expected_model_identity_must_also_be_paired(self):
        for kwargs in (
            {'expected_model_id': MODEL_ID},
            {'expected_model_sha256': MODEL_SHA256},
        ):
            with self.subTest(kwargs=kwargs), self.assertRaises(observation.HealthObservationError):
                observation.bind_health_observation(
                    valid_observation(model_id=MODEL_ID, model_sha256=MODEL_SHA256),
                    expected_service_id=SERVICE_ID,
                    expected_service_version=SERVICE_VERSION,
                    expected_activation_id=ACTIVATION_ID,
                    **kwargs,
                )

    def test_model_id_and_digest_rules_are_strict(self):
        for model_id in ('', '../model', 'model/name', 'model name', 'x' * 65, 'é-model'):
            with self.subTest(model_id=model_id), self.assertRaises(observation.HealthObservationError):
                observation.bind_health_observation(
                    valid_observation(model_id=model_id, model_sha256=MODEL_SHA256),
                    expected_service_id=SERVICE_ID,
                    expected_service_version=SERVICE_VERSION,
                    expected_activation_id=ACTIVATION_ID,
                )

        for digest in ('A' * 64, 'g' * 64, 'a' * 63, 'a' * 65, 'sha256:' + MODEL_SHA256):
            with self.subTest(digest=digest), self.assertRaises(observation.HealthObservationError):
                observation.bind_health_observation(
                    valid_observation(model_id=MODEL_ID, model_sha256=digest),
                    expected_service_id=SERVICE_ID,
                    expected_service_version=SERVICE_VERSION,
                    expected_activation_id=ACTIVATION_ID,
                )

    def test_command_path_credentials_audio_and_arbitrary_fields_are_rejected(self):
        for forbidden in ('command', 'path', 'password', 'token', 'credential', 'audio', 'arbitrary'):
            with self.subTest(forbidden=forbidden), self.assertRaises(observation.HealthObservationError):
                observation.bind_health_observation(
                    valid_observation(**{forbidden: 'secret'}),
                    expected_service_id=SERVICE_ID,
                    expected_service_version=SERVICE_VERSION,
                    expected_activation_id=ACTIVATION_ID,
                )

    def test_errors_are_bounded_and_do_not_echo_observed_or_expected_values(self):
        observed_secret = 'secret-service-id'
        expected_secret = 'expected-service-id'
        with self.assertRaises(observation.HealthObservationError) as raised:
            observation.bind_health_observation(
                valid_observation(service_id=observed_secret),
                expected_service_id=expected_secret,
                expected_service_version=SERVICE_VERSION,
                expected_activation_id=ACTIVATION_ID,
            )

        message = str(raised.exception)
        self.assertLessEqual(len(message.encode('utf-8')), observation.MAX_ERROR_BYTES)
        self.assertNotIn(observed_secret, message)
        self.assertNotIn(expected_secret, message)

    def test_duplicate_json_fields_and_non_object_serialized_values_are_rejected(self):
        duplicate = (
            '{"service_id":"magic-mirror-stt","service_id":"other",'
            '"service_version":"1.2.3","activation_id":"activation-7",'
            '"status":"ok"}'
        ).encode('utf-8')
        with self.assertRaises(observation.HealthObservationError):
            observation.bind_health_observation(
                duplicate,
                expected_service_id=SERVICE_ID,
                expected_service_version=SERVICE_VERSION,
                expected_activation_id=ACTIVATION_ID,
            )
        with self.assertRaises(observation.HealthObservationError):
            observation.bind_health_observation(
                b'[]',
                expected_service_id=SERVICE_ID,
                expected_service_version=SERVICE_VERSION,
                expected_activation_id=ACTIVATION_ID,
            )


if __name__ == '__main__':
    unittest.main()
