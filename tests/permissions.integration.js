#!/usr/bin/env node
const fs = require('fs');
const middleware = fs.readFileSync('src/middleware.rs', 'utf8');

function assert(condition, message) {
  if (!condition) throw new Error(message);
}

const capabilities = [
  'manage_users', 'manage_backups', 'view_audit', 'server_update',
  'server_admin', 'manage_submissions', 'manage_layouts', 'publish_layout',
  'manage_presets', 'manage_files', 'manage_fonts', 'manage_schedules',
  'control_displays', 'manage_clients', 'assign_clients', 'control_clients',
  'control_system', 'viewer'
];
for (const capability of capabilities) {
  assert(middleware.includes(`"${capability}"`), `missing capability: ${capability}`);
}
assert(middleware.includes('fn required_capability'), 'central capability classifier missing');
assert(middleware.includes('fn role_has_capability'), 'central role capability matrix missing');
assert(middleware.includes('capability required'), 'capability denial response missing');
assert(middleware.includes('Fail closed for any new state-changing endpoint'), 'fail-closed policy missing');
assert(!middleware.includes('fn required_role'), 'legacy broad role classifier still active');
assert(!middleware.includes('fn role_allows'), 'legacy broad role matrix still active');
console.log('permissions integration contract: ok');
