#!/usr/bin/env node
'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

const source = fs.readFileSync(path.join(__dirname, '..', 'static', 'js', 'api.js'), 'utf8');
const calls = [];
const fakeFetch = async (input, init = {}) => {
  calls.push({ input, method: init.method || 'GET', body: init.body });
  return { ok: true, json: async () => ({ ok: true }), text: async () => '' };
};
const window = {
  fetch: fakeFetch,
  location: { href: 'https://localhost/' },
  localStorage: { getItem: () => null },
};
const context = {
  window,
  fetch: fakeFetch,
  Headers,
  URL,
  FormData,
  Blob,
  console,
  globalThis: window,
};
vm.runInNewContext(source, context, { filename: 'static/js/api.js' });

(async () => {
  await window.API.installAddonPackage(new Blob(['package']));
  await window.API.updateAddonPackage(new Blob(['package-v2']));
  await window.API.rollbackAddonPackage('example.client');
  assert.deepEqual(calls.map(call => [call.input, call.method]), [
    ['/api/addons/package', 'POST'],
    ['/api/addons/package', 'PUT'],
    ['/api/addons/package/example.client/rollback', 'POST'],
  ]);
  console.log('addon package API integration: install, update, and rollback methods use bounded routes');
})().catch(error => {
  console.error(error);
  process.exitCode = 1;
});
