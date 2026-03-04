use super::EmbeddingError;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

/// Gemini Embedding API Client — dynamic model selection
/// Docs: https://ai.google.dev/gemini-api/docs/embeddings
/// Hỗ trợ NHIỀU API keys — xoay vòng khi bị 429
pub struct GeminiEmbedding {
    client: Client,
    api_keys: Vec<String>,
    current_key: AtomicUsize,
    /// Model đang dùng (configurable từ ModelRegistry)
    model: std::sync::RwLock<String>,
}

// ── Request/Response ─────────────────────────────────

#[derive(Serialize)]
struct EmbedRequest {
    model: String,
    content: EmbedContent,
    #[serde(rename = "taskType")]
    task_type: String,
    /// outputDimensionality: Chỉ định số chiều vector output
    /// MRL models (gemini-embedding-001) hỗ trợ 768/1536/3072
    /// Set Some(768) để giữ tương thích khi đổi model
    #[serde(rename = "outputDimensionality", skip_serializing_if = "Option::is_none")]
    output_dimensionality: Option<u32>,
}

#[derive(Serialize)]
struct EmbedContent {
    parts: Vec<EmbedPart>,
}

#[derive(Serialize)]
struct EmbedPart {
    text: String,
}

#[derive(Serialize)]
struct BatchEmbedRequest {
    requests: Vec<EmbedRequest>,
}

#[derive(Deserialize)]
struct EmbedResponse {
    embedding: Option<EmbedValues>,
}

#[derive(Deserialize)]
struct EmbedValues {
    values: Vec<f32>,
}

#[derive(Deserialize)]
struct BatchEmbedResponse {
    embeddings: Vec<EmbedValues>,
}

// ── Constants ────────────────────────────────────────

/// Model mặc định — dùng khi chưa có ModelRegistry
const DEFAULT_MODEL: &str = "models/gemini-embedding-001";
const BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta";

const TASK_TYPE_DOCUMENT: &str = "RETRIEVAL_DOCUMENT";
const TASK_TYPE_QUERY: &str = "QUESTION_ANSWERING";

const MAX_BATCH_SIZE: usize = 5;
const MAX_TEXT_LENGTH: usize = 6000;
/// Delay giữa batches — nhanh, vì 429 có retry
const BATCH_DELAY_MS: u64 = 300;
/// Khi 429: đợi bao lâu trước khi thử key tiếp theo (ms)
const RATE_LIMIT_WAIT_MS: u64 = 60_000;
/// Max retries tổng (xoay hết keys × rounds)
const MAX_RETRIES: u32 = 20;

// ── Implementation ───────────────────────────────────

