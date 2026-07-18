use std::collections::{BTreeSet, HashMap};

use uuid::Uuid;

use crate::domains::query_ir::{
    LiteralKind, QueryAct, QueryIR, QueryScope, QueryTargetKind, literal_text_is_identifier_shaped,
};
use crate::infra::knowledge_rows::KnowledgeDocumentRow;
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
        classify_query_ir_intents, query_ir_has_focused_document_answer_intent,
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

fn explicit_multi_token_target_document_ids_from_values<'a, I>(
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
    let targets =
        explicit_target_document_ids_from_values(question, concrete_values.iter().copied());
    if targets.len() != 1 {
        return BTreeSet::new();
    }
    let Some(target_document_id) = targets.iter().next().copied() else {
        return BTreeSet::new();
    };
    let has_multi_token_identity_match = concrete_values
        .iter()
        .filter(|(document_id, _)| *document_id == target_document_id)
        .any(|(_, raw_value)| {
            ranked_document_target_candidates([*raw_value]).into_iter().any(|candidate| {
                document_candidate_match_tokens(&candidate.text).len() >= 2
                    && document_candidate_is_matchable_for_surface(
                        &candidate.text,
                        &normalized_question,
                    )
                    && normalized_question_contains_document_candidate(
                        &normalized_question,
                        candidate.text.as_str(),
                        "",
                    )
            })
        });

    if has_multi_token_identity_match { targets } else { BTreeSet::new() }
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

