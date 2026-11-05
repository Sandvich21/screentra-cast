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

// Connected clients compare the reported client version with the server version
// and expose the existing signed update command only when the client is older.
assert.match(source, /API\.getVersion\(\)\.catch\(\(\) => null\)/);
assert.match(source, /Client v\$\{client\.version\} is older than server v\$\{serverVersion\}/);
assert.match(source, /className = 'btn btn-warning btn-sm client-update-button'/);
assert.match(source, /API\.sendClientCommand\(client\.id, 'update_client'\)/);
const versionStart = source.indexOf('  function parseVersion(value)');
const versionEnd = source.indexOf('  function updateAvailable(client)', versionStart);
assert(versionStart >= 0 && versionEnd > versionStart, 'client version comparator must be present');
eval(source.slice(versionStart, versionEnd));
assert.strictEqual(compareVersions('0.1.0', '0.1.5'), -1);
assert.strictEqual(compareVersions('0.1.5-rc.6', '0.1.5'), -1);
assert.strictEqual(compareVersions('0.1.5', '0.1.5'), 0);

console.log('dashboard loading integration: hidden pages and secondary catalogs are lazy');
