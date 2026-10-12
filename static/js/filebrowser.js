// File Browser — modal dialog for browsing/uploading media files

const FileBrowser = {

    // ── YouTube helpers ──
    isYouTube(url) {
        if (!url) return false;
        return url.includes('youtube.com/watch') ||
               url.includes('youtu.be/') ||
               url.includes('youtube.com/embed/');
    },

    getYouTubeId(url) {
        if (!url) return null;
        // youtu.be/VIDEO_ID
        let match = url.match(/youtu\.be\/([a-zA-Z0-9_-]{11})/);
        if (match) return match[1];
        // youtube.com/watch?v=VIDEO_ID
        match = url.match(/[?&]v=([a-zA-Z0-9_-]{11})/);
        if (match) return match[1];
        // youtube.com/embed/VIDEO_ID
        match = url.match(/youtube\.com\/embed\/([a-zA-Z0-9_-]{11})/);
        if (match) return match[1];
        return null;
    },

    getYouTubeEmbedUrl(url) {
        const id = this.getYouTubeId(url);
        if (!id) return null;
        // Use enablejsapi so the player page can control playback via the
        // YouTube IFrame API — this lets us loop only on actual video end,
        // not on buffer stutters.
        return `https://www.youtube.com/embed/${id}?autoplay=1&mute=1&controls=0&modestbranding=1&rel=0&playsinline=1`;
    },

    // ── File browser modal ──
    open(category, onSelect) {
        const overlay = document.createElement('div');
        overlay.className = 'fb-overlay';
        overlay.innerHTML = `
            <div class="fb-modal">
                <div class="fb-header">
                    <h3>Browse ${category}</h3>
                    <button class="fb-close">&times;</button>
                </div>
                <div class="fb-upload">
                    <input type="file" id="fb-file-input" accept="${category === 'images' ? 'image/*' : 'video/*'}" style="display:none">
                    <button class="btn btn-primary btn-sm" id="fb-upload-btn">⬆ Upload New ${category === 'images' ? 'Image' : 'Video'}</button>
                </div>
                <div class="fb-list" id="fb-list">
                    <p class="fb-loading">Loading...</p>
                </div>
            </div>
        `;
        document.body.appendChild(overlay);

        const close = () => overlay.remove();
        overlay.querySelector('.fb-close').onclick = close;
        overlay.onclick = (e) => { if (e.target === overlay) close(); };

        // Upload button
        const fileInput = overlay.querySelector('#fb-file-input');
        overlay.querySelector('#fb-upload-btn').onclick = () => fileInput.click();
        fileInput.onchange = async () => {
            if (fileInput.files[0]) {
                const list = overlay.querySelector('#fb-list');
                list.innerHTML = '<p class="fb-loading">Uploading...</p>';
                try {
                    await API.uploadFile(category, fileInput.files[0]);
                    await this.refreshList(overlay, category, onSelect);
                } catch (e) {
                    list.innerHTML = `<p class="fb-error">Upload failed: ${esc(e.message)}</p>`;
                }
            }
        };

        this.refreshList(overlay, category, onSelect);
    },

    async refreshList(overlay, category, onSelect) {
        const list = overlay.querySelector('#fb-list');
        try {
            const files = await API.listFiles(category);
            if (files.length === 0) {
                list.innerHTML = `<p class="fb-empty">No ${esc(category)} uploaded yet. Use the upload button above.</p>`;
                return;
            }

            list.innerHTML = '';
            for (const file of files) {
                const item = document.createElement('div');
                item.className = 'fb-item';

                if (category === 'images') {
                    item.innerHTML = `
                        <div class="fb-thumb"><img src="${esc(file.url)}" alt="${esc(file.name)}"></div>
                        <div class="fb-info">
                            <span class="fb-name">${esc(file.name)}</span>
                            <span class="fb-size">${this.formatSize(file.size)}</span>
                        </div>
                        <button class="fb-delete btn btn-danger btn-sm">🗑</button>
                    `;
                } else {
                    item.innerHTML = `
                        <div class="fb-thumb"><span style="font-size:2rem">🎬</span></div>
                        <div class="fb-info">
                            <span class="fb-name">${esc(file.name)}</span>
                            <span class="fb-size">${this.formatSize(file.size)}</span>
                        </div>
                        <button class="fb-delete btn btn-danger btn-sm">🗑</button>
                    `;
                }

                // Click to select
                item.querySelector('.fb-info').onclick = () => {
                    onSelect(file.url);
                    overlay.remove();
                };
                item.querySelector('.fb-thumb').onclick = () => {
                    onSelect(file.url);
                    overlay.remove();
                };

                // Delete button
                item.querySelector('.fb-delete').onclick = async (e) => {
                    e.stopPropagation();
                    if (!confirm(`Delete ${file.name}?`)) return;
                    try {
                        await API.deleteFile(category, file.name);
                        item.remove();
                    } catch (e) {
                        alert('Delete failed: ' + e.message);
                    }
                };

                list.appendChild(item);
            }
        } catch (e) {
            list.innerHTML = `<p class="fb-error">Failed to load: ${esc(e.message)}</p>`;
        }
    },

    formatSize(bytes) {
        if (bytes > 1e9) return (bytes / 1e9).toFixed(1) + ' GB';
        if (bytes > 1e6) return (bytes / 1e6).toFixed(1) + ' MB';
        if (bytes > 1e3) return (bytes / 1e3).toFixed(1) + ' KB';
        return bytes + ' B';
    },
};