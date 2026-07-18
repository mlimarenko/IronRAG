use std::collections::{BTreeSet, HashMap};

use crate::{
    infra::knowledge_rows::KnowledgeTechnicalFactRow,
    services::query::assistant_grounding::AssistantGroundingEvidence,
    shared::text_tokens::literal_wildcard_prefixes,
};

use super::{
    types::{CanonicalAnswerEvidence, RuntimeMatchedChunk},
    verification_claims::{
        FormalExactClaim, FormalExactClaimKind, normalize_boundary_verification_text,
        normalize_verification_literal,
    },
};

const VERIFICATION_LITERAL_COMPONENT_MAX_NORMALIZED_SPAN: usize = 256;

pub(super) fn literal_is_user_supplied_wildcard_scope(
    literal: &str,
    question_wildcard_prefixes: &[String],
) -> bool {
    if question_wildcard_prefixes.is_empty() {
        return false;
    }
    let literal_prefixes = literal_wildcard_prefixes(literal, 2);
    !literal_prefixes.is_empty()
        && literal_prefixes
            .iter()
            .any(|prefix| question_wildcard_prefixes.iter().any(|candidate| candidate == prefix))
}

pub(super) fn has_canonical_grounding_evidence(
    evidence: &CanonicalAnswerEvidence,
    chunks: &[RuntimeMatchedChunk],
    assistant_grounding: &AssistantGroundingEvidence,
) -> bool {
    !evidence.chunk_rows.is_empty()
        || !evidence.structured_blocks.is_empty()
        || !evidence.technical_facts.is_empty()
        || !chunks.is_empty()
        || assistant_grounding.has_verifier_grade_evidence()
}

pub(super) fn build_verification_corpus(
    evidence: &CanonicalAnswerEvidence,
    chunks: &[RuntimeMatchedChunk],
    assistant_grounding: &AssistantGroundingEvidence,
) -> Vec<String> {
    build_verification_corpus_with(
        evidence,
        chunks,
        assistant_grounding,
        normalize_verification_literal,
    )
}

pub(super) fn build_boundary_verification_corpus(
    evidence: &CanonicalAnswerEvidence,
    chunks: &[RuntimeMatchedChunk],
    assistant_grounding: &AssistantGroundingEvidence,
) -> Vec<String> {
    build_verification_corpus_with(
        evidence,
        chunks,
        assistant_grounding,
        normalize_boundary_verification_text,
    )
}

pub(super) fn build_exact_relationship_verification_corpus(
    evidence: &CanonicalAnswerEvidence,
    chunks: &[RuntimeMatchedChunk],
    assistant_grounding: &AssistantGroundingEvidence,
) -> Vec<String> {
    let mut corpus = Vec::new();
    for fact in &evidence.technical_facts {
        push_exact_relationship_records(&mut corpus, &fact.display_value);
        push_exact_relationship_records(&mut corpus, &fact.canonical_value_text);
        push_exact_relationship_records(&mut corpus, &fact.canonical_value_exact);
    }
    for block in &evidence.structured_blocks {
        push_exact_relationship_records(&mut corpus, &block.text);
        push_exact_relationship_records(&mut corpus, &block.normalized_text);
    }
    for chunk in &evidence.chunk_rows {
        push_exact_relationship_records(&mut corpus, &chunk.content_text);
        push_exact_relationship_records(&mut corpus, &chunk.normalized_text);
    }
    for chunk in chunks {
        push_exact_relationship_records(&mut corpus, &chunk.source_text);
        push_exact_relationship_records(&mut corpus, &chunk.excerpt);
    }
    for fragment in assistant_grounding.verifier_grade_corpus() {
        push_exact_relationship_records(&mut corpus, fragment);
    }
    for reference in assistant_grounding.verifier_grade_document_references() {
        push_exact_relationship_records(&mut corpus, &reference.excerpt);
    }
    corpus.sort();
    corpus.dedup();
    corpus
}

fn push_exact_relationship_records(corpus: &mut Vec<String>, value: &str) {
    for line in value.lines() {
        let normalized = normalize_verification_literal(line);
        if !normalized.is_empty() {
            corpus.push(normalized);
        }
    }
}

