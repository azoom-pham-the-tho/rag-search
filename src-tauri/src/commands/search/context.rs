//! Context building — search, ranking, intent detection, context assembly
//!
//! Module xử lý:
//! - Hybrid search (BM25 + Vector parallel)
//! - Multi-signal file re-ranking
//! - Query intent detection (Lookup/Compare/Summarize/Aggregate/General)
//! - Context budget management
//! - Build AI context from search results

use crate::search::hybrid::{HybridResult, HybridSearch};
use crate::AppState;
use tauri::State;
use super::{SearchResult, SearchResponse, AttachedFile};
use super::keyword::{extract_snippet, contains_cjk, normalize_scores, extract_compound_terms};


/// ═══════════════════════════════════════════════════════════
/// Multi-Signal File Re-ranking for AI Context
/// Thuật toán xếp hạng file dựa trên nhiều tín hiệu:
///   1. BM25 Aggregate Score (trung bình score các chunks)
///   2. Keyword Hit Rate (% keywords xuất hiện trong file)  
///   3. Keyword Density (mật độ keywords / 1000 ký tự)
///   4. Filename Match (tên file chứa keywords)
///   5. File Freshness (file mới hơn → ưu tiên hơn)
/// ═══════════════════════════════════════════════════════════

/// Thông tin ranking cho mỗi file
#[derive(Debug)]
pub struct FileRankInfo {
    file_path: String,
    file_name: String,
    bm25_scores: Vec<f32>,       // scores từ các chunks
    chunk_contents: Vec<String>, // nội dung chunks
    // Computed scores
    composite_score: f32,
}

/// Parse AI response → chỉ giữ file thực sự được trích dẫn [N]
pub fn filter_cited_files(ai_response: &str, all_files: &[AttachedFile]) -> Vec<AttachedFile> {
    let mut cited_indices = std::collections::HashSet::new();

    // Tìm tất cả [N] trong response (N = 1, 2, 3...)
    let mut i = 0;
    let chars: Vec<char> = ai_response.chars().collect();
    while i < chars.len() {
        if chars[i] == '[' {
            i += 1;
            let mut num_str = String::new();
            while i < chars.len() && chars[i].is_ascii_digit() {
                num_str.push(chars[i]);
                i += 1;
            }
            if i < chars.len() && chars[i] == ']' && !num_str.is_empty() {
                if let Ok(n) = num_str.parse::<usize>() {
                    if n >= 1 && n <= all_files.len() {
                        cited_indices.insert(n - 1); // 0-indexed
                    }
                }
            }
        }
        i += 1;
    }

    if cited_indices.is_empty() {
        // Fallback: AI không ghi nguồn → giữ tất cả
        log::info!(
            "[Citation] No [N] found in response, keeping all {} files",
            all_files.len()
        );
        return all_files.to_vec();
    }

    let cited: Vec<AttachedFile> = all_files
        .iter()
        .enumerate()
        .filter(|(idx, _)| cited_indices.contains(idx))
        .map(|(_, f)| f.clone())
        .collect();

    log::info!(
        "[Citation] AI cited {} of {} files: {:?}",
        cited.len(),
        all_files.len(),
        cited
            .iter()
            .map(|f| f.file_name.as_str())
            .collect::<Vec<_>>()
    );

    cited
}

/// Normalize filename: bỏ version suffix để nhận diện file trùng lặp
/// "報告_v3.xlsx" → "報告.xlsx", "report_v2.xlsx" → "report.xlsx"
pub fn normalize_filename(name: &str) -> String {
    let lower = name.to_lowercase();
    // Bỏ phần extension
    let (stem, ext) = match lower.rfind('.') {
        Some(pos) => (&lower[..pos], &lower[pos..]),
        None => (lower.as_str(), ""),
    };
    // Bỏ version patterns: _v2, _v3, -v2, (2), copy, _copy2...
    let cleaned = stem
        .trim_end_matches(|c: char| c.is_ascii_digit())
        .trim_end_matches("_v")
        .trim_end_matches("-v")
        .trim_end_matches(" v")
        .trim_end_matches("_copy")
        .trim_end_matches("-copy")
        .trim_end_matches(" copy")
        .trim_end_matches(')')
        .trim_end_matches(|c: char| c.is_ascii_digit())
        .trim_end_matches('(');
    format!("{}{}", cleaned, ext)
}

