use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum GeminiError {
    #[error("API error: {0}")]
    ApiError(String),
    #[error("Network error: {0}")]
    NetworkError(#[from] reqwest::Error),
    #[error("Rate limited — thử lại sau {0} giây")]
    RateLimited(u64),
    #[error("API key chưa được cấu hình")]
    #[allow(dead_code)]
    NoApiKey,
    #[error("Parse error: {0}")]
    ParseError(String),
}

/// Gemini API Client — chỉ dùng cho chat, KHÔNG embedding
pub struct GeminiClient {
    client: Client,
    api_key: String,
    base_url: String,
}

/// Gemini chat request — hỗ trợ cả text và function calling
#[derive(Serialize, Clone, Debug)]
struct GenerateRequest {
    contents: Vec<Content>,
    #[serde(rename = "systemInstruction", skip_serializing_if = "Option::is_none")]
    system_instruction: Option<Content>,
    #[serde(rename = "generationConfig", skip_serializing_if = "Option::is_none")]
    generation_config: Option<GenerationConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<ToolDeclaration>>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Content {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    pub parts: Vec<Part>,
}

/// Part — có thể là text, inline_data (ảnh/file), function_call, hoặc function_response
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(untagged)]
pub enum Part {
    Text {
        text: String,
    },
    InlineData {
        #[serde(rename = "inlineData")]
        inline_data: InlineDataPart,
    },
    FunctionCall {
        #[serde(rename = "functionCall")]
        function_call: FunctionCallData,
    },
    FunctionResponse {
        #[serde(rename = "functionResponse")]
        function_response: FunctionResponseData,
    },
}

/// Inline image/file data (base64 encoded)
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct InlineDataPart {
    #[serde(rename = "mimeType")]
    pub mime_type: String,
    pub data: String, // base64 encoded
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct FunctionCallData {
    pub name: String,
    pub args: serde_json::Value,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct FunctionResponseData {
    pub name: String,
    pub response: serde_json::Value,
}

impl Part {
    pub fn text(s: &str) -> Self {
        Part::Text {
            text: s.to_string(),
        }
    }

    #[allow(dead_code)]
    pub fn image(mime_type: &str, data_base64: String) -> Self {
        Part::InlineData {
            inline_data: InlineDataPart {
                mime_type: mime_type.to_string(),
                data: data_base64,
            },
        }
    }
}

#[derive(Serialize, Clone, Debug)]
struct GenerationConfig {
    temperature: f32,
    #[serde(rename = "topP")]
    top_p: f32,
    #[serde(rename = "maxOutputTokens")]
    max_output_tokens: u32,
    /// Ép Gemini trả JSON: "application/json"
    #[serde(rename = "responseMimeType", skip_serializing_if = "Option::is_none")]
    response_mime_type: Option<String>,
    /// JSON Schema cho structured output
    #[serde(rename = "responseSchema", skip_serializing_if = "Option::is_none")]
    response_schema: Option<serde_json::Value>,
}

/// Tool declaration cho function calling
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ToolDeclaration {
    #[serde(rename = "functionDeclarations")]
    pub function_declarations: Vec<FunctionDecl>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct FunctionDecl {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

/// Kết quả từ generate_with_tools
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub enum ToolCallResult {
    /// AI muốn gọi tool (VD: search_documents)
    FunctionCall(FunctionCallData),
    /// AI trả lời trực tiếp (không cần tool)
    Text(String),
}

/// Gemini API response — hỗ trợ text + function_call
#[derive(Deserialize, Debug)]
struct GenerateResponse {
    candidates: Option<Vec<Candidate>>,
    error: Option<ApiErrorResponse>,
}

#[derive(Deserialize, Debug)]
struct Candidate {
    content: CandidateContent,
}

#[derive(Deserialize, Clone, Debug)]
struct CandidateContent {
    parts: Vec<PartResponse>,
}

#[derive(Deserialize, Clone, Debug)]
struct PartResponse {
    text: Option<String>,
    #[serde(rename = "functionCall")]
    function_call: Option<FunctionCallData>,
}

#[derive(Deserialize, Debug)]
struct ApiErrorResponse {
    message: String,
    #[allow(dead_code)]
    code: Option<u32>,
}

/// Kết quả phân tích query bởi AI
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryAnalysis {
    pub keywords: String,
    pub intent: String, // "search" | "chat"
}

/// Kết quả AI rewrite query cho RAG
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)]
pub struct QueryRewrite {
    pub action: String, // "search" | "direct" | "reuse" | "clarify"
    pub terms: String,  // BM25 search terms
    #[serde(default)]
    pub message: String, // Clarification question (chỉ dùng khi action=clarify)
}
impl GeminiClient {
    pub fn new(api_key: String) -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(60))
            .connect_timeout(Duration::from_secs(10))
            .pool_idle_timeout(Duration::from_secs(90))
            .build()
            .expect("Failed to create HTTP client");

        Self {
            client,
            api_key,
            base_url: "https://generativelanguage.googleapis.com/v1beta".to_string(),
        }
    }

    // Rate limiting handled by 429 retry logic — no artificial wait needed

    /// Stream chat response từ Gemini — gọi callback cho mỗi text chunk
    pub async fn stream_generate_content<F>(
        &self,
        model: &str,
        messages: &[(String, String)],
        system_prompt: Option<&str>,
        temperature: f32,
        mut on_chunk: F,
    ) -> Result<String, GeminiError>
    where
        F: FnMut(&str),
    {
        let url = format!(
            "{}/models/{}:streamGenerateContent?alt=sse&key={}",
            self.base_url, model, self.api_key
        );

        let contents: Vec<Content> = messages
            .iter()
            .map(|(role, text)| {
                let gemini_role = if role == "assistant" {
                    "model".to_string()
                } else {
                    role.clone()
                };
                Content {
                    role: Some(gemini_role),
                    parts: vec![Part::text(&text)],
                }
            })
            .collect();

        let system_instruction = system_prompt.map(|prompt| Content {
            role: None,
            parts: vec![Part::text(prompt)],
        });

        let request = GenerateRequest {
            contents,
            system_instruction,
            generation_config: Some(GenerationConfig {
                temperature,
                top_p: 0.9,
                max_output_tokens: 65536, // Max output — model tu dung khi xong
                response_mime_type: None,
                response_schema: None,
            }),
            tools: None,
        };

        let mut last_error = GeminiError::ApiError("Unknown error".to_string());
        // Chỉ retry khi lỗi NETWORK (timeout, connection refused), không retry API errors
        let max_retries = 2;
        log::info!(
            "[Gemini Stream] ▶ Starting: model={} url_prefix={} key_prefix={}...",
            model,
            &url[..url.find('?').unwrap_or(url.len())],
            &self.api_key[..self.api_key.len().min(8)]
        );

        for attempt in 0..max_retries {
            if attempt > 0 {
                let delay = Duration::from_secs(2);
                log::info!(
                    "[Gemini Stream] Network retry {} after {}s...",
                    attempt,
                    delay.as_secs()
                );
                tokio::time::sleep(delay).await;
            }

            let response = match self.client.post(&url).json(&request).send().await {
                Ok(r) => r,
                Err(e) => {
                    last_error = GeminiError::ApiError(format!("Network: {}", e));
                    log::warn!(
                        "[Gemini Stream] Network error (attempt {}): {}",
                        attempt + 1,
                        e
                    );
                    continue; // retry on network error
                }
            };

            let status = response.status();
            log::info!("[Gemini Stream] HTTP status: {}", status);

            if status.as_u16() == 429 {
                // Extract headers trước khi consume response
                let retry_after = response
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("N/A")
                    .to_owned(); // owned String để tránh borrow issue
                let body = response.text().await.unwrap_or_default();
                log::error!(
                    "[Gemini Stream] ❌ 429 Rate Limited!\n  retry-after: {}\n  Full body: {}",
                    retry_after,
                    body
                );
                return Err(GeminiError::RateLimited(60));
            }
            if status.is_server_error() {
                let body = response.text().await.unwrap_or_default();
                log::error!("[Gemini Stream] ❌ Server error {}:\n  {}", status, body);
                last_error = GeminiError::ApiError(format!("HTTP {}", status));
                continue;
            }
            if !status.is_success() {
                let body = response.text().await.unwrap_or_default();
                log::error!("[Gemini Stream] ❌ Client error {}:\n  {}", status, body);
                return Err(GeminiError::ApiError(format!(
                    "HTTP {}: {}",
                    status,
                    &body[..body.len().min(300)]
                )));
            }

            // Stream SSE chunks with 30s timeout
            let mut full_text = String::new();
            let mut stream = response.bytes_stream();
            let mut buffer = String::new();
            let stream_start = std::time::Instant::now();

            use futures_util::StreamExt;
            while let Some(chunk_result) = stream.next().await {
                // Timeout: 30s max cho toàn bộ stream
                if stream_start.elapsed().as_secs() > 30 {
                    log::warn!("[Gemini Stream] Stream timeout (30s), returning partial");
                    break;
                }

                let chunk = match chunk_result {
                    Ok(c) => c,
                    Err(e) => {
                        log::error!("[Gemini Stream] Chunk error: {}", e);
                        break;
                    }
                };

                buffer.push_str(&String::from_utf8_lossy(&chunk));

                // Parse SSE data lines
                while let Some(data_start) = buffer.find("data: ") {
                    let data_content = &buffer[data_start + 6..];

                    // Find end of this JSON object
                    if let Some(end_pos) = find_json_end(data_content) {
                        let json_str = &data_content[..end_pos];

                        // Parse the JSON chunk
                        if let Ok(gen_resp) = serde_json::from_str::<GenerateResponse>(json_str) {
                            if let Some(candidates) = gen_resp.candidates {
                                for candidate in candidates {
                                    for part in candidate.content.parts {
                                        if let Some(text) = part.text {
                                            on_chunk(&text);
                                            full_text.push_str(&text);
                                        }
                                    }
                                }
                            }
                        }

                        buffer = buffer[data_start + 6 + end_pos..].to_string();
                    } else {
                        break;
                    }
                }
            }

            log::info!(
                "[Gemini Stream] Done: {} chars in {}ms",
                full_text.len(),
                stream_start.elapsed().as_millis()
            );
            if !full_text.is_empty() {
                return Ok(full_text);
            }

            log::warn!(
                "[Gemini Stream] ⚠️ Stream returned 0 chars (attempt {})",
                attempt + 1
            );
            last_error = GeminiError::ParseError("Stream kết thúc nhưng không có text".to_string());
        }

        log::error!(
            "[Gemini Stream] ❌ All {} attempts failed: {}",
            max_retries,
            last_error
        );
        Err(last_error)
    }

    /// Gửi chat request đến Gemini
    #[allow(dead_code)]
    pub async fn generate_content(
        &self,
        model: &str,
        messages: &[(String, String)], // (role, text) pairs
        system_prompt: Option<&str>,
    ) -> Result<String, GeminiError> {
        let url = format!(
            "{}/models/{}:generateContent?key={}",
            self.base_url, model, self.api_key
        );

        let contents: Vec<Content> = messages
            .iter()
            .map(|(role, text)| {
                let gemini_role = if role == "assistant" {
                    "model".to_string()
                } else {
                    role.clone()
                };
                Content {
                    role: Some(gemini_role),
                    parts: vec![Part::text(&text)],
                }
            })
            .collect();

        let system_instruction = system_prompt.map(|prompt| Content {
            role: None,
            parts: vec![Part::text(prompt)],
        });

        let request = GenerateRequest {
            contents,
            system_instruction,
            generation_config: Some(GenerationConfig {
                temperature: 0.7,
                top_p: 0.9,
                max_output_tokens: 16384,
                response_mime_type: None,
                response_schema: None,
            }),
            tools: None,
        };

        // Retry logic: max 5 attempts with exponential backoff for rate limits
        let mut last_error = GeminiError::ApiError("Unknown error".to_string());
        let max_retries = 5;
        for attempt in 0..max_retries {
            if attempt > 0 {
                let delay = Duration::from_secs(2u64.pow(attempt as u32).min(30));
                log::info!("[Gemini] Retry {} after {}s...", attempt, delay.as_secs());
                tokio::time::sleep(delay).await;
            }

            match self.do_request(&url, &request).await {
                Ok(text) => return Ok(text),
                Err(GeminiError::RateLimited(_)) => {
                    let wait_secs = (10u64 * 2u64.pow(attempt as u32)).min(60);
                    log::warn!(
                        "[Gemini] Rate limited, waiting {}s (attempt {}/{})",
                        wait_secs,
                        attempt + 1,
                        max_retries
                    );
                    tokio::time::sleep(Duration::from_secs(wait_secs)).await;
                    last_error = GeminiError::RateLimited(wait_secs);
                }
                Err(e) => {
                    log::error!(
                        "[Gemini] Error (attempt {}/{}): {}",
                        attempt + 1,
                        max_retries,
                        e
                    );
                    last_error = e;
                }
            }
        }

        Err(last_error)
    }

    async fn do_request(
        &self,
        url: &str,
        request: &GenerateRequest,
    ) -> Result<String, GeminiError> {
        log::info!(
            "[Gemini Request] POST {} key={}...",
            &url[..url.find('?').unwrap_or(url.len())],
            &self.api_key[..self.api_key.len().min(8)]
        );
        let response = self.client.post(url).json(request).send().await?;

        let status = response.status();
        let body = response.text().await?;
        log::info!(
            "[Gemini Request] Status: {}, body_len: {}",
            status,
            body.len()
        );

        if status.as_u16() == 429 {
            log::error!("[Gemini Request] ❌ 429 FULL BODY:\n{}", body);
            return Err(GeminiError::RateLimited(20));
        }

        if !status.is_success() {
            log::error!("[Gemini Request] ❌ {} FULL BODY:\n{}", status, body);
            return Err(GeminiError::ApiError(format!("HTTP {}: {}", status, body)));
        }

        let gen_response: GenerateResponse = serde_json::from_str(&body)
            .map_err(|e| GeminiError::ParseError(format!("Lỗi parse response: {}", e)))?;

        if let Some(error) = gen_response.error {
            log::error!("[Gemini Request] API error in body: {}", error.message);
            return Err(GeminiError::ApiError(error.message));
        }

        let text = gen_response
            .candidates
            .and_then(|c| c.into_iter().next())
            .and_then(|c| c.content.parts.into_iter().next())
            .and_then(|p| p.text)
            .ok_or_else(|| GeminiError::ParseError("Response không có text".to_string()))?;

        Ok(text)
    }

    /// Raw request trả về full candidate parts (hỗ trợ function_call)
    async fn do_request_full(
        &self,
        url: &str,
        request: &GenerateRequest,
    ) -> Result<Vec<PartResponse>, GeminiError> {
        log::info!(
            "[Gemini Full] POST {} key={}...",
            &url[..url.find('?').unwrap_or(url.len())],
            &self.api_key[..self.api_key.len().min(8)]
        );
        let response = self.client.post(url).json(request).send().await?;

        let status = response.status();
        let body = response.text().await?;
        log::info!("[Gemini Full] Status: {}, body_len: {}", status, body.len());

        if status.as_u16() == 429 {
            log::error!("[Gemini Full] ❌ 429 FULL BODY:\n{}", body);
            return Err(GeminiError::RateLimited(20));
        }
        if !status.is_success() {
            log::error!("[Gemini Full] ❌ {} FULL BODY:\n{}", status, body);
            return Err(GeminiError::ApiError(format!("HTTP {}: {}", status, body)));
        }

        let gen_response: GenerateResponse = serde_json::from_str(&body).map_err(|e| {
            GeminiError::ParseError(format!(
                "Lỗi parse response: {}\nBody: {}",
                e,
                &body[..body.len().min(500)]
            ))
        })?;

        if let Some(error) = gen_response.error {
            return Err(GeminiError::ApiError(error.message));
        }

        let parts = gen_response
            .candidates
            .and_then(|c| c.into_iter().next())
            .map(|c| c.content.parts)
            .unwrap_or_default();

        Ok(parts)
    }

    /// AI-First: Gửi query + tools → AI quyết định có gọi tool không
    /// Trả về: (Option<FunctionCallData>, Option<text>)
    /// - Nếu AI gọi tool → trả FunctionCallData (search_documents)
    /// - Nếu AI trả text → trả text trực tiếp (follow-up, không cần search)
    pub async fn generate_with_tools(
        &self,
        model: &str,
        contents: Vec<Content>,
        system_prompt: &str,
        tools: Vec<ToolDeclaration>,
    ) -> Result<ToolCallResult, GeminiError> {
        let url = format!(
            "{}/models/{}:generateContent?key={}",
            self.base_url, model, self.api_key
        );

        let request = GenerateRequest {
            contents,
            system_instruction: Some(Content {
                role: None,
                parts: vec![Part::text(system_prompt)],
            }),
            generation_config: Some(GenerationConfig {
                temperature: 0.1,
                top_p: 0.95,
                max_output_tokens: 50,
                response_mime_type: None,
                response_schema: None,
            }),
            tools: Some(tools),
        };

        let parts = self.do_request_full(&url, &request).await?;

        // Kiểm tra: AI gọi tool hay trả text?
        for part in &parts {
            if let Some(ref fc) = part.function_call {
                log::info!("[Gemini Tools] AI called tool: {}({:?})", fc.name, fc.args);
                return Ok(ToolCallResult::FunctionCall(fc.clone()));
            }
            if let Some(ref text) = part.text {
                log::info!(
                    "[Gemini Tools] AI responded directly: {}...",
                    &text[..text.len().min(80)]
                );
                return Ok(ToolCallResult::Text(text.clone()));
            }
        }

        Err(GeminiError::ParseError(
            "AI không trả function_call lẫn text".to_string(),
        ))
    }

    /// Kiểm tra API key bằng cách gọi models.list (nhanh, nhẹ)
    pub async fn validate_key(&self) -> Result<bool, GeminiError> {
        let url = format!("{}/models?key={}", self.base_url, self.api_key);

        let response = self.client.get(&url).send().await?;

        let status = response.status();
        if status.as_u16() == 400 || status.as_u16() == 403 {
            return Ok(false);
        }
        if !status.is_success() {
            return Err(GeminiError::ApiError(format!("HTTP {}", status)));
        }

        Ok(true)
    }

    /// AI phân tích câu hỏi → trích keywords + phân loại intent
    /// Model truyền từ ngoài (ModelRegistry.resolve_utility())
    pub async fn analyze_query(&self, query: &str, model: &str) -> Result<QueryAnalysis, GeminiError> {
        let system_prompt = r#"Trích keywords + phân loại intent từ câu hỏi tìm kiếm tài liệu.

KEYWORDS: Bỏ từ phụ (tìm/kiếm/cho/tôi/có/là/các/file/tài liệu/giúp...). Giữ thuật ngữ, tên riêng, mã code, số. Thêm từ đồng nghĩa EN nếu cần.
INTENT:
"search"=chỉ muốn LIỆT KÊ/TÌM file, không hỏi nội dung
"chat"=hỏi về NỘI DUNG/GIÁ TRỊ/DỮ LIỆU bên trong file

⚠️ QUAN TRỌNG: Nếu câu hỏi chứa "bao nhiêu","là gì","giá trị","số tiền","nội dung","chi tiết" thì LUÔN là "chat" dù có từ "tìm"

Output JSON duy nhất: {"keywords":"...","intent":"search|chat"}

search (chỉ tìm file):
"tìm tài liệu có tho123"→{"keywords":"tho123","intent":"search"}
"file nào liên quan parking"→{"keywords":"parking","intent":"search"}
"tìm file tiếng nhật"→{"keywords":"tiếng nhật japanese 日本語","intent":"search"}
"kiếm code payment"→{"keywords":"payment","intent":"search"}
"TRIGGER_SETUP"→{"keywords":"TRIGGER_SETUP","intent":"search"}
"file .env"→{"keywords":".env environment config","intent":"search"}
"hàm getStaffById ở đâu"→{"keywords":"getStaffById","intent":"search"}
"tìm file markdown"→{"keywords":"markdown .md","intent":"search"}
"file deploy"→{"keywords":"deploy deployment","intent":"search"}

chat (hỏi nội dung/giá trị):
"tìm cho tôi AZV Test 3 cột 支払合計額 là bao nhiêu tiền"→{"keywords":"AZV Test 3 支払合計額","intent":"chat"}
"báo cáo tháng 1 có tổng doanh thu bao nhiêu"→{"keywords":"báo cáo tháng 1 doanh thu","intent":"chat"}
"trong file invoice giá trị cột total là gì"→{"keywords":"invoice total","intent":"chat"}
"tóm tắt báo cáo quý 3"→{"keywords":"báo cáo quý 3 report","intent":"chat"}
"tại sao API lỗi 500?"→{"keywords":"API lỗi 500 error","intent":"chat"}
"so sánh dev và production"→{"keywords":"dev production","intent":"chat"}
"SETUP.md nói gì?"→{"keywords":"SETUP.md","intent":"chat"}
"code có bug không?"→{"keywords":"bug code","intent":"chat"}
"tại sao deploy fail?"→{"keywords":"deploy fail error","intent":"chat"}
"phân tích database"→{"keywords":"database structure schema","intent":"chat"}
"khác nhau dev staging?"→{"keywords":"dev staging difference","intent":"chat"}
"có bao nhiêu test chưa pass?"→{"keywords":"test case fail","intent":"chat"}"#;

        // Dùng model từ ModelRegistry
        let url = format!(
            "{}/models/{}:generateContent?key={}",
            self.base_url, model, self.api_key
        );

        let request = GenerateRequest {
            contents: vec![Content {
                role: Some("user".to_string()),
                parts: vec![Part::text(query)],
            }],
            system_instruction: Some(Content {
                role: None,
                parts: vec![Part::text(system_prompt)],
            }),
            generation_config: Some(GenerationConfig {
                temperature: 0.0,
                top_p: 1.0,
                max_output_tokens: 60,
                response_mime_type: None,
                response_schema: None,
            }),
            tools: None,
        };

        let text = self.do_request(&url, &request).await?;

        // Parse JSON response
        // Tìm JSON trong response (Gemini có thể wrap trong ```json...```)
        let json_str = if let Some(start) = text.find('{') {
            if let Some(end) = text.rfind('}') {
                &text[start..=end]
            } else {
                &text
            }
        } else {
            &text
        };

        match serde_json::from_str::<QueryAnalysis>(json_str) {
            Ok(analysis) => {
                log::info!(
                    "[AI Analyze] '{}' → keywords='{}', intent={}",
                    query,
                    analysis.keywords,
                    analysis.intent
                );
                Ok(analysis)
            }
            Err(e) => {
                log::warn!("[AI Analyze] Parse failed: {}. Raw: {}", e, text);
                // Fallback: dùng query gốc
                Ok(QueryAnalysis {
                    keywords: query.to_string(),
                    intent: "search".to_string(),
                })
            }
        }
    }

    /// AI-First Query Rewriter: phân tích query + history → quyết định hành động + tạo search terms
    /// Thay thế local routing + regex keyword extraction
    #[allow(dead_code)]
    pub async fn rewrite_query(
        &self,
        query: &str,
        history_summary: &str,
        prev_keywords: &str,
        model: &str,
    ) -> Result<QueryRewrite, GeminiError> {
        let system_prompt = format!(
            r#"Bạn là router cho hệ thống tìm kiếm tài liệu. Phân tích câu hỏi user và quyết định hành động.

Output JSON duy nhất: {{"action":"search|direct|reuse|clarify","terms":"...","message":"..."}}

action:
- "search" = cần tìm tài liệu trong kho (có từ khóa cụ thể để search)
- "direct" = trả lời trực tiếp KHÔNG cần tài liệu (chào hỏi, kiến thức chung, phàn nàn, giải thích khái niệm)
- "reuse" = dùng lại dữ liệu đã tìm trước đó (follow-up trên data đang xem: phân tích, tổng hợp, so sánh, lọc thêm, đổi format)
- "clarify" = câu hỏi MƠ HỒ, thiếu ngữ cảnh → hỏi lại user. CHỈ dùng khi: (1) không có history trước, VÀ (2) câu hỏi quá chung chung hoặc không rõ muốn tìm gì

terms: keywords tối ưu cho BM25 search engine. Quy tắc:
- Giữ: tên riêng, mã code, CJK terms, thuật ngữ chuyên ngành, tên file
- Bỏ: stop words (tìm, cho tôi, là, có, bao nhiêu, tất cả...)
- Follow-up: viết lại thành câu search ĐỘC LẬP dựa vào history + keywords trước
- "reuse"/"direct"/"clarify": terms = ""

message: (chỉ khi action=clarify) Câu hỏi ngắn gọn bằng tiếng Việt để hỏi lại user. Dưới 50 từ. Gợi ý cụ thể.

{history_ctx}
{prev_kw}

Ví dụ:
SEARCH: "tìm file có AZV Test 3" → {{"action":"search","terms":"AZV Test 3","message":""}}
SEARCH: "支払合計額 là bao nhiêu" → {{"action":"search","terms":"支払合計額","message":""}}
REUSE: "tổng hợp thành báo cáo" → {{"action":"reuse","terms":"","message":""}}
DIRECT: "xin chào" → {{"action":"direct","terms":"","message":""}}
CLARIFY: "tìm file" → {{"action":"clarify","terms":"","message":"Bạn muốn tìm loại file gì? Ví dụ: hóa đơn, báo cáo, hợp đồng...?"}}
CLARIFY: "so sánh" → {{"action":"clarify","terms":"","message":"Bạn muốn so sánh những file hoặc dữ liệu nào? Cho tôi biết tên file hoặc nội dung cụ thể."}}"#,
            history_ctx = if history_summary.is_empty() {
                String::new()
            } else {
                format!("Lịch sử hội thoại gần nhất:\n{}", history_summary)
            },
            prev_kw = if prev_keywords.is_empty() {
                String::new()
            } else {
                format!("Keywords trước: {}", prev_keywords)
            },
        );

        let url = format!(
            "{}/models/{}:generateContent?key={}",
            self.base_url, model, self.api_key
        );

        let request = GenerateRequest {
            contents: vec![Content {
                role: Some("user".to_string()),
                parts: vec![Part::text(query)],
            }],
            system_instruction: Some(Content {
                role: None,
                parts: vec![Part::text(&system_prompt)],
            }),
            generation_config: Some(GenerationConfig {
                temperature: 0.0,
                top_p: 1.0,
                max_output_tokens: 120,
                response_mime_type: None,
                response_schema: None,
            }),
            tools: None,
        };

        let text = self.do_request(&url, &request).await?;

        // Parse JSON
        let json_str = if let Some(start) = text.find('{') {
            if let Some(end) = text.rfind('}') {
                &text[start..=end]
            } else {
                &text
            }
        } else {
            &text
        };

        match serde_json::from_str::<QueryRewrite>(json_str) {
            Ok(rewrite) => {
                log::info!(
                    "[AI Rewrite] '{}' → action={}, terms='{}'",
                    query,
                    rewrite.action,
                    rewrite.terms
                );
                Ok(rewrite)
            }
            Err(e) => {
                log::warn!(
                    "[AI Rewrite] Parse failed: {}. Raw: {}. Fallback to search.",
                    e,
                    text
                );
                Ok(QueryRewrite {
                    action: "search".to_string(),
                    terms: query.to_string(),
                    message: String::new(),
                })
            }
        }
    }

    /// Lấy danh sách models phù hợp cho chat/RAG từ Gemini API
    /// Lọc thông minh: chỉ Gemini chat models, bỏ TTS/Image/Robotics/Gemma...
    pub async fn list_models(&self) -> Result<Vec<GeminiModel>, GeminiError> {
        let url = format!("{}/models?key={}", self.base_url, self.api_key);

        let response = self.client.get(&url).send().await?;

        let status = response.status();
        let body = response.text().await?;

        if !status.is_success() {
            return Err(GeminiError::ApiError(format!("HTTP {}: {}", status, body)));
        }

        let list: ModelsListResponse = serde_json::from_str(&body)
            .map_err(|e| GeminiError::ParseError(format!("Parse models: {}", e)))?;

        let mut models: Vec<GeminiModel> = list
            .models
            .into_iter()
            .filter(|m| {
                // Phải hỗ trợ generateContent
                let supports_gen = m
                    .supported_generation_methods
                    .as_ref()
                    .map(|methods| methods.iter().any(|method| method == "generateContent"))
                    .unwrap_or(false);
                if !supports_gen {
                    return false;
                }

                let id = m.name.replace("models/", "");
                is_chat_model(&id)
            })
            .map(|m| {
                let id = m.name.replace("models/", "");
                GeminiModel {
                    id: id.clone(),
                    display_name: m.display_name.unwrap_or(id),
                }
            })
            .collect();

        // Sort: version mới nhất lên đầu
        models.sort_by(|a, b| b.id.cmp(&a.id));

        Ok(models)
    }

    /// Probe xem model có còn quota không (gọi API với maxOutputTokens=1)
    /// Trả về true nếu còn quota, false nếu bị rate-limited (429)
    pub async fn probe_model_quota(&self, model_id: &str) -> bool {
        let url = format!(
            "{}/models/{}:generateContent?key={}",
            self.base_url, model_id, self.api_key
        );

        let body = serde_json::json!({
            "contents": [{ "role": "user", "parts": [{ "text": "Hi" }] }],
            "generationConfig": { "maxOutputTokens": 1 }
        });

        let resp = self
            .client
            .post(&url)
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await;

        match resp {
            Ok(r) => r.status().as_u16() != 429,
            Err(_) => false, // Network error → assume unavailable
        }
    }

    /// ═══════════════════════════════════════════════════════
    /// Structured JSON Output — Ép AI trả JSON type-safe
    /// ═══════════════════════════════════════════════════════
    ///
    /// Gọi Gemini API với `response_mime_type: "application/json"` + JSON Schema.
    /// Parse kết quả qua serde → struct T. Auto-retry nếu parse fail (max 2 lần).
    ///
    /// # Example
    /// ```rust
    /// let result: LogicReasoning = client.generate_json(
    ///     "gemini-2.0-flash",
    ///     "Phân tích logic...",
    ///     Some("System prompt"),
    ///     logic_reasoning_schema(),
    ///     0.2,
    /// ).await?;
    /// ```
    #[allow(dead_code)]
    pub async fn generate_json<T: serde::de::DeserializeOwned>(
        &self,
        model: &str,
        prompt: &str,
        system_prompt: Option<&str>,
        json_schema: serde_json::Value,
        temperature: f32,
    ) -> Result<T, GeminiError> {
        let url = format!(
            "{}/models/{}:generateContent?key={}",
            self.base_url, model, self.api_key
        );

        let system_instruction = system_prompt.map(|sp| Content {
            role: None,
            parts: vec![Part::text(sp)],
        });

        let request = GenerateRequest {
            contents: vec![Content {
                role: Some("user".to_string()),
                parts: vec![Part::text(prompt)],
            }],
            system_instruction,
            generation_config: Some(GenerationConfig {
                temperature,
                top_p: 0.95,
                max_output_tokens: 8192,
                response_mime_type: Some("application/json".to_string()),
                response_schema: Some(json_schema),
            }),
            tools: None,
        };

        // Retry logic: max 2 attempts
        let max_retries = 2;
        let mut last_error = GeminiError::ParseError("Unknown".to_string());

        for attempt in 0..max_retries {
            if attempt > 0 {
                log::info!("[Gemini JSON] Retry {} ...", attempt);
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }

            match self.do_request(&url, &request).await {
                Ok(text) => {
                    // Parse JSON response type-safe
                    match crate::ai::structured::parse_structured::<T>(&text) {
                        Ok(parsed) => {
                            log::info!(
                                "[Gemini JSON] ✓ Parsed OK (attempt {})",
                                attempt + 1
                            );
                            return Ok(parsed);
                        }
                        Err(e) => {
                            log::warn!(
                                "[Gemini JSON] Parse failed (attempt {}): {}",
                                attempt + 1,
                                e
                            );
                            last_error = GeminiError::ParseError(format!(
                                "Structured parse: {}",
                                e
                            ));
                        }
                    }
                }
                Err(e) => {
                    log::error!(
                        "[Gemini JSON] API error (attempt {}): {}",
                        attempt + 1,
                        e
                    );
                    last_error = e;
                }
            }
        }

        log::error!("[Gemini JSON] ❌ All {} attempts failed", max_retries);
        Err(last_error)
    }
}

/// Kiểm tra model có phù hợp cho chat/RAG không
/// Rule: tên phải bắt đầu bằng "gemini-", không chứa keyword đặc biệt,
/// và cấu trúc tên phải gọn (max 4 phần khi split bằng '-')
/// → Tự động bắt model mới khi Google thêm (gemini-4.0-flash, gemini-3-pro...)
fn is_chat_model(id: &str) -> bool {
    // Phải bắt đầu bằng gemini- (loại gemma-, nano-, etc.)
    if !id.starts_with("gemini-") {
        return false;
    }

    // Loại models chuyên biệt (TTS, hình ảnh, robotics, embedding...)
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

    // Cấu trúc tên gọn: gemini-{ver}-{variant}[-preview|-latest]
    // Max 4 phần: ["gemini", "2.0", "flash", "preview"]
    // Loại: gemini-2.0-flash-001 (snapshot), gemini-2.5-flash-preview-sep-2025 (dated)
    let parts: Vec<&str> = id.split('-').collect();
    if parts.len() > 4 {
        return false;
    }

    // Phần cuối phải là variant hoặc modifier hợp lệ
    let last = *parts.last().unwrap_or(&"");
    matches!(last, "flash" | "pro" | "preview" | "latest")
}

/// Model info cho frontend
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct GeminiModel {
    pub id: String,
    pub display_name: String,
}

/// API response cho models.list
#[derive(Deserialize, Debug)]
struct ModelsListResponse {
    models: Vec<ModelEntry>,
}

#[derive(Deserialize, Debug)]
struct ModelEntry {
    name: String,
    #[serde(rename = "displayName")]
    display_name: Option<String>,
    #[serde(rename = "supportedGenerationMethods")]
    supported_generation_methods: Option<Vec<String>>,
}

/// Helper: tìm vị trí kết thúc JSON object trong SSE stream
fn find_json_end(s: &str) -> Option<usize> {
    let mut depth = 0i32;
    let mut in_string = false;
    let mut escape = false;

    for (i, ch) in s.char_indices() {
        if escape {
            escape = false;
            continue;
        }
        match ch {
            '\\' if in_string => escape = true,
            '"' => in_string = !in_string,
            '{' if !in_string => depth += 1,
            '}' if !in_string => {
                depth -= 1;
                if depth == 0 {
                    return Some(i + 1);
                }
            }
            _ => {}
        }
    }
    None
}
