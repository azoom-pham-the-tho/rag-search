use crate::db::sqlite;
use crate::AppState;
use serde::{Deserialize, Serialize};
use tauri::State;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct FolderInfo {
    pub id: String,
    pub path: String,
    pub name: String,
    pub file_count: usize,
    pub indexed_count: usize,
    pub status: String, // "active" | "indexing" | "error"
}

/// Thêm folder để theo dõi và index
#[tauri::command]
pub async fn add_folder(path: String, state: State<'_, AppState>) -> Result<FolderInfo, String> {
    // Validate folder exists
    let path_buf = std::path::PathBuf::from(&path);
    if !path_buf.exists() || !path_buf.is_dir() {
        return Err("Thư mục không tồn tại".to_string());
    }

    let name = path_buf
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path.clone());

    let id = uuid::Uuid::new_v4().to_string();

    // Save to database
    {
        let db = state.db.lock().await;
        sqlite::folders::insert(&db.conn, &id, &path, &name)
            .map_err(|e| format!("Lỗi lưu database: {}", e))?;
    }

    log::info!("[Folder] Added: {} → {}", name, path);

    // Trigger indexing (emits progress events to frontend)
    let (total, indexed) = super::index::index_folder(&id, &path, &name, state.inner())
        .await
        .unwrap_or_else(|e| {
            log::error!("[Folder] Index error: {}", e);
            (0, 0)
        });

    let folder = FolderInfo {
        id,
        path,
        name,
        file_count: total,
        indexed_count: indexed,
        status: "active".to_string(),
    };

    Ok(folder)
}

/// Xóa folder khỏi danh sách theo dõi + xoá index
#[tauri::command]
pub async fn remove_folder(folder_id: String, state: State<'_, AppState>) -> Result<(), String> {
    // 1. Lấy danh sách file paths trước khi xoá DB
    let file_paths: Vec<String> = {
        let db = state.db.lock().await;
        let known =
            sqlite::file_tracking::get_known_files(&db.conn, &folder_id).unwrap_or_default();
        known.keys().cloned().collect()
    };

    let file_count = file_paths.len();
    log::info!(
        "[Folder] Removing folder {} with {} files",
        folder_id,
        file_count
    );

    // 2. Xoá khỏi Tantivy index — dùng 1 writer cho tất cả (batch)
    {
        let tantivy_guard = state.tantivy.lock().await;
        if let Some(ref tantivy) = *tantivy_guard {
            match tantivy.index.writer::<tantivy::TantivyDocument>(50_000_000) {
                Ok(mut writer) => {
                    for file_path in &file_paths {
                        let term =
                            tantivy::Term::from_field_text(tantivy.field_file_path, file_path);
                        writer.delete_term(term);
                    }
                    if let Err(e) = writer.commit() {
                        log::error!("[Folder] Tantivy commit error: {}", e);
                    } else {
                        log::info!("[Folder] Tantivy: cleared {} files", file_count);
                    }
                }
                Err(e) => {
                    log::error!("[Folder] Could not create Tantivy writer: {}", e);
                    // Fallback: try clear_index if this was the only folder
                    let _ = tantivy.clear_index();
                }
            }
        }
    }

    // 3. Xoá khỏi Vector index (HNSW)
    for file_path in &file_paths {
        state.vector_index.remove_file(file_path);
    }
    // Save vector index to disk
    if file_count > 0 {
        if let Err(e) = state.vector_index.save() {
            log::warn!("[Folder] Vector index save error: {}", e);
        } else {
            log::info!(
                "[Folder] Vector index: removed {} files, {} remaining",
                file_count,
                state.vector_index.len()
            );
        }
    }

    // 4. Xoá DB records
    {
        let db = state.db.lock().await;
        let _ = sqlite::file_tracking::delete_by_folder(&db.conn, &folder_id);
        sqlite::folders::delete(&db.conn, &folder_id)
            .map_err(|e| format!("Lỗi xóa folder: {}", e))?;
    }

    log::info!(
        "[Folder] ✓ Removed folder {} ({} files cleared from all indexes)",
        folder_id,
        file_count
    );
    Ok(())
}

/// Lấy danh sách tất cả folders đang theo dõi
#[tauri::command]
pub async fn list_folders(state: State<'_, AppState>) -> Result<Vec<FolderInfo>, String> {
    let db = state.db.lock().await;

    let rows = sqlite::folders::list(&db.conn).map_err(|e| format!("Lỗi đọc folders: {}", e))?;

    let mut folders = Vec::new();
    for (id, path, name) in rows {
        let file_count = sqlite::file_tracking::count_by_folder(&db.conn, &id).unwrap_or(0);

        folders.push(FolderInfo {
            id,
            path,
            name,
            file_count,
            indexed_count: file_count,
            status: "active".to_string(),
        });
    }

    Ok(folders)
}

