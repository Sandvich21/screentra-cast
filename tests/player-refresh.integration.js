const assert = require('assert');
const fs = require('fs');
const path = require('path');
const vm = require('vm');

const source = fs.readFileSync(path.join(__dirname, '..', 'static', 'js', 'player.js'), 'utf8');
const previewSource = fs.readFileSync(path.join(__dirname, '..', 'static', 'js', 'preview.js'), 'utf8');
const editorSource = fs.readFileSync(path.join(__dirname, '..', 'static', 'js', 'editor.js'), 'utf8');
const editorCss = fs.readFileSync(path.join(__dirname, '..', 'static', 'css', 'editor.css'), 'utf8');
const interactiveEditorCss = fs.readFileSync(path.join(__dirname, '..', 'static', 'css', 'interactive-editor.css'), 'utf8');

assert.match(source, /data-ripley-widget-id/);
assert.match(source, /async function refreshCurrentDataWidgets\(connector, layout\)/);
assert.match(source, /node\.innerHTML = ripleyTemplateElementsHTML/);
assert.match(source, /Refresh only weather\/calendar template contents/);
assert.match(source, /active_layout_revision/);
assert.match(source, /:revision:/);
assert.match(source, /async function fetchAndCacheLayout\(layoutId, forceFetch\)/);
assert.match(source, /if \(!forceFetch && layoutCache\[layoutId\]\)/);
assert.match(source, /fetchAndCacheLayout\(targetId, true\)/);
assert.match(source, /initVideoWidgets\(target\.widgets \|\| \[\], root\)/);
assert.match(source, /initVideoWidgets\(target\.widgets \|\| \[\], rg\)/);
assert.match(source, /sxRenderSingle\(layoutId, myConnector, true\)/);
assert.doesNotMatch(source, /currentContentKey = null; renderLoop\(\)/);
assert.match(previewSource, /function preparePreviewVideoPlayback\(root\)/);
assert.match(previewSource, /video\.muted=true/);
assert.match(previewSource, /video\.setAttribute\('controls',''\)/);
assert.match(previewSource, /preparePreviewVideoPlayback\(stage\)/);
assert.doesNotMatch(previewSource, /x:\(Number\(w\.x\)\|\|0\)\/100/);
assert.match(previewSource, /buildWidgetHTML accepts the API's percentage geometry directly/);
assert.match(source, /Preview playback must be muted before the first play\(\) call/);
assert.match(source, /state\.node\.load\(\)/);
assert.match(source, /node\.addEventListener\('canplay'/);
assert.match(source, /function initVideoWidgets\(widgets, root\)/);
assert.match(editorSource, /resetWidgetRotation\(\)/);
assert.match(editorSource, /data-action="reset-rotation"/);
assert.match(editorSource, /widget\.props\.rotation = 0/);
assert.match(editorSource, /data-editor-countdown-id/);
assert.match(editorSource, /_formatEditorCountdown\(widget\)/);
assert.doesNotMatch(editorSource, /12m 34s/);
const editorContext = {
  Date: class extends Date {
    constructor(...args) { super(args.length ? args[0] : '2026-01-01T00:00:00Z'); }
    static now() { return Date.parse('2026-01-01T00:00:00Z'); }
  },
  window: {},
  document: {},
};
vm.runInNewContext(`${editorSource}; globalThis.__Editor = Editor;`, editorContext);
assert.strictEqual(editorContext.__Editor._formatEditorCountdown({ props: {
  countdown_mode: 'date', targetDate: '2026-01-01T01:05:00Z', expiry_text: 'Expired',
} }), '1h 5m');

assert.match(editorSource, /class="rotation-handle"/);
assert.match(editorSource, /startAngle:/);
assert.match(editorSource, /this\.rotationState\.rotation/);
assert.match(editorSource, /node\.style\.transform = `rotate\(/);
assert.match(interactiveEditorCss, /\.interactive-widget \{[^}]*overflow:visible/);
assert.match(editorCss, /\.interactive-widget-label \{[\s\S]*visibility:\s*hidden/);
assert.match(editorCss, /\.interactive-widget\.selected \.interactive-widget-label \{[\s\S]*visibility:\s*visible/);
assert.doesNotMatch(editorCss, /\.interactive-widget:hover \.interactive-widget-label/);

// Execute the production source in preview mode and verify the shared widget
// wrapper exposes a stable identity for in-place updates.
const context = {
  console,
  URLSearchParams,
  window: { location: { pathname: '/preview.html', search: '' } },
  location: { pathname: '/preview.html', search: '' },
  document: { documentElement: {}, querySelector: () => null, getElementById: () => null },
  ripleyWeatherIconSvg: () => '<svg></svg>',
  ripleyWeatherIconDescription: () => 'sample',
};
vm.runInNewContext(source, context, { filename: 'player.js' });
const html = context.buildWidgetHTML({
  id: 'weather-1',
  widget_type: 'image',
  x: 0,
  y: 0,
  w: 100,
  h: 100,
  z: 1,
  props: { url: '/uploads/images/sample.png', fit: 'contain' },
});
assert.match(html, /data-ripley-widget-id="weather-1"/);
assert.match(html, /data-ripley-widget-type="image"/);

const videoHtml = context.buildWidgetHTML({
  id: 'video-1',
  widget_type: 'video',
  x: 0,
  y: 0,
  w: 100,
  h: 100,
  z: 1,
  props: { url: '/uploads/videos/sample.mp4', muted: false },
});
assert.match(videoHtml, /<video[^>]+src="\/uploads\/videos\/sample\.mp4"/);
const playlistVideoHtml = context.buildWidgetHTML({
  id: 'playlist-video-1',
  widget_type: 'video',
  x: 0,
  y: 0,
  w: 100,
  h: 100,
  z: 1,
  props: { playlist: [{ url: '/uploads/videos/sample.mp4' }], loop: true },
});
assert.match(playlistVideoHtml, /data-video-widget-id="playlist-video-1"/);
assert.match(playlistVideoHtml, /autoplay playsinline preload="auto" loop /);

const rotatedHtml = context.buildWidgetHTML({
  id: 'rotated-1',
  widget_type: 'image',
  x: 10,
  y: 10,
  w: 20,
  h: 10,
  z: 1,
  props: { content: 'Rotated', rotation: 45 },
});
assert.match(rotatedHtml, /transform:rotate\(45deg\)/);
assert.match(rotatedHtml, /transform-origin:center center/);

const videoNode = {
  src: '',
  currentTime: 0,
  duration: 10,
  onended: null,
  ontimeupdate: null,
  onerror: null,
  playCount: 0,
  load() {},
  pause() {},
  play() { this.playCount += 1; return Promise.resolve(); },
  setAttribute() {},
  addEventListener(event, handler) {
    if (event === 'ended') this.onended = handler;
    if (event === 'timeupdate') this.ontimeupdate = handler;
  },
  removeEventListener() {},
};
context.initVideoWidgets([{ id: 'playlist-1', widget_type: 'video', props: {
  playlist: [{ url: '/one.mp4' }, { url: '/two.mp4' }], loop: true, shuffle: false,
} }], { querySelector: () => videoNode });
assert.match(videoNode.src, /^\/one\.mp4\?ripley_video=/);
videoNode.onended();
assert.match(videoNode.src, /^\/two\.mp4\?ripley_video=/);
videoNode.currentTime = videoNode.duration;
videoNode.ontimeupdate();
assert.match(videoNode.src, /^\/one\.mp4\?ripley_video=/);

console.log('player refresh integration: in-place data refresh contract passed');
