//! Pipeline — Tauri command entry points for search and chatbot
//!
//! Contains:
//! - chatbot_query: AI-first chatbot pipeline
//! - pre_search: debounced pre-search
//! - search_documents: backward compat search
//! - search_direct: direct search mode

use crate::search::hybrid::{HybridResult, HybridSearch};
use crate::AppState;
use tauri::State;
use super::{SearchResponse, SmartResponse, AttachedFile};
use super::keyword::{extract_keywords, STOP_WORDS};
use super::prompt::{detect_prompt_type, build_prompt, build_history_context};
use super::context::{
    do_search, build_context_from_results,
    filter_cited_files,
};

/// ═══════════════════════════════════════════════════════
/// Chatbot Query — AI-First: Gemini là brain, RAG là tool
/// Flow: User → Gemini (có tool) → Gemini gọi RAG nếu cần → Stream response
/// ═══════════════════════════════════════════════════════

#[tauri::command]
pub async fn chatbot_query(
    query: String,
    model: String,
    session_id: String,
    selected_files: Option<Vec<String>>,
    folder_id: Option<String>,
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

    // ── Step 0: Load state ──
    let (api_keys, history, min_match_score, creativity_level) = {
        let db = state.db.lock().await;

        // ★ Đọc tất cả API keys (gemini_api_keys array) — dùng xoay vòng
        let keys: Vec<String> = crate::db::sqlite::settings::get(&db.conn, "gemini_api_keys")
            .ok()
            .flatten()
            .and_then(|v| serde_json::from_str::<Vec<String>>(&v).ok())
            .unwrap_or_else(|| {
                // Fallback: single key
                crate::db::sqlite::settings::get(&db.conn, "gemini_api_key")
                    .ok()
                    .flatten()
                    .filter(|k| !k.is_empty())
                    .map(|k| vec![k])
                    .unwrap_or_default()
            });

        let min_score: f32 = crate::db::sqlite::settings::get(&db.conn, "min_match_score")
            .ok()
            .flatten()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0.60);

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

        let hist = crate::ai::memory::ConversationMemory::get_context_pairs(&db.conn, &session_id)
            .unwrap_or_default();

        let creativity: f32 = crate::db::sqlite::settings::get(&db.conn, "creativity_level")
            .ok()
            .flatten()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0.7);

        (keys, hist, min_score, creativity)
    };

    if api_keys.is_empty() {
        return Ok(SmartResponse {
            intent: "chat".to_string(),
            search_results: None,
            chat_response: Some(
                "💡 Cấu hình Gemini API key trong ⚙️ Cài đặt để sử dụng chatbot.".to_string(),
            ),
            keywords: String::new(),
            attached_files: vec![],
        });
    }

    // ★ api_keys dùng cho rotation logic
    let all_api_keys = api_keys;

    log::info!(
        "[Chatbot] Session '{}', history: {} msgs, query: '{}'",
        &session_id[..session_id.len().min(8)],
        history.len(),
        query.chars().take(50).collect::<String>()
    );


    // ════════════════════════════════════════════════════════
    // Phase 2: User đã chọn files → skip rewrite, re-search filtered → stream
    // ════════════════════════════════════════════════════════
    if let Some(ref sel_files) = selected_files {
        log::info!("[Chatbot] Phase 2: user selected {} files", sel_files.len());
        emit_step(
            "search",
            &format!("📂 Đang tải {} tài liệu đã chọn...", sel_files.len()),
        );
        let t_s = std::time::Instant::now();

        let mut ctx = String::new();
        let mut att: Vec<AttachedFile> = Vec::new();

        // ★ Load chunks directly from tantivy by file_path (not keyword search)
        {
            let tantivy_guard = state.tantivy.lock().await;
            if let Some(ref tantivy) = *tantivy_guard {
                for file_path in sel_files {
                    match tantivy.get_chunks_by_file_path(file_path) {
                        Ok(chunks) if !chunks.is_empty() => {
                            let file_name = chunks[0].file_name.clone();
                            let idx = att.len() + 1;
                            if !ctx.is_empty() {
                                ctx.push_str("\n---\n");
                            }
                            // Combine all chunks for this file
                            let combined: String = chunks
                                .iter()
                                .map(|c| c.content.as_str())
                                .collect::<Vec<_>>()
                                .join("\n");
                            let truncated = if combined.chars().count() > 200_000 {
                                combined.chars().take(200_000).collect::<String>()
                            } else {
                                combined
                            };
                            ctx.push_str(&format!("[{}] {}:\n{}", idx, file_name, truncated));
                            att.push(AttachedFile {
                                file_name,
                                file_path: file_path.clone(),
                                score: 1.0,
                            });
                            log::info!(
                                "[Chatbot] ✓ Loaded {} chunks from index: {}",
                                chunks.len(),
                                file_path
                            );
                        }
                        _ => {
                            // File not in index → parse on-the-fly
                            log::info!("[Chatbot] File not indexed, will parse: {}", file_path);
                            let path = std::path::Path::new(file_path);
                            if !path.exists() {
                                log::warn!("[Chatbot] File not found: {}", file_path);
                                continue;
                            }
                            emit_step(
                                "search",
                                &format!(
                                    "📄 Đang đọc {}...",
                                    path.file_name().unwrap_or_default().to_string_lossy()
                                ),
                            );
                            let path_owned = path.to_path_buf();
                            match tokio::task::spawn_blocking(move || {
                                crate::parser::parse_file(&path_owned)
                            })
                            .await
                            {
                                Ok(Ok(doc)) => {
                                    let truncated = if doc.content.chars().count() > 200_000 {
                                        doc.content.chars().take(200_000).collect::<String>()
                                    } else {
                                        doc.content.clone()
                                    };
                                    let idx = att.len() + 1;
                                    if !ctx.is_empty() {
                                        ctx.push_str("\n---\n");
                                    }
                                    ctx.push_str(&format!(
                                        "[{}] {}:\n{}",
                                        idx, doc.file_name, truncated
                                    ));
                                    att.push(AttachedFile {
                                        file_name: doc.file_name,
                                        file_path: file_path.clone(),
                                        score: 1.0,
                                    });
                                    log::info!("[Chatbot] ✓ Parsed on-the-fly: {}", file_path);
                                }
                                Ok(Err(e)) => {
                                    log::warn!("[Chatbot] Parse failed: {} — {}", file_path, e);
                                }
                                Err(e) => {
                                    log::warn!("[Chatbot] Spawn error: {} — {}", file_path, e);
                                }
                            }
                        }
                    }
                }
            }
        }

        emit_done("search", t_s.elapsed().as_millis());

        // Lấy stored keywords từ session (nếu có) hoặc extract từ query
        let stored_kw = {
            let contexts = state.session_contexts.lock().await;
            contexts
                .get(&session_id)
                .map(|c| c.keywords.clone())
                .unwrap_or_else(|| extract_keywords(&query))
        };

        let _ = state.app_handle.emit("smart-sources-found", serde_json::json!({
            "files": att.iter().map(|f| serde_json::json!({"file_name": f.file_name, "file_path": f.file_path, "score": f.score})).collect::<Vec<_>>(),
            "total": att.len(), "keywords": &stored_kw,
        }));

        // Lưu context vào session
        if !att.is_empty() {
            let sf: Vec<crate::SessionFile> = att
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
                    attached_files: sf,
                    keywords: stored_kw.clone(),
                    context_text: ctx.clone(),
                    timestamp: std::time::Instant::now(),
                },
            );
        }

        // Jump to Step 3: Build prompt + Stream
        let context = ctx;
        let attached = att;
        let keywords = stored_kw;

        // ── Reuse Step 3 logic (copy from below) ──
        let _ctx_chars = context.chars().count();
        let ctx_files = attached.len();
        emit_step("rank", &format!("📄 Đã chọn {} tài liệu", ctx_files));
        emit_done("rank", 0);

        let has_context = !context.is_empty();
        let source_list: String = attached
            .iter()
            .enumerate()
            .map(|(i, f)| format!("[{}] {}", i + 1, f.file_name))
            .collect::<Vec<_>>()
            .join(", ");
        let history_note = if !history.is_empty() {
            let prev = build_history_context(
                &history
                    .iter()
                    .cloned()
                    .chain(std::iter::once(("user".to_string(), query.clone())))
                    .collect::<Vec<_>>(),
            );
            format!("\nĐÂY LÀ HỘI THOẠI TIẾP NỐI. Ngữ cảnh trước:\n{}\n", prev)
        } else {
            String::new()
        };

        let (system_prompt, doc_block) = if has_context {
            let prompt_type = detect_prompt_type(&query);
            build_prompt(&prompt_type, &history_note, &source_list, &context)
        } else {
            (format!("Bạn là trợ lý AI chuyên phân tích tài liệu. Trả lời bằng tiếng Việt, dùng Markdown.\n{}\n📌 Trả lời thẳng, chuyên nghiệp.", history_note), String::new())
        };

        let mut stream_history = crate::commands::context_guard::trim_history(
            &history,
            crate::commands::context_guard::MAX_HISTORY_TURNS,
            crate::commands::context_guard::MAX_HISTORY_TOKENS,
        );
        // ★ Documents vào user message (dynamic), system prompt giữ stable cho caching
        let user_msg = if doc_block.is_empty() {
            query.clone()
        } else {
            format!("{}\n\n{}", doc_block, query)
        };
        stream_history.push(("user".to_string(), user_msg));

        // Log token usage cho monitoring
        crate::commands::context_guard::log_context_stats(
            &system_prompt,
            &stream_history,
            &context,
        );

        let source_map: serde_json::Value = attached
            .iter()
            .enumerate()
            .map(|(i, f)| {
                (
                    format!("{}", i + 1),
                    serde_json::json!({"file_name": f.file_name, "file_path": f.file_path}),
                )
            })
            .collect::<serde_json::Map<String, serde_json::Value>>()
            .into();
        let _ = state.app_handle.emit(
            "smart-stream-start",
            serde_json::json!({"source_map": source_map}),
        );

        let effective_model = if model == "auto" || model.is_empty() {
            state.model_registry.resolve_chat(None)
        } else {
            model.clone()
        };

        log::info!(
            "[Chatbot] ══ PIPELINE ══ query='{}' keywords='{}' context={}chars files={} model={}",
            &query.chars().take(50).collect::<String>(),
            keywords,
            context.chars().count(),
            attached.len(),
            effective_model
        );

        let model_label = if effective_model.contains("lite") {
            "⚡ Flash Lite"
        } else if effective_model.contains("flash") {
            "🚀 Flash"
        } else {
            &effective_model
        };
        emit_step("ai", &format!("✍️ Đang viết câu trả lời ({})", model_label));
        let t_ai = std::time::Instant::now();
        let app_handle = state.app_handle.clone();

        // Key rotation: try each key on 429
        let mut last_err_str = String::new();
        let mut stream_res: Option<Result<String, _>> = None;
        for (kidx, try_key) in all_api_keys.iter().enumerate() {
            let rc = crate::ai::gemini::GeminiClient::new(try_key.clone());
            let ah2 = app_handle.clone();
            let r = rc
                .stream_generate_content(
                    &effective_model,
                    &stream_history,
                    Some(&system_prompt),
                    creativity_level,
                    |chunk| {
                        let _ = ah2.emit("smart-stream", serde_json::json!({"chunk": chunk}));
                    },
                )
                .await;
            match r {
                Ok(t) => {
                    if kidx > 0 {
                        log::info!("[Chatbot] Key rotation: used key #{}", kidx + 1);
                    }
                    stream_res = Some(Ok(t));
                    break;
                }
                Err(e) => {
                    let es = format!("{}", e);
                    if es.contains("Rate") || es.contains("429") || es.contains("quota") {
                        log::warn!("[Chatbot] Key #{} rate limited, trying next...", kidx + 1);
                        last_err_str = es;
                        continue;
                    } else {
                        stream_res = Some(Err(e));
                        break;
                    }
                }
            }
        }
        let result = match stream_res.unwrap_or_else(|| {
            Err(crate::ai::gemini::GeminiError::ApiError(format!(
                "All {} keys rate limited: {}",
                all_api_keys.len(),
                last_err_str
            )))
        }) {
            Ok(text) => {
                let cited = if has_context {
                    filter_cited_files(&text, &attached)
                } else {
                    vec![]
                };
                let _ = state.app_handle.emit("smart-stream-end", serde_json::json!({
                    "attached_files": cited.iter().map(|f| serde_json::json!({"file_name": f.file_name, "file_path": f.file_path, "score": f.score})).collect::<Vec<_>>(),
                    "total_ms": pipeline_start.elapsed().as_millis() as u64,
                }));
                emit_done("ai", t_ai.elapsed().as_millis());
                let total_ms = pipeline_start.elapsed().as_millis();
                let time_label = if total_ms < 1000 {
                    format!("{}ms", total_ms)
                } else {
                    format!("{:.1}s", total_ms as f64 / 1000.0)
                };
                emit_step("done", &format!("✅ Hoàn thành trong {}", time_label));
                emit_done("done", total_ms);
                Ok(SmartResponse {
                    intent: "chat".to_string(),
                    search_results: None,
                    chat_response: Some(text),
                    keywords,
                    attached_files: cited,
                })
            }
            Err(e) => {
                log::error!(
                    "[Chatbot] ❌ Stream FAILED: {} | model={} | query='{}'",
                    e,
                    effective_model,
                    &query.chars().take(50).collect::<String>()
                );
                emit_done("ai", t_ai.elapsed().as_millis());
                let msg = if format!("{}", e).contains("Rate limited")
                    || format!("{}", e).contains("429")
                {
                    format!(
                        "⚠️ **API rate limit** — Model `{}` đã hết quota tạm thời (free tier: 15 req/phút).\n\n\
                        **Cách xử lý:**\n\
                        • Chờ 1-2 phút rồi thử lại\n\
                        • Hoặc chọn model khác trong dropdown\n\
                        • Hoặc nâng cấp API key lên Pay-as-you-go",
                        effective_model
                    )
                } else {
                    format!("⚠️ Lỗi AI: {}", e)
                };
                // Emit error as stream chunk để UI hiển thị trong chat bubble
                let _ = state
                    .app_handle
                    .emit("smart-stream", serde_json::json!({"chunk": &msg}));
                let _ = state.app_handle.emit(
                    "smart-stream-end",
                    serde_json::json!({
                        "attached_files": [],
                        "total_ms": pipeline_start.elapsed().as_millis() as u64,
                    }),
                );

                Ok(SmartResponse {
                    intent: "chat".to_string(),
                    search_results: None,
                    chat_response: Some(msg),
                    keywords,
                    attached_files: vec![],
                })
            }
        };

        // Save assistant response + source metadata
        if let Ok(ref resp) = result {
            let raw_content = resp.chat_response.clone().unwrap_or_default();
            let file_names: Vec<&str> = resp.attached_files.iter().map(|f| f.file_name.as_str()).collect();
            let content = if file_names.is_empty() {
                raw_content
            } else {
                format!("{}\n[Sources: {}]", raw_content, file_names.join(", "))
            };
            // ★ T2: Fire-and-forget save — không block return
            let db_clone = state.db.clone();
            let session_clone = session_id.clone();
            let model_clone = model.clone();
            tokio::spawn(async move {
                let db = db_clone.lock().await;
                let msg = crate::ai::memory::ChatMessage {
                    id: uuid::Uuid::new_v4().to_string(),
                    session_id: session_clone,
                    role: "assistant".to_string(),
                    content,
                    citations: None,
                    model: Some(model_clone),
                    timestamp: chrono::Utc::now().timestamp(),
                };
                let _ = crate::ai::memory::ConversationMemory::save_message(&db.conn, &msg);
            });
        }
        log::info!(
            "[Chatbot] ✅ Phase 2 complete: {}ms",
            pipeline_start.elapsed().as_millis()
        );
        return result;
    }

    // ════════════════════════════════════════════════════════
    // Phase 1: Pure RAG — extract keywords → embed → search → context
    // 3-Layer Follow-up:
    //   L1: Keyword Enrichment (weak keywords + prev session → merge)
    //   L2: Search-then-Fallback (search first → empty? → reuse prev context)
    //   L3: History Context (conversation history luôn gửi kèm)
    // ════════════════════════════════════════════════════════
    emit_step("analyze", "🧠 Đang phân tích câu hỏi...");

    // Load previous session context (nếu có)
    let prev_context = {
        let contexts = state.session_contexts.lock().await;
        contexts.get(&session_id).cloned()
    };

    // ════════════════════════════════════════════════════════
    // Layer 0: Fast Follow-up Detection (< 1ms, local only)
    // Nếu session có context cũ + query giống follow-up → skip search
    // ════════════════════════════════════════════════════════
    let is_fast_followup = if let Some(ref prev) = prev_context {
        let age_secs = prev.timestamp.elapsed().as_secs();
        let fresh = age_secs < 600; // Context < 10 phút
        let has_files = !prev.attached_files.is_empty();
        let query_lower = query.to_lowercase();
        let query_len = query.chars().count();

        // Signals: query ngắn, follow-up indicators, pronouns, keyword overlap
        let short_query = query_len < 30;
        let followup_indicators = [
            "thêm", "tiếp", "nữa", "chi tiết", "tại sao", "giải thích",
            "so sánh", "ý tôi là", "ý là", "cụ thể", "chính xác",
            "đúng không", "phải không", "sai", "đúng", "không đúng",
            "vậy thì", "còn gì", "thế còn", "ngoài ra", "bổ sung",
            "ví dụ", "liệt kê", "tóm lại", "kết luận", "có nghĩa",
        ];
        let pronoun_refs = [
            "nó", "đó", "này", "kia", "file đó", "file này",
            "tài liệu đó", "tài liệu này", "bảng đó", "bảng này",
            "cái đó", "cái này", "mục đó", "mục này",
        ];
        let new_topic_signals = [
            "tìm file khác", "tài liệu khác", "tìm thêm file",
            "file mới", "chủ đề khác", "hãy tìm", "search",
        ];

        // Check: KHÔNG phải follow-up nếu có new topic signal
        let is_new_topic = new_topic_signals.iter().any(|s| query_lower.contains(s));

        if is_new_topic || !fresh || !has_files {
            false
        } else {
            let has_indicator = followup_indicators.iter().any(|s| query_lower.contains(s));
            let has_pronoun = pronoun_refs.iter().any(|s| query_lower.contains(s));

            // Keyword overlap: check if query keywords overlap with session keywords
            let prev_kw_set: std::collections::HashSet<&str> =
                prev.keywords.split_whitespace().collect();
            let query_words: Vec<&str> = query_lower.split_whitespace().collect();
            let overlap = if !prev_kw_set.is_empty() && !query_words.is_empty() {
                let matched = query_words.iter().filter(|w| prev_kw_set.contains(*w)).count();
                matched as f32 / query_words.len() as f32
            } else {
                0.0
            };
            let high_overlap = overlap > 0.4;

            // ★ FIX: short_query ALONE không đủ trigger follow-up
            // Phải kết hợp với indicator/pronoun/overlap để tránh reuse context sai topic
            // VD: "cho tôi tài liệu liên quan đến nghiệp vụ" = 29 chars nhưng hoàn toàn khác topic
            let has_followup_signal = has_indicator || has_pronoun || high_overlap;
            has_followup_signal || (short_query && has_followup_signal)
        }
    } else {
        false
    };

    // ★ Fast path: skip search, reuse context trực tiếp
    if is_fast_followup {
        let prev = prev_context.as_ref().unwrap();
        log::info!(
            "[Chatbot] ⚡ L0 Fast Follow-up! Skipping search, reusing {} files, {} chars context (age={}s)",
            prev.attached_files.len(),
            prev.context_text.chars().count(),
            prev.timestamp.elapsed().as_secs()
        );

        emit_step("analyze", "⚡ Follow-up — dùng lại ngữ cảnh trước");
        emit_done("analyze", 0);
        emit_step("search", "⚡ Bỏ qua tìm kiếm — câu hỏi liên quan");
        emit_done("search", 0);

        let prev_att: Vec<AttachedFile> = prev
            .attached_files
            .iter()
            .map(|f| AttachedFile {
                file_name: f.file_name.clone(),
                file_path: f.file_path.clone(),
                score: 1.0,
            })
            .collect();

        let _ = state.app_handle.emit(
            "smart-sources-found",
            serde_json::json!({
                "files": prev_att.iter().map(|f| serde_json::json!({
                    "file_name": f.file_name,
                    "file_path": f.file_path,
                    "score": f.score,
                })).collect::<Vec<_>>(),
                "total": prev_att.len(),
                "keywords": &prev.keywords,
            }),
        );

        // ★ Jump straight to Step 3 (prompt + stream)
        // Use the section below (Line ~643+) with these values:
        let context = prev.context_text.clone();
        let attached = prev_att;
        let keywords = prev.keywords.clone();

        // ── Build prompt + stream ──
        let ctx_chars = context.chars().count();
        let ctx_files = attached.len();
        emit_step("rank", &format!("📄 {} tài liệu từ câu hỏi trước", ctx_files));
        emit_done("rank", 0);

        let has_context = !context.is_empty();
        let source_list: String = attached
            .iter()
            .enumerate()
            .map(|(i, f)| format!("[{}] {}", i + 1, f.file_name))
            .collect::<Vec<_>>()
            .join(", ");

        let history_note = if !history.is_empty() {
            let prev_hist = build_history_context(
                &history
                    .iter()
                    .cloned()
                    .chain(std::iter::once(("user".to_string(), query.clone())))
                    .collect::<Vec<_>>(),
            );
            format!("\nĐÂY LÀ HỘI THOẠI TIẾP NỐI. Ngữ cảnh trước:\n{}\n", prev_hist)
        } else {
            String::new()
        };

        let (system_prompt, doc_block) = if has_context {
            let prompt_type = detect_prompt_type(&query);
            build_prompt(&prompt_type, &history_note, &source_list, &context)
        } else {
            (format!(
                "Bạn là trợ lý AI chuyên phân tích tài liệu. Trả lời bằng tiếng Việt, dùng Markdown.\n{}\n📌 Trả lời thẳng, chuyên nghiệp.",
                history_note
            ), String::new())
        };

        let mut stream_history = crate::commands::context_guard::trim_history(
            &history,
            crate::commands::context_guard::MAX_HISTORY_TURNS,
            crate::commands::context_guard::MAX_HISTORY_TOKENS,
        );
        let user_msg = if doc_block.is_empty() {
            query.clone()
        } else {
            format!("{}\n\n{}", doc_block, query)
        };
        stream_history.push(("user".to_string(), user_msg));
        crate::commands::context_guard::log_context_stats(&system_prompt, &stream_history, &context);

        let source_map: serde_json::Value = attached
            .iter()
            .enumerate()
            .map(|(i, f)| {
                (
                    format!("{}", i + 1),
                    serde_json::json!({"file_name": f.file_name, "file_path": f.file_path}),
                )
            })
            .collect::<serde_json::Map<String, serde_json::Value>>()
            .into();
        let _ = state.app_handle.emit(
            "smart-stream-start",
            serde_json::json!({"source_map": source_map}),
        );

        let effective_model = if model == "auto" || model.is_empty() {
            state.model_registry.resolve_chat(None)
        } else {
            model.clone()
        };

        log::info!(
            "[Chatbot] ⚡ FAST FOLLOWUP query='{}' context={}chars files={} model={}",
            &query.chars().take(50).collect::<String>(),
            ctx_chars, ctx_files, effective_model
        );

        let model_label = if effective_model.contains("lite") {
            "⚡ Flash Lite"
        } else if effective_model.contains("flash") {
            "🚀 Flash"
        } else {
            &effective_model
        };
        emit_step("ai", &format!("✍️ Đang viết câu trả lời ({})", model_label));
        let t_ai = std::time::Instant::now();
        let app_handle = state.app_handle.clone();

        // Key rotation: try each key on 429
        let mut last_err_str = String::new();
        let mut stream_res: Option<Result<String, _>> = None;
        for (kidx, try_key) in all_api_keys.iter().enumerate() {
            let rc = crate::ai::gemini::GeminiClient::new(try_key.clone());
            let ah2 = app_handle.clone();
            let r = rc
                .stream_generate_content(
                    &effective_model,
                    &stream_history,
                    Some(&system_prompt),
                    creativity_level,
                    |chunk| {
                        let _ = ah2.emit("smart-stream", serde_json::json!({"chunk": chunk}));
                    },
                )
                .await;
            match r {
                Ok(t) => {
                    if kidx > 0 {
                        log::info!("[Chatbot] Key rotation: used key #{}", kidx + 1);
                    }
                    stream_res = Some(Ok(t));
                    break;
                }
                Err(e) => {
                    let es = format!("{}", e);
                    if es.contains("Rate") || es.contains("429") || es.contains("quota") {
                        log::warn!("[Chatbot] Key #{} rate limited, trying next...", kidx + 1);
                        last_err_str = es;
                        continue;
                    } else {
                        stream_res = Some(Err(e));
                        break;
                    }
                }
            }
        }

        let result = match stream_res.unwrap_or_else(|| {
            Err(crate::ai::gemini::GeminiError::ApiError(format!(
                "All {} keys exhausted: {}",
                all_api_keys.len(),
                last_err_str
            )))
        }) {
            Ok(text) => {
                let cited = if has_context {
                    filter_cited_files(&text, &attached)
                } else {
                    vec![]
                };
                let _ = state.app_handle.emit("smart-stream-end", serde_json::json!({
                    "attached_files": cited.iter().map(|f| serde_json::json!({"file_name": f.file_name, "file_path": f.file_path, "score": f.score})).collect::<Vec<_>>(),
                    "total_ms": pipeline_start.elapsed().as_millis() as u64,
                }));
                emit_done("ai", t_ai.elapsed().as_millis());
                let total_ms = pipeline_start.elapsed().as_millis();
                let time_label = if total_ms < 1000 {
                    format!("{}ms", total_ms)
                } else {
                    format!("{:.1}s", total_ms as f64 / 1000.0)
                };
                emit_step("done", &format!("✅ Hoàn thành trong {} ⚡", time_label));
                emit_done("done", total_ms);
                Ok(SmartResponse {
                    intent: "chat".to_string(),
                    search_results: None,
                    chat_response: Some(text),
                    keywords,
                    attached_files: cited,
                })
            }
            Err(e) => {
                log::error!("[Chatbot] Follow-up stream error: {}", e);
                let _ = state.app_handle.emit("smart-stream-end", serde_json::json!({}));
                emit_done("ai", t_ai.elapsed().as_millis());
                let user_msg = if format!("{}", e).contains("Rate limited") {
                    "⚠️ API đang bị rate limit. Chờ 1-2 phút rồi thử lại.".to_string()
                } else {
                    format!("⚠️ Lỗi AI: {}", e)
                };
                Ok(SmartResponse {
                    intent: "chat".to_string(),
                    search_results: None,
                    chat_response: Some(user_msg),
                    keywords,
                    attached_files: vec![],
                })
            }
        };

        // Save assistant response + source metadata
        if let Ok(ref resp) = result {
            let raw_content = resp.chat_response.clone().unwrap_or_default();
            let file_names: Vec<&str> = resp.attached_files.iter().map(|f| f.file_name.as_str()).collect();
            let content = if file_names.is_empty() {
                raw_content
            } else {
                format!("{}\n[Sources: {}]", raw_content, file_names.join(", "))
            };
            // ★ T2: Fire-and-forget save — không block return
            let db_clone = state.db.clone();
            let session_clone = session_id.clone();
            let model_clone = model.clone();
            tokio::spawn(async move {
                let db = db_clone.lock().await;
                let msg = crate::ai::memory::ChatMessage {
                    id: uuid::Uuid::new_v4().to_string(),
                    session_id: session_clone,
                    role: "assistant".to_string(),
                    content,
                    citations: None,
                    model: Some(model_clone),
                    timestamp: chrono::Utc::now().timestamp(),
                };
                let _ = crate::ai::memory::ConversationMemory::save_message(&db.conn, &msg);
            });
        }
        log::info!(
            "[Chatbot] ⚡ Fast follow-up complete: {}ms",
            pipeline_start.elapsed().as_millis()
        );
        return result;
    }

    // ══════════════════════════════════════════════════════════════
    // ★ T1: PARALLEL TRACKS — AI keyword + embed_text đồng thời
    // embed_text chỉ cần query gốc, KHÔNG cần AI keywords
    // → chạy song song tiết kiệm 200-1500ms
    // ══════════════════════════════════════════════════════════════

    // Track A: AI keyword extraction (0-1500ms, needs Gemini Flash)
    let prev_kw_owned = prev_context.as_ref().map(|p| p.keywords.clone());
    let prev_files_owned: Option<Vec<String>> = prev_context.as_ref().map(|p| {
        p.attached_files.iter().map(|f| f.file_name.clone()).collect()
    });
    let query_for_kw = query.clone();
    let keys_for_kw = all_api_keys.clone();

    let keyword_fut = async move {
        let prev_kw_ref = prev_kw_owned.as_deref();
        let prev_files_ref: Option<Vec<&str>> = prev_files_owned.as_ref().map(|v| {
            v.iter().map(|s| s.as_str()).collect()
        });
        extract_keywords_ai(&query_for_kw, &keys_for_kw, prev_kw_ref, prev_files_ref.as_deref()).await
    };

    // Track B: embed_text for vector search (200-500ms, needs Gemini Embedding API)
    let has_vectors = state.vector_index.len() > 0;
    let embed_fut = async {
        if !has_vectors {
            return None;
        }
        match tokio::time::timeout(
            std::time::Duration::from_secs(3),
            state.embedding_pipeline.embed_text(&query),
        )
        .await
        {
            Ok(Ok(vec)) => Some(vec),
            Ok(Err(e)) => {
                log::warn!("[Search] Embed failed (parallel): {}", e);
                None
            }
            Err(_) => {
                log::warn!("[Search] Embed timeout 3s (parallel)");
                None
            }
        }
    };

    // ★ Run both tracks in parallel!
    let (ai_kw_result, query_vec_opt) = tokio::join!(keyword_fut, embed_fut);

    // Process keyword result (same logic as before)
    let raw_kw = match ai_kw_result {
        Some(kw) if !kw.trim().is_empty() => {
            log::info!("[Chatbot] AI keywords: '{}'", kw);
            kw
        }
        _ => {
            let fallback = extract_keywords(&query);
            log::info!("[Chatbot] Fallback heuristic keywords: '{}'", fallback);
            fallback
        }
    };

    let meaningful_count = raw_kw
        .split_whitespace()
        .filter(|w| w.len() >= 2)
        .filter(|w| !STOP_WORDS.contains(w))
        .count();

    let kw = if meaningful_count < 2 && prev_context.is_some() {
        let prev_kw = &prev_context.as_ref().unwrap().keywords;
        let merged = format!("{} {}", raw_kw, prev_kw);
        let mut seen = std::collections::HashSet::new();
        let deduped: Vec<&str> = merged
            .split_whitespace()
            .filter(|w| seen.insert(w.to_lowercase()))
            .collect();
        let enriched = deduped.join(" ");
        log::info!(
            "[Chatbot] L1 Enriched: '{}' (raw='{}' + prev='{}')",
            enriched,
            raw_kw,
            prev_kw
        );
        enriched
    } else {
        log::info!(
            "[Chatbot] Keywords: '{}' ({} meaningful)",
            raw_kw,
            meaningful_count
        );
        raw_kw
    };
    emit_done("analyze", 0);

    // ── Search: BM25 (needs keywords) + Vector (pre-computed) ──
    emit_step(
        "search",
        &format!("🔍 Tìm: {}...", kw.chars().take(30).collect::<String>()),
    );
    let t_search = std::time::Instant::now();

    // BM25 search — needs keywords (from Track A)
    // ★ If folder_id provided, scope BM25 search to that folder only
    let bm25_results = {
        let tantivy_guard = state.tantivy.lock().await;
        match tantivy_guard.as_ref() {
            Some(t) => {
                if let Some(ref fid) = folder_id {
                    log::info!("[Search] BM25 scoped to folder: {}", fid);
                    crate::search::hybrid::HybridSearch::search_bm25_in_folder(t, &kw, 10, fid)
                        .unwrap_or_default()
                } else {
                    crate::search::hybrid::HybridSearch::search_bm25_only(t, &kw, 10)
                        .unwrap_or_default()
                }
            }
            None => vec![],
        }
    };

    // ★ Get folder file paths for vector filtering (if folder_id provided)
    let folder_file_paths: Option<std::collections::HashSet<String>> =
        if let Some(ref fid) = folder_id {
            let db = state.db.lock().await;
            let files = crate::db::sqlite::file_tracking::list_files_by_folder(&db.conn, fid)
                .unwrap_or_default();
            Some(files.into_iter().map(|(path, _, _, _)| path).collect())
        } else {
            None
        };

    // Vector search — uses pre-computed query_vec (from Track B)
    let results = if let Some(query_vec) = query_vec_opt {
        let raw_vec_results = state.vector_index.search(&query_vec, 15);

        // ★ Filter vector results by folder if needed
        let vec_results = if let Some(ref folder_paths) = folder_file_paths {
            raw_vec_results
                .into_iter()
                .filter(|vr| folder_paths.contains(&vr.file_path))
                .collect::<Vec<_>>()
        } else {
            raw_vec_results
        };

        log::info!(
            "[Search] Parallel: BM25={}, Vector={} in {}ms{}",
            bm25_results.len(),
            vec_results.len(),
            t_search.elapsed().as_millis(),
            if folder_id.is_some() { " (folder-scoped)" } else { "" }
        );
        if vec_results.is_empty() {
            bm25_results
        } else {
            // Merge: vector first (semantic), then unique BM25
            let mut merged: Vec<crate::search::hybrid::HybridResult> = vec_results
                .into_iter()
                .map(|vr| crate::search::hybrid::HybridResult {
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
        log::info!(
            "[Search] BM25 only: {} results in {}ms",
            bm25_results.len(),
            t_search.elapsed().as_millis()
        );
        bm25_results
    };
    log::info!("[Chatbot] Search: {} chunks for '{}'", results.len(), kw);

    let tantivy_guard = state.tantivy.lock().await;
    let tantivy_ref = (*tantivy_guard).as_ref();
    let (ctx, att) =
        build_context_from_results(&kw, &results, min_match_score, &query, tantivy_ref);
    drop(tantivy_guard);

    // ── Layer 2: Search-then-Fallback ──
    // Nếu search trống + có context cũ → reuse
    let (context, attached, keywords) =
        if (ctx.trim().is_empty() || ctx.contains("Không tìm thấy")) && prev_context.is_some() {
            let prev = prev_context.as_ref().unwrap();
            log::info!(
                "[Chatbot] L2 Fallback: search empty → reusing prev context ({} chars, {} files)",
                prev.context_text.chars().count(),
                prev.attached_files.len()
            );
            let prev_att: Vec<AttachedFile> = prev
                .attached_files
                .iter()
                .map(|f| AttachedFile {
                    file_name: f.file_name.clone(),
                    file_path: f.file_path.clone(),
                    score: 1.0,
                })
                .collect();
            emit_done("search", t_search.elapsed().as_millis());
            emit_step("search", "🔄 Dùng lại ngữ cảnh từ câu hỏi trước...");
            emit_done("search", 0);
            (prev.context_text.clone(), prev_att, prev.keywords.clone())
        } else {
            emit_done("search", t_search.elapsed().as_millis());
            log::info!(
                "[Chatbot] Context: {} chars, {} files",
                ctx.chars().count(),
                att.len()
            );

            // Emit sources
            let _ = state.app_handle.emit(
                "smart-sources-found",
                serde_json::json!({
                    "files": att.iter().map(|f| serde_json::json!({
                        "file_name": f.file_name,
                        "file_path": f.file_path,
                        "score": f.score,
                    })).collect::<Vec<_>>(),
                    "total": att.len(),
                    "keywords": &kw,
                }),
            );

            // ★ T3: Fire-and-forget session save — streaming sau đó tốn 1.5-5s
            if !att.is_empty() {
                let sf: Vec<crate::SessionFile> = att
                    .iter()
                    .map(|f| crate::SessionFile {
                        file_path: f.file_path.clone(),
                        file_name: f.file_name.clone(),
                    })
                    .collect();
                let ctx_clone = ctx.clone();
                let kw_clone = kw.clone();
                let session_clone = session_id.clone();
                let contexts_clone = state.session_contexts.clone();
                tokio::spawn(async move {
                    let mut contexts = contexts_clone.lock().await;
                    contexts.insert(
                        session_clone.clone(),
                        crate::SessionContext {
                            session_id: session_clone,
                            attached_files: sf,
                            keywords: kw_clone,
                            context_text: ctx_clone,
                            timestamp: std::time::Instant::now(),
                        },
                    );
                });
            }

            (ctx, att, kw)
        };

    // ── Step 3: Build prompt + Stream response ──
    let ctx_chars = context.chars().count();
    let ctx_files = attached.len();
    if ctx_chars > 0 {
        emit_step(
            "rank",
            &format!("📄 Đã chọn {} tài liệu phù hợp nhất", ctx_files),
        );
    } else {
        emit_step("rank", "💬 Không cần tài liệu — trả lời trực tiếp");
    }
    emit_done("rank", 0);

    let has_context = !context.is_empty();

    // Build system prompt
    let source_list: String = attached
        .iter()
        .enumerate()
        .map(|(i, f)| format!("[{}] {}", i + 1, f.file_name))
        .collect::<Vec<_>>()
        .join(", ");

    let history_note = if !history.is_empty() {
        let prev = build_history_context(
            &history
                .iter()
                .cloned()
                .chain(std::iter::once(("user".to_string(), query.clone())))
                .collect::<Vec<_>>(),
        );
        format!("\nĐÂY LÀ HỘI THOẠI TIẾP NỐI. Ngữ cảnh trước:\n{}\n", prev)
    } else {
        String::new()
    };

    let (system_prompt, doc_block) = if has_context {
        let prompt_type = detect_prompt_type(&query);
        log::info!(
            "[Chatbot] Prompt type: {:?}, context: {} chars, {} files",
            prompt_type,
            ctx_chars,
            ctx_files
        );
        build_prompt(&prompt_type, &history_note, &source_list, &context)
    } else {
        (format!(
            "Bạn là trợ lý AI chuyên phân tích tài liệu. Trả lời bằng tiếng Việt, dùng Markdown.\n\
            {}\n\
            📌 Quy tắc:\n\
            • Trả lời thẳng, ngắn gọn, chuyên nghiệp.\n\
            • Nếu câu hỏi cần tài liệu mà bạn không có → gợi ý user thêm thư mục hoặc tìm kiếm cụ thể hơn.\n\
            • Dùng **bold** cho điểm quan trọng, bullet points cho danh sách.",
            history_note
        ), String::new())
    };

    // Build messages cho streaming — context guard trim
    let mut stream_history = crate::commands::context_guard::trim_history(
        &history,
        crate::commands::context_guard::MAX_HISTORY_TURNS,
        crate::commands::context_guard::MAX_HISTORY_TOKENS,
    );
    // ★ Documents in user message, system prompt stable for Gemini implicit caching
    let user_msg = if doc_block.is_empty() {
        query.clone()
    } else {
        format!("{}\n\n{}", doc_block, query)
    };
    stream_history.push(("user".to_string(), user_msg));

    // Log token usage cho monitoring
    crate::commands::context_guard::log_context_stats(
        &system_prompt,
        &stream_history,
        &context,
    );

    // Emit stream start
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

    // Auto model selection — ModelRegistry resolve
    let effective_model = if model == "auto" || model.is_empty() {
        state.model_registry.resolve_chat(None)
    } else {
        model.clone()
    };

    let model_label = if effective_model.contains("lite") {
        "⚡ Flash Lite"
    } else if effective_model.contains("flash") {
        "🚀 Flash"
    } else {
        &effective_model
    };
    emit_step("ai", &format!("✍️ Đang viết câu trả lời ({})", model_label));
    let t_ai = std::time::Instant::now();

    log::info!(
        "[Chatbot] Streaming: {} files, prompt {} chars, pipeline: {}ms",
        attached.len(),
        system_prompt.len(),
        pipeline_start.elapsed().as_millis()
    );

    let app_handle = state.app_handle.clone();

    // Key rotation on 429
    let mut lerr = String::new();
    let mut sres: Option<Result<String, _>> = None;
    for (ki, tk) in all_api_keys.iter().enumerate() {
        let rc2 = crate::ai::gemini::GeminiClient::new(tk.clone());
        let ah3 = app_handle.clone();
        let r2 = rc2
            .stream_generate_content(
                &effective_model,
                &stream_history,
                Some(&system_prompt),
                creativity_level,
                |chunk| {
                    let _ = ah3.emit("smart-stream", serde_json::json!({"chunk": chunk}));
                },
            )
            .await;
        match r2 {
            Ok(t) => {
                sres = Some(Ok(t));
                break;
            }
            Err(e) => {
                let es = format!("{}", e);
                if es.contains("Rate") || es.contains("429") || es.contains("quota") {
                    log::warn!("[Chatbot] Main: key #{} rate limited, rotating...", ki + 1);
                    lerr = es;
                    continue;
                } else {
                    sres = Some(Err(e));
                    break;
                }
            }
        }
    }
    let result = match sres.unwrap_or_else(|| {
        Err(crate::ai::gemini::GeminiError::ApiError(format!(
            "All keys exhausted: {}",
            lerr
        )))
    }) {
        Ok(text) => {
            let cited = if has_context {
                filter_cited_files(&text, &attached)
            } else {
                vec![]
            };

            let _ = state.app_handle.emit(
                "smart-stream-end",
                serde_json::json!({
                    "attached_files": cited.iter().map(|f| serde_json::json!({
                        "file_name": f.file_name,
                        "file_path": f.file_path,
                        "score": f.score,
                    })).collect::<Vec<_>>(),
                    "total_ms": pipeline_start.elapsed().as_millis() as u64,
                }),
            );

            emit_done("ai", t_ai.elapsed().as_millis());

            // Pipeline total
            let total_ms = pipeline_start.elapsed().as_millis();
            let time_label = if total_ms < 1000 {
                format!("{}ms", total_ms)
            } else {
                format!("{:.1}s", total_ms as f64 / 1000.0)
            };
            emit_step("done", &format!("✅ Hoàn thành trong {}", time_label));
            emit_done("done", total_ms);

            Ok(SmartResponse {
                intent: "chat".to_string(),
                search_results: None,
                chat_response: Some(text),
                keywords,
                attached_files: cited,
            })
        }
        Err(e) => {
            log::error!("[Chatbot] Stream error: {}", e);
            let _ = state
                .app_handle
                .emit("smart-stream-end", serde_json::json!({}));
            emit_done("ai", t_ai.elapsed().as_millis());

            let user_msg = if format!("{}", e).contains("Rate limited") {
                "⚠️ API đang bị rate limit. Chờ 1-2 phút rồi thử lại.".to_string()
            } else {
                format!("⚠️ Lỗi AI: {}", e)
            };

            Ok(SmartResponse {
                intent: "chat".to_string(),
                search_results: None,
                chat_response: Some(user_msg),
                keywords,
                attached_files: vec![],
            })
        }
    };

    // ★ T2: Fire-and-forget save — không block return
    if let Ok(ref resp) = result {
        let db_clone = state.db.clone();
        let session_clone = session_id.clone();
        let model_clone = model.clone();
        let raw_content = resp.chat_response.clone().unwrap_or_default();
        let file_names: Vec<String> = resp.attached_files.iter().map(|f| f.file_name.clone()).collect();
        let content = if file_names.is_empty() {
            raw_content
        } else {
            format!("{}\n[Sources: {}]", raw_content, file_names.join(", "))
        };
        tokio::spawn(async move {
            let db = db_clone.lock().await;
            let msg = crate::ai::memory::ChatMessage {
                id: uuid::Uuid::new_v4().to_string(),
                session_id: session_clone,
                role: "assistant".to_string(),
                content,
                citations: None,
                model: Some(model_clone),
                timestamp: chrono::Utc::now().timestamp(),
            };
            let _ = crate::ai::memory::ConversationMemory::save_message(&db.conn, &msg);
        });
    }

    log::info!(
        "[Chatbot] ✅ Complete: {}ms total",
        pipeline_start.elapsed().as_millis()
    );
    result
}

/// ⚡ Pre-search: Chạy ngầm khi user đang gõ (debounced 500ms)
/// → Vector-first search, fallback BM25 nếu chưa sẵn sàng
#[tauri::command]
pub async fn pre_search(
    query: String,
    state: State<'_, AppState>,
) -> Result<serde_json::Value, String> {
    if query.trim().len() < 2 {
        return Ok(serde_json::json!({"cached": false}));
    }

    let keywords = extract_keywords(&query);
    let vector_index = &state.vector_index;
    let has_vectors = vector_index.len() > 0;

    // Vector-first: dùng HNSW nếu có vectors
    let results = if has_vectors {
        let pipeline = &state.embedding_pipeline;
        match pipeline.embed_text(&query).await {
            Ok(query_vec) => {
                let vec_results = vector_index.search(&query_vec, 30);
                log::info!("[PreSearch] Vector: {} results", vec_results.len());
                vec_results
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
                    .collect()
            }
            Err(e) => {
                log::warn!("[PreSearch] Embed failed: {}, fallback BM25", e);
                // Fallback BM25
                let tantivy_guard = state.tantivy.lock().await;
                match tantivy_guard.as_ref() {
                    Some(t) => HybridSearch::search_bm25_only(t, &keywords, 30).unwrap_or_default(),
                    None => vec![],
                }
            }
        }
    } else {
        // Không có vectors → dùng BM25
        let tantivy_guard = state.tantivy.lock().await;
        match tantivy_guard.as_ref() {
            Some(t) => HybridSearch::search_bm25_only(t, &keywords, 30).unwrap_or_default(),
            None => return Ok(serde_json::json!({"cached": false})),
        }
    };

    let count = results.len();

    // Cache kết quả
    let mut cache = state.pre_search_cache.lock().await;
    *cache = Some(crate::PreSearchCache {
        query: query.clone(),
        keywords: keywords.clone(),
        results,
        timestamp: std::time::Instant::now(),
    });

    let preview: String = query.chars().take(30).collect();
    log::info!("[PreSearch] Cached {} results for '{}'", count, preview);

    Ok(serde_json::json!({
        "cached": true,
        "count": count,
        "keywords": keywords,
    }))
}


/// Search qua AI Chat mode (giữ backward compat)
#[tauri::command]
pub async fn search_documents(
    query: String,
    state: State<'_, AppState>,
) -> Result<SearchResponse, String> {
    let keywords = extract_keywords(&query);
    match do_search(&keywords, &query, &state).await? {
        Some(resp) => Ok(resp),
        None => Err("Tantivy index chưa được khởi tạo".to_string()),
    }
}

/// Direct Search mode (giữ backward compat)
#[tauri::command]
pub async fn search_direct(
    query: String,
    state: State<'_, AppState>,
) -> Result<SearchResponse, String> {
    let keywords = extract_keywords(&query);
    match do_search(&keywords, &query, &state).await? {
        Some(resp) => Ok(resp),
        None => Err("Tantivy index chưa được khởi tạo".to_string()),
    }
}

/// AI-based keyword extraction using Gemini Flash
/// Timeout 1.5s → fallback to heuristic if slow/fail
/// prev_keywords + prev_files: context từ session trước để hiểu follow-up
async fn extract_keywords_ai(
    query: &str,
    api_keys: &[String],
    prev_keywords: Option<&str>,
    prev_files: Option<&[&str]>,
) -> Option<String> {
    if api_keys.is_empty() {
        return None;
    }

    let key = &api_keys[0];
    let client = crate::ai::gemini::GeminiClient::new(key.to_string());

    // Build context hint nếu có session trước
    let context_hint = match (prev_keywords, prev_files) {
        (Some(kw), Some(files)) if !kw.is_empty() && !files.is_empty() => {
            format!(
                "\nNgữ cảnh trước: đã tìm kiếm [{}] trong files [{}]. Nếu câu hỏi là follow-up (ví dụ 'file nào khác?', 'còn gì nữa?'), hãy dùng lại keywords trước đó.",
                kw,
                files.join(", ")
            )
        }
        (Some(kw), _) if !kw.is_empty() => {
            format!("\nNgữ cảnh trước: keywords [{}].", kw)
        }
        _ => String::new(),
    };

    let prompt = format!(
        r#"Từ câu hỏi sau, trích xuất CHỈ các từ khóa tìm kiếm chính (tên riêng, mã, số phiên bản, thuật ngữ chuyên môn).
Loại bỏ: động từ hành động (tìm, tạo, viết, báo cáo), từ nối, đại từ.
Giữ nguyên cụm từ ghép (ví dụ: "AZV Test 3" → giữ nguyên, KHÔNG tách).
Chỉ trả về các từ khóa, cách nhau bởi dấu cách. Không giải thích.{}

Câu hỏi: {}"#,
        context_hint, query
    );

    let messages = vec![("user".to_string(), prompt)];

    // Timeout 1.5s — nếu AI chậm → fallback
    let result = tokio::time::timeout(
        std::time::Duration::from_millis(1500),
        client.generate_content("gemini-2.0-flash", &messages, None),
    )
    .await;

    match result {
        Ok(Ok(text)) => {
            let cleaned = text.trim().to_string();
            // Sanity check: AI không trả về quá dài hoặc rỗng
            if cleaned.is_empty() || cleaned.len() > 200 {
                log::warn!("[AI-KW] Response too long/empty: '{}' → fallback", cleaned.chars().take(50).collect::<String>());
                return None;
            }
            log::info!("[AI-KW] Extracted: '{}' from query: '{}'", cleaned, query.chars().take(50).collect::<String>());
            Some(cleaned)
        }
        Ok(Err(e)) => {
            log::warn!("[AI-KW] API error: {} → fallback", e);
            None
        }
        Err(_) => {
            log::warn!("[AI-KW] Timeout (1.5s) → fallback to heuristic");
            None
        }
    }
}
