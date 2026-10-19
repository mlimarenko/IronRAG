use super::{
    BTreeMap, BTreeSet, HashSet, QueryAct, QueryIR, QueryScope, QueryTargetKind, QuestionIntent,
    RuntimeChunkScoreKind, RuntimeMatchedChunk, Uuid, chunk_is_setup_focus_command_path_anchor,
    classify_query_ir_intents, comparison_target_document_ids_from_values, has_question_intent,
    normalized_alnum_tokens, soft_token_match, structural_literal_inventory_score,
};

#[derive(Default)]
pub(super) struct SetupVariantAnchorFamilyModel {
    pub(super) label_count: usize,
    pub(super) token_frequency: BTreeMap<String, usize>,
}

pub(super) fn setup_variant_anchor_family_model(
    labels: &[&str],
    query_terms: &BTreeSet<String>,
) -> SetupVariantAnchorFamilyModel {
    let mut token_frequency = BTreeMap::<String, usize>::new();
    let mut label_count = 0usize;
    for label in labels {
        let tokens = setup_variant_anchor_family_tokens(label, query_terms);
        if tokens.is_empty() {
            continue;
        }
        label_count = label_count.saturating_add(1);
        for token in tokens {
            *token_frequency.entry(token).or_default() += 1;
        }
    }
    SetupVariantAnchorFamilyModel { label_count, token_frequency }
}

pub(super) fn setup_variant_anchor_family(
    label: &str,
    query_terms: &BTreeSet<String>,
    family_model: &SetupVariantAnchorFamilyModel,
) -> String {
    let tokens = setup_variant_anchor_family_tokens(label, query_terms);
    if let Some(token) = tokens.iter().find(|token| {
        family_model
            .token_frequency
            .get(*token)
            .is_some_and(|frequency| *frequency < family_model.label_count)
    }) {
        return token.clone();
    }
    if !tokens.is_empty() {
        return tokens.join(" ");
    }
    label.to_lowercase()
}

pub(super) fn setup_variant_anchor_family_tokens(
    label: &str,
    query_terms: &BTreeSet<String>,
) -> Vec<String> {
    normalized_alnum_tokens(label, 3)
        .into_iter()
        .filter(|token| !query_terms.iter().any(|query_token| soft_token_match(query_token, token)))
        .collect()
}

pub(super) fn ensure_setup_variant_anchors_retained(
    selected: &mut Vec<(usize, RuntimeMatchedChunk)>,
    anchors: Vec<(usize, RuntimeMatchedChunk)>,
    top_k: usize,
) {
    if top_k == 0 || anchors.is_empty() {
        return;
    }
    for anchor in anchors {
        if selected.iter().any(|(_, chunk)| chunk.chunk_id == anchor.1.chunk_id) {
            continue;
        }
        if selected.len() < top_k {
            selected.push(anchor);
            continue;
        }
        let evict_position = selected
            .iter()
            .rposition(|(_, chunk)| {
                !chunk_is_setup_focus_command_path_anchor(chunk)
                    && chunk.score_kind != RuntimeChunkScoreKind::FocusedDocument
            })
            .or_else(|| {
                selected
                    .iter()
                    .rposition(|(_, chunk)| !chunk_is_setup_focus_command_path_anchor(chunk))
            });
        if let Some(position) = evict_position {
            let anchor_chunk_id = anchor.1.chunk_id;
            selected[position] = anchor;
            tracing::info!(
                stage = "retrieval.setup_variant_anchor_reserved",
                chunk_id = %anchor_chunk_id,
                "setup variant anchor reserved past score-ordered truncation"
            );
        }
    }
}

