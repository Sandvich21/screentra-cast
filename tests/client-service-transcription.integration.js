#!/usr/bin/env node
'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

const root = path.resolve(__dirname, '..');
const html = fs.readFileSync(path.join(root, 'static', 'index.html'), 'utf8');
const appSource = fs.readFileSync(path.join(root, 'static', 'js', 'app.js'), 'utf8');
const apiSource = fs.readFileSync(path.join(root, 'static', 'js', 'api.js'), 'utf8');

function assertInputContract(id) {
  const input = html.match(new RegExp(`<input[^>]*id=["']${id}["'][^>]*>`));
  assert(input, `${id} input is missing`);
  assert.match(input[0], /maxlength=["']128["']/);
  assert.match(input[0], /pattern=["'][^"']*\[A-Za-z0-9._-\][^"']*["']/);
}

assertInputContract('addon-client-service-id');
assertInputContract('addon-client-transcription-request-id');
assert.match(html, /Request push-to-talk transcription/);
assert.match(html, /execution (?:is )?(?:currently )?disabled/i);
assert.match(appSource, /client_service_transcribe_request/);
assert.match(appSource, /push_to_talk: true/);
assert.match(apiSource, /client_service_transcribe_request/);
assert.match(apiSource, /credentials: 'same-origin'/);
assert.doesNotMatch(appSource, /getUserMedia|MediaRecorder|audio\.blob|uploadAudio/i);

class Element {
  constructor(id) {
    this.id = id;
    this.dataset = {};
    this.style = {};
    this.value = '';
    this.disabled = false;
    this.innerHTML = '';
    this.textContent = '';
    this.onclick = null;
    this.oninput = null;
    this.onchange = null;
  }
  focus() {}
  addEventListener() {}
  querySelectorAll() { return []; }
}

const ids = [
  'addon-client-status-select',
  'addon-client-status',
  'addon-client-service-id',
  'addon-client-activation-id',
  'addon-client-service-options',
  'addon-client-activation-options',
  'btn-client-service-status',
  'btn-client-service-rollback',
  'btn-client-service-refresh',
  'btn-client-service-transcription',
  'addon-client-transcription-request-id',
  'addon-client-service-command-status',
];
const elements = new Map(ids.map(id => [id, new Element(id)]));
const commands = [];
const context = {
  console,
  Promise,
  Date,
  setTimeout,
  clearTimeout,
  window: { addEventListener() {} },
  esc: (value) => String(value ?? '').replace(/[&<>"'`]/g, (c) => ({
    '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;', '`': '&#96;',
  }[c])),
  document: {
    readyState: 'loading',
    addEventListener() {},
    querySelectorAll() { return []; },
    querySelector() { return null; },
    getElementById(id) { return elements.get(id) || null; },
  },
  localStorage: { getItem() { return null; }, setItem() {} },
  API: {
    setupStatus: async () => ({ setup_required: false }),
    status: async () => ({ authenticated: true }),
    getClients: async () => [{ id: 'client-a', enrolled: true, name: 'Client A' }],
    getClientAddons: async () => ({
      status: {
        report: {
          addons: [],
          capabilities: [],
          services: [{
            id: 'speech-to-text',
            state: 'disabled',
            model_id: 'whisper-tiny.en',
            model_sha256: 'abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789',
          }],
        },
      },
    }),
    sendClientCommand: async (clientId, action, payload) => {
      commands.push({ clientId, action, payload });
      return { queued: true, requested_at: '2026-10-18T18:00:00Z' };
    },
  },
  Editor: { init() {} },
};

vm.runInNewContext(`${appSource}\n;globalThis.__App = App;`, context, {
  filename: 'static/js/app.js',
});

(async () => {
  await context.__App.loadAddonClientStatuses();
  const select = elements.get('addon-client-status-select');
  select.value = 'client-a';
  await select.onchange();
  assert.match(elements.get('addon-client-status').innerHTML, /model whisper-tiny\.en/);
  assert.match(elements.get('addon-client-status').innerHTML, /verified SHA-256 abcdef012345/);

  elements.get('addon-client-service-id').value = 'speech-to-text';
  elements.get('addon-client-transcription-request-id').value = 'bad request id';
  await elements.get('btn-client-service-transcription').onclick();
  assert.equal(commands.length, 0);
  assert.match(elements.get('addon-client-service-command-status').textContent, /valid push-to-talk request ID/i);

  elements.get('addon-client-transcription-request-id').value = 'request-001';
  await elements.get('btn-client-service-transcription').onclick();

  assert.equal(JSON.stringify(commands), JSON.stringify([{
    clientId: 'client-a',
    action: 'client_service_transcribe_request',
    payload: {
      service_id: 'speech-to-text',
      request_id: 'request-001',
      push_to_talk: true,
    },
  }]));
  assert.match(elements.get('addon-client-service-command-status').textContent, /queued/i);
  assert.match(elements.get('addon-client-service-command-status').textContent, /execution remains disabled/i);
  console.log('client service transcription integration: bounded queued DOM control works');
})().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
