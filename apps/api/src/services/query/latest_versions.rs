use crate::domains::query_ir::{LiteralKind, QueryAct, QueryIR};

const LATEST_VERSION_DEFAULT_COUNT: usize = 5;
const LATEST_VERSION_MAX_COUNT: usize = 10;
pub(crate) const LATEST_VERSION_CHUNKS_PER_DOCUMENT: usize = 4;

pub(crate) fn query_requests_latest_versions(ir: &QueryIR) -> bool {
    matches!(ir.act, QueryAct::Describe | QueryAct::Enumerate | QueryAct::Meta)
        && ir_target_types_include(ir, &["release", "version", "changelog", "change_log"])
}

pub(crate) fn requested_latest_version_count(ir: &QueryIR) -> usize {
    for literal in &ir.literal_constraints {
        if !matches!(literal.kind, LiteralKind::NumericCode) {
            continue;
        };
        let Ok(value) = literal.text.parse::<usize>() else {
            continue;
        };
        if value == 0 || (1900..=2100).contains(&value) {
            continue;
        }
        return value.clamp(1, LATEST_VERSION_MAX_COUNT);
    }
    LATEST_VERSION_DEFAULT_COUNT
}

pub(crate) fn latest_version_context_top_k(ir: &QueryIR, base_limit: usize) -> usize {
    if !query_requests_latest_versions(ir) {
        return base_limit;
    }
    base_limit
        .max(requested_latest_version_count(ir).saturating_mul(LATEST_VERSION_CHUNKS_PER_DOCUMENT))
}

pub(crate) fn latest_version_chunk_score(
    score_floor: f32,
    requested_count: usize,
    document_rank: usize,
    chunk_rank: usize,
) -> f32 {
    let band = LATEST_VERSION_CHUNKS_PER_DOCUMENT.saturating_sub(chunk_rank).max(1);
    let offset = band.saturating_mul(requested_count).saturating_sub(document_rank);
    score_floor + offset as f32
}

pub(crate) fn latest_version_scope_terms(ir: &QueryIR) -> Vec<String> {
    let mut terms = Vec::new();
    for entity in &ir.target_entities {
        terms.extend(lexical_tokens(&entity.label));
    }
    if let Some(document_focus) = &ir.document_focus {
        terms.extend(lexical_tokens(&document_focus.hint));
    }
    terms.extend(
        ir.literal_constraints
            .iter()
            .filter(|literal| {
                !matches!(literal.kind, LiteralKind::Version | LiteralKind::NumericCode)
            })
            .flat_map(|literal| lexical_tokens(&literal.text)),
    );
    terms
        .into_iter()
        .filter(|token| token.chars().count() >= 3)
        .filter(|token| !token.chars().any(|ch| ch.is_ascii_digit()))
        .collect()
}

pub(crate) fn latest_version_family_key(text: &str) -> String {
    let lower = text.to_lowercase();
    let chars = lower.chars().collect::<Vec<_>>();
    let mut index = 0;
    let mut out = String::with_capacity(lower.len());
    while index < chars.len() {
        let ch = chars[index];
        if ch.is_ascii_digit() {
            let start = index;
            let mut end = index + 1;
            let mut has_dot = false;
            while end < chars.len() && (chars[end].is_ascii_digit() || chars[end] == '.') {
                if chars[end] == '.' {
                    has_dot = true;
                }
                end += 1;
            }
            if has_dot {
                out.push_str("{version}");
                index = end;
                continue;
            }
            out.extend(chars[start..end].iter());
            index = end;
            continue;
        }
        out.push(ch);
        index += 1;
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub(crate) fn text_has_release_version_marker(text: &str) -> bool {
    extract_semver_like_version(text).is_some()
}

fn lexical_tokens(query: &str) -> Vec<String> {
    query
        .to_lowercase()
        .split(|ch: char| !(ch.is_alphanumeric() || ch == '.'))
        .map(|token| token.trim_matches('.'))
        .filter(|token| !token.is_empty())
        .map(str::to_string)
        .collect()
}

fn ir_target_types_include(ir: &QueryIR, tags: &[&str]) -> bool {
    ir.target_types.iter().any(|target_type| {
        let normalized = target_type.trim().to_ascii_lowercase().replace('-', "_");
        tags.iter().any(|tag| normalized == *tag)
    })
}

pub(crate) fn extract_semver_like_version(text: &str) -> Option<Vec<u32>> {
    let chars = text.char_indices().collect::<Vec<_>>();
    for (index, &(start, ch)) in chars.iter().enumerate() {
        if !ch.is_ascii_digit() {
            continue;
        }
        let mut end = start + ch.len_utf8();
        for &(_, next) in chars.iter().skip(index + 1) {
            if next.is_ascii_digit() || next == '.' {
                end += next.len_utf8();
            } else {
                break;
            }
        }
        let candidate = text[start..end].trim_matches('.');
        let parts = candidate
            .split('.')
            .filter(|part| !part.is_empty())
            .map(str::parse::<u32>)
            .collect::<Result<Vec<_>, _>>()
            .ok()?;
        if parts.len() >= 2 {
            return Some(parts);
        }
    }
    None
}

pub(crate) fn compare_version_desc(left: &[u32], right: &[u32]) -> std::cmp::Ordering {
    let len = left.len().max(right.len());
    for index in 0..len {
        let left_part = left.get(index).copied().unwrap_or(0);
        let right_part = right.get(index).copied().unwrap_or(0);
        match right_part.cmp(&left_part) {
            std::cmp::Ordering::Equal => continue,
            ordering => return ordering,
        }
    }
    std::cmp::Ordering::Equal
}