/// Internal search helper — BM25 + Vector search PARALLEL
pub async fn do_search_with_raw(
    keywords: &str,
    original_query: &str,
    state: &State<'_, AppState>,
) -> Result<(Option<SearchResponse>, Vec<HybridResult>), String> {
    let start = std::time::Instant::now();

    let vector_index = &state.vector_index;
    let has_vectors = vector_index.len() > 0;

    // ★ Run BM25 + embed_text truly in PARALLEL
    // BM25 needs tantivy lock (~10ms), embed needs embedding lock (~200-500ms)
    // They're independent → tokio::join! saves overlap time
    let bm25_fut = async {
        let tantivy_guard = state.tantivy.lock().await;
        match tantivy_guard.as_ref() {
            Some(t) => HybridSearch::search_bm25_only(t, keywords, 10).unwrap_or_default(),
            None => vec![],
        }
    };

    let embed_fut = async {
        if !has_vectors {
            return None;
        }
        let pipeline = &state.embedding_pipeline;
        match tokio::time::timeout(
            std::time::Duration::from_secs(3),
            pipeline.embed_text(original_query),
        )
        .await
        {
            Ok(Ok(vec)) => Some(vec),
            Ok(Err(e)) => {
                log::warn!("[Search] Embed failed: {}, using BM25 only", e);
                None
            }
            Err(_) => {
                log::warn!("[Search] Embed timeout 3s, using BM25 only");
                None
            }
        }
    };

    let (bm25_results, query_vec_opt) = tokio::join!(bm25_fut, embed_fut);

    let search_results = if let Some(query_vec) = query_vec_opt {
        let vec_results = vector_index.search(&query_vec, 15);
        log::info!(
            "[Search] Vector: {} results in {}ms (BM25: {})",
            vec_results.len(),
            start.elapsed().as_millis(),
            bm25_results.len()
        );
        if vec_results.is_empty() {
            bm25_results
        } else {
            // Merge: vector results first (semantic), then unique BM25 results
            let mut merged: Vec<HybridResult> = vec_results
                .into_iter()
                .map(|vr| HybridResult {
                    chunk_id: vr.chunk_id,
                    file_path: vr.file_path,
                    file_name: vr.file_name,
                    content: vr.content,
                    section: vr.section,
                    score: vr.similarity,
                    bm25_rank: None,
                    vector_rank: None,
                })
                .collect();
            let existing_ids: std::collections::HashSet<String> =
                merged.iter().map(|r| r.chunk_id.clone()).collect();
            for br in bm25_results {
                if !existing_ids.contains(&br.chunk_id) {
                    merged.push(br);
                }
            }
            merged.truncate(15);
            merged
        }
    } else {
        bm25_results
    };

    log::info!(
        "[Search] Final: {} results in {}ms",
        search_results.len(),
        start.elapsed().as_millis()
    );

    // Clone raw results cho AI context
    let raw_results = search_results.clone();

    let mut results: Vec<SearchResult> = search_results
        .into_iter()
        .take(20)
        .map(|r| to_search_result(r, original_query))
        .collect();

    normalize_scores(&mut results);

    let score_threshold = results.first().map(|r| r.score * 0.3).unwrap_or(0.0);
    results.retain(|r| r.score >= score_threshold);
    results.truncate(5);
    let total = results.len();

    let duration_ms = start.elapsed().as_millis() as u64;

    Ok((
        Some(SearchResponse {
            results,
            total,
            query: original_query.to_string(),
            duration_ms,
        }),
        raw_results,
    ))
}

