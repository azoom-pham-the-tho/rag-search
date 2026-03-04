#![allow(dead_code)]
//! LLM Provider Trait — Tính trừu tượng để dễ swap model
//!
//! Tạo trait `LlmProvider` để bọc logic gọi API.
//! Khi nâng cấp model (Gemini → Claude, local LLM), chỉ cần implement trait
//! cho struct mới. Compiler của Rust đảm bảo không phá vỡ logic cũ.

use thiserror::Error;

/// Lỗi chung cho mọi LLM provider
#[derive(Error, Debug)]
pub enum LlmError {
    #[error("API error: {0}")]
    ApiError(String),
    #[error("Network error: {0}")]
    NetworkError(String),
    #[error("Rate limited — thử lại sau {0} giây")]
    RateLimited(u64),
    #[error("API key chưa được cấu hình")]
    NoApiKey,
    #[error("Parse error: {0}")]
    ParseError(String),
}

/// Cấu hình chung cho mọi LLM call
#[derive(Debug, Clone)]
pub struct LlmConfig {
    pub temperature: f32,
    pub max_output_tokens: u32,
    /// Tên model cụ thể (VD: "gemini-2.0-flash", "claude-3.5-sonnet")
    pub model: String,
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            temperature: 0.7,
            max_output_tokens: 8192,
            model: "gemini-2.0-flash".to_string(),
        }
    }
}

/// Trait trừu tượng cho mọi LLM provider
/// Implement trait này cho từng provider (Gemini, Claude, local...)
///
/// # Example
/// ```rust
/// struct GeminiProvider { client: GeminiClient }
///
/// #[async_trait::async_trait]
/// impl LlmProvider for GeminiProvider {
///     async fn generate(&self, prompt: &str, system: Option<&str>, config: &LlmConfig)
///         -> Result<String, LlmError> { ... }
/// }
/// ```
///
/// Khi cần swap model:
/// ```rust
/// let provider: Box<dyn LlmProvider> = Box::new(GeminiProvider::new(key));
/// // Đổi sang Claude:
/// let provider: Box<dyn LlmProvider> = Box::new(ClaudeProvider::new(key));
/// // Code gọi provider.generate() không cần thay đổi!
/// ```
pub trait LlmProvider: Send + Sync {
    /// Generate response (non-streaming)
    fn generate(
        &self,
        prompt: &str,
        system_prompt: Option<&str>,
        config: &LlmConfig,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, LlmError>> + Send + '_>>;

    /// Stream response — gọi callback cho mỗi text chunk
    fn stream(
        &self,
        prompt: &str,
        system_prompt: Option<&str>,
        config: &LlmConfig,
        on_chunk: Box<dyn FnMut(&str) + Send>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, LlmError>> + Send + '_>>;

    /// Embed text → vector
    fn embed(
        &self,
        text: &str,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Vec<f32>, LlmError>> + Send + '_>,
    >;

    /// Generate structured JSON output — type-safe
    fn generate_json(
        &self,
        prompt: &str,
        system_prompt: Option<&str>,
        config: &LlmConfig,
        json_schema: serde_json::Value,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<serde_json::Value, LlmError>> + Send + '_>,
    >;

    /// Kiểm tra provider sẵn sàng (có API key, network OK)
    fn is_ready(&self) -> bool;

    /// Tên provider (VD: "gemini", "claude", "local")
    fn provider_name(&self) -> &str;
}

/// Convert từ GeminiError sang LlmError
impl From<super::gemini::GeminiError> for LlmError {
    fn from(e: super::gemini::GeminiError) -> Self {
        match e {
            super::gemini::GeminiError::ApiError(msg) => LlmError::ApiError(msg),
            super::gemini::GeminiError::NetworkError(e) => LlmError::NetworkError(e.to_string()),
            super::gemini::GeminiError::RateLimited(secs) => LlmError::RateLimited(secs),
            super::gemini::GeminiError::NoApiKey => LlmError::NoApiKey,
            super::gemini::GeminiError::ParseError(msg) => LlmError::ParseError(msg),
        }
    }
}

/// GeminiProvider — Implement LlmProvider cho Gemini API
pub struct GeminiProvider {
    client: super::gemini::GeminiClient,
}

impl GeminiProvider {
    pub fn new(api_key: String) -> Self {
        Self {
            client: super::gemini::GeminiClient::new(api_key),
        }
    }

    /// Access underlying GeminiClient cho advanced features (function calling, etc.)
    pub fn client(&self) -> &super::gemini::GeminiClient {
        &self.client
    }
}

impl LlmProvider for GeminiProvider {
    fn generate(
        &self,
        prompt: &str,
        system_prompt: Option<&str>,
        config: &LlmConfig,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, LlmError>> + Send + '_>>
    {
        let prompt = prompt.to_string();
        let system = system_prompt.map(|s| s.to_string());
        let model = config.model.clone();

        Box::pin(async move {
            let messages = vec![("user".to_string(), prompt)];
            self.client
                .generate_content(&model, &messages, system.as_deref())
                .await
                .map_err(LlmError::from)
        })
    }

    fn stream(
        &self,
        prompt: &str,
        system_prompt: Option<&str>,
        config: &LlmConfig,
        mut on_chunk: Box<dyn FnMut(&str) + Send>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, LlmError>> + Send + '_>>
    {
        let prompt = prompt.to_string();
        let system = system_prompt.map(|s| s.to_string());
        let model = config.model.clone();
        let temperature = config.temperature;

        Box::pin(async move {
            let messages = vec![("user".to_string(), prompt)];
            self.client
                .stream_generate_content(
                    &model,
                    &messages,
                    system.as_deref(),
                    temperature,
                    |chunk| on_chunk(chunk),
                )
                .await
                .map_err(LlmError::from)
        })
    }

    fn embed(
        &self,
        _text: &str,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Vec<f32>, LlmError>> + Send + '_>,
    > {
        // Embedding dùng pipeline riêng (GeminiEmbedding), không qua GeminiClient
        Box::pin(async move {
            Err(LlmError::ApiError(
                "Embedding uses separate pipeline (EmbeddingPipeline), not LlmProvider".to_string(),
            ))
        })
    }

    fn generate_json(
        &self,
        prompt: &str,
        system_prompt: Option<&str>,
        config: &LlmConfig,
        json_schema: serde_json::Value,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<serde_json::Value, LlmError>> + Send + '_>,
    > {
        let prompt = prompt.to_string();
        let system = system_prompt.map(|s| s.to_string());
        let model = config.model.clone();
        let temperature = config.temperature;

        Box::pin(async move {
            self.client
                .generate_json::<serde_json::Value>(
                    &model,
                    &prompt,
                    system.as_deref(),
                    json_schema,
                    temperature,
                )
                .await
                .map_err(LlmError::from)
        })
    }

    fn is_ready(&self) -> bool {
        true // GeminiClient always has key set at construction
    }

    fn provider_name(&self) -> &str {
        "gemini"
    }
}
