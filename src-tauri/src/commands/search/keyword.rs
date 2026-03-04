//! Keyword extraction, CJK ngrams, stop words, score normalization
//!
//! Module xử lý:
//! - Tách keywords từ query (Vietnamese + CJK + Latin)
//! - CJK ngram generation cho BM25
//! - Vietnamese stop words filtering
//! - Score normalization
//! - Query intent detection (search-only vs chat)

use super::SearchResult;

/// ═══════════════════════════════════════════════════════
/// Vietnamese Stop Words — loại từ phụ, giữ keyword thật
/// ═══════════════════════════════════════════════════════
pub const STOP_WORDS: &[&str] = &[
    // Động từ hỗ trợ / chỉ hành động tìm kiếm
    "tìm",
    "kiếm",
    "tìm kiếm",
    "cho",
    "giúp",
    "xem",
    "mở",
    "lấy",
    "tra",
    "tra cứu",
    "hiện",
    "hiển thị",
    "liệt kê",
    "show",
    "find",
    "search",
    "list",
    "tạo",
    "viết",
    "phân tích",
    "tổng hợp",
    "báo cáo",
    "report",
    "cần",
    "muốn",
    "nên",
    // Đại từ
    "tôi",
    "tao",
    "mình",
    "bạn",
    "nó",
    "chúng",
    "ta",
    // Giới từ / liên từ
    "của",
    "và",
    "với",
    "để",
    "từ",
    "trong",
    "ở",
    "về",
    "cho",
    "đến",
    "bằng",
    "hay",
    "hoặc",
    "cùng",
    "nhưng",
    "mà",
    "vì",
    "nên",
    "do",
    "bởi",
    // Từ chỉ định / mạo từ
    "này",
    "đó",
    "kia",
    "các",
    "những",
    "một",
    "mỗi",
    "mọi",
    "toàn bộ",
    "nào",
    "gì",
    "đâu",
    "sao",
    "bao nhiêu",
    // Từ đệm phổ biến trong câu tìm kiếm
    "là",
    "có",
    "được",
    "bị",
    "đã",
    "đang",
    "sẽ",
    "rồi",
    "chưa",
    "thì",
    "cũng",
    "còn",
    "lại",
    "ra",
    "lên",
    "xuống",
    // Danh từ chung liên quan tìm kiếm
    "file",
    "tài liệu",
    "tập tin",
    "văn bản",
    "document",
    "doc",
    "nội dung",
    "thông tin",
    "dữ liệu",
    "data",
    "liên quan",
    "chứa",
    "bao gồm",
    "thuộc",
    "quan",
    "theo",
    // Tính từ phổ biến
    "tất cả",
    "hết",
    "nhiều",
    "ít",
    "mới",
    "cũ",
    // Từ hỏi
    "hãy",
    "vui lòng",
    "xin",
    "nhờ",
    "ơi",
    // Từ so sánh / toán tử (NOISE cho search)
    "lớn hơn",
    "nhỏ hơn",
    "cao hơn",
    "thấp hơn",
    "nhiều hơn",
    "ít hơn",
    "bằng hoặc lớn hơn",
    "bằng hoặc nhỏ hơn",
    "lớn",
    "nhỏ",
    "cao",
    "thấp",
    "hơn",
    "trên",
    "dưới",
    "khoảng",
    "gần",
    "xấp xỉ",
    "greater",
    "less",
    "than",
    "more",
    "equal",
    // Từ phụ thêm
    "giá trị",
    "giá",
    "số",
    "tổng",
    "trung bình",
    "at least",
    "at most",
];

/// Kiểm tra ký tự CJK (Chinese/Japanese/Korean)
pub fn is_cjk_char(c: char) -> bool {
    matches!(c,
        '\u{4E00}'..='\u{9FFF}'   | // CJK Unified Ideographs
        '\u{3400}'..='\u{4DBF}'   | // CJK Extension A
        '\u{3000}'..='\u{303F}'   | // CJK Symbols & Punctuation
        '\u{3040}'..='\u{309F}'   | // Hiragana
        '\u{30A0}'..='\u{30FF}'   | // Katakana
        '\u{FF00}'..='\u{FFEF}'   | // Fullwidth Latin / Halfwidth Katakana
        '\u{AC00}'..='\u{D7AF}'   | // Korean Hangul
        '\u{F900}'..='\u{FAFF}'   | // CJK Compat Ideographs
        '\u{20000}'..='\u{2A6DF}'   // CJK Extension B
    )
}

