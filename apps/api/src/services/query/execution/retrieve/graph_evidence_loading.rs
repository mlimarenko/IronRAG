use crate::infra::knowledge_plane::ChunkLexicalQuery;

use anyhow::Context;

use super::{
    AppState, BTreeSet, CONTENT_ANCHOR_CHUNK_SCORE_BASE, CONTENT_ANCHOR_CHUNK_SCORE_STEP,
    CONTENT_ANCHOR_EVIDENCE_SCORE_STEP, DateTime, ENTITY_BIO_CHUNK_CAP,
    ENTITY_BIO_CHUNK_SCORE_BASE, ENTITY_BIO_CHUNK_SCORE_STEP, EntityMention,
    GRAPH_EVIDENCE_CHUNK_CAP, GRAPH_EVIDENCE_CHUNK_SCORE_BASE, GRAPH_EVIDENCE_CHUNK_SCORE_STEP,
    GRAPH_EVIDENCE_CONTEXT_LINE_CAP, GraphEvidenceScope, HashMap, HashSet, KnowledgeDocumentRow,
    LexicalQueryOrigin, QUERY_IR_FOCUS_CHUNK_CAP, QUERY_IR_FOCUS_CHUNK_SCORE_BASE,
    QUERY_IR_FOCUS_CHUNK_SCORE_STEP, QueryGraphIndex, QueryIR, RuntimeChunkScoreKind,
    RuntimeMatchedChunk, Utc, Uuid, apply_graph_evidence_texts_to_chunks, batch_hydrate_hits,
    build_graph_evidence_text_queries, current_document_revision_id,
    graph_evidence_chunk_hits_from_rows, graph_evidence_context_fetch_cap,
    graph_evidence_context_line, graph_evidence_context_line_focus_keywords,
    load_ranked_graph_evidence_rows_for_query, query_ir_lexical_focus_queries, repositories,
    resolve_document_hint, retain_entity_bio_candidates, timed_stage,
};

#[derive(Debug, Clone)]
pub(crate) struct RuntimeGraphEvidenceRetrieval {
    pub(crate) chunks: Vec<RuntimeMatchedChunk>,
    pub(crate) context_lines: Vec<String>,
    pub(crate) source_document_ids: Vec<Uuid>,
}

pub(crate) fn entity_bio_chunk_score(rank: usize) -> f32 {
    ENTITY_BIO_CHUNK_SCORE_BASE - (rank as f32 * ENTITY_BIO_CHUNK_SCORE_STEP)
}

pub(crate) fn entity_bio_target_mentions(query_ir: &QueryIR) -> Vec<&EntityMention> {
    query_ir.target_entities.iter().filter(|mention| !mention.label.trim().is_empty()).collect()
}

pub(crate) fn graph_evidence_chunk_score(rank: usize) -> f32 {
    GRAPH_EVIDENCE_CHUNK_SCORE_BASE - (rank as f32 * GRAPH_EVIDENCE_CHUNK_SCORE_STEP)
}

pub(crate) fn query_ir_focus_chunk_score(rank: usize) -> f32 {
    QUERY_IR_FOCUS_CHUNK_SCORE_BASE - (rank as f32 * QUERY_IR_FOCUS_CHUNK_SCORE_STEP)
}

pub(crate) fn content_anchor_chunk_score(rank: usize, evidence_score: usize) -> f32 {
    CONTENT_ANCHOR_CHUNK_SCORE_BASE - (rank as f32 * CONTENT_ANCHOR_CHUNK_SCORE_STEP)
        + (evidence_score.min(4096) as f32 * CONTENT_ANCHOR_EVIDENCE_SCORE_STEP)
}

pub(crate) fn graph_evidence_context_top_k(base_limit: usize) -> usize {
    base_limit.saturating_add(GRAPH_EVIDENCE_CHUNK_CAP)
}

pub(crate) fn query_ir_focus_context_top_k(base_limit: usize) -> usize {
    base_limit.saturating_add(QUERY_IR_FOCUS_CHUNK_CAP)
}

