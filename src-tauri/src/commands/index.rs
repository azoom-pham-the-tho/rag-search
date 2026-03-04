use crate::db::sqlite;
use crate::indexer::chunker::{self, Chunk, ChunkConfig};
use crate::parser;
use crate::AppState;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::sync::atomic::Ordering;
use tauri::{Emitter, State};

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct IndexStatus {
    pub total_files: usize,
    pub indexed_files: usize,
    pub pending_files: usize,
    pub error_files: usize,
    pub is_indexing: bool,
    pub current_file: Option<String>,
}

/// Event payload gửi cho frontend trong quá trình index
#[derive(Debug, Clone, Serialize)]
pub struct IndexProgressEvent {
    pub folder_id: String,
    pub folder_name: String,
    pub phase: String, // "scanning" | "parsing" | "indexing" | "done" | "error"
    pub current_file: String,
    pub current_index: usize, // file thứ mấy
    pub total_files: usize,
    pub indexed_count: usize,
    pub error_count: usize,
    pub total_vectors: usize,
    pub message: String,
}

/// Kết quả parse + chunk từ Stage 1 (rayon)
struct ParsedFileResult {
    file_path: String,
    file_name: String,
    file_modified: i64,
    chunks: Vec<Chunk>,
}

/// Quét và index tất cả files trong một folder — emit events cho frontend
/// ★ Sử dụng rayon để parse + chunk song song trên ALL CPU cores
pub async fn index_folder(
    folder_id: &str,
    folder_path: &str,
    folder_name: &str,
    state: &AppState,
) -> Result<(usize, usize), String> {
    let path = std::path::PathBuf::from(folder_path);
    if !path.exists() || !path.is_dir() {
        return Err(format!("Thư mục không tồn tại: {}", folder_path));
    }

    // Emit: scanning phase
    let _ = state.app_handle.emit(
        "index-progress",
        IndexProgressEvent {
            folder_id: folder_id.to_string(),
            folder_name: folder_name.to_string(),
            phase: "scanning".to_string(),
            current_file: String::new(),
            current_index: 0,
            total_files: 0,
            indexed_count: 0,
            error_count: 0,
            total_vectors: 0,
            message: format!("Đang quét thư mục {}...", folder_name),
        },
    );

    // Step 1: Walk directory to find supported files
    let files = walk_supported_files(&path).map_err(|e| format!("Lỗi quét thư mục: {}", e))?;

    let total_files = files.len();
    log::info!(
        "[Index] Found {} supported files in '{}'",
        total_files,
        folder_path
    );

    if total_files == 0 {
        let _ = state.app_handle.emit(
            "index-progress",
            IndexProgressEvent {
                folder_id: folder_id.to_string(),
                folder_name: folder_name.to_string(),
                phase: "done".to_string(),
                current_file: String::new(),
                current_index: 0,
                total_files: 0,
                indexed_count: 0,
                error_count: 0,
                total_vectors: 0,
                message: format!("Không tìm thấy file hỗ trợ trong {}", folder_name),
            },
        );
        return Ok((0, 0));
    }

    // ★ INCREMENTAL INDEXING: So sanh filesystem vs DB de tim file moi/thay doi/da xoa
    let known_files = {
        let db = state.db.lock().await;
        sqlite::file_tracking::get_known_files(&db.conn, folder_id).unwrap_or_default()
    };

    // Phan loai files
    let mut files_to_index: Vec<(String, i64)> = Vec::new(); // new + changed
    let mut unchanged_count = 0usize;

    for (file_path, file_modified) in &files {
        match known_files.get(file_path) {
            Some(db_modified) if *db_modified == *file_modified => {
                // File khong thay doi → skip
                unchanged_count += 1;
            }
            Some(_db_modified) => {
                // File da thay doi (modified timestamp khac) → re-index
                log::info!("[Index] 📝 Changed: {}", file_path);
                files_to_index.push((file_path.clone(), *file_modified));
            }
            None => {
                // File moi → index
                log::info!("[Index] 🆕 New: {}", file_path);
                files_to_index.push((file_path.clone(), *file_modified));
            }
        }
    }

    // Tim files da xoa (co trong DB nhung khong con tren disk)
    let current_paths: std::collections::HashSet<&String> = files.iter().map(|(p, _)| p).collect();
    let deleted_files: Vec<String> = known_files
        .keys()
        .filter(|p| !current_paths.contains(p))
        .cloned()
        .collect();

    // Xoa data cua files da xoa
    if !deleted_files.is_empty() {
        log::info!(
            "[Index] 🗑 Removing {} deleted files from index",
            deleted_files.len()
        );
        for del_path in &deleted_files {
            // Xoa vectors
            state.vector_index.remove_file(del_path);
            // Xoa tantivy chunks
            {
                let tantivy_guard = state.tantivy.lock().await;
                if let Some(ref tantivy) = *tantivy_guard {
                    let _ = tantivy.delete_file_chunks(del_path);
                }
            }
            // Xoa file_tracking
            {
                let db = state.db.lock().await;
                let _ = sqlite::file_tracking::delete_by_path(&db.conn, del_path);
            }
            log::info!("[Index] 🗑 Removed: {}", del_path);
        }
        let _ = state.vector_index.save();
    }

    // Xoa data cu cua files thay doi (de index lai)
    for (file_path, _) in &files_to_index {
        if known_files.contains_key(file_path) {
            // File changed → xoa old data truoc khi re-index
            state.vector_index.remove_file(file_path);
            {
                let tantivy_guard = state.tantivy.lock().await;
                if let Some(ref tantivy) = *tantivy_guard {
                    let _ = tantivy.delete_file_chunks(file_path);
                }
            }
        }
    }

    log::info!(
        "[Index] 📊 Incremental: {} unchanged, {} to index, {} deleted",
        unchanged_count,
        files_to_index.len(),
        deleted_files.len()
    );

    // Neu khong co gi thay doi
    if files_to_index.is_empty() {
        let _ = state.app_handle.emit(
            "index-progress",
            IndexProgressEvent {
                folder_id: folder_id.to_string(),
                folder_name: folder_name.to_string(),
                phase: "done".to_string(),
                current_file: String::new(),
                current_index: 0,
                total_files,
                indexed_count: unchanged_count,
                error_count: 0,
                total_vectors: 0,
                message: format!(
                    "✅ Không có thay đổi ({} file đã index{})",
                    unchanged_count,
                    if !deleted_files.is_empty() {
                        format!(", {} file đã xóa", deleted_files.len())
                    } else {
                        String::new()
                    }
                ),
            },
        );
        return Ok((total_files, unchanged_count));
    }

    // Thay files bang files_to_index de chi index cac file can thiet
    let files = files_to_index;
    let total_to_index = files.len();

    let mut indexed_count = 0;
    #[allow(unused_assignments)]
    let mut error_count = 0;
    let mut total_chunks = 0;
    let mut total_vectors = 0;

    // Check Gemini API key for vector embedding
    let embedding_ready = state.embedding_pipeline.is_ready().await;
    if embedding_ready {
        log::info!("[Index] Gemini Embedding API ready for vector indexing");
    } else {
        log::warn!("[Index] Gemini API key chưa cấu hình — indexing without vectors");
    }

    // ════════════════════════════════════════════════════════
    // ★ STAGE 1: Rayon Parallel Parse + Chunk (CPU-bound)
    // Sử dụng tất cả CPU cores để parse + chunk song song
    // ════════════════════════════════════════════════════════
    let _ = state.app_handle.emit(
        "index-progress",
        IndexProgressEvent {
            folder_id: folder_id.to_string(),
            folder_name: folder_name.to_string(),
            phase: "parsing".to_string(),
            current_file: String::new(),
            current_index: 0,
            total_files: total_to_index,
            indexed_count: 0,
            error_count: 0,
            total_vectors: 0,
            message: format!(
                "⚡ Đang parse {} files song song ({} CPU cores)...",
                total_to_index,
                rayon::current_num_threads()
            ),
        },
    );
    tokio::task::yield_now().await;

    let cancel_flag = state.cancel_indexing.clone();
    let files_for_rayon = files.clone();

    // Chạy rayon par_iter trong spawn_blocking (vì rayon block thread)
    let (parsed_results, parse_errors) = tokio::task::spawn_blocking(move || {
        let chunk_config = ChunkConfig::default();
        let mut successes: Vec<ParsedFileResult> = Vec::new();
        let mut errors: Vec<String> = Vec::new();

        // Rayon par_iter: parse + chunk song song trên ALL CPU cores
        let results: Vec<Result<ParsedFileResult, String>> = files_for_rayon
            .par_iter()
            .map(|(file_path_str, file_modified)| {
                // Check cancel flag
                if cancel_flag.load(Ordering::Relaxed) {
                    return Err("cancelled".to_string());
                }

                let file_path = std::path::PathBuf::from(file_path_str);
                let file_name = file_path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| file_path_str.clone());

                // Parse file (CPU-heavy: PDF extraction, OCR, text processing)
                let parsed = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    parser::parse_file(&file_path)
                })) {
                    Ok(Ok(doc)) => doc,
                    Ok(Err(e)) => {
                        let err_str = format!("{}", e);
                        if err_str.contains("không chứa nội dung") {
                            log::warn!("[Index] ⚠ Empty content: {}", file_name);
                        } else {
                            log::warn!("[Index] ❌ Parse error {}: {}", file_name, e);
                        }
                        return Err(format!("parse:{}", file_name));
                    }
                    Err(_) => {
                        log::warn!("[Index] ❌ Parse panic: {}", file_name);
                        return Err(format!("panic:{}", file_name));
                    }
                };

                // Chunk document (CPU-bound)
                let chunks = if let Some(ref kr_chunks) = parsed.kreuzberg_chunks {
                    log::info!(
                        "[Index] ✓ {} → {} kreuzberg chunks",
                        file_name,
                        kr_chunks.len()
                    );
                    chunker::from_kreuzberg_chunks(kr_chunks, file_path_str, &parsed.file_name)
                } else {
                    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        chunker::chunk_text(
                            &parsed.content,
                            file_path_str,
                            &parsed.file_name,
                            &chunk_config,
                        )
                    })) {
                        Ok(c) => {
                            log::info!("[Index] ✓ {} → {} manual chunks", file_name, c.len());
                            c
                        }
                        Err(_) => {
                            log::warn!("[Index] ❌ Chunker panic: {}", file_name);
                            return Err(format!("chunk_panic:{}", file_name));
                        }
                    }
                };

                if chunks.is_empty() {
                    return Err(format!("empty:{}", file_name));
                }

                Ok(ParsedFileResult {
                    file_path: file_path_str.clone(),
                    file_name,
                    file_modified: *file_modified,
                    chunks,
                })
            })
            .collect();

        // Phân loại kết quả
        for result in results {
            match result {
                Ok(parsed) => successes.push(parsed),
                Err(e) => {
                    if e != "cancelled" && !e.starts_with("empty:") {
                        errors.push(e);
                    }
                }
            }
        }

        (successes, errors)
    })
    .await
    .map_err(|e| format!("Rayon thread pool error: {}", e))?;

    error_count = parse_errors.len();
    log::info!(
        "[Index] ⚡ Stage 1 done: {} parsed OK, {} errors ({} CPU cores)",
        parsed_results.len(),
        error_count,
        rayon::current_num_threads()
    );

    // ════════════════════════════════════════════════════════
    // ★ STAGE 2: Sequential Embed + Tantivy + DB (I/O-bound)
    // Embedding qua Gemini API (network I/O) + Tantivy write + SQLite
    // ════════════════════════════════════════════════════════

    // Create ONE Tantivy writer for all files (batch mode)
    let tantivy_writer = {
        let tantivy_guard = state.tantivy.lock().await;
        tantivy_guard.as_ref().and_then(|t| t.create_writer().ok())
    };

    for (i, parsed_file) in parsed_results.iter().enumerate() {
        // Kiểm tra cancel flag
        if state.cancel_indexing.load(Ordering::Relaxed) {
            log::info!(
                "[Index] ⏹ Dừng indexing theo yêu cầu user (sau {} files)",
                indexed_count
            );
            break;
        }

        let file_path_str = &parsed_file.file_path;
        let file_name = &parsed_file.file_name;
        let chunks = &parsed_file.chunks;
        let chunk_count = chunks.len();

        // Emit progress event
        let _ = state.app_handle.emit(
            "index-progress",
            IndexProgressEvent {
                folder_id: folder_id.to_string(),
                folder_name: folder_name.to_string(),
                phase: "indexing".to_string(),
                current_file: file_name.clone(),
                current_index: i + 1,
                total_files: parsed_results.len(),
                indexed_count,
                error_count,
                total_vectors,
                message: format!(
                    "Đang index: {} ({}/{})",
                    file_name,
                    i + 1,
                    parsed_results.len()
                ),
            },
        );
        tokio::task::yield_now().await;

        // Add to Tantivy using shared writer (no commit yet)
        if let Some(ref writer) = tantivy_writer {
            let tantivy_guard = state.tantivy.lock().await;
            if let Some(tantivy) = tantivy_guard.as_ref() {
                tantivy.delete_file_with_writer(writer, file_path_str);
                if let Err(e) = tantivy.add_chunks_to_writer(writer, chunks, folder_id) {
                    log::error!("[Index] Tantivy error {}: {}", file_name, e);
                    error_count += 1;
                    continue;
                }
            }
        }

        // Embedding → HNSW Vector Index (nếu API key sẵn sàng)
        let mut file_has_vectors = false;
        if embedding_ready && !state.cancel_indexing.load(Ordering::Relaxed) {
            log::info!(
                "[Index] Embedding {} chunks for: {}",
                chunks.len(),
                file_name
            );

            // ★ Emit 'embedding' phase
            let _ = state.app_handle.emit(
                "index-progress",
                IndexProgressEvent {
                    folder_id: folder_id.to_string(),
                    folder_name: folder_name.to_string(),
                    phase: "indexing".to_string(),
                    current_file: file_name.clone(),
                    current_index: i + 1,
                    total_files: parsed_results.len(),
                    indexed_count,
                    error_count,
                    total_vectors,
                    message: format!(
                        "Đang embed: {} ({} chunks, tổng {} vectors)",
                        file_name, chunk_count, total_vectors
                    ),
                },
            );
            tokio::task::yield_now().await;

            let cancel = state.cancel_indexing.as_ref();

            // ★ Closure emit progress per-batch
            let app_handle = state.app_handle.clone();
            let fid = folder_id.to_string();
            let fname = folder_name.to_string();
            let fname_file = file_name.clone();
            let base_vectors = total_vectors;
            let ci = i + 1;
            let tf = parsed_results.len();
            let ic = indexed_count;
            let ec = error_count;

            let on_batch = move |batch_vectors_so_far: usize| {
                let running = base_vectors + batch_vectors_so_far;
                let _ = app_handle.emit(
                    "index-progress",
                    serde_json::json!({
                        "folder_id": fid,
                        "folder_name": fname,
                        "phase": "indexing",
                        "current_file": fname_file,
                        "current_index": ci,
                        "total_files": tf,
                        "indexed_count": ic,
                        "error_count": ec,
                        "total_vectors": running,
                        "message": format!("Đang embed: {} vectors...", running),
                    }),
                );
            };

            match state
                .embedding_pipeline
                .process_chunks(chunks, Some(cancel), Some(&on_batch), None)
                .await
            {
                Ok((vectors, metas)) => {
                    if !vectors.is_empty() {
                        state.vector_index.remove_file(file_path_str);
                        state.vector_index.add_vectors(vectors.clone(), metas);
                        total_vectors += vectors.len();
                        file_has_vectors = true;

                        // ★ Save vectors ngay sau mỗi file (resume-safe)
                        if let Err(e) = state.vector_index.save() {
                            log::error!("[Index] Vector save error: {}", e);
                        }

                        log::info!(
                            "[Index] ✓ Embedded {} vectors for: {}",
                            vectors.len(),
                            file_name
                        );
                    } else {
                        log::warn!("[Index] ⚠ Embed returned 0 vectors for: {}", file_name);
                    }
                }
                Err(e) => {
                    log::error!("[Index] ❌ Embed FAILED for {}: {}", file_name, e);
                    // Notify frontend when all API keys exhausted (429)
                    if matches!(e, crate::embedding::EmbeddingError::QuotaExhausted(_, _)) {
                        let _ = state.app_handle.emit(
                            "api-quota-exhausted",
                            serde_json::json!({
                                "message": "API quota hết — tất cả keys đều bị 429",
                                "file": file_name,
                            }),
                        );
                    }
                }
            }
        }

        // Track in database (with has_vectors flag)
        let file_path = std::path::PathBuf::from(file_path_str);
        let file_size = file_path.metadata().map(|m| m.len() as i64).unwrap_or(0);
        let file_hash = format!("{:x}", djb2_hash(file_path_str));

        {
            let db = state.db.lock().await;
            if let Err(e) = sqlite::file_tracking::upsert(
                &db.conn,
                file_path_str,
                folder_id,
                &file_hash,
                parsed_file.file_modified,
                file_size,
                chunk_count,
                file_has_vectors,
            ) {
                log::error!("[Index] ❌ DB upsert FAILED for {}: {}", file_name, e);
            } else {
                log::debug!("[Index] ✓ DB upsert OK: {}", file_name);
            }
        }

        indexed_count += 1;
        total_chunks += chunk_count;
        log::info!(
            "[Index] ✓ {} ({} chunks, vectors: {})",
            file_name,
            chunk_count,
            if file_has_vectors { "✓" } else { "✗" }
        );

        // ★ Emit progress SAU khi file xong
        let _ = state.app_handle.emit(
            "index-progress",
            IndexProgressEvent {
                folder_id: folder_id.to_string(),
                folder_name: folder_name.to_string(),
                phase: "indexing".to_string(),
                current_file: file_name.clone(),
                current_index: i + 1,
                total_files: parsed_results.len(),
                indexed_count,
                error_count,
                total_vectors,
                message: format!(
                    "Đã index: {} ({}/{}) — {} vectors",
                    file_name,
                    indexed_count,
                    parsed_results.len(),
                    total_vectors
                ),
            },
        );
    }

    // ★★★ Phase 2: Resume embedding cho files chưa có vectors ★★★
    let chunk_config = ChunkConfig::default();
    if embedding_ready && !state.cancel_indexing.load(Ordering::Relaxed) {
        let pending_files = {
            let db = state.db.lock().await;
            sqlite::file_tracking::get_files_without_vectors(&db.conn, folder_id)
                .unwrap_or_default()
        };

        if !pending_files.is_empty() {
            log::info!(
                "[Index] 🔄 Resume: {} files cần embed lại",
                pending_files.len()
            );

            for file_path_str in &pending_files {
                if state.cancel_indexing.load(Ordering::Relaxed) {
                    log::info!("[Index] ⏹ Dừng resume embedding");
                    break;
                }

                let file_path = std::path::PathBuf::from(file_path_str);
                let file_name = file_path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();

                if !file_path.exists() {
                    continue;
                }

                // Re-parse file → ParsedDocument
                let fp = file_path.clone();
                let parsed =
                    match tokio::task::spawn_blocking(move || parser::parse_file(&fp)).await {
                        Ok(Ok(doc)) => doc,
                        Ok(Err(e)) => {
                            log::error!("[Index] Re-parse error {}: {}", file_name, e);
                            continue;
                        }
                        Err(e) => {
                            log::error!("[Index] Thread error {}: {}", file_name, e);
                            continue;
                        }
                    };

                // Chunk: same logic as main loop
                let file_path_s = file_path_str.to_string();
                let chunks = if let Some(ref kr_chunks) = parsed.kreuzberg_chunks {
                    chunker::from_kreuzberg_chunks(kr_chunks, &file_path_s, &parsed.file_name)
                } else {
                    let content = parsed.content.clone();
                    let fname = parsed.file_name.clone();
                    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        chunker::chunk_text(&content, &file_path_s, &fname, &chunk_config)
                    })) {
                        Ok(c) => c,
                        Err(_) => {
                            log::warn!("[Index] Chunker panic re-embed: {}", file_name);
                            continue;
                        }
                    }
                };

                if chunks.is_empty() {
                    continue;
                }

                log::info!(
                    "[Index] 🔄 Re-embedding {} chunks for: {}",
                    chunks.len(),
                    file_name
                );

                let cancel = state.cancel_indexing.as_ref();
                match state
                    .embedding_pipeline
                    .process_chunks(
                        &chunks,
                        Some(cancel),
                        None::<&(dyn Fn(usize) + Send + Sync)>,
                        None,
                    )
                    .await
                {
                    Ok((vectors, metas)) => {
                        if !vectors.is_empty() {
                            state.vector_index.remove_file(file_path_str);
                            state.vector_index.add_vectors(vectors.clone(), metas);
                            total_vectors += vectors.len();

                            if let Err(e) = state.vector_index.save() {
                                log::error!("[Index] Vector save error: {}", e);
                            }

                            let db = state.db.lock().await;
                            let _ = sqlite::file_tracking::update_has_vectors(
                                &db.conn,
                                file_path_str,
                                true,
                            );

                            log::info!(
                                "[Index] ✓ Re-embedded {} vectors for: {}",
                                vectors.len(),
                                file_name
                            );
                        }
                    }
                    Err(e) => {
                        log::error!("[Index] ❌ Re-embed FAILED for {}: {}", file_name, e);
                        if matches!(e, crate::embedding::EmbeddingError::QuotaExhausted(_, _)) {
                            let _ = state.app_handle.emit(
                                "api-quota-exhausted",
                                serde_json::json!({
                                    "message": "API quota hết — tất cả keys đều bị 429",
                                    "file": file_name,
                                }),
                            );
                        }
                    }
                }
            }
        }
    }

    // Commit all Tantivy writes at once
    if let Some(mut writer) = tantivy_writer {
        if let Err(e) = writer.commit() {
            log::error!("[Index] Tantivy commit error: {}", e);
        } else {
            log::info!("[Index] Tantivy committed {} chunks", total_chunks);
        }
    }

    // Save HNSW vector index to disk (persist across restarts)
    if total_vectors > 0 {
        if let Err(e) = state.vector_index.save() {
            log::error!("[Index] Vector index save error: {}", e);
        } else {
            log::info!(
                "[Index] Vector index saved: {} total vectors",
                state.vector_index.len()
            );
        }
    }

    // Emit: done hoặc stopped
    let was_cancelled = state.cancel_indexing.load(Ordering::Relaxed);
    let phase = if was_cancelled { "stopped" } else { "done" };
    let message = if was_cancelled {
        format!(
            "Đã dừng: {} files đã xử lý, {} chunks{}",
            indexed_count,
            total_chunks,
            if error_count > 0 {
                format!(", {} lỗi", error_count)
            } else {
                String::new()
            }
        )
    } else {
        let unchanged_info = if unchanged_count > 0 {
            format!(", {} file không đổi", unchanged_count)
        } else {
            String::new()
        };
        let deleted_info = if !deleted_files.is_empty() {
            format!(", {} file đã xóa", deleted_files.len())
        } else {
            String::new()
        };
        format!(
            "Hoàn tất: {} files mới/thay đổi, {} chunks{}{}{}",
            indexed_count,
            total_chunks,
            if error_count > 0 {
                format!(", {} lỗi", error_count)
            } else {
                String::new()
            },
            unchanged_info,
            deleted_info
        )
    };

    let final_indexed = indexed_count + unchanged_count;
    let _ = state.app_handle.emit(
        "index-progress",
        IndexProgressEvent {
            folder_id: folder_id.to_string(),
            folder_name: folder_name.to_string(),
            phase: phase.to_string(),
            current_file: String::new(),
            current_index: if was_cancelled {
                indexed_count
            } else {
                total_to_index
            },
            total_files: total_to_index,
            indexed_count: final_indexed,
            error_count,
            total_vectors,
            message,
        },
    );

    log::info!(
        "[Index] Done: {}/{} new files indexed ({} chunks), {} unchanged, {} deleted, {} errors",
        indexed_count,
        total_to_index,
        total_chunks,
        unchanged_count,
        deleted_files.len(),
        error_count
    );

    Ok((total_files, final_indexed))
}

