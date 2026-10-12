#!/usr/bin/env node
'use strict';

const fs = require('fs');
const path = require('path');

const root = path.resolve(__dirname, '..');
const rateLimit = fs.readFileSync(path.join(root, 'src/rate_limit.rs'), 'utf8');
const db = fs.readFileSync(path.join(root, 'src/db.rs'), 'utf8');
const postgres = fs.readFileSync(path.join(root, 'src/postgres.rs'), 'utf8');
const api = fs.readFileSync(path.join(root, 'src/api.rs'), 'utf8');
const sqliteSchema = db.slice(db.indexOf('CREATE TABLE IF NOT EXISTS client_commands'));
const postgresSchema = fs.readFileSync(path.join(root, 'migrations/postgres/0001_initial.sql'), 'utf8');

function expect(condition, message) {
  if (!condition) throw new Error(message);
}

for (const action of ['restart_client', 'restart_kiosk', 'restart_renderer', 'update_client', 'reboot']) {
  expect(rateLimit.includes(`"${action}"`), `missing long cooldown action: ${action}`);
}
for (const action of ['ddc_set', 'cec_send', 'ir_send', 'audio_set_volume', 'bluetooth_power']) {
  expect(rateLimit.includes(`"${action}"`), `missing hardware cooldown action: ${action}`);
}
expect(rateLimit.includes('"reload_layout" | "refresh_cache" => 10'), 'missing short refresh cooldown');
expect(rateLimit.includes('"set_display_count" | "set_touch_binding" => 60'), 'missing display-control cooldown');
expect(rateLimit.includes('_ => 15'), 'missing default cooldown');
expect(sqliteSchema.includes('CREATE TABLE IF NOT EXISTS client_command_cooldowns'), 'missing SQLite cooldown table');
expect(postgresSchema.includes('CREATE TABLE IF NOT EXISTS client_command_cooldowns'), 'missing PostgreSQL cooldown table');
expect(db.includes('let tx = conn.transaction()'), 'SQLite queue must reserve cooldown and command in one transaction');
expect(db.includes('client_command_cooldowns WHERE client_id = ?1 AND action = ?2'), 'SQLite cooldown must be scoped by client and action');
expect(postgres.includes('WITH eligible AS'), 'PostgreSQL queue must reserve cooldown atomically');
expect(postgres.includes('client_command_cooldowns.last_requested_at'), 'PostgreSQL cooldown must compare persisted timestamps');
expect(api.includes('"client_command_cooldown"'), 'cooldown rejection must be audited separately');
expect(api.includes('"command cooldown active"'), 'cooldown audit detail must be fixed and bounded');
expect(api.includes('StatusCode::TOO_MANY_REQUESTS'), 'cooldown HTTP responses must be 429');
expect(api.includes('retry after {retry_after} seconds'), 'cooldown response must expose bounded retry guidance');

console.log('command_cooldown_contract=passed');
