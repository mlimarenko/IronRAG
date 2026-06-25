use std::collections::{BTreeSet, HashMap, HashSet};

use anyhow::Context;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::query::{QueryVerificationState, QueryVerificationWarning},
    infra::knowledge_rows::KnowledgeTechnicalFactRow,
    services::query::assistant_grounding::AssistantGroundingEvidence,
    services::query::planner::QueryIntentProfile,
    shared::text_tokens::literal_wildcard_prefixes,
};

use super::types::{CanonicalAnswerEvidence, RuntimeAnswerVerification, RuntimeMatchedChunk};

const VERIFICATION_LITERAL_COLOCATION_MAX_NORMALIZED_SPAN: usize = 2_048;

pub(crate) fn verify_answer_against_canonical_evidence(
    question: &str,
    answer: &str,
    intent_profile: &QueryIntentProfile,
    evidence: &CanonicalAnswerEvidence,
    chunks: &[RuntimeMatchedChunk],
    prompt_context: &str,
    assistant_grounding: &AssistantGroundingEvidence,
) -> RuntimeAnswerVerification {
    if answer.trim().is_empty() {
        return RuntimeAnswerVerification {
            state: QueryVerificationState::Failed,
            warnings: vec![QueryVerificationWarning {
                code: "empty_answer".to_string(),
                message: "Answer generation returned empty output.".to_string(),
                related_segment_id: None,
                related_fact_id: None,
            }],
            unsupported_literals: Vec::new(),
        };
    }
    if !has_canonical_grounding_evidence(evidence, chunks, assistant_grounding) {
        return RuntimeAnswerVerification {
            state: QueryVerificationState::InsufficientEvidence,
            warnings: vec![QueryVerificationWarning {
                code: "no_canonical_evidence".to_string(),
                message: "Answer verification requires selected canonical evidence.".to_string(),
                related_segment_id: None,
                related_fact_id: None,
            }],
            unsupported_literals: Vec::new(),
        };
    }

    let (inline_literals, fenced_line_literals) = extract_answer_literals(answer);
    let mut normalized_corpus = build_verification_corpus(evidence, chunks, assistant_grounding);
    let normalized_grounding_corpus = normalized_corpus.clone();
    // Library summary, document file names, document titles and other prompt
    // metadata are part of what the LLM saw — include the whole rendered
    // prompt context so file-name backticks like `customers.csv` are not
    // marked as hallucinations.
    let normalized_prompt_context = normalize_verification_literal(prompt_context);
    if !normalized_prompt_context.is_empty() {
        normalized_corpus.push(normalized_prompt_context);
    }
    let question_wildcard_prefixes = literal_wildcard_prefixes(question, 2);
    let mut warnings = Vec::<QueryVerificationWarning>::new();
    let mut unsupported_literals = Vec::<String>::new();
    for literal in inline_literals.iter().chain(fenced_line_literals.iter()) {
        let normalized_literal = normalize_verification_literal(literal);
        if normalized_literal.is_empty() {
            continue;
        }
        if literal_is_user_supplied_wildcard_scope(literal, &question_wildcard_prefixes) {
            continue;
        }
        if !literal_is_supported_by_canonical_corpus(
            literal,
            &normalized_corpus,
            &normalized_grounding_corpus,
        ) {
            unsupported_literals.push(literal.clone());
            warnings.push(QueryVerificationWarning {
                code: "unsupported_literal".to_string(),
                message: format!("Literal `{literal}` is not grounded in selected evidence."),
                related_segment_id: None,
                related_fact_id: None,
            });
        }
    }
    let has_unsupported_literals =
        warnings.iter().any(|warning| warning.code == "unsupported_literal");
    let conflicting_groups = if intent_profile.exact_literal_technical {
        collect_conflicting_fact_groups(&evidence.technical_facts)
    } else {
        HashMap::new()
    };
    if !conflicting_groups.is_empty() {
        warnings.push(QueryVerificationWarning {
            code: "conflicting_evidence".to_string(),
            message: format!(
                "Selected evidence contains {} conflicting technical fact group(s).",
                conflicting_groups.len()
            ),
            related_segment_id: None,
            related_fact_id: None,
        });
    }

    let lower_answer = answer.to_ascii_lowercase();
    let insufficient = lower_answer.contains("no grounded evidence")
        || lower_answer.contains("exact value is not grounded");
    let has_conflicting_evidence =
        warnings.iter().any(|warning| warning.code == "conflicting_evidence");
    let has_unsupported_canonical_claim =
        warnings.iter().any(|warning| warning.code == "unsupported_canonical_claim");
    let state = if insufficient || has_unsupported_literals || has_unsupported_canonical_claim {
        QueryVerificationState::InsufficientEvidence
    } else if has_conflicting_evidence {
        QueryVerificationState::Conflicting
    } else {
        QueryVerificationState::Verified
    };

    RuntimeAnswerVerification { state, warnings, unsupported_literals }
}