/// Walk directory recursively, return (path, modified_timestamp) for supported files
/// Chỉ quét file mà parser thực sự hỗ trợ, skip thư mục rác
/// Walk directory recursively, return (path, modified_timestamp) for supported files
/// Chỉ quét file mà parser thực sự hỗ trợ, skip thư mục rác
pub fn walk_supported_files_pub(
    dir: &std::path::Path,
) -> Result<Vec<(String, i64)>, std::io::Error> {
    walk_supported_files(dir)
}

fn walk_supported_files(dir: &std::path::Path) -> Result<Vec<(String, i64)>, std::io::Error> {
    let mut results = Vec::new();
    walk_recursive(dir, &mut results, 0, MAX_DEPTH)?;
    log::info!(
        "[Walker] Found {} supported files (skipped system dirs)",
        results.len()
    );
    Ok(results)
}

/// Max recursion depth (tránh quét quá sâu)
const MAX_DEPTH: usize = 10;

/// Min file size (skip empty/tiny files)
const MIN_FILE_SIZE: u64 = 10;

/// Max file size (skip files > 50MB)
const MAX_FILE_SIZE: u64 = 50 * 1024 * 1024;

/// Thư mục hệ thống/build — skip hoàn toàn
const SKIP_DIRS: &[&str] = &[
    "node_modules",
    "__pycache__",
    ".git",
    ".svn",
    ".hg",
    ".vscode",
    ".idea",
    ".DS_Store",
    "target",
    "build",
    "dist",
    ".next",
    ".nuxt",
    "vendor",
    "venv",
    ".venv",
    "env",
    ".env",
    "cache",
    ".cache",
    "tmp",
    ".tmp",
    "logs",
    "log",
    ".Trash",
    ".Spotlight-V100",
    ".fseventsd",
    ".TemporaryItems",
    "Library",
    "$RECYCLE.BIN",
    "System Volume Information",
];

