use anyhow::Context;

use super::{
    AppState, BTreeSet, GRAPH_EVIDENCE_CHUNK_CAP, GRAPH_EVIDENCE_SOURCE_DOCUMENT_PRIORITY_CAP,
    GRAPH_EVIDENCE_TEXTS_PER_CHUNK, GraphEvidenceChunkHits, GraphEvidenceContextCandidate,
    GraphEvidenceContextRequest, GraphEvidenceSourceDocumentCandidate, GraphEvidenceTextsByChunk,
    GraphTargetEntityProfile, HashMap, HashSet, QueryGraphIndex, QueryIR, RankedGraphEvidenceRows,
    RuntimeMatchedEntity, RuntimeMatchedRelationship, RuntimeQueryPlan, Uuid,
    build_graph_evidence_text_queries, graph_evidence_chunk_score,
    graph_evidence_context_candidate_fields, graph_evidence_context_fetch_cap,
    graph_evidence_context_focus_texts, graph_evidence_context_focus_tokens,
    graph_evidence_db_text_queries, graph_evidence_targets_for_query,
    query_ir_lexical_focus_queries, rank_graph_evidence_context_rows,
    repair_technical_layout_noise, repositories, score_graph_evidence_context_candidates,
};

/// Runtime graph bundle a graph-evidence search and its chunk hydration rank:
/// the matched entities/relationships, the compiled query signal they were
/// matched from and the graph/target-profile indexes they resolve against.
#[derive(Clone, Copy)]
pub(crate) struct GraphEvidenceScope<'a> {
    pub(crate) state: &'a AppState,
    pub(crate) library_id: Uuid,
    pub(crate) question: &'a str,
    pub(crate) entities: &'a [RuntimeMatchedEntity],
    pub(crate) relationships: &'a [RuntimeMatchedRelationship],
    pub(crate) plan: &'a RuntimeQueryPlan,
    pub(crate) query_ir: Option<&'a QueryIR>,
    pub(crate) target_entity_profiles: &'a [GraphTargetEntityProfile],
    pub(crate) graph_index: &'a QueryGraphIndex,
}

pub(super) async fn load_ranked_graph_evidence_rows_for_query(
    scope: GraphEvidenceScope<'_>,
    targeted_document_ids: &BTreeSet<Uuid>,
    limit: usize,
) -> anyhow::Result<RankedGraphEvidenceRows> {
    let GraphEvidenceScope {
        state,
        library_id,
        question,
        entities,
        relationships,
        plan,
        query_ir,
        target_entity_profiles,
        graph_index,
    } = scope;
    let started = std::time::Instant::now();
    let targets = graph_evidence_targets_for_query(
        entities,
        relationships,
        plan,
        query_ir,
        target_entity_profiles,
        graph_index,
    );
    let target_build_elapsed_ms = started.elapsed().as_millis();
    let query_build_started = std::time::Instant::now();
    let query_ir_focus_queries = query_ir.map(query_ir_lexical_focus_queries).unwrap_or_default();
    let text_queries =
        build_graph_evidence_text_queries(question, plan, &query_ir_focus_queries, query_ir);
    let db_text_queries = graph_evidence_db_text_queries(&text_queries, query_ir);
    let query_build_elapsed_ms = query_build_started.elapsed().as_millis();

    let target_search_started = std::time::Instant::now();
    let target_evidence = if targets.is_empty() {
        Vec::new()
    } else {
        repositories::list_runtime_graph_evidence_by_targets(
            &state.persistence.postgres,
            library_id,
            &targets,
            graph_evidence_context_fetch_cap() as i64,
        )
        .await
        .context("failed to list graph evidence context for retrieved graph targets")?
    };
    let target_search_elapsed_ms = target_search_started.elapsed().as_millis();
    let target_filter_started = std::time::Instant::now();
    let target_evidence = if targeted_document_ids.is_empty() {
        target_evidence
    } else {
        filter_graph_evidence_rows_for_target_documents(
            state,
            target_evidence,
            targeted_document_ids,
        )
        .await?
    };
    let target_filter_elapsed_ms = target_filter_started.elapsed().as_millis();

    let target_source_document_ids = graph_evidence_source_document_ids_from_scored_targets(
        &target_evidence,
        question,
        &text_queries,
        graph_index,
        target_entity_profiles,
    );
    let text_search_document_ids = graph_evidence_text_search_document_scope(
        targeted_document_ids,
        &target_source_document_ids,
    );

    let text_started = std::time::Instant::now();
    let text_evidence = repositories::search_runtime_graph_evidence_by_text(
        &state.persistence.postgres,
        library_id,
        &db_text_queries,
        &text_search_document_ids,
        graph_evidence_context_fetch_cap() as i64,
    )
    .await
    .context("failed to search graph evidence context by evidence text")?;
    let text_search_elapsed_ms = text_started.elapsed().as_millis();
    let text_filter_started = std::time::Instant::now();
    let text_evidence = if targeted_document_ids.is_empty() {
        text_evidence
    } else {
        filter_graph_evidence_rows_for_target_documents(state, text_evidence, targeted_document_ids)
            .await?
    };
    let text_filter_elapsed_ms = text_filter_started.elapsed().as_millis();

    let rank_started = std::time::Instant::now();
    let rows = rank_graph_evidence_context_rows(GraphEvidenceContextRequest {
        text_evidence: &text_evidence,
        target_evidence: &target_evidence,
        question,
        text_queries: &text_queries,
        graph_index,
        target_entity_profiles,
        limit,
    });
    let rank_elapsed_ms = rank_started.elapsed().as_millis();
    tracing::info!(
        stage = "retrieval.graph_evidence_breakdown",
        graph_target_count = targets.len(),
        text_query_count = text_queries.len(),
        text_query_executed_count = db_text_queries.len(),
        text_evidence_count = text_evidence.len(),
        target_evidence_count = target_evidence.len(),
        ranked_evidence_count = rows.len(),
        target_build_elapsed_ms,
        target_profile_count = target_entity_profiles.len(),
        query_build_elapsed_ms,
        text_search_elapsed_ms,
        text_filter_elapsed_ms,
        target_search_elapsed_ms,
        target_filter_elapsed_ms,
        text_search_document_scope_count = text_search_document_ids.len(),
        target_document_scope_count = targeted_document_ids.len(),
        target_source_document_count = target_source_document_ids.len(),
        rank_elapsed_ms,
        total_elapsed_ms = started.elapsed().as_millis(),
        "graph evidence retrieval substeps completed",
    );

    Ok(RankedGraphEvidenceRows {
        rows,
        target_source_document_ids,
        graph_target_count: targets.len(),
        text_query_count: text_queries.len(),
        text_query_executed_count: db_text_queries.len(),
        text_evidence_count: text_evidence.len(),
        target_evidence_count: target_evidence.len(),
    })
}

