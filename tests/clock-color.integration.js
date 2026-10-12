#!/usr/bin/env node
const assert = require('assert');
const fs = require('fs');
const path = require('path');
const vm = require('vm');

const playerSource = fs.readFileSync(path.join(__dirname, '..', 'static', 'js', 'player.js'), 'utf8');
assert.match(playerSource, /var clkColor =/, 'player must pass clock color into the iframe URL');

const clockSource = fs.readFileSync(path.join(__dirname, '..', 'static', 'js', 'clock.js'), 'utf8');
const clock = { style: {}, textContent: '' };
const context = {
  URLSearchParams,
  window: { location: { search: '?format=HH%3Amm&color=%23ff00aa&font_size=72&font_family=sans-serif' } },
  document: { getElementById: (id) => id === 'clock' ? clock : null },
  Date: class extends Date { constructor() { super('2026-01-01T12:34:00Z'); } },
  setInterval: () => {},
};
vm.runInNewContext(clockSource, context, { filename: 'clock.js' });
assert.strictEqual(clock.style.color, '#ff00aa', 'clock renderer must apply the configured color');
assert.strictEqual(clock.style.fontSize, '72px');

console.log('clock color integration: player passes color and iframe applies it');
