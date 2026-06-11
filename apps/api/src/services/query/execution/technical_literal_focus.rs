use std::collections::{HashMap, HashSet};

use uuid::Uuid;

use crate::domains::query_ir::{
    LiteralKind, QueryAct, QueryIR, QueryScope, literal_text_is_identifier_shaped,
};
use crate::services::query::effective_query::current_question_segment;
use crate::services::query::planner::strip_leading_question_marker;
use crate::services::query::text_match::related_prefix_token_match;

use super::retrieve::score_value;
use super::technical_literal_extractors::{
    extract_config_section_literals, extract_explicit_path_literals, extract_http_methods,
    extract_package_command_literals, extract_parameter_literals, extract_prefix_literals,
    extract_url_literals,
};
use super::types::RuntimeMatchedChunk;

const TECHNICAL_LITERAL_INVENTORY_SCORE_CAP: isize = 48;

/// Extracts focus keywords for technical chunk ranking.
///
/// When `ir` carries literal constraints, tokens from those constraints are
/// emitted first because they are the strongest focus signal. The remaining
/// structural tokens from the question are still retained afterwards: exact
/// technical answers often require the surrounding verb, endpoint role, or
/// setting purpose to disambiguate between nearby literal blocks.
///
/// When `ir` is `None` (retrieval runs in parallel with IR compilation, so
/// the lexical query builder cannot see the IR yet) or carries no literal
/// constraints (Describe / ConfigureHow / Enumerate questions), every
/// ≥4-char token from the question is kept. Downstream ranking already
/// weighs tokens by their presence in document text, so tokens that do not
/// appear in candidate chunks contribute nothing without needing a
/// hard-coded stop list.
pub(super) fn technical_literal_focus_keywords(
    question: &str,
    ir: Option<&QueryIR>,
) -> Vec<String> {
    let mut keywords = Vec::new();
    let mut seen = HashSet::new();
    if let Some(ir) = ir {
        for literal in &ir.literal_constraints {
            for token in technical_literal_question_tokens(&literal.text) {
                if seen.insert(token.clone()) {
                    keywords.push(token);
                }
            }
        }
    }
    let current_question = current_question_segment(question);
    for token in strip_leading_question_marker(current_question)
        .split(|ch: char| !ch.is_alphanumeric() && ch != '_' && ch != '/')
        .map(str::trim)
        .filter(|token| should_keep_technical_focus_token(token))
        .map(str::to_lowercase)
    {
        if seen.insert(token.clone()) {
            keywords.push(token.clone());
        }
    }
    keywords
}

fn technical_literal_question_tokens(value: &str) -> impl Iterator<Item = String> + '_ {
    value
        .split(|ch: char| !ch.is_alphanumeric() && ch != '_' && ch != '/')
        .map(str::trim)
        .filter(|token| should_keep_technical_focus_token(token))
        .map(str::to_lowercase)
}

fn should_keep_technical_focus_token(token: &str) -> bool {
    token.chars().count() >= 4 || is_short_technical_focus_token(token)
}

fn is_short_technical_focus_token(token: &str) -> bool {
    let char_count = token.chars().count();
    (2..=3).contains(&char_count)
        && token.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
        && token.chars().any(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit())
}

fn technical_keyword_stem(keyword: &str) -> Option<String> {
    let stem = keyword.chars().take(5).collect::<String>();
    (stem.chars().count() >= 4).then_some(stem)
}

pub(super) fn technical_keyword_present(lowered_text: &str, keyword: &str) -> bool {
    lowered_text.contains(keyword)
        || technical_keyword_stem(keyword).is_some_and(|stem| lowered_text.contains(stem.as_str()))
        || technical_keyword_related_prefix_present(lowered_text, keyword)
}

pub(super) fn technical_keyword_weight(lowered_text: &str, keyword: &str) -> usize {
    if lowered_text.contains(keyword) {
        return keyword.chars().count().min(24);
    }
    if technical_keyword_stem(keyword).is_some_and(|stem| lowered_text.contains(stem.as_str())) {
        return 4;
    }
    if technical_keyword_related_prefix_present(lowered_text, keyword) {
        return 3;
    }
    0
}

