use crate::infra::knowledge_plane::ChunkLexicalQuery;

use anyhow::Context;

use super::{
    ARTIFACT_SIBLING_SOURCE_CHUNKS_PER_DOCUMENT, ARTIFACT_SIBLING_SOURCE_DOCUMENT_CAP,
    ARTIFACT_SIBLING_SOURCE_FOCUS_TERM_CAP, ARTIFACT_SIBLING_SOURCE_SCORE_BASE,
    ARTIFACT_SIBLING_SOURCE_SCORE_STEP, ARTIFACT_SIBLING_SOURCE_STRUCTURAL_CHUNKS_PER_DOCUMENT,
    AppState, BTreeSet, HashMap, KnowledgeChunkRow, KnowledgeDocumentRow, KnowledgeRevisionRow,
    LINKED_ANCHOR_CONTEXT_CHUNKS_PER_QUERY, LexicalQueryOrigin, QueryIR, RetrievalLaneContext,
    RuntimeChunkScoreKind, RuntimeMatchedChunk, Uuid, attachment_parent_page_id,
    batch_hydrate_hits, combine_query_ir_focus_search_results, common_prefix_char_count,
    current_document_revision_id, join_all, linked_anchor_focus_queries, map_chunk_hit,
    near_token_overlap_count, normalize_document_identity_value, normalized_alnum_token_sequence,
    normalized_alnum_tokens, setup_focus_document_identity_values,
    setup_focus_document_is_standalone_image, source_page_id,
};

pub(super) async fn load_linked_anchor_context_chunks(
    context: RetrievalLaneContext<'_>,
    library_id: Uuid,
    query_ir: Option<&QueryIR>,
    chunks: &[RuntimeMatchedChunk],
) -> anyhow::Result<Vec<RuntimeMatchedChunk>> {
    let RetrievalLaneContext {
        state,
        question,
        document_index,
        plan_keywords,
        temporal_start,
        temporal_end,
    } = context;
    let anchor_queries = linked_anchor_focus_queries(question, query_ir, plan_keywords, chunks);
    if anchor_queries.is_empty() {
        return Ok(Vec::new());
    }

    let per_query_futures = anchor_queries.iter().cloned().map(|anchor_query| async move {
        state
            .search_store
            .search_chunks(ChunkLexicalQuery {
                library_id,
                query: &anchor_query,
                query_origin: LexicalQueryOrigin::DerivedProbe,
                limit: LINKED_ANCHOR_CONTEXT_CHUNKS_PER_QUERY,
                temporal_start,
                temporal_end,
                text_search_config: None,
            })
            .await
            .map(|rows| {
                rows.into_iter().map(|row| (row.chunk_id, row.score as f32)).collect::<Vec<_>>()
            })
            .with_context(|| format!("failed to run linked-anchor chunk search: {anchor_query}"))
    });
    let per_query_results: Vec<Result<Vec<_>, anyhow::Error>> = join_all(per_query_futures).await;
    let hits = combine_query_ir_focus_search_results(per_query_results, anchor_queries.len())?;
    if hits.is_empty() {
        return Ok(Vec::new());
    }

    // Linked anchors are explicit cross-document affordances in the retrieved source text.
    // Hydrate them library-wide instead of applying the current scoped-document filter;
    // otherwise a focused source document can link to the exact answer document and we
    // would pay the search cost only to discard that linked evidence.
    let linked_anchor_target_filter = linked_anchor_hydration_target_filter();
    let mut chunks = batch_hydrate_hits(
        state,
        hits,
        document_index,
        plan_keywords,
        &linked_anchor_target_filter,
    )
    .await
    .context("failed to hydrate linked-anchor chunks")?;
    for chunk in &mut chunks {
        chunk.score_kind = RuntimeChunkScoreKind::SourceContext;
    }
    tracing::info!(
        stage = "retrieval.linked_anchor_context",
        anchor_query_count = anchor_queries.len(),
        anchor_chunk_count = chunks.len(),
        "linked anchor context chunks loaded from retrieved source links",
    );
    Ok(chunks)
}

pub(crate) fn linked_anchor_hydration_target_filter() -> BTreeSet<Uuid> {
    BTreeSet::new()
}

