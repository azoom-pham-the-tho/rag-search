// RAG Search - Hệ thống tìm kiếm tài liệu thông minh
// Modules
mod ai;
mod commands;
mod db;
mod embedding;
mod indexer;
mod license;
mod parser;
mod search;
mod watcher;

use db::Database;
use embedding::pipeline::EmbeddingPipeline;
use indexer::tantivy_index::TantivyIndex;
use search::vector_index::VectorIndex;
use std::sync::Arc;
use tauri::Manager;
use tokio::sync::Mutex;

/// Pre-search cache — lưu kết quả tìm kiếm trước khi user bấm Enter
pub struct PreSearchCache {
    pub query: String,
    pub keywords: String,
    pub results: Vec<search::hybrid::HybridResult>,
    pub timestamp: std::time::Instant,
}

/// Session context — lưu file đã tìm thấy từ câu hỏi trước trong cuộc hội thoại
/// Giúp follow-up queries tìm TRONG file đã có, thay vì search lại toàn bộ
#[derive(Clone)]
pub struct SessionContext {
    pub session_id: String,
    pub attached_files: Vec<SessionFile>,
    pub keywords: String,
    pub context_text: String, // Full context đã build → reuse cho follow-up
    pub timestamp: std::time::Instant,
}

#[derive(Clone, Debug)]
pub struct SessionFile {
    pub file_path: String,
    pub file_name: String,
}

