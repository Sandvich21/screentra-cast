const assert = require('assert');
const fs = require('fs');
const path = require('path');

const api = fs.readFileSync(path.join(__dirname, '..', 'src', 'api.rs'), 'utf8');
const postgres = fs.readFileSync(path.join(__dirname, '..', 'src', 'postgres.rs'), 'utf8');

function assertIncludes(text, needle, message) {
  assert(text.includes(needle), `${message}: missing ${needle}`);
}

assertIncludes(api, 'async fn audit_update_import_failure', 'update import failure audit helper');
assertIncludes(api, 'async fn audit_update_export_failure', 'update export failure audit helper');
assertIncludes(api, 'audit_update_import_failure(&state, &principal, "multipart rejected")', 'multipart parse failure audit');
assertIncludes(api, '"bundle field read failed"', 'bundle read failure audit');
assertIncludes(api, '"bundle file missing"', 'missing bundle audit');
assertIncludes(api, '"bundle field read failed"', 'bundle staging/read failure audit');
assertIncludes(api, 'audit_update_export_failure(&state, &principal, "bundle export failed")', 'export failure audit');
assertIncludes(api, 'authenticated_audit_listing_reads_bounded_update_failure_events', 'authenticated audit listing test');
assertIncludes(api, 'disposable_sqlite_invalid_update_failure_is_audited', 'disposable SQLite update failure test');
assertIncludes(api, 'assert_eq!(result.0[0].detail.as_deref(), Some("bundle file missing"))', 'sanitized audit detail assertion');
assertIncludes(api, 'postgres_authenticated_audit_listing_matches_sqlite_sanitization', 'disposable PostgreSQL audit test');
assertIncludes(postgres, 'filter(|c| !c.is_control())', 'PostgreSQL audit control-character sanitization');
assertIncludes(postgres, 'let detail = detail.map(|value| bounded(value, 512));', 'PostgreSQL audit detail bound');
assertIncludes(api, 'error.starts_with("parse manifest:")', 'manifest parse audit classification');
assertIncludes(api, 'error.starts_with("required runtime dependency missing:")', 'runtime dependency audit classification');
assertIncludes(api, 'error.starts_with("could not inspect update archive entry types")', 'archive entry inspection audit classification');

console.log('update_audit_lifecycle_contract=passed');
