/**
 * RAG Search — Main App Logic
 * Theme switching, keyboard shortcuts, panel management
 */
const App = {
    currentMode: 'search', // 'search' | 'chat'
    currentTheme: 'dark',

    init() {
        this.initTheme();
        this.initKeyboardShortcuts();
        this.initModeToggle();
        this.initTextareaAutoResize();
        this.initSendButton();
        this.initSettingsModal();
        this.initSourcePanel();
        this.initSidebarResize();

        // Initialize Lucide icons
        if (window.lucide) {
            lucide.createIcons();
        }

        console.log('[App] RAG Search initialized');
    },

    // ═══════════════════════════════════════
    // SIDEBAR RESIZE
    // ═══════════════════════════════════════
    initSidebarResize() {
        const resizer = document.getElementById('sidebar-resizer');
        const sidebar = document.getElementById('sidebar');
        if (!resizer || !sidebar) return;

        // Restore saved width
        const saved = localStorage.getItem('rag-sidebar-width');
        if (saved) {
            const w = parseInt(saved, 10);
            if (w >= 200 && w <= 500) {
                sidebar.style.setProperty('--sidebar-width', w + 'px');
            }
        }

        let isResizing = false;

        resizer.addEventListener('mousedown', (e) => {
            e.preventDefault();
            isResizing = true;
            resizer.classList.add('resizing');
            document.body.style.cursor = 'col-resize';
            document.body.style.userSelect = 'none';
        });

        document.addEventListener('mousemove', (e) => {
            if (!isResizing) return;
            const newWidth = Math.min(500, Math.max(200, e.clientX));
            sidebar.style.setProperty('--sidebar-width', newWidth + 'px');
        });

        document.addEventListener('mouseup', () => {
            if (!isResizing) return;
            isResizing = false;
            resizer.classList.remove('resizing');
            document.body.style.cursor = '';
            document.body.style.userSelect = '';
            // Persist
            const w = sidebar.getBoundingClientRect().width;
            localStorage.setItem('rag-sidebar-width', Math.round(w));
        });
    },

    // ═══════════════════════════════════════
    // THEME
    // ═══════════════════════════════════════
    initTheme() {
        // Check saved preference or OS preference
        const saved = localStorage.getItem('rag-theme');
        if (saved && saved !== 'auto') {
            this.setTheme(saved);
        } else {
            // Auto-detect from OS
            const prefersDark = window.matchMedia('(prefers-color-scheme: dark)').matches;
            this.setTheme(prefersDark ? 'dark' : 'light');
        }

        // Theme toggle button
        const btnToggle = document.getElementById('btn-theme-toggle');
        if (btnToggle) {
            btnToggle.addEventListener('click', () => {
                this.setTheme(this.currentTheme === 'dark' ? 'light' : 'dark');
            });
        }

        // Listen for OS theme changes
        window.matchMedia('(prefers-color-scheme: dark)').addEventListener('change', (e) => {
            const saved = localStorage.getItem('rag-theme');
            if (!saved || saved === 'auto') {
                this.setTheme(e.matches ? 'dark' : 'light');
            }
        });
    },

    setTheme(theme) {
        this.currentTheme = theme;
        document.documentElement.setAttribute('data-theme', theme);
        localStorage.setItem('rag-theme', theme);

        // Toggle icons
        const darkIcon = document.getElementById('theme-icon-dark');
        const lightIcon = document.getElementById('theme-icon-light');
        if (darkIcon && lightIcon) {
            darkIcon.style.display = theme === 'dark' ? 'none' : '';
            lightIcon.style.display = theme === 'light' ? 'none' : '';
        }

        // Re-create icons after DOM change
        if (window.lucide) lucide.createIcons();
    },

    // ═══════════════════════════════════════
    // KEYBOARD SHORTCUTS
    // ═══════════════════════════════════════
    initKeyboardShortcuts() {
        document.addEventListener('keydown', (e) => {
            // Ctrl+K → Focus chat input
            if ((e.ctrlKey || e.metaKey) && e.key === 'k') {
                e.preventDefault();
                const input = document.getElementById('chat-input');
                if (input) input.focus();
            }

            // Ctrl+N → New chat
            if ((e.ctrlKey || e.metaKey) && e.key === 'n') {
                e.preventDefault();
                Chat.newSession();
            }

            // Enter → Send (when input focused, not Shift+Enter)
            if (e.key === 'Enter' && !e.shiftKey) {
                const chatInput = document.getElementById('chat-input');
                if (document.activeElement === chatInput) {
                    // Skip if folder command dropdown is open (let chat.js handle)
                    const folderDropdown = document.getElementById('folder-cmd-dropdown');
                    if (folderDropdown && folderDropdown.style.display !== 'none') return;

                    e.preventDefault();
                    Chat.send();
                }
            }

            // Escape → Close modals/panels/dropdowns
            if (e.key === 'Escape') {
                const folderCmdDropdown = document.getElementById('folder-cmd-dropdown');
                if (folderCmdDropdown && folderCmdDropdown.style.display !== 'none') {
                    folderCmdDropdown.style.display = 'none';
                    return;
                }
                const filePicker = document.getElementById('file-picker-dropdown');
                if (filePicker && filePicker.style.display !== 'none') {
                    filePicker.style.display = 'none';
                    return;
                }
                this.closeSettingsModal();
                this.closeSourcePanel();
            }
        });
    },

    // ═══════════════════════════════════════
    // MODE TOGGLE (Search / Chat)
    // ═══════════════════════════════════════
    initModeToggle() {
        const btnSearch = document.getElementById('btn-mode-search');
        const btnChat = document.getElementById('btn-mode-chat');
        const input = document.getElementById('chat-input');

        if (btnSearch) {
            btnSearch.addEventListener('click', () => {
                this.setMode('search');
            });
        }

        if (btnChat) {
            btnChat.addEventListener('click', () => {
                this.setMode('chat');
            });
        }
    },

    setMode(mode) {
        this.currentMode = mode;
        const btnSearch = document.getElementById('btn-mode-search');
        const btnChat = document.getElementById('btn-mode-chat');
        const input = document.getElementById('chat-input');

        if (btnSearch) btnSearch.classList.toggle('active', mode === 'search');
        if (btnChat) btnChat.classList.toggle('active', mode === 'chat');

        // Update placeholder
        if (input) {
            input.placeholder = mode === 'search'
                ? 'Tìm kiếm file và nội dung...'
                : 'Hỏi AI về tài liệu của bạn...';
        }
    },

    // ═══════════════════════════════════════
    // TEXTAREA AUTO-RESIZE
    // ═══════════════════════════════════════
    initTextareaAutoResize() {
        const textarea = document.getElementById('chat-input');
        if (!textarea) return;

        textarea.addEventListener('input', () => {
            textarea.style.height = 'auto';
            textarea.style.height = Math.min(textarea.scrollHeight, 120) + 'px';
        });
    },

    // ═══════════════════════════════════════
    // SEND BUTTON
    // ═══════════════════════════════════════
    initSendButton() {
        const btn = document.getElementById('btn-send');
        if (btn) {
            btn.addEventListener('click', () => Chat.send());
        }
    },

    // ═══════════════════════════════════════
    // SETTINGS MODAL
    // ═══════════════════════════════════════
    initSettingsModal() {
        const btnOpen = document.getElementById('btn-settings');
        const btnClose = document.getElementById('btn-close-settings');
        const modal = document.getElementById('settings-modal');

        if (btnOpen) {
            btnOpen.addEventListener('click', () => {
                if (modal) modal.style.display = '';
            });
        }

        if (btnClose) {
            btnClose.addEventListener('click', () => this.closeSettingsModal());
        }

        // Click overlay to close
        if (modal) {
            modal.addEventListener('click', (e) => {
                if (e.target === modal) this.closeSettingsModal();
            });
        }
    },

    closeSettingsModal() {
        const modal = document.getElementById('settings-modal');
        if (modal) modal.style.display = 'none';
    },

    // ═══════════════════════════════════════
    // SOURCE PANEL
    // ═══════════════════════════════════════
    initSourcePanel() {
        const btnClose = document.getElementById('btn-close-source');
        if (btnClose) {
            btnClose.addEventListener('click', () => this.closeSourcePanel());
        }
    },

    openSourcePanel(filename, content) {
        const panel = document.getElementById('source-panel');
        const nameEl = document.getElementById('source-filename');
        const contentEl = document.getElementById('source-content');

        if (panel) panel.style.display = '';
        if (nameEl) nameEl.textContent = filename;
        if (contentEl) contentEl.textContent = content;
    },

    closeSourcePanel() {
        const panel = document.getElementById('source-panel');
        if (panel) panel.style.display = 'none';
    },

    // ═══════════════════════════════════════
    // TOAST NOTIFICATIONS
    // ═══════════════════════════════════════
    toast(message, type = 'info') {
        const container = document.getElementById('toast-container');
        if (!container) return;

        const icons = {
            success: 'check-circle',
            error: 'alert-circle',
            warning: 'alert-triangle',
            info: 'info',
        };

        const toast = document.createElement('div');
        toast.className = `toast toast--${type}`;
        toast.innerHTML = `
      <i data-lucide="${icons[type] || 'info'}"></i>
      <span>${message}</span>
    `;

        container.appendChild(toast);
        if (window.lucide) lucide.createIcons({ nodes: [toast] });

        // Auto-remove after 4s
        setTimeout(() => {
            toast.classList.add('toast-exit');
            setTimeout(() => toast.remove(), 300);
        }, 4000);
    },
};

// Initialize when DOM is ready
document.addEventListener('DOMContentLoaded', () => {
    App.init();

    // ── Chatbot UI Wiring ──

    // New session button (sidebar)
    const btnNewSession = document.getElementById('btn-new-session');
    if (btnNewSession) {
        btnNewSession.addEventListener('click', () => Chat.newSession());
    }

    // Clear all history (settings)
    const btnClearAll = document.getElementById('btn-clear-all-history');
    if (btnClearAll) {
        btnClearAll.addEventListener('click', async () => {
            if (!confirm('Xóa toàn bộ lịch sử hội thoại?\n\nHành động này không thể hoàn tác.')) return;
            try {
                const sessions = await API.listChatSessions();
                for (const s of sessions) {
                    await API.deleteChatSession(s.id);
                }
                Chat.newSession();
                Chat._sessions = [];
                Chat.renderSessions();
                App.toast('Đã xóa toàn bộ lịch sử', 'success');
            } catch (e) {
                console.error('[App] Clear all history error:', e);
                App.toast('Lỗi xóa lịch sử', 'error');
            }
        });
    }
});
