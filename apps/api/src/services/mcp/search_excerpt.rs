//! Best-effort excerpt extraction for `search_documents` hit previews:
//! given a chunk of document text and a lowercased query, find the most
//! relevant slice to show the agent (exact phrase, then keyword, then a
//! head fallback) without ever panicking on multi-byte UTF-8 boundaries.
//!
//! Split out of the former `services/mcp/support.rs` god-file (plan
//! §6.4): this domain has exactly one caller,
//! [`super::access::types`], and no relation to the continuation-token
//! or mutation-idempotency helpers that used to share the file.

const EXCERPT_CONTEXT_BEFORE: usize = 80;
const EXCERPT_CONTEXT_AFTER: usize = 200;
const EXCERPT_BASE_RELEVANCE_SCORE: f64 = 0.7;

/// Slices `text[start..end]` but walks back/forward to the nearest valid
/// UTF-8 boundary so Cyrillic, CJK, or accented content does not panic
/// in the middle of a multi-byte codepoint. Used by every excerpt
/// branch below.
fn safe_substring(text: &str, start: usize, end: usize) -> &str {
    if text.is_empty() {
        return "";
    }
    let mut s = start.min(text.len());
    while s > 0 && !text.is_char_boundary(s) {
        s -= 1;
    }
    let mut e = end.min(text.len());
    if e < s {
        e = s;
    }
    while e < text.len() && !text.is_char_boundary(e) {
        e += 1;
    }
    &text[s..e]
}

pub(crate) fn preview_hit(text: &str, query_lower: &str) -> Option<(String, usize, usize, f64)> {
    if text.trim().is_empty() {
        return None;
    }
    let text_lower = text.to_lowercase();

    // 1. Exact-phrase match wins — gives the tightest excerpt around the
    //    phrase boundaries.
    if let Some(start) = text_lower.find(query_lower) {
        let end = start.saturating_add(query_lower.len());
        let excerpt_start = start.saturating_sub(80);
        let excerpt_end = end + 160;
        let slice = safe_substring(text, excerpt_start, excerpt_end);
        let score = 1.0f64 / (1.0 + start as f64);
        return Some((slice.trim().to_string(), excerpt_start, excerpt_end.min(text.len()), score));
    }

    // 2. Token fallback — the first query word that actually appears in
    //    the chunk anchors the excerpt. This covers vector hits where
    //    the full phrase is absent but a keyword is present, and stops
    //    `excerpt` from going null on semantically-similar chunks.
    for raw_word in query_lower.split_whitespace() {
        let word = raw_word.trim_matches(|c: char| !c.is_alphanumeric());
        if word.chars().count() < 3 {
            continue;
        }
        if let Some(start) = text_lower.find(word) {
            let end = start.saturating_add(word.len());
            let excerpt_start = start.saturating_sub(EXCERPT_CONTEXT_BEFORE);
            let excerpt_end = end + EXCERPT_CONTEXT_AFTER;
            let slice = safe_substring(text, excerpt_start, excerpt_end);
            let score = EXCERPT_BASE_RELEVANCE_SCORE / (1.0 + start as f64);
            return Some((
                slice.trim().to_string(),
                excerpt_start,
                excerpt_end.min(text.len()),
                score,
            ));
        }
    }

    // 3. Last resort — return the head of the chunk so the agent has at
    //    least something to decide whether to read further.
    let slice = safe_substring(text, 0, 240);
    let excerpt = slice.trim().to_string();
    if excerpt.is_empty() {
        return None;
    }
    Some((excerpt, 0, slice.len(), 0.1))
}
