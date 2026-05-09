use std::collections::{HashMap, HashSet};

use axum::{
    Json,
    extract::{Path, Query, State},
};
use futures::stream::{self, StreamExt};
use serde::{Deserialize, Serialize};
use tracing::warn;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::ai::AiBindingPurpose,
    infra::arangodb::{
        document_store::{
            KnowledgeChunkRow, KnowledgeDocumentRow, KnowledgeLibraryGenerationRow,
            KnowledgeRevisionRow,
        },
        graph_store::KnowledgeEvidenceRow,
        search_store::{
            KnowledgeChunkSearchRow, KnowledgeChunkVectorSearchRow, KnowledgeEntitySearchRow,
            KnowledgeEntityVectorSearchRow, KnowledgeRelationSearchRow,
        },
    },
    integrations::llm::EmbeddingRequest,
    interfaces::http::{
        auth::AuthContext,
        authorization::{POLICY_KNOWLEDGE_READ, load_library_and_authorize},
        router_support::ApiError,
    },
    shared::{
        extraction::text_render::repair_technical_layout_noise,
        text_tokens::normalized_alnum_token_sequence_by,
    },
};

use super::{
    KnowledgeDocumentProvenanceSummary, KnowledgeGraphEvidenceSummary,
    KnowledgeTechnicalFactProvenanceSummary, load_chunks_by_ids, summarize_graph_evidence,
    summarize_typed_technical_facts,
};

const DEFAULT_SEARCH_LIMIT: usize = 10;
const DEFAULT_EVIDENCE_SAMPLE_LIMIT: usize = 5;
const SEARCH_LEXICAL_QUERY_PARALLELISM: usize = 4;
const SEARCH_CHUNK_EVIDENCE_PARALLELISM: usize = 4;
const SEARCH_DOCUMENT_ENRICHMENT_PARALLELISM: usize = 4;
const DOCUMENT_KEYWORD_COVERAGE_BONUS_WEIGHT: f64 = 0.8;
const DOCUMENT_KEYWORD_AGGREGATE_COVERAGE_BONUS_WEIGHT: f64 = 0.25;

#[derive(Debug, Deserialize, utoipa::ToSchema, utoipa::IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct KnowledgeDocumentSearchQuery {
    #[serde(alias = "q")]
    query: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    chunk_hit_limit_per_document: Option<usize>,
    #[serde(default)]
    evidence_sample_limit: Option<usize>,
}