impl GeminiEmbedding {
    pub fn new(api_keys: Vec<String>) -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(60))
            .build()
            .unwrap_or_default();
        let count = api_keys.len();
        log::info!(
            "[Embed] Init GeminiEmbedding với {} key(s): {}",
            count,
            api_keys
                .iter()
                .map(|k| mask_key(k))
                .collect::<Vec<_>>()
                .join(", ")
        );
        Self {
            client,
            api_keys,
            current_key: AtomicUsize::new(0),
            model: std::sync::RwLock::new(DEFAULT_MODEL.to_string()),
        }
    }

    pub fn has_key(&self) -> bool {
        self.api_keys.iter().any(|k| !k.is_empty())
    }

    /// Model đang dùng
    #[allow(dead_code)]
    pub fn current_model(&self) -> String {
        self.model.read().unwrap().clone()
    }

    /// Đổi embedding model (từ ModelRegistry auto-resolve)
    #[allow(dead_code)]
    pub fn set_model(&self, model_id: &str) {
        let full = if model_id.starts_with("models/") {
            model_id.to_string()
        } else {
            format!("models/{}", model_id)
        };
        log::info!("[Embed] Set model: {}", full);
        *self.model.write().unwrap() = full;
    }

    /// Probe dimension: embed text ngắn → đếm vector length
    #[allow(dead_code)]
    pub async fn probe_dimension(&self) -> Result<usize, EmbeddingError> {
        let vec = self.embed_single("test", TASK_TYPE_QUERY).await?;
        Ok(vec.len())
    }

    /// Cập nhật danh sách keys
    pub fn set_api_keys(&mut self, keys: Vec<String>) {
        log::info!(
            "[Embed] Update {} key(s): {}",
            keys.len(),
            keys.iter()
                .map(|k| mask_key(k))
                .collect::<Vec<_>>()
                .join(", ")
        );
        self.api_keys = keys;
        self.current_key.store(0, Ordering::Relaxed);
    }

    /// Lấy key hiện tại
    fn get_current_key(&self) -> Option<&str> {
        if self.api_keys.is_empty() {
            return None;
        }
        let idx = self.current_key.load(Ordering::Relaxed) % self.api_keys.len();
        let key = &self.api_keys[idx];
        if key.is_empty() {
            None
        } else {
            Some(key)
        }
    }

    /// Xoay sang key tiếp theo, trả về key mới
    fn rotate_key(&self) -> Option<&str> {
        if self.api_keys.len() <= 1 {
            return self.get_current_key();
        }
        let old = self.current_key.fetch_add(1, Ordering::Relaxed);
        let new_idx = (old + 1) % self.api_keys.len();
        let key = &self.api_keys[new_idx];
        log::info!(
            "[Embed] 🔄 Xoay key: {} → {} (key {})",
            mask_key(&self.api_keys[old % self.api_keys.len()]),
            mask_key(key),
            new_idx + 1
        );
        if key.is_empty() {
            None
        } else {
            Some(key)
        }
    }

    /// Embed single text (search query) — QUESTION_ANSWERING
    pub async fn embed(&self, text: &str) -> Result<Vec<f32>, EmbeddingError> {
        self.embed_single(text, TASK_TYPE_QUERY).await
    }

    /// Internal: single embedContent with key rotation on 429
    async fn embed_single(&self, text: &str, task_type: &str) -> Result<Vec<f32>, EmbeddingError> {
        let api_key = self
            .get_current_key()
            .ok_or_else(|| EmbeddingError::ModelError("Chưa có API key".into()))?;

        let truncated = truncate_text(text);
        if truncated.is_empty() {
            return Err(EmbeddingError::ModelError("Text rỗng".into()));
        }

        let model = self.model.read().unwrap().clone();
        let url = format!("{}/{}:embedContent", BASE_URL, model);
        let request = EmbedRequest {
            model: model,
            content: EmbedContent {
                parts: vec![EmbedPart { text: truncated }],
            },
            task_type: task_type.to_string(),
            output_dimensionality: Some(768),
        };

        let mut current_api_key = api_key.to_string();

        for attempt in 0..MAX_RETRIES {
            let resp = self
                .client
                .post(&url)
                .header("x-goog-api-key", &current_api_key)
                .header("Content-Type", "application/json")
                .json(&request)
                .send()
                .await
                .map_err(|e| EmbeddingError::ModelError(format!("Network: {}", e)))?;

            let status = resp.status();
            let body = resp
                .text()
                .await
                .map_err(|e| EmbeddingError::ModelError(format!("Read: {}", e)))?;

            if status.as_u16() == 429 {
                log::warn!(
                    "[Embed] ⏳ 429 embedContent key={} (lần {}/{})",
                    mask_key(&current_api_key),
                    attempt + 1,
                    MAX_RETRIES
                );
                // Xoay key
                if let Some(next) = self.rotate_key() {
                    current_api_key = next.to_string();
                }
                // Nếu đã xoay hết 1 vòng → đợi 60s
                if self.api_keys.len() > 1 && (attempt + 1) % self.api_keys.len() as u32 != 0 {
                    // Chưa hết vòng → thử key mới ngay
                    continue;
                }
                log::info!("[Embed] Đã thử hết keys, đợi 60s...");
                tokio::time::sleep(Duration::from_millis(RATE_LIMIT_WAIT_MS)).await;
                continue;
            }

            if !status.is_success() {
                log::error!(
                    "[Embed] ❌ HTTP {} embedContent:\n{}",
                    status,
                    &body[..body.len().min(500)]
                );
                return Err(EmbeddingError::ModelError(format!(
                    "HTTP {}: {}",
                    status,
                    &body[..body.len().min(300)]
                )));
            }

            let result: EmbedResponse = serde_json::from_str(&body)
                .map_err(|e| EmbeddingError::ModelError(format!("Parse: {}", e)))?;

            if let Some(emb) = result.embedding {
                return Ok(emb.values);
            }
            return Err(EmbeddingError::ModelError("Thiếu embedding".into()));
        }

        Err(EmbeddingError::QuotaExhausted(
            self.api_keys.len(),
            MAX_RETRIES,
        ))
    }

    /// Batch texts → vectors (indexing) — RETRIEVAL_DOCUMENT
    pub async fn embed_batch(
        &self,
        texts: &[String],
        cancel_flag: Option<&AtomicBool>,
        on_batch_done: Option<&(dyn Fn(usize) + Send + Sync)>,
        on_quota_wait: Option<&(dyn Fn(u64) + Send + Sync)>,
    ) -> Result<Vec<Vec<f32>>, EmbeddingError> {
        if !self.has_key() {
            return Err(EmbeddingError::ModelError("Chưa có API key".into()));
        }
        if texts.is_empty() {
            return Ok(vec![]);
        }

        let mut all_embeddings = Vec::with_capacity(texts.len());
        let batches: Vec<&[String]> = texts.chunks(MAX_BATCH_SIZE).collect();

        log::info!(
            "[Embed] Bắt đầu embed {} texts ({} batches × {}, {} keys)",
            texts.len(),
            batches.len(),
            MAX_BATCH_SIZE,
            self.api_keys.len()
        );

        for (idx, batch) in batches.iter().enumerate() {
            if let Some(flag) = cancel_flag {
                if flag.load(Ordering::Relaxed) {
                    log::info!("[Embed] ⏹ Dừng (batch {}/{})", idx + 1, batches.len());
                    break;
                }
            }

            if idx > 0 {
                tokio::time::sleep(Duration::from_millis(BATCH_DELAY_MS)).await;
            }

            log::info!(
                "[Embed] Batch {}/{}: {} texts",
                idx + 1,
                batches.len(),
                batch.len()
            );

            match self.call_batch_with_retry(batch, cancel_flag, on_quota_wait).await {
                Ok(result) => {
                    log::info!(
                        "[Embed] ✓ Batch {}/{} → {} vectors",
                        idx + 1,
                        batches.len(),
                        result.len()
                    );
                    all_embeddings.extend(result);
                    // ★ Gọi callback sau mỗi batch — để frontend cập nhật progress
                    if let Some(cb) = on_batch_done {
                        cb(all_embeddings.len());
                    }
                }
                Err(e) => {
                    log::error!("[Embed] ❌ Batch {}/{}: {}", idx + 1, batches.len(), e);
                    return Err(e);
                }
            }
        }

        Ok(all_embeddings)
    }

    /// Internal: batchEmbedContents with key rotation on 429
    async fn call_batch_with_retry(
        &self,
        texts: &[String],
        cancel_flag: Option<&AtomicBool>,
        on_quota_wait: Option<&(dyn Fn(u64) + Send + Sync)>,
    ) -> Result<Vec<Vec<f32>>, EmbeddingError> {
        let model = self.model.read().unwrap().clone();
        let url = format!("{}/{}:batchEmbedContents", BASE_URL, model);

        let requests: Vec<EmbedRequest> = texts
            .iter()
            .map(|t| EmbedRequest {
                model: model.clone(),
                content: EmbedContent {
                    parts: vec![EmbedPart {
                        text: truncate_text(t),
                    }],
                },
                task_type: TASK_TYPE_DOCUMENT.to_string(),
                output_dimensionality: Some(768),
            })
            .collect();

        let req_body = BatchEmbedRequest { requests };

        let mut current_api_key = self
            .get_current_key()
            .ok_or_else(|| EmbeddingError::ModelError("Chưa có API key".into()))?
            .to_string();

        for attempt in 0..MAX_RETRIES {
            if let Some(flag) = cancel_flag {
                if flag.load(Ordering::Relaxed) {
                    return Err(EmbeddingError::ModelError("Đã dừng".into()));
                }
            }

            let resp = self
                .client
                .post(&url)
                .header("x-goog-api-key", &current_api_key)
                .header("Content-Type", "application/json")
                .json(&req_body)
                .send()
                .await
                .map_err(|e| EmbeddingError::ModelError(format!("Network: {}", e)))?;

            let status = resp.status();
            let body = resp
                .text()
                .await
                .map_err(|e| EmbeddingError::ModelError(format!("Read: {}", e)))?;

            if status.as_u16() == 429 {
                log::warn!(
                    "[Embed] ⏳ 429 batch key={} (lần {}/{})",
                    mask_key(&current_api_key),
                    attempt + 1,
                    MAX_RETRIES
                );
                // ★ Xoay sang key tiếp theo
                if let Some(next) = self.rotate_key() {
                    current_api_key = next.to_string();
                }
                // Nếu chưa hết vòng keys → thử key mới ngay (không đợi)
                if self.api_keys.len() > 1 && (attempt + 1) % self.api_keys.len() as u32 != 0 {
                    continue;
                }
                // Đã xoay hết 1 vòng → đợi 60s cho quota reset
                log::info!(
                    "[Embed] Đã thử hết {} keys, đợi 60s...",
                    self.api_keys.len()
                );
                // ★ Notify frontend before sleeping
                if let Some(cb) = on_quota_wait {
                    cb(RATE_LIMIT_WAIT_MS / 1000);
                }
                tokio::time::sleep(Duration::from_millis(RATE_LIMIT_WAIT_MS)).await;
                continue;
            }

            if !status.is_success() {
                log::error!(
                    "[Embed] ❌ HTTP {} batch:\n{}",
                    status,
                    &body[..body.len().min(500)]
                );
                return Err(EmbeddingError::ModelError(format!("HTTP {}", status)));
            }

            let result: BatchEmbedResponse = serde_json::from_str(&body)
                .map_err(|e| EmbeddingError::ModelError(format!("Parse: {}", e)))?;

            return Ok(result.embeddings.into_iter().map(|e| e.values).collect());
        }

        Err(EmbeddingError::QuotaExhausted(
            self.api_keys.len(),
            MAX_RETRIES,
        ))
    }

    #[allow(dead_code)]
    pub fn embedding_dim(&self) -> usize {
        768
    }
}

// ── Helpers ──────────────────────────────────────────

fn mask_key(key: &str) -> String {
    if key.len() > 8 {
        format!("{}...{}", &key[..4], &key[key.len() - 4..])
    } else if key.is_empty() {
        "(rỗng)".into()
    } else {
        "***".into()
    }
}

fn truncate_text(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= MAX_TEXT_LENGTH {
        trimmed.to_string()
    } else {
        trimmed.chars().take(MAX_TEXT_LENGTH).collect()
    }
}
