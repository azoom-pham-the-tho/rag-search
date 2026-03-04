use crate::db::sqlite;
use crate::AppState;
use serde::{Deserialize, Serialize};
use tauri::State;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AppSettings {
    pub gemini_api_key: Option<String>, // backward compat (single key)
    pub gemini_api_keys: Option<Vec<String>>, // nhiều keys xoay vòng
    pub default_model: String,
    pub theme: String,
    pub max_chunks_per_query: usize,
    pub language: String,
    pub min_match_score: f32,
    pub creativity_level: f32,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            gemini_api_key: None,
            gemini_api_keys: None,
            default_model: "gemini-2.0-flash".to_string(),
            theme: "auto".to_string(),
            max_chunks_per_query: 5,
            language: "vi".to_string(),
            min_match_score: 0.60,
            creativity_level: 0.7,
        }
    }
}

/// Lấy cài đặt hiện tại
#[tauri::command]
pub async fn get_settings(state: State<'_, AppState>) -> Result<AppSettings, String> {
    let db = state.db.lock().await;

    let api_key = sqlite::settings::get(&db.conn, "gemini_api_key")
        .map_err(|e| format!("Lỗi đọc settings: {}", e))?;
    let api_keys: Option<Vec<String>> = sqlite::settings::get(&db.conn, "gemini_api_keys")
        .map_err(|e| format!("Lỗi đọc settings: {}", e))?
        .and_then(|json| serde_json::from_str(&json).ok());
    let model = sqlite::settings::get(&db.conn, "default_model")
        .map_err(|e| format!("Lỗi đọc settings: {}", e))?;
    let theme =
        sqlite::settings::get(&db.conn, "theme").map_err(|e| format!("Lỗi đọc settings: {}", e))?;
    let max_chunks = sqlite::settings::get(&db.conn, "max_chunks_per_query")
        .map_err(|e| format!("Lỗi đọc settings: {}", e))?;
    let lang = sqlite::settings::get(&db.conn, "language")
        .map_err(|e| format!("Lỗi đọc settings: {}", e))?;
    let min_score = sqlite::settings::get(&db.conn, "min_match_score")
        .map_err(|e| format!("Lỗi đọc settings: {}", e))?;

    Ok(AppSettings {
        gemini_api_key: api_key,
        gemini_api_keys: api_keys,
        default_model: model.unwrap_or_else(|| "gemini-2.0-flash".to_string()),
        theme: theme.unwrap_or_else(|| "auto".to_string()),
        max_chunks_per_query: max_chunks.and_then(|v| v.parse().ok()).unwrap_or(5),
        language: lang.unwrap_or_else(|| "vi".to_string()),
        min_match_score: min_score.and_then(|v| v.parse().ok()).unwrap_or(0.60),
        creativity_level: {
            let cl = sqlite::settings::get(&db.conn, "creativity_level")
                .map_err(|e| format!("Lỗi đọc settings: {}", e))?;
            cl.and_then(|v| v.parse().ok()).unwrap_or(0.7)
        },
    })
}

/// Lưu cài đặt
#[tauri::command]
pub async fn save_settings(
    settings: AppSettings,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let db = state.db.lock().await;

    // Save API keys (both single + multi for backward compat)
    if let Some(ref keys) = settings.gemini_api_keys {
        let keys_clean: Vec<&String> = keys.iter().filter(|k| !k.is_empty()).collect();
        let json = serde_json::to_string(&keys_clean).unwrap_or_default();
        sqlite::settings::set(&db.conn, "gemini_api_keys", &json)
            .map_err(|e| format!("Lỗi lưu: {}", e))?;
        // Also save first key as gemini_api_key (backward compat for chat AI)
        if let Some(first) = keys_clean.first() {
            sqlite::settings::set(&db.conn, "gemini_api_key", first)
                .map_err(|e| format!("Lỗi lưu: {}", e))?;
        }
    } else if let Some(ref key) = settings.gemini_api_key {
        sqlite::settings::set(&db.conn, "gemini_api_key", key)
            .map_err(|e| format!("Lỗi lưu: {}", e))?;
        // Also save as array
        let json = serde_json::to_string(&vec![key]).unwrap_or_default();
        sqlite::settings::set(&db.conn, "gemini_api_keys", &json)
            .map_err(|e| format!("Lỗi lưu: {}", e))?;
    } else {
        let _ = sqlite::settings::delete(&db.conn, "gemini_api_key");
        let _ = sqlite::settings::delete(&db.conn, "gemini_api_keys");
    }

    sqlite::settings::set(&db.conn, "default_model", &settings.default_model)
        .map_err(|e| format!("Lỗi lưu: {}", e))?;
    sqlite::settings::set(&db.conn, "theme", &settings.theme)
        .map_err(|e| format!("Lỗi lưu: {}", e))?;

    sqlite::settings::set(
        &db.conn,
        "max_chunks_per_query",
        &settings.max_chunks_per_query.to_string(),
    )
    .map_err(|e| format!("Lỗi lưu: {}", e))?;
    sqlite::settings::set(&db.conn, "language", &settings.language)
        .map_err(|e| format!("Lỗi lưu: {}", e))?;
    sqlite::settings::set(
        &db.conn,
        "min_match_score",
        &settings.min_match_score.to_string(),
    )
    .map_err(|e| format!("Lỗi lưu: {}", e))?;
    sqlite::settings::set(
        &db.conn,
        "creativity_level",
        &settings.creativity_level.to_string(),
    )
    .map_err(|e| format!("Lỗi lưu: {}", e))?;

    // Drop DB lock trước khi gọi async pipeline
    drop(db);

    // Sync API keys vào EmbeddingPipeline
    let keys: Vec<String> = settings
        .gemini_api_keys
        .clone()
        .or_else(|| settings.gemini_api_key.clone().map(|k| vec![k]))
        .unwrap_or_default()
        .into_iter()
        .filter(|k| !k.is_empty())
        .collect();
    state.embedding_pipeline.update_api_keys(keys).await;

    log::info!(
        "[Settings] Saved settings: model={}, theme={}",
        settings.default_model,
        settings.theme
    );
    Ok(())
}