/// Kiểm tra token có chứa ký tự CJK không
pub fn contains_cjk(s: &str) -> bool {
    s.chars().any(is_cjk_char)
}

/// Kiểm tra token chỉ gồm số thuần (không gắn CJK)
fn is_standalone_number(s: &str) -> bool {
    // "2000" → true (noise), "2000円" → false (giữ), "v3" → false (giữ)
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_digit() || c == '.' || c == ',')
}

/// Tách CJK text thành các ngram 2-3 ký tự để BM25 match substring
/// "支払合計額" → "支払 払合 合計 計額 支払合 払合計 合計額"
pub fn cjk_ngrams(text: &str) -> Vec<String> {
    let cjk_chars: Vec<char> = text.chars().filter(|c| is_cjk_char(*c)).collect();
    let mut ngrams = Vec::new();

    // Bigrams
    for i in 0..cjk_chars.len().saturating_sub(1) {
        ngrams.push(cjk_chars[i..i + 2].iter().collect::<String>());
    }
    // Trigrams
    for i in 0..cjk_chars.len().saturating_sub(2) {
        ngrams.push(cjk_chars[i..i + 3].iter().collect::<String>());
    }
    // Full term nếu >= 2 chars
    if cjk_chars.len() >= 2 {
        ngrams.push(cjk_chars.iter().collect::<String>());
    }

    ngrams
}

/// Loại bỏ stop words, giữ lại keywords thật sự
/// Đặc biệt:
///   - Giữ NGUYÊN CJK (không lowercase), tạo ngram cho CJK
///   - Phát hiện compound terms (VD: "AZV Test 3", "v3.xlsx") và giữ nguyên
pub fn extract_keywords(query: &str) -> String {
    // ── Bước 0: Phát hiện compound terms (viết hoa, mã code, tên file) ──
    // VD: "AZV Test 3" → giữ nguyên "AZV Test 3"
    //     "TESTCASE_DETAIL" → giữ nguyên
    //     "v3.xlsx" → giữ nguyên
    let compound_terms = extract_compound_terms(query);

    // ── Bước 1: Tách CJK spans ra khỏi phần Latin/Vietnamese ──
    let mut cjk_parts: Vec<String> = Vec::new();
    let mut latin_parts: Vec<String> = Vec::new();

    let mut current_cjk = String::new();
    let mut current_latin = String::new();

    for c in query.chars() {
        if is_cjk_char(c) {
            if !current_latin.is_empty() {
                latin_parts.push(current_latin.clone());
                current_latin.clear();
            }
            current_cjk.push(c);
        } else {
            if !current_cjk.is_empty() {
                cjk_parts.push(current_cjk.clone());
                current_cjk.clear();
            }
            current_latin.push(c);
        }
    }
    if !current_cjk.is_empty() {
        cjk_parts.push(current_cjk);
    }
    if !current_latin.is_empty() {
        latin_parts.push(current_latin);
    }

    // ── Bước 2: Xử lý phần Latin/Vietnamese — loại stop words ──
    let latin_text = latin_parts.join(" ");
    let latin_lower = latin_text.to_lowercase();

    // Loại multi-word stop words trước (dài hơn trước)
    let mut cleaned = latin_lower.clone();
    let mut multi_word: Vec<&&str> = STOP_WORDS.iter().filter(|w| w.contains(' ')).collect();
    multi_word.sort_by(|a, b| b.len().cmp(&a.len()));
    for sw in multi_word {
        cleaned = cleaned.replace(*sw, " ");
    }

    // Loại single-word stop words + số thuần + strip punctuation
    let latin_keywords: Vec<String> = cleaned
        .split_whitespace()
        .map(|word| word.trim_matches(|c: char| c == ',' || c == '.' || c == ';' || c == ':' || c == '!' || c == '?').to_string())
        .filter(|w| {
            if w.is_empty() || w.len() < 2 {
                return false;
            }
            if STOP_WORDS.contains(&w.as_str()) {
                return false;
            }
            if is_standalone_number(w) && w.len() >= 5 {
                return false;
            } // "20000" → noise, nhưng giữ "3", "v3", "100"
            true
        })
        .collect();

    // ── Bước 3: Ghép kết quả ──
    let mut all_keywords: Vec<String> = Vec::new();

    // ★ Compound terms ưu tiên CAO NHẤT → đứng đầu
    for ct in &compound_terms {
        all_keywords.push(ct.clone());
    }

    // CJK terms
    for cjk in &cjk_parts {
        let trimmed = cjk.trim();
        if trimmed.is_empty() {
            continue;
        }
        all_keywords.push(trimmed.to_string());
        all_keywords.extend(cjk_ngrams(trimmed));
    }

    // Latin keywords (skip những từ đã có trong compound terms)
    let compound_lower: Vec<String> = compound_terms.iter().map(|t| t.to_lowercase()).collect();
    for kw in &latin_keywords {
        // Skip nếu keyword đã nằm trong compound term
        let in_compound = compound_lower.iter().any(|ct| ct.contains(kw.as_str()));
        if !in_compound {
            all_keywords.push(kw.to_string());
        }
    }

    // Deduplicate giữ thứ tự
    let mut seen = std::collections::HashSet::new();
    all_keywords.retain(|k| seen.insert(k.to_lowercase()));

    if all_keywords.is_empty() {
        query.to_string()
    } else {
        all_keywords.join(" ")
    }
}

