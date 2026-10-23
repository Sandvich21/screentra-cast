import importlib.util
import unittest
from pathlib import Path


MODULE_PATH = Path(__file__).parents[1] / "player-client/scripts/screentra-discover.py"
spec = importlib.util.spec_from_file_location("screentra_discover", MODULE_PATH)
assert spec and spec.loader
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class DiscoveryHelperTests(unittest.TestCase):
    def test_accepts_only_verified_https_discovery_records(self):
        ca = "-----BEGIN CERTIFICATE-----\nCA\n-----END CERTIFICATE-----\n"
        record = module.validate_record({
            "protocol": module.DISCOVERY_PROTOCOL,
            "server_name": "Signage",
            "url": "https://10.0.0.25:3000",
            "ca_pem_b64": module.base64.b64encode(ca.encode()).decode(),
            "ca_sha256": module.sha256_bytes(ca.encode()),
        })
        self.assertEqual(record["url"], "https://10.0.0.25:3000")
        self.assertTrue(record["ca_pem_b64"])

    def test_rejects_duplicate_discovery_json_fields(self):
        with self.assertRaises(ValueError):
            module._parse_discovery_json(
                '{"protocol":"screentra-cast-discovery-v1",'
                '"protocol":"other"}'
            )

    def test_rejects_unknown_discovery_fields(self):
        ca = "-----BEGIN CERTIFICATE-----\nCA\n-----END CERTIFICATE-----\n"
        record = {
            "protocol": module.DISCOVERY_PROTOCOL,
            "server_name": "Signage",
            "url": "https://10.0.0.25:3000",
            "ca_pem_b64": module.base64.b64encode(ca.encode()).decode(),
            "ca_sha256": module.sha256_bytes(ca.encode()),
            "unexpected": "ignored?",
        }
        self.assertIsNone(module.validate_record(record))

    def test_rejects_nonstandard_json_constants(self):
        with self.assertRaises(ValueError):
            module._parse_discovery_json('{"protocol":NaN}')

    def test_rejects_oversized_discovery_record_names(self):
        ca = "-----BEGIN CERTIFICATE-----\\nCA\\n-----END CERTIFICATE-----\\n"
        record = {
            "protocol": module.DISCOVERY_PROTOCOL,
            "server_name": "é" * 128,
            "url": "https://10.0.0.25:3000",
            "ca_pem_b64": module.base64.b64encode(ca.encode()).decode(),
            "ca_sha256": module.sha256_bytes(ca.encode()),
        }
        self.assertIsNone(module.validate_record(record))

    def test_rejects_http_or_mismatched_ca(self):
        ca = "-----BEGIN CERTIFICATE-----\nCA\n-----END CERTIFICATE-----\n"
        base = {
            "protocol": module.DISCOVERY_PROTOCOL,
            "server_name": "Signage",
            "url": "https://10.0.0.25:3000",
            "ca_pem_b64": module.base64.b64encode(ca.encode()).decode(),
            "ca_sha256": module.sha256_bytes(ca.encode()),
        }
        self.assertIsNone(module.validate_record({**base, "url": "http://10.0.0.25:3000"}))
        self.assertIsNone(module.validate_record({**base, "ca_sha256": "0" * 64}))


if __name__ == "__main__":
    unittest.main()
