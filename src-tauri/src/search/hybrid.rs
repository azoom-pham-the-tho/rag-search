use crate::indexer::tantivy_index::TantivyIndex;

/// BM25-only search — fallback khi chưa có vectors
pub struct HybridSearch;

/// Kết quả search
#[derive(Debug, Clone)]
pub struct HybridResult {
    pub chunk_id: String,
    pub file_path: String,
    pub file_name: String,
    pub content: String,
    pub section: Option<String>,
    pub score: f32,
    pub bm25_rank: Option<usize>,
    pub vector_rank: Option<usize>,
}

impl HybridSearch {
    /// BM25 keyword search (Tantivy) — dùng khi chưa có vectors hoặc embed thất bại
    pub fn search_bm25_only(
        tantivy: &TantivyIndex,
        query: &str,
        top_n: usize,
    ) -> Result<Vec<HybridResult>, String> {
        let results = tantivy
            .search(query, top_n)
            .map_err(|e| format!("BM25 error: {}", e))?;

        Ok(results
            .into_iter()
            .enumerate()
            .map(|(rank, (score, chunk))| HybridResult {
                chunk_id: chunk.id,
                file_path: chunk.file_path,
                file_name: chunk.file_name,
                content: chunk.content,
                section: chunk.section,
                score,
                bm25_rank: Some(rank + 1),
                vector_rank: None,
            })
            .collect())
    }

    /// BM25 keyword search scoped to a folder
    pub fn search_bm25_in_folder(
        tantivy: &TantivyIndex,
        query: &str,
        top_n: usize,
        folder_id: &str,
    ) -> Result<Vec<HybridResult>, String> {
        let results = tantivy
            .search_in_folder(query, top_n, folder_id)
            .map_err(|e| format!("BM25 folder error: {}", e))?;

        Ok(results
            .into_iter()
            .enumerate()
            .map(|(rank, (score, chunk))| HybridResult {
                chunk_id: chunk.id,
                file_path: chunk.file_path,
                file_name: chunk.file_name,
                content: chunk.content,
                section: chunk.section,
                score,
                bm25_rank: Some(rank + 1),
                vector_rank: None,
            })
            .collect())
    }
}
