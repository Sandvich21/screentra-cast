from pathlib import Path
import unittest


ROOT = Path(__file__).resolve().parents[1]


class ServerSecurityAuditTests(unittest.TestCase):
    def test_initial_setup_does_not_return_raw_repository_errors(self):
        source = (ROOT / 'src/api.rs').read_text(encoding='utf-8')
        marker = 'async fn create_initial_admin('
        start = source.index(marker)
        end = source.index('\n#[derive(Debug, Serialize)]', start)
        block = source[start:end]
        self.assertIn('else {\n                internal_server_error()\n            }', block)
        self.assertNotIn('StatusCode::INTERNAL_SERVER_ERROR, e', block)


if __name__ == '__main__':
    unittest.main()
