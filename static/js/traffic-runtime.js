(function (root) {
  'use strict';

  var STORAGE_KEY = 'screentra-cast.traffic-closure-tracking.v1';
  var DAY_MS = 24 * 60 * 60 * 1000;

  function readState() {
    try {
      var value = root.localStorage && root.localStorage.getItem(STORAGE_KEY);
      var parsed = value ? JSON.parse(value) : {};
      return parsed && typeof parsed === 'object' && !Array.isArray(parsed) ? parsed : {};
    } catch (_) {
      return {};
    }
  }

  function writeState(state) {
    try {
      if (root.localStorage) root.localStorage.setItem(STORAGE_KEY, JSON.stringify(state));
    } catch (_) { /* Private browsing or a full storage quota must not break traffic. */ }
  }

  function eventKey(event) {
    var id = String(event && event.id || '').trim();
    if (id) return 'id:' + id;
    var fields = [
      event && event.category,
      event && event.event_type,
      event && event.road,
      event && event.direction,
      event && event.description,
      event && event.latitude,
      event && event.longitude
    ].map(function (value) { return String(value == null ? '' : value).trim().toLowerCase(); });
    var fallback = fields.join('|');
    return fallback.replace(/^\|+|\|+$/g, '') ? 'fallback:' + fallback : '';
  }

  function isClosure(event) {
    var category = String(event && event.category || '').trim().toLowerCase();
    var type = String(event && event.event_type || '').trim().toLowerCase();
    var description = String(event && event.description || '').trim().toLowerCase();
    var laneStatus = String(event && event.lane_status || '').trim().toLowerCase();
    // The public WZDx fallback labels lane/shoulder closures as
    // category=other and event_type=work-zone. Treat the explicit closure
    // wording as a closure for manual blocking and long-term aging without
    // changing the provider's normalized category used by existing widgets.
    return category === 'closure' || type.indexOf('closure') >= 0 || /\b(?:closed|closure|closing)\b/.test(description + ' ' + laneStatus);
  }

  root.ripleyTrafficEventKey = eventKey;
  root.ripleyTrafficIsClosure = isClosure;

  // Long-term aging and manual blocking are client-local display rules. A
  // closure starts aging when this display first observes it. Manual blocks
  // are pruned as soon as the feed stops reporting the closure, so a later
  // re-closure is visible again and can be blocked again deliberately.
  root.ripleyFilterTrafficEvents = function (events, props, widgetId) {
    var list = Array.isArray(events) ? events : [];
    var options = props || {};
    var scope = String(widgetId || options._traffic_widget_id || 'traffic-widget');
    var state = readState();
    var manualBlocking = options.manual_closure_blocking === true;
    var activeClosures = {};
    list.forEach(function (event) {
      if (!isClosure(event)) return;
      var key = eventKey(event);
      if (key) activeClosures[key] = true;
    });

    var blockedRecords = Array.isArray(options.blocked_closures) ? options.blocked_closures : [];
    if (manualBlocking) {
      var prunedBlocks = blockedRecords.filter(function (record) {
        var key = String(record && record.key || eventKey(record) || '');
        return key && activeClosures[key];
      });
      if (prunedBlocks.length !== blockedRecords.length) options.blocked_closures = prunedBlocks;
      blockedRecords = prunedBlocks;
    }
    var blocked = {};
    if (manualBlocking) blockedRecords.forEach(function (record) {
      var key = String(record && record.key || eventKey(record) || '');
      if (key) blocked[key] = true;
    });

    if (options.hide_long_term_closures !== true) {
      if (state[scope]) {
        delete state[scope];
        writeState(state);
      }
      return list.filter(function (event) { return !isClosure(event) || !blocked[eventKey(event)]; });
    }

    var days = Math.max(1, Math.min(3650, Number(options.long_term_closure_days) || 5));
    var cutoff = days * DAY_MS;
    var now = Date.now();
    var tracked = state[scope] && typeof state[scope] === 'object' ? state[scope] : {};
    var visible = [];

    list.forEach(function (event) {
      if (!isClosure(event)) {
        visible.push(event);
        return;
      }
      var key = eventKey(event);
      if (!key) {
        visible.push(event);
        return;
      }
      if (blocked[key]) return;
      var firstSeen = Number(tracked[key]);
      if (!Number.isFinite(firstSeen) || firstSeen > now) firstSeen = now;
      tracked[key] = firstSeen;
      if (now - firstSeen < cutoff) visible.push(event);
    });

    Object.keys(tracked).forEach(function (key) {
      if (!activeClosures[key]) delete tracked[key];
    });
    state[scope] = tracked;
    writeState(state);
    return visible;
  };
})(typeof window !== 'undefined' ? window : globalThis);
