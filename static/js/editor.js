// Unified Editor: all layouts in one UI.
// A layout with `interactive: true` collects widget submissions on the player runtime.
// The editor is identical for both — the flag is the only difference.

const Editor = {
  // State
  layouts: [],
  layoutSets: [],
  folders: [],
  browserFolderId: null,
  browserSearch: '',
  currentLayoutId: null,
  selectedWidgetId: null,
  presetCache: {},          // widget_type -> [{id, name, props_json, is_builtin}]
  addonWidgetTypes: [],      // enabled declarative addon registrations
  presetCacheAt: 0,
  dragState: null,
  resizeState: null,
  rotationState: null,

  // Simple widget types: use hardcoded defaults from makeLayoutWidget().
  // Widget Templates (presets) are not allowed for these — the API rejects
  // them and the editor ignores any preset rows that may have been written
  // before this restriction was added. Keep in sync with RESTRICTED_PRESET_TYPES
  // in src/api.rs.
  // Simple widget types: use hardcoded defaults from makeLayoutWidget().
  // Widget Templates (presets) are not allowed for these - the API rejects
  // them and the editor ignores any preset rows that may have been written
  // before this restriction was added. Keep in sync with
  // RESTRICTED_PRESET_TYPES in src/api.rs.
  //
  // Note: "embed" is NOT in this list. The editor used to expose the
  // Embed widget under the type "embed" but persists it to the server
  // as "webpage" (see renderLayoutWidgetProps: apiType = w.type === 'embed' ? 'webpage' : w.type).
  // Templates therefore use "webpage" as their widget_type, and the
  // Widget Templates editor's dropdown offers "webpage" (not "embed").
  // Likewise "news" is absent because the server allows it as a
  // template target (the runtime can now render it).
  RESTRICTED_PRESET_TYPES: new Set([
    'text', 'image', 'video', 'clock', 'date', 'datetime',
    'countdown',
  ]),
  paneMode: null,  // restored from localStorage in init()
  _savePromise: Promise.resolve(),
  _publishBusy: false,
  _fetchEpoch: 0,

  init() {
    this.cacheEls();
    if (!this.el.page) return;
    // Restore the previously selected Properties-panel pane from localStorage.
    try {
      const saved = localStorage.getItem('signage.editorPaneMode');
      if (saved) this.paneMode = saved;
    } catch (e) {}

    this.bind();

    // Snap & Grid options — runs after bind() so Editor._tplSnapGrid is initialized
    (() => {
        const btn = document.getElementById('btn-snap-options');
        const panel = document.getElementById('snap-options-panel');
        if (!btn || !panel) return;

        let snapSettings = { enabled: true, visible: true, size: 10, snap_size: 10 };
        try { snapSettings = Object.assign(snapSettings, JSON.parse(localStorage.getItem('signage.snapSettings') || 'null')); } catch(e) {}

        if (window.Editor && !Editor._tplSnapGrid) {
            Editor._tplSnapGrid = Object.assign({ enabled: true, visible: true, size: 10, snap_size: 10 }, snapSettings);
        }

        const snapEnabled = document.getElementById('snap-enabled');
        const snapVisible = document.getElementById('snap-visible');
        const snapGridSize = document.getElementById('snap-grid-size');
        const snapSnapSize = document.getElementById('snap-snap-size');
        if (snapEnabled) snapEnabled.checked = snapSettings.enabled !== false;
        if (snapVisible) snapVisible.checked = snapSettings.visible !== false;
        if (snapGridSize) snapGridSize.value = snapSettings.size || 10;
        if (snapSnapSize) snapSnapSize.value = snapSettings.snap_size || snapSettings.size || 10;

        const applySettings = () => {
            const enabled = snapEnabled ? snapEnabled.checked : true;
            const visible = snapVisible ? snapVisible.checked : true;
            const size = Math.max(2, Math.min(100, parseInt(snapGridSize?.value) || 10));
            const snap_size = Math.max(1, Math.min(100, parseInt(snapSnapSize?.value) || size));
            const settings = { enabled, visible, size, snap_size };
            try { localStorage.setItem('signage.snapSettings', JSON.stringify(settings)); } catch(e) {}
            if (window.Editor && Editor._tplSnapGrid) {
                Object.assign(Editor._tplSnapGrid, settings);
                Editor.renderCanvas();
            }
        };

        if (snapEnabled) snapEnabled.addEventListener('change', applySettings);
        if (snapVisible) snapVisible.addEventListener('change', applySettings);
        if (snapGridSize) snapGridSize.addEventListener('input', applySettings);
        if (snapSnapSize) snapSnapSize.addEventListener('input', applySettings);

        btn.addEventListener('click', (e) => {
            e.stopPropagation();
            const isOpen = panel.style.display !== 'none';
            panel.style.display = isOpen ? 'none' : 'block';
            if (!isOpen) applySettings();
        });

        document.addEventListener('click', (e) => {
            if (!panel.contains(e.target) && !btn.contains(e.target)) {
                panel.style.display = 'none';
            }
        });
    })();

    window.addEventListener('pagehide', () => {
      if (this._previewWindow && !this._previewWindow.closed) this._previewWindow.close();
      this._previewWindow = null;
    }, { once: true });
    this.renderPalette();
    if (this._clockTimer) clearInterval(this._clockTimer);
    this._clockTimer = setInterval(() => this._refreshEditorClocks(), 1000);
    this.fetchAll();
  },

  cacheEls() {
    this.el = {
      page: document.getElementById('page-editor'),
      // Sidebar
      layoutSelect: document.getElementById('layout-select'),
      layoutBrowser: document.getElementById('layout-browser'),
      layoutBrowserButton: document.getElementById('layout-browser-button'),
      layoutBrowserPanel: document.getElementById('layout-browser-panel'),
      layoutBrowserCurrent: document.getElementById('layout-browser-current'),
      layoutBrowserSearch: document.getElementById('layout-browser-search'),
      layoutBrowserBreadcrumb: document.getElementById('layout-browser-breadcrumb'),
      layoutBrowserList: document.getElementById('layout-browser-list'),
      layoutBrowserNew: document.getElementById('layout-browser-new'),
      layoutBrowserNewFolder: document.getElementById('layout-browser-new-folder'),
      // Buttons
      assignLayoutBtn: document.getElementById('btn-assign-layout'),
      saveBtn: document.getElementById('btn-editor-save'),
      publishBtn: document.getElementById('btn-editor-publish'),
      publishStatus: document.getElementById('editor-publish-status'),
      deleteBtn: document.getElementById('btn-editor-delete'),
      previewBtn: document.getElementById('btn-editor-preview'),
      exportBtn: document.getElementById('btn-editor-export'),
      // Canvas
      canvas: document.getElementById('interactive-canvas'),
      canvasInfo: document.getElementById('interactive-canvas-info'),
      // Props
      props: document.getElementById('interactive-props-content'),
      // Palette
      palette: document.getElementById('interactive-widget-palette'),
      // widgetMode is rendered dynamically inside palette; read from DOM each time
    };
  },

  bind() {
    const E = this.el;
    if (!E.layoutSelect) return;

    if (E.layoutBrowserButton) {
      E.layoutBrowserButton.onclick = (e) => {
        e.stopPropagation();
        const open = E.layoutBrowserPanel.style.display !== 'none';
        E.layoutBrowserPanel.style.display = open ? 'none' : 'block';
        if (!open) this.renderLayoutBrowser();
      };
      E.layoutBrowserNew.onclick = () => this.createLayout();
      E.layoutBrowserNewFolder.onclick = () => this.createLayoutFolder();
      E.layoutBrowserSearch.oninput = e => {
        this.browserSearch = e.target.value || '';
        this.renderLayoutBrowser();
      };
      document.addEventListener('click', e => {
        const insideBrowser = e.composedPath
          ? e.composedPath().includes(E.layoutBrowser)
          : E.layoutBrowser.contains(e.target);
        if (E.layoutBrowserPanel.style.display !== 'none' && !insideBrowser) {
          E.layoutBrowserPanel.style.display = 'none';
        }
      });
    }

    E.layoutSelect.onchange = () => {
      const v = E.layoutSelect.value || '';
      if (v.startsWith('layout:')) this.switchToLayout(v.slice(7));
      else if (v) this.switchToLayout(v);
    };
    E.saveBtn.onclick = () => this.saveCurrent({ silent: false }).catch(() => {});
    if (E.publishBtn) E.publishBtn.onclick = () => this.publishCurrent();
    E.previewBtn.onclick = () => this.openPreview();
    E.exportBtn.onclick = () => this.exportAll();

    if (E.assignLayoutBtn) {
        E.assignLayoutBtn.onclick = (e) => {
            e.stopPropagation();
            const panel = document.getElementById('assign-dropdown-panel');
            document.querySelectorAll('.assign-dropdown-visible').forEach(p => { if (p !== panel) { p.style.display = 'none'; p.classList.remove('assign-dropdown-visible'); } });
            if (!panel) return;
            const isOpen = panel.style.display !== 'none';
            panel.style.display = isOpen ? 'none' : 'block';
            if (!isOpen) panel.classList.add('assign-dropdown-visible');
            if (!isOpen) {
                const layoutId = document.getElementById('layout-select')?.value;
                const layoutNameVal = document.getElementById('layout-browser-current')?.textContent?.trim();
                const nameEl = document.getElementById('assign-layout-name');
                if (nameEl) nameEl.textContent = layoutNameVal || layoutId || '—';
                const clientSel = document.getElementById('assign-client');
                const displaySel = document.getElementById('assign-display');
                const assignDo = document.getElementById('assign-do');
                const displayHint = document.getElementById('assign-display-hint');
                if (clientSel && !clientSel.dataset.loaded) {
                    clientSel.dataset.loaded = '1';
                    ;(async () => {
                        try {
                            const clients = await API.getClients();
                            const online = clients.filter(c => c.status === 'online');
                            clientSel.replaceChildren(new Option('— Select a device —', ''));
                            for (const client of online) {
                                const option = new Option(`${client.name || client.id} (${client.status})`, client.id);
                                clientSel.appendChild(option);
                            }
                            if (online.length === 0) clientSel.replaceChildren(new Option('No online devices', ''));
                        } catch(e) { clientSel.innerHTML = '<option value="">Failed to load</option>'; }
                    })();
                    clientSel.onchange = async () => {
                        displaySel.disabled = true;
                        displaySel.innerHTML = '<option value="">— Select a display —</option>';
                        if (assignDo) assignDo.disabled = true;
                        const hint = document.getElementById('assign-display-hint');
                        if (hint) { hint.textContent = 'Loading...'; hint.style.color = ''; }
                        const clientId = clientSel.value;
                        if (!clientId) { if (hint) hint.textContent = 'Choose a device first'; return; }
                        try {
                            const displays = await API.getClientDisplays(clientId);
                            displaySel.replaceChildren(new Option('— Select a display —', ''));
                            displays.forEach((d, i) => {
                                const active = d.status === 'active' || d.status === 'online' || d.assigned_layout_id;
                                const label = d.label || d.name || `Display ${i}`;
                                const option = new Option(`${active ? '●' : '●'} ${label} — ${d.status || 'unknown'}`, String(i));
                                option.dataset.active = String(!!active);
                                displaySel.appendChild(option);
                            });
                            displaySel.disabled = false;
                            if (hint) { hint.textContent = `${displays.length} display(s) found`; hint.style.color = displays.length ? '#3fb950' : '#f85149'; }
                        } catch(e) { if (hint) { hint.textContent = 'Failed to load'; hint.style.color = '#f85149'; } }
                    };
                    displaySel.onchange = () => { if (assignDo) assignDo.disabled = !displaySel.value; };
                }
            }
        };
    }

    document.addEventListener('click', (e) => {
        const panel = document.getElementById('assign-dropdown-panel');
        const btn = document.getElementById('btn-assign-layout');
        if (panel && !panel.contains(e.target) && !btn.contains(e.target)) {
            panel.style.display = 'none';
            panel.classList.remove('assign-dropdown-visible');
        }
    });

    document.getElementById('assign-do')?.addEventListener('click', async () => {
        const layoutSelectValue = document.getElementById('layout-select')?.value || '';
        const layoutId = layoutSelectValue.startsWith('layout:') ? layoutSelectValue.slice(7) : layoutSelectValue;
        const clientId = document.getElementById('assign-client')?.value;
        const displayIdx = parseInt(document.getElementById('assign-display')?.value);
        const assignDo = document.getElementById('assign-do');
        const statusDiv = document.getElementById('assign-status');
        if (!clientId || isNaN(displayIdx)) return;
        if (assignDo) { assignDo.disabled = true; assignDo.textContent = 'Assigning...'; }
        if (statusDiv) statusDiv.style.display = 'none';
        try {
            await API.assignClientDisplayLayout(clientId, displayIdx, layoutId);
            if (statusDiv) {
                statusDiv.textContent = '✓ Layout assigned';
                statusDiv.style.color = '#3fb950';
                statusDiv.style.background = 'rgba(63,185,80,0.1)';
                statusDiv.style.display = 'block';
            }
            if (assignDo) assignDo.textContent = 'Assigned!';
            setTimeout(() => {
                const panel = document.getElementById('assign-dropdown-panel');
                if (panel) { panel.style.display = 'none'; panel.classList.remove('assign-dropdown-visible'); }
                if (assignDo) { assignDo.disabled = false; assignDo.textContent = 'Assign'; }
            }, 1500);
        } catch(e) {
            if (statusDiv) {
                statusDiv.textContent = '✗ ' + e.message;
                statusDiv.style.color = '#f85149';
                statusDiv.style.background = 'rgba(248,81,73,0.1)';
                statusDiv.style.display = 'block';
            }
            if (assignDo) { assignDo.disabled = false; assignDo.textContent = 'Assign'; }
        }
    });

    document.getElementById('assign-cancel')?.addEventListener('click', () => {
        const panel = document.getElementById('assign-dropdown-panel');
        if (panel) { panel.style.display = 'none'; panel.classList.remove('assign-dropdown-visible'); }
    });

    E.canvas.onclick = e => {
      if (e.target === E.canvas) {
        this.selectedWidgetId = null;
        this.selectedWidget = null;
        this.renderProps();
      }
    };
    E.canvas.ondragover = e => { e.preventDefault(); E.canvas.classList.add('drop-target'); };
    E.canvas.ondragleave = () => E.canvas.classList.remove('drop-target');
    E.canvas.ondrop = e => {
      e.preventDefault();
      E.canvas.classList.remove('drop-target');
      const t = e.dataTransfer.getData('interactive-widget-type');
      if (t) this.addWidget(t, e.offsetX, e.offsetY);
    };

    // Global handlers: click outside the context menu closes it, Escape closes it.
    this._onDocClickForMenu = e => {
      const menu = document.querySelector('.interactive-context-menu');
      if (!menu) return;
      if (menu.contains(e.target)) return;
      this.closeContextMenu();
    };
    this._onDocKeyForMenu = e => {
      if (e.key === 'Escape') this.closeContextMenu();
    };
    document.addEventListener('mousedown', this._onDocClickForMenu);
    document.addEventListener('keydown', this._onDocKeyForMenu);

    // Palette drag
    E.palette.querySelectorAll('.interactive-palette-item').forEach(i => {
      i.draggable = true;
      i.ondragstart = e => e.dataTransfer.setData('interactive-widget-type', i.dataset.type);
      i.onclick = () => this.addWidget(i.dataset.type);
    });

    // Normal/Interactive mode switcher (delegated via palette -- widgetMode is dynamic)
    document.addEventListener('change', e => {
      if (e.target && e.target.id === 'interactive-widget-mode') this.renderPalette();
    });
  },

  refresh() {
    // Authentication can complete after Editor.init() has already attempted
    // its first API load. Re-run the complete load after sign-in so the layout
    // selector is not left with the unauthenticated empty result.
    return this.fetchAll();
  },

  async fetchAll() {
    const epoch = ++this._fetchEpoch;
    await Promise.all([this.fetchLayouts(epoch), this.fetchFolders(epoch), this.fetchLayoutSets(epoch), this.fetchPresets(false, epoch), this.fetchAddonWidgetTypes(epoch)]);
    if (epoch !== this._fetchEpoch) return;
    // Refresh palette so the per-type preset selectors show up.
    if (this.el && this.el.palette) this.renderPalette();
    // Listen for template changes from the Widget Templates page so cache stays fresh.
    if (!this._presetChangeWired) {
      this._presetChangeWired = true;
      window.addEventListener('widget-templates-changed', () => {
        this.presetCache = {};
        this.presetCacheAt = 0;
        this.fetchPresets(true).then(() => this.renderPalette());
      });
    }
    // Auto-select first layout if nothing selected.
    if (!this.currentLayoutId && this.layouts.length) {
      this.switchToLayout(this.layouts[0].id);
    }
    this.renderSidebar();
    this.renderCanvas();
    this.renderProps();
  },

  async fetchLayouts(epoch = this._fetchEpoch) {
    try {
      const layouts = await API.getLayouts();
      if (epoch !== this._fetchEpoch) return;
      this.layouts = (Array.isArray(layouts) ? layouts : []).map(l => this._normalizeLayout(l));
    } catch (e) { console.warn(e); this.layouts = []; }
  },

  async fetchFolders(epoch = this._fetchEpoch) {
    try {
      const folders = await API.getLayoutFolders();
      if (epoch !== this._fetchEpoch) return;
      this.folders = Array.isArray(folders) ? folders : [];
    } catch (e) {
      console.warn('fetchFolders error', e);
      this.folders = [];
    }
  },

  async fetchLayoutSets(epoch = this._fetchEpoch) {
    try {
      const summaries = await API.getLayoutSets();
      if (epoch !== this._fetchEpoch) return;
      const fullSets = await Promise.all((Array.isArray(summaries) ? summaries : []).map(set => API.getLayoutSet(set.id).catch(() => null)));
      if (epoch !== this._fetchEpoch) return;
      this.layoutSets = fullSets.filter(Boolean);
    } catch (e) {
      console.warn('fetchLayoutSets error', e);
      this.layoutSets = [];
    }
  },

  _layoutRotationSet(layout) {
    if (!layout) return null;
    return (this.layoutSets || []).find(set => Array.isArray(set.items) && set.items.some(item => item.layout_id === layout.id)) || null;
  },

  _layoutRotationState(layout) {
    const set = this._layoutRotationSet(layout);
    if (set && Array.isArray(set.items) && set.items.length) {
      const entries = set.items.slice().sort((a, b) => Number(a.position || 0) - Number(b.position || 0));
      const containerId = entries[0].layout_id;
      return {
        set,
        containerId,
        entries,
        items: entries.slice(1).map(entry => ({ layout_id: entry.layout_id, duration: entry.interval_secs ?? null })),
      };
    }
    const legacyItems = Array.isArray(layout?.items) ? layout.items : [];
    return {
      set: null,
      containerId: layout?.id || null,
      entries: layout ? [{ layout_id: layout.id, position: 0, interval_secs: layout.duration || 15 }, ...legacyItems.map((item, index) => ({ layout_id: item.layout_id, position: index + 1, interval_secs: item.duration ?? null }))] : [],
      items: legacyItems.slice(),
    };
  },

  async _saveLayoutRotation(layout, state, settings = {}) {
    if (!layout) return null;
    const itemEntries = (state?.items || []).map((item, index) => ({
      layout_id: item.layout_id,
      position: index + 1,
      interval_secs: settings.timingMode === 'per_layout' && item.duration != null ? Math.max(1, Number(item.duration) || 1) : null,
    }));
    const entries = [{
      layout_id: state?.containerId || layout.id,
      position: 0,
      interval_secs: Math.max(1, Number(settings.duration || layout.duration || 15) || 15),
    }, ...itemEntries];
    const existingSet = state?.set || this._layoutRotationSet(layout);
    if (entries.length === 1) {
      if (existingSet) {
        await API.deleteLayoutSet(existingSet.id);
        this.layoutSets = (this.layoutSets || []).filter(set => set.id !== existingSet.id);
      }
      layout.page_type = 'single';
      layout.items = [];
      await this.saveCurrent({ silent: false });
      return null;
    }
    layout.page_type = 'set';
    layout.duration = Math.max(1, Number(settings.duration || layout.duration || 15) || 15);
    layout.shuffle = settings.shuffle == null ? !!layout.shuffle : !!settings.shuffle;
    layout.timing_mode = settings.timingMode || layout.timing_mode || 'global';
    layout.items = itemEntries.map(item => ({ layout_id: item.layout_id, duration: item.interval_secs }));
    await this.saveCurrent({ silent: false });
    let savedSet;
    if (!existingSet) {
      savedSet = await API.saveLayoutSet({
        id: '',
        name: `${layout.name || 'Layout'} rotation`,
        shuffle: !!layout.shuffle,
        dwell_secs: layout.duration,
        items: entries,
      });
    } else {
      await API.saveLayoutSet({ id: existingSet.id, name: existingSet.name, shuffle: !!layout.shuffle, dwell_secs: layout.duration });
      savedSet = await API.replaceLayoutSetEntries(existingSet.id, entries);
    }
    this.layoutSets = [...(this.layoutSets || []).filter(set => set.id !== savedSet.id), savedSet];
    return savedSet;
  },

  // Normalize a layout from the API. The backend serializes WidgetType as
  // `widget_type`, but the editor code historically reads `w.type` — alias
  // them so the type-keyed branches in renderLayoutWidget/Inner work.
  _normalizeLayout(l) {
    if (!l) return l;
    const widgets = Array.isArray(l.widgets) ? l.widgets : [];
    l.widgets = widgets.map(w => {
      if (w) {
        const serverType = w.type || w.widget_type;
        if (serverType) {
          const normalizedType = String(serverType).trim().toLowerCase();
          w.type = normalizedType === 'datetime' ? 'date' : (normalizedType === 'webpage' ? 'embed' : normalizedType);
        }
      }
      return w;
    });
    return l;
  },

  async fetchPresets(force = false, epoch = this._fetchEpoch) {
    const now = Date.now();
    if (!force && this.presetCacheAt && (now - this.presetCacheAt) < 30000 && Object.keys(this.presetCache).length) {
      return this.presetCache;
    }
    // Two-pass load: listWidgetPresets returns metadata only (no props_json).
    // For each preset we fetch the full record so the palette has props to apply.
    try {
      const meta = await API.listWidgetPresets();
      if (epoch !== this._fetchEpoch) return this.presetCache;
      const full = [];
      for (const p of (meta || [])) {
        try {
          const f = await API.getWidgetPresetFull(p.id);
          full.push(f);
        } catch (e) {
          // Fall back to the metadata row so the selector still appears.
          full.push({ id: p.id, name: p.name, widget_type: p.widget_type, props_json: '{}', is_builtin: false });
        }
      }
      const byType = {};
      for (const p of full) {
        const t = (p.widget_type || '').toLowerCase();
        if (!t) continue;
        (byType[t] = byType[t] || []).push(p);
      }
      for (const t of Object.keys(byType)) {
        byType[t].sort((a, b) => {
          if (!!a.is_builtin !== !!b.is_builtin) return a.is_builtin ? -1 : 1;
          return String(a.name).localeCompare(String(b.name));
        });
      }
      if (epoch !== this._fetchEpoch) return this.presetCache;
      this.presetCache = byType;
      this.presetCacheAt = now;
    } catch (e) {
      console.warn('fetchPresets error', e);
      this.presetCache = this.presetCache || {};
      // Keep the bundled Weather default available if the metadata list
      // request is temporarily unauthorized or unavailable.
      try {
        const response = await fetch('/api/widget-presets/default-weather-simple-rows');
        if (response.ok) {
          const preset = await response.json();
          if (epoch !== this._fetchEpoch) return this.presetCache;
          this.presetCache.weather = [preset];
          this.presetCacheAt = now;
        }
      } catch (_) {}
    }
    return this.presetCache;
  },

  async fetchAddonWidgetTypes(epoch = this._fetchEpoch) {
    try {
      const registrations = await API.listAddonWidgetTypes();
      if (epoch !== this._fetchEpoch) return;
      this.addonWidgetTypes = Array.isArray(registrations) ? registrations : [];
    } catch (e) {
      console.warn('fetchAddonWidgetTypes error', e);
      this.addonWidgetTypes = [];
    }
  },

  // Selected default preset per widget_type (persistent in localStorage).
  getDefaultPresetId(widgetType) {
    try { return localStorage.getItem('ripley.defaultPreset.' + widgetType) || ''; } catch (e) { return ''; }
  },
  setDefaultPresetId(widgetType, presetId) {
    try {
      if (presetId) localStorage.setItem('ripley.defaultPreset.' + widgetType, presetId);
      else localStorage.removeItem('ripley.defaultPreset.' + widgetType);
    } catch (e) {}
  },

  // Resolve preset props for a widget type, honoring user selection then built-in default.
  resolvePresetProps(widgetType) {
    // Simple widget types use hardcoded defaults only — presets are intentionally ignored.
    if (this.RESTRICTED_PRESET_TYPES.has(widgetType)) return null;
    const presets = (this.presetCache || {})[widgetType] || [];
    if (!presets.length) return null;
    const wanted = this.getDefaultPresetId(widgetType);
    let chosen = presets.find(p => p.id === wanted);
    if (!chosen) chosen = presets.find(p => p.is_builtin) || presets[0];
    let props = {};
    try { props = JSON.parse(chosen.props_json || '{}') || {}; } catch (e) { props = {}; }
    return { preset: chosen, props };
  },

  switchToLayout(id) {
    this.currentLayoutId = id;
    this.selectedWidgetId = null;
    this.selectedWidget = null;
    this.renderSidebar();
    this.renderCanvas();
    this.renderProps();
    this.updateInfo();
  },

  get currentLayout() {
    return this.layouts.find(l => l.id === this.currentLayoutId) || null;
  },

  updateInfo() {
    const E = this.el;
    const l = this.currentLayout;
    if (E.canvasInfo) E.canvasInfo.textContent = l ? `${l.name}${l.interactive ? ' · interactive' : ''} · ${(l.widgets || []).length} widget(s)` : '';
    if (E.publishBtn) E.publishBtn.disabled = !l || !l.id || this._publishBusy;
  },

  renderLayoutBrowser() {
    const E = this.el;
    if (!E.layoutBrowserList) return;
    const search = String(this.browserSearch || '').trim().toLowerCase();
    const currentFolder = this.folders.find(f => f.id === this.browserFolderId);
    if (E.layoutBrowserCurrent) {
      const current = this.currentLayout;
      E.layoutBrowserCurrent.textContent = current ? `${current.interactive ? '🕹️' : '📐'} ${current.name}` : 'Select Layout';
    }
    if (E.layoutBrowserBreadcrumb) {
      if (this.browserFolderId) {
        const parentFolder = currentFolder?.parent_id
          ? this.folders.find(f => f.id === currentFolder.parent_id)
          : null;
        E.layoutBrowserBreadcrumb.innerHTML = `<div style="display:flex;align-items:center;gap:.55rem;"><button type="button" data-browser-back title="Go back to the parent folder" style="border:1px solid var(--border);border-radius:4px;background:var(--bg-card);color:var(--text);cursor:pointer;padding:.2rem .45rem;">← ${parentFolder ? this.esc(parentFolder.name) : 'Back to Layouts'}</button><span style="color:var(--text-muted);">Layouts / ${this.esc(currentFolder?.name || '')}</span></div>`;
        E.layoutBrowserBreadcrumb.querySelector('[data-browser-back]')?.addEventListener('click', (e) => {
          e.stopPropagation();
          this.browserFolderId = parentFolder?.id || null;
          this.renderLayoutBrowser();
        });
      } else {
        E.layoutBrowserBreadcrumb.innerHTML = '<span>Layouts <small style="color:var(--text-muted);">(main directory)</small></span>';
      }
    }

    const visibleLayouts = search
      ? this.layouts.filter(l => String(l.name || '').toLowerCase().includes(search))
      : this.layouts.filter(l => (l.folder_id || null) === (this.browserFolderId || null));
    const visibleFolders = search ? [] : this.folders.filter(f => (f.parent_id || null) === (this.browserFolderId || null));

    E.layoutBrowserList.innerHTML = [
      ...visibleFolders.map(folder => `<div style="display:flex;align-items:center;gap:.25rem;padding:.35rem;border-radius:5px;" data-folder-row="${this.esc(folder.id)}"><button type="button" data-folder-open="${this.esc(folder.id)}" style="flex:1;text-align:left;border:0;background:none;color:var(--text);cursor:pointer;padding:.3rem;">📁 ${this.esc(folder.name)}</button><button type="button" data-folder-rename="${this.esc(folder.id)}" title="Rename folder" style="border:0;background:none;color:var(--text-muted);cursor:pointer;">✎</button><button type="button" data-folder-delete="${this.esc(folder.id)}" title="Delete empty folder" style="border:0;background:none;color:var(--danger);cursor:pointer;">🗑</button></div>`),
      ...visibleLayouts.map(layout => `<div style="display:flex;align-items:center;gap:.35rem;padding:.35rem;border-radius:5px;${layout.id === this.currentLayoutId ? 'background:rgba(167,139,250,.16);' : ''}" data-layout-row="${this.esc(layout.id)}"><button type="button" data-layout-open="${this.esc(layout.id)}" style="flex:1;min-width:0;text-align:left;border:0;background:none;color:var(--text);cursor:pointer;padding:.25rem;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;">${layout.interactive ? '🕹️' : '📐'} ${this.esc(layout.name)} <small style="color:var(--text-muted);">· ${(layout.widgets || []).length} widget(s)</small></button><button type="button" data-layout-rename="${this.esc(layout.id)}" title="Rename layout" style="border:0;background:none;color:var(--text-muted);cursor:pointer;">✎</button><button type="button" data-layout-delete="${this.esc(layout.id)}" title="Delete layout" style="border:0;background:none;color:var(--danger);cursor:pointer;">🗑</button><select data-layout-move="${this.esc(layout.id)}" title="Move layout" style="max-width:92px;font-size:.7rem;background:var(--bg);color:var(--text);border:1px solid var(--border);"><option value="">Root</option>${this.folders.map(f => `<option value="${this.esc(f.id)}" ${layout.folder_id === f.id ? 'selected' : ''}>${this.esc(f.name)}</option>`).join('')}</select></div>`),
    ].join('') || `<div style="padding:1rem;text-align:center;color:var(--text-muted);">${search ? 'No matching layouts' : 'This folder is empty'}</div>`;

    E.layoutBrowserList.querySelectorAll('[data-folder-open]').forEach(btn => btn.onclick = (e) => {
      e.stopPropagation();
      this.browserFolderId = btn.dataset.folderOpen;
      this.renderLayoutBrowser();
    });
    E.layoutBrowserList.querySelectorAll('[data-layout-open]').forEach(btn => btn.onclick = () => {
      this.switchToLayout(btn.dataset.layoutOpen);
      E.layoutBrowserPanel.style.display = 'none';
    });
    E.layoutBrowserList.querySelectorAll('[data-layout-rename]').forEach(btn => btn.onclick = () => this.renameLayoutFromBrowser(btn.dataset.layoutRename));
    E.layoutBrowserList.querySelectorAll('[data-layout-delete]').forEach(btn => btn.onclick = (e) => {
      e.stopPropagation();
      this.showDeleteConfirmation('layout', btn.dataset.layoutDelete);
    });
    E.layoutBrowserList.querySelectorAll('[data-folder-rename]').forEach(btn => btn.onclick = () => this.renameFolderFromBrowser(btn.dataset.folderRename));
    E.layoutBrowserList.querySelectorAll('[data-folder-delete]').forEach(btn => btn.onclick = (e) => {
      e.stopPropagation();
      this.showDeleteConfirmation('folder', btn.dataset.folderDelete);
    });
    E.layoutBrowserList.querySelectorAll('[data-layout-move]').forEach(select => select.onchange = () => this.moveLayoutFromBrowser(select.dataset.layoutMove, select.value || null));
  },

  showDeleteConfirmation(type, id) {
    const E = this.el;
    if (!E.layoutBrowserList) return;
    E.layoutBrowserList.querySelectorAll('[data-delete-confirm]').forEach(panel => panel.remove());
    const item = type === 'folder'
      ? this.folders.find(f => f.id === id)
      : this.layouts.find(l => l.id === id);
    if (!item) return;
    const row = E.layoutBrowserList.querySelector(`[data-${type}-row="${CSS.escape(id)}"]`);
    if (!row) return;

    row.style.flexWrap = 'wrap';
    const panel = document.createElement('div');
    panel.dataset.deleteConfirm = '1';
    panel.style.cssText = 'flex:1 0 100%;display:flex;align-items:center;justify-content:space-between;gap:.5rem;margin-top:.25rem;padding:.35rem .45rem;border:1px solid var(--danger);border-radius:4px;background:rgba(220,38,38,.1);font-size:.75rem;';
    const message = document.createElement('span');
    message.textContent = type === 'folder'
      ? `Delete folder “${item.name}”? It must be empty.`
      : `Delete layout “${item.name}”? This cannot be undone.`;
    const actions = document.createElement('span');
    actions.style.cssText = 'display:flex;gap:.3rem;flex-shrink:0;';
    const cancel = document.createElement('button');
    cancel.type = 'button';
    cancel.textContent = 'Cancel';
    cancel.className = 'btn btn-secondary btn-sm';
    const remove = document.createElement('button');
    remove.type = 'button';
    remove.textContent = 'Delete';
    remove.className = 'btn btn-danger btn-sm';
    actions.append(cancel, remove);
    panel.append(message, actions);
    row.appendChild(panel);
    cancel.onclick = () => this.renderLayoutBrowser();
    remove.onclick = async () => {
      remove.disabled = true;
      cancel.disabled = true;
      if (type === 'folder') await this.deleteFolderFromBrowser(id, true);
      else await this.deleteLayout(id, true);
    };
  },

  requestDeleteLayout(id) {
    const layout = this.layouts.find(l => l.id === id);
    if (!layout || !this.el.layoutBrowserPanel) return;
    this.browserSearch = '';
    this.browserFolderId = layout.folder_id || null;
    this.el.layoutBrowserPanel.style.display = 'block';
    this.renderLayoutBrowser();
    this.showDeleteConfirmation('layout', id);
  },

  requestDeleteFolder(id) {
    const folder = this.folders.find(f => f.id === id);
    if (!folder || !this.el.layoutBrowserPanel) return;
    this.browserSearch = '';
    this.browserFolderId = folder.parent_id || null;
    this.el.layoutBrowserPanel.style.display = 'block';
    this.renderLayoutBrowser();
    this.showDeleteConfirmation('folder', id);
  },

  async createLayoutFolder() {
    const name = prompt('Folder name:', 'New Folder');
    if (!name?.trim()) return;
    try {
      const folder = await API.saveLayoutFolder({ id: '', name: name.trim(), parent_id: this.browserFolderId || null });
      this.folders.push(folder);
      this.renderLayoutBrowser();
    } catch (e) { alert('Failed to create folder: ' + e.message); }
  },

  async renameFolderFromBrowser(id) {
    const folder = this.folders.find(f => f.id === id);
    if (!folder) return;
    const name = prompt('Rename folder:', folder.name);
    if (!name?.trim() || name.trim() === folder.name) return;
    try {
      const saved = await API.saveLayoutFolder({ ...folder, name: name.trim() });
      this.folders = this.folders.map(f => f.id === saved.id ? saved : f);
      this.renderLayoutBrowser();
    } catch (e) { alert('Failed to rename folder: ' + e.message); }
  },

  async deleteFolderFromBrowser(id, confirmed = false) {
    const folder = this.folders.find(f => f.id === id);
    if (!folder) return;
    if (!confirmed) {
      this.requestDeleteFolder(id);
      return;
    }
    try {
      await API.deleteLayoutFolder(id);
      this.folders = this.folders.filter(f => f.id !== id);
      if (this.browserFolderId === id) this.browserFolderId = null;
      this.renderLayoutBrowser();
    } catch (e) { alert(e.message); }
  },

  async renameLayoutFromBrowser(id) {
    const layout = this.layouts.find(l => l.id === id);
    const E = this.el;
    if (!layout || !E.layoutBrowserList) return;
    const row = E.layoutBrowserList.querySelector(`[data-layout-row="${CSS.escape(id)}"]`);
    const openButton = row?.querySelector('[data-layout-open]');
    const renameButton = row?.querySelector('[data-layout-rename]');
    if (!row || !openButton || !renameButton) return;

    // Replace the row label with an inline editor; no popup is needed.
    const input = document.createElement('input');
    input.type = 'text';
    input.value = layout.name || '';
    input.maxLength = 160;
    input.setAttribute('aria-label', 'Layout name');
    input.style.cssText = 'flex:1;min-width:0;padding:.25rem .4rem;border:1px solid var(--accent);border-radius:4px;background:var(--bg);color:var(--text);';
    openButton.replaceWith(input);
    renameButton.textContent = '✓';
    renameButton.title = 'Save layout name';
    input.focus();
    input.select();

    let finished = false;
    const finish = async (save) => {
      if (finished) return;
      finished = true;
      const nextName = input.value.trim();
      if (!save || !nextName || nextName === layout.name) {
        this.renderLayoutBrowser();
        return;
      }
      const previousName = layout.name;
      layout.name = nextName;
      try {
        const saved = await API.saveLayout(layout);
        this._normalizeLayout(saved);
        this.layouts = this.layouts.map(l => l.id === saved.id ? saved : l);
        this.renderSidebar();
      } catch (e) {
        layout.name = previousName;
        alert('Failed to rename layout: ' + e.message);
        this.renderLayoutBrowser();
      }
    };
    input.onkeydown = e => {
      if (e.key === 'Enter') { e.preventDefault(); finish(true); }
      if (e.key === 'Escape') { e.preventDefault(); finish(false); }
    };
    input.onblur = () => finish(true);
    renameButton.onclick = () => finish(true);
  },

  async moveLayoutFromBrowser(id, folderId) {
    const layout = this.layouts.find(l => l.id === id);
    if (!layout) return;
    layout.folder_id = folderId;
    try {
      const saved = await API.saveLayout(layout);
      this._normalizeLayout(saved);
      this.layouts = this.layouts.map(l => l.id === saved.id ? saved : l);
      this.renderSidebar();
      this.renderLayoutBrowser();
    } catch (e) { alert('Failed to move layout: ' + e.message); }
  },

  renderSidebar() {
    const E = this.el;
    if (!E.layoutSelect) return;

    E.layoutSelect.innerHTML = '<option value="">— Select Layout —</option>';
    const seenLayouts = new Set();
    this.layouts.forEach(l => {
      if (seenLayouts.has(l.id)) return;
      seenLayouts.add(l.id);
      const opt = document.createElement('option');
      opt.value = `layout:${l.id}`;
      opt.textContent = `${l.interactive ? '🕹️' : '📐'} ${l.name}`;
      if (l.id === this.currentLayoutId) opt.selected = true;
      E.layoutSelect.appendChild(opt);
    });
    this.renderLayoutBrowser();
  },

  renderPalette() {
    const E = this.el;
    if (!E.palette) return;
    const modeEl = document.getElementById('interactive-widget-mode');
    const mode = modeEl ? modeEl.value : 'normal';
    const interactiveWidgets = [
      { type: 'button', icon: '🔘', label: 'Button' },
      { type: 'dropdown', icon: '▾', label: 'Drop-down' },
      { type: 'checkbox', icon: '☑️', label: 'Checkbox' },
      { type: 'checkbox-list', icon: '☑', label: 'Checkbox List' },
      { type: 'radio', icon: '◉', label: 'Radio Buttons' },
      { type: 'textbox', icon: '⌨️', label: 'Text Box' },
      { type: 'keyboard', icon: '🗝', label: 'On-screen Keyboard' },
    ];
    const normalWidgets = [
      { type: 'text', icon: '📝', label: 'Text' },
      { type: 'image', icon: '🖼️', label: 'Image' },
      { type: 'video', icon: '🎬', label: 'Video' },
      { type: 'clock', icon: '🕐', label: 'Clock' },
      { type: 'date', icon: '📅', label: 'Date' },
      { type: 'weather', icon: '🌤️', label: 'Weather' },
      { type: 'traffic', icon: '🚧', label: 'Traffic' },
      { type: 'countdown', icon: '⏱️', label: 'Countdown' },
      { type: 'sound', icon: '🔊', label: 'Sound' },
      { type: 'embed', icon: '🔗', label: 'Embed' },
      { type: 'calendar', icon: '📆', label: 'Calendar' },
      { type: 'news', icon: '📰', label: 'News Ticker' },
      { type: 'rectangle', icon: '🟦', label: 'Shape' },
    ];
    const widgets = mode === 'normal' ? normalWidgets.concat((this.addonWidgetTypes || []).map(addon => ({
      type: `addon:${addon.addon_id}:${addon.id}`,
      icon: addon.icon || '🧩',
      label: addon.name,
      addon,
    }))) : interactiveWidgets;
    const note = mode === 'normal'
      ? 'Display/media widgets for normal layouts.'
      : 'Interactive touch widgets. Switch to Widgets for display/media widgets.';
    E.palette.innerHTML = `
      <h3>Widget Palette</h3>
      <select id="interactive-widget-mode" class="select interactive-widget-mode">
        <option value="normal" ${mode === 'normal' ? 'selected' : ''}>Widgets</option>
        <option value="interactive" ${mode === 'interactive' ? 'selected' : ''}>Interactive Widgets</option>
      </select>
      <p class="interactive-palette-note">${this.esc(note)}</p>
      <div class="palette-items">
        ${widgets.map(w => `
          <div class="interactive-palette-item" data-type="${this.esc(w.type)}">
            <span class="palette-icon">${this.esc(w.icon)}</span>
            <span class="palette-label">${this.esc(w.label)}</span>
          </div>`).join('')}
      </div>
    `;
    // Rebind palette events
    E.palette.querySelectorAll('.interactive-palette-item').forEach(i => {
      i.draggable = true;
      i.ondragstart = e => e.dataTransfer.setData('interactive-widget-type', i.dataset.type);
      i.onclick = () => this.addWidget(i.dataset.type);
    });
    // Presets are selected after a widget is added, in Widget Properties.
  },

  renderCanvas() {
    this.renderLayoutCanvas();
  },

  renderLayoutCanvas() {
    const E = this.el;
    const snapFromSettings = this._tplSnapGrid || { enabled: true, visible: true, size: 10, snap_size: 10 };
    const layout = this.currentLayout;
    if (!layout) {
      E.canvas.innerHTML = '<div class="canvas-empty">Select or create a Layout to begin</div>';
      E.canvas.classList.remove('interactive-grid-visible');
      return;
    }
    const orientation = layout.canvas_orientation === 'portrait' ? 'portrait' : 'landscape';
    E.canvas.classList.toggle('orientation-portrait', orientation === 'portrait');
    E.canvas.classList.toggle('orientation-landscape', orientation === 'landscape');
    E.canvas.style.aspectRatio = orientation === 'portrait' ? '9 / 16' : '16 / 9';
    const widgets = layout.widgets || [];
    if (!widgets.length) {
      E.canvas.innerHTML = '<div class="canvas-empty">Drag or click a Widget to add it</div>';
      E.canvas.classList.remove('interactive-grid-visible');
      return;
    }

    // Apply snap/grid settings to canvas
    if (snapFromSettings.visible) {
      E.canvas.style.setProperty('--interactive-grid-size', snapFromSettings.size + 'px');
      E.canvas.classList.add('interactive-grid-visible');
    } else {
      E.canvas.classList.remove('interactive-grid-visible');
    }
    E.canvas.innerHTML = widgets.slice().sort((a, b) => a.z - b.z).map(w => this.renderLayoutWidget(w)).join('');
    this._fitTrafficTemplateText();
    this.bindLayoutWidgetNodes();
    this._refreshWeatherEditorPreviews();
    this._refreshTrafficEditorPreviews();
    this._refreshCalendarEditorPreviews();
  },

  _weatherIconHtml(code, isNight, props = {}) {
    const icons = props.custom_icons || {};
    const iconKey = String(code) + (isNight ? '_night' : '');
    const customUrl = icons[iconKey] || icons[String(code)] || '';
    if (customUrl) return `<img src="${this.esc(customUrl)}" alt="${this.esc(typeof ripleyWeatherIconDescription === 'function' ? ripleyWeatherIconDescription(code) : 'Weather')}" style="width:100%;height:100%;object-fit:contain;">`;
    return typeof ripleyWeatherIconSvg === 'function' ? ripleyWeatherIconSvg(code, isNight, {staticOnly: true}) : '';
  },

  _weatherPreviewCode(data, offset = 0) {
    const daily = data?.daily || {};
    const dayCodes = daily.weather_code_day || daily.weathercode_day || daily.weather_code || daily.weathercode || [];
    const nightCodes = daily.weather_code_night || daily.weathercode_night || dayCodes;
    const isNight = offset === 0 && data?.current_weather?.is_daytime === false;
    const series = isNight ? nightCodes : dayCodes;
    return { code: series?.[offset] ?? 2, isNight };
  },

  async _refreshWeatherEditorPreviews() {
    const widgets = (this.currentLayout?.widgets || []).filter(w => w.type === 'weather' && w.props?.lat && w.props?.lon);
    for (const w of widgets) {
      const root = document.querySelector(`[data-weather-preview-id="${CSS.escape(w.id)}"]`);
      if (!root) continue;
      const p = w.props || {};
      const unit = String(p.units || 'metric') === 'imperial' ? 'fahrenheit' : 'celsius';
      const days = Math.max(1, Math.min(5, Number(p.shown_days) || 4));
      try {
        const response = await fetch('/api/weather/forecast?lat=' + encodeURIComponent(p.lat) + '&lon=' + encodeURIComponent(p.lon), {cache: 'no-store'});
        if (!response.ok) throw new Error('Weather request failed');
        const data = await response.json();
        const toDisplayTemp = value => { let n = Number(value); if (unit === 'celsius' && data.source === 'noaa_nws') n = (n - 32) * 5 / 9; return Math.round(n); };
        const toDisplayWind = value => { let n = Number(value) || 0; if (unit === 'celsius') n *= 1.60934; return Math.round(n); };
        const windSuffix = unit === 'fahrenheit' ? 'mph' : 'km/h';
        const current = root.querySelector('.editor-weather-temp');
        if (current) {
          let currentTemp = Number(data.current_weather?.temperature ?? data.current?.temperature_2m);
          current.textContent = toDisplayTemp(currentTemp) + (unit === 'fahrenheit' ? '°F' : '°C');
        }
        const rows = root.querySelectorAll('.editor-weather-forecast > div');
        rows.forEach((row, index) => {
          const hi = toDisplayTemp(data.daily?.temperature_2m_max?.[index]);
          const lo = toDisplayTemp(data.daily?.temperature_2m_min?.[index]);
          const span = row.querySelector('span');
          if (span) span.textContent = `${hi}° / ${lo}°${unit === 'fahrenheit' ? 'F' : 'C'}`;
        });
        const currentIcon = root.querySelector('.editor-weather-icon');
        if (currentIcon) {
          const currentWeather = this._weatherPreviewCode(data, 0);
          currentIcon.innerHTML = this._weatherIconHtml(currentWeather.code, currentWeather.isNight, p);
        }
        const weatherNames = {0:'Clear',1:'Mainly clear',2:'Partly cloudy',3:'Overcast',45:'Fog',48:'Rime fog',51:'Light drizzle',53:'Moderate drizzle',55:'Dense drizzle',61:'Slight rain',63:'Moderate rain',65:'Heavy rain',71:'Slight snow',73:'Moderate snow',75:'Heavy snow',80:'Rain showers',81:'Heavy rain showers',82:'Violent rain showers',95:'Thunderstorm',96:'Thunderstorm with hail',99:'Severe thunderstorm'};
        root.querySelectorAll('[data-weather-template-value="1"]').forEach(node => {
          const key = node.dataset.weatherKey || '';
          const source = node.dataset.weatherSource || 'weather';
          const offset = Math.max(0, Number(node.dataset.weatherDayOffset) || 0);
          const date = data.daily?.time?.[offset];
          let value = node.textContent;
          if (source === 'date' || key === 'full_date') {
            const parsed = date ? new Date(date + 'T12:00:00') : new Date();
            value = parsed.toLocaleDateString(undefined, { weekday: 'long', month: 'short', day: 'numeric' });
          } else if (key === 'high_temp') value = `High ${toDisplayTemp(data.daily?.temperature_2m_max?.[offset])}°${unit === 'fahrenheit' ? 'F' : 'C'}`;
 else if (key === 'low_temp') value = `Low ${toDisplayTemp(data.daily?.temperature_2m_min?.[offset])}°${unit === 'fahrenheit' ? 'F' : 'C'}`;
          else if (key === 'weather_description') value = weatherNames[data.daily?.weather_code?.[offset]] || 'Unknown';
          else if (key === 'weather_icon') {
            const weather = this._weatherPreviewCode(data, offset);
            node.innerHTML = this._weatherIconHtml(weather.code, weather.isNight, p);
            return;
          }
          else if (key === 'precip_chance') value = `${Math.round(Number(data.daily?.precipitation_probability_max?.[offset]) || 0)}%`;
          else if (key === 'wind_speed') value = `Wind ${toDisplayWind(data.daily?.wind_speed?.[offset])} ${windSuffix}`;
          else if (key === 'current_temp') { let t = Number(data.current_weather?.temperature ?? data.current?.temperature_2m); value = `${toDisplayTemp(t)}°${unit === 'fahrenheit' ? 'F' : 'C'}`; }
          if (value != null && value !== 'NaN' && !String(value).includes('NaN')) node.textContent = value;
        });
        const status = root.querySelector('.editor-weather-status');
        if (status) status.textContent = 'Live preview';
      } catch (_) {
        const status = root.querySelector('.editor-weather-status');
        if (status) status.textContent = 'Location selected · live preview unavailable';
      }
    }
  },

  async _refreshTrafficEditorPreviews() {
    const widgets = (this.currentLayout?.widgets || []).filter(w => w.type === 'traffic' && w.props?.lat && w.props?.lon);
    for (const w of widgets) {
      const root = document.querySelector(`[data-traffic-preview-id="${CSS.escape(w.id)}"]`);
      if (!root) continue;
      const p = w.props || {};
      try {
        const url = '/api/traffic/events?lat=' + encodeURIComponent(p.lat) + '&lon=' + encodeURIComponent(p.lon) + '&radius_miles=' + encodeURIComponent(p.radius_miles || 25);
        const response = await fetch(url, {cache: 'no-store'});
        if (!response.ok) throw new Error('Traffic request failed');
        const data = await response.json();
        const rawEvents = Array.isArray(data.events) ? data.events : [];
        const events = (data.configured !== false && data.available !== false && typeof window.ripleyFilterTrafficEvents === 'function')
          ? window.ripleyFilterTrafficEvents(rawEvents, p, w.id)
          : rawEvents;
        data.events = events;
        p._traffic_data = data;
        const status = root.querySelector('.editor-traffic-status');
        const list = root.querySelector('.editor-traffic-list');
        const defaultNode = root.querySelector('.editor-traffic-default-variable');
        const trafficKey = p.traffic_variable || 'traffic_summary';
        const displayEvents = trafficKey === 'traffic_crashes_closures'
          ? events.filter(event => event.category === 'crash' || event.category === 'closure')
          : events;
        const trafficDefaultValue = trafficKey === 'traffic_incidents'
          ? String(events.filter(event => event.category === 'crash').length)
          : trafficKey === 'traffic_crashes_closures'
            ? (displayEvents[0]?.description || '')
          : trafficKey === 'traffic_closures'
            ? String(events.filter(event => event.category === 'closure').length)
            : trafficKey === 'traffic_delays'
              ? String(events.filter(event => event.category === 'delay').length)
              : `${events.length} traffic event${events.length === 1 ? '' : 's'}`;
        if (defaultNode) defaultNode.textContent = displayEvents.length || p.hide_if_empty === false ? trafficDefaultValue : '';
        if (!data.configured && !displayEvents.length) {
          if (status) status.textContent = 'Traffic feed credentials are not configured';
          if (list) list.innerHTML = '<div>🚧 Feed not configured</div>';
          continue;
        }
        if (status) status.textContent = data.fetched_at ? 'Live preview' : 'Traffic feed unavailable';
        if (list) list.innerHTML = displayEvents.length ? displayEvents.slice(0, Math.max(1, Number(p.max_events) || 3)).map(event => `<div><b>${this.esc(event.category || 'Event')}</b> · ${this.esc(event.road || 'Road')} ${this.esc(event.direction || '')}</div>`).join('') : '<div>No active crashes or road closures in this area</div>';
      } catch (_) {
        const status = root.querySelector('.editor-traffic-status');
        if (status) status.textContent = 'Area selected · live preview unavailable';
      }
    }
  },

  renderLayoutWidget(w) {
    const sel = w.id === this.selectedWidgetId ? 'selected' : '';
    const label = w.props?._addon_label || (this.getWidgetLabel ? this.getWidgetLabel(w.type) : w.type);
    const rotation = Math.max(-180, Math.min(180, Number.isFinite(Number(w.props?.rotation)) ? Number(w.props.rotation) : 0));
    const x = Math.max(0, Math.min(100, Number(w.x) || 0));
    const y = Math.max(0, Math.min(100, Number(w.y) || 0));
    const width = Math.max(0.1, Math.min(100, Number(w.w) || 1));
    const height = Math.max(0.1, Math.min(100, Number(w.h) || 1));
    const z = Math.max(0, Math.min(100000, Number(w.z) || 0));
    return `<div class="interactive-widget ${this.esc(sel)}" data-id="${this.esc(w.id)}" data-type="layout" style="left:${x}%;top:${y}%;width:${width}%;height:${height}%;z-index:${z};transform:rotate(${rotation}deg);transform-origin:center center">
      <div class="interactive-widget-content">${this.layoutWidgetInner(w)}</div>
      <div class="rotation-handle" title="Drag to rotate" aria-label="Drag to rotate">↻</div>
      <div class="interactive-widget-label"><span class="interactive-widget-label-text">${this.esc(label)}</span><span class="interactive-widget-label-arrow interactive-widget-label-arrow-ne"></span><span class="interactive-widget-label-arrow interactive-widget-label-arrow-sw"></span></div>
      <div class="resize-handle nw"></div>
      <div class="resize-handle n"></div>
      <div class="resize-handle ne"></div>
      <div class="resize-handle e"></div>
      <div class="resize-handle se"></div>
      <div class="resize-handle s"></div>
      <div class="resize-handle sw"></div>
      <div class="resize-handle w"></div>
    </div>`;
  },

  _trafficTemplatePreviewHtml(w) {
    const p = w.props || {};
    const elements = Array.isArray(p.template_elements) ? p.template_elements : [];
    if (!elements.length) return null;
    let designW = Math.max(1, Number(p.template_width) || 760);
    let designH = Math.max(1, Number(p.template_height) || 428);
    const labels = { traffic_summary: '3 traffic events', traffic_incidents: '1', traffic_crashes_closures: 'Crash on I-80 eastbound', traffic_closures: '1', traffic_delays: '1', traffic_event_road: 'US-22', traffic_event_direction: 'eastbound', traffic_event_description: 'Traffic event', traffic_event_category: 'delay', traffic_event_distance: '4.2 mi', traffic_event_map: 'Event map' };
    const repeatGroupFor = el => el.repeat?.group || el.repeat?.group_id || el.repeat_group || el.repeatGroup || null;
    const liveEvents = Array.isArray(p._traffic_data?.events) ? p._traffic_data.events : null;
    const availableEvents = liveEvents ? liveEvents.length : 3;
    const repeatModeFor = el => repeatGroupFor(el) && (!el.repeat?.mode || el.repeat.mode === 'none') ? 'rows' : (el.repeat?.mode || 'none');
    const repeatOrderFor = el => String(el.repeat?.order || '').toLowerCase().replace(/_/g, '-') === 'bottom-up' ? 'bottom-up' : 'top-down';
    const countFor = el => {
      const group = repeatGroupFor(el);
      if (!group) return 1;
      const rep = el.repeat || {};
      const requested = Math.max(1, Math.min(10, Number(p.max_events) || Number(rep.count) || 3));
      const hide = rep.hide_if_empty === true || String(rep.hide_if_empty).toLowerCase() === 'true' || p.hide_if_empty === true;
      if (liveEvents && hide && availableEvents === 0) return 0;
      return liveEvents ? Math.min(requested, availableEvents) : requested;
    };
    const sizingCountFor = el => {
      const actual = countFor(el);
      const rep = el.repeat || {};
      const group = repeatGroupFor(el);
      if (!group) return actual;
      const groupPreservesSize = elements.some(item => repeatGroupFor(item) === group && (item.repeat?.preserve_size === true || String(item.repeat?.preserve_size).toLowerCase() === 'true'));
      if (!groupPreservesSize) return actual;
      const requested = Math.max(1, Math.min(10, Number(p.max_events) || Number(rep.count) || 3));
      return Math.max(actual, requested);
    };
    const groups = {};
    elements.forEach(el => {
      const group = repeatGroupFor(el);
      if (!group) return;
      const rep = el.repeat || {};
      const m = groups[group] || (groups[group] = { minX: Infinity, minY: Infinity, maxX: -Infinity, maxY: -Infinity, gapX: Number(rep.gap_x) || 0, gapY: Number(rep.gap_y) || 0 });
      const x = Number(el.x) || 0, y = Number(el.y) || 0, width = Number(el.width) || 120, height = Number(el.height) || 60;
      m.minX = Math.min(m.minX, x); m.minY = Math.min(m.minY, y); m.maxX = Math.max(m.maxX, x + width); m.maxY = Math.max(m.maxY, y + height);
      m.gapX = Number(rep.gap_x) || m.gapX; m.gapY = Number(rep.gap_y) || m.gapY;
    });
    const positionFor = (el, index) => {
      const group = repeatGroupFor(el), m = group && groups[group];
      const x = Number(el.x) || 0, y = Number(el.y) || 0;
      if (!m) return { x, y };
      const mode = repeatModeFor(el);
      const order = repeatOrderFor(el);
      if (index === 0 && order !== 'bottom-up') return { x, y };
      if (mode === 'columns') return { x: x + index * ((m.maxX - m.minX) + m.gapX), y };
      const step = (m.maxY - m.minY) + m.gapY;
      if (order === 'bottom-up') {
        const requiredHeight = m.maxY + Math.max(0, sizingCountFor(el) - 1) * step;
        const canvasHeight = Math.max(Number(p.template_height) || 0, requiredHeight);
        return { x, y: canvasHeight - (m.maxY - m.minY) + y - m.minY - index * step };
      }
      return { x, y: y + index * step };
    };
    elements.forEach(el => {
      for (let index = 0; index < sizingCountFor(el); index++) {
        const pos = positionFor(el, index);
        designW = Math.max(designW, pos.x + (Number(el.width) || 120));
        designH = Math.max(designH, pos.y + (Number(el.height) || 60));
      }
    });
        const html = elements.slice().sort((a, b) => (Number(a.z) || 0) - (Number(b.z) || 0)).flatMap(el => {
      const ep = el.props || {};
      const count = countFor(el);
      return Array.from({ length: count }, (_, index) => {
        const pos = positionFor(el, index);
        const width = Number(el.width) || 120, height = Number(el.height) || 60;
        const key = ep.variable_key || '';
        const fs = Number(ep.font_size) || 26;
        const text = key && labels[key] != null ? labels[key] : (ep.text || ep.default_text || 'Traffic');
        const align = ep.align === 'left' ? 'flex-start' : (ep.align === 'right' ? 'flex-end' : 'center');
        let elementStyle = `position:absolute;left:${pos.x / designW * 100}%;top:${pos.y / designH * 100}%;width:${width / designW * 100}%;height:${height / designH * 100}%;display:flex;align-items:center;justify-content:${align};overflow:hidden;color:${this.esc(ep.color || '#fff')};font-size:${this.templateEditorFontCss(fs, designW, designH)};font-weight:800;line-height:1.08;text-align:${this.esc(ep.align || 'center')};`;
        let content = this.esc(text);
        let fitAttrs = '';
        if (['variable_text', 'variable_icon', 'variable', 'date'].includes(el.type) && ep.variable_source === 'traffic' && key === 'traffic_event_map') {
          elementStyle += 'padding:0;background:#0f172a;';
          content = '<span style="display:block;position:relative;width:100%;height:100%;overflow:hidden;background:#0f172a;"><img src="/api/traffic/map.png?lat=40.4406&lon=-79.9959&category=crash" alt="Sample traffic event map" style="display:block;width:100%;height:100%;object-fit:cover;" onerror="this.style.display=\'none\';this.nextElementSibling.style.display=\'flex\';"><span style="display:none;align-items:center;justify-content:center;width:100%;height:100%;font-size:42%;background:linear-gradient(135deg,#243447,#0f172a);color:#f8fafc;">🗺️</span></span>';
        } else if (['variable_text', 'variable', 'date'].includes(el.type) && ep.variable_source === 'traffic') {
          const fitGroup = repeatGroupFor(el) || `traffic-${el.type || 'variable'}-${index}`;
          content = `<span class="ripley-template-variable-text" style="display:block;width:100%;max-width:100%;max-height:100%;overflow-wrap:anywhere;word-break:break-word;line-height:1.08;">${content}</span>`;
          elementStyle += 'white-space:normal;overflow-wrap:anywhere;word-break:break-word;';
          fitAttrs = ` data-ripley-fit-text="1" data-ripley-fit-group="${this.esc(fitGroup)}"`;
        }
        if (el.type === 'image') {
          const imageBaseRadius = Number(ep.radius ?? 0) || 0;
          const imageRadius = `${Number(ep.radius_tl ?? imageBaseRadius)||0}px ${Number(ep.radius_tr ?? imageBaseRadius)||0}px ${Number(ep.radius_br ?? imageBaseRadius)||0}px ${Number(ep.radius_bl ?? imageBaseRadius)||0}px`;
          const imageFit = ['contain', 'cover', 'fill', 'none', 'scale-down'].includes(ep.fit) ? ep.fit : 'contain';
          elementStyle += `border-radius:${imageRadius};clip-path:inset(0 round ${imageRadius});-webkit-clip-path:inset(0 round ${imageRadius});`;
          content = ep.url ? `<img src="${this.esc(ep.url)}" alt="${this.esc(ep.alt || '')}" style="display:block;width:100%;height:100%;object-fit:${imageFit};border-radius:${imageRadius};clip-path:inset(0 round ${imageRadius});-webkit-clip-path:inset(0 round ${imageRadius});">` : '🖼️ Image';
        } else if (el.type === 'rectangle') {
          const shape = ep.shape || 'rectangle';
          const opacity = Math.max(0, Math.min(100, Number(ep.opacity == null ? 100 : ep.opacity))) / 100;
          const baseRadius = Number(ep.radius ?? 0) || 0;
          const radius = shape === 'circle' ? '50%' : (shape === 'pill' ? '999px' : `${Number(ep.radius_tl ?? baseRadius)||0}px ${Number(ep.radius_tr ?? baseRadius)||0}px ${Number(ep.radius_br ?? baseRadius)||0}px ${Number(ep.radius_bl ?? baseRadius)||0}px`);
          const clipMap = { triangle: 'polygon(50% 0%, 0% 100%, 100% 100%)', diamond: 'polygon(50% 0%, 100% 50%, 50% 100%, 0% 50%)', hexagon: 'polygon(25% 0%, 75% 0%, 100% 50%, 75% 100%, 25% 100%, 0% 50%)', star: 'polygon(50% 0%, 61% 35%, 98% 35%, 68% 57%, 79% 91%, 50% 70%, 21% 91%, 32% 57%, 2% 35%, 39% 35%)', parallelogram: 'polygon(20% 0%, 100% 0%, 80% 100%, 0% 100%)', arrow: 'polygon(0% 25%, 60% 25%, 60% 0%, 100% 50%, 60% 100%, 60% 75%, 0% 75%)', plus: 'polygon(35% 0%, 65% 0%, 65% 35%, 100% 35%, 100% 65%, 65% 65%, 65% 100%, 35% 100%, 35% 65%, 0% 65%, 0% 35%, 35% 35%)', minus: 'polygon(0% 35%, 100% 35%, 100% 65%, 0% 65%)', x: 'polygon(20% 0%, 50% 30%, 80% 0%, 100% 20%, 70% 50%, 100% 80%, 80% 100%, 50% 70%, 20% 100%, 0% 80%, 30% 50%, 0% 20%)' };
          const clip = clipMap[shape] || '';
          const lineStyle = shape === 'line' ? 'height:6px;' : '';
          elementStyle += `border-radius:${radius};${clip ? `clip-path:${clip};-webkit-clip-path:${clip};` : ''}`;
          content = `<span style="position:absolute;inset:0;background:${this.esc(ep.color || '#1e293b')};opacity:${opacity};border-radius:${radius};${lineStyle}${clip ? `clip-path:${clip};-webkit-clip-path:${clip};` : ''}"></span>`;
        }
        return `<div${fitAttrs} style="${elementStyle}">${content}</div>`;
      });
    }).join('');
    return `<div class="editor-traffic-template-preview" style="position:relative;width:100%;height:100%;overflow:hidden;container-type:size;background:rgba(30,41,59,.55);">${html}</div>`;
  },

  _weatherTemplatePreviewHtml(w) {
    const p = w.props || {};
    const elements = Array.isArray(p.template_elements) ? p.template_elements : [];
    if (!elements.length) return null;
    let designW = Math.max(1, Number(p.template_width) || 760);
    let designH = Math.max(1, Number(p.template_height) || 428);
    const units = String(p.units || 'metric') === 'imperial' ? '°F' : '°C';
    const repeatGroupFor = el => el.repeat?.group || el.repeat?.group_id || el.repeat_group || el.repeatGroup || null;
    const countFor = el => repeatGroupFor(el) ? Math.max(1, Math.min(30, Number(p.shown_days) || Number(el.repeat?.count) || 4)) : Math.max(1, Math.min(30, Number(el.repeat?.count) || 1));
    const modeFor = el => repeatGroupFor(el) && (!el.repeat?.mode || el.repeat.mode === 'none') ? 'rows' : (el.repeat?.mode || 'none');
    const groupMetrics = {};
    elements.forEach(el => {
      const group = repeatGroupFor(el);
      if (!group) return;
      const m = groupMetrics[group] || (groupMetrics[group] = { minX: Infinity, minY: Infinity, maxX: -Infinity, maxY: -Infinity, gapX: Number(el.repeat?.gap_x) || 0, gapY: Number(el.repeat?.gap_y) || 0 });
      m.minX = Math.min(m.minX, Number(el.x) || 0); m.minY = Math.min(m.minY, Number(el.y) || 0);
      m.maxX = Math.max(m.maxX, (Number(el.x) || 0) + (Number(el.width) || 120)); m.maxY = Math.max(m.maxY, (Number(el.y) || 0) + (Number(el.height) || 80));
      m.gapX = Number(el.repeat?.gap_x) || m.gapX; m.gapY = Number(el.repeat?.gap_y) || m.gapY;
    });
    const repeatPosition = (el, index) => {
      const group = repeatGroupFor(el); const m = group && groupMetrics[group];
      if (!m || index === 0) return { x: Number(el.x) || 0, y: Number(el.y) || 0 };
      const mode = modeFor(el);
      return { x: (Number(el.x) || 0) + (mode === 'columns' ? index * ((m.maxX - m.minX) + m.gapX) : 0), y: (Number(el.y) || 0) + (mode === 'rows' ? index * ((m.maxY - m.minY) + m.gapY) : 0) };
    };
    elements.forEach(el => {
      const rep = el.repeat || {};
      const mode = modeFor(el);
      const count = countFor(el);
      for (let index = 0; index < count; index++) {
        const pos = repeatPosition(el, index);
        const x = pos.x;
        const y = pos.y;
        designW = Math.max(designW, x + (Number(el.width) || 120));
        designH = Math.max(designH, y + (Number(el.height) || 80));
      }
    });
    const variableText = (el, index) => {
      const props = el.props || {};
      const key = props.variable_key || 'current_temp';
      const source = props.variable_source || 'weather';
      if (source === 'date' || key === 'full_date') {
        const d = new Date(); d.setDate(d.getDate() + index);
        return d.toLocaleDateString(undefined, { weekday: 'long', month: 'short', day: 'numeric' });
      }
      if (key === 'current_temp') return index === 0 ? (units === '°F' ? '72°F' : '22°C') : (units === '°F' ? `${72 - index}°F` : `${22 - index}°C`);
      if (key === 'high_temp') return `High ${units === '°F' ? 76 - index : 24 - index}${units}`;
      if (key === 'low_temp') return `Low ${units === '°F' ? 58 - index : 14 - index}${units}`;
      if (key === 'weather_description') return ['Clear', 'Partly cloudy', 'Cloudy', 'Rain'][index % 4];
      if (key === 'weather_icon') return '';
      if (key === 'wind_speed') return `Wind ${5 + index} mph`;
      if (key === 'precip_chance') return `${index * 10}%`;
      return props.default_text || props.label || key;
    };
    const html = elements.slice().sort((a, b) => (Number(a.z) || 0) - (Number(b.z) || 0)).flatMap(el => {
      const rep = el.repeat || {};
      const mode = modeFor(el);
      const count = countFor(el);
      return Array.from({ length: count }, (_, index) => {
        const props = el.props || {};
        const pos = repeatPosition(el, index);
        const x = pos.x;
        const y = pos.y;
        const style = `position:absolute;left:${x / designW * 100}%;top:${y / designH * 100}%;width:${(Number(el.width) || 120) / designW * 100}%;height:${(Number(el.height) || 80) / designH * 100}%;z-index:${Number(el.z) || 0};display:flex;align-items:center;justify-content:${props.align === 'left' ? 'flex-start' : props.align === 'right' ? 'flex-end' : 'center'};overflow:hidden;box-sizing:border-box;color:${this.esc(props.color || '#fff')};font-size:${this.templateEditorFontCss(Number(props.font_size) || 24, designW)};`;
        let content = '';
        if (el.type === 'text') content = this.esc(props.text || 'Text box');
        else if (['variable_text', 'variable_icon', 'variable', 'date'].includes(el.type)) {
          const variableProps = props;
          const variableKey = variableProps.variable_key || '';
          const variableSource = variableProps.variable_source || 'weather';
          const baseOffset = Number(variableProps.weather_day_offset ?? variableProps.day_offset) || 0;
          const marker = ` data-weather-template-value="1" data-weather-key="${this.esc(variableKey)}" data-weather-source="${this.esc(variableSource)}" data-weather-day-offset="${baseOffset + index}"`;
          const sampleCode = [2, 3, 61, 80][index % 4];
          const iconMarkerStyle = variableKey === 'weather_icon' ? ' style="display:block;width:100%;height:100%;"' : '';
          content = `<span${marker}${iconMarkerStyle}>${variableKey === 'weather_icon' ? this._weatherIconHtml(sampleCode, false, p) : this.esc(variableText(el, index))}</span>`;
        }
        else if (el.type === 'image') {
          const imageBaseRadius = Number(props.radius ?? 0) || 0;
          const imageRadius = `${Number(props.radius_tl ?? imageBaseRadius)||0}px ${Number(props.radius_tr ?? imageBaseRadius)||0}px ${Number(props.radius_br ?? imageBaseRadius)||0}px ${Number(props.radius_bl ?? imageBaseRadius)||0}px`;
          const imageFit = ['contain', 'cover', 'fill', 'none', 'scale-down'].includes(props.fit) ? props.fit : 'contain';
          content = props.url ? `<img src="${this.esc(props.url)}" style="width:100%;height:100%;object-fit:${imageFit};border-radius:${imageRadius};">` : '🖼️';
        }
        else if (el.type === 'rectangle') content = `<span style="position:absolute;inset:0;background:${this.esc(props.color || '#1e293b')};opacity:${Math.max(0, Math.min(100, Number(props.opacity ?? 100))) / 100};border-radius:${Number(props.radius) || 0}px;"></span>`;
        else content = this.esc(props.label || el.type || '');
        return `<div class="editor-weather-template-element" style="${style}">${content}</div>`;
      }).join('');
    }).join('');
    return `<div class="editor-weather-template-preview" data-weather-preview-id="${this.esc(w.id)}" style="position:relative;display:block;flex:1 1 auto;align-self:stretch;min-height:100%;width:100%;overflow:visible;container-type:inline-size;background:rgba(15,23,42,.35);">${html}</div>`;
  },

  async _refreshCalendarEditorPreviews() {
    const widgets = (this.currentLayout?.widgets || []).filter(w => w.type === 'calendar' && (w.props?.ical_url || w.props?.ical || w.props?.calendar_url));
    for (const w of widgets) {
      const root = document.querySelector(`[data-calendar-preview-id="${CSS.escape(w.id)}"]`);
      if (!root) continue;
      const url = w.props.ical_url || w.props.ical || w.props.calendar_url;
      try {
        const response = await fetch(url);
        if (!response.ok) throw new Error('iCal request failed');
        const raw = (await response.text()).replace(/\\r?\\n[ \\t]/g, '');
        const events = [...raw.matchAll(/BEGIN:VEVENT([\\s\\S]*?)END:VEVENT/g)].map(m => {
          const block = m[1];
          const date = (block.match(/DTSTART(?:;[^:]+)?:([0-9]{8})/) || [])[1];
          const summary = (block.match(/SUMMARY:(.*)/) || [,'iCal event'])[1].trim();
          if (!date) return null;
          const d = new Date(Number(date.slice(0,4)), Number(date.slice(4,6))-1, Number(date.slice(6,8)));
          return { date: d, summary };
        }).filter(Boolean).sort((a,b) => a.date - b.date).slice(0, Math.max(1, Math.min(10, Number(w.props.max_events) || 5)));
        root.querySelectorAll('.editor-calendar-row,.editor-calendar-empty').forEach(x => x.remove());
        const status = root.querySelector('.editor-calendar-status');
        const html = events.length ? events.map(e => `<div class="editor-calendar-row"><b>${this.esc(e.date.toLocaleDateString(undefined,{month:'short',day:'numeric'}))}</b><span>${this.esc(e.summary)}</span></div>`).join('') : '<div class="editor-calendar-empty">No iCal events found</div>';
        if (status) status.insertAdjacentHTML('beforebegin', html);
        if (status) status.textContent = 'iCal preview';
      } catch (_) {
        const status = root.querySelector('.editor-calendar-status');
        root.querySelectorAll('.editor-calendar-row,.editor-calendar-empty').forEach(x => x.remove());
        if (status) {
          status.insertAdjacentHTML('beforebegin', '<div class="editor-calendar-empty">iCal unavailable</div>');
          status.textContent = 'iCal unavailable · check the URL or server access';
        }
      }
    }
  },

  _calendarTemplatePreviewHtml(w) {
    const p = w.props || {};
    const elements = Array.isArray(p.template_elements) ? p.template_elements : [];
    if (!elements.length) return null;
    let designW = Math.max(1, Number(p.template_width) || 760), designH = Math.max(1, Number(p.template_height) || 428);
    const groupFor = el => el.repeat?.group || el.repeat?.group_id || el.repeat_group || el.repeatGroup || null;
    const modeFor = el => groupFor(el) && (!el.repeat?.mode || el.repeat.mode === 'none') ? 'rows' : (el.repeat?.mode || 'none');
    const countFor = el => groupFor(el) ? Math.max(1, Math.min(30, Number(p.max_events) || Number(el.repeat?.count) || 5)) : Math.max(1, Math.min(30, Number(el.repeat?.count) || 1));
    const groups = {};
    elements.forEach(el => { const g = groupFor(el); if (!g) return; const r = el.repeat || {}; const m = groups[g] || (groups[g] = {minX:Infinity,minY:Infinity,maxX:-Infinity,maxY:-Infinity,gapX:Number(r.gap_x)||0,gapY:Number(r.gap_y)||0}); const x=Number(el.x)||0,y=Number(el.y)||0,wid=Number(el.width)||120,hei=Number(el.height)||80; m.minX=Math.min(m.minX,x);m.minY=Math.min(m.minY,y);m.maxX=Math.max(m.maxX,x+wid);m.maxY=Math.max(m.maxY,y+hei);m.gapX=Number(r.gap_x)||m.gapX;m.gapY=Number(r.gap_y)||m.gapY; });
    const posFor = (el, i) => { const m = groups[groupFor(el)]; if (!m || !i) return {x:Number(el.x)||0,y:Number(el.y)||0}; const mode=modeFor(el); return {x:(Number(el.x)||0)+(mode==='columns'?i*((m.maxX-m.minX)+m.gapX):0), y:(Number(el.y)||0)+(mode==='rows'?i*((m.maxY-m.minY)+m.gapY):0)}; };
    elements.forEach(el => { for (let i=0;i<countFor(el);i++){const q=posFor(el,i);designW=Math.max(designW,q.x+(Number(el.width)||120));designH=Math.max(designH,q.y+(Number(el.height)||80));} });
    const sample = (el, i) => { const q=el.props||{}, key=q.variable_key||''; if (q.variable_source==='date'||key==='full_date') { const d=new Date();d.setDate(d.getDate()+i);return d.toLocaleDateString(undefined,{weekday:'long',month:'short',day:'numeric'}); } if(key==='calendar_event_time')return '9:00 AM'; if(key==='calendar_event_location')return 'United States'; if(key==='calendar_event_title')return ['New Year’s Day','Martin Luther King Jr. Day','Presidents’ Day','Memorial Day','Independence Day'][i%5]; return q.default_text||q.label||key; };
    const html=elements.slice().sort((a,b)=>(Number(a.z)||0)-(Number(b.z)||0)).map(el=>Array.from({length:countFor(el)},(_,i)=>{const q=el.props||{},sampleIndex=groupFor(el)?i+1:i,pos=posFor(el,i),align=q.align||'center',justify=align==='left'?'flex-start':align==='right'?'flex-end':'center',style=`position:absolute;left:${pos.x/designW*100}%;top:${pos.y/designH*100}%;width:${(Number(el.width)||120)/designW*100}%;height:${(Number(el.height)||80)/designH*100}%;z-index:${Number(el.z)||0};display:flex;align-items:center;justify-content:${justify};overflow:hidden;box-sizing:border-box;color:${this.esc(q.color||'#fff')};font-size:${this.templateEditorFontCss(Number(q.font_size)||24, designW)};`;let content='';if(el.type==='text')content=this.esc(q.text||'Text box');else if(['variable_text','variable_icon','variable','date'].includes(el.type))content=this.esc(sample(el,sampleIndex));else if(el.type==='image')content=q.url?`<img src="${this.esc(q.url)}" style="max-width:100%;max-height:100%;object-fit:contain;">`:'🖼️';else if(el.type==='rectangle')content=`<span style="position:absolute;inset:0;background:${this.esc(q.color||'#1e293b')};opacity:${Math.max(0,Math.min(100,Number(q.opacity??100)))/100};border-radius:${Number(q.radius)||0}px;"></span>`;else content=this.esc(q.label||el.type||'');return `<div style="${style}">${content}</div>`;}).join('')).join('');
    return `<div class="editor-calendar-template-preview" style="position:relative;width:100%;height:100%;overflow:hidden;container-type:inline-size;background:rgba(15,23,42,.35);">${html}</div>`;
  },

  _calendarEditorPreviewHtml(w) {
    const p = w.props || {};
    const max = Math.max(1, Math.min(10, Number(p.max_events) || 5));
    const year = new Date().getFullYear();
    const nthMonday = (month, nth) => { const d = new Date(year, month, 1); d.setDate(1 + ((8 - d.getDay()) % 7) + (nth - 1) * 7); return d; };
    const lastMonday = month => { const d = new Date(year, month + 1, 0); d.setDate(d.getDate() - ((d.getDay() + 6) % 7)); return d; };
    const nthThursday = (month, nth) => { const d = new Date(year, month, 1); d.setDate(1 + ((11 - d.getDay()) % 7) + (nth - 1) * 7); return d; };
    const holidays = [
      ['New Year’s Day', new Date(year, 0, 1)], ['Martin Luther King Jr. Day', nthMonday(0, 3)], ['Presidents’ Day', nthMonday(1, 3)],
      ['Memorial Day', lastMonday(4)], ['Juneteenth', new Date(year, 5, 19)], ['Independence Day', new Date(year, 6, 4)],
      ['Labor Day', nthMonday(8, 1)], ['Columbus Day', nthMonday(9, 2)], ['Veterans Day', new Date(year, 10, 11)],
      ['Thanksgiving Day', nthThursday(10, 4)], ['Christmas Day', new Date(year, 11, 25)]
    ].filter(x => x[1] >= new Date(new Date().setHours(0, 0, 0, 0))).sort((a, b) => a[1] - b[1]).slice(0, max);
    const source = p.ical_url || p.ical || p.calendar_url;
    const rows = holidays.map(([name, date]) => `<div class="editor-calendar-row"><b>${this.esc(date.toLocaleDateString(undefined, { month: 'short', day: 'numeric' }))}</b><span>${this.esc(name)}</span></div>`).join('');
    return `<div class="editor-calendar-preview" data-calendar-preview-id="${this.esc(w.id)}"><div class="editor-calendar-title">📆 ${this.esc(source ? 'iCal Calendar' : 'US Holidays')}</div>${rows || '<div class="editor-calendar-empty">No upcoming events</div>'}${source ? `<small class="editor-calendar-status">Source configured: ${this.esc(source)}</small>` : '<small class="editor-calendar-status">Editor preview uses US holidays</small>'}</div>`;
  },

  _formatEditorClock(format) {
    const now = new Date();
    const pad = value => String(value).padStart(2, '0');
    const h24 = now.getHours();
    const h12 = h24 % 12 || 12;
    const replacements = {
      HH: pad(h24), hh: pad(h12), mm: pad(now.getMinutes()), ss: pad(now.getSeconds()),
      A: h24 >= 12 ? 'PM' : 'AM'
    };
    return String(format || 'HH:mm').replace(/HH|hh|mm|ss|A/g, token => replacements[token]);
  },

  _formatEditorCountdown(widget) {
    const p = widget?.props || {};
    const mode = p.countdown_mode || 'date';
    if (mode === 'duration') {
      const value = Math.max(1, Number(p.duration_value) || 1);
      const unit = p.duration_unit || 'minutes';
      const seconds = Math.round(value * ({ seconds: 1, minutes: 60, hours: 3600, days: 86400 }[unit] || 60));
      return this._formatCountdownRemaining(seconds * 1000);
    }
    const target = new Date(p.targetDate || '').getTime();
    if (!Number.isFinite(target) || target <= 0) return 'Set date & time';
    const remaining = target - Date.now();
    if (remaining <= 0) return p.expiry_text || 'Expired';
    return this._formatCountdownRemaining(remaining);
  },

  _formatCountdownRemaining(milliseconds) {
    const seconds = Math.max(0, Math.floor(milliseconds / 1000));
    const days = Math.floor(seconds / 86400);
    const hours = Math.floor((seconds % 86400) / 3600);
    const minutes = Math.floor((seconds % 3600) / 60);
    const secs = seconds % 60;
    if (days) return `${days}d ${hours}h`;
    if (hours) return `${hours}h ${minutes}m`;
    if (minutes) return `${minutes}m ${secs}s`;
    return `${secs}s`;
  },

  _formatEditorDate(format) {
    const now = new Date();
    const days = ['Sunday', 'Monday', 'Tuesday', 'Wednesday', 'Thursday', 'Friday', 'Saturday'];
    const months = ['January', 'February', 'March', 'April', 'May', 'June', 'July', 'August', 'September', 'October', 'November', 'December'];
    const day = String(now.getDate()).padStart(2, '0');
    const month = String(now.getMonth() + 1).padStart(2, '0');
    const year = now.getFullYear();
    switch (format || 'dddd, MMMM D') {
      case 'MMMM D, YYYY': return `${months[now.getMonth()]} ${now.getDate()}, ${year}`;
      case 'MM/DD/YYYY': return `${month}/${day}/${year}`;
      default: return `${days[now.getDay()]}, ${months[now.getMonth()]} ${now.getDate()}`;
    }
  },

  _refreshEditorClocks() {
    if (!this.el?.canvas) return;
    this.el.canvas.querySelectorAll('[data-editor-clock-id]').forEach(node => {
      const widget = (this.currentLayout?.widgets || []).find(item => item.id === node.dataset.editorClockId);
      if (widget) node.textContent = this._formatEditorClock(widget.props?.format);
    });
    this.el.canvas.querySelectorAll('[data-editor-date-id]').forEach(node => {
      const widget = (this.currentLayout?.widgets || []).find(item => item.id === node.dataset.editorDateId);
      if (widget) node.textContent = this._formatEditorDate(widget.props?.format);
    });
    this.el.canvas.querySelectorAll('[data-editor-countdown-id]').forEach(node => {
      const widget = (this.currentLayout?.widgets || []).find(item => item.id === node.dataset.editorCountdownId);
      if (widget) node.textContent = this._formatEditorCountdown(widget);
    });
  },

  editorFontCss(size) {
    const value = Math.max(1, Number(size) || 1);
    const designWidth = this.currentLayout?.canvas_orientation === 'portrait' ? 540 : 960;
    return `clamp(6px,calc(${value / designWidth * 100}cqw),${value * 4}px)`;
  },

  templateEditorFontCss(size, templateWidth, templateHeight) {
    const value = Math.max(1, Number(size) || 1);
    const width = Math.max(1, Number(templateWidth) || 760);
    const height = Math.max(1, Number(templateHeight) || width * 9 / 16);
    return `clamp(4px,min(calc(${value / width * 100}cqw),calc(${value / height * 100}cqh)),${value * 4}px)`;
  },

  _fitTrafficTemplateText() {
    const roots = Array.from(this.el.canvas.querySelectorAll('.editor-traffic-template-preview'));
    roots.forEach(root => {
      const nodes = Array.from(root.querySelectorAll('[data-ripley-fit-text="1"]'));
      if (!nodes.length) return;
      const run = () => {
        const groups = {};
        nodes.forEach(node => {
          if (!node.isConnected || node.clientWidth <= 0 || node.clientHeight <= 0) return;
          let base = Number(node.dataset.ripleyFitBaseFont);
          if (!Number.isFinite(base) || base <= 0) {
            base = parseFloat(getComputedStyle(node).fontSize) || 26;
            node.dataset.ripleyFitBaseFont = String(base);
          }
          const fits = size => {
            node.style.fontSize = `${size}px`;
            return node.scrollWidth <= node.clientWidth + 1 && node.scrollHeight <= node.clientHeight + 1;
          };
          let low = 4, high = base;
          if (!fits(high)) {
            if (!fits(low)) high = low;
            else for (let i = 0; i < 10; i++) {
              const mid = (low + high) / 2;
              if (fits(mid)) low = mid; else high = mid;
            }
          } else low = high;
          const ratio = Math.max(0.1, Math.min(1, low / base));
          const group = node.dataset.ripleyFitGroup || node.dataset.ripleyFitBaseFont;
          groups[group] = groups[group] == null ? ratio : Math.min(groups[group], ratio);
        });
        nodes.forEach(node => {
          const base = Number(node.dataset.ripleyFitBaseFont);
          const ratio = groups[node.dataset.ripleyFitGroup || node.dataset.ripleyFitBaseFont];
          if (Number.isFinite(base) && Number.isFinite(ratio)) node.style.fontSize = `${Math.max(4, base * ratio)}px`;
        });
      };
      if (!root.__ripleyTrafficFitObserver && typeof ResizeObserver !== 'undefined') {
        root.__ripleyTrafficFitObserver = new ResizeObserver(() => requestAnimationFrame(run));
        root.__ripleyTrafficFitObserver.observe(root);
      }
      requestAnimationFrame(run);
    });
  },

  _keyboardPreviewHtml(w) {
    const p = w.props || {};
    const mode = p.mode === 'number' ? 'number' : (p.mode === 'full_keyboard' ? 'full_keyboard' : 'full');
    const rows = mode === 'number'
      ? [['1', '2', '3'], ['4', '5', '6'], ['7', '8', '9'], ['−', '0', '.', '⌫'], ['Clear', 'Enter']]
      : mode === 'full_keyboard'
        ? [['`', '1', '2', '3', '4', '5', '6', '7', '8', '9', '0', '-', '='], ['Q', 'W', 'E', 'R', 'T', 'Y', 'U', 'I', 'O', 'P'], ['A', 'S', 'D', 'F', 'G', 'H', 'J', 'K', 'L'], ['Z', 'X', 'C', 'V', 'B', 'N', 'M', ',', '.', '/'], ['Shift', 'Space', '⌫', 'Enter']]
        : [['Q', 'W', 'E', 'R', 'T', 'Y', 'U', 'I', 'O', 'P'], ['A', 'S', 'D', 'F', 'G', 'H', 'J', 'K', 'L'], ['Z', 'X', 'C', 'V', 'B', 'N', 'M', '⌫'], ['Space', 'Clear', 'Enter']];
    const label = this.esc(p.label || 'Keyboard');
    const keyHtml = key => `<span style="flex:1;min-width:0;display:flex;align-items:center;justify-content:center;border-radius:6px;background:#334155;color:#fff;font:700 clamp(8px,1.6cqw,18px) system-ui,sans-serif;">${this.esc(key)}</span>`;
    return `<div style="width:100%;height:100%;display:flex;flex-direction:column;gap:.25rem;padding:.35rem;box-sizing:border-box;background:rgba(15,23,42,.94);border:1px solid rgba(148,163,184,.45);border-radius:8px;pointer-events:none;overflow:hidden;">
      <div style="color:#e2e8f0;font:700 clamp(9px,1.8cqw,16px) system-ui,sans-serif;display:flex;justify-content:space-between;align-items:center;flex:0 0 auto;"><span>${label}</span><small style="color:#94a3b8;font-weight:600;">${mode === 'number' ? 'number pad' : 'input keys'}</small></div>
      <div style="display:flex;flex-direction:column;gap:.25rem;flex:1;min-height:0;">${rows.map(row => `<div style="display:flex;gap:.25rem;flex:1;min-height:0;">${row.map(keyHtml).join('')}</div>`).join('')}</div>
    </div>`;
  },

  layoutWidgetInner(w) {
    const p = w.props || {};
    // Normal (display) widgets
    if (w.type === 'text') {
      const fontFamily = this.esc(p.font_family || p.fontFamily || 'sans-serif');
      return `<div style="font-size:${this.editorFontCss(p.font_size || p.fontSize || 16)};color:${this.esc(p.color || '#fff')};font-family:${fontFamily};padding:4px;">${this.esc(p.content || '')}</div>`;
    }
    if (w.type === 'image') {
      const fit = ['cover', 'contain', 'fill', 'none', 'scale-down'].includes(p.fit) ? p.fit : 'cover';
      const baseRadius = Number(p.radius == null ? 0 : p.radius) || 0;
      const radius = `${Number(p.radius_tl == null ? baseRadius : p.radius_tl)||0}px ${Number(p.radius_tr == null ? baseRadius : p.radius_tr)||0}px ${Number(p.radius_br == null ? baseRadius : p.radius_br)||0}px ${Number(p.radius_bl == null ? baseRadius : p.radius_bl)||0}px`;
      const hasImage = Boolean(p.url);
      return `<div style="width:100%;height:100%;background:${hasImage ? 'transparent' : '#222'};display:flex;align-items:center;justify-content:center;font-size:12px;color:#888;overflow:hidden;border-radius:${radius};">${hasImage ? `<img src="${this.esc(p.url)}" style="display:block;width:100%;height:100%;object-fit:${fit};border-radius:${radius};">` : '📷 Image'}</div>`;
    }
    if (w.type === 'video') return `<div style="width:100%;height:100%;background:#111;display:flex;align-items:center;justify-content:center;font-size:12px;color:#666;">▶ Video</div>`;
    if (w.type === 'clock') {
      const fontFamily = this.esc(p.font_family || p.fontFamily || 'sans-serif');
      const color = this.esc(p.color || '#ffffff');
      return `<div data-editor-clock-id="${this.esc(w.id)}" style="font-size:${this.editorFontCss(Math.max(6, Math.min(400, Number(p.font_size) || 36)))};font-weight:bold;color:${color};font-family:${fontFamily};text-align:center;line-height:1;">${this.esc(this._formatEditorClock(p.format))}</div>`;
    }
    if (w.type === 'date') {
      const fontFamily = this.esc(p.font_family || p.fontFamily || 'sans-serif');
      const color = this.esc(p.color || '#cccccc');
      return `<div data-editor-date-id="${this.esc(w.id)}" style="font-size:${this.editorFontCss(Math.max(6, Math.min(400, Number(p.font_size) || 24)))};color:${color};font-family:${fontFamily};text-align:center;line-height:1.2;">${this.esc(this._formatEditorDate(p.format))}</div>`;
    }
    if (w.type === 'weather') {
      const templatePreview = this._weatherTemplatePreviewHtml(w);
      if (templatePreview) return templatePreview;
      const units = String(p.units || 'metric') === 'imperial' ? '°F' : '°C';
      const city = p.city || p.location || 'Choose a location';
      const days = Math.max(1, Math.min(5, Number(p.shown_days) || 4));
      return `<div class="editor-weather-preview" data-weather-preview-id="${this.esc(w.id)}"><div class="editor-weather-location">${this.esc(city)}</div><div class="editor-weather-current"><span class="editor-weather-icon" style="display:inline-flex;width:1.4em;height:1.4em;flex:0 0 1.4em;">${this._weatherIconHtml(2, false, p)}</span><span class="editor-weather-temp">—${units}</span></div><div class="editor-weather-forecast">${Array.from({length: days}, (_, i) => `<div><b>${i === 0 ? 'Today' : 'Day ' + (i + 1)}</b><span>— / —${units}</span></div>`).join('')}</div><small class="editor-weather-status">Loading weather…</small></div>`;
    }
    if (w.type === 'traffic') {
      const templatePreview = this._trafficTemplatePreviewHtml(w);
      if (templatePreview) return templatePreview;
      const area = p.area_label || 'Choose a traffic area';
      const defaultTrafficKey = p.traffic_variable || 'traffic_summary';
      const defaultTrafficValues = { traffic_summary: '3 traffic events', traffic_incidents: '1', traffic_crashes_closures: 'Crash on I-80 eastbound', traffic_closures: '1', traffic_delays: '1' };
      const defaultTrafficValue = defaultTrafficValues[defaultTrafficKey] || defaultTrafficValues.traffic_summary;
      return `<div class="editor-traffic-preview" data-traffic-preview-id="${this.esc(w.id)}" style="width:100%;height:100%;padding:.5rem;box-sizing:border-box;overflow:hidden;color:#fff;background:rgba(30,41,59,.55);"><div style="font-weight:700;">🚧 ${this.esc(area)}</div><div class="editor-traffic-default-variable" style="font-size:1.15em;font-weight:700;margin-top:.35rem;">${this.esc(defaultTrafficValue)}</div><div class="editor-traffic-list" style="margin-top:.4rem;font-size:.8em;"><div>Select an area to load road events</div></div><small class="editor-traffic-status" style="display:block;margin-top:.35rem;color:#94a3b8;">Traffic preview</small></div>`;
    }
    if (w.type === 'countdown') {
      const fontSize = this.editorFontCss(Math.max(8, Math.min(160, Number(p.font_size) || 48)));
      const fontFamily = this.esc(p.font_family || p.fontFamily || 'sans-serif');
      const color = this.esc(p.color || '#ffffff');
      const countdownText = this.esc(this._formatEditorCountdown(w));
      return `<div style="width:100%;height:100%;display:flex;align-items:center;justify-content:center;overflow:hidden;color:${color};font-family:${fontFamily};text-align:center;"><div data-editor-countdown-id="${this.esc(w.id)}" style="font-size:${fontSize};font-weight:800;line-height:1;white-space:nowrap;">${countdownText}</div></div>`;
    }
    if (w.type === 'sound') return `<div style="font-size:18px;font-weight:bold;color:#8ab4f8;text-align:center;">🔊 Sound</div>`;
    if (w.type === 'embed') return `<div style="font-size:12px;color:#888;text-align:center;padding:4px;">🔗 Embed</div>`;
    if (w.type === 'calendar') {
      const templatePreview = this._calendarTemplatePreviewHtml(w);
      if (templatePreview) return templatePreview;
      return this._calendarEditorPreviewHtml(w);
    }
    if (w.type === 'news') return `<div style="font-size:12px;color:#888;text-align:center;padding:4px;overflow:hidden;">📰 News Ticker</div>`;
    if (w.type === 'rectangle') {
      // Mirror the player's `w.widget_type === 'rectangle'` branch so the
      // editor canvas shows a faithful preview of what the runtime
      // renders. Shape mode drives both the border-radius and an
      // optional CSS clip-path for the geometric shapes the player
      // supports (triangle / diamond / hexagon / star / parallelogram /
      // arrow / plus / minus / x — see ripleyShapeClipPath in player.js).
      const sm = p.shape_mode || 'rounded';
      const op = Math.max(0, Math.min(100, Number(p.opacity == null ? 100 : p.opacity))) / 100;
      let radiusCss;
      if (sm === 'circle') radiusCss = '50%';
      else if (sm === 'pill') radiusCss = '999px';
      else {
        const base = Number(p.radius == null ? 0 : p.radius) || 0;
        const rtl = Number(p.radius_tl == null ? base : p.radius_tl) || 0;
        const rtr = Number(p.radius_tr == null ? base : p.radius_tr) || 0;
        const rbr = Number(p.radius_br == null ? base : p.radius_br) || 0;
        const rbl = Number(p.radius_bl == null ? base : p.radius_bl) || 0;
        radiusCss = rtl+'px '+rtr+'px '+rbr+'px '+rbl+'px';
      }
      const clipMap = {
        triangle: 'polygon(50% 0%, 0% 100%, 100% 100%)',
        diamond: 'polygon(50% 0%, 100% 50%, 50% 100%, 0% 50%)',
        hexagon: 'polygon(25% 0%, 75% 0%, 100% 50%, 75% 100%, 25% 100%, 0% 50%)',
        star: 'polygon(50% 0%, 61% 35%, 98% 35%, 68% 57%, 79% 91%, 50% 70%, 21% 91%, 32% 57%, 2% 35%, 39% 35%)',
        parallelogram: 'polygon(20% 0%, 100% 0%, 80% 100%, 0% 100%)',
        arrow: 'polygon(0% 25%, 60% 25%, 60% 0%, 100% 50%, 60% 100%, 60% 75%, 0% 75%)',
        plus: 'polygon(35% 0%, 65% 0%, 65% 35%, 100% 35%, 100% 65%, 65% 65%, 65% 100%, 35% 100%, 35% 65%, 0% 65%, 0% 35%, 35% 35%)',
        minus: 'polygon(0% 35%, 100% 35%, 100% 65%, 0% 65%)',
        x: 'polygon(20% 0%, 50% 30%, 80% 0%, 100% 20%, 70% 50%, 100% 80%, 80% 100%, 50% 70%, 20% 100%, 0% 80%, 30% 50%, 0% 20%)',
      };
      const clip = clipMap[sm] || '';
      const lineStyle = sm === 'line' ? 'height:' + (Number(p.line_thickness == null ? 6 : p.line_thickness) || 6) + 'px;' : '';
      return `<div style="width:100%;height:100%;background:${this.esc(p.color || '#e94560')};border-radius:${this.esc(radiusCss)};opacity:${op};${lineStyle}${clip ? 'clip-path:'+this.esc(clip)+';-webkit-clip-path:'+this.esc(clip)+';' : ''}"></div>`;
    }
    // Interactive widgets
    if (w.type === 'button') { const buttonLabel = p.action === 'page_switch' && /^go to page$/i.test(String(p.label || '')) ? 'Go to layout' : (p.label || 'Button'); return `<button class="interactive-preview-button ${this.esc(p.style || 'primary')}">${this.esc(buttonLabel)}</button>`; }
    if (w.type === 'dropdown') return `<label>${this.esc(p.label || 'Drop-down')}<select>${(p.options || []).map(o => `<option>${this.esc(o)}</option>`).join('')}</select></label>`;
    if (w.type === 'checkbox') return `<label class="interactive-check"><input type="checkbox" ${p.checked ? 'checked' : ''}> ${this.esc(p.label || 'Checkbox')}</label>`;
    if (w.type === 'checkbox-list') return `<fieldset><legend>${this.esc(p.label || 'Checkbox List')}</legend>${(p.options || []).map(o => `<label class="interactive-check"><input type="checkbox"> ${this.esc(o)}</label>`).join('')}</fieldset>`;
    if (w.type === 'radio') return `<fieldset><legend>${this.esc(p.label || 'Radio')}</legend>${(p.options || []).map(o => `<label class="interactive-check"><input name="${this.esc(w.id)}" type="radio" ${o === p.selected ? 'checked' : ''}> ${this.esc(o)}</label>`).join('')}</fieldset>`;
    if (w.type === 'textbox') return `<label>${this.esc(p.label || 'Text Box')}<input type="text" placeholder="${this.esc(p.placeholder || '')}" value="${this.esc(p.value || '')}"></label>`;
    if (w.type === 'keyboard') return this._keyboardPreviewHtml(w);
    return `<div style="font-size:12px;color:#888;">${this.esc(w.type)}</div>`;
  },

  // Snap a value to the nearest grid line if within threshold of that line.
  // v: the value (in %)
  // gridCell: grid cell size in %
  // threshold: snap threshold in % (how close to a grid line before snapping)
  // Returns snapped value
  snapToGrid(v, gridCell, threshold) {
    const nearest = Math.round(v / gridCell) * gridCell;
    if (Math.abs(v - nearest) <= threshold) return nearest;
    return v;
  },

  // Snap a proposed widget position to matching edges/centers of widgets at the same z-index.
  // candidateX/candidateY and thresholds are percentages of the canvas.
  snapToWidgetEdges(candidateX, candidateY, w, thresholdX, thresholdY) {
    const wW = Number(w.w) || 0;
    const wH = Number(w.h) || 0;
    const nx = Math.max(0, Math.min(100 - wW, candidateX));
    const ny = Math.max(0, Math.min(100 - wH, candidateY));
    const snap = this._tplSnapGrid || { enabled: true, visible: true, size: 10, snap_size: 10 };
    if (!snap.enabled) return { nx, ny, snapped: false };

    const myZ = Number(w.z) || 0;
    const threshX = Math.max(Number(thresholdX) || 0, 3);
    const threshY = Math.max(Number(thresholdY) || 0, 3);
    const others = (this.currentLayout?.widgets || [])
      .filter(other => other.id !== w.id && other.id != null && Math.abs((Number(other.z) || 0) - myZ) < 0.5)
      .map(other => {
        const x = Number(other.x) || 0;
        const y = Number(other.y) || 0;
        const width = Number(other.w) || 0;
        const height = Number(other.h) || 0;
        return {
          x: [x, x + width, x + width / 2],
          y: [y, y + height, y + height / 2],
        };
      });

    let bestX = nx;
    let bestY = ny;
    let bestDistanceX = Infinity;
    let bestDistanceY = Infinity;

    for (const other of others) {
      for (const edge of other.x) {
        const leftDistance = Math.abs(nx - edge);
        if (leftDistance <= threshX && leftDistance < bestDistanceX) {
          bestDistanceX = leftDistance;
          bestX = edge;
        }
        const rightDistance = Math.abs(nx + wW - edge);
        if (rightDistance <= threshX && rightDistance < bestDistanceX) {
          bestDistanceX = rightDistance;
          bestX = edge - wW;
        }
      }
      for (const edge of other.y) {
        const topDistance = Math.abs(ny - edge);
        if (topDistance <= threshY && topDistance < bestDistanceY) {
          bestDistanceY = topDistance;
          bestY = edge;
        }
        const bottomDistance = Math.abs(ny + wH - edge);
        if (bottomDistance <= threshY && bottomDistance < bestDistanceY) {
          bestDistanceY = bottomDistance;
          bestY = edge - wH;
        }
      }
    }

    const snappedX = Number.isFinite(bestDistanceX);
    const snappedY = Number.isFinite(bestDistanceY);
    return {
      nx: snappedX ? Math.max(0, Math.min(100 - wW, bestX)) : nx,
      ny: snappedY ? Math.max(0, Math.min(100 - wH, bestY)) : ny,
      snapped: snappedX || snappedY,
    };
  },

  bindLayoutWidgetNodes() {
    // Same as the interactive-widget rendering path, but for layout preview thumbnails
    const E = this.el;
    E.canvas.querySelectorAll('.interactive-widget[data-type="layout"]').forEach(node => {
      const id = node.dataset.id;
      node.onpointerdown = e => {
        if (e.button !== 0) return;
        e.stopPropagation();
        this.selectedWidgetId = id;
        const w = this.currentLayout?.widgets?.find(x => x.id === id);
        this.selectedWidget = w || null;
        this.paneMode = 'widget';
        const rect = E.canvas.getBoundingClientRect();
        this.dragState = null;
        this.resizeState = null;
        this.rotationState = null;
        if (e.target.classList.contains('rotation-handle')) {
          const widgetRect = node.getBoundingClientRect();
          const centerX = widgetRect.left + widgetRect.width / 2;
          const centerY = widgetRect.top + widgetRect.height / 2;
          this.rotationState = {
            id,
            startAngle: Math.atan2(e.clientY - centerY, e.clientX - centerX) * 180 / Math.PI,
            rotation: Math.max(-180, Math.min(180, Number(w?.props?.rotation) || 0)),
          };
        } else if (e.target.classList.contains('resize-handle')) {
          const resizeHandles = ['nw','ne','sw','se','n','e','s','w'];
          const side = [...e.target.classList].find(c => resizeHandles.includes(c)) || 'se';
          this.resizeState = {
            id, side,
            startX: e.clientX, startY: e.clientY,
            x: w?.x || 0, y: w?.y || 0,
            w: w?.w || 20, h: w?.h || 10,
            rect,
          };
        } else if (e.target.classList.contains('interactive-widget-label-arrow')) {
          const resizeHandles = ['nw','ne','sw','se','n','e','s','w'];
          const side = [...e.target.classList].find(c => resizeHandles.includes(c)) || 'se';
          this.resizeState = {
            id, side,
            startX: e.clientX, startY: e.clientY,
            x: w?.x || 0, y: w?.y || 0,
            w: w?.w || 20, h: w?.h || 10,
            rect,
          };
        } else {
          this.dragState = { id, startX: e.clientX, startY: e.clientY, x: w?.x || 0, y: w?.y || 0, rect };
        }
        node.setPointerCapture(e.pointerId);
        this.renderProps();
      };
      node.onpointermove = e => {
        const w = this.currentLayout?.widgets?.find(x => x.id === id);
        if (!w) return;
        if (this.rotationState?.id === id) {
          const widgetRect = node.getBoundingClientRect();
          const centerX = widgetRect.left + widgetRect.width / 2;
          const centerY = widgetRect.top + widgetRect.height / 2;
          const angle = Math.atan2(e.clientY - centerY, e.clientX - centerX) * 180 / Math.PI;
          let rotation = this.rotationState.rotation + (angle - this.rotationState.startAngle);
          while (rotation > 180) rotation -= 360;
          while (rotation < -180) rotation += 360;
          w.props = w.props || {};
          w.props.rotation = Math.max(-180, Math.min(180, rotation));
          node.style.transform = `rotate(${w.props.rotation}deg)`;
          const input = this.el.props?.querySelector('[data-prop="rotation"]');
          if (input) input.value = String(Math.round(w.props.rotation));
          return;
        }
        const s = this.dragState?.id === id ? this.dragState : this.resizeState?.id === id ? this.resizeState : null;
        if (!s) return;
        const dx = ((e.clientX - s.startX) / s.rect.width) * 100;
        const dy = ((e.clientY - s.startY) / s.rect.height) * 100;
        const snap = this._tplSnapGrid || { enabled: true, visible: true, size: 10, snap_size: 10 };
        if (this.dragState?.id === id) {
          let nx = Math.max(0, Math.min(100 - w.w, s.x + dx));
          let ny = Math.max(0, Math.min(100 - w.h, s.y + dy));
          if (snap.enabled) {
            // Convert pixel snap settings to %: size/snap_size are in px, rect in px
            const gridCellX = (snap.size / s.rect.width) * 100;
            const gridCellY = (snap.size / s.rect.height) * 100;
            const threshX = (snap.snap_size / s.rect.width) * 100;
            const threshY = (snap.snap_size / s.rect.height) * 100;
            // Snap to widget edges first (higher priority), then snap to grid
            const edgeSnap = this.snapToWidgetEdges(nx, ny, w, threshX, threshY);
            if (edgeSnap.snapped) {
              nx = edgeSnap.nx;
              ny = edgeSnap.ny;
            } else {
              nx = this.snapToGrid(nx, gridCellX, threshX);
              ny = this.snapToGrid(ny, gridCellY, threshY);
            }
          }
          w.x = nx;
          w.y = ny;
        } else if (this.resizeState?.id === id) {
          const c = s.side || 'se';
          if (c === 'nw') {
            const nx0 = Math.max(0, Math.min(s.x + s.w - 6, s.x + dx));
            const ny0 = Math.max(0, Math.min(s.y + s.h - 5, s.y + dy));
            const gridCellX = snap.enabled ? (snap.size / s.rect.width) * 100 : 0;
            const gridCellY = snap.enabled ? (snap.size / s.rect.height) * 100 : 0;
            const threshX = snap.enabled ? (snap.snap_size / s.rect.width) * 100 : 0;
            const threshY = snap.enabled ? (snap.snap_size / s.rect.height) * 100 : 0;
            w.w = Math.max(6, s.w + (s.x - nx0));
            w.h = Math.max(5, s.h + (s.y - ny0));
            w.x = snap.enabled ? this.snapToGrid(nx0, gridCellX, threshX) : nx0;
            w.y = snap.enabled ? this.snapToGrid(ny0, gridCellY, threshY) : ny0;
          } else if (c === 'ne') {
            const ny0 = Math.max(0, Math.min(s.y + s.h - 5, s.y + dy));
            const gridCellY = snap.enabled ? (snap.size / s.rect.height) * 100 : 0;
            const threshY = snap.enabled ? (snap.snap_size / s.rect.height) * 100 : 0;
            w.w = Math.max(6, Math.min(100 - s.x, s.w + dx));
            w.h = Math.max(5, s.h + (s.y - ny0));
            w.y = snap.enabled ? this.snapToGrid(ny0, gridCellY, threshY) : ny0;
          } else if (c === 'sw') {
            const nx0 = Math.max(0, Math.min(s.x + s.w - 6, s.x + dx));
            const gridCellX = snap.enabled ? (snap.size / s.rect.width) * 100 : 0;
            const threshX = snap.enabled ? (snap.snap_size / s.rect.width) * 100 : 0;
            w.w = Math.max(6, s.w + (s.x - nx0));
            w.h = Math.max(5, Math.min(100 - s.y, s.h + dy));
            w.x = snap.enabled ? this.snapToGrid(nx0, gridCellX, threshX) : nx0;
          } else if (c === 'w') {
            const nx0 = Math.max(0, Math.min(s.x + s.w - 6, s.x + dx));
            const gridCellX = snap.enabled ? (snap.size / s.rect.width) * 100 : 0;
            const threshX = snap.enabled ? (snap.snap_size / s.rect.width) * 100 : 0;
            w.w = Math.max(6, s.w + (s.x - nx0));
            w.x = snap.enabled ? this.snapToGrid(nx0, gridCellX, threshX) : nx0;
          } else if (c === 'n') {
            const ny0 = Math.max(0, Math.min(s.y + s.h - 5, s.y + dy));
            const gridCellY = snap.enabled ? (snap.size / s.rect.height) * 100 : 0;
            const threshY = snap.enabled ? (snap.snap_size / s.rect.height) * 100 : 0;
            w.h = Math.max(5, s.h + (s.y - ny0));
            w.y = snap.enabled ? this.snapToGrid(ny0, gridCellY, threshY) : ny0;
          } else if (c === 'e') {
            w.w = Math.max(6, Math.min(100 - s.x, s.w + dx));
          } else if (c === 's') {
            w.h = Math.max(5, Math.min(100 - s.y, s.h + dy));
          } else {
            w.w = Math.max(6, Math.min(100 - s.x, s.w + dx));
            w.h = Math.max(5, Math.min(100 - s.y, s.h + dy));
          }
        }
        node.style.left = w.x + '%';
        node.style.top = w.y + '%';
        node.style.width = w.w + '%';
        node.style.height = w.h + '%';
      };
      node.onpointerup = () => {
        this.dragState = null;
        this.resizeState = null;
        this.rotationState = null;
        this.saveCurrent();
        this.renderCanvas();
      };
      node.oncontextmenu = e => {
        e.preventDefault();
        e.stopPropagation();
        this.closeContextMenu();
        this.showContextMenu(e.clientX, e.clientY, id);
      };
    });
  },

  // Make sure there's an active layout. If the user clicks a widget
  // before creating or selecting one, auto-create a default layout
  // instead of failing silently. Returns true if a layout is ready
  // (either pre-existing or just created); false if the create failed.
  async ensureCurrentLayout() {
    if (this.currentLayout) return true;
    const name = 'Untitled Layout ' + new Date().toLocaleString();
    const layout = { id: '', name, interactive: false, page_type: 'single', timing_mode: 'global', duration: 15, shuffle: false, items: [], widgets: [] };
    try {
      const saved = await API.saveLayout(layout);
      this.layouts.unshift(saved);
      this.switchToLayout(saved.id);
      this.renderSidebar();
      return true;
    } catch (e) {
      alert('Failed to create layout: ' + e.message);
      return false;
    }
  },

  _addonWidgetForType(type) {
    const key = String(type || '');
    return (this.addonWidgetTypes || []).find(addon => `addon:${addon.addon_id}:${addon.id}` === key) || null;
  },

  addWidget(type, px, py) {
    // No current layout? Make one instead of failing silently.
    if (!this.currentLayout) {
      this.ensureCurrentLayout().then(ok => {
        if (ok) this.addWidget(type, px, py);
      });
      return;
    }
    const canvas = this.el.canvas;
    const rect = canvas.getBoundingClientRect();
    let x, y;
    if (typeof px === 'number') {
      x = Math.max(0, Math.min(80, ((px / rect.width) * 100) - 8));
      y = Math.max(0, Math.min(80, ((py / rect.height) * 100) - 5));
    } else {
      const widgets = this.currentLayout?.widgets || [];
      y = 12 + (widgets.length % 5) * 8;
      x = 12;
    }
    const id = this.uid();
    const w = this.makeLayoutWidget(type, id, x, y);
    const existingZ = (this.currentLayout.widgets || []).reduce((max, item) => Math.max(max, Number(item.z) || 0), -1);
    w.z = existingZ + 1;
    (this.currentLayout.widgets = this.currentLayout.widgets || []).push(w);
    this.selectedWidgetId = id;
    this.selectedWidget = w;
    this.paneMode = 'widget';
    this.saveCurrent();
    this.renderCanvas();
    this.renderProps();
  },

  // Make a new widget for a layout. Both display and interactive widget
  // types use this factory — an interactive layout is one with
  // `interactive: true`, and the runtime client decides whether to wire
  // up input handlers based on that flag.
  makeLayoutWidget(type, id, x, y) {
    const addon = this._addonWidgetForType(type);
    if (addon) {
      const widget = this.makeLayoutWidget(addon.renderer_type, id, x, y);
      const defaultProps = addon.default_props && typeof addon.default_props === 'object' && !Array.isArray(addon.default_props) ? addon.default_props : {};
      widget.props = {
        ...widget.props,
        ...defaultProps,
        _addon_id: addon.addon_id,
        _addon_type: addon.id,
        _addon_version: addon.addon_version,
        _addon_label: addon.name,
      };
      return widget;
    }
    const base = { id, type, x, y, w: 30, h: 20, z: 1, props: {} };
    if (type === 'text') return { ...base, w: 40, h: 15, props: { content: 'text widget', font_size: 18, color: '#ffffff', font_family: 'sans-serif' } };
    if (type === 'image') return { ...base, w: 40, h: 30, props: { url: '', playlist: [], loop: true, shuffle: false, slide_duration: 10, alt: 'Image', fit: 'cover', radius: 0, radius_tl: 0, radius_tr: 0, radius_br: 0, radius_bl: 0 } };
    if (type === 'video') return { ...base, w: 50, h: 30, props: { url: '', playlist: [], loop: true, shuffle: false, muted: false, fit: 'cover' } };
    if (type === 'clock') return { ...base, w: 20, h: 12, props: { format: 'HH:mm', font_size: 36, color: '#ffffff' } };
    if (type === 'date') return { ...base, w: 25, h: 10, props: { format: 'dddd, MMMM D', font_size: 24 } };
    if (type === 'weather') {
      const baseProps = { city: '', units: 'metric', shown_days: 4, weather_morning_icon: 'off' };
      const weatherPresets = (this.presetCache || {}).weather || [];
      const defaultPreset = weatherPresets.find(item => item.is_builtin) || weatherPresets.find(item => item.name === 'Weather Simple Rows');
      let presetProps = {};
      try { presetProps = typeof defaultPreset?.props_json === 'string' ? JSON.parse(defaultPreset.props_json) : (defaultPreset?.props_json || {}); } catch (_) { presetProps = {}; }
      return { ...base, w: 30, h: 20, props: { ...baseProps, ...presetProps, shown_days: 4, ...(defaultPreset ? { preset: defaultPreset.id } : {}) } };
    }
    if (type === 'traffic') {
      const baseProps = { area_label: '', lat: null, lon: null, radius_miles: 25, max_events: 3, hide_if_empty: true, traffic_variable: 'traffic_summary', hide_long_term_closures: false, long_term_closure_days: 5, manual_closure_blocking: false, blocked_closures: [] };
      const trafficPresets = (this.presetCache || {}).traffic || [];
      const defaultPreset = trafficPresets.find(item => item.is_builtin);
      let presetProps = {};
      try { presetProps = typeof defaultPreset?.props_json === 'string' ? JSON.parse(defaultPreset.props_json) : (defaultPreset?.props_json || {}); } catch (_) { presetProps = {}; }
      return { ...base, w: 42, h: 28, props: { ...baseProps, ...presetProps, area_label: '', lat: null, lon: null, radius_miles: 25, max_events: 3, hide_if_empty: true, traffic_variable: 'traffic_summary', hide_long_term_closures: false, long_term_closure_days: 5, manual_closure_blocking: false, blocked_closures: [], ...(defaultPreset ? { preset: defaultPreset.id } : {}) } };
    }
    if (type === 'countdown') return { ...base, w: 30, h: 12, props: { targetDate: '', countdown_mode: 'date', duration_value: 5, duration_unit: 'minutes', expiry_text: '', font_size: 48, hide_on_layout: false, expiry_action: 'none', expiry_target_layout: '' } };
    if (type === 'sound') return { ...base, w: 10, h: 8, props: { playlist: [], replay: true, shuffle: false, trigger_mode: 'auto', volume: 0.7, priority: 10 } };
    if (type === 'embed') return { ...base, w: 40, h: 30, props: { url: '', aspectRatio: '16:9' } };
    if (type === 'calendar') return { ...base, w: 40, h: 35, props: { max_events: 5, ical_url: '', time_format: '12' } };
    if (type === 'news') return { ...base, w: 60, h: 10, props: { feedUrl: '', speed: 'normal' } };
    // Shape widget. Matches the player runtime's props (player.js
    // `w.widget_type === 'rectangle'` branch): color, shape_mode,
    // radius, radius_tl/tr/br/bl, opacity, line_thickness.
    if (type === 'rectangle') return { ...base, w: 30, h: 20, props: { color: '#e94560', shape_mode: 'rectangle', radius: 0, radius_tl: 0, radius_tr: 0, radius_br: 0, radius_bl: 0, opacity: 100, line_thickness: 6 } };
    // Interactive widgets
    if (type === 'button') return { ...base, w: 24, h: 10, props: { label: 'Go to layout', targetPageId: '', style: 'primary', action: 'page_switch' } };
    if (type === 'dropdown') return { ...base, w: 28, h: 9, props: { label: 'Choose option', options: ['Option A', 'Option B', 'Option C'] } };
    if (type === 'checkbox') return { ...base, w: 24, h: 8, props: { label: 'I agree', checked: false } };
    if (type === 'checkbox-list') return { ...base, w: 30, h: 18, props: { label: 'Select items', options: ['First', 'Second', 'Third'] } };
    if (type === 'radio') return { ...base, w: 30, h: 18, props: { label: 'Pick one', options: ['One', 'Two', 'Three'], selected: 'One' } };
    if (type === 'textbox') return { ...base, w: 30, h: 9, props: { label: 'Text Input', placeholder: 'Type here...', value: '' } };
    if (type === 'keyboard') return { ...base, w: 60, h: 30, props: { label: 'Keyboard', mode: 'full', visibility: 'on_focus' } };
    return base;
  },

  uid() {
    return Math.random().toString(36).slice(2, 8) + '-' + Date.now().toString(36);
  },

  renderProps() {
    const E = this.el;
    if (!E.props) return;
    // Clear the panel and write the mode selector as the first child.
    E.props.innerHTML = this._buildPaneSelectorHtml();
    this._wirePaneSelector();
    const mode = this.paneMode || 'widget';
    if (mode === 'integrations') {
      E.props.insertAdjacentHTML('beforeend', '<p class="props-empty" style="margin-top:10px;">Integrations coming soon</p>');
      return;
    }
    if (mode === 'slideshow') {
      this.renderLayoutSlideshowProperties();
      return;
    }
    if (mode === 'widget') {
      if (!this.selectedWidget) {
        E.props.insertAdjacentHTML('beforeend', '<p class="props-empty" style="margin-top:10px;">Click a widget on the canvas to edit its properties.</p>');
        return;
      }
      this.renderLayoutWidgetProps(this.selectedWidget);
      return;
    }
    // mode === 'page' — Canvas Properties for the current layout
    this.renderLayoutCanvasProperties();
  },

  renderLayoutSlideshowProperties() {
    const E = this.el;
    const layout = this.currentLayout;
    const rotation = this._layoutRotationState(layout);
    const isSet = !!layout && (layout.page_type === 'set' || !!rotation.set);
    const containerLayout = (this.layouts || []).find(item => item.id === rotation.containerId) || layout;
    const duration = rotation.set?.dwell_secs || layout?.duration || 15;
    const shuffle = rotation.set ? !!rotation.set.shuffle : !!layout?.shuffle;
    const timingMode = layout?.timing_mode || 'global';
    const items = rotation.items;
    const rotationIds = new Set(rotation.entries.map(item => item.layout_id));
    const otherLayouts = (this.layouts || []).filter(l => !rotationIds.has(l.id));
    const itemRows = items.map((it, idx) => {
      const linked = (this.layouts || []).find(l => l.id === it.layout_id);
      const linkedName = linked ? linked.name : '(missing layout)';
      const perItemDur = (timingMode === 'per_layout' && it.duration != null) ? it.duration : '';
      const linkedMode = linked && linked.interactive ? 'interactive' : 'display';
      return `
        <div class="slideshow-item-row layout-slideshow-item-row" draggable="true" data-idx="${idx}" style="display:flex;align-items:center;gap:.4rem;padding:.35rem .45rem;background:rgba(255,255,255,.04);border:1px solid var(--border,#30363d);border-radius:6px;">
          <span class="layout-slideshow-drag" title="Drag to reorder" aria-hidden="true">☰</span>
          <span style="flex:1;min-width:0;font-size:.85rem;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;" title="${this.esc(linkedName)}">${idx + 1}. ${this.esc(linkedName)} <small style="color:#888;">· ${linkedMode}</small></span>
          ${timingMode === 'per_layout' ? `<input type="number" class="text-input item-duration" placeholder="${duration}" value="${this.esc(String(perItemDur))}" min="1" max="3600" style="width:64px;padding:.3rem .4rem;border-radius:4px;border:1px solid var(--border,#30363d);background:var(--bg,#0d1117);color:var(--text,#e6edf3);" title="Per-item duration (sec)">` : ''}
          <button type="button" class="btn btn-secondary btn-sm item-up" style="padding:.2rem .45rem;" title="Move up">↑</button>
          <button type="button" class="btn btn-secondary btn-sm item-down" style="padding:.2rem .45rem;" title="Move down">↓</button>
          <button type="button" class="btn btn-danger btn-sm item-remove" style="padding:.2rem .55rem;" title="Remove">✕</button>
        </div>
      `;
    }).join('');
    const selectedLayoutIds = new Set(items.map(item => item.layout_id));
    const availableLayoutCount = otherLayouts.filter(l => !selectedLayoutIds.has(l.id)).length;
    const pickerRows = otherLayouts.map(l => {
      const alreadyAdded = selectedLayoutIds.has(l.id);
      const modeLabel = l.interactive ? 'interactive' : 'display';
      return `<label class="layout-slideshow-picker-row${alreadyAdded ? ' is-added' : ''}" data-picker-row data-layout-name="${this.esc(String(l.name || '').toLowerCase())}">
        <input type="checkbox" data-picker-layout-id="${this.esc(l.id)}" ${alreadyAdded ? 'disabled' : ''}>
        <span class="layout-slideshow-picker-name">${this.esc(l.name || 'Untitled layout')}</span>
        <small>${alreadyAdded ? 'already in rotation' : modeLabel}</small>
      </label>`;
    }).join('');
    E.props.insertAdjacentHTML('beforeend', `
      <div class="layout-rotation-summary">
        <div><strong>Layout rotation</strong><small>${containerLayout ? `Container: ${this.esc(containerLayout.name || 'Untitled layout')}` : 'Select a layout to configure its rotation'}</small></div>
        <span class="layout-rotation-badge">${isSet ? `${items.length} slide${items.length === 1 ? '' : 's'}` : 'single layout'}</span>
      </div>
      <details class="tpl-prop-section" open>
        <summary><span>Layouts in this rotation</span><small>${!layout ? 'select a layout first' : `${items.length} slide${items.length === 1 ? '' : 's'}`}</small></summary>
        <div class="tpl-prop-section-body">
          ${containerLayout ? `<div class="layout-rotation-container"><span>Assigned container</span><strong>${this.esc(containerLayout.name || 'Untitled layout')}</strong></div>` : '<div class="layout-rotation-empty">Select a layout from the Layouts list to begin.</div>'}
          <button type="button" class="btn btn-primary" id="prop-layout-add-btn" ${!layout || !availableLayoutCount ? 'disabled' : ''}>+ Add layouts</button>
          <small id="prop-layout-add-status" class="tpl-field-help">${!layout ? 'Select a layout first, then add the layouts that should rotate on its display.' : (availableLayoutCount ? 'Add one or more layouts, then drag them into the order they should play.' : (otherLayouts.length ? 'All other layouts are already in this rotation.' : 'Create another layout before adding it to this rotation.'))}</small>
          <div id="prop-layout-items" class="layout-slideshow-list" style="display:flex;flex-direction:column;gap:.35rem;">${itemRows || '<p style="font-size:.85rem;color:#888;margin:.25rem 0;">No layouts in this rotation yet.</p>'}</div>
        </div>
      </details>
      <details class="tpl-prop-section" open>
        <summary><span>Rotation settings</span><small>${isSet ? (shuffle ? 'shuffle' : 'ordered') : 'single layout'}</small></summary>
        <div class="tpl-prop-section-body">
          <p class="tpl-field-help layout-slideshow-help">${containerLayout ? `The client starts at ${this.esc(containerLayout.name || 'the container')} and follows the ordered layouts above. Save and assign the container to a physical display.` : 'Select a layout in the Layouts list first.'}</p>
          <div class="prop-group"><label>Mode</label>
            <select id="prop-layout-type" class="select" style="width:100%;padding:6px;border-radius:6px;border:1px solid var(--border,#30363d);background:var(--bg,#0d1117);color:var(--text,#e6edf3);">
            <option value="single" ${!isSet ? 'selected' : ''}>Single layout</option>
              <option value="set" ${isSet ? 'selected' : ''}>Layout rotation</option>
            </select>
          </div>
          <div class="prop-group"><label>Timing</label>
            <select id="prop-layout-timing" class="select" style="width:100%;padding:6px;border-radius:6px;border:1px solid var(--border,#30363d);background:var(--bg,#0d1117);color:var(--text,#e6edf3);">
              <option value="global" ${timingMode === 'global' ? 'selected' : ''}>Global duration</option>
              <option value="per_layout" ${timingMode === 'per_layout' ? 'selected' : ''}>Per-layout duration</option>
            </select>
          </div>
          <div class="prop-group"><label>Default Duration (s)</label><input type="number" id="prop-layout-duration" value="${duration}" min="1" max="3600" class="text-input" style="width:100%;padding:6px;border-radius:6px;border:1px solid var(--border,#30363d);background:var(--bg,#0d1117);color:var(--text,#e6edf3);"></div>
          <div class="prop-group"><label><input type="checkbox" id="prop-layout-shuffle" ${shuffle ? 'checked' : ''}> Shuffle</label></div>
        </div>
      </details>
    `);
    const typeSel = E.props.querySelector('#prop-layout-type');
    const timingSel = E.props.querySelector('#prop-layout-timing');
    const durInput = E.props.querySelector('#prop-layout-duration');
    const shuffleInput = E.props.querySelector('#prop-layout-shuffle');
    const addBtn = E.props.querySelector('#prop-layout-add-btn');
    const addStatus = E.props.querySelector('#prop-layout-add-status');
    const commitRotation = async (nextItems, overrides = {}) => {
      if (!layout) return;
      try {
        await this._saveLayoutRotation(layout, { set: rotation.set, containerId: rotation.containerId, items: nextItems }, {
          duration: overrides.duration ?? duration,
          shuffle: overrides.shuffle ?? shuffle,
          timingMode: overrides.timingMode ?? timingMode,
        });
        this.renderProps();
      } catch (error) {
        if (addStatus) addStatus.textContent = `Could not save the rotation: ${error.message || 'server error'}`;
      }
    };
    if (typeSel) typeSel.onchange = async e => {
      if (!layout) return;
      if (e.target.value === 'single') {
        await commitRotation([]);
      } else if (!items.length) {
        layout.page_type = 'set';
        try { await this.saveCurrent({ silent: false }); this.renderProps(); } catch (error) { if (addStatus) addStatus.textContent = `Could not save the rotation: ${error.message || 'server error'}`; }
      } else {
        await commitRotation(items.slice());
      }
    };
    if (timingSel) timingSel.onchange = e => commitRotation(items.slice(), { timingMode: e.target.value });
    if (durInput) durInput.onchange = e => commitRotation(items.slice(), { duration: Math.max(1, parseInt(e.target.value || '15', 10)) });
    if (shuffleInput) shuffleInput.onchange = e => commitRotation(items.slice(), { shuffle: e.target.checked });
    if (addBtn) addBtn.onclick = () => {
      if (!layout) {
        if (addStatus) addStatus.textContent = 'Select a layout first, then choose layouts to add.';
        return;
      }
      const existing = document.getElementById('layout-slideshow-picker');
      if (existing) existing.remove();
      const picker = document.createElement('div');
      picker.id = 'layout-slideshow-picker';
      picker.className = 'layout-slideshow-picker-backdrop';
      picker.innerHTML = `
        <div class="layout-slideshow-picker" role="dialog" aria-modal="true" aria-labelledby="layout-slideshow-picker-title">
          <div class="layout-slideshow-picker-header">
            <div><h3 id="layout-slideshow-picker-title">Add layouts to rotation</h3><p>Select one or more layouts to rotate after <strong>${this.esc(containerLayout?.name || 'the container')}</strong>.</p></div>
            <button type="button" class="btn btn-secondary btn-sm" data-picker-cancel aria-label="Close">×</button>
          </div>
          <input type="search" class="text-input layout-slideshow-picker-search" data-picker-search placeholder="Search layouts…" aria-label="Search layouts">
          <div class="layout-slideshow-picker-list" data-picker-list>${pickerRows || '<div class="layout-rotation-empty">There are no other layouts available yet.</div>'}</div>
          <div class="layout-slideshow-picker-footer">
            <small data-picker-count>${availableLayoutCount ? '0 layouts selected' : 'All other layouts are already in this rotation.'}</small>
            <div><button type="button" class="btn btn-secondary" data-picker-cancel>Cancel</button><button type="button" class="btn btn-primary" data-picker-add>Add selected layouts</button></div>
          </div>
        </div>`;
      document.body.appendChild(picker);
      const editor = this;
      const searchInput = picker.querySelector('[data-picker-search]');
      const addSelected = picker.querySelector('[data-picker-add]');
      const countLabel = picker.querySelector('[data-picker-count]');
      const closePicker = () => {
        document.removeEventListener('keydown', onKeyDown);
        picker.remove();
      };
      const onKeyDown = event => { if (event.key === 'Escape') closePicker(); };
      const updatePicker = () => {
        const term = String(searchInput?.value || '').trim().toLowerCase();
        picker.querySelectorAll('[data-picker-row]').forEach(row => {
          row.style.display = !term || String(row.dataset.layoutName || '').includes(term) ? '' : 'none';
        });
        const selected = picker.querySelectorAll('[data-picker-layout-id]:checked').length;
        if (countLabel) countLabel.textContent = `${selected} layout${selected === 1 ? '' : 's'} selected`;
        if (addSelected) addSelected.disabled = false;
      };
      searchInput?.addEventListener('input', updatePicker);
      picker.querySelectorAll('[data-picker-layout-id]').forEach(input => input.addEventListener('change', updatePicker));
      picker.querySelectorAll('[data-picker-cancel]').forEach(button => button.addEventListener('click', closePicker));
      picker.addEventListener('click', event => { if (event.target === picker) closePicker(); });
      addSelected?.addEventListener('click', async () => {
        const ids = Array.from(picker.querySelectorAll('[data-picker-layout-id]:checked')).map(input => input.dataset.pickerLayoutId);
        if (!ids.length) {
          if (countLabel) countLabel.textContent = 'Select at least one layout first.';
          return;
        }
        addSelected.disabled = true;
        addSelected.textContent = 'Adding…';
        try {
          const nextItems = rotation.items.slice();
          ids.forEach(id => {
            if (!nextItems.some(item => item.layout_id === id)) nextItems.push({ layout_id: id, duration: null });
          });
          await editor._saveLayoutRotation(layout, { set: rotation.set, containerId: rotation.containerId, items: nextItems }, { duration, shuffle, timingMode });
        } catch (error) {
          closePicker();
          editor.renderProps();
          const saveStatus = editor.el?.props?.querySelector('#prop-layout-add-status');
          if (saveStatus) saveStatus.textContent = `Could not save the rotation: ${error.message || 'server error'}`;
          return;
        }
        closePicker();
        editor.renderProps();
      });
      document.addEventListener('keydown', onKeyDown);
      searchInput?.focus();
    };

    E.props.querySelectorAll('.slideshow-item-row').forEach(row => {
      const idx = parseInt(row.dataset.idx, 10);
      const upBtn = row.querySelector('.item-up');
      const downBtn = row.querySelector('.item-down');
      const rmBtn = row.querySelector('.item-remove');
      const durInput2 = row.querySelector('.item-duration');
      if (upBtn) upBtn.onclick = () => {
        if (idx <= 0) return;
        const arr = items.slice();
        [arr[idx - 1], arr[idx]] = [arr[idx], arr[idx - 1]];
        commitRotation(arr);
      };
      if (downBtn) downBtn.onclick = () => {
        const arr = items.slice();
        if (idx >= arr.length - 1) return;
        [arr[idx + 1], arr[idx]] = [arr[idx], arr[idx + 1]];
        commitRotation(arr);
      };
      if (rmBtn) rmBtn.onclick = () => {
        const arr = items.slice();
        arr.splice(idx, 1);
        commitRotation(arr);
      };
      if (durInput2) durInput2.onchange = () => {
        const v = durInput2.value.trim();
        const arr = items.slice();
        arr[idx] = Object.assign({}, arr[idx], { duration: v ? Math.max(1, parseInt(v, 10)) : null });
        commitRotation(arr);
      };
    });
    let draggedIndex = null;
    E.props.querySelectorAll('.layout-slideshow-item-row').forEach(row => {
      row.addEventListener('dragstart', event => {
        draggedIndex = Number(row.dataset.idx);
        if (event.dataTransfer) {
          event.dataTransfer.effectAllowed = 'move';
          event.dataTransfer.setData('text/plain', String(draggedIndex));
        }
      });
      row.addEventListener('dragover', event => event.preventDefault());
      row.addEventListener('drop', event => {
        event.preventDefault();
        const targetIndex = Number(row.dataset.idx);
        if (draggedIndex == null || draggedIndex === targetIndex) return;
        const arr = items.slice();
        if (draggedIndex < 0 || targetIndex < 0 || draggedIndex >= arr.length || targetIndex >= arr.length) return;
        const [item] = arr.splice(draggedIndex, 1);
        arr.splice(targetIndex, 0, item);
        draggedIndex = null;
        commitRotation(arr);
      });
    });
  },

  // Build the mode selector markup. Returns an HTML string.
  _buildPaneSelectorHtml() {
    const mode = this.paneMode || 'widget';
    return `
      <div class="interactive-pane-selector" style="margin-bottom:10px;">
        <select class="select interactive-pane-select" style="width:100%;font-size:.85rem;font-weight:600;">
          <option value="widget" ${mode === 'widget' ? 'selected' : ''}>Widget Properties</option>
          <option value="page" ${mode === 'page' ? 'selected' : ''}>Canvas Properties</option>
          <option value="slideshow" ${mode === 'slideshow' ? 'selected' : ''}>Slideshow Options</option>
          <option value="integrations" ${mode === 'integrations' ? 'selected' : ''}>Integrations</option>
        </select>
      </div>
    `;
  },

  _wirePaneSelector() {
    const sel = this.el?.props?.querySelector('.interactive-pane-select');
    if (!sel) return;
    sel.onchange = () => {
      this.paneMode = sel.value;
      try { localStorage.setItem('signage.editorPaneMode', sel.value); } catch (e) {}
      if (sel.value !== 'widget') {
        this.selectedWidgetId = null;
        this.selectedWidget = null;
      }
      this.renderProps();
    };
  },

  // Canvas Properties pane: shows the current layout's name, mode,
  // interactive flag, and a clickable list of widgets on the canvas.
  renderLayoutCanvasProperties() {
    const E = this.el;
    const layout = this.currentLayout;
    if (!layout) {
      E.props.insertAdjacentHTML('beforeend', '<p class="props-empty">Select a layout to edit its canvas properties</p>');
      return;
    }
    const widgets = layout.widgets || [];
    const widgetList = widgets.length ? `
      <details class="tpl-prop-section" open>
        <summary><span>Widgets</span><small>${widgets.length} on canvas</small></summary>
        <div class="tpl-prop-section-body">
          <div class="interactive-widget-list" style="display:flex;flex-direction:column;gap:4px;">
            ${widgets.map(w => {
              const isSel = w.id === this.selectedWidgetId;
              const label = this.getWidgetLabel ? this.getWidgetLabel(w.type) : w.type;
              return `<button type="button" class="interactive-widget-list-item${isSel ? ' selected' : ''}" data-widget-id="${this.esc(w.id)}" style="text-align:left;background:${isSel ? 'rgba(167,139,250,.18)' : 'transparent'};border:1px solid var(--border);color:var(--text);padding:.4rem .55rem;border-radius:6px;cursor:pointer;font-size:.85rem;">${this.esc(w.props?.label || label)} <span style="color:#888;font-size:.75rem;">· ${this.esc(w.type)}</span></button>`;
            }).join('')}
          </div>
        </div>
      </details>
    ` : '';
    E.props.insertAdjacentHTML('beforeend', `
      <details class="tpl-prop-section" open>
        <summary><span>Canvas</span><small>${widgets.length} widget${widgets.length === 1 ? '' : 's'}</small></summary>
        <div class="tpl-prop-section-body">
          <div class="prop-group"><label>Mode</label>
            <select id="prop-layout-type" class="select" style="width:100%;padding:6px;border-radius:6px;border:1px solid var(--border,#30363d);background:var(--bg,#0d1117);color:var(--text,#e6edf3);">
              <option value="single" ${widgets.length <= 1 && (layout.page_type || 'single') !== 'set' ? 'selected' : ''}>Single layout</option>
              <option value="set" ${layout.page_type === 'set' ? 'selected' : ''}>Slideshow (multiple layouts)</option>
            </select>
          </div>
          <div class="prop-group"><label>Canvas Orientation</label>
            <select id="prop-layout-orientation" class="select" style="width:100%;padding:6px;border-radius:6px;border:1px solid var(--border,#30363d);background:var(--bg,#0d1117);color:var(--text,#e6edf3);">
              <option value="landscape" ${(layout.canvas_orientation || 'landscape') === 'landscape' ? 'selected' : ''}>Landscape (16:9)</option>
              <option value="portrait" ${layout.canvas_orientation === 'portrait' ? 'selected' : ''}>Portrait (9:16)</option>
            </select>
            <small class="tpl-field-help" style="display:block;margin-top:.35rem;color:#888;">Changes this layout's design canvas only. Display rotation correction is configured separately per display.</small>
          </div>
          <div class="prop-group">
            <label style="display:flex;align-items:center;gap:.5rem;cursor:pointer;">
              <input type="checkbox" id="prop-layout-interactive" ${layout.interactive ? 'checked' : ''}>
              <span>Interactive layout?</span>
            </label>
            <small class="tpl-field-help" style="display:block;margin-top:.35rem;color:#888;">When enabled, the client will look for input on widgets and capture/temp-store submissions. Use this for layouts that collect data via interactive widgets.</small>
          </div>
        </div>
      </details>
      ${widgetList}
    `);
    const typeSel = document.getElementById('prop-layout-type');
    if (typeSel) typeSel.onchange = e => { layout.page_type = e.target.value; this.saveCurrent(); };
    const orientationSel = document.getElementById('prop-layout-orientation');
    if (orientationSel) orientationSel.onchange = e => {
      layout.canvas_orientation = e.target.value === 'portrait' ? 'portrait' : 'landscape';
      this.saveCurrent();
      this.renderCanvas();
      this.renderProps();
    };
    const interactiveCb = document.getElementById('prop-layout-interactive');
    if (interactiveCb) interactiveCb.onchange = e => {
      layout.interactive = !!e.target.checked;
      this.saveCurrent();
      this.renderSidebar();
    };
    E.props.querySelectorAll('.interactive-widget-list-item').forEach(btn => {
      btn.onclick = () => {
        const id = btn.dataset.widgetId;
        const w = (layout.widgets || []).find(x => x.id === id);
        if (!w) return;
        this.selectedWidgetId = id;
        this.selectedWidget = w;
        this.paneMode = 'widget';
        this.renderProps();
      };
    });
  },

  _imagePlaylistMarkup(p) {
    const items = Array.isArray(p.playlist) ? p.playlist : [];
    const defaultDuration = Math.max(1, Math.min(3600, Number(p.slide_duration) || 10));
    const rows = items.map((item, index) => {
      const url = item?.url || '';
      const name = item?.name || url || `Image ${index + 1}`;
      const duration = Math.max(1, Math.min(3600, Number(item?.duration) || defaultDuration));
      return `<div class="media-playlist-row" draggable="true" data-image-playlist-row="${index}">
        <span class="media-playlist-drag" title="Drag to reorder" aria-hidden="true">☰</span>
        <img src="${this.esc(url)}" alt="" loading="lazy" class="media-playlist-thumb">
        <span class="media-playlist-name" title="${this.esc(name)}">${index + 1}. ${this.esc(name)}</span>
        <label class="media-playlist-duration">Seconds<input type="number" data-image-duration-index="${index}" value="${duration}" min="1" max="3600" step="1"></label>
        <button type="button" data-image-move-up="${index}" class="btn btn-secondary btn-sm" ${index === 0 ? 'disabled' : ''} title="Move up">↑</button>
        <button type="button" data-image-move-down="${index}" class="btn btn-secondary btn-sm" ${index === items.length - 1 ? 'disabled' : ''} title="Move down">↓</button>
        <button type="button" data-image-remove-index="${index}" class="btn btn-danger btn-sm" title="Remove image">×</button>
      </div>`;
    }).join('');
    return `<div class="media-playlist-panel">
      <div class="media-playlist-heading"><strong>Slideshow Options</strong><small data-image-playlist-summary>${items.length} image${items.length === 1 ? '' : 's'} · ${defaultDuration}s default</small></div>
      <div class="media-playlist-list" data-image-playlist-list>${rows || '<div class="media-playlist-empty">No images selected. Choose images or upload one to begin.</div>'}</div>
      <div class="media-playlist-actions">
        <button type="button" data-action="browse-image-playlist" class="btn btn-secondary btn-sm">Choose images…</button>
        <button type="button" data-image-upload-button class="btn btn-secondary btn-sm">Upload…</button>
        <input type="file" data-image-upload accept="image/png,image/jpeg,image/webp,image/gif" style="display:none;">
        <button type="button" data-action="clear-image-playlist" class="btn btn-danger btn-sm" ${items.length ? '' : 'disabled'}>Clear playlist</button>
      </div>
      <div class="media-playlist-defaults">
        <label>Default time per image<input type="number" data-image-default-duration value="${defaultDuration}" min="1" max="3600" step="1"></label>
        <button type="button" data-action="apply-image-duration" class="btn btn-secondary btn-sm" ${items.length ? '' : 'disabled'}>Apply to all</button>
      </div>
      <small data-image-status class="tpl-field-help">Per-image durations override the default.</small>
    </div>`;
  },

  _videoPlaylistMarkup(p) {
    const items = Array.isArray(p.playlist) ? p.playlist : [];
    const rows = items.map((item, index) => {
      const url = item?.url || '';
      const name = item?.name || url || `Video ${index + 1}`;
      return `<div class="media-playlist-row" draggable="true" data-video-playlist-row="${index}">
        <span class="media-playlist-drag" title="Drag to reorder" aria-hidden="true">☰</span>
        <div class="media-playlist-thumb media-playlist-video-thumb" aria-hidden="true">▶</div>
        <span class="media-playlist-name" title="${this.esc(name)}">${index + 1}. ${this.esc(name)}</span>
        <button type="button" data-video-move-up="${index}" class="btn btn-secondary btn-sm" ${index === 0 ? 'disabled' : ''} title="Move up">↑</button>
        <button type="button" data-video-move-down="${index}" class="btn btn-secondary btn-sm" ${index === items.length - 1 ? 'disabled' : ''} title="Move down">↓</button>
        <button type="button" data-video-remove-index="${index}" class="btn btn-danger btn-sm" title="Remove video">×</button>
      </div>`;
    }).join('');
    return `<div class="media-playlist-panel">
      <div class="media-playlist-heading"><strong>Video Playlist</strong><small data-video-playlist-summary>${items.length} video${items.length === 1 ? '' : 's'}</small></div>
      <div class="media-playlist-list" data-video-playlist-list>${rows || '<div class="media-playlist-empty">No videos selected. Choose videos or upload one to begin.</div>'}</div>
      <div class="media-playlist-actions">
        <button type="button" data-action="browse-video-playlist" class="btn btn-secondary btn-sm">Choose videos…</button>
        <button type="button" data-video-upload-button class="btn btn-secondary btn-sm">Upload…</button>
        <input type="file" data-video-upload accept="video/mp4,video/webm,video/ogg,video/quicktime" style="display:none;">
        <button type="button" data-action="clear-video-playlist" class="btn btn-danger btn-sm" ${items.length ? '' : 'disabled'}>Clear playlist</button>
      </div>
      <small data-video-status class="tpl-field-help">Videos play in the order shown.</small>
    </div>`;
  },

  renderLayoutWidgetProps(w) {
    const E = this.el;
    const p = w.props || {};
    const rotation = Math.max(-180, Math.min(180, Number.isFinite(Number(p.rotation)) ? Number(p.rotation) : 0));
    // Map 'date' editor type back to 'datetime' for display.
    const apiType = w.type === 'date' ? 'datetime' : (w.type === 'embed' ? 'webpage' : w.type);
    // Defaults content per type for layout widgets.
    let defaultsHtml = '';
    if (w.type === 'text') {
      defaultsHtml = `
        <div class="prop-group"><label>Content</label><textarea data-prop="content" rows="2" class="text-input" style="width:100%;padding:6px;border-radius:6px;border:1px solid var(--border,#30363d);background:var(--bg,#0d1117);color:var(--text,#e6edf3);">${this.esc(p.content || '')}</textarea></div>
        <div class="prop-group"><label>Color</label><input type="color" data-prop="color" value="${this.esc(p.color || '#ffffff')}" style="width:60px;height:32px;border-radius:6px;border:1px solid var(--border,#30363d);background:transparent;"></div>
        <div class="prop-group"><label>Font Size</label><input type="number" data-prop="font_size" value="${this.esc(String(p.font_size || 24))}" min="6" max="400" class="text-input" style="width:100%;padding:6px;border-radius:6px;border:1px solid var(--border,#30363d);background:var(--bg,#0d1117);color:var(--text,#e6edf3);"></div>
        <div class="prop-group"><label>Font Family</label><div style="display:flex;gap:6px;"><select data-prop="font_family" class="font-family-select select" data-select-id="${this.esc(w.id)}" style="flex:1;padding:6px;border-radius:6px;border:1px solid var(--border,#30363d);background:var(--bg,#0d1117);color:var(--text,#e6edf3);"><option value="sans-serif" selected>Loading…</option></select><input data-prop="font_family_custom" value="${this.esc(p.font_family || p.fontFamily || '')}" placeholder="CSS font-family, e.g. Arial, Helvetica, sans-serif" class="text-input font-family-custom-input" style="${'display:none;'}flex:1;padding:6px;border-radius:6px;border:1px solid var(--border,#30363d);background:var(--bg,#0d1117);color:var(--text,#e6edf3);"><button type="button" data-action="browse-font" class="text-input" style="padding:6px 10px;border-radius:6px;border:1px solid var(--border,#30363d);background:var(--bg-elev,#21262d);color:var(--text,#e6edf3);cursor:pointer;">Browse…</button></div></div>
      `;
    } else if (w.type === 'image') {
      defaultsHtml = `
        ${this._imagePlaylistMarkup(p)}
        <div class="prop-group"><label><input type="checkbox" data-prop="loop" ${p.loop !== false ? 'checked' : ''}> Loop slideshow</label></div>
        <div class="prop-group"><label><input type="checkbox" data-prop="shuffle" ${p.shuffle ? 'checked' : ''}> Shuffle slideshow</label></div>
        <div class="prop-group"><label>Scaling</label><select data-prop="fit" class="select" style="width:100%;padding:6px;border-radius:6px;border:1px solid var(--border,#30363d);background:var(--bg,#0d1117);color:var(--text,#e6edf3);"><option value="cover" ${(p.fit || 'cover') === 'cover' ? 'selected' : ''}>Cover (crop to fill)</option><option value="contain" ${p.fit === 'contain' ? 'selected' : ''}>Contain</option><option value="fill" ${p.fit === 'fill' ? 'selected' : ''}>Fill</option><option value="none" ${p.fit === 'none' ? 'selected' : ''}>None</option><option value="scale-down" ${p.fit === 'scale-down' ? 'selected' : ''}>Scale down</option></select></div>
        <div class="prop-group"><label>Corner Radius (px)</label><input type="number" data-prop="radius" value="${this.esc(String(p.radius == null ? 0 : p.radius))}" min="0" max="999" class="text-input" style="width:100%;padding:6px;border-radius:6px;border:1px solid var(--border,#30363d);background:var(--bg,#0d1117);color:var(--text,#e6edf3);"></div>
        <div class="prop-group"><label>Per-corner radius (px)</label><div style="display:grid;grid-template-columns:1fr 1fr;gap:6px;"><label style="font-size:.8em;">Top left<input type="number" data-prop="radius_tl" value="${this.esc(String(p.radius_tl == null ? (p.radius == null ? 0 : p.radius) : p.radius_tl))}" min="0" max="999" class="text-input" style="width:100%;padding:6px;"></label><label style="font-size:.8em;">Top right<input type="number" data-prop="radius_tr" value="${this.esc(String(p.radius_tr == null ? (p.radius == null ? 0 : p.radius) : p.radius_tr))}" min="0" max="999" class="text-input" style="width:100%;padding:6px;"></label><label style="font-size:.8em;">Bottom right<input type="number" data-prop="radius_br" value="${this.esc(String(p.radius_br == null ? (p.radius == null ? 0 : p.radius) : p.radius_br))}" min="0" max="999" class="text-input" style="width:100%;padding:6px;"></label><label style="font-size:.8em;">Bottom left<input type="number" data-prop="radius_bl" value="${this.esc(String(p.radius_bl == null ? (p.radius == null ? 0 : p.radius) : p.radius_bl))}" min="0" max="999" class="text-input" style="width:100%;padding:6px;"></label></div><small class="tpl-field-help">Changing Corner Radius sets all four corners to the same value.</small></div>
        <div class="prop-group"><label>Image URL</label><input data-prop="url" value="${this.esc(p.url || '')}" placeholder="/uploads/images/example.png" class="text-input" style="width:100%;padding:6px;border-radius:6px;border:1px solid var(--border,#30363d);background:var(--bg,#0d1117);color:var(--text,#e6edf3);"></div>
        <div class="prop-group"><label>Alt Text</label><input data-prop="alt" value="${this.esc(p.alt || '')}" class="text-input" style="width:100%;padding:6px;border-radius:6px;border:1px solid var(--border,#30363d);background:var(--bg,#0d1117);color:var(--text,#e6edf3);"></div>
      `;
    } else if (w.type === 'video') {
      defaultsHtml = `
        ${this._videoPlaylistMarkup(p)}
        <div class="prop-group"><label><input type="checkbox" data-prop="loop" ${p.loop !== false ? 'checked' : ''}> Loop playlist</label></div>
        <div class="prop-group"><label><input type="checkbox" data-prop="shuffle" ${p.shuffle ? 'checked' : ''}> Shuffle playlist</label></div>
        <div class="prop-group"><label><input type="checkbox" data-prop="muted" ${p.muted ? 'checked' : ''}> Muted</label></div>
        <div class="prop-group"><label>Scaling</label><select data-prop="fit" class="select" style="width:100%;padding:6px;border-radius:6px;border:1px solid var(--border,#30363d);background:var(--bg,#0d1117);color:var(--text,#e6edf3);"><option value="cover" ${(p.fit || 'cover') === 'cover' ? 'selected' : ''}>Cover</option><option value="contain" ${p.fit === 'contain' ? 'selected' : ''}>Contain</option><option value="fill" ${p.fit === 'fill' ? 'selected' : ''}>Fill</option></select></div>
        <div class="prop-group"><label>Video URL</label><input data-prop="url" value="${this.esc(p.url || '')}" placeholder="YouTube or direct .mp4" class="text-input" style="width:100%;padding:6px;border-radius:6px;border:1px solid var(--border,#30363d);background:var(--bg,#0d1117);color:var(--text,#e6edf3);"></div>
      `;
    } else if (w.type === 'clock') {
      defaultsHtml = `
        <div class="prop-group"><label>Format</label>
          <select data-prop="format" class="select" style="width:100%;padding:6px;border-radius:6px;border:1px solid var(--border,#30363d);background:var(--bg,#0d1117);color:var(--text,#e6edf3);">
            <option value="HH:mm" ${p.format === 'HH:mm' ? 'selected' : ''}>HH:mm (24h)</option>
            <option value="HH:mm:ss" ${p.format === 'HH:mm:ss' ? 'selected' : ''}>HH:mm:ss (24h)</option>
            <option value="hh:mm A" ${p.format === 'hh:mm A' ? 'selected' : ''}>hh:mm A (12h)</option>
            <option value="hh:mm:ss A" ${p.format === 'hh:mm:ss A' ? 'selected' : ''}>hh:mm:ss A (12h)</option>
          </select>
        </div>
        <div class="prop-group"><label>Font Size</label><input type="number" data-prop="font_size" value="${this.esc(String(p.font_size || 36))}" min="6" max="400" step="1" class="text-input" style="width:100%;padding:6px;border-radius:6px;border:1px solid var(--border,#30363d);background:var(--bg,#0d1117);color:var(--text,#e6edf3);"></div>
        <div class="prop-group"><label>Font Family</label><div style="display:flex;gap:6px;"><select data-prop="font_family" class="font-family-select select" data-select-id="${this.esc(w.id)}" style="flex:1;padding:6px;border-radius:6px;border:1px solid var(--border,#30363d);background:var(--bg,#0d1117);color:var(--text,#e6edf3);"><option value="sans-serif" selected>Loading…</option></select><input data-prop="font_family_custom" value="${this.esc(p.font_family || p.fontFamily || '')}" placeholder="CSS font-family, e.g. Arial, Helvetica, sans-serif" class="text-input font-family-custom-input" style="${'display:none;'}flex:1;padding:6px;border-radius:6px;border:1px solid var(--border,#30363d);background:var(--bg,#0d1117);color:var(--text,#e6edf3);"><button type="button" data-action="browse-font" class="text-input" style="padding:6px 10px;border-radius:6px;border:1px solid var(--border,#30363d);background:var(--bg-elev,#21262d);color:var(--text,#e6edf3);cursor:pointer;">Browse…</button></div></div>
        <div class="prop-group"><label>Text Color</label><input type="color" data-prop="color" value="${this.esc(p.color || '#ffffff')}" style="width:60px;height:32px;border-radius:6px;border:1px solid var(--border,#30363d);background:transparent;"></div>
      `;
    } else if (w.type === 'date') {
      defaultsHtml = `
        <div class="prop-group"><label>Format</label>
          <select data-prop="format" class="select" style="width:100%;padding:6px;border-radius:6px;border:1px solid var(--border,#30363d);background:var(--bg,#0d1117);color:var(--text,#e6edf3);">
            <option value="dddd, MMMM D" ${(p.format || 'dddd, MMMM D') === 'dddd, MMMM D' ? 'selected' : ''}>Monday, January 1</option>
            <option value="MMMM D, YYYY" ${p.format === 'MMMM D, YYYY' ? 'selected' : ''}>January 1, 2026</option>
            <option value="MM/DD/YYYY" ${p.format === 'MM/DD/YYYY' ? 'selected' : ''}>01/01/2026</option>
          </select>
        </div>
        <div class="prop-group"><label>Font Size</label><input type="number" data-prop="font_size" value="${this.esc(String(p.font_size || 24))}" min="6" max="400" step="1" class="text-input" style="width:100%;padding:6px;border-radius:6px;border:1px solid var(--border,#30363d);background:var(--bg,#0d1117);color:var(--text,#e6edf3);"></div>
        <div class="prop-group"><label>Font Family</label><div style="display:flex;gap:6px;"><select data-prop="font_family" class="font-family-select select" data-select-id="${this.esc(w.id)}" style="flex:1;padding:6px;border-radius:6px;border:1px solid var(--border,#30363d);background:var(--bg,#0d1117);color:var(--text,#e6edf3);"><option value="sans-serif" selected>Loading…</option></select><input data-prop="font_family_custom" value="${this.esc(p.font_family || p.fontFamily || '')}" placeholder="CSS font-family, e.g. Arial, Helvetica, sans-serif" class="text-input font-family-custom-input" style="${'display:none;'}flex:1;padding:6px;border-radius:6px;border:1px solid var(--border,#30363d);background:var(--bg,#0d1117);color:var(--text,#e6edf3);"><button type="button" data-action="browse-font" class="text-input" style="padding:6px 10px;border-radius:6px;border:1px solid var(--border,#30363d);background:var(--bg-elev,#21262d);color:var(--text,#e6edf3);cursor:pointer;">Browse…</button></div></div>
        <div class="prop-group"><label>Text Color</label><input type="color" data-prop="color" value="${this.esc(p.color || '#ffffff')}" style="width:60px;height:32px;border-radius:6px;border:1px solid var(--border,#30363d);background:transparent;"></div>
      `;
    } else if (w.type === 'weather') {
      defaultsHtml = `
        <div class="prop-group"><label>Location Label</label><input data-prop="city" value="${this.esc(p.city || p.location || '')}" class="text-input" style="width:100%;padding:6px;border-radius:6px;border:1px solid var(--border,#30363d);background:var(--bg,#0d1117);color:var(--text,#e6edf3);"></div>
        <div class="prop-group"><label>Location Lookup</label><div style="display:flex;gap:6px;"><input id="weather-location-search" class="text-input" placeholder="City, region, or country" style="flex:1;"><button type="button" id="weather-location-lookup" class="btn btn-secondary btn-sm">Search</button></div><select id="weather-location-results" class="select" style="width:100%;margin-top:6px;display:none;"><option value="">Select a location</option></select><small id="weather-location-status" class="tpl-field-help" style="display:block;margin-top:4px;color:#888;"></small></div>
        <div class="prop-group"><label>Units</label>
          <select data-prop="units" class="select" style="width:100%;padding:6px;border-radius:6px;border:1px solid var(--border,#30363d);background:var(--bg,#0d1117);color:var(--text,#e6edf3);">
            <option value="metric" ${(p.units || 'metric') === 'metric' ? 'selected' : ''}>Metric (°C)</option>
            <option value="imperial" ${p.units === 'imperial' ? 'selected' : ''}>Imperial (°F)</option>
          </select>
        </div>
        <div class="prop-group"><label>Days to show</label><input type="number" data-prop="shown_days" value="${this.esc(String(p.shown_days || 4))}" min="1" max="30" step="1" class="text-input" style="width:100%;padding:6px;border-radius:6px;border:1px solid var(--border,#30363d);background:var(--bg,#0d1117);color:var(--text,#e6edf3);"></div>
        <div class="prop-group"><label>Morning forecast icon</label><select data-prop="weather_morning_icon" class="select" style="width:100%;padding:6px;border-radius:6px;border:1px solid var(--border,#30363d);background:var(--bg,#0d1117);color:var(--text,#e6edf3);"><option value="off" ${(p.weather_morning_icon || 'off') !== 'split' ? 'selected' : ''}>Day icon only</option><option value="split" ${p.weather_morning_icon === 'split' ? 'selected' : ''}>Split morning → day icon</option></select><small class="tpl-field-help" style="display:block;margin-top:.35rem;color:#888;">When NOAA reports a morning transition, show the morning condition on the left and later daytime condition on the right.</small></div>
      `;
    } else if (w.type === 'traffic') {
      defaultsHtml = `
        <div class="prop-group"><label>Area Label</label><input data-prop="area_label" value="${this.esc(p.area_label || p.city || '')}" placeholder="Selected traffic area" class="text-input" style="width:100%;"></div>
        <div class="prop-group"><label>Area Lookup</label><div style="display:flex;gap:6px;"><input id="traffic-area-search" class="text-input" placeholder="City, county, or road area" style="flex:1;"><button type="button" id="traffic-area-lookup" class="btn btn-secondary btn-sm">Search</button></div><select id="traffic-area-results" class="select" style="width:100%;margin-top:6px;display:none;"><option value="">Select an area</option></select><small id="traffic-area-status" class="tpl-field-help" style="display:block;margin-top:4px;color:#888;">Choose the center point used to filter traffic events.</small></div>
        <div class="prop-group"><label>Search Radius (miles)</label><input type="number" data-prop="radius_miles" value="${this.esc(String(p.radius_miles == null ? 25 : p.radius_miles))}" min="1" max="100" step="1" class="text-input" style="width:100%;"></div>
        <div class="prop-group"><label>Maximum Events</label><input type="number" data-prop="max_events" value="${this.esc(String(p.max_events == null ? 3 : p.max_events))}" min="1" max="10" step="1" class="text-input" style="width:100%;"></div>
        <div class="prop-group"><label>Default Traffic Variable</label><select data-prop="traffic_variable" class="select" style="width:100%;"><option value="traffic_summary" ${(p.traffic_variable || 'traffic_summary') === 'traffic_summary' ? 'selected' : ''}>Summary</option><option value="traffic_incidents" ${p.traffic_variable === 'traffic_incidents' ? 'selected' : ''}>Crash / Incident Count</option><option value="traffic_crashes_closures" ${p.traffic_variable === 'traffic_crashes_closures' ? 'selected' : ''}>Crash / Road Closure Description</option><option value="traffic_closures" ${p.traffic_variable === 'traffic_closures' ? 'selected' : ''}>Closure Count</option><option value="traffic_delays" ${p.traffic_variable === 'traffic_delays' ? 'selected' : ''}>Delay Count</option></select></div>
        <div class="prop-group"><label><input type="checkbox" data-prop="hide_if_empty" ${p.hide_if_empty !== false ? 'checked' : ''}> Hide when there are no events</label></div>
        <div class="prop-group"><label><input type="checkbox" data-prop="hide_long_term_closures" ${p.hide_long_term_closures === true ? 'checked' : ''}> Ignore long-term road closures</label><small class="tpl-field-help" style="display:block;margin-top:.35rem;color:#888;">A closure is hidden after the selected number of days while it remains active. If the feed stops reporting it, its timer resets when it returns.</small></div>
        <div class="prop-group"><label>Hide closure after (days)</label><input type="number" data-prop="long_term_closure_days" value="${this.esc(String(Math.max(1, Math.min(3650, Number(p.long_term_closure_days) || 5))))}" min="1" max="3650" step="1" class="text-input" style="width:100%;"><small class="tpl-field-help">Used only when Ignore long-term road closures is enabled.</small></div>
        <div class="prop-group"><label><input type="checkbox" data-prop="manual_closure_blocking" ${p.manual_closure_blocking === true ? 'checked' : ''}> Enable manual road-closure blocking</label><small class="tpl-field-help" style="display:block;margin-top:.35rem;color:#888;">Check the live closure list below and block individual closures. A block is automatically removed when the feed no longer reports that closure.</small><button type="button" id="traffic-closure-manager" class="btn btn-secondary btn-sm" style="margin-top:.45rem;">Check active road closures</button><div id="traffic-closure-manager-status" class="tpl-field-help" style="margin-top:.35rem;color:#888;"></div><div id="traffic-closure-manager-list" style="display:flex;flex-direction:column;gap:.35rem;margin-top:.45rem;"></div></div>
      `;
    } else if (w.type === 'sound') {
      const playlistText = (Array.isArray(p.playlist) ? p.playlist : []).map(t => `${t.name || ''}|${t.url || ''}`).join('\n');
      defaultsHtml = `
        <div class="prop-group"><label>Playlist</label><textarea data-prop-playlist rows="5" class="text-input" placeholder="Name | https://example.com/audio.mp3\nOne URL per line" style="width:100%;padding:6px;border-radius:6px;border:1px solid var(--border,#30363d);background:var(--bg,#0d1117);color:var(--text,#e6edf3);">${this.esc(playlistText)}</textarea><small class="tpl-field-help" style="display:block;margin-top:.35rem;color:#888;">Use one track per line as Name | URL. The player is hidden and only produces sound.</small></div>
        <div class="prop-group"><label><input type="checkbox" data-prop="replay" ${p.replay !== false ? 'checked' : ''}> Replay playlist</label></div>
        <div class="prop-group"><label><input type="checkbox" data-prop="shuffle" ${p.shuffle ? 'checked' : ''}> Shuffle playlist</label></div>
        <div class="prop-group"><label>Playback</label><select data-prop="trigger_mode" class="select" style="width:100%;padding:6px;border-radius:6px;border:1px solid var(--border,#30363d);background:var(--bg,#0d1117);color:var(--text,#e6edf3);"><option value="auto" ${(p.trigger_mode || 'auto') === 'auto' ? 'selected' : ''}>Start when layout loads</option><option value="button" ${p.trigger_mode === 'button' ? 'selected' : ''}>Start from a Button widget</option></select></div>
        <div class="prop-group"><label>Volume (0–1)</label><input type="number" data-prop="volume" value="${this.esc(String(p.volume == null ? 0.7 : p.volume))}" min="0" max="1" step="0.05" class="text-input" style="width:100%;padding:6px;border-radius:6px;border:1px solid var(--border,#30363d);background:var(--bg,#0d1117);color:var(--text,#e6edf3);"></div>
        <div class="prop-group"><label>Sound Priority</label><input type="number" data-prop="priority" value="${this.esc(String(p.priority == null ? 10 : p.priority))}" min="0" max="100" step="1" class="text-input" style="width:100%;padding:6px;border-radius:6px;border:1px solid var(--border,#30363d);background:var(--bg,#0d1117);color:var(--text,#e6edf3);"></div>
      `;
    } else if (w.type === 'countdown') {
      const countdownLayouts = (this.layouts || []).filter(l => l.id !== this.currentLayoutId);
      const countdownTargetOpts = ['<option value="">— Select layout —</option>'].concat(countdownLayouts.map(l => `<option value="${this.esc(l.id)}" ${(p.expiry_target_layout || '') === l.id ? 'selected' : ''}>${this.esc(l.interactive ? '🕹️ ' : '📐 ') + this.esc(l.name)}</option>`)).join('');
      defaultsHtml = `
        <div class="prop-group"><label>Countdown mode</label><select data-countdown-mode class="select" style="width:100%;padding:6px;border-radius:6px;border:1px solid var(--border,#30363d);background:var(--bg,#0d1117);color:var(--text,#e6edf3);"><option value="date" ${(p.countdown_mode || 'date') === 'date' ? 'selected' : ''}>Count down to a date and time</option><option value="duration" ${p.countdown_mode === 'duration' ? 'selected' : ''}>Count down for a set duration</option></select></div><div data-countdown-date-settings style="display:${(p.countdown_mode || 'date') === 'date' ? 'block' : 'none'};"><div class="prop-group"><label>Target date</label><input type="date" data-countdown-date value="${this.esc(String(p.targetDate || '').slice(0,10))}" class="text-input" style="width:100%;padding:6px;border-radius:6px;border:1px solid var(--border,#303d3d);background:var(--bg,#0d1117);color:var(--text,#e6edf3);"></div><div class="prop-group"><label>Target time</label><input type="time" data-countdown-time value="${this.esc(String(p.targetDate || '').slice(11,16))}" class="text-input" style="width:100%;padding:6px;border-radius:6px;border:1px solid var(--border,#30363d);background:var(--bg,#0d1117);color:var(--text,#e6edf3);"></div></div><div data-countdown-duration-settings style="display:${p.countdown_mode === 'duration' ? 'block' : 'none'};"><div class="prop-group"><label>Duration</label><div style="display:flex;gap:8px;"><input type="number" data-countdown-duration-value value="${this.esc(String(p.duration_value == null ? 5 : p.duration_value))}" min="1" max="999999" step="1" class="text-input" style="flex:1;padding:6px;border-radius:6px;border:1px solid var(--border,#30363d);background:var(--bg,#0d1117);color:var(--text,#e6edf3);"><select data-countdown-duration-unit class="select" style="flex:1;padding:6px;border-radius:6px;border:1px solid var(--border,#30363d);background:var(--bg,#0d1117);color:var(--text,#e6edf3);"><option value="seconds" ${p.duration_unit === 'seconds' ? 'selected' : ''}>Seconds</option><option value="minutes" ${(p.duration_unit || 'minutes') === 'minutes' ? 'selected' : ''}>Minutes</option><option value="hours" ${p.duration_unit === 'hours' ? 'selected' : ''}>Hours</option><option value="days" ${p.duration_unit === 'days' ? 'selected' : ''}>Days</option></select></div></div></div><input type="hidden" data-prop="targetDate" value="${this.esc(String(p.targetDate || '').slice(0,16))}"><small class="tpl-field-help" style="display:block;margin-top:.35rem;color:#888;">The countdown starts when this layout loads.</small></div>
        <div class="prop-group"><label>Text when timer runs out</label><input data-prop="expiry_text" value="${this.esc(p.expiry_text || '')}" placeholder="Leave blank for no text" class="text-input" style="width:100%;padding:6px;border-radius:6px;border:1px solid var(--border,#30363d);background:var(--bg,#0d1117);color:var(--text,#e6edf3);"></div>
        <div class="prop-group"><label>Text Size (px)</label><input type="number" data-prop="font_size" value="${this.esc(String(Math.max(8, Math.min(160, Number(p.font_size) || 48))))}" min="8" max="160" step="1" class="text-input" style="width:100%;padding:6px;border-radius:6px;border:1px solid var(--border,#30363d);background:var(--bg,#0d1117);color:var(--text,#e6edf3);"><small class="tpl-field-help">Maximum: 160 px.</small></div>
        <div class="prop-group"><label><input type="checkbox" data-prop="hide_on_layout" ${p.hide_on_layout ? 'checked' : ''}> Hide on live layout</label><small class="tpl-field-help" style="display:block;margin-top:.35rem;color:#888;">The countdown remains visible in the editor.</small></div>
        <div class="prop-group"><label>When countdown reaches zero</label><select data-prop="expiry_action" class="select" style="width:100%;padding:6px;border-radius:6px;border:1px solid var(--border,#30363d);background:var(--bg,#0d1117);color:var(--text,#e6edf3);"><option value="none" ${(p.expiry_action || 'none') === 'none' ? 'selected' : ''}>Do nothing</option><option value="submit_page" ${p.expiry_action === 'submit_page' ? 'selected' : ''}>Submit data</option><option value="page_switch" ${p.expiry_action === 'page_switch' ? 'selected' : ''}>Change layout</option></select></div>
        <div class="prop-group"><label>Target Layout</label><select data-prop="expiry_target_layout" class="select" style="width:100%;padding:6px;border-radius:6px;border:1px solid var(--border,#30363d);background:var(--bg,#0d1117);color:var(--text,#e6edf3);">${countdownTargetOpts}</select></div>
        <div class="prop-group"><label>Font Family</label><div style="display:flex;gap:6px;"><select data-prop="font_family" class="font-family-select select" data-select-id="${this.esc(w.id)}" style="flex:1;padding:6px;border-radius:6px;border:1px solid var(--border,#30363d);background:var(--bg,#0d1117);color:var(--text,#e6edf3);"><option value="sans-serif" selected>Loading…</option></select><input data-prop="font_family_custom" value="${this.esc(p.font_family || p.fontFamily || '')}" placeholder="CSS font-family, e.g. Arial, Helvetica, sans-serif" class="text-input font-family-custom-input" style="${'display:none;'}flex:1;padding:6px;border-radius:6px;border:1px solid var(--border,#30363d);background:var(--bg,#0d1117);color:var(--text,#e6edf3);"><button type="button" data-action="browse-font" class="text-input" style="padding:6px 10px;border-radius:6px;border:1px solid var(--border,#30363d);background:var(--bg-elev,#212529);color:var(--text,#e6edf3);cursor:pointer;">Browse…</button></div></div>
        <div class="prop-group"><label>Text Color</label><input type="color" data-prop="color" value="${this.esc(p.color || '#ffffff')}" style="width:60px;height:32px;border-radius:6px;border:1px solid var(--border,#30363d);background:transparent;"></div>
      `;
    } else if (w.type === 'embed') {
      defaultsHtml = `
        <div class="prop-group"><label>URL</label><input data-prop="url" value="${this.esc(p.url || '')}" placeholder="https://..." class="text-input" style="width:100%;padding:6px;border-radius:6px;border:1px solid var(--border,#30363d);background:var(--bg,#0d1117);color:var(--text,#e6edf3);"></div>
        <div class="prop-group"><label>Aspect Ratio</label><input data-prop="aspectRatio" value="${this.esc(p.aspectRatio || '16:9')}" class="text-input" style="width:100%;padding:6px;border-radius:6px;border:1px solid var(--border,#30363d);background:var(--bg,#0d1117);color:var(--text,#e6edf3);"></div>
      `;
    } else if (w.type === 'calendar') {
      defaultsHtml = `<div class="prop-group"><label>Events to show</label><input type="number" data-prop="max_events" value="${this.esc(String(p.max_events || 5))}" min="1" max="30" step="1" class="text-input" style="width:100%;"><small class="tpl-field-help">Leave iCal URL blank to preview US holidays in the editor.</small></div><div class="prop-group"><label>Event time format</label><select data-prop="time_format" class="select" style="width:100%;"><option value="12" ${(p.time_format || '12') === '12' ? 'selected' : ''}>12-hour (AM/PM)</option><option value="24" ${p.time_format === '24' ? 'selected' : ''}>24-hour</option></select></div><div class="prop-group"><label>iCal URL</label><input type="url" data-prop="ical_url" value="${this.esc(p.ical_url || p.ical || p.calendar_url || '')}" placeholder="https://example.com/calendar.ics" class="text-input" style="width:100%;"><small class="tpl-field-help">Optional .ics calendar source used by the player.</small></div><p style="font-size:.8rem;color:#888;">Calendar events repeat using the selected template group.</p>`;
    } else if (w.type === 'news') {
      defaultsHtml = `
        <div class="prop-group"><label>Feed URL</label><input data-prop="feedUrl" value="${this.esc(p.feedUrl || '')}" class="text-input" style="width:100%;padding:6px;border-radius:6px;border:1px solid var(--border,#30363d);background:var(--bg,#0d1117);color:var(--text,#e6edf3);"></div>
        <div class="prop-group"><label>Speed</label>
          <select data-prop="speed" class="select" style="width:100%;padding:6px;border-radius:6px;border:1px solid var(--border,#30363d);background:var(--bg,#0d1117);color:var(--text,#e6edf3);">
            <option value="slow" ${p.speed === 'slow' ? 'selected' : ''}>Slow</option>
            <option value="normal" ${(p.speed || 'normal') === 'normal' ? 'selected' : ''}>Normal</option>
            <option value="fast" ${p.speed === 'fast' ? 'selected' : ''}>Fast</option>
          </select>
        </div>
      `;
    } else if (w.type === 'rectangle') {
      // Shape widget editor. Mirrors the player runtime's
      // `w.widget_type === 'rectangle'` branch (player.js around
      // line 285) — shape_mode drives both the border-radius CSS and
      // an optional clip-path; radius is the base corner radius and
      // the per-corner overrides fall back to it; opacity is 0..100;
      // line_thickness only matters for the 'line' shape mode.
      const shapeMode = ['rounded', 'rectangle'].includes(p.shape_mode) ? 'rectangle' : (p.shape_mode || 'rectangle');
      defaultsHtml = `
        <div class="prop-group"><label>Color</label><input type="color" data-prop="color" value="${this.esc(p.color || '#e94560')}" style="width:60px;height:32px;border-radius:6px;border:1px solid var(--border,#30363d);background:transparent;"></div>
        <div class="prop-group"><label>Shape Mode</label>
          <select data-prop="shape_mode" class="select" style="width:100%;padding:6px;border-radius:6px;border:1px solid var(--border,#30363d);background:var(--bg,#0d1117);color:var(--text,#e6edf3);">
            <option value="rectangle" ${shapeMode === 'rectangle' ? 'selected' : ''}>Rectangle</option>
            <option value="circle" ${shapeMode === 'circle' ? 'selected' : ''}>Circle / Ellipse</option>
            <option value="pill" ${shapeMode === 'pill' ? 'selected' : ''}>Pill</option>
            <option value="line" ${shapeMode === 'line' ? 'selected' : ''}>Line / Divider</option>
            <option value="triangle" ${shapeMode === 'triangle' ? 'selected' : ''}>Triangle</option>
            <option value="diamond" ${shapeMode === 'diamond' ? 'selected' : ''}>Diamond</option>
            <option value="hexagon" ${shapeMode === 'hexagon' ? 'selected' : ''}>Hexagon</option>
            <option value="star" ${shapeMode === 'star' ? 'selected' : ''}>Star</option>
            <option value="parallelogram" ${shapeMode === 'parallelogram' ? 'selected' : ''}>Parallelogram</option>
            <option value="arrow" ${shapeMode === 'arrow' ? 'selected' : ''}>Arrow</option>
            <option value="plus" ${shapeMode === 'plus' ? 'selected' : ''}>Plus</option>
            <option value="minus" ${shapeMode === 'minus' ? 'selected' : ''}>Minus</option>
            <option value="x" ${shapeMode === 'x' ? 'selected' : ''}>X</option>
          </select>
        </div>
        <div class="prop-group"><label>Corner Radius (px)</label><input type="number" data-prop="radius" value="${this.esc(String(p.radius == null ? 0 : p.radius))}" min="0" max="999" class="text-input" style="width:100%;padding:6px;border-radius:6px;border:1px solid var(--border,#30363d);background:var(--bg,#0d1117);color:var(--text,#e6edf3);"></div>
        <div class="prop-group"><label>Per-corner radius (px)</label><div style="display:grid;grid-template-columns:1fr 1fr;gap:6px;"><label style="font-size:.8em;">Top left<input type="number" data-prop="radius_tl" value="${this.esc(String(p.radius_tl == null ? (p.radius == null ? 0 : p.radius) : p.radius_tl))}" min="0" max="999" class="text-input" style="width:100%;padding:6px;"></label><label style="font-size:.8em;">Top right<input type="number" data-prop="radius_tr" value="${this.esc(String(p.radius_tr == null ? (p.radius == null ? 0 : p.radius) : p.radius_tr))}" min="0" max="999" class="text-input" style="width:100%;padding:6px;"></label><label style="font-size:.8em;">Bottom right<input type="number" data-prop="radius_br" value="${this.esc(String(p.radius_br == null ? (p.radius == null ? 0 : p.radius) : p.radius_br))}" min="0" max="999" class="text-input" style="width:100%;padding:6px;"></label><label style="font-size:.8em;">Bottom left<input type="number" data-prop="radius_bl" value="${this.esc(String(p.radius_bl == null ? (p.radius == null ? 0 : p.radius) : p.radius_bl))}" min="0" max="999" class="text-input" style="width:100%;padding:6px;"></label></div><small class="tpl-field-help">Changing Corner Radius sets all four corners to the same value.</small></div>
        <div class="prop-group"><label>Opacity (0-100)</label><input type="number" data-prop="opacity" value="${this.esc(String(p.opacity == null ? 100 : p.opacity))}" min="0" max="100" class="text-input" style="width:100%;padding:6px;border-radius:6px;border:1px solid var(--border,#30363d);background:var(--bg,#0d1117);color:var(--text,#e6edf3);"></div>
        <div class="prop-group"><label>Line Thickness (px, line mode only)</label><input type="number" data-prop="line_thickness" value="${this.esc(String(p.line_thickness == null ? 6 : p.line_thickness))}" min="1" max="100" class="text-input" style="width:100%;padding:6px;border-radius:6px;border:1px solid var(--border,#30363d);background:var(--bg,#0d1117);color:var(--text,#e6edf3);"></div>
      `;
    } else if (w.type === 'button') {
      // Build the layout-target dropdown from this.layouts (every layout
      // except the one this button lives on, since buttons shouldn't point
      // at their own layout).
      const otherLayouts = (this.layouts || []).filter(l => l.id !== this.currentLayoutId);
      const soundWidgets = (this.currentLayout?.widgets || []).filter(x => x.type === 'sound' || x.widget_type === 'sound');
      const targetOpts = ['<option value="">— No layout switch —</option>'].concat(otherLayouts.map(l => {
        const sel = (p.targetPageId || '') === l.id ? ' selected' : '';
        return `<option value="${this.esc(l.id)}"${sel}>${this.esc(l.interactive ? '🕹️ ' : '📐 ') + this.esc(l.name)}</option>`;
      })).join('');
      defaultsHtml = `
        <div class="prop-group"><label>Label</label><input data-prop="label" value="${this.esc(p.label || 'Button')}" class="text-input" style="width:100%;padding:6px;border-radius:6px;border:1px solid var(--border,#30363d);background:var(--bg,#0d1117);color:var(--text,#e6edf3);"></div>
        <div class="prop-group"><label>Action</label>
          <select data-prop="action" class="select" style="width:100%;padding:6px;border-radius:6px;border:1px solid var(--border,#30363d);background:var(--bg,#0d1117);color:var(--text,#e6edf3);">
            <option value="page_switch" ${(p.action || 'page_switch') === 'page_switch' ? 'selected' : ''}>Switch to another layout</option>
            <option value="submit_page" ${p.action === 'submit_page' ? 'selected' : ''}>Submit form values</option>
            <option value="play_sound" ${p.action === 'play_sound' ? 'selected' : ''}>Play sound</option>
          </select>
        </div>
        <div class="prop-group"><label>Target Layout</label>
          <select data-prop="targetPageId" class="select" style="width:100%;padding:6px;border-radius:6px;border:1px solid var(--border,#30363d);background:var(--bg,#0d1117);color:var(--text,#e6edf3);">
            ${targetOpts}
          </select>
          <small class="tpl-field-help" style="display:block;margin-top:.35rem;color:#888;">When the user taps the button, the player switches to this layout (using the cached set — works offline for a few seconds).</small>
        </div>
        <div class="prop-group"><label>Sound Widget</label>
          <select data-prop="targetSoundWidgetId" class="select" style="width:100%;padding:6px;border-radius:6px;border:1px solid var(--border,#30363d);background:var(--bg,#0d1117);color:var(--text,#e6edf3);">
            <option value="">— Select sound widget —</option>
            ${soundWidgets.map(sw => `<option value="${this.esc(sw.id)}" ${(p.targetSoundWidgetId || '') === sw.id ? 'selected' : ''}>${this.esc((sw.props && sw.props.label) || sw.id)}</option>`).join('')}
          </select>
          <small class="tpl-field-help" style="display:block;margin-top:.35rem;color:#888;">Used when the button action is Play sound.</small>
        </div>
        <div class="prop-group"><label>Font Size</label><input type="number" data-prop="fontSize" value="${this.esc(String(p.fontSize || 28))}" min="8" max="200" class="text-input" style="width:100%;padding:6px;border-radius:6px;border:1px solid var(--border,#30363d);background:var(--bg,#0d1117);color:var(--text,#e6edf3);"></div>
      `;
    } else if (w.type === 'dropdown') {
      const optionsVal = Array.isArray(p.options) ? p.options.join('\n') : (p.options || '');
      defaultsHtml = `
        <div class="prop-group"><label>Label</label><input data-prop="label" value="${this.esc(p.label || 'Drop-down')}" class="text-input" style="width:100%;padding:6px;border-radius:6px;border:1px solid var(--border,#30363d);background:var(--bg,#0d1117);color:var(--text,#e6edf3);"></div>
        <div class="prop-group"><label>Options (one per line)</label><textarea data-prop-options rows="4" class="text-input" style="width:100%;padding:6px;border-radius:6px;border:1px solid var(--border,#30363d);background:var(--bg,#0d1117);color:var(--text,#e6edf3);">${this.esc(optionsVal)}</textarea>
          <small class="tpl-field-help" style="display:block;margin-top:.35rem;color:#888;">One option per line. The first option is blank so the user always picks intentionally.</small>
        </div>
      `;
    } else if (w.type === 'checkbox') {
      defaultsHtml = `
        <div class="prop-group"><label>Label</label><input data-prop="label" value="${this.esc(p.label || 'I agree')}" class="text-input" style="width:100%;padding:6px;border-radius:6px;border:1px solid var(--border,#30363d);background:var(--bg,#0d1117);color:var(--text,#e6edf3);"></div>
        <div class="prop-group"><label><input type="checkbox" data-prop="checked" ${p.checked ? 'checked' : ''}> Checked by default</label></div>
      `;
    } else if (w.type === 'checkbox-list') {
      const optionsVal = Array.isArray(p.options) ? p.options.join('\n') : (p.options || '');
      defaultsHtml = `
        <div class="prop-group"><label>Label</label><input data-prop="label" value="${this.esc(p.label || 'Select items')}" class="text-input" style="width:100%;padding:6px;border-radius:6px;border:1px solid var(--border,#30363d);background:var(--bg,#0d1117);color:var(--text,#e6edf3);"></div>
        <div class="prop-group"><label>Options (one per line)</label><textarea data-prop-options rows="4" class="text-input" style="width:100%;padding:6px;border-radius:6px;border:1px solid var(--border,#30363d);background:var(--bg,#0d1117);color:var(--text,#e6edf3);">${this.esc(optionsVal)}</textarea></div>
      `;
    } else if (w.type === 'radio') {
      const optionsVal = Array.isArray(p.options) ? p.options.join('\n') : (p.options || '');
      defaultsHtml = `
        <div class="prop-group"><label>Label</label><input data-prop="label" value="${this.esc(p.label || 'Pick one')}" class="text-input" style="width:100%;padding:6px;border-radius:6px;border:1px solid var(--border,#30363d);background:var(--bg,#0d1117);color:var(--text,#e6edf3);"></div>
        <div class="prop-group"><label>Options (one per line)</label><textarea data-prop-options rows="4" class="text-input" style="width:100%;padding:6px;border-radius:6px;border:1px solid var(--border,#30363d);background:var(--bg,#0d1117);color:var(--text,#e6edf3);">${this.esc(optionsVal)}</textarea></div>
        <div class="prop-group"><label>Default selected</label><input data-prop="selected" value="${this.esc(p.selected || '')}" class="text-input" style="width:100%;padding:6px;border-radius:6px;border:1px solid var(--border,#30363d);background:var(--bg,#0d1117);color:var(--text,#e6edf3);"></div>
      `;
    } else if (w.type === 'textbox') {
      defaultsHtml = `
        <div class="prop-group"><label>Label</label><input data-prop="label" value="${this.esc(p.label || 'Text Input')}" class="text-input" style="width:100%;padding:6px;border-radius:6px;border:1px solid var(--border,#30363d);background:var(--bg,#0d1117);color:var(--text,#e6edf3);"></div>
        <div class="prop-group"><label>Placeholder</label><input data-prop="placeholder" value="${this.esc(p.placeholder || '')}" class="text-input" style="width:100%;padding:6px;border-radius:6px;border:1px solid var(--border,#30363d);background:var(--bg,#0d1117);color:var(--text,#e6edf3);"></div>
        <div class="prop-group"><label>Default value</label><input data-prop="value" value="${this.esc(p.value || '')}" class="text-input" style="width:100%;padding:6px;border-radius:6px;border:1px solid var(--border,#30363d);background:var(--bg,#0d1117);color:var(--text,#e6edf3);"></div>
        <div class="prop-group"><label><input type="checkbox" data-prop="multiline" ${p.multiline ? 'checked' : ''}> Multi-line</label></div>
      `;
    } else if (w.type === 'keyboard') {
      defaultsHtml = `
        <div class="prop-group"><label>Label</label><input data-prop="label" value="${this.esc(p.label || 'Keyboard')}" class="text-input" style="width:100%;padding:6px;border-radius:6px;border:1px solid var(--border,#30363d);background:var(--bg,#0d1117);color:var(--text,#e6edf3);"></div>
        <div class="prop-group"><label>Mode</label>
          <select data-prop="mode" class="select" style="width:100%;padding:6px;border-radius:6px;border:1px solid var(--border,#30363d);background:var(--bg,#0d1117);color:var(--text,#e6edf3);">
            <option value="full" ${(p.mode || 'full') === 'full' ? 'selected' : ''}>Full keyboard</option>
            <option value="number" ${p.mode === 'number' ? 'selected' : ''}>Number pad</option>
            <option value="full_keyboard" ${p.mode === 'full_keyboard' ? 'selected' : ''}>Full keyboard + symbols</option>
          </select>
        </div>
      `;
    }
    // Preset metadata is normalized to lowercase when it is fetched. Layouts
    // created by older editor versions can still carry a capitalized or
    // widget_type-only value, so normalize the lookup key as well. Without
    // this, Calendar widgets render correctly but their template selector is
    // silently omitted because the cache is indexed by "calendar".
    const presetType = String(w.type || w.widget_type || '').trim().toLowerCase();
    const presets = (this.presetCache || {})[presetType] || [];
    const noPresetLabel = w.type === 'weather' ? 'No template preset (uses Weather Simple Rows)' : 'No template preset';
    const presetOptions = [`<option value="">${this.esc(noPresetLabel)}</option>`].concat(
      presets.map(preset => `<option value="${this.esc(preset.id)}" ${p.preset === preset.id ? 'selected' : ''}>${this.esc(preset.name)}${preset.is_builtin ? ' · built-in' : ''}</option>`)
    ).join('');
    const presetHtml = presets.length ? `<details class="tpl-prop-section" open><summary><span>Widget Template</span><small>Reusable template</small></summary><div class="tpl-prop-section-body"><div class="prop-group"><label>Preset</label><select id="widget-preset-select" class="select" style="width:100%;">${presetOptions}</select><small class="tpl-field-help">Choose a template for this widget. Template repeat groups and their layout are applied here.</small></div></div></details>` : '';
    const fontCapableTypes = ['clock', 'date', 'datetime', 'weather', 'traffic', 'countdown', 'news', 'calendar', 'button', 'dropdown', 'checkbox', 'checkbox-list', 'radio', 'textbox', 'keyboard'];
    if (fontCapableTypes.includes(w.type) && !defaultsHtml.includes('data-prop="font_family"')) {
      defaultsHtml += `<div class="prop-group"><label>Font Family</label><div style="display:flex;gap:6px;"><select data-prop="font_family" class="font-family-select select" data-select-id="${this.esc(w.id)}" style="flex:1;padding:6px;border-radius:6px;border:1px solid var(--border,#30363d);background:var(--bg,#0d1117);color:var(--text,#e6edf3);"><option value="sans-serif" selected>Loading…</option></select><input data-prop="font_family_custom" value="${this.esc(p.font_family || p.fontFamily || '')}" placeholder="CSS font-family, e.g. Arial, Helvetica, sans-serif" class="text-input font-family-custom-input" style="${'display:none;'}flex:1;padding:6px;border-radius:6px;border:1px solid var(--border,#30363d);background:var(--bg,#0d1117);color:var(--text,#e6edf3);"><button type="button" data-action="browse-font" class="text-input" style="padding:6px 10px;border-radius:6px;border:1px solid var(--border,#30363d);background:var(--bg-elev,#21262d);color:var(--text,#e6edf3);cursor:pointer;">Browse…</button></div></div>`;
    }
    // Top toolbar for the selected widget: Duplicate + Delete. Delete is
    // immediate (no confirm) per the editor UI; Duplicate offsets by +2
    // in both axes so the copy is visibly distinct from the source.
    const wTypeLabel = this.getWidgetLabel ? this.getWidgetLabel(w.type) : w.type;
    E.props.insertAdjacentHTML('beforeend', `
      <div class="tpl-widget-actions" style="display:flex;gap:.5rem;margin:0 0 .75rem 0;">
        <button type="button" id="btn-duplicate-widget" class="btn btn-secondary" style="flex:1;">\u2398 Duplicate</button>
        <button type="button" id="btn-delete-widget" class="btn btn-danger" style="flex:1;">\u2398 Delete</button>
      </div>
      <div style="font-size:.75rem;color:var(--text-dim);margin:0 0 .5rem 0;">Selected: <strong style="color:var(--text);">${this.esc(wTypeLabel)}</strong></div>
      ${presetHtml}${defaultsHtml ? `<details class="tpl-prop-section" open>
        <summary><span>Defaults</span><small>${this.esc(w.type)} default values</small></summary>
        <div class="tpl-prop-section-body">${defaultsHtml}</div>
      </details>` : ''}
      <details class="tpl-prop-section">
        <summary><span>Position</span><small>x:${w.x.toFixed(1)} y:${w.y.toFixed(1)} w:${w.w.toFixed(1)} h:${w.h.toFixed(1)} z:${w.z}</small></summary>
        <div class="tpl-prop-section-body">
          <div class="prop-row" style="display:flex;gap:8px;">
            <div class="prop-group" style="flex:1;"><label>X %</label><input type="number" data-field="x" value="${w.x.toFixed(1)}" class="text-input" style="width:100%;padding:6px;border-radius:6px;border:1px solid var(--border,#30363d);background:var(--bg,#0d1117);color:var(--text,#e6edf3);"></div>
            <div class="prop-group" style="flex:1;"><label>Y %</label><input type="number" data-field="y" value="${w.y.toFixed(1)}" class="text-input" style="width:100%;padding:6px;border-radius:6px;border:1px solid var(--border,#30363d);background:var(--bg,#0d1117);color:var(--text,#e6edf3);"></div>
          </div>
          <div class="prop-row" style="display:flex;gap:8px;">
            <div class="prop-group" style="flex:1;"><label>W %</label><input type="number" data-field="w" value="${w.w.toFixed(1)}" class="text-input" style="width:100%;padding:6px;border-radius:6px;border:1px solid var(--border,#30363d);background:var(--bg,#0d1117);color:var(--text,#e6edf3);"></div>
            <div class="prop-group" style="flex:1;"><label>H %</label><input type="number" data-field="h" value="${w.h.toFixed(1)}" class="text-input" style="width:100%;padding:6px;border-radius:6px;border:1px solid var(--border,#30363d);background:var(--bg,#0d1117);color:var(--text,#e6edf3);"></div>
          </div>
          <div class="prop-group"><label>Z Index</label><input type="number" min="0" step="1" data-field="z" value="${Math.max(0, Number(w.z ?? 0))}" class="text-input" style="width:100%;padding:6px;border-radius:6px;border:1px solid var(--border,#30363d);background:var(--bg,#0d1117);color:var(--text,#e6edf3);"></div>
          <div class="prop-group"><label>Rotation (degrees)</label><input type="number" min="-180" max="180" step="1" data-prop="rotation" value="${rotation}" class="text-input" style="width:100%;padding:6px;border-radius:6px;border:1px solid var(--border,#30363d);background:var(--bg,#0d1117);color:var(--text,#e6edf3);"><small class="tpl-field-help">Rotate this widget around its center. Range: −180° to 180°.</small></div>
        </div>
      </details>
    `);
    // Wire the widget-actions toolbar (Duplicate / Delete).
    const btnDup = E.props.querySelector('#btn-duplicate-widget');
    if (btnDup) btnDup.onclick = () => this.duplicateSelectedWidget();
    const btnDel = E.props.querySelector('#btn-delete-widget');
    if (btnDel) btnDel.onclick = () => this.deleteSelectedWidget();
    const presetSelect = E.props.querySelector('#widget-preset-select');
    if (presetSelect) presetSelect.onchange = () => {
      const live = (this.currentLayout?.widgets || []).find(x => x.id === w.id) || w;
      live.props = live.props || {};
      const keepCounts = { shown_days: live.props.shown_days, max_events: live.props.max_events };
      if (!presetSelect.value) {
        if (live.type === 'weather') {
          const defaultPreset = presets.find(item => item.is_builtin) || presets.find(item => item.name === 'Weather Simple Rows');
          let defaultProps = {};
          try { defaultProps = typeof defaultPreset?.props_json === 'string' ? JSON.parse(defaultPreset.props_json) : (defaultPreset?.props_json || {}); } catch (_) { defaultProps = {}; }
          live.props = Object.assign({}, live.props, defaultProps, { preset: defaultPreset?.id || live.props.preset });
          if (keepCounts.shown_days != null) live.props.shown_days = keepCounts.shown_days;
          if (live.props.shown_days == null) live.props.shown_days = 4;
        } else {
          delete live.props.preset;
          delete live.props.template_elements;
          delete live.props.template_width;
          delete live.props.template_height;
          delete live.props.snap_grid;
        }
      } else {
        const preset = presets.find(item => item.id === presetSelect.value);
        let presetProps = {};
        try { presetProps = typeof preset?.props_json === 'string' ? JSON.parse(preset.props_json) : (preset?.props_json || {}); } catch (_) { presetProps = {}; }
        live.props = Object.assign({}, live.props, presetProps, { preset: presetSelect.value });
        if (keepCounts.shown_days != null) live.props.shown_days = keepCounts.shown_days;
        if (keepCounts.max_events != null) live.props.max_events = keepCounts.max_events;
        if (live.type === 'weather' && live.props.shown_days == null) live.props.shown_days = 4;
      }
      this.selectedWidget = live;
      this.saveCurrent();
      this.renderCanvas();
      this.renderProps();
    };
    const locationLookup = E.props.querySelector('#weather-location-lookup');
    const locationSearch = E.props.querySelector('#weather-location-search');
    const locationResults = E.props.querySelector('#weather-location-results');
    const locationStatus = E.props.querySelector('#weather-location-status');
    let locationMatches = [];
    if (locationLookup && locationSearch && locationResults) {
      locationLookup.onclick = async () => {
        const query = locationSearch.value.trim();
        if (!query) { if (locationStatus) locationStatus.textContent = 'Enter a city or place first.'; return; }
        locationLookup.disabled = true;
        if (locationStatus) locationStatus.textContent = 'Searching…';
        try {
          const response = await fetch('/api/weather/lookup?q=' + encodeURIComponent(query));
          if (!response.ok) throw new Error('Lookup failed');
          const data = await response.json();
          locationMatches = Array.isArray(data.results) ? data.results : [];
          locationResults.innerHTML = '<option value="">Select a location</option>' + locationMatches.map((place, index) => {
            const label = [place.name, place.admin1, place.country].filter(Boolean).join(', ');
            return `<option value="${index}">${this.esc(label)}</option>`;
          }).join('');
          locationResults.style.display = locationMatches.length ? '' : 'none';
          if (locationStatus) locationStatus.textContent = locationMatches.length ? 'Select the matching location.' : 'No locations found.';
        } catch (error) {
          if (locationStatus) locationStatus.textContent = 'Location lookup failed. Check the connection and try again.';
        } finally { locationLookup.disabled = false; }
      };
      locationResults.onchange = () => {
        const place = locationMatches[Number(locationResults.value)];
        if (!place) return;
        const live = (this.currentLayout?.widgets || []).find(x => x.id === w.id) || w;
        const label = [place.name, place.admin1, place.country].filter(Boolean).join(', ');
        live.props.city = label;
        live.props.location = label;
        live.props.lat = Number(place.latitude);
        live.props.lon = Number(place.longitude);
        this.selectedWidget = live;
        this.saveCurrent();
        this.renderCanvas();
        this.renderProps();
      };
    }
    const trafficLookup = E.props.querySelector('#traffic-area-lookup');
    const trafficSearch = E.props.querySelector('#traffic-area-search');
    const trafficResults = E.props.querySelector('#traffic-area-results');
    const trafficStatus = E.props.querySelector('#traffic-area-status');
    let trafficMatches = [];
    if (trafficLookup && trafficSearch && trafficResults) {
      trafficLookup.onclick = async () => {
        const query = trafficSearch.value.trim();
        if (!query) { if (trafficStatus) trafficStatus.textContent = 'Enter an area first.'; return; }
        trafficLookup.disabled = true;
        if (trafficStatus) trafficStatus.textContent = 'Searching…';
        try {
          const response = await fetch('/api/weather/lookup?q=' + encodeURIComponent(query));
          if (!response.ok) throw new Error('Lookup failed');
          const data = await response.json();
          trafficMatches = Array.isArray(data.results) ? data.results : [];
          trafficResults.innerHTML = '<option value="">Select an area</option>' + trafficMatches.map((place, index) => {
            const label = [place.name, place.admin1, place.country].filter(Boolean).join(', ');
            return `<option value="${index}">${this.esc(label)}</option>`;
          }).join('');
          trafficResults.style.display = trafficMatches.length ? '' : 'none';
          if (trafficStatus) trafficStatus.textContent = trafficMatches.length ? 'Select the matching area.' : 'No areas found.';
        } catch (_) {
          if (trafficStatus) trafficStatus.textContent = 'Area lookup failed. Check the connection and try again.';
        } finally { trafficLookup.disabled = false; }
      };
      trafficResults.onchange = () => {
        const place = trafficMatches[Number(trafficResults.value)];
        if (!place) return;
        const live = (this.currentLayout?.widgets || []).find(x => x.id === w.id) || w;
        live.props.area_label = [place.name, place.admin1, place.country].filter(Boolean).join(', ');
        live.props.lat = Number(place.latitude);
        live.props.lon = Number(place.longitude);
        this.selectedWidget = live;
        this.saveCurrent();
        this.renderCanvas();
        this.renderProps();
      };
    }
    const closureManager = E.props.querySelector('#traffic-closure-manager');
    const closureManagerStatus = E.props.querySelector('#traffic-closure-manager-status');
    const closureManagerList = E.props.querySelector('#traffic-closure-manager-list');
    if (closureManager && closureManagerStatus && closureManagerList) {
      closureManager.onclick = async () => {
        const live = (this.currentLayout?.widgets || []).find(x => x.id === w.id) || w;
        const p = live.props || {};
        if (!Number(p.lat) || !Number(p.lon)) {
          closureManagerStatus.textContent = 'Choose a traffic area first.';
          return;
        }
        closureManager.disabled = true;
        closureManagerStatus.textContent = 'Checking the live traffic feed…';
        try {
          const url = '/api/traffic/events?lat=' + encodeURIComponent(p.lat) + '&lon=' + encodeURIComponent(p.lon) + '&radius_miles=' + encodeURIComponent(p.radius_miles || 25);
          const response = await fetch(url, { cache: 'no-store' });
          if (!response.ok) throw new Error('Traffic request failed');
          const data = await response.json();
          if (data.available === false || data.configured === false) {
            closureManagerList.innerHTML = '';
            closureManagerStatus.textContent = data.message || 'Traffic feed is unavailable.';
            return;
          }
          const closures = (Array.isArray(data.events) ? data.events : []).filter(event => typeof window.ripleyTrafficIsClosure === 'function' ? window.ripleyTrafficIsClosure(event) : event.category === 'closure');
          const existing = Array.isArray(p.blocked_closures) ? p.blocked_closures : [];
          const activeKeys = new Set(closures.map(event => typeof window.ripleyTrafficEventKey === 'function' ? window.ripleyTrafficEventKey(event) : String(event.id || '')));
          const pruned = existing.filter(record => activeKeys.has(String(record.key || '')));
          if (pruned.length !== existing.length) {
            p.blocked_closures = pruned;
            this.selectedWidget = live;
            this.saveCurrent();
          }
          if (!closures.length) {
            closureManagerList.innerHTML = '<div style="font-size:.82rem;color:#888;">No active road closures are being reported.</div>';
            closureManagerStatus.textContent = 'The feed is current; previously blocked closures were cleared if they reopened.';
            return;
          }
          closureManagerList.innerHTML = closures.map((event, index) => {
            const key = typeof window.ripleyTrafficEventKey === 'function' ? window.ripleyTrafficEventKey(event) : String(event.id || index);
            const label = [event.road || 'Road closure', event.direction || '', event.description || ''].filter(Boolean).join(' · ');
            const checked = pruned.some(record => String(record.key || '') === key) ? ' checked' : '';
            return `<label style="display:flex;align-items:flex-start;gap:.45rem;padding:.4rem .5rem;border:1px solid var(--border,#30363d);border-radius:6px;background:rgba(255,255,255,.03);font-size:.82rem;"><input type="checkbox" data-traffic-block-key="${this.esc(key)}"${checked} style="margin-top:.15rem;"><span>${this.esc(label)}</span></label>`;
          }).join('') + '<button type="button" id="traffic-closure-save-blocks" class="btn btn-primary btn-sm" style="align-self:flex-start;margin-top:.25rem;">Save blocked closures</button>';
          closureManagerStatus.textContent = `${closures.length} active road closure${closures.length === 1 ? '' : 's'} found.`;
          const saveBlocks = closureManagerList.querySelector('#traffic-closure-save-blocks');
          if (saveBlocks) saveBlocks.onclick = () => {
            const selectedKeys = new Set(Array.from(closureManagerList.querySelectorAll('[data-traffic-block-key]:checked')).map(input => input.dataset.trafficBlockKey));
            live.props.manual_closure_blocking = true;
            live.props.blocked_closures = closures.filter(event => {
              const key = typeof window.ripleyTrafficEventKey === 'function' ? window.ripleyTrafficEventKey(event) : String(event.id || '');
              return selectedKeys.has(key);
            }).map(event => ({
              key: typeof window.ripleyTrafficEventKey === 'function' ? window.ripleyTrafficEventKey(event) : String(event.id || ''),
              id: event.id || '', category: event.category || 'closure', road: event.road || '', direction: event.direction || '', description: event.description || '', latitude: event.latitude, longitude: event.longitude
            }));
            this.selectedWidget = live;
            this.saveCurrent();
            closureManagerStatus.textContent = `${live.props.blocked_closures.length} closure${live.props.blocked_closures.length === 1 ? '' : 's'} blocked. The block clears automatically when the feed reports the road open.`;
            this.renderCanvas();
          };
        } catch (error) {
          closureManagerList.innerHTML = '';
          closureManagerStatus.textContent = 'Traffic feed check failed. Try again later.';
        } finally { closureManager.disabled = false; }
      };
    }
    E.props.querySelectorAll('[data-prop]').forEach(i => {
      i.oninput = i.onchange = () => {
        const k = i.dataset.prop;
        // Re-resolve `w` from the live layout each event so prop edits
        // keep hitting the object the canvas is actually rendering, even
        // if a previous save round-trip swapped the array contents.
        const live = (this.currentLayout?.widgets || []).find(x => x.id === w.id) || w;
        if (i.type === 'checkbox') live.props[k] = i.checked;
        else if (k === 'rotation') {
          const parsed = Number(i.value);
          live.props[k] = Number.isFinite(parsed) ? Math.max(-180, Math.min(180, parsed)) : 0;
          i.value = String(live.props[k]);
        } else if (k === 'font_size') {
          const parsed = Number(i.value);
          const fallback = Number(live.props[k]) || (live.type === 'countdown' ? 48 : 24);
          const maximum = live.type === 'countdown' ? 160 : 400;
          live.props[k] = Number.isFinite(parsed) ? Math.max(6, Math.min(maximum, parsed)) : fallback;
          if (live.type === 'countdown') i.value = String(live.props[k]);
        } else if (/^radius(?:_(?:tl|tr|br|bl))?$/.test(k)) {
          const radius = Number.isFinite(Number(i.value)) ? Math.max(0, Math.min(999, Number(i.value))) : 0;
          live.props[k] = radius;
          i.value = String(radius);
        } else live.props[k] = i.value;
        if ((w.type === 'rectangle' || w.type === 'image') && k === 'radius') {
          const radius = Math.max(0, Math.min(999, Number(i.value) || 0));
          live.props.radius = radius;
          live.props.radius_tl = radius;
          live.props.radius_tr = radius;
          live.props.radius_br = radius;
          live.props.radius_bl = radius;
        }
        this.selectedWidget = live;
        this.saveCurrent();
        this.renderCanvas();
      };
    });
    if (w.type === 'image') {
      const liveImage = () => (this.currentLayout?.widgets || []).find(x => x.id === w.id) || w;
      const library = E.props.querySelector('[data-image-library]');
      const upload = E.props.querySelector('[data-image-upload]');
      const uploadButton = E.props.querySelector('[data-image-upload-button]');
      const status = E.props.querySelector('[data-image-status]');
      const browseButton = E.props.querySelector('[data-action="browse-image-playlist"]');
      const clampSeconds = value => Math.max(1, Math.min(3600, Number(value) || 10));
      const refreshImageEditor = () => {
        const live = liveImage();
        this.selectedWidget = live;
        this.saveCurrent();
        this.renderCanvas();
        this.renderProps();
      };
      const moveImage = (from, to) => {
        const live = liveImage();
        const items = Array.isArray(live.props.playlist) ? live.props.playlist.slice() : [];
        if (from < 0 || from >= items.length || to < 0 || to >= items.length) return;
        const [item] = items.splice(from, 1);
        items.splice(to, 0, item);
        live.props.playlist = items;
        refreshImageEditor();
      };
      if (browseButton) browseButton.onclick = () => this.openImagePlaylistPicker();
      E.props.querySelectorAll('[data-image-move-up]').forEach(button => {
        button.onclick = () => moveImage(Number(button.dataset.imageMoveUp), Number(button.dataset.imageMoveUp) - 1);
      });
      E.props.querySelectorAll('[data-image-move-down]').forEach(button => {
        button.onclick = () => moveImage(Number(button.dataset.imageMoveDown), Number(button.dataset.imageMoveDown) + 1);
      });
      E.props.querySelectorAll('[data-image-remove-index]').forEach(button => {
        button.onclick = () => {
          const live = liveImage();
          const items = Array.isArray(live.props.playlist) ? live.props.playlist.slice() : [];
          const index = Number(button.dataset.imageRemoveIndex);
          if (!items[index]) return;
          items.splice(index, 1);
          live.props.playlist = items;
          live.props.url = items[0]?.url || '';
          refreshImageEditor();
        };
      });
      E.props.querySelectorAll('[data-image-duration-index]').forEach(input => {
        input.onchange = () => {
          const live = liveImage();
          const items = Array.isArray(live.props.playlist) ? live.props.playlist.slice() : [];
          const index = Number(input.dataset.imageDurationIndex);
          if (!items[index]) return;
          items[index] = { ...items[index], duration: clampSeconds(input.value) };
          live.props.playlist = items;
          refreshImageEditor();
        };
      });
      let draggedIndex = null;
      E.props.querySelectorAll('[data-image-playlist-row]').forEach(row => {
        row.addEventListener('dragstart', event => {
          draggedIndex = Number(row.dataset.imagePlaylistRow);
          if (event.dataTransfer) {
            event.dataTransfer.effectAllowed = 'move';
            event.dataTransfer.setData('text/plain', String(draggedIndex));
          }
        });
        row.addEventListener('dragover', event => event.preventDefault());
        row.addEventListener('drop', event => {
          event.preventDefault();
          const targetIndex = Number(row.dataset.imagePlaylistRow);
          if (draggedIndex == null || draggedIndex === targetIndex) return;
          moveImage(draggedIndex, targetIndex);
          draggedIndex = null;
        });
      });
      const defaultDuration = E.props.querySelector('[data-image-default-duration]');
      if (defaultDuration) defaultDuration.onchange = () => {
        const live = liveImage();
        live.props.slide_duration = clampSeconds(defaultDuration.value);
        refreshImageEditor();
      };
      const applyDuration = E.props.querySelector('[data-action="apply-image-duration"]');
      if (applyDuration) applyDuration.onclick = () => {
        const live = liveImage();
        const seconds = clampSeconds(defaultDuration?.value);
        live.props.slide_duration = seconds;
        live.props.playlist = (Array.isArray(live.props.playlist) ? live.props.playlist : []).map(item => ({ ...item, duration: seconds }));
        refreshImageEditor();
      };
      const clearPlaylist = E.props.querySelector('[data-action="clear-image-playlist"]');
      if (clearPlaylist) clearPlaylist.onclick = () => {
        const live = liveImage();
        if (!Array.isArray(live.props.playlist) || !live.props.playlist.length) return;
        if (!window.confirm('Clear all images from this slideshow?')) return;
        live.props.playlist = [];
        live.props.url = '';
        refreshImageEditor();
      };
      const applyImage = url => {
        const live = liveImage();
        live.props.url = url;
        this.selectedWidget = live;
        this.saveCurrent();
        this.renderCanvas();
      };
      if (library) {
        library.onchange = () => { if (library.value) applyImage(library.value); };
        API.listFiles('images').then(files => {
          const rows = Array.isArray(files) ? files : [];
          library.innerHTML = '<option value="">Choose a saved image…</option>' + rows.map(file => {
            const url = file.url || ('/uploads/images/' + file.name);
            return `<option value="${this.esc(url)}" ${url === (p.url || '') ? 'selected' : ''}>${this.esc(file.name || url)}</option>`;
          }).join('');
        }).catch(() => { library.innerHTML = '<option value="">No saved images found</option>'; });
      }
      if (uploadButton && upload) {
        uploadButton.onclick = () => upload.click();
        upload.onchange = async () => {
          const file = upload.files && upload.files[0];
          upload.value = '';
          if (!file) return;
          try {
            uploadButton.disabled = true;
            const uploaded = await API.uploadFile('images', file);
            const live = liveImage();
            live.props.playlist = Array.isArray(live.props.playlist) ? live.props.playlist.slice() : [];
            if (!live.props.playlist.some(item => item.url === uploaded.url)) {
              live.props.playlist.push({ name: file.name, url: uploaded.url, duration: clampSeconds(live.props.slide_duration) });
            }
            live.props.url = uploaded.url;
            this.selectedWidget = live;
            this.saveCurrent();
            this.renderCanvas();
            this.renderProps();
          } catch (error) {
            const detail = error && error.message ? error.message : 'unknown error';
            if (status) status.textContent = 'Image upload failed: ' + detail;
          } finally { uploadButton.disabled = false; }
        };
      }
    }
    if (w.type === 'video') {
      const liveVideo = () => (this.currentLayout?.widgets || []).find(x => x.id === w.id) || w;
      const upload = E.props.querySelector('[data-video-upload]');
      const uploadButton = E.props.querySelector('[data-video-upload-button]');
      const browseButton = E.props.querySelector('[data-action="browse-video-playlist"]');
      const status = E.props.querySelector('[data-video-status]');
      const refreshVideoEditor = () => {
        const live = liveVideo();
        this.selectedWidget = live;
        this.saveCurrent();
        this.renderCanvas();
        this.renderProps();
      };
      const moveVideo = (from, to) => {
        const live = liveVideo();
        const items = Array.isArray(live.props.playlist) ? live.props.playlist.slice() : [];
        if (from < 0 || from >= items.length || to < 0 || to >= items.length) return;
        const [item] = items.splice(from, 1);
        items.splice(to, 0, item);
        live.props.playlist = items;
        refreshVideoEditor();
      };
      if (browseButton) browseButton.onclick = () => this.openVideoPlaylistPicker();
      E.props.querySelectorAll('[data-video-move-up]').forEach(button => {
        button.onclick = () => moveVideo(Number(button.dataset.videoMoveUp), Number(button.dataset.videoMoveUp) - 1);
      });
      E.props.querySelectorAll('[data-video-move-down]').forEach(button => {
        button.onclick = () => moveVideo(Number(button.dataset.videoMoveDown), Number(button.dataset.videoMoveDown) + 1);
      });
      E.props.querySelectorAll('[data-video-remove-index]').forEach(button => {
        button.onclick = () => {
          const live = liveVideo();
          const items = Array.isArray(live.props.playlist) ? live.props.playlist.slice() : [];
          const index = Number(button.dataset.videoRemoveIndex);
          if (!items[index]) return;
          items.splice(index, 1);
          live.props.playlist = items;
          live.props.url = items[0]?.url || '';
          refreshVideoEditor();
        };
      });
      let draggedIndex = null;
      E.props.querySelectorAll('[data-video-playlist-row]').forEach(row => {
        row.addEventListener('dragstart', event => {
          draggedIndex = Number(row.dataset.videoPlaylistRow);
          if (event.dataTransfer) {
            event.dataTransfer.effectAllowed = 'move';
            event.dataTransfer.setData('text/plain', String(draggedIndex));
          }
        });
        row.addEventListener('dragover', event => event.preventDefault());
        row.addEventListener('drop', event => {
          event.preventDefault();
          const targetIndex = Number(row.dataset.videoPlaylistRow);
          if (draggedIndex == null || draggedIndex === targetIndex) return;
          moveVideo(draggedIndex, targetIndex);
          draggedIndex = null;
        });
      });
      const clearPlaylist = E.props.querySelector('[data-action="clear-video-playlist"]');
      if (clearPlaylist) clearPlaylist.onclick = () => {
        const live = liveVideo();
        if (!Array.isArray(live.props.playlist) || !live.props.playlist.length) return;
        if (!window.confirm('Clear all videos from this playlist?')) return;
        live.props.playlist = [];
        live.props.url = '';
        refreshVideoEditor();
      };
      if (uploadButton && upload) { uploadButton.onclick = () => upload.click(); upload.onchange = async () => { const file = upload.files && upload.files[0]; upload.value = ''; if (!file) return; try { uploadButton.disabled = true; const uploaded = await API.uploadFile('videos', file); const live = liveVideo(); live.props.playlist = Array.isArray(live.props.playlist) ? live.props.playlist.slice() : []; live.props.playlist.push({ name: file.name, url: uploaded.url }); live.props.url = live.props.playlist[0].url; this.selectedWidget = live; this.saveCurrent(); this.renderCanvas(); this.renderProps(); } catch (err) { if (status) status.textContent = 'Video upload failed: ' + (err?.message || 'unknown error'); } finally { uploadButton.disabled = false; } }; }
    }
    // Special handler for textarea-backed multi-line option lists. Stores
    // the value as a string[] on the widget's props, splitting on newlines.
    E.props.querySelectorAll('[data-prop-options]').forEach(t => {
      t.oninput = () => {
        const live = (this.currentLayout?.widgets || []).find(x => x.id === w.id) || w;
        live.props.options = String(t.value || '').split('\n').map(s => s.trim()).filter(Boolean);
        this.selectedWidget = live;
        this.saveCurrent();
        this.renderCanvas();
      };
    });
    E.props.querySelectorAll('[data-prop-playlist]').forEach(t => {
      t.oninput = () => {
        const live = (this.currentLayout?.widgets || []).find(x => x.id === w.id) || w;
        live.props.playlist = String(t.value || '').split('\n').map(line => line.trim()).filter(Boolean).map(line => {
          const cut = line.indexOf('|');
          return cut >= 0 ? { name: line.slice(0, cut).trim(), url: line.slice(cut + 1).trim() } : { name: '', url: line };
        }).filter(track => track.url);
        this.selectedWidget = live;
        this.saveCurrent();
        this.renderCanvas();
      };
    });
    // Font picker "Browse…" buttons live in the props panel next to
    // the font_family input. We delegate the click here (rather than
    // inline onclick) so the handler survives prop-panel re-renders.
    E.props.querySelectorAll('button[data-action="browse-font"]').forEach(b => {
      b.onclick = (ev) => {
        ev.preventDefault();
        this.openFontPicker();
      };
    });
    E.props.querySelectorAll('button[data-action="pick-countdown-date"]').forEach(b => {
      b.onclick = (ev) => {
        ev.preventDefault();
        const input = E.props.querySelector('input[data-prop="targetDate"]');
        if (!input) return;
        if (typeof input.showPicker === 'function') input.showPicker();
        else input.focus();
      };
    });
    const syncCountdownDateTime = () => {
      const dateInput = E.props.querySelector('input[data-countdown-date]');
      const timeInput = E.props.querySelector('input[data-countdown-time]');
      const targetInput = E.props.querySelector('input[data-prop="targetDate"]');
      if (!dateInput || !timeInput || !targetInput || !dateInput.value || !timeInput.value) return;
      targetInput.value = dateInput.value + 'T' + timeInput.value;
      targetInput.dispatchEvent(new Event('change', { bubbles: true }));
    };
    E.props.querySelectorAll('input[data-countdown-date], input[data-countdown-time]').forEach(i => {
      i.oninput = i.onchange = syncCountdownDateTime;
    });
    const countdownLive = () => (this.currentLayout?.widgets || []).find(x => x.id === w.id) || w;
    const saveCountdownProp = (key, value, rerenderProps = false) => {
      const live = countdownLive();
      live.props[key] = value;
      this.selectedWidget = live;
      this.saveCurrent();
      this.renderCanvas();
      if (rerenderProps) this.renderProps();
    };
    const modeSelect = E.props.querySelector('select[data-countdown-mode]');
    if (modeSelect) modeSelect.onchange = () => saveCountdownProp('countdown_mode', modeSelect.value, true);
    const durationValue = E.props.querySelector('[data-countdown-duration-value]');
    if (durationValue) durationValue.oninput = durationValue.onchange = () => saveCountdownProp('duration_value', Math.max(1, Number(durationValue.value || 1)));
    const durationUnit = E.props.querySelector('[data-countdown-duration-unit]');
    if (durationUnit) durationUnit.onchange = () => saveCountdownProp('duration_unit', durationUnit.value);
    // Populate every font-family <select> in this panel. The selects
    // are rendered with a placeholder __FONT_FAMILY_OPTIONS__ marker
    // string that we swap out for real <option>s here. We do this
    // after the rest of the panel is set up so the change handler
    // wired by the [data-prop] loop above picks up our real options.
    this._populateFontFamilySelects(E, w);
    // Custom font input: when the user types into the custom field,
    // copy its value to font_family (the select keeps its current
    // "Custom…" selection).
    E.props.querySelectorAll('input[data-prop="font_family_custom"]').forEach(inp => {
      inp.oninput = () => {
        const live = (this.currentLayout?.widgets || []).find(x => x.id === w.id) || w;
        live.props.font_family = inp.value;
        this.selectedWidget = live;
        this.saveCurrent();
        this.renderCanvas();
      };
    });
    E.props.querySelectorAll('[data-field]').forEach(i => {
      i.oninput = () => {
        const k = i.dataset.field;
        const live = (this.currentLayout?.widgets)?.find(x => x.id === w.id) || w;
        live[k] = k === 'z' ? Math.max(0, parseInt(i.value || '1', 10)) : parseFloat(i.value || '0');
        this.selectedWidget = live;
        this.saveCurrent();
        this.renderCanvas();
      };
    });
  },

  getWidgetLabel(t) {
    const labels = {
      text: 'Text', image: 'Image', video: 'Video', clock: 'Clock', date: 'Date',
      datetime: 'Date & Time', weather: 'Weather', countdown: 'Countdown', sound: 'Sound', embed: 'Embed',
      webpage: 'Web Page', calendar: 'Calendar', news: 'News Ticker', music: 'Music',
      rectangle: 'Shape', button: 'Button', dropdown: 'Drop-down',
      checkbox: 'Checkbox', 'checkbox-list': 'Checkbox List', radio: 'Radio Buttons',
      textbox: 'Text Box', keyboard: 'Keyboard',
    };
    return labels[t] || t;
  },

  // CRUD
  async createLayout() {
    const name = prompt('Layout name:', 'Untitled Layout');
    if (!name) return;
    // `layouts` columns: id, name, interactive, page_type, timing_mode, duration, shuffle, items, created_at, updated_at
    const layout = { id: '', name: name.trim(), interactive: false, page_type: 'single', timing_mode: 'global', duration: 15, shuffle: false, folder_id: this.browserFolderId || null, items: [], widgets: [] };
    try {
      const saved = await API.saveLayout(layout);
      this.layouts.unshift(saved);
      this.switchToLayout(saved.id);
      this.renderSidebar();
    } catch (e) { alert('Failed to create layout: ' + e.message); }
  },

  async saveCurrent(options = {}) {
    const silent = options.silent !== false;
    const layout = this.currentLayout;
    if (!layout) return null;
    // Queue writes so a fast drag/input sequence cannot let an older request
    // finish after a newer one and overwrite the latest draft.
    const snapshot = JSON.parse(JSON.stringify(layout));
    const operation = this._savePromise.then(async () => {
      const saved = await API.saveLayout(snapshot);
      this._normalizeLayout(saved);
      const idx = this.layouts.findIndex(l => l.id === saved.id);
      if (idx >= 0) this.layouts[idx] = saved;
      if (this.selectedWidgetId && saved.widgets) {
        const live = saved.widgets.find(x => x.id === this.selectedWidgetId);
        if (live) this.selectedWidget = live;
      }
      this.updateInfo();
      return saved;
    });
    this._savePromise = operation.catch(() => {});
    try {
      return await operation;
    } catch (e) {
      if (!silent) throw e;
      console.warn(e);
      return null;
    }
  },

  async publishCurrent() {
    const layout = this.currentLayout;
    if (!layout || this._publishBusy) return;
    this._publishBusy = true;
    const E = this.el;
    if (E.publishStatus) E.publishStatus.textContent = 'Saving and publishing…';
    this.updateInfo();
    try {
      await this.saveCurrent({ silent: false });
      const result = await API.publishLayout(layout.id);
      if (E.publishStatus) {
        const epoch = result?.refresh_epoch;
        E.publishStatus.textContent = epoch == null ? 'Published · checking players…' : `Published · refresh ${epoch} · checking players…`;
      }
      let propagation = null;
      for (let attempt = 0; attempt < 15; attempt += 1) {
        propagation = await API.getLayoutPropagationStatus(layout.id);
        const total = Number(propagation?.target_count || 0);
        const ack = Number(propagation?.acknowledged_count || 0);
        const offline = Number(propagation?.offline_count || 0);
        const pending = Number(propagation?.pending_count || 0);
        if (E.publishStatus) {
          if (total === 0) {
            E.publishStatus.textContent = 'Published · no assigned displays';
          } else if (ack >= total) {
            E.publishStatus.textContent = `Published · ${ack}/${total} displays acknowledged`;
          } else {
            const suffix = offline ? ` · ${offline} offline` : (pending ? ` · ${pending} pending` : '');
            E.publishStatus.textContent = `Published · ${ack}/${total} displays acknowledged${suffix}`;
          }
        }
        if (total === 0 || ack >= total) break;
        await new Promise(resolve => setTimeout(resolve, 3000));
      }

    } catch (e) {
      if (E.publishStatus) E.publishStatus.textContent = `Publish failed: ${e.message}`;
    } finally {
      this._publishBusy = false;
      this.updateInfo();
    }
  },



  deleteSelectedWidget() {
    // Immediate delete (no confirm) per the editor toolbar buttons.
    if (!this.selectedWidgetId) return;
    const layout = this.currentLayout;
    if (!layout) return;
    layout.widgets = (layout.widgets || []).filter(w => w.id !== this.selectedWidgetId);
    this.selectedWidgetId = null;
    this.selectedWidget = null;
    this.saveCurrent();
    this.renderCanvas();
    this.renderProps();
  },

  duplicateSelectedWidget() {
    if (!this.selectedWidgetId) return;
    const layout = this.currentLayout;
    if (!layout) return;
    const src = (layout.widgets || []).find(w => w.id === this.selectedWidgetId);
    if (!src) return;
    const id = (crypto && crypto.randomUUID) ? crypto.randomUUID() : ('w-' + Date.now() + '-' + Math.random().toString(36).slice(2, 8));
    const copy = JSON.parse(JSON.stringify(src));
    copy.id = id;
    copy.x = (src.x ?? 0) + 2;
    copy.y = (src.y ?? 0) + 2;
    layout.widgets = layout.widgets || [];
    layout.widgets.push(copy);
    this.selectedWidgetId = id;
    this.selectedWidget = copy;
    this.saveCurrent();
    this.renderCanvas();
    this.renderProps();
  },

  bringToFront() {
    if (!this.selectedWidgetId) return;
    const layout = this.currentLayout;
    if (!layout) return;
    const widgets = layout.widgets || [];
    const maxZ = widgets.reduce((m, w) => Math.max(m, w.z || 0), 0);
    const w = widgets.find(x => x.id === this.selectedWidgetId);
    if (!w) return;
    w.z = maxZ + 1;
    this.saveCurrent();
    this.renderCanvas();
  },

  sendToBack() {
    if (!this.selectedWidgetId) return;
    const layout = this.currentLayout;
    if (!layout) return;
    const widgets = layout.widgets || [];
    const target = widgets.find(x => x.id === this.selectedWidgetId);
    if (!target) return;
    // Send to back = drop the target to z=0 and bump every other
    // widget up by 1. The relative stacking order between the
    // remaining widgets is preserved; the target becomes the new
    // bottom of the stack. All z values stay >= 0 because we
    // started at 0 and only incremented.
    for (const w of widgets) {
      if (w.id === target.id) w.z = 0;
      else w.z = (Number(w.z) || 0) + 1;
    }
    this.saveCurrent();
    this.renderCanvas();
  },

  resetWidgetRotation() {
    if (!this.selectedWidgetId) return;
    const layout = this.currentLayout;
    if (!layout) return;
    const widget = (layout.widgets || []).find(w => w.id === this.selectedWidgetId);
    if (!widget) return;
    widget.props = widget.props || {};
    widget.props.rotation = 0;
    this.selectedWidget = widget;
    this.saveCurrent();
    this.renderCanvas();
    this.renderProps();
  },

  showContextMenu(x, y, widgetId) {
    // Auto-select the right-clicked widget so the user can act on it.
    const layout = this.currentLayout;
    if (layout) {
      const w = (layout.widgets || []).find(x => x.id === widgetId);
      if (w) {
        this.selectedWidgetId = widgetId;
        this.selectedWidget = w;
        this.paneMode = 'widget';
        this.renderProps();
      }
    }
    this.closeContextMenu();
    const menu = document.createElement('div');
    menu.className = 'interactive-context-menu';
    menu.dataset.ownWidget = widgetId;
    menu.innerHTML = `
      <button type="button" data-action="bring-front">⤴ Send to Front</button>
      <button type="button" data-action="send-back">⤵ Send to Back</button>
      <button type="button" data-action="reset-rotation">↻ Reset Rotation</button>
      <hr style="border:none;border-top:1px solid #444;margin:4px 0;">
      <button type="button" data-action="duplicate">⎘ Duplicate</button>
      <hr style="border:none;border-top:1px solid #444;margin:4px 0;">
      <button type="button" data-action="delete" class="interactive-context-danger">🗑 Delete</button>
    `;
    // Clamp position so menu doesn't overflow viewport.
    menu.style.left = `${x}px`;
    menu.style.top = `${y}px`;
    document.body.appendChild(menu);
    const r = menu.getBoundingClientRect();
    if (r.right > window.innerWidth) menu.style.left = `${Math.max(4, x - r.width)}px`;
    if (r.bottom > window.innerHeight) menu.style.top = `${Math.max(4, y - r.height)}px`;
    menu.addEventListener('click', e => {
      const btn = e.target.closest('button[data-action]');
      if (!btn) return;
      const action = btn.dataset.action;
      this.closeContextMenu();
      if (action === 'duplicate') this.duplicateSelectedWidget();
      else if (action === 'bring-front') this.bringToFront();
      else if (action === 'send-back') this.sendToBack();
      else if (action === 'reset-rotation') this.resetWidgetRotation();
      else if (action === 'delete') this.deleteSelectedWidget();
    });
  },

  closeContextMenu() {
    document.querySelectorAll('.interactive-context-menu').forEach(m => m.remove());
  },

  exportJSON() {
    const data = JSON.stringify(this.currentLayout, null, 2);
    const blob = new Blob([data], { type: 'application/json' });
    const url = URL.createObjectURL(blob);
    const a = document.createElement('a');
    a.href = url;
    a.download = `layout-${this.currentLayout?.name || 'export'}.json`;
    a.click();
    URL.revokeObjectURL(url);
  },
  async deleteCurrent() {
    if (!this.currentLayout || !this.currentLayout.id) return;
    this.requestDeleteLayout(this.currentLayout.id);
  },
  async deleteLayout(id, confirmed = false) {
    const layout = this.layouts.find(l => l.id === id);
    if (!layout) return;
    if (!confirmed) {
      this.requestDeleteLayout(id);
      return;
    }
    try {
      await API.deleteLayout(id);
      this.layouts = this.layouts.filter(l => l.id !== id);
      if (this.currentLayoutId === id) {
        this.currentLayoutId = null;
        this.currentLayout = null;
        if (this.layouts.length) this.switchToLayout(this.layouts[0].id);
        else this.renderCanvas();
      }
      this.renderSidebar();
    } catch (e) { alert('Failed to delete: ' + e.message); }
  },

  openPreview() {
    if (!this.currentLayoutId) return;
    const url = `/preview.html?layout=${encodeURIComponent(this.currentLayoutId)}&preview=1&t=${Date.now()}`;
    if (this._previewWindow && !this._previewWindow.closed) {
      this._previewWindow.close();
    }
    this._previewWindow = window.open(url, 'screentra-cast-layout-preview');
    if (this._previewWindow) this._previewWindow.focus();
  },

  exportAll() {
    const data = { layouts: this.layouts };
    const blob = new Blob([JSON.stringify(data, null, 2)], { type: 'application/json' });
    const url = URL.createObjectURL(blob);
    const a = document.createElement('a');
    a.href = url;
    a.download = `signage-export-${Date.now()}.json`;
    a.click();
    URL.revokeObjectURL(url);
  },

  esc(s) { return s == null ? '' : String(s).replace(/[&<>"']/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[c]); },

  // ================== Font Picker (text / clock / date / countdown) ==================
  // The font-picker.html iframe posts a { type: 'font-picked', cssValue }
  // message when the user clicks "Use This Font". We listen once on
  // the window and route the value into the active widget's
  // font_family prop. The iframe is opened by openFontPicker() below,
  // which is wired to the "Browse…" button next to the font_family
  // input in the props panel.
  _fontPickerMessageHandler: null,
  _fontPickerIframe: null,
  _fontPickerOverlay: null,
  _installedFonts: null,           // cached installed-fonts list
  _installedFontsAt: 0,           // ms timestamp
  _installedFontsCacheMs: 30000,  // refresh every 30s
  _installedFontsPromise: null,   // dedupe concurrent fetches

  // System-font shortcuts: always available, no install required.
  // Match the ids used by font-picker.html so the dropdown and the
  // picker show identical entries.
  _systemFontOptions: [
    { value: 'sans-serif', label: 'Sans Serif (browser default)', cssValue: 'sans-serif' },
    { value: 'serif', label: 'Serif (browser default)', cssValue: 'serif' },
    { value: 'monospace', label: 'Monospace (browser default)', cssValue: 'monospace' },
    { value: 'system-ui', label: 'System UI', cssValue: 'system-ui, -apple-system, "Segoe UI", Roboto, sans-serif' },
    { value: 'system-serif', label: 'System Serif', cssValue: 'Georgia, "Times New Roman", serif' },
    { value: 'system-mono', label: 'System Monospace', cssValue: '"SF Mono", Consolas, "Liberation Mono", monospace' },
  ],

  async _fetchInstalledFonts() {
    // Cache-bust after a font install: if the picker closed within
    // the last 5s, force a fresh fetch.
    var now = Date.now();
    if (this._installedFonts && this._installedFontsPromise == null &&
        (now - this._installedFontsAt) < this._installedFontsCacheMs) {
      return this._installedFonts;
    }
    if (this._installedFontsPromise) return this._installedFontsPromise;
    var self = this;
    this._installedFontsPromise = (async () => {
      try {
        var headers = {};
        try {
          var tok = localStorage.getItem('signage.adminToken');
          if (tok) headers['Authorization'] = 'Bearer ' + tok;
        } catch (_e) {}
        var r = await fetch('/api/fonts/installed', { headers: headers });
        if (!r.ok) throw new Error('HTTP ' + r.status);
        var list = await r.json();
        // Make installed Google Fonts available to the editor canvas itself.
        // The picker iframe has its own @font-face rules, but those do not
        // cross the iframe boundary into the parent editor document.
        var faceStyle = document.getElementById('installed-font-faces');
        if (!faceStyle) {
          faceStyle = document.createElement('style');
          faceStyle.id = 'installed-font-faces';
          document.head.appendChild(faceStyle);
        }
        faceStyle.textContent = list.filter(function (f) {
          return f && f.family && f.weight && f.url;
        }).map(function (f) {
          var family = String(f.family).replace(/\\/g, '\\\\').replace(/"/g, '\\\\"');
          var url = String(f.url).replace(/"/g, '%22');
          return '@font-face{font-family:"' + family + '";font-style:normal;font-weight:' + Number(f.weight) + ';src:url("' + url + '") format("woff2");font-display:swap;}';
        }).join('\n');
        // Dedupe by family: the API returns one row per weight, but
        // the dropdown shows one row per family. Pick the first entry.
        var byFamily = {};
        for (var i = 0; i < list.length; i++) {
          var f = list[i];
          if (!byFamily[f.family]) byFamily[f.family] = f;
        }
        self._installedFonts = Object.keys(byFamily)
          .sort()
          .map(function (family) {
            var f = byFamily[family];
            return {
              value: '"' + family + '", sans-serif',
              label: family,
              cssValue: '"' + family + '", sans-serif',
              family: family,
            };
          });
        self._installedFontsAt = Date.now();
        return self._installedFonts;
      } catch (_e) {
        // Network failure: return whatever we had, or empty.
        return self._installedFonts || [];
      } finally {
        self._installedFontsPromise = null;
      }
    })();
    return this._installedFontsPromise;
  },

  // Render the <option> list for the font-family <select>. The
  // current value (whether it's a system, installed, or custom
  // CSS string) is preserved so the user sees their existing
  // choice highlighted.
  async _buildFontFamilyOptions(currentValue) {
    var opts = [];
    // Group header for system fonts.
    opts.push('<optgroup label="System">');
    for (var i = 0; i < this._systemFontOptions.length; i++) {
      var s = this._systemFontOptions[i];
      opts.push('<option value="' + this.esc(s.value) + '"' +
        (currentValue === s.value || currentValue === s.cssValue ? ' selected' : '') +
        '>' + this.esc(s.label) + '</option>');
    }
    opts.push('</optgroup>');
    // Installed fonts.
    var installed = await this._fetchInstalledFonts();
    if (installed && installed.length) {
      opts.push('<optgroup label="Installed (' + installed.length + ')">');
      var currentInInstalled = false;
      for (var j = 0; j < installed.length; j++) {
        var f = installed[j];
        var sel = currentValue === f.value ? ' selected' : '';
        if (sel) currentInInstalled = true;
        opts.push('<option value="' + this.esc(f.value) + '"' + sel + '>' + this.esc(f.label) + '</option>');
      }
      opts.push('</optgroup>');
    } else {
      opts.push('<optgroup label="Installed (none — click Browse to add fonts)"></optgroup>');
    }
    // Custom option for arbitrary CSS strings. Preserves the value
    // if it's not in any of the above groups so the user's existing
    // choice isn't lost.
    var isStandard = this._systemFontOptions.some(function (s) {
      return s.value === currentValue || s.cssValue === currentValue;
    }) || (installed && installed.some(function (f) { return f.value === currentValue; }));
    var customSel = (currentValue && !isStandard && !currentInInstalled) ? ' selected' : '';
    opts.push('<option value="__custom__"' + customSel + '>Custom…</option>');
    return opts.join('');
  },

  // Walk the props panel and populate every font-family select.
  // Called by renderLayoutWidgetProps after the panel HTML is in
  // place. Async because installed fonts are fetched.
  async _populateFontFamilySelects(E, w) {
    if (!E || !E.props) return;
    var selects = E.props.querySelectorAll('select.font-family-select');
    if (!selects.length) return;
    var live = (this.currentLayout?.widgets || []).find(function (x) { return x.id === w.id; }) || w;
    var currentValue = (live.props && (live.props.font_family || live.props.fontFamily)) || 'sans-serif';
    // Fetch options once; reuse for every select (the widget prop
    // forms only have one font-family select, but if there were
    // more they would share the same options).
    var optsHtml = await this._buildFontFamilyOptions(currentValue);
    for (var i = 0; i < selects.length; i++) {
      var sel = selects[i];
      // The select was rendered with __FONT_FAMILY_OPTIONS__ as
      // a placeholder text node; replace it with the real options.
      sel.innerHTML = optsHtml;
      // The existing [data-prop] change handler reads .value on
      // change and writes to live.props.font_family. We just need
      // to add a special case for "Custom…" — when the user picks
      // that, show the custom input.
      var customInput = E.props.querySelector('input[data-prop="font_family_custom"]');
      var customSelectId = sel.getAttribute('data-select-id');
      // The widget's font_family_custom input is unique per widget
      // (only one custom input per prop form), but if there were
      // multiple selects in the same form we'd need scoping.
      // Filter by data-select-id to be safe.
      var scopedCustom = E.props.querySelector('input[data-prop="font_family_custom"]');
      sel.addEventListener('change', (function (selectEl, customEl) {
        return function () {
          if (selectEl.value === '__custom__') {
            if (customEl) customEl.style.display = '';
            if (customEl) customEl.focus();
          } else {
            if (customEl) customEl.style.display = 'none';
          }
        };
      })(sel, scopedCustom));
      // If the current value is custom (not in any optgroup),
      // show the custom input on first render.
      if (sel.value === '__custom__' && customInput) {
        customInput.style.display = '';
      }
    }
  },

  // Called by openFontPicker() after the iframe closes (whether
  // the user picked a font or not). Refreshes every font-family
  // select in the open props panel so a newly-installed font
  // shows up immediately.
  async _refreshOpenFontFamilySelects() {
    var E = this.el;
    if (!E || !E.props) return;
    if (!this.selectedWidget) return;
    var w = this.selectedWidget;
    // Invalidate the installed-font cache so we re-fetch.
    this._installedFonts = null;
    this._installedFontsAt = 0;
    await this._populateFontFamilySelects(E, w);
  },

  openImagePlaylistPicker() {
    if (this._imagePickerOverlay) return;
    const widget = (this.currentLayout?.widgets || []).find(x => x.id === this.selectedWidgetId) || this.selectedWidget;
    if (!widget) return;
    const overlay = document.createElement('div');
    overlay.style.cssText = 'position:fixed;inset:0;background:rgba(0,0,0,.72);z-index:9999;display:flex;align-items:center;justify-content:center;';
    const box = document.createElement('div');
    box.style.cssText = 'width:min(760px,94vw);height:min(680px,90vh);background:var(--bg,#0d1117);border:1px solid var(--border,#30363d);border-radius:8px;overflow:hidden;display:flex;flex-direction:column;color:var(--text,#e6edf3);';
    box.innerHTML = '<div style="padding:10px 16px;border-bottom:1px solid var(--border,#30363d);display:flex;align-items:center;justify-content:space-between;background:var(--bg-card,#161b22);"><strong>Image Slideshow</strong><button type="button" data-image-picker-close style="padding:4px 10px;">×</button></div><div style="padding:12px 16px;border-bottom:1px solid var(--border,#30363d);"><input type="search" data-image-picker-search placeholder="Search saved images…" style="width:100%;box-sizing:border-box;padding:8px;border-radius:6px;background:var(--bg,#0d1117);color:var(--text,#e6edf3);"></div><div style="display:grid;grid-template-columns:1fr 1fr;gap:16px;min-height:0;flex:1;overflow:hidden;padding:14px 16px;"><section style="min-width:0;min-height:0;overflow:hidden;display:flex;flex-direction:column;"><h3 style="margin:0 0 8px;font-size:14px;">Saved images</h3><div data-image-picker-library style="overflow-y:auto;min-height:0;flex:1;"><small>Loading saved images…</small></div></section><section style="min-width:0;min-height:0;overflow:hidden;display:flex;flex-direction:column;"><h3 style="margin:0 0 8px;font-size:14px;">Slideshow</h3><div data-image-picker-playlist style="overflow-y:auto;min-height:0;flex:1;"></div></section></div>';
    overlay.appendChild(box); document.body.appendChild(overlay); this._imagePickerOverlay = overlay;
    const libraryBox = box.querySelector('[data-image-picker-library]'), playlistBox = box.querySelector('[data-image-picker-playlist]'), search = box.querySelector('[data-image-picker-search]');
    let files = [];
    const live = () => (this.currentLayout?.widgets || []).find(x => x.id === widget.id) || widget;
    const current = () => Array.isArray(live().props?.playlist) ? live().props.playlist : [];
    const commit = images => { const w = live(); w.props.playlist = images; w.props.url = images[0]?.url || ''; this.selectedWidget = w; this.saveCurrent(); this.renderCanvas(); this.renderProps(); render(); };
    const render = () => {
      const images = current(), query = String(search?.value || '').toLowerCase();
      const allDuration = Math.max(1, Number(live().props?.slide_duration) || 10);
      libraryBox.innerHTML = files.filter(f => String(f.name || '').toLowerCase().includes(query)).map(f => { const url = f.url || ('/uploads/images/' + f.name); const added = images.some(x => x.url === url); return `<div style="display:flex;align-items:center;gap:8px;padding:8px 0;border-bottom:1px solid rgba(148,163,184,.12);"><img src="${this.esc(url)}" style="width:72px;height:48px;object-fit:cover;background:#000;border-radius:4px;"><span style="flex:1;min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;" title="${this.esc(f.name || url)}">${this.esc(f.name || url)}</span><button type="button" data-image-add-url="${this.esc(url)}" ${added ? 'disabled' : ''} style="padding:4px 8px;">${added ? 'Added' : 'Add'}</button><button type="button" data-image-delete-name="${this.esc(f.name || '')}" style="padding:4px 8px;">Delete</button></div>`; }).join('') || '<small style="color:#888;">No matching saved images.</small>';
      playlistBox.innerHTML = '<div style="display:flex;align-items:center;gap:8px;padding:6px 0 10px;"><label style="flex:1;">All images default (seconds)<input type="number" data-image-all-duration value="' + allDuration + '" min="1" max="3600" step="1" style="width:100%;padding:5px;margin-top:3px;"></label><button type="button" data-image-apply-all style="padding:5px 8px;align-self:end;">Apply to all</button></div>' + images.map((x, i) => `<div style="display:flex;align-items:center;gap:8px;padding:8px 0;border-bottom:1px solid rgba(148,163,184,.12);"><img src="${this.esc(x.url || '')}" style="width:72px;height:48px;object-fit:cover;background:#000;border-radius:4px;"><span style="flex:1;min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;">${i + 1}. ${this.esc(x.name || x.url)}</span><label style="font-size:11px;white-space:nowrap;">Seconds<input type="number" data-image-duration-index="${i}" value="${Math.max(1, Number(x.duration) || allDuration)}" min="1" max="3600" step="1" style="display:block;width:68px;padding:4px;"></label><button type="button" data-image-remove-index="${i}" style="padding:4px 8px;">Remove</button></div>`).join('') || '<small style="color:#888;">Slideshow is empty.</small>';
    };
    playlistBox.onclick = e => {
      const apply = e.target.closest('[data-image-apply-all]');
      if (apply) { const value = Math.max(1, Math.min(3600, Number(playlistBox.querySelector('[data-image-all-duration]').value) || 10)); const images = current().slice().map(x => ({ ...x, duration: value })); const w = live(); w.props.slide_duration = value; commit(images); return; }
      const b = e.target.closest('[data-image-remove-index]');
      if (!b) return;
      const images = current().slice(); images.splice(Number(b.dataset.imageRemoveIndex), 1); commit(images);
    };
    playlistBox.onchange = e => {
      const input = e.target.closest('[data-image-duration-index]');
      if (!input) return;
      const images = current().slice(), index = Number(input.dataset.imageDurationIndex);
      if (!images[index]) return;
      images[index].duration = Math.max(1, Math.min(3600, Number(input.value) || 10));
      commit(images);
    };
    libraryBox.onclick = async e => {
      const add = e.target.closest('[data-image-add-url]');
      if (add) { const url = add.dataset.imageAddUrl, f = files.find(x => (x.url || ('/uploads/images/' + x.name)) === url); const images = current().slice(); if (!images.some(x => x.url === url)) images.push({ name: f?.name || url, url, duration: Math.max(1, Number(live().props?.slide_duration) || 10) }); commit(images); return; }
      const del = e.target.closest('[data-image-delete-name]');
      if (!del) return;
      const name = del.dataset.imageDeleteName;
      if (!name || !window.confirm('Delete this saved image?')) return;
      try { await API.deleteFile('images', name); files = files.filter(f => f.name !== name); const url = '/uploads/images/' + name; commit(current().slice().filter(x => x.url !== url)); } catch (_) { window.alert('Unable to delete the saved image.'); }
    };
    if (search) search.oninput = render;
    const close = () => { overlay.remove(); this._imagePickerOverlay = null; };
    box.querySelector('[data-image-picker-close]').onclick = close; overlay.onclick = e => { if (e.target === overlay) close(); };
    API.listFiles('images').then(rows => { files = Array.isArray(rows) ? rows : []; render(); }).catch(() => { files = []; render(); });
  },

  openVideoPlaylistPicker() {
    if (this._videoPickerOverlay) return;
    const widget = (this.currentLayout?.widgets || []).find(x => x.id === this.selectedWidgetId) || this.selectedWidget;
    if (!widget) return;
    const overlay = document.createElement('div');
    overlay.style.cssText = 'position:fixed;inset:0;background:rgba(0,0,0,.72);z-index:9999;display:flex;align-items:center;justify-content:center;';
    const box = document.createElement('div');
    box.style.cssText = 'width:min(760px,94vw);height:min(680px,90vh);background:var(--bg,#0d1117);border:1px solid var(--border,#30363d);border-radius:8px;overflow:hidden;display:flex;flex-direction:column;color:var(--text,#e6edf3);';
    box.innerHTML = '<div style="padding:10px 16px;border-bottom:1px solid var(--border,#30363d);display:flex;align-items:center;justify-content:space-between;background:var(--bg-card,#161b22);"><strong>Video Playlist</strong><button type="button" data-video-picker-close style="padding:4px 10px;border-radius:4px;border:1px solid var(--border,#30363d);background:var(--bg-elev,#21262d);color:var(--text,#e6edf3);cursor:pointer;">×</button></div><div style="padding:12px 16px;border-bottom:1px solid var(--border,#30363d);"><input type="search" data-video-picker-search placeholder="Search saved videos…" style="width:100%;box-sizing:border-box;padding:8px;border-radius:6px;border:1px solid var(--border,#30363d);background:var(--bg,#0d1117);color:var(--text,#e6edf3);"></div><div style="display:grid;grid-template-columns:1fr 1fr;gap:16px;min-height:0;flex:1;overflow:hidden;padding:14px 16px;"><section style="min-width:0;min-height:0;overflow:hidden;display:flex;flex-direction:column;"><h3 style="margin:0 0 8px;font-size:14px;">Saved videos</h3><div data-video-picker-library style="overflow-y:auto;min-height:0;flex:1;"><small>Loading saved videos…</small></div></section><section style="min-width:0;min-height:0;overflow:hidden;display:flex;flex-direction:column;"><h3 style="margin:0 0 8px;font-size:14px;">Playlist</h3><div data-video-picker-playlist style="overflow-y:auto;min-height:0;flex:1;"></div></section></div>';
    overlay.appendChild(box); document.body.appendChild(overlay); this._videoPickerOverlay = overlay;
    const libraryBox = box.querySelector('[data-video-picker-library]'), playlistBox = box.querySelector('[data-video-picker-playlist]'), search = box.querySelector('[data-video-picker-search]');
    let files = [];
    const live = () => (this.currentLayout?.widgets || []).find(x => x.id === widget.id) || widget;
    const current = () => Array.isArray(live().props?.playlist) ? live().props.playlist : [];
    const commit = tracks => { const w = live(); w.props.playlist = tracks; w.props.url = tracks[0]?.url || ''; this.selectedWidget = w; this.saveCurrent(); this.renderCanvas(); this.renderProps(); render(); };
    const hydrateVideoThumbs = root => {
      root.querySelectorAll('[data-video-thumb-url]').forEach(holder => {
        if (holder.dataset.thumbReady) return;
        holder.dataset.thumbReady = '1';
        const video = document.createElement('video');
        video.src = holder.dataset.videoThumbUrl;
        video.preload = 'auto';
        video.muted = true;
        video.playsInline = true;
        video.controls = false;
        video.style.cssText = 'width:100%;height:100%;object-fit:cover;display:block;background:#000;';
        video.addEventListener('loadeddata', () => { video.currentTime = 0; }, { once: true });
        video.addEventListener('seeked', () => video.pause(), { once: true });
        video.addEventListener('error', () => { holder.textContent = 'No preview'; holder.style.color = '#888'; holder.style.fontSize = '11px'; }, { once: true });
        holder.replaceChildren(video);
      });
    };
    const render = () => {
      const tracks = current();
      const query = String(search?.value || '').toLowerCase();
      libraryBox.innerHTML = files.filter(f => String(f.name || '').toLowerCase().includes(query)).map(f => {
        const i = files.indexOf(f);
        const url = f.url || ('/uploads/videos/' + f.name);
        const added = tracks.some(t => t.url === url);
        return `<div style="display:flex;align-items:center;gap:10px;padding:8px 0;border-bottom:1px solid rgba(148,163,184,.12);"><div data-video-thumb-url="${this.esc(url)}" style="width:88px;height:50px;flex:0 0 88px;border-radius:4px;overflow:hidden;background:#000;display:grid;place-items:center;color:#666;font-size:18px;">▶</div><span style="flex:1;min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;" title="${this.esc(f.name || url)}">${this.esc(f.name || url)}</span><button type="button" data-video-add-index="${i}" ${added ? 'disabled' : ''} style="padding:4px 8px;">${added ? 'Added' : 'Add'}</button><button type="button" data-video-delete-index="${i}" title="Delete from server" style="padding:4px 8px;color:#dc2626;">🗑</button></div>`;
      }).join('') || '<small style="color:#888;">No matching saved videos.</small>';
      playlistBox.innerHTML = tracks.map((t, i) => `<div style="display:flex;align-items:center;gap:10px;padding:8px 0;border-bottom:1px solid rgba(148,163,184,.12);"><div data-video-thumb-url="${this.esc(t.url || '')}" style="width:88px;height:50px;flex:0 0 88px;border-radius:4px;overflow:hidden;background:#000;display:grid;place-items:center;color:#666;font-size:18px;">▶</div><span style="flex:1;min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;" title="${this.esc(t.name || t.url)}">${i + 1}. ${this.esc(t.name || t.url)}</span><button type="button" data-video-remove-index="${i}" style="padding:4px 8px;">Remove</button></div>`).join('') || '<small style="color:#888;">Playlist is empty.</small>';
      hydrateVideoThumbs(box);
    };
    libraryBox.onclick = e => { const button = e.target.closest('[data-video-delete-index]'); if (button) { const f = files[Number(button.dataset.videoDeleteIndex)]; if (!f) return; const name = f.name || ''; const url = f.url || ('/uploads/videos/' + name); const otherWidgets = (this.currentLayout?.widgets || []).filter(w => w.id !== widget.id && w.props?.playlist?.some(t => t.url === url)); const msg = otherWidgets.length ? 'WARNING: This video is also used in ' + otherWidgets.length + ' other widget(s) in this layout. Those will show a blank slot.' : ''; if (!confirm('Delete "' + name + '" from the server? This cannot be undone.' + (msg ? '\n\n' + msg : ''))) return; API.deleteFile('videos', name).then(() => { files = files.filter(x => x.name !== name); if (current().some(t => t.url === url)) { const w = live(); w.props.playlist = w.props.playlist.filter(t => t.url !== url); if (!w.props.playlist.length) w.props.url = ''; this.selectedWidget = w; this.saveCurrent(); } render(); }).catch(err => { alert('Delete failed: ' + (err?.message || err)); }); } else { const button = e.target.closest('[data-video-add-index]'); if (!button) return; const f = files[Number(button.dataset.videoAddIndex)]; if (!f) return; const url = f.url || ('/uploads/videos/' + f.name); const tracks = current().slice(); if (!tracks.some(t => t.url === url)) tracks.push({ name: f.name || url, url }); commit(tracks); } };
    playlistBox.onclick = e => { const button = e.target.closest('[data-video-remove-index]'); if (!button) return; const tracks = current().slice(); tracks.splice(Number(button.dataset.videoRemoveIndex), 1); commit(tracks); };
    if (search) search.oninput = render;
    const close = () => { overlay.remove(); this._videoPickerOverlay = null; };
    box.querySelector('[data-video-picker-close]').onclick = close; overlay.onclick = e => { if (e.target === overlay) close(); };
    API.listFiles('videos').then(rows => { files = Array.isArray(rows) ? rows : []; render(); }).catch(() => { files = []; render(); });
  },

  openFontPicker() {
    // Don't double-open.
    if (this._fontPickerIframe) return;
    const overlay = document.createElement('div');
    overlay.style.cssText = 'position:fixed;inset:0;background:rgba(0,0,0,0.7);z-index:9999;display:flex;align-items:center;justify-content:center;';
    const box = document.createElement('div');
    box.style.cssText = 'width:min(900px,95vw);height:min(700px,90vh);background:var(--bg,#0d1117);border:1px solid var(--border,#30363d);border-radius:8px;overflow:hidden;display:flex;flex-direction:column;';
    const header = document.createElement('div');
    header.style.cssText = 'padding:10px 16px;border-bottom:1px solid var(--border,#30363d);display:flex;align-items:center;justify-content:space-between;background:var(--bg-card,#161b22);';
    header.innerHTML = '<span style="font-weight:600;color:var(--text,#e6edf3);">Font Picker</span><button type="button" id="font-picker-close" style="padding:4px 10px;border-radius:4px;border:1px solid var(--border,#30363d);background:var(--bg-elev,#21262d);color:var(--text,#e6edf3);cursor:pointer;">×</button>';
    const props = (this.selectedWidget && this.selectedWidget.props) || {};
    const current = props.font_family || props.fontFamily || 'sans-serif';
    const iframe = document.createElement('iframe');
    iframe.src = '/font-picker.html?current=' + encodeURIComponent(current);
    iframe.style.cssText = 'flex:1;border:0;background:transparent;';
    box.appendChild(header);
    box.appendChild(iframe);
    overlay.appendChild(box);
    document.body.appendChild(overlay);
    this._fontPickerIframe = iframe;
    this._fontPickerOverlay = overlay;
    const self = this;
    const E = this.el;
    const close = () => {
      window.removeEventListener('message', self._fontPickerMessageHandler);
      if (self._fontPickerOverlay && self._fontPickerOverlay.parentNode) {
        self._fontPickerOverlay.parentNode.removeChild(self._fontPickerOverlay);
      }
      self._fontPickerIframe = null;
      self._fontPickerOverlay = null;
      self._fontPickerMessageHandler = null;
    };
    self._fontPickerMessageHandler = (ev) => {
      const data = ev.data || {};
      if (data && data.type === 'font-picked' && data.cssValue) {
        if (self.selectedWidget) {
          if (!self.selectedWidget.props) self.selectedWidget.props = {};
          // Clear the legacy fontFamily so future saves don't keep it.
          delete self.selectedWidget.props.fontFamily;
          self.selectedWidget.props.font_family = data.cssValue;
          // Update the font-family <select> in the props panel so
          // the user sees the picked value as the selected option.
          // If the cssValue isn't in any optgroup (e.g. a custom
          // string the user typed in the picker), we add it as a
          // temporary option and select it; the user can still see
          // and edit it via the Custom... fallback.
          const select = E && E.props && E.props.querySelector && E.props.querySelector('select[data-prop="font_family"]');
          if (select) {
            // Try to find an existing option matching the picked value.
            var found = false;
            for (var oi = 0; oi < select.options.length; oi++) {
              if (select.options[oi].value === data.cssValue) {
                select.selectedIndex = oi;
                found = true;
                break;
              }
            }
            if (!found) {
              // Insert a temporary option and select it.
              var opt = document.createElement('option');
              opt.value = data.cssValue;
              opt.textContent = data.cssValue;
              // Put it before the __custom__ option so it shows up.
              var customOpt = Array.from(select.options).find(function (o) { return o.value === '__custom__'; });
              if (customOpt) select.insertBefore(opt, customOpt);
              else select.appendChild(opt);
              opt.selected = true;
              // Hide the custom input — the picker-supplied value
              // is shown in the select itself.
              const customInput = E && E.props && E.props.querySelector('input[data-prop="font_family_custom"]');
              if (customInput) customInput.style.display = 'none';
            }
          }
          self.saveCurrent();
          self.renderCanvas();
        }
        close();
        onPickerClose();
      }
    };
    window.addEventListener('message', self._fontPickerMessageHandler);
    // Close button + overlay click + refresh select on close so any
    // newly-installed font is reflected in the open dropdown.
    const onPickerClose = () => {
      // Refresh the open select with the latest installed list.
      // We don't await — the UI updates as soon as the fetch
      // resolves. If the picker was used to install a new font,
      // the user's pick is preserved by the message handler above.
      self._refreshOpenFontFamilySelects();
    };
    const closeBtn = header.querySelector('#font-picker-close');
    if (closeBtn) closeBtn.addEventListener('click', () => { close(); onPickerClose(); });
    overlay.addEventListener('click', (e) => {
      if (e.target === overlay) {
        close();
        onPickerClose();
      }
    });
  },
};
