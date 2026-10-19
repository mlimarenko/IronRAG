use crate::infra::knowledge_plane::VectorSimilaritySearchInput;
use crate::interfaces::http::router_support::current_embedding_search_guard;

use super::{candidates::*, ranking::*, *};

/// Raw (unresolved) search limits from the request. Kept as one value so the
/// handler passes a single cohesive argument.
pub(super) struct KnowledgeSearchLimits {
    pub(super) limit: Option<usize>,
    pub(super) chunk_hit_limit_per_document: Option<usize>,
    pub(super) evidence_sample_limit: Option<usize>,
}

/// Defaults applied once, so every later stage reads the same resolved numbers.
struct ResolvedSearchLimits {
    limit: usize,
    chunk_hit_limit_per_document: usize,
    evidence_sample_limit: usize,
}

/// The independent candidate sets gathered before document loading.
struct HybridSearchCandidates {
    lexical_chunk_hits: Vec<KnowledgeChunkSearchRow>,
    vector_chunk_hits: Vec<KnowledgeChunkVectorSearchRow>,
    lexical_entity_hits: Vec<KnowledgeEntitySearchRow>,
    lexical_relation_hits: Vec<KnowledgeRelationSearchRow>,
    vector_entity_hits: Vec<KnowledgeEntityVectorSearchRow>,
    hybrid_context: Option<KnowledgeHybridSearchContext>,
}

/// The readable rows behind every candidate chunk, used both to drop stale hits
/// and to accumulate the document hits.
struct SearchableChunkScope {
    chunk_map: HashMap<Uuid, KnowledgeChunkRow>,
    revision_map: HashMap<Uuid, KnowledgeRevisionRow>,
    document_map: HashMap<Uuid, KnowledgeDocumentRow>,
}

pub(super) async fn search_documents_impl(
    state: AppState,
    library_id: Uuid,
    query: Option<String>,
    limits: KnowledgeSearchLimits,
) -> Result<Json<KnowledgeDocumentSearchResponse>, ApiError> {
    let query_text = query.unwrap_or_default().trim().to_string();
    if query_text.is_empty() {
        return Err(ApiError::BadRequest("query must not be empty".to_string()));
    }
    let limits = ResolvedSearchLimits {
        limit: limits.limit.unwrap_or(DEFAULT_SEARCH_LIMIT).max(1),
        chunk_hit_limit_per_document: limits.chunk_hit_limit_per_document.unwrap_or(10).max(1),
        evidence_sample_limit: resolved_evidence_sample_limit(limits.evidence_sample_limit),
    };
    let query_keywords = document_search_keywords(&query_text);

    let mut candidates =
        collect_hybrid_candidates(&state, library_id, &query_text, &limits).await?;
    let scope = load_searchable_chunk_scope(&state, library_id, &mut candidates).await?;
    let document_hits = accumulate_document_hits(&candidates, &scope, &query_keywords, &query_text);
    let prepared = prepare_search_candidates(
        state.clone(),
        &query_text,
        &query_keywords,
        &limits,
        document_hits,
    )
    .await?;
    let response_hits = enrich_search_candidates(state, &query_text, prepared).await?;

    Ok(Json(KnowledgeDocumentSearchResponse {
        library_id,
        query_text,
        limit: limits.limit,
        embedding_provider_kind: candidates
            .hybrid_context
            .as_ref()
            .map_or_else(|| "lexical_only".to_string(), |context| context.provider_kind.clone()),
        embedding_model_name: candidates
            .hybrid_context
            .as_ref()
            .map(|context| context.model_name.clone())
            .unwrap_or_default(),
        embedding_model_catalog_id: candidates
            .hybrid_context
            .as_ref()
            .map_or_else(Uuid::nil, |context| context.model_catalog_id),
        freshness_generation: candidates
            .hybrid_context
            .as_ref()
            .map(|context| context.freshness_generation)
            .unwrap_or_default(),
        document_hits: response_hits,
        entity_hits: candidates.lexical_entity_hits.into_iter().map(Into::into).collect(),
        relation_hits: candidates.lexical_relation_hits.into_iter().map(Into::into).collect(),
        vector_chunk_hits: candidates.vector_chunk_hits.into_iter().map(Into::into).collect(),
        vector_entity_hits: candidates.vector_entity_hits.into_iter().map(Into::into).collect(),
    }))
}

