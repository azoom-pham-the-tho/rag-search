/**
 * RAG Search — Chat Module (AI-First Chatbot)
 * Gemini là brain, RAG là tool hỗ trợ tìm kiếm
 */
const Chat = {
    sessionId: null,
    messages: [],
    _sessions: [],
    _activeQueryId: null,     // Track query đang chạy để tránh cross-session events
    _activeUnlisteners: [],   // Pending event listeners cần cleanup

    init() {
        this.sessionId = this.generateId();
        this._attachedFiles = []; // {file_path, file_name, is_indexed}
        this._selectedFolder = null; // {id, name, path} — slash command filter
        // Load sessions sau khi UI ready
        setTimeout(() => this.loadSessions(), 300);
        // Init attach button
        setTimeout(() => this.initAttachButton(), 100);
        // Init slash command
        setTimeout(() => this.initSlashCommand(), 100);
        // Load models từ API key
        setTimeout(() => this.loadModels(), 500);
    },

    /** Fetch danh sách models từ API và populate dropdown */
    async loadModels() {
        const select = document.getElementById('model-select');
        if (!select) return;

        try {
            const models = await API.listGeminiModels();
            if (!models || models.length === 0) return;

            // Backend đã filter sẵn generateContent models
            // Giữ lại model đang được chọn
            const currentVal = select.value;

            // Clear và rebuild options
            select.innerHTML = '';
            models.forEach(m => {
                const opt = document.createElement('option');
                opt.value = m.id;
                // Tên ngắn gọn cho chat compact UI
                opt.textContent = (m.display_name || m.id)
                    .replace('Gemini ', '')
                    .replace(' (Preview)', '');
                select.appendChild(opt);
            });

            // ★ Ưu tiên: default_model từ settings > giá trị cũ > gemini-2.0-flash
            let toSelect = currentVal;
            try {
                const saved = await API.getSettings();
                if (saved?.default_model) toSelect = saved.default_model;
            } catch (_) { }

            if ([...select.options].some(o => o.value === toSelect)) {
                select.value = toSelect;
            } else if ([...select.options].some(o => o.value === 'gemini-2.0-flash')) {
                select.value = 'gemini-2.0-flash';
            }
            console.log(`[Chat] Loaded ${models.length} models, selected: ${select.value}`);
        } catch (e) {
            console.warn('[Chat] Could not load models:', e);
            // Giữ nguyên static options trong HTML
        }
    },

    /** Init attach file button + file picker */
    initAttachButton() {
        const btn = document.getElementById('btn-attach');
        const dropdown = document.getElementById('file-picker-dropdown');
        const searchInput = document.getElementById('file-picker-search');
        const browseBtn = document.getElementById('btn-browse-file');
        if (!btn || !dropdown) return;

        // Toggle dropdown
        btn.addEventListener('click', async (e) => {
            e.stopPropagation();
            const isVisible = dropdown.style.display !== 'none';
            if (isVisible) {
                dropdown.style.display = 'none';
                return;
            }
            dropdown.style.display = 'block';
            if (searchInput) { searchInput.value = ''; searchInput.focus(); }
            await this._loadFilePicker('');
        });

        // Search filter
        if (searchInput) {
            let debounce;
            searchInput.addEventListener('input', () => {
                clearTimeout(debounce);
                debounce = setTimeout(() => this._loadFilePicker(searchInput.value), 200);
            });
        }

        // Browse native file picker — use Tauri Dialog for full paths
        if (browseBtn) {
            browseBtn.addEventListener('click', async () => {
                try {
                    // Tauri Dialog plugin → returns full native path
                    const selected = await window.__TAURI__.dialog.open({
                        multiple: true,
                        title: 'Chọn tài liệu đính kèm',
                        filters: [{
                            name: 'Tài liệu',
                            extensions: ['pdf', 'xlsx', 'xls', 'csv', 'docx', 'doc', 'txt', 'md', 'json', 'html', 'htm', 'png', 'jpg', 'jpeg']
                        }]
                    });
                    if (!selected) return;
                    const paths = Array.isArray(selected) ? selected : [selected];
                    for (const p of paths) {
                        const fileName = p.split('/').pop() || p;
                        if (!this._attachedFiles.find(af => af.file_path === p)) {
                            this._attachedFiles.push({ file_path: p, file_name: fileName, is_indexed: false });
                        }
                    }
                    this._renderAttachedFiles();
                    dropdown.style.display = 'none';
                } catch (err) {
                    console.error('[FilePicker] Dialog error:', err);
                }
            });
        }

        // Close dropdown on outside click
        document.addEventListener('click', (e) => {
            if (!dropdown.contains(e.target) && !btn.contains(e.target)) {
                dropdown.style.display = 'none';
            }
        });
    },

    /** Load indexed files into picker */
    async _loadFilePicker(search) {
        const listEl = document.getElementById('file-picker-list');
        if (!listEl) return;
        listEl.innerHTML = '<div class="file-picker-loading">Đang tải...</div>';

        try {
            const files = await API.listAllIndexedFiles() || [];
            const filtered = search
                ? files.filter(f => f.file_name.toLowerCase().includes(search.toLowerCase()))
                : files;

            if (filtered.length === 0) {
                listEl.innerHTML = `<div class="file-picker-empty">${search ? 'Không tìm thấy file phù hợp' : 'Chưa có file nào được index'}</div>`;
                return;
            }

            listEl.innerHTML = filtered.slice(0, 30).map(f => {
                const isAttached = this._attachedFiles.some(af => af.file_path === f.file_path);
                const icon = {
                    pdf: 'file-text', excel: 'table', csv: 'table', word: 'file-type',
                    image: 'image', text: 'file-text', html: 'globe', json: 'braces'
                }[f.file_type] || 'file';
                return `<div class="file-picker-item ${isAttached ? 'attached' : ''}" data-path="${f.file_path}" data-name="${f.file_name}">
                    <i data-lucide="${icon}"></i>
                    <span class="file-picker-name">${f.file_name}</span>
                    <span class="file-picker-meta">${f.chunk_count} chunks</span>
                    ${isAttached ? '<i data-lucide="check" class="file-picker-check"></i>' : ''}
                </div>`;
            }).join('');

            if (window.lucide) lucide.createIcons({ nodes: [listEl] });

            // Click handler
            listEl.querySelectorAll('.file-picker-item').forEach(item => {
                item.addEventListener('click', () => {
                    const path = item.dataset.path;
                    const name = item.dataset.name;
                    const idx = this._attachedFiles.findIndex(af => af.file_path === path);
                    if (idx >= 0) {
                        this._attachedFiles.splice(idx, 1);
                        item.classList.remove('attached');
                        const checkEl = item.querySelector('.file-picker-check');
                        if (checkEl) checkEl.remove();
                    } else {
                        this._attachedFiles.push({ file_path: path, file_name: name, is_indexed: true });
                        item.classList.add('attached');
                        // Add check icon
                        const check = document.createElement('i');
                        check.setAttribute('data-lucide', 'check');
                        check.className = 'file-picker-check';
                        item.appendChild(check);
                        if (window.lucide) lucide.createIcons({ nodes: [item] });
                    }
                    this._renderAttachedFiles();
                });
            });
        } catch (err) {
            console.error('[FilePicker] Error:', err);
            listEl.innerHTML = '<div class="file-picker-empty">Lỗi tải danh sách file</div>';
        }
    },

    /** Render attached files preview above input */
    _renderAttachedFiles() {
        const container = document.getElementById('attached-files-preview');
        if (!container) return;

        if (this._attachedFiles.length === 0) {
            container.style.display = 'none';
            container.innerHTML = '';
            return;
        }

        container.style.display = 'flex';
        container.innerHTML = this._attachedFiles.map((f, i) => `
            <span class="attached-chip" data-idx="${i}" title="${f.file_path}">
                <i data-lucide="${f.is_indexed ? 'file-check' : 'file-plus'}"></i>
                <span>${f.file_name}</span>
                <button class="attached-remove" data-idx="${i}" title="Bỏ file này">×</button>
            </span>
        `).join('');
        if (window.lucide) lucide.createIcons({ nodes: [container] });

        container.querySelectorAll('.attached-remove').forEach(btn => {
            btn.addEventListener('click', (e) => {
                e.stopPropagation();
                const idx = parseInt(btn.dataset.idx);
                this._attachedFiles.splice(idx, 1);
                this._renderAttachedFiles();
            });
        });
    },

    /** ══════════════════════════════════════
     * SLASH COMMAND: /folder_name
     * Gõ / → hiện dropdown chọn folder → filter scope tìm kiếm
     * ══════════════════════════════════════ */
    initSlashCommand() {
        const input = document.getElementById('chat-input');
        if (!input) return;

        // Create dropdown container
        const dropdown = document.createElement('div');
        dropdown.id = 'folder-cmd-dropdown';
        dropdown.className = 'folder-cmd-dropdown';
        dropdown.style.display = 'none';
        input.closest('.input-wrapper')?.appendChild(dropdown);

        // Create folder tag container (above input)
        const tagContainer = document.createElement('div');
        tagContainer.id = 'folder-filter-tag';
        tagContainer.className = 'folder-filter-tag';
        tagContainer.style.display = 'none';
        input.closest('.chat-input-area')?.insertBefore(tagContainer,
            document.getElementById('attached-files-preview')?.nextSibling || input.closest('.input-wrapper')
        );

        // Listen for input changes
        input.addEventListener('input', () => {
            const val = input.value;
            const cursorPos = input.selectionStart;

            // Only trigger at beginning of input: /something
            if (val.startsWith('/') && !this._selectedFolder) {
                const slashText = val.substring(1).split(' ')[0]; // text after /
                if (!val.includes(' ') || cursorPos <= val.indexOf(' ')) {
                    this._showFolderDropdown(slashText);
                    return;
                }
            }
            this._hideFolderDropdown();
        });

        // Handle keyboard in dropdown
        input.addEventListener('keydown', (e) => {
            const dd = document.getElementById('folder-cmd-dropdown');
            if (!dd || dd.style.display === 'none') return;

            const items = dd.querySelectorAll('.folder-cmd-item');
            const active = dd.querySelector('.folder-cmd-item--active');
            let activeIdx = [...items].indexOf(active);

            if (e.key === 'ArrowDown') {
                e.preventDefault();
                if (active) active.classList.remove('folder-cmd-item--active');
                activeIdx = (activeIdx + 1) % items.length;
                items[activeIdx]?.classList.add('folder-cmd-item--active');
                items[activeIdx]?.scrollIntoView({ block: 'nearest' });
            } else if (e.key === 'ArrowUp') {
                e.preventDefault();
                if (active) active.classList.remove('folder-cmd-item--active');
                activeIdx = activeIdx <= 0 ? items.length - 1 : activeIdx - 1;
                items[activeIdx]?.classList.add('folder-cmd-item--active');
                items[activeIdx]?.scrollIntoView({ block: 'nearest' });
            } else if (e.key === 'Enter' && active) {
                e.preventDefault();
                e.stopPropagation();
                active.click();
            } else if (e.key === 'Escape') {
                e.preventDefault();
                this._hideFolderDropdown();
            } else if (e.key === 'Tab' && items.length > 0) {
                e.preventDefault();
                (active || items[0]).click();
            }
        });

        // Close dropdown on click outside
        document.addEventListener('click', (e) => {
            const dd = document.getElementById('folder-cmd-dropdown');
            if (dd && dd.style.display !== 'none' && !dd.contains(e.target) && e.target !== input) {
                this._hideFolderDropdown();
            }
        });
    },

    _showFolderDropdown(filter = '') {
        const dd = document.getElementById('folder-cmd-dropdown');
        if (!dd) return;

        const folders = (Folder.folders || []).filter(f =>
            !filter || f.name.toLowerCase().includes(filter.toLowerCase())
        );

        if (folders.length === 0) {
            dd.innerHTML = '<div class="folder-cmd-empty">Không tìm thấy thư mục</div>';
            dd.style.display = 'block';
            return;
        }

        dd.innerHTML = `
            <div class="folder-cmd-hint">Chọn thư mục để lọc tìm kiếm</div>
            ${folders.map((f, i) => `
                <div class="folder-cmd-item${i === 0 ? ' folder-cmd-item--active' : ''}"
                     data-id="${f.id}" data-name="${f.name}" data-path="${f.path}">
                    <i data-lucide="folder" class="folder-cmd-icon"></i>
                    <span class="folder-cmd-name">${f.name}</span>
                    <span class="folder-cmd-count">${f.file_count || ''} file</span>
                </div>
            `).join('')}
        `;

        // Click handler
        dd.querySelectorAll('.folder-cmd-item').forEach(item => {
            item.addEventListener('click', () => {
                const folder = {
                    id: item.dataset.id,
                    name: item.dataset.name,
                    path: item.dataset.path,
                };
                this._selectedFolder = folder;
                this._renderFolderTag();
                this._hideFolderDropdown();

                // Clear the /prefix from input
                const input = document.getElementById('chat-input');
                if (input) {
                    const val = input.value;
                    const spaceIdx = val.indexOf(' ');
                    input.value = spaceIdx > 0 ? val.substring(spaceIdx + 1) : '';
                    input.focus();
                }
            });

            // Hover highlight
            item.addEventListener('mouseenter', () => {
                dd.querySelector('.folder-cmd-item--active')?.classList.remove('folder-cmd-item--active');
                item.classList.add('folder-cmd-item--active');
            });
        });

        dd.style.display = 'block';
        if (window.lucide) lucide.createIcons({ nodes: [dd] });
    },

    _hideFolderDropdown() {
        const dd = document.getElementById('folder-cmd-dropdown');
        if (dd) dd.style.display = 'none';
    },

    _renderFolderTag() {
        const container = document.getElementById('folder-filter-tag');
        if (!container) return;

        if (!this._selectedFolder) {
            container.style.display = 'none';
            container.innerHTML = '';
            return;
        }

        container.style.display = 'flex';
        container.innerHTML = `
            <i data-lucide="folder" class="folder-tag-icon"></i>
            <span class="folder-tag-name">${this._selectedFolder.name}</span>
            <button class="folder-tag-remove" title="Bỏ lọc thư mục">✕</button>
        `;

        container.querySelector('.folder-tag-remove')?.addEventListener('click', () => {
            this._selectedFolder = null;
            this._renderFolderTag();
            document.getElementById('chat-input')?.focus();
        });

        if (window.lucide) lucide.createIcons({ nodes: [container] });
    },

    /** Load danh sách sessions từ DB */
    async loadSessions() {
        try {
            const sessions = await API.listChatSessions();
            this._sessions = sessions || [];
            this.renderSessions();
        } catch (e) {
            console.warn('[Chat] Load sessions failed:', e);
        }
    },

    /** Render sessions trong sidebar */
    renderSessions() {
        const container = document.getElementById('chat-sessions-list');
        if (!container) return;

        if (this._sessions.length === 0) {
            container.innerHTML = '<div class="sessions-empty">Chưa có hội thoại nào</div>';
            return;
        }

        // Group by time
        const now = Date.now() / 1000;
        const today = [];
        const yesterday = [];
        const thisWeek = [];
        const older = [];

        for (const s of this._sessions) {
            const age = now - s.updated_at;
            if (age < 86400) today.push(s);
            else if (age < 172800) yesterday.push(s);
            else if (age < 604800) thisWeek.push(s);
            else older.push(s);
        }

        let html = '';
        const renderGroup = (label, items) => {
            if (items.length === 0) return '';
            let h = `<div class="sessions-group-label">${label}</div>`;
            for (const s of items) {
                const isActive = s.id === this.sessionId;
                const title = s.title || 'Hội thoại mới';
                const displayTitle = title.length > 35 ? title.slice(0, 35) + '...' : title;
                h += `<div class="session-item ${isActive ? 'active' : ''}" data-id="${s.id}" title="${title}">
                    <i data-lucide="message-square" class="session-icon"></i>
                    <span class="session-title">${displayTitle}</span>
                    <button class="session-delete" data-id="${s.id}" title="Xóa hội thoại">
                        <i data-lucide="trash-2"></i>
                    </button>
                </div>`;
            }
            return h;
        };

        html += renderGroup('Hôm nay', today);
        html += renderGroup('Hôm qua', yesterday);
        html += renderGroup('Tuần này', thisWeek);
        html += renderGroup('Trước đó', older);

        container.innerHTML = html;

        // Lucide icons
        if (window.lucide) lucide.createIcons({ nodes: [container] });

        // Click handlers
        container.querySelectorAll('.session-item').forEach(el => {
            el.addEventListener('click', (e) => {
                if (e.target.closest('.session-delete')) return;
                this.switchSession(el.dataset.id);
            });
        });

        container.querySelectorAll('.session-delete').forEach(btn => {
            btn.addEventListener('click', async (e) => {
                e.stopPropagation();
                const id = btn.dataset.id;
                const item = btn.closest('.session-item');

                // Visual feedback
                if (item) item.style.opacity = '0.4';
                btn.disabled = true;

                // Nếu đang xóa session hiện tại → tạo chat mới trước
                if (id === this.sessionId) {
                    this.newSession();
                }

                try {
                    await API.deleteChatSession(id);
                    console.log('[Chat] ✅ Deleted session:', id);
                } catch (err) {
                    // Backend error → vẫn xóa khỏi UI
                    console.warn('[Chat] Delete error (ok, removing from UI):', err);
                }

                // Luôn reload từ server
                await this.loadSessions();
            });
        });

    },

    /** Chuyển sang session khác */
    async switchSession(sessionId) {
        if (sessionId === this.sessionId) return;

        // ★ Cancel query đang chạy từ session cũ
        this._cancelActiveQuery();

        this.sessionId = sessionId;
        this.messages = [];

        // Clear UI
        const messagesEl = document.getElementById('chat-messages');
        const welcomeEl = document.getElementById('welcome-screen');
        if (messagesEl) messagesEl.innerHTML = '';

        // Load history
        try {
            const history = await API.getChatHistory(sessionId);
            if (history && history.length > 0) {
                if (welcomeEl) welcomeEl.style.display = 'none';
                if (messagesEl) messagesEl.style.display = '';

                for (const msg of history) {
                    this.addMessage(msg.role, msg.content, [], false, {});
                }
            } else {
                if (welcomeEl) welcomeEl.style.display = '';
                if (messagesEl) messagesEl.style.display = 'none';
            }
        } catch (e) {
            console.error('[Chat] Load history error:', e);
        }

        this.renderSessions();
    },

    generateId() {
        return 'session-' + Date.now() + '-' + Math.random().toString(36).slice(2, 8);
    },

    /** Tạo session mới */
    newSession() {
        // ★ Cancel query đang chạy từ session cũ
        this._cancelActiveQuery();

        this.sessionId = this.generateId();
        this.messages = [];

        const messagesEl = document.getElementById('chat-messages');
        const welcomeEl = document.getElementById('welcome-screen');

        if (messagesEl) {
            messagesEl.innerHTML = '';
            messagesEl.style.display = 'none';
        }
        if (welcomeEl) welcomeEl.style.display = '';

        this.renderSessions();
        App.toast('Đã tạo cuộc trò chuyện mới', 'success');
    },

    /** Cancel query đang chạy — cleanup listeners + UI */
    _cancelActiveQuery() {
        if (this._activeQueryId) {
            console.log('[Chat] Cancelling active query:', this._activeQueryId);
            this._activeQueryId = null;
        }
        // Cleanup pending event listeners
        if (this._activeUnlisteners.length > 0) {
            this._activeUnlisteners.forEach(fn => fn());
            this._activeUnlisteners = [];
        }
        this.hideTyping();
        this._cleanupProgress();
    },

    /** Gửi tin nhắn — unified smart query */
    async send() {
        // Guard: ngăn gửi trùng lặp
        if (this._sending) return;

        const input = document.getElementById('chat-input');
        if (!input) return;

        let text = input.value.trim();
        if (!text) return;

        // Hide folder dropdown if open
        this._hideFolderDropdown();

        this._sending = true;
        const sendBtn = document.getElementById('btn-send');
        if (sendBtn) sendBtn.disabled = true;

        // Clear input
        input.value = '';
        input.style.height = 'auto';

        // Hide welcome, show messages
        const welcomeEl = document.getElementById('welcome-screen');
        const messagesEl = document.getElementById('chat-messages');
        if (welcomeEl) welcomeEl.style.display = 'none';
        if (messagesEl) messagesEl.style.display = '';

        // ★ Handle /folder_name prefix inline (fallback if tag not used)
        const slashMatch = text.match(/^\/([^\s]+)\s+(.+)$/);
        if (slashMatch && !this._selectedFolder) {
            const folderName = slashMatch[1];
            const query = slashMatch[2];
            const folder = (Folder.folders || []).find(f =>
                f.name.toLowerCase() === folderName.toLowerCase()
            );
            if (folder) {
                this._selectedFolder = { id: folder.id, name: folder.name, path: folder.path };
                text = query;
            }
        }

        // Build display text with folder indicator
        const displayText = this._selectedFolder
            ? `📂 ${this._selectedFolder.name}: ${text}`
            : text;

        // Add user message
        this.addMessage('user', displayText);

        // ★ Build context: folderId for scoped RAG search, attachedFiles for direct loading
        const folderId = this._selectedFolder?.id || null;

        const attachedPaths = this._attachedFiles.length > 0
            ? this._attachedFiles.map(f => f.file_path)
            : null;

        // Clear attachments & folder after sending
        this._attachedFiles = [];
        this._renderAttachedFiles();
        this._selectedFolder = null;
        this._renderFolderTag();

        try {
            await this.handleSmartQuery(text, attachedPaths, folderId);
        } finally {
            this._sending = false;
            if (sendBtn) sendBtn.disabled = false;
        }
    },

    /** Smart Query — streaming response */
    async handleSmartQuery(text, selectedFiles = null, folderId = null) {
        this.showTyping();

        // ★ Tạo queryId unique cho request này — dùng để ignore events từ query cũ
        const queryId = 'q-' + Date.now() + '-' + Math.random().toString(36).slice(2, 6);
        this._activeQueryId = queryId;

        // Setup streaming state
        let streamText = '';
        let streamEl = null;
        let contentEl = null;
        let streamSourceMap = {};
        let streamAttached = [];
        const unlisteners = [];

        // Helper: check query vẫn active (chưa bị cancel bởi switch/new session)
        const isActive = () => this._activeQueryId === queryId;

        try {
            const { listen } = window.__TAURI__.event;

            // Listen: stream start → create message bubble
            const unlisten1 = await listen('smart-stream-start', (event) => {
                if (!isActive()) return; // ★ Ignore nếu query đã bị cancel
                // source_map từ backend: mapping [N] → file chính xác theo prompt
                const sm = event.payload?.source_map || {};
                streamSourceMap = {};
                Object.keys(sm).forEach(key => {
                    streamSourceMap[parseInt(key)] = sm[key];
                });
                streamAttached = [];
            });
            unlisteners.push(unlisten1);

            // Listen: files changed → auto re-index notification
            const unlistenFiles = await listen('files-changed', (event) => {
                const p = event.payload || {};
                const count = p.count || 0;
                const indexed = p.indexed || 0;
                const deleted = p.deleted || 0;
                const isStartup = p.startup || false;
                if (count > 0) {
                    const label = isStartup
                        ? `🔄 ${count} file thay đổi khi offline`
                        : `✅ Tự động cập nhật: ${indexed} indexed, ${deleted} xoá`;
                    // Inline toast notification
                    const toast = document.createElement('div');
                    toast.className = 'auto-reindex-toast';
                    toast.textContent = label;
                    toast.style.cssText = 'position:fixed;bottom:20px;right:20px;background:var(--bg-tertiary,#2a2a3e);color:var(--text-primary,#e0e0e0);padding:10px 16px;border-radius:8px;font-size:13px;z-index:9999;opacity:0;transition:opacity 0.3s;box-shadow:0 4px 12px rgba(0,0,0,0.3);';
                    document.body.appendChild(toast);
                    requestAnimationFrame(() => toast.style.opacity = '1');
                    setTimeout(() => { toast.style.opacity = '0'; setTimeout(() => toast.remove(), 300); }, 3000);
                    console.log('[Watcher]', label);
                }
            });
            unlisteners.push(unlistenFiles);

            // Listen: sources found → hiển thị danh sách file NGAY LẬP TỨC
            const unlistenSources = await listen('smart-sources-found', (event) => {
                if (!isActive()) return; // ★ Ignore nếu query đã bị cancel
                const allFiles = event.payload?.files || [];
                // Backend đã filter theo min_match_score từ settings
                const files = allFiles;
                if (files.length === 0) return;

                this.hideTyping();
                const messagesEl = document.getElementById('chat-messages');
                if (!messagesEl) return;

                // Tạo khối "Nguồn tài liệu" hiển thị trước AI response
                const sourcesEl = document.createElement('div');
                sourcesEl.id = 'sources-preview';
                sourcesEl.className = 'message message--assistant sources-preview';

                const countLabel = files.length > 0
                    ? `📂 Tìm thấy ${files.length} tài liệu liên quan`
                    : `📂 Đang tìm kiếm trong ${allFiles.length} tài liệu...`;

                sourcesEl.innerHTML = `
                    <div class="message-avatar">
                        <i data-lucide="file-search"></i>
                    </div>
                    <div class="message-body">
                        <div class="sources-header">
                            <span class="sources-label">${countLabel}</span>
                        </div>
                        <div class="sources-chips">
                            ${files.map(f => {
                    const scorePercent = Math.round(f.score * 100);
                    return `<span class="source-chip" data-path="${f.file_path}" title="${f.file_path}">
                                    <i data-lucide="file-text"></i>
                                    <span class="source-name">${f.file_name}</span>
                                    <span class="source-score">${scorePercent}%</span>
                                </span>`;
                }).join('')}
                        </div>
                        <div class="sources-status">
                            <span class="sources-analyzing">Đang phân tích nội dung...</span>
                            <span class="privacy-badge" title="Tìm kiếm cục bộ, dữ liệu đã được lọc PII trước khi gửi AI">
                                🔒 Tìm kiếm cục bộ • ☁️ AI: Gemini (PII đã lọc)
                            </span>
                        </div>
                    </div>
                `;
                messagesEl.appendChild(sourcesEl);
                if (window.lucide) lucide.createIcons({ nodes: [sourcesEl] });

                // Lưu query + keywords để filter chunks
                // ★ Compound terms (VD: "Test 123") phải match exact phrase
                const rawKw = (event.payload?.keywords || text || '').toLowerCase();
                const searchKeywords = (() => {
                    // Detect compound terms: chuỗi 2+ từ có chữ hoa/số liền nhau
                    // Backend gửi keywords đã giữ compound (VD: "Test 123" không split)
                    // Nhưng nếu có thêm từ đơn lẻ, tách riêng
                    const tokens = [];
                    // Tìm compound patterns: "word1 word2" mà word1 có uppercase hoặc là alphanumeric
                    const compoundRe = /\b([A-Za-z]\w*(?:\s+\d+|\s+[A-Z]\w*)+)\b/g;
                    const compounds = [];
                    let match;
                    const original = event.payload?.keywords || text || '';
                    while ((match = compoundRe.exec(original)) !== null) {
                        compounds.push(match[1].toLowerCase());
                    }
                    // Add compound terms as single keywords
                    for (const ct of compounds) {
                        if (ct.length >= 2) tokens.push(ct);
                    }
                    // Add remaining individual words not in any compound
                    for (const w of rawKw.split(/\s+/).filter(w => w.length >= 2)) {
                        const inCompound = compounds.some(ct =>
                            ct.split(/\s+/).includes(w)
                        );
                        if (!inCompound) tokens.push(w);
                    }
                    // Fallback: nếu không detect được compound, dùng toàn bộ raw string nếu ngắn
                    if (tokens.length === 0 && rawKw.length >= 2) {
                        tokens.push(rawKw.trim());
                    }
                    return tokens;
                })();

                // Click handler cho từng file chip → expand inline MATCHED chunks
                sourcesEl.querySelectorAll('.source-chip').forEach(chip => {
                    chip.addEventListener('click', async () => {
                        const path = chip.dataset.path;

                        // Toggle: nếu đã expand → collapse
                        const existing = chip.parentElement.querySelector(`.source-expand[data-for="${CSS.escape(path)}"]`);
                        if (existing) {
                            existing.remove();
                            chip.classList.remove('source-chip--expanded');
                            return;
                        }

                        // Tạo expand area
                        chip.classList.add('source-chip--expanded');
                        const expandEl = document.createElement('div');
                        expandEl.className = 'source-expand';
                        expandEl.dataset.for = path;
                        expandEl.innerHTML = '<div class="preview-loading">Đang tải nội dung...</div>';
                        chip.parentElement.insertBefore(expandEl, chip.nextSibling);

                        try {
                            const data = await API.getFileChunks(path);
                            if (!data?.chunks?.length) {
                                expandEl.innerHTML = '<div class="preview-empty">Chưa có chunks</div>';
                                return;
                            }

                            // ★ AND-majority: chunk phải match ≥50% keywords (giống backend)
                            const totalKw = searchKeywords.length || 1;
                            const minHitRatio = 0.5;

                            const scored = data.chunks.map((c, idx) => {
                                const lower = (c.content || '').toLowerCase();
                                let hits = 0;
                                const matchedKws = [];
                                for (const kw of searchKeywords) {
                                    if (lower.includes(kw)) {
                                        hits++;
                                        matchedKws.push(kw);
                                    }
                                }
                                const ratio = hits / totalKw;
                                return { chunk: c, idx, hits, ratio, matchedKws };
                            });

                            // Chỉ lấy chunks match ≥50% keywords
                            let matched = scored.filter(s => s.ratio >= minHitRatio);
                            matched.sort((a, b) => b.hits - a.hits);

                            // Nếu không chunk nào đủ 50% → thử ≥30%, nếu vẫn không → top 3 theo hits
                            if (matched.length === 0) {
                                matched = scored.filter(s => s.ratio >= 0.3);
                                matched.sort((a, b) => b.hits - a.hits);
                            }
                            if (matched.length === 0) {
                                matched = scored.filter(s => s.hits > 0).sort((a, b) => b.hits - a.hits).slice(0, 3);
                            }

                            const maxShow = 5;
                            const shown = matched.slice(0, maxShow);
                            const totalMatched = matched.length;

                            // Helper: highlight keywords trong text
                            const highlightKws = (text, kws) => {
                                let html = text.replace(/</g, '&lt;').replace(/>/g, '&gt;');
                                for (const kw of kws) {
                                    const escaped = kw.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
                                    html = html.replace(new RegExp(`(${escaped})`, 'gi'), '<mark class="kw-hl">$1</mark>');
                                }
                                return html;
                            };

                            // Helper: detect tabular data (tab-separated hoặc nhiều spaces)
                            const renderChunkContent = (content, kws) => {
                                const lines = content.split('\n').filter(l => l.trim());
                                // Detect: ≥3 dòng có ≥2 tab hoặc ≥2 chuỗi 2+ spaces
                                const tabLines = lines.filter(l => (l.match(/\t/g) || []).length >= 2 || (l.match(/  +/g) || []).length >= 2);

                                if (tabLines.length >= 3 && tabLines.length >= lines.length * 0.5) {
                                    // Render dạng bảng
                                    const rows = lines.slice(0, 15).map(l => l.split(/\t|  +/).filter(c => c.trim()));
                                    const maxCols = Math.max(...rows.map(r => r.length));
                                    let tableHtml = '<div class="chunk-table-wrap"><table class="chunk-table">';
                                    rows.forEach((row, ri) => {
                                        const tag = ri === 0 ? 'th' : 'td';
                                        tableHtml += '<tr>';
                                        for (let ci = 0; ci < maxCols; ci++) {
                                            const cell = row[ci] || '';
                                            tableHtml += `<${tag}>${highlightKws(cell, kws)}</${tag}>`;
                                        }
                                        tableHtml += '</tr>';
                                    });
                                    tableHtml += '</table></div>';
                                    if (lines.length > 15) tableHtml += `<div class="chunk-table-more">+${lines.length - 15} dòng nữa...</div>`;
                                    return tableHtml;
                                }

                                // Render text thường với highlight
                                const preview = content.substring(0, 400);
                                const more = content.length > 400 ? '…' : '';
                                return `<span class="preview-chunk-text">${highlightKws(preview, kws)}${more}</span>`;
                            };

                            const matchLabel = totalMatched === 0
                                ? `<span class="match-none">Không có chunk nào đủ khớp</span>`
                                : `<span class="match-count">${totalMatched}</span>/<span class="match-total">${data.chunks.length}</span> chunks khớp (≥${Math.round(minHitRatio * 100)}% từ khóa)`;

                            expandEl.innerHTML = `<div class="expand-match-info">🎯 ${matchLabel}</div>` +
                                shown.map(s => {
                                    const hitPct = Math.round(s.ratio * 100);
                                    return `<div class="preview-chunk">
                                        <div class="preview-chunk-header">
                                            <span class="preview-chunk-idx">#${s.idx + 1}</span>
                                            <span class="preview-chunk-hits" title="${s.hits}/${totalKw} từ khóa">${hitPct}%</span>
                                        </div>
                                        <div class="preview-chunk-body">${renderChunkContent(s.chunk.content, s.matchedKws)}</div>
                                    </div>`;
                                }).join('') +
                                (totalMatched > maxShow
                                    ? `<div class="preview-more-info">Còn ${totalMatched - maxShow} chunks khớp khác</div>`
                                    : '');

                        } catch (err) {
                            expandEl.innerHTML = `<div class="preview-empty">Lỗi: ${err}</div>`;
                        }
                    });
                });

                // Auto-scroll
                messagesEl.scrollTop = messagesEl.scrollHeight;

                // Hiện lại typing indicator bên dưới (chờ AI)
                this.showTyping();
            });
            unlisteners.push(unlistenSources);

            // Listen: stream chunk → append text progressively
            const unlisten2 = await listen('smart-stream', (event) => {
                if (!isActive()) return; // ★ Ignore nếu query đã bị cancel
                const chunk = event.payload?.chunk;
                if (!chunk) return;

                // First chunk → hide typing, create message element
                if (!streamEl) {
                    this.hideTyping();
                    // Cập nhật sources-preview: AI đã bắt đầu trả lời
                    const statusEl = document.querySelector('#sources-preview .sources-analyzing');
                    if (statusEl) statusEl.textContent = '✅ Đã phân tích xong';

                    const messagesEl = document.getElementById('chat-messages');
                    if (!messagesEl) return;

                    streamEl = document.createElement('div');
                    streamEl.className = 'message message--assistant streaming';
                    streamEl.innerHTML = `
                        <div class="message-avatar">
                            <i data-lucide="sparkles"></i>
                        </div>
                        <div class="message-body">
                            <div class="message-content"></div>
                        </div>
                    `;
                    messagesEl.appendChild(streamEl);
                    if (window.lucide) lucide.createIcons({ nodes: [streamEl] });
                    contentEl = streamEl.querySelector('.message-content');
                }

                streamText += chunk;

                // Re-render markdown progressively
                if (contentEl) {
                    contentEl.innerHTML = this.formatMarkdown(streamText);
                }

                // Auto-scroll
                const messagesEl = document.getElementById('chat-messages');
                if (messagesEl) messagesEl.scrollTop = messagesEl.scrollHeight;
            });
            unlisteners.push(unlisten2);

            // Listen: stream end
            const unlisten3 = await listen('smart-stream-end', (event) => {
                if (!isActive()) return; // ★ Ignore nếu query đã bị cancel
                // Nhận attached_files đã filter (chỉ file AI thực sự trích dẫn)
                streamAttached = event.payload?.attached_files || [];
                const totalMs = event.payload?.total_ms || 0;

                // ★ Hiện tổng thời gian dưới tin nhắn AI
                if (totalMs > 0 && streamEl) {
                    const timeEl = document.createElement('div');
                    timeEl.className = 'message-timing';
                    const secs = (totalMs / 1000).toFixed(1);
                    timeEl.innerHTML = `⚡ ${secs}s`;
                    timeEl.title = `Tổng thời gian xử lý: ${totalMs}ms`;
                    const body = streamEl.querySelector('.message-body');
                    if (body) body.appendChild(timeEl);
                }

                // Cập nhật tất cả status "Đang phân tích" → "Đã xong"
                document.querySelectorAll('.sources-analyzing').forEach(el => {
                    el.textContent = '✅ Đã phân tích xong';
                });
            });
            unlisteners.push(unlisten3);

            // Execute smart query
            const model = document.getElementById('model-select')?.value || 'gemini-2.0-flash';
            // ★ Lưu unlisteners để _cancelActiveQuery có thể cleanup
            this._activeUnlisteners = unlisteners;

            const result = await API.smartQuery(text, model, this.sessionId, selectedFiles, folderId);

            // Cleanup listeners
            unlisteners.forEach(fn => fn());
            this._activeUnlisteners = [];

            this.hideTyping();
            this._cleanupProgress();

            if (!result) {
                if (streamEl) streamEl.remove();
                this.addMessage('assistant', 'Hệ thống chưa sẵn sàng. Vui lòng thêm thư mục và đợi index.');
                return;
            }

            if (result.keywords && result.keywords !== text.toLowerCase()) {
                console.log(`[SmartQuery] "${text}" → keywords: "${result.keywords}" (${result.intent})`);
            }

            if (result.intent === 'clarify' && result.chat_response) {
                // AI hỏi lại — hiển thị như tin nhắn bình thường
                if (streamEl) streamEl.remove();
                this.hideTyping();
                this._cleanupProgress();
                this.addMessage('assistant', `❓ ${result.chat_response}`);
                this.loadSessions();
                return;
            }

            if (result.intent === 'chat' && result.chat_response) {
                let responseText = result.chat_response;
                const allAttached = result.attached_files || [];

                // Source map từ stream (giữ thứ tự [N] gốc trong prompt)
                const sourceMap = streamSourceMap || {};

                // Citations = attached_files (backend đã filter chỉ file AI thực sự trích dẫn)
                let citations = allAttached.map(f => ({ file_path: f.file_path, file_name: f.file_name }));

                // Bỏ footer 📎 Nguồn: khỏi response text (đã hiện qua citation chips)
                responseText = responseText.replace(/\n?---\s*\n?📎\s*Nguồn:?\s*.+$/s, '').trim();
                responseText = responseText.replace(/\n?📎\s*Nguồn:?\s*.+$/s, '').trim();

                // If streamed → update the existing element with final content + citations
                if (streamEl) {
                    streamEl.classList.remove('streaming');

                    // Render final content with inline citations
                    let renderedContent = this.formatMarkdown(responseText);
                    renderedContent = renderedContent.replace(/\[(\d+)\]/g, (match, num) => {
                        const id = parseInt(num);
                        const src = sourceMap[id];
                        if (src) return `<span class="inline-citation" data-source-id="${id}" data-path="${src.file_path}" title="${src.file_name}">${id}</span>`;
                        if (id <= citations.length && citations[id - 1]) {
                            const c = citations[id - 1];
                            return `<span class="inline-citation" data-source-id="${id}" data-path="${c.file_path}" title="${c.file_name}">${id}</span>`;
                        }
                        return match;
                    });

                    // Build citation chips
                    let citationHTML = '';
                    if (citations.length > 0) {
                        citationHTML = `<div class="message-citations">
                            ${citations.map(c => `<span class="citation-chip" data-path="${c.file_path}" title="${c.file_path}">
                                <i data-lucide="file-text"></i> ${c.file_name}
                            </span>`).join('')}
                        </div>`;
                    }

                    const bodyEl = streamEl.querySelector('.message-body');
                    bodyEl.innerHTML = `<div class="message-content">${renderedContent}</div>${citationHTML}`;

                    if (window.lucide) lucide.createIcons({ nodes: [streamEl] });

                    // Add click handlers
                    streamEl.querySelectorAll('.citation-chip').forEach(chip => {
                        chip.addEventListener('click', () => {
                            App.openSourcePanel(chip.textContent.trim(), `Đang tải nội dung từ: ${chip.dataset.path}...`);
                        });
                    });
                    streamEl.querySelectorAll('.inline-citation').forEach(badge => {
                        badge.addEventListener('click', () => {
                            if (badge.dataset.path) App.openSourcePanel(badge.title, `Đang tải nội dung từ: ${badge.dataset.path}...`);
                        });
                    });

                    this.messages.push({ role: 'assistant', content: responseText, citations, sourceMap });
                } else {
                    // No streaming (fallback)
                    this.addMessage('assistant', responseText, citations, false, sourceMap);
                }
            } else if (result.intent === 'select_files' && result.attached_files?.length > 0) {
                // ★ Interactive file selection — user chọn file trước khi AI phân tích
                if (streamEl) streamEl.remove();
                this.hideTyping();
                this._cleanupProgress();

                const files = result.attached_files;
                const messagesEl = document.getElementById('chat-messages');
                if (!messagesEl) return;

                const selectEl = document.createElement('div');
                selectEl.className = 'message message--assistant file-select-message';
                selectEl.innerHTML = `
                    <div class="message-avatar">
                        <i data-lucide="files"></i>
                    </div>
                    <div class="message-body">
                        <div class="file-select-header">
                            <span class="file-select-label">📋 Tìm thấy <strong>${files.length}</strong> tài liệu phù hợp</span>
                            <span class="file-select-hint">Chọn tài liệu cần phân tích</span>
                        </div>
                        <div class="file-select-chips">
                            ${files.map((f, i) => {
                    const scorePercent = Math.round(f.score * 100);
                    return `<label class="file-select-chip selected" data-path="${f.file_path}">
                                    <input type="checkbox" checked data-path="${f.file_path}" />
                                    <i data-lucide="file-text"></i>
                                    <span class="file-select-name">${f.file_name}</span>
                                    <span class="file-select-score">${scorePercent}%</span>
                                </label>`;
                }).join('')}
                        </div>
                        <div class="file-select-actions">
                            <button class="file-select-btn" id="btn-analyze-selected" title="Phân tích các file đã chọn">
                                <i data-lucide="sparkles"></i>
                                <span>Phân tích <span id="selected-count">${files.length}</span> tài liệu</span>
                            </button>
                        </div>
                    </div>
                `;
                messagesEl.appendChild(selectEl);
                if (window.lucide) lucide.createIcons({ nodes: [selectEl] });

                // Toggle file chips
                selectEl.querySelectorAll('.file-select-chip').forEach(chip => {
                    chip.addEventListener('click', (e) => {
                        if (e.target.tagName === 'INPUT') return; // checkbox handles itself
                        const cb = chip.querySelector('input[type="checkbox"]');
                        cb.checked = !cb.checked;
                        chip.classList.toggle('selected', cb.checked);
                        // Update count
                        const count = selectEl.querySelectorAll('.file-select-chip input:checked').length;
                        const countEl = document.getElementById('selected-count');
                        if (countEl) countEl.textContent = count;
                        const btn = document.getElementById('btn-analyze-selected');
                        if (btn) btn.disabled = count === 0;
                    });
                    // Keep chip synced with checkbox
                    const cb = chip.querySelector('input[type="checkbox"]');
                    cb.addEventListener('change', () => {
                        chip.classList.toggle('selected', cb.checked);
                        const count = selectEl.querySelectorAll('.file-select-chip input:checked').length;
                        const countEl = document.getElementById('selected-count');
                        if (countEl) countEl.textContent = count;
                        const btn = document.getElementById('btn-analyze-selected');
                        if (btn) btn.disabled = count === 0;
                    });
                });

                // "Phân tích" button → Phase 2
                const analyzeBtn = document.getElementById('btn-analyze-selected');
                if (analyzeBtn) {
                    analyzeBtn.addEventListener('click', async () => {
                        const selectedPaths = [];
                        selectEl.querySelectorAll('.file-select-chip input:checked').forEach(cb => {
                            selectedPaths.push(cb.dataset.path);
                        });
                        if (selectedPaths.length === 0) {
                            App.toast('Vui lòng chọn ít nhất 1 tài liệu', 'warning');
                            return;
                        }

                        // Disable button, show loading
                        analyzeBtn.disabled = true;
                        analyzeBtn.innerHTML = '<i data-lucide="loader-2" class="spin"></i> <span>Đang phân tích...</span>';
                        if (window.lucide) lucide.createIcons({ nodes: [analyzeBtn] });

                        // Phase 2: call with selected files
                        try {
                            await this.handleSmartQuery(text, selectedPaths);
                        } catch (err) {
                            console.error('[FileSelect] Error:', err);
                            this.addMessage('assistant', '❌ Lỗi phân tích. Vui lòng thử lại.', [], true);
                        }
                    });
                }

                messagesEl.scrollTop = messagesEl.scrollHeight;
                this.loadSessions();
                return; // Don't continue — wait for user selection

            } else if (result.search_results && result.search_results.results?.length > 0) {
                if (streamEl) streamEl.remove();
                this.addSearchResults(result.search_results);
            } else if (!result.chat_response && result.intent !== 'select_files') {
                if (streamEl) streamEl.remove();
                this.addMessage('assistant', `Không tìm thấy kết quả cho "${text}".\n\nThử:\n• Dùng từ khóa cụ thể hơn\n• Kiểm tra thư mục đã được thêm chưa\n• Đợi quá trình index hoàn tất`);
            }
            // Reload sessions sidebar
            this.loadSessions();
        } catch (err) {
            unlisteners.forEach(fn => fn());
            this.hideTyping();
            this._cleanupProgress();
            if (streamEl) streamEl.remove();
            console.error('[Chatbot] Error:', err);
            this.addMessage('assistant', '❌ Lỗi xử lý. Vui lòng thử lại.', [], true);
            this.loadSessions();
        }
    },

    /** Thêm tin nhắn vào UI */
    addMessage(role, content, citations = [], showRetry = false, sourceMap = {}) {
        const messagesEl = document.getElementById('chat-messages');
        if (!messagesEl) return;

        // ★ Strip [Sources: ...] metadata (dùng cho follow-up context, không hiện UI)
        // AI đã tự cite "Nguồn: [N]..." nên [Sources:] thừa
        if (role === 'assistant' && content) {
            content = content.replace(/\n?\[Sources:\s*[^\]]*\]/g, '').trim();
        }

        const msg = document.createElement('div');
        msg.className = `message message--${role}`;

        const avatarIcon = role === 'assistant' ? 'sparkles' : 'user';

        let citationHTML = '';
        if (citations.length > 0) {
            citationHTML = `
        <div class="message-citations">
          ${citations.map(c => `
            <span class="citation-chip" data-path="${c.file_path}" title="${c.file_path}">
              <i data-lucide="file-text"></i>
              ${c.file_name}
            </span>
          `).join('')}
        </div>
      `;
        }

        let retryHTML = '';
        if (showRetry) {
            retryHTML = `
        <button class="message-retry" onclick="Chat.retry()" title="Thử lại">
          <i data-lucide="refresh-cw"></i>
          Thử lại
        </button>
      `;
        }

        let renderedContent = role === 'assistant'
            ? this.formatMarkdown(content)
            : this.escapeHtml(content);

        // Render inline citations [1], [2] as styled badges
        if (role === 'assistant') {
            renderedContent = renderedContent.replace(/\[(\d+)\]/g, (match, num) => {
                const id = parseInt(num);
                const src = sourceMap[id];
                // Also check from previous messages' sourceMap
                if (src) {
                    return `<span class="inline-citation" data-source-id="${id}" data-path="${src.file_path}" title="${src.file_name}">${id}</span>`;
                }
                // Check from citations array
                if (id <= citations.length && citations[id - 1]) {
                    const c = citations[id - 1];
                    return `<span class="inline-citation" data-source-id="${id}" data-path="${c.file_path}" title="${c.file_name}">${id}</span>`;
                }
                return match;
            });
        }

        msg.innerHTML = `
      <div class="message-avatar">
        <i data-lucide="${avatarIcon}"></i>
      </div>
      <div class="message-body">
        <div class="message-content">${renderedContent}</div>
        ${citationHTML}
        ${retryHTML}
      </div>
    `;

        messagesEl.appendChild(msg);
        messagesEl.scrollTop = messagesEl.scrollHeight;

        // Initialize Lucide icons in new message
        if (window.lucide) lucide.createIcons({ nodes: [msg] });

        // Add citation chip click handlers
        msg.querySelectorAll('.citation-chip').forEach(chip => {
            chip.addEventListener('click', () => {
                const path = chip.dataset.path;
                App.openSourcePanel(chip.textContent.trim(), `Đang tải nội dung từ: ${path}...`);
            });
        });

        // Add inline citation click handlers
        msg.querySelectorAll('.inline-citation').forEach(badge => {
            badge.addEventListener('click', () => {
                const path = badge.dataset.path;
                const name = badge.title;
                if (path) App.openSourcePanel(name, `Đang tải nội dung từ: ${path}...`);
            });
        });

        // Save to messages array (include sourceMap for follow-ups)
        this.messages.push({ role, content, citations, sourceMap });
    },

    /** Hiện search results dạng danh sách file */
    addSearchResults(response) {
        const messagesEl = document.getElementById('chat-messages');
        if (!messagesEl) return;

        const msg = document.createElement('div');
        msg.className = 'message message--assistant';

        const resultsHTML = response.results.map(r => {
            // Highlight keywords in preview
            let preview = this.escapeHtml(r.content_preview);
            if (r.highlights && r.highlights.length > 0) {
                for (const kw of r.highlights) {
                    const regex = new RegExp(`(${kw.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')})`, 'gi');
                    preview = preview.replace(regex, '<mark>$1</mark>');
                }
            }

            // Score already normalized to 0-1
            const scorePercent = Math.round(r.score * 100);

            return `
        <div class="search-result-item" data-path="${r.file_path}">
            <div class="search-result-header">
                <i data-lucide="file-text"></i>
                <span class="search-result-filename">${this.escapeHtml(r.file_name)}</span>
                <span class="search-result-score">${scorePercent}%</span>
            </div>
            <div class="search-result-preview">${preview}</div>
            <div class="search-result-path">${this.escapeHtml(r.file_path)}</div>
        </div>`;
        }).join('');

        msg.innerHTML = `
      <div class="message-avatar">
        <i data-lucide="search"></i>
      </div>
      <div class="message-body">
        <div class="message-content">Tìm thấy <strong>${response.total}</strong> kết quả (${response.duration_ms}ms)</div>
        ${resultsHTML}
      </div>
    `;

        messagesEl.appendChild(msg);
        messagesEl.scrollTop = messagesEl.scrollHeight;

        if (window.lucide) lucide.createIcons({ nodes: [msg] });

        // Click to open source preview
        msg.querySelectorAll('.search-result-item').forEach(item => {
            item.addEventListener('click', () => {
                App.openSourcePanel(
                    item.querySelector('.search-result-filename')?.textContent || 'File',
                    `Đang tải: ${item.dataset.path}...`
                );
            });
        });
    },

    /** Hiện typing indicator dạng thinking log */
    showTyping() {
        const messagesEl = document.getElementById('chat-messages');
        if (!messagesEl) return;

        // Remove existing typing
        this.hideTyping();

        const typing = document.createElement('div');
        typing.id = 'typing-indicator';
        typing.className = 'message message--assistant';
        typing.innerHTML = `
      <div class="message-avatar">
        <i data-lucide="sparkles"></i>
      </div>
      <div class="message-body">
        <div class="thinking-log">
          <div class="thinking-steps"></div>
          <div class="thinking-dots">
            <div class="typing-dot"></div>
            <div class="typing-dot"></div>
            <div class="typing-dot"></div>
          </div>
        </div>
      </div>
    `;

        messagesEl.appendChild(typing);
        messagesEl.scrollTop = messagesEl.scrollHeight;

        if (window.lucide) lucide.createIcons({ nodes: [typing] });

        // Listen for progress events — each step = new line
        if (window.__TAURI__) {
            const { listen } = window.__TAURI__.event;
            listen('smart-progress', (event) => {
                const stepsEl = document.querySelector('#typing-indicator .thinking-steps');
                if (stepsEl && event.payload) {
                    const step = event.payload.step || '';
                    const detail = event.payload.detail || '';
                    const isDone = event.payload.done || false;
                    const duration = event.payload.duration_ms || 0;

                    let line = stepsEl.querySelector(`#step-${step}`);
                    if (!line) {
                        line = document.createElement('div');
                        line.id = `step-${step}`;
                        line.className = 'thinking-step pending';
                        stepsEl.appendChild(line);
                    }

                    if (isDone) {
                        line.classList.remove('pending');
                        line.classList.add('done');
                        // Always show duration badge for transparency
                        const timeSpan = line.querySelector('.step-time');
                        if (!timeSpan) {
                            const badge = document.createElement('span');
                            badge.className = 'step-time';
                            if (duration > 2000) badge.classList.add('step-time--slow');
                            const label = duration < 1000 ? `${duration}ms` : `${(duration / 1000).toFixed(1)}s`;
                            badge.textContent = label;
                            line.appendChild(badge);
                        }
                    } else if (detail) {
                        // Backend already sends emojis — render directly
                        line.innerHTML = `<span class="step-text">${detail}</span>`;
                    }

                    // Auto scroll
                    const messagesEl = document.getElementById('chat-messages');
                    if (messagesEl) messagesEl.scrollTop = messagesEl.scrollHeight;
                }
            }).then(unlisten => {
                this._progressUnlisten = unlisten;
            });
        }
    },

    hideTyping() {
        const typing = document.getElementById('typing-indicator');
        if (typing) typing.remove();
        // ★ KHÔNG xóa _progressUnlisten ở đây — giữ listener sống
        // Listener sẽ bị cleanup khi query hoàn thành (handleSmartQuery finally)
    },

    /** Cleanup progress listener (gọi khi query xong) */
    _cleanupProgress() {
        if (this._progressUnlisten) {
            this._progressUnlisten();
            this._progressUnlisten = null;
        }
    },

    /** Retry last failed message */
    retry() {
        if (this.messages.length >= 2) {
            const lastUser = [...this.messages].reverse().find(m => m.role === 'user');
            if (lastUser) {
                // Remove error message
                const messagesEl = document.getElementById('chat-messages');
                if (messagesEl && messagesEl.lastElementChild) {
                    messagesEl.lastElementChild.remove();
                    this.messages.pop();
                }

                // Re-send via smart query
                this.handleSmartQuery(lastUser.content);
            }
        }
    },

    /** Escape HTML for safe rendering */
    escapeHtml(text) {
        const div = document.createElement('div');
        div.textContent = text;
        return div.innerHTML;
    },

    /** Simple markdown → HTML for assistant messages */
    formatMarkdown(text) {
        // Step 1: Parse markdown tables first (before any escaping)
        const lines = text.split('\n');
        const processed = [];
        let tableLines = [];
        let inTable = false;

        const flushTable = () => {
            if (tableLines.length < 2) {
                tableLines.forEach(l => processed.push(l));
                tableLines = [];
                return;
            }
            let html = '<table class="md-table"><thead>';
            let inHead = true;

            for (const line of tableLines) {
                const trimmed = line.trim();
                // Split cells by pipe
                const cells = trimmed.replace(/^\|/, '').replace(/\|$/, '').split('|');

                // Detect separator row: all cells are only dashes/colons/spaces
                const isSep = cells.length > 0 && cells.every(c => /^[\s\-:]+$/.test(c) && c.trim().length > 0);

                if (isSep) {
                    html += '</thead><tbody>';
                    inHead = false;
                    continue;
                }

                const tag = inHead ? 'th' : 'td';
                const cellsContent = cells.map(c => c.trim()).filter(c => c.length > 0 || cells.length <= 3);
                if (cellsContent.length === 0) continue;

                html += '<tr>' + cellsContent.map(c => {
                    let cell = this.escapeHtml(c);
                    cell = cell.replace(/\*\*(.+?)\*\*/g, '<strong>$1</strong>');
                    cell = cell.replace(/\*(.+?)\*/g, '<em>$1</em>');
                    cell = cell.replace(/`([^`]+)`/g, '<code>$1</code>');
                    return `<${tag}>${cell}</${tag}>`;
                }).join('') + '</tr>';
            }

            if (inHead) html += '</thead>';
            html += '</tbody></table>';
            processed.push(`<div class="md-table-wrapper">${html}</div>`);
            tableLines = [];
        };

        // Detect if text is mid-stream (last line might be incomplete)
        const isStreaming = !text.endsWith('\n') && lines.length > 0;

        for (let i = 0; i < lines.length; i++) {
            const line = lines[i];
            const t = line.trim();
            const isLastLine = i === lines.length - 1;

            // A line is a table row if it starts with |
            // During streaming, last line might not end with | yet
            const looksLikeTable = t.startsWith('|') && (t.endsWith('|') || (isLastLine && isStreaming));

            if (looksLikeTable && t.includes('|', 1)) {
                inTable = true;
                tableLines.push(line);
            } else {
                if (inTable) {
                    // If streaming and this is a continuation line (no pipe), 
                    // skip it — it's likely a wrapped separator
                    if (isStreaming && isLastLine && t.length > 0) {
                        // Don't flush yet, table might still be building
                        processed.push(line);
                    } else {
                        flushTable();
                    }
                    inTable = false;
                } else {
                    processed.push(line);
                }
            }
        }
        if (inTable) {
            if (isStreaming) {
                // Still streaming — render partial table anyway so user sees progress
                flushTable();
            } else {
                flushTable();
            }
        }

        // Step 2: Process non-table lines
        let html = processed.map(line => {
            if (line.startsWith('<div class="md-table-wrapper">')) return line;

            let h = this.escapeHtml(line);
            // Bold
            h = h.replace(/\*\*(.+?)\*\*/g, '<strong>$1</strong>');
            // Italic
            h = h.replace(/\*(.+?)\*/g, '<em>$1</em>');
            // Inline code
            h = h.replace(/`([^`]+)`/g, '<code>$1</code>');
            // Headers: # ## ### ####
            h = h.replace(/^#### (.+)$/, '<h5>$1</h5>');
            h = h.replace(/^### (.+)$/, '<h4>$1</h4>');
            h = h.replace(/^## (.+)$/, '<h3>$1</h3>');
            h = h.replace(/^# (.+)$/, '<h2>$1</h2>');
            // Horizontal rule
            h = h.replace(/^---+$/, '<hr>');
            // Bullet lists
            h = h.replace(/^[•\-\*] (.+)$/, '<li>$1</li>');
            // Numbered lists
            h = h.replace(/^\d+\. (.+)$/, '<li>$1</li>');
            // Blockquote
            h = h.replace(/^&gt; (.+)$/, '<blockquote>$1</blockquote>');
            return h;
        }).join('\n');

        // Wrap consecutive <li> in <ul>
        html = html.replace(/(<li>.+<\/li>\n?)+/g, '<ul>$&</ul>');
        // Line breaks (not inside tables/lists)
        html = html.replace(/\n/g, '<br>');
        // Clean up br around block elements
        const blockTags = ['ul', 'ol', 'li', 'table', 'div', 'h2', 'h3', 'h4', 'h5', 'hr', 'blockquote'];
        for (const tag of blockTags) {
            html = html.replace(new RegExp(`<br>(<${tag}[^>]*>)`, 'g'), '$1');
            html = html.replace(new RegExp(`(</${tag}>)<br>`, 'g'), '$1');
        }

        return html;
    },

};

// Initialize chat
document.addEventListener('DOMContentLoaded', () => {
    Chat.init();
});