fn build_verification_corpus_with(
    evidence: &CanonicalAnswerEvidence,
    chunks: &[RuntimeMatchedChunk],
    assistant_grounding: &AssistantGroundingEvidence,
    normalize: fn(&str) -> String,
) -> Vec<String> {
    let mut corpus = Vec::<String>::new();
    for fact in &evidence.technical_facts {
        corpus.push(normalize(&fact.display_value));
        corpus.push(normalize(&fact.canonical_value_text));
        if let Ok(qualifiers) = serde_json::from_value::<
            Vec<crate::shared::extraction::technical_facts::TechnicalFactQualifier>,
        >(fact.qualifiers_json.clone())
        {
            for qualifier in qualifiers {
                corpus.push(normalize(&qualifier.key));
                corpus.push(normalize(&qualifier.value));
            }
        }
    }
    for block in &evidence.structured_blocks {
        corpus.push(normalize(&block.text));
        corpus.push(normalize(&block.normalized_text));
    }
    for chunk in &evidence.chunk_rows {
        corpus.push(normalize(&chunk.content_text));
        corpus.push(normalize(&chunk.normalized_text));
    }
    for chunk in chunks {
        corpus.push(normalize(&chunk.source_text));
        corpus.push(normalize(&chunk.excerpt));
    }
    for fragment in assistant_grounding.verifier_grade_corpus() {
        corpus.push(normalize(fragment));
    }
    for reference in assistant_grounding.verifier_grade_document_references() {
        corpus.push(normalize(&reference.document_title));
        corpus.push(normalize(&reference.excerpt));
    }
    corpus.retain(|value| !value.is_empty());
    corpus
}

pub(super) fn literal_is_supported_by_canonical_corpus(
    literal: &str,
    corpus: &[String],
    grounding_corpus: &[String],
    exact_relationship_corpus: &[String],
) -> bool {
    let normalized_literal = normalize_verification_literal(literal);
    if normalized_literal.is_empty() {
        return true;
    }
    if literal.split_once('=').is_some() {
        return exact_relationship_corpus
            .iter()
            .any(|candidate| candidate.contains(&normalized_literal));
    }
    if corpus.iter().any(|candidate| candidate.contains(&normalized_literal)) {
        return true;
    }
    if decorated_version_literal_is_supported_by_corpus(literal, grounding_corpus) {
        return true;
    }
    if slash_alternative_literal_is_supported_by_corpus(literal, grounding_corpus) {
        return true;
    }
    if structural_literal_is_supported_by_corpus(literal, grounding_corpus) {
        return true;
    }
    let Some((method, path)) = split_http_literal(literal) else {
        return false;
    };
    let normalized_method = normalize_verification_literal(method);
    let normalized_path = normalize_verification_literal(path);
    !normalized_method.is_empty()
        && !normalized_path.is_empty()
        && grounding_corpus.iter().any(|candidate| {
            candidate_contains_components_within_span(
                candidate,
                &[normalized_method.clone(), normalized_path.clone()],
            )
        })
}

pub(super) fn answer_is_verbatim_supported_by_corpus(answer: &str, corpus: &[String]) -> bool {
    let normalized_answer = normalize_verbatim_answer(answer);
    !normalized_answer.is_empty()
        && corpus
            .iter()
            .any(|candidate| normalize_verbatim_answer(candidate).contains(&normalized_answer))
}

fn normalize_verbatim_answer(value: &str) -> String {
    normalize_verification_literal(value).replace('`', "")
}

pub(super) fn formal_exact_claim_is_supported_by_corpus(
    claim: &FormalExactClaim,
    corpus: &[String],
) -> bool {
    match claim.kind() {
        FormalExactClaimKind::Numeric | FormalExactClaimKind::IsoDate => {
            exact_numeric_literal_is_supported_by_corpus(claim.literal(), corpus)
        }
        FormalExactClaimKind::PrefixedVersion => {
            prefixed_version_is_supported_by_corpus(claim.literal(), corpus)
        }
    }
}

pub(super) fn exact_numeric_literal_is_supported_by_corpus(
    literal: &str,
    corpus: &[String],
) -> bool {
    let needle = normalize_boundary_verification_text(literal);
    !needle.is_empty()
        && corpus.iter().any(|candidate| contains_exact_numeric_with_boundaries(candidate, &needle))
}

fn contains_exact_numeric_with_boundaries(candidate: &str, needle: &str) -> bool {
    candidate.match_indices(needle).any(|(start, matched)| {
        let end = start + matched.len();
        numeric_boundary_before(candidate, start) && numeric_boundary_after(candidate, end)
    })
}