fn walk_recursive(
    dir: &std::path::Path,
    results: &mut Vec<(String, i64)>,
    depth: usize,
    max_depth: usize,
) -> Result<(), std::io::Error> {
    if depth > max_depth || !dir.is_dir() {
        return Ok(());
    }

    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) => {
            // Permission denied, etc — skip silently
            log::warn!("[Walker] Cannot read {:?}: {}", dir, e);
            return Ok(());
        }
    };

    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        let path = entry.path();

        if path.is_dir() {
            let dir_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");

            // Skip hidden directories
            if dir_name.starts_with('.') {
                continue;
            }

            // Skip system/build directories
            if SKIP_DIRS.contains(&dir_name) {
                continue;
            }

            walk_recursive(&path, results, depth + 1, max_depth)?;
        } else if path.is_file() {
            // Check extension via parser::is_supported
            if !crate::parser::is_supported(&path) {
                continue;
            }

            // Check file size
            let metadata = match path.metadata() {
                Ok(m) => m,
                Err(_) => continue,
            };

            let file_size = metadata.len();
            if file_size < MIN_FILE_SIZE || file_size > MAX_FILE_SIZE {
                continue;
            }

            let modified = metadata
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);

            results.push((path.to_string_lossy().to_string(), modified));
        }
    }
    Ok(())
}

