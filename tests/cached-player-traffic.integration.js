const assert = require('assert');
const fs = require('fs');
const vm = require('vm');

const file = 'player-client/static/install/player-client.sh';
const source = fs.readFileSync(file, 'utf8');
const match = source.match(/PLAYER_HTML = r'''([\s\S]*?)'''/);
assert(match, `${file}: PLAYER_HTML not found`);
const scriptMatch = match[1].match(/<script>([\s\S]*?)<\/script>/);
assert(scriptMatch, `${file}: cached player script not found`);

const storage = new Map();
const localStorage = {
  getItem: (key) => storage.has(key) ? storage.get(key) : null,
  setItem: (key, value) => storage.set(key, String(value)),
};

const context = {
  console,
  URLSearchParams,
  Date,
  Math,
  Number,
  String,
  JSON,
  encodeURIComponent,
  setInterval() {},
  setTimeout() {},
  fetch: async () => ({ ok: false }),
  window: { addEventListener() {}, localStorage },
  __SCREENTRA_SERVER_URL__: '',
  document: {
    documentElement: { contains: () => true, style: {} },
    querySelectorAll: () => [],
    getElementById: () => ({ innerHTML: '' }),
  },
};
vm.createContext(context);
vm.runInContext(scriptMatch[1], context, { filename: file });

assert.strictEqual(typeof context.cachedTrafficRelevantEvents, 'function', `${file}: traffic filter missing`);
assert.strictEqual(typeof context.cachedTrafficVariableText, 'function', `${file}: traffic variable resolver missing`);
assert.strictEqual(typeof context.filterTrafficEvents, 'function', `${file}: server traffic block-rule filter missing`);

const trafficEvents = [
  { id: 'roadwork-1', category: 'roadwork', description: 'Generic road work' },
  { id: 'closure-1', category: 'closure', description: 'Road closure configured on server' },
  { id: 'crash-1', category: 'crash', description: 'Crash configured on server' },
];
const blockedProps = {
  manual_closure_blocking: true,
  hide_long_term_closures: false,
  blocked_closures: [{ key: 'id:closure-1' }],
};
const blockFiltered = context.filterTrafficEvents(trafficEvents, blockedProps, 'traffic-test-block');
assert.deepStrictEqual(
  Array.from(blockFiltered).map((event) => event.id),
  ['roadwork-1', 'crash-1'],
  `${file}: manually blocked closure was not removed`,
);
assert.deepStrictEqual(
  Array.from(context.cachedTrafficRelevantEvents({ _traffic_data: { events: blockFiltered } }, 'traffic_crashes_closures')).map((event) => event.id),
  ['crash-1'],
  `${file}: Crash/Road Closure selection did not exclude roadwork after blocking`,
);

storage.set('screentra-cast.traffic-closure-tracking.v1', JSON.stringify({
  'traffic-test-age': { 'id:closure-1': Date.now() - (10 * 24 * 60 * 60 * 1000) },
}));
const aged = context.filterTrafficEvents(
  trafficEvents,
  { hide_long_term_closures: true, long_term_closure_days: 5, manual_closure_blocking: false },
  'traffic-test-age',
);
assert.deepStrictEqual(
  Array.from(aged).map((event) => event.id),
  ['roadwork-1', 'crash-1'],
  `${file}: long-term closure rule was not applied`,
);

const trafficProps = {
  traffic_variable: 'traffic_crashes_closures',
  _traffic_data: {
    available: true,
    events: [
      { category: 'roadwork', description: 'Generic road work' },
      { category: 'crash', description: 'Crash configured on server' },
      { category: 'closure', description: 'Road closure configured on server' },
    ],
  },
};
const filtered = context.cachedTrafficRelevantEvents(trafficProps, 'traffic_crashes_closures');
assert.deepStrictEqual(
  filtered.map((event) => event.category),
  ['crash', 'closure'],
  `${file}: Crash/Road Closure traffic variable included generic road work`,
);
assert.strictEqual(
  context.cachedTrafficVariableText(trafficProps, 'traffic_crashes_closures', 0),
  'Crash configured on server',
  `${file}: traffic variable did not use the configured Crash description`,
);
assert.strictEqual(
  context.cachedTrafficVariableText(trafficProps, 'traffic_crashes_closures', 1),
  'Road closure configured on server',
  `${file}: traffic variable did not use the configured Road Closure description`,
);

const sizingMarkup = context.templateHtml({
  widget_type: 'traffic',
  props: {
    _widget_type: 'traffic',
    template_width: 328,
    template_height: 82,
    max_events: '4',
    hide_if_empty: true,
    _traffic_data: { available: true, events: [{ category: 'crash', description: 'One crash' }] },
    template_elements: [
      { type: 'image', width: 330, height: 80, x: 0, y: 0, z: 0, props: {}, repeat: { group: 'rows', count: 1, gap_y: 10, mode: 'rows', preserve_size: true, hide_if_empty: true } },
      { type: 'rectangle', width: 306, height: 60, x: 12, y: 10, z: 1, props: {}, repeat: { group: 'rows', count: 1, gap_y: 10, mode: 'rows' } },
      { type: 'variable_text', width: 300, height: 56, x: 16, y: 12, z: 2, props: { variable_source: 'traffic', variable_key: 'traffic_crashes_closures', font_size: 11 }, repeat: { group: 'rows', count: 1, gap_y: 10, mode: 'rows', preserve_size: true, hide_if_empty: true } },
    ],
  },
});
assert.match(
  sizingMarkup,
  /height:16(?:\.[0-9]+)?%;/,
  `${file}: one traffic event collapsed the preserved multi-row template height`,
);

console.log('cached player traffic integration: Crash/Road Closure excludes road work and preserves configured descriptions');