pub(super) fn ensure_exact_literal_context_anchors_retained(
    selected: &mut Vec<(usize, RuntimeMatchedChunk)>,
    anchors: Vec<(usize, RuntimeMatchedChunk)>,
    top_k: usize,
) {
    ensure_anchor_chunks_retained(
        selected,
        anchors,
        top_k,
        |selected, protected_anchor_ids, _| {
            selected
                .iter()
                .rposition(|(_, chunk)| {
                    !protected_anchor_ids.contains(&chunk.chunk_id)
                        && chunk.score_kind != RuntimeChunkScoreKind::LatestVersion
                        && structural_literal_inventory_score(chunk) == 0
                })
                .or_else(|| {
                    selected.iter().rposition(|(_, chunk)| {
                        !protected_anchor_ids.contains(&chunk.chunk_id)
                            && !matches!(
                                chunk.score_kind,
                                RuntimeChunkScoreKind::LatestVersion
                                    | RuntimeChunkScoreKind::FocusedDocument
                                    | RuntimeChunkScoreKind::SourceContext
                            )
                    })
                })
                .or_else(|| {
                    selected.iter().rposition(|(_, chunk)| {
                        !protected_anchor_ids.contains(&chunk.chunk_id)
                            && chunk.score_kind != RuntimeChunkScoreKind::LatestVersion
                    })
                })
        },
        |anchor_chunk_id| {
            tracing::info!(
                stage = "retrieval.exact_literal_context_anchor_reserved",
                chunk_id = %anchor_chunk_id,
                "focus-aligned exact literal context anchor reserved past score-ordered truncation"
            );
        },
    );
}

pub(super) fn ensure_content_anchor_chunks_retained(
    selected: &mut Vec<(usize, RuntimeMatchedChunk)>,
    anchors: Vec<(usize, RuntimeMatchedChunk)>,
    top_k: usize,
) {
    ensure_anchor_chunks_retained(
        selected,
        anchors,
        top_k,
        |selected, protected_anchor_ids, _| {
            selected
                .iter()
                .rposition(|(_, chunk)| {
                    !protected_anchor_ids.contains(&chunk.chunk_id)
                        && !matches!(
                            chunk.score_kind,
                            RuntimeChunkScoreKind::ContentAnchor
                                | RuntimeChunkScoreKind::LatestVersion
                                | RuntimeChunkScoreKind::FocusedDocument
                                | RuntimeChunkScoreKind::SourceContext
                        )
                })
                .or_else(|| {
                    selected.iter().rposition(|(_, chunk)| {
                        !protected_anchor_ids.contains(&chunk.chunk_id)
                            && chunk.score_kind != RuntimeChunkScoreKind::ContentAnchor
                    })
                })
        },
        |anchor_chunk_id| {
            tracing::info!(
                stage = "retrieval.content_anchor_reserved",
                chunk_id = %anchor_chunk_id,
                "content-anchor evidence reserved past score-ordered truncation"
            );
        },
    );
}

pub(super) fn ensure_anchor_chunks_retained<E, L>(
    selected: &mut Vec<(usize, RuntimeMatchedChunk)>,
    anchors: Vec<(usize, RuntimeMatchedChunk)>,
    top_k: usize,
    mut evict_position: E,
    mut log_reservation: L,
) where
    E: FnMut(
        &[(usize, RuntimeMatchedChunk)],
        &BTreeSet<Uuid>,
        &(usize, RuntimeMatchedChunk),
    ) -> Option<usize>,
    L: FnMut(Uuid),
{
    if top_k == 0 || anchors.is_empty() {
        return;
    }
    let protected_anchor_ids =
        anchors.iter().map(|(_, chunk)| chunk.chunk_id).collect::<BTreeSet<_>>();
    for anchor in anchors {
        if selected.iter().any(|(_, chunk)| chunk.chunk_id == anchor.1.chunk_id) {
            continue;
        }
        if selected.len() < top_k {
            selected.push(anchor);
            continue;
        }
        if let Some(position) = evict_position(selected, &protected_anchor_ids, &anchor) {
            let anchor_chunk_id = anchor.1.chunk_id;
            selected[position] = anchor;
            log_reservation(anchor_chunk_id);
        }
    }
}

