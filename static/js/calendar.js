
var p = new URLSearchParams(location.search);
var requestedFont = p.get('font_family');
if (requestedFont && requestedFont.length <= 200 && !/[<>]|javascript:/i.test(requestedFont)) document.documentElement.style.fontFamily = requestedFont;
var ICS_URL = p.get('ics') || '';
var TIME_FORMAT = p.get('time_format') === '24' ? '24' : '12';
var MAX_CALENDAR_RESPONSE_BYTES = 4 * 1024 * 1024;
function readCalendarTextBounded(response, maximumBytes) {
  var reader = response.body && response.body.getReader ? response.body.getReader() : null;
  if (!reader) return Promise.reject(new Error('response body is unavailable'));
  var decoder = new TextDecoder(), total = 0, text = '';
  return (async function() {
    try {
      while (true) {
        var part = await reader.read();
        if (part.done) { text += decoder.decode(); return text; }
        var value = part.value || new Uint8Array();
        total += value.byteLength;
        if (total > maximumBytes) { try { await reader.cancel(); } catch (_) {} throw new Error('response exceeds its size limit'); }
        text += decoder.decode(value, { stream: true });
      }
    } finally { reader.releaseLock(); }
  })();
}
function readCalendarJsonBounded(response) { return readCalendarTextBounded(response, MAX_CALENDAR_RESPONSE_BYTES).then(function(text){ return JSON.parse(text); }); }

// Built-in sample ICS data for previews and unconfigured widgets
var SAMPLE_ICS = `BEGIN:VCALENDAR
VERSION:2.0
PRODID:-//Digital Signage//Widget Preview//EN
BEGIN:VEVENT
UID:sample-1@signage
DTSTART:{today}T090000
DTEND:{today}T100000
SUMMARY:Team standup
LOCATION:Conference Room A
END:VEVENT
BEGIN:VEVENT
UID:sample-2@signage
DTSTART:{today}T110000
DTEND:{today}T113000
SUMMARY:Design review
LOCATION:Zoom
END:VEVENT
BEGIN:VEVENT
UID:sample-3@signage
DTSTART:{today}T140000
DTEND:{today}T153000
SUMMARY:Sprint planning
LOCATION:Room 202
END:VEVENT
BEGIN:VEVENT
UID:sample-4@signage
DTSTART:{tomorrow}T100000
DTEND:{tomorrow}T110000
SUMMARY:Lunch with Alex
LOCATION:Cafeteria
END:VEVENT
BEGIN:VEVENT
UID:sample-5@signage
DTSTART:{tomorrow}T150000
DTEND:{tomorrow}T154500
SUMMARY:Quarterly check-in
LOCATION:Office
END:VEVENT
BEGIN:VEVENT
UID:sample-6@signage
DTSTART:{day2}T090000
DTEND:{day2}T110000
SUMMARY:Workshop: UX patterns
LOCATION:Auditorium
END:VEVENT
BEGIN:VEVENT
UID:sample-7@signage
DTSTART:{day2}T130000
DTEND:{day2}T133000
SUMMARY:Coffee chat
LOCATION:Cafeteria
END:VEVENT
BEGIN:VEVENT
UID:sample-8@signage
DTSTART:{day3}T110000
DTEND:{day3}T120000
SUMMARY:Architecture sync
LOCATION:Room 101
END:VEVENT
BEGIN:VEVENT
UID:sample-9@signage
DTSTART:{day4}T140000
DTEND:{day4}T170000
SUMMARY:Customer demo
LOCATION:Zoom
END:VEVENT
BEGIN:VEVENT
UID:sample-10@signage
DTSTART:{day5}T100000
DTEND:{day5}T110000
SUMMARY:1-on-1 with manager
LOCATION:Office
END:VEVENT
END:VCALENDAR`;
var TEMPLATE = p.get('template') || 'list';
var COLOR = p.get('color') || '#ffffff';
var ACCENT = p.get('accent') || '#e94560';
var MAX_EVENTS = parseInt(p.get('maxevents') || '5', 10);
var INCLUDE_WEATHER = p.get('incweather') === 'true';
var LAT = parseFloat(p.get('lat') || '0');
var LON = parseFloat(p.get('lon') || '0');

document.documentElement.style.setProperty('--accent', ACCENT);
document.body.style.color = COLOR;

var events = [];
var lastFetch = 0;
var weatherData = null;

