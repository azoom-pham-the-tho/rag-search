#![allow(dead_code)]
//! Context Guard — Token estimation + context budget for RAG pipeline
//!
//! Đảm bảo tổng context (history + search results + system prompt)
//! không vượt quá budget, tránh "lost in the middle" → hallucination

// ═══════════════════════════════════════════════════════
// Token Estimation
// ═══════════════════════════════════════════════════════

/// Budget tokens cho toàn bộ context gửi cho AI
/// 15K tokens ≈ 60K chars Latin — sweet spot cho accuracy
/// Gemini Flash hỗ trợ 1M input, nhưng AI trả lời chính xác hơn với context ngắn
pub const MAX_CONTEXT_TOKENS: usize = 15_000;

/// Budget cho chat history (phần của MAX_CONTEXT_TOKENS)
pub const MAX_HISTORY_TOKENS: usize = 3_000;

/// Số turns tối đa giữ lại
pub const MAX_HISTORY_TURNS: usize = 5;

/// Ước tính số tokens từ text
/// Rule: ~4 chars/token cho Latin, ~2 chars/token cho CJK/Vietnamese
pub fn estimate_tokens(text: &str) -> usize {
    if text.is_empty() {
        return 0;
    }

    let mut latin_chars = 0usize;
    let mut cjk_chars = 0usize;

    for ch in text.chars() {
        if is_cjk_char(ch) {
            cjk_chars += 1;
        } else {
            latin_chars += 1;
        }
    }

    // Latin: ~4 chars/token, CJK: ~2 chars/token
    (latin_chars / 4) + (cjk_chars / 2) + 1 // +1 avoid zero
}

/// Check CJK character (Chinese/Japanese/Korean + Vietnamese diacritics count as Latin)
fn is_cjk_char(ch: char) -> bool {
    matches!(ch,
        '\u{4E00}'..='\u{9FFF}'   | // CJK Unified
        '\u{3400}'..='\u{4DBF}'   | // CJK Extension A
        '\u{3040}'..='\u{309F}'   | // Hiragana
        '\u{30A0}'..='\u{30FF}'   | // Katakana
        '\u{AC00}'..='\u{D7AF}'     // Korean Hangul
    )
}

// ═══════════════════════════════════════════════════════
// History Trimming
// ═══════════════════════════════════════════════════════

/// Trim chat history theo budget: max N turns + max tokens
/// Giữ turns GẦN NHẤT (quan trọng nhất cho conversational context)
pub fn trim_history(
    history: &[(String, String)],
    max_turns: usize,
    max_tokens: usize,
) -> Vec<(String, String)> {
    if history.is_empty() {
        return vec![];
    }

    // Bước 1: Lấy N turns gần nhất
    let recent: Vec<&(String, String)> = if history.len() > max_turns {
        history[history.len() - max_turns..].iter().collect()
    } else {
        history.iter().collect()
    };

    // Bước 2: Trim theo token budget (từ mới → cũ)
    let mut result: Vec<(String, String)> = Vec::new();
    let mut total_tokens = 0usize;

    for pair in recent.iter().rev() {
        let pair_tokens = estimate_tokens(&pair.0) + estimate_tokens(&pair.1);
        if total_tokens + pair_tokens > max_tokens && !result.is_empty() {
            break; // Đã đủ budget
        }
        total_tokens += pair_tokens;
        result.push((pair.0.clone(), pair.1.clone()));
    }

    // Reverse lại (đã duyệt ngược)
    result.reverse();

    log::info!(
        "[ContextGuard] History: {} → {} turns, ~{} tokens",
        history.len(),
        result.len(),
        total_tokens
    );

    result
}

// ═══════════════════════════════════════════════════════
// Context Budget
// ═══════════════════════════════════════════════════════

/// Tính budget còn lại cho search results sau khi trừ system_prompt + history
pub fn remaining_budget(
    system_prompt_tokens: usize,
    history_tokens: usize,
) -> usize {
    let used = system_prompt_tokens + history_tokens;
    if used >= MAX_CONTEXT_TOKENS {
        log::warn!(
            "[ContextGuard] Budget exceeded! prompt={} + history={} > max={}",
            system_prompt_tokens,
            history_tokens,
            MAX_CONTEXT_TOKENS
        );
        1000 // Minimum: ít nhất 1 chunk
    } else {
        MAX_CONTEXT_TOKENS - used
    }
}

/// Truncate search context (danh sách chunks) theo token budget
/// Giữ chunks có relevance score CAO NHẤT trước
pub fn truncate_search_context(context: &str, max_tokens: usize) -> String {
    let current_tokens = estimate_tokens(context);
    if current_tokens <= max_tokens {
        return context.to_string();
    }

    // Cắt theo chars (ước tính)
    let max_chars = max_tokens * 4; // ~4 chars/token
    let truncated: String = context.chars().take(max_chars).collect();

    // Tìm vị trí newline cuối cùng để cắt sạch (không cắt giữa chunk)
    if let Some(last_newline) = truncated.rfind('\n') {
        let clean = &truncated[..last_newline];
        log::info!(
            "[ContextGuard] Context truncated: {} → {} tokens (~{} chars)",
            current_tokens,
            estimate_tokens(clean),
            clean.len()
        );
        clean.to_string()
    } else {
        truncated
    }
}

/// Log thống kê context cho monitoring
pub fn log_context_stats(
    system_prompt: &str,
    history: &[(String, String)],
    search_context: &str,
) {
    let prompt_tokens = estimate_tokens(system_prompt);
    let history_tokens: usize = history
        .iter()
        .map(|(r, c)| estimate_tokens(r) + estimate_tokens(c))
        .sum();
    let search_tokens = estimate_tokens(search_context);
    let total = prompt_tokens + history_tokens + search_tokens;

    log::info!(
        "[ContextGuard] Token usage: prompt={}, history={} ({} turns), search={}, TOTAL={}/{} ({:.0}%)",
        prompt_tokens,
        history_tokens,
        history.len(),
        search_tokens,
        total,
        MAX_CONTEXT_TOKENS,
        (total as f64 / MAX_CONTEXT_TOKENS as f64) * 100.0
    );

    if total > MAX_CONTEXT_TOKENS {
        log::warn!(
            "[ContextGuard] ⚠️ OVER BUDGET by {} tokens!",
            total - MAX_CONTEXT_TOKENS
        );
    }
}