/// Guarantee the reserved setup anchor is present in the truncated selection.
/// If the score-ordered truncation already kept it, this is a no-op; otherwise
/// the anchor displaces the lowest-priority non-anchor chunk so the context
/// budget is preserved.
pub(super) fn ensure_setup_focus_anchor_retained(
    selected: &mut Vec<(usize, RuntimeMatchedChunk)>,
    anchor: (usize, RuntimeMatchedChunk),
    top_k: usize,
) {
    if top_k == 0 || selected.iter().any(|(_, chunk)| chunk.chunk_id == anchor.1.chunk_id) {
        return;
    }
    let evict_position =
        selected.iter().rposition(|(_, chunk)| !chunk_is_setup_focus_command_path_anchor(chunk));
    let anchor_chunk_id = anchor.1.chunk_id;
    match evict_position {
        Some(position) if selected.len() >= top_k => {
            selected[position] = anchor;
        }
        _ => selected.push(anchor),
    }
    tracing::info!(
        stage = "retrieval.setup_focus_anchor_reserved",
        chunk_id = %anchor_chunk_id,
        "focused-document setup anchor reserved past score-ordered truncation"
    );
}

pub(super) fn ensure_versioned_update_procedure_runbook_anchor_retained(
    selected: &mut Vec<(usize, RuntimeMatchedChunk)>,
    anchor: (usize, RuntimeMatchedChunk),
    top_k: usize,
) {
    if top_k == 0 || selected.iter().any(|(_, chunk)| chunk.chunk_id == anchor.1.chunk_id) {
        return;
    }
    let evict_position = selected
        .iter()
        .rposition(|(_, chunk)| chunk.score_kind != RuntimeChunkScoreKind::LatestVersion);
    let anchor_chunk_id = anchor.1.chunk_id;
    match evict_position {
        Some(position) if selected.len() >= top_k => {
            selected[position] = anchor;
        }
        _ => selected.push(anchor),
    }
    tracing::info!(
        stage = "retrieval.versioned_update_procedure_runbook_anchor_reserved",
        chunk_id = %anchor_chunk_id,
        "versioned update runbook anchor reserved past score-ordered truncation"
    );
}

pub(super) fn ensure_versioned_update_procedure_runbook_chunk_retained(
    selected: &mut Vec<RuntimeMatchedChunk>,
    anchor: RuntimeMatchedChunk,
    top_k: usize,
) {
    if top_k == 0 || selected.iter().any(|chunk| chunk.chunk_id == anchor.chunk_id) {
        return;
    }
    let evict_position =
        selected.iter().rposition(|chunk| chunk.score_kind != RuntimeChunkScoreKind::LatestVersion);
    let anchor_chunk_id = anchor.chunk_id;
    match evict_position {
        Some(position) if selected.len() >= top_k => {
            selected[position] = anchor;
        }
        _ => selected.push(anchor),
    }
    tracing::info!(
        stage = "retrieval.versioned_update_procedure_runbook_anchor_reserved",
        chunk_id = %anchor_chunk_id,
        "versioned update runbook anchor reserved during procedure merge truncation"
    );
}

