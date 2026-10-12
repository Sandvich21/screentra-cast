#!/usr/bin/env node
'use strict';

const fs = require('fs');
const path = require('path');

const root = path.resolve(__dirname, '..');
const api = fs.readFileSync(path.join(root, 'src/api.rs'), 'utf8');
const player = fs.readFileSync(path.join(root, 'static/install/player-client.sh'), 'utf8');

const events = [
  'client_command_create',
  'client_command_persist',
  'client_command_poll',
  'client_command_claim',
  'client_command_execute',
  'client_command_ack',
  'client_command_expire',
  'client_command_clear',
  'client_command_result',
];

for (const event of events) {
  if (!api.includes(`"${event}"`)) {
    throw new Error(`missing audit event ${event}`);
  }
}

if (!api.includes('fn sanitized_command_result')) {
  throw new Error('command results are not sanitized before retention');
}
if (!api.includes('heartbeat result persistence failed')) {
  throw new Error('failed command-result persistence is not audited');
}
if (!api.includes('command persistence failed')) {
  throw new Error('failed command persistence is not audited');
}
if (!player.includes("result['requested_at'] = spec.get('requested_at')")) {
  throw new Error('player result does not preserve command correlation metadata');
}
if (!player.includes('command-result.json uploaded and cleared')) {
  throw new Error('player result acknowledgement/retention cleanup is missing');
}

// Audit detail must use fixed metadata, not complete command/result payloads.
if (!api.includes('Some("command persistence failed")')
  || !api.includes('Some("heartbeat result persistence failed")')) {
  throw new Error('audit failure details are not fixed metadata');
}

console.log('command audit lifecycle integration: ok');
