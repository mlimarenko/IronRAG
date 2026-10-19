use crate::infra::knowledge_plane::{ChunkLexicalQuery, VectorSimilaritySearchInput};

use futures::StreamExt;

use anyhow::Context;

use super::{
    AppState, BTreeSet, DOCUMENT_IDENTITY_CHUNKS_PER_DOCUMENT,
    DOCUMENT_IDENTITY_FOCUS_PREFIX_CHARS, DOCUMENT_IDENTITY_FOCUSED_CHUNKS_PER_DOCUMENT,
    DOCUMENT_IDENTITY_SCORE_FLOOR, DateTime, HashMap, HashSet, KnowledgeDocumentRow,
    LexicalQueryOrigin, MAX_CONCURRENT_LEXICAL_QUERIES, QueryIR, QueryServiceError,
    RuntimeChunkScoreKind, RuntimeMatchedChunk, RuntimeVectorSearchContext, Utc, Uuid,
    VERSIONED_UPDATE_PROCEDURE_SCORE_BASE, batch_hydrate_hits, combine_lexical_query_results,
    current_document_revision_id, map_chunk_hit, normalized_alnum_tokens, stream,
    validate_embedding_vector_dimensions, validate_runtime_vector_search_context,
};

#[derive(Clone)]
pub(super) struct ChunkRetrievalLaneOutcome {
    pub(super) vector_hits: Vec<RuntimeMatchedChunk>,
    pub(super) vector_elapsed_ms: u128,
    pub(super) lexical_hits: Vec<RuntimeMatchedChunk>,
    pub(super) lexical_query_count: usize,
    pub(super) lexical_elapsed_ms: u128,
    pub(super) degraded_lane_count: usize,
}

pub(super) struct PrimaryRetrievalRequest<'a> {
    pub(super) state: &'a AppState,
    pub(super) library_id: Uuid,
    pub(super) limit: usize,
    pub(super) question_embedding: &'a [f32],
    pub(super) vector_search_context: Option<&'a RuntimeVectorSearchContext>,
    pub(super) document_index: &'a HashMap<Uuid, KnowledgeDocumentRow>,
    pub(super) plan_keywords: &'a [String],
    pub(super) hydration_document_ids: &'a BTreeSet<Uuid>,
    pub(super) lexical_queries: Vec<String>,
    pub(super) lexical_limit: usize,
    pub(super) text_search_config: &'a str,
    pub(super) temporal_start: Option<DateTime<Utc>>,
    pub(super) temporal_end: Option<DateTime<Utc>>,
}