/// Runs the four lexical/graph candidate searches and, when a hybrid context is
/// resolvable, the two vector searches. Keeps the vector-plane read guard alive
/// across both vector calls exactly as the single-function version did.
async fn collect_hybrid_candidates(
    state: &AppState,
    library_id: Uuid,
    query_text: &str,
    limits: &ResolvedSearchLimits,
) -> Result<HybridSearchCandidates, ApiError> {
    let internal_candidate_limit = limits
        .limit
        .saturating_mul(limits.chunk_hit_limit_per_document.max(3))
        .saturating_mul(4)
        .max(16);
    let (vector_chunk_limit, vector_entity_limit) =
        document_search_vector_limits(limits.limit, limits.chunk_hit_limit_per_document);
    let (lexical_chunk_hits, lexical_entity_hits, lexical_relation_hits, hybrid_context) = tokio::try_join!(
        search_expanded_lexical_chunks(state, library_id, query_text, internal_candidate_limit),
        search_entities_by_library(state, library_id, query_text, limits.limit),
        search_relations_by_library(state, library_id, query_text, limits.limit),
        resolve_hybrid_search_context(state, library_id, query_text),
    )?;
    let (vector_chunk_hits, vector_entity_hits) = collect_vector_hits(
        state,
        library_id,
        vector_chunk_limit,
        vector_entity_limit,
        hybrid_context.as_ref(),
    )
    .await?;
    Ok(HybridSearchCandidates {
        lexical_chunk_hits,
        vector_chunk_hits,
        lexical_entity_hits,
        lexical_relation_hits,
        vector_entity_hits,
        hybrid_context,
    })
}

async fn collect_vector_hits(
    state: &AppState,
    library_id: Uuid,
    vector_chunk_limit: usize,
    vector_entity_limit: usize,
    hybrid_context: Option<&KnowledgeHybridSearchContext>,
) -> Result<(Vec<KnowledgeChunkVectorSearchRow>, Vec<KnowledgeEntityVectorSearchRow>), ApiError> {
    let Some(context) = hybrid_context else {
        return Ok((Vec::new(), Vec::new()));
    };
    let _vector_guard = current_embedding_search_guard(
        state,
        library_id,
        &context.embedding_profile_key,
        context.inventory_version,
    )
    .await?;
    validate_embedding_vector_dimensions(
        context.dimensions,
        &context.query_vector,
        "knowledge hybrid search query",
    )
    .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
    let vector_chunk_future = state.search_store.search_chunk_vectors_by_similarity(
        VectorSimilaritySearchInput {
            dim: context.dimensions,
            library_id,
            embedding_model_key: &context.embedding_profile_key,
            query_vector: &context.query_vector,
            n_probe: None,
        },
        vector_chunk_limit,
        None,
        None,
    );
    let vector_entity_future = state.search_store.search_entity_vectors_by_similarity(
        VectorSimilaritySearchInput {
            dim: context.dimensions,
            library_id,
            embedding_model_key: &context.embedding_profile_key,
            query_vector: &context.query_vector,
            n_probe: None,
        },
        vector_entity_limit,
    );
    let (chunk_result, entity_result) = tokio::join!(vector_chunk_future, vector_entity_future);
    let chunk_rows =
        log_vector_search_failure(chunk_result, library_id, context.model_catalog_id, "chunk");
    let entity_rows =
        log_vector_search_failure(entity_result, library_id, context.model_catalog_id, "entity");
    Ok((chunk_rows, entity_rows))
}