pub(super) async fn load_artifact_sibling_source_chunks(
    state: &AppState,
    chunks: &[RuntimeMatchedChunk],
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    plan_keywords: &[String],
) -> anyhow::Result<Vec<RuntimeMatchedChunk>> {
    let candidate_document_ids = artifact_sibling_source_document_ids(
        chunks,
        document_index,
        ARTIFACT_SIBLING_SOURCE_DOCUMENT_CAP,
    );
    if candidate_document_ids.is_empty() {
        return Ok(Vec::new());
    }

    let anchor_document_ids = chunks.iter().map(|chunk| chunk.document_id).collect::<BTreeSet<_>>();
    let structural_parent_document_ids = artifact_sibling_source_parent_page_document_ids(
        chunks,
        document_index,
        &anchor_document_ids,
        ARTIFACT_SIBLING_SOURCE_DOCUMENT_CAP,
    )
    .into_iter()
    .collect::<BTreeSet<_>>();
    let focus_terms = artifact_sibling_source_focus_terms(plan_keywords, chunks);
    let mut selected = Vec::new();
    for (document_rank, document_id) in candidate_document_ids.iter().enumerate() {
        let Some(document) = document_index.get(document_id) else {
            continue;
        };
        let Some(revision_id) = current_document_revision_id(document) else {
            continue;
        };
        let include_revision_history = structural_parent_document_ids.contains(document_id);
        let rows = load_artifact_sibling_source_rows_for_document(
            state,
            document,
            revision_id,
            include_revision_history,
            &focus_terms,
        )
        .await?;
        for (chunk_rank, row) in rows.into_iter().enumerate() {
            let score = artifact_sibling_source_chunk_score(document_rank, chunk_rank);
            if let Some(mut chunk) = map_chunk_hit(row, score, document_index, plan_keywords) {
                chunk.score = Some(score);
                chunk.score_kind = RuntimeChunkScoreKind::SourceContext;
                selected.push(chunk);
            }
        }
    }
    if !selected.is_empty() {
        tracing::info!(
            stage = "retrieval.artifact_sibling_source",
            sibling_document_count = candidate_document_ids.len(),
            structural_parent_document_count = structural_parent_document_ids.len(),
            sibling_chunk_count = selected.len(),
            "artifact sibling source chunks loaded from structurally related documents",
        );
    }
    Ok(selected)
}

pub(super) fn extend_unique_chunk_rows(
    source: impl IntoIterator<Item = KnowledgeChunkRow>,
    chunk_cap: usize,
    seen: &mut BTreeSet<Uuid>,
    rows: &mut Vec<KnowledgeChunkRow>,
) -> bool {
    for row in source {
        if seen.insert(row.chunk_id) {
            rows.push(row);
        }
        if rows.len() >= chunk_cap {
            return true;
        }
    }
    false
}

pub(super) async fn load_artifact_sibling_source_rows_for_document(
    state: &AppState,
    document: &KnowledgeDocumentRow,
    current_revision_id: Uuid,
    include_revision_history: bool,
    focus_terms: &[String],
) -> anyhow::Result<Vec<KnowledgeChunkRow>> {
    let revision_ids = if include_revision_history {
        let revisions = state
            .document_store
            .list_revisions_by_document(document.document_id)
            .await
            .with_context(|| {
                format!(
                    "failed to list artifact sibling source revisions for document {}",
                    document.document_id
                )
            })?;
        artifact_sibling_source_revision_ids(
            document,
            current_revision_id,
            &revisions,
            crate::domains::content::HISTORICAL_PROJECTION_RETENTION_REVISIONS,
        )
    } else {
        vec![current_revision_id]
    };
    if revision_ids.is_empty() {
        return Ok(Vec::new());
    }

    let chunk_cap = if include_revision_history {
        ARTIFACT_SIBLING_SOURCE_STRUCTURAL_CHUNKS_PER_DOCUMENT
    } else {
        ARTIFACT_SIBLING_SOURCE_CHUNKS_PER_DOCUMENT.max(0) as usize
    };
    let mut rows = Vec::new();
    let mut seen = BTreeSet::new();

    if include_revision_history {
        let windows =
            revision_ids.iter().map(|revision_id| (*revision_id, 0, 0)).collect::<Vec<_>>();
        let head_rows =
            state.document_store.list_chunks_by_revisions_windows(&windows).await.with_context(
                || {
                    format!(
                        "failed to load artifact sibling source head chunks for document {}",
                        document.document_id
                    )
                },
            )?;
        if extend_unique_chunk_rows(head_rows, chunk_cap, &mut seen, &mut rows) {
            return Ok(rows);
        }
    }

    if !focus_terms.is_empty() && rows.len() < chunk_cap {
        let focused_rows = state
            .document_store
            .list_chunks_by_revisions_matching_terms(&revision_ids, focus_terms, chunk_cap)
            .await
            .with_context(|| {
                format!(
                    "failed to load artifact sibling source focused chunks for document {}",
                    document.document_id
                )
            })?;
        if extend_unique_chunk_rows(focused_rows, chunk_cap, &mut seen, &mut rows) {
            return Ok(rows);
        }
    }

    if rows.len() < chunk_cap {
        let fallback_max_index = ARTIFACT_SIBLING_SOURCE_CHUNKS_PER_DOCUMENT.saturating_sub(1);
        let fallback_rows = state
            .document_store
            .list_chunks_by_revision_range(current_revision_id, 0, fallback_max_index)
            .await
            .with_context(|| {
                format!(
                    "failed to load artifact sibling source fallback chunks for document {} revision {}",
                    document.document_id, current_revision_id
                )
            })?;
        extend_unique_chunk_rows(fallback_rows, chunk_cap, &mut seen, &mut rows);
    }

    Ok(rows)
}