/// DJB2 hash function
pub fn djb2_hash(input: &str) -> u64 {
    let mut hash: u64 = 5381;
    for byte in input.bytes() {
        hash = hash.wrapping_mul(33).wrapping_add(byte as u64);
    }
    hash
}

/// Bắt đầu index tất cả files trong registered folders
#[tauri::command]
pub async fn start_indexing(state: State<'_, AppState>) -> Result<IndexStatus, String> {
    let folders = {
        let db = state.db.lock().await;
        sqlite::folders::list(&db.conn).map_err(|e| format!("Lỗi đọc folders: {}", e))?
    };

    log::info!("[Index] Start indexing {} folders", folders.len());

    // Reset cancel flag
    state.cancel_indexing.store(false, Ordering::Relaxed);

    for (folder_id, folder_path, folder_name) in &folders {
        // Kiểm tra cancel trước mỗi folder
        if state.cancel_indexing.load(Ordering::Relaxed) {
            log::info!("[Index] ⏹ Dừng — bỏ qua folder: {}", folder_name);
            break;
        }

        match index_folder(folder_id, folder_path, folder_name, state.inner()).await {
            Ok((total, indexed)) => {
                log::info!("[Index] {} — {}/{} indexed", folder_name, indexed, total);
            }
            Err(e) => {
                log::error!("[Index] Error indexing {}: {}", folder_name, e);
            }
        }
    }

    // Reset cancel flag khi xong
    state.cancel_indexing.store(false, Ordering::Relaxed);

    get_index_status(state).await
}