/// Loads the readable chunk/revision/document rows behind every candidate and
/// drops candidates whose rows are gone. Returns the maps the accumulator needs.
async fn load_searchable_chunk_scope(
    state: &AppState,
    library_id: Uuid,
    candidates: &mut HybridSearchCandidates,
) -> Result<SearchableChunkScope, ApiError> {
    let chunk_ids: Vec<Uuid> = candidates
        .lexical_chunk_hits
        .iter()
        .map(|hit| hit.chunk_id)
        .chain(candidates.vector_chunk_hits.iter().map(|hit| hit.chunk_id))
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    let chunks = load_chunks_by_ids(state, &chunk_ids).await?;
    let chunk_map: HashMap<Uuid, KnowledgeChunkRow> =
        chunks.into_iter().map(|chunk| (chunk.chunk_id, chunk)).collect();
    if chunk_map.len() < chunk_ids.len() {
        log_missing_chunks(library_id, chunk_ids.len(), chunk_map.len());
    }

    let revision_ids: Vec<Uuid> = chunk_map
        .values()
        .map(|chunk| chunk.revision_id)
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    let document_ids: Vec<Uuid> = chunk_map
        .values()
        .map(|chunk| chunk.document_id)
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    let (revisions, documents) = tokio::try_join!(
        load_revisions_by_ids(state, &revision_ids),
        load_documents_by_ids(state, &document_ids),
    )?;
    let revision_map: HashMap<Uuid, KnowledgeRevisionRow> =
        revisions.into_iter().map(|revision| (revision.revision_id, revision)).collect();
    let document_map: HashMap<Uuid, KnowledgeDocumentRow> =
        documents.into_iter().map(|document| (document.document_id, document)).collect();
    if revision_map.len() < revision_ids.len() {
        log_missing_revisions(library_id, revision_ids.len(), revision_map.len());
    }
    if document_map.len() < document_ids.len() {
        log_missing_documents(library_id, document_ids.len(), document_map.len());
    }

    candidates
        .lexical_chunk_hits
        .retain(|hit| resolved_chunk_hit(&hit.chunk_id, &chunk_map, &revision_map, &document_map));
    candidates
        .vector_chunk_hits
        .retain(|hit| resolved_chunk_hit(&hit.chunk_id, &chunk_map, &revision_map, &document_map));
    Ok(SearchableChunkScope { chunk_map, revision_map, document_map })
}

/// Builds one document accumulator per document from the lexical and vector
/// chunk hits, then rescores and ranks them.
fn accumulate_document_hits(
    candidates: &HybridSearchCandidates,
    scope: &SearchableChunkScope,
    query_keywords: &[String],
    query_text: &str,
) -> Vec<KnowledgeDocumentAccumulator> {
    let mut accumulators: HashMap<Uuid, KnowledgeDocumentAccumulator> = HashMap::new();
    accumulate_lexical_chunk_hits(&mut accumulators, &candidates.lexical_chunk_hits, scope);
    accumulate_vector_chunk_hits(&mut accumulators, &candidates.vector_chunk_hits, scope);

    let mut document_hits: Vec<KnowledgeDocumentAccumulator> = accumulators
        .into_values()
        .map(|mut accumulator| {
            accumulator.chunk_hits.sort_by(|left, right| {
                document_chunk_keyword_coverage(right, query_keywords)
                    .cmp(&document_chunk_keyword_coverage(left, query_keywords))
                    .then_with(|| {
                        right.score.partial_cmp(&left.score).unwrap_or(std::cmp::Ordering::Equal)
                    })
                    .then_with(|| left.chunk_id.cmp(&right.chunk_id))
            });
            accumulator.vector_chunk_hits.sort_by(|left, right| {
                right
                    .score
                    .partial_cmp(&left.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| left.chunk_id.cmp(&right.chunk_id))
            });
            accumulator
        })
        .collect();

    for accumulator in &mut document_hits {
        rescore_document_accumulator(accumulator, query_keywords, query_text);
    }
    sort_document_hits_by_relevance(&mut document_hits);
    document_hits
}

