
var rendered = {};
const PLAYER_URL_PARAMS = new URLSearchParams(window.location.search);
const PLAYER_HASH_PARAMS = new URLSearchParams(window.location.hash.slice(1));
const PLAYER_CAPABILITY = PLAYER_HASH_PARAMS.get('player_capability') || PLAYER_URL_PARAMS.get('player_capability') || '';
function playerFetch(url, options) {
  options = options || {};
  const headers = new Headers(options.headers || {});
  if (PLAYER_CAPABILITY) headers.set('X-SCREENTRA-PLAYER-CAPABILITY', PLAYER_CAPABILITY);
  options.headers = headers;
  return window.fetch(url, options);
}
const MAX_PLAYER_RESPONSE_BYTES = 4 * 1024 * 1024;
async function readPlayerTextBounded(response, maximumBytes = MAX_PLAYER_RESPONSE_BYTES) {
  const reader = response.body && response.body.getReader ? response.body.getReader() : null;
  if (!reader) throw new Error('response body is unavailable');
  const decoder = new TextDecoder(); let total = 0, text = '';
  try {
    while (true) {
      const part = await reader.read();
      if (part.done) { text += decoder.decode(); return text; }
      const value = part.value || new Uint8Array();
      total += value.byteLength;
      if (total > maximumBytes) {
        try { await reader.cancel(); } catch (_) {}
        throw new Error('response exceeds its size limit');
      }
      text += decoder.decode(value, { stream: true });
    }
  } finally {
    reader.releaseLock();
  }
}
async function readPlayerJsonBounded(response, maximumBytes = MAX_PLAYER_RESPONSE_BYTES) {
  return JSON.parse(await readPlayerTextBounded(response, maximumBytes));
}

function ripleyTextShadowStyle(p) {
  if (!p || !p.text_shadow_enabled) return '';
  return 'text-shadow:' + (Number(p.text_shadow_x == null ? 2 : p.text_shadow_x)||0) + 'px ' + (Number(p.text_shadow_y == null ? 2 : p.text_shadow_y)||0) + 'px ' + (Number(p.text_shadow_blur == null ? 4 : p.text_shadow_blur)||0) + 'px ' + ripleyCssColor(p.text_shadow_color, '#000000') + ';';
}
function ripleyTextOutlineStyle(p) {
  if (!p || !p.text_outline_enabled) return '';
  return '-webkit-text-stroke:' + (Number(p.text_outline_width == null ? 1 : p.text_outline_width)||0) + 'px ' + ripleyCssColor(p.text_outline_color, '#000000') + ';';
}