fn numeric_boundary_before(candidate: &str, start: usize) -> bool {
    let before = candidate[..start].chars().next_back();
    if before.is_some_and(|ch| ch.is_alphanumeric() || ch == '_') {
        return false;
    }
    // A sign is part of the value, not punctuation around it: positive `90`
    // must not be accepted from canonical evidence containing `-90` or `+90`.
    if before.is_some_and(|ch| matches!(ch, '-' | '+')) {
        return false;
    }
    if before.is_some_and(|ch| matches!(ch, '.' | ':' | '/')) {
        return !candidate[..start].chars().rev().nth(1).is_some_and(|ch| ch.is_ascii_digit());
    }
    true
}

fn numeric_boundary_after(candidate: &str, end: usize) -> bool {
    let mut after = candidate[end..].chars();
    let adjacent = after.next();
    if adjacent.is_some_and(|ch| ch.is_alphanumeric() || ch == '_') {
        return false;
    }
    if adjacent.is_some_and(|ch| matches!(ch, '.' | ':' | '/' | '-' | '+')) {
        return !after.next().is_some_and(|ch| ch.is_ascii_digit());
    }
    true
}

fn prefixed_version_is_supported_by_corpus(literal: &str, corpus: &[String]) -> bool {
    let normalized = normalize_verification_literal(literal);
    let Some(unprefixed) = normalized.strip_prefix('v') else {
        return false;
    };
    if unprefixed.is_empty() {
        return false;
    }
    corpus.iter().any(|candidate| {
        contains_version_with_boundaries(candidate, &normalized)
            || contains_version_with_boundaries(candidate, unprefixed)
    })
}

fn contains_version_with_boundaries(candidate: &str, needle: &str) -> bool {
    candidate.match_indices(needle).any(|(start, matched)| {
        let end = start + matched.len();
        let before = candidate[..start].chars().next_back();
        let after = candidate[end..].chars().next();
        let before_supported = before.is_none_or(version_boundary);
        let after_supported = after.is_none_or(version_boundary);
        before_supported && after_supported
    })
}

fn version_boundary(ch: char) -> bool {
    !ch.is_alphanumeric() && ch != '_' && !matches!(ch, '.' | '-' | '+')
}

fn decorated_version_literal_is_supported_by_corpus(literal: &str, corpus: &[String]) -> bool {
    let normalized_literal = normalize_verification_literal(literal);
    let version_tokens = extract_numeric_version_tokens(literal);
    if version_tokens.is_empty() {
        return false;
    }
    let literal_without_versions = version_tokens
        .iter()
        .fold(normalized_literal.clone(), |accumulator, version| accumulator.replace(version, ""));
    if literal_without_versions.chars().filter(|ch| ch.is_alphanumeric()).count() > 32 {
        return false;
    }
    let mut components = version_tokens;
    if literal_without_versions.chars().any(char::is_alphanumeric) {
        components.push(literal_without_versions);
    }
    corpus.iter().any(|candidate| candidate_contains_components_within_span(candidate, &components))
}

fn extract_numeric_version_tokens(value: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let chars = value.chars().collect::<Vec<_>>();
    let mut start: Option<usize> = None;
    for (index, ch) in chars.iter().copied().enumerate() {
        if ch.is_ascii_digit() || ch == '.' {
            start.get_or_insert(index);
            continue;
        }
        if let Some(start_index) = start.take() {
            push_version_token(&chars[start_index..index], &mut tokens);
        }
    }
    if let Some(start_index) = start {
        push_version_token(&chars[start_index..], &mut tokens);
    }
    tokens.sort();
    tokens.dedup();
    tokens
}

fn push_version_token(chars: &[char], tokens: &mut Vec<String>) {
    let token = chars.iter().collect::<String>().trim_matches('.').to_string();
    if token.is_empty() {
        return;
    }
    let parts = token.split('.').collect::<Vec<_>>();
    let has_version_shape = (2..=3).contains(&parts.len())
        && parts.iter().all(|part| !part.is_empty() && part.chars().all(|ch| ch.is_ascii_digit()));
    if has_version_shape {
        tokens.push(token);
    }
}

fn slash_alternative_literal_is_supported_by_corpus(literal: &str, corpus: &[String]) -> bool {
    if !literal.contains('/') {
        return false;
    }
    let alternatives = expand_slash_literal_alternatives(literal);
    if alternatives.len() < 2 {
        return false;
    }
    corpus
        .iter()
        .any(|candidate| candidate_contains_components_within_span(candidate, &alternatives))
}

