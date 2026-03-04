//! Smart Query — Legacy RAG pipeline
//!
//! AI-First RAG:
//! Step 1: AI rewrite query → keywords
//! Step 2: BM25 + Vector parallel search
//! Step 3: Search intent → trả file, Chat intent → Gemini phân tích

use crate::search::hybrid::HybridResult;
use crate::AppState;
use tauri::State;
use super::{SearchResult, SearchResponse, SmartResponse};
use super::keyword::{extract_keywords, normalize_scores, is_search_only};
use super::prompt::{detect_prompt_type, build_prompt, build_history_context};
use super::context::{
    do_search_with_raw, build_context_from_results,
    filter_cited_files, to_search_result,
};

#[tauri::command]
pub async fn smart_query(
    query: String,
    model: String,
    session_id: String,
    state: State<'_, AppState>,
) -> Result<SmartResponse, String> {
    use tauri::Emitter;
    let pipeline_start = std::time::Instant::now();

    let emit_step = |step: &str, detail: &str| {
        let _ = state.app_handle.emit(
            "smart-progress",
            serde_json::json!({
                "step": step,
                "detail": detail,
                "done": false
            }),
        );
    };
    let emit_done = |step: &str, duration_ms: u128| {
        let _ = state.app_handle.emit(
            "smart-progress",
            serde_json::json!({
                "step": step,
                "duration_ms": duration_ms,
                "done": true
            }),
        );
    };

    // ── Step 0: Lấy API keys + Lưu user message ──
    let (api_key, all_api_keys, history) = {
        let db = state.db.lock().await;

        // ★ Đọc tất cả API keys để xoay vòng khi 429
        let keys: Vec<String> = crate::db::sqlite::settings::get(&db.conn, "gemini_api_keys")
            .ok()
            .flatten()
            .and_then(|v| serde_json::from_str::<Vec<String>>(&v).ok())
            .unwrap_or_else(|| {
                crate::db::sqlite::settings::get(&db.conn, "gemini_api_key")
                    .ok()
                    .flatten()
                    .filter(|k| !k.is_empty())
                    .map(|k| vec![k])
                    .unwrap_or_default()
            });

        let key = keys.first().cloned();

        // Lưu user message cùng lúc
        let title = query.chars().take(50).collect::<String>();
        let _ = crate::ai::memory::ConversationMemory::create_session(
            &db.conn,
            &session_id,
            Some(&title),
        );
        let user_msg = crate::ai::memory::ChatMessage {
            id: uuid::Uuid::new_v4().to_string(),
            session_id: session_id.clone(),
            role: "user".to_string(),
            content: query.clone(),
            citations: None,
            model: None,
            timestamp: chrono::Utc::now().timestamp(),
        };
        let _ = crate::ai::memory::ConversationMemory::save_message(&db.conn, &user_msg);

        // Load history cùng lúc (tiết kiệm 1 lock nữa)
        let hist = crate::ai::memory::ConversationMemory::get_context_pairs(&db.conn, &session_id)
            .unwrap_or_default();

        (key, keys, hist)
    };
    log::info!(
        "[SmartQuery] API key: {}, history: {} msgs, DB lock: {}ms",
        if api_key.is_some() { "✓" } else { "✗" },
        history.len(),
        pipeline_start.elapsed().as_millis()
    );

    // ── Step 1: AI Router — AI quyết định search hay dùng context cũ ──
    let has_vectors = state.vector_index.len() > 0;

    let query_lower = query.to_lowercase();
    let intent = if is_search_only(&query_lower) {
        "search".to_string()
    } else {
        "chat".to_string()
    };

    let has_history = !history.is_empty();
    let prev_context = {
        let contexts = state.session_contexts.lock().await;
        contexts.get(&session_id).cloned()
    };

    // ★ AI-First: Dùng Gemini function calling để quyết định search hay follow-up
    let has_prev_files = prev_context.as_ref().map_or(false, |ctx| {
        !ctx.attached_files.is_empty() && ctx.timestamp.elapsed().as_secs() < 1800
    });

    let (is_followup, ai_search_query) = if has_history && has_prev_files && intent == "chat" {
        if let Some(ref key) = api_key {
            // Build context summary cho AI router
            let ctx = prev_context.as_ref().unwrap();
            let prev_files: String = ctx
                .attached_files
                .iter()
                .map(|f| f.file_name.clone())
                .collect::<Vec<_>>()
                .join(", ");

            // Tóm tắt conversation history (max 3 turns)
            let history_summary: String = history
                .iter()
                .rev()
                .take(10)
                .rev()
                .map(|(role, text)| {
                    let short = text.chars().take(300).collect::<String>();
                    format!("{}: {}", role, short)
                })
                .collect::<Vec<_>>()
                .join("\n");

            let tool = crate::ai::gemini::ToolDeclaration {
                function_declarations: vec![crate::ai::gemini::FunctionDecl {
                    name: "search_documents".to_string(),
                    description: "Tìm kiếm tài liệu mới trong kho dữ liệu. CHỈ gọi khi cần tìm tài liệu KHÁC hoặc chủ đề MỚI. KHÔNG gọi nếu câu hỏi liên quan đến tài liệu đã có.".to_string(),
                    parameters: serde_json::json!({
                        "type": "object",
                        "properties": {
                            "query": {
                                "type": "string",
                                "description": "Từ khóa tìm kiếm tài liệu mới"
                            }
                        },
                        "required": ["query"]
                    }),
                }],
            };

            let router_system = format!(
                "Bạn là AI router. Nhiệm vụ: quyết định câu hỏi user CẦN TÌM tài liệu mới hay DÙNG LẠI tài liệu đã có.\n\
                \n\
                Tài liệu hiện có trong context: [{}]\n\
                \n\
                QUY TẮC:\n\
                - Nếu user hỏi về NỘI DUNG/DỮ LIỆU của tài liệu đã có → KHÔNG gọi tool, trả lời \"REUSE\"\n\
                - Nếu user muốn xem thêm/chi tiết/so sánh dữ liệu ĐÃ CÓ → KHÔNG gọi tool, trả lời \"REUSE\"\n\
                - Nếu user hỏi chủ đề HOÀN TOÀN MỚI không liên quan → GỌI tool search_documents\n\
                - Nếu user yêu cầu tìm FILE KHÁC/tài liệu khác → GỌI tool search_documents\n\
                \n\
                Chỉ trả lời \"REUSE\" hoặc gọi search_documents. Không giải thích.",
                prev_files
            );

            let mut contents = vec![];
            // Thêm history summary
            if !history_summary.is_empty() {
                contents.push(crate::ai::gemini::Content {
                    role: Some("user".to_string()),
                    parts: vec![crate::ai::gemini::Part::text(&format!(
                        "[Lịch sử hội thoại]\n{}",
                        history_summary
                    ))],
                });
                contents.push(crate::ai::gemini::Content {
                    role: Some("model".to_string()),
                    parts: vec![crate::ai::gemini::Part::text("Đã ghi nhận lịch sử.")],
                });
            }
            contents.push(crate::ai::gemini::Content {
                role: Some("user".to_string()),
                parts: vec![crate::ai::gemini::Part::text(&query)],
            });

            let client = crate::ai::gemini::GeminiClient::new(key.clone());

            match tokio::time::timeout(
                std::time::Duration::from_millis(2000),
                client.generate_with_tools(
                    &state.model_registry.resolve_utility(),
                    contents,
                    &router_system,
                    vec![tool],
                ),
            )
            .await
            {
                Ok(Ok(crate::ai::gemini::ToolCallResult::FunctionCall(fc))) => {
                    // AI muốn search mới
                    let search_q = fc
                        .args
                        .get("query")
                        .and_then(|v| v.as_str())
                        .unwrap_or(&query)
                        .to_string();
                    log::info!(
                        "[SmartQuery] 🔍 AI Router: SEARCH '{}' (original: '{}')",
                        search_q,
                        query
                    );
                    (false, Some(search_q))
                }
                Ok(Ok(crate::ai::gemini::ToolCallResult::Text(_))) => {
                    // AI nói REUSE — follow-up
                    log::info!(
                        "[SmartQuery] ♻️ AI Router: REUSE context ({} files)",
                        prev_context.as_ref().map_or(0, |c| c.attached_files.len())
                    );
                    (true, None)
                }
                Ok(Err(e)) => {
                    log::warn!("[SmartQuery] AI Router error: {}, fallback to search", e);
                    (false, None) // Fallback: search mới
                }
                Err(_) => {
                    log::warn!("[SmartQuery] AI Router timeout (>2s), fallback to search");
                    (false, None) // Fallback: search mới
                }
            }
        } else {
            (false, None) // Không có API key → luôn search
        }
    } else {
        (false, None) // Không có history hoặc search-only → luôn search
    };

    if is_followup {
        log::info!(
            "[SmartQuery] ★ Follow-up! Reusing {} previous files for session '{}'",
            prev_context.as_ref().map_or(0, |c| c.attached_files.len()),
            &session_id[..session_id.len().min(8)]
        );
    }

    // Thử dùng cache từ pre_search (đã chạy ngầm khi user đang gõ)
    let cached = {
        let cache = state.pre_search_cache.lock().await;
        if let Some(ref c) = *cache {
            if c.query == query && c.timestamp.elapsed().as_secs() < 5 {
                Some((c.keywords.clone(), c.results.clone()))
            } else {
                None
            }
        } else {
            None
        }
    };

    let (keywords, bm25_results, search_resp) = if let Some((cached_keywords, cached_results)) =
        cached
    {
        // ⚡ CACHE HIT — Step 1+2 = 0ms
        emit_step("analyze", "⚡ Cache hit — bỏ qua phân tích");
        emit_done("analyze", 0);
        emit_step("search", "⚡ Cache hit — dùng kết quả sẵn có");
        emit_done("search", 0);

        log::info!(
            "[SmartQuery] ⚡ Pre-search cache HIT: {} results, skip Step 1+2",
            cached_results.len()
        );

        // Follow-up: filter cached results về chỉ file trước đó
        let filtered = if is_followup {
            let prev_files = prev_context.as_ref().unwrap();
            let prev_paths: std::collections::HashSet<&str> = prev_files
                .attached_files
                .iter()
                .map(|f| f.file_path.as_str())
                .collect();
            let filtered: Vec<HybridResult> = cached_results
                .iter()
                .filter(|r| prev_paths.contains(r.file_path.as_str()))
                .cloned()
                .collect();
            if filtered.is_empty() {
                cached_results
            } else {
                filtered
            }
        } else {
            cached_results
        };

        let mut results: Vec<SearchResult> = filtered
            .iter()
            .take(20)
            .map(|r| to_search_result(r.clone(), &query))
            .collect();
        normalize_scores(&mut results);
        let score_threshold = results.first().map(|r| r.score * 0.3).unwrap_or(0.0);
        results.retain(|r| r.score >= score_threshold);
        results.truncate(5);
        let total = results.len();

        let resp = SearchResponse {
            results,
            total,
            query: query.clone(),
            duration_ms: 0,
        };
        (cached_keywords, filtered, Some(resp))
    } else {
        // CACHE MISS — chạy Step 1+2 bình thường
        // Follow-up: dùng keywords cũ (từ session context) để tìm lại đúng file
        // New search: dùng AI Router query hoặc extract từ query gốc
        let (_effective_query, mut keywords) = if is_followup {
            let prev_kw = prev_context
                .as_ref()
                .map(|c| c.keywords.clone())
                .unwrap_or_default();
            if !prev_kw.is_empty() {
                log::info!(
                    "[SmartQuery] ♻️ Follow-up: reusing previous keywords: '{}'",
                    prev_kw
                );
                (query.clone(), prev_kw)
            } else {
                (query.clone(), extract_keywords(&query))
            }
        } else if let Some(ref aq) = ai_search_query {
            log::info!("[SmartQuery] 🔍 Using AI Router query: '{}' → keywords", aq);
            let kw = extract_keywords(aq);
            (aq.clone(), kw)
        } else {
            (query.clone(), extract_keywords(&query))
        };

        if is_followup {
            // Follow-up: đã có keywords từ session trước, skip analyze
            emit_step("analyze", "♻️ Dùng lại context trước...");
            emit_done("analyze", 0);
        } else if has_vectors {
            emit_step("analyze", "Phân tích cục bộ... (Vector mode ⚡)");
            let t_analyze = std::time::Instant::now();
            log::info!("[SmartQuery] Vector mode: skip Flash-Lite, using local keywords + HNSW");
            emit_done("analyze", t_analyze.elapsed().as_millis());
        } else if intent == "chat" && api_key.is_some() {
            emit_step("analyze", "Phân tích yêu cầu... (Flash-Lite)");
            let t_analyze = std::time::Instant::now();
            let key = api_key.as_ref().unwrap().clone();
            let client = crate::ai::gemini::GeminiClient::new(key);
            match tokio::time::timeout(
                std::time::Duration::from_millis(1500),
                client.analyze_query(&query, &state.model_registry.resolve_utility()),
            )
            .await
            {
                Ok(Ok(analysis)) => {
                    log::info!(
                        "[SmartQuery] AI keywords='{}' ({}ms)",
                        analysis.keywords,
                        t_analyze.elapsed().as_millis()
                    );
                    keywords = analysis.keywords;
                }
                Ok(Err(e)) => log::warn!("[SmartQuery] AI analyze failed: {}", e),
                Err(_) => log::warn!("[SmartQuery] AI analyze timeout (1.5s)"),
            }
            emit_done("analyze", t_analyze.elapsed().as_millis());
        } else {
            emit_step("analyze", "Phân tích cục bộ...");
            let t_analyze = std::time::Instant::now();
            emit_done("analyze", t_analyze.elapsed().as_millis());
        }

        // Step 2: Tìm kiếm
        let search_mode = if is_followup {
            "Follow-up"
        } else if has_vectors {
            "Hybrid"
        } else {
            "BM25"
        };
        emit_step("search", &format!("Tìm kiếm tài liệu... ({})", search_mode));
        let t_search = std::time::Instant::now();
        let (resp, results) = do_search_with_raw(&keywords, &query, &state).await?;

        // ★ Follow-up: ưu tiên file từ câu trước
        let results = if is_followup && !results.is_empty() {
            let prev_files = prev_context.as_ref().unwrap();
            let prev_paths: std::collections::HashSet<&str> = prev_files
                .attached_files
                .iter()
                .map(|f| f.file_path.as_str())
                .collect();

            // Tách: results từ previous files vs results mới
            let (from_prev, from_new): (Vec<HybridResult>, Vec<HybridResult>) = results
                .into_iter()
                .partition(|r| prev_paths.contains(r.file_path.as_str()));

            if from_prev.is_empty() {
                // Không có match trong file cũ → fallback global
                log::info!("[SmartQuery] Follow-up: no results in prev files, using global");
                from_new
            } else {
                log::info!(
                    "[SmartQuery] ★ Follow-up: {} results from prev files (skipped {} global)",
                    from_prev.len(),
                    from_new.len()
                );
                // Chỉ dùng kết quả từ previous files
                from_prev
            }
        } else {
            results
        };

        // Rebuild SearchResponse nếu follow-up đã filter
        let resp = if is_followup {
            let mut sr: Vec<SearchResult> = results
                .iter()
                .take(20)
                .map(|r| to_search_result(r.clone(), &query))
                .collect();
            normalize_scores(&mut sr);
            let threshold = sr.first().map(|r| r.score * 0.3).unwrap_or(0.0);
            sr.retain(|r| r.score >= threshold);
            sr.truncate(5);
            let total = sr.len();
            Some(SearchResponse {
                results: sr,
                total,
                query: query.clone(),
                duration_ms: t_search.elapsed().as_millis() as u64,
            })
        } else {
            resp
        };

        emit_done("search", t_search.elapsed().as_millis());

        (keywords, results, resp)
    };

    // ── Step 3: Phân tích và quyết định ──
    let result = if intent == "chat" {
        if let Some(ref _key) = api_key {
            // History đã load sẵn từ DB
            let has_history = !history.is_empty();
            let mut history = history;
            // ★ Không giới hạn history — Gemini hỗ trợ 1M tokens
            // Chỉ giữ 20 turns gần nhất để tránh quá dài
            if history.len() > 20 {
                history = history[history.len() - 20..].to_vec();
            }
            history.push(("user".to_string(), query.clone()));

            // Step 3.1: Đánh giá & Rút trích ngữ cảnh
            emit_step("rank", "Chọn lọc ngữ cảnh... (Reranking)");
            let t_rank = std::time::Instant::now();
            let tantivy_guard2 = state.tantivy.lock().await;
            let tantivy_ref2 = (*tantivy_guard2).as_ref();
            let (context, att) =
                build_context_from_results(&keywords, &bm25_results, 0.25, &query, tantivy_ref2);
            drop(tantivy_guard2);
            emit_done("rank", t_rank.elapsed().as_millis());

            let (system_prompt, doc_block, attached) = {
                let source_list: Vec<String> = att
                    .iter()
                    .enumerate()
                    .map(|(i, f)| format!("[{}] {}", i + 1, f.file_name))
                    .collect();

                let history_note = if has_history {
                    // Tóm tắt ngắn context từ câu hỏi trước
                    let prev_context = build_history_context(&history);
                    format!("\nĐÂY LÀ HỘI THOẠI TIẾP NỐI. Ngữ cảnh trước:\n{}\nNếu câu hỏi hiện tại ngắn hoặc mơ hồ → hiểu dựa trên ngữ cảnh trên.\nNếu câu hỏi đề cập cùng đối tượng/cột/file → giữ nguyên context trước đó.\n", prev_context)
                } else {
                    String::new()
                };

                let prompt_type = detect_prompt_type(&query);
                log::info!("[SmartQuery] prompt_type = {:?}", prompt_type);

                let (prompt, doc_block) = build_prompt(
                    &prompt_type,
                    &history_note,
                    &source_list.join(", "),
                    &context,
                );

                (prompt, doc_block, att)
            };

            // ⚡ Emit danh sách file tìm thấy NGAY LẬP TỨC lên UI
            // → Người dùng thấy kết quả trong ~200ms, trước cả khi AI bắt đầu
            let _ = state.app_handle.emit(
                "smart-sources-found",
                serde_json::json!({
                    "files": attached.iter().map(|f| serde_json::json!({
                        "file_name": f.file_name,
                        "file_path": f.file_path,
                        "score": f.score,
                    })).collect::<Vec<_>>(),
                    "total": attached.len(),
                    "keywords": keywords,
                }),
            );

            // ★ Lưu session context — cho follow-up queries dùng
            if !attached.is_empty() {
                let session_files: Vec<crate::SessionFile> = attached
                    .iter()
                    .map(|f| crate::SessionFile {
                        file_path: f.file_path.clone(),
                        file_name: f.file_name.clone(),
                    })
                    .collect();
                let mut contexts = state.session_contexts.lock().await;
                contexts.insert(
                    session_id.clone(),
                    crate::SessionContext {
                        session_id: session_id.clone(),
                        attached_files: session_files,
                        keywords: keywords.clone(),
                        context_text: String::new(),
                        timestamp: std::time::Instant::now(),
                    },
                );
                log::info!(
                    "[SmartQuery] ★ Saved session context: {} files for session '{}'",
                    attached.len(),
                    &session_id[..session_id.len().min(8)]
                );
            }

            // Emit stream start VỚI source_map (mapping [N] → file trong prompt)
            let source_map: serde_json::Value = attached
                .iter()
                .enumerate()
                .map(|(i, f)| {
                    (
                        format!("{}", i + 1),
                        serde_json::json!({
                            "file_name": f.file_name,
                            "file_path": f.file_path,
                        }),
                    )
                })
                .collect::<serde_json::Map<String, serde_json::Value>>()
                .into();

            let _ = state.app_handle.emit(
                "smart-stream-start",
                serde_json::json!({
                    "source_map": source_map,
                }),
            );

            // Step 4: Gửi cho AI
            emit_step("ai", &format!("Tổng hợp đáp án... ({})", model));
            let t_ai = std::time::Instant::now();

            log::info!(
                "[SmartQuery] {} files in context, prompt {} chars (pipeline so far: {}ms)",
                attached.len(),
                system_prompt.len() + doc_block.len(),
                pipeline_start.elapsed().as_millis()
            );

            let app_handle = state.app_handle.clone();

            // \u2605 Key rotation: th\u1eed t\u1eebng key n\u1ebfu b\u1ecb 429
            let mut last_err = String::new();
            let mut stream_result = None;
            // Context guard: trim history
            let mut trimmed_history = crate::commands::context_guard::trim_history(
                &history,
                crate::commands::context_guard::MAX_HISTORY_TURNS,
                crate::commands::context_guard::MAX_HISTORY_TOKENS,
            );
            // ★ Documents in user message for Gemini implicit caching
            let user_msg = if doc_block.is_empty() {
                query.clone()
            } else {
                format!("{}\n\n{}", doc_block, query)
            };
            trimmed_history.push(("user".to_string(), user_msg));
            crate::commands::context_guard::log_context_stats(
                &system_prompt,
                &trimmed_history,
                &context,
            );

            for (key_idx, try_key) in all_api_keys.iter().enumerate() {
                let client = crate::ai::gemini::GeminiClient::new(try_key.clone());
                let app_handle2 = app_handle.clone();

                let result = client
                    .stream_generate_content(&model, &trimmed_history, Some(&system_prompt), 0.7, |chunk| {
                        let _ =
                            app_handle2.emit("smart-stream", serde_json::json!({ "chunk": chunk }));
                    })
                    .await;

                match result {
                    Ok(text) => {
                        if key_idx > 0 {
                            log::info!(
                                "[SmartQuery] Key rotation: used key #{} after {} failed",
                                key_idx + 1,
                                key_idx
                            );
                        }
                        stream_result = Some(Ok(text));
                        break;
                    }
                    Err(e) => {
                        let err_str = format!("{}", e);
                        if err_str.contains("Rate")
                            || err_str.contains("429")
                            || err_str.contains("quota")
                        {
                            log::warn!(
                                "[SmartQuery] Key #{} rate limited ({}), trying next...",
                                key_idx + 1,
                                err_str
                            );
                            last_err = err_str;
                            continue; // Try next key
                        } else {
                            // Non-rate-limit error — don't rotate
                            stream_result = Some(Err(e));
                            break;
                        }
                    }
                }
            }

            let stream_result = stream_result.unwrap_or_else(|| {
                Err(crate::ai::gemini::GeminiError::ApiError(format!(
                    "Tat ca {} API keys deu bi rate limit: {}",
                    all_api_keys.len(),
                    last_err
                )))
            });

            match stream_result {
                Ok(text) => {
                    // Chỉ giữ file thực sự được AI trích dẫn [N]
                    let cited = filter_cited_files(&text, &attached);

                    // Emit stream-end VỚI attached_files đã filter
                    let _ = state.app_handle.emit(
                        "smart-stream-end",
                        serde_json::json!({
                            "attached_files": cited.iter().map(|f| serde_json::json!({
                                "file_name": f.file_name,
                                "file_path": f.file_path,
                                "score": f.score,
                            })).collect::<Vec<_>>(),
                        }),
                    );

                    emit_done("ai", t_ai.elapsed().as_millis());

                    Ok(SmartResponse {
                        intent: "chat".to_string(),
                        search_results: search_resp,
                        chat_response: Some(text),
                        keywords,
                        attached_files: cited,
                    })
                }
                Err(e) => {
                    log::error!("[SmartQuery] Gemini error: {}", e);
                    let _ = state
                        .app_handle
                        .emit("smart-stream-end", serde_json::json!({}));
                    emit_done("ai", t_ai.elapsed().as_millis());

                    // User-friendly error message
                    let user_msg = if format!("{}", e).contains("Rate limited") {
                        "⚠️ API Gemini đang bị giới hạn tốc độ (rate limit). Vui lòng chờ 1-2 phút rồi thử lại.\n\n💡 Nếu lỗi này xảy ra thường xuyên, hãy nâng cấp API key lên gói trả phí trong Google AI Studio.".to_string()
                    } else {
                        format!("⚠️ Lỗi AI: {}. Hiện kết quả tìm kiếm thay thế.", e)
                    };

                    Ok(SmartResponse {
                        intent: "chat".to_string(),
                        search_results: search_resp,
                        chat_response: Some(user_msg),
                        keywords,
                        attached_files: attached.clone(),
                    })
                }
            }
        } else {
            Ok(SmartResponse {
                intent: "chat".to_string(),
                search_results: search_resp,
                chat_response: Some("💡 Cấu hình Gemini API key trong ⚙️ Cài đặt để AI phân tích nội dung tài liệu.".to_string()),
                keywords,
                attached_files: vec![],
            })
        }
    } else {
        // ★ Search-only: cũng save context nếu có kết quả
        if let Some(ref sr) = search_resp {
            if !sr.results.is_empty() {
                let session_files: Vec<crate::SessionFile> = sr
                    .results
                    .iter()
                    .map(|r| crate::SessionFile {
                        file_path: r.file_path.clone(),
                        file_name: r.file_name.clone(),
                    })
                    .collect();
                // Deduplicate by file_path
                let mut seen = std::collections::HashSet::new();
                let session_files: Vec<crate::SessionFile> = session_files
                    .into_iter()
                    .filter(|f| seen.insert(f.file_path.clone()))
                    .collect();
                let mut contexts = state.session_contexts.lock().await;
                contexts.insert(
                    session_id.clone(),
                    crate::SessionContext {
                        session_id: session_id.clone(),
                        attached_files: session_files,
                        keywords: keywords.clone(),
                        context_text: String::new(),
                        timestamp: std::time::Instant::now(),
                    },
                );
            }
        }
        Ok(SmartResponse {
            intent: "search".to_string(),
            search_results: search_resp,
            chat_response: None,
            keywords,
            attached_files: vec![],
        })
    };

    // ── Lưu assistant response vào lịch sử ──
    if let Ok(ref resp) = result {
        let content = resp.chat_response.clone().unwrap_or_else(|| {
            format!(
                "🔍 Tìm thấy {} kết quả cho \"{}\"",
                resp.search_results.as_ref().map(|r| r.total).unwrap_or(0),
                resp.keywords
            )
        });
        let db = state.db.lock().await;
        let assistant_msg = crate::ai::memory::ChatMessage {
            id: uuid::Uuid::new_v4().to_string(),
            session_id: session_id.clone(),
            role: "assistant".to_string(),
            content,
            citations: None,
            model: Some(model.clone()),
            timestamp: chrono::Utc::now().timestamp(),
        };
        let _ = crate::ai::memory::ConversationMemory::save_message(&db.conn, &assistant_msg);
    }

    log::info!(
        "[SmartQuery] ✅ Pipeline complete: {}ms total",
        pipeline_start.elapsed().as_millis()
    );
    result
}