fn literal_is_user_supplied_wildcard_scope(
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

fn has_canonical_grounding_evidence(
    evidence: &CanonicalAnswerEvidence,
    chunks: &[RuntimeMatchedChunk],
    assistant_grounding: &AssistantGroundingEvidence,
) -> bool {
    !evidence.chunk_rows.is_empty()
        || !evidence.structured_blocks.is_empty()
        || !evidence.technical_facts.is_empty()
        || !chunks.is_empty()
        || !assistant_grounding.verification_corpus.is_empty()
        || !assistant_grounding.document_references.is_empty()
}

fn extract_answer_literals(answer: &str) -> (Vec<String>, Vec<String>) {
    let mut inline = Vec::<String>::new();
    let mut fenced_lines = Vec::<String>::new();
    let mut seen_inline = HashSet::<String>::new();
    let mut seen_fenced = HashSet::<String>::new();

    let bytes = answer.as_bytes();
    let mut cursor = 0usize;
    while cursor < bytes.len() {
        if bytes[cursor..].starts_with(b"```") {
            let body_start = cursor + 3;
            if let Some(relative) = find_subslice(&bytes[body_start..], b"```") {
                let body_end = body_start + relative;
                let body = &answer[body_start..body_end];
                for line in fenced_block_content_lines(body) {
                    if seen_fenced.insert(line.clone()) {
                        fenced_lines.push(line);
                    }
                }
                cursor = body_end + 3;
                continue;
            } else {
                break;
            }
        }
        if bytes[cursor] == b'`' {
            let literal_start = cursor + 1;
            if let Some(relative) = find_byte(&bytes[literal_start..], b'`') {
                let literal_end = literal_start + relative;
                let literal = answer[literal_start..literal_end].trim().to_string();
                if !literal.is_empty() && seen_inline.insert(literal.clone()) {
                    inline.push(literal);
                }
                cursor = literal_end + 1;
                continue;
            } else {
                break;
            }
        }
        cursor += utf8_char_len(bytes[cursor]);
    }

    (inline, fenced_lines)
}

fn fenced_block_content_lines(body: &str) -> Vec<String> {
    let trimmed = body.strip_prefix('\n').or_else(|| body.strip_prefix("\r\n")).unwrap_or(body);
    let mut raw_lines: Vec<&str> = trimmed.split('\n').collect();
    if let Some(first) = raw_lines.first() {
        if is_language_hint(first.trim()) {
            raw_lines.remove(0);
        }
    }
    raw_lines
        .into_iter()
        .map(|line| line.trim_end_matches('\r').trim().to_string())
        .filter(|line| !line.is_empty())
        .collect()
}

fn is_language_hint(candidate: &str) -> bool {
    if candidate.is_empty() || candidate.len() > 20 {
        return false;
    }
    candidate
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '+' || ch == '_' || ch == '.')
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|window| window == needle)
}

fn find_byte(haystack: &[u8], needle: u8) -> Option<usize> {
    haystack.iter().position(|byte| *byte == needle)
}