/// Build AI context directly from raw BM25 results (NO second search!)
/// Query intent → context budget
#[derive(Debug, Clone, Copy)]
pub enum QueryIntent {
    Lookup,    // "API key là gì?" → nhỏ
    Compare,   // "So sánh A và B" → trung bình
    Summarize, // "Tổng hợp tài liệu X" → lớn
    Aggregate, // "Liệt kê tất cả API" → trung bình+
    General,   // Câu hỏi chung → nhỏ
}

/// (max_per_file, max_total, max_chunks_per_file)
pub fn context_budget(intent: &QueryIntent) -> (usize, usize, usize) {
    // Gemini Flash/Pro ho tro 1M tokens (~4M chars)
    // Tang limit de AI doc duoc toan bo document lon
    match intent {
        QueryIntent::Lookup => (80_000, 200_000, 30),
        QueryIntent::Compare => (100_000, 300_000, 40),
        QueryIntent::Summarize => (200_000, 500_000, 60),
        QueryIntent::Aggregate => (120_000, 300_000, 40),
        QueryIntent::General => (80_000, 200_000, 30),
    }
}

pub fn detect_query_intent(query: &str) -> QueryIntent {
    let q = query.to_lowercase();

    // Scored phrase matching:
    // Strong phrases (multi-word, unambiguous) = 3 points
    // Weak phrases (could be contextual) = 1 point
    // Min 2 points to activate. Highest score wins.

    struct IntentRule {
        intent: QueryIntent,
        strong: &'static [&'static str],
        weak: &'static [&'static str],
    }

    let rules = [
        IntentRule {
            intent: QueryIntent::Summarize,
            strong: &[
                // VN - cum tu dai, ro rang "can toan bo noi dung"
                "tóm tắt",
                "tổng quan",
                "tổng hợp",
                "nội dung chính",
                "toàn bộ nội dung",
                "mô tả chi tiết",
                "phân tích chi tiết",
                "tạo tài liệu",
                "viết tài liệu",
                "tạo báo cáo",
                "viết báo cáo",
                "hướng dẫn sử dụng",
                "giải thích toàn bộ",
                "giải thích về",
                // EN - phrases
                "summarize the",
                "summary of",
                "give me an overview",
                "create a document",
                "write a document",
                "generate a report",
                "explain how",
                "explain the",
                "full analysis",
                "detailed analysis",
                "training document",
                "training material",
                // JP
                "要約して",
                "まとめて",
                "概要を",
            ],
            weak: &[
                "tóm tắt",
                "tổng quan",
                "báo cáo",
                "overview",
                "summarize",
                "summary",
                "training",
            ],
        },
        IntentRule {
            intent: QueryIntent::Aggregate,
            strong: &[
                "liệt kê tất cả",
                "danh sách tất cả",
                "liệt kê các",
                "danh sách các",
                "tổng số lượng",
                "thống kê theo",
                "trích xuất tất cả",
                "list all",
                "how many",
                "count all",
            ],
            weak: &[
                "liệt kê",
                "danh sách",
                "thống kê",
                "trích xuất",
                "tổng số",
                "bao nhiêu",
                "list",
                "count",
                "total",
            ],
        },
        IntentRule {
            intent: QueryIntent::Compare,
            strong: &[
                "so sánh giữa",
                "khác nhau giữa",
                "khác biệt giữa",
                "compare between",
                "difference between",
                "so sánh với",
                "giống và khác",
            ],
            weak: &[
                "so sánh",
                "đối chiếu",
                "khác nhau",
                "khác biệt",
                "giống nhau",
                "compare",
                "versus",
                "vs",
            ],
        },
        IntentRule {
            intent: QueryIntent::Lookup,
            strong: &[
                "là gì",
                "ở đâu",
                "khi nào",
                "giá trị của",
                "what is the",
                "where is the",
                "find the",
                "cho tôi biết",
            ],
            weak: &["là gì", "ở đâu", "what is", "where is", "which", "find"],
        },
    ];

    let mut best_intent = QueryIntent::General;
    let mut best_score = 0i32;

    for rule in &rules {
        let mut score = 0i32;
        for phrase in rule.strong {
            if q.contains(phrase) {
                score += 3;
            }
        }
        for phrase in rule.weak {
            if q.contains(phrase) {
                score += 1;
            }
        }
        if score > best_score {
            best_score = score;
            best_intent = rule.intent;
        }
    }

    // Min 2 points to trigger (prevents single weak match)
    if best_score < 2 {
        best_intent = QueryIntent::General;
    }

    log::info!(
        "[Intent] '{}' -> {:?} (score={})",
        q.chars().take(80).collect::<String>(),
        best_intent,
        best_score
    );

    best_intent
}