fn expand_slash_literal_alternatives(literal: &str) -> Vec<String> {
    let parts = literal
        .split('/')
        .map(|part| {
            part.trim_matches(|ch: char| {
                ch.is_whitespace()
                    || matches!(ch, '"' | '\'' | '`' | ',' | ';' | '(' | ')' | '[' | ']')
            })
        })
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    let Some(first) = parts.first().copied() else {
        return Vec::new();
    };
    let prefix = shared_prefix_for_slash_tail(first);
    let mut alternatives = Vec::with_capacity(parts.len());
    for (index, part) in parts.iter().enumerate() {
        let candidate = if index == 0 || part.chars().any(|ch| matches!(ch, '.' | '_' | '-' | ':'))
        {
            (*part).to_string()
        } else if let Some(prefix) = prefix {
            format!("{prefix}{part}")
        } else {
            (*part).to_string()
        };
        let normalized = normalize_verification_literal(&candidate);
        if !normalized.is_empty() {
            alternatives.push(normalized);
        }
    }
    alternatives.sort();
    alternatives.dedup();
    alternatives
}

fn shared_prefix_for_slash_tail(first: &str) -> Option<&str> {
    let delimiter_index = first
        .char_indices()
        .rev()
        .find(|(_, ch)| matches!(ch, '.' | '_' | '-' | ':'))
        .map(|(index, ch)| index + ch.len_utf8())?;
    (delimiter_index < first.len()).then(|| &first[..delimiter_index])
}

#[derive(Debug, Default)]
struct StructuralLiteralComponents {
    has_marker: bool,
    has_placeholder_or_ellipsis: bool,
    has_non_placeholder_bracket: bool,
    components: Vec<String>,
}

fn structural_literal_is_supported_by_corpus(literal: &str, corpus: &[String]) -> bool {
    let mut parsed = parse_structural_literal_components(literal);
    if !parsed.has_marker {
        return false;
    }
    parsed.components.sort();
    parsed.components.dedup();
    if parsed.components.is_empty() {
        return false;
    }

    let component_shape_supported = parsed.has_non_placeholder_bracket
        || parsed.components.len() >= 2
        || (parsed.has_placeholder_or_ellipsis && parsed.components.len() == 1);
    component_shape_supported
        && corpus.iter().any(|candidate| {
            structural_components_match_candidate_within_span(&parsed.components, candidate)
        })
}

fn parse_structural_literal_components(literal: &str) -> StructuralLiteralComponents {
    let mut parsed = StructuralLiteralComponents::default();
    let mut scrubbed = String::new();
    let chars: Vec<char> = literal.chars().collect();
    let mut index = 0usize;
    while index < chars.len() {
        let ch = chars[index];
        if ch == '['
            && let Some(end) = chars[index + 1..].iter().position(|candidate| *candidate == ']')
        {
            let end_index = index + 1 + end;
            parsed.has_marker = true;
            parsed.has_non_placeholder_bracket = true;
            scrubbed.push(' ');
            scrubbed.extend(chars[index..=end_index].iter());
            scrubbed.push(' ');
            index = end_index + 1;
            continue;
        }
        if ch == '<'
            && let Some(end) = chars[index + 1..].iter().position(|candidate| *candidate == '>')
        {
            let end_index = index + 1 + end;
            let content: String = chars[index + 1..end_index].iter().collect();
            if is_placeholder_angle_content(&content) {
                parsed.has_marker = true;
                parsed.has_placeholder_or_ellipsis = true;
                scrubbed.push(' ');
                index = end_index + 1;
                continue;
            }
        }
        if ch == '…' {
            parsed.has_marker = true;
            parsed.has_placeholder_or_ellipsis = true;
            scrubbed.push(' ');
            index += 1;
            continue;
        }
        if ch == '.'
            && index + 2 < chars.len()
            && chars[index + 1] == '.'
            && chars[index + 2] == '.'
        {
            parsed.has_marker = true;
            parsed.has_placeholder_or_ellipsis = true;
            scrubbed.push(' ');
            index += 3;
            continue;
        }
        scrubbed.push(ch);
        index += 1;
    }

    parsed.components = scrubbed
        .split(is_structural_component_separator)
        .map(|component| {
            component
                .trim_matches(|ch: char| {
                    ch.is_whitespace()
                        || matches!(ch, '"' | '\'' | '`' | ',' | ';' | '(' | ')' | '{' | '}')
                })
                .to_string()
        })
        .filter(|component| !component.is_empty())
        .map(|component| normalize_verification_literal(&component))
        .filter(|component| !component.is_empty())
        .collect();
    parsed
}