/// Phát hiện compound terms: chuỗi từ liền nhau chứa uppercase, số, underscore
/// VD: "AZV Test 3" → ["AZV Test 3"]
///     "file TESTCASE_DETAIL.md" → ["TESTCASE_DETAIL.md"]
///     "v3 report" → ["v3"]
pub fn extract_compound_terms(query: &str) -> Vec<String> {
    let mut terms: Vec<String> = Vec::new();

    // Regex-like: tìm chuỗi gồm (chữ hoa | chữ_hoa_underscore | alphanum có chữ hoa)
    // kết hợp với từ/số liền kề
    let words: Vec<&str> = query.split_whitespace().collect();
    let mut i = 0;

    while i < words.len() {
        let w = words[i];
        let is_significant = is_compound_word(w);

        if is_significant {
            // Mở rộng: gom các từ liền kề (viết hoa, số, hoặc ngắn)
            let mut compound = vec![w];
            let mut j = i + 1;
            while j < words.len() {
                let next = words[j].trim_end_matches(|c: char| c == ',' || c == '.' || c == ';');
                if next.is_empty() {
                    break;
                }
                // Gom tiếp nếu: số, viết hoa, hoặc từ ngắn < 4 ký tự (thường là phần của tên)
                let next_is_num = next.chars().all(|c| c.is_ascii_digit());
                let next_is_upper = is_compound_word(next);
                let next_is_short = next.len() <= 3 && next.chars().any(|c| c.is_ascii_digit());
                if next_is_num || next_is_upper || next_is_short {
                    compound.push(words[j].trim_end_matches(|c: char| c == ',' || c == ';'));
                    j += 1;
                } else {
                    break;
                }
            }

            let term = compound.join(" ");
            // Chỉ giữ compound >= 2 ký tự
            if term.len() >= 2 {
                terms.push(term);
            }
            i = j;
        } else {
            i += 1;
        }
    }

    terms
}

/// Kiểm tra 1 từ có phải "compound word" (tên riêng, mã code, ID)
fn is_compound_word(w: &str) -> bool {
    let clean = w.trim_end_matches(|c: char| c == ',' || c == '.' || c == ';' || c == ':');
    if clean.is_empty() {
        return false;
    }
    // Chứa underscore → code identifier (VD: TESTCASE_DETAIL)
    if clean.contains('_') && clean.len() >= 3 {
        return true;
    }
    // Chứa extension (VD: v3.xlsx)
    if clean.contains('.') && clean.len() >= 3 {
        let parts: Vec<&str> = clean.splitn(2, '.').collect();
        if parts.len() == 2 && !parts[1].is_empty() {
            return true;
        }
    }
    // Có ít nhất 1 chữ hoa → tên riêng (VD: AZV, Test)
    // Nhưng loại trừ Vietnamese words thường viết hoa đầu câu
    let has_upper = clean.chars().any(|c| c.is_uppercase());
    let all_upper = clean.chars().filter(|c| c.is_alphabetic()).all(|c| c.is_uppercase());
    let has_digit = clean.chars().any(|c| c.is_ascii_digit());
    // VD: "AZV" (all upper) → compound
    // VD: "Test" (first upper) → compound nếu đi kèm từ uppercase khác
    // VD: "v3" (lower + digit) → compound
    if all_upper && clean.len() >= 2 {
        return true;
    }
    if has_upper && has_digit {
        return true; // VD: "Test3", "V2"
    }
    if has_digit && clean.chars().any(|c| c.is_alphabetic()) {
        return true; // VD: "v3", "3a"
    }
    // Chữ hoa đầu + phần còn lại lower → possible name
    if has_upper && clean.len() >= 2 && clean.chars().next().unwrap().is_uppercase() {
        return true;
    }
    false
}