fn technical_keyword_related_prefix_present(lowered_text: &str, keyword: &str) -> bool {
    keyword.chars().count() >= 5
        && lowered_text
            .split(|ch: char| !ch.is_alphanumeric() && ch != '_' && ch != '/')
            .map(str::trim)
            .filter(|token| !token.is_empty())
            .any(|token| related_prefix_token_match(keyword, token))
}

pub(super) fn technical_literal_focus_keyword_segments(
    question: &str,
    ir: Option<&QueryIR>,
) -> Vec<Vec<String>> {
    if let Some(ir) = ir
        && matches!(ir.scope, QueryScope::MultiDocument)
    {
        let literal_segments = ir
            .literal_constraints
            .iter()
            .map(|literal| technical_literal_question_tokens(&literal.text).collect::<Vec<_>>())
            .filter(|keywords| !keywords.is_empty())
            .collect::<Vec<_>>();
        if !literal_segments.is_empty() {
            return literal_segments;
        }
    }

    let current_question = current_question_segment(question);
    let segments = current_question
        .split([';', ',', '\n'])
        .map(|segment| technical_literal_focus_keywords(&segment, ir))
        .filter(|keywords| !keywords.is_empty())
        .collect::<Vec<_>>();
    if segments.is_empty() {
        let fallback = technical_literal_focus_keywords(current_question, ir);
        if fallback.is_empty() { Vec::new() } else { vec![fallback] }
    } else {
        segments
    }
}

pub(super) fn document_local_focus_keywords(
    question: &str,
    ir: Option<&QueryIR>,
    chunks: &[&RuntimeMatchedChunk],
    question_keywords: &[String],
) -> Vec<String> {
    if question_keywords.is_empty() {
        return Vec::new();
    }

    let document_text = chunks
        .iter()
        .map(|chunk| format!("{} {}", chunk.excerpt, chunk.source_text))
        .collect::<Vec<_>>()
        .join("\n")
        .to_lowercase();
    let best_segment = technical_literal_focus_keyword_segments(question, ir)
        .into_iter()
        .map(|segment_keywords| {
            let score = segment_keywords
                .iter()
                .map(|keyword| technical_keyword_weight(&document_text, keyword))
                .sum::<usize>();
            (score, segment_keywords)
        })
        .max_by_key(|(score, _)| *score)
        .filter(|(score, _)| *score > 0)
        .map(|(_, segment_keywords)| segment_keywords);
    if let Some(segment_keywords) = best_segment {
        let local_segment_keywords = segment_keywords
            .iter()
            .filter(|keyword| technical_keyword_present(&document_text, keyword))
            .cloned()
            .collect::<Vec<_>>();
        if !local_segment_keywords.is_empty() {
            return local_segment_keywords;
        }
        return segment_keywords;
    }
    let local_keywords = question_keywords
        .iter()
        .filter(|keyword| technical_keyword_present(&document_text, keyword))
        .cloned()
        .collect::<Vec<_>>();
    if local_keywords.is_empty() { question_keywords.to_vec() } else { local_keywords }
}

pub(super) fn technical_chunk_selection_score(
    text: &str,
    keywords: &[String],
    _pagination_requested: bool,
) -> isize {
    let lowered = text.to_lowercase();
    let keyword_count = keywords.len();
    keywords
        .iter()
        .enumerate()
        .map(|(index, keyword)| {
            let priority = keyword_count.saturating_sub(index).max(1) as isize;
            (technical_keyword_weight(&lowered, keyword) as isize) * priority
        })
        .sum::<isize>()
}

