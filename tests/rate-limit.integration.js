#!/usr/bin/env node
const fs = require('fs');
const assert = require('assert');

const rateLimit = fs.readFileSync('src/rate_limit.rs', 'utf8');
const api = fs.readFileSync('src/api.rs', 'utf8');

assert.match(rateLimit, /POLICY_CLIENT_CONTROL:\s*Policy\s*=\s*Policy::new\(30,\s*Duration::from_secs\(60\)\)/);
assert.match(api, /\("client_control", crate::rate_limit::POLICY_CLIENT_CONTROL\)/);
assert.match(api, /"rate_limit_reject"/);
assert.match(api, /"high-impact rate limit exceeded"/);
assert.match(api, /header::RETRY_AFTER/);

console.log('rate limit integration contract: ok');
