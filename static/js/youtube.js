
    // ── Parse the YouTube URL from the query string ──
    var p = new URLSearchParams(location.search);
    var url = p.get('url') || '';
    var m = url.match(/youtu\.be\/([a-zA-Z0-9_-]{11})/) ||
            url.match(/[?&]v=([a-zA-Z0-9_-]{11})/) ||
            url.match(/youtube\.com\/embed\/([a-zA-Z0-9_-]{11})/);
    var vid = m ? m[1] : null;

    // Read the configured priority from the query string (default: YOUTUBE = 50).
    // The editor will pass ?priority=N so the widget can be configured as MUSIC, YOUTUBE, or ALERT.
    var priorityParam = parseInt(p.get('priority'));
    var priority = isNaN(priorityParam) ? 50 : priorityParam;

    // Read the mute prop. If mute=1, the video plays visually without sound, so a
    // lower-priority music widget can keep playing.
    var muteParam = p.get('mute') === '1';

    // ── YouTube IFrame Player API ──
    // The API loads asynchronously and calls window.onYouTubeIframeAPIReady when ready.
    var tag = document.createElement('script');
    tag.src = 'https://www.youtube.com/iframe_api';
    var firstScriptTag = document.getElementsByTagName('script')[0];
    firstScriptTag.parentNode.insertBefore(tag, firstScriptTag);

    var player = null;
    var ownerId = 'youtube-' + (vid || 'unknown');

    // Called automatically by the YouTube IFrame API once it loads.
    window.onYouTubeIframeAPIReady = function () {
        if (!vid) {
            document.body.innerHTML = '<div style="color:#fff;padding:2rem;font-family:sans-serif;">No video ID found in URL.</div>';
            return;
        }
        player = new YT.Player('player', {
            width: '100%',
            height: '100%',
            videoId: vid,
            playerVars: {
                autoplay: 1,
                mute: 1,            // Start muted so autoplay works; we'll unmute after acquiring audio
                controls: 0,
                modestbranding: 1,
                rel: 0,
                playsinline: 1,
                loop: 1,
                playlist: vid,      // Required for loop=1 to work on a single video
            },
            events: {
                onReady: onPlayerReady,
                onStateChange: onPlayerStateChange,
            },
        });
    };

    function onPlayerReady(event) {
        // Start muted so we don't violate autoplay policies.
        event.target.mute();
        event.target.playVideo();

        // Register with the SoundManager. The volume will be controlled by SoundManager.
        if (window.SoundManager) {
            window.SoundManager.register({
                id: ownerId,
                priority: priority,
                type: 'youtube-iframe',
                ref: event.target,
            });
            // If the user has set the "Mute" prop on this video widget, register at
            // SILENT priority (0) so the video plays visually without acquiring the
            // audio output. This lets lower-priority music keep playing.
            if (muteParam) {
                window.SoundManager.unregister(ownerId);
                window.SoundManager.register({
                    id: ownerId,
                    priority: 0, // SILENT — visual only
                    type: 'youtube-iframe',
                    ref: event.target,
                });
            } else {
                window.SoundManager.acquire(ownerId);
            }
        }
    }

    function onPlayerStateChange(event) {
        // YT.PlayerState.ENDED === 0; with loop=1+playlist=vid it should auto-loop,
        // but as a safety net, restart if we somehow hit the end.
        if (event.data === 0) {
            event.target.playVideo();
        }
    }

    // Release ownership on unload.
    window.addEventListener('beforeunload', function () {
        if (window.SoundManager) {
            window.SoundManager.release(ownerId);
            window.SoundManager.unregister(ownerId);
        }
    });

    // Expose for debugging.
    window.__youtubeOwnerId = function () { return ownerId; };