fn query_ir_wants_literal_inventory_boost(question: &str, ir: Option<&QueryIR>) -> bool {
    let Some(ir) = ir else {
        return false;
    };
    if ir.is_exact_literal_technical()
        || matches!(ir.act, QueryAct::ConfigureHow | QueryAct::RetrieveValue)
    {
        return true;
    }
    if ir.target_types.iter().any(|tag| {
        matches!(
            tag.trim().to_ascii_lowercase().as_str(),
            "endpoint"
                | "path"
                | "url"
                | "wsdl"
                | "base_url"
                | "parameter"
                | "config_key"
                | "software_module"
                | "package"
                | "configuration_file"
                | "filesystem_path"
                | "http_method"
                | "port"
                | "protocol"
                | "connection"
        )
    }) {
        return true;
    }
    if ir.literal_constraints.iter().any(|literal| match literal.kind {
        LiteralKind::Url | LiteralKind::Path => true,
        LiteralKind::Identifier => literal_text_is_identifier_shaped(&literal.text),
        LiteralKind::Version | LiteralKind::NumericCode | LiteralKind::Other => false,
    }) {
        return true;
    }
    ir.confidence <= 0.3
        && matches!(ir.act, QueryAct::Describe | QueryAct::ConfigureHow)
        && matches!(ir.scope, QueryScope::SingleDocument)
        && ir.source_slice.is_none()
        && ir.target_types.is_empty()
        && ir.literal_constraints.is_empty()
        && technical_literal_focus_keywords(question, Some(ir))
            .iter()
            .any(|keyword| keyword.chars().count() < 4)
}

fn technical_literal_inventory_score(text: &str) -> isize {
    let score = extract_parameter_literals(text, 16).len().saturating_mul(4)
        + extract_config_section_literals(text, 8).len().saturating_mul(3)
        + extract_explicit_path_literals(text, 8).len().saturating_mul(3)
        + extract_package_command_literals(text, 4).len().saturating_mul(3)
        + extract_url_literals(text, 4).len().saturating_mul(2)
        + extract_prefix_literals(text, 4).len().saturating_mul(2)
        + extract_http_methods(text, 4).len();
    (score as isize).min(TECHNICAL_LITERAL_INVENTORY_SCORE_CAP)
}