#[derive(Debug, Deserialize, utoipa::ToSchema, utoipa::IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct KnowledgeDocumentSearchRequest {
    library_id: Uuid,
    #[serde(alias = "q")]
    query: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    chunk_hit_limit_per_document: Option<usize>,
    #[serde(default)]
    evidence_sample_limit: Option<usize>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct KnowledgeSearchRevisionSummary {
    revision_id: Uuid,
    document_id: Uuid,
    revision_number: i64,
    revision_state: String,
    revision_kind: String,
    mime_type: String,
    title: Option<String>,
    byte_size: i64,
    text_state: String,
    vector_state: String,
    graph_state: String,
    created_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct KnowledgeSearchDocumentHit {
    document: KnowledgeDocumentRow,
    revision: KnowledgeSearchRevisionSummary,
    score: f64,
    lexical_rank: Option<usize>,
    vector_rank: Option<usize>,
    lexical_score: Option<f64>,
    vector_score: Option<f64>,
    chunk_hits: Vec<KnowledgeChunkSearchRow>,
    vector_chunk_hits: Vec<KnowledgeChunkVectorSearchRow>,
    evidence_samples: Vec<KnowledgeEvidenceRow>,
    technical_fact_samples: Vec<crate::domains::knowledge::TypedTechnicalFact>,
    provenance_summary: KnowledgeDocumentProvenanceSummary,
    technical_fact_summary: KnowledgeTechnicalFactProvenanceSummary,
    graph_evidence_summary: KnowledgeGraphEvidenceSummary,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct KnowledgeDocumentSearchResponse {
    library_id: Uuid,
    query_text: String,
    limit: usize,
    embedding_provider_kind: String,
    embedding_model_name: String,
    embedding_model_catalog_id: Uuid,
    freshness_generation: i64,
    document_hits: Vec<KnowledgeSearchDocumentHit>,
    entity_hits: Vec<KnowledgeEntitySearchRow>,
    relation_hits: Vec<KnowledgeRelationSearchRow>,
    vector_chunk_hits: Vec<KnowledgeChunkVectorSearchRow>,
    vector_entity_hits: Vec<KnowledgeEntityVectorSearchRow>,
}

#[derive(Debug, Clone)]
struct KnowledgeDocumentAccumulator {
    document: KnowledgeDocumentRow,
    revision: KnowledgeRevisionRow,
    score: f64,
    lexical_rank: Option<usize>,
    vector_rank: Option<usize>,
    lexical_score: Option<f64>,
    vector_score: Option<f64>,
    chunk_hits: Vec<KnowledgeChunkSearchRow>,
    vector_chunk_hits: Vec<KnowledgeChunkVectorSearchRow>,
    evidence_samples: Vec<KnowledgeEvidenceRow>,
    evidence_ids: HashSet<Uuid>,
}

#[derive(Debug, Clone)]
struct KnowledgeHybridSearchContext {
    provider_kind: String,
    model_name: String,
    model_catalog_id: Uuid,
    freshness_generation: i64,
    query_vector: Vec<f32>,
}

fn sanitize_chunk_search_hit(hit: &KnowledgeChunkSearchRow) -> KnowledgeChunkSearchRow {
    let mut sanitized = hit.clone();
    sanitized.content_text = repair_technical_layout_noise(&sanitized.content_text);
    sanitized.normalized_text = repair_technical_layout_noise(&sanitized.normalized_text);
    sanitized.content_text = normalize_search_hit_text(&sanitized.content_text);
    sanitized.normalized_text = normalize_search_hit_text(&sanitized.normalized_text);
    sanitized
}

fn normalize_search_hit_text(text: &str) -> String {
    text.to_string()
}

fn document_search_keywords(query_text: &str) -> Vec<String> {
    crate::services::query::planner::extract_keywords(query_text)
}

fn document_chunk_keyword_coverage(hit: &KnowledgeChunkSearchRow, keywords: &[String]) -> usize {
    if keywords.is_empty() {
        return 0;
    }
    let haystack = format!("{}\n{}", hit.content_text, hit.normalized_text).to_lowercase();
    keywords.iter().filter(|keyword| haystack.contains(keyword.as_str())).count()
}

fn document_keyword_coverage_bonus(
    chunk_hits: &[KnowledgeChunkSearchRow],
    keywords: &[String],
) -> f64 {
    if chunk_hits.is_empty() || keywords.is_empty() {
        return 0.0;
    }
    let denominator = keywords.len() as f64;
    let best_chunk_coverage = chunk_hits
        .iter()
        .map(|hit| document_chunk_keyword_coverage(hit, keywords))
        .max()
        .unwrap_or_default() as f64;
    let aggregate_haystack = chunk_hits
        .iter()
        .take(3)
        .map(|hit| format!("{}\n{}", hit.content_text, hit.normalized_text))
        .collect::<Vec<_>>()
        .join("\n")
        .to_lowercase();
    let aggregate_coverage =
        keywords.iter().filter(|keyword| aggregate_haystack.contains(keyword.as_str())).count()
            as f64;
    let best_signal = (best_chunk_coverage / denominator).min(1.0);
    let aggregate_signal = (aggregate_coverage / denominator).min(1.0);
    (best_signal * DOCUMENT_KEYWORD_COVERAGE_BONUS_WEIGHT)
        + (aggregate_signal * DOCUMENT_KEYWORD_AGGREGATE_COVERAGE_BONUS_WEIGHT)
}

fn resolved_evidence_sample_limit(value: Option<usize>) -> usize {
    value.unwrap_or(DEFAULT_EVIDENCE_SAMPLE_LIMIT)
}

fn document_search_vector_limits(
    limit: usize,
    chunk_hit_limit_per_document: usize,
) -> (usize, usize) {
    let chunk_limit =
        limit.saturating_mul(chunk_hit_limit_per_document).max(limit.saturating_mul(2)).max(1);
    let entity_limit = limit.max(1);
    (chunk_limit, entity_limit)
}

fn expand_document_search_queries(query_text: &str) -> Vec<String> {
    let mut queries = Vec::<String>::new();
    let mut seen = HashSet::<String>::new();
    let mut push_query = |value: &str| {
        let normalized = value.trim();
        if normalized.is_empty() {
            return;
        }
        let dedupe_key = normalized.to_lowercase();
        if seen.insert(dedupe_key) {
            queries.push(normalized.to_string());
        }
    };

    push_query(query_text);
    for phrase in document_search_anchor_phrases(query_text).into_iter().take(6) {
        push_query(&phrase);
    }

    queries
}

#[tracing::instrument(
    level = "info",
    name = "http.search_documents",
    skip_all,
    fields(library_id = %library_id, elapsed_ms)
)]
#[utoipa::path(
    get,
    path = "/v1/knowledge/libraries/{libraryId}/search/documents",
    tag = "search",
    operation_id = "searchKnowledgeDocuments",
    params(
        ("libraryId" = uuid::Uuid, Path, description = "Library identifier"),
        KnowledgeDocumentSearchQuery,
    ),
    responses(
        (status = 200, description = "Hybrid lexical + vector document search results", body = KnowledgeDocumentSearchResponse),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not authorized for the library"),
    ),
)]
pub async fn search_documents(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(library_id): Path<Uuid>,
    Query(query): Query<KnowledgeDocumentSearchQuery>,
) -> Result<Json<KnowledgeDocumentSearchResponse>, ApiError> {
    let started_at = std::time::Instant::now();
    let result = search_documents_impl(
        auth,
        state,
        library_id,
        query.query,
        query.limit,
        query.chunk_hit_limit_per_document,
        query.evidence_sample_limit,
    )
    .await;
    tracing::Span::current().record("elapsed_ms", started_at.elapsed().as_millis() as u64);
    result
}