/// Kiểm tra API key có hợp lệ không (dùng models.list — nhanh)
#[tauri::command]
pub async fn validate_api_key(api_key: String) -> Result<bool, String> {
    if api_key.is_empty() {
        return Ok(false);
    }

    let masked = if api_key.len() > 8 {
        format!("{}...", &api_key[..8])
    } else {
        "***".to_string()
    };
    log::info!("[Settings] Validating API key: {}", masked);

    let client = crate::ai::gemini::GeminiClient::new(api_key);
    match client.validate_key().await {
        Ok(valid) => Ok(valid),
        Err(e) => {
            log::warn!("[Settings] API key validation error: {}", e);
            Err(format!("Lỗi kiểm tra: {}", e))
        }
    }
}

/// Lấy danh sách models từ Gemini API
#[tauri::command]
pub async fn list_gemini_models(
    state: State<'_, AppState>,
) -> Result<Vec<crate::ai::gemini::GeminiModel>, String> {
    let db = state.db.lock().await;
    let api_key = sqlite::settings::get(&db.conn, "gemini_api_key")
        .map_err(|e| format!("Lỗi đọc settings: {}", e))?
        .ok_or("Chưa cấu hình API key")?;
    drop(db);

    let client = crate::ai::gemini::GeminiClient::new(api_key);
    match client.list_models().await {
        Ok(models) => {
            log::info!("[Settings] Found {} Gemini models", models.len());
            Ok(models)
        }
        Err(e) => Err(format!("Lỗi lấy models: {}", e)),
    }
}

/// Lấy models còn quota — probe nhanh từng model song song
#[tauri::command]
pub async fn list_available_models(
    state: State<'_, AppState>,
) -> Result<Vec<crate::ai::gemini::GeminiModel>, String> {
    let db = state.db.lock().await;
    let api_key = sqlite::settings::get(&db.conn, "gemini_api_key")
        .map_err(|e| format!("Lỗi đọc settings: {}", e))?
        .ok_or("Chưa cấu hình API key")?;
    drop(db);

    let client = crate::ai::gemini::GeminiClient::new(api_key.clone());

    // Bước 1: Lấy danh sách models
    let models = client
        .list_models()
        .await
        .map_err(|e| format!("Lỗi lấy models: {}", e))?;

    // Bước 2: Probe song song tất cả models (timeout 4s mỗi model)
    log::info!("[Settings] Probing {} models for quota...", models.len());

    let probe_futures: Vec<_> = models
        .iter()
        .map(|m| {
            let client = crate::ai::gemini::GeminiClient::new(api_key.clone());
            let model_id = m.id.clone();
            async move {
                let ok = tokio::time::timeout(
                    std::time::Duration::from_secs(4),
                    client.probe_model_quota(&model_id),
                )
                .await
                .unwrap_or(false);
                (model_id, ok)
            }
        })
        .collect();

    let probe_results: Vec<(String, bool)> = futures_util::future::join_all(probe_futures).await;

    let available: Vec<crate::ai::gemini::GeminiModel> = models
        .into_iter()
        .filter(|m| {
            probe_results
                .iter()
                .find(|(id, _)| id == &m.id)
                .map(|(_, ok)| *ok)
                .unwrap_or(false)
        })
        .collect();

    log::info!(
        "[Settings] Available models: {}/{}",
        available.len(),
        probe_results.len()
    );

    Ok(available)
}
