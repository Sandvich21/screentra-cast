const assert = require('assert');
const fs = require('fs');
const path = require('path');

const source = fs.readFileSync(path.join(__dirname, '..', 'static', 'js', 'app.js'), 'utf8');

// Hidden pages must not start their network work while the editor is opening.
assert.doesNotMatch(source, /this\.loadDisplaysPage\(\);\s*this\.loadSystemPage\(\);/);
assert.match(source, /const activePage = document\.querySelector\('\.page\.active'\)\?\.id/);

// Client cards should not wait for the full layout catalog; layouts are needed
// only by detail/assignment controls and are fetched on demand.
assert.doesNotMatch(source, /this\._clientLayouts = await API\.getLayouts\(\);/);

// Schedules should paint the schedule rows after the primary list response,
// then enrich names/options with the slower secondary catalogs.
assert.match(source, /const schedules = await API\.listSchedules\(\);/);
assert.match(source, /this\.renderSchedules\(schedules, \[\], \[\], \[\]\);/);
assert.match(source, /const needsScheduleTargets = schedules\.some/);

console.log('dashboard loading integration: hidden pages and secondary catalogs are lazy');
