    // Bottom-left diagnostic: shows JS state so silent failures are visible.
    const __dbg = (m) => { const d = document.getElementById("__dbg"); if (d) d.textContent = m + " " + new Date().toISOString().slice(11,19); };
    window.addEventListener("error", e => __dbg("err " + (e.message || e)));
    window.addEventListener("unhandledrejection", e => __dbg("rej " + (e.reason && e.reason.message || e.reason || e)));
    const $ = (id) => document.getElementById(id);
    let currentDevices = [];
    let selectedPath = null;

    async function refreshStatus() {
      try {
        const r = await fetch('/api/status.json', { cache: 'no-store' });
        if (!r.ok) return;
        const s = await r.json();
        if (s.display_index != null) document.title = 'Screentra Cast Player display ' + s.display_index;
        $('serverUrl').textContent = s.server_url || '—';
        $('hostname').textContent  = s.hostname || '—';
        $('clientId').textContent  = s.client_id || '—';
        const adminUrl = s.admin_url || (s.lan_ip ? ('http://' + s.lan_ip + ':3910/') : 'http://11.1.1.54:3910/');
        const adminUrlEl = $('adminUrl');
        if (adminUrlEl) adminUrlEl.textContent = adminUrl;
        $('lastSeen').textContent  = 'Last seen ' + (s.last_seen || '—');
        $('serverLatency').textContent = (s.server_latency_ms != null ? s.server_latency_ms + ' ms' : '— ms');
        $('displayIdx').textContent = 'idx ' + (s.display_index != null ? s.display_index : '—');
        $('displayInfo').textContent =
          (s.display_label || '—') +
          (s.display_resolution ? ' · ' + s.display_resolution : '') +
          (s.display_connector ? ' · ' + s.display_connector : '');
        $('role').textContent = s.role || 'Digital Signage Player';
        $('assignedLayout').textContent = s.assigned_layout_id
          ? 'Layout ' + s.assigned_layout_id.slice(0,8) + '…'
          : 'No layout assigned';

        const pill = $('statusPill');
        if (s.connection === 'online') { pill.textContent='CONNECTED'; pill.className='pill ok'; }
        else if (s.connection === 'stale') { pill.textContent='STALE'; pill.className='pill warn'; }
        else { pill.textContent='OFFLINE'; pill.className='pill err'; }

        const tl = $('touchline');
        if (s.touch_device) {
          tl.innerHTML = 'Touchscreen <strong>'+ s.touch_device +'</strong>' +
                         (s.touch_path ? ' <span style="color:var(--muted)">('+ s.touch_path +')</span>' : '') +
                         ' · <a style="color:var(--accent)" id="cfgLink" href="#">configure</a>';
          $('cfgLink').addEventListener('click', openModal);
        } else {
          tl.innerHTML = 'Touchscreen <strong>not detected</strong> · ' +
                         '<a style="color:var(--accent)" id="cfgLink" href="#">configure</a>';
          $('cfgLink').addEventListener('click', openModal);
        }

        $('foot').textContent = 'Screentra Cast · build ' + (s.build || '?') +
                                ' · display ' + (s.display_index != null ? s.display_index : '?') +
                                ' · port ' + (s.port || '?');
        __dbg('JS ok cid=' + (s.client_id || '-').slice(0, 12));
      } catch (e) {
        __dbg('JS err ' + (e && e.message || e));
      }
    }

    async function openModal() {
      $('modal').classList.add('on');
      await loadDevices();
    }
    function closeModal() { $('modal').classList.remove('on'); }

    async function loadDevices() {
      const list = $('deviceList');
      list.textContent = 'Detecting…';
      try {
        const r = await fetch('/api/touchscreen.json', { cache: 'no-store' });
        const data = await r.json();
        currentDevices = data.devices || [];
        selectedPath = data.selected || null;
        if (currentDevices.length === 0) {
          list.innerHTML = '<div class="sub">No input devices detected. Try Rescan.</div>';
          return;
        }
        list.innerHTML = '';
        for (const d of currentDevices) {
          const el = document.createElement('div');
          el.className = 'device' + ((selectedPath && d.path === selectedPath) ? ' selected' : (d.recommended ? ' selected' : ''));
          el.innerHTML = '<div><div class="name">' + d.name + '</div>' +
                         '<div class="path">' + d.path + (d.type ? ' · ' + d.type : '') + '</div></div>' +
                         '<div class="pick"><span class="pill ' + (d.recommended ? 'ok' : '') + '">' +
                         (d.recommended ? 'RECOMMENDED' : 'USE') + '</span></div>';
          el.addEventListener('click', () => {
            selectedPath = d.path;
            for (const c of list.children) c.classList.remove('selected');
            el.classList.add('selected');
          });
          list.appendChild(el);
          if (d.recommended && !selectedPath) selectedPath = d.path;
        }
      } catch (e) {
        list.innerHTML = '<div class="sub">Detection failed: ' + e + '</div>';
      }
    }

    async function saveSelection() {
      try {
        const r = await fetch('/api/touchscreen/select', {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ path: selectedPath }),
        });
        if (r.ok) {
          closeModal();
          refreshStatus();
        } else {
          alert('Save failed: ' + r.status);
        }
      } catch (e) {
        alert('Save failed: ' + e);
      }
    }

    // Touch test pad
    $('testpad').addEventListener('pointerdown', (ev) => {
      ev.preventDefault();
      const r = ev.target.getBoundingClientRect();
      const x = Math.round((ev.clientX - r.left) / r.width  * 100);
      const y = Math.round((ev.clientY - r.top ) / r.height * 100);
      ev.target.textContent = 'Tapped at ' + x + '%, ' + y + '%';
      ev.target.style.background = 'rgba(34,197,94,.18)';
      setTimeout(() => {
        ev.target.style.background = '';
        ev.target.textContent = 'Tap here to test';
      }, 1200);
    });

    document.addEventListener('DOMContentLoaded', () => {
      $('gear').addEventListener('click', openModal);
      $('cancelBtn').addEventListener('click', closeModal);
      $('rescanBtn').addEventListener('click', loadDevices);
      $('saveBtn').addEventListener('click', saveSelection);
    });
    const __calBtn = document.getElementById('calibrateBtn');
    if (__calBtn) __calBtn.addEventListener('click', () => {
      // Open the calibration page in the same tab. The renderer serves
      // /touch-calibration.html which posts back to /api/touch-calibration.json.
      window.location.href = '/touch-calibration.html';
    });
    const __calGear = document.getElementById('calGear');
    if (__calGear) {
      __calGear.addEventListener('click', () => {
        if (confirm('Run touchscreen calibration test? You\'ll tap 4 corners, a grid, edge bars, then drag a circle.')) {
          window.location.href = '/touch-calibration.html';
        }
      });
    }
    // Wire the gear icon to open the touchscreen configuration modal.
    // Without this, tapping the gear icon (top-right, SVG) does nothing
    // because the cfgLink anchor is inside the touchline, not the gear.
    const __gear = document.getElementById('gear');
    if (__gear) __gear.addEventListener('click', openModal);

    refreshStatus();



    async function checkAssignedLayoutAndReload() {
      try {
        const r = await fetch('/assigned-layout.json?ts=' + Date.now(), { cache: 'no-store' });
        if (r.ok) {
          const txt = await r.text();
          if (txt && txt.trim() && txt.trim() !== '{}') {
            window.location.reload();
          }
        }
      } catch (e) {}
    }
    setInterval(refreshStatus, 4000);
    setInterval(checkAssignedLayoutAndReload, 5000);
    setTimeout(checkAssignedLayoutAndReload, 1200);

    // === Cursor auto-hide (3.5s default, configurable via /api/cursor-config.json) ===
    let __cursorIdle = Date.now();
    let __cursorHideSecs = 3.5;
    const __cursorShow = () => { __cursorIdle = Date.now(); document.documentElement.style.cursor = 'default'; };
    ['pointermove','pointerdown','pointerup','mousemove','mousedown','mouseup','touchstart','touchmove','wheel','keydown','scroll'].forEach(ev =>
      window.addEventListener(ev, __cursorShow, { capture: true, passive: true }));
    async function __cursorLoadConfig() {
      try {
        const r = await fetch('/api/cursor-config.json', { cache: 'no-store' });
        if (r.ok) { const c = await r.json(); __cursorHideSecs = Number(c.hide_secs) || 3.5; }
      } catch (e) {}
    }
    setInterval(() => {
      if (Date.now() - __cursorIdle > __cursorHideSecs * 1000) {
        document.documentElement.style.cursor = 'none';
      }
    }, 200);
    __cursorLoadConfig();

    // === Watchdog: chrome tab/renderer-process crashes silently on this
    // labwc/wayland box and leaves the page black while still capturing
    // touch events. Force a full page reload every 5 min as a safety net
    // so the user recovers without manual intervention.
    setTimeout(() => { try { window.location.reload(); } catch (_e) {} }, 5 * 60 * 1000);
  