const assert = require('node:assert/strict');
const fs = require('node:fs');

const source = fs.readFileSync('static/js/editor.js', 'utf8');

// API-controlled client/display labels must be inserted as DOM option nodes,
// not interpolated into innerHTML.
assert.match(source, /clientSel\.replaceChildren\(new Option/);
assert.match(source, /displaySel\.replaceChildren\(new Option/);
assert.doesNotMatch(source, /clientSel\.innerHTML\s*=.*\$\{/);
assert.doesNotMatch(source, /displaySel\.innerHTML\s*=.*\$\{/);

// Stored widget color and geometry values must not become raw CSS text.
assert.match(source, /color:\$\{this\.esc\(p\.color/);
assert.match(source, /const width = Math\.max\(0\.1, Math\.min\(100, Number\(w\.w\)/);
assert.match(source, /const height = Math\.max\(0\.1, Math\.min\(100, Number\(w\.h\)/);

console.log('editor stored-data rendering security checks passed');
