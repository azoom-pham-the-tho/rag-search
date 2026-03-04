use super::gemini_embed::GeminiEmbedding;
use super::{EmbeddingError, EmbeddingResult};
use crate::indexer::chunker::Chunk;
use crate::search::vector_index::VectorMeta;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use tokio::sync::Mutex;

/// Embedding pipeline — Gemini API based
/// text → gemini-embedding-001 → 768-dim vector → HNSW index
pub struct EmbeddingPipeline {
    embedding: Arc<Mutex<GeminiEmbedding>>,
}

impl EmbeddingPipeline {
    pub fn new(api_keys: Vec<String>) -> Self {
        Self {
            embedding: Arc::new(Mutex::new(GeminiEmbedding::new(api_keys))),
        }
    }

    /// Cập nhật danh sách API keys
    pub async fn update_api_keys(&self, keys: Vec<String>) {
        let mut guard = self.embedding.lock().await;
        guard.set_api_keys(keys);
        log::info!("[Embedding] API keys updated");
    }

    /// Kiểm tra API key đã sẵn sàng chưa
    pub async fn is_ready(&self) -> bool {
        let guard = self.embedding.lock().await;
        guard.has_key()
    }

    /// Tính embedding cho một text
    pub async fn embed_text(&self, text: &str) -> Result<Vec<f32>, EmbeddingError> {
        let guard = self.embedding.lock().await;
        guard.embed(text).await
    }

    /// Xử lý batch chunks → tính embedding qua Gemini API
    /// cancel_flag: để nút "Dừng" hoạt động trong lúc embedding
    /// on_batch_done: callback(vectors_so_far) gọi sau mỗi batch để emit progress
    pub async fn process_chunks(
        &self,
        chunks: &[Chunk],
        cancel_flag: Option<&AtomicBool>,
        on_batch_done: Option<&(dyn Fn(usize) + Send + Sync)>,
        on_quota_wait: Option<&(dyn Fn(u64) + Send + Sync)>,
    ) -> Result<(Vec<Vec<f32>>, Vec<VectorMeta>), EmbeddingError> {
        let guard = self.embedding.lock().await;

        let mut all_vectors = Vec::new();
        let mut all_metas = Vec::new();

        // Gom texts → gọi embed_batch (tự split theo MAX_BATCH_SIZE bên trong)
        let texts: Vec<String> = chunks.iter().map(|c| c.content.clone()).collect();

        match guard.embed_batch(&texts, cancel_flag, on_batch_done, on_quota_wait).await {
            Ok(vectors) => {
                for (i, vec) in vectors.into_iter().enumerate() {
                    if i < chunks.len() {
                        all_vectors.push(vec);
                        all_metas.push(VectorMeta {
                            chunk_id: chunks[i].id.clone(),
                            file_path: chunks[i].file_path.clone(),
                            file_name: chunks[i].file_name.clone(),
                            content: chunks[i].content.clone(),
                            section: chunks[i].section.clone(),
                        });
                    }
                }
            }
            Err(e) => {
                log::error!("[Embedding] embed_batch failed: {}", e);
                return Err(e);
            }
        }

        log::info!(
            "[Embedding] Processed {}/{} chunks → {} vectors",
            all_vectors.len(),
            chunks.len(),
            all_vectors.len()
        );

        Ok((all_vectors, all_metas))
    }

    /// Embed query text → vector cho search
    pub async fn embed_query(&self, query: &str) -> Result<EmbeddingResult, EmbeddingError> {
        let vector = self.embed_text(query).await?;
        Ok(EmbeddingResult {
            text: query.to_string(),
            vector,
        })
    }

    /// Embedding dimension (768 for gemini-embedding-001)
    pub fn embedding_dim(&self) -> usize {
        768
    }
}