fn explicit_document_format_markers(
    normalized_question: &str,
    values: &[(Uuid, &str)],
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

fn explicit_document_format_matches(
    normalized_question: &str,
    values: &[(Uuid, &str)],
    extensions: &[&'static str],
) -> BTreeSet<Uuid> {
    if extensions.is_empty() {
        return BTreeSet::new();
    }

    let extension_set = extensions.iter().copied().collect::<BTreeSet<_>>();
    values
        .iter()
        .filter_map(|(document_id, raw_value)| {
            document_value_matches_format(normalized_question, raw_value, &extension_set)
                .then_some(*document_id)
        })
        .collect()
}

fn document_value_matches_format(
    normalized_question: &str,
    raw_value: &str,
    extensions: &BTreeSet<&str>,
) -> bool {
    normalized_explicit_document_reference_candidates(raw_value).into_iter().any(|candidate| {
        let Some((stem, extension)) = candidate.rsplit_once('.') else {
            return false;
        };
        extensions.contains(extension)
            && ranked_document_target_candidates([stem, &candidate]).into_iter().any(|candidate| {
                candidate.text.len() >= 4
                    && normalized_question_contains_document_candidate(
                        normalized_question,
                        candidate.text.as_str(),
                        extension,
                    )
            })
    })
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
    for raw in values {
        append_ranked_document_target_candidates(raw, &mut seen, &mut candidates);
    }
    candidates
}

fn append_ranked_document_target_candidates(
    raw: &str,
    seen: &mut BTreeSet<String>,
    candidates: &mut Vec<DocumentTargetCandidate>,
) {
    let normalized = normalize_document_target_text(raw);
    if normalized.is_empty() {
        return;
    }
    push_document_target_candidate(&normalized, 4, seen, candidates);
    if let Some(separator_variant) = separator_normalized_document_target_candidate(&normalized) {
        push_document_target_candidate(&separator_variant, 2, seen, candidates);
    }

    let Some((stem, _)) = normalized.rsplit_once('.') else {
        return;
    };
    let stem = stem.trim();
    if stem.is_empty() {
        return;
    }
    push_document_target_candidate(stem, 3, seen, candidates);
    if let Some(separator_variant) = separator_normalized_document_target_candidate(stem) {
        push_document_target_candidate(&separator_variant, 1, seen, candidates);
    }
}

fn push_document_target_candidate(
    value: &str,
    priority: usize,
    seen: &mut BTreeSet<String>,
    candidates: &mut Vec<DocumentTargetCandidate>,
) {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() || !seen.insert(normalized.clone()) {
        return;
    }
    candidates.push(DocumentTargetCandidate { text: normalized, priority });
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
    if let Some(targets) = explicit_scoped_target_document_ids(question, &document_values) {
        return targets;
    }

    let Some(ir) = query_ir.filter(|ir| query_ir_allows_document_focus_scope(ir)) else {
        return BTreeSet::new();
    };
    resolve_query_ir_target_document_ids(question, ir, &document_values)
}

fn explicit_scoped_target_document_ids(
    question: &str,
    document_values: &[(Uuid, String)],
) -> Option<BTreeSet<Uuid>> {
    if let Some(current_question) = structured_current_question_segment(question) {
        let targets = explicit_document_reference_target_ids_from_values(
            current_question,
            document_value_refs(document_values),
        );
        if targets.len() == 1 {
            return Some(targets);
        }
    }
    let targets = explicit_document_reference_target_ids_from_values(
        question,
        document_value_refs(document_values),
    );
    (!targets.is_empty()).then_some(targets)
}

fn resolve_query_ir_target_document_ids(
    question: &str,
    ir: &QueryIR,
    document_values: &[(Uuid, String)],
) -> BTreeSet<Uuid> {
    if !ir.is_follow_up() {
        let title_targets = explicit_multi_token_target_document_ids_from_values(
            question,
            document_value_refs(document_values),
        );
        if title_targets.len() == 1 {
            return title_targets;
        }
    }
    if query_ir_has_explicit_document_focus_target(ir)
        && let Some(targets) = explicit_focus_target_document_ids(question, ir, document_values)
    {
        return targets;
    }
    if let Some(targets) = document_focus_target_ids(ir, document_values) {
        return targets;
    }
    target_entity_document_ids(ir, document_values)
}

fn explicit_focus_target_document_ids(
    question: &str,
    ir: &QueryIR,
    document_values: &[(Uuid, String)],
) -> Option<BTreeSet<Uuid>> {
    if ir.is_follow_up() {
        return None;
    }
    if let Some(current_question) = structured_current_question_segment(question) {
        let targets = document_ids_matching_focus_values(&[current_question], document_values);
        if targets.len() == 1 {
            return Some(targets);
        }
    }
    let targets =
        explicit_target_document_ids_from_values(question, document_value_refs(document_values));
    (!targets.is_empty()).then_some(targets)
}

fn document_focus_target_ids(
    ir: &QueryIR,
    document_values: &[(Uuid, String)],
) -> Option<BTreeSet<Uuid>> {
    let hint = ir.document_focus.as_ref()?.hint.trim();
    if hint.is_empty() {
        return None;
    }
    let targets = document_ids_matching_focus_hint(hint, document_values);
    if targets.len() == 1 {
        return Some(targets);
    }
    let entity_hints = ir
        .target_entities
        .iter()
        .filter_map(|entity| {
            let label = entity.label.trim();
            (!label.is_empty()).then_some(label)
        })
        .collect::<Vec<_>>();
    let targets = refine_document_focus_targets(&targets, &entity_hints, document_values);
    Some(single_target_or_empty(targets))
}

fn target_entity_document_ids(ir: &QueryIR, document_values: &[(Uuid, String)]) -> BTreeSet<Uuid> {
    if !ir.targets(QueryTargetKind::Document) {
        return BTreeSet::new();
    }
    let focused_targets = ir
        .target_entities
        .iter()
        .filter_map(|entity| {
            let label = entity.label.trim();
            (!label.is_empty()).then_some(label)
        })
        .flat_map(|hint| document_ids_matching_focus_hint(hint, document_values))
        .collect();
    single_target_or_empty(focused_targets)
}

fn document_value_refs(document_values: &[(Uuid, String)]) -> impl Iterator<Item = (Uuid, &str)> {
    document_values.iter().map(|(document_id, value)| (*document_id, value.as_str()))
}

fn single_target_or_empty(targets: BTreeSet<Uuid>) -> BTreeSet<Uuid> {
    if targets.len() == 1 { targets } else { BTreeSet::new() }
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
    query_ir_has_focused_document_answer_intent(ir) || ir.targets(QueryTargetKind::Document)
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
    let normalized = normalized.trim();
    if normalized.is_empty() {
        return document_label.to_string();
    }

    normalized
        .split_whitespace()
        .enumerate()
        .map(|(index, word)| format_document_label_word(word, index == 0))
        .collect::<Vec<_>>()
        .join(" ")
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

fn format_document_label_word(word: &str, is_first: bool) -> String {
    if word.is_empty() {
        return String::new();
    }
    if document_word_has_explicit_source_casing(word) {
        return word.to_string();
    }

    let lowered = word.to_lowercase();
    if !is_first {
        return lowered;
    }
    let mut chars = lowered.chars();
    let Some(first) = chars.next() else {
        return String::new();
    };
    first.to_uppercase().collect::<String>() + chars.as_str()
}

fn document_word_has_explicit_source_casing(word: &str) -> bool {
    let mut cased_characters =
        word.chars().filter(|character| character.is_uppercase() || character.is_lowercase());
    cased_characters.next().is_some() && cased_characters.any(|character| character.is_uppercase())
}

#[cfg(test)]
#[path = "document_target_tests.rs"]
mod tests;
