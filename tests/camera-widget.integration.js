const assert = require('assert');
const fs = require('fs');
const path = require('path');
const vm = require('vm');

const root = path.join(__dirname, '..');
const playerSource = fs.readFileSync(path.join(root, 'static/js/player.js'), 'utf8');
const previewSource = fs.readFileSync(path.join(root, 'static/js/preview.js'), 'utf8');
const editorSource = fs.readFileSync(path.join(root, 'static/js/editor.js'), 'utf8');
const cachedRendererSource = fs.readFileSync(path.join(root, 'player-client/scripts/screentra-player-renderer.py'), 'utf8');

assert.match(playerSource, /function initCameraWidgets\(widgets, root\)/);
assert.match(playerSource, /capture_enabled === true/);
assert.match(playerSource, /width: \{ ideal: 1280, max: 1280 \}/);
assert.match(playerSource, /height: \{ ideal: 720, max: 720 \}/);
assert.match(playerSource, /frameRate: \{ ideal: 30, max: 30 \}/);
assert.match(previewSource, /initCameraWidgets\(layout\.widgets\|\|\[\], stage\)/);
assert.match(editorSource, /type: 'camera'/);
assert.match(editorSource, /w\.type === 'camera'/);
assert.match(cachedRendererSource, /t==='camera'/);
assert.match(cachedRendererSource, /capture_enabled/);

const context = {
  console,
  URLSearchParams,
  Date,
  Math,
  Number,
  String,
  JSON,
  encodeURIComponent,
  window: { location: { pathname: '/preview.html', search: '?preview=1' } },
  location: { pathname: '/preview.html', search: '?preview=1' },
  document: { documentElement: {}, querySelector: () => null, getElementById: () => null },
  ripleyWeatherIconSvg: () => '<svg></svg>',
  ripleyWeatherIconDescription: () => 'sample',
};
vm.runInNewContext(playerSource, context, { filename: 'player.js' });

const widget = {
  id: 'camera-1', widget_type: 'camera', x: 0, y: 0, w: 50, h: 50, z: 1,
  props: {},
};
const placeholder = context.buildWidgetHTML(widget);
assert.match(placeholder, /data-camera-widget-id="camera-1"/);
assert.match(placeholder, /Camera preview/);
assert.match(placeholder, /capture disabled/);
assert.doesNotMatch(placeholder, /<video/);

let captureCalls = 0;
const videoNode = {
  srcObject: null,
  setAttribute() {},
  play() { return Promise.resolve(); },
};
const rootNode = {
  querySelector(selector) {
    return selector === '[data-camera-widget-id="camera-1"]' ? videoNode : null;
  },
};
context.navigator = {
  mediaDevices: {
    getUserMedia(constraints) {
      captureCalls += 1;
      assert.strictEqual(JSON.stringify(constraints), JSON.stringify({
        video: {
          facingMode: { ideal: 'environment' },
          width: { ideal: 1280, max: 1280 },
          height: { ideal: 720, max: 720 },
          frameRate: { ideal: 30, max: 30 },
        },
        audio: false,
      }));
      return Promise.resolve({ id: 'local-camera-stream' });
    },
  },
};

context.initCameraWidgets([widget], rootNode);
assert.strictEqual(captureCalls, 0, 'camera capture must remain off by default');

context.initCameraWidgets([{ ...widget, props: { capture_enabled: true } }], rootNode);
setImmediate(() => {
  assert.strictEqual(captureCalls, 1, 'capture must require explicit opt-in');
  assert.deepStrictEqual(videoNode.srcObject, { id: 'local-camera-stream' });
  console.log('camera widget integration: contract passed');
});