#[tracing::instrument(
    level = "info",
    name = "http.search_documents_by_library_query",
    skip_all,
    fields(library_id = %query.library_id, elapsed_ms)
)]
#[utoipa::path(
    get,
    path = "/v1/search/documents",
    tag = "search",
    operation_id = "searchDocuments",
    params(KnowledgeDocumentSearchRequest),
    responses(
        (status = 200, description = "Hybrid document search across the requested library", body = KnowledgeDocumentSearchResponse),
        (status = 400, description = "libraryId is required"),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not authorized for the library"),
    ),
)]
pub async fn search_documents_by_library_query(
    auth: AuthContext,
    State(state): State<AppState>,
    Query(query): Query<KnowledgeDocumentSearchRequest>,
) -> Result<Json<KnowledgeDocumentSearchResponse>, ApiError> {
    let started_at = std::time::Instant::now();
    let result = search_documents_impl(
        auth,
        state,
        query.library_id,
        query.query,
        query.limit,
        query.chunk_hit_limit_per_document,
        query.evidence_sample_limit,
    )
    .await;
    tracing::Span::current().record("elapsed_ms", started_at.elapsed().as_millis() as u64);
    result
}

async fn search_documents_impl(
    auth: AuthContext,
    state: AppState,
    library_id: Uuid,
    query: Option<String>,
    limit: Option<usize>,
    chunk_hit_limit_per_document: Option<usize>,
    evidence_sample_limit: Option<usize>,
) -> Result<Json<KnowledgeDocumentSearchResponse>, ApiError> {
    let _ = load_library_and_authorize(&auth, &state, library_id, POLICY_KNOWLEDGE_READ).await?;
    let query_text = query.unwrap_or_default().trim().to_string();
    if query_text.is_empty() {
        return Err(ApiError::BadRequest("query must not be empty".to_string()));
    }

    let limit = limit.unwrap_or(DEFAULT_SEARCH_LIMIT).max(1);
    let chunk_hit_limit_per_document = chunk_hit_limit_per_document.unwrap_or(10).max(1);
    let evidence_sample_limit = resolved_evidence_sample_limit(evidence_sample_limit);
    let query_keywords = document_search_keywords(&query_text);
    let internal_candidate_limit =
        limit.saturating_mul(chunk_hit_limit_per_document.max(3)).saturating_mul(4).max(16);
    let (vector_chunk_limit, vector_entity_limit) =
        document_search_vector_limits(limit, chunk_hit_limit_per_document);
    let (lexical_chunk_hits_result, lexical_entity_hits, lexical_relation_hits, hybrid_context) = tokio::try_join!(
        search_expanded_lexical_chunks(&state, library_id, &query_text, internal_candidate_limit),
        search_entities_by_library(&state, library_id, &query_text, limit),
        search_relations_by_library(&state, library_id, &query_text, limit),
        resolve_hybrid_search_context(&state, library_id, &query_text),
    )?;
    let mut lexical_chunk_hits = lexical_chunk_hits_result;

    let (mut vector_chunk_hits, vector_entity_hits) = if let Some(context) = hybrid_context.as_ref()
    {
        let embedding_model_key = context.model_catalog_id.to_string();
        let vector_chunk_future = state.arango_search_store.search_chunk_vectors_by_similarity(
            library_id,
            &embedding_model_key,
            &context.query_vector,
            vector_chunk_limit,
            None,
            None,
            None,
        );
        let vector_entity_future = state.arango_search_store.search_entity_vectors_by_similarity(
            library_id,
            &embedding_model_key,
            &context.query_vector,
            vector_entity_limit,
            None,
        );
        let (chunk_result, entity_result) = tokio::join!(vector_chunk_future, vector_entity_future);
        let chunk_rows = match chunk_result {
            Ok(rows) => rows,
            Err(error) => {
                warn!(
                    library_id = %library_id,
                    model_catalog_id = %context.model_catalog_id,
                    error = ?error,
                    "hybrid knowledge chunk vector search failed; returning lexical chunk hits only",
                );
                Vec::new()
            }
        };
        let entity_rows = match entity_result {
            Ok(rows) => rows,
            Err(error) => {
                warn!(
                    library_id = %library_id,
                    model_catalog_id = %context.model_catalog_id,
                    error = ?error,
                    "hybrid knowledge entity vector search failed; returning lexical entity hits only",
                );
                Vec::new()
            }
        };
        (chunk_rows, entity_rows)
    } else {
        (Vec::new(), Vec::new())
    };

    let chunk_ids: Vec<Uuid> = lexical_chunk_hits
        .iter()
        .map(|hit| hit.chunk_id)
        .chain(vector_chunk_hits.iter().map(|hit| hit.chunk_id))
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    let chunks = load_chunks_by_ids(&state, &chunk_ids).await?;
    let chunk_map: HashMap<Uuid, KnowledgeChunkRow> =
        chunks.into_iter().map(|chunk| (chunk.chunk_id, chunk)).collect();
    if chunk_map.len() < chunk_ids.len() {
        warn!(
            library_id = %library_id,
            requested_chunk_count = chunk_ids.len(),
            loaded_chunk_count = chunk_map.len(),
            "knowledge document search ignored stale chunk hits missing from the canonical chunk store",
        );
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
        load_revisions_by_ids(&state, &revision_ids),
        load_documents_by_ids(&state, &document_ids),
    )?;
    let revision_map: HashMap<Uuid, KnowledgeRevisionRow> =
        revisions.into_iter().map(|revision| (revision.revision_id, revision)).collect();
    let document_map: HashMap<Uuid, KnowledgeDocumentRow> =
        documents.into_iter().map(|document| (document.document_id, document)).collect();
    if revision_map.len() < revision_ids.len() {
        warn!(
            library_id = %library_id,
            requested_revision_count = revision_ids.len(),
            loaded_revision_count = revision_map.len(),
            "knowledge document search ignored chunk hits whose revisions are no longer readable",
        );
    }
    if document_map.len() < document_ids.len() {
        warn!(
            library_id = %library_id,
            requested_document_count = document_ids.len(),
            loaded_document_count = document_map.len(),
            "knowledge document search ignored chunk hits whose documents are no longer readable",
        );
    }

    lexical_chunk_hits
        .retain(|hit| resolved_chunk_hit(&hit.chunk_id, &chunk_map, &revision_map, &document_map));
    vector_chunk_hits
        .retain(|hit| resolved_chunk_hit(&hit.chunk_id, &chunk_map, &revision_map, &document_map));

    let mut accumulators: HashMap<Uuid, KnowledgeDocumentAccumulator> = HashMap::new();
    for (rank, hit) in lexical_chunk_hits.iter().enumerate() {
        let Some(chunk) = chunk_map.get(&hit.chunk_id) else {
            continue;
        };
        let Some(revision) = revision_map.get(&chunk.revision_id) else {
            continue;
        };
        let Some(document) = document_map.get(&chunk.document_id) else {
            continue;
        };
        let accumulator = accumulators.entry(document.document_id).or_insert_with(|| {
            KnowledgeDocumentAccumulator {
                document: document.clone(),
                revision: revision.clone(),
                score: 0.0,
                lexical_rank: None,
                vector_rank: None,
                lexical_score: None,
                vector_score: None,
                chunk_hits: Vec::new(),
                vector_chunk_hits: Vec::new(),
                evidence_samples: Vec::new(),
                evidence_ids: HashSet::new(),
            }
        });
        accumulator.lexical_rank =
            Some(accumulator.lexical_rank.map_or(rank + 1, |current| current.min(rank + 1)));
        accumulator.lexical_score =
            Some(accumulator.lexical_score.map_or(hit.score, |current| current.max(hit.score)));
        accumulator.chunk_hits.push(sanitize_chunk_search_hit(hit));
    }

    for (rank, hit) in vector_chunk_hits.iter().enumerate() {
        let Some(chunk) = chunk_map.get(&hit.chunk_id) else {
            continue;
        };
        let Some(revision) = revision_map.get(&chunk.revision_id) else {
            continue;
        };
        let Some(document) = document_map.get(&chunk.document_id) else {
            continue;
        };
        let accumulator = accumulators.entry(document.document_id).or_insert_with(|| {
            KnowledgeDocumentAccumulator {
                document: document.clone(),
                revision: revision.clone(),
                score: 0.0,
                lexical_rank: None,
                vector_rank: None,
                lexical_score: None,
                vector_score: None,
                chunk_hits: Vec::new(),
                vector_chunk_hits: Vec::new(),
                evidence_samples: Vec::new(),
                evidence_ids: HashSet::new(),
            }
        });
        accumulator.vector_rank =
            Some(accumulator.vector_rank.map_or(rank + 1, |current| current.min(rank + 1)));
        accumulator.vector_score =
            Some(accumulator.vector_score.map_or(hit.score, |current| current.max(hit.score)));
        accumulator.vector_chunk_hits.push(hit.clone());
    }

    let mut document_hits: Vec<KnowledgeDocumentAccumulator> = accumulators
        .into_values()
        .map(|mut accumulator| {
            accumulator.chunk_hits.sort_by(|left, right| {
                document_chunk_keyword_coverage(right, &query_keywords)
                    .cmp(&document_chunk_keyword_coverage(left, &query_keywords))
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
        accumulator.chunk_hits.truncate(chunk_hit_limit_per_document);
        accumulator.vector_chunk_hits.truncate(chunk_hit_limit_per_document);
        let mut seen_evidence_chunks = HashSet::new();
        let candidate_chunk_ids: Vec<Uuid> = accumulator
            .chunk_hits
            .iter()
            .map(|hit| hit.chunk_id)
            .chain(accumulator.vector_chunk_hits.iter().map(|hit| hit.chunk_id))
            .collect();
        let mut deduped_chunk_ids = Vec::<Uuid>::new();
        for chunk_id in candidate_chunk_ids {
            if !seen_evidence_chunks.insert(chunk_id) {
                continue;
            }
            deduped_chunk_ids.push(chunk_id);
        }

        if evidence_sample_limit > 0 {
            let evidence_by_chunk = load_evidence_samples_by_chunk_ids(
                &state,
                accumulator.document.document_id,
                &deduped_chunk_ids,
            )
            .await?;
            for chunk_id in deduped_chunk_ids {
                let Some(evidence_rows) = evidence_by_chunk.get(&chunk_id) else {
                    continue;
                };
                for evidence in evidence_rows {
                    if accumulator.evidence_ids.insert(evidence.evidence_id) {
                        accumulator.evidence_samples.push(evidence.clone());
                    }
                    if accumulator.evidence_samples.len() >= evidence_sample_limit {
                        break;
                    }
                }
                if accumulator.evidence_samples.len() >= evidence_sample_limit {
                    break;
                }
            }
        }

        let lexical_rank = accumulator.lexical_rank.unwrap_or(usize::MAX / 2);
        let vector_rank = accumulator.vector_rank.unwrap_or(usize::MAX / 2);
        let lexical_signal = accumulator.lexical_score.map(f64::ln_1p).unwrap_or_default();
        let vector_signal = accumulator.vector_score.map(f64::ln_1p).unwrap_or_default();
        let provenance_bonus = (accumulator.evidence_samples.len() as f64) / 1000.0;
        let keyword_coverage_bonus =
            document_keyword_coverage_bonus(&accumulator.chunk_hits, &query_keywords);
        accumulator.score = lexical_signal
            + vector_signal
            + (1.0 / (60.0 + lexical_rank as f64))
            + (1.0 / (60.0 + vector_rank as f64))
            + provenance_bonus
            + keyword_coverage_bonus;
    }

    let mut document_hits = document_hits;
    document_hits.sort_by(|left, right| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.document.document_id.cmp(&right.document.document_id))
    });
    document_hits.truncate(limit);
    let enrichment_parallelism =
        SEARCH_DOCUMENT_ENRICHMENT_PARALLELISM.min(document_hits.len()).max(1);
    let enriched_results =
        stream::iter(document_hits.into_iter().enumerate().map(|(index, accumulator)| {
            let state = state.clone();
            let query_text = query_text.clone();
            async move {
                enrich_document_search_hit(
                    state,
                    query_text,
                    chunk_hit_limit_per_document,
                    index,
                    accumulator,
                )
                .await
            }
        }))
        .buffer_unordered(enrichment_parallelism)
        .collect::<Vec<_>>()
        .await;
    let mut indexed_response_hits = Vec::with_capacity(enriched_results.len());
    for result in enriched_results {
        indexed_response_hits.push(result?);
    }
    indexed_response_hits.sort_by_key(|(index, _)| *index);
    let response_hits = indexed_response_hits.into_iter().map(|(_, hit)| hit).collect::<Vec<_>>();

    Ok(Json(KnowledgeDocumentSearchResponse {
        library_id,
        query_text,
        limit,
        embedding_provider_kind: hybrid_context
            .as_ref()
            .map(|context| context.provider_kind.clone())
            .unwrap_or_else(|| "lexical_only".to_string()),
        embedding_model_name: hybrid_context
            .as_ref()
            .map(|context| context.model_name.clone())
            .unwrap_or_default(),
        embedding_model_catalog_id: hybrid_context
            .as_ref()
            .map(|context| context.model_catalog_id)
            .unwrap_or_else(Uuid::nil),
        freshness_generation: hybrid_context
            .as_ref()
            .map(|context| context.freshness_generation)
            .unwrap_or_default(),
        document_hits: response_hits,
        entity_hits: lexical_entity_hits,
        relation_hits: lexical_relation_hits,
        vector_chunk_hits,
        vector_entity_hits,
    }))
}

async fn search_expanded_lexical_chunks(
    state: &AppState,
    library_id: Uuid,
    query_text: &str,
    internal_candidate_limit: usize,
) -> Result<Vec<KnowledgeChunkSearchRow>, ApiError> {
    let search_queries = expand_document_search_queries(query_text);
    let parallelism = SEARCH_LEXICAL_QUERY_PARALLELISM.min(search_queries.len()).max(1);
    let search_results = stream::iter(search_queries.into_iter().map(|search_query| {
        let state = state.clone();
        async move {
            state
                .arango_search_store
                .search_chunks(library_id, &search_query, internal_candidate_limit, None, None)
                .await
                .map_err(|e| ApiError::internal_with_log(e, "internal"))
        }
    }))
    .buffer_unordered(parallelism)
    .collect::<Vec<_>>()
    .await;

    let mut lexical_chunk_hit_map = HashMap::<Uuid, KnowledgeChunkSearchRow>::new();
    for result in search_results {
        for row in result? {
            match lexical_chunk_hit_map.entry(row.chunk_id) {
                std::collections::hash_map::Entry::Occupied(mut occupied) => {
                    if row.score > occupied.get().score {
                        occupied.insert(row);
                    }
                }
                std::collections::hash_map::Entry::Vacant(vacant) => {
                    vacant.insert(row);
                }
            }
        }
    }

    let mut lexical_chunk_hits = lexical_chunk_hit_map.into_values().collect::<Vec<_>>();
    lexical_chunk_hits.sort_by(|left, right| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.chunk_id.cmp(&right.chunk_id))
    });
    Ok(lexical_chunk_hits)
}

