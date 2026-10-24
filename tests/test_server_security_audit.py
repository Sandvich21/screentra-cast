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

    def test_privileged_and_player_error_responses_are_opaque(self):
        source = (ROOT / 'src/api.rs').read_text(encoding='utf-8')
        for marker, end_marker in (
            ('async fn create_user(', 'async fn update_user('),
            ('async fn update_user(', 'async fn delete_user('),
            ('async fn get_client_command(', 'async fn clear_client_command('),
            ('async fn acknowledge_client_command(', 'async fn register_client('),
        ):
            start = source.index(marker)
            end = source.index(end_marker, start)
            block = source[start:end]
            self.assertNotIn('StatusCode::INTERNAL_SERVER_ERROR, e', block, marker)
            self.assertNotIn('StatusCode::INTERNAL_SERVER_ERROR, error', block, marker)

    def test_infrastructure_error_responses_do_not_expose_details(self):
        source = (ROOT / 'src/api.rs').read_text(encoding='utf-8')
        self.assertNotIn('StatusCode::INTERNAL_SERVER_ERROR, format!', source)
        self.assertNotIn('format!("multipart:', source)
        self.assertIn('"Invalid multipart request"', source)


if __name__ == '__main__':
    unittest.main()
