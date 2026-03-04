#![allow(dead_code)]
//! Structured JSON Output — Ép AI trả kết quả có cấu trúc
//!
//! Sử dụng Gemini `response_mime_type: "application/json"` kết hợp serde
//! để parse kết quả type-safe. Auto-retry khi parse fail.

use serde::{de::DeserializeOwned, Deserialize, Serialize};

// ═══════════════════════════════════════════════════════
// Structured Output Structs — "Khuôn" suy luận cho AI
// ═══════════════════════════════════════════════════════

/// Ép AI suy luận logic theo từng bước
/// Step 1: Trích xuất sự thật → Step 2: Phân tích → Step 3: Kết luận
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogicReasoning {
    /// Bước 1: Trích xuất các sự thật/dữ kiện từ tài liệu
    pub step_1_extract_facts: Vec<String>,
    /// Bước 2: Phân tích mâu thuẫn, so sánh, logic
    pub step_2_analysis: String,
    /// Kết luận đúng/sai rõ ràng
    pub final_conclusion: String,
}

/// Tách câu hỏi phức tạp thành sub-queries để tìm song song
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryDecomposition {
    /// Câu hỏi có phức tạp cần tách không
    pub needs_decomposition: bool,
    /// Danh sách sub-queries (1 nếu đơn giản, 2-4 nếu phức tạp)
    pub sub_queries: Vec<SubQuery>,
}

/// Một sub-query sau khi tách
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubQuery {
    /// Search terms tối ưu cho BM25
    pub search_terms: String,
    /// Mô tả ngắn mục đích tìm
    pub purpose: String,
}

/// Kết quả phân tích so sánh (VD: "So sánh A và B")
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComparisonResult {
    /// Các điểm giống nhau
    pub similarities: Vec<String>,
    /// Các điểm khác nhau
    pub differences: Vec<String>,
    /// Kết luận tổng hợp
    pub conclusion: String,
}

/// Kết quả trích xuất dữ liệu (VD: "Liệt kê tất cả giá trị X")
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataExtraction {
    /// Các giá trị tìm được
    pub values: Vec<ExtractedValue>,
    /// Tổng kết
    pub summary: String,
}

/// Một giá trị được trích xuất
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractedValue {
    /// Tên/nhãn
    pub label: String,
    /// Giá trị
    pub value: String,
    /// Nguồn (file, section)
    pub source: String,
}

// ═══════════════════════════════════════════════════════
// JSON Schema Helpers — Sinh schema cho Gemini API
// ═══════════════════════════════════════════════════════

/// Sinh JSON Schema cho LogicReasoning
pub fn logic_reasoning_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "step_1_extract_facts": {
                "type": "array",
                "items": { "type": "string" },
                "description": "Trích xuất các sự thật/dữ kiện từ tài liệu"
            },
            "step_2_analysis": {
                "type": "string",
                "description": "Phân tích logic, mâu thuẫn, so sánh"
            },
            "final_conclusion": {
                "type": "string",
                "description": "Kết luận rõ ràng"
            }
        },
        "required": ["step_1_extract_facts", "step_2_analysis", "final_conclusion"]
    })
}

/// Sinh JSON Schema cho QueryDecomposition
pub fn query_decomposition_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "needs_decomposition": {
                "type": "boolean",
                "description": "Câu hỏi có phức tạp cần tách không"
            },
            "sub_queries": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "search_terms": {
                            "type": "string",
                            "description": "Keywords tối ưu cho BM25 search"
                        },
                        "purpose": {
                            "type": "string",
                            "description": "Mục đích tìm kiếm ngắn gọn"
                        }
                    },
                    "required": ["search_terms", "purpose"]
                }
            }
        },
        "required": ["needs_decomposition", "sub_queries"]
    })
}

/// Sinh JSON Schema cho ComparisonResult
pub fn comparison_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "similarities": {
                "type": "array",
                "items": { "type": "string" }
            },
            "differences": {
                "type": "array",
                "items": { "type": "string" }
            },
            "conclusion": { "type": "string" }
        },
        "required": ["similarities", "differences", "conclusion"]
    })
}

/// Sinh JSON Schema cho DataExtraction
pub fn data_extraction_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "values": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "label": { "type": "string" },
                        "value": { "type": "string" },
                        "source": { "type": "string" }
                    },
                    "required": ["label", "value", "source"]
                }
            },
            "summary": { "type": "string" }
        },
        "required": ["values", "summary"]
    })
}

// ═══════════════════════════════════════════════════════
// Type-safe JSON Parsing — Retry khi parse fail
// ═══════════════════════════════════════════════════════

/// Parse JSON string → struct T (type-safe)
/// Tự động tìm JSON object trong response (Gemini có thể wrap ```json...```)
pub fn parse_structured<T: DeserializeOwned>(json_str: &str) -> Result<T, StructuredError> {
    // Bước 1: Tìm JSON object trong response
    let clean = extract_json(json_str);

    // Bước 2: Parse với serde
    serde_json::from_str::<T>(&clean).map_err(|e| StructuredError::ParseFailed {
        error: e.to_string(),
        raw_response: json_str.to_string(),
    })
}

/// Extract JSON object từ response (handle ```json...``` wrapper)
fn extract_json(text: &str) -> String {
    let trimmed = text.trim();

    // Trường hợp 1: Đã là JSON thuần
    if trimmed.starts_with('{') {
        return trimmed.to_string();
    }

    // Trường hợp 2: Wrapped trong ```json...```
    if let Some(start) = trimmed.find("```json") {
        let after_marker = &trimmed[start + 7..];
        if let Some(end) = after_marker.find("```") {
            return after_marker[..end].trim().to_string();
        }
    }

    // Trường hợp 3: Wrapped trong ```...```
    if let Some(start) = trimmed.find("```") {
        let after_marker = &trimmed[start + 3..];
        // Skip optional language identifier on same line
        let content_start = after_marker.find('\n').map(|n| n + 1).unwrap_or(0);
        let after_lang = &after_marker[content_start..];
        if let Some(end) = after_lang.find("```") {
            return after_lang[..end].trim().to_string();
        }
    }

    // Trường hợp 4: Tìm { ... } trong text
    if let Some(start) = trimmed.find('{') {
        if let Some(end) = trimmed.rfind('}') {
            if end > start {
                return trimmed[start..=end].to_string();
            }
        }
    }

    // Fallback: return as-is
    trimmed.to_string()
}

#[derive(Debug, thiserror::Error)]
pub enum StructuredError {
    #[error("Parse JSON thất bại: {error}")]
    ParseFailed { error: String, raw_response: String },
}