fn utf8_char_len(first_byte: u8) -> usize {
    match first_byte {
        b if b < 0x80 => 1,
        b if b & 0xE0 == 0xC0 => 2,
        b if b & 0xF0 == 0xE0 => 3,
        b if b & 0xF8 == 0xF0 => 4,
        _ => 1,
    }
}

fn build_verification_corpus(
    evidence: &CanonicalAnswerEvidence,
    chunks: &[RuntimeMatchedChunk],
    assistant_grounding: &AssistantGroundingEvidence,
) -> Vec<String> {
    let mut corpus = Vec::<String>::new();
    for fact in &evidence.technical_facts {
        corpus.push(normalize_verification_literal(&fact.display_value));
        corpus.push(normalize_verification_literal(&fact.canonical_value_text));
        if let Ok(qualifiers) = serde_json::from_value::<
            Vec<crate::shared::extraction::technical_facts::TechnicalFactQualifier>,
        >(fact.qualifiers_json.clone())
        {
            for qualifier in qualifiers {
                corpus.push(normalize_verification_literal(&qualifier.key));
                corpus.push(normalize_verification_literal(&qualifier.value));
            }
        }
    }
    for block in &evidence.structured_blocks {
        corpus.push(normalize_verification_literal(&block.text));
        corpus.push(normalize_verification_literal(&block.normalized_text));
    }
    for chunk in &evidence.chunk_rows {
        corpus.push(normalize_verification_literal(&chunk.content_text));
        corpus.push(normalize_verification_literal(&chunk.normalized_text));
    }
    for chunk in chunks {
        corpus.push(normalize_verification_literal(&chunk.source_text));
        corpus.push(normalize_verification_literal(&chunk.excerpt));
    }
    for fragment in &assistant_grounding.verification_corpus {
        corpus.push(normalize_verification_literal(fragment));
    }
    for reference in &assistant_grounding.document_references {
        corpus.push(normalize_verification_literal(&reference.document_title));
        corpus.push(normalize_verification_literal(&reference.excerpt));
    }
    corpus.retain(|value| !value.is_empty());
    corpus
}

