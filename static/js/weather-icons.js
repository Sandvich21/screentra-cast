// Shared local Meteocons renderer used by the editor, Layout Preview, player,
// and weather iframe. The animated and reduced-motion-safe SVGs are vendored
// under /assets/weather/meteocons so disconnected clients do not depend on CDN.
const RIPLEY_WMO_DESCRIPTIONS = {
  0: 'Clear sky', 1: 'Mainly clear', 2: 'Partly cloudy', 3: 'Overcast',
  45: 'Fog', 48: 'Depositing rime fog', 51: 'Light drizzle',
  53: 'Moderate drizzle', 55: 'Dense drizzle', 61: 'Slight rain',
  63: 'Moderate rain', 65: 'Heavy rain', 71: 'Slight snow',
  73: 'Moderate snow', 75: 'Heavy snow', 80: 'Rain showers',
  81: 'Heavy rain showers', 82: 'Violent rain showers', 95: 'Thunderstorm',
  96: 'Thunderstorm with hail', 99: 'Severe thunderstorm with hail'
};

const RIPLEY_METEOCONS_WMO = {
  0: { day: 'clear-day', night: 'clear-night' },
  1: { day: 'mostly-clear-day', night: 'mostly-clear-night' },
  2: { day: 'partly-cloudy-day', night: 'partly-cloudy-night' },
  3: { day: 'overcast-day', night: 'overcast-night' },
  45: { day: 'fog-day', night: 'fog-night' },
  48: { day: 'overcast-day-fog', night: 'overcast-night-fog' },
  51: { day: 'drizzle', night: 'drizzle' },
  53: { day: 'drizzle', night: 'drizzle' },
  55: { day: 'extreme-drizzle', night: 'extreme-drizzle' },
  61: { day: 'rain', night: 'rain' },
  63: { day: 'rain', night: 'rain' },
  65: { day: 'extreme-rain', night: 'extreme-rain' },
  71: { day: 'snow', night: 'snow' },
  73: { day: 'snow', night: 'snow' },
  75: { day: 'extreme-snow', night: 'extreme-snow' },
  80: { day: 'partly-cloudy-day-rain', night: 'partly-cloudy-night-rain' },
  81: { day: 'overcast-day-rain', night: 'overcast-night-rain' },
  82: { day: 'extreme-day-rain', night: 'extreme-night-rain' },
  95: { day: 'thunderstorms-day', night: 'thunderstorms-night' },
  96: { day: 'thunderstorms-day-hail', night: 'thunderstorms-night-hail' },
  99: { day: 'extreme-thunderstorms-day', night: 'extreme-thunderstorms-night' }
};

function ripleyWeatherIconDescription(code) {
  return RIPLEY_WMO_DESCRIPTIONS[Number(code)] || 'Weather';
}

function ripleyWeatherAlertMarkup(slug, label, options = {}) {
  const escape = value => String(value).replace(/[&<>"']/g, c => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));
  const base = '/assets/weather/meteocons';
  const animated = `${base}/fill/${slug}.svg`;
  const staticIcon = `${base}/fill-static/${slug}.svg`;
  if (options.staticOnly) return `<img class="ripley-weather-alert-icon" src="${staticIcon}" alt="${escape(label)}" loading="eager" decoding="async" style="display:block;width:100%;height:100%;object-fit:contain;">`;
  return `<picture class="ripley-weather-alert-icon" style="display:block;width:100%;height:100%;"><source media="(prefers-reduced-motion: reduce)" srcset="${staticIcon}"><img src="${animated}" alt="${escape(label)}" loading="eager" decoding="async" style="display:block;width:100%;height:100%;object-fit:contain;"></picture>`;
}

function ripleyWeatherAlertIconSvg(alerts, options = {}) {
  const items = Array.isArray(alerts) ? alerts.filter(Boolean).map(String) : (alerts ? [String(alerts)] : []);
  if (!items.length) return '';
  const label = items.join(', ');
  const lower = label.toLowerCase();
  let slug = null;
  if (/flood/.test(lower)) slug = 'extreme-rain';
  else if (/warning/.test(lower)) slug = 'code-red';
  else if (/watch/.test(lower)) slug = 'code-orange';
  else if (/advisory|statement|outlook/.test(lower)) slug = 'code-yellow';
  else slug = 'code-purple';
  return ripleyWeatherAlertMarkup(slug, label, options);
}

function ripleyWeatherAlertColorCodeIconSvg(alerts, options = {}) {
  const items = Array.isArray(alerts) ? alerts.filter(Boolean).map(String) : (alerts ? [String(alerts)] : []);
  if (!items.length) return '';
  const label = items.join(', ');
  const lower = label.toLowerCase();
  const slug = /warning/.test(lower) ? 'code-red' : /watch/.test(lower) ? 'code-orange' : /advisory|statement|outlook/.test(lower) ? 'code-yellow' : 'code-purple';
  return ripleyWeatherAlertMarkup(slug, label, options);
}

function ripleyWeatherAlertConditionIconSvg(alerts, options = {}) {
  const items = Array.isArray(alerts) ? alerts.filter(Boolean).map(String) : (alerts ? [String(alerts)] : []);
  if (!items.length) return '';
  const label = items.join(', ');
  const lower = label.toLowerCase();
  let slug = null;
  if (/flood/.test(lower)) slug = 'extreme-rain';
  else if (/fog|low visibility/.test(lower)) slug = 'fog-day';
  else if (/thunder|lightning|severe/.test(lower)) slug = 'extreme-thunderstorms-day';
  else if (/snow|blizzard|winter|ice|freez/.test(lower) && !/squall/.test(lower)) slug = 'extreme-snow';
  return slug ? ripleyWeatherAlertMarkup(slug, label, options) : '';
}

function ripleyWeatherAlertHasWarningOrWatch(alerts) {
  const items = Array.isArray(alerts) ? alerts : (alerts ? [alerts] : []);
  return items.some(alert => /\b(?:warning|watch)\b/i.test(String(alert)));
}

function ripleyWeatherIconSvg(code, isNight, options = {}) {
  const entry = RIPLEY_METEOCONS_WMO[Number(code)] || RIPLEY_METEOCONS_WMO[2];
  const slug = entry[isNight ? 'night' : 'day'];
  const label = ripleyWeatherIconDescription(code);
  const base = '/assets/weather/meteocons';
  const animated = `${base}/fill/${slug}.svg`;
  const staticIcon = `${base}/fill-static/${slug}.svg`;
  if (options.staticOnly) {
    return `<img class="ripley-weather-icon" src="${staticIcon}" alt="${label}" loading="eager" decoding="async" style="display:block;width:100%;height:100%;object-fit:contain;">`;
  }
  return `<picture class="ripley-weather-icon" style="display:block;width:100%;height:100%;"><source media="(prefers-reduced-motion: reduce)" srcset="${staticIcon}"><img src="${animated}" alt="${label}" loading="eager" decoding="async" style="display:block;width:100%;height:100%;object-fit:contain;"></picture>`;
}