/// Normalize BM25 scores to 0.0 → 1.0 range (relative to best result)
pub fn normalize_scores(results: &mut Vec<SearchResult>) {
    if results.is_empty() {
        return;
    }
    let max_score = results
        .iter()
        .map(|r| r.score)
        .fold(f32::NEG_INFINITY, f32::max);

    if max_score > 0.0 {
        for r in results.iter_mut() {
            r.score = (r.score / max_score).clamp(0.0, 1.0);
        }
    }
}

/// Xác định query chỉ muốn liệt kê/tìm file (không cần AI phân tích)
/// Trả về false → mặc định hỏi AI (chat mode)
pub fn is_search_only(query: &str) -> bool {
    let q = query.trim();
    let words: Vec<&str> = q.split_whitespace().collect();

    // 1. Chỉ 1 từ mà trông giống tên file/mã code (không phải câu hỏi)
    //    VD: "TRIGGER_SETUP", "tho123", ".env"
    if words.len() == 1 {
        let w = words[0];
        // Có extension → tìm file
        if w.contains('.') && w.len() > 2 {
            return true;
        }
        // Toàn chữ hoa + số/underscore → code/ID
        if w.len() >= 3
            && w.chars()
                .all(|c| c.is_uppercase() || c.is_ascii_digit() || c == '_')
        {
            return true;
        }
    }

    // 2. Pattern "tìm file X", "kiếm file X", "file X ở đâu"
    let file_search_prefixes = [
        "tìm file",
        "kiếm file",
        "file nào",
        "list file",
        "liệt kê file",
        "danh sách file",
    ];
    if file_search_prefixes.iter().any(|p| q.starts_with(p)) {
        return true;
    }

    // 3. Chứa extension rõ ràng → user muốn tìm file cụ thể
    let extensions = [
        ".pdf", ".docx", ".xlsx", ".xls", ".pptx", ".csv", ".md", ".txt",
    ];
    let has_extension = extensions.iter().any(|ext| q.contains(ext));
    let has_question = ["?", "bao", "sao", "gì", "nào", "thế"]
        .iter()
        .any(|w| q.contains(w));
    if has_extension && !has_question {
        return true;
    }

    // 4. Mọi thứ khác → chat (AI phân tích)
    false
}

/// Tạo context snippet xung quanh keyword match
pub fn extract_snippet(content: &str, query: &str, max_len: usize) -> (String, Vec<String>) {
    // Clean content: collapse whitespace, remove excessive line breaks
    let cleaned: String = content
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" | ");
    // Collapse multiple spaces
    let cleaned = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");

    let content_lower = cleaned.to_lowercase();
    let clean_query = extract_keywords(query);
    let query_lower = clean_query.to_lowercase();

    let keywords: Vec<&str> = query_lower
        .split_whitespace()
        .filter(|w| w.len() >= 2)
        .collect();

    if keywords.is_empty() {
        let snippet: String = cleaned.chars().take(max_len).collect();
        return (snippet, vec![]);
    }

    // Tìm vị trí match đầu tiên (CJK: match trên raw, Latin: match trên lowercase)
    let mut best_pos = 0;
    let mut found_keywords = Vec::new();

    for kw in &keywords {
        let pos_opt = if contains_cjk(kw) {
            cleaned.find(*kw) // CJK: exact match on original
        } else {
            content_lower.find(*kw)
        };
        if let Some(pos) = pos_opt {
            if found_keywords.is_empty() || pos < best_pos {
                best_pos = pos;
            }
            found_keywords.push(kw.to_string());
        }
    }

    // Lấy snippet xung quanh vị trí match
    let half_len = max_len / 2;
    let char_count = cleaned.chars().count();

    let char_indices: Vec<(usize, char)> = cleaned.char_indices().collect();

    let start_char = if best_pos > half_len {
        char_indices
            .iter()
            .position(|(byte_idx, _)| *byte_idx >= best_pos.saturating_sub(half_len))
            .unwrap_or(0)
    } else {
        0
    };

    let end_char = (start_char + max_len).min(char_count);
    let snippet: String = cleaned
        .chars()
        .skip(start_char)
        .take(end_char - start_char)
        .collect();

    let prefix = if start_char > 0 { "..." } else { "" };
    let suffix = if end_char < char_count { "..." } else { "" };

    (
        format!("{}{}{}", prefix, snippet.trim(), suffix),
        found_keywords,
    )
}
