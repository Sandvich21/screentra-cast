const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');

const root = path.resolve(__dirname, '..');
const appSource = fs.readFileSync(path.join(root, 'static/js/app.js'), 'utf8');
const apiSource = fs.readFileSync(path.join(root, 'static/js/api.js'), 'utf8');

assert.match(appSource, /rollback_available/);
assert.match(appSource, /rollback_version/);
assert.match(appSource, /addon-rollback/);
assert.match(appSource, /API\.rollbackAddonPackage\(btn\.dataset\.id\)/);
assert.match(appSource, /Client execution remains disabled/);
assert.match(apiSource, /async rollbackAddonPackage\(id\)/);

console.log('addon manager integration: rollback is conditional, visible, and inert');
