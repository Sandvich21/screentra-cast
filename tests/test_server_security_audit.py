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

    def test_approval_and_external_command_errors_are_opaque(self):
        source = (ROOT / 'src/api.rs').read_text(encoding='utf-8')
        for marker, end_marker in (
            ('async fn create_addon_execution_approval(', 'async fn update_addon_execution_capabilities('),
            ('async fn approve_addon_execution_approval(', 'async fn revoke_addon_execution_approval('),
            ('async fn revoke_addon_execution_approval(', 'fn parse_addon_execution_target('),
            ('async fn approve_addon_execution_target(', 'async fn revoke_addon_execution_target('),
            ('async fn revoke_addon_execution_target(', 'async fn list_addon_widget_types('),
            ('async fn push_submissions_to_google_sheets(', 'async fn '),
        ):
            start = source.index(marker)
            end = source.index(end_marker, start + len(marker))
            block = source[start:end]
            self.assertNotIn('format!("Invalid approval request:', block, marker)
            self.assertNotIn('format!("Approval rejected:', block, marker)
            self.assertNotIn('format!("Revocation rejected:', block, marker)
            self.assertNotIn('format!("Target approval rejected:', block, marker)
            self.assertNotIn('format!("Target revocation rejected:', block, marker)
            self.assertNotIn('format!("Google Sheets push failed:', block, marker)
    def test_remaining_operational_error_responses_are_opaque(self):
        source = (ROOT / 'src/api.rs').read_text(encoding='utf-8')
        for marker, end_marker, forbidden in (
            ('async fn apply_github_update_handler(', 'struct PlayerClientBundleBuildResult', ('Err((StatusCode::INTERNAL_SERVER_ERROR, message))', 'StatusCode::BAD_GATEWAY, error')),
            ('async fn build_player_client_bundle_handler(', 'async fn get_system_time(', ('bundle builder missing:', 'server bundle builder missing:', 'download file not available:')),
            ('async fn get_system_time(', 'async fn set_timezone(', ('timedatectl:', 'timedatectl show failed:')),
        ):
            start = source.index(marker)
            end = source.index(end_marker, start + len(marker))
            for needle in forbidden:
                self.assertNotIn(needle, source[start:end], marker)

        source = (ROOT / 'src/api.rs').read_text(encoding='utf-8')
        self.assertNotIn('StatusCode::INTERNAL_SERVER_ERROR, format!', source)
        self.assertNotIn('format!("multipart:', source)
        self.assertIn('"Invalid multipart request"', source)


if __name__ == '__main__':
    unittest.main()