pub(super) fn artifact_sibling_source_revision_ids(
    document: &KnowledgeDocumentRow,
    current_revision_id: Uuid,
    revisions: &[KnowledgeRevisionRow],
    limit: usize,
) -> Vec<Uuid> {
    let candidates = revisions
        .iter()
        .map(|revision| crate::domains::content::HistoricalProjectionRevision {
            revision_id: revision.revision_id,
            document_id: revision.document_id,
            text_state: revision.text_state.as_str(),
        })
        .collect::<Vec<_>>();
    crate::domains::content::historical_projection_retained_revision_ids(
        document.document_id,
        current_revision_id,
        &candidates,
        limit,
    )
}

pub(super) fn artifact_sibling_source_focus_terms(
    plan_keywords: &[String],
    chunks: &[RuntimeMatchedChunk],
) -> Vec<String> {
    let mut terms = Vec::new();
    let mut seen = BTreeSet::new();
    for keyword in plan_keywords {
        push_artifact_sibling_source_focus_term(keyword, &mut seen, &mut terms);
        for token in normalized_alnum_token_sequence(keyword, 3) {
            push_artifact_sibling_source_focus_term(&token, &mut seen, &mut terms);
        }
        if terms.len() >= ARTIFACT_SIBLING_SOURCE_FOCUS_TERM_CAP {
            return terms;
        }
    }
    for chunk in chunks {
        push_artifact_sibling_source_focus_term(&chunk.document_label, &mut seen, &mut terms);
        for token in normalized_alnum_token_sequence(&chunk.document_label, 3) {
            push_artifact_sibling_source_focus_term(&token, &mut seen, &mut terms);
        }
        if terms.len() >= ARTIFACT_SIBLING_SOURCE_FOCUS_TERM_CAP {
            return terms;
        }
    }
    terms
}

pub(super) fn push_artifact_sibling_source_focus_term(
    value: &str,
    seen: &mut BTreeSet<String>,
    terms: &mut Vec<String>,
) {
    if terms.len() >= ARTIFACT_SIBLING_SOURCE_FOCUS_TERM_CAP {
        return;
    }
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.chars().filter(|ch| ch.is_alphanumeric()).count() < 3 {
        return;
    }
    let key = normalized.to_lowercase();
    if seen.insert(key) {
        terms.push(normalized);
    }
}

