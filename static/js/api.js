const _origFetch = window.fetch.bind(window);
let _stepUpInFlight = {};

function _stepUpScopeForRequest(input, init) {
    const method = String((init && init.method) || (input && input.method) || 'GET').toUpperCase();
    const url = typeof input === 'string' ? input : (input && input.url) || '';
    const path = new URL(url, window.location.href).pathname;
    if (path === '/api/version/update/export' || path === '/api/version/update/import' || path === '/api/version/update/apply' || path === '/api/version/update/github/settings') return 'server-update';
    if (method !== 'GET' && method !== 'HEAD' && method !== 'OPTIONS') {

    }
    return null;
}

function _stepUpDialog(scope) {
    if (_stepUpInFlight[scope]) return _stepUpInFlight[scope];
    _stepUpInFlight[scope] = (typeof window.requestClientControlReauth === 'function')
        ? window.requestClientControlReauth(scope)
        : Promise.reject(new Error('Re-login is not available yet; reload the dashboard.'));
    _stepUpInFlight[scope].finally(() => { delete _stepUpInFlight[scope]; });
    return _stepUpInFlight[scope];
}

window.fetch = async function(input, init) {
    init = init || {};
    let headers = new Headers(init.headers || {});
    try {
        const tok = localStorage.getItem('signage.adminToken');
        if (tok && !headers.has('Authorization')) headers.set('Authorization', 'Bearer ' + tok);
    } catch(e) {}
    const requestInit = Object.assign({}, init, { headers, credentials: init.credentials || 'same-origin' });
    let response = await _origFetch(input, requestInit);
    const scope = _stepUpScopeForRequest(input, init);
    if (response.status === 403 && scope) {
        const detail = await response.clone().text().catch(() => '');
        if (/step-up authentication required/i.test(detail)) {
            await _stepUpDialog(scope);
            const retryHeaders = new Headers(requestInit.headers);
            retryHeaders.delete('Authorization');
            response = await _origFetch(input, Object.assign({}, requestInit, { headers: retryHeaders }));
        }
    }
    return response;
};

// API client for the digital signage backend

function normalizeLayoutForApi(layout) {
    const mapType = (t) => {
        if (t === 'date') return 'datetime';
        if (t === 'embed') return 'webpage';
        return t;
    };
    // `layouts` columns: id, name, interactive, page_type, timing_mode, duration, shuffle, items, created_at, updated_at
    const allowed = new Set(['id','name','interactive','page_type','timing_mode','duration','shuffle','canvas_orientation','folder_id','items','widgets']);
    const clean = {};
    for (const k of Object.keys(layout || {})) {
        if (allowed.has(k)) clean[k] = layout[k];
    }
    if (!Array.isArray(clean.widgets)) clean.widgets = [];
    clean.widgets = clean.widgets.map(w => {
        const out = Object.assign({}, w);
        out.widget_type = mapType(w.widget_type || w.type || 'text');
        delete out.type; // Rust accepts widget_type; keeping both trips serde duplicate-field errors.
        if (!out.props) out.props = {};
        return out;
    });
    if (clean.page_type == null || clean.page_type === '') clean.page_type = 'single';
    if (clean.timing_mode == null || clean.timing_mode === '') clean.timing_mode = 'global';
    if (!Number.isFinite(Number(clean.duration))) clean.duration = 15;
    return clean;
}

function normalizeLayoutFromApi(layout) {
    if (!layout) return layout;
    const mapType = (t) => {
        if (t === 'datetime') return 'date';
        if (t === 'webpage') return 'embed';
        return t;
    };
    return Object.assign({}, layout, {
        widgets: (layout.widgets || []).map(w => {
            const out = Object.assign({}, w);
            out.type = mapType(w.type || w.widget_type || 'text');
            return out;
        })
    });
}