async fn load_evidence_samples_by_chunk_ids(
    state: &AppState,
    document_id: Uuid,
    chunk_ids: &[Uuid],
) -> Result<HashMap<Uuid, Vec<KnowledgeEvidenceRow>>, ApiError> {
    if chunk_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let parallelism = SEARCH_CHUNK_EVIDENCE_PARALLELISM.min(chunk_ids.len()).max(1);
    let evidence_results = stream::iter(chunk_ids.iter().copied().map(|chunk_id| {
        let state = state.clone();
        async move {
            let evidence_rows = state
                .arango_graph_store
                .list_evidence_by_chunk(chunk_id)
                .await
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
            Ok::<(Uuid, Vec<KnowledgeEvidenceRow>), ApiError>((chunk_id, evidence_rows))
        }
    }))
    .buffer_unordered(parallelism)
    .collect::<Vec<_>>()
    .await;

    let mut evidence_by_chunk: HashMap<Uuid, Vec<KnowledgeEvidenceRow>> = HashMap::new();
    for result in evidence_results {
        let (chunk_id, evidence_rows) = result?;
        let rows = evidence_rows
            .into_iter()
            .filter(|evidence| evidence.document_id == document_id)
            .collect::<Vec<_>>();
        if !rows.is_empty() {
            evidence_by_chunk.insert(chunk_id, rows);
        }
    }
    Ok(evidence_by_chunk)
}

