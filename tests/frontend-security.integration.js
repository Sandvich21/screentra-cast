const assert = require('node:assert/strict');
const fs = require('node:fs');

const app = fs.readFileSync('static/js/app.js', 'utf8');
const weather = fs.readFileSync('static/js/weather.js', 'utf8');
const touch = fs.readFileSync('static/js/touch-calibration.js', 'utf8');

// Status values used in CSS class positions must be reduced to a safe token.
assert.match(app, /replace\(\/\[\^a-z0-9_-\]\+\/g,'-'\)/);
assert.match(app, /safeOkLabel = String\(okLabel\)\.replace/);

// Diagnostic and provider error text must use text nodes, not HTML interpolation.
assert.match(weather, /app\.replaceChildren\(/);
assert.doesNotMatch(weather, /app\.innerHTML\s*=\s*`<div class="error">\$\{msg\}/);
assert.match(touch, /label\.textContent\s*=\s*`\$\{stage\}/);
assert.doesNotMatch(touch, /li\.innerHTML\s*=\s*`<b>\$\{stage\}/);

console.log('frontend stored-data rendering security checks passed');
