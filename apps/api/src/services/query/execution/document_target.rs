use std::collections::{BTreeSet, HashMap};

use uuid::Uuid;

use crate::domains::query_ir::{
    LiteralKind, QueryAct, QueryIR, QueryScope, literal_text_is_identifier_shaped,
};
use crate::infra::arangodb::document_store::KnowledgeDocumentRow;
use crate::services::query::effective_query::{
    current_question_segment, structured_current_question_segment,
};
use crate::services::query::text_match::{
    build_related_token_candidates, common_prefix_char_count, near_token_match,
    near_token_overlap_count, normalized_alnum_tokens,
    select_related_overlap_tokens_from_candidates,
};

use super::{
    question_intent::{
        canonical_target_type_tag, classify_query_ir_intents,
        query_ir_has_focused_document_answer_intent,
        query_ir_targets_graph_entities_or_relationships,
    },
    retrieve::score_value,
    types::RuntimeMatchedChunk,
};

/// Score gap multiplier for dominant-document detection in answer assembly.
const DOMINANT_DOCUMENT_SCORE_MULTIPLIER: f32 = 1.2;
const EXPLICIT_DOCUMENT_REFERENCE_EXTENSIONS: &[&str] = &[
    "md", "txt", "pdf", "docx", "csv", "tsv", "xls", "xlsx", "xlsb", "ods", "pptx", "png", "jpg",
    "jpeg",
];
const KNOWN_DOCUMENT_LABEL_EXTENSIONS: &[&str] = &[
    "md", "txt", "pdf", "docx", "csv", "tsv", "xls", "xlsx", "xlsb", "ods", "pptx", "png", "jpg",
    "jpeg",
];
const DOCUMENT_LABEL_ACRONYMS: &[&str] = &[
    "rag", "llm", "ocr", "pdf", "docx", "csv", "tsv", "xls", "xlsx", "xlsb", "ods", "pptx", "api",
];

#[derive(Debug, Clone)]
struct DocumentTargetCandidate {
    text: String,
    priority: usize,
}

