/**
 * RAG Search — Search UI
 * Direct search results display
 */
const Search = {
    init() {
        this.initWelcomeActions();
    },

    initWelcomeActions() {
        const actionSearch = document.getElementById('action-search');
        if (actionSearch) {
            actionSearch.addEventListener('click', () => {
                App.setMode('search');
                document.getElementById('chat-input')?.focus();
            });
        }

        const actionChat = document.getElementById('action-chat');
        if (actionChat) {
            actionChat.addEventListener('click', () => {
                App.setMode('chat');
                document.getElementById('chat-input')?.focus();
            });
        }
    },

    /**
     * Auto-detect intent từ query
     * Keywords: tìm, kiếm, file → Direct Search
     * Keywords: tóm tắt, phân tích, giải thích → AI Chat
     */
    detectIntent(query) {
        const searchKeywords = ['tìm', 'kiếm', 'file', 'tài liệu nào', 'ở đâu', 'danh sách', 'liệt kê'];
        const chatKeywords = ['tóm tắt', 'phân tích', 'giải thích', 'so sánh', 'tại sao', 'như thế nào', 'là gì'];

        const lower = query.toLowerCase();

        for (const kw of chatKeywords) {
            if (lower.includes(kw)) return 'chat';
        }

        for (const kw of searchKeywords) {
            if (lower.includes(kw)) return 'search';
        }

        // Default: keep current mode
        return App.currentMode;
    },
};

document.addEventListener('DOMContentLoaded', () => {
    Search.init();
});
