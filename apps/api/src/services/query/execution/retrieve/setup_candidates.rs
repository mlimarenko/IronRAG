use futures::{StreamExt, TryStreamExt};

use anyhow::Context;

use super::{
    AppState, BTreeMap, BTreeSet, DateTime, EntityRole, HashMap, HashSet, KnowledgeChunkRow,
    KnowledgeDocumentRow, QueryAct, QueryIR, QueryScope, QueryTargetKind,
    RAW_SETUP_FOCUS_STRUCTURAL_DOCUMENT_SCORE_FLOOR, RetrievalLaneContext, RuntimeChunkScoreKind,
    RuntimeMatchedChunk, SETUP_FOCUS_DOCUMENT_CANDIDATE_CAP,
    SETUP_FOCUS_DOCUMENT_FETCH_CONCURRENCY, SETUP_FOCUS_DOCUMENT_SCAN_CHUNKS,
    SETUP_FOCUS_DOCUMENT_SCORE_BASE, SETUP_VARIANT_CHUNK_CAP, SETUP_VARIANT_CHUNKS_PER_DOCUMENT,
    SETUP_VARIANT_DOCUMENT_CANDIDATE_CAP, SETUP_VARIANT_DOCUMENT_CAP,
    SETUP_VARIANT_DOCUMENT_FETCH_CAP, SETUP_VARIANT_DOCUMENT_FETCH_CONCURRENCY, Utc, Uuid,
    adjacent_query_ir_focus_compounds, common_prefix_char_count, current_document_revision_id,
    is_usable_query_ir_focus, literal_kind_has_exact_technical_shape, map_chunk_hit,
    near_token_match, normalize_document_identity_value, normalized_alnum_tokens,
    query_ir_has_setup_focus_identity, query_ir_has_setup_focus_target,
    query_ir_requests_setup_focus_document_candidates,
    query_ir_requests_versioned_update_procedure_context, raw_setup_focus_text_structural_score,
    select_setup_focus_document_chunks, select_setup_focus_document_rows,
    setup_focus_candidate_document_ids, setup_focus_document_candidate_score,
    setup_focus_document_chunk_score, setup_focus_document_identity_values,
    setup_focus_document_is_standalone_image, setup_focus_document_label_identity_score,
    setup_focus_identity_tokens, stream, strip_leading_question_marker,
};

pub(super) async fn load_setup_focus_document_chunks(
    state: &AppState,
    query_ir: Option<&QueryIR>,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    plan_keywords: &[String],
    temporal_start: Option<DateTime<Utc>>,
    temporal_end: Option<DateTime<Utc>>,
) -> anyhow::Result<Vec<RuntimeMatchedChunk>> {
    let Some(query_ir) = query_ir else {
        return Ok(Vec::new());
    };
    if temporal_start.is_some() || temporal_end.is_some() {
        return Ok(Vec::new());
    }
    if !query_ir_requests_setup_focus_document_candidates(query_ir) {
        return Ok(Vec::new());
    }
    let mut candidate_document_ids = setup_focus_candidate_document_ids(
        query_ir,
        document_index,
        SETUP_FOCUS_DOCUMENT_CANDIDATE_CAP,
    );
    let mut seen_candidate_document_ids = HashSet::new();
    candidate_document_ids.retain(|document_id| seen_candidate_document_ids.insert(*document_id));
    if candidate_document_ids.is_empty() {
        return Ok(Vec::new());
    }

    let fetch_inputs: Vec<(usize, Uuid, Uuid)> = candidate_document_ids
        .iter()
        .enumerate()
        .filter_map(|(candidate_rank, document_id)| {
            let document = document_index.get(document_id)?;
            current_document_revision_id(document)
                .map(|revision_id| (candidate_rank, *document_id, revision_id))
        })
        .collect();
    let mut fetched = stream::iter(fetch_inputs.into_iter().map(
        |(candidate_rank, document_id, revision_id)| async move {
            let rows = state
                .document_store
                .list_chunks_by_revision_range(revision_id, 0, SETUP_FOCUS_DOCUMENT_SCAN_CHUNKS)
                .await
                .with_context(|| {
                    format!(
                        "failed to load setup-focused document chunks for document {} revision {}",
                        document_id, revision_id
                    )
                })?;
            Ok::<_, anyhow::Error>((candidate_rank, document_id, rows))
        },
    ))
    .buffer_unordered(SETUP_FOCUS_DOCUMENT_FETCH_CONCURRENCY)
    .try_collect::<Vec<_>>()
    .await?;
    fetched.sort_by_key(|(candidate_rank, _, _)| *candidate_rank);

    let mut document_candidates = Vec::<(usize, Uuid, Vec<RuntimeMatchedChunk>)>::new();
    for (_, document_id, rows) in fetched {
        let Some(document) = document_index.get(&document_id) else {
            continue;
        };
        let selected_rows = select_setup_focus_document_rows(&rows);
        if selected_rows.is_empty() {
            continue;
        }
        let document_score = setup_focus_document_candidate_score(&selected_rows, query_ir)
            .saturating_add(
                setup_focus_document_label_identity_score(document, query_ir).saturating_mul(4096),
            );
        let mut document_chunks = Vec::new();
        for row in selected_rows {
            if let Some(mut chunk) = map_chunk_hit(row, 0.0, document_index, plan_keywords) {
                chunk.score = Some(setup_focus_document_chunk_score(&chunk));
                chunk.score_kind = RuntimeChunkScoreKind::DocumentIdentity;
                document_chunks.push(chunk);
            }
        }
        if !document_chunks.is_empty() {
            document_candidates.push((document_score, document_id, document_chunks));
        }
    }
    let chunks = select_setup_focus_document_chunks(document_candidates);
    if !chunks.is_empty() {
        tracing::info!(
            stage = "retrieval.setup_focus_document",
            setup_focus_chunk_count = chunks.len(),
            "setup-focused document chunks loaded from title-matched documents",
        );
    }
    Ok(chunks)
}

