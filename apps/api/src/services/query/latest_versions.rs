use crate::domains::query_ir::{
    LiteralKind, QueryAct, QueryIR, SourceSliceDirection, SourceSliceFilter,
};
use uuid::Uuid;

pub(crate) const LATEST_VERSION_DEFAULT_COUNT: usize = 10;
pub(crate) const LATEST_VERSION_MAX_COUNT: usize = 20;
pub(crate) const LATEST_VERSION_CHUNKS_PER_DOCUMENT: usize = 4;

/// Canonical provenance for release evidence. Titles are intentionally absent:
/// two records are the same source only when the content model says they are
/// the same document revision.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct ReleaseSourceIdentity {
    document_id: Uuid,
    revision_id: Uuid,
}

impl ReleaseSourceIdentity {
    #[must_use]
    pub(crate) const fn new(document_id: Uuid, revision_id: Uuid) -> Self {
        Self { document_id, revision_id }
    }
}

pub(crate) fn query_requests_latest_versions(ir: &QueryIR) -> bool {
    let has_typed_release_tail_slice = ir.source_slice.as_ref().is_some_and(|slice| {
        matches!(slice.direction, SourceSliceDirection::Tail)
            && matches!(slice.filter, SourceSliceFilter::ReleaseMarker)
    });
    matches!(ir.act, QueryAct::Describe | QueryAct::Enumerate | QueryAct::Meta)
        && has_typed_release_tail_slice
}

