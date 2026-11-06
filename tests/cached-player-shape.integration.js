const assert = require('assert');
const fs = require('fs');
const vm = require('vm');

const files = [
  'static/install/player-client.sh',
  'player-client/static/install/player-client.sh',
];

async function checkCachedPlayer(file) {
  const source = fs.readFileSync(file, 'utf8');
  const match = source.match(/PLAYER_HTML = r'''([\s\S]*?)'''/);
  assert(match, `${file}: PLAYER_HTML not found`);
  const scriptMatch = match[1].match(/<script>([\s\S]*?)<\/script>/);
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
    __SCREENTRA_SERVER_URL__: '',
    document: {
      documentElement: { contains: () => true, style: {} },
      querySelectorAll: () => [],
      getElementById: () => ({ innerHTML: '' }),
    },
  };
  vm.createContext(context);
  vm.runInContext(scriptMatch[1], context, { filename: file });

  const directMarkup = await context.renderWidget({
    widget_type: 'rectangle',
    props: {
      shape_mode: 'rectangle',
      radius: 0,
      radius_tl: 11,
      radius_tr: 22,
      radius_br: 33,
      radius_bl: 44,
      color: '#363636',
    },
  });
  assert.match(
    directMarkup,
    /border-radius:11px 22px 33px 44px;/,
    `${file}: direct cached shape dropped per-corner radius values`,
  );

  const templateMarkup = await context.renderWidget({
    widget_type: 'weather',
    props: {
      template_width: 760,
      template_height: 427.5,
      template_elements: [{
        type: 'rectangle',
        x: 0,
        y: 0,
        width: 200,
        height: 100,
        z: 1,
        props: {
          color: '#363636',
          radius: 0,
          radius_tl: 5,
          radius_tr: 6,
          radius_br: 7,
          radius_bl: 8,
        },
      }],
    },
  });
  assert.match(
    templateMarkup,
    /border-radius:5px 6px 7px 8px;/,
    `${file}: template shape dropped per-corner radius values`,
  );
}

(async () => {
  for (const file of files) await checkCachedPlayer(file);
  console.log('cached player shape integration: corner radius values preserved');
})().catch((error) => {
  console.error(error.stack || error);
  process.exitCode = 1;
});
