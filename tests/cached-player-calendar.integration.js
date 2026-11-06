const assert = require('assert');
const fs = require('fs');
const vm = require('vm');

const file = 'player-client/static/install/player-client.sh';
const source = fs.readFileSync(file, 'utf8');
const match = source.match(/PLAYER_HTML = r'''([\s\S]*?)'''/);
assert(match, `${file}: PLAYER_HTML not found`);
const scriptMatch = match[1].match(/<script>([\s\S]*?)<\/script>/);
assert(scriptMatch, `${file}: cached player script not found`);

const ics = [
  'BEGIN:VCALENDAR',
  'VERSION:2.0',
  'BEGIN:VEVENT',
  'UID:calendar-test-1',
  'DTSTART:20261107T090000',
  'SUMMARY:Calendar retry event',
  'LOCATION:Test room',
  'END:VEVENT',
  'END:VCALENDAR',
].join('\r\n');
const responseWithCalendar = () => ({
  ok: true,
  status: 200,
  body: {
    getReader: () => {
      let sent = false;
      return {
        read: async () => {
          if (sent) return { done: true, value: new Uint8Array() };
          sent = true;
          return { done: false, value: new TextEncoder().encode(ics) };
        },
        cancel: async () => {},
        releaseLock: () => {},
      };
    },
  },
});
let calendarCalls = 0;
const context = {
  console,
  URLSearchParams,
  Date,
  Math,
  Number,
  String,
  JSON,
  TextEncoder,
  TextDecoder,
  encodeURIComponent,
  setInterval() {},
  setTimeout: (callback) => { callback(); return 1; },
  fetch: async (url) => {
    if (!String(url).includes('/api/ics-proxy')) return { ok: false, status: 404, body: { getReader: () => ({ read: async () => ({ done: true, value: new Uint8Array() }), releaseLock() {} }) } };
    calendarCalls += 1;
    if (calendarCalls === 1) return { ok: false, status: 429 };
    return responseWithCalendar();
  },
  window: { addEventListener() {}, localStorage: { getItem: () => null, setItem() {} } },
  __SCREENTRA_SERVER_URL__: '',
  document: {
    documentElement: { contains: () => true, style: {} },
    querySelectorAll: () => [],
    getElementById: () => ({ innerHTML: '' }),
  },
};
vm.createContext(context);
vm.runInContext(scriptMatch[1], context, { filename: file });

assert.strictEqual(typeof context.loadCalendar, 'function', `${file}: loadCalendar missing`);
(async () => {
  const props = { max_events: 6, time_format: '12' };
  await context.loadCalendar(props, 'layout-test', 'widget-test');
  assert.strictEqual(calendarCalls, 2, `${file}: calendar loader did not retry a temporary 429`);
  assert.strictEqual(props._calendar_events.length, 1, `${file}: calendar retry did not parse an event`);
  assert.strictEqual(props._calendar_events[0].summary, 'Calendar retry event', `${file}: calendar event title was not preserved`);
  console.log('cached player calendar integration: retries temporary proxy limits and renders the event');
})().catch((error) => {
  console.error(error.stack || error);
  process.exitCode = 1;
});