pub(crate) fn graph_evidence_text_search_document_scope(
    targeted_document_ids: &BTreeSet<Uuid>,
    target_source_document_ids: &[Uuid],
) -> Vec<Uuid> {
    if !targeted_document_ids.is_empty() {
        return targeted_document_ids.iter().copied().collect();
    }
    let mut seen = BTreeSet::new();
    target_source_document_ids
        .iter()
        .copied()
        .filter(|document_id| seen.insert(*document_id))
        .collect()
}

pub(crate) fn graph_evidence_source_document_ids_from_scored_targets(
    target_evidence: &[repositories::RuntimeGraphEvidenceRow],
    question: &str,
    text_queries: &[String],
    graph_index: &QueryGraphIndex,
    target_entity_profiles: &[GraphTargetEntityProfile],
) -> Vec<Uuid> {
    let focus_texts = graph_evidence_context_focus_texts(question, text_queries);
    let focus_tokens = graph_evidence_context_focus_tokens(&focus_texts);
    if target_evidence.is_empty() || focus_tokens.is_empty() {
        return Vec::new();
    }

    let mut candidates = target_evidence
        .iter()
        .enumerate()
        .map(|(ordinal, row)| {
            let fields = graph_evidence_context_candidate_fields(row, graph_index);
            let tokens = fields
                .iter()
                .flat_map(|field| field.tokens.iter().cloned())
                .collect::<BTreeSet<_>>();
            GraphEvidenceContextCandidate { row: row.clone(), ordinal, fields, tokens, score: 0 }
        })
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        return Vec::new();
    }

    score_graph_evidence_context_candidates(&mut candidates, &focus_tokens, target_entity_profiles);

    let mut documents = HashMap::<Uuid, GraphEvidenceSourceDocumentCandidate>::new();
    for candidate in candidates.into_iter().filter(|candidate| candidate.score > 0) {
        let Some(document_id) = candidate.row.document_id else {
            continue;
        };
        let confidence = candidate.row.confidence_score.unwrap_or(f64::NEG_INFINITY);
        documents
            .entry(document_id)
            .and_modify(|document| {
                document.best_score = document.best_score.max(candidate.score);
                document.total_score = document.total_score.saturating_add(candidate.score);
                document.first_ordinal = document.first_ordinal.min(candidate.ordinal);
                document.best_confidence = document.best_confidence.max(confidence);
                document.latest_created_at =
                    document.latest_created_at.max(candidate.row.created_at);
            })
            .or_insert(GraphEvidenceSourceDocumentCandidate {
                document_id,
                best_score: candidate.score,
                total_score: candidate.score,
                first_ordinal: candidate.ordinal,
                best_confidence: confidence,
                latest_created_at: candidate.row.created_at,
            });
    }

    let mut documents = documents.into_values().collect::<Vec<_>>();
    documents.sort_by(|left, right| {
        right
            .best_score
            .cmp(&left.best_score)
            .then_with(|| right.total_score.cmp(&left.total_score))
            .then_with(|| left.first_ordinal.cmp(&right.first_ordinal))
            .then_with(|| {
                right
                    .best_confidence
                    .partial_cmp(&left.best_confidence)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| right.latest_created_at.cmp(&left.latest_created_at))
            .then_with(|| right.document_id.cmp(&left.document_id))
    });
    documents
        .into_iter()
        .take(GRAPH_EVIDENCE_SOURCE_DOCUMENT_PRIORITY_CAP)
        .map(|document| document.document_id)
        .collect()
}