fn accumulate_lexical_chunk_hits(
    accumulators: &mut HashMap<Uuid, KnowledgeDocumentAccumulator>,
    hits: &[KnowledgeChunkSearchRow],
    scope: &SearchableChunkScope,
) {
    for (rank, hit) in hits.iter().enumerate() {
        let Some(accumulator) = scope.document_accumulator_for_chunk(accumulators, hit.chunk_id)
        else {
            continue;
        };
        accumulator.lexical_rank =
            Some(accumulator.lexical_rank.map_or(rank + 1, |current| current.min(rank + 1)));
        accumulator.lexical_score =
            Some(accumulator.lexical_score.map_or(hit.score, |current| current.max(hit.score)));
        accumulator.exact_identifier_rarity =
            accumulator.exact_identifier_rarity.max(hit.exact_identifier_rarity);
        extend_unique_identifier_terms(
            &mut accumulator.exact_identifier_terms,
            &hit.exact_identifier_terms,
        );
        extend_unique_identifier_terms(
            &mut accumulator.rare_exact_identifier_terms,
            &hit.rare_exact_identifier_terms,
        );
        accumulator.chunk_hits.push(sanitize_chunk_search_hit(hit));
    }
}

fn accumulate_vector_chunk_hits(
    accumulators: &mut HashMap<Uuid, KnowledgeDocumentAccumulator>,
    hits: &[KnowledgeChunkVectorSearchRow],
    scope: &SearchableChunkScope,
) {
    for (rank, hit) in hits.iter().enumerate() {
        let Some(accumulator) = scope.document_accumulator_for_chunk(accumulators, hit.chunk_id)
        else {
            continue;
        };
        accumulator.vector_rank =
            Some(accumulator.vector_rank.map_or(rank + 1, |current| current.min(rank + 1)));
        accumulator.vector_score =
            Some(accumulator.vector_score.map_or(hit.score, |current| current.max(hit.score)));
        accumulator.vector_chunk_hits.push(hit.clone());
    }
}

impl SearchableChunkScope {
    fn document_accumulator_for_chunk<'a>(
        &self,
        accumulators: &'a mut HashMap<Uuid, KnowledgeDocumentAccumulator>,
        chunk_id: Uuid,
    ) -> Option<&'a mut KnowledgeDocumentAccumulator> {
        let chunk = self.chunk_map.get(&chunk_id)?;
        let revision = self.revision_map.get(&chunk.revision_id)?;
        let document = self.document_map.get(&chunk.document_id)?;
        Some(accumulators.entry(document.document_id).or_insert_with(|| {
            KnowledgeDocumentAccumulator::new(document.clone(), revision.clone())
        }))
    }
}

/// Backfills and enriches the top candidates in bounded parallel batches, then
/// re-sorts and truncates to the requested page size.
async fn prepare_search_candidates(
    state: AppState,
    query_text: &str,
    query_keywords: &[String],
    limits: &ResolvedSearchLimits,
    mut document_hits: Vec<KnowledgeDocumentAccumulator>,
) -> Result<Vec<KnowledgeDocumentAccumulator>, ApiError> {
    let backfill_candidate_limit = limits
        .limit
        .max(limits.limit.saturating_mul(4).min(SEARCH_DOCUMENT_BACKFILL_MAX_CANDIDATES))
        .min(document_hits.len());
    document_hits.truncate(backfill_candidate_limit);
    let backfill_parallelism =
        SEARCH_DOCUMENT_ENRICHMENT_PARALLELISM.min(document_hits.len()).max(1);
    let chunk_hit_limit_per_document = limits.chunk_hit_limit_per_document;
    let evidence_sample_limit = limits.evidence_sample_limit;
    let prepared_results = stream::iter(document_hits.into_iter().map(|accumulator| {
        let state = state.clone();
        let query_text = query_text.to_string();
        let query_keywords = query_keywords.to_vec();
        async move {
            prepare_document_search_candidate(
                state,
                query_text,
                query_keywords,
                chunk_hit_limit_per_document,
                evidence_sample_limit,
                accumulator,
            )
            .await
        }
    }))
    .buffer_unordered(backfill_parallelism)
    .collect::<Vec<_>>()
    .await;
    let mut prepared = Vec::with_capacity(prepared_results.len());
    for result in prepared_results {
        prepared.push(result?);
    }
    sort_document_hits_by_relevance(&mut prepared);
    prepared.truncate(limits.limit);
    Ok(prepared)
}