/// Lấy danh sách files đã index trong 1 folder
#[derive(Debug, Serialize, Deserialize)]
pub struct IndexedFileInfo {
    pub file_path: String,
    pub file_name: String,
    pub file_size: i64,
    pub chunk_count: i64,
    pub status: String,
    pub file_type: String,
}

#[tauri::command]
pub async fn get_folder_files(
    folder_id: String,
    state: State<'_, AppState>,
) -> Result<Vec<IndexedFileInfo>, String> {
    let db = state.db.lock().await;
    let rows = sqlite::file_tracking::list_files_by_folder(&db.conn, &folder_id)
        .map_err(|e| format!("Lỗi đọc files: {}", e))?;

    let files: Vec<IndexedFileInfo> = rows
        .into_iter()
        .map(|(path, size, chunks, status)| {
            let file_name = std::path::Path::new(&path)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| path.clone());

            let ext = std::path::Path::new(&path)
                .extension()
                .map(|e| e.to_string_lossy().to_lowercase())
                .unwrap_or_default();

            let file_type = match ext.as_str() {
                "pdf" => "pdf",
                "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" => "image",
                "xlsx" | "xls" => "excel",
                "csv" => "csv",
                "doc" | "docx" => "word",
                "html" | "htm" => "html",
                "txt" | "md" => "text",
                "json" => "json",
                _ => "other",
            }
            .to_string();

            IndexedFileInfo {
                file_path: path,
                file_name,
                file_size: size,
                chunk_count: chunks,
                status,
                file_type,
            }
        })
        .collect();

    Ok(files)
}

/// Lấy ALL indexed files từ tất cả folders (cho file picker trong chat)
#[tauri::command]
pub async fn list_all_indexed_files(
    state: State<'_, AppState>,
) -> Result<Vec<IndexedFileInfo>, String> {
    let db = state.db.lock().await;
    let folders = sqlite::folders::list(&db.conn).map_err(|e| format!("Lỗi đọc folders: {}", e))?;

    let mut all_files = Vec::new();
    for (folder_id, _path, _name) in folders {
        let rows =
            sqlite::file_tracking::list_files_by_folder(&db.conn, &folder_id).unwrap_or_default();
        for (path, size, chunks, status) in rows {
            let file_name = std::path::Path::new(&path)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| path.clone());
            let ext = std::path::Path::new(&path)
                .extension()
                .map(|e| e.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            let file_type = match ext.as_str() {
                "pdf" => "pdf",
                "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" => "image",
                "xlsx" | "xls" => "excel",
                "csv" => "csv",
                "doc" | "docx" => "word",
                "html" | "htm" => "html",
                "txt" | "md" => "text",
                "json" => "json",
                _ => "other",
            }
            .to_string();
            all_files.push(IndexedFileInfo {
                file_path: path,
                file_name,
                file_size: size,
                chunk_count: chunks,
                status,
                file_type,
            });
        }
    }
    all_files.sort_by(|a, b| a.file_name.to_lowercase().cmp(&b.file_name.to_lowercase()));
    Ok(all_files)
}

/// Re-index mot folder da co — incremental: chi index file moi/thay doi
#[tauri::command]
pub async fn reindex_folder(folder_id: String, state: State<'_, AppState>) -> Result<(), String> {
    let (path, name) = {
        let db = state.db.lock().await;
        let rows = sqlite::folders::list(&db.conn).map_err(|e| format!("Lỗi đọc folder: {}", e))?;
        rows.into_iter()
            .find(|(id, _, _)| id == &folder_id)
            .map(|(_, path, name)| (path, name))
            .ok_or_else(|| format!("Không tìm thấy folder: {}", folder_id))?
    };

    log::info!("[Folder] Re-indexing (incremental): {} → {}", name, path);

    super::index::index_folder(&folder_id, &path, &name, state.inner())
        .await
        .map(|_| ())
        .map_err(|e| format!("Lỗi re-index: {}", e))
}

/// Kiem tra thay doi trong folder — khong index, chi tra ve so luong
#[derive(Debug, Serialize)]
pub struct FolderChanges {
    pub new_files: usize,
    pub changed_files: usize,
    pub deleted_files: usize,
    pub unchanged_files: usize,
    pub has_changes: bool,
    pub new_file_names: Vec<String>,
    pub changed_file_names: Vec<String>,
    pub deleted_file_names: Vec<String>,
}

