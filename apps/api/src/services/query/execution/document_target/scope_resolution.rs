use super::matching::{
    explicit_document_reference_target_ids_from_values,
    explicit_multi_token_target_document_ids_from_values,
    normalized_question_contains_document_candidate, ranked_document_target_candidates,
};
use super::{
    BTreeSet, HashMap, KnowledgeDocumentRow, LiteralKind, QueryAct, QueryIR, QueryScope,
    QueryTargetKind, Uuid, build_related_token_candidates, classify_query_ir_intents,
    common_prefix_char_count, explicit_target_document_ids_from_values,
    literal_text_is_identifier_shaped, near_token_match, near_token_overlap_count,
    normalize_document_target_text, normalized_alnum_tokens, normalized_document_target_candidates,
    query_ir_has_focused_document_answer_intent, query_ir_targets_graph_entities_or_relationships,
    select_related_overlap_tokens_from_candidates, structured_current_question_segment,
};

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

pub(super) fn document_ids_with_focus_value_contained_in_hint(
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

pub(super) fn document_candidate_is_matchable_for_surface(candidate: &str, surface: &str) -> bool {
    document_candidate_meets_minimum_length(candidate) || surface == candidate
}

pub(super) fn document_candidate_length_score(candidate: &str) -> usize {
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
