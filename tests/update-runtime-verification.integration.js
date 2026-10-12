#!/usr/bin/env node
const assert = require('assert');
const fs = require('fs');
const path = require('path');

const appSource = fs.readFileSync(path.join(__dirname, '..', 'static', 'js', 'app.js'), 'utf8');

assert.match(appSource, /ripley-pending-update-commit/, 'update import must persist the expected build identity');
assert.match(appSource, /getVersion\(\)/, 'system page must read the running build identity after restart');
assert.match(appSource, /running build identity verified/, 'system page must report successful post-restart verification');
assert.match(appSource, /does not match expected/, 'system page must report a mismatched running build identity');
assert.match(appSource, /result\.manifest && result\.manifest\.git_commit/, 'import must use the signed manifest commit');

console.log('update runtime verification integration: expected commit is checked after restart');
