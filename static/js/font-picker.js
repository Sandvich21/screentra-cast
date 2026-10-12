
    // ============= State =============
    const state = {
      catalog: [],
      installed: [],
      filter: '',
      category: '',
      picked: null,       // { family, weights: [400, 700], from: 'google'|'installed' }
      tab: 'google',
    };

    // ============= Auth token (re-use main app's auth.js pattern) =============
    function getToken() {
      try { return localStorage.getItem('signage.adminToken') || ''; } catch (e) { return ''; }
    }
    function authHeaders() {
      const t = getToken();
      const h = { 'Content-Type': 'application/json' };
      if (t) h['Authorization'] = 'Bearer ' + t;
      return h;
    }
    // For install calls we DON'T send Content-Type because we use FormData-less JSON.
    // The above is fine for JSON. For DELETE we don't need body.
    function authHeadersNoBody() {
      const t = getToken();
      const h = {};
      if (t) h['Authorization'] = 'Bearer ' + t;
      return h;
    }

    // ============= API =============
    async function fetchCatalog() {
      const r = await fetch('/api/fonts/catalog', { headers: authHeadersNoBody() });
      if (!r.ok) {
        const t = await r.text();
        throw new Error('catalog fetch failed: ' + t);
      }
      return r.json();
    }
    async function fetchInstalled() {
      const r = await fetch('/api/fonts/installed', { headers: authHeadersNoBody() });
      if (!r.ok) throw new Error('installed fetch failed: ' + r.status);
      return r.json();
    }
    async function downloadFont(family, weights) {
      const r = await fetch('/api/fonts/download', {
        method: 'POST',
        headers: authHeaders(),
        body: JSON.stringify({ family, weights }),
      });
      if (!r.ok) {
        const t = await r.text();
        throw new Error('download failed: ' + t);
      }
      return r.json();
    }
    async function deleteFont(filename) {
      const r = await fetch('/api/fonts/' + encodeURIComponent(filename), {
        method: 'DELETE',
        headers: authHeadersNoBody(),
      });
      if (!r.ok) throw new Error('delete failed: ' + r.status);
    }

    // ============= Font loading =============
    // We dynamically inject @font-face for installed fonts so the
    // preview in the picker matches what the player will render.
    const injectedFonts = new Set();
    function injectFont(family, weight, url) {
      // Defensive: skip if anything is missing. A bad url here
      // (undefined / empty / non-string) would create an
      // @font-face with `src:url('undefined')` and the browser
      // would log a request for /undefined, which is what the
      // user was seeing as "<url> undefined" in devtools.
      if (!family || !url || typeof url !== 'string') return;
      const key = family + ':' + weight;
      if (injectedFonts.has(key)) return;
      injectedFonts.add(key);
      // Single-quote the family name in the @font-face to match
      // the card preview CSS (which is itself single-quoted so
      // the CSS doesn't break the surrounding HTML attribute
      // when interpolated into `style="..."`). Both the
      // @font-face and the card reference the same family
      // name, so the browser finds the woff2 and renders in
      // the real font. (Using double-quotes in the CSS would
      // terminate the HTML attribute prematurely and the
      // browser would fall back to sans-serif.)
      const ff = family.replace(/'/g, "\\'");
      const css = "@font-face{font-family:'" + ff + "';font-weight:" + weight + ";font-style:normal;src:url('" + url + "') format('woff2');font-display:swap;}";
      const st = document.createElement('style');
      st.textContent = css;
      document.head.appendChild(st);
    }

    // ============= System font families (always available) =============
    const SYSTEM_FONTS = [
      { family: 'System Default', id: 'system-default', category: 'system', variants: ['400','700'], subsets: [], popularity: -1, version: '', isSystem: true },
      { family: 'System Sans', id: 'system-sans', category: 'system', variants: ['400','700'], subsets: [], popularity: -1, version: '', isSystem: true,
        cssValue: 'system-ui, -apple-system, "Segoe UI", Roboto, sans-serif' },
      { family: 'System Serif', id: 'system-serif', category: 'system', variants: ['400','700'], subsets: [], popularity: -1, version: '', isSystem: true,
        cssValue: 'Georgia, "Times New Roman", serif' },
      { family: 'System Monospace', id: 'system-mono', category: 'system', variants: ['400','700'], subsets: [], popularity: -1, version: '', isSystem: true,
        cssValue: '"SF Mono", Consolas, "Liberation Mono", monospace' },
    ];
    // Map id -> css family string
    function systemFontCss(id) {
      switch (id) {
        case 'system-default': return 'sans-serif';
        case 'system-sans': return SYSTEM_FONTS[1].cssValue;
        case 'system-serif': return SYSTEM_FONTS[2].cssValue;
        case 'system-mono': return SYSTEM_FONTS[3].cssValue;
        default: return id;
      }
    }

    // ============= Rendering =============
    function cardHtml(item) {
      const isInstalled = isFontInstalled(item);
      const weights = (item.variants || ['400','700'])
        .map(v => variantToWeight(v))
        .filter(w => w != null);
      const weightChips = weights.map(w =>
        '<span class="weight-chip" data-weight="' + w + '">' + w + '</span>'
      ).join('');
      const isSystem = item.isSystem === true;
      const previewStyle = isSystem
        ? 'font-family:' + systemFontCss(item.id) + ';'
        : "font-family:'" + item.family.replace(/'/g, "\\'") + "', sans-serif;";
      const meta = isSystem
        ? 'Built-in · no download needed'
        : (item.category || '') + (item.subsets && item.subsets.length ? ' · ' + item.subsets.length + ' subsets' : '');
      return `
        <div class="font-card" data-id="${esc(item.id)}" data-family="${esc(item.family)}" data-system="${isSystem ? '1' : '0'}">
          <div class="info">
            <div class="family">${esc(item.family)}</div>
            <div class="meta">${esc(meta)}</div>
            ${isSystem ? '' : '<div class="weights">' + weightChips + '</div>'}
            <div class="preview" style="${previewStyle}">The quick brown fox 123</div>
          </div>
          <div class="actions">
            ${isSystem
              ? '<button class="btn" data-action="use-system">Use</button>'
              : (isInstalled
                  ? '<button class="btn installed" disabled>Installed</button>'
                  : '<button class="btn primary" data-action="install">Install</button>')}
          </div>
        </div>`;
    }

    function esc(s) {
      return String(s == null ? '' : s)
        .replace(/&/g, '&amp;')
        .replace(/</g, '&lt;')
        .replace(/>/g, '&gt;')
        .replace(/"/g, '&quot;');
    }

    function variantToWeight(v) {
      const v0 = String(v).trim().replace(/italic$/, '').replace(/i$/, '');
      const map = { thin:100, extralight:200, light:300, regular:400, normal:400, medium:500, semibold:600, bold:700, extrabold:800, black:900 };
      if (map[v0] != null) return map[v0];
      const n = parseInt(v0, 10);
      return isNaN(n) ? null : n;
    }

    function isFontInstalled(item) {
      // Match by family name (case-insensitive). Multiple weights
      // count as one installed family for the picker UI.
      const target = (item.family || '').toLowerCase().replace(/\s+/g, '');
      return state.installed.some(f => {
        const got = (f.family || '').toLowerCase().replace(/\s+/g, '');
        return got === target;
      });
    }

    // Batch fetch of preview-status for the currently visible families.
    // The server returns a {family: woff2_url} map for the families
    // that have a cached woff2 in /uploads/google-fonts-cache/.
    // We use the returned URLs to inject @font-face before rendering
    // so the preview text renders in each family's real face.
    // Uncached families fall back to sans-serif (same as before).
    async function fetchPreviewStatus(families) {
      if (!families || families.length === 0) return {};
      // Dedupe.
      const uniq = Array.from(new Set(families));
      try {
        const r = await fetch('/api/fonts/preview-status?families=' +
          encodeURIComponent(uniq.join(',')));
        if (!r.ok) return {};
        const data = await r.json();
        return data.cached || {};
      } catch (e) {
        return {};
      }
    }

    // Inject @font-face rules for every family in the cache map.
    // Uses the same injectFont() helper that the Installed tab
    // uses, so cached previews share the dedupe Set.
    // Lazy font injection via IntersectionObserver.
    //
    // The picker used to inject @font-face for every cached family
    // in one shot, which fired ~1,955 woff2 requests the moment the
    // user opened the picker. Most of those fonts were for cards the
    // user never scrolled to, so the network traffic was wasted and
    // the picker felt slow to load.
    //
    // Now we attach @font-face only as each card scrolls into the
    // viewport (with a 400px rootMargin so the woff2 starts
    // downloading just before the card appears -- the browser has
    // time to fetch and the user never sees a sans-serif flash).
    // Cards the user never visits never get a network request.
    //
    // We do still inject the visible-card fonts immediately
    // (rootMargin: 400px above the viewport covers what's on
    // screen + a bit of padding for fast scrolls).
    let fontInjectionObserver = null;

    function setupLazyFontInjection(pane, cachedMap) {
      // The picker used to inject @font-face pointing at our local
      // cache (/uploads/google-fonts-cache/*.woff2). That meant the
      // picker could only preview families the server had already
      // downloaded -- on a fresh install the user saw sans-serif
      // until the cache loop caught up.
      //
      // Now we point @font-face at Google Fonts directly. Each card
      // gets a <link rel="stylesheet" href="https://fonts.googleapis.com/css2?family=...">
      // injected into the document head when it scrolls into view.
      // The browser fetches the CSS, parses out the woff2 URLs
      // (one per subset), and downloads the latin subset for the
      // preview text. Cards the user never scrolls to never fire
      // a request.
      //
      // CSP allows fonts.gstatic.com (woff2) and fonts.googleapis.com
      // (CSS) so this works without touching the server.
      if (fontInjectionObserver) {
        fontInjectionObserver.disconnect();
        fontInjectionObserver = null;
      }
      // Strip any <link> tags left over from a prior render so the
      // document doesn't accumulate hundreds of CSS imports.
      document.querySelectorAll('link[data-google-font]').forEach(el => el.remove());

      const cards = pane.querySelectorAll('.font-card[data-family]');
      if (!cards.length) return;
      // Build a map from data-family (display name) -> the catalog
      // entry, so the observer can read the properly-cased family
      // name + variants for the Google URL.
      const byDisplayName = {};
      for (const entry of state.catalog) {
        byDisplayName[entry.family] = entry;
      }
      // No-op browsers: if IntersectionObserver is missing (very old
      // browser), fall back to injecting everything. Same behavior as
      // before.
      if (typeof IntersectionObserver === 'undefined') {
        cards.forEach(card => injectGoogleFontLink(card, byDisplayName));
        return;
      }
      // Dedupe so we don't attach the same <link> twice for a
      // family that happens to appear in multiple cards (e.g. if
      // two filters match the same family).
      const injectedFamilies = new Set();
      function attachLink(card) {
        if (!card || injectedFamilies.has(card.dataset.family)) return;
        injectedFamilies.add(card.dataset.family);
        injectGoogleFontLink(card, byDisplayName);
      }
      fontInjectionObserver = new IntersectionObserver((entries) => {
        entries.forEach(entry => {
          if (!entry.isIntersecting) return;
          attachLink(entry.target);
          // We can unobserve -- once a <link> is in the DOM the
          // browser caches the CSS for the session and the font
          // is registered. Re-attaching on later scrolls is just
          // extra work.
          fontInjectionObserver.unobserve(entry.target);
        });
      }, {
        // 400px above/below the viewport so a font starts loading
        // before its card scrolls into view. The picker is a
        // vertical list so 400px vertical is enough.
        rootMargin: '400px 0px',
        threshold: 0,
      });
      cards.forEach(card => fontInjectionObserver.observe(card));
      if (window.fontPickerDebug) {
        console.log('[font-picker] lazy google font observer attached to ' + cards.length + ' cards');
      }
    }

    // Build the Google Fonts CSS URL for one family and attach it
    // as a <link rel="stylesheet"> in the document head. Google
    // returns the CSS as if you had called
    // fonts.googleapis.com/css2?family=Roboto:wght@400&display=swap
    // -- the browser parses it, registers @font-face, and fetches
    // the latin subset for the visible card.
    //
    // We pick the variant closest to 400 (same logic the server uses
    // for the comprehensive cache). For italic-only families the
    // picker shows the font in italic style (better than nothing).
    function injectGoogleFontLink(card, byDisplayName) {
      const displayName = card.dataset.family;
      const entry = byDisplayName[displayName];
      if (!entry) return;  // system fonts etc.
      const variants = entry.variants || [];
      const variantPair = pickGoogleVariant(variants);
      const style = variantPair[0];
      const weight = variantPair[1];
      const url = 'https://fonts.googleapis.com/css2?family=' +
        encodeURIComponent(displayName).replace(/%20/g, '+') +
        (style === 'italic' ? ':ital,wght@1,' : ':wght@') + weight +
        '&display=swap';
      const link = document.createElement('link');
      link.rel = 'stylesheet';
      link.href = url;
      link.dataset.googleFont = '1';
      document.head.appendChild(link);
    }

    // Pick the (style, weight) closest to (normal, 400) from a
    // family's variant list. Same algorithm as the Rust
    // pick_preview_style_and_weight() on the server, kept in sync
    // because both run independently.
    function pickGoogleVariant(variants) {
      const target = 400;
      // Parse each variant into (is_italic, weight).
      const pairs = [];
      for (const v of variants) {
        const lower = String(v).toLowerCase();
        const isItalic = lower.endsWith('italic') || lower.endsWith('i');
        const base = lower.replace(/italic$/, '').replace(/i$/, '');
        const namedWeights = {
          thin: 100, extralight: 200, light: 300,
          regular: 400, normal: 400, '': 400,
          medium: 500, semibold: 600, bold: 700,
          extrabold: 800, black: 900,
        };
        let w = namedWeights[base];
        if (w == null) {
          const n = parseInt(base, 10);
          if (!isNaN(n)) w = n;
        }
        if (w != null) pairs.push([isItalic, w]);
      }
      if (pairs.length === 0) return ['normal', 400];
      // Prefer normal (false) over italic, then closest to 400,
      // then higher weight on ties.
      pairs.sort((a, b) => {
        const ai = a[0] ? 1 : 0;
        const bi = b[0] ? 1 : 0;
        if (ai !== bi) return ai - bi;
        const ad = Math.abs(a[1] - target);
        const bd = Math.abs(b[1] - target);
        if (ad !== bd) return ad - bd;
        return b[1] - a[1];
      });
      return [pairs[0][0] ? 'italic' : 'normal', pairs[0][1]];
    }

    
    // Set this from the devtools console to enable verbose logging:
    //   window.fontPickerDebug = true; location.reload();
    // The default is off so end users don't see noise in their console.

    // Show a small progress banner above the Google Fonts list while
    // the server-side cache is still filling. The banner hides
    // itself once every family the user can see is cached.
    function updatePreviewBanner(cachedMap, visibleFamilies) {
      const total = visibleFamilies.length;
      if (total === 0) {
        const old = document.getElementById('preview-banner');
        if (old) old.remove();
        return;
      }
      const cached = visibleFamilies.filter(f => cachedMap[f]).length;
      let banner = document.getElementById('preview-banner');
      if (cached === total) {
        if (banner) banner.remove();
        return;
      }
      if (!banner) {
        banner = document.createElement('div');
        banner.id = 'preview-banner';
        banner.className = 'preview-banner';
        const pane = document.getElementById('google-pane');
        if (pane && pane.parentNode) pane.parentNode.insertBefore(banner, pane);
      }
      const pct = Math.round(cached * 100 / total);
      banner.innerHTML = 'Caching previews: ' + cached + ' / ' + total +
        ' fonts ready (' + pct + '%) - uncached previews use the default font.';
    }

    async function renderGooglePane() {
      const pane = document.getElementById('google-pane');
      const q = state.filter.toLowerCase().trim();
      const cat = state.category;
      const list = SYSTEM_FONTS.concat(state.catalog).filter(item => {
        if (q && !item.family.toLowerCase().includes(q)) return false;
        if (cat && (item.category || '') !== cat) return false;
        return true;
      });
      // Sort: system first, then by popularity (low number = more popular).
      list.sort((a, b) => {
        if (a.isSystem && !b.isSystem) return -1;
        if (!a.isSystem && b.isSystem) return 1;
        return (a.popularity || 9999) - (b.popularity || 9999);
      });
      if (list.length === 0) {
        pane.innerHTML = '<div class="empty">No fonts match your search.</div>';
        return;
      }
      // Render cards first (instant). Then use IntersectionObserver
      // to inject Google Fonts <link rel="stylesheet"> tags as
      // each card scrolls into view. The browser handles fetching
      // the CSS, parsing subset URLs, and downloading the latin
      // woff2 -- the picker doesn't need to track anything itself.
      pane.innerHTML = list.map(cardHtml).join('');
      setupLazyFontInjection(pane, {});
      // Force a reflow once the fonts have loaded so cards whose
      // previews were rendered before their @font-face was injected
      // repaint in the real face. Without this, the browser's
      // "font-display: swap" timing can leave cards in fallback
      // (sans-serif) until something else triggers a repaint.
      // We only touch cards that are currently in view (the
      // observer injects the rest as the user scrolls), so this
      // loop is bounded by the viewport size, not the catalog.
      if (document.fonts && document.fonts.ready) {
        document.fonts.ready.then(() => {
          // Touch only the .preview elements that are currently
          // visible. Walking all 1,955 of them on every render
          // is wasteful now that font injection is lazy.
          const cards = pane.querySelectorAll('.font-card[data-family]');
          for (const card of cards) {
            const preview = card.querySelector('.preview');
            if (!preview) continue;
            const r = preview.getBoundingClientRect();
            // Skip cards that are entirely outside the viewport.
            if (r.bottom < 0 || r.top > window.innerHeight) continue;
            // Reading offsetWidth forces a reflow / repaint.
            void preview.offsetWidth;
          }
          // Some cached woff2 files (e.g. Exile) are rejected by
          // the browser's font sanitizer ("empty gid used as
          // component"). The font is in the document's font set
          // but check() returns false because the @font-face
          // never matched. Mark those cards with a `.broken`
          // class so the user can see "this preview is broken
          // upstream, not by us" instead of wondering why every
          // card shows the same sans-serif face.
          if (document.fonts.check) {
            // Only check fonts that have actually been injected
            // (visible cards). For lazy-injected fonts the
            // @font-face hasn't been added yet so check() always
            // returns false -- we'd mark every card as broken.
            const cards = pane.querySelectorAll('.font-card[data-family]');
            for (const card of cards) {
              const r = card.getBoundingClientRect();
              if (r.bottom < 0 || r.top > window.innerHeight) continue;
              const family = card.dataset.family;
              const ok = document.fonts.check('400 12px "' + family.replace(/"/g, '\\"') + '"');
              if (ok) continue;
              card.classList.add('broken');
              const meta = card.querySelector('.meta');
              if (meta && !meta.dataset.originalMeta) {
                meta.dataset.originalMeta = meta.textContent;
                meta.textContent = 'Font preview unavailable (upstream issue)';
                meta.classList.add('meta-broken');
              }
            }
          }
        });
      }
    }

    // Render one card per installed weight. The Installed tab used
    // to collapse all weights of a family into a single card with a
    // single "Delete" button (which nuked the whole family). That
    // turned out to be too coarse: a user who only wants to drop
    // 700 to save space shouldn't have to reinstall 400 + 200 + 300.
    // Each card is one .woff2 file with its own Use + Delete buttons.
    // A small "Delete all" link in the family header removes every
    // weight of that family in one click for users who do want that.
    function renderInstalledPane() {
      const pane = document.getElementById('installed-pane');
      if (!state.installed || state.installed.length === 0) {
        pane.innerHTML = '<div class="empty">No fonts installed yet. Install one from the Google Fonts tab.</div>';
        return;
      }
      // Group by family for the header, but render one card per weight.
      const byFamily = {};
      state.installed.forEach(f => {
        if (!byFamily[f.family]) byFamily[f.family] = [];
        byFamily[f.family].push(f);
      });
      const families = Object.keys(byFamily).sort();
      pane.innerHTML = families.map(family => {
        const files = byFamily[family].slice().sort((a, b) => a.weight - b.weight);
        const header = '<div class="family-header">' +
          '<span>' + esc(family) + ' &middot; ' + files.length + ' weight' + (files.length === 1 ? '' : 's') + '</span>' +
          (files.length > 1
            ? '<button class="delete-family" data-action="delete-family" data-family="' + esc(family) + '">Delete all</button>'
            : '') +
          '</div>';
        const cards = '<div class="weight-cards">' +
          files.map(installedWeightCardHtml).join('') +
          '</div>';
        return header + cards;
      }).join('');
    }

    // One card = one installed .woff2 file. The card shows the family
    // name + the weight (e.g. "Roboto  400") and offers Use + Delete.
    // Delete is a single-click action with no confirm dialog (per the
    // editor's "no confirm" convention for destructive widget actions).
    // The Delete button is also disabled while the request is in flight
    // so a double-click doesn't fire two DELETE requests.
    function installedWeightCardHtml(file) {
      const previewStyle = "font-family:'" + esc(file.family.replace(/'/g, "\\'")) + "', sans-serif;";
      const sizeKb = (file.size_bytes / 1024).toFixed(0);
      return '<div class="font-card" data-filename="' + esc(file.filename) + '" data-family="' + esc(file.family) + '" data-weight="' + file.weight + '">' +
        '<div class="info">' +
          '<div class="family">' + esc(file.family) +
            ' <span class="weight-label">weight ' + file.weight + '</span>' +
          '</div>' +
          '<div class="meta">' + sizeKb + ' KB &middot; ' + esc(file.filename) + '</div>' +
          '<div class="preview" style="' + previewStyle + '">The quick brown fox 123</div>' +
        '</div>' +
        '<div class="actions">' +
          '<button class="btn" data-action="use-installed">Use</button>' +
          '<button class="btn danger" data-action="delete-file">Delete</button>' +
        '</div>' +
      '</div>';
    }

    function updateCounts() {
      document.getElementById('google-count').textContent = SYSTEM_FONTS.length + state.catalog.length;
      document.getElementById('installed-count').textContent = state.installed.length;
    }

    // ============= Event wiring =============
    document.getElementById('search').addEventListener('input', e => {
      state.filter = e.target.value;
      renderGooglePane();
    });
    document.getElementById('category-filter').addEventListener('change', e => {
      state.category = e.target.value;
      renderGooglePane();
    });
    document.querySelectorAll('.tab').forEach(t => {
      t.addEventListener('click', () => {
        document.querySelectorAll('.tab').forEach(x => x.classList.remove('active'));
        t.classList.add('active');
        state.tab = t.dataset.tab;
        document.getElementById('google-pane').style.display = state.tab === 'google' ? '' : 'none';
        document.getElementById('installed-pane').style.display = state.tab === 'installed' ? '' : 'none';
        if (state.tab === 'installed') renderInstalledPane();
      });
    });

    // Delegated click handler on the font list. Picks up install /
    // delete / use buttons and weight chips.
    document.querySelector('.main').addEventListener('click', async e => {
      const btn = e.target.closest('button');
      if (btn) {
        const card = btn.closest('.font-card');
        const family = card.dataset.family;
        const isSystem = card.dataset.system === '1';
        const action = btn.dataset.action;
        if (action === 'install') {
          // Collect selected weights; default to [400, 700] if none.
          const selWeights = Array.from(card.querySelectorAll('.weight-chip.selected'))
            .map(c => parseInt(c.dataset.weight, 10))
            .filter(n => !isNaN(n));
          const weights = selWeights.length ? selWeights : [400, 700];
          await installFont(family, weights, card, btn);
          return;
        }
        if (action === 'use-system' || action === 'use-installed') {
          pickFont(family, isSystem);
          return;
        }
        if (action === 'delete-file') {
          // The button is inside a .font-card that has data-filename
          // (per-weight cards) and data-family. card.family on the
          // delegated handler is read from the data-family attribute,
          // which we set on the card. For per-weight cards we want
          // the filename, not the family, so we read it from the
          // closest card.
          const wc = btn.closest('.font-card');
          const filename = wc && wc.dataset.filename;
          if (filename) await deleteWeight(filename, btn);
          return;
        }
        if (action === 'delete-family') {
          // The button is the small "Delete all" link in the family
          // header. It has its own data-family attribute.
          const f = btn.dataset.family;
          if (f) await deleteFamily(f);
          return;
        }
        return;
      }
      const chip = e.target.closest('.weight-chip');
      if (chip) {
        chip.classList.toggle('selected');
        return;
      }
    });

    // ============= Actions =============
    async function installFont(family, weights, card, btn) {
      btn.disabled = true;
      btn.textContent = 'Installing...';
      try {
        const id = card.dataset.id;
        const installed = await downloadFont(id, weights);
        // Inject @font-face for the freshly installed weights so
        // the preview updates immediately.
        installed.forEach(f => { if (f && f.url) injectFont(f.family, f.weight, f.url); });
        // Refresh the installed list.
        state.installed = await fetchInstalled();
        updateCounts();
        // Re-render the active pane.
        if (state.tab === 'google') renderGooglePane(); else renderInstalledPane();
        // Auto-pick the just-installed font.
        pickFont(family, false);
      } catch (err) {
        btn.disabled = false;
        btn.textContent = 'Install';
        showError('Install failed: ' + err.message);
      }
    }

    // Delete a single installed weight. The button is disabled
    // while the request is in flight to prevent double-clicks.
    async function deleteWeight(filename, btn) {
      if (btn) {
        btn.disabled = true;
        btn.textContent = '...';
      }
      try {
        await deleteFont(filename);
        state.installed = await fetchInstalled();
        updateCounts();
        if (state.tab === 'installed') renderInstalledPane();
      } catch (err) {
        showError('Delete failed: ' + err.message);
        if (btn) {
          btn.disabled = false;
          btn.textContent = 'Delete';
        }
      }
    }

    // Delete every installed weight of a family. No confirm() dialog:
    // the per-weight cards right below this button give the user a
    // visual reminder of what they're about to remove, and they can
    // use "Undo" by re-installing from the Google Fonts tab.
    async function deleteFamily(family) {
      const files = state.installed.filter(f => f.family === family);
      try {
        for (const f of files) {
          await deleteFont(f.filename);
        }
        state.installed = await fetchInstalled();
        updateCounts();
        if (state.tab === 'installed') renderInstalledPane();
      } catch (err) {
        showError('Delete failed: ' + err.message);
      }
    }

    function pickFont(family, isSystem) {
      // Build the css font-family string the editor will store.
      let cssValue;
      if (isSystem) {
        // family here is the system id
        cssValue = systemFontCss(family);
      } else {
        // For installed fonts, the cssValue is just the family name
        // (no quotes). The runtime injects @font-face for all
        // installed weights at startup, so the browser will find it.
        cssValue = '"' + family.replace(/"/g, '\\"') + '", sans-serif';
      }
      state.picked = { family, isSystem, cssValue };
      const label = document.getElementById('picked-label');
      label.innerHTML = '<strong>' + esc(family) + '</strong> &mdash; ' + esc(cssValue);
      document.getElementById('use-btn').disabled = false;
    }

    document.getElementById('use-btn').addEventListener('click', () => {
      if (!state.picked) return;
      // Post message to parent so the editor can grab the value.
      try {
        window.parent.postMessage({
          type: 'font-picked',
          family: state.picked.family,
          cssValue: state.picked.cssValue,
        }, '*');
      } catch (e) {}
    });

    function showError(msg) {
      // Replace google-pane with an error message (but only briefly,
      // don't permanently break the UI).
      const main = document.querySelector('.main');
      const old = main.innerHTML;
      const err = document.createElement('div');
      err.className = 'error';
      err.textContent = msg;
      main.prepend(err);
      setTimeout(() => err.remove(), 5000);
    }

    // ============= Init =============
    async function init() {
      // Pre-fill any font-family hint passed in the URL so the
      // picker can show "Currently using: ..." if the parent wants.
      const params = new URLSearchParams(window.location.search);
      const current = params.get('current');
      if (current) {
        const label = document.getElementById('picked-label');
        label.innerHTML = 'Currently using: <strong>' + esc(current) + '</strong>';
      }

      // Load installed first (always cheap, drives initial state).
      try {
        state.installed = await fetchInstalled();
        // Inject @font-face for every installed file so the installed
        // tab can preview them with the right font.
        state.installed.forEach(f => { if (f && f.url) injectFont(f.family, f.weight, f.url); });
      } catch (e) {
        showError('Could not load installed fonts: ' + e.message);
      }
      updateCounts();
      renderInstalledPane();

      // Catalog in the background; show placeholder while loading.
      const pane = document.getElementById('google-pane');
      pane.innerHTML = '<div class="loading">Loading Google Fonts catalog…</div>';
      try {
        state.catalog = await fetchCatalog();
        renderGooglePane();
        updateCounts();
      } catch (e) {
        pane.innerHTML = '<div class="error">Could not load Google Fonts: ' + esc(e.message) + '<br><br>The font picker needs network access to gwfh.mranftl.com. Existing installed fonts are still usable in the "Installed" tab.</div>';
      }
    }
    init();
  