pub(super) fn source_context_reservation_count(
    chunks: &[(usize, RuntimeMatchedChunk)],
    top_k: usize,
    query_ir: &QueryIR,
) -> Option<usize> {
    let intents = classify_query_ir_intents(query_ir);
    let requests_error_code_context = has_question_intent(&intents, QuestionIntent::ErrorCode);
    let requests_transport_context = has_question_intent(&intents, QuestionIntent::Port)
        || has_question_intent(&intents, QuestionIntent::Protocol)
        || query_ir.targets(QueryTargetKind::Connection);
    let requests_configuration_context = has_question_intent(&intents, QuestionIntent::ConfigKey)
        || has_question_intent(&intents, QuestionIntent::Parameter)
        || query_ir.targets_any(&[
            QueryTargetKind::ConfigurationFile,
            QueryTargetKind::ConfigKey,
            QueryTargetKind::Parameter,
        ]);
    let requests_table_context = query_ir_requests_table_source_context_reservation(query_ir);
    let requests_structured_fallback_context =
        query_ir_requests_structured_source_context_reservation(query_ir);
    let reserves_source_context = query_ir.is_exact_literal_technical()
        || requests_error_code_context
        || requests_transport_context
        || requests_configuration_context
        || requests_table_context
        || requests_structured_fallback_context;
    if top_k < 2 || !reserves_source_context {
        return None;
    }
    let source_context_count = chunks
        .iter()
        .filter(|(_, chunk)| chunk.score_kind == RuntimeChunkScoreKind::SourceContext)
        .count();
    if source_context_count == 0 {
        return None;
    }
    let max_reserved = if requests_error_code_context {
        2
    } else if requests_transport_context {
        8
    } else if requests_table_context || requests_structured_fallback_context {
        top_k.saturating_sub(4).clamp(8, 28)
    } else if requests_configuration_context {
        top_k.saturating_sub(2).clamp(4, 12)
    } else {
        4
    };
    Some(source_context_count.min(top_k.saturating_sub(1)).min(max_reserved))
}

pub(super) fn query_ir_requests_table_source_context_reservation(query_ir: &QueryIR) -> bool {
    if query_ir.source_slice.is_some() || !matches!(query_ir.scope, QueryScope::SingleDocument) {
        return false;
    }
    query_ir.targets(QueryTargetKind::TableRow) && query_ir.targets(QueryTargetKind::TableSummary)
}

pub(super) fn query_ir_requests_structured_source_context_reservation(query_ir: &QueryIR) -> bool {
    query_ir.confidence <= 0.3
        && matches!(query_ir.scope, QueryScope::SingleDocument)
        && matches!(query_ir.act, QueryAct::Describe | QueryAct::ConfigureHow)
        && query_ir.source_slice.is_none()
        && query_ir.document_focus.is_none()
        && query_ir.target_types.is_empty()
        && query_ir.literal_constraints.is_empty()
}

pub(super) fn truncate_chunks_with_source_context_reservation(
    chunks: &[(usize, RuntimeMatchedChunk)],
    top_k: usize,
    reserved_count: usize,
) -> Vec<(usize, RuntimeMatchedChunk)> {
    if top_k == 0 {
        return Vec::new();
    }
    let mut selected = Vec::with_capacity(top_k);
    let mut selected_indices = HashSet::new();
    let mut selected_source_context_count = 0usize;

    for (index, chunk) in
        chunks.iter().filter(|(_, chunk)| chunk.score_kind == RuntimeChunkScoreKind::SourceContext)
    {
        if selected.len() >= reserved_count {
            break;
        }
        selected_indices.insert(*index);
        selected.push((*index, chunk.clone()));
        selected_source_context_count = selected_source_context_count.saturating_add(1);
    }

    for (index, chunk) in chunks {
        if selected.len() >= top_k {
            break;
        }
        if chunk.score_kind == RuntimeChunkScoreKind::SourceContext
            && selected_source_context_count >= reserved_count
        {
            continue;
        }
        if selected_indices.insert(*index) {
            if chunk.score_kind == RuntimeChunkScoreKind::SourceContext {
                selected_source_context_count = selected_source_context_count.saturating_add(1);
            }
            selected.push((*index, chunk.clone()));
        }
    }
    selected
}