var weatherIcons = {0:'\u2600\ufe0f',1:'\ud83c\udf25\ufe0f',2:'\u26ab',3:'\u2601\ufe0f',
  45:'\ud83c\udf2b\ufe0f',48:'\ud83c\udf2b\ufe0f',
  51:'\ud83c\udf26\ufe0f',53:'\ud83c\udf26\ufe0f',55:'\ud83c\udf27\ufe0f',
  56:'\ud83c\udf27\ufe0f',57:'\ud83c\udf27\ufe0f',
  61:'\ud83c\udf27\ufe0f',63:'\ud83c\udf27\ufe0f',65:'\ud83c\udf27\ufe0f',
  66:'\ud83c\udf27\ufe0f',67:'\ud83c\udf27\ufe0f',
  71:'\ud83c\udf28\ufe0f',73:'\ud83c\udf28\ufe0f',75:'\u2744\ufe0f',77:'\u2744\ufe0f',
  80:'\ud83c\udf26\ufe0f',81:'\ud83c\udf27\ufe0f',82:'\ud83c\udf27\ufe0f',
  85:'\ud83c\udf28\ufe0f',86:'\u2744\ufe0f',
  95:'\u26c8\ufe0f',96:'\u26c8\ufe0f',99:'\u26c8\ufe0f'};

function getWeatherIcon(dateStr) {
  if (!weatherData || !weatherData.daily) return '';
  var idx = weatherData.daily.time.indexOf(dateStr);
  if (idx < 0) return '';
  var code = weatherData.daily.weather_code[idx];
  return weatherIcons[code] || '';
}

function fetchWeatherData() {
  if (!INCLUDE_WEATHER || !LAT || !LON) return Promise.resolve();
  var url = '/api/weather/forecast?lat=' + encodeURIComponent(LAT) + '&lon=' + encodeURIComponent(LON);
  return fetch(url).then(function(r){return readCalendarJsonBounded(r);}).then(function(d){weatherData=d;}).catch(function(){});
}

function parseICS(text) {
  var evts = [];
  // 2026-08-11: unfold RFC 5545 long lines. A line that starts with a space
  // or tab is a continuation of the previous line. Without this, events
  // with long summaries/locations get split into bogus extra VEVENTs.
  var rawLines = text.replace(/\r\n/g, '\n').replace(/\r/g, '\n').split('\n');
  var lines = [];
  for (var i = 0; i < rawLines.length; i++) {
    var ln = rawLines[i];
    if ((ln.charAt(0) === ' ' || ln.charAt(0) === '\t') && lines.length > 0) {
      lines[lines.length - 1] += ln.substr(1);
    } else {
      lines.push(ln);
    }
  }
  var current = null;
  for (var i = 0; i < lines.length; i++) {
    var line = lines[i];
    if (line.startsWith('BEGIN:VEVENT')) { current = {}; }
    else if (line.startsWith('END:VEVENT')) {
      if (current && current.start && current.summary) evts.push(current);
      current = null;
    }
    else if (current) {
      // 2026-08-11: split base property name (before params) so we don't
      // confuse 'DTSTART;VALUE=DATE:...' with 'DTSTART:...'
      var propName = line.split(':', 1)[0];
      var baseName = propName.split(';', 1)[0];
      if (baseName === 'DTSTART') current.start = parseICSDate(line);
      else if (baseName === 'DTEND') current.end = parseICSDate(line);
      else if (baseName === 'SUMMARY') current.summary = unescapeICS(line.substring(propName.length + 1));
      else if (baseName === 'LOCATION') current.location = unescapeICS(line.substring(propName.length + 1));
      else if (baseName === 'DESCRIPTION') current.description = unescapeICS(line.substring(propName.length + 1));
    }
  }
  return evts;
}

function unescapeICS(s) {
  if (!s) return s;
  // RFC 5545 §3.3.11: backslash-escaped chars. Order matters (escape first).
  return s.replace(/\\n/gi, '\n').replace(/\\,/g, ',').replace(/\\;/g, ';').replace(/\\\\/g, '\\');
}

function parseICSDate(line) {
  // 2026-08-11: handle ALL-DAY (VALUE=DATE) and UTC 'Z' suffix.
  var colon = line.indexOf(':');
  if (colon < 0) return null;
  var params = line.substring(0, colon);
  var val = line.substring(colon + 1).trim();
  if (!val || val.length < 8) return null;
  var y = parseInt(val.substr(0, 4), 10);
  var mo = parseInt(val.substr(4, 2), 10) - 1;
  var d = parseInt(val.substr(6, 2), 10);
  if (val.length === 8 || /VALUE=DATE/i.test(params)) {
    return new Date(y, mo, d, 0, 0, 0, 0);
  }
  var h = 0, mi = 0, s = 0;
  if (val.length > 9 && val.charAt(8) === 'T') {
    h = parseInt(val.substr(9, 2), 10) || 0;
    mi = parseInt(val.substr(11, 2), 10) || 0;
    if (val.length >= 15) s = parseInt(val.substr(13, 2), 10) || 0;
  }
  var isUtc = /Z$/.test(val);
  return isUtc ? new Date(Date.UTC(y, mo, d, h, mi, s)) : new Date(y, mo, d, h, mi, s);
}

