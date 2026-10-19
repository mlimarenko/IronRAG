//! Best-effort excerpt extraction for `search_documents` hit previews:
//! given a chunk of document text and a query, find the most
//! relevant slice to show the agent (exact phrase, then keyword, then a
//! head fallback) without ever panicking on multi-byte UTF-8 boundaries.
//!
//! Split out of the former `services/mcp/support.rs` god-file (plan
//! §6.4): this domain has exactly one caller,
//! [`super::access::types`], and no relation to the continuation-token
//! or mutation-idempotency helpers that used to share the file.

use unicode_normalization::{UnicodeNormalization, char::is_combining_mark};

const EXCERPT_CONTEXT_BEFORE: usize = 80;
const EXCERPT_CONTEXT_AFTER: usize = 200;
const EXCERPT_EXACT_MATCH_SCORE_FLOOR: f64 = 0.9;
const EXCERPT_TOKEN_MATCH_SCORE_FLOOR: f64 = 0.6;
const EXCERPT_POSITION_BONUS: f64 = 0.1;

#[derive(Debug, Clone, Copy)]
struct NormalizedOffsetRun {
    normalized_start: usize,
    normalized_end: usize,
    original_start: usize,
    original_end: usize,
    byte_linear: bool,
}

#[derive(Debug)]
struct NormalizedSearchText {
    value: String,
    offset_runs: Vec<NormalizedOffsetRun>,
}

impl NormalizedSearchText {
    /// Maps a non-empty byte range in the normalized value back to the
    /// complete original UTF-8 scalar range that produced it. A normalized
    /// scalar may be shorter than its source (`é` -> `e`) or several scalars
    /// may come from one source scalar, so non-linear runs conservatively map
    /// to the whole source scalar rather than inventing an interior boundary.
    fn original_range(&self, start: usize, end: usize) -> Option<(usize, usize)> {
        if start >= end || end > self.value.len() {
            return None;
        }
        let first_index = self.offset_runs.partition_point(|run| run.normalized_end <= start);
        let first = self.offset_runs.get(first_index)?;
        let last_index = self.offset_runs.partition_point(|run| run.normalized_start < end);
        let last = self.offset_runs.get(last_index.checked_sub(1)?)?;
        if start < first.normalized_start || end > last.normalized_end {
            return None;
        }

        let original_start = if first.byte_linear {
            first.original_start + (start - first.normalized_start)
        } else {
            first.original_start
        };
        let original_end = if last.byte_linear {
            last.original_start + (end - last.normalized_start)
        } else {
            last.original_end
        };
        Some((original_start, original_end))
    }
}

/// Applies the language-neutral subset shared by PostgreSQL's `simple` FTS
/// configuration and its `unaccent` dictionary: compatibility decomposition,
/// lowercase expansion, and combining-mark removal. Offset runs retain the
/// exact original scalar boundaries across every length-changing transform.
///
/// PostgreSQL's configured `unaccent.rules` also contains dictionary-specific
/// substitutions that Unicode decomposition cannot reproduce universally. If
/// one of those is the only reason FTS matched, `preview_hit` deliberately uses
/// its safe head fallback; it never guesses an offset into the original text.
fn normalize_for_search(value: &str) -> NormalizedSearchText {
    let mut normalized = String::with_capacity(value.len());
    let mut offset_runs = Vec::<NormalizedOffsetRun>::new();

    for (original_start, source) in value.char_indices() {
        let original_end = original_start + source.len_utf8();
        let normalized_start = normalized.len();
        let mut emitted_scalars = 0usize;
        for normalized_scalar in std::iter::once(source).nfkd().flat_map(char::to_lowercase).nfkd()
        {
            if is_combining_mark(normalized_scalar) {
                continue;
            }
            normalized.push(normalized_scalar);
            emitted_scalars += 1;
        }
        let normalized_end = normalized.len();
        if normalized_start == normalized_end {
            continue;
        }

        let byte_linear = emitted_scalars == 1
            && normalized_end - normalized_start == original_end - original_start;
        let extends_linear_run = byte_linear
            && offset_runs.last().is_some_and(|last| {
                last.byte_linear
                    && last.normalized_end == normalized_start
                    && last.original_end == original_start
            });
        match offset_runs.last_mut() {
            Some(last) if extends_linear_run => {
                last.normalized_end = normalized_end;
                last.original_end = original_end;
            }
            _ => offset_runs.push(NormalizedOffsetRun {
                normalized_start,
                normalized_end,
                original_start,
                original_end,
                byte_linear,
            }),
        }
    }

    NormalizedSearchText { value: normalized, offset_runs }
}

