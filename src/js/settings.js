/**
 * RAG Search — Settings
 * API key, model, theme preferences
 */
const Settings = {
    init() {
        this.initThemeRadios();
        this.initValidateKey();
        this.initSaveSettings();
        this.initSliders();
        this.initCustomModelDropdown();
        this.loadSettings();
    },

    /** Slider live value update */
    initSliders() {
        const slider = document.getElementById('setting-min-score');
        const label = document.getElementById('min-score-value');
        if (slider && label) {
            slider.addEventListener('input', () => {
                label.textContent = slider.value + '%';
            });
        }

        // Creativity slider
        const cSlider = document.getElementById('setting-creativity');
        const cIndicator = document.getElementById('creativity-indicator');
        if (cSlider && cIndicator) {
            const updateIndicator = (val) => {
                const v = parseInt(val);
                if (v <= 3) cIndicator.textContent = '🎯 Chính xác';
                else if (v <= 6) cIndicator.textContent = '⚖️ Cân bằng';
                else cIndicator.textContent = '✨ Sáng tạo';
            };
            cSlider.addEventListener('input', () => updateIndicator(cSlider.value));
            updateIndicator(cSlider.value);
        }
    },

    /** Theme radio selection */
    initThemeRadios() {
        document.querySelectorAll('input[name="theme"]').forEach(radio => {
            radio.addEventListener('change', (e) => {
                const theme = e.target.value;
                if (theme === 'auto') {
                    const prefersDark = window.matchMedia('(prefers-color-scheme: dark)').matches;
                    App.setTheme(prefersDark ? 'dark' : 'light');
                    localStorage.setItem('rag-theme', 'auto');
                } else {
                    App.setTheme(theme);
                }
            });
        });
    },

    /** Validate API key button — nhanh vì dùng models.list */
    initValidateKey() {
        const btn = document.getElementById('btn-validate-key');
        if (btn) {
            btn.addEventListener('click', async () => {
                const textarea = document.getElementById('input-api-keys');
                const lines = (textarea?.value || '').split('\n').map(l => l.trim()).filter(l => l);
                const key = lines[0]; // Kiểm tra key đầu tiên

                if (!key) {
                    App.toast('Vui lòng nhập ít nhất 1 API key', 'warning');
                    return;
                }

                btn.textContent = 'Đang kiểm...';
                btn.disabled = true;

                try {
                    const valid = await API.validateApiKey(key);
                    if (valid) {
                        App.toast(`API key hợp lệ ✓ (${lines.length} key)`, 'success');
                        btn.textContent = '✓ Hợp lệ';

                        // Lưu tất cả keys
                        await API.saveSettings({
                            gemini_api_keys: lines,
                            default_model: document.getElementById('setting-model')?.value || 'gemini-2.0-flash',
                            theme: document.querySelector('input[name="theme"]:checked')?.value || 'dark',
                            max_chunks_per_query: 5,
                            language: 'vi',
                            min_match_score: parseFloat(document.getElementById('setting-min-score')?.value || '60') / 100,
                            creativity_level: parseFloat(document.getElementById('setting-creativity')?.value || '7') / 10,
                        });

                        // Load models từ API
                        await this.loadModelsFromAPI();
                    } else {
                        App.toast('API key không hợp lệ', 'error');
                        btn.textContent = '✗ Không hợp lệ';
                    }
                } catch (err) {
                    App.toast('Lỗi kiểm tra: ' + err, 'error');
                    btn.textContent = 'Kiểm tra';
                }

                btn.disabled = false;
                // Reset text sau 3s
                setTimeout(() => { btn.textContent = 'Kiểm tra'; }, 3000);
            });
        }
    },

    /** Custom model dropdown (thay native select xấu) */
    initCustomModelDropdown() {
        const btn = document.getElementById('model-select-btn');
        const dropdown = document.getElementById('model-select-dropdown');
        if (!btn || !dropdown) return;

        btn.addEventListener('click', (e) => {
            e.stopPropagation();
            const isOpen = dropdown.style.display !== 'none';
            dropdown.style.display = isOpen ? 'none' : 'block';
            if (window.lucide) lucide.createIcons({ nodes: [btn] });
        });

        document.addEventListener('click', () => {
            if (dropdown) dropdown.style.display = 'none';
        });
    },

    /** Set custom dropdown value */
    setModelValue(value, label) {
        const hidden = document.getElementById('setting-model');
        const labelEl = document.getElementById('model-select-label');
        const dropdown = document.getElementById('model-select-dropdown');
        if (hidden) hidden.value = value;
        if (labelEl) labelEl.textContent = label || value;
        if (dropdown) {
            dropdown.querySelectorAll('.custom-model-select__option').forEach(opt => {
                opt.classList.toggle('selected', opt.dataset.value === value);
            });
        }
    },

    /** Load models từ Gemini API và populate cả settings dropdown và chat selector */
    async loadModelsFromAPI() {
        const hintEl = document.querySelector('.setting-hint');
        try {
            // Show all generateContent models — key rotation handles 429 at query time
            const models = await API.listGeminiModels();
            if (!models || models.length === 0) {
                if (hintEl) hintEl.textContent = '⚠️ Không lấy được danh sách models';
                return;
            }

            if (hintEl) hintEl.textContent = `${models.length} models khả dụng ✓`;

            const currentVal = document.getElementById('setting-model')?.value || 'gemini-2.0-flash';
            const dropdownEl = document.getElementById('model-select-dropdown');

            // 1. Custom settings dropdown
            if (dropdownEl) {
                dropdownEl.innerHTML = '';
                models.forEach(m => {
                    const opt = document.createElement('div');
                    opt.className = 'custom-model-select__option' + (m.id === currentVal ? ' selected' : '');
                    opt.dataset.value = m.id;
                    opt.innerHTML = `<span class="opt-name">${m.display_name}</span>`;
                    opt.addEventListener('click', () => {
                        this.setModelValue(m.id, m.display_name);
                        dropdownEl.style.display = 'none';
                    });
                    dropdownEl.appendChild(opt);
                });
                const cur = models.find(m => m.id === currentVal);
                if (cur) this.setModelValue(cur.id, cur.display_name);
                else if (models[0]) this.setModelValue(models[0].id, models[0].display_name);
            }

            // 2. Chat model-select (all models)
            const chatSelect = document.getElementById('model-select');
            if (chatSelect) {
                const savedDefault = document.getElementById('setting-model')?.value;
                const toSelect = savedDefault || chatSelect.value;
                chatSelect.innerHTML = '';
                models.forEach(m => {
                    const opt = document.createElement('option');
                    opt.value = m.id;
                    opt.textContent = m.display_name.replace('Gemini ', '').replace(' (Preview)', '');
                    chatSelect.appendChild(opt);
                });
                chatSelect.value = models.some(m => m.id === toSelect) ? toSelect : models[0]?.id;
            }

            console.log(`[Settings] Loaded ${models.length} models`);
        } catch (err) {
            console.warn('[Settings] Failed to load models:', err);
            if (hintEl) hintEl.textContent = 'Models tự cập nhật khi xác thực API key ✓';
        }
    },

    /** Save settings button */
    initSaveSettings() {
        const btn = document.getElementById('btn-save-settings');
        if (btn) {
            btn.addEventListener('click', async () => {
                const settings = {
                    gemini_api_keys: (() => {
                        const lines = (document.getElementById('input-api-keys')?.value || '').split('\n').map(l => l.trim()).filter(l => l);
                        return lines.length > 0 ? lines : null;
                    })(),
                    default_model: document.getElementById('setting-model')?.value || 'gemini-2.0-flash',
                    theme: document.querySelector('input[name="theme"]:checked')?.value || 'dark',
                    max_chunks_per_query: 5,
                    language: 'vi',
                    min_match_score: parseFloat(document.getElementById('setting-min-score')?.value || '60') / 100,
                    creativity_level: parseFloat(document.getElementById('setting-creativity')?.value || '7') / 10,
                };

                try {
                    await API.saveSettings(settings);
                    App.toast('Đã lưu cài đặt', 'success');
                    App.closeSettingsModal();

                    // Chat model is now always 'auto' — no need to update
                } catch (err) {
                    App.toast('Lỗi lưu: ' + err, 'error');
                }
            });
        }
    },

    /** Load saved settings */
    async loadSettings() {
        try {
            const settings = await API.getSettings();
            if (settings) {
                if (settings.gemini_api_keys && settings.gemini_api_keys.length > 0) {
                    const textarea = document.getElementById('input-api-keys');
                    if (textarea) textarea.value = settings.gemini_api_keys.join('\n');
                    await this.loadModelsFromAPI();
                } else if (settings.gemini_api_key) {
                    const textarea = document.getElementById('input-api-keys');
                    if (textarea) textarea.value = settings.gemini_api_key;
                    await this.loadModelsFromAPI();
                }
                if (settings.default_model) {
                    const chatSelect = document.getElementById('model-select');
                    // ★ Sync default model → chat selector ngay (kể cả trước khi load API)
                    if (chatSelect) chatSelect.value = settings.default_model;
                    // For custom dropdown label (will be properly set after loadModelsFromAPI)
                    const labelEl = document.getElementById('model-select-label');
                    if (labelEl) labelEl.textContent = settings.default_model;
                    const hidden = document.getElementById('setting-model');
                    if (hidden) hidden.value = settings.default_model;
                }
                // Min match score slider
                if (settings.min_match_score !== undefined) {
                    const slider = document.getElementById('setting-min-score');
                    const label = document.getElementById('min-score-value');
                    const val = Math.round(settings.min_match_score * 100);
                    if (slider) slider.value = val;
                    if (label) label.textContent = val + '%';
                }
                // Creativity level slider
                if (settings.creativity_level !== undefined) {
                    const cSlider = document.getElementById('setting-creativity');
                    const cIndicator = document.getElementById('creativity-indicator');
                    const val = Math.round(settings.creativity_level * 10);
                    if (cSlider) cSlider.value = val;
                    if (cIndicator) {
                        if (val <= 3) cIndicator.textContent = '🎯 Chính xác';
                        else if (val <= 6) cIndicator.textContent = '⚖️ Cân bằng';
                        else cIndicator.textContent = '✨ Sáng tạo';
                    }
                }
            }
        } catch (err) {
            console.warn('[Settings] Could not load:', err);
        }
    },
};

document.addEventListener('DOMContentLoaded', () => {
    Settings.init();
});