pub(super) async fn execute_primary_retrieval(
    request: PrimaryRetrievalRequest<'_>,
) -> anyhow::Result<ChunkRetrievalLaneOutcome> {
    let PrimaryRetrievalRequest {
        state,
        library_id,
        limit,
        question_embedding,
        vector_search_context,
        document_index,
        plan_keywords,
        hydration_document_ids,
        lexical_queries,
        lexical_limit,
        text_search_config,
        temporal_start,
        temporal_end,
    } = request;
    let vector_future = async {
        let started = std::time::Instant::now();
        if question_embedding.is_empty() {
            tracing::info!(
                stage = "retrieval.vector_skip",
                reason = "question_embedding_empty",
                "vector retrieve skipped: no question embedding"
            );
            return Ok::<(Vec<RuntimeMatchedChunk>, u128), anyhow::Error>((Vec::new(), 0));
        }
        let context = vector_search_context.ok_or_else(|| QueryServiceError::StateConflict {
            message: format!(
                "runtime query for library {library_id} has a vector without a ready exact-profile preflight; retry the query"
            ),
        })?;
        let _vector_guard =
            state.application_services.search.vector_plane_read_guard(state, library_id).await?;
        validate_runtime_vector_search_context(state, library_id, context).await?;
        validate_embedding_vector_dimensions(
            context.dimensions,
            question_embedding,
            "runtime chunk search",
        )?;
        let raw_hits = state
            .search_store
            .search_chunk_vectors_by_similarity(
                VectorSimilaritySearchInput {
                    dim: context.dimensions,
                    library_id,
                    embedding_model_key: &context.embedding_profile_key,
                    query_vector: question_embedding,
                    n_probe: None,
                },
                limit.max(1),
                temporal_start,
                temporal_end,
            )
            .await
            .context("failed to search active chunk vectors for runtime query")?;
        tracing::info!(
            stage = "retrieval.vector_search",
            raw_hit_count = raw_hits.len(),
            embedding_dims = question_embedding.len(),
            limit = limit.max(1),
            "vector search returned raw hits"
        );
        let hits = batch_hydrate_hits(
            state,
            raw_hits.iter().map(|hit| (hit.chunk_id, hit.score as f32)).collect(),
            document_index,
            plan_keywords,
            hydration_document_ids,
        )
        .await?;
        let elapsed = started.elapsed();
        crate::services::query::turn_spans::record_span(
            "retrieve.vector",
            "lane",
            elapsed.as_millis().try_into().unwrap_or(u64::MAX),
            None,
            Some(hits.len() as u64),
        );
        Ok((hits, elapsed.as_millis()))
    };
    let lexical_future = async {
        let started = std::time::Instant::now();
        let lexical_query_count = lexical_queries.len();
        let text_search_config_owned = text_search_config.to_owned();
        let per_query_futures =
            lexical_queries.into_iter().enumerate().map(|(query_index, lexical_query)| {
                let text_search_config_owned = text_search_config_owned.clone();
                async move {
                    let search_result = state
                        .search_store
                        .search_chunks(ChunkLexicalQuery {
                            library_id,
                            query: &lexical_query,
                            query_origin: if query_index == 0 {
                                LexicalQueryOrigin::UserQuery
                            } else {
                                LexicalQueryOrigin::DerivedProbe
                            },
                            limit: lexical_limit,
                            temporal_start,
                            temporal_end,
                            text_search_config: Some(&text_search_config_owned),
                        })
                        .await
                        .with_context(|| {
                            format!(
                                "failed to run lexical chunk search for runtime query: {lexical_query}"
                            )
                        });
                    let result = match search_result {
                        Ok(hits) => {
                            batch_hydrate_hits(
                                state,
                                hits.into_iter()
                                    .map(|hit| (hit.chunk_id, hit.score as f32))
                                    .collect(),
                                document_index,
                                plan_keywords,
                                hydration_document_ids,
                            )
                            .await
                        }
                        Err(error) => Err(error),
                    };
                    (query_index, result)
                }
            });
        let mut indexed_results = stream::iter(per_query_futures)
            .buffer_unordered(MAX_CONCURRENT_LEXICAL_QUERIES)
            .collect::<Vec<_>>()
            .await;
        indexed_results.sort_by_key(|(query_index, _)| *query_index);
        let per_query_results =
            indexed_results.into_iter().map(|(_, result)| result).collect::<Vec<_>>();
        let lexical_hits =
            combine_lexical_query_results(per_query_results, lexical_query_count, lexical_limit)?;
        let elapsed = started.elapsed();
        crate::services::query::turn_spans::record_span(
            "retrieve.lexical",
            "lane",
            elapsed.as_millis().try_into().unwrap_or(u64::MAX),
            None,
            Some(lexical_hits.len() as u64),
        );
        Ok::<(Vec<RuntimeMatchedChunk>, usize, u128), anyhow::Error>((
            lexical_hits,
            lexical_query_count,
            elapsed.as_millis(),
        ))
    };
    let (vector_result, lexical_result) = tokio::join!(vector_future, lexical_future);
    combine_chunk_retrieval_lanes(vector_result, lexical_result)
}

/// Logs a failed vector chunk lane without holding the record inside the lane combiner.
fn log_vector_lane_failure(summary: &str) {
    tracing::warn!(
        stage = "retrieval.vector_failed",
        error = %summary,
        retrieval_degraded = true,
        failed_lane = "vector",
        "vector chunk retrieval failed; continuing with lexical lane if available"
    );
}

