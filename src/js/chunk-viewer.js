/**
 * Chunk Viewer — Xem các chunks đã lưu cho mỗi file
 * Hiện modal với danh sách chunk cards
 */
const ChunkViewer = {
    /** Mở chunk viewer cho một file */
    async show(filePath) {
        try {
            const data = await API.getFileChunks(filePath);
            if (!data || !data.chunks) {
                this._showEmpty(filePath);
                return;
            }
            this._render(data);
        } catch (err) {
            console.error('[ChunkViewer] Error:', err);
            this._showError(filePath, err);
        }
    },

    /** Render chunk viewer modal */
    _render(data) {
        // Remove existing modal
        const existing = document.getElementById('chunk-viewer-modal');
        if (existing) existing.remove();

        const modal = document.createElement('div');
        modal.id = 'chunk-viewer-modal';
        modal.className = 'chunk-viewer-overlay';
        modal.innerHTML = `
            <div class="chunk-viewer-panel">
                <div class="chunk-viewer-header">
                    <div class="chunk-viewer-title">
                        <span class="chunk-icon">📦</span>
                        <h3>${this._escapeHtml(data.file_path.split('/').pop() || data.file_path)}</h3>
                        <span class="chunk-badge">${data.total_chunks} chunks</span>
                    </div>
                    <button class="chunk-viewer-close" title="Đóng (Esc)">✕</button>
                </div>
                <div class="chunk-viewer-body">
                    ${data.chunks.map((chunk, i) => this._renderChunkCard(chunk, i)).join('')}
                </div>
            </div>
        `;

        // Close handlers
        modal.querySelector('.chunk-viewer-close').addEventListener('click', () => modal.remove());
        modal.addEventListener('click', (e) => {
            if (e.target === modal) modal.remove();
        });

        // Esc to close
        const escHandler = (e) => {
            if (e.key === 'Escape') {
                modal.remove();
                document.removeEventListener('keydown', escHandler);
            }
        };
        document.addEventListener('keydown', escHandler);

        document.body.appendChild(modal);
    },

    /** Render single chunk card */
    _renderChunkCard(chunk, index) {
        // Render FULL content — CSS max-height handles truncation
        const fullContent = chunk.content;
        const section = chunk.section
            ? `<span class="chunk-section" title="${this._escapeHtml(chunk.section)}">📑 ${this._escapeHtml(chunk.section)}</span>`
            : '';
        const needsExpand = fullContent.length > 300;

        return `
            <div class="chunk-card${needsExpand ? '' : ' expanded'}" data-chunk-id="${chunk.chunk_id}">
                <div class="chunk-card-header">
                    <span class="chunk-index">#${index + 1}</span>
                    ${section}
                    <div class="chunk-stats">
                        <span class="chunk-stat" title="Ký tự">📝 ${chunk.char_count.toLocaleString()}</span>
                        <span class="chunk-stat" title="Token (ước tính)">🔢 ~${chunk.token_estimate}</span>
                    </div>
                </div>
                <div class="chunk-card-content">
                    <div class="chunk-text">${this._escapeHtml(fullContent)}</div>
                </div>
                ${needsExpand ? `<button class="chunk-expand-btn" title="Xem đầy đủ nội dung" onclick="ChunkViewer._toggleExpand(this, '${chunk.chunk_id}')">
                    Xem thêm ▼
                </button>` : ''}
            </div>
        `;
    },

    /** Toggle expand/collapse chunk */
    _toggleExpand(btn, chunkId) {
        const card = document.querySelector(`[data-chunk-id="${chunkId}"]`);
        if (!card) return;
        card.classList.toggle('expanded');
        btn.textContent = card.classList.contains('expanded') ? 'Thu gọn ▲' : 'Xem thêm ▼';
    },

    _showEmpty(filePath) {
        const fileName = filePath.split('/').pop() || filePath;
        alert(`File "${fileName}" chưa có chunks (chưa được embed).`);
    },

    _showError(filePath, err) {
        console.error('[ChunkViewer]', err);
        alert(`Lỗi khi tải chunks: ${err}`);
    },

    _escapeHtml(str) {
        const div = document.createElement('div');
        div.textContent = str;
        return div.innerHTML;
    }
};
