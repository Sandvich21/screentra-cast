const assert = require('assert');
const fs = require('fs');
const path = require('path');

const source = fs.readFileSync(path.join(__dirname, '..', 'static', 'js', 'app.js'), 'utf8');

// Enrolled client cards expose a direct live-preview action. It reuses the
// display-tile screenshot stream and falls back to the detail view while tiles
// are still loading.
assert.match(source, /client-live-preview/);
assert.match(source, /Open Live Preview/);
assert.match(source, /card\.querySelector\('\.client-display-live-button'\)/);
assert.match(source, /liveButton\.click\(\)/);
assert.match(source, /App\.openClientDetail\(clientObj\)/);

console.log('client live preview integration: card action is wired to display live view');
