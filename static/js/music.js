
    // ── Parse params ──
    var p = new URLSearchParams(location.search);
    var audioUrl = p.get('url') || '';
    var priorityParam = parseInt(p.get('priority'));
    var priority = isNaN(priorityParam) ? 10 : priorityParam; // default MUSIC

    var audioEl = document.getElementById('audio');
    var ownerId = 'music-' + (audioUrl || 'default');

    if (audioUrl) {
        audioEl.src = audioUrl;
    }

    // Register with SoundManager when the audio is ready to play.
    audioEl.addEventListener('canplay', function () {
        if (window.SoundManager) {
            window.SoundManager.register({
                id: ownerId,
                priority: priority,
                type: 'html5-audio',
                ref: audioEl,
            });
            window.SoundManager.acquire(ownerId);
            // Try to play (may be blocked by autoplay policy until user interaction,
            // but player displays usually have user interaction in their history).
            audioEl.play().catch(function (e) {
                console.log('[music] Autoplay blocked, will retry on user interaction:', e.message);
            });
        }
    });

    // Release on unload.
    window.addEventListener('beforeunload', function () {
        if (window.SoundManager) {
            window.SoundManager.release(ownerId);
            window.SoundManager.unregister(ownerId);
        }
    });