async fn enrich_document_search_hit(
    state: AppState,
    query_text: String,
    chunk_hit_limit_per_document: usize,
    index: usize,
    mut accumulator: KnowledgeDocumentAccumulator,
) -> Result<(usize, KnowledgeSearchDocumentHit), ApiError> {
    backfill_document_chunk_hits(
        &state,
        &query_text,
        chunk_hit_limit_per_document,
        &mut accumulator,
    )
    .await?;
    let technical_fact_samples = state
        .canonical_services
        .knowledge
        .list_typed_technical_facts(&state, accumulator.revision.revision_id)
        .await?;
    Ok((
        index,
        KnowledgeSearchDocumentHit {
            provenance_summary: KnowledgeDocumentProvenanceSummary {
                supporting_evidence_count: accumulator.evidence_samples.len(),
                lexical_chunk_count: accumulator.chunk_hits.len(),
                vector_chunk_count: accumulator.vector_chunk_hits.len(),
            },
            technical_fact_summary: summarize_typed_technical_facts(&technical_fact_samples),
            graph_evidence_summary: summarize_graph_evidence(&accumulator.evidence_samples),
            document: accumulator.document,
            revision: map_search_revision_summary(accumulator.revision),
            score: accumulator.score,
            lexical_rank: accumulator.lexical_rank,
            vector_rank: accumulator.vector_rank,
            lexical_score: accumulator.lexical_score,
            vector_score: accumulator.vector_score,
            chunk_hits: accumulator.chunk_hits,
            vector_chunk_hits: accumulator.vector_chunk_hits,
            evidence_samples: accumulator.evidence_samples,
            technical_fact_samples,
        },
    ))
}