function escapeHTML(s) {
  if (s == null) return '';
  return String(s).replace(/[&<>"']/g, function (c) {
    return { '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c];
  });
}

function getUpcomingEvents(allEvents, max) {
  var now = new Date();
  return allEvents.filter(function(e) {
    return e.start && e.start >= now;
  }).sort(function(a, b) { return a.start - b.start; }).slice(0, max);
}

function getTodayEvents(allEvents) {
  var now = new Date();
  var today = new Date(now.getFullYear(), now.getMonth(), now.getDate());
  var tomorrow = new Date(today.getTime() + 86400000);
  return allEvents.filter(function(e) {
    return e.start && e.start >= today && e.start < tomorrow;
  }).sort(function(a, b) { return a.start - b.start; });
}

function fmtTime(d) {
  var h = d.getHours();
  var m = d.getMinutes().toString().padStart(2, '0');
  if (TIME_FORMAT === '24') return String(h).padStart(2, '0') + ':' + m;
  var ap = h >= 12 ? 'PM' : 'AM';
  h = h % 12 || 12;
  return h + ':' + m + ' ' + ap;
}

function fmtDay(d) {
  var days = ['SUN','MON','TUE','WED','THU','FRI','SAT'];
  var months = ['JAN','FEB','MAR','APR','MAY','JUN','JUL','AUG','SEP','OCT','NOV','DEC'];
  return { day: d.getDate(), month: months[d.getMonth()], weekday: days[d.getDay()] };
}

function fmtFullDate(d) {
  var days = ['Sunday','Monday','Tuesday','Wednesday','Thursday','Friday','Saturday'];
  var months = ['January','February','March','April','May','June','July','August','September','October','November','December'];
  return days[d.getDay()] + ', ' + months[d.getMonth()] + ' ' + d.getDate();
}

function renderList(events) {
  var html = '<div class="cal-list">';
  events.forEach(function(e) {
    var d = fmtDay(e.start);
    var timeStr = fmtTime(e.start);
    var wIcon = getWeatherIcon(e.start.toISOString().split('T')[0]);
    html += '<div class="event">';
    html += '<div class="event-date"><div class="day">' + d.day + '</div><div class="month">' + d.month + '</div></div>';
    html += '<div class="event-info"><div class="event-title">' + escapeHTML(e.summary) + '</div>';
    html += '<div class="event-time">' + d.weekday + ' ' + timeStr;
    if (wIcon) html += ' <span class="event-weather">' + wIcon + '</span>';
    html += '</div></div></div>';
  });
  if (events.length === 0) html += '<div style="opacity:0.5;text-align:center;padding:3vh;">No upcoming events</div>';
  html += '</div>';
  return html;
}

function renderAgenda(events) {
  var html = '<div class="cal-agenda">';
  var lastDay = null;
  events.forEach(function(e) {
    var dayKey = e.start.toDateString();
    if (dayKey !== lastDay) {
      html += '<div class="agenda-day">' + fmtFullDate(e.start) + '</div>';
      lastDay = dayKey;
    }
    var wIcon = getWeatherIcon(e.start.toISOString().split('T')[0]);
    html += '<div class="event"><div class="event-time">' + fmtTime(e.start) + '</div>';
    html += '<div class="event-dot"></div><div class="event-title">' + escapeHTML(e.summary) + '</div>';
    if (wIcon) html += '<span class="event-weather">' + wIcon + '</span>';
    html += '</div>';
  });
  if (events.length === 0) html += '<div style="opacity:0.5;text-align:center;padding:3vh;">No upcoming events</div>';
  html += '</div>';
  return html;
}

function renderToday(events) {
  var now = new Date();
  var html = '<div class="cal-today">';
  html += '<div class="today-header">Today</div>';
  html += '<div class="today-date">' + fmtFullDate(now) + '</div>';
  events.forEach(function(e) {
    var wIcon = getWeatherIcon(e.start.toISOString().split('T')[0]);
    html += '<div class="event"><div class="event-time">' + fmtTime(e.start) + '</div>';
    html += '<div class="event-title">' + escapeHTML(e.summary) + '</div>';
    if (wIcon) html += '<span class="event-weather">' + wIcon + '</span>';
    html += '</div>';
  });
  if (events.length === 0) html += '<div class="no-events">No events today</div>';
  html += '</div>';
  return html;
}

function renderMiniMonth(events) {
  var now = new Date();
  var year = now.getFullYear();
  var month = now.getMonth();
  var months = ['January','February','March','April','May','June','July','August','September','October','November','December'];
  var firstDay = new Date(year, month, 1);
  var lastDate = new Date(year, month + 1, 0).getDate();
  var startDow = firstDay.getDay();
  var dows = ['Sun','Mon','Tue','Wed','Thu','Fri','Sat'];
  var eventDays = {};
  events.forEach(function(e) {
    if (e.start) { eventDays[e.start.getFullYear() + '-' + e.start.getMonth() + '-' + e.start.getDate()] = true; }
  });
  var html = '<div class="cal-mini-month">';
  html += '<div class="month-header">' + months[month] + ' ' + year + '</div>';
  html += '<div class="grid">';
  dows.forEach(function(d) { html += '<div class="dow">' + d + '</div>'; });
  for (var i = 0; i < startDow; i++) html += '<div class="cell other"></div>';
  for (var day = 1; day <= lastDate; day++) {
    var isToday = (day === now.getDate());
    var hasEvent = eventDays[year + '-' + month + '-' + day];
    var cls = 'cell';
    if (isToday) cls += ' today';
    if (hasEvent) cls += ' has-event';
    html += '<div class="' + cls + '">' + day + '</div>';
  }
  html += '</div></div>';
  return html;
}

// Returns sample calendar events matching the ICS-parsed shape.
// Used as a fallback when no real ICS URL is configured, or for previews.
function getSampleEvents() {
  var now = new Date();
  var today = new Date(now.getFullYear(), now.getMonth(), now.getDate());
  function ev(offsetDays, startHour, durationMinutes, summary, location) {
    var start = new Date(today.getTime() + offsetDays * 86400000);
    start.setHours(startHour, 0, 0, 0);
    var end = new Date(start.getTime() + durationMinutes * 60000);
    return {
      start: start,
      end: end,
      summary: summary,
      location: location || '',
      description: ''
    };
  }
  return [
    ev(0, 9, 60, 'Team standup', 'Conference Room A'),
    ev(0, 11, 30, 'Design review', 'Zoom'),
    ev(0, 14, 90, 'Sprint planning', 'Room 202'),
    ev(1, 10, 60, 'Lunch with Alex', 'Cafeteria'),
    ev(1, 15, 45, 'Quarterly check-in', 'Office'),
    ev(2, 9, 120, 'Workshop: UX patterns', 'Auditorium'),
    ev(2, 13, 30, 'Coffee chat', 'Cafeteria'),
    ev(3, 11, 60, 'Architecture sync', 'Room 101'),
    ev(4, 14, 180, 'Customer demo', 'Zoom'),
    ev(5, 10, 60, '1-on-1 with manager', 'Office')
  ];
}

function renderCalendar() {
  var el = document.getElementById('calendar');
  if (!ICS_URL) {
    // No ICS URL configured — use sample ICS data so the widget shows something.
    try {
      events = getSampleEvents();
    } catch (e) {
      events = [];
    }
    doRender();
    return;
  }
  var now = Date.now();
  if (now - lastFetch > 60000) {
    lastFetch = now;
    fetch('/api/ics-proxy?url=' + encodeURIComponent(ICS_URL))
      .then(function(r) { return readCalendarTextBounded(r, MAX_CALENDAR_RESPONSE_BYTES); })
      .then(function(text) {
        events = parseICS(text);
        return fetchWeatherData();
      })
      .then(function() { doRender(); })
      .catch(function(e) {
        el.textContent = '';
        var error = document.createElement('div');
        error.style.cssText = 'opacity:0.5;text-align:center;padding:3vh;font-size:1.8vh;';
        error.textContent = 'Failed to load calendar: ' + (e && e.message ? e.message : 'unknown error');
        el.appendChild(error);
        // Fall back to sample events so the widget still shows something
        events = getSampleEvents();
        doRender();
      });
  } else {
    doRender();
  }
}

function doRender() {
  var el = document.getElementById('calendar');
  var displayEvents;
  if (TEMPLATE === 'today') {
    displayEvents = getTodayEvents(events);
  } else {
    displayEvents = getUpcomingEvents(events, MAX_EVENTS);
  }
  if (TEMPLATE === 'agenda') el.innerHTML = renderAgenda(displayEvents);
  else if (TEMPLATE === 'today') el.innerHTML = renderToday(displayEvents);
  else if (TEMPLATE === 'mini-month') {
    var allMonthEvents = events.filter(function(e) {
      return e.start && e.start.getMonth() === new Date().getMonth() && e.start.getFullYear() === new Date().getFullYear();
    });
    el.innerHTML = renderMiniMonth(allMonthEvents);
  }
  else el.innerHTML = renderList(displayEvents);
}

renderCalendar();
setInterval(renderCalendar, 60000);