pub(super) fn setup_variant_fetch_inputs(
    candidate_document_ids: &[Uuid],
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
) -> Vec<(usize, Uuid, Uuid)> {
    candidate_document_ids
        .iter()
        .take(SETUP_VARIANT_DOCUMENT_FETCH_CAP)
        .enumerate()
        .filter_map(|(document_rank, document_id)| {
            let document = document_index.get(document_id)?;
            current_document_revision_id(document)
                .map(|revision_id| (document_rank, *document_id, revision_id))
        })
        .collect()
}

pub(super) fn append_setup_variant_chunks(
    selected_rows: Vec<KnowledgeChunkRow>,
    selected_document_count: usize,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    plan_keywords: &[String],
    chunks: &mut Vec<RuntimeMatchedChunk>,
) {
    for (chunk_rank, row) in
        selected_rows.into_iter().take(SETUP_VARIANT_CHUNKS_PER_DOCUMENT).enumerate()
    {
        let score = setup_variant_document_chunk_score(selected_document_count - 1, chunk_rank);
        if let Some(mut chunk) = map_chunk_hit(row, score, document_index, plan_keywords) {
            chunk.score = Some(score);
            chunk.score_kind = RuntimeChunkScoreKind::DocumentIdentity;
            chunks.push(chunk);
        }
        if chunks.len() >= SETUP_VARIANT_CHUNK_CAP {
            break;
        }
    }
}

