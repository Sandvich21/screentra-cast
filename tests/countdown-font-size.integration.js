const assert = require('assert');
const fs = require('fs');
const path = require('path');
const vm = require('vm');

const playerPath = path.join(__dirname, '..', 'static', 'js', 'player.js');
const playerSource = fs.readFileSync(playerPath, 'utf8');
const previewSource = fs.readFileSync(path.join(__dirname, '..', 'static', 'js', 'preview.js'), 'utf8');
const editorSource = fs.readFileSync(path.join(__dirname, '..', 'static', 'js', 'editor.js'), 'utf8');

assert.match(playerSource, /ripleyResponsiveFontCss\(ripleyCountdownFontSize\(p\.font_size\)\)/);
assert.doesNotMatch(playerSource, /class=\\"countdown-value\\" style=\\"font-size:10px/);
assert.match(playerSource, /RIPLEY_COUNTDOWN_MAX_FONT_SIZE = 160/);
assert.match(playerSource, /Math\.min\(RIPLEY_COUNTDOWN_MAX_FONT_SIZE, Math\.round\(size\)\)/);
assert.match(playerSource, /value\.style\.fontSize = Math\.floor\(requested\) \+ 'px'/);
assert.match(previewSource, /value\.style\.fontSize=Math\.floor\(effective\)\+'px'/);
assert.match(previewSource, /ripleyScaledPixels\(requested\)/);
assert.doesNotMatch(previewSource, /const RIPLEY_COUNTDOWN_MAX_FONT_SIZE=160/);
assert.match(editorSource, /min="8" max="160"/);
assert.match(editorSource, /live\.type === 'countdown' \? 160 : 400/);
assert.match(editorSource, /Number\(p\.font_size\) \|\| 48/);
assert.match(editorSource, /this\.editorFontCss\(Math\.max\(8, Math\.min\(160/);

const context = {
  console,
  URLSearchParams,
  window: { location: { pathname: '/preview.html', search: '' } },
  location: { pathname: '/preview.html', search: '' },
  document: { documentElement: { dataset: {} }, querySelector: () => null, getElementById: () => null },
  ripleyWeatherIconSvg: () => '<svg></svg>',
  ripleyWeatherIconDescription: () => 'sample',
};
vm.runInNewContext(playerSource, context, { filename: 'player.js' });

function countdownHtml(fontSize) {
  return context.buildWidgetHTML({
    id: 'countdown-1',
    widget_type: 'countdown',
    x: 0,
    y: 0,
    w: 100,
    h: 100,
    z: 1,
    props: {
      font_size: fontSize,
      countdown_mode: 'duration',
      duration_value: 5,
      duration_unit: 'minutes',
    },
  });
}

const small = countdownHtml(24);
const large = countdownHtml(96);
const capped = countdownHtml(999);
assert.notStrictEqual(small, large);
assert.match(small, /2\.5cqw/);
assert.match(large, /10cqw/);
assert.match(small, /data-countdown-font-size=\"24\"/);
assert.match(large, /data-countdown-font-size=\"96\"/);
assert.match(capped, /data-countdown-font-size=\"160\"/);
assert.match(capped, /16\.666666666666664cqw/);

const medium = countdownHtml(48);
const larger = countdownHtml(72);
assert.notStrictEqual(medium, larger);
assert.match(medium, /5cqw/);
assert.match(larger, /7\.5cqw/);

console.log('countdown font-size integration: configured text size reaches editor/player/preview');