fn resolved_chunk_hit(
    chunk_id: &Uuid,
    chunk_map: &HashMap<Uuid, KnowledgeChunkRow>,
    revision_map: &HashMap<Uuid, KnowledgeRevisionRow>,
    document_map: &HashMap<Uuid, KnowledgeDocumentRow>,
) -> bool {
    let Some(chunk) = chunk_map.get(chunk_id) else {
        return false;
    };
    revision_map.contains_key(&chunk.revision_id) && document_map.contains_key(&chunk.document_id)
}

fn map_search_revision_summary(revision: KnowledgeRevisionRow) -> KnowledgeSearchRevisionSummary {
    KnowledgeSearchRevisionSummary {
        revision_id: revision.revision_id,
        document_id: revision.document_id,
        revision_number: revision.revision_number,
        revision_state: revision.revision_state,
        revision_kind: revision.revision_kind,
        mime_type: revision.mime_type,
        title: revision.title,
        byte_size: revision.byte_size,
        text_state: revision.text_state,
        vector_state: revision.vector_state,
        graph_state: revision.graph_state,
        created_at: revision.created_at,
    }
}

async fn backfill_document_chunk_hits(
    state: &AppState,
    query_text: &str,
    chunk_hit_limit_per_document: usize,
    accumulator: &mut KnowledgeDocumentAccumulator,
) -> Result<(), ApiError> {
    let keywords = crate::services::query::planner::extract_keywords(query_text);
    if keywords.is_empty() {
        return Ok(());
    }

    let existing_chunk_ids =
        accumulator.chunk_hits.iter().map(|chunk| chunk.chunk_id).collect::<HashSet<_>>();
    let mut candidates = accumulator.chunk_hits.clone();
    let mut backfill_terms = keywords.clone();
    backfill_terms.extend(document_search_anchor_tokens(query_text));
    backfill_terms.extend(document_search_anchor_phrases(query_text));
    candidates.extend(
        state
            .arango_document_store
            .list_chunks_by_revision_matching_terms(
                accumulator.revision.revision_id,
                &backfill_terms,
                chunk_hit_limit_per_document.saturating_mul(4).max(chunk_hit_limit_per_document),
            )
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
            .into_iter()
            .filter(|chunk| !existing_chunk_ids.contains(&chunk.chunk_id))
            .filter_map(|chunk| {
                let haystack =
                    format!("{} {}", chunk.content_text, chunk.normalized_text).to_lowercase();
                let score = keywords
                    .iter()
                    .map(|keyword| haystack.matches(keyword.as_str()).count() as f64)
                    .sum::<f64>();
                (score > 0.0).then_some(KnowledgeChunkSearchRow {
                    chunk_id: chunk.chunk_id,
                    workspace_id: chunk.workspace_id,
                    library_id: chunk.library_id,
                    revision_id: chunk.revision_id,
                    content_text: repair_technical_layout_noise(&chunk.content_text),
                    normalized_text: repair_technical_layout_noise(&chunk.normalized_text),
                    section_path: chunk.section_path,
                    heading_trail: chunk.heading_trail,
                    score,
                    quality_score: chunk.quality_score,
                })
            }),
    );
    candidates.sort_by(|left, right| {
        document_search_chunk_relevance(query_text, right)
            .cmp(&document_search_chunk_relevance(query_text, left))
            .then_with(|| right.score.partial_cmp(&left.score).unwrap_or(std::cmp::Ordering::Equal))
            .then_with(|| left.chunk_id.cmp(&right.chunk_id))
    });
    candidates.dedup_by(|left, right| left.chunk_id == right.chunk_id);
    accumulator.chunk_hits = candidates.into_iter().take(chunk_hit_limit_per_document).collect();
    Ok(())
}