#[tauri::command]
pub async fn check_folder_changes(
    folder_id: String,
    state: State<'_, AppState>,
) -> Result<FolderChanges, String> {
    let path = {
        let db = state.db.lock().await;
        let rows = sqlite::folders::list(&db.conn).map_err(|e| format!("Lỗi: {}", e))?;
        rows.into_iter()
            .find(|(id, _, _)| id == &folder_id)
            .map(|(_, path, _)| path)
            .ok_or_else(|| format!("Không tìm thấy folder: {}", folder_id))?
    };

    let dir = std::path::PathBuf::from(&path);
    if !dir.exists() || !dir.is_dir() {
        return Err(format!("Thư mục không tồn tại: {}", path));
    }

    // Scan filesystem
    let files =
        super::index::walk_supported_files_pub(&dir).map_err(|e| format!("Lỗi quét: {}", e))?;

    // Get DB records
    let known_files = {
        let db = state.db.lock().await;
        sqlite::file_tracking::get_known_files(&db.conn, &folder_id).unwrap_or_default()
    };

    let mut new_files = Vec::new();
    let mut changed_files = Vec::new();
    let mut unchanged = 0usize;

    for (file_path, file_modified) in &files {
        let name = std::path::Path::new(file_path)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| file_path.clone());

        match known_files.get(file_path) {
            Some(db_mod) if *db_mod == *file_modified => {
                unchanged += 1;
            }
            Some(_) => {
                changed_files.push(name);
            }
            None => {
                new_files.push(name);
            }
        }
    }

    let current_paths: std::collections::HashSet<&String> = files.iter().map(|(p, _)| p).collect();
    let deleted_files: Vec<String> = known_files
        .keys()
        .filter(|p| !current_paths.contains(p))
        .map(|p| {
            std::path::Path::new(p)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| p.clone())
        })
        .collect();

    let has_changes =
        !new_files.is_empty() || !changed_files.is_empty() || !deleted_files.is_empty();

    log::info!(
        "[Folder] Changes check: {} new, {} changed, {} deleted, {} unchanged",
        new_files.len(),
        changed_files.len(),
        deleted_files.len(),
        unchanged
    );

    Ok(FolderChanges {
        new_files: new_files.len(),
        changed_files: changed_files.len(),
        deleted_files: deleted_files.len(),
        unchanged_files: unchanged,
        has_changes,
        new_file_names: new_files,
        changed_file_names: changed_files,
        deleted_file_names: deleted_files,
    })
}

/// Xóa 1 file khỏi index (Tantivy + Vector + DB)
#[tauri::command]
pub async fn delete_indexed_file(
    file_path: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    log::info!("[Folder] Deleting indexed file: {}", file_path);

    // 1. Xóa khỏi Tantivy
    {
        let tantivy_guard = state.tantivy.lock().await;
        if let Some(ref tantivy) = *tantivy_guard {
            let _ = tantivy.delete_file_chunks(&file_path);
        }
    }

    // 2. Xóa khỏi Vector index
    state.vector_index.remove_file(&file_path);
    let _ = state.vector_index.save();

    // 3. Xóa khỏi DB
    {
        let db = state.db.lock().await;
        let _ = sqlite::file_tracking::delete_by_path(&db.conn, &file_path);
    }

    log::info!("[Folder] ✓ Deleted file from all indexes: {}", file_path);
    Ok(())
}

/// Quét folder filesystem → trả về tất cả files kèm sync status
#[derive(Debug, Serialize)]
pub struct ScannedFileInfo {
    pub file_path: String,
    pub file_name: String,
    pub file_size: i64,
    pub file_type: String,
    pub sync_status: String, // "synced" | "new" | "changed"
    pub chunk_count: i64,
}