/// Logs a failed lexical chunk lane without holding the record inside the lane combiner.
fn log_lexical_lane_failure(summary: &str) {
    tracing::warn!(
        stage = "retrieval.lexical_failed",
        error = %summary,
        retrieval_degraded = true,
        failed_lane = "lexical",
        "lexical chunk retrieval failed; continuing with vector lane if available"
    );
}

pub(super) fn combine_chunk_retrieval_lanes(
    vector_result: anyhow::Result<(Vec<RuntimeMatchedChunk>, u128)>,
    lexical_result: anyhow::Result<(Vec<RuntimeMatchedChunk>, usize, u128)>,
) -> anyhow::Result<ChunkRetrievalLaneOutcome> {
    let mut degraded_lane_count = 0usize;
    let mut failures = Vec::new();

    let (vector_hits, vector_elapsed_ms) = match vector_result {
        Ok(result) => result,
        Err(error) => {
            degraded_lane_count += 1;
            let summary = format!("{error:#}");
            log_vector_lane_failure(&summary);
            failures.push(format!("vector: {summary}"));
            (Vec::new(), 0)
        }
    };

    let (lexical_hits, lexical_query_count, lexical_elapsed_ms) = match lexical_result {
        Ok(result) => result,
        Err(error) => {
            degraded_lane_count += 1;
            let summary = format!("{error:#}");
            log_lexical_lane_failure(&summary);
            failures.push(format!("lexical: {summary}"));
            (Vec::new(), 0, 0)
        }
    };

    if degraded_lane_count == 2 {
        anyhow::bail!("all chunk retrieval lanes failed: {}", failures.join("; "));
    }

    // A degraded lane whose surviving partner returned nothing leaves the turn
    // with no chunk evidence at all, which is the same evidential position as
    // both lanes failing. Reporting it as success is not harmless: the answer
    // stage still injects the document-title inventory, so the model writes a
    // confident reply sourced from titles alone, every reference array comes
    // back empty, and verification passes vacuously because it has nothing to
    // check. An empty bundle is only a legitimate answer when both lanes ran
    // clean and genuinely matched nothing.
    if degraded_lane_count > 0 && vector_hits.is_empty() && lexical_hits.is_empty() {
        anyhow::bail!("chunk retrieval degraded to no evidence: {}", failures.join("; "));
    }

    Ok(ChunkRetrievalLaneOutcome {
        vector_hits,
        vector_elapsed_ms,
        lexical_hits,
        lexical_query_count,
        lexical_elapsed_ms,
        degraded_lane_count,
    })
}

pub(super) fn retain_entity_bio_candidates(
    candidates: Vec<RuntimeMatchedChunk>,
    evidence_chunk_ids: &HashSet<Uuid>,
    label_tokens: &[String],
) -> Vec<RuntimeMatchedChunk> {
    candidates
        .into_iter()
        .filter(|chunk| {
            if evidence_chunk_ids.contains(&chunk.chunk_id) {
                return true;
            }
            let haystack = chunk.source_text.to_lowercase();
            label_tokens.iter().any(|token| haystack.contains(token))
        })
        .collect()
}

