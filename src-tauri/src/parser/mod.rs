use kreuzberg::core::config::OcrConfig;
use kreuzberg::{extract_file_sync, ChunkingConfig, ExtractionConfig};
use std::path::Path;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum ParseError {
    #[error("File không tồn tại: {0}")]
    FileNotFound(String),
    #[error("Định dạng không hỗ trợ: {0}")]
    UnsupportedFormat(String),
    #[error("Lỗi đọc file: {0}")]
    IoError(#[from] std::io::Error),
    #[error("Lỗi parse: {0}")]
    ParseFailed(String),
}

/// Chunk data từ kreuzberg (lightweight, chỉ giữ content + offset)
#[derive(Debug, Clone)]
pub struct KreuzbergChunkData {
    pub content: String,
    pub byte_start: usize,
    pub byte_end: usize,
}

/// Kết quả parse document
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct ParsedDocument {
    pub file_path: String,
    pub file_name: String,
    pub content: String,
    pub sections: Vec<DocumentSection>,
    pub metadata: DocumentMetadata,
    /// Kreuzberg chunks (nếu có) — ưu tiên dùng thay manual chunker
    pub kreuzberg_chunks: Option<Vec<KreuzbergChunkData>>,
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct DocumentSection {
    pub title: Option<String>,
    pub content: String,
    pub page: Option<usize>,
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct DocumentMetadata {
    pub file_type: String,
    pub file_size: u64,
    pub page_count: Option<usize>,
}

// === Supported file lists ===

/// Plain text — đọc trực tiếp, không cần kreuzberg
const PLAIN_TEXT_EXTENSIONS: &[&str] = &[
    "txt", "md", "markdown", "rst", "log", "csv", "tsv", "json", "xml", "yaml", "yml", "toml",
];

/// Ảnh — cần OCR (kreuzberg-tesseract)
const IMAGE_EXTENSIONS: &[&str] = &["jpg", "jpeg", "png", "tiff", "tif", "bmp", "webp"];

/// Structured data — chunk lớn hơn để giữ nguyên bảng
const STRUCTURED_DATA_EXTENSIONS: &[&str] = &["xlsx", "xls", "ods"];

/// Tất cả extensions hỗ trợ
const SUPPORTED_EXTENSIONS: &[&str] = &[
    // Rich documents
    "pdf", "docx", "doc", "pptx", "ppt", "xlsx", "xls", "odt", "ods", "odp", "rtf",
    // Plain text (fast path)
    "txt", "md", "markdown", "rst", "csv", "tsv", "log", // Web & Data
    "html", "htm", "json", "xml", "yaml", "yml", "toml", // Email & Ebook
    "eml", "msg", "epub", // Images (OCR)
    "jpg", "jpeg", "png", "tiff", "tif", "bmp", "webp",
];

/// Kiểm tra file có hỗ trợ không
pub fn is_supported(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| SUPPORTED_EXTENSIONS.contains(&e.to_lowercase().as_str()))
        .unwrap_or(false)
}

/// Parse file — entry point chính
pub fn parse_file(path: &Path) -> Result<ParsedDocument, ParseError> {
    if !path.exists() {
        return Err(ParseError::FileNotFound(path.to_string_lossy().to_string()));
    }

    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase())
        .ok_or_else(|| ParseError::UnsupportedFormat("Không có extension".to_string()))?;

    let file_size = std::fs::metadata(path)?.len();
    let file_name = path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();

    // === Fast path: plain text — đọc trực tiếp ===
    if PLAIN_TEXT_EXTENSIONS.contains(&ext.as_str()) {
        return parse_plain_text(path, &file_name, &ext, file_size);
    }

    // === Kreuzberg path: PDF, DOCX, XLSX, HTML, ảnh, v.v. ===
    let (content, kr_chunks) = extract_with_kreuzberg(path, &ext, &file_name)?;

    if content.trim().is_empty() {
        return Err(ParseError::ParseFailed(format!(
            "File không chứa nội dung text: {}",
            file_name
        )));
    }

    Ok(ParsedDocument {
        file_path: path.to_string_lossy().to_string(),
        file_name,
        content: content.clone(),
        sections: vec![DocumentSection {
            title: None,
            content,
            page: None,
        }],
        metadata: DocumentMetadata {
            file_type: ext,
            file_size,
            page_count: None,
        },
        kreuzberg_chunks: kr_chunks,
    })
}

/// Đọc plain text trực tiếp
fn parse_plain_text(
    path: &Path,
    file_name: &str,
    ext: &str,
    file_size: u64,
) -> Result<ParsedDocument, ParseError> {
    let raw = std::fs::read(path)?;
    let content = match String::from_utf8(raw.clone()) {
        Ok(s) => s,
        Err(_) => {
            log::warn!("[Parser] {} is not UTF-8, using lossy decode", file_name);
            String::from_utf8_lossy(&raw).to_string()
        }
    };
    let content = content.trim().to_string();
    if content.is_empty() {
        return Err(ParseError::ParseFailed(format!(
            "File không chứa nội dung text: {}",
            file_name
        )));
    }
    Ok(ParsedDocument {
        file_path: path.to_string_lossy().to_string(),
        file_name: file_name.to_string(),
        content: content.clone(),
        sections: vec![DocumentSection {
            title: None,
            content,
            page: None,
        }],
        metadata: DocumentMetadata {
            file_type: ext.to_string(),
            file_size,
            page_count: None,
        },
        kreuzberg_chunks: None, // Plain text → dùng manual chunker
    })
}

/// Ngôn ngữ OCR — auto-detect dựa trên tessdata có sẵn
/// Hỗ trợ: Tiếng Việt (vie) + Tiếng Nhật (jpn) + English (eng)
fn ocr_language() -> &'static str {
    use std::sync::OnceLock;
    static LANG: OnceLock<String> = OnceLock::new();
    LANG.get_or_init(|| {
        // Các thư mục tessdata có thể có
        let tessdata_dirs = [
            std::env::var("TESSDATA_PREFIX").ok().unwrap_or_default(),
            "/opt/homebrew/share/tessdata".to_string(),
            "/usr/local/share/tessdata".to_string(),
            "/usr/share/tessdata".to_string(),
        ];

        let has_lang = |lang: &str| -> bool {
            let filename = format!("{}.traineddata", lang);
            tessdata_dirs
                .iter()
                .any(|dir| std::path::Path::new(dir).join(&filename).exists())
        };

        let mut langs = vec!["eng"];
        if has_lang("vie") {
            langs.push("vie");
        }
        if has_lang("jpn") {
            langs.push("jpn");
        }

        let result = langs.join("+");
        log::info!("[Parser] OCR languages: {}", result);
        result
    })
}