/// Walks back/forward to valid UTF-8 boundaries and adjusts for the whitespace
/// removed from the returned excerpt, keeping offsets aligned with its actual
/// contents.
fn safe_trimmed_excerpt(text: &str, start: usize, end: usize) -> (&str, usize, usize) {
    if text.is_empty() {
        return ("", 0, 0);
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

    let slice = &text[s..e];
    let trimmed_start = slice.trim_start();
    s += slice.len() - trimmed_start.len();
    let trimmed = trimmed_start.trim_end();
    e = s + trimmed.len();
    (trimmed, s, e)
}

fn excerpt_around_match(
    text: &str,
    match_start: usize,
    match_end: usize,
    context_before: usize,
    context_after: usize,
    score: f64,
) -> Option<(String, usize, usize, f64)> {
    let excerpt_start = match_start.saturating_sub(context_before);
    let excerpt_end = match_end.saturating_add(context_after).min(text.len());
    let (slice, excerpt_start, excerpt_end) =
        safe_trimmed_excerpt(text, excerpt_start, excerpt_end);
    if slice.is_empty() {
        return None;
    }
    Some((slice.to_string(), excerpt_start, excerpt_end, score))
}

/// Keeps every real match above the head-fallback score while still giving a
/// small, bounded preference to matches nearer the start of the document.
/// Dividing the entire score by a byte offset made a late exact match rank
/// below the `0.1` fallback even though it was the stronger result.
fn matched_excerpt_score(score_floor: f64, original_start: usize) -> f64 {
    score_floor + EXCERPT_POSITION_BONUS / (1.0 + original_start as f64)
}

pub(crate) fn preview_hit(text: &str, query: &str) -> Option<(String, usize, usize, f64)> {
    if text.trim().is_empty() {
        return None;
    }
    let normalized_text = normalize_for_search(text);
    let normalized_query = normalize_for_search(query).value;

    // 1. Exact-phrase match wins — gives the tightest excerpt around the
    //    phrase boundaries.
    if !normalized_query.is_empty()
        && let Some(start) = normalized_text.value.find(&normalized_query)
    {
        let end = start.saturating_add(normalized_query.len());
        if let Some((original_start, original_end)) = normalized_text.original_range(start, end) {
            let score = matched_excerpt_score(EXCERPT_EXACT_MATCH_SCORE_FLOOR, original_start);
            return excerpt_around_match(text, original_start, original_end, 80, 160, score);
        }
    }

    // 2. Token fallback — the first query word that actually appears in
    //    the chunk anchors the excerpt. This covers vector hits where
    //    the full phrase is absent but a keyword is present, and stops
    //    `excerpt` from going null on semantically-similar chunks.
    for raw_word in normalized_query.split_whitespace() {
        let word = raw_word.trim_matches(|c: char| !c.is_alphanumeric());
        if word.chars().count() < 3 {
            continue;
        }
        if let Some(start) = normalized_text.value.find(word) {
            let end = start.saturating_add(word.len());
            let Some((original_start, original_end)) = normalized_text.original_range(start, end)
            else {
                continue;
            };
            let score = matched_excerpt_score(EXCERPT_TOKEN_MATCH_SCORE_FLOOR, original_start);
            return excerpt_around_match(
                text,
                original_start,
                original_end,
                EXCERPT_CONTEXT_BEFORE,
                EXCERPT_CONTEXT_AFTER,
                score,
            );
        }
    }

    // 3. Last resort — return the head of the chunk so the agent has at
    //    least something to decide whether to read further.
    let (slice, start, end) = safe_trimmed_excerpt(text, 0, 240);
    if slice.is_empty() {
        return None;
    }
    Some((slice.to_string(), start, end, 0.1))
}

#[cfg(test)]
mod tests {
    use super::{normalize_for_search, preview_hit};

    #[test]
    fn accent_insensitive_match_keeps_original_excerpt_boundaries() {
        let text = format!("{}Résumé details", "x".repeat(100));

        let (excerpt, start, end, score) = preview_hit(&text, "resume").expect("preview");

        assert!(excerpt.contains("Résumé"));
        assert_eq!(start, 20);
        assert_eq!(&text[start..end], excerpt);
        assert!(score > 0.1);
    }

    #[test]
    fn lowercase_expansion_before_match_does_not_shift_original_window() {
        let text = format!("{}search-anchor suffix", "İ".repeat(100));

        let (excerpt, start, end, score) = preview_hit(&text, "SEARCH-ANCHOR").expect("preview");

        assert!(excerpt.contains("search-anchor"));
        assert_eq!(start, 120);
        assert!(text.is_char_boundary(start));
        assert!(text.is_char_boundary(end));
        assert_eq!(&text[start..end], excerpt);
        assert!(score > 0.1);
    }

    #[test]
    fn lowercase_expansion_inside_match_maps_to_whole_source_scalar() {
        let text = format!("{}AİM tail", "x".repeat(100));
        let normalized = normalize_for_search(&text);
        let match_start = normalized.value.find("aim").expect("normalized match");
        let match_end = match_start + "aim".len();

        assert_eq!(normalized.original_range(match_start, match_end), Some((100, 104)));

        let (excerpt, start, end, score) = preview_hit(&text, "aim").expect("preview");
        assert!(excerpt.contains("AİM"));
        assert_eq!(&text[start..end], excerpt);
        assert!(score > 0.1);
    }

    #[test]
    fn head_fallback_never_returns_invalid_utf8_offsets() {
        let text = format!("  {} tail", "界".repeat(100));

        let (excerpt, start, end, score) = preview_hit(&text, "absent").expect("preview");

        assert!(text.is_char_boundary(start));
        assert!(text.is_char_boundary(end));
        assert_eq!(&text[start..end], excerpt);
        assert_eq!(score, 0.1);
    }
}