pub(super) async fn load_document_identity_chunks_for_targets(
    state: &AppState,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    targeted_document_ids: &BTreeSet<Uuid>,
    plan_keywords: &[String],
    query_ir: Option<&QueryIR>,
) -> anyhow::Result<Vec<RuntimeMatchedChunk>> {
    if targeted_document_ids.is_empty() {
        return Ok(Vec::new());
    }

    let focus_terms = document_identity_focus_terms(plan_keywords, query_ir);
    let mut chunks = Vec::new();
    for (document_rank, document_id) in targeted_document_ids.iter().enumerate() {
        let Some(document) = document_index.get(document_id) else {
            continue;
        };
        let Some(revision_id) = current_document_revision_id(document) else {
            continue;
        };
        let rows = state
            .document_store
            .list_chunks_by_revision_range(
                revision_id,
                0,
                DOCUMENT_IDENTITY_CHUNKS_PER_DOCUMENT.saturating_sub(1) as i32,
            )
            .await
            .with_context(|| {
                format!(
                    "failed to load document-identity chunks for document {} revision {}",
                    document_id, revision_id
                )
            })?;
        for (chunk_rank, row) in rows.into_iter().enumerate() {
            let score = document_identity_chunk_score(document_rank, chunk_rank);
            if let Some(mut chunk) = map_chunk_hit(row, score, document_index, plan_keywords) {
                chunk.score = Some(score);
                chunk.score_kind = RuntimeChunkScoreKind::DocumentIdentity;
                chunks.push(chunk);
            }
        }
        let focused_rows = state
            .document_store
            .list_chunks_by_revision_matching_terms(
                revision_id,
                &focus_terms,
                DOCUMENT_IDENTITY_FOCUSED_CHUNKS_PER_DOCUMENT,
            )
            .await
            .with_context(|| {
                format!(
                    "failed to load document-identity focus chunks for document {} revision {}",
                    document_id, revision_id
                )
            })?;
        for (chunk_rank, row) in focused_rows.into_iter().enumerate() {
            let score = document_identity_chunk_score(
                document_rank,
                DOCUMENT_IDENTITY_CHUNKS_PER_DOCUMENT.saturating_add(chunk_rank),
            );
            if let Some(mut chunk) = map_chunk_hit(row, score, document_index, &focus_terms) {
                chunk.score = Some(score);
                chunk.score_kind = RuntimeChunkScoreKind::DocumentIdentity;
                chunks.push(chunk);
            }
        }
    }
    Ok(chunks)
}

pub(super) fn document_identity_chunk_score(document_rank: usize, chunk_rank: usize) -> f32 {
    DOCUMENT_IDENTITY_SCORE_FLOOR + 10_000.0 - document_rank as f32 * 100.0 - chunk_rank as f32
}

pub(super) fn versioned_update_procedure_chunk_score(
    document_rank: usize,
    chunk_rank: usize,
) -> f32 {
    VERSIONED_UPDATE_PROCEDURE_SCORE_BASE + 10_000.0
        - document_rank as f32 * 100.0
        - chunk_rank as f32
}

pub(super) fn push_document_identity_focus_term(
    value: &str,
    seen: &mut BTreeSet<String>,
    terms: &mut Vec<String>,
) {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() {
        return;
    }
    if seen.insert(normalized.to_lowercase()) {
        terms.push(normalized.clone());
    }
    for token in normalized_alnum_tokens(&normalized, 3) {
        if seen.insert(token.clone()) {
            terms.push(token.clone());
        }
        if token.chars().count() > DOCUMENT_IDENTITY_FOCUS_PREFIX_CHARS {
            let prefix =
                token.chars().take(DOCUMENT_IDENTITY_FOCUS_PREFIX_CHARS).collect::<String>();
            if seen.insert(prefix.clone()) {
                terms.push(prefix);
            }
        }
    }
}

pub(super) fn document_identity_focus_terms(
    plan_keywords: &[String],
    query_ir: Option<&QueryIR>,
) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut terms = Vec::new();
    for keyword in plan_keywords {
        push_document_identity_focus_term(keyword, &mut seen, &mut terms);
    }
    if let Some(query_ir) = query_ir {
        let values = query_ir
            .document_focus
            .as_ref()
            .map(|focus| focus.hint.as_str())
            .into_iter()
            .chain(query_ir.target_entities.iter().map(|entity| entity.label.as_str()))
            .chain(query_ir.literal_constraints.iter().map(|literal| literal.text.as_str()));
        for value in values {
            push_document_identity_focus_term(value, &mut seen, &mut terms);
        }
    }
    terms
}
