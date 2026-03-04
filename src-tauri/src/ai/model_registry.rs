//! Model Registry — Auto-discovery + cache cho Gemini models
//!
//! Tự động fetch danh sách models từ API, phân loại (chat/utility/embedding),
//! và resolve model tốt nhất cho từng use-case.
//! Khi Google ra model mới, app tự cập nhật mà không cần sửa code.

use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::sync::RwLock;
use std::time::{Duration, Instant};

// ═══════════════════════════════════════════════════════
// Types
// ═══════════════════════════════════════════════════════

/// Thông tin 1 model từ API
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    pub display_name: String,
    pub category: ModelCategory,
    /// Version parsed từ tên (VD: "2.5" → 2.5, "2.0" → 2.0)
    pub version: f32,
    /// Variant: "flash", "pro", "lite"
    pub variant: String,
}

/// Phân loại model
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ModelCategory {
    Chat,      // generateContent — cho stream response
    Embedding, // embedContent — cho vector index
}

/// Thông tin embedding model đang active
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActiveEmbedding {
    pub model_id: String,
    pub dimension: usize,
}

/// Defaults — dùng khi chưa fetch được từ API
const DEFAULT_CHAT_MODEL: &str = "gemini-2.0-flash";
const DEFAULT_EMBED_MODEL: &str = "gemini-embedding-001";
const DEFAULT_EMBED_DIM: usize = 768;

/// Cache refresh interval
const CACHE_TTL_SECS: u64 = 86400; // 24h

// ═══════════════════════════════════════════════════════
// ModelRegistry
// ═══════════════════════════════════════════════════════

/// Registry quản lý danh sách models + auto-resolve
pub struct ModelRegistry {
    /// Cache: tất cả models từ API
    models: RwLock<Vec<ModelInfo>>,
    /// Embedding model đang dùng (cần track dimension để safe-upgrade)
    active_embedding: RwLock<ActiveEmbedding>,
    /// Thời điểm cache lần cuối
    last_refresh: RwLock<Option<Instant>>,
    /// HTTP client (reusable)
    client: Client,
}

impl ModelRegistry {
    pub fn new() -> Self {
        Self {
            models: RwLock::new(Vec::new()),
            active_embedding: RwLock::new(ActiveEmbedding {
                model_id: DEFAULT_EMBED_MODEL.to_string(),
                dimension: DEFAULT_EMBED_DIM,
            }),
            last_refresh: RwLock::new(None),
            client: Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .unwrap_or_default(),
        }
    }

    // ─── Refresh: Fetch models từ API ───────────────────

    /// Fetch tất cả models từ Gemini API, phân loại, cache lại
    pub async fn refresh(&self, api_key: &str) -> Result<(), String> {
        if api_key.is_empty() {
            return Err("API key rỗng".to_string());
        }

        let url = format!(
            "https://generativelanguage.googleapis.com/v1beta/models?key={}",
            api_key
        );

        let response = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| format!("Network: {}", e))?;

        let status = response.status();
        let body = response.text().await.map_err(|e| format!("Read: {}", e))?;

        if !status.is_success() {
            return Err(format!("HTTP {}", status));
        }

        let list: ModelsListResponse = serde_json::from_str(&body)
            .map_err(|e| format!("Parse: {}", e))?;

        let mut chat_models: Vec<ModelInfo> = Vec::new();
        let mut embed_models: Vec<ModelInfo> = Vec::new();

        for entry in list.models {
            let id = entry.name.replace("models/", "");
            let methods = entry.supported_generation_methods.as_deref().unwrap_or(&[]);

            if methods.iter().any(|m| m == "embedContent") && is_embedding_model(&id) {
                let (version, variant) = parse_model_version(&id);
                embed_models.push(ModelInfo {
                    display_name: entry.display_name.clone().unwrap_or_else(|| id.clone()),
                    id,
                    category: ModelCategory::Embedding,
                    version,
                    variant,
                });
            } else if methods.iter().any(|m| m == "generateContent") && is_chat_model(&id) {
                let (version, variant) = parse_model_version(&id);
                chat_models.push(ModelInfo {
                    display_name: entry.display_name.clone().unwrap_or_else(|| id.clone()),
                    id,
                    category: ModelCategory::Chat,
                    version,
                    variant,
                });
            }
        }

