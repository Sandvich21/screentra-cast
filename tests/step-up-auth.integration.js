#!/usr/bin/env node
const fs = require('fs');
const auth = fs.readFileSync('src/auth.rs', 'utf8');
const middleware = fs.readFileSync('src/middleware.rs', 'utf8');
const api = fs.readFileSync('src/api.rs', 'utf8');
const clientApi = fs.readFileSync('static/js/api.js', 'utf8');
const clientAuth = fs.readFileSync('static/js/auth.js', 'utf8');
const app = fs.readFileSync('static/js/app.js', 'utf8');
const index = fs.readFileSync('static/index.html', 'utf8');

function assert(condition, message) {
  if (!condition) throw new Error(message);
}

assert(auth.includes('issue_step_up_for'), 'step-up token issuer missing');
assert(auth.includes('verify_step_up'), 'step-up verifier missing');
assert(auth.includes('step_up_ttl'), 'step-up TTL policy missing');
assert(auth.includes('step_up_scope'), 'login re-authentication scope missing');
assert(auth.includes('clear_step_up'), 'step-up clear endpoint missing');

assert(auth.includes('HttpOnly; Secure; SameSite=Strict'), 'step-up cookie flags missing');
assert(auth.includes('pub async fn step_up'), 'step-up endpoint missing');
assert(auth.includes('get_active_password_hash'), 'step-up must verify the authenticated account password');
assert(auth.includes('step_up_authentication'), 'step-up audit event missing');
assert(auth.includes('step-up password verification failed'), 'step-up failure audit detail missing');
assert(!auth.includes('println!("{}", body.password'), 'step-up password must not be printed');
assert(middleware.includes('required_step_up_scope'), 'middleware step-up scope classifier missing');
assert(middleware.includes('server-update'), 'server-update step-up scope missing');
assert(middleware.includes('step-up authentication required'), 'middleware guard missing');
assert(api.includes('.route("/step-up", post(crate::auth::step_up))'), 'step-up route not wired');
assert(clientApi.includes('_stepUpDialog'), 'dashboard step-up dialog missing');
assert(!clientApi.includes('client-control'), 'client commands must not use step-up scopes');
assert(clientAuth.includes('credentials: \'same-origin\''), 'login request must be same-origin');
assert(clientApi.includes('credentials: init.credentials || \'same-origin\''), 'protected request retry must preserve same-origin cookies');
assert(!clientAuth.includes('ensureClientControl'), 'client-control preflight must be removed');
assert(!app.includes('API.ensureClientControl'), 'client-card publish must not preflight client authorization');
assert(middleware.includes('Client commands and display assignments'), 'client commands must not require step-up authentication');
assert(index.includes('/js/api.js?v=20261003233500'), 'api.js cache-busting marker missing');
assert(index.includes('/js/auth.js?v=20261003004000'), 'auth.js cache-busting marker missing');
assert(index.includes('/js/app.js?v=20261004012317'), 'app.js cache-busting marker missing');
console.log('step-up auth integration contract: ok');