fn is_placeholder_angle_content(content: &str) -> bool {
    let trimmed = content.trim();
    !trimmed.is_empty()
        && trimmed.len() <= 40
        && trimmed.chars().any(|ch| ch.is_alphabetic())
        && trimmed.chars().all(|ch| ch.is_alphanumeric() || matches!(ch, '_' | '-' | ' '))
}

fn is_structural_component_separator(ch: char) -> bool {
    ch.is_whitespace() || matches!(ch, '=' | ':' | ',' | ';')
}

fn structural_components_match_candidate_within_span(
    components: &[String],
    candidate: &str,
) -> bool {
    let normalized_components = components
        .iter()
        .filter_map(|component| {
            if candidate.contains(component) {
                Some(component.clone())
            } else {
                component
                    .strip_prefix('[')
                    .and_then(|value| value.strip_suffix(']'))
                    .filter(|inner| !inner.is_empty() && candidate.contains(*inner))
                    .map(ToOwned::to_owned)
            }
        })
        .collect::<Vec<_>>();
    normalized_components.len() == components.len()
        && candidate_contains_components_within_span(candidate, &normalized_components)
}

fn candidate_contains_components_within_span(candidate: &str, components: &[String]) -> bool {
    if components.is_empty() {
        return false;
    }
    let mut ranges_by_component = Vec::<Vec<(usize, usize)>>::with_capacity(components.len());
    for component in components {
        if component.is_empty() {
            return false;
        }
        let ranges = find_component_ranges(candidate, component, 32);
        if ranges.is_empty() {
            return false;
        }
        ranges_by_component.push(ranges);
    }
    let anchor_index = ranges_by_component
        .iter()
        .enumerate()
        .min_by_key(|(_, ranges)| ranges.len())
        .map(|(index, _)| index)
        .unwrap_or(0);
    for &(anchor_start, anchor_end) in &ranges_by_component[anchor_index] {
        let mut min_start = anchor_start;
        let mut max_end = anchor_end;
        let mut matched = true;
        for (index, ranges) in ranges_by_component.iter().enumerate() {
            if index == anchor_index {
                continue;
            }
            let Some((next_start, next_end)) = ranges
                .iter()
                .copied()
                .filter(|(start, end)| {
                    max_end.max(*end).saturating_sub(min_start.min(*start))
                        <= VERIFICATION_LITERAL_COMPONENT_MAX_NORMALIZED_SPAN
                })
                .min_by_key(|(start, end)| max_end.max(*end).saturating_sub(min_start.min(*start)))
            else {
                matched = false;
                break;
            };
            min_start = min_start.min(next_start);
            max_end = max_end.max(next_end);
        }
        if matched
            && max_end.saturating_sub(min_start)
                <= VERIFICATION_LITERAL_COMPONENT_MAX_NORMALIZED_SPAN
        {
            return true;
        }
    }
    false
}

fn find_component_ranges(
    candidate: &str,
    component: &str,
    max_ranges: usize,
) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    let mut cursor = 0usize;
    while cursor <= candidate.len() {
        let Some(relative) = candidate[cursor..].find(component) else {
            break;
        };
        let start = cursor + relative;
        let end = start + component.len();
        ranges.push((start, end));
        if ranges.len() >= max_ranges {
            break;
        }
        cursor = start.saturating_add(component.len().max(1));
    }
    ranges
}

fn split_http_literal(literal: &str) -> Option<(&str, &str)> {
    let trimmed = literal.trim();
    for method in ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"] {
        let Some(rest) = trimmed.strip_prefix(method) else {
            continue;
        };
        let path = rest.trim();
        if path.starts_with('/') || path.starts_with("http://") || path.starts_with("https://") {
            return Some((method, path));
        }
    }
    None
}

pub(super) fn collect_conflicting_fact_groups(
    facts: &[KnowledgeTechnicalFactRow],
) -> HashMap<String, BTreeSet<String>> {
    let mut groups = HashMap::<String, BTreeSet<String>>::new();
    for fact in facts {
        let Some(group_id) = fact.conflict_group_id.as_ref() else {
            continue;
        };
        groups.entry(group_id.clone()).or_default().insert(fact.canonical_value_text.clone());
    }
    groups.into_iter().filter(|(_, values)| values.len() > 1).collect()
}