pub(crate) fn requested_latest_version_count(ir: &QueryIR) -> usize {
    if let Some(count) = ir.source_slice.as_ref().and_then(|slice| slice.count) {
        return usize::from(count).clamp(1, LATEST_VERSION_MAX_COUNT);
    }
    for literal in &ir.literal_constraints {
        if !matches!(literal.kind, LiteralKind::NumericCode) {
            continue;
        }
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

pub(crate) fn text_has_release_version_marker(text: &str) -> bool {
    extract_semver_like_version(text).is_some() && !text_has_directional_version_transition(text)
}

fn text_has_directional_version_transition(text: &str) -> bool {
    semver_like_version_candidates_with_spans(text)
        .windows(2)
        .any(|pair| formal_directional_separator(&text[pair[0].end..pair[1].start]))
}

fn formal_directional_separator(separator: &str) -> bool {
    separator.contains("->")
        || separator.contains("=>")
        || separator.contains('→')
        || separator.contains('⇒')
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

pub(crate) fn extract_semver_like_version(text: &str) -> Option<Vec<u32>> {
    semver_like_version_candidates(text).into_iter().next()
}

fn semver_like_version_candidates(text: &str) -> Vec<Vec<u32>> {
    semver_like_version_candidates_with_spans(text)
        .into_iter()
        .map(|candidate| candidate.parts)
        .collect()
}

struct SemverLikeVersionCandidate {
    start: usize,
    end: usize,
    parts: Vec<u32>,
}

fn semver_like_version_candidates_with_spans(text: &str) -> Vec<SemverLikeVersionCandidate> {
    let mut versions = Vec::new();
    let mut cursor = 0usize;
    while let Some((token_start, token_end)) = next_version_token_span(text, cursor) {
        cursor = token_end;
        if let Some(candidate) = semver_like_candidate_from_span(text, token_start, token_end) {
            versions.push(candidate);
        }
    }
    versions
}

fn next_version_token_span(text: &str, mut cursor: usize) -> Option<(usize, usize)> {
    while cursor < text.len() {
        let ch = text[cursor..].chars().next()?;
        if is_version_token_continuation(ch) {
            let token_start = cursor;
            cursor += ch.len_utf8();
            while cursor < text.len() {
                let Some(next) = text[cursor..].chars().next() else {
                    break;
                };
                if !is_version_token_continuation(next) {
                    break;
                }
                cursor += next.len_utf8();
            }
            return Some((token_start, cursor));
        }
        cursor += ch.len_utf8();
    }
    None
}

fn semver_like_candidate_from_span(
    text: &str,
    token_start: usize,
    token_end: usize,
) -> Option<SemverLikeVersionCandidate> {
    let parse_end = if text.as_bytes().get(token_end.wrapping_sub(1)) == Some(&b'.')
        && trailing_period_is_version_punctuation(text, token_end)
    {
        token_end - 1
    } else {
        token_end
    };
    let parsed = parse_semver_like_token(text.get(token_start..parse_end)?)?;
    version_parts_are_release_like(&parsed.parts).then_some(SemverLikeVersionCandidate {
        start: token_start + parsed.numeric_start,
        end: token_start + parsed.numeric_end,
        parts: parsed.parts,
    })
}

pub(crate) fn is_version_token_continuation(ch: char) -> bool {
    ch.is_alphanumeric() || matches!(ch, '_' | '.' | '-' | '+')
}

pub(crate) fn prefixed_semver_like_literal_at(value: &str, start: usize) -> Option<&str> {
    let marker = value.get(start..)?.chars().next()?;
    if !matches!(marker, 'v' | 'V')
        || value.get(..start)?.chars().next_back().is_some_and(is_version_token_continuation)
    {
        return None;
    }

    let mut token_end = start + marker.len_utf8();
    while let Some(ch) = value.get(token_end..)?.chars().next() {
        if !is_version_token_continuation(ch) {
            break;
        }
        token_end += ch.len_utf8();
    }
    let parse_end = if value.as_bytes().get(token_end.checked_sub(1)?) == Some(&b'.')
        && trailing_period_is_version_punctuation(value, token_end)
    {
        token_end - 1
    } else {
        token_end
    };
    let parsed = parse_semver_like_token(value.get(start..parse_end)?)?;
    (parsed.numeric_start == 1 && parsed.parts.len() >= 2).then(|| &value[start..parse_end])
}

fn trailing_period_is_version_punctuation(text: &str, token_end: usize) -> bool {
    text[token_end..].chars().next().is_none_or(|ch| {
        ch.is_whitespace() || matches!(ch, ')' | ']' | '}' | '>' | '\'' | '"' | '`')
    })
}

struct ParsedSemverLikeToken {
    numeric_start: usize,
    numeric_end: usize,
    parts: Vec<u32>,
}

fn parse_semver_like_token(token: &str) -> Option<ParsedSemverLikeToken> {
    let numeric_start = if token.starts_with(['v', 'V']) { 1 } else { 0 };
    if !token[numeric_start..].starts_with(|ch: char| ch.is_ascii_digit()) {
        return None;
    }

    let mut cursor = numeric_start;
    let mut parts = Vec::new();
    loop {
        let part_start = cursor;
        while cursor < token.len() && token[cursor..].starts_with(|ch: char| ch.is_ascii_digit()) {
            cursor += 1;
        }
        if cursor == part_start {
            return None;
        }
        parts.push(token[part_start..cursor].parse::<u32>().ok()?);

        let Some(after_dot) = token[cursor..].strip_prefix('.') else {
            break;
        };
        if !after_dot.starts_with(|ch: char| ch.is_ascii_digit()) {
            break;
        }
        cursor += 1;
    }

    if !semver_decoration_is_valid(&token[cursor..]) {
        return None;
    }
    Some(ParsedSemverLikeToken { numeric_start, numeric_end: cursor, parts })
}

fn semver_decoration_is_valid(decoration: &str) -> bool {
    if decoration.is_empty() {
        return true;
    }
    if let Some(prerelease_and_build) = decoration.strip_prefix('-') {
        return prerelease_and_build.split_once('+').map_or_else(
            || semver_identifier_sequence_is_valid(prerelease_and_build),
            |(prerelease, build)| {
                semver_identifier_sequence_is_valid(prerelease)
                    && semver_identifier_sequence_is_valid(build)
            },
        );
    }
    decoration.strip_prefix('+').is_some_and(semver_identifier_sequence_is_valid)
}

fn semver_identifier_sequence_is_valid(sequence: &str) -> bool {
    sequence.split('.').all(|identifier| {
        !identifier.is_empty() && identifier.chars().all(|ch| ch.is_alphanumeric() || ch == '-')
    })
}

pub(crate) fn extract_release_context_version(text: &str) -> Option<Vec<u32>> {
    // Release semantics belong to the typed query/source-slice contract. This
    // extractor deliberately considers only language-neutral syntax.
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(version) = extract_structural_release_version(line) {
            return Some(version);
        }
    }
    None
}

fn extract_structural_release_version(line: &str) -> Option<Vec<u32>> {
    let candidates = semver_like_version_candidates_with_spans(line);
    let group_start = candidates
        .iter()
        .enumerate()
        .find(|(_, candidate)| candidate_is_structural_release(line, candidate))
        .map(|(index, _)| index)?;

    let mut group_end = group_start + 1;
    while group_end < candidates.len()
        && compound_version_separator(
            &line[candidates[group_end - 1].end..candidates[group_end].start],
        )
        && candidate_is_structural_release(line, &candidates[group_end])
    {
        group_end += 1;
    }

    candidates[group_start..group_end]
        .iter()
        .min_by(|left, right| compare_version_desc(&left.parts, &right.parts))
        .map(|candidate| candidate.parts.clone())
}

fn candidate_is_structural_release(line: &str, candidate: &SemverLikeVersionCandidate) -> bool {
    !candidate_is_nested(line, candidate.start)
        && (!version_parts_are_ipv4_like(&candidate.parts)
            || candidate_has_formal_version_prefix(line, candidate.start))
}

fn compound_version_separator(separator: &str) -> bool {
    let separator = separator.trim();
    !separator.is_empty() && separator.chars().all(|ch| matches!(ch, '|' | '/'))
}

fn candidate_is_nested(line: &str, candidate_start: usize) -> bool {
    let mut delimiters = Vec::new();
    for ch in line[..candidate_start].chars() {
        match ch {
            '(' | '[' | '{' => delimiters.push(ch),
            ')' if delimiters.last() == Some(&'(') => {
                delimiters.pop();
            }
            ']' if delimiters.last() == Some(&'[') => {
                delimiters.pop();
            }
            '}' if delimiters.last() == Some(&'{') => {
                delimiters.pop();
            }
            _ => {}
        }
    }
    !delimiters.is_empty()
}

fn candidate_has_formal_version_prefix(line: &str, candidate_start: usize) -> bool {
    let prefix = line[..candidate_start].trim_end();
    let Some(without_marker) = prefix.strip_suffix('v').or_else(|| prefix.strip_suffix('V')) else {
        return false;
    };
    without_marker.chars().next_back().is_none_or(|ch| !is_version_token_continuation(ch))
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
    use crate::domains::query_ir::{
        LiteralKind, LiteralSpan, QueryAct, QueryIR, QueryLanguage, QueryScope,
        SourceSliceDirection, SourceSliceFilter, SourceSliceSpec,
    };

    use super::{
        LATEST_VERSION_CHUNKS_PER_DOCUMENT, extract_release_context_version,
        extract_semver_like_version, latest_version_context_top_k, query_requests_latest_versions,
        requested_latest_version_count, text_has_release_version_marker,
    };

    fn release_inventory_ir(source_slice: Option<SourceSliceSpec>) -> QueryIR {
        QueryIR {
            act: QueryAct::Enumerate,
            scope: QueryScope::MultiDocument,
            language: QueryLanguage::En,
            target_types: vec![
                crate::domains::query_ir::QueryTargetKind::Release,
                crate::domains::query_ir::QueryTargetKind::Version,
            ],
            target_entities: Vec::new(),
            literal_constraints: Vec::new(),
            temporal_constraints: Vec::new(),
            comparison: None,
            document_focus: None,
            conversation_refs: Vec::new(),
            needs_clarification: None,
            source_slice,
            retrieval_query: None,
            confidence: 1.0,
        }
    }

    fn release_tail_slice(count: Option<u16>) -> SourceSliceSpec {
        SourceSliceSpec {
            direction: SourceSliceDirection::Tail,
            count,
            filter: SourceSliceFilter::ReleaseMarker,
        }
    }

    #[test]
    fn latest_version_count_preserves_explicit_twenty_item_source_slice() {
        let ir = release_inventory_ir(Some(release_tail_slice(Some(20))));

        assert_eq!(requested_latest_version_count(&ir), 20);
        assert_eq!(latest_version_context_top_k(&ir, 12), 20 * LATEST_VERSION_CHUNKS_PER_DOCUMENT);
        assert_eq!(latest_version_context_top_k(&ir, 96), 96);
    }

    #[test]
    fn latest_inventory_requires_typed_release_slice_instead_of_target_words() {
        assert!(!query_requests_latest_versions(&release_inventory_ir(None)));

        let mut typed = release_inventory_ir(Some(release_tail_slice(Some(4))));
        typed.target_types = vec![crate::domains::query_ir::QueryTargetKind::Concept];
        assert!(query_requests_latest_versions(&typed));
    }

    #[test]
    fn latest_version_count_preserves_twenty_item_numeric_literal() {
        let mut ir = release_inventory_ir(None);
        ir.literal_constraints
            .push(LiteralSpan { text: "20".to_string(), kind: LiteralKind::NumericCode });

        assert_eq!(requested_latest_version_count(&ir), 20);
    }

    #[test]
    fn latest_version_count_clamps_explicit_values_above_twenty() {
        let ir = release_inventory_ir(Some(release_tail_slice(Some(21))));

        assert_eq!(requested_latest_version_count(&ir), 20);
    }

    #[test]
    fn latest_version_count_defaults_to_ten_when_count_is_absent() {
        let ir = release_inventory_ir(Some(release_tail_slice(None)));

        assert_eq!(requested_latest_version_count(&ir), 10);
    }

    #[test]
    fn release_context_version_extractor_uses_only_structural_version_candidates() {
        assert_eq!(extract_release_context_version("alpha 9.8.765 - omega"), Some(vec![9, 8, 765]));
        assert_eq!(
            extract_release_context_version("7.8.901 (02.01.2037) - delta"),
            Some(vec![7, 8, 901])
        );
        assert_eq!(
            extract_release_context_version("alpha 10.0.2000 - omega"),
            Some(vec![10, 0, 2000])
        );
        assert_eq!(extract_release_context_version("10.0.1.108 :: 9.8.765"), Some(vec![9, 8, 765]));
        assert_eq!(extract_release_context_version("10.0.1.108"), None);
        assert_eq!(extract_release_context_version("06.15.2026"), None);
    }

    #[test]
    fn release_context_version_extractor_prefers_top_level_candidate_over_nested_metadata() {
        assert_eq!(
            extract_release_context_version("alpha (31.14) - 7.8.321 (03.02.2038)"),
            Some(vec![7, 8, 321])
        );
    }

    #[test]
    fn release_context_version_extractor_rejects_nested_only_candidate() {
        assert_eq!(extract_release_context_version("alpha (31.14)"), None);
    }

    #[test]
    fn release_context_version_extractor_uses_newest_direct_compound_version() {
        assert_eq!(
            extract_release_context_version("alpha 7.8 | 12.407 (04.03.2039)"),
            Some(vec![12, 407])
        );
    }

    #[test]
    fn release_context_version_extractor_requires_formal_prefix_for_ipv4_shaped_version() {
        assert_eq!(extract_release_context_version("v1.2.3.4"), Some(vec![1, 2, 3, 4]));
        assert_eq!(extract_release_context_version("alpha 1.2.3.4"), None);
    }

    #[test]
    fn release_context_version_extractor_prefers_first_top_level_candidate() {
        assert_eq!(
            extract_release_context_version("alpha 7.8.654 :: beta 33.42.02"),
            Some(vec![7, 8, 654])
        );
    }

    #[test]
    fn release_context_version_extractor_is_invariant_to_arbitrary_prefix_text() {
        assert_eq!(
            extract_release_context_version("alpha 7.8.654 :: 33.42.02"),
            extract_release_context_version("任意 7.8.654 :: 33.42.02")
        );
    }

    #[test]
    fn release_version_marker_rejects_only_formal_directional_transitions() {
        for text in ["1.2.3 -> 2.0.0", "1.2.3 => 2.0.0", "1.2.3 → 2.0.0", "1.2.3 ⇒ 2.0.0"] {
            assert!(!text_has_release_version_marker(text), "{text}");
        }
        assert!(text_has_release_version_marker("1.2.3 alpha 2.0.0"));
    }

    #[test]
    fn release_context_version_extractor_uses_first_top_level_candidate_after_nested_prefix() {
        assert_eq!(
            extract_release_context_version("alpha (31.14) - 7.8.321"),
            Some(vec![7, 8, 321])
        );
    }

    #[test]
    fn semver_extractor_validates_the_whole_decorated_token() {
        assert_eq!(extract_semver_like_version("1.2.3_candidate"), None);
        assert_eq!(extract_semver_like_version("v2.4.-beta"), None);
        assert_eq!(extract_semver_like_version("v2.4-beta"), Some(vec![2, 4]));
        assert_eq!(extract_semver_like_version("v2.4+build.7"), Some(vec![2, 4]));
    }

    #[test]
    fn semver_extractor_treats_a_trailing_period_as_punctuation_only_at_a_boundary() {
        assert_eq!(extract_semver_like_version("v2.4."), Some(vec![2, 4]));
        assert_eq!(extract_semver_like_version("(v2.4.)"), Some(vec![2, 4]));
        assert_eq!(extract_semver_like_version("v2.4.next"), None);
    }
}