pub(super) async fn load_setup_variant_document_chunks(
    context: RetrievalLaneContext<'_>,
    query_ir: Option<&QueryIR>,
) -> anyhow::Result<Vec<RuntimeMatchedChunk>> {
    let RetrievalLaneContext {
        state,
        question,
        document_index,
        plan_keywords,
        temporal_start,
        temporal_end,
    } = context;
    if temporal_start.is_some()
        || temporal_end.is_some()
        || !question_requests_setup_variant_evidence(question, query_ir)
    {
        return Ok(Vec::new());
    }
    let query_terms = setup_variant_query_terms(question, query_ir);
    let candidate_document_ids = setup_variant_candidate_document_ids(
        question,
        query_ir,
        document_index,
        SETUP_VARIANT_DOCUMENT_CANDIDATE_CAP,
    );
    if candidate_document_ids.is_empty() {
        return Ok(Vec::new());
    }
    let family_model =
        setup_variant_family_model(&candidate_document_ids, document_index, &query_terms);

    let fetch_inputs = setup_variant_fetch_inputs(&candidate_document_ids, document_index);
    let fetched_results = stream::iter(fetch_inputs.into_iter().map(
        |(document_rank, document_id, revision_id)| async move {
            let rows = state
                .document_store
                .list_chunks_by_revision_range(revision_id, 0, SETUP_FOCUS_DOCUMENT_SCAN_CHUNKS)
                .await
                .with_context(|| {
                    format!(
                        "failed to load setup-variant chunks for document {} revision {}",
                        document_id, revision_id
                    )
                })?;
            Ok::<_, anyhow::Error>((document_rank, document_id, rows))
        },
    ))
    .buffer_unordered(SETUP_VARIANT_DOCUMENT_FETCH_CONCURRENCY)
    .collect::<Vec<_>>()
    .await;
    let mut fetched = fetched_results.into_iter().collect::<anyhow::Result<Vec<_>>>()?;
    fetched.sort_by_key(|(document_rank, _, _)| *document_rank);

    let mut selected_document_count = 0usize;
    let mut selected_families = BTreeSet::<String>::new();
    let mut chunks = Vec::new();
    for (_, document_id, rows) in fetched {
        let Some(document) = document_index.get(&document_id) else {
            continue;
        };
        let selected_rows = select_setup_focus_document_rows(&rows);
        if selected_rows.is_empty() {
            continue;
        }
        let structural_score = selected_rows
            .iter()
            .map(|row| raw_setup_focus_text_structural_score(&row.content_text))
            .sum::<usize>();
        if structural_score < RAW_SETUP_FOCUS_STRUCTURAL_DOCUMENT_SCORE_FLOOR {
            continue;
        }
        let family = setup_variant_document_family(document, &query_terms, &family_model);
        if !selected_families.insert(family) {
            continue;
        }
        selected_document_count = selected_document_count.saturating_add(1);
        append_setup_variant_chunks(
            selected_rows,
            selected_document_count,
            document_index,
            plan_keywords,
            &mut chunks,
        );
        if selected_document_count >= SETUP_VARIANT_DOCUMENT_CAP
            || chunks.len() >= SETUP_VARIANT_CHUNK_CAP
        {
            break;
        }
    }
    if !chunks.is_empty() {
        tracing::info!(
            stage = "retrieval.setup_variant",
            setup_variant_document_count = selected_document_count,
            setup_variant_chunk_count = chunks.len(),
            "setup variant evidence anchored into answer context"
        );
    }
    Ok(chunks)
}

pub(super) fn setup_variant_document_chunk_score(document_rank: usize, chunk_rank: usize) -> f32 {
    SETUP_FOCUS_DOCUMENT_SCORE_BASE - document_rank as f32 * 10.0 - chunk_rank as f32
}

pub(super) fn question_requests_setup_variant_evidence(
    question: &str,
    query_ir: Option<&QueryIR>,
) -> bool {
    let Some(query_ir) = query_ir else {
        return false;
    };
    if !matches!(query_ir.act, QueryAct::ConfigureHow | QueryAct::Describe)
        || query_ir.source_slice.is_some()
        || query_ir_has_exact_non_focus_literal_constraint(query_ir)
        || query_ir_requests_versioned_update_procedure_context(question, query_ir)
        || query_ir_has_specific_adjacent_entity_identity(query_ir)
    {
        return false;
    }
    query_ir_has_setup_focus_target(query_ir) && query_ir_has_setup_focus_identity(query_ir)
}

pub(super) fn query_ir_has_exact_non_focus_literal_constraint(query_ir: &QueryIR) -> bool {
    query_ir.literal_constraints.iter().any(|literal| {
        literal_kind_has_exact_technical_shape(literal.kind, &literal.text)
            && !query_ir_literal_matches_named_focus(query_ir, &literal.text)
    })
}

pub(super) fn query_ir_literal_matches_named_focus(query_ir: &QueryIR, literal: &str) -> bool {
    let literal_key = normalize_document_identity_value(literal);
    if literal_key.is_empty() {
        return false;
    }
    if query_ir
        .document_focus
        .as_ref()
        .is_some_and(|focus| normalize_document_identity_value(&focus.hint) == literal_key)
    {
        return true;
    }
    query_ir
        .target_entities
        .iter()
        .any(|entity| normalize_document_identity_value(&entity.label) == literal_key)
}

