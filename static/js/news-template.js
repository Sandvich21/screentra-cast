  // Read the preset id from the URL: ?preset=<uuid>&cache=<seconds>
  var params = new URLSearchParams(window.location.search);
  var requestedFont = params.get('font_family');
  if (requestedFont && requestedFont.length <= 200 && !/[<>]|javascript:/i.test(requestedFont)) document.documentElement.style.fontFamily = requestedFont;
  var presetId = params.get('preset') || '';
  var cacheSeconds = Number(params.get('cache') || 60);
  if (cacheSeconds < 10) cacheSeconds = 10;
  if (cacheSeconds > 600) cacheSeconds = 600;

  // Cached fetch with TTL — the iframe re-renders when the page reloads,
  // and the player caches the layout every few minutes. Keeping the cache
  // window short means fresh headlines after a refresh; long enough to
  // avoid hammering the server on a busy ticker.
  var _cache = {};
  var MAX_NEWS_RESPONSE_BYTES = 4 * 1024 * 1024;
  function readNewsJsonBounded(response, maximumBytes) {
    maximumBytes = maximumBytes || MAX_NEWS_RESPONSE_BYTES;
    var reader = response.body && response.body.getReader ? response.body.getReader() : null;
    if (!reader) return Promise.reject(new Error('response body is unavailable'));
    var decoder = new TextDecoder();
    var total = 0;
    var text = '';
    function release() { try { reader.releaseLock(); } catch (_) {} }
    function readNext() {
      return reader.read().then(function(part) {
        if (part.done) { text += decoder.decode(); release(); return JSON.parse(text); }
        var value = part.value || new Uint8Array();
        total += value.byteLength;
        if (total > maximumBytes) {
          return Promise.resolve(reader.cancel()).catch(function() {}).then(function() {
            release();
            throw new Error('response exceeds its size limit');
          });
        }
        text += decoder.decode(value, { stream: true });
        return readNext();
      }, function(error) { release(); throw error; });
    }
    return readNext();
  }
  function cachedFetch(url) {
    var now = Date.now();
    var entry = _cache[url];
    if (entry && (now - entry.at) < cacheSeconds * 1000) {
      return Promise.resolve(entry.json);
    }
    return fetch(url, { credentials: 'include' })
      .then(function(r) {
        if (!r.ok) throw new Error('HTTP ' + r.status);
        return readNewsJsonBounded(r);
      })
      .then(function(j) {
        _cache[url] = { at: now, json: j };
        return j;
      });
  }

  function showError(msg) {
    var el = document.getElementById('error-banner');
    el.textContent = msg;
    el.classList.add('visible');
    console.warn('news-template:', msg);
  }
  function setStatus(msg) {
    document.getElementById('status').textContent = msg;
  }
  function esc(s) {
    return String(s == null ? '' : s)
      .replace(/&/g, '&amp;')
      .replace(/</g, '&lt;')
      .replace(/>/g, '&gt;')
      .replace(/"/g, '&quot;')
      .replace(/'/g, '&#39;');
  }

  // ── Template elements renderer ─────────────────────────────
  //
  // A trimmed clone of the player's ripleyTemplateElementsHTML
  // (static/js/player.js:156) so this page can render the same kind
  // of elements the Widget Templates editor produces. The differences:
  //
  //   * `variable_text` / `variable_icon` resolve through `sample()`
  //     which has access to a headlines array — so the user can
  //     bind text elements to specific headline indices.
  //   * `news_ticker` (new element type) renders a horizontal
  //     scrolling marquee that cycles through the cached headlines.
  //
  // The renderer is intentionally minimal: we don't ship the entire
  // player.js to the iframe, just the small subset needed for news
  // templates.

  function repeatCount(widgetProps, el) {
    var rep = el.repeat || {};
    if (rep.group) {
      // News templates don't use the weather/calendar repeat group;
      // fall through to count.
    }
    return Math.max(1, Math.min(30, Number(rep.count) || 1));
  }
  function repeatMode(el) {
    var rep = el.repeat || {};
    if (rep.group && (!rep.mode || rep.mode === 'none')) return 'rows';
    return rep.mode || 'none';
  }
  function copyProps(el, repeatIndex) {
    var p = Object.assign({}, el.props || {});
    if (repeatIndex <= 0) return p;
    if (p.variable_source === 'news') {
      p.news_index = (Number(p.news_index) || 0) + repeatIndex;
    } else if (p.variable_source === 'date' || p.variable_source === 'weather') {
      var base = Math.max(0, Math.min(29, Number(p.day_offset) || 0));
      p.day_offset = base + repeatIndex;
    }
    return p;
  }
  function expandedBounds(widgetProps, elements) {
    var width = Number(widgetProps.template_width) || 760;
    var height = Number(widgetProps.template_height) || 427.5;
    elements.forEach(function(el) {
      var rep = el.repeat || { mode: 'none', count: 1, gap_x: 0, gap_y: 0 };
      var mode = repeatMode(el);
      var count = repeatCount(widgetProps, el);
      for (var i = 0; i < count; i++) {
        var x = (Number(el.x) || 0) + i * ((Number(rep.gap_x) || 0) + (mode === 'columns' ? (Number(el.width) || 0) : 0));
        var y = (Number(el.y) || 0) + i * ((Number(rep.gap_y) || 0) + (mode === 'rows' ? (Number(el.height) || 0) : 0));
        width = Math.max(width, x + (Number(el.width) || 0));
        height = Math.max(height, y + (Number(el.height) || 0));
      }
    });
    return { width: width, height: height };
  }

  // Resolve a variable_text / variable_icon element to its value.
  // For news, `news_index` selects which headline to show. Indices
  // beyond the array length wrap around (modulo).
  function sample(el, widgetProps, headlines) {
    var p = el.props || {};
    if (el.type === 'variable_icon' || p.variable_source === 'weather_icon' || p.display_mode === 'weather_icon') {
      return { html: '<span style="font-size:2.2em;line-height:1;">\u2600\ufe0f</span>' };
    }
    var source = p.variable_source || 'date';
    if (source === 'news') {
      var idx = Number(p.news_index) || 0;
      if (!headlines || !headlines.length) {
        return { text: p.default_text || p.label || '(no news)' };
      }
      var i = ((idx % headlines.length) + headlines.length) % headlines.length;
      var h = headlines[i];
      return { text: h && h.title ? h.title : (p.default_text || '') };
    }
    if (source === 'date') {
      var d = new Date();
      d.setDate(d.getDate() + (Number(p.day_offset) || 0));
      var key = p.variable_key || 'full_date';
      if (key === 'day_name') return { text: d.toLocaleDateString(undefined, { weekday: 'long' }) };
      if (key === 'date_number') return { text: String(d.getDate()) };
      if (key === 'month_day') return { text: d.toLocaleDateString(undefined, { month: 'short', day: 'numeric' }) };
      return { text: d.toLocaleDateString(undefined, { weekday: 'long', month: 'short', day: 'numeric' }) };
    }
    return { text: p.default_text || p.label || source };
  }

  // Build a single news ticker DOM node (the actual marquee animation
  // is CSS-driven; this just sets up the wrapper and seed content).
  function newsTickerHTML(el, widgetProps, headlines) {
    var p = el.props || {};
    var w = Number(el.width) || 800;
    var h = Number(el.height) || 60;
    var dur = ({ slow: '60s', normal: '30s', fast: '15s' })[p.speed] || '30s';
    var sep = p.separator || '\u2022';
    var bg = p.color ? p.color : 'rgba(0,0,0,0.4)';
    var accent = p.accent_color || '#e94560';
    var fontSize = p.font_size || 24;
    var items = '';
    if (headlines && headlines.length) {
      // Duplicate the headlines so the marquee has no visible seam
      // as the first set scrolls off the left edge.
      var pool = headlines.concat(headlines);
      pool.forEach(function(h, i) {
        items += '<span class="news-ticker-headline">' + esc(h.title) + '</span>';
        if (i < pool.length - 1) {
          items += '<span class="news-ticker-sep">' + esc(sep) + '</span>';
        }
      });
    } else {
      items = '<span class="news-ticker-headline">' +
        esc(p.empty_text || 'No headlines yet') + '</span>';
    }
    return ''
      + '<div class="news-ticker" style="background:' + esc(bg) + ';">'
      +   '<div class="news-ticker-track" style="font-size:' + fontSize + 'px;color:#fff;font-weight:700;'
      +     'animation:ripleyNewsMarquee ' + dur + ' linear infinite;">'
      +     items
      +   '</div>'
      + '</div>';
  }

  function renderTemplateElements(widgetProps, headlines) {
    var elements = Array.isArray(widgetProps.template_elements) ? widgetProps.template_elements : [];
    if (!elements.length) return '';
    var bounds = expandedBounds(widgetProps, elements);
    var designW = Math.max(1, Number(bounds.width) || 760);
    var designH = Math.max(1, Number(bounds.height) || 427.5);
    var out = '<div class="ripley-template-elements-render">';
    elements.slice().sort(function(a, b) { return (Number(a.z) || 0) - (Number(b.z) || 0); }).forEach(function(el) {
      var rep = el.repeat || { mode: 'none', count: 1, gap_x: 0, gap_y: 0 };
      var mode = repeatMode(el);
      var count = repeatCount(widgetProps, el);
      for (var i = 0; i < count; i++) {
        var x = (Number(el.x) || 0) + i * ((Number(rep.gap_x) || 0) + (mode === 'columns' ? (Number(el.width) || 0) : 0));
        var y = (Number(el.y) || 0) + i * ((Number(rep.gap_y) || 0) + (mode === 'rows' ? (Number(el.height) || 0) : 0));
        var w = Number(el.width) || 120, h = Number(el.height) || 80;
        var p = copyProps(el, i);
        var renderEl = Object.assign({}, el, { props: p });
        var style = ''
          + 'position:absolute;box-sizing:border-box;overflow:hidden;'
          + 'display:flex;align-items:center;justify-content:center;'
          + 'text-align:' + esc(p.align || 'center') + ';'
          + 'left:' + (x / designW * 100) + '%;top:' + (y / designH * 100) + '%;'
          + 'width:' + (w / designW * 100) + '%;height:' + (h / designH * 100) + '%;'
          + 'z-index:' + (Number(el.z) || 0) + ';';
        var inner = '';
        if (el.type === 'rectangle') {
          var shape = p.shape || 'rounded';
          var base = Number(p.radius == null ? 10 : p.radius) || 0;
          var rtl = Number(p.radius_tl == null ? base : p.radius_tl) || 0;
          var rtr = Number(p.radius_tr == null ? base : p.radius_tr) || 0;
          var rbr = Number(p.radius_br == null ? base : p.radius_br) || 0;
          var rbl = Number(p.radius_bl == null ? base : p.radius_bl) || 0;
          var radius = shape === 'circle' ? '50%' : (shape === 'pill' ? '999px' : (rtl + 'px ' + rtr + 'px ' + rbr + 'px ' + rbl + 'px'));
          var opacity = Math.max(0, Math.min(100, Number(p.opacity == null ? 100 : p.opacity))) / 100;
          style += 'background:' + esc(p.color || '#1e293b') + ';border-radius:' + radius + ';opacity:' + opacity + ';';
          if (shape === 'line') style += 'height:6px;';
        } else if (el.type === 'image') {
          var imageBaseRadius = Number(p.radius == null ? 0 : p.radius) || 0;
          var imageRadius = (Number(p.radius_tl == null ? imageBaseRadius : p.radius_tl)||0) + 'px ' + (Number(p.radius_tr == null ? imageBaseRadius : p.radius_tr)||0) + 'px ' + (Number(p.radius_br == null ? imageBaseRadius : p.radius_br)||0) + 'px ' + (Number(p.radius_bl == null ? imageBaseRadius : p.radius_bl)||0) + 'px';
          style += 'border-radius:' + imageRadius + ';';
          inner = p.url
            ? '<img src="' + esc(p.url) + '" alt="' + esc(p.alt || '') + '" style="width:100%;height:100%;object-fit:contain;border-radius:' + imageRadius + ';">'
            : '';
        } else if (el.type === 'text') {
          var fs = Number(p.font_size) || 24;
          style += 'color:' + esc(p.color || '#fff') + ';font-size:clamp(6px, calc(' + (fs / designW * 100) + 'cqw), ' + (fs * 4) + 'px);font-weight:' + esc(p.font_weight || 700) + ';';
          inner = esc(p.text || 'Text box');
        } else if (el.type === 'variable_text' || el.type === 'variable_icon' || el.type === 'variable' || el.type === 'date') {
          var s = sample(renderEl, widgetProps, headlines);
          var fs2 = Number(p.font_size) || 26;
          style += 'color:' + esc(p.color || '#fff') + ';font-size:clamp(6px, calc(' + (fs2 / designW * 100) + 'cqw), ' + (fs2 * 4) + 'px);font-weight:800;';
          inner = s.html || esc(s.text || '');
        } else if (el.type === 'news_ticker') {
          // A dedicated element type that renders the scrolling
          // headline marquee. Sits inside a wrapper so the .news-ticker
          // CSS can handle overflow + animation.
          inner = newsTickerHTML(renderEl, widgetProps, headlines);
          style = 'position:absolute;left:' + (x / designW * 100) + '%;top:' + (y / designH * 100) + '%;width:' + (w / designW * 100) + '%;height:' + (h / designH * 100) + '%;z-index:' + (Number(el.z) || 0) + ';overflow:hidden;';
        } else {
          inner = esc(p.label || el.type || '');
        }
        out += '<div style="' + style + '">' + inner + '</div>';
      }
    });
    out += '</div>';
    return out;
  }

  // ── Boot ───────────────────────────────────────────────────
  function boot() {
    if (!presetId) {
      showError('No preset id in URL; use ?preset=<uuid>');
      setStatus('error');
      return;
    }
    // 1) Fetch the preset (the template config + feedUrl).
    cachedFetch('/api/widget-presets/' + encodeURIComponent(presetId))
      .then(function(preset) {
        var props = {};
        try { props = JSON.parse(preset.props_json || '{}'); } catch (e) { props = {}; }
        // 2) Resolve the feed URL: preset's feedUrl > fallback to ?url=
        var feedUrl = props.feedUrl || params.get('url') || '';
        if (!feedUrl) {
          // No feed URL: still render the template's static elements
          // (logos, frames, etc.) but show a "no feed" placeholder.
          document.getElementById('canvas').innerHTML = renderTemplateElements(props, []);
          setStatus('no feed URL');
          return;
        }
        // 3) Fetch the cached headlines.
        return cachedFetch('/api/news/headlines?url=' + encodeURIComponent(feedUrl))
          .then(function(headlineData) {
            var headlines = Array.isArray(headlineData.headlines) ? headlineData.headlines : [];
            document.getElementById('canvas').innerHTML = renderTemplateElements(props, headlines);
            var ts = headlineData.last_fetched_at
              ? 'last fetched ' + new Date(headlineData.last_fetched_at).toLocaleTimeString()
              : 'never fetched';
            setStatus(headlines.length + ' headlines, ' + ts);
            if (headlineData.last_error) {
              showError('Feed error: ' + headlineData.last_error);
            }
          });
      })
      .catch(function(err) {
        showError('Failed to load template: ' + (err && err.message ? err.message : err));
        setStatus('error');
      });
  }
  boot();