fn literal_is_supported_by_canonical_corpus(
    literal: &str,
    corpus: &[String],
    grounding_corpus: &[String],
) -> bool {
    let normalized_literal = normalize_verification_literal(literal);
    if normalized_literal.is_empty() {
        return true;
    }
    if corpus.iter().any(|candidate| candidate.contains(&normalized_literal)) {
        return true;
    }
    if decorated_version_literal_is_supported_by_corpus(literal, grounding_corpus) {
        return true;
    }
    if code_assignment_literal_is_supported_by_corpus(literal, grounding_corpus) {
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
    corpus
        .iter()
        .any(|candidate| candidate_contains_components_within_span(candidate, &version_tokens))
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

fn code_assignment_literal_is_supported_by_corpus(literal: &str, corpus: &[String]) -> bool {
    let Some((left, right)) = literal.split_once('=') else {
        return false;
    };
    let left = left.trim();
    let right = right.trim();
    if !is_specific_code_identifier(left) {
        return false;
    }
    let normalized_left = normalize_verification_literal(left);
    let normalized_right = normalize_verification_literal(right);
    let components = vec![normalized_left, normalized_right];
    components.iter().all(|component| !component.is_empty())
        && corpus
            .iter()
            .any(|candidate| candidate_contains_components_within_span(candidate, &components))
}

fn is_specific_code_identifier(value: &str) -> bool {
    let trimmed = value
        .trim_matches(|ch: char| ch.is_whitespace() || matches!(ch, '"' | '\'' | '`' | '[' | ']'));
    if trimmed.len() < 3 || !trimmed.chars().any(|ch| ch.is_alphabetic()) {
        return false;
    }
    let has_code_separator = trimmed.chars().any(|ch| matches!(ch, '_' | '-' | '.'));
    let has_lowercase = trimmed.chars().any(|ch| ch.is_lowercase());
    let has_uppercase = trimmed.chars().any(|ch| ch.is_uppercase());
    let has_embedded_digit = trimmed.chars().any(|ch| ch.is_ascii_digit())
        && trimmed.chars().any(|ch| ch.is_alphabetic());
    has_code_separator || (has_lowercase && has_uppercase) || has_embedded_digit
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
            let content: String = chars[index + 1..end_index].iter().collect();
            parsed.has_marker = true;
            if is_placeholder_bracket_content(&content) {
                parsed.has_placeholder_or_ellipsis = true;
                scrubbed.push(' ');
            } else {
                parsed.has_non_placeholder_bracket = true;
                scrubbed.push(' ');
                scrubbed.extend(chars[index..=end_index].iter());
                scrubbed.push(' ');
            }
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

fn is_placeholder_bracket_content(content: &str) -> bool {
    let trimmed = content.trim();
    !trimmed.is_empty()
        && trimmed.chars().all(|ch| {
            ch.is_ascii_uppercase() || ch.is_ascii_digit() || matches!(ch, '_' | '-' | ' ')
        })
        && trimmed.chars().any(|ch| ch.is_ascii_uppercase())
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
                        <= VERIFICATION_LITERAL_COLOCATION_MAX_NORMALIZED_SPAN
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
                <= VERIFICATION_LITERAL_COLOCATION_MAX_NORMALIZED_SPAN
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

fn normalize_verification_literal(value: &str) -> String {
    let mut normalized = String::new();
    let mut chars = value.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '&'
            && let Some(decoded) = decode_html_entity(&mut chars)
        {
            normalized.extend(decoded.to_lowercase());
            continue;
        }
        if ch.is_whitespace() {
            continue;
        }
        if ch == '\\'
            && let Some(next) = chars.peek().copied()
            && is_markdown_escaped_literal_punctuation(next)
            && let Some(escaped) = chars.next()
        {
            normalized.extend(escaped.to_lowercase());
            continue;
        }
        normalized.extend(ch.to_lowercase());
    }
    normalized
}

fn decode_html_entity<I>(chars: &mut std::iter::Peekable<I>) -> Option<char>
where
    I: Iterator<Item = char> + Clone,
{
    let mut entity = String::new();
    let probe = chars.clone();
    let mut saw_semicolon = false;
    for next in probe {
        if next == ';' {
            saw_semicolon = true;
            break;
        }
        if entity.len() >= 16 || next.is_whitespace() {
            return None;
        }
        entity.push(next);
    }
    if !saw_semicolon {
        return None;
    }
    let decoded = match entity.as_str() {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" | "#39" => '\'',
        value if value.starts_with("#x") || value.starts_with("#X") => {
            let codepoint = u32::from_str_radix(&value[2..], 16).ok()?;
            char::from_u32(codepoint)?
        }
        value if value.starts_with('#') => {
            let codepoint = value[1..].parse::<u32>().ok()?;
            char::from_u32(codepoint)?
        }
        _ => return None,
    };
    for _ in 0..entity.chars().count() {
        chars.next();
    }
    chars.next();
    Some(decoded)
}

fn is_markdown_escaped_literal_punctuation(ch: char) -> bool {
    ch.is_ascii_punctuation() && ch != '/' && ch != '\\'
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

fn collect_conflicting_fact_groups(
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

pub(crate) async fn persist_query_verification(
    state: &AppState,
    execution_id: Uuid,
    verification: &RuntimeAnswerVerification,
    canonical_evidence: &CanonicalAnswerEvidence,
    assistant_grounding: &AssistantGroundingEvidence,
) -> anyhow::Result<()> {
    let Some(bundle) =
        state.context_store.get_bundle_by_query_execution(execution_id).await.with_context(
            || format!("failed to load context bundle for verification {execution_id}"),
        )?
    else {
        return Ok(());
    };
    let warnings_json = serde_json::to_value(&verification.warnings)
        .context("failed to serialize verification warnings")?;
    let candidate_summary = enrich_query_candidate_summary(
        bundle.candidate_summary.clone(),
        canonical_evidence,
        assistant_grounding,
    );
    let assembly_diagnostics = enrich_query_assembly_diagnostics(
        bundle.assembly_diagnostics.clone(),
        verification,
        &candidate_summary,
        assistant_grounding,
    );
    let _ = state
        .context_store
        .update_bundle_state(
            bundle.bundle_id,
            &bundle.bundle_state,
            &bundle.selected_fact_ids,
            verification_state_label(verification.state),
            warnings_json,
            bundle.freshness_snapshot,
            candidate_summary,
            assembly_diagnostics,
        )
        .await
        .context("failed to persist query verification state")?;
    Ok(())
}

fn verification_state_label(state: QueryVerificationState) -> &'static str {
    match state {
        QueryVerificationState::Verified => "verified",
        QueryVerificationState::PartiallySupported => "partially_supported",
        QueryVerificationState::Conflicting => "conflicting_evidence",
        QueryVerificationState::InsufficientEvidence => "insufficient_evidence",
        QueryVerificationState::Failed => "failed",
        QueryVerificationState::NotRun => "not_run",
    }
}

pub(crate) fn enrich_query_candidate_summary(
    candidate_summary: serde_json::Value,
    canonical_evidence: &CanonicalAnswerEvidence,
    assistant_grounding: &AssistantGroundingEvidence,
) -> serde_json::Value {
    let mut summary = candidate_summary;
    let Some(object) = summary.as_object_mut() else {
        return summary;
    };
    object.insert(
        "finalPreparedSegmentReferences".to_string(),
        serde_json::json!(canonical_evidence.structured_blocks.len()),
    );
    object.insert(
        "finalTechnicalFactReferences".to_string(),
        serde_json::json!(canonical_evidence.technical_facts.len()),
    );
    object.insert(
        "finalChunkReferences".to_string(),
        serde_json::json!(canonical_evidence.chunk_rows.len()),
    );
    object.insert(
        "finalAssistantDocumentReferences".to_string(),
        serde_json::json!(assistant_grounding.document_references.len()),
    );
    summary
}

pub(crate) fn enrich_query_assembly_diagnostics(
    assembly_diagnostics: serde_json::Value,
    verification: &RuntimeAnswerVerification,
    candidate_summary: &serde_json::Value,
    assistant_grounding: &AssistantGroundingEvidence,
) -> serde_json::Value {
    let mut diagnostics = assembly_diagnostics;
    let Some(object) = diagnostics.as_object_mut() else {
        return diagnostics;
    };
    object.insert(
        "verificationState".to_string(),
        serde_json::Value::String(verification_state_label(verification.state).to_string()),
    );
    object.insert(
        "verificationWarnings".to_string(),
        serde_json::to_value(&verification.warnings).unwrap_or_else(|_| serde_json::json!([])),
    );
    object.insert(
        "graphParticipation".to_string(),
        serde_json::json!({
            "entityReferenceCount": json_count(candidate_summary, "finalEntityReferences"),
            "relationReferenceCount": json_count(candidate_summary, "finalRelationReferences"),
            "graphBacked": json_count(candidate_summary, "finalEntityReferences") > 0
                || json_count(candidate_summary, "finalRelationReferences") > 0,
        }),
    );
    object.insert(
        "structuredEvidence".to_string(),
        serde_json::json!({
            "preparedSegmentReferenceCount": json_count(candidate_summary, "finalPreparedSegmentReferences"),
            "technicalFactReferenceCount": json_count(candidate_summary, "finalTechnicalFactReferences"),
            "chunkReferenceCount": json_count(candidate_summary, "finalChunkReferences"),
            "assistantDocumentReferenceCount": json_count(candidate_summary, "finalAssistantDocumentReferences"),
        }),
    );
    if !assistant_grounding.document_references.is_empty() {
        object.insert(
            "assistantGrounding".to_string(),
            serde_json::json!({
                "documentReferenceCount": assistant_grounding.document_references.len(),
                "documentReferences": assistant_grounding.document_references,
            }),
        );
    }
    diagnostics
}

fn json_count(value: &serde_json::Value, key: &str) -> usize {
    value
        .get(key)
        .and_then(serde_json::Value::as_u64)
        .and_then(|count| usize::try_from(count).ok())
        .unwrap_or_default()
}
