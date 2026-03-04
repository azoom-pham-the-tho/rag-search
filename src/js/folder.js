/**
 * RAG Search — Folder Management
 * Folder tree, drag-drop, status display
 */
const Folder = {
    folders: [],

    init() {
        this.initAddFolderButton();
        this.initDragDrop();
        this.initStopButton();
        this.loadFolders();
        this.refreshStatus();
        this.listenIndexEvents();
    },

    /** Lắng nghe events từ backend về tiến trình index (bulk) */
    listenIndexEvents() {
        if (!window.__TAURI__) return;

        window.__TAURI__.event.listen('index-progress', (event) => {
            const data = event.payload;

            if (data.phase === 'scanning') {
                App.toast(`🔍 Đang quét ${data.folder_name}...`, 'info');
            } else if (data.phase === 'done') {
                App.toast(`✅ Index xong: ${data.indexed_count} files`, 'success');
                setTimeout(async () => {
                    this.clearFileCache();
                    await this.loadFolders();
                    // Auto-expand folder vừa index
                    const folderItem = document.querySelector(`[data-id="${data.folder_id}"]`);
                    if (folderItem && !folderItem.classList.contains('folder-item--expanded')) {
                        this.toggleFiles(data.folder_id, folderItem);
                    }
                }, 300);
            } else if (data.phase === 'stopped') {
                App.toast(`⏹ Đã dừng: ${data.indexed_count}/${data.total_files} files`, 'info');
                setTimeout(async () => {
                    this.clearFileCache();
                    await this.loadFolders();
                }, 300);
            } else if (data.phase === 'error') {
                App.toast(`❌ Lỗi index: ${data.message}`, 'error');
            }
        });

        // ★ Listen: single file indexing progress → update file row inline
        this.listenSingleFileProgress();
    },

    /** Lắng nghe progress từng file — hiện phase icon + progress bar */
    listenSingleFileProgress() {
        if (!window.__TAURI__) return;

        const phaseIcons = {
            parsing: '📄',
            chunking: '✂️',
            chunked: '✂️',
            indexing: '📝',
            embedding: '🧠',
            saving: '💾',
            done: '✅',
        };

        window.__TAURI__.event.listen('index-single-progress', (event) => {
            const data = event.payload;
            const filePath = data.file_path;
            const icon = phaseIcons[data.phase] || '⏳';

            // Find the file row
            const fileItem = document.querySelector(`.folder-file-item[data-path="${CSS.escape(filePath)}"]`);
            if (!fileItem) return;

            // Update status icon
            const statusEl = fileItem.querySelector('.file-sync-status');
            if (statusEl) {
                statusEl.textContent = icon;
                statusEl.title = data.detail || data.phase;
            }

            // ★ Show/update mini progress bar for embedding phase
            let progressEl = fileItem.querySelector('.file-progress');

            if (data.phase === 'embedding' && data.percent !== undefined) {
                // Clear any waiting countdown
                if (fileItem._waitTimer) {
                    clearInterval(fileItem._waitTimer);
                    fileItem._waitTimer = null;
                }
                if (!progressEl) {
                    progressEl = document.createElement('div');
                    progressEl.className = 'file-progress';
                    fileItem.appendChild(progressEl);
                }
                progressEl.classList.remove('file-progress--waiting');
                progressEl.innerHTML = `
                    <div class="file-progress-track">
                        <div class="file-progress-fill" style="width:${data.percent}%"></div>
                    </div>
                    <span class="file-progress-text">${data.chunks_done}/${data.total_chunks} — ${data.percent}%</span>
                `;
            } else if (data.phase === 'waiting') {
                // ★ Show waiting countdown
                if (!progressEl) {
                    progressEl = document.createElement('div');
                    progressEl.className = 'file-progress';
                    fileItem.appendChild(progressEl);
                }
                progressEl.classList.add('file-progress--waiting');
                let remaining = data.wait_seconds || 60;
                const updateWait = () => {
                    progressEl.innerHTML = `
                        <span class="file-progress-text file-progress-text--waiting">⏳ Hết token, đợi ${remaining}s...</span>
                    `;
                };
                updateWait();
                // Clear old timer
                if (fileItem._waitTimer) clearInterval(fileItem._waitTimer);
                fileItem._waitTimer = setInterval(() => {
                    remaining--;
                    if (remaining <= 0) {
                        clearInterval(fileItem._waitTimer);
                        fileItem._waitTimer = null;
                        return;
                    }
                    updateWait();
                }, 1000);
            } else if (data.phase === 'done' || data.phase === 'saving') {
                // Remove progress bar + cleanup
                if (fileItem._waitTimer) {
                    clearInterval(fileItem._waitTimer);
                    fileItem._waitTimer = null;
                }
                if (progressEl) progressEl.remove();
            }
        });
    },

    /** Clear cached file lists (khi index xong, cần re-fetch) */
    clearFileCache() {
        document.querySelectorAll('.folder-files').forEach(el => {
            delete el.dataset.loaded;
        });
    },

    /** Nút dừng indexing */
    initStopButton() {
        const btn = document.getElementById('btn-stop-indexing');
        if (btn) {
            btn.addEventListener('click', async () => {
                try {
                    btn.disabled = true;
                    btn.textContent = '⏳';
                    await API.stopIndexing();
                } catch (err) {
                    console.warn('[Folder] Stop error:', err);
                } finally {
                    btn.disabled = false;
                    btn.textContent = '⏹';
                }
            });
        }
    },

    /** Hiện/ẩn nút dừng */
    showStopButton(show) {
        const btn = document.getElementById('btn-stop-indexing');
        if (btn) btn.style.display = show ? 'inline-flex' : 'none';
    },

    /** Nút thêm folder */
    initAddFolderButton() {
        const btn = document.getElementById('btn-add-folder');
        if (btn) {
            btn.addEventListener('click', () => this.addFolder());
        }

        // Welcome screen action
        const actionFolder = document.getElementById('action-folder');
        if (actionFolder) {
            actionFolder.addEventListener('click', () => this.addFolder());
        }
    },

    /** Thêm folder qua Tauri dialog */
    async addFolder() {
        try {
            // Dùng Tauri dialog để chọn folder
            if (window.__TAURI__) {
                const { open } = window.__TAURI__.dialog;
                const selected = await open({
                    directory: true,
                    multiple: false,
                    title: 'Chọn thư mục cần index',
                });
                if (selected) {
                    await this.registerFolder(selected);
                }
            } else {
                // Dev fallback
                const path = prompt('Nhập đường dẫn thư mục:');
                if (path) {
                    await this.registerFolder(path);
                }
            }
        } catch (err) {
            App.toast('Không thể thêm thư mục: ' + err, 'error');
        }
    },

    /** Register folder với backend */
    async registerFolder(path) {
        try {
            App.toast('Đang quét và index thư mục...', 'info');
            const folder = await API.addFolder(path);
            if (folder) {
                this.folders.push(folder);
                this.renderTree();
                this.refreshStatus();
                App.toast(`Đã thêm: ${folder.name} (${folder.file_count} files, ${folder.indexed_count} indexed)`, 'success');
            }
        } catch (err) {
            App.toast('Lỗi: ' + err, 'error');
        }
    },

    /** Cập nhật trạng thái — nay chỉ reload folder badges */
    async refreshStatus() {
        // Status section đã xóa — thông tin hiện trực tiếp trên folder header
    },

    /** Drag & Drop folders */
    initDragDrop() {
        const sidebar = document.getElementById('sidebar');
        if (!sidebar) return;

        sidebar.addEventListener('dragover', (e) => {
            e.preventDefault();
            sidebar.classList.add('drag-over');
        });

        sidebar.addEventListener('dragleave', () => {
            sidebar.classList.remove('drag-over');
        });

        sidebar.addEventListener('drop', async (e) => {
            e.preventDefault();
            sidebar.classList.remove('drag-over');

            // Handle dropped folders
            const items = e.dataTransfer?.items;
            if (items) {
                for (const item of items) {
                    if (item.kind === 'file') {
                        const entry = item.webkitGetAsEntry?.();
                        if (entry?.isDirectory) {
                            // Note: web API doesn't give full path,
                            // need Tauri for actual folder selection
                            App.toast('Dùng nút + để thêm thư mục', 'info');
                        }
                    }
                }
            }
        });
    },

    /** Load folders từ backend */
    async loadFolders() {
        try {
            const folders = await API.listFolders();
            if (folders) {
                this.folders = folders;
                this.renderTree();
            }
        } catch (err) {
            console.warn('[Folder] Could not load folders:', err);
        }
    },

    /** Render folder tree UI — expandable with file list */
    renderTree() {
        const tree = document.getElementById('folder-tree');
        const empty = document.getElementById('empty-folders');
        if (!tree) return;

        // Clear existing items (keep empty state)
        tree.querySelectorAll('.folder-item').forEach(el => el.remove());

        if (this.folders.length === 0) {
            if (empty) empty.style.display = '';
            return;
        }

        if (empty) empty.style.display = 'none';

        this.folders.forEach(folder => {
            const item = document.createElement('div');
            item.className = 'folder-item';
            item.dataset.id = folder.id;

            const fileCount = folder.file_count || 0;

            item.innerHTML = `
        <div class="folder-header" title="${folder.path}">
          <i data-lucide="chevron-right" class="folder-chevron"></i>
          <i data-lucide="folder" class="folder-icon"></i>
          <span class="folder-name">${folder.name}</span>
          <span class="folder-file-count" title="${fileCount} files">${fileCount}</span>
          <span class="folder-sync-badge" id="sync-badge-${folder.id}" style="display:none;"></span>
          <button class="btn-icon-sm folder-sync-all" id="sync-all-${folder.id}" title="Đồng bộ tất cả" style="display:none;" onclick="event.stopPropagation(); Folder.syncAllFiles('${folder.id}', this)">
            <i data-lucide="upload-cloud"></i>
          </button>
          <div class="folder-actions">
            <button class="btn-icon-sm folder-reindex" title="Quét lại file" onclick="event.stopPropagation(); Folder.refreshFolderFiles('${folder.id}')">
              <i data-lucide="refresh-cw"></i>
            </button>
            <button class="btn-icon-sm folder-remove" title="Xóa thư mục" onclick="event.stopPropagation(); Folder.removeFolder('${folder.id}')">
              <i data-lucide="x"></i>
            </button>
          </div>
        </div>
        <div class="folder-files" id="folder-files-${folder.id}" style="display:none;">
          <div class="folder-files-loading">
            <i data-lucide="loader-2" class="spin"></i> Đang tải...
          </div>
        </div>
      `;

            // Click header to toggle file list
            const header = item.querySelector('.folder-header');
            header.addEventListener('click', () => this.toggleFiles(folder.id, item));

            tree.appendChild(item);
        });

        if (window.lucide) lucide.createIcons();

        // Auto-scan cho các folder để hiện sync badge
        this.folders.forEach(folder => {
            this.updateSyncBadge(folder.id);
        });
    },

    /** Update sync badge cho 1 folder (async, non-blocking) */
    async updateSyncBadge(folderId) {
        try {
            const changes = await API.checkFolderChanges(folderId);
            const badge = document.getElementById(`sync-badge-${folderId}`);
            const syncBtn = document.getElementById(`sync-all-${folderId}`);
            if (!badge) return;

            if (changes.has_changes) {
                const unsynced = changes.new_files + changes.changed_files;
                badge.textContent = `⚠ ${unsynced}`;
                badge.style.display = 'inline-block';
                badge.title = [
                    `${unsynced} chưa đồng bộ`,
                    ...changes.new_file_names.map(n => `🆕 ${n}`),
                    ...changes.changed_file_names.map(n => `📝 ${n}`),
                ].join('\n');

                if (syncBtn && unsynced > 0) {
                    syncBtn.style.display = '';
                    syncBtn.title = `Đồng bộ ${unsynced} file`;
                }
            } else {
                badge.style.display = 'none';
                if (syncBtn) syncBtn.style.display = 'none';
            }
        } catch (err) {
            console.warn('[Folder] Sync badge error:', err);
        }
    },

    /** Kiem tra thay doi trong folder — hien badge neu co */
    async checkChanges(folderId) {
        return this.updateSyncBadge(folderId);
    },

    /** Refresh folder files — quét lại và reload list */
    async refreshFolderFiles(folderId) {
        const filesEl = document.getElementById(`folder-files-${folderId}`);
        if (filesEl) {
            delete filesEl.dataset.loaded;
            // If expanded, reload
            if (filesEl.style.display !== 'none') {
                const item = filesEl.closest('.folder-item');
                if (item) {
                    filesEl.innerHTML = '<div class="folder-files-loading"><i data-lucide="loader-2" class="spin"></i> Đang quét...</div>';
                    if (window.lucide) lucide.createIcons({ nodes: [filesEl] });
                    await this._loadFileList(folderId, filesEl);
                }
            }
        }
        this.updateSyncBadge(folderId);
    },

    /** Toggle file list for a folder */
    async toggleFiles(folderId, itemEl) {
        const filesEl = itemEl.querySelector('.folder-files');
        const chevron = itemEl.querySelector('.folder-chevron');
        if (!filesEl) return;

        const isOpen = filesEl.style.display !== 'none';
        if (isOpen) {
            filesEl.style.display = 'none';
            itemEl.classList.remove('folder-item--expanded');
            if (chevron) chevron.style.transform = '';
            return;
        }

        // Expand
        filesEl.style.display = '';
        itemEl.classList.add('folder-item--expanded');
        if (chevron) chevron.style.transform = 'rotate(90deg)';

        // Load files if not loaded yet
        if (filesEl.dataset.loaded) return;

        await this._loadFileList(folderId, filesEl);
    },

    /** Internal: load file list (scan or indexed) */
    async _loadFileList(folderId, filesEl) {
        try {
            const files = await API.scanFolderFiles(folderId);
            filesEl.dataset.loaded = '1';

            if (!files || files.length === 0) {
                filesEl.innerHTML = '<div class="folder-files-empty">Chưa có file nào</div>';
                return;
            }

            const fileTypeIcons = {
                pdf: 'file-text', image: 'image', excel: 'table', csv: 'table',
                word: 'file-text', html: 'globe', text: 'file-type', json: 'braces', other: 'file',
            };



            const fileListHtml = files.map(f => {
                const icon = fileTypeIcons[f.file_type] || 'file';
                const sizeStr = f.file_size > 1024 * 1024
                    ? `${(f.file_size / 1024 / 1024).toFixed(1)} MB`
                    : f.file_size > 1024
                        ? `${(f.file_size / 1024).toFixed(0)} KB`
                        : `${f.file_size} B`;
                const escapedPath = f.file_path.replace(/'/g, "\\'");
                const isSynced = f.sync_status === 'synced';
                const statusLabel = f.sync_status === 'new' ? '🆕' : f.sync_status === 'changed' ? '📝' : '✅';
                const rowClass = isSynced ? '' : ' folder-file-item--unsynced';

                let actionBtn = '';
                if (isSynced) {
                    // Delete button for synced files
                    actionBtn = `<button class="btn-delete-file" title="Xóa file khỏi index" onclick="event.stopPropagation(); Folder.deleteFile('${escapedPath}', '${folderId}', this)">
                        <i data-lucide="trash-2"></i>
                    </button>`;
                } else {
                    // Sync button for unsynced files
                    actionBtn = `<button class="btn-sync-file" title="Đồng bộ file này" onclick="event.stopPropagation(); Folder.syncSingleFile('${escapedPath}', '${folderId}', this)">
                        <i data-lucide="upload-cloud"></i>
                    </button>`;
                }

                const chunksInfo = isSynced && f.chunk_count > 0 ? `<span class="file-chunks">${f.chunk_count} chunks</span>` : '';

                return `<div class="folder-file-item${rowClass}" title="${f.file_path}" data-path="${f.file_path}" data-status="${f.sync_status}">
                    <span class="file-sync-status" title="${f.sync_status}">${statusLabel}</span>
                    <i data-lucide="${icon}" class="file-icon file-icon--${f.file_type}"></i>
                    <span class="file-name">${f.file_name}</span>
                    <span class="file-meta">${sizeStr}</span>
                    ${chunksInfo}
                    ${actionBtn}
                  </div>`;
            }).join('');

            filesEl.innerHTML = fileListHtml;

            // Add countdown container (hidden by default)
            const countdownEl = document.createElement('div');
            countdownEl.className = 'sync-countdown';
            countdownEl.id = `countdown-${folderId}`;
            countdownEl.style.display = 'none';
            filesEl.prepend(countdownEl);

            if (window.lucide) lucide.createIcons({ nodes: [filesEl] });
        } catch (err) {
            filesEl.innerHTML = `<div class="folder-files-empty">Lỗi tải: ${err}</div>`;
        }
    },

    /** Đồng bộ 1 file */
    async syncSingleFile(filePath, folderId, btnEl) {
        const fileName = filePath.split('/').pop() || filePath;
        try {
            if (btnEl) {
                btnEl.disabled = true;
                btnEl.innerHTML = '<i data-lucide="loader-2" class="spin"></i>';
                if (window.lucide) lucide.createIcons({ nodes: [btnEl] });
            }

            const result = await API.indexSingleFile(filePath, folderId);
            const fileItem = btnEl?.closest('.folder-file-item');

            if (result && result.rate_limited) {
                App.toast(`⚠️ ${fileName}: index xong nhưng embed bị giới hạn quota`, 'warning');
            } else {
                App.toast(`✅ Đã đồng bộ: ${fileName} (${result?.chunk_count || 0} chunks)`, 'success');
            }

            // Update UI — mark as synced
            if (fileItem) {
                fileItem.classList.remove('folder-file-item--unsynced');
                fileItem.dataset.status = 'synced';
                const statusEl = fileItem.querySelector('.file-sync-status');
                if (statusEl) statusEl.textContent = '✅';

                // Replace sync button with delete button
                if (btnEl) {
                    const escapedPath = filePath.replace(/'/g, "\\'");
                    btnEl.outerHTML = `<button class="btn-delete-file" title="Xóa file khỏi index" onclick="event.stopPropagation(); Folder.deleteFile('${escapedPath}', '${folderId}', this)">
                        <i data-lucide="trash-2"></i>
                    </button>`;
                }

                // Add chunks info
                if (result?.chunk_count) {
                    const chunksEl = document.createElement('span');
                    chunksEl.className = 'file-chunks';
                    chunksEl.textContent = `${result.chunk_count} chunks`;
                    const metaEl = fileItem.querySelector('.file-meta');
                    if (metaEl) metaEl.after(chunksEl);
                }
            }

            if (window.lucide) lucide.createIcons();
            this.updateSyncBadge(folderId);
            this.loadFolders();
            this.refreshStatus();

            return result;
        } catch (err) {
            App.toast(`Lỗi đồng bộ ${fileName}: ${err}`, 'error');
            if (btnEl) {
                btnEl.disabled = false;
                btnEl.innerHTML = '<i data-lucide="upload-cloud"></i>';
                if (window.lucide) lucide.createIcons({ nodes: [btnEl] });
            }
            return null;
        }
    },

    /** Đồng bộ tất cả files chưa synced trong folder */
    async syncAllFiles(folderId, btnEl) {
        const filesEl = document.getElementById(`folder-files-${folderId}`);
        const unsyncedItems = filesEl
            ? [...filesEl.querySelectorAll('.folder-file-item--unsynced')]
            : [];

        if (unsyncedItems.length === 0) {
            App.toast('Tất cả files đã đồng bộ!', 'success');
            return;
        }

        if (btnEl) {
            btnEl.disabled = true;
            btnEl.innerHTML = '<i data-lucide="loader-2" class="spin"></i>';
            if (window.lucide) lucide.createIcons({ nodes: [btnEl] });
        }

        // ★ Create progress bar
        const total = unsyncedItems.length;
        let progressEl = document.getElementById(`sync-progress-${folderId}`);
        if (!progressEl) {
            progressEl = document.createElement('div');
            progressEl.id = `sync-progress-${folderId}`;
            progressEl.className = 'sync-progress-bar';
            filesEl?.prepend(progressEl);
        }
        const updateProgress = (current) => {
            const pct = Math.round((current / total) * 100);
            progressEl.innerHTML = `
                <div class="sync-progress-track">
                    <div class="sync-progress-fill" style="width: ${pct}%"></div>
                </div>
                <span class="sync-progress-text">${current}/${total} — ${pct}%</span>
            `;
            progressEl.style.display = 'flex';
        };
        updateProgress(0);

        this._syncingFolderId = folderId;
        this._syncCancelled = false;
        let synced = 0;
        let failed = 0;

        for (let i = 0; i < unsyncedItems.length; i++) {
            if (this._syncCancelled) break;

            const item = unsyncedItems[i];
            const filePath = item.dataset.path;
            const syncBtn = item.querySelector('.btn-sync-file');

            const result = await this.syncSingleFile(filePath, folderId, syncBtn);

            if (result) {
                synced++;
                updateProgress(i + 1);

                // Rate limited → start countdown
                if (result.rate_limited && i < unsyncedItems.length - 1) {
                    const remaining = unsyncedItems.length - i - 1;
                    App.toast(`⏳ Quota hết — chờ 60s rồi tiếp tục ${remaining} file`, 'warning');
                    const shouldContinue = await this.startQuotaCountdown(folderId, remaining);
                    if (!shouldContinue) {
                        App.toast('Đã dừng đồng bộ', 'info');
                        break;
                    }
                }
            } else {
                failed++;
                updateProgress(i + 1);
            }
        }

        this._syncingFolderId = null;

        // ★ Remove progress bar
        if (progressEl) {
            progressEl.style.display = 'none';
            progressEl.remove();
        }

        if (btnEl) {
            btnEl.disabled = false;
            btnEl.innerHTML = '<i data-lucide="upload-cloud"></i>';
            if (window.lucide) lucide.createIcons({ nodes: [btnEl] });
        }

        this.updateSyncBadge(folderId);
        this.loadFolders();

        App.toast(`Đồng bộ xong: ${synced} thành công, ${failed} lỗi`, synced > 0 ? 'success' : 'error');
    },

    /** Countdown 60s khi quota hết — trả về Promise<boolean> (true = tiếp tục, false = dừng) */
    startQuotaCountdown(folderId, remainingFiles) {
        return new Promise((resolve) => {
            const countdownEl = document.getElementById(`countdown-${folderId}`);
            if (!countdownEl) {
                // Fallback: just wait 60s
                setTimeout(() => resolve(true), 60000);
                return;
            }

            let seconds = 60;
            countdownEl.style.display = 'flex';
            countdownEl.innerHTML = `
                <div class="countdown-info">
                    <i data-lucide="clock" class="countdown-icon"></i>
                    <span class="countdown-text">Quota hết — chờ <strong id="cd-seconds-${folderId}">${seconds}</strong>s</span>
                    <span class="countdown-remaining">(còn ${remainingFiles} file)</span>
                </div>
                <button class="btn-countdown-stop" id="cd-stop-${folderId}" title="Dừng đồng bộ">
                    <i data-lucide="square"></i> Dừng
                </button>
            `;
            if (window.lucide) lucide.createIcons({ nodes: [countdownEl] });

            const secEl = document.getElementById(`cd-seconds-${folderId}`);
            const stopBtn = document.getElementById(`cd-stop-${folderId}`);

            let stopped = false;

            stopBtn?.addEventListener('click', () => {
                stopped = true;
                this._syncCancelled = true;
                clearInterval(intervalId);
                countdownEl.style.display = 'none';
                resolve(false);
            });

            const intervalId = setInterval(() => {
                seconds--;
                if (secEl) secEl.textContent = seconds;

                if (seconds <= 0) {
                    clearInterval(intervalId);
                    countdownEl.style.display = 'none';
                    if (!stopped) resolve(true);
                }
            }, 1000);
        });
    },

    /** Xóa 1 file khỏi index */
    async deleteFile(filePath, folderId, btnEl) {
        const fileName = filePath.split('/').pop() || filePath;
        if (!confirm(`Xóa "${fileName}" khỏi index?`)) return;

        try {
            if (btnEl) {
                btnEl.disabled = true;
                btnEl.innerHTML = '<i data-lucide="loader-2" class="spin"></i>';
                if (window.lucide) lucide.createIcons({ nodes: [btnEl] });
            }

            await API.deleteIndexedFile(filePath);

            // Remove from UI
            const fileItem = btnEl?.closest('.folder-file-item');
            if (fileItem) {
                fileItem.style.opacity = '0';
                fileItem.style.transform = 'translateX(-10px)';
                fileItem.style.transition = 'all 0.2s ease';
                setTimeout(() => fileItem.remove(), 200);
            }

            // Refresh folder badge
            this.clearFileCache();
            await this.loadFolders();
            await this.refreshStatus();

            App.toast(`Đã xóa: ${fileName}`, 'success');
        } catch (err) {
            App.toast('Lỗi xóa file: ' + err, 'error');
            if (btnEl) {
                btnEl.disabled = false;
                btnEl.innerHTML = '<i data-lucide="trash-2"></i>';
                if (window.lucide) lucide.createIcons({ nodes: [btnEl] });
            }
        }
    },

    /** Xóa folder */
    async removeFolder(id) {
        // ★ Cancel ongoing sync if this folder is being synced
        if (this._syncingFolderId === id) {
            this._syncCancelled = true;
            App.toast('Đang dừng đồng bộ...', 'info');
            // Wait briefly for sync loop to notice cancellation
            await new Promise(r => setTimeout(r, 500));
        }

        try {
            await API.removeFolder(id);
            this.folders = this.folders.filter(f => f.id !== id);
            this.renderTree();
            App.toast('Đã xóa thư mục và index', 'success');
        } catch (err) {
            App.toast('Lỗi xóa: ' + err, 'error');
        }
    },
    /** Re-index folder đã có — dùng khi mất tracking data */
    async reindexFolder(folderId, btnEl) {
        try {
            if (btnEl) {
                btnEl.disabled = true;
                btnEl.innerHTML = '<i data-lucide="loader-2" class="spin"></i>';
                if (window.lucide) lucide.createIcons({ nodes: [btnEl] });
            }
            App.toast('Đang cập nhật index...', 'info');
            await API.reindexFolder(folderId);
            // Clear cached files so folder re-fetches
            this.clearFileCache();
            this.loadFolders();
            this.refreshStatus();
        } catch (err) {
            App.toast('Lỗi: ' + err, 'error');
        } finally {
            if (btnEl) {
                btnEl.disabled = false;
                btnEl.innerHTML = '<i data-lucide="refresh-cw"></i>';
                if (window.lucide) lucide.createIcons({ nodes: [btnEl] });
            }
        }
    },
};

document.addEventListener('DOMContentLoaded', () => {
    Folder.init();

    // Listen for api-quota-exhausted from backend
    if (window.__TAURI__) {
        window.__TAURI__.event.listen('api-quota-exhausted', (event) => {
            const data = event.payload;
            console.warn('[Folder] API quota exhausted:', data);
            App.toast(`⚠️ API quota hết: ${data?.message || 'tất cả keys bị 429'}`, 'warning');
        });
    }
});
