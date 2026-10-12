const assert = require('assert');
const fs = require('fs');
const vm = require('vm');

const files = [
  'static/install/player-client.sh',
  'static/install/screentra-cast-player-client.sh',
];

async function checkCachedPlayer(file) {
  const source = fs.readFileSync(file, 'utf8');
  const match = source.match(/PLAYER_HTML = r'''([\s\S]*?)'''/);
  assert(match, `${file}: PLAYER_HTML not found`);
  const html = match[1];
  const scriptMatch = html.match(/<script>([\s\S]*?)<\/script>/);
  assert(scriptMatch, `${file}: cached player script not found`);

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
    window: { addEventListener() {} },
    document: {
      documentElement: { contains: () => true, style: {} },
      querySelectorAll: () => [],
      getElementById: () => ({ innerHTML: '' }),
    },
  };
  vm.createContext(context);
  vm.runInContext(scriptMatch[1], context, { filename: file });

  assert.strictEqual(typeof context.renderWidget, 'function', `${file}: renderWidget missing`);
  assert.strictEqual(typeof context.tickCountdowns, 'function', `${file}: tickCountdowns missing`);

  const markup = await context.renderWidget({
    widget_type: 'countdown',
    x: 0,
    y: 0,
    w: 1,
    h: 1,
    props: {
      countdown_mode: 'duration',
      duration_value: 5,
      duration_unit: 'seconds',
      expiry_text: 'Expired',
    },
  });
  assert.match(markup, /data-countdown-mode="duration"/, `${file}: duration mode missing from cached markup`);
  assert.match(markup, /data-countdown-duration-value="5"/, `${file}: duration value missing from cached markup`);
  assert.match(markup, /data-countdown-duration-unit="seconds"/, `${file}: duration unit missing from cached markup`);

  const attrs = {};
  for (const [, name, value] of markup.matchAll(/data-countdown-([\w-]+)="([^"]*)"/g)) {
    const key = `countdown${name.split('-').map((part) => part[0].toUpperCase() + part.slice(1)).join('')}`;
    attrs[key] = value;
  }
  const value = { textContent: '' };
  const node = {
    dataset: attrs,
    querySelector: (selector) => selector === '.countdown-value' ? value : null,
  };
  context.document.querySelectorAll = (selector) => selector === '[data-countdown-widget]' ? [node] : [];
  context.tickCountdowns();
  assert.match(value.textContent, /[1-5]s/, `${file}: duration countdown did not start`);

  node.dataset.countdownStartedAt = String(Date.now() - 6000);
  context.tickCountdowns();
  assert.strictEqual(value.textContent, 'Expired', `${file}: duration countdown did not expire`);

  const videoMarkup = await context.renderWidget({
    id: 'video-1',
    widget_type: 'video',
    props: {
      playlist: [{ url: 'one.mp4' }, { url: 'two.mp4' }],
      loop: true,
      muted: true,
    },
  });
  assert.match(videoMarkup, /data-video-widget-id="video-1"/, `${file}: video identity missing`);
  assert.match(videoMarkup, /src="one\.mp4"/, `${file}: first video source missing`);
  assert.strictEqual(typeof context.initCachedVideoWidgets, 'function', `${file}: cached video initializer missing`);
  const videoNode = {
    src: '',
    loop: false,
    onended: null,
    onerror: null,
    dataset: {},
    getAttribute: (name) => name === 'data-video-widget-id' ? 'video-1' : null,
    pause() {},
    load() {},
    play() { return Promise.resolve(); },
  };
  context.document.querySelectorAll = (selector) => selector === '[data-video-widget-id]' ? [videoNode] : [];
  context.initCachedVideoWidgets([{ id: 'video-1', widget_type: 'video', props: {
    playlist: [{ url: 'one.mp4' }, { url: 'two.mp4' }], loop: true,
  } }]);
  assert.strictEqual(videoNode.src, 'one.mp4', `${file}: cached video did not start`);
  videoNode.onended();
  assert.strictEqual(videoNode.src, 'two.mp4', `${file}: cached video did not advance`);
  videoNode.onended();
  assert.strictEqual(videoNode.src, 'one.mp4', `${file}: cached video did not loop`);
}

(async () => {
  for (const file of files) await checkCachedPlayer(file);
  console.log('cached player countdown integration: duration countdown starts and expires');
})().catch((error) => {
  console.error(error.stack || error);
  process.exitCode = 1;
});