        // Sort: version DESC, flash > pro
        chat_models.sort_by(|a, b| {
            b.version
                .partial_cmp(&a.version)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| variant_priority(&a.variant).cmp(&variant_priority(&b.variant)))
        });
        embed_models.sort_by(|a, b| {
            b.version
                .partial_cmp(&a.version)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        let total_chat = chat_models.len();
        let total_embed = embed_models.len();

        // Auto-resolve best embedding (cùng dimension)
        if let Some(best_embed) = embed_models.first() {
            let current = self.active_embedding.read().unwrap();
            if best_embed.id != current.model_id {
                log::info!(
                    "[ModelRegistry] New embedding model available: {} (current: {})",
                    best_embed.id,
                    current.model_id
                );
                // Chỉ auto-upgrade nếu dimension compatible
                // (hiện tại tất cả gemini-embedding đều 768, nhưng tương lai có thể thay đổi)
                // → Giữ model cũ, log thông báo
            }
        }

        // Merge vào cache
        let mut all = chat_models;
        all.extend(embed_models);

        log::info!(
            "[ModelRegistry] Refreshed: {} chat models, {} embedding models",
            total_chat,
            total_embed
        );

        if let Some(best) = all.iter().find(|m| m.category == ModelCategory::Chat) {
            log::info!("[ModelRegistry] Best chat: {}", best.id);
        }
        if let Some(best) = all.iter().find(|m| m.category == ModelCategory::Embedding) {
            log::info!("[ModelRegistry] Best embedding: {}", best.id);
        }

        *self.models.write().unwrap() = all;
        *self.last_refresh.write().unwrap() = Some(Instant::now());

        Ok(())
    }

    /// Refresh nếu cache đã hết hạn (> 24h)
    pub async fn refresh_if_stale(&self, api_key: &str) -> Result<(), String> {
        let needs_refresh = {
            let last = self.last_refresh.read().unwrap();
            match *last {
                Some(t) => t.elapsed() > Duration::from_secs(CACHE_TTL_SECS),
                None => true, // chưa bao giờ refresh
            }
        };
        if needs_refresh {
            self.refresh(api_key).await?;
        }
        Ok(())
    }

    // ─── Resolve: Chọn model tốt nhất ──────────────────

    /// Resolve chat model: user setting > auto (best Flash)
    pub fn resolve_chat(&self, user_preference: Option<&str>) -> String {
        // User đã chọn model cụ thể → dùng luôn
        if let Some(pref) = user_preference {
            if !pref.is_empty() && pref != "auto" {
                return pref.to_string();
            }
        }

        // Auto: pick best Flash model
        let models = self.models.read().unwrap();
        models
            .iter()
            .find(|m| m.category == ModelCategory::Chat && m.variant == "flash")
            .or_else(|| models.iter().find(|m| m.category == ModelCategory::Chat))
            .map(|m| m.id.clone())
            .unwrap_or_else(|| DEFAULT_CHAT_MODEL.to_string())
    }

    /// Resolve utility model: LUÔN auto (Flash mới nhất, nhanh + rẻ)
    /// Dùng cho: analyze_query, rewrite_query, query decomposition
    pub fn resolve_utility(&self) -> String {
        let models = self.models.read().unwrap();
        models
            .iter()
            .find(|m| m.category == ModelCategory::Chat && m.variant == "flash")
            .map(|m| m.id.clone())
            .unwrap_or_else(|| DEFAULT_CHAT_MODEL.to_string())
    }

    /// Resolve embedding model: model embedding mới nhất (cùng dimension)
    pub fn resolve_embedding(&self) -> ActiveEmbedding {
        self.active_embedding.read().unwrap().clone()
    }

    /// Set active embedding (khi user chọn re-index với model mới)
    pub fn set_active_embedding(&self, model_id: &str, dimension: usize) {
        let mut active = self.active_embedding.write().unwrap();
        log::info!(
            "[ModelRegistry] Set embedding: {} (dim={})",
            model_id,
            dimension
        );
        active.model_id = model_id.to_string();
        active.dimension = dimension;
    }

    // ─── Listing: Cho frontend ─────────────────────────

    /// Danh sách chat models cho frontend dropdown
    pub fn list_chat_models(&self) -> Vec<ModelInfo> {
        let models = self.models.read().unwrap();
        models
            .iter()
            .filter(|m| m.category == ModelCategory::Chat)
            .cloned()
            .collect()
    }

    /// Danh sách embedding models
    pub fn list_embedding_models(&self) -> Vec<ModelInfo> {
        let models = self.models.read().unwrap();
        models
            .iter()
            .filter(|m| m.category == ModelCategory::Embedding)
            .cloned()
            .collect()
    }

    /// Cache đã có data chưa
    pub fn is_loaded(&self) -> bool {
        !self.models.read().unwrap().is_empty()
    }
}

