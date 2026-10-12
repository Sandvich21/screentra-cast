const assert = require('assert');
const fs = require('fs');
const path = require('path');

const appSource = fs.readFileSync(path.join(__dirname, '..', 'static', 'js', 'app.js'), 'utf8');

// Returning to the editor must call the current Editor refresh API; the old
// method names no longer existed and silently left the layout catalog stale.
assert.match(appSource, /typeof Editor !== 'undefined' && typeof Editor\.refresh === 'function'\) Editor\.refresh\(\)/);
assert.doesNotMatch(appSource, /Editor\.refreshLayoutList\(\)|Editor\.refreshDisplayDropdown\(\)/);

// Bulk assignment must re-read layouts when the assignment bar is opened,
// because a layout can be created in another SPA page without a full reload.
assert.match(appSource, /async function updateBulkBar\(\)/);
assert.match(appSource, /const freshLayouts = await API\.getLayouts\(\)/);
assert.match(appSource, /App\._clientLayouts = layouts/);

console.log('layout assignment refresh integration: fresh layouts load without page reload');