const assert = require('assert');
const fs = require('fs');
const path = require('path');
const vm = require('vm');

const root = path.join(__dirname, '..');
const playerSource = fs.readFileSync(path.join(root, 'static/js/player.js'), 'utf8');
const previewSource = fs.readFileSync(path.join(root, 'static/js/preview.js'), 'utf8');
const editorSource = fs.readFileSync(path.join(root, 'static/js/editor.js'), 'utf8');
const cachedRendererSource = fs.readFileSync(path.join(root, 'player-client/scripts/screentra-player-renderer.py'), 'utf8');
const apiSource = fs.readFileSync(path.join(root, 'src/api.rs'), 'utf8');
const middlewareSource = fs.readFileSync(path.join(root, 'src/middleware.rs'), 'utf8');
const editorApiSource = fs.readFileSync(path.join(root, 'static/js/api.js'), 'utf8');
const cameraManifest = JSON.parse(fs.readFileSync(path.join(root, 'addon-packages/camera-widget/manifest.json'), 'utf8'));

assert.match(playerSource, /function initCameraWidgets\(widgets, root\)/);
assert.match(playerSource, /capture_enabled === true/);
assert.match(playerSource, /width: \{ ideal: 1280, max: 1280 \}/);
assert.match(playerSource, /height: \{ ideal: 720, max: 720 \}/);
assert.match(playerSource, /frameRate: \{ ideal: 30, max: 30 \}/);
assert.match(playerSource, /grayscale\(1\)/);
assert.match(playerSource, /sepia\(1\)/);
assert.match(playerSource, /hue-rotate\(300deg\)/);
assert.match(previewSource, /initCameraWidgets\(layout\.widgets\|\|\[\], stage\)/);
assert.match(editorSource, /const builtinLabels = new Map/);
assert.match(editorSource, /\.filter\(addon => builtinLabels\.get\(addon\.id\) !== addon\.name\)/);
assert.match(editorSource, /type: `addon:\$\{addon\.addon_id\}:\$\{addon\.id\}`/);
assert.match(editorSource, /filter: 'none'/);
assert.match(editorSource, /data-prop="filter"/);
assert.match(editorSource, /value="halloween"/);
assert.match(cachedRendererSource, /t==='camera'/);
assert.match(cachedRendererSource, /capture_enabled/);
assert.match(cachedRendererSource, /grayscale\(1\)/);
assert.match(cachedRendererSource, /sepia\(1\)/);
assert.match(cachedRendererSource, /hue-rotate\(300deg\)/);
assert.match(cachedRendererSource, /camera-stream\.mjpeg/);
assert.match(playerSource, /stallLimit = 90000/);
assert.match(playerSource, /node\.src = 'about:blank'/);
assert.match(playerSource, /getImageData\(0, 0, 16, 9\)/);
assert.match(cachedRendererSource, /stallLimit=90000/);
assert.match(cachedRendererSource, /node\.src='about:blank'/);
assert.match(cachedRendererSource, /getImageData\(0,0,16,9\)/);
assert.match(cachedRendererSource, /rtsp_transport/);
assert.match(cachedRendererSource, /'-timeout', '10000000'/);
assert.match(cachedRendererSource, /output_fps/);
assert.match(cachedRendererSource, /output_width/);
assert.match(cachedRendererSource, /frame_interpolation/);
assert.match(cachedRendererSource, /minterpolate=fps=/);
assert.doesNotMatch(cachedRendererSource, /'-rw_timeout'/);
assert.match(playerSource, /function previewCameraAuthToken\(\)/);
assert.match(playerSource, /startAuthenticatedCameraPreview/);
assert.match(playerSource, /localStorage\.getItem\('signage\.adminToken'\)/);
assert.match(playerSource, /\/api\/layouts\/.*\/player/);
assert.match(previewSource, /initCameraStreamWidgets\(layout\.widgets\|\|\[\], stage\)/);
assert.match(apiSource, /async fn camera_stream_mjpeg/);
assert.match(apiSource, /redact_camera_stream_sources/);
assert.match(apiSource, /multipart\/x-mixed-replace/);
assert.match(middlewareSource, /"\/camera-stream\.mjpeg"/);
assert.match(editorApiSource, /\/api\/layouts\/\$\{id\}\/edit/);
assert.match(cachedRendererSource, /def _camera_layout_has_stream\(cache_dir: Path\)/);
assert.match(cachedRendererSource, /function cachedLayoutPercent\(value\)/);
assert.match(cachedRendererSource, /cachedLayoutPercent\(Number\(w\.x\)\|\|0\)/);
assert.match(cachedRendererSource, /def _camera_public_layout\(layout: dict\)/);
assert.match(cachedRendererSource, /props\.pop\('stream_url', None\)/);
assert.match(cachedRendererSource, /Never send the source URL/);
assert.strictEqual(cameraManifest.name, 'RTSP widget');
assert.strictEqual(cameraManifest.widget_types.length, 1);
assert.strictEqual(cameraManifest.widget_types[0].id, 'rtsp-stream');
assert.strictEqual(cameraManifest.widget_types[0].name, 'RTSP widget');
assert.strictEqual(cameraManifest.widget_types[0].host_renderer, 'camera');
assert.deepStrictEqual(cameraManifest.widget_types[0].schema.properties.stream_url.pattern, '^rtsps?://');
assert.strictEqual(cameraManifest.widget_types[0].schema.properties.output_fps.maximum, 30);
assert.strictEqual(cameraManifest.widget_types[0].schema.properties.output_width.maximum, 1920);
assert.strictEqual(cameraManifest.widget_types[0].schema.properties.frame_interpolation.type, 'boolean');

const context = {
  console,
  URLSearchParams,
  Date,
  Math,
  Number,
  String,
  JSON,
  encodeURIComponent,
  window: { location: { hash: '', pathname: '/preview.html', search: '?preview=1' } },
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
assert.doesNotMatch(placeholder, /data-camera-filter-overlay/);

const cameraMarkup = (filter) => context.buildWidgetHTML({
  ...widget,
  props: { capture_enabled: true, filter },
});
assert.match(cameraMarkup('none'), /filter:none/);
assert.match(cameraMarkup('grayscale'), /filter:grayscale\(1\)/);
assert.match(cameraMarkup('sepia'), /filter:sepia\(1\)/);
assert.match(cameraMarkup('halloween'), /filter:saturate\(1\.35\) contrast\(1\.15\) hue-rotate\(300deg\)/);
assert.match(cameraMarkup('halloween'), /data-camera-filter-overlay="halloween"/);
assert.match(cameraMarkup('not-css'), /filter:none/);
assert.doesNotMatch(cameraMarkup('not-css'), /not-css/);
const streamMarkup = context.buildWidgetHTML({
  ...widget,
  props: { _addon_type: 'rtsp-stream', stream_url: 'rtsps://camera.invalid/live', fit: 'contain' },
});
assert.match(streamMarkup, /rstp widget/);
assert.doesNotMatch(streamMarkup, /camera\.invalid/);
assert.doesNotMatch(streamMarkup, /rtsps?:/);

const serverStreamMarkup = context.buildWidgetHTML({
  ...widget,
  _layout_id: 'layout-1',
  props: { _addon_type: 'rtsp-stream', stream_url: 'rtsps://camera.invalid/live', fit: 'contain' },
});
assert.match(serverStreamMarkup, /camera-stream\.mjpeg\?layout_id=layout-1/);
assert.match(serverStreamMarkup, /widget_id=camera-1/);
assert.doesNotMatch(serverStreamMarkup, /camera\.invalid/);
assert.doesNotMatch(serverStreamMarkup, /rtsps?:/);

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