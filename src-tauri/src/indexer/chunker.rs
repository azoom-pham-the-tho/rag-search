use crate::parser::KreuzbergChunkData;
use serde::{Deserialize, Serialize};

/// Một chunk text đã được tách từ document
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Chunk {
    pub id: String,
    pub file_path: String,
    pub file_name: String,
    pub content: String,
    pub section: Option<String>,
    pub position: usize, // Vị trí trong document
    pub char_start: usize,
    pub char_end: usize,
}

/// Smart chunking config (dùng cho fallback manual chunker)
pub struct ChunkConfig {
    pub max_chunk_size: usize, // Max tokens per chunk (default 512)
    pub overlap_size: usize,   // Overlap tokens between chunks (default 50)
}

impl Default for ChunkConfig {
    fn default() -> Self {
        Self {
            max_chunk_size: 512,
            overlap_size: 50,
        }
    }
}

/// Convert kreuzberg chunks → our Chunk struct
/// Dùng khi kreuzberg đã chunk sẵn (xlsx, pdf, docx, html)
pub fn from_kreuzberg_chunks(
    kr_chunks: &[KreuzbergChunkData],
    file_path: &str,
    file_name: &str,
) -> Vec<Chunk> {
    kr_chunks
        .iter()
        .enumerate()
        .filter(|(_, kc)| !kc.content.trim().is_empty())
        .map(|(i, kc)| Chunk {
            id: uuid::Uuid::new_v4().to_string(),
            file_path: file_path.to_string(),
            file_name: file_name.to_string(),
            content: kc.content.clone(),
            section: None,
            position: i,
            char_start: kc.byte_start,
            char_end: kc.byte_end,
        })
        .collect()
}

/// Fallback: tách document content thành các chunks
/// Dùng cho plain text, images (OCR), và files không có kreuzberg chunks
pub fn chunk_text(
    content: &str,
    file_path: &str,
    file_name: &str,
    config: &ChunkConfig,
) -> Vec<Chunk> {
    let mut chunks = Vec::new();
    let max_chars = config.max_chunk_size * 4; // ~4 chars per token

    // Split by paragraphs (double newline)
    let paragraphs: Vec<&str> = content.split("\n\n").collect();

    let mut current_chunk = String::new();
    let mut current_pos = 0;
    let mut char_start = 0;

    for para in paragraphs {
        let para_trimmed = para.trim();
        if para_trimmed.is_empty() {
            continue;
        }

        // If adding this paragraph exceeds max size, save current chunk
        if !current_chunk.is_empty() && current_chunk.len() + para_trimmed.len() > max_chars {
            chunks.push(Chunk {
                id: uuid::Uuid::new_v4().to_string(),
                file_path: file_path.to_string(),
                file_name: file_name.to_string(),
                content: current_chunk.clone(),
                section: None,
                position: current_pos,
                char_start,
                char_end: char_start + current_chunk.len(),
            });
            current_pos += 1;

            // Overlap: keep last portion (UTF-8 safe)
            let overlap_chars = config.overlap_size * 4;
            if current_chunk.len() > overlap_chars {
                let target_start = current_chunk.len() - overlap_chars;
                let safe_start = current_chunk
                    .char_indices()
                    .map(|(i, _)| i)
                    .find(|&i| i >= target_start)
                    .unwrap_or(current_chunk.len());
                char_start += safe_start;
                current_chunk = current_chunk[safe_start..].to_string();
            }
        }

        if !current_chunk.is_empty() {
            current_chunk.push_str("\n\n");
        }
        current_chunk.push_str(para_trimmed);
    }

    // Push remaining content
    if !current_chunk.is_empty() {
        chunks.push(Chunk {
            id: uuid::Uuid::new_v4().to_string(),
            file_path: file_path.to_string(),
            file_name: file_name.to_string(),
            content: current_chunk.clone(),
            section: None,
            position: current_pos,
            char_start,
            char_end: char_start + current_chunk.len(),
        });
    }

    chunks
}
