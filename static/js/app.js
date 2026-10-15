// App navigation and page logic

(function bootstrapAuth(){
    let pendingReauth = null;
    let reauthOriginalText = null;
    function showOverlay(){
        const o = document.getElementById('auth-login-overlay');
        if (o) o.hidden = false;
    }
    function hideOverlay(){
        const o = document.getElementById('auth-login-overlay');
        if (o) o.hidden = true;
    }
    function showSetup(){
        const o = document.getElementById('auth-setup-overlay');
        if (o) o.hidden = false;
        hideOverlay();
    }
    function hideSetup(){
        const o = document.getElementById('auth-setup-overlay');
        if (o) o.hidden = true;
    }
    function setReauthMode(enabled) {
        const title = document.querySelector('.auth-login-title');
        const sub = document.querySelector('.auth-login-sub');
        const hint = document.querySelector('.auth-login-hint');
        const submit = document.querySelector('.auth-login-submit');
        if (!title || !sub || !hint || !submit) return;
        if (enabled) {
            if (!reauthOriginalText) reauthOriginalText = {
                title: title.textContent, sub: sub.textContent,
                hint: hint.textContent, submit: submit.textContent,
            };
            title.textContent = 'Re-authenticate to control this client';
            sub.textContent = 'Sign in again to unlock client controls. Your saved site password can fill this form.';
            hint.textContent = 'Client control stays authorized while commands are sent, up to 20 minutes idle.';
            submit.textContent = 'Re-authenticate';
        } else if (reauthOriginalText) {
            title.textContent = reauthOriginalText.title;
            sub.textContent = reauthOriginalText.sub;
            hint.textContent = reauthOriginalText.hint;
            submit.textContent = reauthOriginalText.submit;
            reauthOriginalText = null;
        }
    }
    window.requestClientControlReauth = function (scope) {
        if (pendingReauth) return pendingReauth.promise;
        let resolveRequest;
        let rejectRequest;
        const promise = new Promise((resolve, reject) => { resolveRequest = resolve; rejectRequest = reject; });
        pendingReauth = { scope, promise, resolve: resolveRequest, reject: rejectRequest };
        setReauthMode(true);
        const overlay = document.getElementById('auth-login-overlay');
        if (overlay) overlay.hidden = false;
        const password = document.getElementById('auth-login-password');
        if (password) { password.value = ''; password.focus(); }
        return promise;
    };
    async function refreshAuthGate(){
        try {
            const setup = await API.setupStatus();
            if (setup.setup_required) {
                showSetup();
                return;
            }
            hideSetup();
            const s = await API.status();
            if (!s.authenticated) showOverlay();
            else hideOverlay();
        } catch (_) { showOverlay(); }
    }
    function wire(){
        const form = document.getElementById('auth-login-form');
        const setupForm = document.getElementById('auth-setup-form');
        if (!form || !setupForm) { setTimeout(wire, 100); return; }
        if (form.dataset.wired) return;
        form.dataset.wired = '1';
        setupForm.dataset.wired = '1';
        const errEl = document.getElementById('auth-login-error');
        const setupErrEl = document.getElementById('auth-setup-error');
        const gbtn = document.getElementById('auth-google-login');
        if (gbtn && !gbtn.dataset.wired) {
            gbtn.dataset.wired = '1';
            gbtn.onclick = async () => { try { await API.loginWithGoogle(); } catch (err) { errEl.textContent = err.message; errEl.hidden = false; } };
        }
        form.addEventListener('submit', async (e) => {
            e.preventDefault();
            errEl.hidden = true;
            const pw = document.getElementById('auth-login-password').value || '';
            const usernameEl = document.getElementById('auth-login-username');
            const username = usernameEl ? usernameEl.value.trim() : '';
            try {
                const reauth = pendingReauth;
                await API.login(pw, username, reauth ? reauth.scope : undefined);
                if (reauth) {
                    pendingReauth = null;
                    setReauthMode(false);
                    hideOverlay();
                    reauth.resolve();
                    return;
                }
                hideOverlay();
                // Refresh only the page the user is currently viewing. Hidden
                // pages load lazily when opened instead of competing with the
                // initial dashboard render.
                const activePage = document.querySelector('.page.active')?.id;
                if (activePage === 'page-displays' && typeof App !== 'undefined' && App.loadConnectedClientsPage) await App.loadConnectedClientsPage();
                if (activePage === 'page-system' && typeof App !== 'undefined' && App.loadSystemPage) await App.loadSystemPage();
                if (activePage === 'page-editor' && typeof Editor !== 'undefined' && typeof Editor.refresh === 'function') await Editor.refresh();
            } catch (err) {
                errEl.textContent = err.message || 'login failed';
                errEl.hidden = false;
            }
        });
        setupForm.addEventListener('submit', async (e) => {
            e.preventDefault();
            setupErrEl.hidden = true;
            const username = document.getElementById('auth-setup-username').value.trim();
            const displayName = document.getElementById('auth-setup-display-name').value.trim();
            const password = document.getElementById('auth-setup-password').value || '';
            const confirm = document.getElementById('auth-setup-confirm').value || '';
            if (password !== confirm) {
                setupErrEl.textContent = 'Passwords do not match';
                setupErrEl.hidden = false;
                return;
            }
            try {
                await API.createInitialAdmin(username, displayName, password);
                window.location.reload();
            } catch (err) {
                setupErrEl.textContent = err.message || 'setup failed';
                setupErrEl.hidden = false;
            }
        });
        window.addEventListener('ripley:unauthorized', refreshAuthGate);
    }
    document.addEventListener('DOMContentLoaded', wire);
    if (document.readyState !== 'loading') wire();
    // Check setup state before deciding whether to show sign-in.
    (async () => {
        try {
            while (typeof API === 'undefined' || typeof API.setupStatus !== 'function') {
                await new Promise(resolve => setTimeout(resolve, 50));
            }
            await refreshAuthGate();
        } catch (_) { showOverlay(); }
    })();
})();

// Global HTML escape helper. Prefer this over inline regex substitutions.
window.esc = window.esc || function (v) { return String(v == null ? "" : v).replace(/[&<>"'`]/g, function (c) { return ({ "&":"&amp;", "<":"&lt;", ">":"&gt;", '"':"&quot;", "'":"&#39;", "`":"&#96;" })[c]; }); };


/**
 * Copy text to clipboard. Prefers the async Clipboard API; falls back to the
 * legacy textarea + document.execCommand("copy") when:
 *   - the page is served over HTTP (Clipboard API requires HTTPS or localhost), OR
 *   - the Clipboard API throws (focus / permission edge cases).
 * Resolves with true on success, false on failure.
 */
async function copyToClipboard(text) {
    // Try the modern API first
    if (navigator.clipboard && window.isSecureContext) {
        try {
            await navigator.clipboard.writeText(text);
            return true;
        } catch (_) { /* fall through to legacy */ }
    }
    // Legacy fallback: needs a focused/visible textarea we can select+copy
    try {
        const ta = document.createElement('textarea');
        ta.value = text;
        ta.setAttribute('readonly', '');
        ta.style.position = 'fixed';
        ta.style.top = '-1000px';
        ta.style.opacity = '0';
        document.body.appendChild(ta);
        ta.focus();
        ta.select();
        ta.setSelectionRange(0, text.length);
        const ok = document.execCommand('copy');
        document.body.removeChild(ta);
        return ok;
    } catch (_) {
        return false;
    }
}

const App = {
    init() {
        this.bindNav();
        Editor.init();
        // Apply persisted system variables (theme, etc.) before any UI renders.
        let _savedVars = null;
        try { _savedVars = JSON.parse(localStorage.getItem('signage.systemVars') || 'null'); } catch (e) {}
        this.applySystemVars(_savedVars);
        // Only the active page should start data loading. Hidden pages are
        // fetched when opened, so the dashboard can become interactive without
        // waiting on clients, displays, system status, or backups.
        const activePage = document.querySelector('.page.active')?.id;
        if (activePage === 'page-displays') this.loadConnectedClientsPage ? this.loadConnectedClientsPage() : this.loadDisplaysPage();
        if (activePage === 'page-system') this.loadSystemPage();
        // Auto-refresh clock widgets every second
        setInterval(() => this.updateClocks(), 1000);
        // Refresh system info every 10s when on system page
        setInterval(() => {
            if (document.getElementById('page-system').classList.contains('active')) {
                this.loadSystemPage();
            }
        }, 10000);

        // Handle backup section
        this.setupBackupSection();
        this.wireBackupUpload();
    },
    wireSystemCollapsibles() {
        document.querySelectorAll('#page-system .sys-collapsible').forEach(section => {
            if (section.dataset.systemCollapseWired) return;
            section.dataset.systemCollapseWired = '1';
            section.addEventListener('toggle', () => {
                if (!section.open) return;
                document.querySelectorAll('#page-system .sys-collapsible[open]').forEach(other => {
                    // Only enforce accordion behavior among sibling sections.
                    // Layout Defaults is nested inside System Variables & Time
                    // and must not close its parent when opened.
                    if (other !== section && other.parentElement === section.parentElement) other.open = false;
                });
            });
        });
    },

    wireBackupUpload() {
        const uploadBtn = document.getElementById('btn-upload-backup');
        const uploadFile = document.getElementById('upload-backup-file');
        if (uploadBtn && uploadFile && !uploadBtn.dataset.wired) {
            uploadBtn.onclick = () => uploadFile.click();
            uploadFile.onchange = () => {
                if (uploadFile.files[0]) this.handleUpload(uploadFile.files[0]);
                uploadFile.value = '';
            };
            uploadBtn.dataset.wired = '1';
        }
    },


    bindNav() {
        document.querySelectorAll('.nav-link').forEach(link => {
            link.onclick = () => {
                const page = link.dataset.page;
                document.querySelectorAll('.nav-link').forEach(l => l.classList.remove('active'));
                link.classList.add('active');
                document.querySelectorAll('.page').forEach(p => p.classList.remove('active'));
                document.getElementById(`page-${page}`).classList.add('active');

                if (page === 'editor') {
                    if (typeof Editor !== 'undefined' && typeof Editor.refresh === 'function') Editor.refresh();
                }
                if (page === 'displays') this.loadConnectedClientsPage ? this.loadConnectedClientsPage() : this.loadDisplaysPage();
                if (page === 'schedules') this.loadSchedulesPage();
                if (page === 'widget-templates') this.loadWidgetTemplatesPage();
                if (page === 'system') this.loadSystemPage();
                if (page === 'interactive-data') this.loadInteractiveDataPage();
                
            };
        });

        const btnRD = document.getElementById('btn-refresh-displays');
        if (btnRD) btnRD.onclick = () => this.loadConnectedClientsPage ? this.loadConnectedClientsPage() : this.loadDisplaysPage();
        const btnRS = document.getElementById('btn-refresh-system');
        if (btnRS) btnRS.onclick = () => this.loadSystemPage();
        this.wireSystemCollapsibles();
        const btnFR = document.getElementById('btn-force-refresh');
        if (btnFR) btnFR.onclick = () => this.forceDisplayRefresh();

        // Sidebar collapse/expand + account menu
        this._initSidebarCollapse();
        this._initAccountMenu();

    },

    _initSidebarCollapse() {
        const sidebar = document.getElementById('sidebar');
        const collapseBtn = document.getElementById('sidebar-collapse-btn');
        if (!sidebar || !collapseBtn) return;

        const applySidebarSize = (collapsed) => {
            const size = collapsed ? '70px' : '220px';
            sidebar.style.setProperty('width', size, 'important');
            sidebar.style.setProperty('min-width', size, 'important');
            sidebar.style.setProperty('max-width', size, 'important');
            sidebar.style.setProperty('flex', `0 0 ${size}`, 'important');
        };
        const setSidebarState = (collapsed) => {
            sidebar.classList.toggle('collapsed', collapsed);
            applySidebarSize(collapsed);
            collapseBtn.innerHTML = collapsed ? '▶' : '◀';
            collapseBtn.title = collapsed ? 'Expand sidebar' : 'Collapse sidebar';
            collapseBtn.setAttribute('aria-label', collapseBtn.title);
        };

        // Restore saved state from localStorage
        const savedState = localStorage.getItem('signage.sidebarCollapsed') === 'true';
        setSidebarState(savedState);

        collapseBtn.onclick = () => {
            const isCollapsed = !sidebar.classList.contains('collapsed');
            setSidebarState(isCollapsed);
            localStorage.setItem('signage.sidebarCollapsed', isCollapsed ? 'true' : 'false');
            // Trigger a resize event for any layout that might need it
            window.dispatchEvent(new Event('resize'));
        };
    },
    _initAccountMenu() {
        const btn = document.getElementById('account-menu-btn');
        const overlay = document.getElementById('account-popup-overlay');
        if (!btn || !overlay || btn.dataset.wired) return;
        btn.dataset.wired = '1';
        const setText = (id, text) => { const el = document.getElementById(id); if (el) el.textContent = text; };
        const status = (id, msg, bad=false) => { const el = document.getElementById(id); if (el) { el.textContent = msg || ''; el.style.color = bad ? 'var(--danger,#ef4444)' : 'var(--text-dim,#888)'; } };
        const close = () => { overlay.hidden = true; };
        const load = async () => {
            try {
                const a = await API.getAccount();
                setText('account-menu-name', a.username || 'Account');
                setText('account-menu-role', a.role || 'signed in');
                setText('account-profile-username', a.display_name || a.username || '—');
                setText('account-profile-role', a.role || '—');
                setText('account-profile-google', a.google_bound ? (a.google_email || 'Bound') : (a.google_configured ? 'Not bound' : 'Not configured'));
                status('account-google-status', a.google_configured ? (a.google_bound ? `Bound to ${a.google_email || 'Google'}` : 'Google OAuth configured. You can bind this account.') : 'Google OAuth is not configured on the server yet.');
                const bind = document.getElementById('btn-account-google-bind'); if (bind) bind.disabled = !a.google_configured || a.env_admin;
                const unbind = document.getElementById('btn-account-google-unbind'); if (unbind) unbind.disabled = !a.google_bound || a.env_admin;
                const passBtn = document.getElementById('btn-account-password'); if (passBtn) passBtn.disabled = !!a.env_admin;
                if (a.env_admin) status('account-password-status', 'Built-in admin password is managed by SIGNAGE_ADMIN_PASSWORD. Create a database admin user to change it here.');
            } catch (e) { status('account-google-status', 'Failed to load account: ' + e.message, true); }
        };
        btn.onclick = async () => { overlay.hidden = false; await load(); };
        const closeBtn = document.getElementById('account-popup-close'); if (closeBtn) closeBtn.onclick = close;
        overlay.addEventListener('click', (e) => { if (e.target === overlay) close(); });
        const passBtn = document.getElementById('btn-account-password');
        if (passBtn) passBtn.onclick = async () => {
            const cur = document.getElementById('account-current-password').value;
            const neu = document.getElementById('account-new-password').value;
            try { await API.changeAccountPassword(cur, neu); status('account-password-status', '✓ Password updated'); document.getElementById('account-current-password').value = ''; document.getElementById('account-new-password').value = ''; }
            catch (e) { status('account-password-status', '✕ ' + e.message, true); }
        };
        const bindBtn = document.getElementById('btn-account-google-bind');
        if (bindBtn) bindBtn.onclick = async () => { try { const r = await API.getGoogleBindUrl(); window.location.href = r.url; } catch (e) { status('account-google-status', '✕ ' + e.message, true); } };
        const unbindBtn = document.getElementById('btn-account-google-unbind');
        if (unbindBtn) unbindBtn.onclick = async () => { if (!(await App.confirm('Unbind Google from this account?', 'This will remove Google sign-in from this account.'))) return; try { await API.unbindGoogle(); status('account-google-status', '✓ Google unbound'); await load(); } catch (e) { status('account-google-status', '✕ ' + e.message, true); } };
        load().catch(() => {});
    },

    async loadDisplaysPage() {
        const displaysEl = document.getElementById('displays-list');
        const assignmentsEl = document.getElementById('assignments-list');
        if (!displaysEl || !assignmentsEl) return;
        try {
            const [displays, state] = await Promise.all([API.getDisplays(), API.getDisplayState()]);
            this.renderDisplays(displays);
            this.renderAssignments(displays, state);
        } catch (e) {
            displaysEl.innerHTML = `<p>Error: ${esc(e.message)}</p>`;
        }
    },
    renderDisplays(displays) {
        const container = document.getElementById('displays-list');
        if (!container) return;
        if (!displays.length) {
            container.innerHTML = '<p>No displays detected.</p>';
            return;
        }
        container.innerHTML = displays.map(d => {
            const status = String(d.status || 'unknown').replace(/[^a-zA-Z0-9_-]/g, '-');
            const connector = esc(d.connector || '');
            const label = esc(d.label || '');
            const hasOverride = d.override_width !== null && d.override_width !== undefined;
            const resText = d.current_mode ? d.current_mode.width + 'x' + d.current_mode.height : '—';
            return `
            <div class="display-card ${status}" data-connector="${connector}">
                <div class="display-badge ${status}">${esc(d.status || 'unknown')}</div>
                <div class="connector">${connector}</div>
                <div class="label">${label}</div>
                <div class="resolution">${resText}</div>
                ${hasOverride ? `<div class="override-badge">Size overridden: ${d.override_width}x${d.override_height}</div>` : ''}
                <div class="mode-list">
                    ${d.enabled ? 'Enabled' : 'Disabled'} ·
                    ${d.modes.slice(0, 3).map(m => m.width + 'x' + m.height).join(', ')}
                </div>
                ${d.status === 'connected' ? `<button class="btn btn-secondary btn-sm display-override-btn" data-connector="${connector}" data-width="${d.current_mode ? d.current_mode.width : 1920}" data-height="${d.current_mode ? d.current_mode.height : 1080}">📏 Set Size Override</button>` : ''}
            </div>
            `;
        }).join('');

        // Wire up override buttons — clicking opens a modal to set custom size
        container.querySelectorAll('.display-override-btn').forEach(btn => {
            btn.onclick = (e) => {
                e.stopPropagation();
                this.openSizeOverrideModal(btn.dataset.connector, parseInt(btn.dataset.width), parseInt(btn.dataset.height));
            };
        });

        // Also make the whole card clickable for connected displays
        container.querySelectorAll('.display-card.connected').forEach(card => {
            card.onclick = () => {
                const btn = card.querySelector('.display-override-btn');
                if (btn) btn.click();
            };
        });

    },
    openSizeOverrideModal(connector, currentW, currentH) {
        const overlay = document.createElement('div');
        overlay.className = 'fb-overlay';
        overlay.innerHTML = `
            <div class="fb-modal" style="width:400px">
                <div class="fb-header">
                    <h3>Set Display Size: ${connector}</h3>
                    <button class="fb-close">&times;</button>
                </div>
                <div style="padding:1.5rem;">
                    <p style="color:var(--text-dim);font-size:.85rem;margin-bottom:1rem;">
                        Force a custom resolution for this display. Useful when an adapter
                        (e.g. DP-to-HDMI) reports the wrong size to the computer.
                        The player will use this size when splitting the screen.
                    </p>
                    <div class="prop-row" style="margin-bottom:1rem;">
                        <div class="prop-group">
                            <label>Width (px)</label>
                            <input type="number" id="override-w" value="${currentW}" min="1" step="1">
                        </div>
                        <div class="prop-group">
                            <label>Height (px)</label>
                            <input type="number" id="override-h" value="${currentH}" min="1" step="1">
                        </div>
                    </div>
                    <div style="display:flex;gap:.5rem;flex-wrap:wrap;margin-bottom:1rem;">
                        <button class="btn btn-secondary btn-sm" data-w="1920" data-h="1080">1920×1080</button>
                        <button class="btn btn-secondary btn-sm" data-w="3840" data-h="2160">3840×2160</button>
                        <button class="btn btn-secondary btn-sm" data-w="4096" data-h="2160">4096×2160</button>
                    </div>
                    <div style="display:flex;gap:.5rem;">
                        <button class="btn btn-success btn-sm" id="override-save" style="flex:1">Save Override</button>
                        <button class="btn btn-danger btn-sm" id="override-clear">Clear Override</button>
                    </div>
                </div>
            </div>
        `;
        document.body.appendChild(overlay);

        const close = () => overlay.remove();
        overlay.querySelector('.fb-close').onclick = close;
        overlay.onclick = (e) => { if (e.target === overlay) close(); };

        // Preset buttons
        overlay.querySelectorAll('[data-w]').forEach(btn => {
            btn.onclick = () => {
                overlay.querySelector('#override-w').value = btn.dataset.w;
                overlay.querySelector('#override-h').value = btn.dataset.h;
            };
        });

        // Save
        overlay.querySelector('#override-save').onclick = async () => {
            const w = parseInt(overlay.querySelector('#override-w').value);
            const h = parseInt(overlay.querySelector('#override-h').value);
            if (!w || !h || w < 1 || h < 1) {
                alert('Enter valid dimensions');
                return;
            }
            try {
                await fetch('/api/display-overrides', {
                    method: 'PUT',
                    headers: { 'Content-Type': 'application/json' },
                    body: JSON.stringify({ display: connector, width: w, height: h }),
                });
                close();
                await this.loadDisplaysPage();
            } catch (e) {
                alert('Failed to save: ' + e.message);
            }
        };

        // Clear
        overlay.querySelector('#override-clear').onclick = async () => {
            try {
                await fetch(`/api/display-overrides/${connector}`, { method: 'DELETE' });
                close();
                await this.loadDisplaysPage();
            } catch (e) {
                alert('Failed to clear: ' + e.message);
            }
        };
    },
    async renderAssignments(displays, state) {
        const container = document.getElementById('assignments-list');
        if (!container) return;
        const connected = displays.filter(d => d.status === 'connected');

        if (!connected.length) {
            container.innerHTML = '<p>No connected displays.</p>';
            return;
        }

        // Load layouts for the dropdown
        let layouts = [];
        try { layouts = await API.getLayouts(); } catch (e) {}

        container.innerHTML = connected.map(d => {
            const assignment = state.assignments.find(a => a.display === d.connector);
            const currentLayoutId = assignment?.layout_id || '';
            const options = '<option value=\"\">— None —</option>' +
                layouts.map(l => `<option value=\"${esc(l.id)}\" ${l.id === currentLayoutId ? 'selected' : ''}>${esc(l.name)}</option>`).join('');
            return `
                <div class="assignment-row">
                    <span class="display-name">${esc(d.connector)}</span>
                    <select class="select" data-display="${esc(d.connector)}">${options}</select>
                </div>
            `;
        }).join('');

        // Wire up change events
        container.querySelectorAll('select').forEach(sel => {
            sel.onchange = async () => {
                const display = sel.dataset.display;
                const layoutId = sel.value || null;
                try {
                    const state = await API.getDisplayState();
                    const idx = state.assignments.findIndex(a => a.display === display);
                    if (idx >= 0) {
                        state.assignments[idx].layout_id = layoutId;
                    } else {
                        state.assignments.push({ display, layout_id: layoutId });
                    }
                    await API.setDisplayState(state);
                } catch (e) {
                    alert('Failed to update assignment: ' + e.message);
                }
            };
        });
    },
    async loadSystemPage() {
        try {
            const status = await API.getSystemStatus();
            this.renderSystem(status);

            this.loadBackups();
            this.loadAddons();
        } catch (e) {
            document.getElementById('system-info').innerHTML = `<p>Error: ${esc(e.message)}</p>`;
        }
        // Wire interactive controls (these are idempotent so safe to call on every load).
        this.wireSystemCollapsibles();
        this.wireSoundSettings();
        this.wireSystemVars();
        this.wireUserManager();
        this.wireSystemGoogleOAuth();
        this.wireVersionManager();
        this.wireAddonManager();
        this.wireSystemTime();
        this.wireExternalMonitoring();
        // Surface last-failure state if any. Shows the failure banner and
        // the force checkbox only when there's a previous partial failure.
        this.refreshVersionUpdateFailure();
        // If an import completed just before the service restarted, verify the
        // newly serving process reports the signed build identity.
        this.verifyPendingUpdate();
    },
    async verifyPendingUpdate() {
        let expected;
        try { expected = localStorage.getItem('ripley-pending-update-commit'); } catch (e) { return; }
        if (!expected) return;
        try {
            const running = await API.getVersion();
            const actual = running && running.git_commit ? running.git_commit : 'unknown';
            const status = document.getElementById('version-update-status');
            if (expected !== 'unknown' && actual !== expected) {
                if (status) status.textContent = '✕ Update applied but running build identity does not match expected commit.';
                try { localStorage.removeItem('ripley-pending-update-commit'); } catch (e) {}
                return;
            }
            if (status) status.textContent = '✓ Update applied; running build identity verified (' + esc(actual) + ').';
            try { localStorage.removeItem('ripley-pending-update-commit'); } catch (e) {}
        } catch (e) {
            // Keep the marker so the next system-page refresh can retry after
            // the restarted service finishes coming back up.
        }
    },
    async refreshVersionUpdateFailure() {
        try {
            const r = await API.getVersionUpdateLastFailure();
            const banner = document.getElementById('version-update-last-failure-banner');
            const forceWrap = document.getElementById('version-update-force-wrap');
            const forceCb = document.getElementById('version-update-force');
            if (!banner || !forceWrap) return;
            if (r && r.failed) {
                banner.style.display = '';
                const at = document.getElementById('version-update-failure-time');
                const ac = document.getElementById('version-update-failure-commit');
                const ae = document.getElementById('version-update-failure-error');
                if (at) at.textContent = 'when: ' + esc(r.timestamp || 'unknown');
                if (ac) ac.textContent = 'commit: ' + esc(r.git_commit || 'unknown');
                if (ae) ae.textContent = 'error: ' + esc(r.error || 'unknown');
                forceWrap.style.display = '';
                if (forceCb) forceCb.checked = false;
            } else {
                banner.style.display = 'none';
                forceWrap.style.display = 'none';
                if (forceCb) forceCb.checked = false;
            }
        } catch (e) {
            // ignore — system page still works without it
        }
    },
    renderSystem(s) {
        const container = document.getElementById('system-info');
        const fmtBytes = (b) => {
            if (b > 1e9) return (b / 1e9).toFixed(1) + ' GB';
            if (b > 1e6) return (b / 1e6).toFixed(1) + ' MB';
            return b + ' KB';
        };
        const fmtUptime = (s) => {
            const d = Math.floor(s / 86400);
            const h = Math.floor((s % 86400) / 3600);
            const m = Math.floor((s % 3600) / 60);
            return `${d}d ${h}h ${m}m`;
        };
        const memPct = s.memory_total ? (s.memory_used / s.memory_total * 100) : 0;
        const diskPct = s.disk_total ? (s.disk_used / s.disk_total * 100) : 0;

        const bar = (pct) => {
            const cls = pct > 90 ? 'crit' : pct > 75 ? 'warn' : '';
            return `<div class="progress-bar"><div class="progress-fill ${cls}" style="width:${pct}%"></div></div>`;
        };

        // Service status indicator: green dot for running, red for anything else.
        // The dot is appended to the "System Status" h2 in index.html.
        const isRunning = (s.service_status || '').toLowerCase() === 'running';
        const statusDotColor = isRunning ? 'var(--success, #2ecc71)' : 'var(--danger, #e74c3c)';
        const statusDotHtml = `<span id="service-status-dot" title="Service ${esc(s.service_status || 'unknown')}" style="display:inline-block;width:14px;height:14px;border-radius:50%;background:${statusDotColor};margin-left:0.5rem;vertical-align:middle;box-shadow:0 0 6px ${statusDotColor};"></span>`;
        const systemStatusHeader = document.getElementById('system-status-header');
        if (systemStatusHeader) {
            // Remove any previous dot first so the dot is always current.
            const oldDot = document.getElementById('service-status-dot');
            if (oldDot) oldDot.remove();
            systemStatusHeader.insertAdjacentHTML('beforeend', statusDotHtml);
        }

        // CPU temp display: "X°C" or "N/A". Appended to the CPU box's sub line so the box stays the same size.
        const cpuTempStr = s.cpu_temp !== null ? s.cpu_temp.toFixed(1) + '°C' : 'N/A';

        container.innerHTML = `
            <div class="sys-card">
                <h3>Hostname</h3>
                <div class="value">${esc(s.hostname || '')}</div>
                <div class="sub">${esc(s.os_name || '')} ${esc(s.os_version || '')}</div>
            </div>
            <div class="sys-card">
                <h3>Uptime</h3>
                <div class="value">${fmtUptime(s.uptime_seconds)}</div>
            </div>
            <div class="sys-card">
                <h3>CPU</h3>
                <div class="value">${s.cpu_usage.toFixed(1)}%</div>
                <div class="sub">${esc(String(s.cpu_count ?? ''))} cores · ${cpuTempStr}</div>
                ${bar(s.cpu_usage)}
            </div>
            <div class="sys-card">
                <h3>Memory</h3>
                <div class="value">${fmtBytes(s.memory_used)} / ${fmtBytes(s.memory_total)}</div>
                ${bar(memPct)}
            </div>
            <div class="sys-card">
                <h3>Disk (/)</h3>
                <div class="value">${fmtBytes(s.disk_used)} / ${fmtBytes(s.disk_total)}</div>
                ${bar(diskPct)}
            </div>
        `;
    },
    async forceDisplayRefresh() {
        const btn = document.getElementById('btn-force-refresh');
        const origText = btn.textContent;
        btn.disabled = true;
        btn.textContent = 'Clearing cache...';
        try {
            const res = await fetch('/api/clear-cache', { method: 'POST' });
            if (!res.ok) throw new Error(await res.text());
            btn.textContent = '✓ Cache cleared! TVs reloading...';
            // The Chromium browser on each display is restarting; TVs will reload automatically
        } catch (e) {
            btn.textContent = '✗ Failed';
            setTimeout(() => { btn.textContent = origText; btn.disabled = false; }, 2000);
            alert('Failed to refresh displays: ' + e.message);
        }
    },
    updateClocks() {
        document.querySelectorAll('.widget-content.clock').forEach(el => {
            el.textContent = new Date().toLocaleTimeString();
        });
        document.querySelectorAll('.widget-content.datetime').forEach(el => {
            el.innerHTML = new Date().toLocaleDateString(undefined, {
                weekday: 'long', year: 'numeric', month: 'long', day: 'numeric'
            });
        });
    },
    confirm(title, body, opts = {}) {
        // opts.anchor: position popup near this element
        // opts.okLabel: label for confirm button (default "Confirm")
        // opts.cancel: whether to show cancel button (default true)
        return new Promise(resolve => {
            const id = 'app-confirm-' + Date.now();
            const ov = document.createElement('div');
            ov.id = id;
            const anchor = opts.anchor || null;
            let style;
            if (anchor) {
                const r = anchor.getBoundingClientRect();
                const sx = window.scrollX, sy = window.scrollY;
                const top = r.bottom + sy + 6;
                const left = Math.max(8, Math.min(r.left + sx, window.innerWidth - 320));
                style = `position:fixed;top:${top}px;left:${left}px;z-index:99998;background:transparent;font-family:system-ui,sans-serif;`;
            } else {
                style = 'position:fixed;inset:0;display:flex;align-items:center;justify-content:center;z-index:99998;background:rgba(0,0,0,0.45);font-family:system-ui,sans-serif;';
            }
            ov.style = style; if(anchor){ ov.style+= ';display:flex;align-items:flex-start;justify-content:flex-start'; }
            const cancelBtn = opts.cancel !== false
                ? `<button id="${id}-c" style="padding:7px 16px;border:1px solid var(--border,#30363d);border-radius:6px;background:transparent;color:var(--text,#e6edf3);cursor:pointer;font-size:14px;">Cancel</button>`
                : '';
            const okLabel = opts.okLabel || 'Confirm';
            const safeOkLabel = String(okLabel).replace(/[&<>"']/g, c => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));
            ov.innerHTML = `<div style="background:var(--bg-card,#161b22);border:1px solid var(--border,#30363d);border-radius:10px;padding:18px 22px;max-width:400px;width:90%;box-shadow:0 8px 32px rgba(0,0,0,0.5);"><div id="${id}-t" style="font-size:15px;font-weight:600;color:var(--text,#e6edf3);margin-bottom:6px;"></div><div id="${id}-b" style="font-size:13px;color:var(--text-muted,#8b949e);margin-bottom:18px;line-height:1.5;"></div><div style="display:flex;gap:8px;justify-content:flex-end;">${cancelBtn}<button id="${id}-o" style="padding:7px 16px;border:none;border-radius:6px;background:var(--accent,#e94560);color:#fff;cursor:pointer;font-size:14px;font-weight:500;">${safeOkLabel}</button></div></div>`;
            document.body.appendChild(ov);
            document.getElementById(id+'-t').textContent = title;
            const be = document.getElementById(id+'-b'); if(be) be.textContent = body || '';
            if (opts.cancel !== false) document.getElementById(id+'-c').addEventListener('click', () => { ov.remove(); resolve(false); });
            document.getElementById(id+'-o').addEventListener('click', () => { ov.remove(); resolve(true); });
            if (!anchor) ov.addEventListener('click', e => { if (e.target === ov) { ov.remove(); resolve(false); } });
        });
    },
    // --- Backup Logic ---
    async setupBackupSection() {
        const container = document.getElementById('backup-section');
        if (!container) return;

        const createBtn = document.getElementById('btn-create-backup');
        if (createBtn) {
            createBtn.onclick = async () => {
                if (!(await App.confirm('Create a new system backup?', 'This may take a moment and will be saved to the backups panel.'))) return;
                
                const progressContainer = document.getElementById('backup-progress-container');
                const progressFill = document.getElementById('backup-progress-fill');
                
                createBtn.disabled = true;
                createBtn.textContent = 'Creating...';
                progressContainer.style.display = 'block';
                progressFill.style.width = '30%'; // Indeterminate start

                try {
                    const res = await fetch('/api/backups', { method: 'POST' });
                    if (!res.ok) throw new Error(await res.text());
                    
                    progressFill.style.width = '100%';
                    setTimeout(() => {
                        progressContainer.style.display = 'none';
                        progressFill.style.width = '0%';
                    }, 1000);


                    await this.loadBackups();
                } catch (e) {
                    alert('Backup failed: ' + e.message);
                    progressContainer.style.display = 'none';
                } finally {
                    createBtn.disabled = false;
                    createBtn.textContent = 'Create New Backup';
                }
            };
        }


        this.loadBackups();
    },
    async loadBackups() {
        const container = document.getElementById('backups-list');
        if (!container) return;

        try {
            const backups = await API.listBackups();
            if (backups.length === 0) {
                container.innerHTML = '<p>No backups found.</p>';
                return;
            }

            container.innerHTML = backups.map(b => `
                <div class="sys-card" style="display:flex; justify-content:space-between; align-items:center; margin-bottom:0.5rem; padding:0.75rem 1rem;">
                    <div style="display:flex; flex-direction:column; gap:0.25rem;">
                        <div style="font-weight:bold">${esc(b.name)}</div>
                        <div style="font-size:0.8rem; color:var(--text-dim)">${esc(new Date(b.created_at).toLocaleString())} · ${b.size_bytes ? (b.size_bytes / 1024 / 1024).toFixed(2) + ' MB' : 'Unknown size'}</div>
                    </div>
                    <div style="display:flex; gap:0.5rem;">
                        <button class="btn btn-secondary btn-sm restore-btn" data-id="${esc(b.id)}">Restore</button>
                        <button class="btn btn-secondary btn-sm download-btn" data-id="${esc(b.id)}">Download</button>
                        <button class="btn btn-danger btn-sm delete-btn" data-id="${esc(b.id)}">Delete</button>
                    </div>
                </div>
            `).join('');

            // Wire up buttons
            container.querySelectorAll('.restore-btn').forEach(btn => {
                btn.onclick = () => this.handleRestore(btn.dataset.id);
            });
            container.querySelectorAll('.download-btn').forEach(btn => {
                btn.onclick = () => this.handleDownload(btn.dataset.id);
            });
            container.querySelectorAll('.delete-btn').forEach(btn => {
                btn.onclick = () => this.handleDelete(btn.dataset.id);
            });

        } catch (e) {
            container.innerHTML = `<p>Error loading backups: ${esc(e.message)}</p>`;
        }
    },
    async loadAddons() {
        const container = document.getElementById('addons-list');
        if (!container) return;
        try {
            const addons = await API.listAddons();
            if (!addons.length) {
                container.innerHTML = '<p>No addons installed.</p>';
                return;
            }
            container.innerHTML = addons.map(addon => `
                <div class="sys-card" style="display:flex; justify-content:space-between; align-items:center; gap:1rem; margin-bottom:0.5rem; padding:0.75rem 1rem;">
                    <div style="display:flex; flex-direction:column; gap:0.25rem; min-width:0;">
                        <div style="font-weight:bold;">${esc(addon.name)} <span style="font-weight:normal; color:var(--text-dim);">v${esc(addon.version)}</span></div>
                        <div style="font-size:0.78rem; color:var(--text-dim);">${esc(addon.id)} · ${addon.widget_types.length} widget type(s) · ${addon.enabled ? 'Enabled' : 'Disabled'}</div>
                        <div style="font-size:0.8rem; color:var(--text-dim);">${esc(addon.description || 'No description')}</div>
                    </div>
                    <div style="display:flex; gap:0.5rem; flex-wrap:wrap; justify-content:flex-end;">
                        <button class="btn btn-secondary btn-sm addon-toggle" data-id="${esc(addon.id)}" data-enabled="${addon.enabled}">${addon.enabled ? 'Disable' : 'Enable'}</button>
                        <button class="btn btn-danger btn-sm addon-delete" data-id="${esc(addon.id)}">Remove</button>
                    </div>
                </div>
            `).join('');
            container.querySelectorAll('.addon-toggle').forEach(btn => {
                btn.onclick = async () => {
                    try {
                        await API.setAddonEnabled(btn.dataset.id, btn.dataset.enabled !== 'true');
                        await this.loadAddons();
                    } catch (e) { alert('Addon update failed: ' + e.message); }
                };
            });
            container.querySelectorAll('.addon-delete').forEach(btn => {
                btn.onclick = async () => {
                    if (!(await App.confirm('Remove this addon?', 'The installed manifest will be deleted.'))) return;
                    try {
                        await API.deleteAddon(btn.dataset.id);
                        await this.loadAddons();
                    } catch (e) { alert('Addon removal failed: ' + e.message); }
                };
            });
        } catch (e) {
            container.innerHTML = `<p>Error loading addons: ${esc(e.message)}</p>`;
        }
    },
    wireAddonManager() {
        const button = document.getElementById('btn-addon-install');
        const input = document.getElementById('addon-manifest-input');
        const packageButton = document.getElementById('btn-addon-package');
        const packageInput = document.getElementById('addon-package-file');
        const status = document.getElementById('addon-install-status');
        if (!button || !input || button.dataset.wired === 'true') return;
        button.dataset.wired = 'true';
        button.onclick = async () => {
            status.textContent = '';
            try {
                const manifest = JSON.parse(input.value);
                await API.installAddon(manifest);
                status.textContent = '✓ Installed';
                input.value = '';
                await this.loadAddons();
            } catch (e) {
                status.textContent = '✕ ' + e.message;
            }
        };
        if (packageButton && packageInput) {
            packageButton.onclick = () => packageInput.click();
            packageInput.onchange = async () => {
                const file = packageInput.files && packageInput.files[0];
                if (!file) return;
                packageButton.disabled = true;
                status.textContent = 'Uploading and verifying signature…';
                try {
                    await API.installAddonPackage(file);
                    status.textContent = '✓ Signed client package installed';
                    packageInput.value = '';
                    await this.loadAddons();
                } catch (e) {
                    status.textContent = '✕ ' + e.message;
                } finally {
                    packageButton.disabled = false;
                }
            };
        }
    },
    async handleRestore(id) {
        if (!(await App.confirm('Restore this backup?', 'This will overwrite current settings and layouts. This cannot be undone.'))) return;
        
        try {
            const res = await fetch(`/api/backups/${id}`, { method: 'PUT' });
            if (!res.ok) throw new Error(await res.text());
            alert('Restore successful! The system will now refresh.');
            window.location.reload();
        } catch (e) {
            alert('Restore failed: ' + e.message);
        }
    },
    async handleDownload(id) {
        window.location.href = `/api/backups/download/${id}`;
    },
    async handleUpload(file) {
        if (!file) return;
        if (!await App.confirm(`Upload and restore from "${file.name}"?`, `This will overwrite current settings and layouts.`)) return;

        const btn = document.getElementById('btn-upload-backup');
        const origText = btn.textContent;
        btn.disabled = true;
        btn.textContent = 'Uploading...';

        try {
            const formData = new FormData();
            formData.append('file', file);

            const res = await fetch('/api/backups/upload', {
                method: 'PUT',
                body: formData
            });

            if (!res.ok) throw new Error(await res.text());

            btn.textContent = '✅ Uploaded!';
            alert('Backup uploaded and restored successfully! The system will now refresh.');
            window.location.reload();
        } catch (e) {
            alert('Upload failed: ' + e.message);
            btn.textContent = origText;
            btn.disabled = false;
        }
    },
    async handleDelete(id) {
        if (!(await App.confirm('Delete this backup forever?', 'This cannot be undone.'))) return;

        try {
            const res = await fetch(`/api/backups/${id}`, { method: 'DELETE' });
            if (!res.ok) throw new Error(await res.text());

            await this.loadBackups();
        } catch (e) {
            alert('Delete failed: ' + e.message);
        }
    },

    // --- Schedule Logic ---
    async loadSchedulesPage() {
        const schedulesList = document.getElementById('schedules-list');
        if (!schedulesList) return;
        const loadId = (this._schedulesLoadId || 0) + 1;
        this._schedulesLoadId = loadId;

        // Wire actions immediately so the page is usable while data loads.
        const newLayoutBtn = document.getElementById('btn-new-schedule');
        if (newLayoutBtn) newLayoutBtn.onclick = () => this.openScheduleModal(null, null, 'layout');
        const newTaskBtn = document.getElementById('btn-new-task');
        if (newTaskBtn) newTaskBtn.onclick = () => this.openScheduleModal(null, null, 'task');
        const newPowerBtn = document.getElementById('btn-new-power-schedule');
        if (newPowerBtn) newPowerBtn.onclick = () => this.openScheduleModal(null, null, 'power', 'power_on');
        const displayScheduleBtn = document.getElementById('btn-new-tv-power');
        if (displayScheduleBtn) displayScheduleBtn.onclick = () => document.querySelector('.nav-link[data-page="schedules"]')?.click();

        try {
            // Paint the primary schedule rows as soon as their endpoint returns.
            const schedules = await API.listSchedules();
            if (loadId !== this._schedulesLoadId) return;
            const powerActions = ['power_on', 'power_off', 'display_sleep'];
            const needsScheduleTargets = schedules.some(s => !powerActions.includes(s.action));
            this.renderSchedules(schedules, [], [], []);
            if (!needsScheduleTargets) return;

            // Names and display metadata enrich the already-visible rows. Do
            // not make the user wait for four full catalogs before seeing them.
            const [displays, layouts, clients] = await Promise.all([
                API.getDisplays(),
                API.getLayouts(),
                API.getClients(),
            ]);
            if (loadId !== this._schedulesLoadId) return;
            this.renderSchedules(schedules, displays, layouts, clients);
        } catch (e) {
            if (loadId === this._schedulesLoadId) schedulesList.innerHTML = `<p>Error loading schedules: ${esc(e.message)}</p>`;
        }
    },



    // ── Sound Settings ──
    // Wires the global duck-volume dropdown. The value is persisted to localStorage
    // so the user's choice survives page reloads.
    wireSoundSettings() {
        const sel = document.getElementById('global-duck-volume');
        const cur = document.getElementById('duck-current');
        if (!sel || !cur) return;

        // Initialize from localStorage if present, otherwise from SoundManager.
        let stored = null;
        try { stored = localStorage.getItem('signage.duckVolume'); } catch (e) { /* localStorage may be blocked */ }
        const initial = (stored !== null && !isNaN(parseFloat(stored)))
            ? parseFloat(stored)
            : (window.SoundManager ? window.SoundManager.DUCK_VOLUME : 0.45);

        // Reflect the value into the SoundManager and the dropdown UI.
        if (window.SoundManager) {
            window.SoundManager.DUCK_VOLUME = initial;
        }
        sel.value = String(initial);
        this._updateDuckLabel(initial);

        // Avoid double-wiring.
        if (sel.dataset.wired === '1') return;
        sel.dataset.wired = '1';

        sel.onchange = () => {
            const v = parseFloat(sel.value);
            if (isNaN(v)) return;
            if (window.SoundManager) {
                window.SoundManager.DUCK_VOLUME = v;
                // Re-run reconcile so all currently-active sources immediately pick up
                // the new duck volume. (Reconcile preserves the top-priority owner at 100%.)
                if (typeof window.SoundManager._reconcile === 'function') {
                    window.SoundManager._reconcile();
                }
                // If no public reconcile exists, the next acquire/release will pick it up.
                // Live changes are best-effort.
            }
            try { localStorage.setItem('signage.duckVolume', String(v)); } catch (e) { /* ignore */ }
            this._updateDuckLabel(v);
        };
    },
    _updateDuckLabel(v) {
        const cur = document.getElementById('duck-current');
        if (cur) cur.textContent = (v * 100).toFixed(0) + '%';
    },
    // ── System Variables (System Variables dropdown) ──
    // Persists the user's chosen system variables to localStorage and applies them.
    // Each variable is read on page load; changing + clicking Save updates both the UI
    // and the persistent store.
    SYSTEM_VARS_DEFAULTS: {
        'var-clock-format': '24',
        'var-refresh-interval': '10',
        'var-theme': 'dark',
        'var-default-color': '#ffffff',
        'var-default-accent': '#e94560',
        'var-default-align': 'center',
    },
    getSystemVars() {
        const out = {};
        for (const id of Object.keys(this.SYSTEM_VARS_DEFAULTS)) {
            const el = document.getElementById(id);
            if (el) out[id] = el.value;
        }
        return out;
    },
    setSystemVars(vars) {
        for (const id of Object.keys(vars || {})) {
            const el = document.getElementById(id);
            if (el) el.value = vars[id];
        }
    },
    saveSystemVars() {
        const vars = this.getSystemVars();
        try { localStorage.setItem('signage.systemVars', JSON.stringify(vars)); } catch (e) { /* ignore */ }
        this.applySystemVars(vars);
        const status = document.getElementById('vars-save-status');
        if (status) {
            status.textContent = '✓ Saved at ' + new Date().toLocaleTimeString();
            setTimeout(() => { status.textContent = ''; }, 3000);
        }
    },
    resetSystemVars() {
        try { localStorage.removeItem('signage.systemVars'); } catch (e) { /* ignore */ }
        this.setSystemVars(this.SYSTEM_VARS_DEFAULTS);
        this.applySystemVars(this.SYSTEM_VARS_DEFAULTS);
        const status = document.getElementById('vars-save-status');
        if (status) {
            status.textContent = '↺ Reset to defaults';
            setTimeout(() => { status.textContent = ''; }, 3000);
        }
    },
    applySystemVars(vars) {
        if (!vars) return;
        // Theme: swap a data-theme attribute on <html> so the CSS can theme itself.
        const theme = vars['var-theme'] || 'dark';
        document.documentElement.setAttribute('data-theme', theme);
        // Player refresh interval: store for the player to read if it ever listens.
        // (Currently the player uses a hard-coded 10s; future work could read this.)
        try { localStorage.setItem('signage.refreshInterval', String(vars['var-refresh-interval'] || 10)); } catch (e) { /* ignore */ }
        // Clock format, defaults, etc. are read by widgets from localStorage on render.
        try {
            localStorage.setItem('signage.clockFormat', String(vars['var-clock-format'] || 24));
            localStorage.setItem('signage.defaultColor', String(vars['var-default-color'] || '#ffffff'));
            localStorage.setItem('signage.defaultAccent', String(vars['var-default-accent'] || '#e94560'));
            localStorage.setItem('signage.defaultAlign', String(vars['var-default-align'] || 'center'));
        } catch (e) { /* ignore */ }
    },
    wireSystemVars() {
        // Load saved variables into the form
        let saved = null;
        try { saved = JSON.parse(localStorage.getItem('signage.systemVars') || 'null'); } catch (e) { /* ignore */ }
        const initial = saved || this.SYSTEM_VARS_DEFAULTS;
        this.setSystemVars(initial);
        this.applySystemVars(initial);

        // Wire Save / Reset (idempotent)
        const saveBtn = document.getElementById('btn-vars-save');
        const resetBtn = document.getElementById('btn-vars-reset');
        if (saveBtn && !saveBtn.dataset.wired) {
            saveBtn.dataset.wired = '1';
            saveBtn.onclick = () => this.saveSystemVars();
        }
        if (resetBtn && !resetBtn.dataset.wired) {
            resetBtn.dataset.wired = '1';
            resetBtn.onclick = () => this.resetSystemVars();
        }
    },


    // ── User Manager ──
    async wireUserManager() {
        const list = document.getElementById('users-list');
        if (!list) return;
        const status = document.getElementById('user-manager-status');
        const setStatus = (msg, bad=false) => { if (status) { status.textContent = msg || ''; status.style.color = bad ? 'var(--danger,#ef4444)' : 'var(--text-dim,#888)'; } };
        const esc = (v) => String(v ?? '').replace(/[&<>"']/g, c => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));
        const roleOptions = (role) => ['admin','layout-manager','operator','viewer'].map(r => `<option value="${r}" ${r===role?'selected':''}>${r}</option>`).join('');
        const load = async () => {
            try {
                const users = await API.listUsers();
                if (!users.length) {
                    list.innerHTML = '<div class="client-empty">No database users yet. The built-in env admin password still works; create an admin user here when ready.</div>';
                    return;
                }
                list.innerHTML = users.map(u => `
                    <div class="user-row" data-id="${esc(u.id)}" style="display:grid;grid-template-columns:minmax(150px,1.2fr) minmax(150px,1.2fr) 150px 90px 220px;gap:.5rem;align-items:center;padding:.65rem;border:1px solid rgba(148,163,184,.18);border-radius:10px;margin-bottom:.5rem;background:rgba(15,23,42,.35);">
                        <input class="select user-username" value="${esc(u.username)}" title="Username">
                        <input class="select user-display" value="${esc(u.display_name)}" title="Display name">
                        <select class="select user-role">${roleOptions(u.role)}</select>
                        <label style="display:flex;align-items:center;gap:.35rem;font-size:.8rem;"><input type="checkbox" class="user-active" ${u.active?'checked':''}> Active</label>
                        <div style="display:flex;gap:.35rem;flex-wrap:wrap;justify-content:flex-end;">
                            <button class="btn btn-secondary btn-sm user-save">Save</button>
                            <button class="btn btn-secondary btn-sm user-password">Password</button>
                            <button class="btn btn-danger btn-sm user-delete">Delete</button>
                        </div>
                        <div style="grid-column:1/-1;font-size:.72rem;color:var(--text-dim,#888);">Role: ${esc(u.role)} · Created ${esc(u.created_at || '—')} · Last login ${esc(u.last_login_at || 'never')}</div>
                    </div>`).join('');
                list.querySelectorAll('.user-row').forEach(row => {
                    const id = row.dataset.id;
                    row.querySelector('.user-save').onclick = async () => {
                        try {
                            await API.updateUser(id, {
                                username: row.querySelector('.user-username').value.trim(),
                                display_name: row.querySelector('.user-display').value.trim(),
                                role: row.querySelector('.user-role').value,
                                active: row.querySelector('.user-active').checked,
                            });
                            setStatus('✓ User saved');
                            await load();
                        } catch (e) { setStatus('✕ ' + e.message, true); }
                    };
                    row.querySelector('.user-password').onclick = async () => {
                        const pw = prompt('Enter a new password for this user (8+ characters):');
                        if (!pw) return;
                        try { await API.updateUser(id, { password: pw }); setStatus('✓ Password updated'); }
                        catch (e) { setStatus('✕ ' + e.message, true); }
                    };
                    row.querySelector('.user-delete').onclick = async () => {
                        if (!(await App.confirm('Delete this user?', 'This will remove the user account and all their data. This cannot be undone.'))) return;
                        try { await API.deleteUser(id); setStatus('✓ User deleted'); await load(); }
                        catch (e) { setStatus('✕ ' + e.message, true); }
                    };
                });
            } catch (e) {
                list.innerHTML = '<div class="client-empty">Failed to load users: ' + esc(e.message) + '</div>';
            }
        };
        const createBtn = document.getElementById('btn-user-create');
        if (createBtn && !createBtn.dataset.wired) {
            createBtn.dataset.wired = '1';
            createBtn.onclick = async () => {
                const username = document.getElementById('user-new-username').value.trim();
                const display = document.getElementById('user-new-display').value.trim();
                const role = document.getElementById('user-new-role').value;
                const password = document.getElementById('user-new-password').value;
                try {
                    await API.createUser({ username, display_name: display || username, role, password, active: true });
                    document.getElementById('user-new-username').value = '';
                    document.getElementById('user-new-display').value = '';
                    document.getElementById('user-new-password').value = '';
                    setStatus('✓ User created');
                    await load();
                } catch (e) { setStatus('✕ ' + e.message, true); }
            };
        }
        const refreshBtn = document.getElementById('btn-users-refresh');
        if (refreshBtn && !refreshBtn.dataset.wired) {
            refreshBtn.dataset.wired = '1';
            refreshBtn.onclick = load;
        }
        await load();
    },

    // ── Version Manager ──
    async wireVersionManager() {
        // Fetch and display the version info
        try {
            const v = await API.getVersion();
            const set = (id, val) => { const el = document.getElementById(id); if (el) el.textContent = val; };
            set('vi-version', v.version || '—');
            set('vi-build-time', v.build_time || '—');
            set('vi-git-commit', v.git_commit || '—');
            // Format uptime
            const up = v.uptime_seconds || 0;
            const days = Math.floor(up / 86400);
            const hours = Math.floor((up % 86400) / 3600);
            const mins = Math.floor((up % 3600) / 60);
            set('vi-uptime', `${days}d ${hours}h ${mins}m`);
            set('vi-install-dir', v.install_dir || '—');
            set('vi-db-path', v.database_path || '—');
        } catch (e) {
            console.warn('Could not load version info:', e);
        }

        // GitHub release updater
        const githubDetails = document.getElementById('github-update-details');
        const githubStatus = document.getElementById('github-update-status');
        const githubCheckBtn = document.getElementById('btn-github-update-check');
        const githubApplyBtn = document.getElementById('btn-github-update-apply');
        const githubAuto = document.getElementById('github-auto-update');
        const renderGithubUpdate = (u) => {
            if (!u) return;
            if (githubAuto) githubAuto.checked = !!u.auto_update;
            const latest = u.latest_version || u.latest_tag || 'not checked';
            const state = u.state || 'unknown';
            if (githubDetails) {
                const lines = [
                    `Current: ${u.current_version || 'unknown'}${u.current_commit && u.current_commit !== 'unknown' ? ' (' + u.current_commit + ')' : ''}`,
                    `Latest stable: ${latest}`,
                    u.latest_name ? `Release: ${u.latest_name}` : '',
                    u.asset_name ? `Artifact: ${u.asset_name}` : '',
                    u.checked_at ? `Last checked: ${u.checked_at}` : '',
                    u.latest_notes ? `\n${u.latest_notes}` : '',
                ].filter(Boolean);
                githubDetails.textContent = lines.join('\n');
            }
            if (githubApplyBtn) githubApplyBtn.disabled = state !== 'available';
            if (githubStatus) {
                githubStatus.textContent = u.error ? '✕ ' + u.error : (state === 'available' ? 'Update available' : state === 'current' ? 'Already current' : '');
            }
        };
        const loadGithubUpdate = async () => {
            try { renderGithubUpdate(await API.getGithubUpdateStatus()); }
            catch (e) { if (githubStatus) githubStatus.textContent = '✕ ' + e.message; }
        };
        if (githubCheckBtn && !githubCheckBtn.dataset.wired) {
            githubCheckBtn.dataset.wired = '1';
            githubCheckBtn.onclick = async () => {
                githubCheckBtn.disabled = true;
                if (githubStatus) githubStatus.textContent = 'Checking GitHub releases…';
                try { renderGithubUpdate(await API.checkGithubUpdate()); }
                catch (e) { if (githubStatus) githubStatus.textContent = '✕ ' + e.message; }
                finally { githubCheckBtn.disabled = false; }
            };
        }
        if (githubApplyBtn && !githubApplyBtn.dataset.wired) {
            githubApplyBtn.dataset.wired = '1';
            githubApplyBtn.onclick = async () => {
                if (!(await App.confirm('Update server software?', 'The signed stable GitHub release will replace the server binary and frontend, preserve application data, and restart the service.'))) return;
                githubApplyBtn.disabled = true;
                if (githubStatus) githubStatus.textContent = 'Downloading and verifying update…';
                try {
                    const result = await API.applyGithubUpdate();
                    const commit = result.manifest && result.manifest.git_commit ? result.manifest.git_commit : 'unknown';
                    try { localStorage.setItem('ripley-pending-update-commit', commit); } catch (e) {}
                    if (githubStatus) githubStatus.textContent = '✓ Update applied. Service restarting…';
                    setTimeout(() => window.location.reload(), 4000);
                } catch (e) {
                    if (githubStatus) githubStatus.textContent = '✕ ' + e.message;
                    await loadGithubUpdate();
                }
            };
        }
        if (githubAuto && !githubAuto.dataset.wired) {
            githubAuto.dataset.wired = '1';
            githubAuto.onchange = async () => {
                const desired = githubAuto.checked;
                githubAuto.disabled = true;
                if (githubStatus) githubStatus.textContent = desired ? 'Enabling automatic updates…' : 'Disabling automatic updates…';
                try { renderGithubUpdate(await API.setGithubAutoUpdate(desired)); }
                catch (e) { githubAuto.checked = !desired; if (githubStatus) githubStatus.textContent = '✕ ' + e.message; }
                finally { githubAuto.disabled = false; }
            };
        }
        await loadGithubUpdate();

        // Wire Export button — download the bundle
        const exportBtn = document.getElementById('btn-version-export');
        if (exportBtn && !exportBtn.dataset.wired) {
            exportBtn.dataset.wired = '1';
            exportBtn.onclick = async () => {
                const status = document.getElementById('version-migration-status');
                if (status) status.textContent = 'Building bundle...';
                try {
                    const res = await fetch(API.exportBundleUrl());
                    if (!res.ok) throw new Error('Export failed: ' + res.status);
                    const blob = await res.blob();
                    const url = URL.createObjectURL(blob);
                    const a = document.createElement('a');
                    a.href = url;
                    a.download = 'signage-state-bundle-' + new Date().toISOString().slice(0, 10) + '.tar.gz';
                    document.body.appendChild(a);
                    a.click();
                    a.remove();
                    URL.revokeObjectURL(url);
                    if (status) status.textContent = '✓ Downloaded (' + (blob.size / 1024).toFixed(1) + ' KB)';
                } catch (err) {
                    if (status) status.textContent = '✕ ' + err.message;
                }
            };
        }

        // Wire Import button
        const importBtn = document.getElementById('btn-version-import');
        const importFile = document.getElementById('version-import-file');
        if (importBtn && importFile && !importBtn.dataset.wired) {
            importBtn.dataset.wired = '1';
            importBtn.onclick = () => importFile.click();
            importFile.onchange = async () => {
                if (!importFile.files || !importFile.files[0]) return;
                if (!(await App.confirm('Import bundle?', 'This will REPLACE the current database, uploads, and backups index with the contents of this bundle. The service will restart. This cannot be undone.'))) {
                    importFile.value = '';
                    return;
                }
                const status = document.getElementById('version-migration-status');
                if (status) status.textContent = 'Uploading bundle...';
                try {
                    await API.importBundle(importFile.files[0]);
                    if (status) status.textContent = '✓ Bundle imported. Service restarting...';
                    setTimeout(() => { window.location.reload(); }, 3000);
                } catch (err) {
                    if (status) status.textContent = '✕ ' + err.message;
                }
                importFile.value = '';
            };
        }



        // Load update-bundle preview
        const updatePreview = document.getElementById('version-update-preview');
        if (updatePreview) {
            try {
                const u = await API.getUpdatePreview();
                const mb = ((u.binary_size_bytes || 0) / (1024 * 1024)).toFixed(1);
                const shortHash = (u.binary_sha256 || '').slice(0, 16);
                const deps = (u.runtime_dependencies && u.runtime_dependencies.binaries || []).join(', ');
                updatePreview.textContent = [
                    `Version: ${u.version || '—'}  commit: ${u.git_commit || 'unknown'}`,
                    `Binary: ${mb} MB  sha256: ${shortHash}…`,
                    `Static: ${u.has_static ? 'included' : 'missing'}  Changelog: ${u.has_changelog ? 'included' : 'none'}`,
                    `Runtime deps checked on import: ${deps || 'none'}`,
                ].join('\n');
            } catch (err) {
                updatePreview.textContent = 'Could not load update manifest: ' + err.message;
            }
        }

        // Wire Update Export button — native download of binary+static+deps update file
        const updateExportBtn = document.getElementById('btn-version-update-export');
        if (updateExportBtn && !updateExportBtn.dataset.wired) {
            updateExportBtn.dataset.wired = '1';
            updateExportBtn.onclick = () => {
                const status = document.getElementById('version-update-status');
                if (status) status.textContent = 'Building update file; download should start shortly...';
                // Use top-level navigation so the browser handles Content-Disposition,
                // filenames, large files, cookies, and download prompts reliably.
                window.location.href = API.exportUpdateBundleUrl();
            };
        }

        // Wire Update Import button
        const updateImportBtn = document.getElementById('btn-version-update-import');
        const updateImportFile = document.getElementById('version-update-import-file');
        const forceCb = document.getElementById('version-update-force');
        const forceWrap = document.getElementById('version-update-force-wrap');
        if (updateImportBtn && updateImportFile && !updateImportBtn.dataset.wired) {
            updateImportBtn.dataset.wired = '1';
            updateImportBtn.onclick = () => updateImportFile.click();
            updateImportFile.onchange = async () => {
                if (!updateImportFile.files || !updateImportFile.files[0]) return;
                if (!(await App.confirm('Install update?', 'This will replace the running signage binary and static frontend files, keep a rollback backup, and restart the service. Database and uploads are NOT changed.'))) {
                    updateImportFile.value = '';
                    return;
                }
                const status = document.getElementById('version-update-status');
                if (status) status.textContent = 'Uploading update file...';
                const force = !!(forceCb && forceCb.checked);
                try {
                    const result = await API.importUpdateBundle(updateImportFile.files[0], force);
                    const commit = result.manifest && result.manifest.git_commit ? result.manifest.git_commit : 'unknown';
                    if (status) status.textContent = '✓ Update applied (' + commit + '). Service restarting...';
                    try {
                        localStorage.setItem('ripley-pending-update-commit', commit);
                        localStorage.setItem('ripley-last-update-reload', '1');
                    } catch (e) {}
                    setTimeout(() => { window.location.reload(); }, 4000);
                } catch (err) {
                    const msg = err && err.message ? err.message : 'unknown error';
                    if (status) status.textContent = '✕ ' + msg;
                }
                updateImportFile.value = '';
            };
        }


        const downloadViaLocation = (url, statusEl, label) => {
            // Same mechanism as backup downloads: a real user click sets window.location.
            // This avoids synthetic anchor/blob behavior and downloads the freshly built file.
            const href = url + (url.includes('?') ? '&' : '?') + 'fresh=' + Date.now();
            if (statusEl) statusEl.textContent = 'Starting download: ' + label;
            window.location.href = href;
        };

        // Wire Player Client Bundle buttons — build tarball and download stable bootstrap
        const playerBundlePreview = document.getElementById('version-player-bundle-preview');
        const playerBundleStatus = document.getElementById('version-player-bundle-status');
        const playerBundleBuildBtn = document.getElementById('btn-version-player-bundle-build');
        const playerBundleDownloadBtn = document.getElementById('btn-version-player-bundle-download');
        const playerBootstrapDownloadBtn = document.getElementById('btn-version-player-bootstrap-download');
        if (playerBundleBuildBtn && !playerBundleBuildBtn.dataset.wired) {
            playerBundleBuildBtn.dataset.wired = '1';
            playerBundleBuildBtn.onclick = async () => {
                if (playerBundleStatus) playerBundleStatus.textContent = 'Building player bundle...';
                try {
                    const r = await API.buildPlayerClientBundle();
                    const sizeBytes = r.size_bytes || 0;
                    const sizeText = sizeBytes < 1024 * 1024 ? ((sizeBytes / 1024).toFixed(1) + ' KB') : ((sizeBytes / (1024 * 1024)).toFixed(2) + ' MB');
                    const shortHash = (r.sha256 || '').slice(0, 16);
                    if (playerBundlePreview) playerBundlePreview.textContent = [
                        `Tarball: ${r.url}`,
                        `Bootstrap: ${r.bootstrap_url}`,
                        `Size: ${sizeText}  sha256: ${shortHash}…`,
                        `Install command: curl --fail --show-error --location --proto '=https' --tlsv1.2 -o /tmp/ripley-player-client-bundle-install.sh https://11.1.1.53:3000${r.bootstrap_url}`,
                    ].join('\n');
                    if (playerBundleStatus) playerBundleStatus.textContent = '✓ Bundle ready';
                    // Auto-log to installer changelog
                } catch (err) {
                    if (playerBundleStatus) playerBundleStatus.textContent = '✕ ' + err.message;
                }
            };
        }
        if (playerBundleDownloadBtn && !playerBundleDownloadBtn.dataset.wired) {
            playerBundleDownloadBtn.dataset.wired = '1';
            playerBundleDownloadBtn.onclick = () => downloadViaLocation(API.playerClientBundleUrl(), playerBundleStatus, 'ripley-player-client-bundle.tar.gz');
        }
        if (playerBootstrapDownloadBtn && !playerBootstrapDownloadBtn.dataset.wired) {
            playerBootstrapDownloadBtn.dataset.wired = '1';
            playerBootstrapDownloadBtn.onclick = () => downloadViaLocation(API.playerClientBootstrapUrl(), playerBundleStatus, 'ripley-player-client-bundle-install.sh');
        }
        // Wire Server Bundle buttons — build server tarball and expose native downloads
        const serverBundlePreview = document.getElementById('version-server-bundle-preview');
        const serverBundleStatus = document.getElementById('version-server-bundle-status');
        const serverBundleBuildBtn = document.getElementById('btn-version-server-bundle-build');
        const serverBundleDownloadBtn = document.getElementById('btn-version-server-bundle-download');
        const serverBootstrapDownloadBtn = document.getElementById('btn-version-server-bootstrap-download');
        if (serverBundleBuildBtn && !serverBundleBuildBtn.dataset.wired) {
            serverBundleBuildBtn.dataset.wired = '1';
            serverBundleBuildBtn.onclick = async () => {
                if (serverBundleStatus) serverBundleStatus.textContent = 'Building server bundle...';
                try {
                    const r = await API.buildServerBundle();
                    const sizeBytes = r.size_bytes || 0;
                    const sizeText = sizeBytes < 1024 * 1024 ? ((sizeBytes / 1024).toFixed(1) + ' KB') : ((sizeBytes / (1024 * 1024)).toFixed(2) + ' MB');
                    const shortHash = (r.sha256 || '').slice(0, 16);
                    if (serverBundlePreview) serverBundlePreview.textContent = [
                        `Tarball: ${r.url}`,
                        `Bootstrap: ${r.bootstrap_url}`,
                        `Size: ${sizeText}  sha256: ${shortHash}…`,
                        `Install command: curl --fail --show-error --location --proto '=https' --tlsv1.2 -o /tmp/ripley-signage-server-bundle-install.sh https://11.1.1.53:3000${r.bootstrap_url}`,
                        `Run: RIPLEY_SERVER_DIR=~/ripley-kiosk /tmp/ripley-signage-server-bundle-install.sh`,
                    ].join('\n');
                    if (serverBundleStatus) serverBundleStatus.textContent = '✓ Server bundle ready';
                    // Auto-log to installer changelog
                } catch (err) {
                    if (serverBundleStatus) serverBundleStatus.textContent = '✕ ' + err.message;
                }
            };
        }
        if (serverBundleDownloadBtn && !serverBundleDownloadBtn.dataset.wired) {
            serverBundleDownloadBtn.dataset.wired = '1';
            serverBundleDownloadBtn.onclick = () => downloadViaLocation(API.serverBundleUrl(), serverBundleStatus, 'ripley-signage-server-bundle.tar.gz');
        }
        if (serverBootstrapDownloadBtn && !serverBootstrapDownloadBtn.dataset.wired) {
            serverBootstrapDownloadBtn.dataset.wired = '1';
            serverBootstrapDownloadBtn.onclick = () => downloadViaLocation(API.serverBootstrapUrl(), serverBundleStatus, 'ripley-signage-server-bundle-install.sh');
        }
        // Wire Restart button
        const restartBtn = document.getElementById('btn-version-restart');
        if (restartBtn && !restartBtn.dataset.wired) {
            restartBtn.dataset.wired = '1';
            restartBtn.onclick = async () => {
                if (!(await App.confirm('Restart signage service?', 'The current connection will drop and the page will reload.'))) return;
                const status = document.getElementById('version-restart-status');
                if (status) status.textContent = 'Restarting...';
                try {
                    await API.restartService();
                    if (status) status.textContent = '✓ Service restarted. Reloading...';
                    setTimeout(() => { window.location.reload(); }, 3000);
                } catch (err) {
                    if (status) status.textContent = '✕ ' + err.message;
                }
            };
        }
    },


    async wireSystemGoogleOAuth() {
        const section = document.getElementById('vars-google-oauth');
        if (!section) return;
        const setText = (id, text) => { const el = document.getElementById(id); if (el) el.textContent = text; };
        const status = (msg, isError = false) => {
            const el = document.getElementById('sys-google-status');
            if (!el) return;
            el.textContent = msg;
            el.style.color = isError ? 'var(--danger,#ef4444)' : 'var(--text-dim,#888)';
        };
        const redirect = document.getElementById('sys-google-redirect-uri');
        if (redirect) redirect.value = `${window.location.origin}/api/auth/google/callback`;
        const copyBtn = document.getElementById('btn-copy-google-redirect');
        if (copyBtn && !copyBtn.dataset.wired) {
            copyBtn.dataset.wired = '1';
            copyBtn.onclick = async () => {
                const value = document.getElementById('sys-google-redirect-uri')?.value || '';
                try {
                    if (window.isSecureContext && navigator.clipboard) await navigator.clipboard.writeText(value);
                    else {
                        const ta = document.createElement('textarea');
                        ta.value = value;
                        ta.style.position = 'fixed';
                        ta.style.left = '-9999px';
                        document.body.appendChild(ta);
                        ta.focus();
                        ta.select();
                        document.execCommand('copy');
                        ta.remove();
                    }
                    status('✓ Redirect URI copied.');
                } catch (e) {
                    status('Copy failed; select the redirect URI and copy manually.', true);
                }
            };
        }
        const load = async () => {
            try {
                const a = await API.getAccount();
                setText('sys-google-configured', a.google_configured ? 'Yes' : 'No');
                setText('sys-google-bound', a.google_bound ? 'Yes' : 'No');
                setText('sys-google-email', a.google_email || '—');
                setText('sys-google-login-type', a.env_admin ? 'Built-in env admin' : `${a.username || 'user'} / ${a.role || 'role'}`);
                const bind = document.getElementById('btn-sys-google-bind');
                const unbind = document.getElementById('btn-sys-google-unbind');
                if (bind) bind.disabled = !a.google_configured || !!a.env_admin;
                if (unbind) unbind.disabled = !a.google_bound || !!a.env_admin;
                if (!a.google_configured) status('Google OAuth is not configured. Add the client ID/secret env vars and restart the server.');
                else if (a.env_admin) status('Google OAuth is configured, but the built-in env admin cannot be bound. Create/sign in as a database admin user first.');
                else if (a.google_bound) status(`Google is bound to ${a.google_email || 'this account'}.`);
                else status('Google OAuth is configured. Bind this account to enable Google sign-in.');
            } catch (e) {
                status('Failed to load Google OAuth status: ' + e.message, true);
            }
        };
        const refresh = document.getElementById('btn-google-oauth-refresh');
        if (refresh && !refresh.dataset.wired) {
            refresh.dataset.wired = '1';
            refresh.onclick = load;
        }
        const bind = document.getElementById('btn-sys-google-bind');
        if (bind && !bind.dataset.wired) {
            bind.dataset.wired = '1';
            bind.onclick = async () => {
                try {
                    const r = await API.getGoogleBindUrl();
                    if (r.url) window.location.href = r.url;
                    else status('Google bind URL was not returned.', true);
                } catch (e) {
                    status('Google bind failed: ' + e.message, true);
                }
            };
        }
        const unbind = document.getElementById('btn-sys-google-unbind');
        if (unbind && !unbind.dataset.wired) {
            unbind.dataset.wired = '1';
            unbind.onclick = async () => {
                if (!(await App.confirm('Unbind Google?', 'This will remove Google sign-in from the current account.'))) return;
                try {
                    await API.unbindGoogle();
                    status('✓ Google unbound.');
                    await load();
                } catch (e) {
                    status('Google unbind failed: ' + e.message, true);
                }
            };
        }
        await load();
    },


    // ── System Time ──
    // Shows the current local time, lets the user change the timezone, set the
    // system clock manually, or trigger an NTP re-sync. Live-updates the time
    // display every second.
    async wireSystemTime() {
        // Populate the timezone dropdown with common IANA zones
        const tzSelect = document.getElementById('var-timezone-select');
        if (tzSelect && !tzSelect.dataset.populated) {
            tzSelect.dataset.populated = '1';
            const common = [
                'America/New_York', 'America/Chicago', 'America/Denver', 'America/Los_Angeles',
                'America/Phoenix', 'America/Anchorage', 'Pacific/Honolulu',
                'America/Toronto', 'America/Mexico_City', 'America/Sao_Paulo',
                'Europe/London', 'Europe/Paris', 'Europe/Berlin', 'Europe/Madrid',
                'Europe/Rome', 'Europe/Moscow',
                'Africa/Cairo', 'Africa/Johannesburg',
                'Asia/Dubai', 'Asia/Kolkata', 'Asia/Bangkok', 'Asia/Shanghai',
                'Asia/Tokyo', 'Asia/Seoul', 'Asia/Singapore',
                'Australia/Sydney', 'Australia/Perth',
                'Pacific/Auckland', 'UTC',
            ];
            // Add a custom entry option
            tzSelect.innerHTML = '<option value="">— Select a timezone —</option>' +
                common.map(z => '<option value="' + z + '">' + z + '</option>').join('') +
                '<option value="__custom__">Other (enter manually)...</option>';
            // On selecting "Other", prompt for a custom value
            tzSelect.addEventListener('change', () => {
                if (tzSelect.value === '__custom__') {
                    const v = prompt('Enter IANA timezone (e.g. America/Chicago):');
                    if (v) {
                        // Add as a new option and select it
                        const opt = document.createElement('option');
                        opt.value = v;
                        opt.textContent = v;
                        opt.selected = true;
                        tzSelect.insertBefore(opt, tzSelect.querySelector('option[value="__custom__"]'));
                    } else {
                        tzSelect.value = '';
                    }
                }
            });
        }

        await this._refreshSystemTime();
        this._renderCurrentTime();

        // Start a 1-second interval to update the time display (only if not already running)
        if (this._timeRefreshInterval) clearInterval(this._timeRefreshInterval);
        this._timeRefreshInterval = setInterval(() => this._renderCurrentTime(), 1000);

        // Wire buttons (idempotent)
        const refreshBtn = document.getElementById('btn-time-refresh');
        if (refreshBtn && !refreshBtn.dataset.wired) {
            refreshBtn.dataset.wired = '1';
            refreshBtn.onclick = () => this._refreshSystemTime();
        }
        const tzBtn = document.getElementById('btn-tz-apply');
        if (tzBtn && !tzBtn.dataset.wired) {
            tzBtn.dataset.wired = '1';
            tzBtn.onclick = async () => {
                const sel = document.getElementById('var-timezone-select');
                const tz = sel.value.trim();
                if (!tz || tz === '__custom__') {
                    alert('Please select a timezone first.');
                    return;
                }
                if (!confirm('Change system timezone to ' + tz + '? This affects all programs and may log the change to system logs.')) return;
                const status = document.getElementById('time-action-status');
                if (status) status.textContent = 'Setting timezone...';
                try {
                    await API.setTimezone(tz);
                    if (status) status.textContent = '✓ Timezone changed. Refreshing...';
                    await this._refreshSystemTime();
                } catch (err) {
                    if (status) status.textContent = '✕ ' + err.message;
                }
            };
        }
        const setTimeBtn = document.getElementById('btn-set-time');
        if (setTimeBtn && !setTimeBtn.dataset.wired) {
            setTimeBtn.dataset.wired = '1';
            setTimeBtn.onclick = async () => {
                const dateInput = document.getElementById('var-set-date');
                const timeInput = document.getElementById('var-set-time');
                const date = dateInput.value;
                const time = timeInput.value;
                if (!date || !time) {
                    alert('Please enter both a date and a time.');
                    return;
                }
                const localDt = date + ' ' + time + ':00';
                if (!confirm('Set the system clock to ' + localDt + '?\n\nNTP will be temporarily disabled, the clock will be set, then NTP will be re-enabled.\n\nThis may briefly affect timestamps in logs and TLS certificate checks.')) return;
                const status = document.getElementById('time-action-status');
                if (status) status.textContent = 'Setting clock...';
                try {
                    await API.setSystemClock(localDt);
                    if (status) status.textContent = '✓ Clock set. Refreshing...';
                    await this._refreshSystemTime();
                } catch (err) {
                    if (status) status.textContent = '✕ ' + err.message;
                }
            };
        }
        const syncBtn = document.getElementById('btn-sync-ntp');
        if (syncBtn && !syncBtn.dataset.wired) {
            syncBtn.dataset.wired = '1';
            syncBtn.onclick = async () => {
                const status = document.getElementById('time-action-status');
                if (status) status.textContent = 'Triggering NTP sync...';
                try {
                    await API.syncNtp();
                    if (status) status.textContent = '✓ NTP sync triggered. Wait a few seconds, then click Refresh.';
                    setTimeout(() => this._refreshSystemTime(), 3000);
                } catch (err) {
                    if (status) status.textContent = '✕ ' + err.message;
                }
            };
        }
        // Trash: load and render trash list
        const openTrashBtn = document.getElementById('btn-open-trash');
        const trashList = document.getElementById('trash-list');
        if (openTrashBtn && !openTrashBtn.dataset.wired) {
            openTrashBtn.dataset.wired = '1';
            openTrashBtn.onclick = async () => {
                if (!trashList) return;
                if (trashList.style.display === 'none') {
                    try {
                        const entries = await API.listTrash();
                        if (!entries || entries.length === 0) {
                            trashList.innerHTML = '<p style="margin:0.5rem 0;color:var(--text-muted)">Trash is empty.</p>';
                        } else {
                            trashList.innerHTML = entries.map(e => `
                                <div style="display:flex;align-items:center;justify-content:space-between;padding:0.3rem 0;border-bottom:1px solid var(--border)" data-trash-id="${esc(e.id)}">
                                    <span style="flex:1;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;margin-right:0.5rem">
                                        <strong>${esc(e.category)}</strong>: ${esc(e.name)}
                                        <span style="color:var(--text-muted);font-size:0.8em"> — ${(e.size_bytes/1024).toFixed(1)} KB · ${esc(e.deleted_at)}</span>
                                    </span>
                                    <button class="btn btn-small btn-secondary btn-trash-restore" style="flex-shrink:0">↩ Restore</button>
                                </div>
                            `).join('');
                            trashList.onclick = async (e) => {
                                const btn = e.target.closest('.btn-trash-restore');
                                if (!btn) return;
                                const row = btn.closest('[data-trash-id]');
                                if (!row) return;
                                const id = row.dataset.trashId;
                                try {
                                    await API.restoreTrash(id);
                                    openTrashBtn.click();
                                } catch(err) {
                                    alert('Restore failed: ' + err.message);
                                }
                            };
                        }
                        trashList.style.display = 'block';
                        openTrashBtn.textContent = 'Close Trash';
                    } catch(err) {
                        trashList.innerHTML = '<p style="color:red">Failed to load trash: ' + esc(err.message) + '</p>';
                        trashList.style.display = 'block';
                    }
                } else {
                    trashList.style.display = 'none';
                    trashList.innerHTML = '';
                    openTrashBtn.textContent = 'Open Trash';
                }
            };
        }
    },

    async _refreshSystemTime() {
        try {
            const t = await API.getSystemTime();
            this._lastSystemTime = t;
            // Update the dropdown to show current timezone
            const sel = document.getElementById('var-timezone-select');
            if (sel) {
                const opts = Array.from(sel.options);
                const match = opts.find(o => o.value === t.timezone);
                if (match) {
                    sel.value = t.timezone;
                } else {
                    // Add the current timezone as a custom option
                    const opt = document.createElement('option');
                    opt.value = t.timezone;
                    opt.textContent = t.timezone + ' (current)';
                    opt.selected = true;
                    sel.insertBefore(opt, sel.firstChild.nextSibling);
                }
            }
            // Update the date/time input defaults
            const dt = t.local_time; // "2026-07-23T09:54:35-04:00"
            const datePart = dt.split('T')[0];
            const timePart = dt.split('T')[1].substring(0, 5);
            const dateIn = document.getElementById('var-set-date');
            const timeIn = document.getElementById('var-set-time');
            if (dateIn && !dateIn.value) dateIn.value = datePart;
            if (timeIn && !timeIn.value) timeIn.value = timePart;
        } catch (e) {
            console.warn('Failed to refresh system time:', e);
        }
    },
    _renderCurrentTime() {
        // Render the current time directly from the browser, since we know the offset.
        const now = new Date();
        const h = String(now.getHours()).padStart(2, '0');
        const m = String(now.getMinutes()).padStart(2, '0');
        const s = String(now.getSeconds()).padStart(2, '0');
        const timeEl = document.getElementById('sys-now-time');
        if (timeEl) timeEl.textContent = h + ':' + m + ':' + s;
        const dateEl = document.getElementById('sys-now-date');
        if (dateEl) dateEl.textContent = now.toLocaleDateString(undefined, { weekday: 'short', year: 'numeric', month: 'short', day: 'numeric' });
        // The timezone, NTP status, etc. only update on _refreshSystemTime
        if (this._lastSystemTime) {
            const t = this._lastSystemTime;
            const tzEl = document.getElementById('sys-now-tz');
            if (tzEl) tzEl.textContent = t.timezone + ' (' + t.timezone_abbr + ')';
            const ntpEl = document.getElementById('sys-now-ntp');
            if (ntpEl) {
                if (t.ntp_synchronized) {
                    ntpEl.innerHTML = '<span style="color:#4ade80;">✓ synced</span>';
                } else if (t.ntp_active) {
                    ntpEl.innerHTML = '<span style="color:#fbbf24;">⟳ syncing</span>';
                } else {
                    ntpEl.innerHTML = '<span style="color:#f87171;">✕ off</span>';
                }
            }
        }
    },

    // ── External Monitoring panel wiring ──
    // Renders the health endpoint URL + a curl one-liner, and wires
    // copy-to-clipboard buttons. Called from init() alongside the
    // other system menu wirings. The URL is derived from the page's
    // own host so it works regardless of which port the operator
    // hit (3000 HTTP, 3005 HTTPS, behind a reverse proxy, etc.).
    wireExternalMonitoring() {
        const urlEl = document.getElementById('monitor-health-url');
        const curlEl = document.getElementById('monitor-curl');
        if (!urlEl || !curlEl) return; // Page not yet rendered, or section collapsed.
        const origin = window.location.origin;
        const healthUrl = origin + '/api/system/health';
        const curlCmd = `curl -fsS ${healthUrl} | jq .`;
        urlEl.textContent = healthUrl;
        curlEl.textContent = curlCmd;
        // Test it now so the user sees a real response (or a clear
        // error) instead of guessing whether the endpoint works.
        this._testHealthEndpoint(healthUrl);
        // Copy buttons
        const copyUrl = document.getElementById('btn-monitor-copy-url');
        const copyCurl = document.getElementById('btn-monitor-copy-curl');
        if (copyUrl) {
            copyUrl.onclick = () => this._copyToClipboard(healthUrl, copyUrl);
        }
        if (copyCurl) {
            copyCurl.onclick = () => this._copyToClipboard(curlCmd, copyCurl);
        }
    },
    async _testHealthEndpoint(healthUrl) {
        // Show a small status line under the URL so the user knows
        // whether the endpoint actually responds. Uses fetch (not
        // API.getSystemTime etc.) so the no-auth path is exercised.
        const curlEl = document.getElementById('monitor-curl');
        if (!curlEl) return;
        const note = document.createElement('div');
        note.style.fontSize = '0.75rem';
        note.style.marginTop = '0.3rem';
        note.style.fontFamily = 'ui-monospace,Menlo,monospace';
        curlEl.parentNode.insertBefore(note, curlEl.nextSibling);
        try {
            const res = await fetch(healthUrl, { method: 'GET' });
            if (res.ok) {
                const body = await res.text();
                note.style.color = '#4ade80';
                note.textContent = `✓ ${res.status} — ${body.slice(0, 120)}${body.length > 120 ? '…' : ''}`;
            } else {
                note.style.color = '#f87171';
                note.textContent = `✕ HTTP ${res.status} — endpoint returned an error`;
            }
        } catch (e) {
            note.style.color = '#fbbf24';
            note.textContent = `⟳ Could not reach the endpoint from this browser (${e.message}). The endpoint is still reachable from external monitors on the same network.`;
        }
    },
    async _copyToClipboard(text, btn) {
        try {
            // navigator.clipboard requires a secure context (HTTPS or
            // localhost). Fall back to a temporary textarea for the
            // dev http://11.1.1.25:3000 case.
            if (navigator.clipboard && window.isSecureContext) {
                await navigator.clipboard.writeText(text);
            } else {
                const ta = document.createElement('textarea');
                ta.value = text;
                ta.style.position = 'fixed';
                ta.style.left = '-9999px';
                document.body.appendChild(ta);
                ta.select();
                document.execCommand('copy');
                document.body.removeChild(ta);
            }
            const old = btn.textContent;
            btn.textContent = '✓ Copied';
            btn.style.color = '#4ade80';
            setTimeout(() => {
                btn.textContent = old;
                btn.style.color = '';
            }, 1500);
        } catch (e) {
            const old = btn.textContent;
            btn.textContent = '✕ Failed';
            btn.style.color = '#f87171';
            setTimeout(() => {
                btn.textContent = old;
                btn.style.color = '';
            }, 1500);
        }
    },

    // ── Template cache invalidation ──
    // Called after any widget-preset mutation so the editor's cached template
    // list refreshes on next render.
    _notifyTemplateChanged() {
        // Invalidate caches for both the editor's properties panel and the template editor's sidebar.
        this._tplSidebarCache = null;
        this._tplSidebarCacheAt = 0;
        window.dispatchEvent(new CustomEvent('widget-templates-changed'));
        if (typeof Editor !== 'undefined' && Editor.invalidateTemplateCache) {
            Editor.invalidateTemplateCache();
        }
        // If the template editor page is currently visible, re-render the sidebar
        const form = document.getElementById('tpl-page-form');
        if (form && form.style.display !== 'none') {
            this._renderTemplateSidebar();
        }
    },

    renderSchedules(schedules, displays, layouts, clients = []) {
        const container = document.getElementById('schedules-list');
        if (!container) return;

        if (schedules.length === 0) {
            container.innerHTML = '<p style="color:var(--text-dim)">No schedules yet. Click "+ New Layout Schedule" to add a layout switch, or "+ New Task" to add a scheduled task.</p>';
            return;
        }

        const dayNames = ['Sunday', 'Monday', 'Tuesday', 'Wednesday', 'Thursday', 'Friday', 'Saturday'];

        // Format a day-of-week bitmask into a human label.
        // Mirrors the Rust helpers in src/schedule.rs.
        function dayLabel(mask) {
            if (mask === -1) return 'Every Day';
            if (mask === 62) return 'Mon–Fri (Business Days)';
            if (mask === 65) return 'Sat–Sun (Weekends)';
            if (mask === 0) return 'Never';
            const days = [];
            for (let i = 0; i < 7; i++) {
                if ((mask & (1 << i)) !== 0) days.push(dayNames[i]);
            }
            return days.length ? days.join(', ') : 'Never';
        }

        // Separate power vs layout schedules
        const powerSchedules = schedules.filter(s => ['power_on', 'power_off', 'display_sleep'].includes(s.action));
        const layoutSchedules = schedules.filter(s => !['power_on', 'power_off', 'display_sleep'].includes(s.action));

        container.innerHTML = '';


        // ── Power schedules section ──────────────────────────────────────────
        if (powerSchedules.length > 0) {
            const powerHeader = document.createElement('div');
            powerHeader.style.cssText = 'font-size:0.85rem; font-weight:bold; color:var(--accent); margin:1rem 0 0.5rem;';
            powerHeader.textContent = '⚡ Display Power Schedules';
            container.appendChild(powerHeader);

            const methodLabels = { auto: 'Auto', cec: 'HDMI-CEC', ddc: 'DDC/CI', ir: 'IR', none: 'None', '': 'Auto' };
            powerSchedules.forEach(s => {
                const card = document.createElement('div');
                card.className = 'sys-card';
                card.style.cssText = 'display:flex; justify-content:space-between; align-items:center; margin-bottom:0.5rem; padding:0.75rem 1rem; border-left:3px solid var(--accent);';
                const isSleep = s.action === 'display_sleep';
                const method = methodLabels[s.control_method || 'auto'] || 'Auto';
                const displayIdx = s.display_index !== undefined && s.display_index !== null ? s.display_index : 0;
                card.innerHTML = `
                    <div style="display:flex; flex-direction:column; gap:0.2rem;">
                        <div style="font-weight:bold; font-size:1rem;">
                            ${s.action === 'power_on' ? '🔆' : s.action === 'display_sleep' ? '😴' : '⏻'} ${esc(dayLabel(s.day_of_week))}, ${esc(s.start_time)} – ${esc(s.end_time)}
                        </div>
                        <div style="font-size:0.85rem; color:var(--text-dim);">
                            Client: <strong>${esc(s.target_client || '—')}</strong> &nbsp;|&nbsp;
                            Display <strong>${esc(String(displayIdx))}</strong> &nbsp;|&nbsp;
                            <strong>${s.action === 'power_on' ? 'Power On' : s.action === 'display_sleep' ? 'Display Sleep' : 'Power Off'}</strong> via <strong>${esc(method)}</strong>
                        </div>
                    </div>
                    <div style="display:flex; gap:0.5rem;">
                        <button class="btn btn-secondary btn-sm edit-schedule-btn" data-id="${esc(s.id)}" data-action="${esc(s.action)}">Edit</button>
                        <button class="btn btn-danger btn-sm delete-schedule-btn" data-id="${esc(s.id)}">Delete</button>
                    </div>`;
                container.appendChild(card);
            });
        }

        // ── Layout schedules section ──────────────────────────────────────────
        if (layoutSchedules.length > 0) {
            const layoutHeader = document.createElement('div');
            layoutHeader.style.cssText = 'font-size:0.85rem; font-weight:bold; color:var(--accent); margin:1rem 0 0.5rem;';
            layoutHeader.textContent = '🎬 Layout Schedules';
            container.appendChild(layoutHeader);

            layoutSchedules.forEach(s => {
                const display = displays.find(d => d.connector === s.display);
                const layout = layouts.find(l => l.id === s.layout_id);
                const client = clients.find(c => c.id === s.target_client);
                const clientLabel = client ? (client.name || client.id) : (s.target_client || 'Unassigned');
                const displayLabel = s.display_name || s.display || '—';
                const card = document.createElement('div');
                card.className = 'sys-card';
                card.style.cssText = 'display:flex; justify-content:space-between; align-items:center; margin-bottom:0.5rem; padding:0.75rem 1rem;';
                card.innerHTML = `
                    <div style="display:flex; flex-direction:column; gap:0.2rem;">
                        <div style="font-weight:bold; font-size:1rem;">
                            ⏰ ${esc(dayLabel(s.day_of_week))}, ${esc(s.start_time)} – ${esc(s.end_time)}
                        </div>
                        <div style="font-size:0.85rem; color:var(--text-dim);">
                            <strong>Client:</strong> ${esc(clientLabel)}
                            &nbsp;|&nbsp;
                            <strong>Display:</strong> ${esc(displayLabel)} ${display ? '(' + esc(display.label) + ')' : ''}
                            &nbsp;|&nbsp;
                            <strong>Layout:</strong> ${esc(layout ? layout.name : s.layout_id)}
                        </div>
                    </div>
                    <div style="display:flex; gap:0.5rem;">
                        <button class="btn btn-secondary btn-sm edit-schedule-btn" data-id="${esc(s.id)}" data-action="layout">Edit</button>
                        <button class="btn btn-danger btn-sm delete-schedule-btn" data-id="${esc(s.id)}">Delete</button>
                    </div>`;
                container.appendChild(card);
            });
        }

        // Empty state
        if (schedules.length === 0) {
            container.innerHTML = '<p style="color:var(--text-dim)">No schedules yet. Click "+ New Layout Schedule" to add a layout switch, or "+ New Display Power Schedule" to add a display power schedule.</p>';
        }

        // Wire up buttons — power schedules open in 'power' mode, layout in 'layout'
        container.querySelectorAll('.edit-schedule-btn').forEach(btn => {
            const action = btn.dataset.action;
            const mode = ['power_on', 'power_off', 'display_sleep'].includes(action) ? 'power' : 'layout';
            btn.onclick = () => this.openScheduleModal(btn.dataset.id, schedules, mode, action);
        });
        container.querySelectorAll('.delete-schedule-btn').forEach(btn => {
            btn.onclick = () => this.handleDeleteSchedule(btn.dataset.id);
        });
    },

    openAssignLayoutModal(layoutId) {
        const layoutNameVal = document.getElementById('layout-browser-current')?.textContent?.trim() || layoutId || '';
        const modalId = 'assign-layout-modal';
        const existing = document.getElementById(modalId);
        if (existing) existing.remove();

        const overlay = document.createElement('div');
        overlay.id = modalId;
        overlay.style.cssText = 'position:fixed;inset:0;background:rgba(0,0,0,0.7);z-index:9000;display:flex;align-items:center;justify-content:center;';
        overlay.innerHTML = `
            <div style="background:var(--bg-card);border:1px solid var(--border);border-radius:8px;width:420px;max-height:80vh;display:flex;flex-direction:column;overflow:hidden;">
                <div style="padding:1rem 1.5rem;border-bottom:1px solid var(--border);display:flex;justify-content:space-between;align-items:center;">
                    <h3 style="margin:0;font-size:1rem;">Assign Layout</h3>
                    <button id="assign-modal-close" style="background:none;border:none;color:var(--text-dim);font-size:1.5rem;cursor:pointer;line-height:1;">&times;</button>
                </div>
                <div style="padding:1.25rem 1.5rem;display:flex;flex-direction:column;gap:1rem;overflow-y:auto;">
                    <div style="font-size:0.85rem;color:var(--text-dim);padding:0.5rem;background:var(--bg-elev);border-radius:4px;">
                        Assigning: <strong id="assign-layout-name" style="color:var(--accent);">${layoutNameVal || layoutId}</strong>
                    </div>
                    <div class="prop-group">
                        <label style="font-size:0.8rem;color:var(--text-dim);display:block;margin-bottom:0.3rem;">Device</label>
                        <select id="assign-client" class="select" style="width:100%;">
                            <option value="">— Select a device —</option>
                        </select>
                    </div>
                    <div class="prop-group">
                        <label style="font-size:0.8rem;color:var(--text-dim);display:block;margin-bottom:0.3rem;">Display</label>
                        <select id="assign-display" class="select" style="width:100%;" disabled>
                            <option value="">— Select a display —</option>
                        </select>
                        <div id="assign-display-hint" style="font-size:0.75rem;color:var(--text-dim);margin-top:0.25rem;">Choose a device first</div>
                    </div>
                    <div id="assign-status" style="display:none;font-size:0.85rem;padding:0.5rem;border-radius:4px;"></div>
                </div>
                <div style="padding:1rem 1.5rem;border-top:1px solid var(--border);display:flex;gap:0.5rem;justify-content:flex-end;">
                    <button id="assign-cancel" class="btn btn-secondary">Cancel</button>
                    <button id="assign-do" class="btn btn-primary" disabled>Assign</button>
                </div>
            </div>
        `;
        document.body.appendChild(overlay);

        const clientSel = overlay.querySelector('#assign-client');
        const displaySel = overlay.querySelector('#assign-display');
        const displayHint = overlay.querySelector('#assign-display-hint');
        const assignDo = overlay.querySelector('#assign-do');
        const statusDiv = overlay.querySelector('#assign-status');

        const close = () => { const m = document.getElementById(modalId); if (m) m.remove(); };
        overlay.querySelector('#assign-modal-close').onclick = close;
        overlay.querySelector('#assign-cancel').onclick = close;
        overlay.addEventListener('click', e => { if (e.target === overlay) close(); });

        ;(async () => {
            try {
                const clients = await API.getClients();
                const online = clients.filter(c => c.status === 'online');
                clientSel.innerHTML = '<option value="">— Select a device —</option>' +
                    online.map(c => `<option value="${esc(c.id)}">${esc(c.name || c.id)} (${esc(c.status)})</option>`).join('');
                if (online.length === 0) clientSel.innerHTML = '<option value="">No online devices</option>';
            } catch(e) { clientSel.innerHTML = '<option value="">Failed to load devices</option>'; }
        })();

        clientSel.onchange = async () => {
            displaySel.disabled = true;
            displaySel.innerHTML = '<option value="">— Select a display —</option>';
            assignDo.disabled = true;
            displayHint.textContent = 'Loading...';
            const clientId = clientSel.value;
            if (!clientId) { displayHint.textContent = 'Choose a device first'; return; }
            try {
                const displays = await API.getClientDisplays(clientId);
                displaySel.innerHTML = '<option value="">— Select a display —</option>' +
                    displays.map((d, i) => {
                        const active = d.status === 'active' || d.status === 'online' || d.assigned_layout_id;
                        const dot = active ? '●' : '○';
                        const label = d.label || d.name || `Display ${i}`;
                        return `<option value="${i}" data-active="${active}" data-connector="${esc(d.connector || '')}">${dot} ${esc(label)} — ${esc(d.status || 'unknown')}</option>`;
                    }).join('');
                displaySel.disabled = false;
                displayHint.textContent = `${displays.length} display(s) found`;
                displayHint.style.color = displays.length ? '#3fb950' : '#f85149';
            } catch(e) { displayHint.textContent = 'Failed to load'; displayHint.style.color = '#f85149'; }
        };

        displaySel.onchange = () => { assignDo.disabled = !displaySel.value; };

        assignDo.onclick = async () => {
            const clientId = clientSel.value;
            const displayIdx = parseInt(displaySel.value);
            if (!clientId || isNaN(displayIdx)) return;
            assignDo.disabled = true;
            assignDo.textContent = 'Assigning...';
            statusDiv.style.display = 'none';
            try {
                await API.assignClientDisplayLayout(clientId, displayIdx, layoutId);
                statusDiv.textContent = '✓ Layout assigned successfully';
                statusDiv.style.color = '#3fb950';
                statusDiv.style.background = 'rgba(63,185,80,0.1)';
                statusDiv.style.display = 'block';
                assignDo.textContent = 'Assigned!';
                setTimeout(close, 1500);
            } catch(e) {
                statusDiv.textContent = '✗ ' + e.message;
                statusDiv.style.color = '#f85149';
                statusDiv.style.background = 'rgba(248,81,73,0.1)';
                statusDiv.style.display = 'block';
                assignDo.disabled = false;
                assignDo.textContent = 'Assign';
            }
        };
    },

    async openScheduleModal(scheduleId, allSchedules, mode = 'layout', defaultAction = null) {
        // mode: 'layout' (with display+layout selectors) or 'task' (with a task selector)
        // Layout schedules target a registered client display. The server's
        // /api/displays endpoint describes the server host, so using it here
        // made the form show stale values such as Virtual-1.
        const [clients, layouts] = await Promise.all([API.getClients(), API.getLayouts()]);

        const existing = scheduleId ? allSchedules.find(s => s.id === scheduleId) : null;

        const layoutOptions = layouts.map(l =>
            `<option value="${esc(l.id)}" ${existing && existing.layout_id === l.id ? 'selected' : ''}>${esc(l.name)}</option>`
        ).join('');

        // Task selector is a placeholder for now — empty select, will be populated later
        const taskOptions = '<option value="">— Select a task (coming soon) —</option>';

        // Build the target-pickers section based on mode
        let targetSection = '';
        let modalTitle = 'New Layout Schedule';
        if (mode === 'task') {
            modalTitle = 'New Task Schedule';
            targetSection = `
                    <div class="prop-group">
                        <label>Task</label>
                        <select id="sched-task" class="select">${taskOptions}</select>
                        <div style="font-size:0.75rem; color:var(--text-dim); margin-top:0.25rem;">
                            Tasks are not yet defined. This schedule will be saved as a task placeholder.
                        </div>
                    </div>`;
        } else if (mode === 'power') {
            // Display Power Schedule: client → display → action → method → warning
            modalTitle = existing ? 'Edit Display Power Schedule' : 'New Display Power Schedule';
            const existingAction = defaultAction || (existing ? existing.action : 'power_on');
            const existingMethod = existing ? (existing.control_method || 'auto') : 'auto';
            const existingClientId = existing ? (existing.target_client || '') : '';
            const existingDisplayIdx = existing ? (existing.display_index !== undefined && existing.display_index !== null ? String(existing.display_index) : '0') : '0';
            const existingPowerState = existing ? (existing.power_state || 'on') : 'on';
            const isDdcWake = existingMethod === 'ddc';
            targetSection = `
                    <div class="prop-group">
                        <label>Client</label>
                        <select id="sched-power-client" class="select">
                            <option value="">— Select a client —</option>
                        </select>
                        <div id="sched-client-loader" style="display:none; font-size:0.75rem; color:var(--text-dim); margin-top:0.25rem;">Loading displays...</div>
                    </div>
                    <div class="prop-group">
                        <label>Display</label>
                        <select id="sched-power-display" class="select" disabled>
                            <option value="">— Select a display —</option>
                        </select>
                        <div style="font-size:0.75rem; color:var(--text-dim); margin-top:0.25rem;" id="sched-display-hint">Choose a client first</div>
                    </div>
                    <div class="prop-group">
                        <label>Power Action</label>
                        <select id="sched-power-action" class="select">
                            <option value="power_on" ${existingAction === 'power_on' ? 'selected' : ''}>Power On</option>
                            <option value="display_sleep" ${existingAction === 'display_sleep' ? 'selected' : ''}>Display Sleep</option>
                            <option value="power_off" ${existingAction === 'power_off' ? 'selected' : ''}>Power Off</option>
                        </select>
                    </div>
                    <div class="prop-group">
                        <label>Control Method</label>
                        <select id="sched-power-method" class="select">
                            <option value="auto" ${existingMethod === 'auto' ? 'selected' : ''}>Auto (CEC preferred)</option>
                            <option value="cec" ${existingMethod === 'cec' ? 'selected' : ''}>HDMI-CEC</option>
                            <option value="ddc" ${existingMethod === 'ddc' ? 'selected' : ''}>DDC/CI</option>
                            <option value="ir" ${existingMethod === 'ir' ? 'selected' : ''}>IR (coming later)</option>
                            <option value="none" ${existingMethod === 'none' ? 'selected' : ''}>None (disabled)</option>
                        </select>
                        <div id="sched-method-warning" style="display:${isDdcWake ? 'block' : 'none'}; font-size:0.75rem; color:#f59e0b; background:rgba(245,158,11,0.1); border:1px solid rgba(245,158,11,0.3); border-radius:4px; padding:0.5rem; margin-top:0.25rem;">
                            ⚠️ DDC/CI cannot wake a fully-powered-off display. Use "Power On" with CEC or Auto to cold-boot the display.
                        </div>
                    </div>
                    <div id="sched-power-state-preview" style="font-size:0.85rem; color:var(--text-dim); padding:0.5rem; background:var(--bg-elev); border-radius:4px; text-align:center;">
                        Will send: <strong id="sched-cmd-preview">—</strong> via <strong id="sched-method-preview">—</strong>
                    </div>`;
        } else {
            modalTitle = existing ? 'Edit Layout Schedule' : 'New Layout Schedule';
            targetSection = `
                    <div class="prop-group">
                        <label>Client Device</label>
                        <select id="sched-layout-client" class="select">
                            <option value="">— Select a client device —</option>
                        </select>
                        <div style="font-size:0.75rem; color:var(--text-dim); margin-top:0.25rem;">Choose the physical client that should receive this layout.</div>
                    </div>
                    <div class="prop-group">
                        <label>Client Display</label>
                        <select id="sched-layout-display" class="select" disabled>
                            <option value="">— Select a client first —</option>
                        </select>
                        <div id="sched-layout-display-hint" style="font-size:0.75rem; color:var(--text-dim); margin-top:0.25rem;">Choose a client device first</div>
                    </div>
                    <div class="prop-group">
                        <label>Layout</label>
                        <select id="sched-layout" class="select">${layoutOptions}</select>
                    </div>`;
        }

        // Day-of-week multi-selector with quick presets.
        // The day_of_week field is a bitmask: bit 0 = Sunday ... bit 6 = Saturday.
        // -1 = Every Day, 62 = Mon-Fri, 65 = Sat+Sun, 0 = None.
        const dayShortNames = ['Sun', 'Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat'];
        const dayLongNames = ['Sunday', 'Monday', 'Tuesday', 'Wednesday', 'Thursday', 'Friday', 'Saturday'];
        const initialMask = existing ? existing.day_of_week : -1;

        const overlay = document.createElement('div');
        overlay.className = 'fb-overlay';
        overlay.innerHTML = `
            <div class="fb-modal" style="width:480px">
                <div class="fb-header">
                    <h3>${modalTitle}</h3>
                    <button class="fb-close">&times;</button>
                </div>
                <div style="padding:1.5rem; display:flex; flex-direction:column; gap:1rem;">
${targetSection}
                    <div class="prop-group">
                        <label>Active Days</label>
                        <div id="sched-day-summary" style="font-size:0.8rem; color:var(--text-dim); margin-bottom:0.5rem;">
                            Select which days this schedule is active
                        </div>
                        <div id="sched-day-grid" style="display:grid; grid-template-columns:repeat(7, 1fr); gap:0.25rem; margin-bottom:0.5rem;">
                            ${dayShortNames.map((n, i) => `
                                <button type="button" class="btn btn-secondary btn-sm sched-day-btn" data-day="${i}" style="padding:0.5rem 0.25rem; font-size:0.85rem;">
                                    ${n}
                                </button>
                            `).join('')}
                        </div>
                        <div style="display:flex; gap:0.25rem; flex-wrap:wrap;">
                            <button type="button" class="btn btn-accent btn-sm sched-preset-btn" data-mask="-1" style="flex:1; padding:0.4rem 0.5rem; font-size:0.8rem;">Every Day</button>
                            <button type="button" class="btn btn-accent btn-sm sched-preset-btn" data-mask="62" style="flex:1; padding:0.4rem 0.5rem; font-size:0.8rem;">Business Days (Mon-Fri)</button>
                            <button type="button" class="btn btn-accent btn-sm sched-preset-btn" data-mask="65" style="flex:1; padding:0.4rem 0.5rem; font-size:0.8rem;">Weekends (Sat-Sun)</button>
                            <button type="button" class="btn btn-secondary btn-sm sched-preset-btn" data-mask="0" style="flex:1; padding:0.4rem 0.5rem; font-size:0.8rem;">Clear</button>
                        </div>
                        <input type="hidden" id="sched-day-mask" value="${initialMask}">
                    </div>
                    <div class="prop-row">
                        <div class="prop-group">
                            <label>Start Time</label>
                            <input type="time" id="sched-start" class="text-input" value="${existing ? existing.start_time : '09:00'}">
                        </div>
                        <div class="prop-group">
                            <label>End Time</label>
                            <input type="time" id="sched-end" class="text-input" value="${existing ? existing.end_time : '17:00'}">
                        </div>
                    </div>
                    <div style="display:flex;gap:.5rem;">
                        <button class="btn btn-success btn-sm" id="sched-save" style="flex:1">${existing ? 'Update' : 'Create'} Schedule</button>
                        <button class="btn btn-secondary btn-sm" id="sched-cancel" style="flex:1">Cancel</button>
                    </div>
                </div>
            </div>
        `;
        document.body.appendChild(overlay);

        const close = () => overlay.remove();
        overlay.querySelector('.fb-close').onclick = close;
        overlay.querySelector('#sched-cancel').onclick = close;
        overlay.onclick = (e) => { if (e.target === overlay) close(); };

        // Day-of-week multi-selector state & logic
        // Replicates the Rust helpers: DAY_EVERY=-1, DAY_WEEKDAYS=62, DAY_WEEKENDS=65, DAY_NONE=0
        const DAY_EVERY = -1;
        const DAY_WEEKDAYS = 62;  // 0b0111110 (Mon-Fri; bit 0 is Sunday)
        const DAY_WEEKENDS = 65;  // 0b1000001 (Sat=bit6, Sun=bit0)
        const DAY_NONE = 0;
        let currentMask = initialMask;

        function maskContains(mask, day) {
            if (mask === DAY_EVERY) return true;
            if (day < 0 || day > 6) return false;
            return (mask & (1 << day)) !== 0;
        }

        function maskToString(mask) {
            if (mask === DAY_EVERY) return 'Every Day';
            if (mask === DAY_WEEKDAYS) return 'Mon-Fri (Business Days)';
            if (mask === DAY_WEEKENDS) return 'Sat-Sun (Weekends)';
            if (mask === DAY_NONE) return 'Never';
            const days = [];
            for (let i = 0; i < 7; i++) {
                if ((mask & (1 << i)) !== 0) days.push(dayLongNames[i]);
            }
            return days.length ? days.join(', ') : 'Never';
        }

        function updateDayUI() {
            // Update each day button's pressed state
            overlay.querySelectorAll('.sched-day-btn').forEach(btn => {
                const day = parseInt(btn.dataset.day);
                const active = maskContains(currentMask, day);
                btn.classList.toggle('btn-primary', active);
                btn.classList.toggle('btn-secondary', !active);
                btn.style.opacity = active ? '1' : '0.5';
            });
            // Update summary
            const summary = overlay.querySelector('#sched-day-summary');
            summary.textContent = maskToString(currentMask);
            // Update hidden input
            overlay.querySelector('#sched-day-mask').value = currentMask;
        }

        // Wire up day buttons
        overlay.querySelectorAll('.sched-day-btn').forEach(btn => {
            btn.onclick = () => {
                const day = parseInt(btn.dataset.day);
                // If currently "Every Day", clicking a day switches to that single day
                if (currentMask === DAY_EVERY) {
                    currentMask = (1 << day);
                } else {
                    // Toggle the bit
                    if ((currentMask & (1 << day)) !== 0) {
                        currentMask &= ~(1 << day);
                    } else {
                        currentMask |= (1 << day);
                    }
                }
                updateDayUI();
            };
        });

        // Wire up preset buttons
        overlay.querySelectorAll('.sched-preset-btn').forEach(btn => {
            btn.onclick = () => {
                currentMask = parseInt(btn.dataset.mask);
                updateDayUI();
            };
        });

        // Initial UI state
        updateDayUI();

        // For layout schedules, populate the client device and then the
        // displays reported by that client. Keep offline clients selectable so
        // an operator can schedule a device that is temporarily unavailable.
        if (mode === 'layout') {
            const clientSel = overlay.querySelector('#sched-layout-client');
            const displaySel = overlay.querySelector('#sched-layout-display');
            const displayHint = overlay.querySelector('#sched-layout-display-hint');
            const existingClientId = existing ? (existing.target_client || '') : '';
            const existingDisplayIndex = existing && existing.display_index !== undefined && existing.display_index !== null
                ? String(existing.display_index) : '';
            const sortedClients = (clients || []).slice().sort((a, b) => {
                const online = value => ['online', 'connected'].includes(String(value || '').toLowerCase()) ? 0 : 1;
                return online(a.status) - online(b.status) || String(a.name || a.id).localeCompare(String(b.name || b.id));
            });
            clientSel.innerHTML = '<option value="">— Select a client device —</option>' +
                sortedClients.map(c => {
                    const status = c.status || 'unknown';
                    return `<option value="${esc(c.id)}" ${c.id === existingClientId ? 'selected' : ''}>${esc(c.name || c.id)} (${esc(status)})</option>`;
                }).join('');

            async function loadDisplaysForLayoutClient(clientId, selectedIndex) {
                if (!clientId) {
                    displaySel.innerHTML = '<option value="">— Select a client first —</option>';
                    displaySel.disabled = true;
                    displayHint.textContent = 'Choose a client device first';
                    return;
                }
                displaySel.innerHTML = '<option value="">Loading displays…</option>';
                displaySel.disabled = true;
                displayHint.textContent = 'Loading displays…';
                try {
                    const clientDisplays = await API.getClientDisplays(clientId);
                    const selected = selectedIndex !== undefined && selectedIndex !== null ? String(selectedIndex) : '';
                    displaySel.innerHTML = '<option value="">— Select a display —</option>' +
                        clientDisplays.map((d, i) => {
                            const index = d.display_index !== undefined && d.display_index !== null ? d.display_index : i;
                            const label = d.connector || d.label || ('Display ' + index);
                            const status = d.status ? ` (${d.status})` : '';
                            return `<option value="${esc(String(index))}" data-connector="${esc(label)}" ${String(index) === selected ? 'selected' : ''}>${esc(label)}${esc(status)}</option>`;
                        }).join('');
                    displaySel.disabled = false;
                    displayHint.textContent = clientDisplays.length ? `${clientDisplays.length} display(s) found` : 'No displays registered on this client';
                } catch (e) {
                    displaySel.innerHTML = '<option value="">— Displays unavailable —</option>';
                    displaySel.disabled = true;
                    displayHint.textContent = 'Could not load client displays: ' + e.message;
                }
            }

            clientSel.onchange = () => loadDisplaysForLayoutClient(clientSel.value);
            if (existingClientId) {
                await loadDisplaysForLayoutClient(existingClientId, existingDisplayIndex);
            }
        }

        // For power schedules, load client displays when client changes
        if (mode === 'power') {
            const clientSel = overlay.querySelector('#sched-power-client');
            const displaySel = overlay.querySelector('#sched-power-display');
            const hint = overlay.querySelector('#sched-display-hint');
            const methodSel = overlay.querySelector('#sched-power-method');
            const warning = overlay.querySelector('#sched-method-warning');
            const cmdPreview = overlay.querySelector('#sched-cmd-preview');
            const methodPreview = overlay.querySelector('#sched-method-preview');
            const actionSel = overlay.querySelector('#sched-power-action');

            // Load clients
            const clients = await API.getClients();
            const connectedClients = clients.filter(c => c.status === 'connected' || c.status === 'online');
            const existingClientId = existing ? (existing.target_client || '') : '';
            clientSel.innerHTML = '<option value="">— Select a client —</option>' +
                connectedClients.map(c =>
                    `<option value="${esc(c.id)}" ${c.id === existingClientId ? 'selected' : ''}>${esc(c.name || c.id)} (${esc(c.status || 'unknown')})</option>`
                ).join('');

            async function loadDisplaysForClient(clientId, selectedIdx) {
                if (!clientId) {
                    displaySel.innerHTML = '<option value="">— Select a display —</option>';
                    displaySel.disabled = true;
                    hint.textContent = 'Choose a client first';
                    return;
                }
                hint.textContent = 'Loading displays...';
                try {
                    const displays = await API.getClientDisplays(clientId);
                    const idx = selectedIdx !== undefined ? String(selectedIdx) : '0';
                    displaySel.innerHTML = '<option value="">— Select a display —</option>' +
                        displays.map((d, i) =>
                            `<option value="${i}" ${String(i) === idx ? 'selected' : ''}>${esc(d.connector || d.label || ('Display ' + i))}</option>`
                        ).join('');
                    displaySel.disabled = false;
                    hint.textContent = displays.length ? `${displays.length} display(s) found` : 'No displays detected on this client';
                } catch(e) {
                    hint.textContent = 'Could not load displays: ' + e.message;
                }
            }

            clientSel.onchange = () => loadDisplaysForClient(clientSel.value);
            actionSel.onchange = updatePreview;
            methodSel.onchange = () => {
                const isDdcWake = methodSel.value === 'ddc' && actionSel.value === 'power_on';
                warning.style.display = isDdcWake ? 'block' : 'none';
                updatePreview();
            };

            function updatePreview() {
                const method = methodSel.value;
                const action = actionSel.value;
                const labels = { auto: 'Auto', cec: 'HDMI-CEC', ddc: 'DDC/CI', ir: 'IR', none: 'None' };
                let cmd;
                if (action === 'power_on') {
                    cmd = (method === 'cec' || method === 'auto') ? 'cec pow 0' : 'ddcutil VCP 6=1';
                } else if (action === 'display_sleep') {
                    cmd = 'ddcutil VCP 6=4';
                } else {
                    // power_off
                    cmd = (method === 'cec' || method === 'auto') ? 'cec standby 0' : 'ddcutil VCP 6=4';
                }
                cmdPreview.textContent = cmd;
                methodPreview.textContent = labels[method] || method;
            }
            updatePreview();

            // If editing existing, load its displays
            if (existingClientId) {
                const existingDisplayIdx = existing ? (existing.display_index !== undefined && existing.display_index !== null ? String(existing.display_index) : '0') : '0';
                await loadDisplaysForClient(existingClientId, parseInt(existingDisplayIdx));
            }
        }

        overlay.querySelector('#sched-save').onclick = async () => {
            const day_of_week = parseInt(overlay.querySelector('#sched-day-mask').value);
            const start_time = overlay.querySelector('#sched-start').value;
            const end_time = overlay.querySelector('#sched-end').value;

            if (!start_time || !end_time) {
                alert('Please specify start and end times.');
                return;
            }

            // In task mode, the task selector is empty/placeholder so we use a placeholder
            // task_id. The backend doesn't have a "task" concept yet; we still create a
            // schedule row with a placeholder display/layout so the schedule can be tracked.
            // Future work: add a `tasks` table and proper task_id foreign key.
            let display, layout_id, action, target_client, power_state, control_method, display_index;
            if (mode === 'power') {
                target_client = overlay.querySelector('#sched-power-client').value;
                const displayIdx = overlay.querySelector('#sched-power-display').value;
                action = overlay.querySelector('#sched-power-action').value;
                control_method = overlay.querySelector('#sched-power-method').value;
                display_index = displayIdx !== '' ? parseInt(displayIdx) : 0;
                power_state = action === 'power_on' ? 'on' : action === 'display_sleep' ? 'sleep' : 'off';
                display = '';  // power schedules use target_client instead
                layout_id = '';
                if (!target_client) {
                    alert('Please select a client.');
                    return;
                }
            } else if (mode === 'task') {
                // For now, task schedules use empty string placeholders. The day-of-week
                // and time fields are the meaningful parts; the backend treats them as
                // opaque strings.
                display = '';
                layout_id = '';
                action = 'task';
                target_client = '';
                power_state = null;
                control_method = null;
                display_index = null;
            } else {
                const clientSel = overlay.querySelector('#sched-layout-client');
                const displaySel = overlay.querySelector('#sched-layout-display');
                target_client = clientSel.value;
                const displayIndexValue = displaySel.value;
                layout_id = overlay.querySelector('#sched-layout').value;
                const selectedDisplay = displaySel.options[displaySel.selectedIndex];
                const connector = selectedDisplay ? selectedDisplay.dataset.connector : '';
                display_index = displayIndexValue !== '' ? parseInt(displayIndexValue, 10) : null;
                display = target_client && connector ? `${target_client}:${connector}` : '';
                if (!target_client || display_index === null || Number.isNaN(display_index) || !layout_id) {
                    alert('Please select a client device, client display, and layout.');
                    return;
                }
                action = 'switch_layout';
                power_state = null;
                control_method = null;
            }

            const schedule = {
                id: existing ? existing.id : '',
                display,
                layout_id,
                day_of_week,
                start_time,
                end_time,
                created_at: existing ? existing.created_at : '',
                action,
                target_client,
                power_state,
                control_method,
                display_index,
                display_name: mode === 'layout' ? (overlay.querySelector('#sched-layout-display option:checked')?.dataset.connector || '') : null,
            };

            try {
                if (existing) {
                    await API.updateSchedule(existing.id, schedule);
                } else {
                    await API.createSchedule(schedule);
                }
                close();
                await this.loadSchedulesPage();
            } catch (e) {
                alert('Failed to save schedule: ' + e.message);
            }
        };
    },
    async handleDeleteSchedule(id) {
        if (!(await App.confirm('Delete this schedule?', 'This will remove the schedule. This cannot be undone.'))) return;
        try {
            await API.deleteSchedule(id);
            await this.loadSchedulesPage();
        } catch (e) {
            alert('Failed to delete schedule: ' + e.message);
        }
    },


    // ── Widget Template Editor ──
    currentTemplateId: null,
    currentTemplateAssets: [],
    removedTemplateAssetIds: [],


    widgetTypeIcon(t) {
        const icons = { text: '📝', variable_text: '🔠', variable_icon: '🌤️', variable: '🌤️', date: '🔠', image: '🖼️', video: '🎬', clock: '🕐', datetime: '📅', calendar: '📆', weather: '🌤️', webpage: '🌐', music: '🎵', rectangle: '⬛' };
        return icons[t] || '📦';
    },

    escapeHtml(s) {
        if (s == null) return '';
        return String(s).replace(/[&<>"']/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
    },

    // ── Page-based Widget Template Editor ──
    // The Widget Templates page IS the editor. The sidebar lists templates,
    // the main area shows the block editor + raw JSON for the selected template.
    _tplSidebarCache: null,
    _tplSidebarCacheAt: 0,
    _tplSidebarFilterType: '',
    _tplSidebarSearch: '',
    _tplPageWired: false,

    // ── Interactive Data Collection ─────────────────────────────
    _interactiveDataState: { submissions: [], stats: null, selected: new Set(), lastGoogleConfigured: null },
    async loadInteractiveDataPage() {
      // Fetch submissions + stats in parallel
      const [list, stats] = await Promise.all([
        API.getInteractiveSubmissions({ limit: 500 }),
        API.getInteractiveSubmissionsStats(),
      ]);
      this._interactiveDataState.submissions = list.submissions || [];
      this._interactiveDataState.stats = stats;
      this.renderInteractiveData();
      this.populateInteractiveDataFilters();
      this.wireInteractiveDataButtons();
    },
    renderInteractiveData() {
      const subs = this._interactiveDataState.submissions;
      const stats = this._interactiveDataState.stats || {};
      const $ = (id) => document.getElementById(id);
      if ($('interactive-data-stat-total')) $('interactive-data-stat-total').textContent = stats.total ?? 0;
      if ($('interactive-data-stat-pending')) $('interactive-data-stat-pending').textContent = stats.pending ?? 0;
      if ($('interactive-data-stat-success')) $('interactive-data-stat-success').textContent = stats.success ?? 0;
      if ($('interactive-data-stat-failed')) $('interactive-data-stat-failed').textContent = (stats.failed ?? 0) + (this._interactiveDataState.lastGoogleConfigured === false ? 0 : 0);
      // failed counts include 'skipped' implicitly via the failed stat — but we want a clear "skipped" view too.
      // We'll merge skipped into the failed pill display for visibility, but display them separately in the table.

      const tbody = $('interactive-data-tbody');
      if (!tbody) return;
      if (!subs.length) {
        tbody.innerHTML = '<tr><td colspan="9" class="interactive-data-empty">No submissions yet. Interactive widgets (textbox, radio, dropdown, etc.) will appear here when users interact with them.</td></tr>';
        return;
      }
      const rows = subs.map(s => {
        const status = s.google_sync_status || 'pending';
        const statusBadge = {
          pending:  '<span class="interactive-data-badge pending">🟡 Pending</span>',
          success:  '<span class="interactive-data-badge success">🟢 Synced</span>',
          failed:   '<span class="interactive-data-badge failed">🔴 Failed</span>',
          skipped:  '<span class="interactive-data-badge skipped">⚪ Skipped</span>',
        }[status] || `<span class="interactive-data-badge">${esc(status)}</span>`;
        let payloadPretty = '';
        try {
          const obj = JSON.parse(s.payload_json || '{}');
          if (obj !== null && typeof obj === 'object' && !Array.isArray(obj)) {
            payloadPretty = Object.entries(obj).map(([k,v]) => `${esc(k)}: ${esc(Array.isArray(v) ? v.join(', ') : String(v))}`).join(' · ');
          } else {
            payloadPretty = esc(Array.isArray(obj) ? obj.join(', ') : String(obj));
          }
        } catch { payloadPretty = esc(s.payload_json || ''); }
        const checked = this._interactiveDataState.selected.has(s.id) ? 'checked' : '';
        const syncMsg = s.google_sync_message ? esc(s.google_sync_message) : '';
        const syncMsgCell = syncMsg ? `<div class="interactive-data-sync-msg">${syncMsg}</div>` : '';
        const collectedAt = s.collected_at ? esc(new Date(s.collected_at).toLocaleString()) : '—';
        const syncedAt = s.google_synced_at ? esc(new Date(s.google_synced_at).toLocaleString()) : '—';
        const widgetLabel = s.widget_label ? esc(s.widget_label) : '';
        return `<tr data-id="${esc(s.id)}">
          <td><input type="checkbox" class="interactive-data-row-check" data-id="${esc(s.id)}" ${checked}></td>
          <td>${collectedAt}</td>
          <td>${esc(s.client_id || '—')}</td>
          <td>${esc(s.interactive_layout_name || s.interactive_layout_id || '—')}</td>
          <td><span class="interactive-data-widget-type">${esc(s.widget_type || '')}</span>${widgetLabel ? '<div class="interactive-data-widget-label">' + widgetLabel + '</div>' : ''}</td>
          <td class="interactive-data-value-cell">${payloadPretty}</td>
          <td>${statusBadge}${syncMsgCell}</td>
          <td>${syncedAt}</td>
          <td><button class="btn btn-secondary btn-sm interactive-data-sync-one" data-id="${esc(s.id)}">☁ Sync</button> <button class="btn btn-ghost btn-sm interactive-data-delete-one" data-id="${esc(s.id)}">🗑</button></td>
        </tr>`;
      }).join('');
      tbody.innerHTML = rows;
      this.wireRowCheckboxes();
      this.wireRowButtons();
      this.updateBulkActionBar();
    },
    populateInteractiveDataFilters() {
      const subs = this._interactiveDataState.submissions;
      const clients = new Set();
      const pages = new Set();
      for (const s of subs) {
        if (s.client_id) clients.add(s.client_id);
        if (s.interactive_layout_id) pages.add(s.interactive_layout_id + '|' + (s.interactive_layout_name || ''));
      }
      const clientSel = document.getElementById('interactive-data-filter-client');
      const pageSel = document.getElementById('interactive-data-filter-page');
      if (clientSel) {
        const cur = clientSel.value;
        clientSel.innerHTML = '<option value="">All clients</option>' +
          Array.from(clients).sort().map(c => `<option value="${esc(c)}">${esc(c)}</option>`).join('');
        clientSel.value = cur;
      }
      if (pageSel) {
        const cur = pageSel.value;
        pageSel.innerHTML = '<option value="">All pages</option>' +
          Array.from(pages).sort().map(p => {
            const [id, name] = p.split('|');
            return `<option value="${esc(id)}">${esc(name || id)}</option>`;
          }).join('');
        pageSel.value = cur;
      }
    },
    wireInteractiveDataButtons() {
      const $ = (id) => document.getElementById(id);
      const refresh = $('btn-interactive-data-refresh');
      if (refresh && refresh.dataset.wired !== '1') {
        refresh.dataset.wired = '1';
        refresh.onclick = () => this.loadInteractiveDataPage();
      }
      const exportBtn = $('btn-interactive-data-export');
      if (exportBtn && exportBtn.dataset.wired !== '1') {
        exportBtn.dataset.wired = '1';
        exportBtn.onclick = async () => {
          const params = this.collectInteractiveDataFilterParams();
          const url = await API.getInteractiveSubmissionsCsvUrl(params);
          window.open(url, '_blank');
        };
      }
      const syncBtn = $('btn-interactive-data-sync');
      if (syncBtn && syncBtn.dataset.wired !== '1') {
        syncBtn.dataset.wired = '1';
        syncBtn.onclick = () => this.runInteractiveDataSync(null);
      }
      const applyBtn = $('btn-interactive-data-apply-filters');
      if (applyBtn && applyBtn.dataset.wired !== '1') {
        applyBtn.dataset.wired = '1';
        applyBtn.onclick = async () => {
          const params = this.collectInteractiveDataFilterParams();
          const list = await API.getInteractiveSubmissions({ ...params, limit: 500 });
          this._interactiveDataState.submissions = list.submissions || [];
          this.renderInteractiveData();
          this.populateInteractiveDataFilters();
        };
      }
      const clearBtn = $('btn-interactive-data-clear-filters');
      if (clearBtn && clearBtn.dataset.wired !== '1') {
        clearBtn.dataset.wired = '1';
        clearBtn.onclick = () => {
          const c = document.getElementById('interactive-data-filter-client'); if (c) c.value = '';
          const p = document.getElementById('interactive-data-filter-page'); if (p) p.value = '';
          const s = document.getElementById('interactive-data-filter-status'); if (s) s.value = '';
          if (applyBtn) applyBtn.click();
        };
      }
      const selectAll = $('interactive-data-select-all');
      if (selectAll && selectAll.dataset.wired !== '1') {
        selectAll.dataset.wired = '1';
        selectAll.onchange = (e) => {
          const checked = !!e.target.checked;
          this._interactiveDataState.selected.clear();
          if (checked) {
            for (const s of this._interactiveDataState.submissions) {
              this._interactiveDataState.selected.add(s.id);
            }
          }
          this.renderInteractiveData();
        };
      }
      const syncSel = $('btn-interactive-data-sync-selected');
      if (syncSel && syncSel.dataset.wired !== '1') {
        syncSel.dataset.wired = '1';
        syncSel.onclick = () => this.runInteractiveDataSync(Array.from(this._interactiveDataState.selected));
      }
      const delSel = $('btn-interactive-data-delete-selected');
      if (delSel && delSel.dataset.wired !== '1') {
        delSel.dataset.wired = '1';
        delSel.onclick = () => this.runInteractiveDataDelete(Array.from(this._interactiveDataState.selected));
      }
      const openG = $('interactive-data-open-google-settings');
      if (openG && openG.dataset.wired !== '1') {
        openG.dataset.wired = '1';
        openG.onclick = (e) => {
          e.preventDefault();
          document.querySelector('.nav-link[data-page="system"]')?.click();
        };
      }
    },
    collectInteractiveDataFilterParams() {
      const params = {};
      const c = document.getElementById('interactive-data-filter-client');
      const p = document.getElementById('interactive-data-filter-page');
      const s = document.getElementById('interactive-data-filter-status');
      if (c && c.value) params.client_id = c.value;
      if (p && p.value) params.page_id = p.value;
      if (s && s.value) params.status = s.value;
      return params;
    },
    wireRowCheckboxes() {
      document.querySelectorAll('.interactive-data-row-check').forEach(cb => {
        cb.onchange = (e) => {
          const id = parseInt(e.target.dataset.id, 10);
          if (e.target.checked) this._interactiveDataState.selected.add(id);
          else this._interactiveDataState.selected.delete(id);
          this.updateBulkActionBar();
        };
      });
    },
    wireRowButtons() {
      document.querySelectorAll('.interactive-data-sync-one').forEach(btn => {
        btn.onclick = () => this.runInteractiveDataSync([parseInt(btn.dataset.id, 10)]);
      });
      document.querySelectorAll('.interactive-data-delete-one').forEach(btn => {
        btn.onclick = () => this.runInteractiveDataDelete([parseInt(btn.dataset.id, 10)]);
      });
    },
    updateBulkActionBar() {
      const bar = document.getElementById('interactive-data-bulk-actions');
      const count = document.getElementById('interactive-data-selected-count');
      if (!bar || !count) return;
      const n = this._interactiveDataState.selected.size;
      if (n > 0) {
        bar.style.display = 'flex';
        count.textContent = String(n);
      } else {
        bar.style.display = 'none';
      }
    },
    async runInteractiveDataSync(ids) {
      const btn = document.getElementById('btn-interactive-data-sync');
      const oldText = btn ? btn.textContent : '';
      if (btn) { btn.disabled = true; btn.textContent = '⏳ Syncing…'; }
      try {
        const payload = ids && ids.length ? { action: 'sync_specific', ids } : { action: 'sync_all_pending' };
        const r = await API.syncInteractiveSubmissionsToGoogle(payload);
        this._interactiveDataState.lastGoogleConfigured = !!r.google_configured;
        this.showInteractiveDataStatus(r.message || (r.synced + ' submissions processed.'), r.google_configured === false ? 'warn' : 'ok');
        // Show banner if Google isn't configured
        const banner = document.getElementById('interactive-data-google-banner');
        if (banner) banner.style.display = (r.google_configured === false) ? '' : 'none';
        await this.loadInteractiveDataPage();
      } catch (e) {
        this.showInteractiveDataStatus('Sync failed: ' + e.message, 'err');
      } finally {
        if (btn) { btn.disabled = false; btn.textContent = oldText; }
      }
    },
    async runInteractiveDataDelete(ids) {
      if (!ids || !ids.length) return;
      if (!(await App.confirm('Delete ' + ids.length + ' submission(s)?', 'This cannot be undone.'))) return;
      try {
        await API.deleteInteractiveSubmissions(ids);
        for (const id of ids) this._interactiveDataState.selected.delete(id);
        await this.loadInteractiveDataPage();
      } catch (e) {
        alert('Delete failed: ' + e.message);
      }
    },
    showInteractiveDataStatus(msg, kind) {
      const banner = document.getElementById('interactive-data-google-banner');
      if (!banner) return;
      if (kind === 'ok' || kind === 'warn') {
        banner.classList.remove('interactive-data-banner-err');
        banner.classList.add('interactive-data-banner-ok');
        banner.style.display = '';
        banner.innerHTML = `<strong>${kind === 'ok' ? '✓' : '⚠'}</strong> <span>${esc(msg)}</span>`;
        setTimeout(() => { banner.style.display = 'none'; }, 5000);
      } else {
        banner.classList.remove('interactive-data-banner-ok');
        banner.classList.add('interactive-data-banner-err');
        banner.style.display = '';
        banner.innerHTML = `<strong>✗</strong> <span>${esc(msg)}</span>`;
      }
    },

    async loadWidgetTemplatesPage() {
        // Render the sidebar list of templates
        await this._renderTemplateSidebar();
        // If no template is currently selected, show the empty state
        if (!this.currentTemplateId) {
            this._showEmptyTemplateEditor();
        } else {
            this._showTemplateEditorForm();
        }
        // Wire page-level controls (idempotent)
        this._wireTemplatePageControls();
        this._wireTemplatePropertySections();
    },

    _showEmptyTemplateEditor() {
        const empty = document.getElementById('tpl-page-empty');
        const form = document.getElementById('tpl-page-form');
        if (empty) empty.style.display = 'flex';
        if (form) form.style.display = 'none';
    },

    _showTemplateEditorForm() {
        const empty = document.getElementById('tpl-page-empty');
        const form = document.getElementById('tpl-page-form');
        if (empty) empty.style.display = 'none';
        if (form) form.style.display = 'flex';
        this._wireTemplatePropertySections();
    },

    _wireTemplatePropertySections() {
        document.querySelectorAll('#tpl-page-block-editor .tpl-prop-section').forEach(section => {
            if (section.dataset.tplCollapseWired) return;
            section.dataset.tplCollapseWired = '1';
            section.addEventListener('toggle', () => {
                if (!section.open) return;
                document.querySelectorAll('#tpl-page-block-editor .tpl-prop-section[open]').forEach(other => {
                    if (other !== section) other.open = false;
                });
            });
        });
    },

    _wireTemplatePageControls() {
        if (this._tplPageWired) return;
        this._tplPageWired = true;

        // Sidebar search/filter
        const search = document.getElementById('tpl-page-search');
        if (search) {
            search.oninput = () => {
                this._tplSidebarSearch = search.value.toLowerCase();
                this._renderTemplateSidebar();
            };
        }
        const filter = document.getElementById('tpl-page-filter');
        if (filter) {
            filter.onchange = () => {
                this._tplSidebarFilterType = filter.value;
                this._renderTemplateSidebar();
            };
        }

        // Page header buttons
        const newBtn = document.getElementById('btn-new-template-page');
        if (newBtn) {
            newBtn.onclick = () => this.openTemplateEditor(null);
        }
        const resetBtn = document.getElementById('btn-reset-default-templates');
        if (resetBtn) {
            resetBtn.onclick = async () => {
                if (!confirm('This will DELETE any default templates (those whose ID starts with "default-") and re-create them from the bundled defaults. User-created templates will be preserved.\n\nContinue?')) return;
                try {
                    const result = await API.resetDefaultWidgetPresets();
                    alert('✓ ' + (result.message || 'Default templates reset'));
                    this._tplSidebarCache = null;
                    await this._renderTemplateSidebar();
                } catch (err) { alert('Reset failed: ' + err.message); }
            };
        }
        const importBtn = document.getElementById('btn-import-template');
        const importInput = document.getElementById('import-template-file');
        if (importBtn && importInput) {
            importBtn.onclick = () => importInput.click();
            importInput.onchange = async () => {
                if (!importInput.files || !importInput.files[0]) return;
                try {
                    const text = await importInput.files[0].text();
                    await API.importWidgetPreset(text);
                    importInput.value = '';
                    this._tplSidebarCache = null;
                    await this._renderTemplateSidebar();
                } catch (err) { alert('Import failed: ' + err.message); }
            };
        }

        // Editor header buttons
        const saveBtn = document.getElementById('tpl-page-save');
        if (saveBtn) saveBtn.onclick = () => this.saveCurrentTemplate();
        const dlBtn = document.getElementById('tpl-page-download');
        if (dlBtn) dlBtn.onclick = () => {
            if (this.currentTemplateId) this.downloadTemplate(this.currentTemplateId);
            else alert('Save the template first to download it.');
        };
        const delBtn = document.getElementById('tpl-page-delete');
        if (delBtn) {
            delBtn.style.position = 'relative';
            delBtn.onclick = () => {
                const box = document.getElementById('tpl-delete-confirm');
                if (!box) return;
                if (box.style.display === 'flex') { box.style.display = 'none'; return; }
                box.style.display = 'flex';
                const rect = delBtn.getBoundingClientRect();
                const gap = 6;
                const width = box.offsetWidth || 220;
                const height = box.offsetHeight || 90;
                const left = Math.max(8, Math.min(rect.left, window.innerWidth - width - 8));
                const below = rect.bottom + gap;
                const top = below + height <= window.innerHeight - 8
                    ? below
                    : Math.max(8, rect.top - height - gap);
                box.style.left = left + 'px';
                box.style.top = top + 'px';
            };
            const confirmBtn = document.getElementById('btn-tpl-confirm-delete');
            if (confirmBtn) {
                confirmBtn.onclick = async () => {
                    if (!this.currentTemplateId) return;
                    const box = document.getElementById('tpl-delete-confirm');
                    if (box) box.style.display = 'none';
                    try {
                        await API.deleteWidgetPreset(this.currentTemplateId);
                        this.currentTemplateId = null;
                        this.currentTemplateAssets = [];
                        this.removedTemplateAssetIds = [];
                        this._tplSidebarCache = null;
                        await this._renderTemplateSidebar();
                        this._showEmptyTemplateEditor();
                    } catch (err) { alert('Delete failed: ' + err.message); }
                };
            }
            const cancelBtn = document.getElementById('btn-tpl-cancel-delete');
            if (cancelBtn) {
                cancelBtn.onclick = () => {
                    const box = document.getElementById('tpl-delete-confirm');
                    if (box) box.style.display = 'none';
                };
            }
        };

        // Form inputs (idempotent via dataset.wired)
        this._wireTemplateFormInputs();
        // Visual drag/drop template builder
        this._wireTemplateVisualBuilder();
        // Icon upload
        this._wireIconUpload();
        // Raw JSON controls
        const copyBtn = document.getElementById('tpl-raw-copy');
        if (copyBtn) {
            copyBtn.onclick = () => {
                const raw = document.getElementById('tpl-raw-props');
                if (raw) {
                    raw.select();
                    document.execCommand('copy');
                    const orig = copyBtn.textContent;
                    copyBtn.textContent = '✓ Copied';
                    setTimeout(() => { copyBtn.textContent = orig; }, 1200);
                }
            };
        }
        const formatBtn = document.getElementById('tpl-raw-format');
        if (formatBtn) {
            formatBtn.onclick = () => this._updateRawPropsView();
        }
    
        // ── Element / Builder tab switching ──
        const elemTab  = document.querySelector('[data-tpl-tab="element"]');
        const buildTab = document.querySelector('[data-tpl-tab="template"]');
        const elemSecs = document.getElementById('tpl-element-sections');
        const buildSecs = document.getElementById('tpl-builder-sections');

        function switchToBuilderTab() {
            if (elemTab)  { elemTab.classList.remove('active');  }
            if (buildTab) { buildTab.classList.add('active'); }
            if (elemSecs) elemSecs.style.display = 'none';
            if (buildSecs) buildSecs.style.display = '';
        }
        function switchToElementTab() {
            if (buildTab) { buildTab.classList.remove('active'); }
            if (elemTab)  { elemTab.classList.add('active');  }
            if (buildSecs) buildSecs.style.display = 'none';
            if (elemSecs) elemSecs.style.display = '';
        }
        // Expose globally so _renderTemplateElementProps can call them
        window.switchToElementTab = switchToElementTab;
        window.switchToBuilderTab = switchToBuilderTab;

        if (elemTab)  elemTab.addEventListener('click',  switchToElementTab);
        if (buildTab) buildTab.addEventListener('click', switchToBuilderTab);

        // Initial state: Builder tab active, element sections hidden
        switchToBuilderTab();
},

    _wireIconUpload() {
        const input = document.getElementById('tpl-icon-upload');
        const btn = document.getElementById('tpl-icon-upload-btn');
        const status = document.getElementById('tpl-icon-upload-status');
        if (!input || !btn || input.dataset.wired) return;
        input.dataset.wired = '1';
        btn.onclick = () => input.click();
        input.addEventListener('change', async () => {
            const file = input.files && input.files[0];
            if (!file) return;
            const el = this._selectedTemplateElement();
            if (!el || el.type !== 'variable_icon' || el.props.variable_source !== 'weather') {
                alert('Select a weather variable icon element first, then upload a custom icon for it.');
                input.value = '';
                return;
            }
            const code = el.props.variable_key || '0';
            status.textContent = ' Uploading…';
            try {
                // Step 1: upload the image file
                const fileForm = new FormData();
                fileForm.append('file', file);
                const fileRes = await fetch('/api/files/icons', { method: 'PUT', body: fileForm });
                if (!fileRes.ok) { const txt = await fileRes.text(); throw new Error(txt || 'Upload failed'); }
                const fileEntry = await fileRes.json();
                // Step 2: register as a weather_icon asset on the preset
                const isNightUpload = String(code).endsWith('_night');
                const uploadKind = isNightUpload ? 'weather_icon_night' : 'weather_icon';
                const uploadKey = isNightUpload ? code : code;
                const assetRes = await fetch(`/api/widget-presets/${this.currentTemplateId}/assets`, {
                    method: 'POST',
                    headers: { 'Content-Type': 'application/json' },
                    body: JSON.stringify({ kind: uploadKind, key: uploadKey, url: fileEntry.url }),
                });
                if (!assetRes.ok) { const txt = await assetRes.text(); throw new Error(txt || 'Asset registration failed'); }
                const uploaded = await assetRes.json();
                // Add or replace the weather_icon asset
                // Add or replace both day and night variants
                this.currentTemplateAssets = (this.currentTemplateAssets || []).filter(a => !(a.kind === uploadKind && a.key === uploadKey));
                this.currentTemplateAssets.push({ kind: uploadKind, key: uploadKey, url: uploaded.url, preset_id: this.currentTemplateId || '' });
                status.textContent = ' ✓';
                setTimeout(() => { if (status) status.textContent = ''; }, 2000);
                this._renderTemplateCanvas();
            } catch (err) {
                status.textContent = ' ✗';
                alert('Upload failed: ' + err.message);
                setTimeout(() => { if (status) status.textContent = ''; }, 3000);
            }
            input.value = '';
        });
    },

    _wireTemplateFormInputs() {
        // The schema-driven editor wires its own listeners. We only need to:
        // 1. Wire the template name input
        // 2. Wire the widget-type selector (re-renders schema on change)
        // 3. Wire the raw JSON blur to sync back into the form
        const nameEl = document.getElementById('tpl-name');
        if (nameEl && !nameEl.dataset.wired) {
            nameEl.dataset.wired = '1';
            nameEl.addEventListener('input', () => {
                const lbl = document.getElementById('tpl-page-name-display');
                if (lbl) lbl.textContent = nameEl.value || '(unnamed)';
            });
        }
        const typeEl = document.getElementById('tpl-widget-type');
        if (typeEl && !typeEl.dataset.wired) {
            typeEl.dataset.wired = '1';
            typeEl.addEventListener('change', () => {
                this._loadSchemaForm(typeEl.value);
            });
        }
        // Raw JSON syncs back to the form on blur
        const rawEl = document.getElementById('tpl-raw-props');
        if (rawEl && !rawEl.dataset.blurWired) {
            rawEl.dataset.blurWired = '1';
            rawEl.addEventListener('blur', () => {
                try {
                    const newProps = JSON.parse(rawEl.value || '{}');
                    this._tplElements = Array.isArray(newProps.template_elements) ? newProps.template_elements : [];
                    delete newProps.template_elements;
                    this._tplSelectedElementId = this._tplElements[0]?.id || null;
                    const typeEl = document.getElementById('tpl-widget-type');
                    this._loadSchemaForm(typeEl ? typeEl.value : 'text', newProps);
                    this._renderTemplateCanvas();
                    this._renderTemplateElementProps();
                    this._updateRawPropsView();
                    this.updateTemplatePreview();
                } catch (e) {
                    alert('Invalid JSON: ' + e.message);
                }
            });
        }
    },

    // Render the schema-driven form for a widget type. If currentProps is
    // provided, pre-fills the fields.
    _loadSchemaForm(widgetType, currentProps) {
        const schema = window.getWidgetSchema(widgetType);
        const container = document.getElementById('wtfk-form');
        if (!container) return;
        // Load extras from props
        const extras = window.WidgetTemplateEditor.loadExtrasFromProps(schema, currentProps || {});
        // Render the schema form
        window.WidgetTemplateEditor.renderForm(container, schema, currentProps || {}, (key, val, props) => {
            // After any change, update the raw JSON view and preview
            this._updateRawPropsView();
            this.updateTemplatePreview();
            // Also update extras (in case a watchIf change affected them)
            const newExtras = window.WidgetTemplateEditor.loadExtrasFromProps(schema, props);
            // Preserve any custom row keys the user added
            const existingKeys = new Set(extras.map(e => e.key));
            for (const e of newExtras) {
                if (!existingKeys.has(e.key)) extras.push(e);
            }
        });
        // Store extras for later collection
        this._wtfCurrentExtras = extras;
        // Render the extras section
        const extrasContainer = document.getElementById('wtfk-extras');
        if (extrasContainer) {
            window.WidgetTemplateEditor.renderExtras(extrasContainer, schema, extras);
        }
        // Show/hide the weather icons section based on widget type
        const iconsSection = document.getElementById('wtfk-weather-icons-section');
        if (iconsSection) {
            iconsSection.style.display = (widgetType === 'weather') ? 'block' : 'none';
        }
    },

    async openTemplateEditor(presetId) {
        this.currentTemplateId = presetId;
        this.currentTemplateAssets = [];
        if (presetId) {
            try {
                const preset = await API.getWidgetPreset(presetId);
                this.fillTemplateForm(preset);
                this.currentTemplateAssets = preset.assets || [];
                this.removedTemplateAssetIds = [];
            } catch (err) { alert('Failed to load template: ' + err.message); return; }
        } else {
            this.fillTemplateForm({ name: '', widget_type: 'weather', props_json: '{}', assets: [] });
            this.currentTemplateAssets = [];
        }
        // Update the name display
        const nameIn = document.getElementById('tpl-name');
        const nameLbl = document.getElementById('tpl-page-name-display');
        if (nameLbl) nameLbl.textContent = (nameIn && nameIn.value) || '(unnamed)';
        // Show the form, hide the empty state
        this._showTemplateEditorForm();
        // Update the raw JSON view
        this._updateRawPropsView();
        // Render the live widget preview
        this.updateTemplatePreview();
        // Render weather icons / assets
        this._renderAssetsGrid();
        this._renderTemplateSnapGridProps();
        // Show/hide assets section
        const wtEl = document.getElementById('tpl-widget-type');
        if (wtEl) { const sec = document.getElementById('tpl-assets-section'); if (sec) sec.style.display = (wtEl.value === 'weather') ? 'block' : 'none'; }
        // Re-render the sidebar to highlight the active template
        this._renderTemplateSidebar();
    },

    async _renderTemplateSidebar() {
        const list = document.getElementById('tpl-page-list');
        if (!list) return;
        const now = Date.now();
        if (!this._tplSidebarCache || (now - this._tplSidebarCacheAt) > 5000) {
            try {
                this._tplSidebarCache = await API.listWidgetPresets();
                this._tplSidebarCacheAt = now;
            } catch (e) {
                list.innerHTML = '<p style="color:#e74c3c; padding:0.5rem; font-size:0.85rem;">Failed to load templates</p>';
                return;
            }
        }
        let templates = this._tplSidebarCache;
        if (this._tplSidebarFilterType) {
            templates = templates.filter(t => t.widget_type === this._tplSidebarFilterType);
        }
        if (this._tplSidebarSearch) {
            const q = this._tplSidebarSearch;
            templates = templates.filter(t => t.name.toLowerCase().includes(q) || t.widget_type.toLowerCase().includes(q));
        }
        if (templates.length === 0) {
            list.innerHTML = '<p style="color:#888; padding:0.5rem; font-size:0.85rem;">No templates match.</p>';
            return;
        }
        // Sort: defaults first, then by name
        templates.sort((a, b) => {
            const ad = a.id.startsWith('default-') ? 0 : 1;
            const bd = b.id.startsWith('default-') ? 0 : 1;
            if (ad !== bd) return ad - bd;
            return a.name.localeCompare(b.name);
        });
        list.innerHTML = templates.map(t => {
            const isDefault = t.id.startsWith('default-');
            const isSelected = t.id === this.currentTemplateId;
            return `
                <div class="tpl-sidebar-item" data-id="${this.escapeHtml(t.id)}" style="
                    padding:0.5rem 0.6rem;
                    margin-bottom:0.3rem;
                    border-radius:5px;
                    cursor:pointer;
                    background:${isSelected ? 'var(--accent,#4a90d9)' : 'rgba(255,255,255,0.05)'};
                    border:1px solid ${isSelected ? 'var(--accent,#4a90d9)' : 'transparent'};
                    color:${isSelected ? '#fff' : 'inherit'};
                ">
                    <div style="display:flex; align-items:center; gap:0.4rem;">
                        <span style="font-size:1.1rem;">${this.widgetTypeIcon(t.widget_type)}</span>
                        <div style="flex:1; min-width:0;">
                            <div style="font-size:0.85rem; font-weight:${isSelected ? 'bold' : 'normal'}; white-space:nowrap; overflow:hidden; text-overflow:ellipsis;">${this.escapeHtml(t.name)}</div>
                            <div style="font-size:0.7rem; color:${isSelected ? 'rgba(255,255,255,0.85)' : '#888'};">${t.widget_type}${isDefault ? ' • DEFAULT' : ''}</div>
                        </div>
                    </div>
                </div>`;
        }).join('');
        // Wire click handlers
        list.querySelectorAll('.tpl-sidebar-item').forEach(el => {
            el.onclick = () => {
                const id = el.dataset.id;
                if (id !== this.currentTemplateId) {
                    this.openTemplateEditor(id);
                }
            };
        });
    },

    _updateRawPropsView() {
        const raw = document.getElementById('tpl-raw-props');
        if (raw) raw.value = JSON.stringify(this.collectTemplateProps() || {}, null, 2);
    },

    fillTemplateForm(preset) {
        const props = (() => { try { return JSON.parse(preset.props_json || '{}'); } catch (e) { return {}; } })();
        this._tplElements = Array.isArray(props.template_elements) ? props.template_elements : [];
        delete props.template_elements;
        this._tplSnapGrid = Object.assign({ enabled: true, visible: true, size: 10, snap_size: 10 }, props.snap_grid || {});
        delete props.snap_grid;
        this._tplCanvasSize = this._normalTemplateCanvasSize(props);
        delete props.template_width;
        delete props.template_height;
        this._normalizeTemplateElementLayers();
        this._tplSelectedElementId = this._tplElements[0]?.id || null;
        // Set the top-level name and widget type
        const nameEl = document.getElementById('tpl-name');
        const typeEl = document.getElementById('tpl-widget-type');
        if (nameEl) nameEl.value = preset.name || '';
        if (typeEl) typeEl.value = preset.widget_type || 'weather';
        // Render the schema-driven form with the current props
        const widgetType = (preset.widget_type || 'weather').toLowerCase();
        this._loadSchemaForm(widgetType, props);
        this._renderTemplateCanvas();
        this._renderTemplateElementProps();
        this._renderTemplateCanvasSizeProps();
        this._renderTemplateSnapGridProps();
    },

    collectTemplateProps() {
        const typeEl = document.getElementById('tpl-widget-type');
        const widgetType = typeEl ? typeEl.value : 'text';
        const schema = window.getWidgetSchema(widgetType);
        const container = document.getElementById('wtfk-form');
        if (!container || !schema) return null;
        // Collect from the schema-driven form
        const props = window.WidgetTemplateEditor.collectProps(container, schema);
        // Merge extras
        window.WidgetTemplateEditor.collectExtrasIntoProps(props, this._wtfCurrentExtras || []);
        delete props.template_elements;
        delete props.snap_grid;
        delete props.template_width;
        delete props.template_height;
        const canvasSize = this._normalTemplateCanvasSize();
        props.template_width = canvasSize.width;
        props.template_height = canvasSize.height;
        props.snap_grid = this._normalTemplateSnapGrid();
        if (Array.isArray(this._tplElements) && this._tplElements.length) {
            props.template_elements = this._tplElements.map(e => ({
                id: e.id,
                type: e.type,
                x: Number(e.x) || 0,
                y: Number(e.y) || 0,
                width: Number(e.width) || 120,
                height: Number(e.height) || 80,
                z: Number(e.z) || 0,
                props: (() => { const p = Object.assign({}, e.props || {}); delete p.calendar_variable; return p; })(),
                repeat: e.repeat || { mode: 'none', count: 1, gap_x: 0, gap_y: 10 },
            }));
        }
        // Also include current weather icon overrides
        if (widgetType === 'weather') {
            const icons = {};
            this.currentTemplateAssets.forEach(a => {
                if (a.kind === 'weather_icon' && a.url) icons[a.key] = a.url;
                if (a.kind === 'weather_icon_night' && a.url) icons[a.key] = a.url;
            });
            if (Object.keys(icons).length) props.custom_icons = icons;
        }
        return props;
    },

    async saveCurrentTemplate() {
        const name = document.getElementById('tpl-name').value.trim();
        const widget_type = document.getElementById('tpl-widget-type').value;
        const props = this.collectTemplateProps();
        if (!props) return;
        if (!name) { alert('Please enter a template name.'); return; }
        const data = { name, widget_type, props };
        try {
            if (this.currentTemplateId) {
                await API.updateWidgetPreset(this.currentTemplateId, data);
            } else {
                const created = await API.createWidgetPreset(data);
                this.currentTemplateId = created.id;
            }
            // Reconcile weather icon assets: delete cleared rows, then upsert current local uploads.
            if (widget_type === 'weather') {
                for (const assetId of (this.removedTemplateAssetIds || [])) {
                    await API.removeWidgetPresetAsset(this.currentTemplateId, assetId);
                }
                this.removedTemplateAssetIds = [];
                const nextAssets = [];
                for (const asset of this.currentTemplateAssets) {
                    if (asset.url && this._isLocalWeatherIconUrl(asset.url)) {
                        const saved = await API.addWidgetPresetAsset(this.currentTemplateId, asset.kind, asset.key, asset.url);
                        nextAssets.push(saved || asset);
                    }
                }
                this.currentTemplateAssets = nextAssets;
            }
            this._notifyTemplateChanged();
            // Re-render the sidebar to show the updated template
            this._tplSidebarCache = null;
            this._renderTemplateSidebar();
            // Show a brief success message
            const saveBtn = document.getElementById('tpl-page-save');
            if (saveBtn) {
                const orig = saveBtn.textContent;
                saveBtn.textContent = '✓ Saved';
                setTimeout(() => { saveBtn.textContent = orig; }, 1500);
            }
        } catch (err) { alert('Save failed: ' + err.message); }
    },

    _normalTemplateCanvasSize(props) {
        const src = props || this._tplCanvasSize || {};
        const width = Math.max(80, Math.min(4000, Number(src.template_width ?? src.width ?? 760) || 760));
        const height = Math.max(60, Math.min(4000, Number(src.template_height ?? src.height ?? 428) || 428));
        return { width: Math.round(width), height: Math.round(height) };
    },

    _setTemplateCanvasSize(width, height) {
        this._tplCanvasSize = this._normalTemplateCanvasSize({ width, height });
        this._renderTemplateCanvas();
        this._updateRawPropsView();
    },

    _renderTemplateCanvasSizeProps() {
        const box = document.getElementById('tpl-canvas-size-props');
        if (!box) return;
        const c = this._normalTemplateCanvasSize();
        this._tplCanvasSize = c;
        box.innerHTML = `
          <div class="tpl-element-row"><label>Canvas Width<input type="number" min="80" max="4000" step="1" data-tpl-canvas="width" value="${Number(c.width)||760}"></label><label>Canvas Height<input type="number" min="60" max="4000" step="1" data-tpl-canvas="height" value="${Number(c.height)||428}"></label></div>
          <div class="tpl-field-help">Minimum canvas size: 80 × 60 px. You can also drag the bottom-right corner of the canvas preview to resize it.</div>`;
        box.querySelectorAll('[data-tpl-canvas]').forEach(input => input.oninput = () => {
            const current = this._normalTemplateCanvasSize();
            const key = input.dataset.tplCanvas;
            const next = Object.assign({}, current, { [key]: Number(input.value) || current[key] });
            this._setTemplateCanvasSize(next.width, next.height);
        });
    },

    _normalTemplateSnapGrid() {
        const g = this._tplSnapGrid || {};
        const size = Math.max(2, Math.min(100, Number(g.size || g.snap_size || 10) || 10));
        const snapSize = Math.max(1, Math.min(100, Number(g.snap_size || size) || size));
        return {
            enabled: g.enabled !== false,
            visible: g.visible !== false,
            size,
            snap_size: snapSize,
        };
    },

    _snapTemplateValue(value) {
        const g = this._normalTemplateSnapGrid();
        if (!g.enabled) return Math.round(Number(value) || 0);
        const step = Math.max(1, Number(g.snap_size) || Number(g.size) || 10);
        return Math.round((Number(value) || 0) / step) * step;
    },

    _renderTemplateSnapGridProps() {
        const box = document.getElementById('tpl-snap-grid-props');
        if (!box) return;
        const g = this._normalTemplateSnapGrid();
        this._tplSnapGrid = g;
        box.innerHTML = `
          <div class="tpl-element-row"><label>Snap to Grid<select data-tpl-snap="enabled"><option value="true">On</option><option value="false">Off</option></select></label><label>Show Grid<select data-tpl-snap="visible"><option value="true">On</option><option value="false">Off</option></select></label></div>
          <div class="tpl-element-row"><label>Grid Size<input type="number" min="2" max="100" data-tpl-snap="size" value="${Number(g.size)||10}"></label><label>Snap Size<input type="number" min="1" max="100" data-tpl-snap="snap_size" value="${Number(g.snap_size)||Number(g.size)||10}"></label></div>
          <div class="tpl-field-help">Grid Size controls the visible grid spacing. Snap Size controls the move/resize increment.</div>`;
        const enabled = box.querySelector('[data-tpl-snap="enabled"]'); if (enabled) enabled.value = String(g.enabled !== false);
        const visible = box.querySelector('[data-tpl-snap="visible"]'); if (visible) visible.value = String(g.visible !== false);
        box.querySelectorAll('[data-tpl-snap]').forEach(input => input.oninput = () => {
            const key = input.dataset.tplSnap;
            if (!this._tplSnapGrid) this._tplSnapGrid = this._normalTemplateSnapGrid();
            this._tplSnapGrid[key] = (key === 'enabled' || key === 'visible') ? input.value === 'true' : Number(input.value) || 10;
            this._tplSnapGrid = this._normalTemplateSnapGrid();
            this._renderTemplateCanvas();
            this._updateRawPropsView();
            this._renderTemplateSnapGridProps();
        });
    },

    _nextTemplateCalendarVariable() {
        const used = new Set((this._tplElements || []).map(e => e?.props?.calendar_variable).filter(Boolean));
        let idx = used.size + 1;
        while (used.has('calendar_' + idx)) idx += 1;
        return 'calendar_' + idx;
    },

    _nextTemplateElementZ() {
        const zs = (this._tplElements || []).map(e => Number(e.z) || 0);
        return zs.length ? Math.max(...zs) + 1 : 0;
    },

    _normalizeTemplateElementLayers() {
        if (!Array.isArray(this._tplElements)) return;
        this._tplElements.forEach((e, idx) => { if (e.z === undefined || e.z === null || e.z === '') e.z = idx; });
    },

    _setTemplateElementLayer(el, mode) {
        if (!el || !Array.isArray(this._tplElements)) return;
        this._normalizeTemplateElementLayers();
        const zs = this._tplElements.filter(e => e.id !== el.id).map(e => Number(e.z) || 0);
        if (mode === 'front') el.z = (zs.length ? Math.max(...zs) : 0) + 1;
        else if (mode === 'back') {
            const others = this._tplElements
                .filter(item => item.id !== el.id)
                .map((item, index) => ({ item, index }))
                .sort((a, b) => (Number(a.item.z) || 0) - (Number(b.item.z) || 0) || a.index - b.index);
            el.z = 0;
            others.forEach((entry, index) => { entry.item.z = index + 1; });
        }
        else if (mode === 'up') el.z = (Number(el.z) || 0) + 1;
        else if (mode === 'down') el.z = (Number(el.z) || 0) - 1;
        this._renderTemplateCanvas();
        this._renderTemplateElementProps();
        this._updateRawPropsView();
    },

    _templateElementDefaults(type, x = 24, y = 24) {
        x = this._snapTemplateValue(x); y = this._snapTemplateValue(y);
        const base = { id: 'tpl-el-' + Date.now() + '-' + Math.random().toString(16).slice(2), type, x, y, width: 180, height: 90, z: this._nextTemplateElementZ(), props: {}, repeat: { mode: 'none', count: 1, gap_x: 0, gap_y: 10 } };
        if (type === 'text') { base.width = 220; base.height = 72; base.props = { text: 'Text box', color: '#ffffff', font_size: 24, align: 'center' }; }
        if (type === 'variable_text') { base.width = 210; base.height = 72; base.props = { label: 'Date Text', variable_source: 'date', variable_key: 'full_date', display_mode: 'text', default_text: 'Monday, Jan 1', day_offset: 0, weather_day_offset: 0, color: '#ffffff', font_size: 24, align: 'center' }; }
        if (type === 'variable_icon') { base.width = 120; base.height = 100; base.props = { label: 'Weather Icon', variable_source: 'weather', variable_key: 'weather_icon', display_mode: 'weather_icon', default_text: '🌤️', icon_style: 'custom_icons', weather_day_offset: 0, weather_icon_mode: 'default', weather_icon_alerts: false, color: '#ffffff', font_size: 26, align: 'center' }; }
        if (type === 'variable') { base.width = 120; base.height = 100; base.type = 'variable_icon'; base.props = { label: 'Weather Icon', variable_source: 'weather', variable_key: 'weather_icon', display_mode: 'weather_icon', default_text: '🌤️', icon_style: 'custom_icons', weather_day_offset: 0, weather_icon_mode: 'default', weather_icon_alerts: false, color: '#ffffff', font_size: 26, align: 'center' }; }
        if (type === 'date') { base.width = 210; base.height = 72; base.type = 'variable_text'; base.props = { label: 'Date Text', variable_source: 'date', variable_key: 'full_date', display_mode: 'text', default_text: 'Monday, Jan 1', day_offset: 0, weather_day_offset: 0, color: '#ffffff', font_size: 24, align: 'center' }; }
        if (type === 'image') { base.width = 160; base.height = 110; base.props = { url: '', alt: 'Image', fit: 'contain', radius: 0, radius_tl: 0, radius_tr: 0, radius_br: 0, radius_bl: 0 }; }
        if (type === 'rectangle') { base.width = 220; base.height = 120; base.props = { color: '#1e293b', opacity: 100, shape: 'rectangle', radius: 0, radius_tl: 0, radius_tr: 0, radius_br: 0, radius_bl: 0 }; }
        return base;
    },

    _wireTemplateVisualBuilder() {
        const palette = document.getElementById('tpl-element-palette');
        const stage = document.getElementById('tpl-preview');
        if (!palette || !stage || palette.dataset.wired === '1') return;
        palette.dataset.wired = '1';
        palette.querySelectorAll('[data-tpl-add]').forEach(btn => {
            btn.onclick = () => this._addTemplateElement(btn.dataset.tplAdd);
            btn.addEventListener('dragstart', (e) => e.dataTransfer.setData('text/plain', btn.dataset.tplAdd));
        });
        stage.addEventListener('dragover', (e) => e.preventDefault());
        stage.addEventListener('drop', (e) => {
            e.preventDefault();
            const type = e.dataTransfer.getData('text/plain');
            if (!type) return;
            const canvas = stage.querySelector('.tpl-visual-canvas');
            const rect = (canvas || stage).getBoundingClientRect();
            this._addTemplateElement(type, Math.max(0, this._snapTemplateValue(e.clientX - rect.left)), Math.max(0, this._snapTemplateValue(e.clientY - rect.top)));
        });
    },

    _openTemplateElementsSection() {
        const section = document.getElementById('tpl-elements-section');
        if (section) section.open = true;
    },

    _addTemplateElement(type, x, y) {
        if (!Array.isArray(this._tplElements)) this._tplElements = [];
        const el = this._templateElementDefaults(type, x ?? (30 + this._tplElements.length * 18), y ?? (30 + this._tplElements.length * 18));
        this._tplElements.push(el);
        this._tplSelectedElementId = el.id;
        this._openTemplateElementsSection();
        this._renderTemplateCanvas();
        this._renderTemplateElementProps();
        this._updateRawPropsView();
    },

    _selectedTemplateElement() {
        return (this._tplElements || []).find(e => e.id === this._tplSelectedElementId) || null;
    },

    _templateElementHtml(el, repeatIndex = 0, ghost = false) {
        if (el.type === 'date') { el.type = 'variable_text'; if (!el.props.variable_source) el.props.variable_source = 'date'; if (!el.props.variable_key) el.props.variable_key = 'full_date'; }
        if (el.type === 'variable') { el.type = 'variable_icon'; if (!el.props.variable_source) el.props.variable_source = 'weather'; if (!el.props.variable_key) el.props.variable_key = 'weather_icon'; if (!el.props.display_mode) el.props.display_mode = 'weather_icon'; }
        const props = el.props || (el.props = {});
        const source = props.variable_source || (el.type === 'variable_icon' ? 'weather' : 'date');
        const key = props.variable_key || (el.type === 'variable_icon' ? 'weather_icon' : 'full_date');
        const mode = props.display_mode || (el.type === 'variable_icon' ? 'weather_icon' : 'text');
        const esc = (v) => this.escapeHtml(v == null ? '' : v);
        const weatherOffset = () => Math.max(0, Math.min(6, Number(props.day_offset ?? props.weather_day_offset) || 0));
        const sampleWeatherCodes = [2, 3, 61, 80, 95, 71, 1];
        const sampleHighs = [76, 72, 64, 66, 61, 41, 46];
        const sampleLows = [58, 56, 51, 50, 45, 28, 32];
        const samplePrecip = [20, 35, 70, 55, 85, 60, 10];
        const sampleAlerts = [['Tornado Warning'], ['Snow Squall Warning'], [], [], [], [], []];
        const sampleDescriptions = ['Partly cloudy', 'Overcast', 'Slight rain', 'Rain showers', 'Thunderstorm', 'Snow', 'Mainly clear'];
        const sampleMorningCodes = [45, null, null, null, null, null, null];
        const sampleMorningUntil = ['8:00 AM', '', '', '', '', '', ''];
        const defaultWeatherIcon = (code) => this.getWmoEmoji(Number(code));
        const assetForWeatherIcon = () => {
            const assets = this.currentTemplateAssets || [];
            const code = String(sampleWeatherCodes[weatherOffset()] ?? sampleWeatherCodes[0]);
            const preferred = assets.find(a => a.kind === 'weather_icon' && a.url && String(a.key) === code);
            return preferred?.url || '';
        };
        const dateText = () => {
            const d = new Date();
            d.setDate(d.getDate() + (Number(props.day_offset ?? props.weather_day_offset) || 0));
            if (key === 'day_name') return d.toLocaleDateString(undefined, {weekday:'long'});
            if (key === 'date_number') return String(d.getDate());
            if (key === 'month_day') return d.toLocaleDateString(undefined, {month:'short', day:'numeric'});
            return d.toLocaleDateString(undefined, {weekday:'long', month:'short', day:'numeric'});
        };
        const variableText = () => {
            // Source/key drive the function. Default Text is only a fallback placeholder.
            if (source === 'date') return dateText();
            if (source === 'calendar') {
                const n = (Number(props.day_offset ?? props.weather_day_offset) || 0) + 1;
                if (key === 'calendar_event_time') return `Day ${n} 9:00 AM`;
                if (key === 'calendar_event_location') return `Day ${n} location`;
                return `Day ${n} event`;
            }
            if (source === 'weather') {
                const idx = weatherOffset();
                if (key === 'current_temp') return idx === 0 ? '72°F' : `${sampleHighs[idx]}°F`;
                if (key === 'high_temp') return `High ${sampleHighs[idx]}°F`;
                if (key === 'low_temp') return `Low ${sampleLows[idx]}°F`;
                if (key === 'weather_description') return sampleDescriptions[idx] || 'Weather';
                if (key === 'weather_alerts' || key === 'weather-alerts') return (sampleAlerts[idx] || []).join(' / ');
                if (key === 'morning_weather_condition') return sampleMorningCodes[idx] == null ? '' : (Number(sampleMorningCodes[idx]) === 45 ? 'Fog' : 'Morning weather');
                if (key === 'morning_weather_until') return sampleMorningUntil[idx] ? `until ${sampleMorningUntil[idx]}` : '';
                if (key === 'morning_weather_summary' || key === 'morning_weather') return sampleMorningUntil[idx] ? `Fog until ${sampleMorningUntil[idx]}` : '';
                if (key === 'precip_chance') return `${samplePrecip[idx]}%`;
                if (key === 'wind_speed') return 'Wind 8 mph';
                if (key === 'weather_icon') return props.default_text || '🌤️';
                return 'Weather';
            }
            return props.default_text || props.label || key;
        };
        let content = '';
        const sampleAlertsForElement = source === 'weather' ? (sampleAlerts[weatherOffset()] || []) : [];
        const sampleHasNoData = source === 'calendar' || (source === 'weather' && (key === 'weather_alerts' || key === 'weather-alerts' || key === 'weather-alert-code') && sampleAlertsForElement.length === 0);
        const imageBaseRadius = Number(props.radius ?? 0) || 0;
        const imageRadiusCss = `${Number(props.radius_tl ?? imageBaseRadius)||0}px ${Number(props.radius_tr ?? imageBaseRadius)||0}px ${Number(props.radius_br ?? imageBaseRadius)||0}px ${Number(props.radius_bl ?? imageBaseRadius)||0}px`;
        const imageFit = ['contain', 'cover', 'fill', 'none', 'scale-down'].includes(props.fit) ? props.fit : 'contain';
        if (el.type === 'text') {
            content = `<span>${esc(props.text || '')}</span>`;
        } else if (el.type === 'variable_text') {
            const value = variableText();
            content = props.hide_if_empty && sampleHasNoData ? '' : `<span>${esc(value)}</span>`;
        } else if (el.type === 'variable_icon') {
            const severeWeatherIconHtml = key === 'weather_icon' && props.weather_icon_alerts && weatherOffset() === 0 && sampleAlertsForElement.length && typeof ripleyWeatherAlertConditionIconSvg === 'function'
                ? ripleyWeatherAlertConditionIconSvg(sampleAlertsForElement)
                : '';
            const alertIconHtml = source === 'weather' && (key === 'weather-alert-code' || key === 'weather_alerts' || key === 'weather-alerts' || severeWeatherIconHtml)
                ? (key === 'weather-alert-code' ? ripleyWeatherAlertColorCodeIconSvg(sampleAlertsForElement) : (severeWeatherIconHtml || ripleyWeatherAlertConditionIconSvg(sampleAlertsForElement)))
                : '';
            const iconUrl = (source === 'weather' && (key === 'weather_icon' || mode === 'weather_icon')) ? assetForWeatherIcon() : ((props.icon_style === 'custom_icons' || props.icon_style === 'image_url') ? assetForWeatherIcon() : '');
            const code = String(sampleWeatherCodes[weatherOffset()] ?? sampleWeatherCodes[0]);
            const morningCode = sampleMorningCodes[weatherOffset()];
            const splitIcon = props.weather_icon_mode === 'morning_split' && morningCode != null && weatherOffset() === 0;
            content = props.hide_if_empty && sampleHasNoData ? '' : (iconUrl ? `<img src="${esc(iconUrl)}" alt="" class="tpl-visual-output-img">` : (alertIconHtml || `<span class="tpl-visual-output-icon">${esc(splitIcon ? `${defaultWeatherIcon(morningCode)} → ${defaultWeatherIcon(code)}` : (source === 'weather' ? defaultWeatherIcon(code) : (props.default_text || '🌤️')))}</span>`));
        } else if (el.type === 'image') {
            content = props.url ? `<img src="${esc(props.url)}" alt="${esc(props.alt || '')}" class="tpl-visual-output-img" style="width:100%;height:100%;object-fit:${imageFit};border-radius:${imageRadiusCss};">` : '';
        } else if (el.type === 'weather') {
            content = '';
        } else if (el.type === 'calendar') {
            content = '';
        }
        const rep = el.repeat || {};
        const canvasSize = this._normalTemplateCanvasSize();
        const groupElements = rep.group ? (this._tplElements || []).filter(item => item.repeat && item.repeat.group === rep.group) : [el];
        const groupMinY = groupElements.reduce((min, item) => Math.min(min, Number(item.y) || 0), Infinity);
        const groupMaxY = groupElements.reduce((max, item) => Math.max(max, (Number(item.y) || 0) + (Number(item.height) || 80)), -Infinity);
        const groupHeight = Number.isFinite(groupMaxY - groupMinY) ? groupMaxY - groupMinY : (Number(el.height) || 80);
        const rowStep = groupHeight + (Number(rep.gap_y) || 0);
        const x = (Number(el.x)||0) + repeatIndex * (Number(rep.gap_x)||0 + (rep.mode === 'columns' ? Number(el.width)||0 : 0));
        const y = rep.order === 'bottom-up' && (rep.mode === 'rows' || (rep.group && !rep.mode))
            ? canvasSize.height - groupHeight + (Number(el.y)||0) - groupMinY - repeatIndex * rowStep
            : (Number(el.y)||0) + repeatIndex * (Number(rep.gap_y)||0 + (rep.mode === 'rows' ? Number(el.height)||0 : 0));
        const shape = ['square', 'rounded'].includes(props.shape) ? 'rectangle' : (props.shape || 'rectangle');
        const baseRadius = Number(props.radius ?? 0) || 0;
        const radiusCss = shape === 'circle' ? '50%' : (shape === 'pill' ? '999px' : `${Number(props.radius_tl ?? baseRadius)||0}px ${Number(props.radius_tr ?? baseRadius)||0}px ${Number(props.radius_br ?? baseRadius)||0}px ${Number(props.radius_bl ?? baseRadius)||0}px`);
        const shapeExtra = shape === 'line' ? 'height:6px;' : '';
        const colorStyle = (el.type === 'text' || el.type === 'variable_text' || el.type === 'variable_icon') ? `color:${esc(props.color || '#ffffff')};font-size:${Number(props.font_size)||24}px;text-align:${esc(props.align || 'center')};` : '';
        const rawShapeOpacity = Number(props.opacity ?? 100);
        const shapeOpacity = Number.isFinite(rawShapeOpacity) ? (rawShapeOpacity >= 0 && rawShapeOpacity <= 1 ? rawShapeOpacity : Math.max(0, Math.min(100, rawShapeOpacity)) / 100) : 1;
        const shapeFill = el.type === 'rectangle' ? `<span class="tpl-visual-shape-fill" style="position:absolute;inset:0;background:${esc(props.color || '#1e293b')};opacity:${shapeOpacity};border-radius:${radiusCss};${shapeExtra}"></span>` : '';
        const style = `left:${x}px; top:${y}px; width:${Number(el.width)||120}px; height:${Number(el.height)||80}px; z-index:${Number(el.z)||0}; ${shapeExtra} border-radius:${radiusCss}; ${colorStyle}`;
        const cls = ['tpl-visual-element', 'output-only', ghost ? 'repeat-ghost' : '', (!ghost && el.id === this._tplSelectedElementId) ? 'selected' : ''].filter(Boolean).join(' ');
        const handles = (!ghost && el.id === this._tplSelectedElementId) ? '<span class="tpl-resize-handle nw" data-resize="nw"></span><span class="tpl-resize-handle n" data-resize="n"></span><span class="tpl-resize-handle ne" data-resize="ne"></span><span class="tpl-resize-handle e" data-resize="e"></span><span class="tpl-resize-handle se" data-resize="se"></span><span class="tpl-resize-handle s" data-resize="s"></span><span class="tpl-resize-handle sw" data-resize="sw"></span><span class="tpl-resize-handle w" data-resize="w"></span>' : '';
        const align = props.align || 'center';
        const justify = align === 'left' ? 'flex-start' : (align === 'right' ? 'flex-end' : 'center');
        return `<div class="${cls}" data-el-id="${esc(el.id)}" style="${style}">${shapeFill}<div class="tpl-visual-content" style="justify-content:${justify};text-align:${esc(align)};">${content}</div>${handles}</div>`;
    },

    _renderTemplateCanvas() {
        const stage = document.getElementById('tpl-preview');
        if (!stage) return;
        const elements = this._tplElements || [];
        const snapGrid = this._normalTemplateSnapGrid();
        const canvasSize = this._normalTemplateCanvasSize();
        this._tplCanvasSize = canvasSize;
        const gridCls = snapGrid.visible ? ' tpl-grid-visible' : '';
        const gridStyle = `--tpl-grid-size:${Number(snapGrid.size)||10}px;width:${canvasSize.width}px;height:${canvasSize.height}px;`;
        stage.innerHTML = `<div class="tpl-visual-canvas${gridCls}" style="${gridStyle}">${elements.length ? '' : '<div class="tpl-visual-empty">Drag elements here to build the widget template JSON.</div>'}<span class="tpl-canvas-resize-handle" title="Resize canvas"></span></div>`;
        const canvas = stage.querySelector('.tpl-visual-canvas');
        const html = [];
        this._normalizeTemplateElementLayers();
        [...elements].sort((a,b) => (Number(a.z)||0) - (Number(b.z)||0)).forEach(el => {
            html.push(this._templateElementHtml(el, 0, false));
            const rep = el.repeat || { mode: 'none', count: 1 };
            const count = Math.max(1, Math.min(30, Number(rep.count) || 1));
            if ((rep.mode && rep.mode !== 'none') || rep.group) {
                for (let i=1; i<count; i++) html.push(this._templateElementHtml(el, i, true));
            }
        });
        canvas.insertAdjacentHTML('beforeend', html.join(''));
        canvas.querySelectorAll('.tpl-visual-element:not(.repeat-ghost)').forEach(node => {
            node.onclick = (e) => { e.stopPropagation(); this._tplSelectedElementId = node.dataset.elId; this._openTemplateElementsSection(); this._renderTemplateCanvas(); this._renderTemplateElementProps(); };
            node.onmousedown = (e) => {
                if (e.button !== 0) return;
                e.preventDefault();
                e.stopPropagation();
                const el = (this._tplElements || []).find(x => x.id === node.dataset.elId);
                if (!el) return;
                this._tplSelectedElementId = el.id;
                this._openTemplateElementsSection();
                const resizeEdge = e.target?.classList?.contains('tpl-resize-handle') ? (e.target.dataset.resize || e.target.dataset.corner) : null;
                const startX = e.clientX, startY = e.clientY;
                const ox = Number(el.x)||0, oy = Number(el.y)||0, ow = Number(el.width)||120, oh = Number(el.height)||80;
                const move = (ev) => {
                    App._tplDragging = true;
                    const dx = Math.round(ev.clientX - startX), dy = Math.round(ev.clientY - startY);
                    if (resizeEdge) {
                        if (resizeEdge.includes('e')) el.width = Math.max(12, this._snapTemplateValue(ow + dx));
                        if (resizeEdge.includes('s')) el.height = Math.max(8, this._snapTemplateValue(oh + dy));
                        if (resizeEdge.includes('w')) { const nw = Math.max(12, this._snapTemplateValue(ow - dx)); el.x = Math.max(0, this._snapTemplateValue(ox + (ow - nw))); el.width = nw; }
                        if (resizeEdge.includes('n')) { const nh = Math.max(8, this._snapTemplateValue(oh - dy)); el.y = Math.max(0, this._snapTemplateValue(oy + (oh - nh))); el.height = nh; }
                    } else {
                        el.x = Math.max(0, this._snapTemplateValue(ox + dx)); el.y = Math.max(0, this._snapTemplateValue(oy + dy));
                    }
                    this._renderTemplateCanvas(); this._renderTemplateElementProps(); this._updateRawPropsView();
                };
                const up = () => { App._tplDragging = false; document.removeEventListener('mousemove', move); document.removeEventListener('mouseup', up); };
                document.addEventListener('mousemove', move); document.addEventListener('mouseup', up);
            };
        });
        const canvasResize = canvas.querySelector('.tpl-canvas-resize-handle');
        if (canvasResize) canvasResize.onmousedown = (e) => {
            if (e.button !== 0) return;
            e.preventDefault();
            e.stopPropagation();
            const startX = e.clientX, startY = e.clientY;
            const start = this._normalTemplateCanvasSize();
            const move = (ev) => {
                const w = this._snapTemplateValue(start.width + Math.round(ev.clientX - startX));
                const h = this._snapTemplateValue(start.height + Math.round(ev.clientY - startY));
                this._tplCanvasSize = this._normalTemplateCanvasSize({ width: w, height: h });
                this._renderTemplateCanvas();
                this._renderTemplateCanvasSizeProps();
                this._updateRawPropsView();
            };
            const up = () => { document.removeEventListener('mousemove', move); document.removeEventListener('mouseup', up); };
            document.addEventListener('mousemove', move);
            document.addEventListener('mouseup', up);
        };
        canvas.onclick = () => { this._tplSelectedElementId = null; this._renderTemplateCanvas(); this._renderTemplateElementProps(); };
    },

    _renderTemplateElementProps() {
        const el = this._selectedTemplateElement();
        const noElMsg = '<div class="tpl-element-empty">No element selected. Drag an element into the preview or click an existing element to edit it.</div>';

        // Helper: clear a container and optionally set its innerHTML
        function clear(container, html) {
            if (!container) return;
            container.innerHTML = html || '';
        }

        // Helper: open/close <details> by element ID
        function setDetailsOpen(id, open) {
            if (App._tplDragging) return;
            const d = document.getElementById(id);
            if (d) d.open = open;
        }

        // Helper: set value on an input/select if it exists
        function setVal(sel, val) {
            const el = typeof sel === 'string' ? document.querySelector(sel) : sel;
            if (el && val !== undefined) el.value = val;
        }

        if (!el) {
            // Nothing selected — clear all section containers, show builder tab
            clear(document.getElementById('tpl-position-props'));
            clear(document.getElementById('tpl-text-props'));
            clear(document.getElementById('tpl-variable-props'));
            clear(document.getElementById('tpl-image-props'));
            clear(document.getElementById('tpl-shape-props'));
            clear(document.getElementById('tpl-repeat-props'));
            setDetailsOpen('tpl-position-section', true);
            setDetailsOpen('tpl-text-section', false);
            setDetailsOpen('tpl-variable-section', false);
            setDetailsOpen('tpl-image-section', false);
            setDetailsOpen('tpl-shape-section', false);
            setDetailsOpen('tpl-repeat-section', false);
            if (window.switchToBuilderTab) window.switchToBuilderTab();
            return;
        }

        // An element is selected — show the Element tab
        if (window.switchToElementTab) window.switchToElementTab();

        const rep = el.repeat || { mode: 'none', count: 1, gap_x: 0, gap_y: 10 };
        const props = el.props || (el.props = {});
        const esc = (v) => String(v == null ? '' : v).replace(/[&<>"'`]/g, c => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":"&#39;",'`':'&#96;'}[c]));
        const topLabel = document.getElementById('tpl-el-top-label');
        if (topLabel) {
            const typeLabels = { text: 'Text', variable_text: 'Variable Text', variable_icon: 'Variable Icon', variable: 'Variable Icon', date: 'Variable Text', image: 'Image', rectangle: 'Shape' };
            const elementLabel = typeLabels[el.type] || 'Element';
            const valueLabel = props.variable_key ? String(props.variable_key).replace(/_/g, ' ') : '';
            topLabel.textContent = valueLabel ? `${elementLabel} · ${valueLabel}` : elementLabel;
        }

        // ── Position & Size (always shown) ──
        const posBox = document.getElementById('tpl-position-props');
        if (posBox) {
            posBox.innerHTML = `
              <div class="tpl-element-row"><label>X<input type="number" data-tpl-el="x" value="${Number(el.x)||0}"></label><label>Y<input type="number" data-tpl-el="y" value="${Number(el.y)||0}"></label></div>
              <div class="tpl-element-row"><label>Width<input type="number" data-tpl-el="width" value="${Number(el.width)||120}"></label><label>Height<input type="number" data-tpl-el="height" value="${Number(el.height)||80}"></label></div>
              <div class="tpl-element-row"><label>Z Layer<input type="number" data-tpl-el="z" value="${Number(el.z)||0}" step="1"></label><span class="tpl-field-help">Higher Z renders in front.</span></div>
            `;
            posBox.querySelectorAll('[data-tpl-el]').forEach(inp => {
                inp.oninput = () => {
                    const k = inp.dataset.tplEl;
                    el[k] = ['x','y','width','height'].includes(k) ? this._snapTemplateValue(Number(inp.value)||0) : Number(inp.value)||0;
                    this._renderTemplateCanvas();
                    this._updateRawPropsView && this._updateRawPropsView();
                };
            });
        }
        setDetailsOpen('tpl-position-section', true);

        // ── Text fields ──
        const textBox = document.getElementById('tpl-text-props');
        const isText = el.type === 'text';
        const isVariable = ['variable_text','variable_icon','variable','date'].includes(el.type);
        if (textBox) {
            if (isText) {
                textBox.innerHTML = `
                  <div class="tpl-element-row full"><label>Text<input type="text" data-tpl-prop="text" value="${esc(props.text||'')}" placeholder="Enter text..."></label></div>
                  <div class="tpl-element-row"><label>Font Size<input type="number" min="1" data-tpl-prop="font_size" value="${Number(props.font_size)||24}"></label><label>Text Color<input type="color" data-tpl-prop="color" value="${esc(props.color||'#ffffff')}"></label></div>
                  <div class="tpl-element-row full"><label>Align<select data-tpl-prop="align"><option value="left">Left</option><option value="center">Center</option><option value="right">Right</option></select></label></div>
                `;
                const alignSelect = textBox.querySelector('[data-tpl-prop="align"]');
                if (alignSelect) alignSelect.value = props.align || 'center';
                textBox.querySelectorAll('[data-tpl-prop]').forEach(inp => {
                    const updateTextProp = () => {
                        const k = inp.dataset.tplProp;
                        const v = inp.type === 'number' ? Number(inp.value)||0 : inp.value;
                        el.props[k] = v;
                        this._renderTemplateCanvas();
                        this._updateRawPropsView && this._updateRawPropsView();
                    };
                    inp.oninput = updateTextProp;
                    inp.onchange = updateTextProp;
                });
            } else if (isVariable) {
                textBox.innerHTML = `
                  <div class="tpl-element-row"><label>Font Size<input type="number" data-tpl-prop="font_size" value="${Number(props.font_size)||26}"></label><label>Text Color<input type="color" data-tpl-prop="color" value="${esc(props.color||'#ffffff')}"></label></div>
                  <div class="tpl-element-row full"><label>Align<select data-tpl-prop="align"><option value="left">Left</option><option value="center">Center</option><option value="right">Right</option></select></label></div>
                `;
                const variableAlignSelect = textBox.querySelector('[data-tpl-prop="align"]');
                if (variableAlignSelect) variableAlignSelect.value = props.align || 'center';
                textBox.querySelectorAll('[data-tpl-prop]').forEach(inp => {
                    const updateTextProp = () => {
                        const k = inp.dataset.tplProp;
                        const v = inp.type === 'checkbox' ? inp.checked : (inp.type === 'number' ? Number(inp.value)||0 : inp.value);
                        el.props[k] = v;
                        this._renderTemplateCanvas();
                        this._updateRawPropsView && this._updateRawPropsView();
                    };
                    inp.oninput = updateTextProp;
                    inp.onchange = updateTextProp;
                });
            } else {
                textBox.innerHTML = '';
            }
        }
        setDetailsOpen('tpl-text-section', isText || isVariable);

        // ── Variable fields ──
        const varBox = document.getElementById('tpl-variable-props');
        if (varBox) {
            if (isVariable) {
                const source = props.variable_source || (el.type === 'variable_icon' ? 'weather' : 'date');
                const key = props.variable_key || (el.type === 'variable_icon' ? 'weather_icon' : 'full_date');
                const isIconVariable = el.type === 'variable_icon' || props.display_mode === 'weather_icon' || key === 'weather_icon' || key === 'weather-alerts' || key === 'weather-alert-code';
                const isWeatherIconVariable = isIconVariable && source === 'weather' && key === 'weather_icon';
                const iconStyleField = isIconVariable ? `<div class="tpl-element-row"><label>Icon Style<select data-tpl-prop="icon_style"><option value="default">Default Icon</option><option value="custom_icons">Custom Icons</option><option value="image_url">Image URL</option></select></label></div>` : '';
                const iconOptionsField = isWeatherIconVariable ? `<div class="tpl-element-row"><label><input type="checkbox" data-tpl-prop="weather_auto_night" value="true" ${props.weather_auto_night!==false?'checked':''}> Auto night (7pm–6am)</label><label>Morning Icon<select data-tpl-prop="weather_icon_mode"><option value="default">Day icon only</option><option value="morning_split">Split morning → day icon</option></select></label></div>` : '';
                const variableHelp = isIconVariable && source === 'traffic' ? 'Traffic Variable Icon can render a server-generated Google Maps location snippet for each event.' : (isIconVariable ? 'Variable Icon renders a weather or alert Meteocon.' : (source === 'traffic' ? 'Traffic Variable renders live road events for the selected Traffic widget area.' : 'Variable Text renders a weather, calendar, traffic, or date value.'));
                const variableBehaviorField = `<div class="tpl-element-row full"><label><input type="checkbox" data-tpl-prop="hide_if_empty" ${props.hide_if_empty ? 'checked' : ''}> Hide when empty or unavailable</label></div>${isWeatherIconVariable ? `<div class="tpl-element-row full"><label><input type="checkbox" data-tpl-prop="weather_icon_alerts" ${props.weather_icon_alerts ? 'checked' : ''}> Use severe-weather icon today</label></div><div class="tpl-field-help">Today-only Weather: Icon override. Uses a Meteocon condition icon, never an alarm color code.</div>` : ''}`;
                const variableGroups = {
                    weather: [
                        ['current_temp', 'Current Temperature'], ['high_temp', 'Forecast High'], ['low_temp', 'Forecast Low'],
                        ['weather_icon', 'Weather Icon'], ['weather_description', 'Description'], ['weather-alerts', 'Weather Alerts'],
                        ['weather-alert-code', 'Alert Color Code'], ['morning_weather_summary', 'Morning Condition + Until'],
                        ['morning_weather_condition', 'Morning Condition'], ['morning_weather_until', 'Morning Until'],
                        ['precip_chance', 'Precipitation Chance'], ['wind_speed', 'Wind Speed']
                    ],
                    traffic: [
                        ['traffic_summary', 'Traffic Summary'], ['traffic_incidents', 'Crash / Incident Count'], ['traffic_crashes_closures', 'Crash / Road Closure Description'],
                        ['traffic_closures', 'Closure Count'], ['traffic_delays', 'Delay Count'],
                        ['traffic_event_road', 'Event Road'], ['traffic_event_direction', 'Event Direction'],
                        ['traffic_event_description', 'Event Description'], ['traffic_event_category', 'Event Category'],
                        ['traffic_event_distance', 'Event Distance'], ['traffic_event_map', 'Event Map Snippet']
                    ],
                    calendar: [
                        ['calendar_event_title', 'Event'], ['calendar_event_time', 'Event Time'],
                        ['calendar_event_date', 'Event Date'], ['calendar_event_location', 'Location']
                    ],
                    date: [
                        ['event', 'Event Number'], ['day_name', 'Day Name'], ['date_number', 'Date Number'],
                        ['month_day', 'Month + Day'], ['full_date', 'Full Date']
                    ]
                };
                const variableGroup = variableGroups[source] || variableGroups.weather;
                const variableLabel = source === 'weather' ? 'Weather Variable' : (source === 'traffic' ? 'Traffic Variable' : (source === 'calendar' ? 'Calendar Variable' : 'Date Variable'));
                const variableOptions = variableGroup.map(([value, label]) => `<option value="${value}">${label}</option>`).join('');
                varBox.innerHTML = `
                  <div class="tpl-element-row"><label>Data Source<select data-tpl-prop="variable_source"><option value="weather">Weather</option><option value="traffic">Traffic</option><option value="calendar">Calendar</option><option value="date">Date</option></select></label><label>${variableLabel}<select data-tpl-prop="variable_key">${variableOptions}</select></label></div>
                  <div class="tpl-element-row"><label>Display Mode<select data-tpl-prop="display_mode"><option value="text">Text</option><option value="weather_icon">Weather Icon</option></select></label><label>Forecast Day<input type="number" min="0" max="31" data-tpl-prop="day_offset" value="${Number(props.day_offset??props.weather_day_offset)||0}"></label></div>
                  <div class="tpl-element-row full"><label>Fallback Text<input type="text" data-tpl-prop="default_text" value="${esc(props.default_text||(el.type==='variable_icon'?'🌤️':'Monday, Jan 1'))}" placeholder="Shown if a value is unavailable"></label></div>
                  ${variableBehaviorField}
                  ${iconStyleField}
                  ${iconOptionsField}
                  <div class="tpl-field-help">${variableHelp}</div>
                `;
                const sourceSelect = varBox.querySelector('[data-tpl-prop="variable_source"]');
                const keySelect = varBox.querySelector('[data-tpl-prop="variable_key"]');
                const modeSelect = varBox.querySelector('[data-tpl-prop="display_mode"]');
                const iconSelect = varBox.querySelector('[data-tpl-prop="icon_style"]');
                const weatherIconModeSelect = varBox.querySelector('[data-tpl-prop="weather_icon_mode"]');
                if (sourceSelect) sourceSelect.value = props.variable_source || 'weather';
                if (keySelect) keySelect.value = props.variable_key || (el.type === 'variable_icon' ? 'weather_icon' : 'current_temp');
                if (modeSelect) modeSelect.value = props.display_mode || (el.type === 'variable_icon' ? 'weather_icon' : 'text');
                if (iconSelect) iconSelect.value = props.icon_style || 'default';
                if (weatherIconModeSelect) weatherIconModeSelect.value = props.weather_icon_mode || 'default';
                varBox.querySelectorAll('[data-tpl-prop]').forEach(inp => {
                    inp.oninput = () => {
                        const k = inp.dataset.tplProp;
                        const v = inp.type === 'checkbox' ? inp.checked : (inp.type === 'number' ? Number(inp.value)||0 : inp.value);
                        if (k === 'day_offset') el.props.weather_day_offset = v;
                        el.props[k] = v;
                        if (k === 'variable_source') {
                            if (v === 'weather') { el.props.variable_key = el.type === 'variable_icon' ? 'weather_icon' : 'current_temp'; el.props.display_mode = el.type === 'variable_icon' ? 'weather_icon' : 'text'; }
                            if (v === 'traffic') { el.props.variable_key = 'traffic_summary'; el.props.display_mode = 'text'; }
                            if (v === 'date') { el.props.variable_key = 'full_date'; el.props.display_mode = 'text'; }
                            if (v === 'calendar') { el.props.variable_key = 'calendar_event_title'; el.props.display_mode = 'text'; }
                            this._renderTemplateCanvas();
                            this._renderTemplateElementProps();
                            return;
                        }
                        if (k === 'variable_key') {
                            const weatherKeys = ['current_temp','high_temp','low_temp','weather_icon','weather_description','weather-alerts','weather-alert-code','morning_weather_summary','morning_weather_condition','morning_weather_until','precip_chance','wind_speed'];
                            const trafficKeys = ['traffic_summary','traffic_incidents','traffic_crashes_closures','traffic_closures','traffic_delays','traffic_event_road','traffic_event_direction','traffic_event_description','traffic_event_category','traffic_event_distance','traffic_event_map'];
                            const calendarKeys = ['calendar_event_title','calendar_event_time','calendar_event_date','calendar_event_location'];
                            if (weatherKeys.includes(v)) el.props.variable_source = 'weather';
                            else if (trafficKeys.includes(v)) el.props.variable_source = 'traffic';
                            else if (calendarKeys.includes(v)) el.props.variable_source = 'calendar';
                            el.props.display_mode = v === 'weather_icon' ? 'weather_icon' : 'text';
                            this._renderTemplateCanvas();
                            this._renderTemplateElementProps();
                            return;
                        }
                        this._renderTemplateCanvas();
                        this._updateRawPropsView && this._updateRawPropsView();
                    };
                });
            } else {
                varBox.innerHTML = '';
            }
        }
        setDetailsOpen('tpl-variable-section', isVariable);

        // ── Image fields ──
        const imgBox = document.getElementById('tpl-image-props');
        const isImage = el.type === 'image';
        if (imgBox) {
            if (isImage) {
                const currentImageUrl = String(props.url || '');
                const embeddedImage = currentImageUrl.startsWith('data:image/');
                const imageUrlValue = embeddedImage ? '' : currentImageUrl;
                imgBox.innerHTML = `
                  <div class="tpl-element-row full"><label>Image URL<input type="text" data-tpl-prop="url" value="${esc(imageUrlValue)}" placeholder="${embeddedImage ? 'Embedded image (upload to replace)' : '/uploads/example.png'}"></label></div>
                  <div class="tpl-element-row full"><input type="file" data-tpl-image-upload accept="image/png,image/jpeg,image/gif,image/webp" style="display:none"><button type="button" class="btn btn-secondary btn-sm" data-tpl-image-upload-btn>⬆ Embed image</button><span class="tpl-inline-status" data-tpl-image-upload-status>${embeddedImage ? 'Embedded image stored in this template.' : ''}</span></div>
                  <div class="tpl-element-row full"><label>Scaling<select data-tpl-prop="fit"><option value="contain" ${(!props.fit || props.fit === 'contain') ? 'selected' : ''}>Fit inside (show entire image)</option><option value="cover" ${props.fit === 'cover' ? 'selected' : ''}>Fill frame (crop if needed)</option><option value="fill" ${props.fit === 'fill' ? 'selected' : ''}>Stretch to fill</option><option value="scale-down" ${props.fit === 'scale-down' ? 'selected' : ''}>Original size, scale down if needed</option><option value="none" ${props.fit === 'none' ? 'selected' : ''}>Original size</option></select></label></div>
                  <div class="tpl-element-row"><label>All Corners<input type="number" min="0" max="999" data-tpl-prop="radius" value="${Number(props.radius??0)||0}"></label></div>
                  <div class="tpl-element-row"><label>Top Left<input type="number" min="0" max="999" data-tpl-prop="radius_tl" value="${Number(props.radius_tl??props.radius??0)||0}"></label><label>Top Right<input type="number" min="0" max="999" data-tpl-prop="radius_tr" value="${Number(props.radius_tr??props.radius??0)||0}"></label></div>
                  <div class="tpl-element-row"><label>Bottom Left<input type="number" min="0" max="999" data-tpl-prop="radius_bl" value="${Number(props.radius_bl??props.radius??0)||0}"></label><label>Bottom Right<input type="number" min="0" max="999" data-tpl-prop="radius_br" value="${Number(props.radius_br??props.radius??0)||0}"></label></div>
                `;
                const imageUpload = imgBox.querySelector('[data-tpl-image-upload]');
                const imageUploadButton = imgBox.querySelector('[data-tpl-image-upload-btn]');
                const imageUploadStatus = imgBox.querySelector('[data-tpl-image-upload-status]');
                if (imageUpload && imageUploadButton) {
                    imageUploadButton.onclick = () => imageUpload.click();
                    imageUpload.onchange = async () => {
                        const file = imageUpload.files && imageUpload.files[0];
                        imageUpload.value = '';
                        if (!file) return;
                        try {
                            const MAX_EMBEDDED_IMAGE_BYTES = 2 * 1024 * 1024;
                            const allowedImageTypes = ['image/png', 'image/jpeg', 'image/gif', 'image/webp'];
                            if (!allowedImageTypes.includes(String(file.type || '').toLowerCase())) throw new Error('Use a PNG, JPG, GIF, or WebP image.');
                            if (file.size > MAX_EMBEDDED_IMAGE_BYTES) throw new Error('Embedded images must be 2 MB or smaller.');
                            if (imageUploadStatus) imageUploadStatus.textContent = 'Embedding…';
                            const dataUrl = await new Promise((resolve, reject) => {
                                const reader = new FileReader();
                                reader.onerror = () => reject(new Error('Could not read the image file.'));
                                reader.onload = () => resolve(String(reader.result || ''));
                                reader.readAsDataURL(file);
                            });
                            if (!dataUrl.startsWith('data:image/')) throw new Error('The selected file did not produce an image.');
                            el.props.url = dataUrl;
                            const urlInput = imgBox.querySelector('[data-tpl-prop="url"]');
                            if (urlInput) {
                                urlInput.value = '';
                                urlInput.placeholder = 'Embedded image (upload to replace)';
                            }
                            if (imageUploadStatus) imageUploadStatus.textContent = `Embedded in template (${Math.ceil(file.size / 1024)} KB). Save template to keep it.`;
                            this._renderTemplateCanvas();
                            this._updateRawPropsView && this._updateRawPropsView();
                        } catch (err) {
                            if (imageUploadStatus) imageUploadStatus.textContent = `Embed failed: ${err.message}`;
                        }
                    };
                }
                imgBox.querySelectorAll('[data-tpl-prop]').forEach(inp => {
                    const updateImageProp = () => {
                        const k = inp.dataset.tplProp;
                        const v = inp.type === 'number' ? Number(inp.value)||0 : inp.value;
                        el.props[k] = v;
                        if (k === 'radius') { el.props.radius_tl = v; el.props.radius_tr = v; el.props.radius_br = v; el.props.radius_bl = v; }
                        this._renderTemplateCanvas();
                        this._updateRawPropsView && this._updateRawPropsView();
                    };
                    inp.oninput = updateImageProp;
                    inp.onchange = updateImageProp;
                });
            } else {
                imgBox.innerHTML = '';
            }
        }
        setDetailsOpen('tpl-image-section', isImage);

        // ── Shape fields ──
        const shapeBox = document.getElementById('tpl-shape-props');
        const isShape = el.type === 'rectangle';
        if (shapeBox) {
            if (isShape) {
                shapeBox.innerHTML = `
                  <div class="tpl-element-row"><label>Shape<select data-tpl-prop="shape"><option value="rectangle" ${['rectangle','square','rounded'].includes(props.shape || 'rectangle') ? 'selected' : ''}>Rectangle</option><option value="pill" ${props.shape === 'pill' ? 'selected' : ''}>Pill/Capsule</option><option value="circle" ${props.shape === 'circle' ? 'selected' : ''}>Circle/Oval</option><option value="line" ${props.shape === 'line' ? 'selected' : ''}>Divider Line</option></select></label><label>Color<input type="color" data-tpl-prop="color" value="${esc(props.color||'#1e293b')}"></label></div>
                  <div class="tpl-element-row"><label>Opacity %<input type="number" min="0" max="100" step="1" data-tpl-prop="opacity" value="${Math.max(0,Math.min(100,Number(props.opacity??100)))}"></label></div>
                  <div class="tpl-element-row"><label>All Corners<input type="number" min="0" data-tpl-prop="radius" value="${Number(props.radius??12)||0}"></label></div>
                  <div class="tpl-element-row"><label>Top Left<input type="number" min="0" data-tpl-prop="radius_tl" value="${Number(props.radius_tl??props.radius??12)||0}"></label><label>Top Right<input type="number" min="0" data-tpl-prop="radius_tr" value="${Number(props.radius_tr??props.radius??12)||0}"></label></div>
                  <div class="tpl-element-row"><label>Bot. Left<input type="number" min="0" data-tpl-prop="radius_bl" value="${Number(props.radius_bl??props.radius??12)||0}"></label><label>Bot. Right<input type="number" min="0" data-tpl-prop="radius_br" value="${Number(props.radius_br??props.radius??12)||0}"></label></div>
                `;
                shapeBox.querySelectorAll('[data-tpl-prop]').forEach(inp => {
                    inp.oninput = () => {
                        const k = inp.dataset.tplProp;
                        const v = inp.type === 'number' ? Number(inp.value)||0 : inp.value;
                        el.props[k] = v;
                        if (k === 'radius') { el.props.radius_tl = v; el.props.radius_tr = v; el.props.radius_br = v; el.props.radius_bl = v; }
                        this._renderTemplateCanvas();
                        this._updateRawPropsView && this._updateRawPropsView();
                    };
                });
            } else {
                shapeBox.innerHTML = '';
            }
        }
        setDetailsOpen('tpl-shape-section', isShape);

        // ── Repeat fields ──
        const repeatBox = document.getElementById('tpl-repeat-props');
        if (repeatBox) {
            const groupIds = [...new Set((this._tplElements || []).map(item => item.repeat && item.repeat.group).filter(Boolean))];
            const currentGroup = rep.group || '';
            const groupOptions = ['<option value="">No repeat group</option>']
                .concat(groupIds.map(group => `<option value="${esc(group)}" ${group === currentGroup ? 'selected' : ''}>${esc(group)}</option>`))
                .concat([`<option value="__new__">＋ Create new repeat group</option>`]).join('');
            repeatBox.innerHTML = `
              <div class="tpl-field-help">Assign elements to the same repeat group to repeat them together. In the Layout Editor, Weather controls the day count and Calendar controls the event count.</div>
              <div class="tpl-element-row"><label>Repeat group<select data-tpl-repeat="group">${groupOptions}</select></label><label>Direction<select data-tpl-repeat="mode"><option value="none">None</option><option value="rows">Rows</option><option value="columns">Columns</option></select></label></div>
              <div class="tpl-element-row"><label>Repeat order<select data-tpl-repeat="order"><option value="top-down">Top to bottom</option><option value="bottom-up">Bottom to top</option></select></label><label>Default count<input type="number" min="1" max="30" data-tpl-repeat="count" value="${Math.max(1, Number(rep.count)||1)}"></label></div>
              <div class="tpl-element-row"><label>Gap X<input type="number" data-tpl-repeat="gap_x" value="${Number(rep.gap_x)||0}"></label><label>Gap Y<input type="number" data-tpl-repeat="gap_y" value="${Number(rep.gap_y)||0}"></label></div>
              <label class="tpl-checkbox"><input type="checkbox" data-tpl-repeat="hide_if_empty" ${rep.hide_if_empty ? 'checked' : ''}> Hide group when there are no events</label>
              <label class="tpl-checkbox"><input type="checkbox" data-tpl-repeat="preserve_size" ${rep.preserve_size ? 'checked' : ''}> Keep layout size when there are fewer events</label>
            `;
            repeatBox.querySelector('[data-tpl-repeat="mode"]').value = rep.mode || (rep.group ? 'rows' : 'none');
            repeatBox.querySelector('[data-tpl-repeat="order"]').value = rep.order === 'bottom-up' ? 'bottom-up' : 'top-down';
            repeatBox.querySelectorAll('[data-tpl-repeat]').forEach(inp => {
                const updateRepeat = () => {
                    if (!el.repeat) el.repeat = {};
                    const k = inp.dataset.tplRepeat;
                    if (k === 'group') {
                        let group = inp.value;
                        if (group === '__new__') {
                            const used = new Set((this._tplElements || []).map(item => item.repeat && item.repeat.group).filter(Boolean));
                            let n = 1;
                            while (used.has('repeat-group-' + n)) n += 1;
                            group = 'repeat-group-' + n;
                        }
                        if (group) {
                            el.repeat.group = group;
                            if (!el.repeat.mode || el.repeat.mode === 'none') el.repeat.mode = 'rows';
                        } else {
                            delete el.repeat.group;
                            el.repeat.mode = 'none';
                        }
                    } else {
                        const value = (k === 'hide_if_empty' || k === 'preserve_size') ? inp.checked : ((k === 'mode' || k === 'order') ? inp.value : Math.max(k === 'count' ? 1 : 0, Number(inp.value) || 0));
                        el.repeat[k] = value;
                        if (el.repeat.group) {
                            (this._tplElements || []).forEach(item => {
                                if (item !== el && item.repeat && item.repeat.group === el.repeat.group) {
                                    item.repeat[k] = value;
                                }
                            });
                        }
                    }
                    this._renderTemplateCanvas();
                    this._updateRawPropsView && this._updateRawPropsView();
                    this._renderTemplateElementProps();
                };
                inp.oninput = inp.tagName === 'SELECT' ? null : updateRepeat;
                inp.onchange = updateRepeat;
            });
        }
        setDetailsOpen('tpl-repeat-section', true);

        // ── Type selector (in position section header) + action buttons ──
        // Update type selector value
        const typeSel = document.querySelector('[data-tpl-el="type"]');
        if (typeSel) typeSel.value = el.type;
        // Wire type change
        document.querySelectorAll('[data-tpl-el="type"]').forEach(sel => {
            sel.onchange = () => {
                el.type = sel.value;
                Object.assign(el, this._templateElementDefaults(sel.value, el.x, el.y), { id: el.id, z: Number(el.z)||0 });
                this._renderTemplateCanvas();
                this._updateRawPropsView && this._updateRawPropsView();
                this._renderTemplateElementProps();
            };
        });
        // Wire action buttons
        const front = document.getElementById('tpl-el-front');
        if (front) front.onclick = () => { this._setTemplateElementLayer(el, 'front'); this._renderTemplateCanvas(); this._updateRawPropsView && this._updateRawPropsView(); };
        const back = document.getElementById('tpl-el-back');
        if (back) back.onclick = () => { this._setTemplateElementLayer(el, 'back'); this._renderTemplateCanvas(); this._updateRawPropsView && this._updateRawPropsView(); };
        const dup = document.getElementById('tpl-el-duplicate');
        if (dup && !dup.onclick) {
            dup.onclick = (e) => {
                e.stopPropagation();
                const el2 = this._selectedTemplateElement();
                if (!el2) { alert('No element selected to duplicate.'); return; }
                const copy = JSON.parse(JSON.stringify(el2));
                copy.id = 'tpl-el-' + Date.now();
                copy.x = this._snapTemplateValue((Number(copy.x)||0) + 20);
                copy.y = this._snapTemplateValue((Number(copy.y)||0) + 20);
                copy.z = this._nextTemplateElementZ();
                this._tplElements.push(copy);
                this._tplSelectedElementId = copy.id;
                this._renderTemplateCanvas();
                this._updateRawPropsView && this._updateRawPropsView();
                this._renderTemplateElementProps();
            };
        }
        const del = document.getElementById('tpl-el-delete');
        if (del && !del.onclick) {
            del.onclick = (e) => {
                e.stopPropagation();
                const el2 = this._selectedTemplateElement();
                if (!el2) return;
                this._tplElements = (this._tplElements||[]).filter(x => x.id !== el2.id);
                this._tplSelectedElementId = null;
                this._renderTemplateCanvas();
                this._updateRawPropsView && this._updateRawPropsView();
                this._renderTemplateElementProps();
            };
        }
    },

    updateTemplatePreview() {
        const preview = document.getElementById('tpl-preview');
        if (!preview) return;
        const props = this.collectTemplateProps() || {};
        // The Visual Template Builder should show only the user-configured visual elements.
        // Do not fall back to default weather/calendar iframe renderers here; that made custom
        // templates look like the default template until elements were added or re-rendered.
        this._renderTemplateCanvas();
        this._renderTemplateElementProps();
        const raw = document.getElementById('tpl-raw-props');
        if (raw) raw.textContent = JSON.stringify(props, null, 2);
    },

    _buildPreviewUrl(widgetType, props) {
        // Build a URL to the widget's HTML page with all the relevant props as query params.
        // This is the same format that player.html uses, so the preview shows exactly what the user will see.
        const params = new URLSearchParams();
        switch (widgetType) {
            case 'weather':
                params.set('lat', props.lat || 0);
                params.set('lon', props.lon || 0);
                params.set('template', props.template || '3day');
                params.set('color', props.color || '#ffffff');
                params.set('accent', props.accent || props.accent_color || '#4a90d9');
                params.set('location', props.location || props.weather_location || '');
                // Pass custom icons so the preview uses them
                if (this.currentTemplateAssets) {
                    const iconAssets = this.currentTemplateAssets.filter(a => a.kind === 'weather_icon' && a.url);
                    if (iconAssets.length) {
                        const iconMap = {};
                        iconAssets.forEach(a => { iconMap[a.key] = a.url; });
                        params.set('icons', JSON.stringify(iconMap));
                    }
                }
                return '/weather.html?' + params.toString();
            case 'calendar':
                params.set('ics', props.ics_url || '');
                params.set('template', props.template || 'list');
                params.set('color', props.color || '#ffffff');
                params.set('accent', props.accent || props.accent_color || '#e94560');
                params.set('maxevents', props.max_events || 5);
                params.set('incweather', props.include_weather === 'true' ? 'true' : 'false');
                params.set('lat', props.lat || 0);
                params.set('lon', props.lon || 0);
                return '/calendar.html?' + params.toString();
            case 'clock':
                // Clock widget is simple - inline render
                return null;
            case 'datetime':
                return null;
            case 'text':
                params.set('text', props.text || 'Sample text');
                params.set('color', props.color || '#ffffff');
                params.set('size', props.font_size || 24);
                params.set('weight', props.font_weight || 400);
                params.set('align', props.align || 'center');
                return null; // For now, fallback to inline
            case 'image':
                params.set('url', props.url || '');
                return null;
            case 'video':
                return null;
            case 'webpage':
                params.set('url', props.url || '');
                return null;
            case 'music':
                return null;
            default:
                return null;
        }
    },

    hexToRgb(hex) {
        const m = /^#?([a-f\d]{2})([a-f\d]{2})([a-f\d]{2})$/i.exec(hex);
        if (!m) return null;
        return { r: parseInt(m[1], 16), g: parseInt(m[2], 16), b: parseInt(m[3], 16) };
    },


    _isLocalWeatherIconUrl(url) {
        const value = String(url || '').trim();
        if (!(value.startsWith('/uploads/images/') || value.startsWith('/uploads/icons/'))) return false;
        return !value.includes('..') && !value.includes('\\') && !value.includes('\0');
    },

    _renderAssetsGrid() {
        const grid = document.getElementById('tpl-weather-icons');
        if (!grid) return;
        const WMO = [
            { code: 0, desc: 'Clear' }, { code: 1, desc: 'Mainly clear' }, { code: 2, desc: 'Partly cloudy' }, { code: 3, desc: 'Overcast' },
            { code: 45, desc: 'Fog' }, { code: 48, desc: 'Rime fog' },
            { code: 51, desc: 'Light drizzle' }, { code: 53, desc: 'Moderate drizzle' }, { code: 55, desc: 'Dense drizzle' },
            { code: 61, desc: 'Slight rain' }, { code: 63, desc: 'Moderate rain' }, { code: 65, desc: 'Heavy rain' },
            { code: 71, desc: 'Slight snow' }, { code: 73, desc: 'Moderate snow' }, { code: 75, desc: 'Heavy snow' },
            { code: 80, desc: 'Rain showers' }, { code: 81, desc: 'Heavy showers' }, { code: 82, desc: 'Violent showers' },
            { code: 95, desc: 'Thunderstorm' }, { code: 96, desc: 'Thunder + hail' }, { code: 99, desc: 'Severe thunder' },
        ];
        const currentByKey = {};
        this.currentTemplateAssets.forEach(a => { if (a.kind === 'weather_icon') currentByKey[a.key] = a.url; });
        const currentByKeyN = {};
        this.currentTemplateAssets.forEach(a => { if (a.kind === 'weather_icon_night') currentByKeyN[a.key] = a.url; });
        const dayGrid = document.getElementById('wmo-day-grid');
        const targetGrid = dayGrid || grid;
        targetGrid.innerHTML = WMO.map(w => {
            const url = currentByKey[String(w.code)] || '';
            const isCustom = !!url;
            return `<div class="weather-icon-tile" data-code="${w.code}" style="background:rgba(0,0,0,0.3); padding:0.4rem; border-radius:4px; display:flex; align-items:center; gap:0.4rem; border:1px solid ${isCustom ? 'var(--accent,#4a90d9)' : 'transparent'}; cursor:pointer;">
                <button type="button" class="weather-icon-pick" data-code="${w.code}" title="Upload icon for weather code ${w.code}" style="font-size:1.5rem; flex-shrink:0; min-width:36px; width:36px; height:36px; text-align:center; background:rgba(255,255,255,.06); border:1px solid rgba(255,255,255,.12); border-radius:6px; color:inherit; cursor:pointer; display:flex; align-items:center; justify-content:center; padding:0;">${isCustom ? `<img src="${this.escapeHtml(url)}" style="width:28px; height:28px; object-fit:contain;"  /><span style="display:none;">${this.getWmoEmoji(w.code)}</span>` : this.getWmoEmoji(w.code)}</button>
                <div style="flex:1; min-width:0;">
                    <div style="font-size:0.7rem; color:#aaa;">${w.code} — ${w.desc}</div>
                    <div style="display:flex; align-items:center; gap:.4rem; margin-top:.2rem;">
                        <span style="flex:1; min-width:0; font-size:.7rem; color:${isCustom ? 'var(--accent,#4a90d9)' : '#aaa'}; white-space:nowrap; overflow:hidden; text-overflow:ellipsis;">${isCustom ? 'Custom icon' : 'Default weather icon'}</span>
                        ${isCustom ? `<button type="button" class="btn btn-danger btn-sm weather-icon-delete" data-code="${w.code}" title="Delete custom icon and use the default weather icon" style="padding:.18rem .45rem; font-size:.68rem;">Delete</button>` : ''}
                    </div>
                </div>
            </div>`;
        }).join('');
        // CSP-safe: attach error handler via JS instead of inline onerror attribute
        grid.querySelectorAll('img').forEach(img => {
            img.addEventListener('error', () => {
                img.style.display = 'none';
                const fallback = img.nextElementSibling;
                if (fallback) fallback.style.display = '';
            });
        });
        const setIconUrl = (code, url) => {
            url = String(url || '').trim();
            const status = document.getElementById('tpl-icon-upload-status');
            if (url && !this._isLocalWeatherIconUrl(url)) {
                if (status) status.textContent = 'Use an uploaded local icon path from /uploads/images/ or /uploads/icons/.';
                return false;
            }
            const isNight = String(code).endsWith('_night');
                const realCode = isNight ? String(code).replace('_night', '') : String(code);
                const kind = isNight ? 'weather_icon_night' : 'weather_icon';
                const existing = this.currentTemplateAssets.find(a => a.kind === kind && a.key === realCode);
            if (url) {
                if (existing) { existing.url = url; }
                else { this.currentTemplateAssets.push({ kind: kind, key: realCode, url, preset_id: this.currentTemplateId || '' }); }
            } else if (existing) {
                if (existing.id) {
                    this.removedTemplateAssetIds = Array.from(new Set([...(this.removedTemplateAssetIds || []), existing.id]));
                }
                this.currentTemplateAssets = this.currentTemplateAssets.filter(a => a !== existing);
            }
            this._updateRawPropsView();
            this._renderTemplateCanvas();
            return true;
        };
        const openUploadForCode = (code) => {
            this._pendingWeatherIconCode = String(code);
            const status = document.getElementById('tpl-icon-upload-status');
            if (status) status.textContent = `Upload icon for code ${code}`;
            const file = document.getElementById('tpl-icon-upload');
            if (file) file.click();
        };
        // Wire click-to-upload on each icon/tile.
        grid.querySelectorAll('.weather-icon-pick').forEach(btn => btn.onclick = (ev) => { ev.preventDefault(); ev.stopPropagation(); openUploadForCode(btn.dataset.code); });
        // Populate night grid with existing night icons
        const nightGrid = document.getElementById('wmo-night-grid');
        if (nightGrid) {
            nightGrid.innerHTML = WMO.map(w => {
                const nkey = String(w.code) + '_night';
                const nurl = (currentByKeyN || {})[nkey] || '';
                const nisCustom = !!nurl;
                return `<div class="weather-icon-tile" data-code="${w.code}_night" style="background:rgba(0,0,0,0.3);padding:0.4rem;border-radius:4px;display:flex;align-items:center;gap:0.4rem;border:1px solid ${nisCustom?'var(--accent,#4a90d9)':'transparent'};cursor:pointer;">
                    <button type="button" class="weather-icon-pick" data-code="${w.code}_night" title="Night icon for ${w.code}" style="font-size:1.5rem;flex-shrink:0;min-width:36px;width:36px;height:36px;text-align:center;background:rgba(255,255,255,.06);border:1px solid rgba(255,255,255,.12);border-radius:6px;color:inherit;cursor:pointer;display:flex;align-items:center;justify-content:center;padding:0;">${nisCustom?`<img src="${this.escapeHtml(nurl)}" style="width:28px;height:28px;object-fit:contain;"  /><span style="display:none;">🌙</span>`:'<span style="font-size:1.4rem;">🌙</span>'}</button>
                    <div style="flex:1;min-width:0;"><div style="font-size:0.7rem;color:#aaa;">${w.code} ${w.desc} (night)</div><div style="font-size:.7rem;color:${nisCustom?'var(--accent,#4a90d9)':'#aaa'};">${nisCustom?'Custom night':'Default night'}</div></div>
                    ${nisCustom?`<button type="button" class="btn btn-danger btn-sm weather-icon-delete" data-code="${w.code}_night" title="Delete" style="padding:.18rem .45rem;font-size:.68rem;">Delete</button>`:''}
                </div>`;
            }).join('');
        }
        // Wire day/night tab toggle
        const nightTabBtn = document.getElementById('wmo-night-tab');
        const dayTabBtn = document.getElementById('wmo-day-tab');
        const dayGridEl = document.getElementById('wmo-day-grid');
        if (nightTabBtn && dayTabBtn) {
            nightTabBtn.addEventListener('click', () => {
                nightTabBtn.style.opacity = '1';
                dayTabBtn.style.opacity = '0.5';
                if (dayGridEl) dayGridEl.style.display = 'none';
                if (nightGrid) nightGrid.style.display = '';
                const status = document.getElementById('tpl-icon-upload-status');
                if (status) status.textContent = 'Night icons — uploaded here, shown after 7pm.';
            });
            dayTabBtn.addEventListener('click', () => {
                dayTabBtn.style.opacity = '1';
                nightTabBtn.style.opacity = '0.5';
                if (dayGridEl) dayGridEl.style.display = '';
                if (nightGrid) nightGrid.style.display = 'none';
                const status = document.getElementById('tpl-icon-upload-status');
                if (status) status.textContent = '';
            });
        }
        grid.querySelectorAll('.weather-icon-tile').forEach(tile => tile.onclick = (ev) => {
            if (ev.target && ev.target.closest && ev.target.closest('.weather-icon-delete')) return;
            openUploadForCode(tile.dataset.code);
        });
        // Delete custom icon overrides and fall back to the built-in/default WMO icon.
        grid.querySelectorAll('.weather-icon-delete').forEach(btn => {
            btn.onclick = (ev) => {
                ev.preventDefault();
                ev.stopPropagation();
                const code = btn.dataset.code;
                const status = document.getElementById('tpl-icon-upload-status');
                if (setIconUrl(code, '')) {
                    if (status) status.textContent = `Custom icon deleted for code ${code}. Save template to keep the default icon.`;
                    this._renderAssetsGrid();
                }
            };
        });
        const uploadBtn = document.getElementById('tpl-icon-upload-btn');
        if (uploadBtn) uploadBtn.onclick = () => {
            const code = this._pendingWeatherIconCode || (WMO[0] && String(WMO[0].code));
            openUploadForCode(code);
        };
        const uploadInput = document.getElementById('tpl-icon-upload');
        if (uploadInput && !uploadInput.dataset.tplWeatherUploadWired) {
            uploadInput.dataset.tplWeatherUploadWired = '1';
            uploadInput.onchange = async () => {
                const file = uploadInput.files && uploadInput.files[0];
                uploadInput.value = '';
                if (!file) return;
                const code = this._pendingWeatherIconCode;
                const status = document.getElementById('tpl-icon-upload-status');
                if (!code) { if (status) status.textContent = 'Pick a weather code first.'; return; }
                try {
                    if (status) status.textContent = `Uploading icon for code ${code}...`;
                    const entry = await API.uploadFile('images', file);
                    setIconUrl(code, entry.url);
                    if (status) status.textContent = `Icon uploaded for code ${code}. Save template to keep it.`;
                    this._renderAssetsGrid();
                } catch (err) {
                    if (status) status.textContent = `Upload failed: ${err.message}`;
                }
            };
        }
    },
    // Backward-compat alias (old name)
    renderWeatherIconGrid() { this._renderAssetsGrid(); },

    getWmoEmoji(code) {
        const map = { 0:'☀️', 1:'🌤️', 2:'⛅', 3:'☁️', 45:'🌫️', 48:'🌫️', 51:'🌦️', 53:'🌦️', 55:'🌦️', 61:'🌧️', 63:'🌧️', 65:'🌧️', 71:'❄️', 73:'❄️', 75:'❄️', 80:'🌦️', 81:'🌧️', 82:'🌧️', 95:'⛈️', 96:'⛈️', 99:'⛈️' };
        return map[code] || '❓';
    },

    async downloadTemplate(presetId) {
        if (!presetId) {
            alert('Save the template first to download it.');
            return;
        }
        // Use a native navigation download instead of fetch->Blob->synthetic click.
        // The browser handles Content-Disposition, cookies, and filename correctly,
        // and this avoids popup/download blocking on some display/browser configs.
        window.location.href = API.exportWidgetPresetUrl(presetId);
    },

};

// Boot
document.addEventListener('DOMContentLoaded', () => App.init());


// Connected Clients process check + error log + downloadable report
// Captures every load / search / assignment / command action result so users
// can download a report from the server UI.
(function extendConnectedClientsDiagnostics(){
  if (typeof App === 'undefined') return;
  if (!App._clientDiagnostics) {
    App._clientDiagnostics = {
      // Each entry: { ts, action, status, detail, durationMs, clientId, info }
      events: [],
      // Cap retained events so the UI log doesn't grow unbounded.
      maxEvents: 500,
      // Per-process checks. lastCheckAt is used to throttle UI polling.
      lastCheckAt: null,
      lastReport: null,
      errors: 0,
      warnings: 0,
      checksRun: 0,
    };
    App.clientDiagnostics = {
      log(action, status, detail, extra) {
        const entry = {
          ts: new Date().toISOString(),
          action: String(action || ''),
          status: String(status || 'info'),
          detail: typeof detail === 'string' ? detail : (() => { try { return JSON.stringify(detail); } catch (_e) { return String(detail); } })(),
          clientId: extra && extra.clientId ? String(extra.clientId) : null,
          durationMs: extra && Number.isFinite(extra.durationMs) ? extra.durationMs : null,
        };
        const diag = App._clientDiagnostics;
        diag.events.push(entry);
        if (diag.events.length > diag.maxEvents) {
          diag.events.splice(0, diag.events.length - diag.maxEvents);
        }
        if (entry.status === 'error') diag.errors++;
        else if (entry.status === 'warning') diag.warnings++;
        try {
          if (typeof console !== 'undefined') {
            const fn = entry.status === 'error' ? console.error
              : entry.status === 'warning' ? console.warn
              : console.info;
            fn('[client-diag]', entry.action, entry.status, entry.detail || '', entry);
          }
        } catch (_e) {}
      },
      async runChecks() {
        const diag = App._clientDiagnostics;
        const startedAt = Date.now();
        const report = {
          generatedAt: new Date().toISOString(),
          browser: {
            ua: (typeof navigator !== 'undefined' && navigator.userAgent) || '',
            onLine: (typeof navigator !== 'undefined' && navigator.onLine) !== false,
            location: (typeof window !== 'undefined' && window.location) ? window.location.origin : '',
          },
          api: {},
          checks: [],
          clients: {
            known: Array.isArray(App._lastClients) ? App._lastClients.length : 0,
            online: 0,
            stale: 0,
            offline: 0,
            candidates: 0,
          },
          errors: 0,
          warnings: 0,
          eventCount: diag.events.length,
        };
        let passed = 0, failed = 0, warned = 0;
        const safeCheck = async (name, fn) => {
          const t0 = Date.now();
          try {
            const out = await fn();
            const dur = Date.now() - t0;
            const ok = !!(out && out.ok !== false);
            const status = ok ? 'pass' : 'warn';
            const entry = { name, status, durationMs: dur, detail: out && out.detail ? out.detail : (out && out.note ? out.note : null) };
            report.checks.push(entry);
            if (ok) passed++; else warned++;
            App.clientDiagnostics.log('check:' + name, status, entry.detail || '', { durationMs: dur });
            return entry;
          } catch (e) {
            const dur = Date.now() - t0;
            const entry = { name, status: 'fail', durationMs: dur, detail: e && e.message ? e.message : String(e) };
            report.checks.push(entry);
            failed++;
            App.clientDiagnostics.log('check:' + name, 'error', entry.detail, { durationMs: dur });
            return entry;
          }
        };
        await safeCheck('auth-token', async () => {
          const tok = (typeof localStorage !== 'undefined') ? (localStorage.getItem('signage.authToken') || localStorage.getItem('ripley.authToken') || '') : '';
          return { ok: !!tok, detail: tok ? 'token present in localStorage' : 'no auth token in localStorage' };
        });
        await safeCheck('server-reachable', async () => {
          try {
            const r = await fetch('/api/version', { cache: 'no-store' });
            report.api.version = { status: r.status, ok: r.ok };
            return { ok: r.ok, detail: `/api/version HTTP ${r.status}` };
          } catch (e) {
            return { ok: false, detail: 'fetch failed: ' + (e && e.message ? e.message : String(e)) };
          }
        });
        await safeCheck('clients-list', async () => {
          try {
            const r = await fetch('/api/clients', { cache: 'no-store' });
            report.api.clients = { status: r.status, ok: r.ok };
            if (!r.ok) return { ok: false, detail: `/api/clients HTTP ${r.status}` };
            const arr = await r.json();
            report.clients.known = Array.isArray(arr) ? arr.length : 0;
            for (const c of (arr || [])) {
              const s = (c && c.status || '').toLowerCase();
              if (s === 'online') report.clients.online++;
              else if (s === 'stale') report.clients.stale++;
              else if (s === 'offline') report.clients.offline++;
              if (c && c.enrolled === false) report.clients.candidates++;
            }
            return { ok: true, detail: `${report.clients.known} client(s) known` };
          } catch (e) {
            return { ok: false, detail: 'fetch failed: ' + (e && e.message ? e.message : String(e)) };
          }
        });
        await safeCheck('clients-search', async () => {
          try {
            const r = await fetch('/api/clients/search', { cache: 'no-store' });
            report.api.clientsSearch = { status: r.status, ok: r.ok };
            if (!r.ok) return { ok: false, detail: `/api/clients/search HTTP ${r.status}` };
            return { ok: true, detail: 'client detection search endpoint reachable' };
          } catch (e) {
            return { ok: false, detail: 'fetch failed: ' + (e && e.message ? e.message : String(e)) };
          }
        });
        await safeCheck('layouts-cache', async () => {
          const layouts = App._clientLayouts || [];
          return { ok: Array.isArray(layouts), detail: `${layouts.length} layout(s) cached for assignment dropdown` };
        });
        await safeCheck('last-render', async () => {
          if (!diag.lastReport || !diag.lastReport.generatedAt) return { ok: true, detail: 'no prior report (first run)' };
          return { ok: true, detail: 'previous run at ' + diag.lastReport.generatedAt };
        });
        report.totals = { passed, warned, failed, durationMs: Date.now() - startedAt };
        report.errors = diag.errors;
        report.warnings = diag.warnings;
        report.eventCount = diag.events.length;
        diag.lastCheckAt = new Date().toISOString();
        diag.lastReport = report;
        diag.checksRun++;
        return report;
      },
      downloadReport() {
        const diag = App._clientDiagnostics;
        const report = diag.lastReport || { generatedAt: new Date().toISOString(), note: 'no checks have been run yet' };
        const payload = {
          report,
          events: diag.events.slice(),
          totals: { errors: diag.errors, warnings: diag.warnings, eventCount: diag.events.length, checksRun: diag.checksRun },
        };
        const text = JSON.stringify(payload, null, 2);
        const ts = new Date().toISOString().replace(/[:.]/g, '-');
        const filename = `ripley-client-diagnostics-${ts}.json`;
        const blob = new Blob([text], { type: 'application/json' });
        const url = URL.createObjectURL(blob);
        const a = document.createElement('a');
        a.href = url;
        a.download = filename;
        document.body.appendChild(a);
        a.click();
        setTimeout(() => { try { URL.revokeObjectURL(url); a.remove(); } catch (_e) {} }, 1500);
        App.clientDiagnostics.log('report:download', 'info', filename);
        return filename;
      },
      async uploadReport() {
        // Optional: POST the latest report to the server so it can be archived.
        const diag = App._clientDiagnostics;
        const report = diag.lastReport || { generatedAt: new Date().toISOString() };
        const payload = { report, events: diag.events.slice() };
        try {
          const r = await fetch('/api/clients/diagnostics', {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify(payload),
          });
          const ok = r.ok;
          App.clientDiagnostics.log('report:upload', ok ? 'info' : 'error', `/api/clients/diagnostics HTTP ${r.status}`);
          return { ok, status: r.status };
        } catch (e) {
          App.clientDiagnostics.log('report:upload', 'error', 'fetch failed: ' + (e && e.message ? e.message : String(e)));
          return { ok: false, error: e && e.message ? e.message : String(e) };
        }
      },
      clear() {
        const diag = App._clientDiagnostics;
        diag.events = [];
        diag.errors = 0;
        diag.warnings = 0;
        diag.lastReport = null;
        App.clientDiagnostics.log('diagnostics:clear', 'info', 'cleared');
      },
    };
  }
})();

// Connected Clients settings panel
(function extendConnectedClientsPanel(){
  if (typeof App === 'undefined' || typeof API === 'undefined') return;
  API.getConnectedClients = async function(search=false) {
    const res = await fetch(search ? '/api/clients/search' : '/api/clients');
    if (!res.ok) throw new Error('Failed to fetch connected clients');
    return res.json();
  };

  App.loadConnectedClientsPage = async function(){
    this.wireConnectedClientsPanel();
    await this.loadConnectedClients(false);
  };

  App.wireConnectedClientsPanel = function(){
    const btn = document.getElementById('btn-client-search');
    if (btn && !btn.dataset.wired) {
      btn.dataset.wired = '1';
      btn.onclick = () => this.loadConnectedClients(true);
    }
  };

  App.loadConnectedClients = async function(search){
    const list = document.getElementById('connected-clients-list');
    const status = document.getElementById('client-search-status');
    const btn = document.getElementById('btn-client-search');
    if (!list) return;
    const t0 = Date.now();
    if (search) {
      if (btn) { btn.disabled = true; btn.textContent = 'Searching…'; }
      if (status) status.textContent = 'Scanning local network and refreshing neighbor table…';
    } else if (status && !status.textContent) status.textContent = 'Showing known connected/seen clients.';
    App.clientDiagnostics.log(search ? 'clients:search:start' : 'clients:list:start', 'info', search ? 'detection search requested' : 'initial list load');
    try {
      const clients = await API.getConnectedClients(search);
      this._lastClients = Array.isArray(clients) ? clients : [];
      this.renderConnectedClients(clients);
      const dur = Date.now() - t0;
      if (status) status.textContent = search ? `Search complete: ${clients.length} client(s) found.` : `${clients.length} client(s) known.`;
      App.clientDiagnostics.log(search ? 'clients:search:ok' : 'clients:list:ok', 'info',
        `${Array.isArray(clients) ? clients.length : 0} client(s)`, { durationMs: dur });
    } catch (e) {
      const dur = Date.now() - t0;
      list.innerHTML = `<div class="client-empty">Failed to load clients: ${esc(e.message)}</div>`;
      if (status) status.textContent = 'Client detection failed.';
      App.clientDiagnostics.log(search ? 'clients:search:error' : 'clients:list:error', 'error',
        e && e.message ? e.message : String(e), { durationMs: dur });
    } finally {
      if (btn) { btn.disabled = false; btn.textContent = '🔍 Client Detection Search'; }
    }
  };

  // Render per-client diagnostics log into the client detail modal.
  // Returns an HTML string showing only events for the given clientId.
  App.renderClientDiagnosticsLog = function(clientId) {
    const diag = App._clientDiagnostics;
    if (!diag) return '<div class="client-diag-empty">Diagnostics not initialized.</div>';
    const events = diag.events.filter(e => !clientId || e.clientId === clientId);
    const recent = events.slice(-30).reverse().map(e => {
      const color = e.status === 'error' ? '#fca5a5' : e.status === 'warning' ? '#fde68a' : '#86efac';
      const dur = e.durationMs != null ? ` <span style="color:#64748b;">(${e.durationMs}ms)</span>` : '';
      return `<div class="client-diag-row"><span style="color:${color};font-weight:600;">[${esc(e.status)}]</span> <span>${esc(e.action)}</span> ${e.detail ? `<span style="color:#94a3b8;">— ${esc(e.detail)}</span>` : ''}${dur} <span style="color:#64748b;">@ ${esc(e.ts.replace('T',' ').replace('Z',''))}</span></div>`;
    }).join('') || '<div class="client-diag-empty">No diagnostic events recorded for this client yet.</div>';
    const errorCount = events.filter(e => e.status === 'error').length;
    const warnCount = events.filter(e => e.status === 'warning').length;
    return `
      <div class="client-diag-log" style="max-height:240px; overflow:auto; padding:.5rem; border:1px solid rgba(148,163,184,.18); border-radius:8px; background:rgba(2,6,23,.45); font-family:ui-monospace,SFMono-Regular,Menlo,monospace; font-size:.78rem;">${recent}</div>
      <div style="display:flex; gap:.5rem; margin-top:.5rem; font-size:.78rem; color:var(--text-dim,#94a3b8);">
        <span>events: ${events.length}</span>
        <span style="color:#fca5a5;">errors: ${errorCount}</span>
        <span style="color:#fde68a;">warnings: ${warnCount}</span>
      </div>
    `;
  };

  App.renderConnectedClients = function(clients){
    const list = document.getElementById('connected-clients-list');
    if (!list) return;
    const esc = (v) => String(v ?? '').replace(/[&<>"']/g, c => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));
    const fmtBytes = (b) => {
      if (b === null || b === undefined) return '—';
      if (b > 1e9) return (b/1e9).toFixed(1)+' GB';
      if (b > 1e6) return (b/1e6).toFixed(1)+' MB';
      return b+' KB';
    };
    const fmtUptime = (s) => {
      if (s === null || s === undefined) return 'Unknown';
      const d=Math.floor(s/86400), h=Math.floor((s%86400)/3600), m=Math.floor((s%3600)/60);
      return `${d}d ${h}h ${m}m`;
    };
    if (!clients || !clients.length) {
      list.innerHTML = '<div class="client-empty">No connected clients found yet. Run Client Detection Search.</div>';
      return;
    }
    list.innerHTML = clients.map(c => {
      return `<div class="client-card" data-client-id="${esc(c.id)}">
        <div class="client-main">
          <div><div class="client-name">${esc(c.name)}</div><div class="client-sub">${esc(c.address)}</div></div>
        </div>
        <div class="client-last-seen">Last seen: ${esc(c.last_seen)}</div>
      </div>`;
    }).join('');
  };
})();
// Connected Clients enrollment/status UI v2
(function extendConnectedClientsEnrollmentUi(){
  if (typeof App === 'undefined') return;
  App.renderConnectedClients = function(clients){
    const list = document.getElementById('connected-clients-list');
    if (!list) return;
    const esc = (v) => String(v ?? '').replace(/[&<>"']/g, c => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));
    const fmtBytes = (b) => {
      if (b === null || b === undefined) return '—';
      if (b > 1e9) return (b/1e9).toFixed(1)+' GB';
      if (b > 1e6) return (b/1e6).toFixed(1)+' MB';
      return b+' KB';
    };
    const fmtUptime = (s) => {
      if (s === null || s === undefined) return 'Unknown';
      const d=Math.floor(s/86400), h=Math.floor((s%86400)/3600), m=Math.floor((s%3600)/60);
      return `${d}d ${h}h ${m}m`;
    };
    if (!clients || !clients.length) {
      list.innerHTML = '<div class="client-empty">No connected clients found yet. Run Client Detection Search.</div>';
      return;
    }
    const order = {online:0, stale:1, offline:2, 'not enrolled':3, seen:4};
    clients = clients.slice().sort((a,b)=>(order[a.status]??9)-(order[b.status]??9));
    list.innerHTML = clients.map(c => {
      const enrolled = c.enrolled !== false;
      if (c.is_virtual) {
        const virtualUrl = `${window.location.origin}/player.html?client_id=${encodeURIComponent(c.id)}`;
        return `<div class="client-card virtual-client-card enrolled" data-client-id="${esc(c.id)}">
          <div class="client-card-kicker"><span>VIRTUAL CLIENT</span></div>
          <div class="client-main">
            <div class="client-identity"><div>
              <div class="client-name">${esc(c.name)}</div>
              <div class="client-sub">Virtual Client</div>
            </div></div>
          </div>
          <div class="virtual-client-card-content">
            <label>URL</label>
            <div class="virtual-client-url-row">
              <input type="text" readonly value="${esc(virtualUrl)}" aria-label="Virtual client URL">
              <button type="button" class="btn btn-secondary btn-sm virtual-client-copy">Copy</button>
            </div>
            <button type="button" class="btn btn-danger btn-sm virtual-client-delete">Delete Virtual Client</button>
          </div>
        </div>`;
      }
      return `<div class="client-card ${esc(c.status).replace(/\s+/g,'-')} ${enrolled?'enrolled':'candidate'}" data-client-id="${esc(c.id)}">
            <div class="client-card-kicker"><span>${enrolled ? 'CLIENT' : 'PENDING ADOPTION'}</span></div>
        <div class="client-main">
          <div class="client-identity"><div>
            <div class="client-name">${esc(c.name)}</div>
            <div class="client-sub">${esc(c.address)} ${c.version ? '· v'+esc(c.version) : ''}</div>
          </div></div>
        </div>
        ${!enrolled ? `<div class="client-pairing-code"><span>PAIRING CODE</span><strong>${esc(c.pairing_code || '—')}</strong><small>Confirm this matches the code shown on the device.</small></div>` : ''}
        <div class="client-last-seen">Last seen: ${esc(c.last_seen)}</div>
        ${enrolled ? '<div class="client-card-footer"><span>DEVICE DETAILS</span><span>Click card to inspect displays and controls →</span></div>' : `<div class="client-action-row"><button type="button" class="btn btn-primary btn-sm client-adopt" data-client-id="${esc(c.id)}">Adopt Client</button><button type="button" class="btn btn-danger btn-sm client-reject" data-client-id="${esc(c.id)}">Reject</button></div><div class="client-action-hint">Confirm the pairing code on the device before adopting it.</div>`}
      </div>`;
    }).join('');
    list.querySelectorAll('.virtual-client-copy').forEach(btn => {
      btn.addEventListener('click', async (e) => {
        e.stopPropagation();
        const input = btn.closest('.virtual-client-url-row')?.querySelector('input');
        if (!input) return;
        try { await navigator.clipboard.writeText(input.value); btn.textContent = 'Copied'; }
        catch (_) { input.select(); document.execCommand('copy'); btn.textContent = 'Copied'; }
        setTimeout(() => { btn.textContent = 'Copy'; }, 1500);
      });
    });
    list.querySelectorAll('.virtual-client-delete').forEach(btn => {
      btn.addEventListener('click', async (e) => {
        e.stopPropagation();
        const card = btn.closest('.virtual-client-card');
        const client = clients.find(x => x.id === card?.dataset.clientId);
        if (!client || !await App.confirm(`Delete virtual client "${client.name || client.id}"?`, 'This removes the virtual client and its display.')) return;
        try {
          const response = await fetch(`/api/clients/${encodeURIComponent(client.id)}/virtual`, { method: 'DELETE' });
          const body = await response.json().catch(() => ({}));
          if (!response.ok) throw new Error(body.error || `HTTP ${response.status}`);
          await App.loadConnectedClients(false);
        } catch (error) {
          if (typeof App.setStatus === 'function') App.setStatus('Delete failed: ' + error.message, true);
          else alert('Delete failed: ' + error.message);
        }
      });
    });
    list.querySelectorAll('.client-adopt').forEach(btn => {
      btn.addEventListener('click', async (e) => {
        e.stopPropagation();
        const clientId = btn.dataset.clientId;
        if (!clientId) return;
        btn.disabled = true;
        btn.textContent = 'Adopting…';
        try {
          await API.adoptClient(clientId);
          await App.loadConnectedClients(false);
        } catch (error) {
          btn.disabled = false;
          btn.textContent = 'Adopt Client';
          if (typeof App.setStatus === 'function') App.setStatus('Adoption failed: ' + error.message, true);
          else alert('Adoption failed: ' + error.message);
        }
      });
    });
    list.querySelectorAll('.client-reject').forEach(btn => {
      btn.addEventListener('click', async (e) => {
        e.stopPropagation();
        const clientId = btn.dataset.clientId;
        if (!clientId) return;
        if (typeof App.confirm === 'function' && !await App.confirm('Reject pending client?', 'This removes the unadopted candidate from the server.')) return;
        btn.disabled = true;
        try {
          const response = await fetch(`/api/clients/${encodeURIComponent(clientId)}`, { method: 'DELETE' });
          const body = await response.json().catch(() => ({}));
          if (!response.ok) throw new Error(body.error || `HTTP ${response.status}`);
          await App.loadConnectedClients(false);
        } catch (error) {
          btn.disabled = false;
          if (typeof App.setStatus === 'function') App.setStatus('Reject failed: ' + error.message, true);
          else alert('Reject failed: ' + error.message);
        }
      });
    });
  };
})();

// Connected Clients install-command UI v3
(function extendConnectedClientInstallCommands(){
  if (typeof App === 'undefined') return;
  App.buildClientInstallCommands = function(opts){
    opts = opts || {};
    const server = String(opts.server || window.location.origin).replace(/\/+$/,'');
    const maxDisplays = Number.isFinite(opts.maxDisplays) ? opts.maxDisplays : null;
    // 1. Bundle install — canonical, recommended. No RIPLEY_SERVER_URL needed
    //    because the bundle's bootstrap script installs without prompting.
    const bundle = `curl --fail --show-error --location --proto '=https' --tlsv1.2 -o /tmp/ripley-bundle-install.sh ${server}/install/player-client-bundle-install.sh && RIPLEY_MAX_DISPLAYS=${maxDisplays || 2} bash /tmp/ripley-bundle-install.sh`;
    // 2. Direct one-liner — installer will prompt for URL on TTY, install CA,
    //    add groups, and start seatd.
    const oneLiner = `RIPLEY_MAX_DISPLAYS=${maxDisplays || 2} curl -fsSL ${server}/install/player-client.sh | bash`;
    // 3. Offline-friendly — assumes RIPLEY_SERVER_URL already set in env.
    const withEnv = `RIPLEY_SERVER_URL=${server} RIPLEY_MAX_DISPLAYS=${maxDisplays || 2} curl -fsSL ${server}/install/player-client.sh | bash`;
    return {
      server: server,
      maxDisplays: maxDisplays || 2,
      bundle: bundle,
      oneLiner: oneLiner,
      withEnv: withEnv,
      recommended: bundle,
      // Legacy compatibility: this is what the older Copy Install Command
      // button used to emit. Some external notes/screenshots still reference it.
      legacy: `RIPLEY_SERVER_URL=${server} curl -fsSL ${server}/install/player-client.sh | bash`,
    };
  };

  App.copyText = async function(text){
    if (typeof copyToClipboard === 'function') {
      try { return await copyToClipboard(text); } catch (_e) {}
    }
    try {
      if (navigator && navigator.clipboard && navigator.clipboard.writeText) {
        await navigator.clipboard.writeText(text);
        return true;
      }
    } catch (_e) {}
    return false;
  };

    App.copyClientInstallCommand = async function(ip){
    const cmds = (typeof this.buildClientInstallCommands === 'function')
      ? this.buildClientInstallCommands()
      : { legacy: `RIPLEY_SERVER_URL=${window.location.origin} curl -fsSL ${window.location.origin}/install/player-client.sh | bash` };
    const cmd = cmds.recommended || cmds.bundle || cmds.legacy;
    if (await App.copyText(cmd)) alert('Install command copied to clipboard.');
    else alert('Clipboard copy failed: browser blocked the copy. The install command is shown above — please copy it manually.');
  };

  const previous = App.renderConnectedClients;
  App.renderConnectedClients = function(clients){
    previous.call(this, clients);
    const list = document.getElementById('connected-clients-list');
    if (!list) return;
    const cmds = (typeof this.buildClientInstallCommands === 'function')
      ? this.buildClientInstallCommands()
      : { bundle: `RIPLEY_SERVER_URL=${window.location.origin} curl -fsSL ${window.location.origin}/install/player-client.sh | bash`, oneLiner: '' };
    const escCmd = (s) => String(s).replace(/[&<>"']/g, c => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));
    list.querySelectorAll('.client-card.candidate').forEach(card => {
      const sub = card.querySelector('.client-sub');
      const ipMatch = sub ? sub.textContent.match(/(?:\d{1,3}\.){3}\d{1,3}/) : null;
      const ip = ipMatch ? ipMatch[0] : '';
      const box = document.createElement('div');
      box.className = 'client-install-box';
      box.innerHTML = `
        <div class="client-install-title">Install / Enroll Command</div>
        <div class="client-install-tabs">
          <button type="button" class="btn btn-secondary btn-sm client-install-tab" data-which="bundle">Bundle (recommended)</button>
          <button type="button" class="btn btn-ghost btn-sm client-install-tab" data-which="oneLiner">One-liner</button>
        </div>
        <code class="client-install-cmd" data-which="bundle">${escCmd(cmds.bundle)}</code>
        <code class="client-install-cmd" data-which="oneLiner" style="display:none">${escCmd(cmds.oneLiner)}</code>
        <div class="client-install-actions">
          <button class="btn btn-secondary btn-sm client-install-copy">Copy Command</button>
          <button class="btn btn-ghost btn-sm client-install-toggle" type="button">Show one-liner</button>
        </div>
        <div class="client-install-note">Run this on ${esc(ip || 'the target player')} as the player user. The installer detects the OS, adds required groups, starts a seat daemon, installs the CA, and registers the client. Set <code>RIPLEY_MAX_DISPLAYS</code> for multi-display boxes.</div>
      `;
      const codeEls = box.querySelectorAll('.client-install-cmd');
      const tabs = box.querySelectorAll('.client-install-tab');
      const toggle = box.querySelector('.client-install-toggle');
      const switchTo = (which) => {
        codeEls.forEach(el => { el.style.display = (el.dataset.which === which) ? '' : 'none'; });
        tabs.forEach(t => { t.classList.toggle('btn-secondary', t.dataset.which === which); t.classList.toggle('btn-ghost', t.dataset.which !== which); });
        toggle.textContent = which === 'bundle' ? 'Show one-liner' : 'Show bundle';
      };
      tabs.forEach(t => { t.onclick = () => switchTo(t.dataset.which); });
      toggle.onclick = () => {
        const visible = box.querySelector('.client-install-cmd:not([style*="display: none"])');
        switchTo(visible && visible.dataset.which === 'bundle' ? 'oneLiner' : 'bundle');
      };
      box.querySelector('.client-install-copy').onclick = async () => {
        const visible = box.querySelector('.client-install-cmd:not([style*="display: none"])');
        const which = visible ? visible.dataset.which : 'bundle';
        const cmd = cmds[which] || cmds.bundle;
        if (await App.copyText(cmd)) {
          box.querySelector('.client-install-copy').textContent = '✓ Copied';
          setTimeout(() => { const b = box.querySelector('.client-install-copy'); if (b) b.textContent = 'Copy Command'; }, 1500);
        }
      };
      card.appendChild(box);
    });
  };
})();

// Connected Clients layout assignment UI v4
(function extendClientLayoutAssignmentUi(){
  if (typeof App === 'undefined' || typeof API === 'undefined') return;
  API.setClientAssignment = async function(clientId, layoutId){
    const t0 = Date.now();
    App.clientDiagnostics.log('assignment:set:start', 'info', layoutId || '(clear)', { clientId });
    try {
      const res = await fetch(`/api/clients/${encodeURIComponent(clientId)}/assignment`, {method:'PUT', headers:{'Content-Type':'application/json'}, body: JSON.stringify({layout_id: layoutId || null})});
      const dur = Date.now() - t0;
      if (!res.ok) {
        const body = await res.text();
        App.clientDiagnostics.log('assignment:set:error', 'error', `HTTP ${res.status} ${body}`, { clientId, durationMs: dur });
        throw new Error(body || ('HTTP ' + res.status));
      }
      App.clientDiagnostics.log('assignment:set:ok', 'info', `HTTP ${res.status}`, { clientId, durationMs: dur });
      return res.json();
    } catch (e) {
      App.clientDiagnostics.log('assignment:set:error', 'error', e && e.message ? e.message : String(e), { clientId, durationMs: Date.now() - t0 });
      throw e;
    }
  };
  const priorLoad = App.loadConnectedClients;
  App.loadConnectedClients = async function(search){
    // The client list is the primary view. Do not block it on the full layout
    // catalog; layout data is fetched when assignment/bulk controls need it.
    return priorLoad.call(this, search);
  };
  const previous = App.renderConnectedClients;
  App.renderConnectedClients = function(clients){
    previous.call(this, clients);
    const layouts = this._clientLayouts || [];
    document.querySelectorAll('.client-card.enrolled:not(.virtual-client-card)').forEach((card, idx) => {
      const c = clients.filter(x => x.enrolled !== false).sort((a,b)=>({online:0,stale:1,offline:2}[a.status]??9)-({online:0,stale:1,offline:2}[b.status]??9))[idx];
      if (!c || card.querySelector('.client-assignment-box')) return;
      const box = document.createElement('div');
      box.className = 'client-assignment-box';
const btn = document.createElement('button');
      btn.className = 'btn btn-secondary';
      btn.style.cssText = 'font-size:0.8rem;padding:0.3rem 0.75rem;';
      btn.textContent = c.assigned_layout_id ? 'Reassign Layout \u2192' : '+ Assign Layout';
      btn.addEventListener('click', () => App.openClientDetail(c.id));
      box.appendChild(btn);
      const hint = card.querySelector('.client-last-seen');
      if (hint) hint.insertAdjacentElement('afterend', box); else card.appendChild(box);
    });
  };
})();

// Connected Clients bulk assignment + nudge UI v5
(function extendBulkAssignmentUi(){
  if (typeof App === 'undefined' || typeof API === 'undefined') return;
  if (!App._bulkSelection) App._bulkSelection = new Set();
  API.bulkAssignClients = async function(clientIds, layoutId){
    const t0 = Date.now();
    App.clientDiagnostics.log('assignment:bulk:start', 'info', `${(clientIds||[]).length} client(s) -> ${layoutId || '(clear)'}`);
    try {
      const res = await fetch('/api/clients/assign-bulk', {method:'POST', headers:{'Content-Type':'application/json'}, body: JSON.stringify({client_ids: clientIds, layout_id: layoutId || null})});
      const dur = Date.now() - t0;
      if (!res.ok) {
        const body = await res.text();
        App.clientDiagnostics.log('assignment:bulk:error', 'error', `HTTP ${res.status} ${body}`, { durationMs: dur });
        throw new Error(body || ('HTTP ' + res.status));
      }
      App.clientDiagnostics.log('assignment:bulk:ok', 'info', `HTTP ${res.status}`, { durationMs: dur });
      return res.json();
    } catch (e) {
      App.clientDiagnostics.log('assignment:bulk:error', 'error', e && e.message ? e.message : String(e), { durationMs: Date.now() - t0 });
      throw e;
    }
  };
  API.nudgeClient = async function(clientId){
    const t0 = Date.now();
    App.clientDiagnostics.log('nudge:start', 'info', clientId, { clientId });
    try {
      const res = await fetch(`/api/clients/${encodeURIComponent(clientId)}/nudge`, {method:'POST'});
      const dur = Date.now() - t0;
      if (!res.ok) {
        const body = await res.text();
        App.clientDiagnostics.log('nudge:error', 'error', `HTTP ${res.status} ${body}`, { clientId, durationMs: dur });
        throw new Error(body || ('HTTP ' + res.status));
      }
      App.clientDiagnostics.log('nudge:ok', 'info', `HTTP ${res.status}`, { clientId, durationMs: dur });
      return res.json();
    } catch (e) {
      App.clientDiagnostics.log('nudge:error', 'error', e && e.message ? e.message : String(e), { clientId, durationMs: Date.now() - t0 });
      throw e;
    }
  };
  const previous = App.renderConnectedClients;
  App.renderConnectedClients = function(clients){
    previous.call(this, clients);
    const list = document.getElementById('connected-clients-list');
    if (!list) return;
    const esc = (v) => String(v ?? '').replace(/[&<>"']/g, c => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));
    let layouts = this._clientLayouts || [];
    list.querySelectorAll('.client-card.enrolled:not(.virtual-client-card)').forEach(card => {
      if (card.querySelector('.client-bulk-toggle')) return;
      const idMatch = (card.querySelector('.client-name')?.textContent || '').trim();
      const id = card.dataset.clientId || idMatch;
      const clientObj = clients.find(c => c.name === idMatch || c.id === idMatch);
      if (clientObj) card.dataset.clientId = clientObj.id;
      const head = card.querySelector('.client-badges');
      const toggle = document.createElement('label');
      toggle.className = 'client-bulk-toggle';
      toggle.innerHTML = `<input type="checkbox" class="client-bulk-check"> Select`;
      head?.insertAdjacentElement('afterbegin', toggle);
      const actions = document.createElement('div');
      actions.className = 'client-actions';
      actions.innerHTML = `<button class="btn btn-secondary btn-sm client-nudge">Nudge</button><button class="btn btn-ghost btn-sm client-preview">Preview</button><button class="btn btn-danger btn-sm client-restart" title="Restart the client device">Restart Client</button>`;
      const actionAnchor = card.querySelector('.client-card-footer') || card.querySelector('.client-last-seen');
      if (actionAnchor) actionAnchor.insertAdjacentElement('beforebegin', actions); else card.appendChild(actions);
      const nudgeBtn = actions.querySelector('.client-nudge');
      const restartBtn = actions.querySelector('.client-restart');
      const previewBtn = actions.querySelector('.client-preview');
      const status = card.querySelector('.client-assignment-status');
      if (clientObj) {
        nudgeBtn.onclick = async () => {
          nudgeBtn.disabled = true;
          const prev = nudgeBtn.textContent;
          nudgeBtn.textContent = 'Nudging…';
          try { const r = await API.nudgeClient(clientObj.id); nudgeBtn.textContent = 'Nudged ✓'; if (status) status.textContent = `Nudged client ${clientObj.id} (assigned: ${r.layout_id || 'none'}). It will pull on next heartbeat.`; }
          catch(e) { nudgeBtn.textContent = 'Nudge failed'; if (status) status.textContent = 'Nudge failed: '+e.message; }
          finally { setTimeout(() => { nudgeBtn.textContent = prev; nudgeBtn.disabled = false; }, 3000); }
        };
        restartBtn.onclick = async (event) => {
          event.stopPropagation();
          if (!await App.confirm(`Restart client "${clientObj.name || clientObj.id}"?`, 'This will reboot the client device and temporarily stop its displays.')) return;
          restartBtn.disabled = true;
          const prev = restartBtn.textContent;
          restartBtn.textContent = 'Restarting…';
          try {
            const r = await API.sendClientCommand(clientObj.id, 'restart_client');
            restartBtn.textContent = 'Restart queued ✓';
            if (status) status.textContent = r.message || `Restart queued for ${clientObj.id}.`;
          } catch(e) {
            restartBtn.textContent = 'Restart failed';
            if (status) status.textContent = 'Restart failed: ' + e.message;
            restartBtn.disabled = false;
          }
          setTimeout(() => { restartBtn.textContent = prev; restartBtn.disabled = false; }, 5000);
        };
        previewBtn.onclick = () => {
          if (!clientObj.assigned_layout_id) { if (status) status.textContent = 'No assigned layout to preview.'; return; }
          window.open(`/preview.html?layout=${encodeURIComponent(clientObj.assigned_layout_id)}`, '_blank');
        };
      }
      const checkbox = toggle.querySelector('input');
      checkbox.checked = App._bulkSelection.has(card.dataset.clientId);
      checkbox.onchange = () => {
        if (checkbox.checked) App._bulkSelection.add(card.dataset.clientId); else App._bulkSelection.delete(card.dataset.clientId);
        updateBulkBar();
      };
    });
    updateBulkBar();

    async function updateBulkBar(){
      let bar = document.getElementById('client-bulk-bar');
      const selected = Array.from(App._bulkSelection);
      if (selected.length === 0) { bar?.remove(); return; }
      // Layouts can be created in the editor while this page remains open.
      // Refresh the catalog when assignment is opened instead of relying on
      // the page-load snapshot (which previously required a full page reload).
      try {
        const freshLayouts = await API.getLayouts();
        layouts = Array.isArray(freshLayouts) ? freshLayouts : [];
        App._clientLayouts = layouts;
      } catch (_) {}
      if (!bar) {
        bar = document.createElement('div');
        bar.id = 'client-bulk-bar';
        bar.className = 'client-bulk-bar';
        bar.innerHTML = `<div class="client-bulk-summary"><span class="client-bulk-count">0</span> clients selected</div><select class="select client-bulk-layout"><option value="">— Select layout to assign —</option></select><button class="btn btn-primary btn-sm client-bulk-apply">Assign to Selected</button><button class="btn btn-ghost btn-sm client-bulk-clear">Clear Selection</button><span class="client-bulk-status"></span>`;
        list.parentElement.insertBefore(bar, list);
        bar.querySelector('.client-bulk-clear').onclick = () => { App._bulkSelection.clear(); document.querySelectorAll('.client-bulk-check').forEach(cb => cb.checked = false); updateBulkBar(); };
        bar.querySelector('.client-bulk-apply').onclick = async () => {
          const layoutSel = bar.querySelector('.client-bulk-layout');
          const target = layoutSel.value || null;
          const status = bar.querySelector('.client-bulk-status');
          status.textContent = `Applying assignment to ${selected.length} client(s)…`;
          try { const r = await API.bulkAssignClients(selected, target); status.textContent = `Updated ${r.updated}/${selected.length} client(s).`; }
          catch(e) { status.textContent = 'Bulk assign failed: '+e.message; }
        };
      }
      bar.querySelector('.client-bulk-count').textContent = selected.length;
      const layoutSel = bar.querySelector('.client-bulk-layout');
      layoutSel.innerHTML = `<option value="">— Select layout to assign —</option>${layouts.map(l=>`<option value="${esc(l.id)}">${esc(l.name)}</option>`).join('')}<option value="__clear__">— Clear assignment —</option>`;
    }
  };
})();
// Connected Clients detail modal/panel UI v6
(function extendClientDetailUi(){
  if (typeof App === 'undefined' || typeof API === 'undefined') return;
  if (App._clientDetailMounted) return;
  App._clientDetailMounted = true;
  App._openClientId = null;

  function esc(v){return String(v??'').replace(/[&<>"']/g,c=>({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));}
  function fmtBytes(b){ if(!b&&b!==0) return '-'; const u=['B','KB','MB','GB','TB']; let i=0; while(b>=1024&&i<u.length-1){b/=1024;i++;} return b.toFixed(i?2:0)+' '+u[i]; }
  function fmtUptime(s){ if(s==null) return '-'; s=Number(s); const d=Math.floor(s/86400), h=Math.floor((s%86400)/3600), m=Math.floor((s%3600)/60); return [d?d+'d':'', h?h+'h':'', m?m+'m':'', Math.floor(s%60)+'s'].filter(Boolean).join(' '); }

  API.deleteClient = async function(id){
    const res = await fetch(`/api/clients/${encodeURIComponent(id)}`, {method:'DELETE'});
    if (!res.ok) throw new Error(await res.text());
    return res.json();
  };

  function ensureOverlay(){
    let ov = document.getElementById('client-detail-overlay');
    if (ov) return ov;
    ov = document.createElement('div');
    ov.id = 'client-detail-overlay';
    ov.className = 'client-detail-overlay';
    ov.addEventListener('click', (e)=>{ if (e.target === ov) close(); });
    document.body.appendChild(ov);
    return ov;
  }

  function close(){
    const closingClientId = App._openClientId;
    const ov = document.getElementById('client-detail-overlay');
    if (ov) ov.innerHTML = '';
    ov?.classList.remove('open');
    if (closingClientId && typeof API.clearStepUp === 'function') API.clearStepUp().catch(() => {});
    App._openClientId = null;
    document.removeEventListener('keydown', onKey);
  }
  function onKey(e){ if (e.key === 'Escape') close(); }

  function buildCard(client, serverUrl){
    const c = client;
    const isVirtual = c.is_virtual;

    // ── Virtual client: simplified layout selector only ──────────────────────
    if (isVirtual) {
      const clientUrl = serverUrl.replace(/\/$/,'') + '/player.html?client_id=' + encodeURIComponent(c.id);
      return `
        <div class="client-detail-modal" role="dialog" aria-modal="true">
          <header class="client-detail-header">
            <div>
              <h3>${esc(c.name || c.hostname || c.id)}</h3>
              <p class="client-detail-sub" style="color:var(--text-muted,#94a3b8);font-size:.8rem;">Virtual Client · ${esc(c.id)}</p>
            </div>
            <button class="btn btn-ghost client-detail-close" type="button">Close ✕</button>
          </header>
          <section class="client-detail-section">
            <h4>Admin URL</h4>
            <div style="display:flex;gap:.5rem;align-items:center;margin-bottom:.5rem;">
              <input id="vc-detail-url" type="text" readonly value="${esc(clientUrl)}"
                style="flex:1;padding:.5rem .75rem;background:var(--bg-elev,#21262d);border:1px solid var(--border,#30363d);border-radius:6px;color:var(--text,#e6edf3);font-size:.8rem;outline:none;" />
              <button class="btn btn-secondary btn-sm vc-detail-copy-url">Copy</button>
            </div>
            <p id="vc-detail-copy-msg" style="color:var(--success,#3fb950);font-size:.8rem;display:none;margin:0;">Copied!</p>
          </section>
          <section class="client-detail-section">
            <h4>Content</h4>
            <div style="display:flex;flex-direction:column;gap:.75rem;">
              <div>
                <label for="vc-detail-layout" style="display:block;margin-bottom:.25rem;color:var(--text,#e6edf3);font-size:.8rem;font-weight:500;">Layout</label>
                <select id="vc-detail-layout" class="select" style="width:100%;">
                  <option value="">— None —</option>
                </select>
              </div>
            </div>
            <p id="vc-detail-status" style="color:var(--text-muted,#94a3b8);font-size:.8rem;margin:.5rem 0 0 0;"></p>
          </section>
          <section class="client-detail-section">
            <button class="btn btn-danger btn-sm client-detail-delete">Delete Virtual Client</button>
          </section>
        </div>
      `;
    }

    // ── Normal client: full detail card ───────────────────────────────────────
    const _cmds = (typeof App.buildClientInstallCommands === 'function')
      ? App.buildClientInstallCommands({ server: serverUrl })
      : { bundle: `RIPLEY_SERVER_URL=${serverUrl} curl -fsSL ${serverUrl.replace(/\/$/,'')}/install/player-client.sh | bash`, oneLiner: '' };
    const installCmd = `${_cmds.bundle}

# Alternative one-liner (will prompt for server URL on TTY):
${_cmds.oneLiner}`;
    return `
      <div class="client-detail-modal" role="dialog" aria-modal="true">
        <header class="client-detail-header">
          <div>
            <h3>${esc(c.name || c.hostname || c.id)}</h3>
            <p class="client-detail-sub">${esc(c.id)} · ${esc(c.address||'')}</p>
          </div>
          <button class="btn btn-ghost client-detail-close" type="button">Close ✕</button>
        </header>
        <section class="client-detail-grid">
          <div><span>Version</span><b>${esc(c.version||'-')}</b></div>
          <div class="client-detail-activity-card"><span>Activity</span><b>Uptime: ${esc(fmtUptime(c.uptime_seconds))}</b><b>Last seen: ${esc(c.last_seen||'-')}</b></div>
        </section>
        <section class="client-detail-section">
          <h4>Controls</h4>
          <div class="client-theme-control-row">
            <label for="client-detail-theme-select">Theme</label>
            <select id="client-detail-theme-select" class="select">
              <option value="default" ${!c.theme||c.theme==='default'?'selected':''}>Default</option>
              <option value="light" ${c.theme==='light'?'selected':''}>Light</option>
              <option value="classic" ${c.theme==='classic'?'selected':''}>Classic</option>
            </select>
          </div>
          <div class="client-detail-actions">
            <button class="btn btn-primary btn-sm client-detail-preview" ${c.assigned_layout_id?'':'disabled'}>Preview Assigned Layout</button>
            <button class="btn btn-secondary btn-sm client-detail-nudge">Nudge Client</button>
            <button class="btn btn-primary btn-sm client-detail-save-theme">Save Theme</button>
            <button class="btn btn-ghost btn-sm client-detail-copy-id">Copy Client ID</button>
            <button class="btn btn-secondary btn-sm client-detail-copy-install">Copy Bundle Install</button><button class="btn btn-ghost btn-sm client-detail-copy-install-one">Copy One-Liner</button>
            <button class="btn btn-danger btn-sm client-detail-delete">Delete Client</button>
          </div>
          <p class="client-detail-status"></p>
        </section>
        <section class="client-detail-section">
          <h4>Display Control <span style="font-weight:normal;font-size:0.8em;color:var(--text-muted)">DDC/CI + CEC</span></h4>
          <div style="display:flex;gap:0.5rem;margin-bottom:0.6rem;align-items:center;">
            <span style="font-size:0.85em">Display:</span>
            <select class="kiosk-pane-select client-detail-display-select" style="flex:1;max-width:220px">
              <option value="0">Display 0</option>
            </select>
            <span style="font-size:0.85em">Method:</span>
            <select class="kiosk-pane-select client-detail-control-method" style="max-width:120px">
              <option value="auto">Auto</option>
              <option value="ddc">DDC/CI</option>
              <option value="cec">CEC</option>
              <option value="ir">IR (later)</option>
            </select>
          </div>
          <div style="display:flex;gap:0.4rem;flex-wrap:wrap;margin-bottom:0.5rem;">
            <button class="btn btn-sm btn-secondary client-detail-ddc" data-vcp="D6" data-val="1" title="DDC/CI: wake display from sleep">Power On (DDC)</button>
            <button class="btn btn-sm btn-secondary client-detail-ddc" data-vcp="D6" data-val="4" title="DDC/CI: put display to sleep">Standby (DDC)</button>
            <button class="btn btn-sm btn-secondary client-detail-cec" data-cmd="pow 0" title="CEC: send power-on to TV">CEC On</button>
            <button class="btn btn-sm btn-secondary client-detail-cec" data-cmd="standby 0" title="CEC: send standby to TV">CEC Off</button>
          </div>
          <div class="client-detail-ddc-value-row">
            <label for="client-detail-brightness-value">Brightness</label>
            <input type="range" min="1" max="100" value="50" class="client-detail-ddc-slider" data-vcp="10" data-value-input="client-detail-brightness-value" aria-label="Brightness slider">
            <input id="client-detail-brightness-value" type="number" min="1" max="100" value="50" class="client-detail-ddc-value" aria-label="Brightness current value" title="Brightness value from 1 to 100">
            <button class="btn btn-xs btn-secondary client-detail-ddc-apply" data-vcp="10" style="flex-shrink:0">Set</button>
          </div>
          <div class="client-detail-ddc-value-row">
            <label for="client-detail-contrast-value">Contrast</label>
            <input type="range" min="1" max="100" value="50" class="client-detail-ddc-slider" data-vcp="12" data-value-input="client-detail-contrast-value" aria-label="Contrast slider">
            <input id="client-detail-contrast-value" type="number" min="1" max="100" value="50" class="client-detail-ddc-value" aria-label="Contrast current value" title="Contrast value from 1 to 100">
            <button class="btn btn-xs btn-secondary client-detail-ddc-apply" data-vcp="12" style="flex-shrink:0">Set</button>
          </div>
          <p class="client-detail-display-status" style="margin-top:0.4rem;font-size:0.8em;color:var(--text-muted)"></p>
        </section>
        <section class="client-detail-section client-audio-control-section">
          <h4>Audio</h4>
          <div class="client-audio-output-panel">
            <h5>Client audio output</h5>
            <div class="client-audio-output-row">
              <button class="btn btn-secondary btn-sm client-audio-scan">Scan audio devices</button>
              <select class="select client-audio-device-select" disabled><option value="">Scan the client to find audio outputs</option></select>
              <button class="btn btn-primary btn-sm client-audio-use" disabled>Use selected output</button>
            </div>
            <div class="client-audio-volume-row">
              <label for="client-audio-volume-value">Volume</label>
              <input type="range" min="0" max="100" value="50" class="client-audio-volume-slider" aria-label="Client audio volume">
              <input id="client-audio-volume-value" type="number" min="0" max="100" value="50" class="client-audio-volume-value" aria-label="Client audio volume value">
              <button class="btn btn-xs btn-secondary client-audio-volume-set">Set</button>
            </div>
            <p class="client-command-status client-audio-status">The client controls the selected PipeWire audio output.</p>
          </div>
          <div style="display:flex;gap:0.4rem;margin-top:0.6rem;flex-wrap:wrap;">
            <button class="btn btn-sm btn-secondary client-detail-cec" data-cmd="is 0">HDMI-1</button>
            <button class="btn btn-sm btn-secondary client-detail-cec" data-cmd="is 1">HDMI-2</button>
          </div>
          <p class="client-detail-display-status" style="margin-top:0.4rem;font-size:0.8em;color:var(--text-muted)"></p>
          <div class="client-bluetooth-panel">
            <h5>Bluetooth speaker</h5>
            <div class="client-bluetooth-row">
              <button class="btn btn-secondary btn-sm client-bluetooth-scan">Search for speakers</button>
              <select class="select client-bluetooth-device-select" disabled><option value="">Search to find Bluetooth devices</option></select>
              <button class="btn btn-primary btn-sm client-bluetooth-connect" disabled>Connect and select audio</button>
            </div>
            <p class="client-command-status client-bluetooth-status">The client performs the Bluetooth search and connection.</p>
          </div>
        </section>
        <section class="client-detail-section">
          <h4>Diagnostics</h4>
          <div class="client-detail-diagnostics-log">${App.renderClientDiagnosticsLog(c.id)}</div>
        </section>
        ${!c.enrolled ? `<section class="client-detail-section"><h4>Install on this device</h4><pre class="client-detail-pre">${esc(installCmd)}</pre></section>` : ''}
      </div>
    `;
  }

  App.openClientDetail = function(client){
    const ov = ensureOverlay();
    const server = (App._serverUrl || (window.location.origin));
    ov.innerHTML = buildCard(client, server);
    ov.classList.add('open');
    document.addEventListener('keydown', onKey);
    App._openClientId = client.id;

    const modal = ov.querySelector('.client-detail-modal');
    const setStatus = (msg) => { const s = modal.querySelector('.client-detail-status'); if (s) s.textContent = msg; };

    modal.querySelector('.client-detail-close').onclick = close;
    // ── Normal client handlers (skip for virtual) ───────────────────────────────
    if (!client.is_virtual) {
      const previewBtn = modal.querySelector('.client-detail-preview');
      if (previewBtn) {
        previewBtn.onclick = () => {
          if (!client.assigned_layout_id) return;
          window.open(`/preview.html?layout=${encodeURIComponent(client.assigned_layout_id)}`, '_blank');
        };
      }
      modal.querySelector('.client-detail-nudge').onclick = async () => {
        try { const r = await API.nudgeClient(client.id); setStatus(`Nudged ${client.id} (assigned: ${r.layout_id || 'none'}).`); }
        catch(e){ setStatus('Nudge failed: '+e.message); }
      };
      modal.querySelector('.client-detail-save-theme').onclick = async () => {
        const sel = document.getElementById('client-detail-theme-select');
        const themeVal = sel ? sel.value : (client.theme || 'default');
        const status = modal.querySelector('.client-detail-status');
        status.textContent = 'Saving...';
        try {
          const res = await fetch(`/api/clients/${encodeURIComponent(client.id)}/theme`, {
            method: 'PATCH',
            headers: {'Content-Type':'application/json'},
            body: JSON.stringify({theme: themeVal})
          });
          if (res.ok) {
            client.theme = themeVal;
            const themeValueEl = document.getElementById('client-detail-theme-value');
            if (themeValueEl) themeValueEl.textContent = themeVal;
            status.textContent = 'Theme saved.';
          } else {
            status.textContent = 'Save failed: ' + res.status;
          }
        } catch(e) { status.textContent = 'Save failed: ' + e.message; }
        setTimeout(() => { if (status.textContent === 'Theme saved.') status.textContent = ''; }, 3000);
      };
      modal.querySelector('.client-detail-copy-id').onclick = async () => {
        if (await copyToClipboard(client.id)) setStatus('Copied client id to clipboard.');
        else setStatus('Clipboard copy failed: browser blocked the copy. Select the text manually from the field below.');
      };
      modal.querySelector('.client-detail-copy-install').onclick = async () => {
        const cmds = (typeof App.buildClientInstallCommands === 'function')
          ? App.buildClientInstallCommands({ server })
          : { bundle: `RIPLEY_SERVER_URL=${server} curl -fsSL ${server.replace(/\/$/,'')}/install/player-client.sh | bash` };
        if (await App.copyText(cmds.bundle)) setStatus('Copied bundle install command. Paste it on the target player.');
        else setStatus('Clipboard copy failed: browser blocked the copy. Use the install command shown below.');
      };
      modal.querySelector('.client-detail-copy-install-one').onclick = async () => {
        const cmds = (typeof App.buildClientInstallCommands === 'function')
          ? App.buildClientInstallCommands({ server })
          : { oneLiner: `RIPLEY_SERVER_URL=${server} curl -fsSL ${server.replace(/\/$/,'')}/install/player-client.sh | bash` };
        if (await App.copyText(cmds.oneLiner)) setStatus('Copied one-liner install command.');
        else setStatus('Clipboard copy failed: browser blocked the copy.');
      };
      modal.querySelector('.client-detail-delete').onclick = async () => {
        if (!await App.confirm(`Delete client "${client.name || client.id}" from registry?`, `This will not uninstall the agent on the device.`)) return;
        try {
          const r = await API.deleteClient(client.id);
          setStatus(r.deleted ? 'Client removed.' : 'Client was already gone.');
          if (typeof App.loadConnectedClients === 'function') App.loadConnectedClients(false);
          setTimeout(close, 800);
        } catch(e){ setStatus('Delete failed: '+e.message); }
      };

      // Display control: DDC/CI + CEC with per-display method selection
      (async () => {
        const dispSelect = modal.querySelector('.client-detail-display-select');
        const methodSelect = modal.querySelector('.client-detail-control-method');
        const setDisplayStatus = (msg, type) => {
          modal.querySelectorAll('.client-detail-display-status').forEach(el => {
            el.textContent = msg;
            el.style.color = type === 'err' ? 'var(--accent)' : type === 'ok' ? 'var(--success)' : 'var(--text-muted)';
          });
        };

        // Load display list
        try {
          const displays = await API.getClientDisplays(client.id);
          dispSelect.innerHTML = '';
          displays.forEach((d, i) => {
            const label = d.label || d.connector || `Display ${i}`;
            dispSelect.add(new Option(label, String(i)));
          });
        } catch(e) {
          dispSelect.innerHTML = '<option value="0">Display 0 (load failed)</option>';
        }

        const loadControlMethod = async (dispIdx) => {
          try {
            const info = await API.getDisplayControl(client.id, Number(dispIdx));
            methodSelect.value = info.control_method || 'auto';
          } catch {
            methodSelect.value = 'auto';
          }
        };

        // Save method on change
        methodSelect.onchange = async () => {
          const idx = dispSelect.value;
          setDisplayStatus('Saving...');
          try {
            await API.setDisplayControlMethod(client.id, Number(idx), methodSelect.value);
            setDisplayStatus('Method saved.', 'ok');
          } catch(e) {
            setDisplayStatus('Save failed: ' + e.message, 'err');
          }
          setTimeout(() => setDisplayStatus(''), 3000);
        };

        // Load on display change
        dispSelect.onchange = () => loadControlMethod(dispSelect.value);

        // Initial load
        await loadControlMethod(dispSelect.value);

        const sendCommand = async (action, payload, displayIdx) => {
          const method = methodSelect.value;
          setDisplayStatus('Sending...');
          let fallback = null;
          if (method === 'auto') {
            fallback = action === 'ddc_set' ? 'cec' : 'ddc';
          }
          const trySend = async (act, pay) => {
            const r = await API.sendDisplayCommand(client.id, Number(displayIdx), act, pay);
            if (!r.result?.ok) throw new Error(r.result?.message || 'Command failed');
            return r.result?.message || 'OK';
          };
          try {
            const msg = await trySend(action, payload);
            setDisplayStatus(msg, 'ok');
          } catch(e) {
            const errMsg = e.message;
            const isDdcFail = errMsg.startsWith('ddcutil_') || ['not found', 'No such file', 'unable', 'communication', 'timeout'].some(t => errMsg.includes(t));
            if (fallback && isDdcFail) {
              const altAction = action === 'ddc_set' ? 'cec_send' : 'ddc_set';
              const altPayload = action === 'ddc_set'
                ? { cmd: payload.vcp === 'D6' ? (payload.value === 1 ? 'pow 0' : 'standby 0') : `setvcp ${payload.vcp} ${payload.value}` }
                : { vcp: payload.cmd.startsWith('pow') ? 'D6' : payload.cmd.startsWith('vol') ? '62' : '10', value: payload.cmd.includes('up') ? 70 : payload.cmd.includes('down') ? 30 : 0 };
              setDisplayStatus(`DDC failed (${errMsg}). Trying CEC...`);
              try {
                const msg = await trySend(altAction, altPayload);
                setDisplayStatus('CEC fallback: ' + msg, 'ok');
              } catch(e2) {
                setDisplayStatus(`Both failed. DDC: ${errMsg} | CEC: ${e2.message}`, 'err');
              }
            } else {
              setDisplayStatus('Error: ' + errMsg, 'err');
            }
          }
        };

        modal.querySelectorAll('.client-detail-ddc-slider[data-value-input]').forEach(slider => {
          const valueInput = document.getElementById(slider.dataset.valueInput);
          if (!valueInput) return;
          const syncValue = (raw) => {
            const value = Math.max(Number(slider.min || 1), Math.min(Number(slider.max || 100), Number(raw) || Number(slider.min || 1)));
            slider.value = String(value);
            valueInput.value = String(value);
          };
          slider.oninput = () => syncValue(slider.value);
          valueInput.oninput = () => {
            if (valueInput.value !== '') syncValue(valueInput.value);
          };
          valueInput.onchange = () => syncValue(valueInput.value);
        });

        modal.querySelectorAll('.client-detail-ddc[data-vcp][data-val]').forEach(btn => {
          btn.onclick = () => {
            const vcp = btn.dataset.vcp, val = btn.dataset.val;
            sendCommand('ddc_set', { vcp, value: Number(val) }, dispSelect.value);
          };
        });
        modal.querySelectorAll('.client-detail-ddc-apply').forEach(btn => {
          btn.onclick = () => {
            const vcp = btn.dataset.vcp;
            const slider = modal.querySelector(`.client-detail-ddc-slider[data-vcp="${vcp}"]`);
            if (!slider) return;
            const valueInput = slider.dataset.valueInput ? document.getElementById(slider.dataset.valueInput) : null;
            const value = Math.max(1, Math.min(100, Number(valueInput?.value ?? slider.value) || 1));
            sendCommand('ddc_set', { vcp, value }, dispSelect.value);
          };
        });
        modal.querySelectorAll('.client-detail-cec').forEach(btn => {
          btn.onclick = () => {
            const cmd = btn.dataset.cmd;
            sendCommand('cec_send', { cmd }, dispSelect.value);
          };
        });
      })();
    }

    // ── Virtual client: wire layout selector ─────────────────────────────────
    if (client.is_virtual) {
      const layoutSel = document.getElementById('vc-detail-layout');
      const statusEl = document.getElementById('vc-detail-status');
      const copyBtn = document.querySelector('.vc-detail-copy-url');
      const copyMsg = document.getElementById('vc-detail-copy-msg');
      const urlInput = document.getElementById('vc-detail-url');

      // Copy URL
      if (copyBtn) {
        copyBtn.addEventListener('click', () => {
          const url = urlInput ? urlInput.value : '';
          if (navigator.clipboard && navigator.clipboard.writeText) {
            navigator.clipboard.writeText(url).then(() => {
              copyMsg.style.display = 'block';
              setTimeout(() => { copyMsg.style.display = 'none'; }, 2000);
            });
          } else {
            urlInput.select();
            document.execCommand('copy');
            copyMsg.style.display = 'block';
            setTimeout(() => { copyMsg.style.display = 'none'; }, 2000);
          }
        });
      }

      // Populate layout selector
      (async () => {
        try {
          const layouts = await API.getLayouts();
          if (layoutSel) {
            layouts.forEach(l => {
              const opt = document.createElement('option');
              opt.value = l.id;
              opt.textContent = l.name || l.id;
              if (l.id === client.assigned_layout_id) opt.selected = true;
              layoutSel.appendChild(opt);
            });
          }
        } catch(e) { if (statusEl) statusEl.textContent = 'Failed to load layouts: ' + e.message; }
      })();

      // Save on layout change
      if (layoutSel) {
        layoutSel.addEventListener('change', async () => {
          if (!statusEl) return;
          statusEl.textContent = 'Saving…';
          try {
            await API.setVirtualClientLayout(client.id, layoutSel.value || null);
            client.assigned_layout_id = layoutSel.value || null;
            statusEl.textContent = 'Saved. Client will pull on next heartbeat.';
          } catch(e) {
            statusEl.textContent = 'Save failed: ' + e.message;
          }
          setTimeout(() => { if (statusEl) statusEl.textContent = ''; }, 3000);
        });
      }

      const virtualDeleteBtn = modal.querySelector('.client-detail-delete');
      if (virtualDeleteBtn) {
        virtualDeleteBtn.onclick = async () => {
          if (!await App.confirm(`Delete virtual client "${client.name || client.id}"?`, 'This removes the virtual client and its display.')) return;
          try {
            const r = await fetch(`/api/clients/${encodeURIComponent(client.id)}/virtual`, { method: 'DELETE' }).then(async response => {
              const body = await response.json().catch(() => ({}));
              if (!response.ok) throw new Error(body.error || `HTTP ${response.status}`);
              return body;
            });
            if (statusEl) statusEl.textContent = r.deleted ? 'Virtual client removed.' : 'Virtual client was already gone.';
            if (typeof App.loadConnectedClients === 'function') App.loadConnectedClients(false);
            setTimeout(close, 800);
          } catch(e) { if (statusEl) statusEl.textContent = 'Delete failed: ' + e.message; }
        };
      }

      return; // end virtual client wiring
    }
  };

  // Wire click on existing client cards to open detail
  const previous = App.renderConnectedClients;
  App.renderConnectedClients = function(clients){
    previous.call(this, clients);
    const list = document.getElementById('connected-clients-list');
    if (!list) return;
    list.querySelectorAll('.client-card').forEach(card => {
      if (card.dataset.detailWired === '1') return;
      card.dataset.detailWired = '1';
      card.style.cursor = 'pointer';
      card.addEventListener('click', (e) => {
        if (e.target.closest('button,select,input,label')) return;
        const id = card.dataset.clientId;
        const c = (clients || []).find(x => x.id === id);
        if (c) App.openClientDetail(c);
      });
    });
  };
})();
// Connected Clients search + filter + counters bar UI v7
(function extendClientFiltersUi(){
  if (typeof App === 'undefined') return;
  if (App._clientFiltersMounted) return;
  App._clientFiltersMounted = true;

  const STORAGE_KEY = 'signage.clientFilters.v1';
  App._clientFilters = Object.assign({
    text: '',
    status: 'all',
    role: 'all',
    enrollment: 'all',
    sort: 'last_seen',
  }, JSON.parse(localStorage.getItem(STORAGE_KEY) || '{}'));
  function saveFilters(){ localStorage.setItem(STORAGE_KEY, JSON.stringify(App._clientFilters)); }

  function passesFilter(client, f){
    if (f.status !== 'all') {
      const s = (client.status || 'unknown').toLowerCase();
      if (f.status === 'online' && s !== 'online') return false;
      if (f.status === 'stale' && s !== 'stale') return false;
      if (f.status === 'offline' && s !== 'offline') return false;
      if (f.status === 'candidate' && client.enrolled) return false;
    }
    if (f.role !== 'all' && (client.role || '').toLowerCase() !== f.role) return false;
    if (f.enrollment === 'enrolled' && !client.enrolled) return false;
    if (f.enrollment === 'candidate' && client.enrolled) return false;
    if (f.text) {
      const haystack = [client.name, client.hostname, client.id, client.address, client.role, client.status].join(' ').toLowerCase();
      if (!haystack.includes(f.text.toLowerCase())) return false;
    }
    return true;
  }

  function sortClients(list, sortKey){
    const arr = list.slice();
    arr.sort((a,b) => {
      if (sortKey === 'name') return (a.name||a.id||'').localeCompare(b.name||b.id||'');
      if (sortKey === 'status') return (a.status||'').localeCompare(b.status||'');
      if (sortKey === 'role') return (a.role||'').localeCompare(b.role||'');
      return new Date(b.last_seen||0) - new Date(a.last_seen||0);
    });
    return arr;
  }

  function ensureBar(){
    let bar = document.getElementById('client-filters-bar');
    if (bar) return bar;
    bar = document.createElement('div');
    bar.id = 'client-filters-bar';
    bar.className = 'client-filters-bar';
    bar.innerHTML = `
      <div class="client-filters-controls">
        <input type="search" id="client-filter-text" placeholder="Search name, IP, id, role…" class="client-filter-input">
        <select id="client-filter-status" class="select client-filter-select">
          <option value="all">All status</option>
          <option value="online">Online</option>
          <option value="stale">Stale</option>
          <option value="offline">Offline</option>
          <option value="candidate">Not enrolled</option>
        </select>
        <select id="client-filter-role" class="select client-filter-select">
          <option value="all">All roles</option>
          <option value="player">Player</option>
          <option value="all-in-one">All-in-one</option>
          <option value="server">Server</option>
        </select>
        <select id="client-filter-enrollment" class="select client-filter-select">
          <option value="all">All enrollment</option>
          <option value="enrolled">Enrolled</option>
          <option value="candidate">Not enrolled</option>
        </select>
        <select id="client-filter-sort" class="select client-filter-select">
          <option value="last_seen">Sort: Last seen</option>
          <option value="name">Sort: Name</option>
          <option value="status">Sort: Status</option>
          <option value="role">Sort: Role</option>
        </select>
        <button type="button" class="btn btn-ghost btn-sm client-filter-clear">Clear</button>
      </div>
      <div class="client-filters-counters" id="client-filters-counters"></div>
    `;
    const list = document.getElementById('connected-clients-list');
    list?.parentElement?.insertBefore(bar, list);

    bar.querySelector('#client-filter-text').value = App._clientFilters.text;
    bar.querySelector('#client-filter-status').value = App._clientFilters.status;
    bar.querySelector('#client-filter-role').value = App._clientFilters.role;
    bar.querySelector('#client-filter-enrollment').value = App._clientFilters.enrollment;
    bar.querySelector('#client-filter-sort').value = App._clientFilters.sort;

    bar.querySelector('#client-filter-text').oninput = (e) => { App._clientFilters.text = e.target.value; saveFilters(); rerender(); };
    bar.querySelector('#client-filter-status').onchange = (e) => { App._clientFilters.status = e.target.value; saveFilters(); rerender(); };
    bar.querySelector('#client-filter-role').onchange = (e) => { App._clientFilters.role = e.target.value; saveFilters(); rerender(); };
    bar.querySelector('#client-filter-enrollment').onchange = (e) => { App._clientFilters.enrollment = e.target.value; saveFilters(); rerender(); };
    bar.querySelector('#client-filter-sort').onchange = (e) => { App._clientFilters.sort = e.target.value; saveFilters(); rerender(); };
    bar.querySelector('.client-filter-clear').onclick = () => {
      App._clientFilters = { text:'', status:'all', role:'all', enrollment:'all', sort:'last_seen' };
      saveFilters(); rerender();
    };
    return bar;
  }

  function rerender(){
    if (typeof App.renderConnectedClients === 'function' && App._lastClients) {
      App.renderConnectedClients(App._lastClients);
    } else if (typeof App.loadConnectedClients === 'function') {
      App.loadConnectedClients();
    }
  }

  function applyCardVisibility(){
    const list = document.getElementById('connected-clients-list');
    if (!list) return;
    const f = App._clientFilters;
    const allCards = list.querySelectorAll('.client-card');
    let shown = 0, total = 0;
    const counters = { online:0, stale:0, offline:0, candidate:0, enrolled:0 };
    allCards.forEach(card => {
      total++;
      const c = (App._lastClients || []).find(x => x.id === card.dataset.clientId) || {};
      const passes = passesFilter(c, f);
      card.style.display = passes ? '' : 'none';
      if (passes) shown++;
      const status = (c.status || 'unknown').toLowerCase();
      if (status in counters) counters[status]++;
      if (c.enrolled === false) counters.candidate++;
      if (c.enrolled === true) counters.enrolled++;
    });
    const bar = ensureBar();
    const ctr = bar.querySelector('#client-filters-counters');
    if (ctr) {
      ctr.innerHTML = `
        <span class="client-counter client-counter-total"><b>${shown}</b>/${total} shown</span>
        <span class="client-counter client-counter-online"><b>${counters.online}</b> online</span>
        <span class="client-counter client-counter-stale"><b>${counters.stale}</b> stale</span>
        <span class="client-counter client-counter-offline"><b>${counters.offline}</b> offline</span>
        <span class="client-counter client-counter-enrolled"><b>${counters.enrolled}</b> enrolled</span>
        <span class="client-counter client-counter-candidate"><b>${counters.candidate}</b> not enrolled</span>
      `;
    }
  }

  // Override render: cache clients, apply filter before display
  const previous = App.renderConnectedClients;
  App.renderConnectedClients = function(clients){
    App._lastClients = clients || [];
    const f = App._clientFilters;
    const sorted = sortClients(App._lastClients, f.sort);
    previous.call(this, sorted);
    ensureBar();
    applyCardVisibility();
  };
})();
// Connected Clients heartbeat history sparkline charts UI v8
(function extendClientHistoryUi(){
  if (typeof App === 'undefined' || typeof API === 'undefined') return;
  if (App._clientHistoryMounted) return;
  App._clientHistoryMounted = true;

  API.getClientHistory = async function(id, limit=50){
    const res = await fetch(`/api/clients/${encodeURIComponent(id)}/history?limit=${limit}`);
    if (!res.ok) throw new Error(await res.text());
    return res.json();
  };

  function esc(v){return String(v??'').replace(/[&<>"']/g,c=>({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));}

  function buildSparkline(values, opts={}){
    const w = opts.width || 320;
    const h = opts.height || 56;
    const pad = 4;
    if (!values || values.length < 2) return `<svg class="${opts.className||'client-sparkline'}" viewBox="0 0 ${w} ${h}"><text x="${w/2}" y="${h/2}" fill="#64748b" font-size="11" text-anchor="middle">no data</text></svg>`;
    const max = Math.max(...values, 1);
    const min = Math.min(...values, 0);
    const range = (max - min) || 1;
    const step = (w - pad*2) / (values.length - 1);
    const pts = values.map((v,i) => [pad + i*step, h - pad - ((v - min) / range) * (h - pad*2)]);
    const path = pts.map((p,i) => (i===0?'M':'L') + p[0].toFixed(1)+','+p[1].toFixed(1)).join(' ');
    const area = path + ` L ${pts[pts.length-1][0].toFixed(1)},${h-pad} L ${pad},${h-pad} Z`;
    const last = values[values.length-1];
    const threshold = opts.threshold || 80;
    const stroke = (opts.percentage && last > threshold) ? '#fca5a5' : (opts.color || '#7dd3fc');
    const grid = [0.25, 0.5, 0.75].map(f => `<line x1="${pad}" y1="${(h-pad-f*(h-pad*2)).toFixed(1)}" x2="${w-pad}" y2="${(h-pad-f*(h-pad*2)).toFixed(1)}" stroke="rgba(148,163,184,.18)" stroke-width=".7"/>`).join('')
      + [0.25, 0.5, 0.75].map(f => `<line x1="${(pad+f*(w-pad*2)).toFixed(1)}" y1="${pad}" x2="${(pad+f*(w-pad*2)).toFixed(1)}" y2="${h-pad}" stroke="rgba(148,163,184,.12)" stroke-width=".7"/>`).join('');
    const points = pts.map((p,i) => {
      const value = Number(values[i] || 0).toFixed(1);
      const timestamp = opts.timestamps && opts.timestamps[i] ? opts.timestamps[i] : 'unknown time';
      return `<circle class="client-spark-point" cx="${p[0].toFixed(1)}" cy="${p[1].toFixed(1)}" r="4" fill="${stroke}" fill-opacity=".01" stroke="${stroke}" stroke-width="1"><title>${esc(opts.label || 'Usage')}: ${value}${esc(opts.unit || '%')} — recorded ${esc(timestamp)}</title></circle>`;
    }).join('');
    return `
      <svg class="${opts.className||'client-sparkline'}" viewBox="0 0 ${w} ${h}" preserveAspectRatio="none" role="img" aria-label="${esc(opts.label || 'Usage')} history">
        ${grid}
        ${opts.percentage ? `<line x1="${pad}" y1="${h-pad-((threshold-min)/range)*(h-pad*2)}" x2="${w-pad}" y2="${h-pad-((threshold-min)/range)*(h-pad*2)}" stroke="#fca5a5" stroke-dasharray="3 3" stroke-width="1" opacity="0.6"/>` : ''}
        <path d="${area}" fill="${stroke}" fill-opacity="0.15"/>
        <path d="${path}" fill="none" stroke="${stroke}" stroke-width="2"/>
        ${points}
      </svg>
    `;
  }

  async function loadHistory(modal, clientId){
    const panel = modal.querySelector('.client-history-panel');
    if (!panel) return;
    modal.querySelectorAll('.client-detail-history-cell').forEach(cell => cell.remove());
    panel.innerHTML = '<p class="client-history-loading">Loading heartbeat history…</p>';
    try {
      const data = await API.getClientHistory(clientId, 50);
      const hb = data.heartbeats || [];
      if (hb.length < 2) {
        panel.innerHTML = `<p class="client-history-loading">Need ≥2 heartbeats to chart. Got ${hb.length}. Wait for another heartbeat.</p>`;
        return;
      }
      const cpu = hb.map(h => Number(h.cpu_usage || 0));
      const mem = hb.map(h => (h.memory_used && h.memory_total) ? (h.memory_used/h.memory_total)*100 : 0);
      const last = hb[hb.length - 1];
      const sampleTimes = hb.map(h => h.recorded_at);
      const diskUsedPct = last.disk_total ? Math.max(0, Math.min(100, (Number(last.disk_used || 0) / Number(last.disk_total)) * 100)) : 0;
      const memUsedGb = Number.isFinite(Number(last.memory_used)) ? (Number(last.memory_used) / (1024 ** 3)) : null;
      const memUsedLabel = memUsedGb === null ? '—' : `${memUsedGb.toFixed(2)} GB used`;
      panel.innerHTML = `
        <div class="client-history-grid">
          <div class="client-history-cell">
            <header><span>CPU</span><b>${Number(last.cpu_usage||0).toFixed(1)}%</b></header>
            ${buildSparkline(cpu, {percentage: true, threshold: 80, color: '#86efac', label: 'CPU', unit: '%', timestamps: sampleTimes})}
            <footer>${hb.length} samples · last ${esc(last.recorded_at)}</footer>
          </div>
          <div class="client-history-cell">
            <header><span>Memory</span><b>${mem[mem.length-1].toFixed(1)}% · ${memUsedLabel}</b></header>
            ${buildSparkline(mem, {percentage: true, threshold: 90, color: '#bae6fd', label: 'Memory', unit: '%', timestamps: sampleTimes})}
            <footer>${hb.length} samples</footer>
          </div>
          <div class="client-history-cell client-history-disk-cell">
            <header><span>Disk</span><b>${diskUsedPct.toFixed(1)}% used</b></header>
            <div class="client-disk-pie" style="--disk-used:${diskUsedPct};" role="img" aria-label="Disk usage ${diskUsedPct.toFixed(1)} percent used"><span>${diskUsedPct.toFixed(1)}%</span></div>
            <footer>Used storage · ${hb.length} samples</footer>
          </div>
        </div>
        <div class="client-history-actions">
          <button class="btn btn-ghost btn-sm client-history-refresh">Refresh</button>
          <button class="btn btn-ghost btn-sm client-history-clear">Clear History</button>
          <span class="client-history-status"></span>
        </div>
      `;
      const detailGrid = modal.querySelector('.client-detail-grid');
      const historyGrid = panel.querySelector('.client-history-grid');
      if (detailGrid && historyGrid) {
        historyGrid.querySelectorAll('.client-history-cell').forEach(cell => {
          cell.classList.add('client-detail-history-cell');
          detailGrid.appendChild(cell);
        });
        historyGrid.remove();
      }
      panel.querySelector('.client-history-refresh').onclick = () => loadHistory(modal, clientId);
      panel.querySelector('.client-history-clear').onclick = async () => {
        if (!(await App.confirm('Clear heartbeat history?', 'Future heartbeats will start fresh.'))) return;
        const status = panel.querySelector('.client-history-status');
        try {
          const r = await fetch(`/api/clients/${encodeURIComponent(clientId)}/history`, {method:'DELETE'});
          status.textContent = r.ok ? 'History cleared.' : `Failed (${r.status})`;
          loadHistory(modal, clientId);
        } catch(e) { status.textContent = 'Failed: '+e.message; }
      };
    } catch(e) {
      panel.innerHTML = `<p class="client-history-loading">Failed to load history: ${esc(e.message)}</p>`;
    }
  }

  // Patch openClientDetail to inject history panel
  const prev = App.openClientDetail;
  if (prev) {
    App.openClientDetail = function(client){
      if (client && client.is_virtual) return prev.call(this, client);
      prev.call(this, client);
      const modal = document.querySelector('.client-detail-modal');
      if (!modal) return;
      if (modal.querySelector('.client-history-panel')) return;
      const details = modal.querySelector('.client-detail-grid');
      const section = document.createElement('section');
      section.className = 'client-detail-section client-history-section';
      section.innerHTML = `<div class="client-history-actions-card"><h4>Heartbeat History</h4><div class="client-history-panel"><p class="client-history-loading">Loading heartbeat history…</p></div></div>`;
      const historyCard = section.querySelector('.client-history-actions-card');
      if (details && historyCard) {
        details.appendChild(historyCard);
        section.remove();
      } else if (!details) {
        modal.appendChild(section);
      }
      loadHistory(modal, client.id);
    };
  }
})();
// Connected Clients command broadcast UI v9
(function extendCommandBroadcastUi(){
  if (typeof App === 'undefined' || typeof API === 'undefined') return;
  if (App._commandBroadcastMounted) return;
  App._commandBroadcastMounted = true;

  API.sendClientCommand = async function(id, action, payload){
    const res = await fetch(`/api/clients/${encodeURIComponent(id)}/command`, {
      method:'POST', headers:{'Content-Type':'application/json'},
      body: JSON.stringify({action, payload: payload || undefined, requested_by:'web-ui'}),
    });
    if (!res.ok) throw new Error(await res.text());
    return res.json();
  };
  API.setClientDisplayCount = async function(id, maxDisplays){
    const res = await fetch(`/api/clients/${encodeURIComponent(id)}/display-count`, {
      method:'POST', headers:{'Content-Type':'application/json'},
      body: JSON.stringify({max_displays: Number(maxDisplays)}),
    });
    if (!res.ok) throw new Error(await res.text());
    return res.json();
  };
  API.setClientTouchBinding = async function(id, payload){
    const res = await fetch(`/api/clients/${encodeURIComponent(id)}/touch-binding`, {
      method:'POST', headers:{'Content-Type':'application/json'},
      body: JSON.stringify(payload || {}),
    });
    if (!res.ok) throw new Error(await res.text());
    return res.json();
  };
  API.sendBulkCommand = async function(clientIds, action){
    const res = await fetch('/api/clients/command-bulk', {
      method:'POST', headers:{'Content-Type':'application/json'},
      body: JSON.stringify({client_ids: clientIds, action, requested_by:'web-ui'}),
    });
    if (!res.ok) throw new Error(await res.text());
    return res.json();
  };
  API.getClientCommand = async function(id){
    const res = await fetch(`/api/clients/${encodeURIComponent(id)}/command`, {method:'GET'});
    if (!res.ok) throw new Error(await res.text());
    return res.json();
  };
  API.clearClientCommand = async function(id){
    const res = await fetch(`/api/clients/${encodeURIComponent(id)}/command`, {method:'DELETE'});
    if (!res.ok) throw new Error(await res.text());
    return res.json();
  };

  const ACTIONS = [
    {key:'restart_renderer', label:'Restart Renderer'},
    {key:'reload_layout',    label:'Reload Layout'},
    {key:'restart_kiosk',    label:'Restart Kiosk Browser'},
    {key:'refresh_cache',    label:'Refresh Cache'},
    {key:'update_client',    label:'Update/Reinstall Client'},
  ];

  function esc(v){return String(v??'').replace(/[&<>"']/g,c=>({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));}

  function commandOptionsHtml(){
    return ACTIONS.map(a => `<option value="${a.key}">${a.label}</option>`).join('');
  }

  async function waitForCommandResult(clientId, action, requestedAt, timeoutMs=90000) {
    const requestedMs = Date.parse(requestedAt || '');
    const deadline = Date.now() + timeoutMs;
    while (Date.now() < deadline) {
      await new Promise(resolve => setTimeout(resolve, 2500));
      try {
        const clients = await API.getConnectedClients(false);
        const current = clients.find(item => item.id === clientId);
        const result = current && current.last_command_result;
        const completedAt = Date.parse(current?.last_command_at || '');
        const resultRequestedAt = result && result.requested_at;
        if (result && result.action === action
          && (resultRequestedAt === requestedAt || (!Number.isNaN(requestedMs) && completedAt >= requestedMs))) {
          return result;
        }
      } catch (_) {
        // Keep waiting; a transient admin request failure must not turn a
        // successfully executing client command into a false failure.
      }
    }
    return null;
  }

  // Inject into existing detail modal
  const prevOpen = App.openClientDetail;
  App.openClientDetail = function(client){
    if (client && client.is_virtual) return prevOpen.call(this, client);
    prevOpen.call(this, client);
    const modal = document.querySelector('.client-detail-modal');
    if (!modal) return;
    if (modal.querySelector('.client-command-panel')) return;
    const section = document.createElement('section');
    section.className = 'client-detail-section client-command-panel';
    section.innerHTML = `
      <h4>Send Command</h4>
      <div class="client-command-row">
        <select class="select client-command-select">${commandOptionsHtml()}</select>
        <button class="btn btn-primary btn-sm client-command-send">Send</button>
        <button class="btn btn-ghost btn-sm client-command-poll">Check Pending</button>
        <button class="btn btn-ghost btn-sm client-command-clear">Clear Pending</button>
      </div>
      <p class="client-command-status"></p>
    `;
    modal.appendChild(section);

    const status = (msg, ok=true) => {
      const s = section.querySelector('.client-command-status');
      s.textContent = msg;
      s.style.color = ok ? '#86efac' : '#fca5a5';
    };
    if (client.last_command_result) {
      const last = client.last_command_result;
      status(`Last result: ${last.ok ? 'succeeded' : 'failed'} "${last.action || 'command'}"${last.message ? ` — ${last.message}` : ''}`, !!last.ok);
    }
    section.querySelector('.client-command-send').onclick = async () => {
      const action = section.querySelector('.client-command-select').value;
      const button = section.querySelector('.client-command-send');
      button.disabled = true;
      try {
        const r = await API.sendClientCommand(client.id, action);
        status(`Queued "${r.action}" for ${client.id} at ${esc(r.requested_at)}; waiting for execution...`);
        const result = await waitForCommandResult(client.id, action, r.requested_at);
        if (!result) {
          status(`Queued "${r.action}". No execution result received within 90 seconds.`, false);
        } else if (result.ok) {
          status(`Completed "${r.action}" at ${esc(result.finished_at || result.started_at || r.requested_at)}.`);
        } else {
          status(`Failed "${r.action}": ${esc(result.message || 'client reported failure')}`, false);
        }
      } catch(e){ status('Failed: '+e.message, false); }
      finally { button.disabled = false; }
    };
    if (!modal.querySelector('.client-display-count-panel')) {
      const countSection = document.createElement('section');
      countSection.className = 'client-detail-section client-display-count-panel';
      const detected = Number(client.installer_detected_max_displays || 1);
      const current = Number(client.max_displays || detected || 1);
      const opts = [1,2,3,4].filter(n => n <= Math.max(1, detected)).map(n => `<option value="${n}" ${n===current?'selected':''}>${n}</option>`).join('');
      countSection.innerHTML = `<h4>Active displays</h4><div class="client-command-row"><label>Enabled display count <select class="select client-display-count-select">${opts}</select></label><button class="btn btn-primary btn-sm client-display-count-save">Apply</button></div><p class="client-command-status client-display-count-status">Detected ${esc(detected)} display(s). Setting this to 1 keeps touchscreen-only mode; 2+ enables secondary browser windows.</p>`;
      modal.appendChild(countSection);
      countSection.querySelector('.client-display-count-save').onclick = async () => {
        const v = Number(countSection.querySelector('.client-display-count-select').value || 1);
        const st = countSection.querySelector('.client-display-count-status');
        if (v > 1 && !await App.confirm(`Enable ${v} active displays?`, `Active displays on ${client.name || client.id}.`)) return;
        try {
          const r = await API.setClientDisplayCount(client.id, v);
          st.textContent = `Queued display count ${r.active_max_displays}; player will restart kiosk services.`;
          st.style.color = '#86efac';
          setTimeout(() => { if (App.loadConnectedClients) App.loadConnectedClients(); }, 1500);
        } catch(e) { st.textContent = 'Failed: '+e.message; st.style.color = '#fca5a5'; }
      };
    }
    if (!modal.querySelector('.client-touch-binding-panel')) {
      const touchSection = document.createElement('section');
      touchSection.className = 'client-detail-section client-touch-binding-panel';
      const tb = client.touch_binding || {};
      const detected = Math.max(1, Number(client.installer_detected_max_displays || client.max_displays || 1));
      const displayOpts = [0,1,2,3].filter(n => n < Math.max(1, detected)).map(n => `<option value="${n}">Display ${n}</option>`).join('') || '<option value="0">Display 0</option>';
      const quickDevices = ['/dev/input/event0','/dev/input/event1','/dev/input/event2','/dev/input/event3','/dev/input/event4','/dev/input/event5'];
      const deviceOpts = quickDevices.map(p => `<option value="${p}" ${p===(tb.device_path||'')?'selected':''}>${p}</option>`).join('');
      touchSection.innerHTML = `<h4>Touchscreen binding</h4>
        <div class="client-command-row">
          <label>Touch device <input class="input client-touch-device" list="client-touch-devices-${esc(client.id)}" value="${esc(tb.device_path||'/dev/input/event0')}" placeholder="/dev/input/event0"></label>
          <datalist id="client-touch-devices-${esc(client.id)}">${deviceOpts}</datalist>
          <label>Device name <input class="input client-touch-name" value="${esc(tb.device_name||'')}" placeholder="optional; client can resolve from path"></label>
          <label>Bind to <select class="select client-touch-display-a">${displayOpts}</select></label>
          <label class="client-touch-joined-label"><input type="checkbox" class="client-touch-joined" ${tb.allow_joined_displays?'checked':''}> allow joined/tiled two-display touch surface</label>
          <label class="client-touch-display-b-wrap" style="display:none">Second display <select class="select client-touch-display-b">${displayOpts}</select></label>
          <button class="btn btn-primary btn-sm client-touch-save">Apply touch binding</button>
        </div>
        <p class="client-command-status client-touch-status">Use one display for normal touch panels. Check joined/tiled only for PCs where two physical outputs act like one touch surface, e.g. Surface Studio/NVIDIA Mosaic-style desktops.</p>`;
      modal.appendChild(touchSection);
      const idxs = tb.target_display_indexes || [0,1];
      touchSection.querySelector('.client-touch-display-a').value = String(idxs[0] ?? 0);
      touchSection.querySelector('.client-touch-display-b').value = String(idxs[1] ?? Math.min(1, detected-1));
      const toggleJoined = () => { touchSection.querySelector('.client-touch-display-b-wrap').style.display = touchSection.querySelector('.client-touch-joined').checked ? 'inline-flex' : 'none'; };
      touchSection.querySelector('.client-touch-joined').onchange = toggleJoined;
      toggleJoined();
      touchSection.querySelector('.client-touch-save').onclick = async () => {
        const joined = touchSection.querySelector('.client-touch-joined').checked;
        const a = Number(touchSection.querySelector('.client-touch-display-a').value || 0);
        const b = Number(touchSection.querySelector('.client-touch-display-b').value || 1);
        if (joined && a === b) { alert('Choose two different displays for joined/tiled touch mode.'); return; }
        if (joined && !(await App.confirm('Enable joined/tiled mode?', 'This maps the touchscreen across the active desktop on the client. Use this only for Surface/NVIDIA-style joined screens.'))) return;
        const st = touchSection.querySelector('.client-touch-status');
        try {
          const r = await API.setClientTouchBinding(client.id, {device_path: touchSection.querySelector('.client-touch-device').value || null, device_name: touchSection.querySelector('.client-touch-name').value || null, target_display_indexes: joined ? [a,b] : [a], allow_joined_displays: joined});
          st.textContent = `Queued touch binding for ${joined ? 'joined displays '+a+','+b : 'display '+a}; player will reload labwc/browser services.`;
          st.style.color = '#86efac';
          setTimeout(() => { if (App.loadConnectedClients) App.loadConnectedClients(); }, 1500);
        } catch(e) { st.textContent = 'Failed: '+e.message; st.style.color = '#fca5a5'; }
      };
    }
    section.querySelector('.client-command-poll').onclick = async () => {
      try {
        const r = await API.getClientCommand(client.id);
        status(r ? `Pending: ${r.action} (queued ${esc(r.requested_at)})` : 'No pending command.', !!r);
      } catch(e){ status('Failed: '+e.message, false); }
    };
    section.querySelector('.client-command-clear').onclick = async () => {
      try { const r = await API.clearClientCommand(client.id); status(`Cleared pending command (${r.cleared}).`, true); }
      catch(e){ status('Failed: '+e.message, false); }
    };
  };

  // Inject into existing bulk bar
  const prevBulk = App.renderConnectedClients;
  App.renderConnectedClients = function(clients){
    prevBulk.call(this, clients);
    const bar = document.getElementById('client-bulk-bar');
    if (!bar) return;
    if (bar.querySelector('.client-bulk-command')) return;
    const wrap = document.createElement('div');
    wrap.className = 'client-bulk-command';
    wrap.innerHTML = `
      <label class="client-bulk-command-label">
        Broadcast:
        <select class="select client-bulk-command-select">${commandOptionsHtml()}</select>
      </label>
      <button class="btn btn-secondary btn-sm client-bulk-command-send">Send to Selected</button>
      <span class="client-bulk-command-status"></span>
    `;
    bar.appendChild(wrap);
    const status = (msg, ok=true) => {
      const s = wrap.querySelector('.client-bulk-command-status');
      s.textContent = msg; s.style.color = ok ? '#86efac' : '#fca5a5';
    };
    wrap.querySelector('.client-bulk-command-send').onclick = async () => {
      const ids = Array.from(App._bulkSelection || []);
      if (!ids.length) { status('Select clients first.', false); return; }
      const action = wrap.querySelector('.client-bulk-command-select').value;
      try {
        const r = await API.sendBulkCommand(ids, action);
        status(`Queued "${action}" for ${r.queued}/${ids.length} clients.`);
      } catch(e){ status('Failed: '+e.message, false); }
    };
  };
})();
// Connected Clients last command result UI v10
(function extendLastCommandResultUi(){
  if (typeof App === 'undefined') return;
  if (App._lastCommandResultMounted) return;
  App._lastCommandResultMounted = true;

  function esc(v){return String(v??'').replace(/[&<>"']/g,c=>({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));}

  function buildPanel(client){
    const result = client.last_command_result;
    const at = client.last_command_at;
    if (!result) {
      return `
        <h4>Last Command Result</h4>
        <p class="client-lastcmd-empty">No command has been executed yet for this client.</p>
      `;
    }
    const ok = result.ok === true;
    const cls = ok ? 'client-lastcmd-ok' : 'client-lastcmd-fail';
    return `
      <h4>Last Command Result</h4>
      <div class="client-lastcmd ${cls}">
        <div class="client-lastcmd-row">
          <span class="client-lastcmd-action">${esc(result.action || '')}</span>
          <span class="client-lastcmd-badge">${ok ? 'OK' : 'FAILED'}</span>
        </div>
        <div class="client-lastcmd-msg">${esc(result.message || '')}</div>
        <div class="client-lastcmd-meta">
          <span>Started: ${esc(result.started_at || '')}</span>
          <span>Finished: ${esc(result.finished_at || '')}</span>
          <span>Reported at: ${esc(at || '')}</span>
        </div>
      </div>
    `;
  }

  const prev = App.openClientDetail;
  if (prev) {
    App.openClientDetail = function(client){
      if (client && client.is_virtual) return prev.call(this, client);
      prev.call(this, client);
      const modal = document.querySelector('.client-detail-modal');
      if (!modal) return;
      if (modal.querySelector('.client-lastcmd')) return;
      const section = document.createElement('section');
      section.className = 'client-detail-section client-lastcmd-section';
      section.innerHTML = buildPanel(client);
      // insert AFTER command panel (which uses class client-command-panel) if present
      const cmdPanel = modal.querySelector('.client-command-panel');
      if (cmdPanel && cmdPanel.nextSibling) {
        modal.insertBefore(section, cmdPanel.nextSibling);
      } else {
        modal.appendChild(section);
      }
    };
  }
})();
// Connected Clients multi-display + per-display layout assignment UI v12
(function extendMultiDisplayUi(){
  if (typeof App === 'undefined' || typeof API === 'undefined') return;
  if (App._multiDisplayMountedV12) return;
  App._multiDisplayMountedV12 = true;

  function esc(v){return String(v??'').replace(/[&<>"']/g,c=>({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));}
  function layoutName(id){
    if (!id) return '—';
    const layouts = (App.state && App.state.layouts) || [];
    const l = layouts.find(x => x.id === id);
    return l ? (l.name || l.id) : id;
  }

  async function ensureLayouts(){
    App.state = App.state || {};
    if (Array.isArray(App.state.layouts) && App.state.layouts.length) return App.state.layouts;
    if (typeof API.getLayouts === 'function') {
      App.state.layouts = await API.getLayouts();
      return App.state.layouts;
    }
    const r = await fetch('/api/layouts');
    if (!r.ok) throw new Error(await r.text());
    App.state.layouts = await r.json();
    return App.state.layouts;
  }

  API.getClientDisplays = async function(id){
    const r = await fetch(`/api/clients/${encodeURIComponent(id)}/displays`, {method:'GET'});
    if (!r.ok) throw new Error(await r.text());
    return r.json();
  };
  API.bulkAssignDisplays = async function(id, layoutId){
    const r = await fetch(`/api/clients/${encodeURIComponent(id)}/displays/bulk-assign`, {
      method:'POST', headers:{'Content-Type':'application/json'},
      body: JSON.stringify({layout_id: layoutId || null}),
    });
    if (!r.ok) throw new Error(await r.text());
    return r.json();
  };
  API.assignDisplayLayout = async function(id, idx, layoutId){
    const r = await fetch(`/api/clients/${encodeURIComponent(id)}/displays/${idx}/layout`, {
      method:'POST', headers:{'Content-Type':'application/json'},
      body: JSON.stringify({layout_id: layoutId || null}),
    });
    if (!r.ok) throw new Error(await r.text());
    return r.json();
  };
  API.sendDisplayCommand = async function(id, idx, action, payload={}){
    const r = await fetch(`/api/clients/${encodeURIComponent(id)}/displays/${idx}/command`, {
      method:'POST', headers:{'Content-Type':'application/json'},
      body: JSON.stringify({action, payload:{display_index:idx, ...payload}, requested_by:'web-ui'}),
    });
    if (!r.ok) throw new Error(await r.text());
    return r.json();
  };

  function layoutOptionsHtml(selectedId, includeClear=true){
    const layouts = (App.state && App.state.layouts) || [];
    const opts = [];
    if (includeClear) opts.push(`<option value="">— No layout —</option>`);
    for (const l of layouts) {
      const sel = selectedId && l.id === selectedId ? ' selected' : '';
      opts.push(`<option value="${esc(l.id)}"${sel}>${esc(l.name || l.id)}</option>`);
    }
    return opts.join('');
  }

  function displayMode(d){
    if (!d.width || !d.height) return '—';
    const refresh = d.refresh_rate ? ` @ ${d.refresh_rate}Hz` : '';
    return `${d.width}×${d.height}${refresh}`;
  }

  // Keep the display API helpers above for the preview/quick-action tiles; do not
  // inject the old redundant tabular assignment section into the client modal.
})();


// Connected Clients display preview/live tile UI v13
(function extendClientDisplayTilesUi(){
  if (typeof App === 'undefined' || typeof API === 'undefined') return;
  if (App._displayTilesMountedV13) return;
  App._displayTilesMountedV13 = true;

  function esc(v){return String(v??'').replace(/[&<>"']/g,c=>({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));}
  function layoutName(id){
    if (!id) return 'No layout';
    const layouts = (App.state && App.state.layouts) || App._clientLayouts || [];
    const l = layouts.find(x => x.id === id);
    return l ? (l.name || l.id) : id;
  }
  async function ensureLayouts(){
    App.state = App.state || {};
    if (Array.isArray(App.state.layouts) && App.state.layouts.length) return App.state.layouts;
    if (Array.isArray(App._clientLayouts) && App._clientLayouts.length) { App.state.layouts = App._clientLayouts; return App.state.layouts; }
    if (typeof API.getLayouts === 'function') App.state.layouts = await API.getLayouts();
    else App.state.layouts = await (await fetch('/api/layouts')).json();
    return App.state.layouts;
  }
  if (!API.getClientDisplays) {
    API.getClientDisplays = async function(id){
      const r = await fetch(`/api/clients/${encodeURIComponent(id)}/displays`, {method:'GET'});
      if (!r.ok) throw new Error(await r.text());
      return r.json();
    };
  }
  if (!API.sendDisplayCommand) {
    API.sendDisplayCommand = async function(id, idx, action, payload={}){
      const r = await fetch(`/api/clients/${encodeURIComponent(id)}/displays/${idx}/command`, {
        method:'POST', headers:{'Content-Type':'application/json'},
        body: JSON.stringify({action, payload:{display_index:idx, ...payload}, requested_by:'web-ui'}),
      });
      if (!r.ok) throw new Error(await r.text());
      return r.json();
    };
  }
  if (!API.assignDisplayLayout) {
    API.assignDisplayLayout = async function(id, idx, layoutId){
      const r = await fetch(`/api/clients/${encodeURIComponent(id)}/displays/${idx}/layout`, {
        method:'POST', headers:{'Content-Type':'application/json'},
        body: JSON.stringify({layout_id: layoutId || null}),
      });
      if (!r.ok) throw new Error(await r.text());
      return r.json();
    };
  }
  function modeText(d){
    if (!d.width || !d.height) return 'No mode';
    return `${d.width}×${d.height}${d.refresh_rate ? ' @ '+d.refresh_rate+'Hz' : ''}`;
  }
  function statusClass(d){
    const s = d.status || 'unknown';
    return String(s).toLowerCase().replace(/[^a-z0-9_-]+/g,'-').replace(/^-+|-+$/g,'').slice(0,40) || 'unknown';
  }
  function tileHtml(d, compact=false){
    const assigned = !!d.assigned_layout_id;
    const title = d.label || d.connector || `Display ${d.display_index}`;
    const preview = assigned
      ? `<iframe class="client-display-tile-frame" src="/preview.html?layout=${encodeURIComponent(d.assigned_layout_id)}&display=${encodeURIComponent(d.display_index)}" loading="lazy" sandbox="allow-scripts allow-same-origin"></iframe>`
      : `<div class="client-display-tile-empty"><strong>No layout</strong><span>Waiting for publish</span></div>`;
    return `<article class="client-display-tile ${compact?'compact':''} status-${esc(statusClass(d))}" data-display-index="${esc(d.display_index)}">
      <div class="client-display-tile-preview">${preview}</div>
      <div class="client-display-live-controls"><button class="btn btn-ghost btn-sm client-display-live-button" type="button">Live View</button><span class="client-display-live-status"></span></div>
      <div class="client-display-tile-meta">
        <div class="client-display-tile-title"><span>${esc(d.display_index)}</span><b>${esc(title)}</b></div>
        <div class="client-display-tile-sub">:${esc(d.port)} · ${esc(modeText(d))}</div>
        <div class="client-display-tile-row"><span class="client-display-status client-display-status-${esc(statusClass(d))}">${esc(d.status || 'unknown')}</span><em>${esc(layoutName(d.assigned_layout_id))}</em></div>
      </div>
    </article>`;
  }
  function wireLiveView(container, client){
    container.querySelectorAll('.client-display-live-button').forEach(button => {
      if (button.dataset.liveWired === '1') return;
      button.dataset.liveWired = '1';
      const tile = button.closest('.client-display-tile');
      const preview = tile?.querySelector('.client-display-tile-preview');
      const status = tile?.querySelector('.client-display-live-status');
      const displayIndex = Number(tile?.dataset.displayIndex);
      if (!preview || !Number.isInteger(displayIndex)) return;
      const fallback = preview.innerHTML;
      let timer = null;
      let stopped = false;
      let liveObjectUrl = null;
      const setStatus = (text, kind='') => { if (status) { status.textContent = text; status.dataset.state = kind; } };
      const clearFrame = () => {
        if (liveObjectUrl) { URL.revokeObjectURL(liveObjectUrl); liveObjectUrl = null; }
      };
      const restore = () => {
        if (timer) { clearInterval(timer); timer = null; }
        clearFrame();
        preview.innerHTML = fallback;
        button.textContent = 'Live View';
        button.disabled = false;
        setStatus('');
      };
      const fetchFrame = async () => {
        if (stopped) return;
        try {
          const url = `/clients/${encodeURIComponent(client.id)}/screenshot?display=${displayIndex}&t=${Date.now()}`;
          const response = await fetch(url, {cache:'no-store', credentials:'same-origin'});
          if (response.status === 204) { setStatus('Waiting for client capture…'); return; }
          if (!response.ok) throw new Error(`HTTP ${response.status}`);
          const blob = await response.blob();
          clearFrame();
          liveObjectUrl = URL.createObjectURL(blob);
          let image = preview.querySelector('.client-display-live-frame');
          if (!image) { preview.innerHTML = ''; image = document.createElement('img'); image.className = 'client-display-live-frame'; preview.appendChild(image); }
          image.src = liveObjectUrl;
          image.alt = `Live display ${displayIndex}`;
          setStatus(`Live · ${new Date().toLocaleTimeString()}`, 'live');
        } catch (error) {
          setStatus(`Live view unavailable: ${error.message}`, 'error');
        }
      };
      button.onclick = async () => {
        if (timer) {
          stopped = true;
          try { await API.sendDisplayCommand(client.id, displayIndex, 'stop_display_preview', {display_index: displayIndex}); } catch (_) {}
          restore();
          return;
        }
        stopped = false;
        button.disabled = true;
        button.textContent = 'Starting…';
        setStatus('Requesting live capture…');
        try {
          await API.sendDisplayCommand(client.id, displayIndex, 'start_display_preview', {display_index: displayIndex, interval_seconds: 1, duration_seconds: 120});
          button.disabled = false;
          button.textContent = 'Stop Live View';
          timer = setInterval(fetchFrame, 1000);
          await fetchFrame();
        } catch (error) {
          button.disabled = false;
          button.textContent = 'Live View';
          setStatus(`Live view request failed: ${error.message}`, 'error');
        }
      };
    });
  }
  function layoutOptionsHtml(selectedId){
    const layouts = (App.state && App.state.layouts) || App._clientLayouts || [];
    return `<option value="">— No layout —</option>` + layouts.map(l => `<option value="${esc(l.id)}" ${selectedId===l.id?'selected':''}>${esc(l.name || l.id)}</option>`).join('');
  }
  async function renderCardTiles(card, client){
    if (!client || client.enrolled === false) return;
    let root = card.querySelector('.client-card-display-strip');
    if (!root) {
      root = document.createElement('div');
      root.className = 'client-card-display-strip';
      root.innerHTML = '<span class="client-card-display-loading">Loading displays…</span>';
      const last = card.querySelector('.client-last-seen') || card.lastElementChild;
      last?.insertAdjacentElement('afterend', root);
    }
    try {
      await ensureLayouts();
      const displays = await API.getClientDisplays(client.id);
      if (!displays.length) { root.innerHTML = '<span class="client-card-display-loading">No displays reported</span>'; return; }
      const active = displays.filter(d => ['online', 'connected'].includes(String(d.status || '').toLowerCase()));
      const inactive = displays.filter(d => !['online', 'connected'].includes(String(d.status || '').toLowerCase()));
      const activeVisible = active.slice(0, 4);
      const activeOverflow = active.length > activeVisible.length
        ? `<span class="client-card-display-more">+${active.length - activeVisible.length} active display${active.length - activeVisible.length === 1 ? '' : 's'}</span>`
        : '';
      const inactiveHtml = inactive.length
        ? `<details class="client-card-disconnected-displays"><summary>${inactive.length} disconnected display${inactive.length === 1 ? '' : 's'}</summary><div class="client-card-disconnected-list">${inactive.map(d => tileHtml(d, true)).join('')}</div></details>`
        : '';
      root.classList.toggle('client-card-display-focus', active.length === 1 && inactive.length > 0);
      root.innerHTML = activeVisible.map(d => tileHtml(d, true)).join('') + activeOverflow + inactiveHtml;
      wireLiveView(root, client);
    } catch(e) {
      root.innerHTML = `<span class="client-card-display-loading">Display load failed</span>`;
    }
  }
  async function renderDetailTiles(modal, client){
    let section = modal.querySelector('.client-display-tiles-section');
    if (!section) {
      section = document.createElement('section');
      section.className = 'client-detail-section client-display-tiles-section';
      section.innerHTML = `<h4>Display previews / quick actions</h4><div class="client-display-tiles-status">Loading display previews…</div><div class="client-display-tiles-grid"></div>`;
      const displaysSection = modal.querySelector('.client-detail-section:last-of-type');
      if (displaysSection && displaysSection.nextSibling) modal.insertBefore(section, displaysSection.nextSibling);
      else modal.appendChild(section);
    }
    const status = section.querySelector('.client-display-tiles-status');
    const grid = section.querySelector('.client-display-tiles-grid');
    try {
      await ensureLayouts();
      const displays = await API.getClientDisplays(client.id);
      if (!displays.length) { status.textContent = 'No displays reported yet.'; grid.innerHTML = ''; return; }
      status.textContent = `${displays.length} display(s) reported. Thumbnails show assigned layout previews or no-layout state.`;
      grid.innerHTML = displays.map(d => {
        const orientation = ['normal', 'rotated_right', 'rotated_left', 'flipped'].includes(d.orientation) ? d.orientation : 'normal';
        return `<div class="client-display-tile-wrap" data-display-index="${esc(d.display_index)}" data-assigned-layout-id="${esc(d.assigned_layout_id || '')}">
        ${tileHtml(d, false)}
        <div class="client-display-tile-actions">
          <div style="display:flex;align-items:center;gap:.5rem;padding:.25rem 0;font-size:.78rem;">
            <span style="color:var(--text-dim,#888);white-space:nowrap;">Orientation</span>
            <select class="select" data-orientation="${esc(orientation)}" style="font-size:.78rem;flex:1;">
              <option value="normal" ${orientation === 'normal' ? 'selected' : ''}>Landscape (normal)</option>
              <option value="rotated_right" ${orientation === 'rotated_right' ? 'selected' : ''}>Portrait (rotate right)</option>
              <option value="rotated_left" ${orientation === 'rotated_left' ? 'selected' : ''}>Portrait (rotate left)</option>
              <option value="flipped" ${orientation === 'flipped' ? 'selected' : ''}>Flipped (180°)</option>
            </select>
          </div>
        </div>
        <div class="client-display-tile-publish">
          <select class="select client-display-tile-layout">${layoutOptionsHtml(d.assigned_layout_id)}</select>
          <button class="btn btn-primary btn-sm client-display-tile-assign">Publish</button>
        </div>
        <div class="client-display-tile-actions">
          <button class="btn btn-ghost btn-sm" data-action="reload_layout">Reload</button>
          <button class="btn btn-ghost btn-sm" data-action="restart_renderer">Renderer</button>
          <button class="btn btn-ghost btn-sm client-display-tile-open" ${d.assigned_layout_id?'':'disabled'}>Open Preview</button>
        </div>
        <div class="client-display-tile-action-status"></div>
      </div>`;
      }).join('');
      wireLiveView(grid, client);
      grid.querySelectorAll('.client-display-tile-assign').forEach(btn => {
        btn.onclick = async () => {
          const wrap = btn.closest('.client-display-tile-wrap');
          const idx = Number(wrap.dataset.displayIndex);
          const sel = wrap.querySelector('.client-display-tile-layout');
          const st = wrap.querySelector('.client-display-tile-action-status');
          try {
            const r = await API.assignDisplayLayout(client.id, idx, sel.value || null);
            st.textContent = `Published ${layoutName(r.assigned_layout_id)}; reload queued.`;
            st.style.color = '#86efac';
            await renderDetailTiles(modal, client);
          } catch(e) { st.textContent = 'Publish failed: '+e.message; st.style.color = '#fca5a5'; }
        };
      });
      grid.querySelectorAll('[data-action]').forEach(btn => {
        btn.onclick = async () => {
          const wrap = btn.closest('.client-display-tile-wrap');
          const idx = Number(wrap.dataset.displayIndex);
          const st = wrap.querySelector('.client-display-tile-action-status');
          try { await API.sendDisplayCommand(client.id, idx, btn.dataset.action); st.textContent = `Queued ${btn.dataset.action} for display ${idx}.`; st.style.color = '#86efac'; }
          catch(e){ st.textContent = 'Command failed: '+e.message; st.style.color = '#fca5a5'; }
        };
      });
      grid.querySelectorAll('.client-display-tile-open').forEach(btn => {
        btn.onclick = () => {
          const wrap = btn.closest('.client-display-tile-wrap');
          const idx = Number(wrap.dataset.displayIndex);
          const sel = wrap.querySelector('.client-display-tile-layout');
          if (sel.value) window.open(`/preview.html?layout=${encodeURIComponent(sel.value)}&display=${encodeURIComponent(idx)}`, '_blank');
        };
      });
      grid.querySelectorAll('select[data-orientation]').forEach(sel => {
        sel.onchange = async () => {
          const wrap = sel.closest('.client-display-tile-wrap');
          const idx = Number(wrap.dataset.displayIndex);
          const st = wrap.querySelector('.client-display-tile-action-status');
          const orig = sel.dataset.orientation;
          try {
            await API.updateDisplayOrientation(client.id, idx, sel.value);
            sel.dataset.orientation = sel.value;
            st.textContent = `Orientation set to ${sel.value} for display ${idx}.`;
            st.style.color = '#86efac';
          } catch(e) {
            sel.value = orig;
            st.textContent = 'Orientation update failed: '+e.message;
            st.style.color = '#fca5a5';
          }
        };
      });
    } catch(e) {
      status.textContent = 'Failed to load display previews: '+e.message;
      grid.innerHTML = '';
    }
  }

  const prevRender = App.renderConnectedClients;
  if (prevRender) {
    App.renderConnectedClients = function(clients){
      const result = prevRender.call(this, clients);
      // The compact client cards are the primary dashboard surface. Populate
      // their display tiles here so Live View is available without opening the
      // detail modal; the modal keeps the same controls as a secondary path.
      const byId = new Map((clients || []).map(client => [String(client.id), client]));
      document.querySelectorAll('.client-card.enrolled:not(.virtual-client-card)').forEach(card => {
        const client = byId.get(String(card.dataset.clientId));
        if (client) renderCardTiles(card, client);
      });
      return result;
    };
  }
  const prevDetail = App.openClientDetail;
  if (prevDetail) {
    App.openClientDetail = function(client){
      prevDetail.call(this, client);
      if (client && client.is_virtual) return;
      const modal = document.querySelector('.client-detail-modal');
      if (modal && client && client.enrolled !== false) renderDetailTiles(modal, client);
    };
  }
})();


// Scene / multi-display publish manager v1
(function sceneManager(){
  if (window.__ripleySceneManagerV1) return; window.__ripleySceneManagerV1 = true;
  const esc = (s) => String(s ?? '').replace(/[&<>"']/g, c => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));
  
  // Template element context menu — wired once on first call
  function showTplElMenu(x, y) {
    var menu = document.getElementById('tpl-element-menu');
    if (!menu) return;
    menu.style.display = 'block';
    menu.style.left = Math.max(4, x) + 'px';
    menu.style.top = Math.max(4, y) + 'px';
    var r = menu.getBoundingClientRect();
    if (r.right > window.innerWidth) menu.style.left = Math.max(4, x - r.width) + 'px';
    if (r.bottom > window.innerHeight) menu.style.top = Math.max(4, y - r.height) + 'px';
  }

  function hideTplElMenu() {
    var m = document.getElementById('tpl-element-menu');
    if (m) m.style.display = 'none';
  }

  // Dismiss on outside click
  document.addEventListener('click', function(e) {
    var m = document.getElementById('tpl-element-menu');
    if (m && e.target.closest('#tpl-element-menu') === null) hideTplElMenu();
  });

  // Top-bar Delete button
  document.addEventListener('click', function(e) {
    var btn = e.target.closest('#tpl-el-top-delete,#tpl-el-delete');
    if (!btn || btn.disabled) return;
    var el = App._selectedTemplateElement();
    if (!el) return;
    App._tplElements = (App._tplElements || []).filter(function(x) { return x.id !== el.id; });
    App._tplSelectedElementId = App._tplElements[0]?.id || null;
    hideTplElMenu();
    if (App._renderTemplateCanvas) App._renderTemplateCanvas();
    if (App._renderTemplateElementProps) App._renderTemplateElementProps();
    if (App._updateRawPropsView) App._updateRawPropsView();
    if (App.updateTemplatePreview) App.updateTemplatePreview();
  });

  // Top-bar Duplicate button
  document.addEventListener('click', function(e) {
    var btn = e.target.closest('#tpl-el-top-duplicate');
    if (!btn || btn.disabled) return;
    var el = App._selectedTemplateElement();
    if (!el) return;
    var copy = JSON.parse(JSON.stringify(el));
    copy.id = 'tpl_' + Date.now() + '_' + Math.random().toString(36).slice(2, 7);
    copy.x = (copy.x || 0) + 20;
    copy.y = (copy.y || 0) + 20;
    App._tplElements = (App._tplElements || []).concat(copy);
    App._tplSelectedElementId = copy.id;
    hideTplElMenu();
    if (App._renderTemplateCanvas) App._renderTemplateCanvas();
    if (App._renderTemplateElementProps) App._renderTemplateElementProps();
    if (App._updateRawPropsView) App._updateRawPropsView();
    if (App.updateTemplatePreview) App.updateTemplatePreview();
  });

  // Context menu actions (Duplicate, Bring to Front, Send to Back, Delete)
  document.addEventListener('click', function(e) {
    var btn = e.target.closest('[data-menu-action]');
    if (!btn) return;
    var action = btn.dataset.menuAction;
    var el = App._selectedTemplateElement();
    if (!el) return;
    if (action === 'delete') {
      App._tplElements = (App._tplElements || []).filter(function(x) { return x.id !== el.id; });
      App._tplSelectedElementId = App._tplElements[0]?.id || null;
    } else if (action === 'duplicate') {
      var copy = JSON.parse(JSON.stringify(el));
      copy.id = 'tpl_' + Date.now() + '_' + Math.random().toString(36).slice(2, 7);
      copy.x = (copy.x || 0) + 20;
      copy.y = (copy.y || 0) + 20;
      App._tplElements = (App._tplElements || []).concat(copy);
      App._tplSelectedElementId = copy.id;
    } else if (action === 'front' || action === 'back') {
      if (App._setTemplateElementLayer) {
        App._setTemplateElementLayer(el, action);
      }
      hideTplElMenu();
      return;
    } else {
      return;
    }
    hideTplElMenu();
    if (App._renderTemplateCanvas) App._renderTemplateCanvas();
    if (App._renderTemplateElementProps) App._renderTemplateElementProps();
    if (App._updateRawPropsView) App._updateRawPropsView();
    if (App.updateTemplatePreview) App.updateTemplatePreview();
  });

  // Wire right-click on template canvas elements → show context menu
  // Use event delegation on the preview stage (never dynamically removed/replaced)
  var previewStage = document.getElementById('tpl-preview');
  if (previewStage) {
    previewStage.addEventListener('contextmenu', function(evt) {
      var el = evt.target.closest ? evt.target.closest('.tpl-visual-element') : null;
      if (!el) return;
      evt.preventDefault();
      App._tplSelectedElementId = el.dataset.elId || null;
      showTplElMenu(evt.clientX, evt.clientY);
    });
  }

  // Also wire existing elements on load (fallback for any not caught by delegation)
  document.querySelectorAll('.tpl-visual-element').forEach(function(el) {
    el.addEventListener('contextmenu', function(evt) {
      evt.preventDefault();
      App._tplSelectedElementId = el.dataset.elId || null;
      showTplElMenu(evt.clientX, evt.clientY);
    });
  });

  // ── expose for layout-editor.js / editor.js ──────────────────
  window._showTplElementMenu = showTplElMenu;
  window._hideTplElementMenu = hideTplElMenu;
  window._App = App; // allow editor.js to call App methods if needed

  // Legacy nav helpers: extend the real editor object without replacing it.
  window.Editor = window.Editor || {};
  Object.assign(window.Editor, {
    refreshLayoutList: window.Editor.refreshLayoutList || function() { return Promise.resolve(); },
    refreshDisplayDropdown: window.Editor.refreshDisplayDropdown || function() {},
    init: window.Editor.init || function() {},
    refresh: window.Editor.refresh || function() {},
    invalidateTemplateCache: window.Editor.invalidateTemplateCache || function() {}
  });
})();


// ── Virtual Client Modal ─────────────────────────────────────────────────────
(function initVirtualClientModal() {
    const btnCreate = document.getElementById('btn-create-virtual');
    const modal = document.getElementById('vc-modal');
    const closeBtn = document.getElementById('vc-modal-close');
    const cancelBtn = document.getElementById('vc-modal-cancel');
    const createBtn = document.getElementById('vc-modal-create');
    const nameInput = document.getElementById('vc-name');
    const resultDiv = document.getElementById('vc-result');
    const urlInput = document.getElementById('vc-url');
    const copyBtn = document.getElementById('vc-copy');
    const copyMsg = document.getElementById('vc-copy-msg');

    function showModal() {
        if (!modal) return;
        modal.hidden = false;
        modal.style.display = 'flex';
        resultDiv.hidden = true;
        resultDiv.style.display = 'none';
        nameInput.value = '';
        nameInput.focus();
    }
    function hideModal() {
        if (!modal) return;
        modal.hidden = true;
        modal.style.display = 'none';
    }

    if (btnCreate) btnCreate.addEventListener('click', showModal);
    if (closeBtn) closeBtn.addEventListener('click', hideModal);
    if (cancelBtn) cancelBtn.addEventListener('click', hideModal);
    if (modal) {
        modal.addEventListener('click', (e) => { if (e.target === modal) hideModal(); });
    }

    if (createBtn) {
        createBtn.addEventListener('click', async () => {
            const name = (nameInput.value || '').trim() || 'Virtual Client';
            createBtn.disabled = true;
            createBtn.textContent = 'Creating…';
            try {
                const result = await API.createVirtualClient(name);
                // result: { id, name, url }
                const serverBase = window.location.origin;
                const fullUrl = new URL(result.url || ('/player.html?client_id=' + encodeURIComponent(result.id)), window.location.origin).href;
                urlInput.value = fullUrl;
                resultDiv.hidden = false;
                resultDiv.style.display = 'flex';
                nameInput.value = result.name || name;
                createBtn.textContent = 'Created';
                createBtn.disabled = false;
                // Refresh client list so new virtual client appears
                if (typeof App !== 'undefined' && App.loadConnectedClients) {
                    App.loadConnectedClients(false);
                }
            } catch (e) {
                alert('Failed to create virtual client: ' + e.message);
                createBtn.textContent = 'Create';
                createBtn.disabled = false;
            }
        });
    }

    if (copyBtn) {
        copyBtn.addEventListener('click', () => {
            const url = urlInput.value;
            if (!url) return;
            if (navigator.clipboard && navigator.clipboard.writeText) {
                navigator.clipboard.writeText(url).then(() => {
                    copyMsg.style.display = 'block';
                    setTimeout(() => { copyMsg.style.display = 'none'; }, 2000);
                }).catch(() => {
                    urlInput.select();
                    document.execCommand('copy');
                    copyMsg.style.display = 'block';
                    setTimeout(() => { copyMsg.style.display = 'none'; }, 2000);
                });
            } else {
                urlInput.select();
                document.execCommand('copy');
                copyMsg.style.display = 'block';
                setTimeout(() => { copyMsg.style.display = 'none'; }, 2000);
            }
        });
    }
})();

// Group client control sections into a tabbed Controls submenu.
(function installClientControlsSubmenu(){
    if (typeof App === 'undefined') return;
    const previousOpen = App.openClientDetail;

    function directHeading(section){
        return Array.from(section.children).find(el => el.tagName === 'H4');
    }

    function sectionWithHeading(modal, prefix){
        return Array.from(modal.querySelectorAll('section')).find(section => {
            const heading = directHeading(section);
            return heading && heading.textContent.trim().toLowerCase().startsWith(prefix);
        });
    }

    function mount(modal){
        if (!modal || modal.querySelector('.client-controls-submenu')) return;
        const controls = sectionWithHeading(modal, 'controls');
        if (!controls) return;
        const previews = modal.querySelector('.client-display-tiles-section');
        if (previews && controls.parentNode) controls.parentNode.insertBefore(previews, controls);

        const themeRow = controls.querySelector('.client-theme-control-row');
        const themeSave = controls.querySelector('.client-detail-save-theme');
        let themeSection = null;
        if (themeRow) {
            themeSection = document.createElement('section');
            themeSection.className = 'client-detail-section client-theme-panel';
            themeSection.innerHTML = '<h4>Theme</h4>';
            themeSection.appendChild(themeRow);
            if (themeSave) {
                const themeActions = document.createElement('div');
                themeActions.className = 'client-detail-actions';
                themeActions.appendChild(themeSave);
                themeSection.appendChild(themeActions);
            }
        }

        const entries = [
            ['display', 'Display Controls', sectionWithHeading(modal, 'display control')],
            ['audio', 'Audio', sectionWithHeading(modal, 'audio')],
            ['theme', 'Theme', themeSection],
            ['command', 'Send Command', modal.querySelector('.client-command-panel')],
            ['active-displays', 'Active Displays', modal.querySelector('.client-display-count-panel')],
            ['touch', 'Touchscreen Binding', modal.querySelector('.client-touch-binding-panel')],
        ].filter(([, , section]) => section);
        if (!entries.length) return;

        const resultSection = modal.querySelector('.client-lastcmd-section');
        const submenu = document.createElement('div');
        submenu.className = 'client-controls-submenu';
        submenu.innerHTML = '<div class="client-controls-tabs" role="tablist" aria-label="Client controls"></div><div class="client-controls-panels"></div>';
        const tabs = submenu.querySelector('.client-controls-tabs');
        const panels = submenu.querySelector('.client-controls-panels');

        entries.forEach(([key, label, section], index) => {
            const tab = document.createElement('button');
            tab.type = 'button';
            tab.className = 'client-controls-tab' + (index === 0 ? ' active' : '');
            tab.dataset.controlsTab = key;
            tab.setAttribute('role', 'tab');
            tab.setAttribute('aria-selected', index === 0 ? 'true' : 'false');
            tab.textContent = label;
            tabs.appendChild(tab);

            const panel = document.createElement('div');
            panel.className = 'client-controls-panel';
            panel.dataset.controlsPanel = key;
            panel.hidden = index !== 0;
            panel.appendChild(section);
            if (key === 'command' && resultSection) panel.appendChild(resultSection);
            panels.appendChild(panel);

            tab.onclick = () => {
                tabs.querySelectorAll('.client-controls-tab').forEach(item => {
                    const active = item === tab;
                    item.classList.toggle('active', active);
                    item.setAttribute('aria-selected', active ? 'true' : 'false');
                });
                panels.querySelectorAll('.client-controls-panel').forEach(item => {
                    item.hidden = item !== panel;
                });
            };
        });
        controls.appendChild(submenu);
    }

    App.openClientDetail = function(client){
        previousOpen.call(this, client);
        mount(document.querySelector('.client-detail-modal'));
    };
})();

// Bluetooth speaker search and audio-output selection in client cards.
(function installBluetoothAudioUi(){
    if (typeof App === 'undefined' || typeof API === 'undefined') return;
    const previousOpen = App.openClientDetail;

    function setStatus(el, text, ok = null){
        if (!el) return;
        el.textContent = text;
        if (ok === true) el.style.color = '#86efac';
        else if (ok === false) el.style.color = '#fca5a5';
        else el.style.color = 'var(--accent-muted)';
    }

    async function waitForResult(clientId, action, requestedAt){
        for (let attempt = 0; attempt < 40; attempt++) {
            await new Promise(resolve => setTimeout(resolve, 1500));
            try {
                const clients = await API.getClients();
                const fresh = (clients || []).find(item => item.id === clientId);
                const result = fresh && fresh.last_command_result;
                if (!result || result.action !== action) continue;
                const finished = result.finished_at || result.started_at || '';
                if (requestedAt && finished && finished < requestedAt) continue;
                return result;
            } catch (_) { /* keep polling through transient refresh failures */ }
        }
        return null;
    }

    function showDevices(select, devices){
        select.innerHTML = '';
        if (!devices.length) {
            select.add(new Option('No Bluetooth devices found', ''));
            select.disabled = true;
            return;
        }
        select.add(new Option('Select a Bluetooth speaker…', ''));
        devices.forEach(device => {
            const option = new Option(`${device.name || 'Unknown device'} — ${device.address}`, device.address);
            option.dataset.name = device.name || device.address;
            select.add(option);
        });
        select.disabled = false;
    }

    function mount(modal, client){
        const panel = modal && modal.querySelector('.client-bluetooth-panel');
        if (!panel || panel.dataset.bound === 'true') return;
        panel.dataset.bound = 'true';
        const scan = panel.querySelector('.client-bluetooth-scan');
        const select = panel.querySelector('.client-bluetooth-device-select');
        const connect = panel.querySelector('.client-bluetooth-connect');
        const status = panel.querySelector('.client-bluetooth-status');

        const result = client.last_command_result;
        if (result && result.action === 'bluetooth_scan' && result.details && Array.isArray(result.details.devices)) {
            showDevices(select, result.details.devices);
        }
        select.onchange = () => { connect.disabled = !select.value; };

        scan.onclick = async () => {
            scan.disabled = true;
            connect.disabled = true;
            setStatus(status, 'Searching on the client for Bluetooth devices…');
            try {
                const queued = await API.sendClientCommand(client.id, 'bluetooth_scan', {duration_seconds: 10});
                const result = await waitForResult(client.id, 'bluetooth_scan', queued.requested_at);
                if (!result) { setStatus(status, 'Search timed out. Check the client command result.', false); return; }
                const devices = result.details && Array.isArray(result.details.devices) ? result.details.devices : [];
                showDevices(select, devices);
                setStatus(status, result.message || `Found ${devices.length} Bluetooth device(s).`, result.ok);
            } catch (e) {
                setStatus(status, 'Search failed: ' + e.message, false);
            } finally {
                scan.disabled = false;
            }
        };

        connect.onclick = async () => {
            const option = select.options[select.selectedIndex];
            if (!option || !select.value) return;
            connect.disabled = true;
            scan.disabled = true;
            setStatus(status, `Connecting to ${option.dataset.name || option.textContent} on the client…`);
            try {
                const queued = await API.sendClientCommand(client.id, 'bluetooth_connect', {
                    address: select.value,
                    name: option.dataset.name || option.textContent,
                });
                const result = await waitForResult(client.id, 'bluetooth_connect', queued.requested_at);
                if (!result) { setStatus(status, 'Connection timed out. Check the client command result.', false); return; }
                setStatus(status, result.message || 'Bluetooth connection completed.', result.ok);
            } catch (e) {
                setStatus(status, 'Connection failed: ' + e.message, false);
            } finally {
                connect.disabled = !select.value;
                scan.disabled = false;
            }
        };
    }

    App.openClientDetail = function(client){
        previousOpen.call(this, client);
        mount(document.querySelector('.client-detail-modal'), client);
    };
})();

// Client-side PipeWire output scan, selection, and volume controls.
(function installClientAudioOutputUi(){
    if (typeof App === 'undefined' || typeof API === 'undefined') return;
    const previousOpen = App.openClientDetail;

    function setStatus(el, text, ok = null){
        if (!el) return;
        el.textContent = text;
        el.style.color = ok === true ? '#86efac' : ok === false ? '#fca5a5' : 'var(--accent-muted)';
    }

    async function waitForResult(clientId, action, requestedAt){
        for (let attempt = 0; attempt < 40; attempt++) {
            await new Promise(resolve => setTimeout(resolve, 1500));
            try {
                const clients = await API.getClients();
                const fresh = (clients || []).find(item => item.id === clientId);
                const result = fresh && fresh.last_command_result;
                if (!result || result.action !== action) continue;
                const finished = result.finished_at || result.started_at || '';
                if (requestedAt && finished && finished < requestedAt) continue;
                return result;
            } catch (_) { /* tolerate transient heartbeat refreshes */ }
        }
        return null;
    }

    function mount(modal, client){
        const panel = modal && modal.querySelector('.client-audio-output-panel');
        if (!panel || !client) return;
        const scan = panel.querySelector('.client-audio-scan');
        const select = panel.querySelector('.client-audio-device-select');
        const use = panel.querySelector('.client-audio-use');
        const slider = panel.querySelector('.client-audio-volume-slider');
        const value = panel.querySelector('.client-audio-volume-value');
        const setVolume = panel.querySelector('.client-audio-volume-set');
        const status = panel.querySelector('.client-audio-status');
        if (!scan || !select || !use || !slider || !value || !setVolume) return;

        const renderDevices = (devices) => {
            select.innerHTML = '';
            if (!devices.length) {
                select.add(new Option('No audio outputs found', ''));
                select.disabled = true;
                use.disabled = true;
                return;
            }
            select.add(new Option('Select an audio output…', ''));
            devices.forEach(device => {
                const option = new Option(`${device.name || 'Audio output'}${device.default ? ' (current)' : ''}`, device.id);
                select.add(option);
                if (device.default) option.selected = true;
            });
            select.disabled = false;
            use.disabled = !select.value;
        };

        select.onchange = () => { use.disabled = !select.value; };
        slider.oninput = () => { value.value = slider.value; };
        value.oninput = () => {
            if (value.value !== '') slider.value = String(Math.max(0, Math.min(100, Number(value.value) || 0)));
        };
        value.onchange = () => {
            const v = Math.max(0, Math.min(100, Number(value.value) || 0));
            value.value = String(v); slider.value = String(v);
        };

        scan.onclick = async () => {
            scan.disabled = true; use.disabled = true;
            setStatus(status, 'Scanning audio outputs on the client…');
            try {
                const queued = await API.sendClientCommand(client.id, 'audio_list');
                const result = await waitForResult(client.id, 'audio_list', queued.requested_at);
                if (!result) { setStatus(status, 'Audio scan timed out. Check the client command result.', false); return; }
                const devices = result.details && Array.isArray(result.details.devices) ? result.details.devices : [];
                renderDevices(devices);
                setStatus(status, result.message || `Found ${devices.length} audio output(s).`, result.ok);
            } catch (e) { setStatus(status, 'Audio scan failed: ' + e.message, false); }
            finally { scan.disabled = false; }
        };

        use.onclick = async () => {
            if (!select.value) return;
            use.disabled = true; scan.disabled = true;
            setStatus(status, 'Selecting audio output on the client…');
            try {
                const queued = await API.sendClientCommand(client.id, 'audio_set_default', {sink_id: select.value});
                const result = await waitForResult(client.id, 'audio_set_default', queued.requested_at);
                if (!result) { setStatus(status, 'Audio output selection timed out.', false); return; }
                setStatus(status, result.message || 'Audio output selected.', result.ok);
            } catch (e) { setStatus(status, 'Audio output selection failed: ' + e.message, false); }
            finally { use.disabled = !select.value; scan.disabled = false; }
        };

        setVolume.onclick = async () => {
            const amount = Math.max(0, Math.min(100, Number(value.value) || 0));
            value.value = String(amount); slider.value = String(amount);
            setVolume.disabled = true;
            setStatus(status, `Setting client volume to ${amount}%…`);
            try {
                const queued = await API.sendClientCommand(client.id, 'audio_set_volume', {
                    sink_id: select.value || '@DEFAULT_AUDIO_SINK@', value: amount,
                });
                const result = await waitForResult(client.id, 'audio_set_volume', queued.requested_at);
                if (!result) { setStatus(status, 'Volume command timed out.', false); return; }
                setStatus(status, result.message || 'Client volume updated.', result.ok);
            } catch (e) { setStatus(status, 'Volume update failed: ' + e.message, false); }
            finally { setVolume.disabled = false; }
        };
    }

    App.openClientDetail = function(client){
        previousOpen.call(this, client);
        mount(document.querySelector('.client-detail-modal'), client);
    };
})();

