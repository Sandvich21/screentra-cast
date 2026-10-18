#!/usr/bin/env node
'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');

const root = path.resolve(__dirname, '..');
const html = fs.readFileSync(path.join(root, 'static', 'index.html'), 'utf8');
const app = fs.readFileSync(path.join(root, 'static', 'js', 'app.js'), 'utf8');
const api = fs.readFileSync(path.join(root, 'static', 'js', 'api.js'), 'utf8');

function hasId(id) {
  return new RegExp(`id=["']${id}["']`).test(html);
}

// The selected-client status card must expose only the bounded service controls.
assert(hasId('addon-client-service-id'), 'service ID field is missing');
assert(hasId('addon-client-activation-id'), 'activation ID field is missing');
assert(hasId('btn-client-service-status'), 'service status button is missing');
assert(hasId('btn-client-service-rollback'), 'rollback assessment button is missing');
assert.match(html, /Request service status/);
assert.match(html, /Request rollback assessment/);
const serviceControls = html.slice(html.indexOf('Client service contract'), html.indexOf('</details>', html.indexOf('Client service contract')));
assert.doesNotMatch(serviceControls, /Start service|Stop service|Update service|arbitrary command/i);

// The API helper must use the existing same-origin root command route.
assert.match(api, /async sendClientCommand\(clientId, action, payload = \{\}\)/);
assert.match(api, /`\/api\/clients\/\$\{encodeURIComponent\(clientId\)\}\/command`/);
assert.match(api, /credentials: 'same-origin'/);
assert.match(api, /headers: \{ 'Content-Type': 'application\/json' \}/);

// The UI must queue only the two named service actions and validate identifiers
// before sending; acceptance must remain visibly queued/disabled, not executed.
assert.match(app, /client_service_status/);
assert.match(app, /client_service_rollback_request/);
assert.match(app, /execution remains disabled/);
assert.match(app, /Request accepted; client execution remains disabled/);
assert.match(app, /serviceIdPattern/);
assert.match(app, /API\.sendClientCommand\(selectedClientId, action/);
assert.match(app, /action === 'client_service_status'/);
assert.match(app, /action === 'client_service_rollback_request'/);
assert.match(app, /queueServiceRequest\('client_service_status'\)/);
assert.match(app, /queueServiceRequest\('client_service_rollback_request'\)/);
assert.doesNotMatch(app, /API\.sendClientCommand\s*=/);
assert.doesNotMatch(app, /innerHTML\s*=\s*[^;]*addon-client-service-id/);
assert.doesNotMatch(app, /onclick\s*=\s*["']/i);

console.log('client service controls integration: bounded queued controls are wired');