pub(super) fn query_ir_has_specific_adjacent_entity_identity(query_ir: &QueryIR) -> bool {
    if !matches!(query_ir.scope, QueryScope::SingleDocument) {
        return false;
    }
    if query_ir.comparison.is_some()
        || !query_ir.temporal_constraints.is_empty()
        || !query_ir.conversation_refs.is_empty()
        || !query_ir.literal_constraints.is_empty()
    {
        return false;
    }
    if !query_ir.targets_any(&[
        QueryTargetKind::Artifact,
        QueryTargetKind::Document,
        QueryTargetKind::Entity,
    ]) {
        return false;
    }
    let entity_values = query_ir
        .target_entities
        .iter()
        .filter(|entity| matches!(entity.role, EntityRole::Subject))
        .filter_map(|entity| {
            let normalized = entity.label.split_whitespace().collect::<Vec<_>>().join(" ");
            is_usable_query_ir_focus(&normalized).then_some(normalized)
        })
        .collect::<Vec<_>>();
    if entity_values.len() < 2 {
        return false;
    }
    adjacent_query_ir_focus_compounds(&entity_values)
        .into_iter()
        .any(|compound| setup_focus_identity_tokens(&compound).len() >= 2)
}

pub(super) fn setup_variant_candidate_document_ids(
    question: &str,
    query_ir: Option<&QueryIR>,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    limit: usize,
) -> Vec<Uuid> {
    let query_terms = setup_variant_query_terms(question, query_ir);
    if query_terms.is_empty() || limit == 0 {
        return Vec::new();
    }
    let subject_terms = setup_variant_subject_terms(query_ir);
    let identity_document_count = document_index
        .values()
        .filter(|document| document.document_role == crate::domains::content::DOCUMENT_ROLE_PRIMARY)
        .filter(|document| !setup_focus_document_is_standalone_image(document))
        .count();
    let query_term_frequencies =
        setup_variant_query_term_document_frequency(&query_terms, document_index);
    let mut candidates = document_index
        .values()
        .filter(|document| document.document_role == crate::domains::content::DOCUMENT_ROLE_PRIMARY)
        .filter(|document| !setup_focus_document_is_standalone_image(document))
        .filter_map(|document| {
            let values = setup_focus_document_identity_values(document);
            let best_score = values
                .iter()
                .map(|value| {
                    let value_tokens = normalized_alnum_tokens(value, 3);
                    let subject_overlap = soft_token_overlap_count(&subject_terms, &value_tokens);
                    let overlap = soft_token_overlap_count(&query_terms, &value_tokens);
                    if overlap == 0 {
                        return 0usize;
                    }
                    let weighted_overlap = setup_variant_weighted_query_overlap_score(
                        &query_terms,
                        &value_tokens,
                        &query_term_frequencies,
                        identity_document_count,
                    );
                    subject_overlap
                        .saturating_mul(384)
                        .saturating_add(weighted_overlap)
                        .saturating_add(overlap.saturating_mul(16))
                        .saturating_add(value_tokens.len().min(24))
                })
                .max()
                .unwrap_or(0);
            (best_score > 0).then_some((
                best_score,
                document.title.clone().unwrap_or_else(|| document.external_key.clone()),
                document.document_id,
            ))
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        right.0.cmp(&left.0).then_with(|| left.1.cmp(&right.1)).then_with(|| left.2.cmp(&right.2))
    });
    candidates.into_iter().take(limit).map(|(_, _, document_id)| document_id).collect()
}

pub(super) fn setup_variant_query_term_document_frequency(
    query_terms: &BTreeSet<String>,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
) -> BTreeMap<String, usize> {
    let mut frequencies = BTreeMap::<String, usize>::new();
    if query_terms.is_empty() {
        return frequencies;
    }
    for document in document_index.values().filter(|document| {
        document.document_role == crate::domains::content::DOCUMENT_ROLE_PRIMARY
            && !setup_focus_document_is_standalone_image(document)
    }) {
        let matched = setup_focus_document_identity_values(document)
            .into_iter()
            .flat_map(|value| normalized_alnum_tokens(&value, 3))
            .flat_map(|value_token| {
                query_terms
                    .iter()
                    .filter(move |query_term| soft_token_match(query_term, &value_token))
                    .cloned()
            })
            .collect::<BTreeSet<_>>();
        for token in matched {
            *frequencies.entry(token).or_default() += 1;
        }
    }
    frequencies
}

