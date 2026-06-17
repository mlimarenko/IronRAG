use crate::domains::query_ir::{LiteralKind, QueryAct, QueryIR, SourceSliceDirection};

const LATEST_VERSION_DEFAULT_COUNT: usize = LATEST_VERSION_MAX_COUNT;
const LATEST_VERSION_MAX_COUNT: usize = 10;
pub(crate) const LATEST_VERSION_CHUNKS_PER_DOCUMENT: usize = 4;

pub(crate) fn query_requests_latest_versions(ir: &QueryIR) -> bool {
    let has_version_literal =
        ir.literal_constraints.iter().any(|literal| matches!(literal.kind, LiteralKind::Version));
    let has_explicit_tail_slice = ir
        .source_slice
        .as_ref()
        .is_some_and(|slice| matches!(slice.direction, SourceSliceDirection::Tail));
    let has_release_inventory_type = ir_target_types_include(ir, &["release", "changelog"]);
    let has_requested_count = ir
        .literal_constraints
        .iter()
        .any(|literal| matches!(literal.kind, LiteralKind::NumericCode));
    matches!(ir.act, QueryAct::Describe | QueryAct::Enumerate | QueryAct::Meta)
        && (has_explicit_tail_slice || has_release_inventory_type || has_requested_count)
        && (!has_version_literal || has_explicit_tail_slice)
        && ir
            .source_slice
            .as_ref()
            .is_none_or(|slice| matches!(slice.direction, SourceSliceDirection::Tail))
}

pub(crate) fn requested_latest_version_count(ir: &QueryIR) -> usize {
    if let Some(count) = ir.source_slice.as_ref().and_then(|slice| slice.count) {
        return usize::from(count).clamp(1, LATEST_VERSION_MAX_COUNT);
    }
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
    terms.into_iter().filter(|token| token.chars().count() >= 3).collect()
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
        let normalized = target_type.trim().to_ascii_lowercase();
        tags.iter().any(|tag| normalized == *tag)
    })
}

pub(crate) fn extract_semver_like_version(text: &str) -> Option<Vec<u32>> {
    semver_like_version_candidates(text).into_iter().next()
}

fn semver_like_version_candidates(text: &str) -> Vec<Vec<u32>> {
    let chars = text.char_indices().collect::<Vec<_>>();
    let mut versions = Vec::new();
    for (index, &(start, ch)) in chars.iter().enumerate() {
        if !ch.is_ascii_digit() {
            continue;
        }
        if index > 0 {
            let previous = chars[index - 1].1;
            if previous.is_ascii_digit() || previous == '.' {
                continue;
            }
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
            .ok();
        let Some(parts) = parts else {
            continue;
        };
        if version_parts_are_release_like(&parts) {
            versions.push(parts);
        }
    }
    versions
}

pub(crate) fn extract_release_context_version(text: &str) -> Option<Vec<u32>> {
    let mut line_start_fallback = None;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        for version in semver_like_version_candidates(line) {
            if !version_parts_are_ipv4_like(&version) {
                return Some(version);
            }
        }
        if line_start_fallback.is_none()
            && let Some(version) = extract_line_start_version(line)
            && !version_parts_are_ipv4_like(&version)
        {
            line_start_fallback = Some(version);
        }
    }
    line_start_fallback
}

fn extract_line_start_version(line: &str) -> Option<Vec<u32>> {
    let trimmed = line.trim_start();
    let version_start =
        trimmed.char_indices().find_map(|(index, ch)| ch.is_ascii_digit().then_some(index))?;
    if trimmed[..version_start].chars().any(|ch| !ch.is_whitespace() && !matches!(ch, 'v' | 'V')) {
        return None;
    }
    extract_semver_like_version(&trimmed[version_start..])
}

fn version_parts_are_release_like(parts: &[u32]) -> bool {
    parts.len() >= 2
        && !parts.first().is_some_and(|part| (1900..=2100).contains(part))
        && !version_parts_are_calendar_like(parts)
}

fn version_parts_are_calendar_like(parts: &[u32]) -> bool {
    if parts.len() != 3 {
        return false;
    }
    let [first, second, third] = [parts[0], parts[1], parts[2]];
    (1900..=2100).contains(&third)
        && (1..=31).contains(&first)
        && (1..=31).contains(&second)
        && (first <= 12 || second <= 12)
}

fn version_parts_are_ipv4_like(parts: &[u32]) -> bool {
    parts.len() == 4 && parts.iter().all(|part| *part <= 255)
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

#[cfg(test)]
mod tests {
    use super::extract_release_context_version;

    #[test]
    fn release_context_version_extractor_rejects_dates_and_plain_ip_addresses() {
        assert_eq!(
            extract_release_context_version("Build 9.8.765 - Product"),
            Some(vec![9, 8, 765])
        );
        assert_eq!(
            extract_release_context_version("2.4.259 (25.06.2024) - fixed flow"),
            Some(vec![2, 4, 259])
        );
        assert_eq!(
            extract_release_context_version("Build 10.0.2000 - Product"),
            Some(vec![10, 0, 2000])
        );
        assert_eq!(
            extract_release_context_version("Host 10.0.1.108 carries build 9.8.765"),
            Some(vec![9, 8, 765])
        );
        assert_eq!(extract_release_context_version("Server 10.0.1.108 is reachable"), None);
        assert_eq!(extract_release_context_version("Screenshot 06.15.2026"), None);
    }
}
