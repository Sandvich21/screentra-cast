const fs = require('fs');
const path = require('path');

const root = path.resolve(__dirname, '..');
const api = fs.readFileSync(path.join(root, 'src/api.rs'), 'utf8');
const db = fs.readFileSync(path.join(root, 'src/db.rs'), 'utf8');
const postgres = fs.readFileSync(path.join(root, 'src/postgres.rs'), 'utf8');
const migration = fs.readFileSync(path.join(root, 'migrations/postgres/0001_initial.sql'), 'utf8');
const installer = fs.readFileSync(path.join(root, 'static/install/player-client.sh'), 'utf8');

function assertIncludes(text, fragment, label) {
  if (!text.includes(fragment)) throw new Error(`${label}: missing ${fragment}`);
}

function assertNotIncludes(text, fragment, label) {
  if (text.includes(fragment)) throw new Error(`${label}: unexpected ${fragment}`);
}

assertIncludes(api, 'crate::postgres::apply_initial_schema().await?', 'PostgreSQL startup migration');
assertIncludes(api, 'X-SCREENTRA-CLIENT-TOKEN', 'client command credential header');
assertIncludes(api, 'headers: HeaderMap', 'client command header extraction');
assertIncludes(api, 'require_client_command_token(&repository, &id, &headers)', 'strict command authentication');
assertNotIncludes(api, 'SCREENTRA_ALLOW_LEGACY_COMMAND_POLLING', 'legacy polling environment gate should be retired');
assertNotIncludes(api, 'allow_legacy_without_token', 'legacy hashless polling parameter should be retired');
assertIncludes(api, 'client_auth_token_matching_accepts_own_token_and_rejects_invalid_or_other_client_tokens', 'credential matching tests');
assertIncludes(api, 'acknowledge_client_command', 'authenticated client command acknowledgement');
assertIncludes(api, '/api/clients/{id}/command/ack', 'client command acknowledgement route');
assertIncludes(api, 'client_auth_token: Option<String>', 'registration request token');
assertIncludes(api, 'client_auth_token_hash', 'server token hash wiring');
assertIncludes(api, 'Sha256', 'server token hashing');
assertIncludes(api, 'client authentication token required', 'registration authentication rejection');
assertIncludes(api, 'ConstantTimeEq', 'constant-time token comparison');
assertIncludes(api, 'get_client_auth_token_hash', 'server token lookup');
assertIncludes(db, 'client_auth_token_hash TEXT', 'SQLite client credential column');
assertIncludes(db, 'COALESCE(excluded.client_auth_token_hash, client_auth_token_hash)', 'SQLite credential preservation');
assertIncludes(postgres, 'client_auth_token_hash', 'PostgreSQL credential wiring');
assertIncludes(migration, 'client_auth_token_hash text', 'PostgreSQL client credential column');
assertIncludes(installer, 'client-auth-token', 'persistent client credential path');
assertIncludes(installer, 'secrets.token_urlsafe', 'client credential generation');
assertIncludes(installer, "'client_auth_token'", 'credential registration field');
assertIncludes(installer, 'command/ack', 'credentialed command acknowledgement');
assertIncludes(installer, 'X-SCREENTRA-CLIENT-TOKEN', 'credentialed command acknowledgement header');
assertIncludes(installer, "def poll_command(server, client_id, client_token)", 'credentialed command polling function');
assertIncludes(installer, 'os.chmod', 'credential file permission hardening');

console.log('client_credential_provisioning_contract=passed');
