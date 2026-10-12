const assert = require('assert');
const fs = require('fs');
const path = require('path');

const appSource = fs.readFileSync(path.join(__dirname, '..', 'static', 'js', 'app.js'), 'utf8');
const scheduleSource = fs.readFileSync(path.join(__dirname, '..', 'src', 'schedule.rs'), 'utf8');

// The scheduler uses Sunday as bit 0: Sun=1, Mon=2, ... Fri=32.
const dayNames = ['Sunday', 'Monday', 'Tuesday', 'Wednesday', 'Thursday', 'Friday', 'Saturday'];
const businessDaysMask = 2 | 4 | 8 | 16 | 32;
const selected = dayNames.filter((_, day) => (businessDaysMask & (1 << day)) !== 0);
assert.strictEqual(businessDaysMask, 62);
assert.deepStrictEqual(selected, ['Monday', 'Tuesday', 'Wednesday', 'Thursday', 'Friday']);

assert.match(appSource, /data-mask="62"[^>]*>Business Days \(Mon-Fri\)/);
assert.match(appSource, /const DAY_WEEKDAYS = 62/);
assert.match(appSource, /if \(mask === DAY_WEEKDAYS\) return 'Mon-Fri \(Business Days\)'/);
assert.match(appSource, /if \(mask === 62\) return 'Mon–Fri \(Business Days\)'/);
assert.match(scheduleSource, /62 \(Mon-Fri\)/);

console.log('schedule days integration: Business Days selects Monday through Friday');
