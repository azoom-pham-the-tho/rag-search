#![allow(dead_code)]
//! Query decomposition — tách câu hỏi phức tạp thành sub-queries
//!
//! Câu hỏi phức tạp → tách sub-queries → tìm song song → merge results

use crate::AppState;
use crate::search::hybrid::HybridResult;
use super::SearchResponse;
use super::context::do_search_with_raw;
use super::keyword::extract_keywords;

// ═══════════════════════════════════════════════════════
// Query Decomposition + Parallel Search
// Câu hỏi phức tạp → tách sub-queries → tìm song song
// ═══════════════════════════════════════════════════════

/// Heuristic: Phát hiện câu hỏi phức tạp cần tách
/// VD: "So sánh doanh thu Q1 và Q2", "Phân tích invoice A và B"
pub fn is_complex_query(query: &str) -> bool {
    let q_lower = query.to_lowercase();

    // Pattern 1: So sánh / đối chiếu
    let comparison_words = ["so sánh", "đối chiếu", "khác nhau", "giống nhau", "compare"];
    if comparison_words.iter().any(|w| q_lower.contains(w)) {
        return true;
    }

    // Pattern 2: "X và Y" hoặc "X vs Y" (ít nhất 2 entities)
    if (q_lower.contains(" và ") || q_lower.contains(" vs "))
        && q_lower.split_whitespace().count() > 5
    {
        return true;
    }

    // Pattern 3: Multiple questions ("... rồi ...")
    if q_lower.contains(" rồi ") || q_lower.contains(" đồng thời ") {
        return true;
    }

    // Pattern 4: "Liệt kê tất cả X trong Y" — cần search rộng
    if q_lower.contains("tất cả") && q_lower.split_whitespace().count() > 6 {
        return true;
    }

    false
}

/// Decompose query phức tạp → sub-queries → parallel search → merge results
///
/// Flow:
/// 1. AI tách câu hỏi thành 2-4 sub-queries (via generate_json + QueryDecomposition schema)
/// 2. Chạy BM25+vector search song song cho mỗi sub-query (tokio::join!)
/// 3. Merge + deduplicate kết quả
///
/// Fallback: Nếu AI decomposition fail hoặc câu hỏi đơn giản → dùng search thường
pub async fn decompose_and_parallel_search(
    query: &str,
    original_query: &str,
    client: &crate::ai::gemini::GeminiClient,
    state: &tauri::State<'_, AppState>,
) -> Result<Vec<HybridResult>, String> {
    use crate::ai::structured::{query_decomposition_schema, QueryDecomposition};

    // Bước 1: AI decompose query
    let prompt = format!(
        "Phân tích câu hỏi và tách thành sub-queries để tìm kiếm tài liệu:\n\n\
         Câu hỏi: \"{}\"\n\n\
         Nếu câu hỏi ĐƠN GIẢN (chỉ tìm 1 thứ) → needs_decomposition = false, 1 sub_query.\n\
         Nếu câu hỏi PHỨC TẠP (so sánh, nhiều chủ đề, nhiều file) → needs_decomposition = true, 2-4 sub_queries.\n\n\
         Mỗi sub_query phải có search_terms tối ưu cho BM25 search (bỏ stop words).",
        query
    );

    let decomposition = match client
        .generate_json::<QueryDecomposition>(
            &state.model_registry.resolve_utility(),
            &prompt,
            None,
            query_decomposition_schema(),
            0.1,
        )
        .await
    {
        Ok(d) if d.needs_decomposition && d.sub_queries.len() > 1 => {
            log::info!(
                "[Search] Query decomposed into {} sub-queries",
                d.sub_queries.len()
            );
            d
        }
        Ok(d) => {
            log::info!("[Search] Query simple, no decomposition needed");
            // Simple query → fall back to normal search
            let kw = if !d.sub_queries.is_empty() {
                d.sub_queries[0].search_terms.clone()
            } else {
                extract_keywords(query)
            };
            let (_, results) = do_search_with_raw(&kw, original_query, state).await?;
            return Ok(results);
        }
        Err(e) => {
            log::warn!("[Search] Decomposition failed: {}, falling back", e);
            let kw = extract_keywords(query);
            let (_, results) = do_search_with_raw(&kw, original_query, state).await?;
            return Ok(results);
        }
    };

    // Bước 2: Parallel search cho tất cả sub-queries
    let sub_queries = decomposition.sub_queries;
    let start = std::time::Instant::now();

    // Tạo futures cho mỗi sub-query
    let mut search_futures = Vec::new();
    for sq in &sub_queries {
        let terms = sq.search_terms.clone();
        let orig = original_query.to_string();
        log::info!(
            "[Search] Sub-query: '{}' (purpose: {})",
            terms,
            sq.purpose
        );
        search_futures.push(do_search_with_raw_owned(terms, orig, state));
    }

    // tokio::join! — chạy TẤT CẢ searches song song
    let all_results = futures_util::future::join_all(search_futures).await;

    // Bước 3: Merge + deduplicate
    let mut merged: Vec<HybridResult> = Vec::new();
    let mut seen_chunks: std::collections::HashSet<String> = std::collections::HashSet::new();

    for (i, result) in all_results.into_iter().enumerate() {
        match result {
            Ok((_, results)) => {
                log::info!(
                    "[Search] Sub-query {} returned {} results",
                    i + 1,
                    results.len()
                );
                for r in results {
                    if seen_chunks.insert(r.chunk_id.clone()) {
                        merged.push(r);
                    }
                }
            }
            Err(e) => {
                log::warn!("[Search] Sub-query {} failed: {}", i + 1, e);
            }
        }
    }

    // Sort by score (cao nhất trước) + truncate
    merged.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    merged.truncate(20); // Max 20 kết quả merged

    log::info!(
        "[Search] Parallel search: {} sub-queries → {} merged results in {}ms",
        sub_queries.len(),
        merged.len(),
        start.elapsed().as_millis()
    );

    Ok(merged)
}

/// Wrapper cho do_search_with_raw với owned strings (để dùng trong futures)
async fn do_search_with_raw_owned(
    keywords: String,
    original_query: String,
    state: &tauri::State<'_, AppState>,
) -> Result<(Option<SearchResponse>, Vec<HybridResult>), String> {
    do_search_with_raw(&keywords, &original_query, state).await
}