pub(super) async fn entity_bio_evidence_chunk_ids(
    state: &AppState,
    library_id: Uuid,
    projection_version: i64,
    target_mentions: &[&EntityMention],
) -> anyhow::Result<(Vec<Uuid>, usize)> {
    let mut seen_nodes = HashSet::new();
    let mut evidence_targets = Vec::new();
    for mention in target_mentions {
        let nodes = repositories::search_admitted_runtime_graph_entities_by_query_text(
            &state.persistence.postgres,
            library_id,
            projection_version,
            &mention.label,
            4,
        )
        .await
        .context("failed to search graph entities by label for entity-bio retrieval")?;
        for node in nodes {
            if seen_nodes.insert(node.id) {
                evidence_targets.push(("node".to_string(), node.id));
            }
        }
    }
    if evidence_targets.is_empty() {
        return Ok((Vec::new(), seen_nodes.len()));
    }
    let evidence_limit =
        evidence_targets.len().saturating_mul(ENTITY_BIO_CHUNK_CAP).min(i64::MAX as usize) as i64;
    let evidence = repositories::list_runtime_graph_evidence_by_targets(
        &state.persistence.postgres,
        library_id,
        &evidence_targets,
        evidence_limit,
    )
    .await
    .context("failed to list graph evidence for entity-bio retrieval")?;
    let mut chunk_ids = Vec::new();
    for chunk_id in evidence.into_iter().filter_map(|row| row.chunk_id) {
        if chunk_ids.len() >= ENTITY_BIO_CHUNK_CAP {
            break;
        }
        if !chunk_ids.contains(&chunk_id) {
            chunk_ids.push(chunk_id);
        }
    }
    Ok((chunk_ids, seen_nodes.len()))
}

pub(super) async fn entity_bio_lexical_chunk_ids(
    state: &AppState,
    library_id: Uuid,
    target_mentions: &[&EntityMention],
    evidence_chunk_ids: &[Uuid],
) -> anyhow::Result<Vec<Uuid>> {
    let mut lexical_chunk_ids = Vec::new();
    for mention in target_mentions {
        let remaining =
            ENTITY_BIO_CHUNK_CAP.saturating_sub(evidence_chunk_ids.len() + lexical_chunk_ids.len());
        if remaining == 0 {
            break;
        }
        let hits = state
            .search_store
            .search_chunks(ChunkLexicalQuery {
                library_id,
                query: mention.label.trim(),
                query_origin: LexicalQueryOrigin::DerivedProbe,
                limit: remaining.max(4),
                temporal_start: None,
                temporal_end: None,
                text_search_config: None,
            })
            .await
            .context("failed to run lexical entity-label search for entity-bio retrieval")?;
        for hit in hits {
            if lexical_chunk_ids.len() + evidence_chunk_ids.len() >= ENTITY_BIO_CHUNK_CAP {
                break;
            }
            if !evidence_chunk_ids.contains(&hit.chunk_id)
                && !lexical_chunk_ids.contains(&hit.chunk_id)
            {
                lexical_chunk_ids.push(hit.chunk_id);
            }
        }
    }
    Ok(lexical_chunk_ids)
}