pub(super) fn artifact_sibling_source_document_ids(
    chunks: &[RuntimeMatchedChunk],
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    limit: usize,
) -> Vec<Uuid> {
    if chunks.is_empty() || limit == 0 {
        return Vec::new();
    }
    let anchor_document_ids = chunks.iter().map(|chunk| chunk.document_id).collect::<BTreeSet<_>>();
    let mut selected = Vec::<Uuid>::new();
    let mut seen = BTreeSet::<Uuid>::new();
    for document_id in artifact_sibling_source_parent_page_document_ids(
        chunks,
        document_index,
        &anchor_document_ids,
        limit,
    ) {
        if seen.insert(document_id) {
            selected.push(document_id);
        }
        if selected.len() >= limit {
            return selected;
        }
    }

    let anchors = artifact_sibling_source_anchor_tokens(chunks, document_index);
    if anchors.is_empty() {
        return selected;
    }
    let mut candidates = document_index
        .values()
        .filter(|document| !anchor_document_ids.contains(&document.document_id))
        .filter(|document| !seen.contains(&document.document_id))
        .filter(|document| !setup_focus_document_is_standalone_image(document))
        .filter_map(|document| {
            let score = artifact_sibling_source_document_score(&anchors, document);
            (score > 0).then_some((score, document.document_id))
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|(left_score, left_id), (right_score, right_id)| {
        right_score.cmp(left_score).then_with(|| left_id.cmp(right_id))
    });
    for (_, document_id) in candidates {
        if seen.insert(document_id) {
            selected.push(document_id);
        }
        if selected.len() >= limit {
            break;
        }
    }
    selected
}

pub(super) fn artifact_sibling_source_parent_page_document_ids(
    chunks: &[RuntimeMatchedChunk],
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    anchor_document_ids: &BTreeSet<Uuid>,
    limit: usize,
) -> Vec<Uuid> {
    let page_ids = artifact_sibling_source_attachment_page_ids(chunks, document_index);
    if page_ids.is_empty() {
        return Vec::new();
    }
    let mut candidates = document_index
        .values()
        .filter(|document| !anchor_document_ids.contains(&document.document_id))
        .filter(|document| !setup_focus_document_is_standalone_image(document))
        .filter_map(|document| {
            let score = artifact_sibling_source_parent_page_score(&page_ids, document);
            (score > 0).then_some((score, document.document_id))
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|(left_score, left_id), (right_score, right_id)| {
        right_score.cmp(left_score).then_with(|| left_id.cmp(right_id))
    });
    candidates.into_iter().map(|(_, document_id)| document_id).take(limit).collect()
}

pub(super) fn artifact_sibling_source_attachment_page_ids(
    chunks: &[RuntimeMatchedChunk],
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
) -> BTreeSet<String> {
    chunks
        .iter()
        .filter_map(|chunk| document_index.get(&chunk.document_id))
        .flat_map(document_structural_source_values)
        .filter_map(attachment_parent_page_id)
        .collect()
}

pub(super) fn artifact_sibling_source_parent_page_score(
    page_ids: &BTreeSet<String>,
    document: &KnowledgeDocumentRow,
) -> usize {
    document_structural_source_values(document)
        .into_iter()
        .filter_map(source_page_id)
        .filter(|page_id| page_ids.contains(page_id))
        .map(|page_id| 10_000usize.saturating_add(page_id.len()))
        .max()
        .unwrap_or(0)
}

pub(super) fn artifact_sibling_source_anchor_tokens(
    chunks: &[RuntimeMatchedChunk],
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
) -> Vec<BTreeSet<String>> {
    let mut seen = BTreeSet::<Vec<String>>::new();
    chunks
        .iter()
        .filter_map(|chunk| document_index.get(&chunk.document_id))
        .filter(|document| setup_focus_document_is_standalone_image(document))
        .flat_map(setup_focus_document_identity_values)
        .flat_map(|value| artifact_sibling_identity_prefixes(&value))
        .map(|prefix| normalized_alnum_tokens(&prefix, 3))
        .filter(|tokens| tokens.len() >= 2)
        .filter(|tokens| seen.insert(tokens.iter().cloned().collect()))
        .collect()
}

pub(super) fn document_structural_source_values(document: &KnowledgeDocumentRow) -> Vec<&str> {
    let mut values = Vec::new();
    values.push(document.external_key.as_str());
    if let Some(file_name) = document.file_name.as_deref() {
        values.push(file_name);
    }
    if let Some(source_uri) = document.source_uri.as_deref() {
        values.push(source_uri);
    }
    if let Some(document_hint) = document.document_hint.as_deref() {
        values.push(document_hint);
    }
    values
}

pub(super) fn artifact_sibling_identity_prefixes(value: &str) -> Vec<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    let mut prefixes = Vec::new();
    prefixes.push(trimmed.to_string());
    for separator in [":", " - ", " – ", " — "] {
        if let Some((prefix, _)) = trimmed.split_once(separator) {
            let prefix = prefix.trim();
            if !prefix.is_empty() {
                prefixes.push(prefix.to_string());
            }
        }
    }
    prefixes
}

pub(super) fn artifact_sibling_source_document_score(
    anchors: &[BTreeSet<String>],
    document: &KnowledgeDocumentRow,
) -> usize {
    setup_focus_document_identity_values(document)
        .into_iter()
        .map(|value| {
            let normalized_value = normalize_document_identity_value(&value);
            let value_tokens = normalized_alnum_tokens(&value, 3);
            anchors
                .iter()
                .map(|anchor_tokens| {
                    let overlap = near_token_overlap_count(anchor_tokens, &value_tokens);
                    if overlap < 2 {
                        return 0;
                    }
                    let normalized_anchor =
                        anchor_tokens.iter().cloned().collect::<Vec<_>>().join(" ");
                    let prefix_score =
                        common_prefix_char_count(&normalized_value, &normalized_anchor);
                    overlap
                        .saturating_mul(64)
                        .saturating_add(prefix_score.min(48))
                        .saturating_add(value_tokens.len().min(16))
                })
                .max()
                .unwrap_or(0)
        })
        .max()
        .unwrap_or(0)
}

pub(super) fn artifact_sibling_source_chunk_score(document_rank: usize, chunk_rank: usize) -> f32 {
    ARTIFACT_SIBLING_SOURCE_SCORE_BASE
        - (document_rank as f32 * ARTIFACT_SIBLING_SOURCE_SCORE_STEP * 16.0)
        - (chunk_rank as f32 * ARTIFACT_SIBLING_SOURCE_SCORE_STEP)
}