// ═══════════════════════════════════════════════════════
// Model Filters — Tự động bắt models mới
// ═══════════════════════════════════════════════════════

/// Kiểm tra model có phải chat model không
/// Rule: `gemini-{ver}-{variant}[-preview|-latest]`, bỏ TTS/image/etc.
fn is_chat_model(id: &str) -> bool {
    if !id.starts_with("gemini-") {
        return false;
    }
    // Loại models chuyên biệt
    const EXCLUDE: &[&str] = &[
        "tts",
        "image",
        "robotics",
        "computer",
        "deep-research",
        "nano",
        "banana",
        "embedding",
        "aqa",
        "exp",
        "1206",
        "-lite",
    ];
    for kw in EXCLUDE {
        if id.contains(kw) {
            return false;
        }
    }

    // Max 4 parts: gemini-{ver}-{variant}[-modifier]
    let parts: Vec<&str> = id.split('-').collect();
    if parts.len() > 4 {
        return false;
    }
    let last = *parts.last().unwrap_or(&"");
    matches!(last, "flash" | "pro" | "preview" | "latest")
}

/// Kiểm tra model có phải embedding model không
/// Rule: tên chứa "embedding" HOẶC bắt đầu bằng "text-embedding"
fn is_embedding_model(id: &str) -> bool {
    // gemini-embedding-001, text-embedding-004, etc.
    id.contains("embedding")
}

/// Parse version number từ model name
/// "gemini-2.5-flash-preview" → (2.5, "flash")
/// "gemini-2.0-flash" → (2.0, "flash")
/// "gemini-embedding-001" → (0.01, "embedding")
fn parse_model_version(id: &str) -> (f32, String) {
    let parts: Vec<&str> = id.split('-').collect();

    // Tìm version number
    let mut version = 0.0f32;
    let mut variant = String::new();

    for part in &parts {
        // Try parse as version (1.5, 2.0, 2.5, 3.0, etc.)
        if let Ok(v) = part.parse::<f32>() {
            version = v;
        }
        // Variant
        if matches!(*part, "flash" | "pro" | "lite") {
            variant = part.to_string();
        }
    }

    // Embedding: dùng suffix number
    if variant.is_empty() && id.contains("embedding") {
        variant = "embedding".to_string();
        // "001" → 0.01, "004" → 0.04
        if let Some(last) = parts.last() {
            if let Ok(v) = last.parse::<f32>() {
                version = v / 100.0;
            }
        }
    }

    (version, variant)
}

/// Variant priority: flash = 0 (tốt nhất), pro = 1, khác = 2
fn variant_priority(variant: &str) -> u8 {
    match variant {
        "flash" => 0,
        "pro" => 1,
        _ => 2,
    }
}

// ═══════════════════════════════════════════════════════
// API Response Types
// ═══════════════════════════════════════════════════════

#[derive(Deserialize)]
struct ModelsListResponse {
    models: Vec<ModelEntry>,
}

#[derive(Deserialize)]
struct ModelEntry {
    name: String,
    #[serde(rename = "displayName")]
    display_name: Option<String>,
    #[serde(rename = "supportedGenerationMethods")]
    supported_generation_methods: Option<Vec<String>>,
}
