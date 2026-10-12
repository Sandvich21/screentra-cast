// Clock renderer for the layout-editor clock widget. Runs inside the
// /clock.html iframe. Reads the desired format from the `?format=`
// query string (player.js passes it from the widget's `format` prop).
// Supported formats:
//   HH:mm        -> 13:45         (24h, no seconds)
//   HH:mm:ss     -> 13:45:08      (24h, with seconds)
//   hh:mm A      ->  1:45 PM      (12h, no seconds, AM/PM)
//   hh:mm:ss A   ->  1:45:08 PM   (12h, with seconds, AM/PM)
// Locale-independent so output is stable across player devices.
function pad2(n) { return n < 10 ? '0' + n : '' + n; }
function formatTime(d, fmt) {
  var h24 = d.getHours();
  var h12 = h24 % 12;
  if (h12 === 0) h12 = 12;
  var ampm = h24 < 12 ? 'AM' : 'PM';
  var hasSeconds = fmt.indexOf('ss') !== -1;
  var hh12 = pad2(h12);
  var HH = pad2(h24);
  var mm = pad2(d.getMinutes());
  var ss = pad2(d.getSeconds());
  if (hasSeconds) {
    return (fmt.indexOf('HH') !== -1 ? HH : hh12) + ':' + mm + ':' + ss + (fmt.indexOf('A') !== -1 ? ' ' + ampm : '');
  }
  return (fmt.indexOf('HH') !== -1 ? HH : hh12) + ':' + mm + (fmt.indexOf('A') !== -1 ? ' ' + ampm : '');
}
function update() {
  var el = document.getElementById('clock');
  if (!el) return;
  var fmt = 'HH:mm';
  var fontSize = 240;
  var fontFamily = '';
  var color = '#fff';
  try {
    var params = new URLSearchParams(window.location.search);
    var raw = params.get('format') || '';
    // Whitelist: only accept the 4 known format strings; everything
    // else (including attempts to inject HTML/JS via the param) falls
    // back to HH:mm.
    if (raw === 'HH:mm' || raw === 'HH:mm:ss' || raw === 'hh:mm A' || raw === 'hh:mm:ss A') {
      fmt = raw;
    }
    var rawSize = Number(params.get('font_size'));
    if (Number.isFinite(rawSize)) fontSize = Math.max(6, Math.min(400, rawSize));
    var rawColor = params.get('color') || '';
    if (/^#[0-9a-fA-F]{3,8}$/.test(rawColor)) color = rawColor;
    // Font family comes from the layout editor's font picker. We
    // the cssValue (e.g. '"Roboto", sans-serif'). Whitelist on
    // length only — the picker is trusted code.
    var ff = params.get('font_family') || '';
    if (ff && ff.length <= 200) {
      // Sanitize: no script tag injection. The picker only sends
      // css font-family strings, but defense in depth.
      if (!/<|>|javascript:/i.test(ff)) {
        fontFamily = ff;
      }
    }
  } catch (e) { /* malformed URL — keep default */ }
  el.style.fontSize = fontSize + 'px';
  el.style.setProperty('color', color, 'important');
  if (fontFamily) {
    el.style.fontFamily = fontFamily;
  } else {
    el.style.fontFamily = '';
  }
  el.textContent = formatTime(new Date(), fmt);
}
update();
setInterval(update, 1000);
