const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

const source = fs.readFileSync(path.join(__dirname, '..', 'static', 'js', 'player.js'), 'utf8');
const start = source.indexOf('function ripleyCalendarOverrideMatches');
const end = source.indexOf('async function ripleyLoadCalendarData', start);
assert.ok(start >= 0 && end > start, 'calendar parser helpers must remain discoverable');
const helpers = source.slice(start, end);
const sandbox = { Date, String, Number, Math, Array, Object, RegExp, console };
vm.createContext(sandbox);
vm.runInContext(`${helpers}\nthis.parseCalendar = ripleyParseCalendarText;`, sandbox);

const ics = [
  'BEGIN:VCALENDAR',
  'BEGIN:VEVENT',
  'UID:uid-title@example.test',
  'DTSTART:20300115T090000',
  'SUMMARY:Morning meeting',
  'LOCATION:Room A',
  'END:VEVENT',
  'BEGIN:VEVENT',
  'UID:uid-recurring@example.test',
  'DTSTART:20300116T130000',
  'SUMMARY:Recurring event',
  'END:VEVENT',
  'BEGIN:VEVENT',
  'UID:uid-recurring@example.test',
  'DTSTART:20300117T130000',
  'SUMMARY:Recurring event',
  'END:VEVENT',
  'END:VCALENDAR',
].join('\r\n');

const titleCorrected = sandbox.parseCalendar(ics, 10, '12', [{
  match_type: 'title',
  match_value: 'Morning meeting',
  time: '16:45',
}]);
assert.equal(titleCorrected.find(e => e.summary === 'Morning meeting').time, '4:45 PM');
assert.equal(titleCorrected.find(e => e.summary === 'Morning meeting').date, '2030-01-15');
assert.equal(titleCorrected.find(e => e.summary === 'Morning meeting').location, 'Room A');

const uidOccurrenceCorrected = sandbox.parseCalendar(ics, 10, '24', [{
  match_type: 'uid',
  match_value: 'uid-recurring@example.test',
  source_date: '2030-01-17',
  date: '2030-01-18',
  time: '07:05',
}]);
const occurrences = uidOccurrenceCorrected.filter(e => e.summary === 'Recurring event');
assert.equal(occurrences.length, 2);
assert.equal(occurrences.find(e => e.source_date === '2030-01-16').time, '13:00');
assert.equal(occurrences.find(e => e.source_date === '2030-01-17').time, '07:05');
assert.equal(occurrences.find(e => e.source_date === '2030-01-17').date, '2030-01-18');

const invalid = sandbox.parseCalendar(ics, 10, '24', [{
  match_type: 'title',
  match_value: 'Morning meeting',
  time: '99:99',
}]);
assert.equal(invalid.find(e => e.summary === 'Morning meeting').time, '09:00');

console.log('calendar overrides integration: 3 passed');