pub(crate) fn explicit_target_document_ids_from_values<'a, I>(
    question: &str,
    values: I,
) -> BTreeSet<Uuid>
where
    I: IntoIterator<Item = (Uuid, &'a str)>,
{
    let question = current_question_segment(question);
    let normalized_question = normalize_document_target_text(question);
    if normalized_question.is_empty() {
        return BTreeSet::new();
    }

    let concrete_values = values.into_iter().collect::<Vec<_>>();
    let explicit_literals = explicit_document_reference_literals(question);
    if !explicit_literals.is_empty() {
        return explicit_document_reference_matching_document_ids(
            &explicit_literals,
            concrete_values.iter().copied(),
        );
    }
    let format_markers = explicit_document_format_markers(&normalized_question, &concrete_values);
    if !format_markers.is_empty() {
        let format_matches = explicit_document_format_matches(
            &normalized_question,
            &concrete_values,
            &format_markers,
        );
        if !format_matches.is_empty() {
            return format_matches;
        }
    }

    let mut best_match_scores = HashMap::<Uuid, (usize, usize)>::new();
    for (document_id, raw_value) in concrete_values {
        for candidate in ranked_document_target_candidates([raw_value]) {
            if document_candidate_is_matchable_for_surface(&candidate.text, &normalized_question)
                && normalized_question_contains_document_candidate(
                    &normalized_question,
                    candidate.text.as_str(),
                    "",
                )
            {
                let score = (document_candidate_length_score(&candidate.text), candidate.priority);
                best_match_scores
                    .entry(document_id)
                    .and_modify(|best| *best = (*best).max(score))
                    .or_insert(score);
            }
        }
    }

    if let Some(best_score) = best_match_scores.values().copied().max() {
        return best_match_scores
            .into_iter()
            .filter_map(|(document_id, score)| (score == best_score).then_some(document_id))
            .collect();
    }

    BTreeSet::new()
}

fn explicit_document_reference_target_ids_from_values<'a, I>(
    question: &str,
    values: I,
) -> BTreeSet<Uuid>
where
    I: IntoIterator<Item = (Uuid, &'a str)>,
{
    let question = current_question_segment(question);
    let concrete_values = values.into_iter().collect::<Vec<_>>();
    let explicit_literals = explicit_document_reference_literals(question);
    if !explicit_literals.is_empty() {
        return explicit_document_reference_matching_document_ids(
            &explicit_literals,
            concrete_values.iter().copied(),
        );
    }

    let normalized_question = normalize_document_target_text(question);
    let format_markers = explicit_document_format_markers(&normalized_question, &concrete_values);
    if format_markers.is_empty() {
        BTreeSet::new()
    } else {
        explicit_document_format_matches(&normalized_question, &concrete_values, &format_markers)
    }
}

fn explicit_document_format_markers<'a>(
    normalized_question: &str,
    values: &[(Uuid, &'a str)],
) -> Vec<&'static str> {
    let mut seen = BTreeSet::new();
    normalized_question
        .split_whitespace()
        .filter_map(|token| {
            EXPLICIT_DOCUMENT_REFERENCE_EXTENSIONS.iter().find_map(|extension| {
                (*extension == token).then_some(*extension).and_then(|extension| {
                    values
                        .iter()
                        .any(|(_, value)| {
                            normalized_explicit_document_reference_candidates(value)
                                .into_iter()
                                .any(|candidate| {
                                    candidate.rsplit_once('.').is_some_and(
                                        |(_, extension_in_value)| extension_in_value == extension,
                                    )
                                })
                        })
                        .then_some(extension)
                })
            })
        })
        .filter(|extension| seen.insert(*extension))
        .collect()
}

fn explicit_document_format_matches<'a>(
    normalized_question: &str,
    values: &[(Uuid, &'a str)],
    extensions: &[&'static str],
) -> BTreeSet<Uuid> {
    if extensions.is_empty() {
        return BTreeSet::new();
    }

    let mut matches = BTreeSet::new();
    let extension_set = extensions.iter().copied().collect::<BTreeSet<_>>();

    for (document_id, raw_value) in values {
        let mut has_matching_extension = false;
        let mut question_matches_candidate_stem = false;
        for candidate in normalized_explicit_document_reference_candidates(raw_value) {
            let Some((stem, extension)) = candidate.rsplit_once('.') else {
                continue;
            };
            if !extension_set.contains(extension) {
                continue;
            }

            has_matching_extension = true;
            for candidate in ranked_document_target_candidates([stem, &candidate]) {
                if candidate.text.len() >= 4
                    && normalized_question_contains_document_candidate(
                        normalized_question,
                        candidate.text.as_str(),
                        extension,
                    )
                {
                    question_matches_candidate_stem = true;
                    break;
                }
            }

            if question_matches_candidate_stem {
                break;
            }
        }

        if has_matching_extension && question_matches_candidate_stem {
            matches.insert(*document_id);
        }
    }

    matches
}

fn normalized_question_contains_document_candidate(
    normalized_question: &str,
    candidate: &str,
    ignored_marker: &str,
) -> bool {
    if normalized_surface_contains_token_sequence(normalized_question, candidate) {
        return true;
    }

    let marker_stripped = normalized_question
        .split_whitespace()
        .filter(|token| *token != ignored_marker)
        .collect::<Vec<_>>()
        .join(" ");
    normalized_surface_contains_token_sequence(&marker_stripped, candidate)
}

fn normalized_surface_contains_token_sequence(surface: &str, candidate: &str) -> bool {
    let surface_tokens = document_candidate_match_tokens(surface);
    let candidate_tokens = document_candidate_match_tokens(candidate);
    if candidate_tokens.is_empty() || candidate_tokens.len() > surface_tokens.len() {
        return false;
    }
    surface_tokens.windows(candidate_tokens.len()).any(|window| window == candidate_tokens)
}

fn document_candidate_match_tokens(value: &str) -> Vec<String> {
    value
        .split_whitespace()
        .map(|token| token.trim_matches('.'))
        .filter(|token| !token.is_empty())
        .map(str::to_string)
        .collect()
}

pub(crate) fn explicit_document_reference_matching_document_ids<'a, I>(
    explicit_literals: &[String],
    values: I,
) -> BTreeSet<Uuid>
where
    I: IntoIterator<Item = (Uuid, &'a str)>,
{
    let explicit_literals = explicit_literals.iter().map(String::as_str).collect::<BTreeSet<_>>();
    if explicit_literals.is_empty() {
        return BTreeSet::new();
    }

    values
        .into_iter()
        .filter_map(|(document_id, raw_value)| {
            normalized_explicit_document_reference_candidates(raw_value)
                .into_iter()
                .any(|candidate| explicit_literals.contains(candidate.as_str()))
                .then_some(document_id)
        })
        .collect()
}

pub(crate) fn explicit_document_reference_literal_is_present<'a, I>(
    explicit_literal: &str,
    values: I,
) -> bool
where
    I: IntoIterator<Item = &'a str>,
{
    values.into_iter().any(|raw_value| {
        normalized_explicit_document_reference_candidates(raw_value)
            .into_iter()
            .any(|candidate| candidate == explicit_literal)
    })
}

pub(crate) fn normalized_document_target_candidates<'a, I>(values: I) -> Vec<String>
where
    I: IntoIterator<Item = &'a str>,
{
    ranked_document_target_candidates(values).into_iter().map(|candidate| candidate.text).collect()
}

fn ranked_document_target_candidates<'a, I>(values: I) -> Vec<DocumentTargetCandidate>
where
    I: IntoIterator<Item = &'a str>,
{
    let mut seen = BTreeSet::new();
    let mut candidates = Vec::new();
    let mut push_candidate =
        |value: String, priority: usize, candidates: &mut Vec<DocumentTargetCandidate>| {
            let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
            if normalized.is_empty() || !seen.insert(normalized.clone()) {
                return;
            }
            candidates.push(DocumentTargetCandidate { text: normalized, priority });
        };

    for raw in values {
        let normalized = normalize_document_target_text(raw);
        if normalized.is_empty() {
            continue;
        }
        push_candidate(normalized.clone(), 4, &mut candidates);
        if let Some(separator_variant) = separator_normalized_document_target_candidate(&normalized)
        {
            push_candidate(separator_variant, 2, &mut candidates);
        }
        if let Some((stem, _)) = normalized.rsplit_once('.') {
            let stem = stem.trim().to_string();
            if !stem.is_empty() {
                push_candidate(stem.clone(), 3, &mut candidates);
                if let Some(separator_variant) =
                    separator_normalized_document_target_candidate(&stem)
                {
                    push_candidate(separator_variant, 1, &mut candidates);
                }
            }
        }
    }

    candidates
}

fn separator_normalized_document_target_candidate(value: &str) -> Option<String> {
    let normalized = value
        .chars()
        .map(|character| match character {
            '_' | '-' | '.' => ' ',
            _ => character,
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    (normalized != value).then_some(normalized).filter(|candidate| !candidate.is_empty())
}

fn normalized_explicit_document_reference_candidates(raw: &str) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut candidates = Vec::new();
    for value in [Some(raw), raw.rsplit(['/', '\\']).next()].into_iter().flatten() {
        let normalized = normalize_document_target_text(value);
        if !normalized.is_empty() && seen.insert(normalized.clone()) {
            candidates.push(normalized);
        }
    }
    candidates
}

pub(crate) fn normalize_document_target_text(value: &str) -> String {
    value
        .trim()
        .to_lowercase()
        .chars()
        .map(|ch| if ch.is_whitespace() || ch == ':' { ' ' } else { ch })
        .filter(|ch| ch.is_alphanumeric() || matches!(ch, '.' | '-' | '_' | ' '))
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

pub(crate) fn explicit_document_reference_literals(question: &str) -> Vec<String> {
    let question = current_question_segment(question);
    let normalized = normalize_document_target_text(question);
    let mut seen = BTreeSet::new();
    normalized
        .split_whitespace()
        .filter_map(|token| {
            let (stem, extension) = token.rsplit_once('.')?;
            if stem.is_empty() {
                return None;
            }
            EXPLICIT_DOCUMENT_REFERENCE_EXTENSIONS.contains(&extension).then(|| token.to_string())
        })
        .filter(|token| seen.insert(token.clone()))
        .collect()
}

/// Does the user's question request retrieval to span multiple documents?
///
/// Answered directly from the compiled IR — `ir.is_multi_document()` covers
/// the `QueryScope::MultiDocument` case (compare / contrast / "across
/// documents" / "which two" and so on) by construction. Without IR the
/// caller has no canonical signal, so the answer is `false`.
pub(crate) fn question_requests_multi_document_scope(
    _question: &str,
    ir: Option<&QueryIR>,
) -> bool {
    ir.is_some_and(QueryIR::is_multi_document)
}

pub(crate) fn resolve_scoped_target_document_ids(
    question: &str,
    query_ir: Option<&QueryIR>,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
) -> BTreeSet<Uuid> {
    let document_values = flattened_document_candidate_values(document_index);

    let is_follow_up = query_ir.is_some_and(QueryIR::is_follow_up);
    if let Some(current_question) = structured_current_question_segment(question) {
        let current_explicit_targets = explicit_document_reference_target_ids_from_values(
            current_question,
            document_values.iter().map(|(document_id, value)| (*document_id, value.as_str())),
        );
        if current_explicit_targets.len() == 1 {
            return current_explicit_targets;
        }
    }
    let explicit_targets = explicit_document_reference_target_ids_from_values(
        question,
        document_values.iter().map(|(document_id, value)| (*document_id, value.as_str())),
    );
    if !explicit_targets.is_empty() {
        return explicit_targets;
    }

    let Some(ir) = query_ir else {
        return BTreeSet::new();
    };
    if !query_ir_allows_document_focus_scope(ir) {
        return BTreeSet::new();
    }

    if query_ir_has_explicit_document_focus_target(ir) {
        if let Some(current_question) = structured_current_question_segment(question)
            && !is_follow_up
        {
            let current_focus_targets =
                document_ids_matching_focus_values(&[current_question], &document_values);
            if current_focus_targets.len() == 1 {
                return current_focus_targets;
            }
        }

        if !is_follow_up {
            let title_targets = explicit_target_document_ids_from_values(
                question,
                document_values.iter().map(|(document_id, value)| (*document_id, value.as_str())),
            );
            if !title_targets.is_empty() {
                return title_targets;
            }
        }
    }

    if let Some(document_focus) = &ir.document_focus {
        let hint = document_focus.hint.trim();
        if !hint.is_empty() {
            let targets = document_ids_matching_focus_hint(hint, &document_values);
            if targets.len() == 1 {
                return targets;
            }
            let entity_hints = ir
                .target_entities
                .iter()
                .filter_map(|entity| {
                    let label = entity.label.trim();
                    (!label.is_empty()).then_some(label)
                })
                .collect::<Vec<_>>();
            let targets = refine_document_focus_targets(&targets, &entity_hints, &document_values);
            return if targets.len() == 1 { targets } else { BTreeSet::new() };
        }
    }

    let target_entities_are_document_selectors = ir
        .target_types
        .iter()
        .any(|target_type| canonical_target_type_tag(target_type) == "document");
    if !target_entities_are_document_selectors {
        return BTreeSet::new();
    }

    let mut focused_targets = BTreeSet::new();
    for hint in ir.target_entities.iter().filter_map(|entity| {
        let trimmed = entity.label.trim();
        (!trimmed.is_empty()).then_some(trimmed)
    }) {
        let hint_targets = document_ids_matching_focus_hint(hint, &document_values);
        focused_targets.extend(hint_targets);
    }

    if focused_targets.len() == 1 { focused_targets } else { BTreeSet::new() }
}

pub(crate) fn query_ir_allows_document_focus_scope(ir: &QueryIR) -> bool {
    if !matches!(ir.scope, QueryScope::SingleDocument) {
        return false;
    }
    if ir.is_follow_up() {
        return false;
    }
    if query_ir_has_explicit_document_focus_target(ir) {
        return true;
    }
    !query_ir_requests_broad_document_recall(ir)
}

fn query_ir_has_explicit_document_focus_target(ir: &QueryIR) -> bool {
    query_ir_has_focused_document_answer_intent(ir)
        || ir
            .target_types
            .iter()
            .any(|target_type| canonical_target_type_tag(target_type) == "document")
}

fn query_ir_requests_broad_document_recall(ir: &QueryIR) -> bool {
    if query_ir_has_precision_literal_signal(ir) || ir.source_slice.is_some() || ir.is_follow_up() {
        return false;
    }

    if !query_ir_has_open_content_target_signal(ir) {
        return false;
    }

    ir.requests_source_coverage_context() || ir.comparison.is_some() || ir.target_entities.len() > 1
}

fn query_ir_has_open_content_target_signal(ir: &QueryIR) -> bool {
    if ir.target_types.is_empty() {
        return matches!(ir.act, QueryAct::Enumerate | QueryAct::Meta);
    }
    query_ir_targets_open_content(ir)
}

fn query_ir_targets_open_content(ir: &QueryIR) -> bool {
    query_ir_targets_graph_entities_or_relationships(ir) || classify_query_ir_intents(ir).is_empty()
}

fn query_ir_has_precision_literal_signal(ir: &QueryIR) -> bool {
    ir.literal_constraints
        .iter()
        .any(|literal| literal_span_has_precision_shape(literal.kind, &literal.text))
        && !query_ir_targets_open_content(ir)
}

fn literal_span_has_precision_shape(kind: LiteralKind, text: &str) -> bool {
    match kind {
        LiteralKind::Url | LiteralKind::Path | LiteralKind::Version => true,
        LiteralKind::Identifier => literal_text_is_identifier_shaped(text),
        LiteralKind::NumericCode | LiteralKind::Other => false,
    }
}

fn document_ids_matching_focus_values(
    hints: &[&str],
    document_values: &[(Uuid, String)],
) -> BTreeSet<Uuid> {
    let hint_tokens =
        hints.iter().flat_map(|hint| normalized_alnum_tokens(hint, 3)).collect::<BTreeSet<_>>();
    if hint_tokens.is_empty() {
        return BTreeSet::new();
    }
    let required_overlap = hint_tokens.len().clamp(1, 2);

    let mut scores = HashMap::<Uuid, usize>::new();
    for (document_id, value) in document_values {
        let value_tokens = normalized_alnum_tokens(value, 3);
        let overlap = near_token_overlap_count(&hint_tokens, &value_tokens);
        if overlap >= required_overlap {
            scores
                .entry(*document_id)
                .and_modify(|score| *score = (*score).max(overlap))
                .or_insert(overlap);
        }
    }

    let max_score = scores.values().copied().max().unwrap_or_default();
    if max_score < required_overlap {
        return BTreeSet::new();
    }
    scores
        .into_iter()
        .filter_map(|(document_id, score)| (score == max_score).then_some(document_id))
        .collect()
}

fn document_ids_matching_focus_hint(
    hint: &str,
    document_values: &[(Uuid, String)],
) -> BTreeSet<Uuid> {
    let exact_value_targets = document_ids_matching_exact_focus_value(hint, document_values);
    if !exact_value_targets.is_empty() {
        return exact_value_targets;
    }
    let contained_value_targets =
        document_ids_with_focus_value_contained_in_hint(hint, document_values);
    if !contained_value_targets.is_empty() {
        return contained_value_targets;
    }
    let exact_targets = document_ids_matching_focus_values(&[hint], document_values);
    if !exact_targets.is_empty() {
        return exact_targets;
    }
    document_ids_matching_related_focus_hint(hint, document_values)
}

fn document_ids_matching_exact_focus_value(
    hint: &str,
    document_values: &[(Uuid, String)],
) -> BTreeSet<Uuid> {
    let normalized_hint = normalize_document_target_text(hint);
    if normalized_hint.is_empty() {
        return BTreeSet::new();
    }
    document_values
        .iter()
        .filter_map(|(document_id, value)| {
            normalized_document_target_candidates([value.as_str()])
                .into_iter()
                .any(|candidate| candidate == normalized_hint)
                .then_some(*document_id)
        })
        .collect()
}

fn document_ids_with_focus_value_contained_in_hint(
    hint: &str,
    document_values: &[(Uuid, String)],
) -> BTreeSet<Uuid> {
    let normalized_hint = normalize_document_target_text(hint);
    if normalized_hint.is_empty() {
        return BTreeSet::new();
    }
    let mut scores = HashMap::<Uuid, (usize, usize, usize)>::new();
    for (document_id, value) in document_values {
        for candidate in ranked_document_target_candidates([value.as_str()]) {
            if !document_candidate_is_matchable_for_surface(&candidate.text, &normalized_hint) {
                continue;
            }
            if normalized_question_contains_document_candidate(
                &normalized_hint,
                &candidate.text,
                "",
            ) {
                let starts_hint =
                    document_candidate_starts_normalized_surface(&normalized_hint, &candidate.text)
                        as usize;
                let score = (
                    document_candidate_length_score(&candidate.text),
                    starts_hint,
                    candidate.priority,
                );
                scores
                    .entry(*document_id)
                    .and_modify(|best| *best = (*best).max(score))
                    .or_insert(score);
            }
        }
    }
    let Some(best_score) = scores.values().copied().max() else {
        return BTreeSet::new();
    };
    scores
        .into_iter()
        .filter_map(|(document_id, score)| (score == best_score).then_some(document_id))
        .collect()
}

fn document_candidate_meets_minimum_length(candidate: &str) -> bool {
    document_candidate_length_score(candidate) >= 4
}

fn document_candidate_is_matchable_for_surface(candidate: &str, surface: &str) -> bool {
    document_candidate_meets_minimum_length(candidate) || surface == candidate
}

fn document_candidate_length_score(candidate: &str) -> usize {
    candidate.chars().count()
}

fn document_candidate_starts_normalized_surface(surface: &str, candidate: &str) -> bool {
    surface == candidate
        || surface.strip_prefix(candidate).is_some_and(|suffix| suffix.starts_with(' '))
}

fn document_ids_matching_related_focus_hint(
    hint: &str,
    document_values: &[(Uuid, String)],
) -> BTreeSet<Uuid> {
    let related_candidates =
        build_related_token_candidates(document_values.iter().map(|(_, value)| value.as_str()), 3);
    let selection = select_related_overlap_tokens_from_candidates(hint, &related_candidates, 3);
    if selection.is_empty() {
        return BTreeSet::new();
    }

    let mut matches = BTreeSet::new();
    for (document_id, value) in document_values {
        let tokens = normalized_alnum_tokens(value, 3);
        if selection.matches_tokens(&tokens) {
            matches.insert(*document_id);
        }
    }
    matches
}

fn refine_document_focus_targets(
    candidates: &BTreeSet<Uuid>,
    hints: &[&str],
    document_values: &[(Uuid, String)],
) -> BTreeSet<Uuid> {
    if candidates.len() < 2 || hints.is_empty() {
        return BTreeSet::new();
    }
    let hint_tokens =
        hints.iter().flat_map(|hint| normalized_alnum_tokens(hint, 3)).collect::<BTreeSet<_>>();
    if hint_tokens.is_empty() {
        return BTreeSet::new();
    }

    let mut scores = HashMap::<Uuid, usize>::new();
    for (document_id, value) in document_values {
        if !candidates.contains(document_id) {
            continue;
        }
        let value_tokens = normalized_alnum_tokens(value, 3);
        let overlap = flexible_token_overlap_count(&hint_tokens, &value_tokens);
        if overlap > 0 {
            scores
                .entry(*document_id)
                .and_modify(|score| *score = (*score).max(overlap))
                .or_insert(overlap);
        }
    }

    let max_score = scores.values().copied().max().unwrap_or_default();
    scores
        .into_iter()
        .filter_map(|(document_id, score)| (score == max_score).then_some(document_id))
        .collect()
}

fn flexible_token_overlap_count(left: &BTreeSet<String>, right: &BTreeSet<String>) -> usize {
    left.iter()
        .filter(|left_token| {
            right.iter().any(|right_token| flexible_document_token_match(left_token, right_token))
        })
        .count()
}

fn flexible_document_token_match(left: &str, right: &str) -> bool {
    if near_token_match(left, right) {
        return true;
    }
    let left_len = left.chars().count();
    let right_len = right.chars().count();
    let min_len = left_len.min(right_len);
    if min_len < 7 {
        return false;
    }
    common_prefix_char_count(left, right) >= 6
}

fn flattened_document_candidate_values(
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
) -> Vec<(Uuid, String)> {
    let mut values = Vec::with_capacity(document_index.len().saturating_mul(3));
    for document in document_index.values() {
        if let Some(file_name) = document.file_name.as_deref() {
            values.push((document.document_id, file_name.to_string()));
        }
        if let Some(title) = document.title.as_deref() {
            values.push((document.document_id, title.to_string()));
        }
        values.push((document.document_id, document.external_key.to_string()));
    }
    values
}

pub(crate) fn focused_answer_document_id(
    question: &str,
    chunks: &[RuntimeMatchedChunk],
) -> Option<Uuid> {
    let question = current_question_segment(question);
    if chunks.is_empty() || question_requests_multi_document_scope(question, None) {
        return None;
    }

    let explicit_targets = explicit_target_document_ids_from_values(
        question,
        chunks.iter().map(|chunk| (chunk.document_id, chunk.document_label.as_str())),
    );
    if explicit_targets.len() == 1 {
        return explicit_targets.iter().next().copied();
    }

    #[derive(Debug, Clone)]
    struct DocumentFocusScore {
        document_id: Uuid,
        document_label: String,
        score_sum: f32,
        chunk_count: usize,
        first_rank: usize,
        label_keyword_hits: usize,
        label_marker_hits: usize,
    }

    let question_keywords = crate::services::query::planner::extract_keywords(question);
    let mut per_document = HashMap::<Uuid, DocumentFocusScore>::new();
    for (rank, chunk) in chunks.iter().enumerate() {
        let lowered_label = chunk.document_label.to_lowercase();
        let entry = per_document.entry(chunk.document_id).or_insert_with(|| DocumentFocusScore {
            document_id: chunk.document_id,
            document_label: chunk.document_label.clone(),
            score_sum: 0.0,
            chunk_count: 0,
            first_rank: rank,
            label_keyword_hits: question_keywords
                .iter()
                .filter(|keyword| lowered_label.contains(keyword.as_str()))
                .count(),
            label_marker_hits: document_focus_marker_hits(question, &chunk.document_label),
        });
        entry.score_sum += score_value(chunk.score);
        entry.chunk_count += 1;
        entry.first_rank = entry.first_rank.min(rank);
    }

    let mut ranked = per_document.into_values().collect::<Vec<_>>();
    if ranked.is_empty() {
        return None;
    }
    ranked.sort_by(|left, right| {
        right.label_marker_hits.cmp(&left.label_marker_hits).then_with(|| {
            right
                .score_sum
                .total_cmp(&left.score_sum)
                .then_with(|| right.chunk_count.cmp(&left.chunk_count))
                .then_with(|| right.label_keyword_hits.cmp(&left.label_keyword_hits))
                .then_with(|| left.first_rank.cmp(&right.first_rank))
                .then_with(|| left.document_label.cmp(&right.document_label))
        })
    });

    if ranked.len() == 1 {
        return Some(ranked[0].document_id);
    }

    let top = &ranked[0];
    let second = &ranked[1];
    if top.label_marker_hits > second.label_marker_hits && top.label_marker_hits > 0 {
        return Some(top.document_id);
    }

    let has_explicit_single_source_anchor = question_mentions_single_source_anchor(question);
    let materially_higher_score =
        top.score_sum >= second.score_sum * DOMINANT_DOCUMENT_SCORE_MULTIPLIER;
    let materially_more_chunks = top.chunk_count > second.chunk_count;
    let stronger_label_match = top.label_keyword_hits > second.label_keyword_hits;

    if has_explicit_single_source_anchor
        || materially_higher_score
        || materially_more_chunks
        || stronger_label_match
    {
        Some(top.document_id)
    } else {
        None
    }
}

pub(crate) fn document_focus_marker_hits(question: &str, document_label: &str) -> usize {
    let lowered_question = current_question_segment(question).to_lowercase();
    document_label_focus_markers(document_label)
        .into_iter()
        .filter(|marker| question_mentions_document_marker(&lowered_question, marker))
        .count()
}

pub(crate) fn concise_document_subject_label(document_label: &str) -> String {
    let normalized = strip_known_document_label_extension(
        document_label
            .split(" - ")
            .next()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(document_label),
    )
    .replace(['_', '-'], " ");
    let normalized = normalized.trim().strip_suffix(" wikipedia").unwrap_or(&normalized).trim();
    if normalized.is_empty() {
        return document_label.to_string();
    }

    if normalized
        .split_whitespace()
        .skip(1)
        .any(|word| word.chars().any(|character| character.is_ascii_uppercase()))
    {
        return normalized.to_string();
    }

    let mut words = normalized.split_whitespace().map(title_case_document_word).collect::<Vec<_>>();
    if words.len() > 1 {
        for word in words.iter_mut().skip(1) {
            if !word.chars().all(|character| character.is_ascii_uppercase()) {
                *word = word.to_lowercase();
            }
        }
    }
    words.join(" ")
}

fn strip_known_document_label_extension(document_label: &str) -> &str {
    let trimmed = document_label.trim();
    let Some((stem, extension)) = trimmed.rsplit_once('.') else {
        return trimmed;
    };
    let lowered_extension = extension.to_ascii_lowercase();
    if KNOWN_DOCUMENT_LABEL_EXTENSIONS.contains(&lowered_extension.as_str()) {
        stem
    } else {
        trimmed
    }
}

fn document_label_focus_markers(document_label: &str) -> Vec<&'static str> {
    let lowered_label = document_label.to_lowercase();
    let mut markers = Vec::new();
    if let Some(extension_marker) = document_label_extension_marker(&lowered_label) {
        markers.push(extension_marker);
    }
    markers
}

fn document_label_extension_marker(lowered_label: &str) -> Option<&'static str> {
    let (_, extension) = lowered_label.rsplit_once('.')?;
    match extension {
        "pdf" => Some("pdf"),
        "docx" => Some("docx"),
        "csv" => Some("csv"),
        "tsv" => Some("tsv"),
        "xls" => Some("xls"),
        "xlsx" => Some("xlsx"),
        "xlsb" => Some("xlsb"),
        "ods" => Some("ods"),
        "pptx" => Some("pptx"),
        "png" => Some("png"),
        "jpg" => Some("jpg"),
        "jpeg" => Some("jpeg"),
        _ => None,
    }
}

fn question_mentions_document_marker(lowered_question: &str, marker: &str) -> bool {
    let extension_marker = format!(".{marker}");
    let extension_match = lowered_question.match_indices(&extension_marker).any(|(start, _)| {
        let end = start + extension_marker.len();
        lowered_question[end..]
            .chars()
            .next()
            .is_none_or(|character| !character.is_ascii_alphanumeric())
    });
    extension_match
        || lowered_question
            .split(|character: char| !character.is_ascii_alphanumeric())
            .any(|token| token == marker)
}

fn question_mentions_single_source_anchor(question: &str) -> bool {
    let _ = question;
    false
}

fn title_case_document_word(word: &str) -> String {
    if word.is_empty() {
        return String::new();
    }
    let lowered = word.to_lowercase();
    if DOCUMENT_LABEL_ACRONYMS.contains(&lowered.as_str()) {
        return lowered.to_uppercase();
    }

    let mut chars = lowered.chars();
    let Some(first) = chars.next() else {
        return String::new();
    };
    first.to_uppercase().collect::<String>() + chars.as_str()
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeSet, HashMap};

    use chrono::Utc;
    use uuid::Uuid;

    use super::{
        document_ids_with_focus_value_contained_in_hint,
        explicit_document_reference_literal_is_present, explicit_document_reference_literals,
        explicit_target_document_ids_from_values, normalize_document_target_text,
        query_ir_allows_document_focus_scope, resolve_scoped_target_document_ids,
    };
    use crate::domains::query_ir::{
        ConversationRefKind, DocumentHint, EntityMention, EntityRole, LiteralKind, LiteralSpan,
        QueryAct, QueryIR, QueryLanguage, QueryScope, UnresolvedRef,
    };
    use crate::services::query::effective_query::{
        EFFECTIVE_QUERY_QUESTION_PREFIX, EFFECTIVE_QUERY_SCOPE_PREFIX,
    };

    fn effective_query_text(scope: &str, question: &str) -> String {
        format!(
            "{EFFECTIVE_QUERY_SCOPE_PREFIX} {scope}\n{EFFECTIVE_QUERY_QUESTION_PREFIX} {question}"
        )
    }

    fn scoped_query_ir(
        scope: QueryScope,
        document_focus: Option<&str>,
        target_entities: &[&str],
    ) -> QueryIR {
        QueryIR {
            act: QueryAct::RetrieveValue,
            scope,
            language: QueryLanguage::Auto,
            target_types: Vec::new(),
            target_entities: target_entities
                .iter()
                .map(|value| EntityMention {
                    label: (*value).to_string(),
                    role: EntityRole::Subject,
                })
                .collect(),
            literal_constraints: Vec::new(),
            temporal_constraints: Vec::new(),
            comparison: None,
            document_focus: document_focus.map(|hint| DocumentHint { hint: hint.to_string() }),
            conversation_refs: Vec::new(),
            needs_clarification: None,
            source_slice: None,
            retrieval_query: None,
            confidence: 1.0,
        }
    }

    fn scoped_document_index<'a>(
        entries: impl IntoIterator<Item = (Uuid, &'a str, Option<&'a str>, &'a str)>,
    ) -> HashMap<Uuid, crate::infra::arangodb::document_store::KnowledgeDocumentRow> {
        let mut index = HashMap::new();
        let library_id = Uuid::now_v7();
        let workspace_id = Uuid::now_v7();
        for (document_id, file_name, title, external_key) in entries {
            index.insert(
                document_id,
                crate::infra::arangodb::document_store::KnowledgeDocumentRow {
                    key: Uuid::now_v7().to_string(),
                    arango_id: None,
                    arango_rev: None,
                    document_id,
                    workspace_id,
                    library_id,
                    external_key: external_key.to_string(),
                    title: title.map(std::string::ToString::to_string),
                    source_uri: None,
                    document_hint: None,
                    document_state: "active".to_string(),
                    active_revision_id: Some(Uuid::now_v7()),
                    readable_revision_id: None,
                    latest_revision_no: Some(1),
                    parent_document_id: None,
                    document_role: crate::domains::content::DOCUMENT_ROLE_PRIMARY.to_string(),
                    created_at: Utc::now(),
                    updated_at: Utc::now(),
                    deleted_at: None,
                    file_name: Some(file_name.to_string()),
                },
            );
        }
        index
    }

    #[test]
    fn resolve_scoped_target_document_ids_prefers_explicit_reference() {
        let scoped_document_id = Uuid::now_v7();
        let other_document_id = Uuid::now_v7();
        let index = scoped_document_index([
            (scoped_document_id, "graphql-api.pdf", Some("GraphQL API"), "graphql-api.pdf"),
            (other_document_id, "rest-api.pdf", Some("REST API"), "rest-api.pdf"),
        ]);

        let ir = scoped_query_ir(QueryScope::SingleDocument, Some("REST API"), &["rest"]);
        let target_ids = resolve_scoped_target_document_ids(
            "Read graphql-api.pdf for the auth setup section",
            Some(&ir),
            &index,
        );

        assert_eq!(target_ids, BTreeSet::from([scoped_document_id]));
    }

    #[test]
    fn resolve_scoped_target_document_ids_keeps_explicit_reference_for_follow_up() {
        let scoped_document_id = Uuid::now_v7();
        let other_document_id = Uuid::now_v7();
        let index = scoped_document_index([
            (scoped_document_id, "alpha-guide.pdf", Some("Alpha Guide"), "alpha-guide.pdf"),
            (other_document_id, "beta-guide.pdf", Some("Beta Guide"), "beta-guide.pdf"),
        ]);

        let mut ir = scoped_query_ir(QueryScope::SingleDocument, Some("Beta Guide"), &["beta"]);
        ir.act = QueryAct::FollowUp;
        ir.conversation_refs.push(UnresolvedRef {
            surface: "that document".to_string(),
            kind: ConversationRefKind::Deictic,
        });
        let target_ids = resolve_scoped_target_document_ids(
            "Use alpha-guide.pdf for the setup details",
            Some(&ir),
            &index,
        );

        assert_eq!(target_ids, BTreeSet::from([scoped_document_id]));
    }

    #[test]
    fn resolve_scoped_target_document_ids_does_not_hard_scope_follow_up_focus() {
        let alpha_document_id = Uuid::now_v7();
        let beta_document_id = Uuid::now_v7();
        let index = scoped_document_index([
            (alpha_document_id, "alpha-guide.md", Some("alpha service handbook"), "alpha-guide.md"),
            (beta_document_id, "beta-guide.md", Some("beta service handbook"), "beta-guide.md"),
        ]);

        let mut ir = scoped_query_ir(QueryScope::SingleDocument, Some("alpha service"), &["alpha"]);
        ir.conversation_refs.push(UnresolvedRef {
            surface: "that one".to_string(),
            kind: ConversationRefKind::Deictic,
        });
        let target_ids =
            resolve_scoped_target_document_ids("What about that one?", Some(&ir), &index);

        assert!(target_ids.is_empty());
        assert!(!query_ir_allows_document_focus_scope(&ir));
    }

    #[test]
    fn resolve_scoped_target_document_ids_does_not_treat_follow_up_subject_as_document_literal() {
        let alpha_document_id = Uuid::now_v7();
        let beta_document_id = Uuid::now_v7();
        let index = scoped_document_index([
            (alpha_document_id, "alpha-guide.md", Some("Alpha Provider Guide"), "alpha-guide.md"),
            (beta_document_id, "beta-guide.md", Some("Beta Provider Guide"), "beta-guide.md"),
        ]);

        let mut ir = scoped_query_ir(QueryScope::SingleDocument, Some("Beta Provider"), &["Beta"]);
        ir.conversation_refs.push(UnresolvedRef {
            surface: "Beta".to_string(),
            kind: ConversationRefKind::Elliptic,
        });
        let target_ids = resolve_scoped_target_document_ids("Beta", Some(&ir), &index);

        assert!(target_ids.is_empty());
    }

    #[test]
    fn resolve_scoped_target_document_ids_selects_single_match_from_query_ir_scope() {
        let alpha_document_id = Uuid::now_v7();
        let beta_document_id = Uuid::now_v7();
        let index = scoped_document_index([
            (alpha_document_id, "alpha-guide.md", Some("alpha service handbook"), "alpha-guide.md"),
            (beta_document_id, "beta-guide.md", Some("beta service handbook"), "beta-guide.md"),
        ]);
        let ir = scoped_query_ir(QueryScope::SingleDocument, Some("alpha service"), &["alpha"]);

        let target_ids = resolve_scoped_target_document_ids(
            "Where are the auth requirements?",
            Some(&ir),
            &index,
        );

        assert_eq!(target_ids, BTreeSet::from([alpha_document_id]));
    }

    #[test]
    fn resolve_scoped_target_document_ids_prefers_exact_focus_title_over_prefixed_variants() {
        let focused_document_id = Uuid::now_v7();
        let image_document_id = Uuid::now_v7();
        let appendix_document_id = Uuid::now_v7();
        let index = scoped_document_index([
            (focused_document_id, "provider-alpha.md", Some("Provider Alpha"), "provider-alpha.md"),
            (
                image_document_id,
                "provider-alpha-screen.png",
                Some("Provider Alpha: payment screen.png"),
                "provider-alpha-screen.png",
            ),
            (
                appendix_document_id,
                "provider-alpha-appendix.md",
                Some("Provider Alpha appendix"),
                "provider-alpha-appendix.md",
            ),
        ]);
        let ir = scoped_query_ir(QueryScope::SingleDocument, Some("Provider Alpha"), &["Alpha"]);

        let target_ids = resolve_scoped_target_document_ids(
            "How do I configure Provider Alpha?",
            Some(&ir),
            &index,
        );

        assert_eq!(target_ids, BTreeSet::from([focused_document_id]));
    }

    #[test]
    fn resolve_scoped_target_document_ids_prefers_focus_title_inside_longer_hint() {
        let focused_document_id = Uuid::now_v7();
        let image_document_id = Uuid::now_v7();
        let appendix_document_id = Uuid::now_v7();
        let tangential_document_id = Uuid::now_v7();
        let index = scoped_document_index([
            (focused_document_id, "provider-alpha.md", Some("Provider Alpha"), "provider-alpha.md"),
            (
                image_document_id,
                "provider-alpha-screen.png",
                Some("Provider Alpha: payment screen.png"),
                "provider-alpha-screen.png",
            ),
            (
                appendix_document_id,
                "provider-alpha-appendix.md",
                Some("Provider Alpha appendix"),
                "provider-alpha-appendix.md",
            ),
            (
                tangential_document_id,
                "alpha-suite.md",
                Some("Alpha in Retail Suite"),
                "alpha-suite.md",
            ),
        ]);
        let ir = scoped_query_ir(
            QueryScope::SingleDocument,
            Some("Provider Alpha in Retail Suite"),
            &[],
        );
        let mut ir = ir;
        ir.target_types = vec!["document".to_string()];

        let target_ids = resolve_scoped_target_document_ids(
            "How do I configure Provider Alpha?",
            Some(&ir),
            &index,
        );

        assert_eq!(target_ids, BTreeSet::from([focused_document_id]));
    }

    #[test]
    fn resolve_scoped_target_document_ids_prefers_current_question_over_scope_history() {
        let focused_document_id = Uuid::now_v7();
        let stale_document_id = Uuid::now_v7();
        let index = scoped_document_index([
            (focused_document_id, "provider-alpha.md", Some("Provider Alpha"), "provider-alpha.md"),
            (
                stale_document_id,
                "provider-beta-deprecated.md",
                Some("Provider Beta Deprecated"),
                "provider-beta-deprecated.md",
            ),
        ]);
        let ir = scoped_query_ir(QueryScope::SingleDocument, Some("Provider Alpha"), &["Alpha"]);

        let target_ids = resolve_scoped_target_document_ids(
            &effective_query_text(
                "How do I configure payments?\nOptions: Provider Alpha; Provider Beta Deprecated.",
                "Alpha Provider setup",
            ),
            Some(&ir),
            &index,
        );

        assert_eq!(target_ids, BTreeSet::from([focused_document_id]));
    }

    #[test]
    fn resolve_scoped_target_document_ids_keeps_follow_up_subject_out_of_fast_path() {
        let focused_document_id = Uuid::now_v7();
        let index = scoped_document_index([(
            focused_document_id,
            "provider-alpha.md",
            Some("Provider Alpha"),
            "provider-alpha.md",
        )]);
        let mut ir =
            scoped_query_ir(QueryScope::SingleDocument, Some("Provider Alpha"), &["Alpha"]);
        ir.act = QueryAct::FollowUp;

        let target_ids = resolve_scoped_target_document_ids(
            &effective_query_text("Prior answer discussed Provider Alpha.", "Provider Alpha"),
            Some(&ir),
            &index,
        );

        assert!(target_ids.is_empty());
    }

    #[test]
    fn resolve_scoped_target_document_ids_uses_related_focus_prefix() {
        let focused_document_id = Uuid::now_v7();
        let other_document_id = Uuid::now_v7();
        let index = scoped_document_index([
            (
                focused_document_id,
                "acmealpha-guide.md",
                Some("Acmealpha payment setup guide"),
                "acmealpha-guide.md",
            ),
            (other_document_id, "beta-guide.md", Some("Beta payment setup guide"), "beta-guide.md"),
        ]);
        let ir = scoped_query_ir(
            QueryScope::SingleDocument,
            Some("Acmew"),
            &["installation", "configuration file", "parameters"],
        );

        let target_ids =
            resolve_scoped_target_document_ids("Show the setup details.", Some(&ir), &index);

        assert_eq!(target_ids, BTreeSet::from([focused_document_id]));
    }

    #[test]
    fn resolve_scoped_target_document_ids_keeps_document_focus_when_entities_are_values() {
        let alpha_document_id = Uuid::now_v7();
        let beta_document_id = Uuid::now_v7();
        let index = scoped_document_index([
            (alpha_document_id, "alpha-guide.md", Some("alpha service handbook"), "alpha-guide.md"),
            (beta_document_id, "beta-guide.md", Some("beta service handbook"), "beta-guide.md"),
        ]);
        let ir = scoped_query_ir(
            QueryScope::SingleDocument,
            Some("alpha service"),
            &["renewal policy", "escalation target"],
        );

        let target_ids =
            resolve_scoped_target_document_ids("What is the renewal policy?", Some(&ir), &index);

        assert_eq!(target_ids, BTreeSet::from([alpha_document_id]));
    }

    #[test]
    fn compare_concept_query_ir_does_not_enable_document_focus_scope() {
        let mut ir = scoped_query_ir(
            QueryScope::SingleDocument,
            Some("Alpha Suite"),
            &["connector options", "fallback behavior", "regional limits"],
        );
        ir.act = QueryAct::Compare;
        ir.target_types = vec!["concept".to_string()];

        assert!(
            !query_ir_allows_document_focus_scope(&ir),
            "broad compare over concepts must preserve cross-document recall"
        );
    }

    #[test]
    fn describe_concept_query_ir_does_not_enable_document_focus_scope() {
        let mut ir = scoped_query_ir(
            QueryScope::SingleDocument,
            Some("Alpha Suite"),
            &["connector options"],
        );
        ir.act = QueryAct::Describe;
        ir.target_types = vec!["concept".to_string()];

        assert!(
            !query_ir_allows_document_focus_scope(&ir),
            "open-content descriptions must preserve source coverage unless the IR explicitly targets a document"
        );
    }

    #[test]
    fn configure_multi_target_query_ir_does_not_enable_document_focus_scope() {
        let mut ir = scoped_query_ir(
            QueryScope::SingleDocument,
            Some("Alpha Suite"),
            &["connector options", "fallback behavior", "regional limits"],
        );
        ir.act = QueryAct::ConfigureHow;
        ir.target_types = vec!["procedure".to_string()];

        assert!(
            !query_ir_allows_document_focus_scope(&ir),
            "multi-topic procedural questions must not collapse to one hinted document"
        );
    }

    #[test]
    fn broad_content_literal_other_does_not_enable_document_focus_scope() {
        let mut ir = scoped_query_ir(
            QueryScope::SingleDocument,
            Some("Alpha Suite connector options"),
            &["connector options", "fallback behavior", "regional limits"],
        );
        ir.act = QueryAct::Enumerate;
        ir.target_types = vec!["concept".to_string()];
        ir.literal_constraints =
            vec![LiteralSpan { text: "Alpha Suite".to_string(), kind: LiteralKind::Other }];

        assert!(
            !query_ir_allows_document_focus_scope(&ir),
            "broad open-content literals must not force single-document packing"
        );
    }

    #[test]
    fn plain_alphabetic_identifier_does_not_enable_document_focus_scope() {
        let mut ir = scoped_query_ir(
            QueryScope::SingleDocument,
            Some("Alpha Suite fallback behavior"),
            &["path", "condition"],
        );
        ir.act = QueryAct::RetrieveValue;
        ir.target_types = vec!["path".to_string(), "concept".to_string()];
        ir.literal_constraints =
            vec![LiteralSpan { text: "alpha".to_string(), kind: LiteralKind::Identifier }];

        assert!(
            !query_ir_allows_document_focus_scope(&ir),
            "plain alphabetic literals are weak topic echoes and must not force single-document packing"
        );
    }

    #[test]
    fn plain_alphabetic_identifier_does_not_block_enumerate_broad_recall() {
        let mut ir = scoped_query_ir(
            QueryScope::SingleDocument,
            Some("Alpha Suite connector options"),
            &["connector options", "fallback behavior"],
        );
        ir.act = QueryAct::Enumerate;
        ir.literal_constraints =
            vec![LiteralSpan { text: "alpha".to_string(), kind: LiteralKind::Identifier }];

        assert!(
            !query_ir_allows_document_focus_scope(&ir),
            "plain alphabetic identifier literals must not cancel broad enumerate recall"
        );
    }

    #[test]
    fn exact_lookup_query_ir_keeps_document_focus_scope() {
        let mut ir = scoped_query_ir(
            QueryScope::SingleDocument,
            Some("Alpha Suite Admin Guide"),
            &["callback URL"],
        );
        ir.act = QueryAct::RetrieveValue;
        ir.target_types = vec!["url".to_string()];
        ir.literal_constraints =
            vec![LiteralSpan { text: "callbackUrl".to_string(), kind: LiteralKind::Identifier }];

        assert!(
            query_ir_allows_document_focus_scope(&ir),
            "exact lookup intents may use the single-document focus for precision and speed"
        );
    }

    #[test]
    fn compare_document_query_ir_keeps_explicit_document_focus_scope() {
        let mut ir = scoped_query_ir(
            QueryScope::SingleDocument,
            Some("Alpha Suite Admin Guide"),
            &["current section", "previous section"],
        );
        ir.act = QueryAct::Compare;
        ir.target_types = vec!["document".to_string()];

        assert!(
            query_ir_allows_document_focus_scope(&ir),
            "compare may pack one document only when the typed IR explicitly targets a document"
        );
    }

    #[test]
    fn resolve_scoped_target_document_ids_refines_focus_with_entity_prefix_overlap() {
        let catalog_document_id = Uuid::now_v7();
        let generic_document_id = Uuid::now_v7();
        let index = scoped_document_index([
            (
                catalog_document_id,
                "catalog-options.md",
                Some("Alpha Suite integrated connector catalog"),
                "catalog-options.md",
            ),
            (
                generic_document_id,
                "alpha-overview.md",
                Some("Alpha Suite overview"),
                "alpha-overview.md",
            ),
        ]);
        let ir = scoped_query_ir(
            QueryScope::SingleDocument,
            Some("Alpha Suite"),
            &["integration variants", "connected catalog"],
        );

        let target_ids =
            resolve_scoped_target_document_ids("Enumerate the variants.", Some(&ir), &index);

        assert_eq!(target_ids, BTreeSet::from([catalog_document_id]));
    }

    #[test]
    fn resolve_scoped_target_document_ids_does_not_hard_scope_enumerate_focus() {
        let focused_document_id = Uuid::now_v7();
        let companion_document_id = Uuid::now_v7();
        let index = scoped_document_index([
            (
                focused_document_id,
                "alpha-overview.md",
                Some("Alpha Suite overview"),
                "alpha-overview.md",
            ),
            (
                companion_document_id,
                "alpha-connectors.md",
                Some("Alpha Suite connector catalog"),
                "alpha-connectors.md",
            ),
        ]);
        let mut ir = scoped_query_ir(
            QueryScope::SingleDocument,
            Some("Alpha Suite"),
            &["connector catalog"],
        );
        ir.act = QueryAct::Enumerate;

        let target_ids = resolve_scoped_target_document_ids(
            "Enumerate the connector options.",
            Some(&ir),
            &index,
        );

        assert!(
            target_ids.is_empty(),
            "enumeration questions must keep library-wide recall unless the user names a concrete document"
        );
    }

    #[test]
    fn resolve_scoped_target_document_ids_keeps_enumerate_document_target() {
        let focused_document_id = Uuid::now_v7();
        let companion_document_id = Uuid::now_v7();
        let index = scoped_document_index([
            (
                focused_document_id,
                "alpha-overview.md",
                Some("Alpha Suite overview"),
                "alpha-overview.md",
            ),
            (
                companion_document_id,
                "alpha-connectors.md",
                Some("Alpha Suite connector catalog"),
                "alpha-connectors.md",
            ),
        ]);
        let mut ir = scoped_query_ir(
            QueryScope::SingleDocument,
            Some("Alpha Suite"),
            &["connector catalog"],
        );
        ir.act = QueryAct::Enumerate;
        ir.target_types = vec!["document".to_string()];

        let target_ids = resolve_scoped_target_document_ids(
            "Enumerate the sections in Alpha Suite overview.",
            Some(&ir),
            &index,
        );

        assert_eq!(target_ids, BTreeSet::from([focused_document_id]));
    }

    #[test]
    fn resolve_scoped_target_document_ids_keeps_focus_anchor_before_entity_refine() {
        let focused_document_id = Uuid::now_v7();
        let entity_collision_document_id = Uuid::now_v7();
        let index = scoped_document_index([
            (
                focused_document_id,
                "alpha-overview.md",
                Some("Alpha Suite overview"),
                "alpha-overview.md",
            ),
            (
                entity_collision_document_id,
                "beta-connectors.md",
                Some("Beta Suite integrated connector catalog"),
                "beta-connectors.md",
            ),
        ]);
        let ir = scoped_query_ir(
            QueryScope::SingleDocument,
            Some("Alpha Suite"),
            &["connector catalog"],
        );

        let target_ids = resolve_scoped_target_document_ids(
            "Enumerate the connector catalog.",
            Some(&ir),
            &index,
        );

        assert_eq!(target_ids, BTreeSet::from([focused_document_id]));
    }

    #[test]
    fn resolve_scoped_target_document_ids_does_not_prefix_loosen_primary_focus() {
        let exact_document_id = Uuid::now_v7();
        let prefix_collision_document_id = Uuid::now_v7();
        let index = scoped_document_index([
            (
                exact_document_id,
                "alpha-integrated.md",
                Some("Alpha integrated connector guide"),
                "alpha-integrated.md",
            ),
            (
                prefix_collision_document_id,
                "alpha-integration.md",
                Some("Alpha integration connector guide"),
                "alpha-integration.md",
            ),
        ]);
        let ir = scoped_query_ir(QueryScope::SingleDocument, Some("Alpha integrated"), &[]);

        let target_ids =
            resolve_scoped_target_document_ids("Open Alpha integrated.", Some(&ir), &index);

        assert_eq!(target_ids, BTreeSet::from([exact_document_id]));
    }

    #[test]
    fn resolve_scoped_target_document_ids_rejects_ambiguous_focus_refine() {
        let first_document_id = Uuid::now_v7();
        let second_document_id = Uuid::now_v7();
        let index = scoped_document_index([
            (
                first_document_id,
                "alpha-connectors-a.md",
                Some("Alpha Suite integrated connector catalog"),
                "alpha-connectors-a.md",
            ),
            (
                second_document_id,
                "alpha-connectors-b.md",
                Some("Alpha Suite connector catalog matrix"),
                "alpha-connectors-b.md",
            ),
        ]);
        let ir = scoped_query_ir(
            QueryScope::SingleDocument,
            Some("Alpha Suite"),
            &["connector catalog"],
        );

        let target_ids = resolve_scoped_target_document_ids(
            "Enumerate the connector catalog.",
            Some(&ir),
            &index,
        );

        assert!(target_ids.is_empty());
    }

    #[test]
    fn resolve_scoped_target_document_ids_returns_empty_for_ambiguous_query_ir_focus() {
        let alpha_document_id = Uuid::now_v7();
        let beta_document_id = Uuid::now_v7();
        let index = scoped_document_index([
            (
                alpha_document_id,
                "service-overview.md",
                Some("service overview"),
                "service-overview.md",
            ),
            (beta_document_id, "service-notes.md", Some("service notes"), "service-notes.md"),
        ]);
        let ir = scoped_query_ir(QueryScope::SingleDocument, Some("service"), &["service"]);

        let target_ids =
            resolve_scoped_target_document_ids("What does the service handle?", Some(&ir), &index);

        assert!(target_ids.is_empty());
    }

    #[test]
    fn resolve_scoped_target_document_ids_ignores_focus_when_not_single_document_scope() {
        let scoped_document_id = Uuid::now_v7();
        let index = scoped_document_index([(
            scoped_document_id,
            "platform-notes.md",
            Some("platform notes"),
            "platform-notes.md",
        )]);
        let ir = scoped_query_ir(QueryScope::MultiDocument, Some("platform"), &["platform"]);

        assert!(
            resolve_scoped_target_document_ids("Which two services...?", Some(&ir), &index)
                .is_empty()
        );
    }

    #[test]
    fn explicit_target_document_ids_prefer_exact_extension_match() {
        let csv_id = Uuid::now_v7();
        let xlsx_id = Uuid::now_v7();
        let matched = explicit_target_document_ids_from_values(
            "In people-100.csv what is Shelby Terrell's job title?",
            [(csv_id, "people-100.csv"), (xlsx_id, "people-100.xlsx")],
        );
        assert_eq!(matched, [csv_id].into_iter().collect());
    }

    #[test]
    fn explicit_target_document_ids_do_not_fuzzy_match_different_file_reference() {
        let organizations_id = Uuid::now_v7();
        let matched = explicit_target_document_ids_from_values(
            "In people-100.csv what is Shelby Terrell's job title?",
            [(organizations_id, "organizations-100.csv")],
        );
        assert!(matched.is_empty());
    }

    #[test]
    fn explicit_target_document_ids_keep_stem_ambiguous_without_extension() {
        let csv_id = Uuid::now_v7();
        let xlsx_id = Uuid::now_v7();
        let matched = explicit_target_document_ids_from_values(
            "What is in people-100?",
            [(csv_id, "people-100.csv"), (xlsx_id, "people-100.xlsx")],
        );
        assert_eq!(matched, [csv_id, xlsx_id].into_iter().collect());
    }

    #[test]
    fn explicit_target_document_ids_prefer_format_marker_with_same_stem() {
        let pdf_id = Uuid::now_v7();
        let docx_id = Uuid::now_v7();
        let pptx_id = Uuid::now_v7();
        let matched = explicit_target_document_ids_from_values(
            "What report name appears in the runtime PDF upload check?",
            [
                (pdf_id, "runtime_upload_check.pdf"),
                (docx_id, "runtime_upload_check.docx"),
                (pptx_id, "runtime_upload_check.pptx"),
            ],
        );
        assert_eq!(matched, [pdf_id].into_iter().collect());
    }

    #[test]
    fn explicit_target_document_ids_keep_stem_ambiguous_without_format_marker() {
        let pdf_id = Uuid::now_v7();
        let docx_id = Uuid::now_v7();
        let pptx_id = Uuid::now_v7();
        let matched = explicit_target_document_ids_from_values(
            "What report name appears in the runtime upload check?",
            [
                (pdf_id, "runtime_upload_check.pdf"),
                (docx_id, "runtime_upload_check.docx"),
                (pptx_id, "runtime_upload_check.pptx"),
            ],
        );
        assert_eq!(matched, [pdf_id, docx_id, pptx_id].into_iter().collect());
    }

    #[test]
    fn explicit_target_document_ids_match_unicode_title_phrase_inside_long_question() {
        let menu_id = Uuid::now_v7();
        let matched = explicit_target_document_ids_from_values(
            "How do I complete the café menu update before opening?",
            [(menu_id, "Café menu")],
        );
        assert_eq!(matched, [menu_id].into_iter().collect());
    }

    #[test]
    fn explicit_target_document_ids_match_separator_normalized_document_stems() {
        let monitoring_id = Uuid::now_v7();
        let schema_id = Uuid::now_v7();
        let matched = explicit_target_document_ids_from_values(
            "What alert rules are defined in the monitoring dashboard documentation?",
            [(monitoring_id, "monitoring_dashboard.pdf"), (schema_id, "database_schema.pdf")],
        );
        assert_eq!(matched, [monitoring_id].into_iter().collect());
    }

    #[test]
    fn normalize_document_target_text_splits_colon_boundaries_canonically() {
        assert_eq!(normalize_document_target_text("Provider:Alpha"), "provider alpha");
        assert_eq!(normalize_document_target_text("Provider : Alpha"), "provider alpha");
        assert_eq!(
            normalize_document_target_text("Provider:Alpha:Guide.md"),
            "provider alpha guide.md"
        );
        assert_eq!(normalize_document_target_text("Provider\tAlpha"), "provider alpha");
    }

    #[test]
    fn focus_value_contained_in_hint_prefers_longer_document_candidate() {
        let short_id = Uuid::now_v7();
        let long_id = Uuid::now_v7();
        let unrelated_prefix_id = Uuid::now_v7();
        let matched = document_ids_with_focus_value_contained_in_hint(
            "Open guide for Provider Alpha Configuration",
            &[
                (short_id, "Provider Alpha".to_string()),
                (long_id, "Provider Alpha Configuration".to_string()),
                (unrelated_prefix_id, "Open guide".to_string()),
            ],
        );

        assert_eq!(matched, [long_id].into_iter().collect());
    }

    #[test]
    fn explicit_target_document_ids_keep_longest_separator_match_canonical() {
        let generic_id = Uuid::now_v7();
        let specific_id = Uuid::now_v7();
        let matched = explicit_target_document_ids_from_values(
            "Summarize the monitoring dashboard guide.",
            [
                (generic_id, "monitoring_dashboard.pdf"),
                (specific_id, "monitoring_dashboard_guide.pdf"),
            ],
        );
        assert_eq!(matched, [specific_id].into_iter().collect());
    }

    #[test]
    fn explicit_target_document_ids_prefers_current_question_segment() {
        let focused_id = Uuid::now_v7();
        let stale_id = Uuid::now_v7();
        let matched = explicit_target_document_ids_from_values(
            &effective_query_text(
                "Prior assistant listed Provider Alpha Admin Guide.",
                "Provider Alpha setup",
            ),
            [(focused_id, "Provider Alpha"), (stale_id, "Provider Alpha Admin Guide")],
        );
        assert_eq!(matched, [focused_id].into_iter().collect());
    }

    #[test]
    fn explicit_target_document_ids_reject_partial_title_token_overlap() {
        let opening_id = Uuid::now_v7();
        let matched = explicit_target_document_ids_from_values(
            "How should operators register opening time at the store?",
            [(opening_id, "Opening time registration")],
        );
        assert!(matched.is_empty());
    }

    #[test]
    fn explicit_target_document_ids_keep_ambiguous_exact_title_matches_tied() {
        let return_container_id = Uuid::now_v7();
        let return_product_id = Uuid::now_v7();
        let matched = explicit_target_document_ids_from_values(
            "Explain return process",
            [(return_container_id, "Return process"), (return_product_id, "Return process")],
        );
        assert_eq!(matched, [return_container_id, return_product_id].into_iter().collect());
    }

    #[test]
    fn explicit_target_document_ids_reject_one_token_generic_overlap() {
        let policy_id = Uuid::now_v7();
        let matched = explicit_target_document_ids_from_values(
            "What status should I use?",
            [(policy_id, "Status Policy")],
        );
        assert!(matched.is_empty());
    }

    #[test]
    fn explicit_target_document_ids_measure_unicode_title_length_in_chars() {
        let short_id = Uuid::now_v7();
        let matched = explicit_target_document_ids_from_values(
            "How do I configure αλφα checkout?",
            [(short_id, "αλφ")],
        );
        assert!(matched.is_empty());
    }

    #[test]
    fn explicit_target_document_ids_keep_exact_short_unicode_title_match() {
        let short_id = Uuid::now_v7();
        let matched = explicit_target_document_ids_from_values("αλφ", [(short_id, "αλφ")]);
        assert_eq!(matched, [short_id].into_iter().collect());
    }

    #[test]
    fn explicit_target_document_ids_require_token_boundaries() {
        let short_id = Uuid::now_v7();
        let matched =
            explicit_target_document_ids_from_values("Explain storage setup.", [(short_id, "RAG")]);
        assert!(matched.is_empty());
    }

    #[test]
    fn resolve_scoped_target_document_ids_keeps_broad_configure_query_unscoped() {
        let generic_document_id = Uuid::now_v7();
        let index = scoped_document_index([(
            generic_document_id,
            "configure.md",
            Some("Configure"),
            "configure.md",
        )]);
        let mut ir = scoped_query_ir(QueryScope::SingleDocument, None, &["checkout setup"]);
        ir.act = QueryAct::ConfigureHow;
        ir.target_types = vec!["procedure".to_string()];

        let target_ids = resolve_scoped_target_document_ids(
            "How do I configure checkout setup?",
            Some(&ir),
            &index,
        );

        assert!(target_ids.is_empty());
    }

    #[test]
    fn extracts_explicit_document_reference_literals_from_question() {
        assert_eq!(
            explicit_document_reference_literals(
                "What is Shelby Terrell's job title in people-100.csv and what is in sample-heavy-1.xls?"
            ),
            vec!["people-100.csv".to_string(), "sample-heavy-1.xls".to_string()]
        );
    }

    #[test]
    fn explicit_document_reference_literals_ignore_scope_history() {
        assert_eq!(
            explicit_document_reference_literals(&effective_query_text(
                "Prior assistant cited report.pdf.",
                "open the other one"
            )),
            Vec::<String>::new()
        );
    }

    #[test]
    fn explicit_document_reference_literal_matches_path_basename() {
        assert!(explicit_document_reference_literal_is_present(
            "people-100.csv",
            ["exports/archive/people-100.csv"]
        ));
    }
}