fn document_search_chunk_relevance(query_text: &str, hit: &KnowledgeChunkSearchRow) -> usize {
    let lowered_text = format!("{} {}", hit.content_text, hit.normalized_text).to_lowercase();
    let keywords = crate::services::query::planner::extract_keywords(query_text);
    let keyword_score = keywords
        .iter()
        .map(|keyword| lowered_text.matches(keyword.as_str()).count())
        .sum::<usize>();
    let anchor_score = document_search_anchor_tokens(query_text)
        .into_iter()
        .filter(|token| lowered_text.contains(token))
        .count();
    let phrase_score = document_search_anchor_phrases(query_text)
        .into_iter()
        .filter(|phrase| lowered_text.contains(phrase))
        .count();
    keyword_score + anchor_score * 50 + phrase_score * 150
}

fn document_search_anchor_tokens(query_text: &str) -> Vec<String> {
    normalized_alnum_token_sequence_by(
        query_text,
        |token| token.chars().count() >= 4 || token.chars().any(|ch| ch.is_ascii_digit()),
        Some(32),
    )
}

fn document_search_anchor_phrases(query_text: &str) -> Vec<String> {
    let tokens = normalized_alnum_token_sequence_by(
        query_text,
        |token| token.chars().count() >= 2 || token.chars().any(|ch| ch.is_ascii_digit()),
        Some(48),
    );
    let anchors = document_search_anchor_tokens(query_text).into_iter().collect::<HashSet<_>>();
    let mut phrases = Vec::<String>::new();
    let mut seen = HashSet::<String>::new();
    for window_size in 2..=5 {
        for window in tokens.windows(window_size) {
            if !window.iter().any(|token| anchors.contains(token)) {
                continue;
            }
            let phrase = window.join(" ");
            if seen.insert(phrase.clone()) {
                phrases.push(phrase);
            }
        }
    }
    phrases
}

