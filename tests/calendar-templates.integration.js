#!/usr/bin/env node
'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

const calendarSource = fs.readFileSync(
  path.join(__dirname, '..', 'static', 'js', 'calendar.js'),
  'utf8',
);

function renderTemplate(template) {
  const calendar = { innerHTML: '' };
  const sandbox = {
    URLSearchParams,
    Date,
    Promise,
    console,
    location: { search: `?template=${encodeURIComponent(template)}&maxevents=5` },
    setInterval() {},
    fetch() {
      throw new Error('an unconfigured calendar must not fetch an ICS URL');
    },
    document: {
      documentElement: { style: { setProperty() {} } },
      body: { style: {} },
      getElementById(id) {
        assert.equal(id, 'calendar');
        return calendar;
      },
    },
  };
  vm.runInNewContext(calendarSource, sandbox, {
    filename: 'static/js/calendar.js',
  });
  return calendar.innerHTML;
}

const expectations = {
  list: (html) => {
    assert.match(html, /class="cal-list"/);
    assert.match(html, /Lunch with Alex/);
    assert.ok((html.match(/class="event-title"/g) || []).length <= 5);
  },
  agenda: (html) => {
    assert.match(html, /class="cal-agenda"/);
    assert.match(html, /Lunch with Alex/);
    assert.match(html, /class="agenda-day"/);
  },
  today: (html) => {
    assert.match(html, /class="cal-today"/);
    assert.match(html, /class="today-date"/);
    assert.match(html, /Team standup/);
  },
  'mini-month': (html) => {
    assert.match(html, /class="cal-mini-month"/);
    assert.match(html, /class="cell has-event"/);
  },
};

for (const [template, check] of Object.entries(expectations)) {
  check(renderTemplate(template));
}

console.log(`calendar templates integration: ${Object.keys(expectations).length} passed`);