window.API = {
    async getLayouts() {
        const res = await fetch('/api/layouts');
        if (!res.ok) throw new Error('Failed to fetch layouts');
        const layouts = await res.json();
        return Array.isArray(layouts) ? layouts.map(normalizeLayoutFromApi) : layouts;
    },

    async getLayout(id) {
        const res = await fetch(`/api/layouts/${id}`);
        if (!res.ok) throw new Error('Failed to fetch layout');
        return normalizeLayoutFromApi(await res.json());
    },

    async getLayoutFolders() {
        const res = await fetch('/api/layout-folders');
        if (!res.ok) throw new Error('Failed to fetch layout folders');
        return res.json();
    },

    async saveLayoutFolder(folder) {
        const creating = !folder.id;
        const url = creating ? '/api/layout-folders' : `/api/layout-folders/${encodeURIComponent(folder.id)}`;
        const res = await fetch(url, {
            method: creating ? 'POST' : 'PUT',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify(folder),
        });
        if (!res.ok) {
            const detail = await res.text().catch(() => '');
            throw new Error(`Failed to save folder (${res.status})${detail ? ': ' + detail : ''}`);
        }
        return res.json();
    },

    async deleteLayoutFolder(id) {
        const res = await fetch(`/api/layout-folders/${encodeURIComponent(id)}`, { method: 'DELETE' });
        if (!res.ok) {
            const detail = await res.text().catch(() => '');
            throw new Error(detail || 'Failed to delete folder');
        }
    },

    async saveLayout(layout) {
        const url = layout.id ? `/api/layouts/${layout.id}` : '/api/layouts';
        const method = layout.id ? 'PUT' : 'POST';
        const res = await fetch(url, {
            method,
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify(normalizeLayoutForApi(layout)),
        });
        if (!res.ok) {
            const detail = await res.text().catch(() => '');
            throw new Error(`Failed to save layout (${res.status})${detail ? ': ' + detail : ''}`);
        }
        return res.json();
    },

    async deleteLayout(id) {
        const res = await fetch(`/api/layouts/${id}`, { method: 'DELETE' });
        if (!res.ok) throw new Error('Failed to delete layout');
    },

    async listLayoutRevisions(id) {
        const res = await fetch(`/api/layouts/${encodeURIComponent(id)}/revisions`);
        const body = await res.json().catch(() => []);
        if (!res.ok) throw new Error(body.error || body.message || `Failed to load layout revisions (HTTP ${res.status})`);
        return body;
    },

    async publishLayout(id) {
        const res = await fetch(`/api/layouts/${encodeURIComponent(id)}/publish`, { method: 'POST' });
        const body = await res.json().catch(() => ({}));
        if (!res.ok) throw new Error(body.error || body.message || `Failed to publish layout (HTTP ${res.status})`);
        return body;
    },

    async getLayoutPropagationStatus(id) {
        const res = await fetch(`/api/layouts/${encodeURIComponent(id)}/propagation`);
        const body = await res.json().catch(() => ({}));
        if (!res.ok) throw new Error(body.error || body.message || `Failed to load propagation status (HTTP ${res.status})`);
        return body;
    },

    async getLayoutSets() {
        const res = await fetch('/api/layout-sets');
        if (!res.ok) throw new Error('Failed to fetch layout sets');
        return res.json();
    },

    async getLayoutSet(id) {
        const res = await fetch(`/api/layout-sets/${encodeURIComponent(id)}`);
        if (!res.ok) throw new Error('Failed to fetch layout set');
        return res.json();
    },

    async saveLayoutSet(set) {
        const creating = !set.id;
        if (creating) set.id = (crypto && crypto.randomUUID) ? crypto.randomUUID() : ('layout-set-' + Date.now());
        const url = creating ? '/api/layout-sets' : `/api/layout-sets/${encodeURIComponent(set.id)}`;
        const method = creating ? 'POST' : 'PUT';
        const res = await fetch(url, {
            method,
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify(set),
        });
        if (!res.ok) throw new Error('Failed to save layout set');
        return res.json();
    },

    async replaceLayoutSetEntries(id, entries) {
        const res = await fetch(`/api/layout-sets/${encodeURIComponent(id)}/entries`, {
            method: 'PUT',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify((entries || []).map((entry, position) => ({
                layout_id: entry.layout_id,
                position: Number.isFinite(Number(entry.position)) ? Number(entry.position) : position,
                interval_secs: entry.interval_secs == null ? null : Math.max(1, Number(entry.interval_secs) || 1),
            }))),
        });
        if (!res.ok) throw new Error(`Failed to save layout rotation entries (HTTP ${res.status})`);
        return res.json();
    },

    async deleteLayoutSet(id) {
        const res = await fetch(`/api/layout-sets/${encodeURIComponent(id)}`, { method: 'DELETE' });
        if (!res.ok) throw new Error('Failed to delete layout set');
    },

    async getDisplays() {
        const res = await fetch('/api/displays');
        if (!res.ok) throw new Error('Failed to fetch displays');
        return res.json();
    },

    async getDisplayState() {
        const res = await fetch('/api/display-state');
        if (!res.ok) throw new Error('Failed to fetch display state');
        return res.json();
    },

    async getDisplayStateForDisplay(display) {
        const res = await fetch(`/api/display-state/${display}`);
        if (!res.ok) throw new Error('Failed to fetch display state');
        return res.json();
    },

    async setDisplayState(state) {
        const res = await fetch('/api/display-state', {
            method: 'PUT',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify(state),
        });
        if (!res.ok) throw new Error('Failed to set display state');
        return res.json();
    },

    async getSystemStatus() {
        const res = await fetch('/api/system');
        if (!res.ok) throw new Error('Failed to fetch system status');
        return res.json();
    },

    async listAddons() {
        const res = await fetch('/api/addons');
        if (!res.ok) throw new Error('Failed to fetch addons');
        return res.json();
    },

    async listAddonWidgetTypes() {
        const res = await fetch('/api/addons/widget-types');
        if (!res.ok) throw new Error('Failed to fetch addon widget types');
        return res.json();
    },

    async installAddon(manifest) {
        const res = await fetch('/api/addons', {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify(manifest),
        });
        if (!res.ok) throw new Error(await res.text() || 'Failed to install addon');
        return res.json();
    },

    async setAddonEnabled(id, enabled) {
        const action = enabled ? 'enable' : 'disable';
        const res = await fetch(`/api/addons/${encodeURIComponent(id)}/${action}`, { method: 'POST' });
        if (!res.ok) throw new Error(await res.text() || `Failed to ${action} addon`);
        return res.json();
    },

    async deleteAddon(id) {
        const res = await fetch(`/api/addons/${encodeURIComponent(id)}`, { method: 'DELETE' });
        if (!res.ok) throw new Error(await res.text() || 'Failed to remove addon');
    },

    async getCertificateStatus() {
        const res = await fetch('/api/system/certificates');
        if (!res.ok) throw new Error('Failed to fetch certificate status');
        return res.json();
    },

    async uploadClientDiagnostics(payload) {
        const res = await fetch('/api/clients/diagnostics', {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify(payload || {}),
        });
        if (!res.ok) throw new Error('Failed to save client diagnostics (HTTP ' + res.status + ')');
        return res.json();
    },

    async listClientDiagnostics() {
        const res = await fetch('/api/clients/diagnostics');
        if (!res.ok) throw new Error('Failed to list client diagnostics (HTTP ' + res.status + ')');
        return res.json();
    },


    // ── Player/client display assignment ──
    async getClients() {
        const res = await fetch('/api/clients');
        if (!res.ok) throw new Error('Failed to fetch clients');
        return res.json();
    },

    async getScenes() {
        const res = await fetch('/api/scenes');
        if (!res.ok) throw new Error('Failed to fetch scenes');
        return res.json();
    },

    async createScene(scene) {
        const res = await fetch('/api/scenes', {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify(scene),
        });
        if (!res.ok) throw new Error('Failed to create scene');
        return res.json();
    },

    async updateScene(id, scene) {
        const res = await fetch(`/api/scenes/${encodeURIComponent(id)}`, {
            method: 'PUT',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify(scene),
        });
        if (!res.ok) throw new Error('Failed to update scene');
        return res.json();
    },

    async deleteScene(id) {
        const res = await fetch(`/api/scenes/${encodeURIComponent(id)}`, { method: 'DELETE' });
        if (!res.ok) throw new Error('Failed to delete scene');
        return res.json();
    },

    async publishSceneToClient(sceneId, clientId) {
        const res = await fetch(`/api/scenes/${encodeURIComponent(sceneId)}/publish/${encodeURIComponent(clientId)}`, { method: 'POST' });
        if (!res.ok) throw new Error('Failed to publish scene');
        return res.json();
    },

    async getClientDisplays(clientId) {
        const res = await fetch(`/api/clients/${encodeURIComponent(clientId)}/displays`);
        if (!res.ok) throw new Error('Failed to fetch client displays');
        return res.json();
    },

    // Adopt a pending physical client (administrator action)
    async adoptClient(clientId) {
        const res = await fetch(`/api/clients/${encodeURIComponent(clientId)}/adopt`, { method: 'POST' });
        const body = await res.json().catch(() => ({}));
        if (!res.ok) throw new Error(body.error || `HTTP ${res.status}`);
        return body;
    },

    // Create a virtual client
    async createVirtualClient(name) {
        const res = await fetch('/api/clients/virtual', {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ name }),
        });
        if (!res.ok) throw new Error('Failed to create virtual client');
        return res.json();
    },

    async assignClientDisplayLayout(clientId, displayIndex, layoutId) {
        const res = await fetch(`/api/clients/${encodeURIComponent(clientId)}/displays/${displayIndex}/layout`, {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ layout_id: layoutId || null }),
        });
        if (!res.ok) throw new Error('Failed to assign display layout');
        return res.json();
    },

    async updateDisplayOrientation(clientId, displayIndex, orientation) {
        const res = await fetch(`/api/clients/${encodeURIComponent(clientId)}/displays/${displayIndex}/orientation`, {
            method: 'PATCH',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ orientation }),
        });
        if (!res.ok) {
            const text = await res.text();
            throw new Error(`Failed to update display orientation (HTTP ${res.status}): ${text}`);
        }
        return res.json();
    },

    async sendDisplayCommand(clientId, displayIndex, action, payload = {}) {
        const res = await fetch(`/api/clients/${encodeURIComponent(clientId)}/displays/${displayIndex}/command`, {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ action, payload }),
        });
        if (!res.ok) throw new Error('Failed to send display command: ' + res.status);
        return res.json();
    },

    async getDisplayControl(clientId, displayIndex) {
        const res = await fetch(`/api/clients/${encodeURIComponent(clientId)}/displays/${displayIndex}/control`);
        if (!res.ok) throw new Error('Failed to get display control info: ' + res.status);
        return res.json();
    },

    async setDisplayControlMethod(clientId, displayIndex, method) {
        const res = await fetch(`/api/clients/${encodeURIComponent(clientId)}/displays/${displayIndex}/control`, {
            method: 'PATCH',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ control_method: method }),
        });
        if (!res.ok) throw new Error('Failed to set control method: ' + res.status);
        return res.json();
    },

    async setVirtualClientLayout(clientId, layoutId) {
        const res = await fetch('/api/clients/' + encodeURIComponent(clientId) + '/virtual', {
            method: 'PUT',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ layout_id: layoutId || null }),
        });
        if (!res.ok) throw new Error('Failed to set virtual client layout');
        return res.json();
    },

    // ── File management ──
    async listFiles(category) {
        const res = await fetch(`/api/files/${category}`);
        if (!res.ok) throw new Error('Failed to list files');
        return res.json();
    },

    async uploadFile(category, file) {
        const formData = new FormData();
        formData.append('file', file);
        const headers = {};
        try {
            const token = localStorage.getItem('signage.adminToken');
            if (token) headers.Authorization = token.startsWith('Bearer ') ? token : 'Bearer ' + token;
        } catch (_) {}
        const res = await fetch(`/api/files/${category}`, {
            method: 'POST',
            headers,
            body: formData,
        });
        if (!res.ok) {
            let detail = '';
            try { detail = (await res.text()).trim(); } catch (_) {}
            throw new Error(detail || `Upload failed (HTTP ${res.status})`);
        }
        return res.json();
    },

    async deleteFile(category, name) {
        const headers = { 'Content-Type': 'application/json' };
        try {
            const token = localStorage.getItem('signage.adminToken');
            if (token) headers.Authorization = token.startsWith('Bearer ') ? token : 'Bearer ' + token;
        } catch (_) {}
        const res = await fetch(`/api/files/${encodeURIComponent(category)}/${encodeURIComponent(name)}`, {
            method: 'DELETE',
            headers,
        });
        if (!res.ok && res.status !== 204) {
            let detail = '';
            try { detail = (await res.text()).trim(); } catch (_) {}
            throw new Error(detail || `Delete failed (HTTP ${res.status})`);
        }
    },

    async listTrash() {
        const headers = {};
        try {
            const token = localStorage.getItem('signage.adminToken');
            if (token) headers.Authorization = token.startsWith('Bearer ') ? token : 'Bearer ' + token;
        } catch (_) {}
        const res = await fetch('/api/files/trash', { headers });
        if (!res.ok) {
            let detail = '';
            try { detail = (await res.text()).trim(); } catch (_) {}
            throw new Error(detail || `Failed to load trash (HTTP ${res.status})`);
        }
        return await res.json();
    },

    async restoreTrash(id) {
        const headers = { 'Content-Type': 'application/json' };
        try {
            const token = localStorage.getItem('signage.adminToken');
            if (token) headers.Authorization = token.startsWith('Bearer ') ? token : 'Bearer ' + token;
        } catch (_) {}
        const res = await fetch(`/api/files/trash/${encodeURIComponent(id)}/restore`, {
            method: 'POST',
            headers,
            body: '{}',
        });
        if (!res.ok) {
            let detail = '';
            try { detail = (await res.text()).trim(); } catch (_) {}
            throw new Error(detail || `Restore failed (HTTP ${res.status})`);
        }
        return await res.json();
    },

    // Templates
    async listTemplates() {
        const res = await fetch('/api/templates');
        if (!res.ok) throw new Error('Failed to list templates');
        return res.json();
    },

    async getTemplate(id) {
        const res = await fetch(`/api/templates/${id}`);
        if (!res.ok) throw new Error('Failed to get template');
        return res.json();
    },

    async saveTemplate(template) {
        const res = await fetch('/api/templates', {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify(template),
        });
        if (!res.ok) throw new Error('Failed to save template');
        return res.json();
    },

    async deleteTemplate(id) {
        const res = await fetch(`/api/templates/${id}`, { method: 'DELETE' });
        if (!res.ok) throw new Error('Failed to delete template');
    },

    // ── Backups ──
    async listBackups() {
        const res = await fetch('/api/backups');
        if (!res.ok) throw new Error('Failed to fetch backups');
        return res.json();
    },

    async createBackup() {
        const res = await fetch('/api/backups', { method: 'POST' });
        if (!res.ok) throw new Error('Failed to create backup');
        return res.json();
    },

    async restoreBackup(id) {
        const res =await fetch(`/api/backups/${id}`, { method: 'PUT' });
        if (!res.ok) throw new Error('Failed to restore backup');
        return res.json();
    },

    async deleteBackup(id) {
        const res = await fetch(`/api/backups/${id}`, { method: 'DELETE' });
        if (!res.ok) throw new Error('Failed to delete backup');
    },

    // ── Schedules ──
    async listSchedules() {
        const res = await fetch('/api/schedules');
        if (!res.ok) throw new Error('Failed to fetch schedules');
        return res.json();
    },

    async createSchedule(schedule) {
        const res = await fetch('/api/schedules', {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify(schedule),
        });
        if (!res.ok) {
            const detail = await res.text();
            throw new Error(detail || `Failed to create schedule (HTTP ${res.status})`);
        }
        return res.json();
    },

    async updateSchedule(id, schedule) {
        const res = await fetch(`/api/schedules/${id}`, {
            method: 'PUT',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify(schedule),
        });
        if (!res.ok) throw new Error('Failed to update schedule');
        return res.json();
    },

    async deleteSchedule(id) {
        const res = await fetch(`/api/schedules/${id}`, { method: 'DELETE' });
        if (!res.ok) throw new Error('Failed to delete schedule');
    },

    // ── Widget Presets ──
    async listWidgetPresets(widgetType) {
        const url = widgetType ? `/api/widget-presets?widget_type=${encodeURIComponent(widgetType)}` : '/api/widget-presets';
        const res = await fetch(url);
        if (!res.ok) throw new Error('Failed to list widget presets');
        return res.json();
    },
    async getWidgetPresetFull(id) {
        // Returns the full preset including props_json, is_builtin, and assets.
        const res = await fetch(`/api/widget-presets/${id}`);
        if (!res.ok) throw new Error('Failed to get widget preset');
        return res.json();
    },
    async getWidgetPreset(id) {
        const res = await fetch(`/api/widget-presets/${id}`);
        if (!res.ok) throw new Error('Failed to get widget preset');
        return res.json();
    },
    async createWidgetPreset(data) {
        const res = await fetch('/api/widget-presets', {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify(data),
        });
        if (!res.ok) throw new Error('Failed to create widget preset');
        return res.json();
    },
    async updateWidgetPreset(id, data) {
        const res = await fetch(`/api/widget-presets/${id}`, {
            method: 'PUT',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify(data),
        });
        if (!res.ok) throw new Error('Failed to update widget preset');
    },
    async deleteWidgetPreset(id) {
        const res = await fetch(`/api/widget-presets/${id}`, { method: 'DELETE' });
        if (!res.ok) throw new Error('Failed to delete widget preset');
    },
    async addWidgetPresetAsset(presetId, kind, key, url) {
        const res = await fetch(`/api/widget-presets/${presetId}/assets`, {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ kind, key, url }),
        });
        if (!res.ok) throw new Error('Failed to add asset');
        return res.json();
    },
    async removeWidgetPresetAsset(presetId, assetId) {
        const res = await fetch(`/api/widget-presets/${presetId}/assets/${assetId}`, { method: 'DELETE' });
        if (!res.ok) throw new Error('Failed to remove asset');
    },
    exportWidgetPresetUrl(id) {
        return `/api/widget-presets/${id}/export`;
    },
    async resetDefaultWidgetPresets() {
        const res = await fetch('/api/widget-presets/reset-defaults', { method: 'POST' });
        if (!res.ok) throw new Error('Reset failed (status ' + res.status + ')');
        return res.json();
    },
    async importWidgetPreset(json) {
        const data = typeof json === 'string' ? JSON.parse(json) : json;
        const res = await fetch('/api/widget-presets/import', {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify(data),
        });
        if (!res.ok) throw new Error('Failed to import widget preset');
        return res.json();
    },

    // ── Account ──
    async getAccount() {
        const res = await fetch('/api/account');
        if (!res.ok) throw new Error(await res.text().catch(() => 'Failed to fetch account'));
        return res.json();
    },
    async changeAccountPassword(current_password, new_password) {
        const res = await fetch('/api/account/password', {
            method: 'POST', headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ current_password, new_password }),
        });
        if (!res.ok) throw new Error(await res.text().catch(() => 'Password update failed'));
        return res.json();
    },
    async getGoogleLoginUrl() {
        const res = await fetch('/api/auth/google/login-url');
        if (!res.ok) throw new Error(await res.text().catch(() => 'Google login unavailable'));
        return res.json();
    },
    async getGoogleBindUrl() {
        const res = await fetch('/api/account/google/bind-url');
        if (!res.ok) throw new Error(await res.text().catch(() => 'Google bind unavailable'));
        return res.json();
    },
    async unbindGoogle() {
        const res = await fetch('/api/account/google/unbind', { method: 'POST' });
        if (!res.ok) throw new Error(await res.text().catch(() => 'Google unbind failed'));
        return res.json();
    },

    // ── User Manager ──
    async listUsers() {
        const res = await fetch('/api/users');
        if (!res.ok) throw new Error('Failed to fetch users');
        return res.json();
    },
    async createUser(user) {
        const res = await fetch('/api/users', {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify(user),
        });
        if (!res.ok) throw new Error((await res.text().catch(() => '')) || 'Failed to create user');
        return res.json();
    },
    async updateUser(id, patch) {
        const res = await fetch(`/api/users/${encodeURIComponent(id)}`, {
            method: 'PUT',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify(patch),
        });
        if (!res.ok) throw new Error((await res.text().catch(() => '')) || 'Failed to update user');
        return res.json();
    },
    async deleteUser(id) {
        const res = await fetch(`/api/users/${encodeURIComponent(id)}`, { method: 'DELETE' });
        if (!res.ok) throw new Error((await res.text().catch(() => '')) || 'Failed to delete user');
        return res.json();
    },

    // ── Version Manager ──
    async getVersion() {
        const res = await fetch('/api/version');
        if (!res.ok) throw new Error('Failed to get version info');
        return res.json();
    },
    exportBundleUrl() {
        return '/api/version/export';
    },
    async importBundle(file) {
        const form = new FormData();
        form.append('bundle', file);
        const res = await fetch('/api/version/import', { method: 'POST', body: form });
        if (!res.ok) throw new Error('Import failed (status ' + res.status + ')');
    },
    async getUpdatePreview() {
        const res = await fetch('/api/version/update/preview');
        if (!res.ok) throw new Error('Failed to get update preview');
        return res.json();
    },
    exportUpdateBundleUrl() {
        return '/api/version/update/export';
    },
    async importUpdateBundle(file, forceSkipRollback) {
        const form = new FormData();
        form.append('bundle', file);
        let url = '/api/version/update/import';
        if (forceSkipRollback) url += '?force=true';
        const res = await fetch(url, { method: 'POST', body: form });
        if (!res.ok) {
            const text = await res.text().catch(() => '');
            throw new Error('Update import failed (status ' + res.status + ')' + (text ? ': ' + text : ''));
        }
        return res.json();
    },
    async getGithubUpdateStatus() {
        const res = await fetch('/api/version/update/github');
        if (!res.ok) throw new Error('Failed to get GitHub update status');
        return res.json();
    },
    async checkGithubUpdate() {
        const res = await fetch('/api/version/update/github/check', { method: 'POST' });
        if (!res.ok) {
            const text = await res.text().catch(() => '');
            throw new Error('Update check failed (status ' + res.status + ')' + (text ? ': ' + text : ''));
        }
        return res.json();
    },
    async setGithubAutoUpdate(autoUpdate) {
        const res = await fetch('/api/version/update/github/settings', {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ auto_update: !!autoUpdate }),
        });
        if (!res.ok) {
            const text = await res.text().catch(() => '');
            throw new Error('Auto-update setting failed (status ' + res.status + ')' + (text ? ': ' + text : ''));
        }
        return res.json();
    },
    async applyGithubUpdate() {
        const res = await fetch('/api/version/update/apply', { method: 'POST' });
        if (!res.ok) {
            const text = await res.text().catch(() => '');
            throw new Error('GitHub update failed (status ' + res.status + ')' + (text ? ': ' + text : ''));
        }
        return res.json();
    },
    async getVersionUpdateLastFailure() {
        const res = await fetch('/api/version/update/last-failure');
        if (!res.ok) {
            if (res.status === 401 || res.status === 403) return { failed: false };
            throw new Error('Failed to read last-failure marker');
        }
        return res.json();
    },
    async buildPlayerClientBundle() {
        const res = await fetch('/api/version/player-client-bundle/build', { method: 'POST' });
        if (!res.ok) {
            const text = await res.text().catch(() => '');
            throw new Error('Player bundle build failed (status ' + res.status + ')' + (text ? ': ' + text : ''));
        }
        return res.json();
    },
    playerClientBundleUrl() {
        return '/install/player-client-bundle.tar.gz';
    },
    playerClientBootstrapUrl() {
        return '/install/player-client-bundle-install.sh';
    },
    playerClientBundleDownloadUrl() {
        return '/install/player-client-bundle-download.tar.gz';
    },
    playerClientBootstrapDownloadUrl() {
        return '/install/player-client-bundle-bootstrap.sh';
    },
    async buildServerBundle() {
        const res = await fetch('/api/version/server-bundle/build', { method: 'POST' });
        if (!res.ok) {
            const text = await res.text().catch(() => '');
            throw new Error('Server bundle build failed (status ' + res.status + ')' + (text ? ': ' + text : ''));
        }
        return res.json();
    },
    serverBundleUrl() {
        return '/install/ripley-signage-server-bundle.tar.gz';
    },
    serverBootstrapUrl() {
        return '/install/ripley-signage-server-bundle-install.sh';
    },
    async restartService() {
        const res = await fetch('/api/version/restart', { method: 'POST' });
        if (!res.ok) throw new Error('Restart failed (status ' + res.status + ')');
    },


    // ── Interactive Submissions (data collection) ──
    async getInteractiveSubmissions(params = {}) {
        const q = new URLSearchParams();
        if (params.limit) q.set('limit', params.limit);
        if (params.client_id) q.set('client_id', params.client_id);
        if (params.page_id) q.set('page_id', params.page_id);
        if (params.status) q.set('status', params.status);
        const url = '/api/interactive/submissions' + (q.toString() ? '?' + q.toString() : '');
        const res = await fetch(url);
        if (!res.ok) throw new Error('Failed to fetch interactive submissions');
        return res.json();
    },
    async getInteractiveSubmissionsStats() {
        const res = await fetch('/api/interactive/submissions/stats');
        if (!res.ok) throw new Error('Failed to fetch interactive submissions stats');
        return res.json();
    },
    async getInteractiveSubmissionsCsvUrl(params = {}) {
        const q = new URLSearchParams();
        if (params.limit) q.set('limit', params.limit);
        if (params.client_id) q.set('client_id', params.client_id);
        if (params.page_id) q.set('page_id', params.page_id);
        if (params.status) q.set('status', params.status);
        return '/api/interactive/submissions/export.csv' + (q.toString() ? '?' + q.toString() : '');
    },
    async syncInteractiveSubmissionsToGoogle(payload = {}) {
        const res = await fetch('/api/interactive/submissions/sync-to-google', {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify(payload),
        });
        if (!res.ok) throw new Error('Failed to sync interactive submissions');
        return res.json();
    },
    async deleteInteractiveSubmissions(ids) {
        const res = await fetch('/api/interactive/submissions', {
            method: 'DELETE',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ ids }),
        });
        if (!res.ok) throw new Error('Failed to delete interactive submissions');
        return res.json();
    },
    // Player-side submission. Sends client_id via headers so the server can
    // attribute the submission to the right interactive device even if the body
    // doesn't carry it.
    async postInteractiveSubmission(payload, clientId) {
        const headers = { 'Content-Type': 'application/json' };
        if (clientId) {
            headers['X-Ripley-Client-Id'] = clientId;
        }
        const res = await fetch('/api/interactive/submit', {
            method: 'POST',
            headers,
            body: JSON.stringify(payload),
        });
        if (!res.ok) {
            const err = await res.text();
            throw new Error(`Submit failed (${res.status}): ${err}`);
        }
        return res.json();
    },

    // ── System Time ──
    async getSystemTime() {
        const res = await fetch('/api/system/time');
        if (!res.ok) throw new Error('Failed to get system time');
        return res.json();
    },
    async setTimezone(tz) {
        const res = await fetch('/api/system/time/timezone', {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ timezone: tz }),
        });
        if (!res.ok) {
            const err = await res.text();
            throw new Error(err || 'Set timezone failed (' + res.status + ')');
        }
    },
    async setSystemClock(localDatetime) {
        const res = await fetch('/api/system/time/clock', {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ local_datetime: localDatetime }),
        });
        if (!res.ok) {
            const err = await res.text();
            throw new Error(err || 'Set clock failed (' + res.status + ')');
        }
    },
    async syncNtp() {
        const res = await fetch('/api/system/time/sync-ntp', { method: 'POST' });
        if (!res.ok) {
            const err = await res.text();
            throw new Error(err || 'NTP sync failed (' + res.status + ')');
        }
    },
};
