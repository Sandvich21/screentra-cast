#!/usr/bin/env node
'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

const root = path.resolve(__dirname, '..');
const appSource = fs.readFileSync(path.join(root, 'static/js/app.js'), 'utf8');
const rollbackCalls = [];
const rollbackButtons = [];
const addonContainer = {
  _html: '',
  set innerHTML(value) { this._html = String(value); },
  get innerHTML() { return this._html; },
  querySelectorAll(selector) {
    if (selector === '.addon-rollback') return rollbackButtons;
    return [];
  },
};

const addons = [
  {
    id: 'no.rollback',
    name: 'No Rollback',
    version: '1.0.0',
    widget_types: [],
    enabled: false,
    description: '',
    kind: 'declarative',
    rollback_available: false,
    rollback_version: null,
  },
  {
    id: 'declarative.weather',
    name: '<img src=x onerror=alert(1)>',
    version: '0.2.0',
    widget_types: [],
    enabled: false,
    description: '<script>alert(1)</script>',
    kind: 'declarative',
    rollback_available: true,
    rollback_version: '0.1.0',
  },
  {
    id: 'client.extension',
    name: 'Client Extension',
    version: '0.2.0',
    widget_types: [],
    enabled: false,
    description: '',
    kind: 'client_extension',
    rollback_available: true,
    rollback_version: '0.1.0',
  },
];

const api = {
  listAddons: async () => addons,
  rollbackAddon: async (id) => rollbackCalls.push(['declarative', id]),
  rollbackAddonPackage: async (id) => rollbackCalls.push(['client_extension', id]),
};

const document = {
  readyState: 'loading',
  addEventListener() {},
  querySelector() { return null; },
  querySelectorAll() { return []; },
  getElementById(id) {
    return id === 'addons-list' ? addonContainer : null;
  },
};

const context = {
  document,
  navigator: {},
  console,
  API: api,
  localStorage: { getItem: () => null },
  location: { href: 'https://localhost/' },
  setTimeout,
  clearTimeout,
  setInterval,
  clearInterval,
  alert() {},
};
context.window = context;
context.globalThis = context;

vm.runInNewContext(`${appSource}\nthis.__App = App;`, context, {
  filename: 'static/js/app.js',
});

(async () => {
  const app = context.__App;
  assert.ok(app, 'production App object should load in the DOM harness');
  await app.loadAddons();

  assert.equal(
    (addonContainer.innerHTML.match(/addon-rollback/g) || []).length,
    2,
    'rollback control is rendered only for addons with retained state',
  );
  assert.match(
    addonContainer.innerHTML,
    /data-id="declarative\.weather" data-kind="declarative"/,
  );
  assert.match(
    addonContainer.innerHTML,
    /data-id="client\.extension" data-kind="client_extension"/,
  );
  assert.match(addonContainer.innerHTML, /&lt;img src=x onerror=alert\(1\)&gt;/);
  assert.doesNotMatch(addonContainer.innerHTML, /<img src=x onerror=alert\(1\)>/);
  assert.match(addonContainer.innerHTML, /&lt;script&gt;alert\(1\)&lt;\/script&gt;/);
  assert.match(addonContainer.innerHTML, /Client execution remains disabled/);

  rollbackButtons.splice(0, rollbackButtons.length,
    { dataset: { id: 'declarative.weather', kind: 'declarative' } },
    { dataset: { id: 'client.extension', kind: 'client_extension' } },
  );
  await app.loadAddons();
  app.confirm = async () => true;
  await rollbackButtons[0].onclick();
  await rollbackButtons[1].onclick();

  assert.deepEqual(rollbackCalls, [
    ['declarative', 'declarative.weather'],
    ['client_extension', 'client.extension'],
  ]);
  console.log('addon manager runtime integration: conditional controls and kind-aware rollback dispatch work');
  process.exit(0);
})().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