/// Dừng indexing — các file đã xử lý vẫn được lưu
#[tauri::command]
pub async fn stop_indexing(state: State<'_, AppState>) -> Result<String, String> {
    state.cancel_indexing.store(true, Ordering::Relaxed);
    log::info!("[Index] ⏹ User yêu cầu dừng indexing");
    Ok("Đang dừng indexing...".to_string())
}

/// Lấy trạng thái indexing hiện tại
#[tauri::command]
pub async fn get_index_status(state: State<'_, AppState>) -> Result<IndexStatus, String> {
    let db = state.db.lock().await;

    // Tự động dọn dẹp orphan data nếu không còn folder nào
    let folders = sqlite::folders::list(&db.conn).unwrap_or_default();
    if folders.is_empty() {
        let (total, _, _) = sqlite::file_tracking::count_by_status(&db.conn).unwrap_or((0, 0, 0));
        if total > 0 {
            log::warn!(
                "[Status] Không có folder nhưng tìm thấy {} record file. Đang dọn dẹp...",
                total
            );
            let _ = sqlite::file_tracking::clear_all_tracking(&db.conn);
            let tantivy_guard = state.tantivy.lock().await;
            if let Some(ref tantivy) = *tantivy_guard {
                let _ = tantivy.clear_index();
            }
        }
    }

    let (total, indexed, errors) = sqlite::file_tracking::count_by_status(&db.conn)
        .map_err(|e| format!("Lỗi đọc status: {}", e))?;

    Ok(IndexStatus {
        total_files: total,
        indexed_files: indexed,
        pending_files: total.saturating_sub(indexed).saturating_sub(errors),
        error_files: errors,
        is_indexing: false,
        current_file: None,
    })
}

