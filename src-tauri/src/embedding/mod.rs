pub mod gemini_embed;
pub mod pipeline;

use thiserror::Error;

#[derive(Error, Debug)]
pub enum EmbeddingError {
    #[error("Model error: {0}")]
    ModelError(String),
    #[error("API error: {0}")]
    ApiError(String),
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
    #[error("API quota exhausted: tất cả {0} keys đều bị 429 sau {1} retries")]
    QuotaExhausted(usize, u32),
}

/// Embedding vector output
#[derive(Debug, Clone)]
pub struct EmbeddingResult {
    pub text: String,
    pub vector: Vec<f32>,
}