function ripleyTplEsc(v) {
  return String(v == null ? '' : v).replace(/[&<>"']/g, function(c){ return {'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]; });
}
function ripleyCssColor(value, fallback) {
  var s = String(value == null ? '' : value).trim();
  if (/^#[0-9a-f]{3,8}$/i.test(s) || /^(?:rgb|hsl)a?\([0-9.% ,+-]+\)$/i.test(s)) return s;
  return fallback;
}
function ripleyCssAlign(value, fallback) {
  var s = String(value == null ? '' : value).trim().toLowerCase();
  return ['left', 'center', 'right', 'justify'].indexOf(s) >= 0 ? s : fallback;
}
function ripleyCssFontFamily(value) {
  var s = String(value == null ? '' : value).trim();
  return /^[A-Za-z0-9 _,'"-]{1,160}$/.test(s) ? s : '';
}
function ripleySafeUrl(value) {
  var s = String(value == null ? '' : value).trim();
  if (/^(?:https?:|blob:|\/(?!\/))/i.test(s) && !/^(?:javascript|data|vbscript):/i.test(s)) return s;
  return '';
}
function ripleyNumber(value, fallback, min, max) {
  var n = Number(value);
  if (!Number.isFinite(n)) n = fallback;
  if (min != null) n = Math.max(min, n);
  if (max != null) n = Math.min(max, n);
  return n;
}
if (typeof document !== 'undefined' && typeof document.addEventListener === 'function') {
  document.addEventListener('error', function(event) {
    var image = event.target;
    if (!image || image.nodeName !== 'IMG' || !image.hasAttribute('data-ripley-fallback')) return;
    image.style.display = 'none';
    if (image.nextElementSibling) image.nextElementSibling.style.display = 'flex';
  }, true);
}
// Layout font sizes are authored against the editor's 960px landscape canvas
// (540px for portrait). Geometry is percentage-based, so text and other
// pixel-valued properties must use the same viewport-relative scale on the
// player and in Layout Preview. The compositor/browser CSS viewport is the
// source of truth; physical DRM pixels may be larger when devicePixelRatio > 1.
function ripleySetRenderOrientation(layout) {
  var orientation = layout && layout.canvas_orientation === 'portrait' ? 'portrait' : 'landscape';
  document.documentElement.dataset.ripleyOrientation = orientation;
  return orientation;
}
function ripleyRenderRoot() {
  return document.getElementById('stage') || document.querySelector('.region') || document.getElementById('player') || document.documentElement;
}
function ripleyRenderDesignWidth() {
  return document.documentElement.dataset.ripleyOrientation === 'portrait' ? 540 : 960;
}
function ripleyRenderScale() {
  var root = ripleyRenderRoot();
  var width = root && root.clientWidth || window.innerWidth || ripleyRenderDesignWidth();
  return Math.max(0.25, Math.min(8, width / ripleyRenderDesignWidth()));
}
function ripleyScaledPixels(value) {
  return Math.max(1, Math.round((Number(value) || 0) * ripleyRenderScale()));
}
function ripleyResponsiveFontCss(value) {
  var size = Math.max(1, Number(value) || 1);
  var designWidth = ripleyRenderDesignWidth();
  return 'clamp(6px,calc(' + (size / designWidth * 100) + 'cqw),' + (size * 4) + 'px)';
}
var RIPLEY_COUNTDOWN_MAX_FONT_SIZE = 160;
function ripleyCountdownFontSize(value) {
  var size = Number(value);
  if (!isFinite(size)) size = 48;
  return Math.max(8, Math.min(RIPLEY_COUNTDOWN_MAX_FONT_SIZE, Math.round(size)));
}
function pad2(n) { n = Number(n) || 0; return n < 10 ? '0' + n : '' + n; }

// Lightweight moment.js-style date formatter. Supports:
//   YYYY 4-digit year, YY 2-digit year
//   MMMM full month name, MMM 3-letter, MM 2-digit, M 1-2 digit
//   DDDD full weekday, DDD 3-letter, DD 2-digit, D 1-2 digit
// Anything else is passed through literally. This avoids the
// locale-dependent toLocaleDateString() in the date widget.
function formatDateLikeMoment(d, fmt) {
  fmt = fmt || 'dddd, MMMM D';
  var monthsFull = ['January','February','March','April','May','June','July','August','September','October','November','December'];
  var monthsShort = ['Jan','Feb','Mar','Apr','May','Jun','Jul','Aug','Sep','Oct','Nov','Dec'];
  var daysFull = ['Sunday','Monday','Tuesday','Wednesday','Thursday','Friday','Saturday'];
  var daysShort = ['Sun','Mon','Tue','Wed','Thu','Fri','Sat'];
  var Y = d.getFullYear();
  var M = d.getMonth();
  var D = d.getDate();
  // Tokenize by replacing longest first to avoid partial matches.
  var tokens = [
    ['YYYY', '' + Y],
    ['YY', ('' + Y).slice(-2)],
    ['MMMM', monthsFull[M]],
    ['MMM', monthsShort[M]],
    ['MM', pad2(M + 1)],
    ['M', '' + (M + 1)],
    ['dddd', daysFull[d.getDay()]],
    ['ddd', daysShort[d.getDay()]],
    ['DDDD', daysFull[d.getDay()]],
    ['DDD', daysShort[d.getDay()]],
    ['DD', pad2(D)],
    ['D', '' + D],
  ];
  for (var i = 0; i < tokens.length; i++) {
    var tk = tokens[i][0], tv = tokens[i][1];
    while (fmt.indexOf(tk) >= 0) fmt = fmt.replace(tk, tv);
  }
  return fmt;
}

// Inject @font-face declarations for every installed font so the
// browser can resolve font_family CSS like '"Roboto", sans-serif'.
// Called once at startup; idempotent because subsequent calls with
// the same family+weight are no-ops.
function injectInstalledFonts(fonts) {
  if (!Array.isArray(fonts) || !fonts.length) return;
  var seen = {};
  for (var i = 0; i < fonts.length; i++) {
    var f = fonts[i];
    if (!f || !f.family || !f.weight || !f.url) continue;
    var key = f.family + ':' + f.weight;
    if (seen[key]) continue;
    seen[key] = true;
    var family = String(f.family).replace(/["']/g, '');
    var url = String(f.url).replace(/"/g, '%22');
    var css = '@font-face{font-family:"' + family + '";font-weight:' + f.weight + ';font-style:normal;src:url("' + url + '") format("woff2");font-display:swap;}';
    var style = document.createElement('style');
    style.textContent = css;
    document.head.appendChild(style);
  }
}
function ripleyShapeClipPath(mode) {
  var m = mode || 'rounded';
  if (m === 'triangle') return 'polygon(50% 0%, 0% 100%, 100% 100%)';
  if (m === 'diamond') return 'polygon(50% 0%, 100% 50%, 50% 100%, 0% 50%)';
  if (m === 'hexagon') return 'polygon(25% 0%, 75% 0%, 100% 50%, 75% 100%, 25% 100%, 0% 50%)';
  if (m === 'star') return 'polygon(50% 0%, 61% 35%, 98% 35%, 68% 57%, 79% 91%, 50% 70%, 21% 91%, 32% 57%, 2% 35%, 39% 35%)';
  if (m === 'parallelogram') return 'polygon(20% 0%, 100% 0%, 80% 100%, 0% 100%)';
  if (m === 'arrow') return 'polygon(0% 25%, 60% 25%, 60% 0%, 100% 50%, 60% 100%, 60% 75%, 0% 75%)';
  if (m === 'plus') return 'polygon(35% 0%, 65% 0%, 65% 35%, 100% 35%, 100% 65%, 65% 65%, 65% 100%, 35% 100%, 35% 65%, 0% 65%, 0% 35%, 35% 35%)';
  if (m === 'minus') return 'polygon(0% 35%, 100% 35%, 100% 65%, 0% 65%)';
  if (m === 'x') return 'polygon(20% 0%, 50% 30%, 80% 0%, 100% 20%, 70% 50%, 100% 80%, 80% 100%, 50% 70%, 20% 100%, 0% 80%, 30% 50%, 0% 20%)';
  return '';
}
function ripleyWmoEmoji(code) {
  var map = {'0':'☀️','1':'🌤️','2':'⛅','3':'☁️','45':'🌫️','48':'🌫️','51':'🌦️','53':'🌦️','55':'🌦️','61':'🌧️','63':'🌧️','65':'🌧️','71':'❄️','73':'❄️','75':'❄️','80':'🌦️','81':'🌧️','82':'🌧️','95':'⛈️','96':'⛈️','99':'⛈️'};
  return map[String(code)] || '🌤️';
}
function ripleyWmoDescription(code) {
  return ripleyWeatherIconDescription(code);
}
function ripleySampleWeatherData() {
  return {
    current_weather: { temperature: 22, weathercode: 2, windspeed: 8, winddirection: 180, is_daytime: true },
    daily: {
      weathercode: [2, 3, 61, 80, 95, 71, 1],
      weather_code_day: [2, 3, 61, 80, 95, 71, 1],
      weather_code_night: [3, 3, 63, 3, 95, 73, 3],
      weather_code_morning: [45, null, null, null, null, null, null],
      weather_morning_until: ['2026-09-29T08:00:00-04:00', null, null, null, null, null, null],
      temperature_2m_max: [24, 22, 18, 19, 16, 5, 8],
      temperature_2m_min: [15, 14, 12, 11, 10, -2, 0],
      precipitation_probability_max: [20, 35, 70, 55, 85, 60, 10]
    }
  };
}
function ripleyConvertTemp(c, unit) {
  if (c == null || Number.isNaN(Number(c))) return '';
  var n = Number(c);
  return Math.round(String(unit || 'F').toUpperCase() === 'F' ? (n * 9 / 5 + 32) : n);
}
function ripleyWeatherForOffset(widgetProps, offset) {
  var data = widgetProps._weather_data || widgetProps.weather_data || ripleySampleWeatherData();
  var daily = data.daily || {};
  var idx = Math.max(0, Math.min(6, Number(offset) || 0));
  var dayCodes = daily.weather_code_day || daily.weathercode_day || daily.weather_code || daily.weathercode;
  var nightCodes = daily.weather_code_night || daily.weathercode_night || dayCodes;
  var dayCode = dayCodes && dayCodes[idx] != null ? dayCodes[idx] : 2;
  var nightCode = nightCodes && nightCodes[idx] != null ? nightCodes[idx] : dayCode;
  var morningCodes = daily.weather_code_morning || daily.weathercode_morning || [];
  var morningUntilSeries = daily.weather_morning_until || daily.morning_weather_until || [];
  var morningCode = morningCodes[idx] != null ? morningCodes[idx] : null;
  var morningUntil = morningUntilSeries[idx] || '';
  var isNight = idx === 0 && data.current_weather && data.current_weather.is_daytime === false;
  var code = idx === 0 && data.current_weather && data.current_weather.weathercode != null
    ? data.current_weather.weathercode
    : (isNight ? nightCode : dayCode);
  var high = daily.temperature_2m_max && daily.temperature_2m_max[idx] != null ? daily.temperature_2m_max[idx] : (data.current_weather && data.current_weather.temperature);
  var low = daily.temperature_2m_min && daily.temperature_2m_min[idx] != null ? daily.temperature_2m_min[idx] : high;
  var precip = daily.precipitation_probability_max && daily.precipitation_probability_max[idx] != null ? daily.precipitation_probability_max[idx] : null;
  var alertSeries = daily.weather_alerts || daily.weather_alert || [];
  var alertValue = alertSeries[idx];
  var alerts = Array.isArray(alertValue) ? alertValue.filter(Boolean).map(String) : (alertValue ? [String(alertValue)] : []);
  var current = data.current_weather && data.current_weather.temperature != null ? data.current_weather.temperature : high;
  var windSeries = daily.wind_speed || daily.windspeed || [];
  var wind = windSeries[idx] != null ? windSeries[idx] : (data.current_weather && data.current_weather.windspeed != null ? data.current_weather.windspeed : null);
  var windMetric = String(widgetProps.unit || (widgetProps.units === 'metric' ? 'C' : 'F')).toUpperCase() !== 'F';
  function displayTemp(value) { if (value == null || Number.isNaN(Number(value))) return ''; var n = Number(value); var sourceF = data.source === 'noaa_nws'; var wantF = !windMetric; if (sourceF) return Math.round(wantF ? n : (n - 32) * 5 / 9); return ripleyConvertTemp(n, widgetProps.unit || 'F'); }
  var morningUntilText = '';
  if (morningUntil) {
    var untilDate = new Date(morningUntil);
    if (!Number.isNaN(untilDate.getTime())) {
      try { morningUntilText = new Intl.DateTimeFormat(undefined, {hour: 'numeric', minute: '2-digit', timeZone: data.timezone || undefined}).format(untilDate); }
      catch (_) { morningUntilText = untilDate.toLocaleTimeString(undefined, {hour: 'numeric', minute: '2-digit'}); }
    }
  }
  var morningDesc = morningCode == null ? '' : ripleyWmoDescription(morningCode);
  var morningSummary = morningDesc ? (morningUntilText ? morningDesc + ' until ' + morningUntilText : morningDesc) : '';
  var weatherAvailable = data.source !== 'unavailable';
  return { code: String(code), day_code: String(dayCode), night_code: String(nightCode), morning_code: morningCode == null ? '' : String(morningCode), morning_until: morningUntil, morning_until_text: morningUntilText ? 'until ' + morningUntilText : '', morning_desc: morningDesc, morning_summary: morningSummary, has_morning_transition: morningCode != null && String(morningCode) !== String(dayCode), is_night: !!isNight, weather_available: weatherAvailable, high: displayTemp(high), low: displayTemp(low), current: displayTemp(current), precip: precip == null ? '' : Math.round(Number(precip) || 0), alerts: alerts, alerts_text: alerts.join(' / '), desc: weatherAvailable ? ripleyWmoDescription(code) : '', wind: wind == null ? '' : Math.round((Number(wind) || 0) * (windMetric ? 1.60934 : 1)), windUnit: windMetric ? 'km/h' : 'mph' };
}
function ripleyWmoSvg(code, isNight) {
  return ripleyWeatherIconSvg(code, isNight);
}
function ripleyWmoSplitHtml(morningCode, dayCode, untilText, icons) {
  var morningKey = String(morningCode), dayKey = String(dayCode);
  var leftUrl = icons[morningKey] || '';
  var rightUrl = icons[dayKey] || '';
  var left = leftUrl ? '<img src="' + ripleyTplEsc(leftUrl) + '" alt="' + ripleyTplEsc(ripleyWmoDescription(morningCode)) + '" style="width:100%;height:100%;object-fit:contain;">' : ripleyWmoSvg(morningCode, false);
  var right = rightUrl ? '<img src="' + ripleyTplEsc(rightUrl) + '" alt="' + ripleyTplEsc(ripleyWmoDescription(dayCode)) + '" style="width:100%;height:100%;object-fit:contain;">' : ripleyWmoSvg(dayCode, false);
  var label = untilText ? ' title="' + ripleyTplEsc(ripleyWmoDescription(morningCode) + ' ' + untilText + '; then ' + ripleyWmoDescription(dayCode)) + '"' : '';
  return '<div class="ripley-weather-morning-split"' + label + ' style="display:flex;align-items:center;justify-content:center;width:100%;height:100%;gap:1%;">' +
    '<div style="width:48%;height:100%;overflow:hidden;">' + left + '</div>' +
    '<div aria-hidden="true" style="width:4%;font-size:18%;line-height:1;text-align:center;color:#64748b;">→</div>' +
    '<div style="width:48%;height:100%;overflow:hidden;">' + right + '</div></div>';
}
function ripleyTrafficVariableText(widgetProps, key) {
  var events = Array.isArray(widgetProps && widgetProps._traffic_data && widgetProps._traffic_data.events)
    ? widgetProps._traffic_data.events
    : [];
  var normalizedKey = key || 'traffic_summary';
  if (normalizedKey === 'traffic_incidents') {
    return String(events.filter(function(e){ return e.category === 'crash'; }).length);
  }
  if (normalizedKey === 'traffic_crashes_closures') {
    var crashClosureEvents = events.filter(function(e){ return e.category === 'crash' || e.category === 'closure'; });
    return crashClosureEvents.length ? String(crashClosureEvents[0].description || '') : '';
  }
  if (normalizedKey === 'traffic_closures') {
    return String(events.filter(function(e){ return e.category === 'closure'; }).length);
  }
  if (normalizedKey === 'traffic_delays') {
    return String(events.filter(function(e){ return e.category === 'delay'; }).length);
  }
  return events.length + ' traffic event' + (events.length === 1 ? '' : 's');
}
function ripleyTrafficMapPlaceholderHtml() {
  return '<span aria-label="Traffic event map unavailable" style="display:flex;align-items:center;justify-content:center;width:100%;height:100%;font-size:42%;background:linear-gradient(135deg,#243447,#0f172a);color:#f8fafc;">🗺️</span>';
}
function ripleyTrafficMapImageHtml(event) {
  if (!event) return ripleyTrafficMapPlaceholderHtml();
  var lat = Number(event.latitude), lon = Number(event.longitude);
  if (!Number.isFinite(lat) || !Number.isFinite(lon) || lat < -90 || lat > 90 || lon < -180 || lon > 180) return ripleyTrafficMapPlaceholderHtml();
  var category = String(event.category || 'other');
  var label = [event.road || 'Traffic event', event.direction || '', event.description || ''].filter(Boolean).join(' · ');
  var src = '/api/traffic/map.png?lat=' + encodeURIComponent(lat.toFixed(6)) + '&lon=' + encodeURIComponent(lon.toFixed(6)) + '&category=' + encodeURIComponent(category);
  return '<span style="display:block;position:relative;width:100%;height:100%;overflow:hidden;background:#0f172a;">' +
    '<img data-ripley-fallback="1" src="' + ripleyTplEsc(src) + '" alt="' + ripleyTplEsc(label) + '" loading="eager" style="display:block;width:100%;height:100%;object-fit:cover;">' +
    '<span aria-hidden="true" style="display:none;align-items:center;justify-content:center;width:100%;height:100%;font-size:42%;background:linear-gradient(135deg,#243447,#0f172a);color:#f8fafc;">🗺️</span>' +
    '</span>';
}

function ripleyTrafficEventsForRepeatGroup(widgetProps, el) {
  var events = Array.isArray(widgetProps && widgetProps._traffic_data && widgetProps._traffic_data.events)
    ? widgetProps._traffic_data.events.slice()
    : [];
  var group = el && el.repeat && el.repeat.group;
  var elements = Array.isArray(widgetProps && widgetProps.template_elements) ? widgetProps.template_elements : [];
  var keys = elements.filter(function(item) {
    return group && item.repeat && item.repeat.group === group && item.props && item.props.variable_source === 'traffic';
  }).map(function(item) { return item.props.variable_key || ''; });
  var key = keys.find(function(value) { return value; }) || (widgetProps && widgetProps.traffic_variable) || '';
  if (key === 'traffic_crashes_closures') return events.filter(function(event) { return event.category === 'crash' || event.category === 'closure'; });
  if (key === 'traffic_incidents') return events.filter(function(event) { return event.category === 'crash'; });
  if (key === 'traffic_closures') return events.filter(function(event) { return event.category === 'closure'; });
  if (key === 'traffic_delays') return events.filter(function(event) { return event.category === 'delay'; });
  return events;
}

function ripleyTplSample(el, widgetProps) {
  var p = el.props || {};
  var source = p.variable_source || (el.type === 'variable_icon' ? 'weather' : 'date');
  var key = p.variable_key || (el.type === 'variable_icon' ? 'weather_icon' : 'full_date');
  var weatherOffset = Math.max(0, Math.min(6, Number(p.day_offset ?? p.weather_day_offset) || 0));
  var wx = ripleyWeatherForOffset(widgetProps || {}, weatherOffset);
  var severeWeatherIcon = key === 'weather_icon' && p.weather_icon_alerts && weatherOffset === 0 && wx.alerts.length
    ? ripleyWeatherAlertConditionIconSvg(wx.alerts)
    : '';
  if (severeWeatherIcon) return {html: severeWeatherIcon, empty: false};
  if (key === 'weather-alert-code' && (el.type === 'variable_icon' || p.display_mode === 'weather_icon')) {
    return {html: ripleyWeatherAlertColorCodeIconSvg(wx.alerts), empty: wx.alerts.length === 0};
  }
  if ((key === 'weather_alerts' || key === 'weather-alerts') && (el.type === 'variable_icon' || p.display_mode === 'weather_icon')) {
    var alertConditionIcon = ripleyWeatherAlertConditionIconSvg(wx.alerts);
    return {html: alertConditionIcon, empty: alertConditionIcon === ''};
  }
  if (source === 'weather' && (key === 'weather_icon' || el.type === 'variable_icon' || p.display_mode === 'weather_icon')) {
    if (!wx.weather_available) return {html: '', empty: true};
    var icons = (widgetProps && widgetProps.custom_icons) || {};
    var autoNight = p.weather_auto_night !== false;
    var night = autoNight && !!wx.is_night;
    if (p.weather_icon_mode === 'morning_split' && !night && wx.has_morning_transition) {
      return {html: ripleyWmoSplitHtml(wx.morning_code, wx.day_code, wx.morning_until_text, icons)};
    }
    var iconCode = weatherOffset === 0 ? wx.code : (night ? wx.night_code : wx.day_code);
    var iconKey = String(iconCode) + (night ? '_night' : '');
    var iconUrl = icons[iconKey] || icons[String(iconCode)] || '';
    return {html: iconUrl ? '<img src="' + ripleyTplEsc(iconUrl) + '" style="max-width:100%;max-height:100%;object-fit:contain;">' : ripleyWmoSvg(iconCode, night)};
  }
  if (source === 'date') {
    if (key === 'event') return {text: String(weatherOffset + 1)};
    var d = new Date();
    d.setDate(d.getDate() + (Number(p.day_offset ?? p.weather_day_offset) || 0));
    if (key === 'day_name') return {text: d.toLocaleDateString(undefined, {weekday:'long'})};
    if (key === 'date_number') return {text: String(d.getDate())};
    if (key === 'month_day') return {text: d.toLocaleDateString(undefined, {month:'short', day:'numeric'})};
    return {text: d.toLocaleDateString(undefined, {weekday:'long', month:'short', day:'numeric'})};
  }
  if (source === 'calendar') {
    var calendarIndex = Math.max(0, Math.min(29, Number(p.calendar_event_index ?? p.day_offset ?? p.weather_day_offset) || 0));
    var event = Array.isArray(widgetProps._calendar_events) ? widgetProps._calendar_events[calendarIndex] : null;
    var n = calendarIndex + 1;
    if (event) {
      if (key === 'calendar_event_time') return {text: event.time || '', empty: !event.time};
      if (key === 'calendar_event_location') return {text: event.location || '', empty: !event.location};
      if (key === 'calendar_event_title') return {text: event.summary || '', empty: !event.summary};
      if (key === 'calendar_event_date') {
        var eventDate = new Date(String(event.date || '') + 'T00:00:00');
        var eventDateText = isNaN(eventDate.getTime()) ? '' : eventDate.toLocaleDateString(undefined, {month:'short', day:'numeric', year:'numeric'});
        return {text: eventDateText, empty: eventDateText === ''};
      }
    }
    if (key === 'calendar_event_time') return {text:'Day ' + n + ' 9:00 AM', empty: true};
    if (key === 'calendar_event_location') return {text:'Day ' + n + ' location', empty: true};
    return {text:'Day ' + n + ' event', empty: true};
  }
  if (source === 'traffic') {
    var trafficEvents = Array.isArray(widgetProps && widgetProps._traffic_data && widgetProps._traffic_data.events) ? widgetProps._traffic_data.events : [];
    var trafficIndex = Math.max(0, Math.min(29, Number(p.traffic_event_index ?? p.day_offset ?? p.weather_day_offset) || 0));
    var trafficVariableEvents = ripleyTrafficEventsForRepeatGroup(widgetProps, el);
    var trafficEvent = trafficVariableEvents[trafficIndex];
    var trafficCount = trafficVariableEvents.length;
    if (key === 'traffic_incidents') return {text: String(trafficVariableEvents.filter(function(e){ return e.category === 'crash'; }).length), empty: trafficCount === 0};
    if (key === 'traffic_crashes_closures') {
      return {text: trafficEvent && trafficEvent.description || '', empty: !trafficEvent || !trafficEvent.description};
    }
    if (key === 'traffic_closures') return {text: String(trafficVariableEvents.filter(function(e){ return e.category === 'closure'; }).length), empty: trafficCount === 0};
    if (key === 'traffic_delays') return {text: String(trafficVariableEvents.filter(function(e){ return e.category === 'delay'; }).length), empty: trafficCount === 0};
    if (!trafficEvent) {
      if (key === 'traffic_event_map' && !(widgetProps && widgetProps._traffic_data)) return {html: ripleyTrafficMapPlaceholderHtml(), empty: false};
      return {text: '', empty: true};
    }
    if (key === 'traffic_event_map') return {html: ripleyTrafficMapImageHtml(trafficEvent), empty: false};
    if (key === 'traffic_event_road') return {text: trafficEvent.road || '', empty: !trafficEvent.road};
    if (key === 'traffic_event_direction') return {text: trafficEvent.direction || '', empty: !trafficEvent.direction};
    if (key === 'traffic_event_description') return {text: trafficEvent.description || '', empty: !trafficEvent.description};
    if (key === 'traffic_event_category') return {text: trafficEvent.category || 'other', empty: false};
    if (key === 'traffic_event_distance') return {text: trafficEvent.distance_miles == null ? '' : Number(trafficEvent.distance_miles).toFixed(1) + ' mi', empty: trafficEvent.distance_miles == null};
    return {text: ripleyTrafficVariableText(widgetProps, 'traffic_summary'), empty: trafficCount === 0};
  }
  if (source === 'weather') {
    var unit = String((widgetProps && (widgetProps.unit || (widgetProps.units === 'metric' ? 'C' : 'F'))) || 'F').toUpperCase() === 'F' ? '°F' : '°C';
    if (key === 'current_temp') return {text: wx.current === '' ? '' : wx.current + unit, empty: wx.current === ''};
    if (key === 'high_temp') return {text: wx.high === '' ? '' : 'High ' + wx.high + unit, empty: wx.high === ''};
    if (key === 'low_temp') return {text: wx.low === '' ? '' : 'Low ' + wx.low + unit, empty: wx.low === ''};
    if (key === 'weather_description') return {text: wx.desc, empty: wx.desc === ''};
    if (key === 'weather_alerts' || key === 'weather-alerts' || key === 'weather-alert-code') return {text: wx.alerts_text, empty: wx.alerts.length === 0};
    if (key === 'morning_weather_condition') return {text: wx.morning_desc, empty: wx.morning_desc === ''};
    if (key === 'morning_weather_until') return {text: wx.morning_until_text, empty: wx.morning_until_text === ''};
    if (key === 'morning_weather_summary' || key === 'morning_weather') return {text: wx.morning_summary, empty: wx.morning_summary === ''};
    if (key === 'precip_chance') return {text: wx.precip === '' ? '' : wx.precip + '%', empty: wx.precip === ''};
    if (key === 'wind_speed') return {text: wx.wind === '' ? '' : 'Wind ' + wx.wind + ' ' + wx.windUnit, empty: wx.wind === ''};
  }
  return {text: p.default_text || p.label || key};
}
function ripleyTemplateRepeatCount(widgetProps, el) {
  var rep = el.repeat || {};
  // The template editor stores repeat groups as repeat.group + gap_y.  The
  // actual number of rows belongs to the layout widget (weather shown_days,
  // calendar max_events), so layout/player rendering must expand grouped
  // elements here instead of relying only on repeat.count.
  if (rep.group) {
    var isCalendar = widgetProps && String(widgetProps._widget_type || widgetProps.widget_type || widgetProps.type || '').toLowerCase() === 'calendar';
    if (rep.hide_if_empty && isCalendar && Array.isArray(widgetProps._calendar_events)) {
      var requestedCalendarCount = Math.max(1, Math.min(30, Number(widgetProps.max_events) || Number(rep.count) || 5));
      return Math.min(requestedCalendarCount, widgetProps._calendar_events.length);
    }
    if (widgetProps && Array.isArray(widgetProps.template_elements)) {
      var wt = String(widgetProps._widget_type || widgetProps.widget_type || widgetProps.type || '').toLowerCase();
      if (wt === 'calendar') return Math.max(1, Math.min(30, Number(widgetProps.max_events) || Number(rep.count) || 5));
      if (wt === 'traffic') {
        var trafficEvents = ripleyTrafficEventsForRepeatGroup(widgetProps, el);
        var trafficCount = trafficEvents.length;
        var requestedTrafficCount = Math.max(1, Math.min(10, Number(widgetProps.max_events) || Number(rep.count) || 3));
        var hideTrafficGroup = ripleyRepeatFlag(rep.hide_if_empty) || ripleyRepeatFlag(widgetProps.hide_if_empty);
        if (hideTrafficGroup && trafficCount === 0) return 0;
        return hideTrafficGroup ? Math.min(requestedTrafficCount, trafficCount) : requestedTrafficCount;
      }
    }
    return Math.max(1, Math.min(30, Number((widgetProps || {}).shown_days) || Number(rep.count) || 4));
  }
  return Math.max(1, Math.min(30, Number(rep.count) || 1));
}
function ripleyTemplateRepeatSizingCount(widgetProps, el) {
  var actual = ripleyTemplateRepeatCount(widgetProps, el);
  var rep = el.repeat || {};
  if (!rep.group) return actual;
  // Repeat settings are stored on each element for backwards compatibility,
  // but preserve-size is a group-level behavior. Older templates can have it
  // enabled on the image/text members and omitted on a rectangle background.
  // Resolve the effective value across the whole group so the first background
  // row uses the same bottom-up anchor as its sibling elements.
  var groupPreservesSize = Array.isArray(widgetProps && widgetProps.template_elements) && widgetProps.template_elements.some(function(item) {
    return item && item.repeat && item.repeat.group === rep.group && ripleyRepeatFlag(item.repeat.preserve_size);
  });
  if (!groupPreservesSize) return actual;
  var wt = String(widgetProps && (widgetProps._widget_type || widgetProps.widget_type || widgetProps.type) || '').toLowerCase();
  var requested;
  if (wt === 'traffic') requested = Number(widgetProps.max_events) || Number(rep.count) || 3;
  else if (wt === 'calendar') requested = Number(widgetProps.max_events) || Number(rep.count) || 5;
  else requested = Number(widgetProps.shown_days) || Number(rep.count) || 4;
  return Math.max(actual, Math.min(30, requested));
}
function ripleyTemplateRepeatMode(el) {
  var rep = el.repeat || {};
  if (rep.group && (!rep.mode || rep.mode === 'none')) return 'rows';
  return rep.mode || 'none';
}
function ripleyRepeatFlag(value) {
  return value === true || value === 1 || /^(true|1|yes|on)$/i.test(String(value || ''));
}
function ripleyTemplateGroupMetrics(elements) {
  var groups = {};
  elements.forEach(function(el) {
    var group = el.repeat && el.repeat.group;
    if (!group) return;
    var r = el.repeat || {};
    var m = groups[group] || (groups[group] = {minX:Infinity,minY:Infinity,maxX:-Infinity,maxY:-Infinity,gapX:Number(r.gap_x)||0,gapY:Number(r.gap_y)||0});
    var x = Number(el.x)||0, y = Number(el.y)||0, w = Number(el.width)||120, h = Number(el.height)||80;
    m.minX = Math.min(m.minX, x); m.minY = Math.min(m.minY, y); m.maxX = Math.max(m.maxX, x+w); m.maxY = Math.max(m.maxY, y+h);
    m.gapX = Number(r.gap_x)||m.gapX; m.gapY = Number(r.gap_y)||m.gapY;
  });
  return groups;
}
function ripleyCornerRadiusCss(p) {
  p = p || {};
  var base = Number(p.radius == null ? 0 : p.radius) || 0;
  var tl = Number(p.radius_tl == null ? base : p.radius_tl) || 0;
  var tr = Number(p.radius_tr == null ? base : p.radius_tr) || 0;
  var br = Number(p.radius_br == null ? base : p.radius_br) || 0;
  var bl = Number(p.radius_bl == null ? base : p.radius_bl) || 0;
  return tl + 'px ' + tr + 'px ' + br + 'px ' + bl + 'px';
}
function ripleyCornerRadiusClipCss(p) {
  return 'inset(0 round ' + ripleyCornerRadiusCss(p) + ')';
}
function ripleyShapeOpacity(value) {
  var n = Number(value);
  if (!Number.isFinite(n)) return 1;
  if (n >= 0 && n <= 1) return n;
  return Math.max(0, Math.min(100, n)) / 100;
}
function ripleyTemplateRepeatPosition(widgetProps, el, index, groups) {
  var group = el.repeat && el.repeat.group, m = group && groups[group];
  var x = Number(el.x)||0, y = Number(el.y)||0;
  if (!m) return {x:x,y:y};
  var mode = ripleyTemplateRepeatMode(el);
  var rep = el.repeat || {};
  var order = String(rep.order || '').toLowerCase().replace(/_/g, '-') === 'bottom-up' ? 'bottom-up' : 'top-down';
  if (index <= 0 && order !== 'bottom-up') return {x:x,y:y};
  if (mode === 'columns') x += index * ((m.maxX-m.minX) + m.gapX);
  if (mode === 'rows') {
    var rowStep = (m.maxY-m.minY) + m.gapY;
    if (order === 'bottom-up') {
      var repeatCount = ripleyTemplateRepeatSizingCount(widgetProps, el);
      var requiredHeight = m.maxY + Math.max(0, repeatCount - 1) * rowStep;
      var canvasHeight = Math.max(Number(widgetProps && widgetProps.template_height) || 0, requiredHeight);
      y = canvasHeight - (m.maxY-m.minY) + (Number(el.y)||0) - m.minY - index * rowStep;
    } else {
      y += index * rowStep;
    }
  }
  return {x:x,y:y};
}

function ripleyTemplateCopyProps(el, repeatIndex) {
  var p = Object.assign({}, el.props || {});
  var source = p.variable_source || (el.type === 'variable_icon' ? 'weather' : 'date');
  if (source === 'calendar') {
    var baseEventIndex = Math.max(0, Math.min(29, Number(p.calendar_event_index) || 0));
    var isRepeatGroup = !!(el.repeat && el.repeat.group);
    p.calendar_event_index = baseEventIndex + (isRepeatGroup ? repeatIndex + 1 : 0);
  }
  if (repeatIndex <= 0) return p;
  if (source === 'weather' || p.display_mode === 'weather_icon' || p.variable_key === 'weather_icon') {
    var baseWeather = Math.max(0, Math.min(29, Number(p.weather_day_offset ?? p.day_offset) || 0));
    p.weather_day_offset = baseWeather + repeatIndex;
    p.day_offset = baseWeather + repeatIndex;
  }
  if (source === 'traffic') {
    var baseTrafficIndex = Math.max(0, Math.min(29, Number(p.traffic_event_index ?? p.day_offset ?? p.weather_day_offset) || 0));
    p.traffic_event_index = baseTrafficIndex + repeatIndex;
    p.day_offset = baseTrafficIndex + repeatIndex;
  }
  if (source === 'date' || source === 'calendar') {
    var baseDay = Math.max(0, Math.min(29, Number(p.day_offset ?? p.weather_day_offset) || 0));
    p.day_offset = baseDay + repeatIndex;
    p.weather_day_offset = baseDay + repeatIndex;
  }
  return p;
}

function ripleyTemplateExpandedBounds(widgetProps, elements) {
  var width = Number(widgetProps.template_width || 760) || 760;
  var height = Number(widgetProps.template_height || 427.5) || 427.5;
  var groups = ripleyTemplateGroupMetrics(elements);
  elements.forEach(function(el) {
    var rep = el.repeat || {mode:'none', count:1, gap_x:0, gap_y:0};
    var count = ripleyTemplateRepeatSizingCount(widgetProps, el);
    for (var i=0; i<count; i++) {
      var pos = ripleyTemplateRepeatPosition(widgetProps, el, i, groups);
      var x = pos.x, y = pos.y;
      var w = Number(el.width)||120;
      var h = Number(el.height)||80;
      width = Math.max(width, x + w);
      height = Math.max(height, y + h);
    }
  });
  return { width: width, height: height };
}
function ripleyTemplateElementsHTML(widgetProps) {
  var elements = Array.isArray(widgetProps.template_elements) ? widgetProps.template_elements : [];
  if (!elements.length) return '';
  var bounds = ripleyTemplateExpandedBounds(widgetProps, elements);
  var designW = Math.max(1, Number(bounds.width) || Number(widgetProps.template_width || 760) || 760);
  var designH = Math.max(1, Number(bounds.height) || Number(widgetProps.template_height || 427.5) || 427.5);
  var out = '<div class="ripley-template-elements-render" style="position:relative;width:100%;height:100%;overflow:hidden;container-type:size;">';
  var groups = ripleyTemplateGroupMetrics(elements);
  elements.slice().sort(function(a,b){ return (Number(a.z)||0) - (Number(b.z)||0); }).forEach(function(el){
    var rep = el.repeat || {mode:'none', count:1, gap_x:0, gap_y:0};
    var count = ripleyTemplateRepeatCount(widgetProps, el);
    for (var i=0; i<count; i++) {
      var pos = ripleyTemplateRepeatPosition(widgetProps, el, i, groups);
      var x = pos.x, y = pos.y;
      var w = Number(el.width)||120, h = Number(el.height)||80;
      var p = ripleyTemplateCopyProps(el, i);
      var renderEl = Object.assign({}, el, { props: p });
      var align = p.align || 'center';
      var justify = align === 'left' ? 'flex-start' : (align === 'right' ? 'flex-end' : 'center');
      var style = 'position:absolute;box-sizing:border-box;overflow:hidden;display:flex;align-items:center;justify-content:' + justify + ';text-align:' + ripleyTplEsc(align) + ';' +
        'left:' + (x/designW*100) + '%;top:' + (y/designH*100) + '%;width:' + (w/designW*100) + '%;height:' + (h/designH*100) + '%;z-index:' + (Number(el.z)||0) + ';';
      var inner = '';
      var fitAttrs = '';
      if (el.type === 'rectangle') {
        var shape = p.shape || 'rounded';
        var base = Number(p.radius == null ? 10 : p.radius) || 0;
        var rtl = Number(p.radius_tl == null ? base : p.radius_tl) || 0;
        var rtr = Number(p.radius_tr == null ? base : p.radius_tr) || 0;
        var rbr = Number(p.radius_br == null ? base : p.radius_br) || 0;
        var rbl = Number(p.radius_bl == null ? base : p.radius_bl) || 0;
        var radius = shape === 'circle' ? '50%' : (shape === 'pill' ? '999px' : (rtl+'px '+rtr+'px '+rbr+'px '+rbl+'px'));
        var opacity = ripleyShapeOpacity(p.opacity == null ? 100 : p.opacity);
        style += 'border-radius:' + radius + ';';
        if (typeof ripleyShapeClipPath === 'function') { var clip = ripleyShapeClipPath(shape); if (clip) style += 'clip-path:' + clip + ';-webkit-clip-path:' + clip + ';'; }
        if (shape === 'line') style += 'height:6px;';
        inner = '<span class="ripley-template-shape-fill" style="position:absolute;inset:0;background:' + ripleyTplEsc(p.color || '#1e293b') + ';border-radius:' + radius + ';opacity:' + opacity + ';' + (typeof ripleyShapeClipPath === 'function' && ripleyShapeClipPath(shape) ? 'clip-path:' + ripleyShapeClipPath(shape) + ';-webkit-clip-path:' + ripleyShapeClipPath(shape) + ';' : '') + '"></span>';
      } else if (el.type === 'image') {
         var imageRadius = ripleyCornerRadiusCss(p);
         var imageClip = ripleyCornerRadiusClipCss(p);
         var imageFit = ['contain', 'cover', 'fill', 'none', 'scale-down'].indexOf(p.fit) >= 0 ? p.fit : 'contain';
         style += 'border-radius:' + imageRadius + ';clip-path:' + imageClip + ';-webkit-clip-path:' + imageClip + ';';
         inner = p.url ? '<img src="' + ripleyTplEsc(p.url) + '" alt="' + ripleyTplEsc(p.alt || '') + '" style="width:100%;height:100%;object-fit:' + imageFit + ';border-radius:' + imageRadius + ';clip-path:' + imageClip + ';-webkit-clip-path:' + imageClip + ';">' : '';
      } else if (el.type === 'text') {
        var fs = Number(p.font_size)||24;
        style += 'color:' + ripleyTplEsc(p.color || '#fff') + ';font-size:clamp(6px,calc(' + (fs / designW * 100) + 'cqw),' + (fs * 4) + 'px);font-weight:' + ripleyTplEsc(p.font_weight || 700) + ';';
        inner = ripleyTplEsc(p.text || 'Text box');
      } else if (el.type === 'variable_text' || el.type === 'variable_icon' || el.type === 'variable' || el.type === 'date') {
        var sample = ripleyTplSample(renderEl, widgetProps);
        if (p.hide_if_empty && sample.empty) continue;
        var fs = Number(p.font_size)||26;
        style += 'color:' + ripleyTplEsc(p.color || '#fff') + ';font-size:clamp(4px,min(calc(' + (fs / designW * 100) + 'cqw),calc(' + (fs / designH * 100) + 'cqh)),' + (fs * 4) + 'px);font-weight:800;line-height:1.08;';
        inner = sample.html || ripleyTplEsc(sample.text || '');
        if (p.variable_source === 'traffic' && el.type !== 'variable_icon') {
          var fitGroup = el.repeat && el.repeat.group ? String(el.repeat.group) : 'traffic-' + String(el.type || 'variable') + '-' + String(i);
          inner = '<span class="ripley-template-variable-text" style="display:block;width:100%;max-width:100%;max-height:100%;overflow-wrap:anywhere;word-break:break-word;line-height:1.08;">' + inner + '</span>';
          style += 'white-space:normal;overflow-wrap:anywhere;word-break:break-word;';
          var fitAttrs = ' data-ripley-fit-text="1" data-ripley-fit-group="' + ripleyTplEsc(fitGroup) + '"';
        } else {
          var fitAttrs = '';
        }
      } else {
        inner = ripleyTplEsc(p.label || el.type || '');
      }
      out += '<div class="ripley-template-el ripley-template-el-' + ripleyTplEsc(el.type || 'unknown') + '"' + fitAttrs + ' style="' + style + '">' + inner + '</div>';
    }
  });
  out += '</div>';
  return out;
}
function ripleyFitTrafficTemplateText(root) {
  if (!root || !root.querySelectorAll) return;
  var nodes = Array.prototype.slice.call(root.querySelectorAll('[data-ripley-fit-text="1"]'));
  if (!nodes.length) return;
  var run = function() {
    var groups = {};
    nodes.forEach(function(node) {
      if (!node.isConnected) return;
      var width = node.clientWidth, height = node.clientHeight;
      if (width <= 0 || height <= 0) return;
      var base = Number(node.dataset.ripleyFitBaseFont);
      if (!Number.isFinite(base) || base <= 0) {
        base = parseFloat(getComputedStyle(node).fontSize) || 26;
        node.dataset.ripleyFitBaseFont = String(base);
      }
      var fits = function(size) {
        node.style.fontSize = size + 'px';
        return node.scrollWidth <= node.clientWidth + 1 && node.scrollHeight <= node.clientHeight + 1;
      };
      var low = 4, high = base;
      if (!fits(high)) {
        if (!fits(low)) high = low;
        else {
          for (var i = 0; i < 10; i++) {
            var mid = (low + high) / 2;
            if (fits(mid)) low = mid; else high = mid;
          }
        }
      } else low = high;
      var ratio = Math.max(0.1, Math.min(1, low / base));
      var group = node.dataset.ripleyFitGroup || node.dataset.ripleyFitBaseFont;
      if (!groups[group] || ratio < groups[group]) groups[group] = ratio;
    });
    nodes.forEach(function(node) {
      var base = Number(node.dataset.ripleyFitBaseFont);
      var ratio = groups[node.dataset.ripleyFitGroup || node.dataset.ripleyFitBaseFont];
      if (Number.isFinite(base) && Number.isFinite(ratio)) node.style.fontSize = Math.max(4, base * ratio) + 'px';
    });
  };
  if (!root.__ripleyTrafficFitObserver && typeof ResizeObserver !== 'undefined') {
    root.__ripleyTrafficFitObserver = new ResizeObserver(function() { requestAnimationFrame(run); });
    root.__ripleyTrafficFitObserver.observe(root);
  }
  requestAnimationFrame(run);
}
function lg(m){playerFetch('/api/log',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({msg:m})}).catch(function(){});}


function ripleyCalendarRequiredEventCount(widgetProps) {
  var baseCount = Math.max(1, Math.min(30, Number(widgetProps && widgetProps.max_events) || 7));
  var required = baseCount;
  var elements = widgetProps && Array.isArray(widgetProps.template_elements) ? widgetProps.template_elements : [];
  elements.forEach(function(el) {
    var p = el.props || {};
    var source = p.variable_source || (el.type === 'variable_icon' ? 'weather' : 'date');
    if (source !== 'calendar') return;
    var baseIndex = Math.max(0, Math.min(29, Number(p.calendar_event_index ?? p.day_offset ?? p.weather_day_offset) || 0));
    if (el.repeat && el.repeat.group) {
      var count = Math.max(1, Math.min(30, Number(widgetProps.max_events) || Number(el.repeat.count) || 5));
      // The first repeated row intentionally starts after the non-repeated event.
      required = Math.max(required, baseIndex + count + 1);
    } else {
      required = Math.max(required, baseIndex + 1);
    }
  });
  return Math.max(1, Math.min(30, required));
}

function ripleyParseCalendarText(raw, maxEvents, timeFormat) {
  var unfolded = String(raw || '').replace(/\r?\n[ \t]/g, ''), out = [];
  var blocks = unfolded.match(/BEGIN:VEVENT[\s\S]*?END:VEVENT/g) || [];
  blocks.forEach(function(block) {
    var start = (block.match(/(?:^|\n)DTSTART(?:;[^:]+)?:([^\n]+)/) || [])[1];
    if (start) start = start.replace(/\r$/, '');
    var summary = (block.match(/(?:^|\n)SUMMARY:(.*)/) || [,''])[1].trim();
    var location = (block.match(/(?:^|\n)LOCATION:(.*)/) || [,''])[1].trim();
    if (!start || !summary) return;
    var date = start.match(/^(\d{4})(\d{2})(\d{2})/);
    var time = '';
    if (date) {
      var hh = (start.match(/T(\d{2})(\d{2})/) || [])[1], mm = (start.match(/T\d{2}(\d{2})/) || [])[1];
      var hhNum = hh == null ? null : Number(hh), minuteText = mm || '00';
      if (hhNum == null) time = 'All day';
      else if (timeFormat === '24') time = String(hhNum).padStart(2, '0') + ':' + minuteText;
      else { var ap = hhNum >= 12 ? 'PM' : 'AM'; time = (hhNum % 12 || 12) + ':' + minuteText + ' ' + ap; }
      out.push({summary: summary, location: location, time: time, date: date[1] + '-' + date[2] + '-' + date[3]});
    }
  });
  var todayKey = new Date().toISOString().slice(0, 10);
  return out.filter(function(event){ return event.date >= todayKey; }).sort(function(a,b){return a.date.localeCompare(b.date) || a.time.localeCompare(b.time);}).slice(0, Math.max(1, Math.min(30, Number(maxEvents) || 7)));
}
async function ripleyLoadCalendarData(widgetProps) {
  var url = widgetProps.ics_url || widgetProps.ical_url || widgetProps.ical || widgetProps.calendar_url || '';
  if (!url || !Array.isArray(widgetProps.template_elements) || !widgetProps.template_elements.length) return [];
  try {
    var response = await playerFetch('/api/ics-proxy?url=' + encodeURIComponent(url), {cache:'no-store'});
    if (!response.ok) throw new Error('calendar ' + response.status);
    widgetProps._calendar_events = ripleyParseCalendarText(await readPlayerTextBounded(response), ripleyCalendarRequiredEventCount(widgetProps), widgetProps.time_format === '24' ? '24' : '12');
  } catch (_) { widgetProps._calendar_events = []; }
  return widgetProps._calendar_events;
}

function ripleyUnavailableWeatherData() {
  return {
    source: 'unavailable',
    weather_meta: {stale: true, stale_reason: 'Weather provider unavailable and no previous reading exists'},
    current_weather: {temperature: null, weathercode: null, windspeed: null, is_daytime: true},
    daily: {
      weather_code_day: [null, null, null, null, null, null, null],
      weather_code_night: [null, null, null, null, null, null, null],
      weather_code_morning: [null, null, null, null, null, null, null],
      weather_morning_until: [null, null, null, null, null, null, null],
      temperature_2m_max: [null, null, null, null, null, null, null],
      temperature_2m_min: [null, null, null, null, null, null, null],
      precipitation_probability_max: [null, null, null, null, null, null, null],
      wind_speed: [null, null, null, null, null, null, null]
    }
  };
}

async function ripleyLoadWeatherData(widgetProps) {
  var lat = Number(widgetProps.lat || 0);
  var lon = Number(widgetProps.lon || 0);
  if (!lat || !lon) {
    widgetProps._weather_data = ripleySampleWeatherData();
    widgetProps._weather_data_source = 'sample';
    return widgetProps._weather_data;
  }
  try {
    var response = await playerFetch('/api/weather/forecast?lat=' + encodeURIComponent(lat) + '&lon=' + encodeURIComponent(lon), {cache: 'no-store'});
    if (!response.ok) throw new Error('weather ' + response.status);
    widgetProps._weather_data = await readPlayerJsonBounded(response);
    widgetProps._weather_data_source = 'live';
  } catch (e) {
    console.warn('weather provider unavailable', e);
    if (widgetProps._weather_data && widgetProps._weather_data_source !== 'sample' && widgetProps._weather_data_source !== 'unavailable') {
      widgetProps._weather_data = JSON.parse(JSON.stringify(widgetProps._weather_data));
      widgetProps._weather_data.weather_meta = Object.assign({}, widgetProps._weather_data.weather_meta, {
        stale: true,
        stale_reason: 'Weather provider unavailable; showing last successful reading'
      });
      widgetProps._weather_data_source = 'last_known_good';
    } else {
      widgetProps._weather_data = ripleyUnavailableWeatherData();
      widgetProps._weather_data_source = 'unavailable';
    }
  }
  return widgetProps._weather_data;
}

async function ripleyLoadTrafficData(widgetProps, widgetId) {
  var lat = Number(widgetProps.lat || 0);
  var lon = Number(widgetProps.lon || 0);
  if (!lat || !lon) {
    widgetProps._traffic_data = {configured: false, events: []};
    widgetProps._traffic_data_source = 'unavailable';
    return widgetProps._traffic_data;
  }
  try {
    var url = '/api/traffic/events?lat=' + encodeURIComponent(lat) + '&lon=' + encodeURIComponent(lon) + '&radius_miles=' + encodeURIComponent(widgetProps.radius_miles || 25);
    var response = await playerFetch(url, {cache: 'no-store'});
    if (!response.ok) throw new Error('traffic ' + response.status);
    widgetProps._traffic_data = await readPlayerJsonBounded(response);
    if (typeof ripleyFilterTrafficEvents === 'function' && widgetProps._traffic_data.configured !== false && widgetProps._traffic_data.available !== false) {
      widgetProps._traffic_data.events = ripleyFilterTrafficEvents(widgetProps._traffic_data.events, widgetProps, widgetId);
    }
    widgetProps._traffic_data_source = widgetProps._traffic_data.configured ? 'live' : 'unconfigured';
  } catch (e) {
    console.warn('traffic provider unavailable', e);
    widgetProps._traffic_data = {configured: false, events: [], message: 'Traffic provider unavailable'};
    widgetProps._traffic_data_source = 'unavailable';
  }
  return widgetProps._traffic_data;
}

async function ripleyHydrateTrafficTemplates(layout) {
  var widgets = layout && Array.isArray(layout.widgets) ? layout.widgets : [];
  await Promise.all(widgets.filter(function(widget) {
    return widget && String(widget.widget_type || widget.type || '').toLowerCase() === 'traffic' && widget.props && widget.props.preset;
  }).map(async function(widget) {
    try {
      var response = await playerFetch('/api/widget-presets/' + encodeURIComponent(widget.props.preset), {cache:'no-store'});
      if (!response.ok) return;
      var preset = await readPlayerJsonBounded(response);
      var presetProps = preset && preset.props_json;
      if (typeof presetProps === 'string') presetProps = JSON.parse(presetProps || '{}');
      if (!presetProps || !Array.isArray(presetProps.template_elements)) return;
      widget.props.template_elements = presetProps.template_elements;
      if (presetProps.template_width != null) widget.props.template_width = presetProps.template_width;
      if (presetProps.template_height != null) widget.props.template_height = presetProps.template_height;
      if (presetProps.snap_grid != null) widget.props.snap_grid = presetProps.snap_grid;
    } catch (_) { /* retain the layout's last embedded template during outages */ }
  }));
  return layout;
}

function injectInstalledFontsIntoIframes(root) {
  if (!root) return;
  root.querySelectorAll('iframe').forEach(function(frame) {
    frame.addEventListener('load', function() {
      try {
        playerFetch('/api/fonts/installed').then(function(r){ return r.ok ? readPlayerJsonBounded(r) : []; }).then(function(fonts) {
          if (!frame.contentDocument || !Array.isArray(fonts)) return;
          var style = frame.contentDocument.createElement('style');
          style.textContent = fonts.map(function(f) {
            var family = String(f.family || '').replace(/["\\]/g, '');
            var url = String(f.url || '').replace(/[\'"\\]/g, '');
            return family && url ? '@font-face{font-family:"' + family + '";font-weight:' + (f.weight || 400) + ';font-style:normal;src:url("' + url + '") format("woff2");font-display:swap;}' : '';
          }).join('');
          frame.contentDocument.head.appendChild(style);
        }).catch(function(){});
      } catch (_) {}
    }, { once: true });
  });
}

function buildWidgetHTML(w) {
  // Layout editor responses historically used `type`/`embed`, while the
  // server/player contract uses `widget_type`/`webpage`. Normalize both forms
  // here so Preview and the live player render the same embedded page.
  w = Object.assign({}, w, {widget_type: String(w.widget_type || w.type || '').toLowerCase()});
  if (w.widget_type === 'embed') w.widget_type = 'webpage';
  var p = w.props || {};
  // Interactive widgets get a wrapping class so wireInteractiveRuntime can
  // find them, plus the data-interactive-* attributes the runtime uses.
  var interactive = {button:1, dropdown:1, radio:1, checkbox:1, 'checkbox-list':1, textbox:1, keyboard:1}[w.widget_type];
  var wrapClass = interactive ? ' class="interactive-live-widget"' : '';
  var wrapAttrs = ' data-ripley-widget-id="' + ripleyTplEsc(w.id || '') + '" data-ripley-widget-type="' + ripleyTplEsc(w.widget_type || '') + '"';
  if (interactive) {
    wrapAttrs += ' data-interactive-widget-id="' + ripleyTplEsc(w.id) + '" data-interactive-widget-type="' + ripleyTplEsc(w.widget_type) + '"';
  }
  var x = Number(w.x) || 0, y = Number(w.y) || 0, width = Number(w.w) || 0, height = Number(w.h) || 0;
  // API layout payloads use percentages; tolerate fractional cached payloads too.
  if (Math.abs(x) > 1 || Math.abs(y) > 1 || Math.abs(width) > 1 || Math.abs(height) > 1) {
    x /= 100; y /= 100; width /= 100; height /= 100;
  }
  var rotation = Number(p.rotation);
  if (!Number.isFinite(rotation)) rotation = 0;
  rotation = Math.max(-180, Math.min(180, rotation));
  var imageWrapStyle = w.widget_type === 'image' ? 'border-radius:' + ripleyCornerRadiusCss(p) + ';clip-path:' + ripleyCornerRadiusClipCss(p) + ';-webkit-clip-path:' + ripleyCornerRadiusClipCss(p) + ';' : '';
  var fontFamily = ripleyCssFontFamily(p.font_family || p.fontFamily);
  var styleStr = 'position:absolute;left:' + (x*100) + '%;top:' + (y*100) + '%;width:' + (width*100) + '%;height:' + (height*100) + '%;z-index:' + Math.max(0, Number(w.z) || 0) + ';overflow:hidden;transform:rotate(' + rotation + 'deg);transform-origin:center center;' + (fontFamily ? 'font-family:' + ripleyTplEsc(fontFamily) + ';' : '') + imageWrapStyle + ((w.widget_type === 'countdown' && p.hide_on_layout) || w.widget_type === 'sound' ? 'display:none;' : '') + (interactive ? 'display:flex;align-items:center;justify-content:center;' : '');
  var countdownFontSize = w.widget_type === 'countdown' ? ripleyCountdownFontSize(p.font_size) : 0;
  var runtimeAttrs = w.widget_type === 'countdown' ? ' data-countdown-widget="1" data-countdown-widget-id="' + ripleyTplEsc(w.id || '') + '" data-countdown-mode="' + ripleyTplEsc(p.countdown_mode || 'date') + '" data-countdown-target="' + ripleyTplEsc(p.targetDate || '') + '" data-countdown-duration-value="' + ripleyTplEsc(String(Math.max(1, Number(p.duration_value || 1)))) + '" data-countdown-duration-unit="' + ripleyTplEsc(p.duration_unit || 'minutes') + '" data-countdown-font-size="' + ripleyTplEsc(String(countdownFontSize)) + '" data-countdown-expiry-text="' + ripleyTplEsc(p.expiry_text || '') + '" data-countdown-action="' + ripleyTplEsc(p.expiry_action || 'none') + '" data-countdown-target-layout="' + ripleyTplEsc(p.expiry_target_layout || '') + '"' : '';
  var html = '<div' + wrapClass + wrapAttrs + runtimeAttrs + ' style="' + styleStr + '">';
  if (interactive) {
    if (w.widget_type === 'textbox') {
      html += '<label style="width:100%;display:grid;gap:.35rem;color:#fff;font-family:system-ui,sans-serif;font-size:' + ripleyResponsiveFontCss(16) + ';"><span>' + ripleyTplEsc(p.label || 'Text') + '</span>' + (p.multiline ? '<textarea data-interactive-value="value" placeholder="' + ripleyTplEsc(p.placeholder||'') + '" style="font:inherit;padding:.5em;border-radius:.5em;border:1px solid #64748b;">' + ripleyTplEsc(p.value||'') + '</textarea>' : '<input data-interactive-value="value" value="' + ripleyTplEsc(p.value||'') + '" placeholder="' + ripleyTplEsc(p.placeholder||'') + '" style="font:inherit;padding:.5em;border-radius:.5em;border:1px solid #64748b;">') + '</label>';
    } else if (w.widget_type === 'dropdown') {
      html += '<label style="width:100%;display:grid;gap:.35rem;color:#fff;font-family:system-ui,sans-serif;font-size:' + ripleyResponsiveFontCss(16) + ';"><span>' + ripleyTplEsc(p.label || 'Choose') + '</span><select data-interactive-value="selected" style="font:inherit;padding:.5em;border-radius:.5em;">' + [''].concat(p.options||[]).map(function(o){return '<option value="'+ripleyTplEsc(o)+'" '+(o===(p.selected||'')?'selected':'')+'>'+ripleyTplEsc(o||'— Select —')+'</option>';}).join('') + '</select></label>';
    } else if (w.widget_type === 'checkbox') {
      html += '<label style="color:#fff;font-family:system-ui,sans-serif;font-size:' + ripleyResponsiveFontCss(18) + ';"><input data-interactive-value="checked" type="checkbox" '+(p.checked?'checked':'')+'> ' + ripleyTplEsc(p.label || 'Checkbox') + '</label>';
    } else if (w.widget_type === 'checkbox-list') {
      html += '<fieldset style="width:100%;height:100%;color:#fff;font-family:system-ui,sans-serif;font-size:' + ripleyResponsiveFontCss(16) + ';border:0;"><legend>'+ripleyTplEsc(p.label||'Select')+'</legend>'+(p.options||[]).map(function(o){return '<label style="display:block"><input data-interactive-list="checkedValues" value="'+ripleyTplEsc(o)+'" type="checkbox" '+((p.checkedValues||[]).indexOf(o)>=0?'checked':'')+'> '+ripleyTplEsc(o)+'</label>';}).join('')+'</fieldset>';
    } else if (w.widget_type === 'radio') {
      html += '<fieldset style="width:100%;height:100%;color:#fff;font-family:system-ui,sans-serif;font-size:' + ripleyResponsiveFontCss(16) + ';border:0;"><legend>'+ripleyTplEsc(p.label||'Pick one')+'</legend>'+(p.options||[]).map(function(o){return '<label style="display:block"><input data-interactive-value="selected" name="'+ripleyTplEsc(w.id)+'" value="'+ripleyTplEsc(o)+'" type="radio" '+(o===(p.selected||'')?'checked':'')+'> '+ripleyTplEsc(o)+'</label>';}).join('')+'</fieldset>';
    } else if (w.widget_type === 'button') {
      html += '<button class="interactive-live-button" data-interactive-action="' + (p.action === 'submit_page' ? 'submit_page' : (p.action === 'play_sound' ? 'play_sound' : 'page_switch')) + '" data-interactive-target-page="' + ripleyTplEsc(p.targetPageId || p.targetLayoutId || '') + '" data-interactive-sound-id="' + ripleyTplEsc(p.targetSoundWidgetId || '') + '" style="font-size:' + ripleyResponsiveFontCss(Number(p.fontSize||28)) + ';padding:.7em 1.1em;border:0;border-radius:.7em;background:#2563eb;color:white;font-weight:800;">' + ripleyTplEsc((p.action === 'page_switch' && /^go to page$/i.test(String(p.label || ''))) ? 'Go to layout' : (p.label || 'Button')) + '</button>';
    } else if (w.widget_type === 'keyboard') {
      html += buildInteractiveKeyboardHTML(w);
    }
  } else if (Array.isArray(p.template_elements) && p.template_elements.length) {
    html += ripleyTemplateElementsHTML(Object.assign({ _widget_type: w.widget_type }, p));
  } else if (w.widget_type === 'video' && (Array.isArray(p.playlist) && p.playlist.length ? /(?:youtube\.com|youtu\.be)/i.test(String(p.playlist[0].url || '')) : /(?:youtube\.com|youtu\.be)/i.test(String(p.url || '')))) {
    // YouTube URLs must use the iframe player. A YouTube watch/short URL is
    // an HTML page, not a media file that the <video> element can decode.
    var youtubeUrl = Array.isArray(p.playlist) && p.playlist.length ? p.playlist[0].url : p.url;
    var youtubeFit = ['cover', 'contain', 'fill', 'none', 'scale-down'].indexOf(p.fit) >= 0 ? p.fit : 'cover';
    html += '<iframe src="/youtube.html?url=' + encodeURIComponent(youtubeUrl) + '&priority=' + ripleyNumber(p.priority, 50, 0, 100) + '&mute=' + ((p.muted || p.mute) ? '1' : '0') + '&fit=' + encodeURIComponent(youtubeFit) + '&v=' + Date.now() + '" allow="autoplay; encrypted-media; picture-in-picture" allowfullscreen style="width:100%;height:100%;border:none;background:#000;overflow:hidden;"></iframe>';
  } else if (w.widget_type === 'video' && Array.isArray(p.playlist) && p.playlist.length) {
    var videoFit = ['cover', 'contain', 'fill', 'none', 'scale-down'].indexOf(p.fit) >= 0 ? p.fit : 'cover';
    html += '<video data-video-widget-id="' + ripleyTplEsc(w.id) + '" src="' + ripleyTplEsc(ripleySafeUrl(p.playlist[0].url)) + '" autoplay playsinline preload="auto" ' + (p.loop !== false && p.playlist.length === 1 ? 'loop ' : '') + (p.muted || p.mute ? 'muted' : '') + ' style="width:100%;height:100%;object-fit:' + videoFit + ';background:#000;"></video>';
  } else if (w.widget_type === 'video' && p.url) {
    // Direct video file. The mute prop sets the <video> muted attribute, so a lower-priority
    // music widget can take over the audio output. Honor fit prop (cover/contain/stretch).
    var fit = ['cover', 'contain', 'fill', 'none', 'scale-down'].indexOf(p.fit) >= 0 ? p.fit : 'cover';
    html += '<video src="' + ripleyTplEsc(ripleySafeUrl(p.url)) + '" autoplay ' + (p.loop !== false ? 'loop ' : '') + (p.muted || p.mute ? 'muted' : '') + ' style="width:100%;height:100%;object-fit:' + fit + ';"></video>';
  } else if (w.widget_type === 'music' || w.widget_type === 'sound') {
    // Audio widgets are intentionally invisible on the layout.
    // The audio element is created in the script block below and registered with
    // SoundManager. We render a zero-height marker so the layout doesn't break.
    html += '<div data-music-widget-id="' + ripleyTplEsc(w.id) + '" style="position:absolute;width:0;height:0;overflow:hidden;pointer-events:none;" aria-hidden="true"></div>';
  } else if (w.widget_type === 'news') {
    // News Ticker. Two render paths:
    //
    //   1. If the widget has a `preset` (UUID of a widget-template
    //      with widget_type=news), render via the /news-template.html
    //      iframe. That page reads the template's template_elements,
    //      fetches the cached headlines, and composes a fully-custom
    //      layout the user built in the Widget Templates editor.
    //      This is the "Template-driven News layout" feature.
    //
    //   2. Otherwise fall back to the basic horizontal scrolling
    //      marquee, reading cached headlines from the global
    //      __ripleyNewsHeadlines map (populated by the player's
    //      layout-load pre-fetch).
    var np = w.props || {};
    if (np.preset) {
      var cache = Number(np.cache_seconds || 60);
      if (cache < 10) cache = 10;
      if (cache > 600) cache = 600;
      // Pass the feedUrl as a query param so /news-template.html can
      // resolve it even if the preset hasn't been fetched yet.
      var presetSrc = '/news-template.html?preset=' + encodeURIComponent(np.preset)
        + '&url=' + encodeURIComponent(np.feedUrl || '')
        + '&cache=' + cache
        + (np.font_family || np.fontFamily ? '&font_family=' + encodeURIComponent(np.font_family || np.fontFamily) : '');
      html += '<iframe src="' + presetSrc + '" style="width:100%;height:100%;border:none;background:transparent;" allowTransparency="true"></iframe>';
    } else {
      var dur = ({slow: '60s', normal: '30s', fast: '15s'})[np.speed] || '30s';
      var headlines = (window.__ripleyNewsHeadlines && window.__ripleyNewsHeadlines[w.id]) || [];
      var items;
      if (headlines.length) {
        items = headlines.map(function (h) {
          // Headlines are objects: {title, link, pub_date, source}.
          // Show the title only.
          var title = (h && h.title) ? h.title : String(h);
          return '<span style="padding:0 2rem;">' + (typeof ripleyTplEsc === 'function' ? ripleyTplEsc(title) : String(title).replace(/[<>&"]/g, function (c) { return ({'<':'&lt;','>':'&gt;','&':'&amp;','"':'&quot;'})[c]; })) + '</span>';
        }).join('');
      } else {
        items = '<span style="padding:0 2rem;color:#888;">📰 ' + (np.feedUrl ? 'Loading news…' : 'No feed URL set') + '</span>';
      }
      var newsFontFamily = ripleyCssFontFamily(np.font_family || np.fontFamily);
      html += '<div class="ripley-news-ticker" style="background:var(--bg-elev,#161b22);color:var(--text,#e6edf3);font-size:1.1em;' + (newsFontFamily ? 'font-family:' + ripleyTplEsc(newsFontFamily) + ';' : '') + '"><div style="display:inline-block;padding-left:100%;animation:ripleyNewsMarquee ' + dur + ' linear infinite;">' + items + '</div></div>';
    }
  } else if (w.widget_type === 'clock') {
    var clkFF = w.props && (w.props.font_family || w.props.fontFamily) ? '&font_family=' + encodeURIComponent(w.props.font_family || w.props.fontFamily) : '';
    var clkColor = w.props && /^#[0-9a-fA-F]{3,8}$/.test(String(w.props.color || '')) ? String(w.props.color) : '#fff';
    var clkColorParam = '&color=' + encodeURIComponent(clkColor);
    var clkBaseSize = Math.max(6, Math.min(400, Number(w.props && w.props.font_size) || 36));
    var clkFS = '&font_size=' + encodeURIComponent(ripleyScaledPixels(clkBaseSize));
    var clkVersion = '&v=20261009101500-clock-color-preview';
    html += '<iframe src="/clock.html?format=' + encodeURIComponent((w.props && w.props.format) || 'HH:mm') + clkFS + clkFF + clkColorParam + clkVersion + '" style="width:100%;height:100%;border:none;background:transparent;" allowTransparency="true"></iframe>';
  } else if (w.widget_type === 'text') {
    var ffEsc = ripleyTplEsc(ripleyCssFontFamily(p.font_family || p.fontFamily || ''));
    var ffStyle = ffEsc ? 'font-family:' + ffEsc + ';' : '';
    var textContent = p.content != null ? p.content : (p.text != null ? p.text : '');
    html += '<div style="width:100%;height:100%;display:flex;align-items:center;justify-content:center;color:' + ripleyCssColor(p.color, '#fff') + ';font-size:' + ripleyResponsiveFontCss(Number(p.font_size || 24)) + ';' + ffStyle + 'text-align:' + ripleyCssAlign(p.align, 'center') + ';' + ripleyTextShadowStyle(p) + ripleyTextOutlineStyle(p) + '">' + ripleyTplEsc(textContent) + '</div>';
  } else if (w.widget_type === 'datetime') {
    var dtFFEsc = ripleyTplEsc(ripleyCssFontFamily(p.font_family || p.fontFamily || ''));
    var dtFFStyle = dtFFEsc ? 'font-family:' + dtFFEsc + ';' : '';
    var dtFmt = p.format || 'dddd, MMMM D';
    var dtText = formatDateLikeMoment(new Date(), dtFmt);
    html += '<div data-ripley-datetime="1" style="width:100%;height:100%;display:flex;align-items:center;justify-content:center;color:' + ripleyCssColor(p.color, '#fff') + ';font-size:' + ripleyResponsiveFontCss(Number(p.font_size || 36)) + ';' + dtFFStyle + 'text-align:center;">' + ripleyTplEsc(dtText) + '</div>';
  } else if (w.widget_type === 'countdown') {
    var cdFFEsc = ripleyTplEsc(ripleyCssFontFamily(p.font_family || p.fontFamily || ''));
    var cdFFStyle = cdFFEsc ? 'font-family:' + cdFFEsc + ';' : '';
    var cdColor = ripleyCssColor(p.color, '#fff');
    var cdMode = p.countdown_mode || 'date';
    var cdDurationUnit = p.duration_unit || 'minutes';
    var cdDurationMs = Math.max(1, Number(p.duration_value || 1)) * ({seconds:1000, minutes:60000, hours:3600000, days:86400000}[cdDurationUnit] || 60000);
    var cdTargetStr = cdMode === 'duration' ? new Date(Date.now() + cdDurationMs).toISOString() : (p.targetDate || '');
    // Compute the target as a Date; bail to a placeholder if invalid.
    var cdTarget = cdTargetStr ? new Date(cdTargetStr) : null;
    var cdValid = cdTarget && !isNaN(cdTarget.getTime());
    var cdNow = new Date();
    var cdDiff = cdValid ? Math.max(0, cdTarget.getTime() - cdNow.getTime()) : 0;
    var cdText;
    if (!cdValid) {
      cdText = 'Choose a date and time';
    } else if (cdDiff === 0) {
      cdText = 'Time\u2019s up!';
    } else {
      var cdDays = Math.floor(cdDiff / 86400000);
      var cdH = Math.floor((cdDiff % 86400000) / 3600000);
      var cdM = Math.floor((cdDiff % 3600000) / 60000);
      var cdS = Math.floor((cdDiff % 60000) / 1000);
      if (cdDays > 0) cdText = cdDays + 'd ' + cdH + 'h';
      else if (cdH > 0) cdText = cdH + 'h ' + cdM + 'm';
      else if (cdM > 0) cdText = cdM + 'm ' + cdS + 's';
      else cdText = cdS + 's';
    }
    html += '<div style="width:100%;height:100%;display:flex;align-items:center;justify-content:center;color:' + cdColor + ';' + cdFFStyle + 'text-align:center;overflow:hidden;"><div class="countdown-value" style="font-size:' + ripleyResponsiveFontCss(ripleyCountdownFontSize(p.font_size)) + ';font-weight:800;line-height:1;white-space:nowrap;">' + ripleyTplEsc(cdText) + '</div></div>';
  } else if (w.widget_type === 'rectangle') {
    var opVal = Math.max(0, Math.min(100, Number((p && p.opacity == null ? 100 : p.opacity)))) / 100;
    var shapeMode = p.shape_mode || 'rectangle';
    var baseR = Number(p.radius == null ? 0 : p.radius) || 0;
    var rtl = Number(p.radius_tl == null ? baseR : p.radius_tl) || 0;
    var rtr = Number(p.radius_tr == null ? baseR : p.radius_tr) || 0;
    var rbr = Number(p.radius_br == null ? baseR : p.radius_br) || 0;
    var rbl = Number(p.radius_bl == null ? baseR : p.radius_bl) || 0;
    var radiusCss = shapeMode === 'circle' ? '50%' : (shapeMode === 'pill' ? '999px' : (rtl + 'px ' + rtr + 'px ' + rbr + 'px ' + rbl + 'px'));
    var clip = ripleyShapeClipPath(shapeMode);
    var lineStyle = shapeMode === 'line' ? 'height:' + (Number(p.line_thickness == null ? 6 : p.line_thickness) || 6) + 'px;' : '';
    var clipStyle = clip ? 'clip-path:' + clip + ';-webkit-clip-path:' + clip + ';' : '';
    html += '<div style="width:100%;height:100%;background:' + ripleyCssColor(p.color, '#e94560') + ';border-radius:' + radiusCss + ';opacity:' + opVal + ';' + lineStyle + clipStyle + '"></div>';
  } else if (w.widget_type === 'image') {
    var imageFit = ['cover', 'contain', 'fill', 'none', 'scale-down'].indexOf(p.fit) >= 0 ? p.fit : 'cover';
    var imagePlaylist = Array.isArray(p.playlist) && p.playlist.length ? p.playlist : (p.url ? [{url:p.url}] : []);
    var imageSrc = imagePlaylist.length ? imagePlaylist[0].url : '';
    var imageRadius = ripleyCornerRadiusCss(p);
    var imageClip = ripleyCornerRadiusClipCss(p);
    if (imageSrc) {
      html += '<img data-image-widget-id="' + ripleyTplEsc(w.id) + '" src="' + ripleyTplEsc(ripleySafeUrl(imageSrc)) + '" alt="' + ripleyTplEsc(p.alt || '') + '" style="width:100%;height:100%;object-fit:' + imageFit + ';border-radius:' + imageRadius + ';clip-path:' + imageClip + ';-webkit-clip-path:' + imageClip + ';">';
    } else {
      html += '<div data-image-widget-id="' + ripleyTplEsc(w.id) + '" aria-hidden="true" style="width:100%;height:100%;"></div>';
    }
  } else if (w.widget_type === 'camera') {
    var cameraCaptureEnabled = p.capture_enabled === true;
    var cameraFilter = ripleyCameraFilterName(p.filter);
    var cameraFilterCss = ripleyCameraFilterCss(cameraFilter);
    var cameraFilterOverlay = ripleyCameraFilterOverlay(cameraFilter, w.id);
    if (cameraCaptureEnabled) {
      html += '<div style="width:100%;height:100%;position:relative;background:#111827;overflow:hidden;display:flex;align-items:center;justify-content:center;"><video data-camera-widget-id="' + ripleyTplEsc(w.id) + '" data-camera-filter="' + cameraFilter + '" autoplay playsinline muted style="width:100%;height:100%;object-fit:cover;background:#111827;filter:' + cameraFilterCss + ';"></video>' + cameraFilterOverlay + '<span data-camera-status="' + ripleyTplEsc(w.id) + '" style="position:absolute;left:.5rem;bottom:.5rem;padding:.25rem .45rem;border-radius:.35rem;background:rgba(15,23,42,.8);color:#cbd5e1;font:600 12px system-ui,sans-serif;">Starting local camera…</span></div>';
    } else {
      html += '<div data-camera-widget-id="' + ripleyTplEsc(w.id) + '" data-camera-placeholder="' + ripleyTplEsc(w.id) + '" style="width:100%;height:100%;box-sizing:border-box;display:flex;flex-direction:column;align-items:center;justify-content:center;gap:.4rem;background:#111827;border:1px dashed #64748b;color:#cbd5e1;font:600 clamp(12px,2cqw,22px) system-ui,sans-serif;text-align:center;"><div>📷 Camera preview</div><small style="color:#94a3b8;font-size:.7em;">capture disabled</small></div>';
    }
  } else if (w.widget_type === 'traffic') {
    var trafficAllEvents = Array.isArray(p._traffic_data && p._traffic_data.events) ? p._traffic_data.events : [];
    var trafficDefaultKey = p.traffic_variable || 'traffic_summary';
    var trafficEvents = trafficDefaultKey === 'traffic_crashes_closures'
      ? trafficAllEvents.filter(function(event){ return event.category === 'crash' || event.category === 'closure'; })
      : trafficAllEvents;
    var trafficLimit = Math.max(1, Math.min(10, Number(p.max_events) || 3));
    var trafficDefaultValue = ripleyTrafficVariableText(p, trafficDefaultKey);
    var trafficDefaultHtml = trafficEvents.length || p.hide_if_empty === false
      ? '<div class="traffic-default-variable" style="font-size:1.15em;font-weight:700;margin-top:.35rem;">' + ripleyTplEsc(trafficDefaultValue) + '</div>'
      : '';
    var trafficRows = trafficEvents.slice(0, trafficLimit).map(function(event) {
      return '<div style="padding:.35rem 0;border-bottom:1px solid rgba(148,163,184,.25);"><b>' + ripleyTplEsc(event.category || 'Event') + '</b> · ' + ripleyTplEsc(event.road || 'Road') + ' ' + ripleyTplEsc(event.direction || '') + '<br><small>' + ripleyTplEsc(event.description || '') + '</small></div>';
    }).join('');
    html += '<div style="width:100%;height:100%;box-sizing:border-box;overflow:hidden;padding:.6rem;color:#fff;background:rgba(30,41,59,.55);font-family:system-ui,sans-serif;"><div style="font-weight:700;">🚧 ' + ripleyTplEsc(p.area_label || 'Traffic') + '</div>' + trafficDefaultHtml + '<div style="font-size:.85em;margin-top:.4rem;">' + (trafficRows || (p.hide_if_empty !== false ? '' : '<div>No active traffic events</div>')) + '</div></div>';
  } else if (w.widget_type === 'calendar') {
    var calSrc = '/calendar.html?ics=' + encodeURIComponent(p.ics_url || p.ical_url || p.ical || p.calendar_url || '') + '&template=' + encodeURIComponent(p.template || 'list') + '&color=' + encodeURIComponent(p.color || '#ffffff') + '&accent=' + encodeURIComponent(p.accent_color || '#e94560') + '&maxevents=' + ripleyNumber(p.max_events, 5, 1, 50) + '&time_format=' + encodeURIComponent(p.time_format === '24' ? '24' : '12') + '&incweather=' + (p.include_weather === 'true' ? 'true' : 'false') + '&lat=' + ripleyNumber(p.lat, 0, -90, 90) + '&lon=' + ripleyNumber(p.lon, 0, -180, 180) + (p.font_family || p.fontFamily ? '&font_family=' + encodeURIComponent(p.font_family || p.fontFamily) : '');
    html += '<iframe src="' + calSrc + '" style="width:100%;height:100%;border:none;background:transparent;" allowTransparency="true"></iframe>';
  } else if (w.widget_type === 'weather') {
    var weatherLocation = p.weather_location || p.location || '';
    var wSrc = '/weather.html?lat=' + ripleyNumber(p.lat, 0, -90, 90) + '&lon=' + ripleyNumber(p.lon, 0, -180, 180) + '&template=' + encodeURIComponent(p.template || '3day') + '&color=' + encodeURIComponent(p.color || '#ffffff') + '&accent=' + encodeURIComponent(p.accent || p.accent_color || '#4a90d9') + '&location=' + encodeURIComponent(weatherLocation) + '&unit=' + encodeURIComponent(p.unit || 'F') + '&morning_split=' + (p.weather_morning_icon === 'split' ? 'true' : 'false') + (p.font_family || p.fontFamily ? '&font_family=' + encodeURIComponent(p.font_family || p.fontFamily) : '');
    if (p.bg_color) wSrc += '&bg_color=' + encodeURIComponent(p.bg_color);
    if (p.bg_opacity !== undefined) wSrc += '&bg_opacity=' + encodeURIComponent(p.bg_opacity);
    if (p.text_outline_color) wSrc += '&text_outline_color=' + encodeURIComponent(p.text_outline_color);
    if (p.text_outline_width !== undefined) wSrc += '&text_outline_width=' + encodeURIComponent(p.text_outline_width);
    if (p.custom_icons) wSrc += '&icons=' + encodeURIComponent(JSON.stringify(p.custom_icons));
    html += '<iframe src="' + wSrc + '" style="width:100%;height:100%;border:none;background:transparent;" allowTransparency="true"></iframe>';
  } else if (w.widget_type === 'webpage') {
    if (p.web_type === 'google_slides' && p.url) {
      var embedUrl = p.url;
      var match = embedUrl.match(/(https:\/\/docs\.google\.com\/presentation\/d\/[a-zA-Z0-9_-]+)/);
      if (match) {
        var delayMs = (p.slide_duration || 5) * 1000;
        embedUrl = match[1] + '/embed?rm=minimal&loop=true&start=true&delayms=' + delayMs;
      } else {
        embedUrl = '';
      }
      html += '<iframe src="' + ripleyTplEsc(ripleySafeUrl(embedUrl)) + '" style="width:100%;height:100%;border:none;" allow="fullscreen"></iframe>';
    } else {
      html += '<iframe src="' + ripleyTplEsc(ripleySafeUrl(p.url)) + '" style="width:100%;height:100%;border:none;"></iframe>';
    }
  }
  html += '</div>';
  return html;
}


function interactiveWidgetValue(w, node) {
  var type = w.widget_type, p = w.props || {};
  if (type === 'textbox') return (node.querySelector('[data-interactive-value="value"]')||{}).value || '';
  if (type === 'dropdown') return (node.querySelector('[data-interactive-value="selected"]')||{}).value || '';
  if (type === 'radio') return (node.querySelector('[data-interactive-value="selected"]:checked')||{}).value || '';
  if (type === 'checkbox') return !!((node.querySelector('[data-interactive-value="checked"]')||{}).checked);
  if (type === 'checkbox-list') return Array.prototype.slice.call(node.querySelectorAll('[data-interactive-list="checkedValues"]:checked')).map(function(x){return x.value;});
  return p.value || '';
}

function interactiveKeyboardRows(mode, p) {
  if (mode === 'number') {
    var rows = [['1','2','3'],['4','5','6'],['7','8','9']];
    var last = [];
    if (p.allowNegative) last.push('-');
    last.push('0');
    if (p.allowDecimal !== false) last.push('.');
    rows.push(last);
    rows.push(['backspace','clear','enter']);
    return rows;
  }
  if (mode === 'full_keyboard') {
    return [['`','1','2','3','4','5','6','7','8','9','0','-','=','backspace'],['tab','q','w','e','r','t','y','u','i','o','p','[',']','\\'],['shift','a','s','d','f','g','h','j','k','l',';','\'','enter'],['symbols','z','x','c','v','b','n','m',',','.','/','arrowleft','arrowright'],['clear','space','enter']];
  }
  return [['Q','W','E','R','T','Y','U','I','O','P'],['A','S','D','F','G','H','J','K','L'],['Z','X','C','V','B','N','M','backspace'],['space','clear','enter']];
}

function buildInteractiveKeyboardHTML(w) {
  var p = w.props || {};
  var mode = p.mode === 'number' ? 'number' : (p.mode === 'full_keyboard' ? 'full_keyboard' : 'full');
  var hidden = (p.visibility || 'on_focus') === 'on_focus';
  var rows = interactiveKeyboardRows(mode, p);
  function labelFor(k){ return ({backspace:'⌫', space:'Space', clear:'Clear', enter:'Done', shift:'Shift', symbols:'?123', tab:'Tab', arrowleft:'←', arrowright:'→'}[k] || k); }
  function flexFor(k){ return k === 'space' ? 5 : (k === 'backspace' || k === 'enter' || k === 'shift' || k === 'symbols' || k === 'clear' ? 1.7 : 1); }
  var title = p.label || (mode === 'number' ? 'Number Pad' : (mode === 'full_keyboard' ? 'Full Keyboard' : 'Keyboard'));
  return '<div class="interactive-live-keyboard ' + (hidden ? 'keyboard-hidden-until-focus' : '') + '" data-interactive-keyboard="1" data-keyboard-mode="' + ripleyTplEsc(mode) + '" data-keyboard-visibility="' + ripleyTplEsc(p.visibility || 'on_focus') + '" data-keyboard-shift="0" data-keyboard-symbols="0" style="width:100%;height:100%;display:flex;flex-direction:column;gap:.35rem;padding:.45rem;box-sizing:border-box;background:rgba(15,23,42,.94);border:1px solid rgba(148,163,184,.45);border-radius:14px;box-shadow:0 12px 28px rgba(0,0,0,.32);">' +
    '<div style="color:#e2e8f0;font:700 clamp(12px,1.5vw,18px) system-ui,sans-serif;display:flex;justify-content:space-between;align-items:center;"><span>' + ripleyTplEsc(title) + '</span><small style="color:#94a3b8;font-weight:600;">' + (hidden ? 'tap input' : 'always on') + '</small></div>' +
    rows.map(function(row){ return '<div class="interactive-key-row" style="display:flex;gap:.35rem;flex:1;">' + row.map(function(k){ return '<button type="button" data-interactive-key="' + ripleyTplEsc(k) + '" style="flex:' + flexFor(k) + ';min-width:0;border:0;border-radius:10px;background:' + ((k === 'shift' || k === 'symbols') ? '#475569' : '#334155') + ';color:#fff;font:800 clamp(12px,1.65vw,24px) system-ui,sans-serif;touch-action:manipulation;">' + ripleyTplEsc(labelFor(k)) + '</button>'; }).join('') + '</div>'; }).join('') +
    '</div>';
}

function bindInteractiveKeyboardRuntime(root) {
  var active = root.querySelector('[data-interactive-value="value"]');
  function keyboards(){ return Array.prototype.slice.call(root.querySelectorAll('[data-interactive-keyboard][data-keyboard-visibility="on_focus"]')); }
  function show(){ keyboards().forEach(function(k){ k.style.visibility='visible'; k.style.pointerEvents='auto'; k.style.opacity='1'; }); }
  function hide(){ keyboards().forEach(function(k){ k.style.visibility='hidden'; k.style.pointerEvents='none'; k.style.opacity='0'; }); }
  root.querySelectorAll('[data-interactive-value="value"]').forEach(function(input){
    input.setAttribute('autocomplete','off');
    input.addEventListener('focus', function(){ active = input; show(); });
    input.addEventListener('pointerdown', function(){ active = input; show(); });
  });
  root.querySelectorAll('[data-interactive-keyboard]').forEach(function(kbd){
    if (kbd.dataset.keyboardVisibility === 'on_focus') { kbd.style.visibility='hidden'; kbd.style.pointerEvents='none'; kbd.style.opacity='0'; kbd.style.transition='opacity .12s ease'; }
    kbd.querySelectorAll('[data-interactive-key]').forEach(function(btn){
      btn.addEventListener('pointerdown', function(e){ e.preventDefault(); });
      btn.addEventListener('click', function(e){
        e.preventDefault();
        if (!active) active = root.querySelector('[data-interactive-value="value"]');
        if (!active) return;
        var key = btn.dataset.interactiveKey;
        var start = active.selectionStart == null ? active.value.length : active.selectionStart;
        var end = active.selectionEnd == null ? active.value.length : active.selectionEnd;
        var before = active.value.slice(0,start), after = active.value.slice(end);
        var next = active.value, pos = start;
        function keyToChar(kbd, key) {
          var mode = kbd.dataset.keyboardMode || 'full';
          var shifted = kbd.dataset.keyboardShift === '1';
          var symbols = kbd.dataset.keyboardSymbols === '1';
          if (key === 'space') return ' ';
          if (key === 'tab') return '\t';
          if (mode !== 'full_keyboard') return key;
          var shiftMap = {'`':'~','1':'!','2':'@','3':'#','4':'$','5':'%','6':'^','7':'&','8':'*','9':'(','0':')','-':'_','=':'+','[':'{',']':'}','\\':'|',';':':','\'':'"',',':'<','.':'>','/':'?'};
          var symbolMap = {q:'@',w:'#',e:'$',r:'%',t:'&',y:'*',u:'(',i:')',o:'_',p:'+',a:'-',s:'=',d:'/',f:'\\',g:':',h:';',j:'"',k:'\'',l:'|',z:'<',x:'>',c:'[',v:']',b:'{',n:'}',m:'^',',':',','.':'.','/':'?', '`':'~'};
          if (symbols && symbolMap[key] != null) return symbolMap[key];
          if (shifted && shiftMap[key] != null) return shiftMap[key];
          if (/^[a-z]$/.test(key)) return shifted ? key.toUpperCase() : key;
          return key;
        }
        function relabelKeys(kbd) {
          var shifted = kbd.dataset.keyboardShift === '1';
          var symbols = kbd.dataset.keyboardSymbols === '1';
          kbd.querySelectorAll('[data-interactive-key]').forEach(function(b){
            var k = b.dataset.interactiveKey;
            var staticLabels = {backspace:'⌫', space:'Space', clear:'Clear', enter:'Done', tab:'Tab', arrowleft:'←', arrowright:'→'};
            if (k === 'shift') { b.textContent = shifted ? 'Shift ✓' : 'Shift'; b.style.background = shifted ? '#2563eb' : '#475569'; return; }
            if (k === 'symbols') { b.textContent = symbols ? 'ABC' : '?123'; b.style.background = symbols ? '#2563eb' : '#475569'; return; }
            b.textContent = staticLabels[k] || keyToChar(kbd, k);
          });
        }
        if (key === 'shift') { kbd.dataset.keyboardShift = kbd.dataset.keyboardShift === '1' ? '0' : '1'; relabelKeys(kbd); return; }
        if (key === 'symbols') { kbd.dataset.keyboardSymbols = kbd.dataset.keyboardSymbols === '1' ? '0' : '1'; relabelKeys(kbd); return; }
        if (key === 'backspace') { next = (start === end && start > 0) ? active.value.slice(0,start-1) + after : before + after; pos = (start === end) ? Math.max(0,start-1) : start; }
        else if (key === 'clear') { next = ''; pos = 0; }
        else if (key === 'enter') { active.blur(); hide(); return; }
        else if (key === 'arrowleft') { pos = Math.max(0, start-1); try { active.setSelectionRange(pos,pos); } catch(_e) {} return; }
        else if (key === 'arrowright') { pos = Math.min(active.value.length, end+1); try { active.setSelectionRange(pos,pos); } catch(_e) {} return; }
        else { var ch = keyToChar(kbd, key); next = before + ch + after; pos = start + ch.length; }
        active.value = next;
        active.dispatchEvent(new Event('input', {bubbles:true}));
        active.focus();
        try { active.setSelectionRange(pos,pos); } catch(_e) {}
      });
    });
  });
}

async function postInteractivePageSubmissions(clientId, layout, widgets, root) {
  // Kept name for backward compat; this is now "submit the values of the
  // interactive widgets on a layout". The backend still stores them in the
  // interactive_submissions table.
  var interactive = {textbox:1, dropdown:1, radio:1, checkbox:1, 'checkbox-list':1};
  var baseAt = new Date().toISOString();
  // Send one submission per widget. Widgets that share a `link_id`
  // (a non-empty Data Handling "Linking Number") are grouped server-side
  // via the `interactive_submissions.link_id` column — the schema has a NOT-NULL
  // `widget_id` and `widget_type`, so client-side aggregation would 422.
  for (var i=0;i<widgets.length;i++) {
    var w = widgets[i];
    if (!interactive[w.widget_type]) continue;
    var node = root.querySelector('[data-interactive-widget-id="' + CSS.escape(w.id) + '"]');
    var props = w.props || {};
    var payload = {
      client_id: clientId || undefined,
      // New: send layout_id + layout_name. Kept: interactive_layout_id + interactive_layout_name
      
      layout_id: layout.id,
      layout_name: layout.name,
      interactive_layout_id: layout.id,
      interactive_layout_name: layout.name,
      widget_id: w.id,
      widget_type: w.widget_type,
      widget_label: props.label || w.widget_type,
      field: props.dataField || props.label || w.id,
      link_id: props.dataLinking || undefined,
      payload: interactiveWidgetValue(w, node),
      collected_at: baseAt,
    };
    var res = await playerFetch('/api/interactive/submit', {method:'POST', headers:{'Content-Type':'application/json'}, body:JSON.stringify(payload)});
    if (!res.ok) throw new Error(await readPlayerTextBounded(res));
  }
}


async function startCountdownRuntime(root, currentLayout, clientId, layoutCache, fetchAndCacheLayout, cacheLayout, wireInteractiveRuntime) {
  if (!root) return;
  if (root.__countdownTimer) clearInterval(root.__countdownTimer);
  var nodes = Array.prototype.slice.call(root.querySelectorAll('[data-countdown-widget]'));
  if (!nodes.length) return;
  function fitCountdownNode(node) {
    var value = node.querySelector('.countdown-value');
    if (!value || !node.clientWidth || !node.clientHeight) return;
    var requested = ripleyCountdownFontSize(node.dataset.countdownFontSize) * ripleyRenderScale();
    // Text Size is an explicit user setting. Do not auto-fit it back to a
    // shared width/height cap: that made different configured values render
    // at the same size. The widget remains overflow-hidden, so the user can
    // enlarge the text deliberately when the countdown string is long.
    value.style.fontSize = Math.floor(requested) + 'px';
    var label = value.previousElementSibling;
    if (label) label.style.fontSize = Math.max(8, Math.min(24, Math.floor(Math.min(node.clientWidth / Math.max(8, label.textContent.length * 0.55), node.clientHeight * 0.16)))) + 'px';
  }
  function formatRemaining(ms, expiryText) {
    if (ms <= 0) return expiryText || '';
    var days = Math.floor(ms / 86400000);
    var hours = Math.floor((ms % 86400000) / 3600000);
    var minutes = Math.floor((ms % 3600000) / 60000);
    var seconds = Math.floor((ms % 60000) / 1000);
    return days > 0 ? days + 'd ' + hours + 'h' : hours > 0 ? hours + 'h ' + minutes + 'm' : minutes > 0 ? minutes + 'm ' + seconds + 's' : seconds + 's';
  }
  async function expire(node) {
    if (node.dataset.countdownExpired === '1') return;
    node.dataset.countdownExpired = '1';
    var action = node.dataset.countdownAction || 'none';
    if (action === 'submit_page') {
      try { await postInteractivePageSubmissions(clientId, currentLayout, currentLayout.widgets || [], root); }
      catch (e) { lg('Countdown submit failed: ' + (e && e.message ? e.message : e)); }
      return;
    }
    if (action !== 'page_switch') return;
    var targetId = node.dataset.countdownTargetLayout;
    if (!targetId) return;
    // Revalidate the target before switching so an edited countdown destination
    // cannot display an older cached snapshot. Keep the cache as an offline
    // fallback if the fresh request fails.
    var target = await fetchAndCacheLayout(targetId, true) || layoutCache[targetId];
    if (!target) { lg('Countdown target layout missing: ' + targetId); return; }
    root.innerHTML = (target.widgets || []).slice().sort(function(a,b){ return a.z - b.z; }).map(buildWidgetHTML).join('');
    ripleyFitTrafficTemplateText(root);
    injectInstalledFontsIntoIframes(root);
    initVideoWidgets(target.widgets || [], root);
    initCameraWidgets(target.widgets || [], root);
    initImageWidgets(target.widgets || [], root);
    cacheLayout(target);
    await startCountdownRuntime(root, target, clientId, layoutCache, fetchAndCacheLayout, cacheLayout, wireInteractiveRuntime);
    await wireInteractiveRuntime(root, target, clientId);
    lg('Switched to layout ' + targetId + ' after countdown expired');
  }
  function tick() {
    var now = Date.now();
    nodes.forEach(function(node) {
      if (!document.documentElement.contains(node)) return;
      var target;
      if ((node.dataset.countdownMode || 'date') === 'duration') {
        var units = {seconds:1000, minutes:60000, hours:3600000, days:86400000};
        var duration = Math.max(1, Number(node.dataset.countdownDurationValue || 1)) * (units[node.dataset.countdownDurationUnit || 'minutes'] || 60000);
        if (!node.dataset.countdownStartedAt) node.dataset.countdownStartedAt = String(now);
        target = new Date(Number(node.dataset.countdownStartedAt) + duration);
      } else {
        target = new Date(node.dataset.countdownTarget || '');
      }
      if (isNaN(target.getTime())) return;
      var diff = target.getTime() - now;
      var value = node.querySelector('.countdown-value');
      if (value) value.textContent = formatRemaining(diff, node.dataset.countdownExpiryText || '');
      fitCountdownNode(node);
      if (diff <= 0) expire(node);
    });
  }
  tick();
  root.__countdownTimer = setInterval(tick, 1000);
}


function initImageWidgets(widgets, root) {
  root = root || document;
  root.querySelectorAll('[data-image-widget-id]').forEach(function(node) {
    var w = (widgets || []).find(function(x){ return x.id === node.dataset.imageWidgetId; });
    if (!w) return;
    var p = w.props || {}, list = Array.isArray(p.playlist) && p.playlist.length ? p.playlist : (p.url ? [{url:p.url}] : []);
    if (list.length < 2) return;
    var ordered = p.shuffle ? shuffleImagePlaylist(list) : list.slice(), index = 0, timer = null, stopped = false;
    node.__imageStop && node.__imageStop();
    function show() { var item = ordered[index]; if (item && item.url) node.src = item.url; }
    function schedule() {
      if (stopped) return;
      var item = ordered[index], seconds = Math.max(1, Math.min(3600, Number(item && item.duration) || Number(p.slide_duration) || 10));
      timer = setTimeout(advance, seconds * 1000);
    }
    function advance() {
      index++;
      if (index >= ordered.length) {
        if (p.loop === false) { stopped = true; return; }
        ordered = p.shuffle ? shuffleImagePlaylist(list) : list.slice(); index = 0;
      }
      show(); schedule();
    }
    show(); schedule();
    node.__imageStop = function(){ stopped = true; if (timer) clearTimeout(timer); };
  });
}
function shuffleImagePlaylist(arr) { var a = arr.slice(); for (var i=a.length-1;i>0;i--) { var j=Math.floor(Math.random()*(i+1)), t=a[i]; a[i]=a[j]; a[j]=t; } return a; }

// Camera filters are a closed, renderer-owned vocabulary. The selected
// value never becomes CSS; it is mapped to one of these fixed effects.
function ripleyCameraFilterName(value) {
  return ['none', 'grayscale', 'sepia', 'halloween'].indexOf(value) >= 0 ? value : 'none';
}
function ripleyCameraFilterCss(value) {
  return ({
    none: 'none',
    grayscale: 'grayscale(1)',
    sepia: 'sepia(1)',
    halloween: 'saturate(1.35) contrast(1.15) hue-rotate(300deg)',
  })[ripleyCameraFilterName(value)];
}
function ripleyCameraFilterOverlay(value, id) {
  if (ripleyCameraFilterName(value) !== 'halloween') return '';
  return '<span data-camera-filter-overlay="halloween" data-camera-filter-overlay-id="' + ripleyTplEsc(id) + '" aria-hidden="true" style="position:absolute;inset:0;pointer-events:none;background:linear-gradient(135deg,rgba(249,115,22,.34),rgba(88,28,135,.42));mix-blend-mode:screen;opacity:.8;"></span>';
}

// Camera capture is deliberately browser-local and disabled unless the
// persisted widget explicitly opts in. No frame is serialized or uploaded.
function initCameraWidgets(widgets, root) {
  root = root || document;
  var nodes = root.querySelectorAll ? root.querySelectorAll('[data-camera-widget-id]') : [];
  (widgets || []).forEach(function(w) {
    if (w.widget_type !== 'camera' || (w.props || {}).capture_enabled !== true) return;
    var p = w.props || {}, node = null;
    for (var i = 0; i < nodes.length; i++) {
      if (nodes[i].getAttribute('data-camera-widget-id') === String(w.id)) { node = nodes[i]; break; }
    }
    if (!node && root.querySelector) node = root.querySelector('[data-camera-widget-id="' + ripleyTplEsc(w.id) + '"]');
    if (!node || !navigator.mediaDevices || typeof navigator.mediaDevices.getUserMedia !== 'function') return;
    var facing = ['user', 'environment', 'left', 'right'].indexOf(p.facing_mode) >= 0 ? p.facing_mode : 'environment';
    var constraints = {
      video: {
        facingMode: { ideal: facing },
        width: { ideal: 1280, max: 1280 },
        height: { ideal: 720, max: 720 },
        frameRate: { ideal: 30, max: 30 },
      },
      audio: false,
    };
    node.muted = true;
    node.autoplay = true;
    node.playsInline = true;
    navigator.mediaDevices.getUserMedia(constraints).then(function(stream) {
      node.srcObject = stream;
      var attempt = node.play();
      if (attempt && typeof attempt.catch === 'function') attempt.catch(function(){});
    }).catch(function() {
      var status = root.querySelector ? root.querySelector('[data-camera-status="' + ripleyTplEsc(w.id) + '"]') : null;
      if (status) status.textContent = 'Camera unavailable';
    });
  });
}

// Video playlist support is shared by the live player and standalone Layout Preview.
// Keep this initializer at top level so preview.js can invoke the canonical runtime.
var videoWidgets = {};
function shuffleVideoPlaylist(arr) { var a = arr.slice(); for (var i=a.length-1;i>0;i--) { var j=Math.floor(Math.random()*(i+1)), t=a[i]; a[i]=a[j]; a[j]=t; } return a; }
function initVideoWidgets(widgets, root) {
  (widgets || []).forEach(function(w) {
    if (w.widget_type !== 'video') return;
    var p = w.props || {}, playlist = Array.isArray(p.playlist) && p.playlist.length ? p.playlist : (p.url ? [{name:'',url:p.url}] : []);
    var node = (root || document).querySelector('[data-video-widget-id="' + w.id + '"]');
    if (!node || !playlist.length) return;
    var ordered = p.shuffle ? shuffleVideoPlaylist(playlist) : playlist.slice(), state = videoWidgets[w.id];
    var key = ordered.map(function(x){return x.url;}).join('|') + '|' + (p.loop ? '1' : '0') + '|' + (p.shuffle ? '1' : '0');
    if (state && state.node === node && state.key === key) return;
    if (state) { state.node.pause(); if (state.endedHandler) state.node.removeEventListener('ended', state.endedHandler); if (state.endCheckHandler) state.node.removeEventListener('timeupdate', state.endCheckHandler); state.node.onended = null; state.node.onerror = null; }
    state = {node:node, list:ordered, index:0, loop:p.loop !== false, shuffle:!!p.shuffle, failed:{}, key:key, advancing:false, finished:false}; videoWidgets[w.id]=state;
    if (typeof ripleyPreviewMode !== 'undefined' && ripleyPreviewMode) {
      // Preview playback must be muted before the first play() call. Setting
      // muted only after play() races the browser autoplay policy.
      node.muted = true;
      node.defaultMuted = true;
      node.autoplay = true;
      node.playsInline = true;
      node.preload = 'auto';
      node.setAttribute('muted', '');
    }
    function play() {
      var item=state.list[state.index]; if (!item || !item.url) return;
      if (typeof ripleyPreviewMode !== 'undefined' && ripleyPreviewMode) {
        state.node.muted = true;
        state.node.defaultMuted = true;
      }
      var playbackUrl=item.url;
      if (/^(https?:|\/)/.test(playbackUrl)) playbackUrl += (playbackUrl.indexOf('?') >= 0 ? '&' : '?') + 'ripley_video=' + encodeURIComponent(w.id) + '-' + Date.now() + '-' + state.index;
      state.node.src=playbackUrl;
      state.node.load();
      var attempt=state.node.play();
      if (attempt && typeof attempt.catch === 'function') attempt.catch(function(){});
    }
    function advanceAfterEnd() {
      if (state.advancing || state.finished) return;
      state.advancing=true;
      state.index++;
      if (state.index >= state.list.length) {
        if (!state.loop) { state.finished=true; state.advancing=false; return; }
        state.list = state.shuffle ? shuffleVideoPlaylist(playlist) : playlist.slice();
        state.index=0;
        state.failed={};
      }
      try { play(); } finally { state.advancing=false; }
    }
    function checkVideoEnd() {
      if (state.advancing || state.finished) return;
      var duration=Number(state.node.duration), current=Number(state.node.currentTime);
      if (state.node.ended || (Number.isFinite(duration) && duration > 0 && current >= duration - 0.25)) advanceAfterEnd();
    }
    function skipFailedVideo() {
      var item=state.list[state.index], failedUrl=item && item.url;
      if (failedUrl) state.failed[failedUrl]=true;
      lg('Video source failed: ' + (failedUrl || '(empty)') + ' — skipping');
      var next=state.index+1;
      while (next < state.list.length && state.failed[state.list[next].url]) next++;
      if (next >= state.list.length) {
        if (!state.loop) return;
        state.list = state.shuffle ? shuffleVideoPlaylist(playlist) : playlist.slice();
        next=0;
        while (next < state.list.length && state.failed[state.list[next].url]) next++;
        if (next >= state.list.length) { lg('All video sources failed'); return; }
        state.failed={};
      }
      state.index=next;
      play();
    }
    state.endedHandler = advanceAfterEnd;
    state.endCheckHandler = checkVideoEnd;
    node.loop = state.list.length === 1 && state.loop;
    node.addEventListener('ended', state.endedHandler);
    node.addEventListener('timeupdate', state.endCheckHandler);
    node.onerror = skipFailedVideo;
    if (typeof ripleyPreviewMode !== 'undefined' && ripleyPreviewMode) {
      node.addEventListener('canplay', function(){
        var attempt=node.play();
        if (attempt && typeof attempt.catch === 'function') attempt.catch(function(){});
      });
    }
    play();
  });
}

async function init() {
  var urlParams = new URLSearchParams(window.location.search);
  var targetDisplay = urlParams.get('display');
  var clientId = urlParams.get('client_id');
  if (clientId) document.documentElement.classList.add('virtual-player');
  lg('Player starting, target display: ' + targetDisplay + (clientId ? ' (client=' + clientId + ')' : ''));

  // Pre-load installed fonts so font_family CSS on any widget can
  // resolve. Failures here are non-fatal: a missing font just falls
  // back to the next family in the stack. Cached for the lifetime
  // of the page; admin installs won't take effect on a running
  // player until reload (acceptable — admin rarely installs fonts
  // on a live screen).
  try {
    var fontsResp = await playerFetch('/api/fonts/installed');
    if (fontsResp.ok) {
      var fontsList = await readPlayerJsonBounded(fontsResp);
      injectInstalledFonts(fontsList);
    }
  } catch (_fe) { /* ignore — fonts are optional */ }

  // Prefer fetching THIS client's displays (DP-2/DP-4 etc. on the player box
  // rather than the server's own outputs). Fall back to /api/displays when
  // we don't know the client_id, or when the client-scoped endpoint fails.
  var displays = null;
  if (clientId) {
    try {
      var r = await playerFetch('/api/clients/' + encodeURIComponent(clientId) + '/displays');
      if (r.ok) displays = await readPlayerJsonBounded(r);
    } catch (_e) { /* fall through */ }
  }
  if (!displays) {
    displays = await playerFetch('/api/displays').then(function(r){return readPlayerJsonBounded(r);});
  }
  var activeDisplays = displays.filter(function(d){
    var status = String(d.status || 'connected').toLowerCase();
    var statusOk = (status === 'connected' || status === 'online' || status === 'active');
    return statusOk && (d.enabled===undefined || d.enabled);
  });

  var myDisplay;
  if (targetDisplay) {
    myDisplay = activeDisplays.find(function(d){return d.connector === targetDisplay;});
  } else {
    myDisplay = activeDisplays[0];
  }

  if (!myDisplay) {
    lg('ERROR: No valid display found for target: ' + targetDisplay);
    document.getElementById('player').innerHTML = '<div style="color:white;padding:20px;font-size:32px;">No valid display found for: ' + (targetDisplay || 'none') + '</div>';
    return;
  }

  lg('Using display: ' + myDisplay.connector);
  var player = document.getElementById('player');
  player.innerHTML = '';

  var r = document.createElement('div');
  r.className = 'region';
  r.id = 'r-' + myDisplay.connector;
  r.style.cssText = 'position:absolute;top:0;left:0;width:100vw;height:100vh;container-type:inline-size;';
  player.appendChild(r);

  var myConnector = myDisplay.connector;
  var currentContentKey = null;

  async function fetchClientDisplayAssignment() {
    if (!clientId) return null;
    try {
      var dr = await playerFetch('/api/clients/' + encodeURIComponent(clientId) + '/displays', {cache:'no-store'});
      if (!dr.ok) return null;
      var ds = await readPlayerJsonBounded(dr);
      var selected = ds.find(function(d){ return d.connector === myConnector || Number(d.display_index) === Number(myDisplay.display_index); });
      if (!selected) return null;
      // The display assignment keeps the layout ID stable across publishes.
      // Read the published revision as well so a new immutable revision causes
      // the already-running player page to rebuild its layer instead of
      // treating the display as unchanged forever.
      var index = Number(selected.display_index);
      if (Number.isFinite(index)) {
        try {
          var cr = await playerFetch('/api/clients/' + encodeURIComponent(clientId) + '/displays/' + index + '/config', {cache:'no-store'});
          if (cr.ok) {
            var config = await readPlayerJsonBounded(cr);
            selected = Object.assign({}, selected, {active_layout_revision: config.active_layout_revision});
          }
        } catch (_configError) { /* keep the assignment usable during a transient config failure */ }
      }
      return selected;
    } catch (_e) { return null; }
  }

  // ── Layout cache for offline button navigation ─────────────────────────
  // Holds the most-recently-rendered layouts keyed by id. Button widgets in
  // an interactive layout can switch to any of these without a server
  // round-trip. Updated by sxLoadLayout and prefetched for slideshow sets.
  var layoutCache = {};
  function cacheLayout(ly) {
    if (ly && ly.id) layoutCache[ly.id] = ly;
  }
  async function fetchAndCacheLayout(layoutId, forceFetch) {
    if (!forceFetch && layoutCache[layoutId]) return layoutCache[layoutId];
    try {
      var ly = await playerFetch('/api/layouts/' + encodeURIComponent(layoutId), {cache:'no-store'}).then(function(r){
        if (!r.ok) throw new Error('layout HTTP ' + r.status);
        return readPlayerJsonBounded(r);
      });
      cacheLayout(ly);
      return ly;
    } catch (e) {
      lg('fetchAndCacheLayout failed for ' + layoutId + ': ' + e.message);
      return null;
    }
  }
  async function prefetchLayouts(ids) {
    var unique = Array.from(new Set((ids || []).filter(Boolean)));
    await Promise.all(unique.map(function(id){ return fetchAndCacheLayout(id); }));
  }

  // ── Interactive runtime wiring ─────────────────────────────────────────
  // Called after every layout render. If the layout is interactive, bind
  // button click handlers (using the cached layouts) and form submission
  // for the interactive_submissions endpoint.
  async function wireInteractiveRuntime(root, currentLayout, clientId) {
    if (!currentLayout || !currentLayout.interactive) return;
    var widgets = currentLayout.widgets || [];
    bindInteractiveKeyboardRuntime(root);

    // Buttons: switch to a target layout, or submit the form.
    root.querySelectorAll('.interactive-live-button').forEach(function(btn){
      btn.onclick = async function() {
        if (btn.dataset.interactiveAction === 'play_sound') {
          playSoundWidget(btn.dataset.interactiveSoundId);
          return;
        }
        if (btn.dataset.interactiveAction === 'submit_page') {
          var old = btn.textContent; btn.disabled = true; btn.textContent = 'Submitting…';
          try {
            await postInteractivePageSubmissions(clientId, currentLayout, widgets, root);
            btn.textContent = 'Submitted';
            setTimeout(function(){ btn.disabled = false; btn.textContent = old; }, 1000);
          } catch (e) {
            btn.disabled = false; btn.textContent = old;
            alert('Interactive submit failed: ' + (e && e.message ? e.message : e));
          }
          return;
        }
        var targetId = btn.dataset.interactiveTargetPage;
        if (!targetId) return;
        // Revalidate explicit page switches as well; use the cached copy only
        // when the fresh request is unavailable.
        var target = await fetchAndCacheLayout(targetId, true) || layoutCache[targetId];
        if (!target) {
          lg('Button target layout missing: ' + targetId);
          return;
        }
        // Find a free layer to render into. Both layers may be in use by the
        // slideshow; for interactive layouts we just render straight into the
        // single region (the slideshow is paused while interactive is up).
        var rg = root;
        rg.innerHTML = widgetsFilter(currentLayout.widgets).map(buildWidgetHTML).join('');
        ripleyFitTrafficTemplateText(rg);
        cacheLayout(currentLayout);
        var targetWidgets = widgetsFilter(target.widgets).sort(function(a,b){ return a.z - b.z; });
        // Defer the swap so any error above doesn't strand the user.
        setTimeout(function(){
          rg.innerHTML = targetWidgets.map(buildWidgetHTML).join('');
          ripleyFitTrafficTemplateText(rg);
          injectInstalledFontsIntoIframes(rg);
          initVideoWidgets(target.widgets || [], rg);
          initCameraWidgets(target.widgets || [], rg);
          initImageWidgets(target.widgets || [], rg);
          cacheLayout(target);
          wireInteractiveRuntime(rg, target, clientId);
          startCountdownRuntime(rg, target, clientId, layoutCache, fetchAndCacheLayout, cacheLayout, wireInteractiveRuntime);
          lg('Switched to layout ' + targetId + ' from button on ' + (target.name || targetId));
        }, 50);
      };
    });
  }
  function widgetsFilter(ws) { return Array.isArray(ws) ? ws.slice() : []; }


  // ── Layout Set / Slideshow Engine ──────────────────────────────────────────
  var _sx = { active: 0, itemIndex: 0, timer: null, items: [], dwell: 15000, shuffle: false, pos: 0, runToken: 0 };
  function sxLayers(connector) {
    var region = document.getElementById('r-' + connector);
    if (!region) return { 0: null, 1: null };
    var layers = region.querySelectorAll(':scope > .ly');
    while (layers.length < 2) {
      var layer = document.createElement('div');
      layer.className = 'ly';
      region.appendChild(layer);
      layers = region.querySelectorAll(':scope > .ly');
    }
    return { 0: layers[0], 1: layers[1] };
  }
  function sxOther(n) { return n === 0 ? 1 : 0; }

  async function sxLoadLayout(layer, layoutId, connector, forceFetch) {
    try {
      var ly = await fetchAndCacheLayout(layoutId, forceFetch);
      if (!ly) throw new Error('layout not in cache and fetch failed');
      await ripleyHydrateTrafficTemplates(ly);
      ripleySetRenderOrientation(ly);
      var ws = (ly.widgets || []).slice().sort(function(a,b){ return a.z - b.z; });
      for (var j = 0; j < ws.length; j++) {
        var wp = ws[j].props || {};
        if (ws[j].widget_type === 'weather' && Array.isArray(wp.template_elements) && wp.template_elements.length) {
          await ripleyLoadWeatherData(wp);
        }
        if (ws[j].widget_type === 'calendar' && Array.isArray(wp.template_elements) && wp.template_elements.length) {
          await ripleyLoadCalendarData(wp);
        }
        if (ws[j].widget_type === 'traffic') {
          await ripleyLoadTrafficData(wp, ws[j].id);
        }
        // Pre-fetch headlines for News widgets that don't use a template
        // (template-driven news widgets fetch inside the /news-template.html
        // iframe instead, so they don't need a pre-fetch here). The result
        // is stored in window.__ripleyNewsHeadlines[<widget.id>] and the
        // basic marquee picks it up at render time.
        if (ws[j].widget_type === 'news' && !wp.preset && wp.feedUrl) {
          try {
            var resp = await playerFetch('/api/news/headlines?url=' + encodeURIComponent(wp.feedUrl));
            if (resp.ok) {
              var data = await readPlayerJsonBounded(resp);
              (window.__ripleyNewsHeadlines = window.__ripleyNewsHeadlines || {})[ws[j].id] = data.headlines || [];
            }
          } catch (e) { /* ignore: render will show "Loading news..." */ }
        }
      }
      var html = '';
      for (var j = 0; j < ws.length; j++) { html += buildWidgetHTML(ws[j]); }
      layer.innerHTML = html;
      ripleyFitTrafficTemplateText(layer);
      injectInstalledFontsIntoIframes(layer);
      initMusicWidgets(ws);
      initVideoWidgets(ws, layer);
      initCameraWidgets(ws, layer);
      initImageWidgets(ws, layer);
      // Wire interactive runtime if this layer is the active one and the
      // layout is interactive (the slideshow engine for non-interactive
      // layouts doesn't need to wire anything).
      if (layer.classList.contains('active') && ly.interactive) {
        await wireInteractiveRuntime(layer, ly, clientId);
      }
    } catch(e) {
      layer.textContent = '';
      var error = document.createElement('div');
      error.style.cssText = 'position:absolute;inset:0;display:grid;place-items:center;color:#f87171;font-family:system-ui,sans-serif;text-align:center;';
      error.textContent = 'Slideshow error: ' + (e && e.message ? e.message : 'unknown error');
      layer.appendChild(error);
    }
  }

  function sxCurrentDwell() {
    var item = _sx.items[_sx.itemIndex];
    return Math.max(1000, (Number(item && item.interval_secs) || _sx.dwell / 1000) * 1000);
  }

  async function sxAdvance(connector) {
    if (_sx.items.length < 2) return;
    var token = _sx.runToken;
    var nextIndex = (_sx.itemIndex + 1) % _sx.items.length;
    var layers = sxLayers(connector);
    var next = sxOther(_sx.active);
    await sxLoadLayout(layers[next], _sx.items[nextIndex].layout_id, connector);
    if (token !== _sx.runToken) return;
    layers[next].classList.add('active');
    var nextLayout = layoutCache[_sx.items[nextIndex].layout_id];
    if (nextLayout && nextLayout.interactive) await wireInteractiveRuntime(layers[next], nextLayout, clientId);
    await startCountdownRuntime(layers[next], nextLayout, clientId, layoutCache, fetchAndCacheLayout, cacheLayout, wireInteractiveRuntime);
    setTimeout(function() {
      if (token !== _sx.runToken) return;
      layers[_sx.active].classList.remove('active');
      layers[_sx.active].innerHTML = '';
      _sx.active = next;
      _sx.itemIndex = nextIndex;
      sxSchedule(connector);
    }, 900);
  }

  function sxSchedule(connector) {
    if (_sx.timer != null) clearTimeout(_sx.timer);
    _sx.timer = setTimeout(function() {
      sxAdvance(connector).catch(function(error) { lg('Slideshow advance error: ' + error.message); sxSchedule(connector); });
    }, sxCurrentDwell());
  }

  function sxStart(connector) {
    sxSchedule(connector);
  }

  function sxStop() {
    if (_sx.timer != null) { clearTimeout(_sx.timer); _sx.timer = null; }
    _sx.runToken += 1;
  }

  // Refresh data-bearing template widgets in place. The outer layout and all
  // unrelated widgets remain visible while network-backed weather/calendar
  // data is fetched; only the completed widget contents are replaced.
  async function refreshCurrentDataWidgets(connector, layout) {
    if (!layout || !Array.isArray(layout.widgets)) return;
    var layers = sxLayers(connector);
    var layer = layers[_sx.active];
    if (!layer || !layer.classList.contains('active')) return;
    function findWidgetNode(widget) {
      return Array.prototype.slice.call(layer.querySelectorAll('[data-ripley-widget-id]')).find(function(el) {
        return el.getAttribute('data-ripley-widget-id') === String(widget.id);
      });
    }
    // Keep inline date widgets current without rebuilding their parent layout.
    layout.widgets.filter(function(w) { return w.widget_type === 'datetime'; }).forEach(function(w) {
      var node = findWidgetNode(w);
      var dateNode = node && node.querySelector('[data-ripley-datetime]');
      if (dateNode) dateNode.textContent = formatDateLikeMoment(new Date(), (w.props || {}).format || 'dddd, MMMM D');
    });
    var widgets = layout.widgets.filter(function(w) {
      return (w.widget_type === 'weather' || w.widget_type === 'calendar' || w.widget_type === 'traffic') &&
        w.props && Array.isArray(w.props.template_elements) && w.props.template_elements.length;
    });
    await Promise.all(widgets.map(async function(w) {
      var props = w.props || {};
      try {
        if (w.widget_type === 'weather') await ripleyLoadWeatherData(props);
        else if (w.widget_type === 'calendar') await ripleyLoadCalendarData(props);
        else await ripleyLoadTrafficData(props, w.id);
        // A layout change may have completed while the request was in flight.
        if (!layer.classList.contains('active')) return;
        var node = findWidgetNode(w);
        if (node) {
          node.innerHTML = ripleyTemplateElementsHTML(Object.assign({_widget_type: w.widget_type}, props));
          ripleyFitTrafficTemplateText(node);
        }
      } catch (e) {
        lg('Data widget refresh failed: ' + (e && e.message ? e.message : e));
      }
    }));
  }

  function sxShuffleItems() {
    for (var i = _sx.items.length - 1; i > 0; i--) {
      var j = Math.floor(Math.random() * (i + 1));
      var tmp = _sx.items[i]; _sx.items[i] = _sx.items[j]; _sx.items[j] = tmp;
    }
  }

  async function sxRenderSingle(layoutId, connector, forceFetch) {
    sxStop();
    var layers = sxLayers(connector);
    layers[0].innerHTML = '';
    layers[1].innerHTML = '';
    layers[0].classList.remove('active');
    layers[1].classList.remove('active');
    _sx.items = [];
    await sxLoadLayout(layers[0], layoutId, connector, forceFetch);
    layers[0].classList.add('active');
    _sx.active = 0;
    // Also wire interactive runtime on the single layout (sxLoadLayout only
    // wires if the layer is already active, which it is by the time the
    // above finishes — but if the active class hasn't been added yet when
    // sxLoadLayout runs, we wire here).
    var ly = layoutCache[layoutId];
    if (ly && ly.interactive) {
      await wireInteractiveRuntime(layers[0], ly, clientId);
    }
    await startCountdownRuntime(layers[0], ly, clientId, layoutCache, fetchAndCacheLayout, cacheLayout, wireInteractiveRuntime);
  }

  async function sxRun(clientDisplay, connector) {
    var set = clientDisplay.layout_set;
    if (!set || !set.items || set.items.length === 0) return false;
    sxStop();
    var layers = sxLayers(connector);
    _sx.dwell = (Number(set.dwell_secs) || 15) * 1000;
    _sx.shuffle = !!set.shuffle;
    _sx.items = set.items.slice().sort(function(a,b){ return a.position - b.position; });
    if (_sx.shuffle) sxShuffleItems();
    _sx.active = 0;
    _sx.itemIndex = 0;
    layers[0].innerHTML = '';
    layers[1].innerHTML = '';
    layers[0].classList.remove('active');
    layers[1].classList.remove('active');
    // Prefetch the whole set (the user can stay on the display if the server
    // goes away for a few seconds).
    await prefetchLayouts(_sx.items.map(function(it){ return it.layout_id; }));
    await sxLoadLayout(layers[0], _sx.items[0].layout_id, connector);
    layers[0].classList.add('active');
    var firstLayout = layoutCache[_sx.items[0].layout_id];
    if (firstLayout && firstLayout.interactive) {
      await wireInteractiveRuntime(layers[0], firstLayout, clientId);
    }
    await startCountdownRuntime(layers[0], firstLayout, clientId, layoutCache, fetchAndCacheLayout, cacheLayout, wireInteractiveRuntime);
    if (_sx.items.length > 1) sxLoadLayout(layers[1], _sx.items[1].layout_id, connector).catch(function(){});
    sxStart(connector);
    return true;
  }

  async function renderLoop() {
    try {
      var rg = document.getElementById('r-' + myConnector);
      if (!rg) return;
      var clientDisplay = await fetchClientDisplayAssignment();
      // (Legacy kiosk_page branch removed in Phase 5. Old clients that had
      // a `kiosk_page_id` assignment were migrated to `interactive_layout_id`
      // and will now fall through to the normal layout rendering path. If
      // their layout_id doesn't resolve, they hit the "no layout assigned"
      // fallback below.)
      var layoutId = clientDisplay && clientDisplay.assigned_layout_id;
      if (!layoutId) {
        var state = await playerFetch('/api/display-state').then(function(r){return readPlayerJsonBounded(r);});
        var a = state.assignments.find(function(x){return x.display === myConnector;});
        layoutId = a && a.layout_id;
      }
      if (!layoutId) {
        sxStop();
        rg.innerHTML = '<div style="position:absolute;inset:0;display:grid;place-items:center;color:#94a3b8;font-family:system-ui,sans-serif;text-align:center;padding:24px;">'
                     + '<div><div style="font-size:clamp(28px,3.6vw,48px);font-weight:800;color:#e2e8f0;margin-bottom:.4em;background:linear-gradient(90deg,#38bdf8,#22d3ee);-webkit-background-clip:text;background-clip:text;color:transparent;">No layout assigned</div>'
                     + '<div style="font-size:clamp(14px,1.4vw,18px);">Display ' + ripleyTplEsc(myConnector) + ' is registered but has no active layout.<br>'
                     + 'Open the editor and assign a layout to this display.</div></div>';
        currentContentKey = null;
        return;
      }

      // If the server sent a layout_set, run the slideshow engine
      if (clientDisplay && clientDisplay.layout_set && clientDisplay.layout_set.items && clientDisplay.layout_set.items.length) {
        await sxRun(clientDisplay, myConnector);
        currentContentKey = 'slideshow:' + clientDisplay.layout_set.id;
        return;
      }

      var layoutKey = 'layout:' + layoutId + ':revision:' + String(clientDisplay && clientDisplay.active_layout_revision != null ? clientDisplay.active_layout_revision : 'unknown');
      if (currentContentKey === layoutKey) return;

      lg('Loading layout ' + layoutId + ' for ' + myConnector);
      currentContentKey = layoutKey;
      await sxRenderSingle(layoutId, myConnector, true);
    } catch(e) {
      lg('Render error: ' + e.message);
      console.error(e);
    }
  }

  // ── Music widget support ──
  // Track each music widget by id so we can update the audio element when the
  // playlist/priority changes (e.g. user reorders tracks in the editor).
  // Map: widgetId → { audio: HTMLAudioElement, ownerId: string, state: 'paused'|'playing'|'ended',
  //                    currentIndex: number, registered: boolean, configHash: string }
  var musicWidgets = {};
  // Pool of audio elements for preloading. Each music widget has its own <audio>.
  // We don't share a single <audio> across widgets because that would conflict with
  // SoundManager's per-source volume control.

  // Simple non-cryptographic string hash for change detection
  function musicConfigHash(w) {
      var p = w.props || {};
      var pl = Array.isArray(p.playlist) ? p.playlist.map(function(t){ return t.name + ':' + t.url; }).join('|') : '';
      return [p.priority||10, p.volume!==undefined?p.volume:0.7, p.shuffle?'1':'0', p.replay===false?'0':'1', p.trigger_mode||'auto', pl].join(';');
  }

  function shuffleArray(arr) {
      var a = arr.slice();
      for (var i = a.length - 1; i > 0; i--) {
          var j = Math.floor(Math.random() * (i + 1));
          var tmp = a[i]; a[i] = a[j]; a[j] = tmp;
      }
      return a;
  }

  function initMusicWidgets(widgets) {
      widgets.forEach(function (w) {
          if (w.widget_type !== 'music' && w.widget_type !== 'sound') return;
          var p = w.props || {};
          var playlist = Array.isArray(p.playlist) ? p.playlist : [];
          if (playlist.length === 0) return; // no tracks — nothing to play

          var hash = musicConfigHash(w);
          var existing = musicWidgets[w.id];
          if (existing && existing.configHash === hash) return; // no change

          // Tear down the old audio element if config changed
          if (existing) {
              if (window.SoundManager && existing.ownerId) {
                  window.SoundManager.release(existing.ownerId);
                  window.SoundManager.unregister(existing.ownerId);
              }
              if (existing.audio) {
                  existing.audio.pause();
                  existing.audio.src = '';
              }
          }

          // Apply shuffle if enabled
          var orderedPlaylist = p.shuffle ? shuffleArray(playlist) : playlist.slice();

          // Create the audio element
          var audio = new Audio();
          audio.preload = 'auto';
          audio.loop = false;
          audio.volume = (p.volume !== undefined ? p.volume : 0.7);
          audio.crossOrigin = 'anonymous';

          var ownerId = (w.widget_type === 'sound' ? 'sound-' : 'music-') + w.id;
          var state = {
              audio: audio,
              ownerId: ownerId,
              playlist: orderedPlaylist,
              originalPlaylist: playlist.slice(),
              props: p,
              currentIndex: 0,
              registered: false,
              configHash: hash,
              volume: audio.volume,
              priority: p.priority !== undefined ? p.priority : 10,
          };

          // When a track ends, advance to the next
          audio.addEventListener('ended', function () {
              state.currentIndex++;
              if (state.currentIndex >= state.playlist.length) {
                  if (p.replay === false) {
                      state.audio.pause();
                      if (state.registered && window.SoundManager) window.SoundManager.release(state.ownerId);
                      state.finished = true;
                      return;
                  }
                  // End of playlist — loop back to start (unless shuffle, then reshuffle)
                  if (p.shuffle) {
                      state.playlist = shuffleArray(playlist);
                  }
                  state.currentIndex = 0;
              }
              playCurrentTrack(state);
          });

          // Error handler: skip to next track on error
          audio.addEventListener('error', function (e) {
              lg('Music track error: ' + state.playlist[state.currentIndex].url + ' — skipping');
              state.currentIndex++;
              if (state.currentIndex < state.playlist.length) {
                  playCurrentTrack(state);
              }
          });

          // Register with SoundManager. We register once, then acquire() to start playing.
          if (window.SoundManager) {
              window.SoundManager.register({
                  id: ownerId,
                  priority: state.priority,
                  type: 'html5-audio',
                  ref: audio,
              });
              state.registered = true;
          }

          musicWidgets[w.id] = state;
          if (p.trigger_mode !== 'button') playCurrentTrack(state);
      });
  }

  function playCurrentTrack(state) {
      if (state.playlist.length === 0) return;
      var track = state.playlist[state.currentIndex];
      state.audio.src = track.url;
      // Acquire audio output from SoundManager (no-op if already acquired)
      if (state.registered && window.SoundManager) {
          window.SoundManager.acquire(state.ownerId);
      }
      // Play (may be blocked by autoplay policy if no user interaction has occurred)
      state.audio.play().catch(function (e) {
          lg('Music autoplay blocked: ' + e.message + ' — will retry on next user interaction');
      });
  }

  function playSoundWidget(widgetId) {
      var state = musicWidgets[widgetId];
      if (!state) { lg('Sound widget not ready: ' + widgetId); return; }
      state.currentIndex = 0;
      state.finished = false;
      if (state.props && state.props.shuffle) state.playlist = shuffleArray(state.originalPlaylist);
      playCurrentTrack(state);
  }

  await renderLoop();
  setInterval(renderLoop, 10000);
  // Refresh only weather/calendar template contents. Do not clear the active
  // layer or rebuild the whole layout: a slow external feed must never turn
  // the physical display black.
  setInterval(function() {
    var prefix = 'layout:';
    var layoutId = currentContentKey && currentContentKey.indexOf(prefix) === 0 ? currentContentKey.slice(prefix.length).split(':revision:')[0] : '';
    var layout = layoutId ? layoutCache[layoutId] : null;
    if (layout) refreshCurrentDataWidgets(myConnector, layout).catch(function(e) {
      lg('Periodic data refresh failed: ' + (e && e.message ? e.message : e));
    });
  }, 15 * 60 * 1000);
}

const ripleyPreviewMode = window.location.pathname.endsWith('/preview.html') || new URLSearchParams(window.location.search).get('preview') === '1';
if (ripleyPreviewMode) {
  window.RipleyPreviewRenderer = {
    buildWidgetHTML: buildWidgetHTML,
    bindInteractiveKeyboardRuntime: bindInteractiveKeyboardRuntime,
  };
} else {
  init();
}