/// Enriches each prepared candidate in bounded parallel batches and restores
/// relevance order by the index assigned before enrichment.
async fn enrich_search_candidates(
    state: AppState,
    query_text: &str,
    document_hits: Vec<KnowledgeDocumentAccumulator>,
) -> Result<Vec<KnowledgeSearchDocumentHit>, ApiError> {
    let enrichment_parallelism =
        SEARCH_DOCUMENT_ENRICHMENT_PARALLELISM.min(document_hits.len()).max(1);
    let enriched_results =
        stream::iter(document_hits.into_iter().enumerate().map(|(index, accumulator)| {
            let state = state.clone();
            let query_text = query_text.to_string();
            async move { enrich_document_search_hit(state, query_text, index, accumulator).await }
        }))
        .buffer_unordered(enrichment_parallelism)
        .collect::<Vec<_>>()
        .await;
    let mut indexed_response_hits = Vec::with_capacity(enriched_results.len());
    for result in enriched_results {
        indexed_response_hits.push(result?);
    }
    indexed_response_hits.sort_by_key(|(index, _)| *index);
    Ok(indexed_response_hits.into_iter().map(|(_, hit)| hit).collect())
}

fn log_vector_search_failure<T>(
    result: anyhow::Result<Vec<T>>,
    library_id: Uuid,
    model_catalog_id: Uuid,
    plane: &str,
) -> Vec<T> {
    match result {
        Ok(rows) => rows,
        Err(error) => {
            warn!(
                library_id = %library_id,
                model_catalog_id = %model_catalog_id,
                error = ?error,
                "hybrid knowledge {} vector search failed; returning lexical {} hits only",
                plane,
                plane,
            );
            Vec::new()
        }
    }
}

fn log_missing_chunks(library_id: Uuid, requested: usize, loaded: usize) {
    warn!(
        library_id = %library_id,
        requested_chunk_count = requested,
        loaded_chunk_count = loaded,
        "knowledge document search ignored stale chunk hits missing from the current chunk store",
    );
}

fn log_missing_revisions(library_id: Uuid, requested: usize, loaded: usize) {
    warn!(
        library_id = %library_id,
        requested_revision_count = requested,
        loaded_revision_count = loaded,
        "knowledge document search ignored chunk hits whose revisions are no longer readable",
    );
}

fn log_missing_documents(library_id: Uuid, requested: usize, loaded: usize) {
    warn!(
        library_id = %library_id,
        requested_document_count = requested,
        loaded_document_count = loaded,
        "knowledge document search ignored chunk hits whose documents are no longer readable",
    );
}