/// For entity-describe questions (`QueryAct::Describe` with at least one
/// target entity) the vector + lexical lanes can miss the full picture.
/// This helper fans out over the graph instead: match the entity label
/// against the admitted runtime graph, then load every chunk of evidence
/// attached to that node (capped at `ENTITY_BIO_CHUNK_CAP`). The caller
/// merges the result into the main retrieval set so the answer model sees
/// all mentions of the entity, not just the top-scored one.
pub(super) async fn load_entity_bio_chunks(
    state: &AppState,
    library_id: Uuid,
    query_ir: Option<&QueryIR>,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    plan_keywords: &[String],
    targeted_document_ids: &BTreeSet<Uuid>,
) -> anyhow::Result<Vec<RuntimeMatchedChunk>> {
    let Some(ir) = query_ir else {
        return Ok(Vec::new());
    };
    if ir.target_entities.is_empty() {
        return Ok(Vec::new());
    }

    let snapshot =
        repositories::get_runtime_graph_snapshot(&state.persistence.postgres, library_id)
            .await
            .context("failed to load graph projection snapshot for entity-bio retrieval")?;
    let Some(snapshot) = snapshot else {
        return Ok(Vec::new());
    };

    // The compiler already decided which spans are entities. Script and
    // capitalization carry no additional domain-neutral meaning here.
    let target_mentions = entity_bio_target_mentions(ir);
    if target_mentions.is_empty() {
        return Ok(Vec::new());
    }

    let (all_evidence_chunk_ids, evidence_node_count) = timed_stage(
        "retrieve.entity_bio_evidence",
        entity_bio_evidence_chunk_ids(
            state,
            library_id,
            snapshot.projection_version,
            &target_mentions,
        ),
    )
    .await?;

    // Graph-evidence is bounded by what the `extract_graph` stage
    // captured — low-confidence or oblique-case mentions often miss
    // that pass. Complement the graph lookup with a dedicated lexical
    // search over the entity label itself so every chunk where the
    // label appears as plain text contributes, not just the ones that
    // became evidence rows.
    let lexical_chunk_ids = timed_stage(
        "retrieve.entity_bio_lexical",
        entity_bio_lexical_chunk_ids(state, library_id, &target_mentions, &all_evidence_chunk_ids),
    )
    .await?;

    if all_evidence_chunk_ids.is_empty() && lexical_chunk_ids.is_empty() {
        return Ok(Vec::new());
    }

    let evidence_chunk_id_set: HashSet<Uuid> = all_evidence_chunk_ids.iter().copied().collect();
    let mut all_ids = all_evidence_chunk_ids;
    all_ids.extend(lexical_chunk_ids.iter().copied());
    let candidate_total = all_ids.len();
    let hits: Vec<(Uuid, f32)> = all_ids
        .into_iter()
        .enumerate()
        .map(|(rank, id)| (id, entity_bio_chunk_score(rank)))
        .collect();
    let candidates = timed_stage(
        "retrieve.entity_bio_hydrate",
        batch_hydrate_hits(state, hits, document_index, plan_keywords, targeted_document_ids),
    )
    .await?;
    // Post-filter: full-text BM25 stems tokens, so a surname like
    // "Foster" can retrieve chunks mentioning "forest" that share a
    // stem but have nothing to do with the target person. Similarly,
    // a graph entity whose label contains the mention as substring may
    // attach evidence chunks that do not carry the name as plain text.
    // Keep only chunks whose raw text actually contains one of the
    // mention labels as a case-insensitive substring — this is the
    // literal grounding the answer model needs.
    let label_tokens: Vec<String> = target_mentions
        .iter()
        .map(|m| m.label.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .collect();
    let mut chunks =
        retain_entity_bio_candidates(candidates, &evidence_chunk_id_set, &label_tokens);
    for chunk in &mut chunks {
        chunk.score_kind = RuntimeChunkScoreKind::EntityBio;
    }
    tracing::info!(
        stage = "retrieval.entity_bio",
        entity_label_count = ir.target_entities.len(),
        evidence_node_count,
        lexical_extra_count = lexical_chunk_ids.len(),
        candidate_chunk_count = candidate_total,
        evidence_chunk_count = chunks.len(),
        "entity-bio fan-out loaded extra chunks for Describe-intent query",
    );
    Ok(chunks)
}

pub(crate) async fn load_graph_evidence_chunks_for_bundle(
    scope: GraphEvidenceScope<'_>,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    targeted_document_ids: &BTreeSet<Uuid>,
    plan_keywords: &[String],
) -> anyhow::Result<RuntimeGraphEvidenceRetrieval> {
    let GraphEvidenceScope { state, library_id, question, plan, query_ir, graph_index, .. } = scope;
    let ranked_evidence = timed_stage(
        "retrieve.graph_evidence_rank",
        load_ranked_graph_evidence_rows_for_query(
            scope,
            targeted_document_ids,
            graph_evidence_context_fetch_cap(),
        ),
    )
    .await
    .context("failed to load ranked graph evidence rows for chunk hydration")?;
    let query_ir_focus_queries = query_ir.map(query_ir_lexical_focus_queries).unwrap_or_default();
    let text_queries =
        build_graph_evidence_text_queries(question, plan, &query_ir_focus_queries, query_ir);
    let context_focus_keywords =
        graph_evidence_context_line_focus_keywords(question, &text_queries);
    let context_source_labels = timed_stage(
        "retrieve.graph_evidence_context_labels",
        load_graph_evidence_context_source_labels(
            state,
            library_id,
            &ranked_evidence.rows,
            document_index,
        ),
    )
    .await;
    let context_lines = graph_evidence_context_lines_from_rows(
        &ranked_evidence.rows,
        graph_index,
        &context_source_labels,
        &context_focus_keywords,
    );
    let source_document_ids = graph_evidence_source_document_ids_with_priority(
        &ranked_evidence.target_source_document_ids,
        &ranked_evidence.rows,
    );
    let (hits, evidence_texts_by_chunk) =
        graph_evidence_chunk_hits_from_rows(&ranked_evidence.rows);
    if hits.is_empty() {
        return Ok(graph_evidence_without_hydratable_chunks(
            &ranked_evidence,
            context_lines,
            source_document_ids,
        ));
    }

    let mut chunks = timed_stage(
        "retrieve.graph_evidence_hydrate",
        batch_hydrate_hits(state, hits, document_index, plan_keywords, targeted_document_ids),
    )
    .await
    .context("failed to hydrate graph evidence chunks")?;
    apply_graph_evidence_texts_to_chunks(
        &mut chunks,
        &evidence_texts_by_chunk,
        plan_keywords,
        &context_focus_keywords,
    );
    for chunk in &mut chunks {
        chunk.score_kind = RuntimeChunkScoreKind::GraphEvidence;
    }
    log_graph_evidence_chunks(&ranked_evidence, context_lines.len(), chunks.len());
    Ok(RuntimeGraphEvidenceRetrieval { chunks, context_lines, source_document_ids })
}

/// Logs the ranked graph evidence that produced no hydratable chunk and hands
/// back the empty-retrieval outcome for the caller to return.
fn graph_evidence_without_hydratable_chunks(
    ranked_evidence: &RankedGraphEvidenceRows,
    context_lines: Vec<String>,
    source_document_ids: Vec<Uuid>,
) -> RuntimeGraphEvidenceRetrieval {
    tracing::info!(
        stage = "retrieval.graph_evidence",
        graph_target_count = ranked_evidence.graph_target_count,
        text_query_count = ranked_evidence.text_query_count,
        text_query_executed_count = ranked_evidence.text_query_executed_count,
        text_evidence_count = ranked_evidence.text_evidence_count,
        target_evidence_count = ranked_evidence.target_evidence_count,
        ranked_evidence_count = ranked_evidence.rows.len(),
        graph_evidence_line_count = context_lines.len(),
        evidence_chunk_count = 0usize,
        "graph evidence rows loaded without hydratable chunks",
    );
    RuntimeGraphEvidenceRetrieval { chunks: Vec::new(), context_lines, source_document_ids }
}

/// Logs the hydrated graph-evidence chunk count for one ranked row set.
fn log_graph_evidence_chunks(
    ranked_evidence: &RankedGraphEvidenceRows,
    graph_evidence_line_count: usize,
    evidence_chunk_count: usize,
) {
    tracing::info!(
        stage = "retrieval.graph_evidence",
        graph_target_count = ranked_evidence.graph_target_count,
        text_query_count = ranked_evidence.text_query_count,
        text_query_executed_count = ranked_evidence.text_query_executed_count,
        text_evidence_count = ranked_evidence.text_evidence_count,
        target_evidence_count = ranked_evidence.target_evidence_count,
        ranked_evidence_count = ranked_evidence.rows.len(),
        graph_evidence_line_count,
        evidence_chunk_count,
        "graph evidence chunks hydrated from ranked graph evidence rows",
    );
}

pub(crate) fn graph_evidence_source_document_ids(
    rows: &[repositories::RuntimeGraphEvidenceRow],
) -> Vec<Uuid> {
    let mut seen = HashSet::new();
    rows.iter()
        .filter_map(|row| row.document_id)
        .filter(|document_id| seen.insert(*document_id))
        .collect()
}

pub(crate) fn graph_evidence_source_document_ids_with_priority(
    priority_document_ids: &[Uuid],
    rows: &[repositories::RuntimeGraphEvidenceRow],
) -> Vec<Uuid> {
    let mut seen = HashSet::new();
    let ranked_document_ids = graph_evidence_source_document_ids(rows);
    priority_document_ids
        .iter()
        .copied()
        .chain(ranked_document_ids)
        .filter(|document_id| seen.insert(*document_id))
        .collect()
}

pub(super) async fn load_graph_evidence_context_source_labels(
    state: &AppState,
    library_id: Uuid,
    rows: &[repositories::RuntimeGraphEvidenceRow],
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
) -> HashMap<Uuid, String> {
    let mut revision_ids = Vec::new();
    let mut seen_revisions = HashSet::new();
    for row in rows {
        let Some(document_id) = row.document_id else {
            continue;
        };
        let Some(revision_id) =
            document_index.get(&document_id).and_then(current_document_revision_id)
        else {
            continue;
        };
        if seen_revisions.insert(revision_id) {
            revision_ids.push(revision_id);
        }
    }
    if revision_ids.is_empty() {
        return HashMap::new();
    }

    let library_setting = repositories::catalog_repository::get_library_by_id(
        &state.persistence.postgres,
        library_id,
    )
    .await
    .ok()
    .flatten()
    .map(|library| library.include_document_hint_in_mcp_answers)
    .unwrap_or(true);

    state
        .document_store
        .list_revisions_by_ids(&revision_ids)
        .await
        .unwrap_or_default()
        .into_iter()
        .filter_map(|revision| {
            let document = document_index.get(&revision.document_id)?;
            let document_title = document.title.as_deref().or(Some(document.external_key.as_str()));
            let source_label = resolve_document_hint(
                &revision.revision_kind,
                revision.source_uri.as_deref(),
                revision.document_hint.as_deref(),
                document_title,
                library_setting,
            )?;
            let source_label = source_label.trim().to_string();
            (!source_label.is_empty()).then_some((revision.document_id, source_label))
        })
        .collect()
}

pub(super) fn graph_evidence_context_lines_from_rows(
    rows: &[repositories::RuntimeGraphEvidenceRow],
    graph_index: &QueryGraphIndex,
    source_labels_by_document_id: &HashMap<Uuid, String>,
    focus_keywords: &[String],
) -> Vec<String> {
    let mut lines = Vec::with_capacity(rows.len());
    for row in rows.iter().take(GRAPH_EVIDENCE_CONTEXT_LINE_CAP) {
        let Some(line) = graph_evidence_context_line(
            row,
            graph_index,
            source_labels_by_document_id,
            focus_keywords,
        ) else {
            continue;
        };
        lines.push(line);
    }
    lines
}

#[derive(Debug)]
pub(super) struct RankedGraphEvidenceRows {
    pub(super) rows: Vec<repositories::RuntimeGraphEvidenceRow>,
    pub(super) target_source_document_ids: Vec<Uuid>,
    pub(super) graph_target_count: usize,
    pub(super) text_query_count: usize,
    pub(super) text_query_executed_count: usize,
    pub(super) text_evidence_count: usize,
    pub(super) target_evidence_count: usize,
}

#[derive(Debug, Clone)]
pub(super) struct GraphEvidenceSourceDocumentCandidate {
    pub(super) document_id: Uuid,
    pub(super) best_score: usize,
    pub(super) total_score: usize,
    pub(super) first_ordinal: usize,
    pub(super) best_confidence: f64,
    pub(super) latest_created_at: DateTime<Utc>,
}

pub(super) type GraphEvidenceChunkHits = Vec<(Uuid, f32)>;
pub(super) type GraphEvidenceTextsByChunk = HashMap<Uuid, Vec<String>>;