#[tauri::command]
pub async fn scan_folder_files(
    folder_id: String,
    state: State<'_, AppState>,
) -> Result<Vec<ScannedFileInfo>, String> {
    let path = {
        let db = state.db.lock().await;
        let rows = sqlite::folders::list(&db.conn).map_err(|e| format!("Lỗi: {}", e))?;
        rows.into_iter()
            .find(|(id, _, _)| id == &folder_id)
            .map(|(_, path, _)| path)
            .ok_or_else(|| format!("Không tìm thấy folder: {}", folder_id))?
    };

    let dir = std::path::PathBuf::from(&path);
    if !dir.exists() || !dir.is_dir() {
        return Err(format!("Thư mục không tồn tại: {}", path));
    }

    let files = super::index::walk_supported_files_pub(&dir)
        .map_err(|e| format!("Lỗi quét: {}", e))?;

    let (known_files, file_chunks) = {
        let db = state.db.lock().await;
        let known = sqlite::file_tracking::get_known_files(&db.conn, &folder_id)
            .unwrap_or_default();
        let chunks_map: std::collections::HashMap<String, i64> =
            sqlite::file_tracking::list_files_by_folder(&db.conn, &folder_id)
                .unwrap_or_default()
                .into_iter()
                .map(|(path, _size, chunks, _status)| (path, chunks))
                .collect();
        (known, chunks_map)
    };

    let mut result: Vec<ScannedFileInfo> = Vec::new();

    for (file_path, file_modified) in &files {
        let name = std::path::Path::new(file_path)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| file_path.clone());

        let file_size = std::fs::metadata(file_path)
            .map(|m| m.len() as i64)
            .unwrap_or(0);

        let ext = std::path::Path::new(file_path)
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default();

        let file_type = match ext.as_str() {
            "pdf" => "pdf",
            "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" => "image",
            "xlsx" | "xls" => "excel",
            "csv" => "csv",
            "doc" | "docx" => "word",
            "html" | "htm" => "html",
            "txt" | "md" => "text",
            "json" => "json",
            _ => "other",
        }
        .to_string();

        let sync_status = match known_files.get(file_path) {
            Some(db_mod) if *db_mod == *file_modified => "synced",
            Some(_) => "changed",
            None => "new",
        }
        .to_string();

        let chunk_count = file_chunks.get(file_path).copied().unwrap_or(0);

        result.push(ScannedFileInfo {
            file_path: file_path.clone(),
            file_name: name,
            file_size,
            file_type,
            sync_status,
            chunk_count,
        });
    }

    // Sort: unsynced first, then by name
    result.sort_by(|a, b| {
        let order = |s: &str| match s {
            "new" => 0,
            "changed" => 1,
            _ => 2,
        };
        order(&a.sync_status)
            .cmp(&order(&b.sync_status))
            .then_with(|| a.file_name.to_lowercase().cmp(&b.file_name.to_lowercase()))
    });

    log::info!(
        "[Folder] Scan {}: {} total, {} synced, {} new/changed",
        folder_id,
        result.len(),
        result.iter().filter(|f| f.sync_status == "synced").count(),
        result.iter().filter(|f| f.sync_status != "synced").count(),
    );

    Ok(result)
}