pub fn build_context_from_results(
    keywords: &str,
    results: &[HybridResult],
    min_score: f32,
    query: &str,
    tantivy: Option<&crate::indexer::tantivy_index::TantivyIndex>,
) -> (String, Vec<AttachedFile>) {
    if results.is_empty() {
        return ("(Khong tim thay tai lieu lien quan)".to_string(), vec![]);
    }

    // ── Intent-based context budget ──
    let intent = detect_query_intent(query);
    let (max_per_file, max_total, max_chunks_per_file) = context_budget(&intent);
    log::info!(
        "[Context] Intent={:?} -> budget: {}K/file, {}K total, max {} chunks/file",
        intent,
        max_per_file / 1000,
        max_total / 1000,
        max_chunks_per_file
    );

    // ── Step 1: Group chunks by file ──
    let mut file_map: std::collections::HashMap<String, FileRankInfo> =
        std::collections::HashMap::new();

    for r in results {
        let entry = file_map
            .entry(r.file_path.clone())
            .or_insert_with(|| FileRankInfo {
                file_path: r.file_path.clone(),
                file_name: r.file_name.clone(),
                bm25_scores: Vec::new(),
                chunk_contents: Vec::new(),
                composite_score: 0.0,
            });
        entry.bm25_scores.push(r.score);
        entry.chunk_contents.push(r.content.clone());
    }

    // ── Step 1.5: Load ALL chunks trực tiếp từ tantivy ──
    // ★ Local DB → search thoải mái, KHÔNG cần giới hạn ở 15 BM25 results
    // Khi có compound term (VD: "Test 123") → LUÔN load ALL chunks để exact phrase match
    // Khi không compound → expand nếu coverage thấp + intent deep hoặc file nhỏ
    let compound_terms_early = extract_compound_terms(query);
    let has_compound = !compound_terms_early.is_empty();

    if let Some(idx) = tantivy {
        let needs_deep = matches!(intent, QueryIntent::Summarize | QueryIntent::Aggregate);
        for (file_path, file_info) in file_map.iter_mut() {
            let bm25_count = file_info.chunk_contents.len();
            if let Ok(all_chunks) = idx.get_chunks_by_file_path(file_path) {
                let total_count = all_chunks.len();
                if total_count == 0 {
                    continue;
                }
                let coverage = bm25_count as f32 / total_count as f32;

                // ★ Compound term → LUÔN load ALL chunks (local DB, miễn phí)
                // Chunk scoring ở Step 6 sẽ filter exact phrase match
                let should_expand = if has_compound {
                    true // Compound term: load hết để tìm chính xác
                } else {
                    coverage < 0.5 && (needs_deep || total_count <= 20)
                };

                if should_expand && total_count > bm25_count {
                    log::info!(
                        "[Context] Expanding {}: BM25 {}/{} chunks → loading ALL{}",
                        file_info.file_name, bm25_count, total_count,
                        if has_compound { " (compound term)" } else { "" }
                    );
                    file_info.chunk_contents = all_chunks.into_iter().map(|c| c.content).collect();
                } else {
                    log::info!(
                        "[Context] Keep {}: BM25 {}/{} chunks (coverage {:.0}%)",
                        file_info.file_name,
                        bm25_count,
                        total_count,
                        coverage * 100.0
                    );
                }
            }
        }
    }

    // ── Step 2: Compute multi-signal scores ──
    // ★ Compound terms (VD: "Test 123") phải match exact phrase, KHÔNG split thành ["Test", "123"]
    let compound_terms = extract_compound_terms(query);
    let compound_lower: Vec<String> = compound_terms.iter().map(|t| t.to_lowercase()).collect();

    let mut keyword_tokens_owned: Vec<String> = Vec::new();
    // Compound terms đứng đầu (match as exact phrase via .contains())
    for ct in &compound_terms {
        keyword_tokens_owned.push(ct.clone());
    }
    // Individual words KHÔNG thuộc compound nào
    for w in keywords.split_whitespace().filter(|k| k.len() >= 2) {
        let w_lower = w.to_lowercase();
        let in_compound = compound_lower.iter().any(|ct| {
            ct.split_whitespace().any(|cw| cw == w_lower)
        });
        if !in_compound {
            keyword_tokens_owned.push(w.to_string());
        }
    }
    let keyword_tokens: Vec<&str> = keyword_tokens_owned.iter().map(|s| s.as_str()).collect();
    let keyword_count = keyword_tokens.len().max(1) as f32;

    log::info!(
        "[Context] Keyword tokens: {:?} (compound={:?})",
        keyword_tokens, compound_terms
    );

    // Xác định "critical keywords" — CJK terms + từ dài >= 4 ký tự unique
    let critical_keywords: Vec<&str> = keyword_tokens
        .iter()
        .filter(|kw| contains_cjk(kw) || kw.chars().count() >= 4)
        .copied()
        .collect();
    let has_critical = !critical_keywords.is_empty();

    let mut ranked_files: Vec<FileRankInfo> = file_map.into_values().collect();

    for file in ranked_files.iter_mut() {
        let avg_bm25 = file.bm25_scores.iter().sum::<f32>() / file.bm25_scores.len() as f32;
        let chunk_boost = (file.bm25_scores.len() as f32).ln().max(0.0) * 0.1;
        let bm25_signal = (avg_bm25 + chunk_boost).min(1.0);

        // Content giữ nguyên case cho CJK matching + lowercase cho Latin
        let all_content_raw = file.chunk_contents.join(" ");
        let all_content_lower = all_content_raw.to_lowercase();
        let file_name_lower = file.file_name.to_lowercase();
        let combined_text = format!("{} {}", file_name_lower, all_content_lower);
        // CJK match trên raw text (case-sensitive)
        let combined_raw = format!("{} {}", file.file_name, all_content_raw);

        // Signal 2: Keyword Hit Rate — % keywords có trong file
        let hits = keyword_tokens
            .iter()
            .filter(|kw| {
                if contains_cjk(kw) {
                    combined_raw.contains(*kw) // CJK: exact match
                } else {
                    combined_text.contains(&kw.to_lowercase())
                }
            })
            .count() as f32;
        let hit_rate = hits / keyword_count;

        // Signal 3: Keyword Density
        let content_len = all_content_raw.len().max(1) as f32;
        let total_matches: f32 = keyword_tokens
            .iter()
            .map(|kw| {
                if contains_cjk(kw) {
                    all_content_raw.matches(*kw).count() as f32
                } else {
                    all_content_lower.matches(&kw.to_lowercase()).count() as f32
                }
            })
            .sum();
        let density = ((total_matches / content_len) * 1000.0).min(1.0);

        // Signal 4: Filename Match
        let name_hits = keyword_tokens
            .iter()
            .filter(|kw| {
                if contains_cjk(kw) {
                    file.file_name.contains(*kw)
                } else {
                    file_name_lower.contains(&kw.to_lowercase())
                }
            })
            .count() as f32;
        let name_match = (name_hits / keyword_count).min(1.0);

        // Signal 5: Freshness
        let freshness = std::fs::metadata(&file.file_path)
            .and_then(|m| m.modified())
            .map(|mod_time| {
                let age_secs = std::time::SystemTime::now()
                    .duration_since(mod_time)
                    .unwrap_or_default()
                    .as_secs() as f64;
                let days = age_secs / 86400.0;
                ((-0.693 * days / 30.0).exp() as f32).max(0.1)
            })
            .unwrap_or(0.5);

        // ★ Signal 6 (MỚI): Critical Keyword Alignment
        // File PHẢI chứa ít nhất 1 critical keyword (CJK / từ dài unique)
        // Nếu không → đây là file match vào noise ("2000", "tiết kiệm")
        let alignment = if has_critical {
            let critical_hits = critical_keywords
                .iter()
                .filter(|kw| {
                    if contains_cjk(kw) {
                        combined_raw.contains(*kw)
                    } else {
                        combined_text.contains(&kw.to_lowercase())
                    }
                })
                .count() as f32;
            critical_hits / critical_keywords.len() as f32
        } else {
            // Không có critical keyword → dùng hit_rate thay thế
            hit_rate
        };

        // ── Weighted composite score (rebalanced) ──
        // BM25=30%, HitRate=15%, Density=10%, Alignment=25%, Name=10%, Fresh=10%
        file.composite_score = bm25_signal   * 0.30 +
            hit_rate      * 0.15 +
            density       * 0.10 +
            alignment     * 0.25 +  // ★ Critical keyword alignment (mới!)
            name_match    * 0.10 +
            freshness     * 0.10;

        log::info!(
            "[Ranking] {} → composite={:.3} (bm25={:.2} hit={:.0}% density={:.3} align={:.2} name={:.1} fresh={:.2})",
            file.file_name, file.composite_score,
            bm25_signal, hit_rate * 100.0, density, alignment, name_match, freshness
        );
    }

    // ── Step 3: Sort by composite score ──
    ranked_files.sort_by(|a, b| {
        b.composite_score
            .partial_cmp(&a.composite_score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });


    // ── Step 4: Version dedup ──
    let mut seen_basenames = std::collections::HashSet::new();
    ranked_files.retain(|f| {
        let base = normalize_filename(&f.file_name);
        seen_basenames.insert(base)
    });


    // ── Step 5: Dynamic max_files ──
    // min_score từ settings (default 0.60) quyết định threshold
    let score_threshold = min_score.max(0.10); // Tối thiểu 10% để tránh lỗi
    const MIN_RATIO: f32 = 0.70; // File phải ≥ 70% so với top file


    // ★ Khi có compound term: file có hit_rate=100% (exact phrase match) LUÔN giữ lại
    // Compound match chính xác > composite score heuristic
    ranked_files.retain(|f| {
        if f.composite_score >= score_threshold {
            return true;
        }
        // Bypass threshold nếu compound term match 100%
        if has_compound {
            let compound_lower = compound_terms_early[0].to_lowercase();
            let has_exact = f.chunk_contents.iter().any(|c| {
                c.to_lowercase()
                    .split_whitespace()
                    .collect::<Vec<_>>().join(" ")
                    .contains(&compound_lower)
            });
            if has_exact {
                log::info!("[Context] Bypass threshold: '{}' score={:.3} (compound match)", f.file_name, f.composite_score);
                return true;
            }
        }
        false
    });


    if ranked_files.is_empty() {
        log::info!("[Context] No files passed threshold (min_score={:.2})", min_score);
        return ("(Không tìm thấy tài liệu đủ liên quan)".to_string(), vec![]);
    }

    let top_score = ranked_files[0].composite_score;
    // Cho phép tối đa 5 files để frontend hiển thị interactive selection
    let max_files = ranked_files
        .iter()
        .take(10)
        .filter(|f| f.composite_score / top_score >= MIN_RATIO || f.composite_score == top_score)
        .count()
        .max(1);


    // ── Step 6: Build context — smart budget per intent ──
    let mut file_contents = Vec::new();
    let mut attached_files = Vec::new();
    let mut total_chars: usize = 0;

    for (idx, file) in ranked_files.iter().take(max_files).enumerate() {
        if idx > 0 && file.composite_score / top_score < MIN_RATIO {
            continue;
        }
        if total_chars >= max_total {
            break;
        }

        // ── Chunk scoring: AND-majority logic ──
        // Chunk phải chứa >= 50% keywords mới tính là match
        let min_hit_ratio = 0.5f32;
        let total_kw = keyword_tokens.len().max(1) as f32;



        let mut scored_chunks: Vec<(f32, &String)> = file
            .chunk_contents
            .iter()
            .map(|chunk| {
                let chunk_lower = chunk.to_lowercase();
                let chunk_len = chunk.len().max(1) as f32;

                // Đếm bao nhiêu keywords có trong chunk
                let mut hit_count = 0u32;
                let mut score_sum = 0.0f32;

                for kw in &keyword_tokens {
                    // Strip trailing punctuation
                    let clean_kw: &str = kw.trim_end_matches(|c: char| c == ',' || c == '.' || c == ';');
                    if clean_kw.len() < 2 { continue; }

                    let (found, count) = if contains_cjk(clean_kw) {
                        (chunk.contains(clean_kw), chunk.matches(clean_kw).count() as f32)
                    } else {
                        let kl = clean_kw.to_lowercase();
                        // ★ Compound term (chứa space) → normalize whitespace trước khi match
                        // Excel data có tab/newline giữa cells, không phải single space
                        if kl.contains(' ') {
                            let chunk_norm: String = chunk_lower.split_whitespace().collect::<Vec<_>>().join(" ");
                            (
                                chunk_norm.contains(&kl),
                                chunk_norm.matches(&kl).count() as f32,
                            )
                        } else {
                            (
                                chunk_lower.contains(&kl),
                                chunk_lower.matches(&kl).count() as f32,
                            )
                        }
                    };
                    if found {
                        hit_count += 1;
                        let density = (count / chunk_len * 1000.0).min(2.0);
                        let pos_bias = if contains_cjk(clean_kw) {
                            if chunk.find(clean_kw).unwrap_or(usize::MAX) < 200 { 0.3 } else { 0.0 }
                        } else {
                            if chunk_lower.find(&clean_kw.to_lowercase()).unwrap_or(usize::MAX) < 200 { 0.3 } else { 0.0 }
                        };
                        score_sum += 1.0 + density + pos_bias;
                    }
                }

                // ★ AND-majority: chunk phải chứa >= 50% keywords
                let hit_ratio = hit_count as f32 / total_kw;
                if hit_ratio < min_hit_ratio {
                    return (0.0, chunk); // Không đủ keywords → reject
                }

                // Bonus cho chunks có nhiều keywords hơn
                let score = score_sum * hit_ratio;
                (score, chunk)
            })
            .collect();

        scored_chunks.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

        // Lay top N chunks theo budget — CHỈ chunks có keyword match
        let has_matches = scored_chunks.iter().any(|(s, _)| *s > 0.0);
        if !has_matches {
            // ★ Không chunk nào match → LOẠI file này luôn (không bịa đặt)
            log::info!(
                "[AI Context] ✗ {}: 0 chunks match → SKIP",
                file.file_name
            );
            continue;
        }

        let selected_full: Vec<&String> = scored_chunks
            .iter()
            .filter(|(score, _)| *score > 0.0)
            .take(max_chunks_per_file)
            .map(|(_, chunk)| *chunk)
            .collect();

        // ★ Trích xuất context QUANH keyword match
        // Chunk ngắn (< 3000 chars) → gửi toàn bộ (tránh cắt mất dữ liệu tabular)
        // Chunk dài → trích ~2000 chars quanh keyword match
        let context_window = 2000usize;
        let full_chunk_threshold = 3000usize;
        let mut selected_snippets: Vec<String> = Vec::new();

        for chunk in &selected_full {
            let chunk_chars = chunk.chars().count();

            // Chunk ngắn → giữ nguyên toàn bộ
            if chunk_chars <= full_chunk_threshold {
                selected_snippets.push(chunk.trim().to_string());
                continue;
            }

            // Chunk dài → trích snippet quanh keyword match
            let chunk_lower = chunk.to_lowercase();
            let mut best_pos: Option<usize> = None;
            for kw in &keyword_tokens {
                let kl = kw.to_lowercase();
                let pos = if contains_cjk(kw) {
                    chunk.find(*kw)
                } else {
                    chunk_lower.find(&kl)
                };
                if let Some(p) = pos {
                    if best_pos.is_none() || p < best_pos.unwrap() {
                        best_pos = Some(p);
                    }
                }
            }

            let snippet = match best_pos {
                Some(pos) => {
                    let half = context_window / 2;
                    let chars: Vec<char> = chunk.chars().collect();
                    let total = chars.len();

                    let char_pos = chunk[..pos.min(chunk.len())].chars().count();
                    let start = char_pos.saturating_sub(half);
                    let end = (char_pos + half).min(total);

                    let snippet: String = chars[start..end].iter().collect();
                    let prefix = if start > 0 { "..." } else { "" };
                    let suffix = if end < total { "..." } else { "" };
                    format!("{}{}{}", prefix, snippet.trim(), suffix)
                }
                None => {
                    // Fallback: lấy 2000 chars đầu
                    let chars: Vec<char> = chunk.chars().collect();
                    let end = context_window.min(chars.len());
                    let snippet: String = chars[..end].iter().collect();
                    if end < chars.len() {
                        format!("{}...", snippet.trim())
                    } else {
                        snippet.trim().to_string()
                    }
                }
            };
            selected_snippets.push(snippet);
        }

        // Join selected snippets — with clear separators for AI
        let content = if selected_snippets.len() == 1 {
            selected_snippets[0].clone()
        } else {
            selected_snippets
                .iter()
                .enumerate()
                .map(|(i, s)| format!("--- Chunk {} ---\n{}", i + 1, s))
                .collect::<Vec<_>>()
                .join("\n")
        };

        // Truncate theo dòng nếu quá dài (không cắt giữa row)
        let remaining = max_total.saturating_sub(total_chars).min(max_per_file);
        let content = if content.chars().count() > remaining {
            let mut result = String::new();
            for line in content.lines() {
                if result.chars().count() + line.chars().count() + 1 > remaining {
                    break;
                }
                if !result.is_empty() {
                    result.push('\n');
                }
                result.push_str(line);
            }
            result
        } else {
            content
        };

        if content.trim().is_empty() {
            continue;
        }

        total_chars += content.chars().count();

        log::info!(
            "[AI Context] ✓ {}: {}/{} matched chunks, snippets {} chars (total: {})",
            file.file_name,
            selected_full.len(),
            file.chunk_contents.len(),
            content.chars().count(),
            total_chars
        );

        attached_files.push(AttachedFile {
            file_name: file.file_name.clone(),
            file_path: file.file_path.clone(),
            score: file.composite_score,
        });

        file_contents.push(format!("[{}] {}:\n{}", idx + 1, file.file_name, content));
    }

    log::info!(
        "[AI Context] Total: {} files, {} chars, keywords={:?}",
        file_contents.len(),
        total_chars,
        keyword_tokens
    );
    // Debug: log first 200 chars of context to verify exact content sent to AI
    if !file_contents.is_empty() {
        let preview: String = file_contents.join("---").chars().take(200).collect();
        log::info!("[AI Context] Preview: '{}'", preview);
    }

    (file_contents.join("\n---\n"), attached_files)
}

/// Internal search helper (backward compat)
pub async fn do_search(
    keywords: &str,
    original_query: &str,
    state: &State<'_, AppState>,
) -> Result<Option<SearchResponse>, String> {
    let (resp, _) = do_search_with_raw(keywords, original_query, state).await?;
    Ok(resp)
}

pub fn to_search_result(r: HybridResult, query: &str) -> SearchResult {
    let (snippet, highlights) = extract_snippet(&r.content, query, 300);
    SearchResult {
        chunk_id: r.chunk_id,
        file_path: r.file_path,
        file_name: r.file_name,
        content_preview: snippet,
        section: r.section,
        score: r.score,
        highlights,
    }
}
