const assert = require('assert');
const fs = require('fs');
const path = require('path');
const vm = require('vm');

const playerSource = fs.readFileSync(path.join(__dirname, '..', 'static', 'js', 'player.js'), 'utf8');
const context = {
  console,
  fetch: () => Promise.resolve({ ok: false, json: async () => ({}) }),
  URLSearchParams,
  window: { location: { pathname: '/preview.html', search: '' } },
  location: { pathname: '/preview.html', search: '' },
  document: { documentElement: { dataset: {} }, querySelector: () => null, getElementById: () => null },
  ripleyWeatherIconSvg: () => '<svg></svg>',
  ripleyWeatherIconDescription: () => 'sample',
};
vm.runInNewContext(playerSource, context, { filename: 'player.js' });

const html = context.buildWidgetHTML({
  id: 'countdown-1',
  widget_type: 'countdown',
  x: 0,
  y: 0,
  w: 100,
  h: 100,
  z: 1,
  props: {
    countdown_mode: 'duration',
    duration_value: 1,
    duration_unit: 'seconds',
    expiry_action: 'page_switch',
    expiry_target_layout: 'target-layout',
  },
});

// A duration countdown has no targetDate. The live player must carry the
// duration contract into its runtime so it can establish a target and expire.
assert.match(html, /data-countdown-mode="duration"/);
assert.match(html, /data-countdown-duration-value="1"/);
assert.match(html, /data-countdown-duration-unit="seconds"/);
assert.match(html, /data-countdown-action="page_switch"/);
assert.match(html, /data-countdown-target-layout="target-layout"/);

let now = 1000;
const intervalCallbacks = [];
context.Date = class extends Date {
  static now() { return now; }
};
context.setInterval = fn => { intervalCallbacks.push(fn); return intervalCallbacks.length; };
context.clearInterval = () => {};
const value = { textContent: '', style: {}, previousElementSibling: null };
const node = {
  dataset: {
    countdownMode: 'duration',
    countdownDurationValue: '1',
    countdownDurationUnit: 'seconds',
    countdownExpiryText: '',
    countdownAction: 'page_switch',
    countdownTargetLayout: 'target-layout',
    countdownFontSize: '48',
  },
  clientWidth: 100,
  clientHeight: 100,
  querySelector: selector => selector === '.countdown-value' ? value : null,
};
let switched = false;
const root = {
  __countdownTimer: null,
  querySelectorAll: () => switched ? [] : [node],
  get innerHTML() { return ''; },
  set innerHTML(next) { switched = String(next).includes('data-ripley-widget-type="text"'); },
};
context.document.documentElement.contains = () => true;
const targetLayout = {
  id: 'target-layout',
  widgets: [{ id: 'target-text', widget_type: 'text', x: 0, y: 0, w: 100, h: 100, z: 1, props: { content: 'Switched' } }],
};
context.startCountdownRuntime(root, { id: 'source-layout', widgets: [] }, 'client-1', {}, async () => targetLayout, () => {}, async () => {});
assert.strictEqual(intervalCallbacks.length, 1);
now = 2500;
intervalCallbacks[0]();
setImmediate(() => {
  assert.strictEqual(switched, true, 'live player must replace the root when a duration countdown expires');
  console.log('countdown layout switch integration: duration expiry carries the live runtime contract');
});
