#!/usr/bin/env node
'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

const root = path.resolve(__dirname, '..');
const appSource = fs.readFileSync(path.join(root, 'static', 'js', 'app.js'), 'utf8');

// Execute the production payload builder in isolation so this regression test
// does not need to construct the full schedule modal DOM.
const match = appSource.match(/function buildPowerSchedulePayload\([\s\S]*?\n}\n/);
assert(match, 'power schedule payload builder is missing');
const context = {};
vm.runInNewContext(`${match[0]}\nglobalThis.__buildPowerSchedulePayload = buildPowerSchedulePayload;`, context, {
  filename: 'static/js/app.js',
});

const build = context.__buildPowerSchedulePayload;
const selectedDisplay = {
  value: '2',
  dataset: { connector: 'HDMI-A-2' },
  textContent: 'HDMI-A-2',
};
const payload = build({
  targetClient: 'client-a',
  displayOption: selectedDisplay,
  action: 'display_sleep',
  controlMethod: 'dpms',
});

assert.equal(payload.action, 'display_sleep');
assert.equal(payload.power_state, 'sleep');
assert.equal(payload.display_index, 2);
assert.equal(payload.display, 'client-a:HDMI-A-2');
assert.ok(payload.display.length > 0 && payload.display.length <= 256);
assert.equal(payload.display_name, 'HDMI-A-2');

const fallback = build({
  targetClient: 'client-a',
  displayOption: { value: '7', dataset: {}, textContent: '' },
  action: 'power_off',
  controlMethod: 'auto',
});
assert.equal(fallback.display, 'client-a:Display 7');
assert.equal(fallback.display_name, 'Display 7');

assert.equal(build({
  targetClient: '',
  displayOption: selectedDisplay,
  action: 'power_on',
  controlMethod: 'auto',
}), null, 'missing client must be rejected');
assert.equal(build({
  targetClient: 'client-a',
  displayOption: { value: '', dataset: { connector: 'HDMI-A-2' }, textContent: 'HDMI-A-2' },
  action: 'power_on',
  controlMethod: 'auto',
}), null, 'missing display must be rejected');

console.log('power schedule payload integration: display identifier and power state are bounded and validated');