pub(super) fn select_document_balanced_chunks<'a>(
    question: &str,
    ir: Option<&QueryIR>,
    chunks: &'a [RuntimeMatchedChunk],
    keywords: &[String],
    pagination_requested: bool,
    max_total_chunks: usize,
    max_chunks_per_document: usize,
) -> Vec<&'a RuntimeMatchedChunk> {
    let mut ordered_document_ids = Vec::<Uuid>::new();
    let mut per_document_chunks = HashMap::<Uuid, Vec<&RuntimeMatchedChunk>>::new();

    for chunk in chunks {
        if !per_document_chunks.contains_key(&chunk.document_id) {
            ordered_document_ids.push(chunk.document_id);
        }
        per_document_chunks.entry(chunk.document_id).or_default().push(chunk);
    }

    for document_chunks in per_document_chunks.values_mut() {
        let local_keywords = document_local_focus_keywords(question, ir, document_chunks, keywords);
        let literal_inventory_boost = query_ir_wants_literal_inventory_boost(question, ir);
        let score_by_chunk_id = document_chunks
            .iter()
            .map(|chunk| {
                let evidence_text =
                    format!("{} {} {}", chunk.document_label, chunk.excerpt, chunk.source_text);
                let match_score = technical_chunk_selection_score(
                    &evidence_text,
                    &local_keywords,
                    pagination_requested,
                );
                let inventory_score = literal_inventory_boost
                    .then(|| technical_literal_inventory_score(&evidence_text))
                    .unwrap_or(0);
                (
                    chunk.chunk_id,
                    (
                        match_score + inventory_score,
                        match_score,
                        inventory_score,
                        score_value(chunk.score),
                    ),
                )
            })
            .collect::<HashMap<_, _>>();
        document_chunks.sort_by(|left, right| {
            let (left_total, left_match, left_inventory, left_score) =
                score_by_chunk_id.get(&left.chunk_id).copied().unwrap_or_default();
            let (right_total, right_match, right_inventory, right_score) =
                score_by_chunk_id.get(&right.chunk_id).copied().unwrap_or_default();
            right_total
                .cmp(&left_total)
                .then_with(|| right_match.cmp(&left_match))
                .then_with(|| right_inventory.cmp(&left_inventory))
                .then_with(|| right_score.total_cmp(&left_score))
        });
    }

    let mut selected = Vec::new();
    for target_document_slot in 0..max_chunks_per_document {
        for document_id in &ordered_document_ids {
            if selected.len() >= max_total_chunks {
                return selected;
            }
            if let Some(chunk) = per_document_chunks
                .get(document_id)
                .and_then(|document_chunks| document_chunks.get(target_document_slot))
            {
                selected.push(*chunk);
            }
        }
    }

    selected
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domains::query_ir::QueryLanguage;

    fn test_chunk(
        document_id: Uuid,
        label: &str,
        index: i32,
        source_text: &str,
    ) -> RuntimeMatchedChunk {
        RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            document_id,
            revision_id: Uuid::now_v7(),
            chunk_index: index,
            chunk_kind: Some("text".to_string()),
            document_label: label.to_string(),
            excerpt: source_text.to_string(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(1.0),
            source_text: source_text.to_string(),
        }
    }

    fn test_query_ir() -> QueryIR {
        QueryIR {
            act: QueryAct::ConfigureHow,
            scope: QueryScope::SingleDocument,
            language: QueryLanguage::Auto,
            target_types: Vec::new(),
            target_entities: Vec::new(),
            literal_constraints: Vec::new(),
            temporal_constraints: Vec::new(),
            comparison: None,
            document_focus: None,
            conversation_refs: Vec::new(),
            needs_clarification: None,
            source_slice: None,
            retrieval_query: None,
            confidence: 1.0,
        }
    }

    #[test]
    fn technical_keyword_weight_accepts_longer_related_prefix_token() {
        assert_eq!(technical_keyword_weight("acmealpha payment configuration", "acmew"), 3);
    }

    #[test]
    fn technical_keyword_weight_rejects_short_prefix_target_tokens() {
        assert_eq!(technical_keyword_weight("acmealpha payment configuration", "acmr"), 0);
    }

    #[test]
    fn chunk_selection_uses_document_label_and_literal_inventory_for_config_chunks() {
        let document_id = Uuid::now_v7();
        let overview = test_chunk(
            document_id,
            "Alpha Connector setup guide",
            0,
            "General behavior without settings.",
        );
        let config = test_chunk(
            document_id,
            "Alpha Connector setup guide",
            1,
            "[Main]\nalphaMerchantId = 10\nsecretKey = value\npollInterval = 30",
        );
        let mut ir = test_query_ir();
        ir.target_types = vec!["config_key".to_string()];

        let chunks = [overview, config.clone()];
        let selected = select_document_balanced_chunks(
            "Alpha Connector configuration parameters",
            Some(&ir),
            &chunks,
            &technical_literal_focus_keywords(
                "Alpha Connector configuration parameters",
                Some(&ir),
            ),
            false,
            1,
            1,
        );

        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].chunk_id, config.chunk_id);
    }

    #[test]
    fn technical_focus_keywords_keep_short_technical_tokens_by_shape() {
        let keywords = technical_literal_focus_keywords(
            "Which QR ID setting maps to the alpha flag?",
            Some(&test_query_ir()),
        );

        assert!(keywords.iter().any(|keyword| keyword == "qr"));
        assert!(keywords.iter().any(|keyword| keyword == "id"));
        assert!(!keywords.iter().any(|keyword| keyword == "to"));
    }

    #[test]
    fn chunk_selection_uses_short_technical_fallback_inventory_for_config_chunks() {
        let document_id = Uuid::now_v7();
        let overview = test_chunk(document_id, "Alpha guide", 0, "QR settings overview.");
        let config = test_chunk(
            document_id,
            "Alpha guide",
            1,
            "[UI.Alpha.QR]\nalphaVisible = true\nprintSlip = false",
        );
        let mut ir = test_query_ir();
        ir.act = QueryAct::Describe;
        ir.confidence = 0.25;

        let chunks = [overview, config.clone()];
        let selected = select_document_balanced_chunks(
            "Which QR setting is used?",
            Some(&ir),
            &chunks,
            &technical_literal_focus_keywords("Which QR setting is used?", Some(&ir)),
            false,
            1,
            1,
        );

        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].chunk_id, config.chunk_id);
    }
}
