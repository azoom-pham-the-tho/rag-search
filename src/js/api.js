/**
 * RAG Search — Tauri IPC API Wrapper
 * Giao tiếp giữa frontend và Rust backend
 */
const API = {
    /** Gọi Tauri command */
    async invoke(cmd, args = {}) {
        try {
            if (window.__TAURI__) {
                return await window.__TAURI__.core.invoke(cmd, args);
            }
            // Fallback khi chạy dev ngoài Tauri
            console.warn(`[API] Tauri not available, mock: ${cmd}`, args);
            return null;
        } catch (err) {
            console.error(`[API] Error calling ${cmd}:`, err);
            throw err;
        }
    },

    // === Folder Management ===
    async addFolder(path) {
        return this.invoke('add_folder', { path });
    },

    async removeFolder(folderId) {
        return this.invoke('remove_folder', { folderId });
    },

    async listFolders() {
        return this.invoke('list_folders');
    },

    async getFolderFiles(folderId) {
        return this.invoke('get_folder_files', { folderId });
    },

    async listAllIndexedFiles() {
        return this.invoke('list_all_indexed_files');
    },

    async reindexFolder(folderId) {
        return this.invoke('reindex_folder', { folderId });
    },

    async checkFolderChanges(folderId) {
        return this.invoke('check_folder_changes', { folderId });
    },

    async deleteIndexedFile(filePath) {
        return this.invoke('delete_indexed_file', { filePath });
    },

    async scanFolderFiles(folderId) {
        return this.invoke('scan_folder_files', { folderId });
    },

    async indexSingleFile(filePath, folderId) {
        return this.invoke('index_single_file', { filePath, folderId });
    },

    // === Indexing ===
    async startIndexing() {
        return this.invoke('start_indexing');
    },

    async getIndexStatus() {
        return this.invoke('get_index_status');
    },

    async stopIndexing() {
        return this.invoke('stop_indexing');
    },

    // === Search ===
    async searchDocuments(query) {
        return this.invoke('search_documents', { query });
    },

    async searchDirect(query) {
        return this.invoke('search_direct', { query });
    },

    // === Chat ===
    async sendMessage(request) {
        return this.invoke('send_message', { request });
    },

    async getChatHistory(sessionId) {
        return this.invoke('get_chat_history', { sessionId });
    },

    async clearChat(sessionId) {
        return this.invoke('clear_chat', { sessionId });
    },

    // === Settings ===
    async getSettings() {
        return this.invoke('get_settings');
    },

    async saveSettings(settings) {
        return this.invoke('save_settings', { settings });
    },

    async validateApiKey(apiKey) {
        return this.invoke('validate_api_key', { apiKey });
    },

    async listGeminiModels() {
        return this.invoke('list_gemini_models');
    },

    /** Ch\u1ec9 tr\u1ea3 v\u1ec1 models c\u00f2n quota (probe song song t\u1eebng model) */
    async listAvailableModels() {
        return this.invoke('list_available_models');
    },

    async smartQuery(query, model, sessionId, selectedFiles = null, folderId = null) {
        const args = { query, model, sessionId };
        if (selectedFiles) args.selectedFiles = selectedFiles;
        if (folderId) args.folderId = folderId;
        return this.invoke('chatbot_query', args);
    },

    /** Danh sách sessions gần đây */
    async listChatSessions() {
        return this.invoke('list_chat_sessions');
    },

    /** Xóa 1 session */
    async deleteChatSession(sessionId) {
        return this.invoke('delete_chat_session', { sessionId });
    },

    /** Lấy history của session */
    async getChatHistory(sessionId) {
        return this.invoke('get_chat_history', { sessionId });
    },

    /** Pre-search: tắt cho chatbot mode */
    async preSearch(_query) {
        return { cached: false };
    },

    /** Lấy danh sách chunks của file (chunk visualization) */
    async getFileChunks(filePath) {
        return this.invoke('get_file_chunks', { filePath });
    },

    /** Retry embed cho files chưa có vectors */
    async retryEmbedMissing() {
        return this.invoke('retry_embed_missing');
    }
};
