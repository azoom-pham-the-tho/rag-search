use serde::{Deserialize, Serialize};

// ═══════════════════════════════════════════════════════
// Shared types — used across all search sub-modules
// ═══════════════════════════════════════════════════════

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SearchResult {
    pub chunk_id: String,
    pub file_path: String,
    pub file_name: String,
    pub content_preview: String,
    pub section: Option<String>,
    pub score: f32, // Normalized 0.0 → 1.0
    pub highlights: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct SearchResponse {
    pub results: Vec<SearchResult>,
    pub total: usize,
    pub query: String,
    pub duration_ms: u64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SmartResponse {
    pub intent: String, // "search" | "chat"
    pub search_results: Option<SearchResponse>,
    pub chat_response: Option<String>,
    pub keywords: String,                  // Keywords AI trích xuất
    pub attached_files: Vec<AttachedFile>, // Files AI đã phân tích
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AttachedFile {
    pub file_name: String,
    pub file_path: String,
    pub score: f32,
}

// ═══════════════════════════════════════════════════════
// Sub-modules
// ═══════════════════════════════════════════════════════
pub mod keyword;
pub mod prompt;
pub mod decompose;
pub mod context;
pub mod smart_query;
pub mod pipeline;

// Re-exports (internal — for sub-modules that import from super::)
// Tauri command paths are defined directly in lib.rs