/// Auto-retry: Re-embed files that were indexed but missing vectors
/// Gọi khi startup hoặc sau khi fix quota
#[tauri::command]
pub async fn retry_embed_missing(
    state: tauri::State<'_, crate::AppState>,
) -> Result<serde_json::Value, String> {
    use crate::db::sqlite;

    let db = state.db.lock().await;
    let files = sqlite::file_tracking::get_all_files_without_vectors(&db.conn)
        .map_err(|e| format!("DB error: {}", e))?;
    drop(db);

    if files.is_empty() {
        return Ok(serde_json::json!({"retried": 0, "message": "Không có file nào cần retry"}));
    }

    log::info!("[Index] Auto-retry: {} files without vectors", files.len());

    let mut success = 0;
    let mut failed = 0;

    for file_path in &files {
        let path = std::path::Path::new(file_path);
        if !path.exists() {
            log::warn!("[Index] Skip retry — file not found: {}", file_path);
            failed += 1;
            continue;
        }

        // Parse file
        let parsed = match crate::parser::parse_file(path) {
            Ok(p) => p,
            Err(e) => {
                log::error!("[Index] Retry parse failed for {}: {}", file_path, e);
                failed += 1;
                continue;
            }
        };

        // Chunk
        let chunks = if let Some(ref kr_chunks) = parsed.kreuzberg_chunks {
            crate::indexer::chunker::from_kreuzberg_chunks(
                kr_chunks,
                file_path,
                &parsed.file_name,
            )
        } else {
            let chunk_config = crate::indexer::chunker::ChunkConfig::default();
            crate::indexer::chunker::chunk_text(
                &parsed.content,
                file_path,
                &parsed.file_name,
                &chunk_config,
            )
        };

        if chunks.is_empty() {
            continue;
        }

        // Embed
        let cancel = state.cancel_indexing.as_ref();
        match state
            .embedding_pipeline
            .process_chunks(&chunks, Some(cancel), None::<&(dyn Fn(usize) + Send + Sync)>, None)
            .await
        {
            Ok((vectors, metas)) => {
                if !vectors.is_empty() {
                    state.vector_index.remove_file(file_path);
                    state.vector_index.add_vectors(vectors.clone(), metas);

                    if let Err(e) = state.vector_index.save() {
                        log::error!("[Index] Vector save error: {}", e);
                    }

                    let db = state.db.lock().await;
                    let _ = sqlite::file_tracking::update_has_vectors(
                        &db.conn, file_path, true,
                    );

                    success += 1;
                    log::info!(
                        "[Index] ✓ Retry embed OK: {} ({} vectors)",
                        file_path, vectors.len()
                    );
                }
            }
            Err(e) => {
                log::error!("[Index] ❌ Retry embed FAILED: {}: {}", file_path, e);
                if matches!(e, crate::embedding::EmbeddingError::QuotaExhausted(_, _)) {
                    let _ = state.app_handle.emit(
                        "api-quota-exhausted",
                        serde_json::json!({
                            "message": "API quota hết — retry dừng lại",
                        }),
                    );
                    break; // Stop retrying when quota exhausted
                }
                failed += 1;
            }
        }
    }

    log::info!(
        "[Index] Auto-retry done: {}/{} success, {} failed",
        success, files.len(), failed
    );

    Ok(serde_json::json!({
        "retried": success,
        "failed": failed,
        "total": files.len(),
        "message": format!("Retry xong: {}/{} thành công", success, files.len()),
    }))
}

/// Get chunks for a file — chunk visualization UI
#[tauri::command]
pub async fn get_file_chunks(
    file_path: String,
    state: tauri::State<'_, crate::AppState>,
) -> Result<serde_json::Value, String> {
    let chunks = state.vector_index.get_chunks_for_file(&file_path);

    let chunk_data: Vec<serde_json::Value> = chunks
        .iter()
        .enumerate()
        .map(|(i, meta)| {
            let token_est = crate::commands::context_guard::estimate_tokens(&meta.content);
            serde_json::json!({
                "index": i,
                "chunk_id": meta.chunk_id,
                "file_name": meta.file_name,
                "section": meta.section,
                "content": meta.content,
                "char_count": meta.content.len(),
                "token_estimate": token_est,
            })
        })
        .collect();

    Ok(serde_json::json!({
        "file_path": file_path,
        "total_chunks": chunk_data.len(),
        "chunks": chunk_data,
    }))
}
