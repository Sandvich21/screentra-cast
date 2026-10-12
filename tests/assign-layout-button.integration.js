const assert = require('assert');
const fs = require('fs');
const path = require('path');

const source = fs.readFileSync(path.join(__dirname, '..', 'static', 'js', 'editor.js'), 'utf8');

// The editor intentionally stores select values as layout:<id>. The assign
// endpoint expects the raw layout ID, not the UI prefix.
assert.match(source, /const layoutSelectValue = document\.getElementById\('layout-select'\)\?\.value/);
assert.match(source, /layoutSelectValue\.startsWith\('layout:'\)/);
assert.match(source, /layoutSelectValue\.slice\(7\)/);
assert.match(source, /API\.assignClientDisplayLayout\(clientId, displayIdx, layoutId\)/);

console.log('assign layout integration: UI prefix is stripped before API assignment');
