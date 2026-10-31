const assert = require('assert');
const fs = require('fs');
const path = require('path');
const vm = require('vm');

const source = fs.readFileSync(path.join(__dirname, '..', 'static', 'js', 'player.js'), 'utf8');
const start = source.indexOf('function parseVideoWaitAfter');
const end = source.indexOf('function initVideoWidgets', start);
assert(start >= 0 && end > start, 'video wait helper must be part of the production player runtime');
const helpers = source.slice(start, end);
const exported = new Function(`${helpers}; return {parseVideoWaitAfter, videoWaitAfterSeconds};`)();

assert.deepStrictEqual(exported.parseVideoWaitAfter('5secs'), {min: 5, max: 5});
assert.deepStrictEqual(exported.parseVideoWaitAfter('1-10secs'), {min: 1, max: 10});
assert.deepStrictEqual(exported.parseVideoWaitAfter(' 2.5 - 4 s '), {min: 2.5, max: 4});
assert.deepStrictEqual(exported.parseVideoWaitAfter('invalid'), {min: 0, max: 0});
assert.strictEqual(exported.videoWaitAfterSeconds('1-10secs', () => 0), 1);
assert.strictEqual(exported.videoWaitAfterSeconds('1-10secs', () => 0.999999), 10);
assert.strictEqual(exported.videoWaitAfterSeconds('8', () => 0.5), 8);

const editor = fs.readFileSync(path.join(__dirname, '..', 'static', 'js', 'editor.js'), 'utf8');
assert.match(editor, /data-video-wait-index/);
assert.match(editor, /Wait after/);
assert.match(editor, /wait_after/);

const cachedRenderer = fs.readFileSync(path.join(__dirname, '..', 'player-client', 'scripts', 'screentra-player-renderer.py'), 'utf8');
assert.match(cachedRenderer, /videoWaitAfterSeconds/);
assert.match(cachedRenderer, /wait_after/);

console.log('video playlist wait-after integration: fixed and randomized delays are covered');