async fn resolve_hybrid_search_context(
    state: &AppState,
    library_id: Uuid,
    query_text: &str,
) -> Result<Option<KnowledgeHybridSearchContext>, ApiError> {
    let Some(binding) = state
        .canonical_services
        .ai_catalog
        .resolve_active_runtime_binding(state, library_id, AiBindingPurpose::EmbedChunk)
        .await?
    else {
        return Ok(None);
    };

    let generations = state
        .canonical_services
        .knowledge
        .derive_library_generation_rows(state, library_id)
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
    let Some(generation): Option<&KnowledgeLibraryGenerationRow> = generations.first() else {
        return Ok(None);
    };
    if generation.active_vector_generation <= 0 {
        return Ok(None);
    }

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

    Ok(Some(KnowledgeHybridSearchContext {
        provider_kind: binding.provider_kind,
        model_name: binding.model_name,
        model_catalog_id: binding.model_catalog_id,
        freshness_generation: generation.active_vector_generation,
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
        .arango_document_store
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
        .arango_document_store
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
        .arango_search_store
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
        .arango_search_store
        .search_relations(library_id, query_text, limit.max(1))
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk_hit(text: &str) -> KnowledgeChunkSearchRow {
        KnowledgeChunkSearchRow {
            chunk_id: Uuid::nil(),
            workspace_id: Uuid::nil(),
            library_id: Uuid::nil(),
            revision_id: Uuid::nil(),
            content_text: text.to_string(),
            normalized_text: text.to_string(),
            section_path: Vec::new(),
            heading_trail: Vec::new(),
            score: 1.0,
            quality_score: None,
        }
    }

    #[test]
    fn document_keyword_coverage_bonus_rewards_concentrated_evidence() {
        let keywords =
            document_search_keywords("Alpha Gateway auth billing payment service ports queue");
        let weak = chunk_hit("Alpha Gateway API mentions payment once.");
        let strong = chunk_hit(
            "Alpha Gateway auth service, billing service, payment service, ports, and queue.",
        );

        let weak_bonus = document_keyword_coverage_bonus(&[weak], &keywords);
        let strong_bonus = document_keyword_coverage_bonus(&[strong], &keywords);

        assert!(strong_bonus > weak_bonus + 0.2);
    }

    #[test]
    fn evidence_sample_limit_preserves_explicit_zero() {
        assert_eq!(resolved_evidence_sample_limit(Some(0)), 0);
        assert_eq!(resolved_evidence_sample_limit(Some(2)), 2);
        assert_eq!(resolved_evidence_sample_limit(None), DEFAULT_EVIDENCE_SAMPLE_LIMIT);
    }

    #[test]
    fn document_search_vector_limits_track_response_surface() {
        assert_eq!(document_search_vector_limits(3, 3), (9, 3));
        assert_eq!(document_search_vector_limits(1, 1), (2, 1));
    }

    #[test]
    fn anchor_phrases_are_generated_from_structure_without_topic_aliases() {
        let queries = expand_document_search_queries("Alpha Gateway auth service ports");

        assert_eq!(queries[0], "Alpha Gateway auth service ports");
        assert!(queries.iter().any(|query| query == "alpha gateway"));
        assert!(queries.iter().any(|query| query == "gateway auth"));
    }
}