/// Application state shared across all Tauri commands
pub struct AppState {
    pub db: Arc<Mutex<Database>>,
    pub tantivy: Arc<Mutex<Option<TantivyIndex>>>,
    pub embedding_pipeline: Arc<EmbeddingPipeline>,
    pub vector_index: Arc<VectorIndex>,
    pub pre_search_cache: Arc<Mutex<Option<PreSearchCache>>>,
    pub session_contexts: Arc<Mutex<std::collections::HashMap<String, SessionContext>>>,
    pub app_handle: tauri::AppHandle,
    /// Flag dừng indexing (set bởi stop_indexing command)
    pub cancel_indexing: Arc<std::sync::atomic::AtomicBool>,
    /// Auto-discovery model registry
    pub model_registry: Arc<ai::model_registry::ModelRegistry>,
    /// File watcher cho auto re-index
    pub file_watcher: Arc<Mutex<Option<watcher::FolderWatcher>>>,
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Initialize logger — mặc định INFO cho crate của mình, WARN cho crates ngoài
    env_logger::Builder::new()
        .filter_level(log::LevelFilter::Warn) // default: chỉ warn/error từ deps
        .filter_module("rag_search_lib", log::LevelFilter::Info) // INFO cho code của mình
        .filter_module("rag_search", log::LevelFilter::Info)
        .parse_default_env() // RUST_LOG override nếu người dùng muốn
        .format_timestamp_secs()
        .init();

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .invoke_handler(tauri::generate_handler![
            commands::folder::add_folder,
            commands::folder::remove_folder,
            commands::folder::list_folders,
            commands::folder::get_folder_files,
            commands::folder::list_all_indexed_files,
            commands::folder::reindex_folder,
            commands::folder::check_folder_changes,
            commands::folder::delete_indexed_file,
            commands::folder::scan_folder_files,
            commands::folder::index_single_file,
            commands::index::start_indexing,
            commands::index::stop_indexing,
            commands::index::get_index_status,
            commands::index::retry_embed_missing,
            commands::index::get_file_chunks,
            commands::search::pipeline::search_documents,
            commands::search::pipeline::search_direct,
            commands::search::smart_query::smart_query,
            commands::search::pipeline::chatbot_query,
            commands::search::pipeline::pre_search,
            commands::chat::send_message,
            commands::chat::get_chat_history,
            commands::chat::clear_chat,
            commands::chat::list_chat_sessions,
            commands::chat::delete_chat_session,
            commands::settings::get_settings,
            commands::settings::save_settings,
            commands::settings::validate_api_key,
            commands::settings::list_gemini_models,
            commands::settings::list_available_models,
        ])
        .setup(|app| {
            let app_handle = app.handle().clone();

            // === Setup TESSDATA_PREFIX cho Tesseract OCR ===
            // Release app: dùng tessdata bundled trong resources
            // Dev mode: dùng Homebrew system tessdata
            let tessdata_set = app_handle
                .path()
                .resource_dir()
                .ok()
                .map(|res| {
                    let bundled = res.join("tessdata");
                    if bundled.join("eng.traineddata").exists() {
                        std::env::set_var("TESSDATA_PREFIX", &bundled);
                        log::info!("[App] Tesseract: bundled tessdata at {:?}", bundled);
                        true
                    } else {
                        false
                    }
                })
                .unwrap_or(false);

            if !tessdata_set {
                // Dev fallback: Homebrew
                for p in &[
                    "/opt/homebrew/share/tessdata",
                    "/usr/local/share/tessdata",
                    "/usr/share/tessdata",
                ] {
                    if std::path::Path::new(p).exists() {
                        std::env::set_var("TESSDATA_PREFIX", p);
                        log::info!("[App] Tesseract: system tessdata at {}", p);
                        break;
                    }
                }
            }

            // Get app data directory
            let app_data_dir = app_handle
                .path()
                .app_data_dir()
                .expect("Không thể tìm thư mục dữ liệu");

            // Ensure data directory exists
            std::fs::create_dir_all(&app_data_dir).expect("Không thể tạo thư mục dữ liệu");

            // Initialize database
            let db_path = app_data_dir.join("rag_search.db");
            log::info!("[App] Database initializing at {:?}", db_path);
            let db = Database::new(db_path).expect("Không thể khởi tạo database");

            // Initialize Tantivy index
            let index_dir = app_data_dir.join("tantivy_index");
            std::fs::create_dir_all(&index_dir).expect("Không thể tạo thư mục index");

            let tantivy = match TantivyIndex::new(index_dir.clone()) {
                Ok(idx) => {
                    log::info!("[App] Tantivy index initialized at {:?}", index_dir);
                    Some(idx)
                }
                Err(e) => {
                    log::error!("[App] Failed to initialize Tantivy: {}", e);
                    None
                }
            };

            // Initialize HNSW vector index (load from disk if exists)
            let vector_dir = app_data_dir.join("vector_index");
            let vector_index = match VectorIndex::load(&vector_dir) {
                Ok(vi) => {
                    log::info!("[App] Vector index loaded: {} vectors", vi.len());
                    vi
                }
                Err(e) => {
                    log::warn!("[App] Vector index load failed: {}, creating empty", e);
                    VectorIndex::new(vector_dir)
                }
            };

            // Initialize embedding pipeline (Gemini API) — hỗ trợ nhiều keys
            let api_keys: Vec<String> = {
                // Ưu tiên đọc gemini_api_keys (JSON array)
                let keys_json = crate::db::sqlite::settings::get(&db.conn, "gemini_api_keys")
                    .ok()
                    .flatten();
                if let Some(ref json) = keys_json {
                    serde_json::from_str::<Vec<String>>(json)
                        .unwrap_or_default()
                        .into_iter()
                        .filter(|k| !k.is_empty())
                        .collect()
                } else {
                    // Fallback: đọc gemini_api_key (single key, backward compat)
                    let single = crate::db::sqlite::settings::get(&db.conn, "gemini_api_key")
                        .ok()
                        .flatten()
                        .unwrap_or_default();
                    if single.is_empty() {
                        vec![]
                    } else {
                        vec![single]
                    }
                }
            };
            let has_keys = !api_keys.is_empty();
            let api_keys_for_registry = api_keys.clone();
            let pipeline = EmbeddingPipeline::new(api_keys);
            log::info!(
                "[App] Embedding pipeline initialized, API keys loaded: {}",
                has_keys
            );

            // Initialize model registry
            let model_registry = Arc::new(ai::model_registry::ModelRegistry::new());
            log::info!("[App] ModelRegistry initialized");
            // ── File Watcher: tự động phát hiện thay đổi ──
            let (fw, mut watcher_rx) = match watcher::FolderWatcher::new() {
                Ok((fw, rx)) => {
                    log::info!("[App] File watcher initialized");
                    (Some(fw), Some(rx))
                }
                Err(e) => {
                    log::warn!("[App] File watcher init failed: {} — auto-reindex disabled", e);
                    (None, None)
                }
            };

            let file_watcher = Arc::new(Mutex::new(fw));

            // Create app state
            let state = AppState {
                db: Arc::new(Mutex::new(db)),
                tantivy: Arc::new(Mutex::new(tantivy)),
                embedding_pipeline: Arc::new(pipeline),
                vector_index: Arc::new(vector_index),
                pre_search_cache: Arc::new(Mutex::new(None)),
                session_contexts: Arc::new(Mutex::new(std::collections::HashMap::new())),
                app_handle: app_handle.clone(),
                cancel_indexing: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                model_registry: model_registry.clone(),
                file_watcher: file_watcher.clone(),
            };

            app.manage(state);

            // Spawn async task: load folders to watch + listen for file events
            if let Some(rx) = watcher_rx.take() {
                let fw_clone = file_watcher.clone();
                let db_clone = app.state::<AppState>().db.clone();
                let tantivy_clone = app.state::<AppState>().tantivy.clone();
                let vector_clone = app.state::<AppState>().vector_index.clone();
                // ★ CRITICAL: Watcher tạo EmbeddingPipeline riêng → KHÔNG share Mutex với chat
                // Chat pipeline dùng state.embedding_pipeline (Mutex A)
                // Watcher dùng watcher_embed (Mutex B) → zero contention
                let ah = app_handle.clone();
                tauri::async_runtime::spawn(async move {
                    use tauri::Emitter;
                    use tokio::time::Duration;

                    // Step 0: Create separate embedding pipeline for watcher
                    // (reads API keys from DB — same keys, separate Mutex)
                    let watcher_embed = {
                        let db_guard = db_clone.lock().await;
                        let keys: Vec<String> = crate::db::sqlite::settings::get(&db_guard.conn, "gemini_api_keys")
                            .ok()
                            .flatten()
                            .and_then(|v| serde_json::from_str::<Vec<String>>(&v).ok())
                            .unwrap_or_else(|| {
                                crate::db::sqlite::settings::get(&db_guard.conn, "gemini_api_key")
                                    .ok()
                                    .flatten()
                                    .filter(|k| !k.is_empty())
                                    .map(|k| vec![k])
                                    .unwrap_or_default()
                            });
                        Arc::new(crate::embedding::pipeline::EmbeddingPipeline::new(keys))
                    };
                    log::info!("[Watcher] Created separate embedding pipeline (no lock contention with chat)");

                    // Step 1: Load existing folders and start watching
                    let mut folder_ids_paths: Vec<(String, String)> = Vec::new();
                    {
                        let db_guard = db_clone.lock().await;
                        if let Ok(folders) = crate::db::sqlite::folders::list(&db_guard.conn) {
                            let mut fw_guard = fw_clone.lock().await;
                            if let Some(ref mut watcher) = *fw_guard {
                                for (id, folder_path, name) in &folders {
                                    let path = std::path::PathBuf::from(folder_path);
                                    if path.exists() {
                                        if let Err(e) = watcher.watch_folder(path.clone()) {
                                            log::warn!("[Watcher] Failed to watch {}: {}", name, e);
                                        } else {
                                            log::info!("[Watcher] Watching: {}", name);
                                            folder_ids_paths.push((id.clone(), folder_path.clone()));
                                        }
                                    }
                                }
                            }
                        }
                    }

                    // Step 1.5: Startup diff — detect offline changes
                    if !folder_ids_paths.is_empty() {
                        let db_guard = db_clone.lock().await;
                        let mut total_changes = 0usize;
                        let mut changed_files: Vec<String> = Vec::new();

                        for (folder_id, folder_path) in &folder_ids_paths {
                            if let Ok(known) = crate::db::sqlite::file_tracking::get_known_files(&db_guard.conn, folder_id) {
                                let path = std::path::PathBuf::from(folder_path);
                                let events = watcher::detect_changed_files(&path, &known);
                                if !events.is_empty() {
                                    log::info!(
                                        "[Watcher] Startup diff: {} changes in {}",
                                        events.len(), folder_path
                                    );
                                    for event in &events {
                                        let fp = match event {
                                            watcher::FileEvent::Created(p) => p.display().to_string(),
                                            watcher::FileEvent::Modified(p) => p.display().to_string(),
                                            watcher::FileEvent::Deleted(p) => p.display().to_string(),
                                        };
                                        changed_files.push(fp);
                                    }
                                    total_changes += events.len();
                                }
                            }
                        }

                        if total_changes > 0 {
                            log::info!("[Watcher] Startup: {} file(s) changed while offline", total_changes);
                            let _ = ah.emit("files-changed", serde_json::json!({
                                "count": total_changes,
                                "startup": true,
                                "files": changed_files
                            }));
                        } else {
                            log::info!("[Watcher] Startup: all files up to date");
                        }
                    }

                    // Step 2: Listen for file events with debounce → auto re-index
                    let mut rx = rx;
                    let mut pending: Vec<watcher::FileEvent> = Vec::new();
                    let debounce = Duration::from_secs(2);

                    loop {
                        match tokio::time::timeout(debounce, rx.recv()).await {
                            Ok(Some(event)) => {
                                log::info!("[Watcher] Event: {:?}", event);
                                pending.push(event);
                                continue;
                            }
                            Ok(None) => break,
                            Err(_) => {
                                if pending.is_empty() {
                                    continue;
                                }
                            }
                        }

                        // ── C2: Auto re-index delta ──
                        let count = pending.len();
                        log::info!("[Watcher] Auto re-indexing {} file change(s)...", count);

                        let mut indexed = 0usize;
                        let mut deleted = 0usize;
                        let mut errors = 0usize;

                        for event in &pending {
                            match event {
                                watcher::FileEvent::Deleted(path) => {
                                    let fp = path.display().to_string();
                                    log::info!("[Watcher] Deleting from index: {}", fp);

                                    // Tantivy delete
                                    {
                                        let tg = tantivy_clone.lock().await;
                                        if let Some(ref t) = *tg {
                                            let _ = t.delete_file_chunks(&fp);
                                        }
                                    }
                                    // Vector delete
                                    vector_clone.remove_file(&fp);
                                    // DB delete
                                    {
                                        let db = db_clone.lock().await;
                                        let _ = crate::db::sqlite::file_tracking::delete_by_path(&db.conn, &fp);
                                    }
                                    deleted += 1;
                                }
                                watcher::FileEvent::Created(path)
                                | watcher::FileEvent::Modified(path) => {
                                    let fp = path.display().to_string();
                                    if !crate::parser::is_supported(path) {
                                        log::debug!("[Watcher] Skipping unsupported: {}", fp);
                                        continue;
                                    }
                                    log::info!("[Watcher] Re-indexing: {}", fp);

                                    // Parse + chunk via EventHandler
                                    match watcher::handler::EventHandler::handle_event(event) {
                                        Ok(watcher::handler::ProcessResult::Processed {
                                            file_path,
                                            file_hash,
                                            chunk_count,
                                            chunks,
                                            metadata: _metadata,
                                        }) => {
                                            // Delete old data first (for Modified)
                                            {
                                                let tg = tantivy_clone.lock().await;
                                                if let Some(ref t) = *tg {
                                                    let _ = t.delete_file_chunks(&file_path);
                                                }
                                            }
                                            vector_clone.remove_file(&file_path);

                                            // Determine folder_id from path
                                            let folder_id = {
                                                let mut fid = String::new();
                                                for (id, fp) in &folder_ids_paths {
                                                    if file_path.starts_with(fp) {
                                                        fid = id.clone();
                                                        break;
                                                    }
                                                }
                                                fid
                                            };

                                            if folder_id.is_empty() {
                                                log::warn!("[Watcher] No folder_id for: {}", file_path);
                                                errors += 1;
                                                continue;
                                            }

                                            // Add to Tantivy
                                            {
                                                let tg = tantivy_clone.lock().await;
                                                if let Some(ref t) = *tg {
                                                    if let Err(e) = t.add_chunks(&chunks, &folder_id) {
                                                        log::error!("[Watcher] Tantivy add error: {}", e);
                                                    }
                                                }
                                            }

                                            // Generate embeddings + add to Vector index
                                            let mut has_vectors = false;
                                            match watcher_embed.process_chunks(&chunks, None, None::<&(dyn Fn(usize) + Send + Sync)>, None).await {
                                                Ok((vectors, metas)) => {
                                                    if !vectors.is_empty() {
                                                        let vec_count = vectors.len();
                                                        vector_clone.add_vectors(vectors, metas);
                                                        has_vectors = true;
                                                        log::info!(
                                                            "[Watcher] Embedded {} vectors for: {}",
                                                            vec_count, file_path
                                                        );
                                                    }
                                                }
                                                Err(e) => {
                                                    log::warn!("[Watcher] Embedding error for {}: {}", file_path, e);
                                                    // BM25 still works even without vectors
                                                }
                                            }

                                            // Update DB
                                            {
                                                let file_size = std::fs::metadata(path)
                                                    .map(|m| m.len() as i64)
                                                    .unwrap_or(0);
                                                let file_modified = std::fs::metadata(path)
                                                    .and_then(|m| m.modified())
                                                    .map(|t| t.duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs() as i64)
                                                    .unwrap_or(0);
                                                let db = db_clone.lock().await;
                                                let _ = crate::db::sqlite::file_tracking::upsert(
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

                                            log::info!(
                                                "[Watcher] ✅ Indexed: {} ({} chunks)",
                                                file_path, chunk_count
                                            );
                                            indexed += 1;
                                        }
                                        Ok(watcher::handler::ProcessResult::Deleted(_)) => {
                                            // Shouldn't happen for Created/Modified
                                            deleted += 1;
                                        }
                                        Err(e) => {
                                            log::warn!("[Watcher] Process error: {}", e);
                                            errors += 1;
                                        }
                                    }
                                }
                            }
                        }

                        // Save vector index if we changed anything
                        if indexed > 0 || deleted > 0 {
                            if let Err(e) = vector_clone.save() {
                                log::warn!("[Watcher] Vector save error: {}", e);
                            }
                        }

                        log::info!(
                            "[Watcher] ✅ Auto re-index done: {} indexed, {} deleted, {} errors",
                            indexed, deleted, errors
                        );

                        // Notify UI
                        let file_list: Vec<String> = pending.iter().map(|e| match e {
                            watcher::FileEvent::Created(p) => p.display().to_string(),
                            watcher::FileEvent::Modified(p) => p.display().to_string(),
                            watcher::FileEvent::Deleted(p) => p.display().to_string(),
                        }).collect();

                        let _ = ah.emit("files-changed", serde_json::json!({
                            "count": count,
                            "indexed": indexed,
                            "deleted": deleted,
                            "errors": errors,
                            "files": file_list
                        }));

                        pending.clear();
                    }
                });
            }

            // Background: refresh model list nếu có API key
            if has_keys {
                let registry = model_registry.clone();
                let first_key = api_keys_for_registry.first().cloned().unwrap_or_default();
                tauri::async_runtime::spawn(async move {
                    match registry.refresh(&first_key).await {
                        Ok(()) => log::info!("[App] Model registry refreshed on startup"),
                        Err(e) => log::warn!("[App] Model registry refresh failed: {}", e),
                    }
                });
            }

            log::info!("[App] RAG Search initialized successfully");

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