/// Chọn ChunkingConfig theo loại file
fn chunking_config_for_ext(ext: &str) -> ChunkingConfig {
    if STRUCTURED_DATA_EXTENSIONS.contains(&ext) {
        // Nhóm 1: Excel/ODS → chunk lớn, giữ nguyên bảng
        ChunkingConfig {
            max_characters: 4000,
            overlap: 400,
            ..Default::default()
        }
    } else {
        // Nhóm 2: PDF/DOCX/HTML/... → chunk vừa
        ChunkingConfig {
            max_characters: 2000,
            overlap: 200,
            ..Default::default()
        }
    }
}

/// Extract document qua kreuzberg — trả content + optional chunks
fn extract_with_kreuzberg(
    path: &Path,
    ext: &str,
    file_name: &str,
) -> Result<(String, Option<Vec<KreuzbergChunkData>>), ParseError> {
    let is_image = IMAGE_EXTENSIONS.contains(&ext);

    // === Pass 1: text-based (PDF có text layer, DOCX, HTML...) ===
    if !is_image {
        let chunking = chunking_config_for_ext(ext);
        let config = ExtractionConfig {
            chunking: Some(chunking),
            output_format: kreuzberg::core::config::formats::OutputFormat::Markdown,
            ..Default::default()
        };

        match extract_file_sync(path, None, &config) {
            Ok(result) if !result.content.trim().is_empty() => {
                let kr_chunks = result.chunks.map(|chunks| {
                    chunks
                        .into_iter()
                        .map(|c| KreuzbergChunkData {
                            content: c.content,
                            byte_start: c.metadata.byte_start,
                            byte_end: c.metadata.byte_end,
                        })
                        .collect()
                });

                log::info!(
                    "[Parser] ✓ Kreuzberg extract: {} → {} chars, {} chunks (Markdown output)",
                    file_name,
                    result.content.len(),
                    kr_chunks
                        .as_ref()
                        .map_or(0, |c: &Vec<KreuzbergChunkData>| c.len())
                );
                return Ok((result.content, kr_chunks));
            }
            Ok(_) => {
                log::info!("[Parser] Pass1 empty: {} → trying OCR...", file_name);
            }
            Err(e) => {
                log::warn!("[Parser] Pass1 error {}: {} → trying OCR...", file_name, e);
            }
        }
    }

    // === Pass 2: OCR via kreuzberg-tesseract ===
    let lang = ocr_language();
    let ocr_config = OcrConfig {
        backend: "tesseract".to_string(),
        language: lang.to_string(),
        ..Default::default()
    };

    let config = ExtractionConfig {
        ocr: Some(ocr_config),
        force_ocr: is_image,
        // OCR text thường ngắn → không cần kreuzberg chunking
        ..Default::default()
    };

    match extract_file_sync(path, None, &config) {
        Ok(result) if !result.content.trim().is_empty() => {
            log::info!(
                "[Parser] ✓ OCR extract: {} ({}) → {} chars",
                file_name,
                lang,
                result.content.len()
            );
            Ok((result.content, None)) // OCR → dùng manual chunker
        }
        Ok(_) => Err(ParseError::ParseFailed(format!(
            "File không chứa nội dung text: {}",
            file_name
        ))),
        Err(e) => {
            // OCR error có thể do Tesseract chưa cài
            log::error!(
                "[Parser] ❌ OCR error {}: {}. \
                 Nếu chưa cài Tesseract: brew install tesseract tesseract-lang",
                file_name,
                e
            );
            Err(ParseError::ParseFailed(format!("OCR error: {}", e)))
        }
    }
}