pub(super) fn truncate_chunks_for_multi_document_scope(
    chunks: &[(usize, RuntimeMatchedChunk)],
    top_k: usize,
    query_ir: &QueryIR,
) -> Vec<(usize, RuntimeMatchedChunk)> {
    if top_k == 0 || chunks.is_empty() {
        return Vec::new();
    }

    let mut selected = Vec::with_capacity(top_k);
    let mut selected_indices = HashSet::new();
    let relevant_documents = multi_document_relevant_document_ids(chunks, query_ir, top_k);

    for document_id in relevant_documents {
        let next_chunk = chunks
            .iter()
            .find(|(_, chunk)| chunk.document_id == document_id)
            .map(|(index, chunk)| (*index, chunk.clone()));
        if let Some((index, chunk)) = next_chunk {
            selected_indices.insert(index);
            selected.push((index, chunk));
        }
        if selected.len() >= top_k {
            return selected;
        }
    }

    for (index, chunk) in chunks {
        if selected.len() >= top_k {
            break;
        }
        if selected_indices.insert(*index) {
            selected.push((*index, chunk.clone()));
        }
    }
    selected
}

pub(super) fn append_distinct_document_ids(
    chunks: &[(usize, RuntimeMatchedChunk)],
    documents: &mut Vec<Uuid>,
    limit: usize,
) {
    let mut selected = documents.iter().copied().collect::<HashSet<_>>();
    for document_id in chunks.iter().map(|(_, chunk)| chunk.document_id) {
        if selected.insert(document_id) {
            documents.push(document_id);
        }
        if documents.len() >= limit {
            break;
        }
    }
}

pub(super) fn multi_document_relevant_document_ids(
    chunks: &[(usize, RuntimeMatchedChunk)],
    query_ir: &QueryIR,
    top_k: usize,
) -> Vec<Uuid> {
    let relevance_terms = query_multi_document_relevance_terms(query_ir);
    let mut relevant_documents = comparison_target_document_ids_from_values(
        query_ir,
        chunks.iter().map(|(_, chunk)| (chunk.document_id, chunk.document_label.as_str())),
    );
    let mut scored_documents = relevant_documents.iter().copied().collect::<HashSet<_>>();
    for (_, chunk) in chunks {
        let document_id = chunk.document_id;
        if scored_documents.insert(document_id)
            && query_chunk_relevance_score(chunk, &relevance_terms) > 0
        {
            relevant_documents.push(document_id);
            if relevant_documents.len() >= top_k {
                return relevant_documents;
            }
        }
    }

    if relevant_documents.len() < 2 && matches!(query_ir.act, QueryAct::Compare) {
        append_distinct_document_ids(chunks, &mut relevant_documents, 2);
    }
    if relevant_documents.is_empty() {
        append_distinct_document_ids(chunks, &mut relevant_documents, top_k);
    }
    relevant_documents.truncate(top_k);
    relevant_documents
}

pub(super) fn query_chunk_relevance_score(chunk: &RuntimeMatchedChunk, terms: &[String]) -> usize {
    if terms.is_empty() {
        return 0;
    }

    let haystack =
        format!("{} {} {}", chunk.document_label, chunk.excerpt, chunk.source_text).to_lowercase();
    terms.iter().filter(|term| haystack.contains(term.as_str())).count()
}

pub(super) fn query_multi_document_relevance_terms(query_ir: &QueryIR) -> Vec<String> {
    let mut terms = BTreeSet::new();
    let push_terms = |value: &str, terms: &mut BTreeSet<String>| {
        for token in normalized_alnum_tokens(value, 3) {
            if !token.is_empty() {
                terms.insert(token);
            }
        }
    };

    for entity in &query_ir.target_entities {
        push_terms(&entity.label, &mut terms);
    }
    if let Some(comparison) = &query_ir.comparison {
        if let Some(value) = &comparison.a {
            push_terms(value, &mut terms);
        }
        if let Some(value) = &comparison.b {
            push_terms(value, &mut terms);
        }
        push_terms(&comparison.dimension, &mut terms);
    }
    if let Some(document_focus) = &query_ir.document_focus {
        push_terms(&document_focus.hint, &mut terms);
    }
    for literal in &query_ir.literal_constraints {
        push_terms(&literal.text, &mut terms);
    }
    terms.into_iter().collect()
}
