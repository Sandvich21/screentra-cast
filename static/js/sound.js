// SoundManager — a priority-based audio ownership system for the digital signage player.
//
// Rules:
//   - At any time, only the highest-priority registered owner is "active" (playing at 100% volume).
//   - All other active owners are "ducked" (playing at DUCK_VOLUME = 0.45 = 45%).
//   - When a higher-priority source acquires the audio, lower-priority ones are ducked.
//   - When a higher-priority source releases, the next-highest-priority active owner returns to 100%.
//
// Priority tiers (higher number = higher priority):
//   100  ALERT      — Weather alerts, calendar reminders, emergency notifications
//    50  YOUTUBE    — YouTube video audio
//    10  MUSIC      — Background music
//     0  SILENT     — Visual-only widgets (clock, text, image, etc.) that don't produce sound
//
// Usage:
//   SoundManager.register({ id: 'youtube-foo', priority: 50, type: 'youtube-iframe', ref: ytPlayer });
//   SoundManager.acquire('youtube-foo');
//   SoundManager.release('youtube-foo');
//   SoundManager.unregister('youtube-foo');

(function () {
    'use strict';

    const PRIORITY = {
        SILENT: 0,
        MUSIC: 10,
        YOUTUBE: 50,
        ALERT: 100,
    };

    let DUCK_VOLUME = 0.45; // 45% — keeps lower-priority audio present but subordinate.
    // `let` (not const) so the app can update it at runtime via SoundManager.DUCK_VOLUME = N
    // and the change immediately applies to all active owners on the next reconcile().

    // All registered owners, keyed by id.
    const owners = new Map();

    // The currently-active (top-priority, not-ducked) owner id, or null.
    let activeId = null;

    function log(msg) {
        // Reuse the player's logging endpoint if available, otherwise console.
        if (typeof window.lg === 'function') {
            try { window.lg('[SoundManager] ' + msg); } catch (e) { console.log('[SoundManager]', msg); }
        } else {
            console.log('[SoundManager]', msg);
        }
    }

    // Set the volume of a single owner. Each owner type knows how to set its own volume.
    function setOwnerVolume(owner, volume) {
        try {
            if (!owner.ref) return;
            if (owner.type === 'youtube-iframe' && typeof owner.ref.setVolume === 'function') {
                // YouTube IFrame Player API: volume is 0-100.
                owner.ref.setVolume(Math.round(volume * 100));
            } else if (owner.type === 'html5-audio' || owner.type === 'html5-video') {
                // Native HTML5 media element: volume is 0.0-1.0.
                owner.ref.volume = volume;
            } else if (typeof owner.setVolume === 'function') {
                // Generic fallback for custom owners.
                owner.setVolume(volume);
            }
        } catch (e) {
            console.warn('[SoundManager] Failed to set volume for', owner.id, e);
        }
    }

    // After any acquire/release, recompute who should be active and duck everyone else.
    function reconcile() {
        // Collect active owners (those that have acquired and aren't released).
        const activeOwners = [];
        owners.forEach(function (owner) {
            if (owner._acquired) {
                activeOwners.push(owner);
            }
        });

        if (activeOwners.length === 0) {
            activeId = null;
            log('No active owners — audio output idle.');
            return;
        }

        // Sort by priority descending. Ties broken by registration order.
        activeOwners.sort(function (a, b) { return b.priority - a.priority; });

        const top = activeOwners[0];
        const previousActiveId = activeId;
        activeId = top.id;

        if (previousActiveId !== top.id) {
            log('Active owner changed: ' + (previousActiveId || 'none') + ' → ' + top.id + ' (priority ' + top.priority + ')');
        }

        // Duck all non-top owners; restore top to 100%.
        // Read the live duck volume from the public SoundManager object so runtime
        // changes via `SoundManager.DUCK_VOLUME = N` take effect on the next reconcile.
        const liveDuck = (window.SoundManager && typeof window.SoundManager.DUCK_VOLUME === 'number')
            ? window.SoundManager.DUCK_VOLUME
            : DUCK_VOLUME;
        for (let i = 0; i < activeOwners.length; i++) {
            const owner = activeOwners[i];
            if (i === 0) {
                setOwnerVolume(owner, 1.0);
            } else {
                setOwnerVolume(owner, liveDuck);
            }
        }
    }

    function register(owner) {
        if (!owner || !owner.id) {
            console.error('[SoundManager] register() requires an id');
            return;
        }
        if (typeof owner.priority !== 'number') {
            owner.priority = PRIORITY.SILENT;
        }
        owners.set(owner.id, owner);
        log('Registered ' + owner.id + ' (priority=' + owner.priority + ', type=' + (owner.type || 'unknown') + ')');
    }

    function unregister(id) {
        const owner = owners.get(id);
        if (!owner) return;
        if (owner._acquired) {
            release(id);
        }
        owners.delete(id);
        log('Unregistered ' + id);
    }

    function acquire(id) {
        const owner = owners.get(id);
        if (!owner) {
            console.warn('[SoundManager] acquire(): unknown owner ' + id);
            return false;
        }
        if (owner.priority === PRIORITY.SILENT) {
            log('Owner ' + id + ' is SILENT — not acquiring audio output.');
            return false;
        }
        if (owner._acquired) {
            log('Owner ' + id + ' already acquired.');
            return true;
        }
        owner._acquired = true;
        log('Acquired audio for ' + id + ' (priority ' + owner.priority + ')');
        reconcile();
        return true;
    }

    function release(id) {
        const owner = owners.get(id);
        if (!owner) return;
        if (!owner._acquired) return;
        owner._acquired = false;
        // Restore this owner's volume to 0 (silent until re-acquired).
        setOwnerVolume(owner, 0);
        log('Released audio for ' + id);
        reconcile();
    }

    // Expose globally.
    window.SoundManager = {
        PRIORITY: PRIORITY,
        DUCK_VOLUME: DUCK_VOLUME,
        register: register,
        unregister: unregister,
        acquire: acquire,
        release: release,
        // Re-run the priority/volume reconciliation. Useful when the user changes
        // DUCK_VOLUME at runtime and you want active sources to immediately reflect
        // the new volume without waiting for a release/acquire cycle.
        _reconcile: reconcile,
        // Debug helpers
        _list: function () {
            const list = [];
            owners.forEach(function (o) { list.push({ id: o.id, priority: o.priority, acquired: !!o._acquired, type: o.type }); });
            return list;
        },
        _activeId: function () { return activeId; },
    };

    log('SoundManager initialized. Duck volume: ' + (DUCK_VOLUME * 100) + '%');
})();