pub(super) fn setup_variant_weighted_query_overlap_score(
    query_terms: &BTreeSet<String>,
    value_tokens: &BTreeSet<String>,
    frequencies: &BTreeMap<String, usize>,
    document_count: usize,
) -> usize {
    query_terms
        .iter()
        .filter(|query_term| {
            value_tokens.iter().any(|value_token| soft_token_match(query_term, value_token))
        })
        .map(|query_term| {
            let frequency = frequencies.get(query_term).copied().unwrap_or(document_count).max(1);
            let rarity = document_count.saturating_div(frequency).clamp(1, 64);
            rarity
                .saturating_mul(48)
                .saturating_add(usize::from(query_term.chars().count() <= 4).saturating_mul(48))
        })
        .sum()
}

pub(super) fn setup_variant_subject_terms(query_ir: Option<&QueryIR>) -> BTreeSet<String> {
    let mut terms = BTreeSet::<String>::new();
    if let Some(query_ir) = query_ir {
        for entity in &query_ir.target_entities {
            terms.extend(normalized_alnum_tokens(&entity.label, 3));
        }
        if let Some(document_focus) = query_ir.document_focus.as_ref() {
            terms.extend(normalized_alnum_tokens(&document_focus.hint, 3));
        }
    }
    terms
}

#[derive(Default)]
pub(super) struct SetupVariantFamilyModel {
    pub(super) document_count: usize,
    pub(super) token_document_frequency: BTreeMap<String, usize>,
}

pub(super) fn setup_variant_family_model(
    candidate_document_ids: &[Uuid],
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    query_terms: &BTreeSet<String>,
) -> SetupVariantFamilyModel {
    let mut token_document_frequency = BTreeMap::<String, usize>::new();
    let mut document_count = 0usize;
    for document_id in candidate_document_ids {
        let Some(document) = document_index.get(document_id) else {
            continue;
        };
        let tokens = setup_variant_document_family_tokens(document, query_terms);
        if tokens.is_empty() {
            continue;
        }
        document_count = document_count.saturating_add(1);
        for token in tokens {
            *token_document_frequency.entry(token).or_default() += 1;
        }
    }
    SetupVariantFamilyModel { document_count, token_document_frequency }
}

pub(super) fn setup_variant_document_family(
    document: &KnowledgeDocumentRow,
    query_terms: &BTreeSet<String>,
    family_model: &SetupVariantFamilyModel,
) -> String {
    let tokens = setup_variant_document_family_tokens(document, query_terms);
    if let Some(token) = tokens.iter().find(|token| {
        family_model
            .token_document_frequency
            .get(*token)
            .is_some_and(|frequency| *frequency < family_model.document_count)
    }) {
        return token.clone();
    }
    if !tokens.is_empty() {
        return tokens.join(" ");
    }
    document.document_id.to_string()
}

pub(super) fn setup_variant_document_family_tokens(
    document: &KnowledgeDocumentRow,
    query_terms: &BTreeSet<String>,
) -> Vec<String> {
    let title = document
        .title
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(&document.external_key);
    normalized_alnum_tokens(title, 3)
        .into_iter()
        .filter(|token| !query_terms.iter().any(|query_token| soft_token_match(query_token, token)))
        .collect::<Vec<_>>()
}

pub(super) fn setup_variant_query_terms(
    question: &str,
    query_ir: Option<&QueryIR>,
) -> BTreeSet<String> {
    let mut terms: BTreeSet<String> =
        normalized_alnum_tokens(strip_leading_question_marker(question), 3).into_iter().collect();
    if let Some(query_ir) = query_ir {
        for entity in &query_ir.target_entities {
            terms.extend(normalized_alnum_tokens(&entity.label, 3));
        }
        if let Some(document_focus) = query_ir.document_focus.as_ref() {
            terms.extend(normalized_alnum_tokens(&document_focus.hint, 3));
        }
        if let Some(retrieval_query) = query_ir.retrieval_query.as_deref() {
            terms.extend(normalized_alnum_tokens(retrieval_query, 3));
        }
    }
    terms
}

pub(super) fn soft_token_overlap_count(left: &BTreeSet<String>, right: &BTreeSet<String>) -> usize {
    left.iter()
        .filter(|left_token| {
            right.iter().any(|right_token| soft_token_match(left_token, right_token))
        })
        .count()
}

pub(super) fn soft_token_match(left: &str, right: &str) -> bool {
    if near_token_match(left, right) {
        return true;
    }
    let left_len = left.chars().count();
    let right_len = right.chars().count();
    if left_len < 5 || right_len < 5 {
        return false;
    }
    let common_prefix = common_prefix_char_count(left, right);
    common_prefix >= 4 && common_prefix.saturating_mul(2) >= left_len.min(right_len)
}
