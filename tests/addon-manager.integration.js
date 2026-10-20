const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');

const root = path.resolve(__dirname, '..');
const appSource = fs.readFileSync(path.join(root, 'static/js/app.js'), 'utf8');
const apiSource = fs.readFileSync(path.join(root, 'static/js/api.js'), 'utf8');
const indexSource = fs.readFileSync(path.join(root, 'static/index.html'), 'utf8');

assert.match(appSource, /rollback_available/);
assert.match(appSource, /rollback_version/);
assert.match(appSource, /addon-rollback/);
assert.match(appSource, /API\.rollbackAddonPackage\(btn\.dataset\.id\)/);
assert.match(appSource, /Client execution remains disabled/);
assert.match(apiSource, /async rollbackAddonPackage\(id\)/);
assert.match(indexSource, /\/js\/api\.js\?v=20261020-addon-rollback-v1/);
assert.match(indexSource, /\/js\/app\.js\?v=20261020-addon-rollback-v1/);

console.log('addon manager integration: rollback is conditional, visible, and inert');
