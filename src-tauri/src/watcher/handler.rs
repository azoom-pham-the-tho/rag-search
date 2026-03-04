#![allow(dead_code)]
use super::FileEvent;
use crate::indexer::chunker::{self, ChunkConfig};
use crate::parser;
use sha2::Digest;
use std::path::PathBuf;

/// Xử lý file events — parse → chunk → index
pub struct EventHandler;

impl EventHandler {
    /// Xử lý một file event
    pub fn handle_event(event: &FileEvent) -> Result<ProcessResult, String> {
        match event {
            FileEvent::Created(path) | FileEvent::Modified(path) => Self::process_file(path),
            FileEvent::Deleted(path) => {
                Ok(ProcessResult::Deleted(path.to_string_lossy().to_string()))
            }
        }
    }

    /// Parse file → tạo chunks
    fn process_file(path: &PathBuf) -> Result<ProcessResult, String> {
        // Check if supported
        if !parser::is_supported(path) {
            return Err(format!("Định dạng không hỗ trợ: {}", path.display()));
        }

        // Parse document
        let doc = parser::parse_file(path).map_err(|e| format!("Lỗi parse: {}", e))?;

        // Chunk text
        let config = ChunkConfig::default();
        let chunks = chunker::chunk_text(&doc.content, &doc.file_path, &doc.file_name, &config);

        // Compute file hash
        let file_bytes = std::fs::read(path).map_err(|e| format!("Lỗi đọc file: {}", e))?;
        let hash = format!(
            "{:x}",
            sha2::Digest::finalize(sha2::Sha256::new_with_prefix(&file_bytes))
        );

        Ok(ProcessResult::Processed {
            file_path: doc.file_path,
            file_hash: hash,
            chunk_count: chunks.len(),
            chunks,
            metadata: doc.metadata,
        })
    }
}

/// Kết quả xử lý file
pub enum ProcessResult {
    Processed {
        file_path: String,
        file_hash: String,
        chunk_count: usize,
        chunks: Vec<chunker::Chunk>,
        metadata: parser::DocumentMetadata,
    },
    Deleted(String),
}
