// Ripley admin auth shim.
// Wraps window.fetch so every API call automatically sends the bearer token
// stored in localStorage, surfaces 401s via a 'ripley:unauthorized' event,
// and exposes API.login / API.logout / API.status / API.getToken / API.setToken
// on the same `API` const that api.js declares.
(function () {
    const TOKEN_KEY = 'signage.adminToken';
    const MAX_AUTH_RESPONSE_BYTES = 64 * 1024;
    async function readAuthTextBounded(response, maximumBytes = MAX_AUTH_RESPONSE_BYTES) {
        const reader = response.body && response.body.getReader ? response.body.getReader() : null;
        if (!reader) throw new Error('response body is unavailable');
        const decoder = new TextDecoder();
        let total = 0;
        let text = '';
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
        } finally { reader.releaseLock(); }
    }
    async function readAuthJsonBounded(response, maximumBytes = MAX_AUTH_RESPONSE_BYTES) {
        return JSON.parse(await readAuthTextBounded(response, maximumBytes));
    }
    let _token = null;
    try { _token = localStorage.getItem(TOKEN_KEY); } catch (_) {}

    function setToken(t) {
        _token = t || null;
        try {
            if (_token) localStorage.setItem(TOKEN_KEY, _token);
            else localStorage.removeItem(TOKEN_KEY);
        } catch (_) {}
    }
    const _realFetch = window.fetch.bind(window);
    window.fetch = async function (input, init) {
        init = init || {};
        let headers;
        if (input instanceof Request) {
            headers = new Headers(input.headers);
            for (const k in (init.headers || {})) headers.set(k, init.headers[k]);
        } else {
            headers = new Headers(init.headers || {});
        }
        let url;
        try {
            url = new URL(input instanceof Request ? input.url : input, window.location.href);
        } catch (_) {
            url = null;
        }
        if (_token && url && url.origin === window.location.origin && url.pathname.startsWith('/api/') && !headers.has('Authorization')) {
            headers.set('Authorization', _token.startsWith('Bearer ') ? _token : 'Bearer ' + _token);
        }
        const res = await _realFetch(input, Object.assign({}, init, { headers }));
        if (res && res.status === 401) {
            try { window.dispatchEvent(new CustomEvent('ripley:unauthorized')); } catch (_) {}
        }
        return res;
    };

    // Install auth helpers onto the same `API` object that api.js declares.
    // We reach it via `globalThis.API` which is bound by the script's lexical
    // scope once api.js evaluates its `const API = {...}` statement.
    let installed = false;
    function tryInstall() {
        if (installed) return;
        // globalThis.API is undefined until api.js declares the const.
        // We can detect it via Object.getOwnPropertyDescriptor on globalThis.
        const desc = Object.getOwnPropertyDescriptor(globalThis, 'API');
        const apiObj = desc && desc.value;
        if (!apiObj || typeof apiObj !== 'object' || apiObj.__ripleyAuthInstalled) {
            return;
        }
        try {
            Object.defineProperty(apiObj, '__ripleyAuthInstalled', {
                value: true, configurable: true, enumerable: false, writable: false,
            });
            Object.defineProperty(apiObj, 'getToken', {
                value: () => _token, configurable: true, enumerable: false, writable: true,
            });
            Object.defineProperty(apiObj, 'setToken', {
                value: setToken, configurable: true, enumerable: false, writable: true,
            });
            Object.defineProperty(apiObj, 'login', {
                value: async function (password, username, stepUpScope) {
                    const payload = { password };
                    if (username) payload.username = username;
                    if (stepUpScope) payload.step_up_scope = stepUpScope;
                    const res = await _realFetch('/api/auth/login', {
                        method: 'POST',
                        headers: { 'Content-Type': 'application/json' },
                        body: JSON.stringify(payload),
                        credentials: 'same-origin',
                    });
                    if (!res.ok) {
                        const txt = await readAuthTextBounded(res, 64 * 1024).catch(() => '');
                        throw new Error('login failed: ' + res.status + ' ' + txt);
                    }
                    const data = await readAuthJsonBounded(res);
                    setToken(data.token);
                    return data;
                },
                configurable: true, enumerable: false, writable: true,
            });
            Object.defineProperty(apiObj, 'clearStepUp', {
                value: async function () {
                    await _realFetch('/api/auth/step-up/clear', {
                        method: 'POST', credentials: 'same-origin',
                    });
                },
                configurable: true, enumerable: false, writable: true,
            });

            Object.defineProperty(apiObj, 'loginWithGoogle', {
                value: async function () {
                    const res = await _realFetch('/api/auth/google/login-url', { credentials: 'same-origin' });
                    if (!res.ok) throw new Error(await readAuthTextBounded(res, 64 * 1024).catch(() => 'Google login unavailable'));
                    const data = await readAuthJsonBounded(res);
                    if (!data.url) throw new Error('Google login is not configured');
                    window.location.href = data.url;
                },
                configurable: true, enumerable: false, writable: true,
            });
            Object.defineProperty(apiObj, 'logout', {
                value: async function () {
                    try {
                        await _realFetch('/api/auth/logout', {
                            method: 'POST',
                            credentials: 'same-origin',
                        });
                    } catch (_) {}
                    setToken(null);
                },
                configurable: true, enumerable: false, writable: true,
            });
            Object.defineProperty(apiObj, 'status', {
                value: async function () {
                    const res = await _realFetch('/api/auth/status', {
                        credentials: 'same-origin',
                    });
                    return readAuthJsonBounded(res).catch(() => ({ authenticated: false }));
                },
                configurable: true, enumerable: false, writable: true,
            });
            Object.defineProperty(apiObj, 'setupStatus', {
                value: async function () {
                    const res = await _realFetch('/api/setup/status', {
                        credentials: 'same-origin',
                    });
                    if (!res.ok) throw new Error(await readAuthTextBounded(res, 64 * 1024).catch(() => 'Setup status unavailable'));
                    return readAuthJsonBounded(res);
                },
                configurable: true, enumerable: false, writable: true,
            });
            Object.defineProperty(apiObj, 'createInitialAdmin', {
                value: async function (username, displayName, password) {
                    const res = await _realFetch('/api/setup', {
                        method: 'POST',
                        headers: { 'Content-Type': 'application/json' },
                        credentials: 'same-origin',
                        body: JSON.stringify({ username, display_name: displayName, password }),
                    });
                    const data = await readAuthJsonBounded(res).catch(() => null);
                    if (!res.ok) {
                        const message = typeof data === 'string' ? data : (data && (data.message || data.error));
                        throw new Error(message || `setup failed: ${res.status}`);
                    }
                    setToken(data.token);
                    return data;
                },
                configurable: true, enumerable: false, writable: true,
            });
            installed = true;
        } catch (e) {
            // Ignore; will retry.
        }
    }

    // Poll for API becoming available; cap at ~5s.
    let tries = 0;
    const iv = setInterval(() => {
        tries++;
        tryInstall();
        if (installed || tries > 100) clearInterval(iv);
    }, 50);
})();