/// Index 1 file — parse + chunk + embed + save
#[tauri::command]
pub async fn index_single_file(
    file_path: String,
    folder_id: String,
    state: State<'_, AppState>,
) -> Result<serde_json::Value, String> {
    use crate::indexer::chunker::{self, ChunkConfig};
    use tauri::Emitter;

    let path = std::path::Path::new(&file_path);
    if !path.exists() {
        return Err(format!("File không tồn tại: {}", file_path));
    }

    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| file_path.clone());

    log::info!("[Index] Single file: {}", file_name);

    // Helper: emit progress for single file
    let emit = |phase: &str, detail: &str| {
        let _ = state.app_handle.emit(
            "index-single-progress",
            serde_json::json!({
                "phase": phase,
                "file_path": file_path,
                "file_name": file_name,
                "detail": detail,
            }),
        );
    };

    // 1. Parse (sync blocking)
    emit("parsing", &format!("📄 Đang đọc {}...", file_name));
    let fp = path.to_path_buf();
    let parsed = tokio::task::spawn_blocking(move || crate::parser::parse_file(&fp))
        .await
        .map_err(|e| format!("Thread error: {}", e))?
        .map_err(|e| format!("Lỗi parse: {}", e))?;

    if parsed.content.trim().is_empty() {
        return Err("File rỗng hoặc không thể đọc".to_string());
    }

    // 2. Chunk
    emit("chunking", &format!("✂️ Đang chia chunks {}...", file_name));
    let chunk_config = ChunkConfig::default();
    let chunks = if let Some(ref kr_chunks) = parsed.kreuzberg_chunks {
        chunker::from_kreuzberg_chunks(kr_chunks, &file_path, &parsed.file_name)
    } else {
        chunker::chunk_text(&parsed.content, &file_path, &parsed.file_name, &chunk_config)
    };

    let chunk_count = chunks.len();
    if chunk_count == 0 {
        return Err("Không tạo được chunks từ file".to_string());
    }

    emit(
        "chunked",
        &format!("✂️ {} chunks từ {}", chunk_count, file_name),
    );

    // 3. Delete old data
    {
        let tg = state.tantivy.lock().await;
        if let Some(ref t) = *tg {
            let _ = t.delete_file_chunks(&file_path);
        }
    }
    state.vector_index.remove_file(&file_path);

    // 4. Add to Tantivy
    emit(
        "indexing",
        &format!("📝 Đang lưu {} chunks...", chunk_count),
    );
    {
        let tg = state.tantivy.lock().await;
        if let Some(ref t) = *tg {
            if let Err(e) = t.add_chunks(&chunks, &folder_id) {
                log::error!("[Index] Tantivy error: {}", e);
            }
        }
    }

    // 5. Embed — with batch progress callback
    let mut has_vectors = false;
    let mut rate_limited = false;
    emit(
        "embedding",
        &format!("🧠 0/{} chunks (0%)", chunk_count),
    );

    // Clone app_handle for progress callback
    let app_handle = state.app_handle.clone();
    let fp_clone = file_path.clone();
    let fn_clone = file_name.clone();
    // ★ embed_batch callback truyền all_embeddings.len() = số vectors đã xong
    let batch_cb = move |vectors_done: usize| {
        let chunks_done = std::cmp::min(vectors_done, chunk_count);
        let pct = if chunk_count > 0 {
            std::cmp::min((chunks_done * 100) / chunk_count, 100)
        } else {
            100
        };
        let _ = app_handle.emit(
            "index-single-progress",
            serde_json::json!({
                "phase": "embedding",
                "file_path": fp_clone,
                "file_name": fn_clone,
                "detail": format!("🧠 {}/{} chunks ({}%)", chunks_done, chunk_count, pct),
                "chunks_done": chunks_done,
                "total_chunks": chunk_count,
                "percent": pct,
            }),
        );
    };

    // ★ Quota wait callback — notify frontend when waiting for rate limit
    let app_handle2 = state.app_handle.clone();
    let fp_clone2 = file_path.clone();
    let fn_clone2 = file_name.clone();
    let quota_cb = move |wait_secs: u64| {
        let _ = app_handle2.emit(
            "index-single-progress",
            serde_json::json!({
                "phase": "waiting",
                "file_path": fp_clone2,
                "file_name": fn_clone2,
                "detail": format!("⏳ Hết token, đợi {}s...", wait_secs),
                "wait_seconds": wait_secs,
            }),
        );
    };

    match state
        .embedding_pipeline
        .process_chunks(
            &chunks,
            None,
            Some(&batch_cb),
            Some(&quota_cb),
        )
        .await
    {
        Ok((vectors, metas)) => {
            if !vectors.is_empty() {
                let vec_count = vectors.len();
                state.vector_index.add_vectors(vectors, metas);
                has_vectors = true;
                let _ = state.vector_index.save();
                log::info!("[Index] Embedded {} vectors for: {}", vec_count, file_name);
            }
        }
        Err(e) => {
            let err_msg = format!("{}", e);
            if err_msg.contains("429")
                || err_msg.to_lowercase().contains("quota")
                || err_msg.to_lowercase().contains("rate")
                || matches!(e, crate::embedding::EmbeddingError::QuotaExhausted(_, _))
            {
                rate_limited = true;
                log::warn!("[Index] Rate limited: {}", err_msg);
            } else {
                log::warn!("[Index] Embedding error: {}", err_msg);
            }
        }
    }

    // 6. Save to DB
    emit("saving", "💾 Đang lưu...");
    {
        let file_size = std::fs::metadata(path).map(|m| m.len() as i64).unwrap_or(0);
        let file_modified = std::fs::metadata(path)
            .and_then(|m| m.modified())
            .map(|t| {
                t.duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs() as i64
            })
            .unwrap_or(0);
        let file_hash = format!("{:x}", super::index::djb2_hash(&file_path));
        let db = state.db.lock().await;
        let _ = sqlite::file_tracking::upsert(
            &db.conn,
            &file_path,
            &folder_id,
            &file_hash,
            file_modified,
            file_size,
            chunk_count,
            has_vectors,
        );
    }

    emit(
        "done",
        &format!(
            "✅ {} — {} chunks{}",
            file_name,
            chunk_count,
            if rate_limited {
                " (embed bị giới hạn)"
            } else {
                ""
            }
        ),
    );

    log::info!(
        "[Index] ✅ Single file done: {} ({} chunks, vectors: {})",
        file_name,
        chunk_count,
        has_vectors
    );

    Ok(serde_json::json!({
        "chunk_count": chunk_count,
        "has_vectors": has_vectors,
        "rate_limited": rate_limited,
    }))
}