pub(super) async fn filter_graph_evidence_rows_for_target_documents(
    state: &AppState,
    rows: Vec<repositories::RuntimeGraphEvidenceRow>,
    targeted_document_ids: &BTreeSet<Uuid>,
) -> anyhow::Result<Vec<repositories::RuntimeGraphEvidenceRow>> {
    if rows.is_empty() || targeted_document_ids.is_empty() {
        return Ok(rows);
    }

    let mut selected = Vec::with_capacity(rows.len());
    let mut unresolved = Vec::with_capacity(rows.len());
    let mut fallback_chunk_ids = BTreeSet::new();

    for (position, row) in rows.into_iter().enumerate() {
        if let Some(document_id) = row.document_id {
            if targeted_document_ids.contains(&document_id) {
                selected.push((position, row));
            }
            continue;
        }

        let Some(chunk_id) = row.chunk_id else {
            continue;
        };
        unresolved.push((position, row, chunk_id));
        fallback_chunk_ids.insert(chunk_id);
    }

    if unresolved.is_empty() {
        selected.sort_unstable_by_key(|(position, _)| *position);
        return Ok(selected.into_iter().map(|(_, row)| row).collect());
    }

    let chunk_rows = state
        .document_store
        .list_chunks_by_ids(&fallback_chunk_ids.iter().copied().collect::<Vec<_>>())
        .await
        .context("failed to resolve chunk document ids for scoped graph evidence filtering")?;
    let chunk_documents = chunk_rows
        .into_iter()
        .map(|chunk| (chunk.chunk_id, chunk.document_id))
        .collect::<HashMap<_, _>>();

    for (position, row, chunk_id) in unresolved {
        if chunk_documents
            .get(&chunk_id)
            .is_some_and(|document_id| targeted_document_ids.contains(document_id))
        {
            selected.push((position, row));
        }
    }

    selected.sort_unstable_by_key(|(position, _)| *position);
    Ok(selected.into_iter().map(|(_, row)| row).collect())
}

pub(crate) fn graph_evidence_chunk_hits_from_rows(
    rows: &[repositories::RuntimeGraphEvidenceRow],
) -> (GraphEvidenceChunkHits, GraphEvidenceTextsByChunk) {
    let mut seen_chunks = HashSet::<Uuid>::new();
    let mut seen_texts = HashSet::<(Uuid, String)>::new();
    let mut evidence_texts_by_chunk = HashMap::<Uuid, Vec<String>>::new();
    let mut hits = Vec::new();
    for row in rows {
        let Some(chunk_id) = row.chunk_id else {
            continue;
        };
        if !seen_chunks.contains(&chunk_id) {
            if hits.len() >= GRAPH_EVIDENCE_CHUNK_CAP {
                continue;
            }
            seen_chunks.insert(chunk_id);
            let score = graph_evidence_chunk_score(hits.len());
            hits.push((chunk_id, score));
        }
        let evidence_text = repair_technical_layout_noise(row.evidence_text.trim());
        if !evidence_text.is_empty() && seen_texts.insert((chunk_id, evidence_text.clone())) {
            let texts = evidence_texts_by_chunk.entry(chunk_id).or_default();
            if texts.len() < GRAPH_EVIDENCE_TEXTS_PER_CHUNK {
                texts.push(evidence_text);
            }
        }
    }
    (hits, evidence_texts_by_chunk)
}