async fn resolve_hybrid_search_context(
    state: &AppState,
    library_id: Uuid,
    query_text: &str,
) -> Result<Option<KnowledgeHybridSearchContext>, ApiError> {
    let _vector_guard = state
        .application_services
        .search
        .vector_plane_read_guard(state, library_id)
        .await
        .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
    let inventory_version = load_embedding_profile_inventory_version(state, library_id)
        .await
        .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
    let binding = state
        .application_services
        .ai_catalog
        .resolve_active_runtime_binding(state, library_id, AiBindingPurpose::EmbedChunk)
        .await?
        .ok_or_else(|| {
            map_runtime_lifecycle_error(anyhow::Error::new(QueryServiceError::StateConflict {
                message: format!(
                    "active embed_chunk binding is unavailable while proving the exact vector inventory for library {library_id}; configure the binding and rebuild before querying"
                ),
            }))
        })?;

    let embedding_profile_key = binding.embedding_execution_profile_key();
    let index_state = ensure_library_embedding_profile_indexed(
        state,
        library_id,
        &embedding_profile_key,
        inventory_version,
    )
    .await
    .map_err(map_runtime_lifecycle_error)?;
    let dimensions = match index_state {
        EmbeddingProfileIndexState::Empty => {
            ensure_embedding_profile_inventory_version_current(
                state,
                library_id,
                inventory_version,
            )
            .await
            .map_err(map_runtime_lifecycle_error)?;
            return Ok(None);
        }
        EmbeddingProfileIndexState::Ready { dimensions } => dimensions,
    };
    drop(_vector_guard);

    let embedding = state
        .llm_gateway
        .embed(EmbeddingRequest {
            provider_kind: binding.provider_kind.clone(),
            model_name: binding.model_name.clone(),
            input: query_text.to_string(),
            api_key_override: binding.api_key.clone(),
            base_url_override: binding.provider_base_url.clone(),
            extra_parameters_json: binding.extra_parameters_json.clone(),
        })
        .await
        .map_err(|error| {
            ApiError::ProviderFailure(format!("failed to embed knowledge search query: {error}"))
        })?;
    ensure_active_embedding_profile_key(state, library_id, &embedding_profile_key)
        .await
        .map_err(map_runtime_lifecycle_error)?;
    ensure_embedding_profile_inventory_version_current(state, library_id, inventory_version)
        .await
        .map_err(map_runtime_lifecycle_error)?;
    validate_embedding_vector_dimensions(
        dimensions,
        &embedding.embedding,
        "knowledge hybrid search query",
    )
    .map_err(|error| ApiError::internal_with_log(error, "internal"))?;

    Ok(Some(KnowledgeHybridSearchContext {
        provider_kind: binding.provider_kind.clone(),
        model_name: binding.model_name.clone(),
        model_catalog_id: binding.model_catalog_id,
        embedding_profile_key,
        freshness_generation: inventory_version.active_vector_generation,
        inventory_version,
        dimensions,
        query_vector: embedding.embedding,
    }))
}

async fn load_revisions_by_ids(
    state: &AppState,
    revision_ids: &[Uuid],
) -> Result<Vec<KnowledgeRevisionRow>, ApiError> {
    if revision_ids.is_empty() {
        return Ok(Vec::new());
    }
    state
        .document_store
        .list_revisions_by_ids(revision_ids)
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))
}

async fn load_documents_by_ids(
    state: &AppState,
    document_ids: &[Uuid],
) -> Result<Vec<KnowledgeDocumentRow>, ApiError> {
    if document_ids.is_empty() {
        return Ok(Vec::new());
    }
    state
        .document_store
        .list_documents_by_ids(document_ids)
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))
}

async fn search_entities_by_library(
    state: &AppState,
    library_id: Uuid,
    query_text: &str,
    limit: usize,
) -> Result<Vec<KnowledgeEntitySearchRow>, ApiError> {
    state
        .search_store
        .search_entities(library_id, query_text, limit.max(1))
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))
}

async fn search_relations_by_library(
    state: &AppState,
    library_id: Uuid,
    query_text: &str,
    limit: usize,
) -> Result<Vec<KnowledgeRelationSearchRow>, ApiError> {
    state
        .search_store
        .search_relations(library_id, query_text, limit.max(1))
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::log_vector_search_failure;

    #[test]
    fn vector_search_failure_passes_rows_and_falls_back_to_empty() {
        let rows: Vec<u8> =
            log_vector_search_failure(Ok(vec![1, 2]), Uuid::nil(), Uuid::nil(), "chunk");
        assert_eq!(rows, vec![1, 2]);

        let empty: Vec<u8> = log_vector_search_failure(
            Err(anyhow::anyhow!("vector plane unavailable")),
            Uuid::nil(),
            Uuid::nil(),
            "chunk",
        );
        assert!(empty.is_empty());
    }
}
