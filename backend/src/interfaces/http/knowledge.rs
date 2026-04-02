use std::{
    collections::{BTreeMap, HashMap, HashSet},
    time::Duration,
};

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    routing::get,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::time::timeout;
use tracing::warn;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::ai::AiBindingPurpose,
    domains::knowledge::{KnowledgeLibraryGeneration, TypedTechnicalFact},
    infra::arangodb::{
        collections::{
            KNOWLEDGE_CHUNK_COLLECTION, KNOWLEDGE_CHUNK_MENTIONS_ENTITY_EDGE,
            KNOWLEDGE_EVIDENCE_SUPPORTS_ENTITY_EDGE, KNOWLEDGE_EVIDENCE_SUPPORTS_RELATION_EDGE,
        },
        context_store::{
            KnowledgeBundleChunkReferenceRow, KnowledgeBundleEntityReferenceRow,
            KnowledgeBundleEvidenceReferenceRow, KnowledgeBundleRelationReferenceRow,
            KnowledgeContextBundleRow, KnowledgeRetrievalTraceRow,
        },
        document_store::{
            KnowledgeChunkRow, KnowledgeDocumentRow, KnowledgeLibraryGenerationRow,
            KnowledgeRevisionRow,
        },
        graph_store::{
            KnowledgeDocumentGraphLinkRow, KnowledgeEntityRow, KnowledgeEvidenceRow,
            KnowledgeRelationTopologyRow,
        },
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
    shared::text_render::repair_technical_layout_noise,
};

const DEFAULT_SEARCH_LIMIT: usize = 10;
const DEFAULT_EVIDENCE_SAMPLE_LIMIT: usize = 5;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/knowledge/context-bundles/{bundle_id}", get(get_context_bundle))
        .route("/knowledge/libraries/{library_id}/context-bundles", get(list_context_bundles))
        .route("/knowledge/libraries/{library_id}/documents", get(list_documents))
        .route("/knowledge/libraries/{library_id}/documents/{document_id}", get(get_document))
        .route("/knowledge/libraries/{library_id}/summary", get(get_library_summary))
        .route("/knowledge/libraries/{library_id}/graph-topology", get(get_graph_topology))
        .route("/knowledge/libraries/{library_id}/entities", get(list_entities))
        .route("/knowledge/libraries/{library_id}/entities/{entity_id}", get(get_entity))
        .route("/knowledge/libraries/{library_id}/relations", get(list_relations))
        .route("/knowledge/libraries/{library_id}/relations/{relation_id}", get(get_relation))
        .route("/knowledge/libraries/{library_id}/generations", get(list_library_generations))
        .route("/knowledge/libraries/{library_id}/search/documents", get(search_documents))
        .route("/search/documents", get(search_documents_by_library_query))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct KnowledgeDocumentSearchQuery {
    #[serde(alias = "q")]
    query: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    chunk_hit_limit_per_document: Option<usize>,
    #[serde(default)]
    evidence_sample_limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct KnowledgeDocumentSearchRequest {
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

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct KnowledgeContextBundleDetailResponse {
    bundle: KnowledgeContextBundleRow,
    traces: Vec<KnowledgeRetrievalTraceRow>,
    chunk_references: Vec<KnowledgeBundleChunkReferenceRow>,
    entity_references: Vec<KnowledgeBundleEntityReferenceRow>,
    relation_references: Vec<KnowledgeBundleRelationReferenceRow>,
    evidence_references: Vec<KnowledgeBundleEvidenceReferenceRow>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct KnowledgeDocumentDetailResponse {
    document: KnowledgeDocumentRow,
    revisions: Vec<KnowledgeRevisionRow>,
    latest_revision: Option<KnowledgeRevisionRow>,
    latest_revision_chunks: Vec<KnowledgeChunkRow>,
    latest_revision_typed_facts: Vec<TypedTechnicalFact>,
    technical_fact_summary: KnowledgeTechnicalFactProvenanceSummary,
    graph_evidence_summary: KnowledgeGraphEvidenceSummary,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct KnowledgeSearchRevisionSummary {
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

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct KnowledgeEntityDetailResponse {
    entity: KnowledgeEntityRow,
    mention_edges: Vec<KnowledgeEntityMentionEdgeRow>,
    mentioned_chunks: Vec<KnowledgeChunkRow>,
    supporting_evidence_edges: Vec<KnowledgeEvidenceSupportEntityEdgeRow>,
    supporting_evidence: Vec<KnowledgeEvidenceRow>,
    supporting_typed_facts: Vec<TypedTechnicalFact>,
    graph_evidence_summary: KnowledgeGraphEvidenceSummary,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct KnowledgeRelationDetailResponse {
    relation: KnowledgeRelationTopologyRow,
    supporting_evidence_edges: Vec<KnowledgeEvidenceSupportRelationEdgeRow>,
    supporting_evidence: Vec<KnowledgeEvidenceRow>,
    supporting_typed_facts: Vec<TypedTechnicalFact>,
    graph_evidence_summary: KnowledgeGraphEvidenceSummary,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct KnowledgeGraphTopologyResponse {
    documents: Vec<KnowledgeDocumentRow>,
    entities: Vec<KnowledgeEntityRow>,
    relations: Vec<KnowledgeRelationTopologyRow>,
    document_links: Vec<KnowledgeDocumentGraphLinkRow>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct KnowledgeLibrarySummaryResponse {
    library_id: Uuid,
    document_counts_by_readiness: BTreeMap<String, i64>,
    graph_ready_document_count: i64,
    graph_sparse_document_count: i64,
    typed_fact_document_count: i64,
    updated_at: DateTime<Utc>,
    latest_generation: Option<KnowledgeLibraryGeneration>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct KnowledgeSearchDocumentHit {
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
    technical_fact_samples: Vec<TypedTechnicalFact>,
    provenance_summary: KnowledgeDocumentProvenanceSummary,
    technical_fact_summary: KnowledgeTechnicalFactProvenanceSummary,
    graph_evidence_summary: KnowledgeGraphEvidenceSummary,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct KnowledgeDocumentSearchResponse {
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

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct KnowledgeDocumentProvenanceSummary {
    supporting_evidence_count: usize,
    lexical_chunk_count: usize,
    vector_chunk_count: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct KnowledgeTechnicalFactProvenanceSummary {
    typed_fact_count: usize,
    fact_kind_counts: BTreeMap<String, usize>,
    conflict_group_count: usize,
    support_block_count: usize,
    support_chunk_count: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct KnowledgeGraphEvidenceSummary {
    evidence_count: usize,
    chunk_backed_count: usize,
    block_backed_count: usize,
    fact_backed_count: usize,
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
    sanitized
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct KnowledgeEntityMentionEdgeRow {
    key: String,
    entity_id: Uuid,
    chunk_id: Uuid,
    rank: Option<i32>,
    score: Option<f64>,
    inclusion_reason: Option<String>,
    created_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct KnowledgeEvidenceSupportEntityEdgeRow {
    key: String,
    evidence_id: Uuid,
    entity_id: Uuid,
    rank: Option<i32>,
    score: Option<f64>,
    inclusion_reason: Option<String>,
    created_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct KnowledgeEvidenceSupportRelationEdgeRow {
    key: String,
    evidence_id: Uuid,
    relation_id: Uuid,
    rank: Option<i32>,
    score: Option<f64>,
    inclusion_reason: Option<String>,
    created_at: chrono::DateTime<chrono::Utc>,
}

async fn list_entities(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(library_id): Path<Uuid>,
) -> Result<Json<Vec<KnowledgeEntityRow>>, ApiError> {
    let _ = load_library_and_authorize(&auth, &state, library_id, POLICY_KNOWLEDGE_READ).await?;
    let entities = state
        .arango_graph_store
        .list_entities_by_library(library_id)
        .await
        .map_err(|_| ApiError::Internal)?;
    Ok(Json(entities))
}

async fn list_relations(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(library_id): Path<Uuid>,
) -> Result<Json<Vec<KnowledgeRelationTopologyRow>>, ApiError> {
    let _ = load_library_and_authorize(&auth, &state, library_id, POLICY_KNOWLEDGE_READ).await?;
    let relations =
        state.arango_graph_store.list_relation_topology_by_library(library_id).await.map_err(
            |error| {
                tracing::error!(%library_id, ?error, "failed to list knowledge relation topology");
                ApiError::Internal
            },
        )?;
    Ok(Json(relations))
}

async fn list_context_bundles(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(library_id): Path<Uuid>,
) -> Result<Json<Vec<KnowledgeContextBundleRow>>, ApiError> {
    let _ = load_library_and_authorize(&auth, &state, library_id, POLICY_KNOWLEDGE_READ).await?;
    let bundles = state
        .arango_context_store
        .list_bundles_by_library(library_id)
        .await
        .map_err(|_| ApiError::Internal)?;
    Ok(Json(bundles))
}

async fn list_documents(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(library_id): Path<Uuid>,
) -> Result<Json<Vec<KnowledgeDocumentRow>>, ApiError> {
    let library =
        load_library_and_authorize(&auth, &state, library_id, POLICY_KNOWLEDGE_READ).await?;
    let documents = state
        .arango_document_store
        .list_documents_by_library(library.workspace_id, library.id)
        .await
        .map_err(|_| ApiError::Internal)?;
    Ok(Json(documents))
}

async fn get_library_summary(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(library_id): Path<Uuid>,
) -> Result<Json<KnowledgeLibrarySummaryResponse>, ApiError> {
    let library =
        load_library_and_authorize(&auth, &state, library_id, POLICY_KNOWLEDGE_READ).await?;
    let summary =
        state.canonical_services.knowledge.get_library_summary(&state, library.id).await?;
    Ok(Json(KnowledgeLibrarySummaryResponse {
        library_id: summary.library_id,
        document_counts_by_readiness: summary.document_counts_by_readiness,
        graph_ready_document_count: summary.graph_ready_document_count,
        graph_sparse_document_count: summary.graph_sparse_document_count,
        typed_fact_document_count: summary.typed_fact_document_count,
        updated_at: summary.updated_at,
        latest_generation: summary.latest_generation,
    }))
}

async fn get_document(
    auth: AuthContext,
    State(state): State<AppState>,
    Path((library_id, document_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<KnowledgeDocumentDetailResponse>, ApiError> {
    let library =
        load_library_and_authorize(&auth, &state, library_id, POLICY_KNOWLEDGE_READ).await?;
    let document = state
        .arango_document_store
        .get_document(document_id)
        .await
        .map_err(|_| ApiError::Internal)?
        .ok_or_else(|| ApiError::resource_not_found("knowledge_document", document_id))?;
    if document.library_id != library.id {
        return Err(ApiError::resource_not_found("knowledge_document", document_id));
    }
    let revisions = state
        .arango_document_store
        .list_revisions_by_document(document_id)
        .await
        .map_err(|_| ApiError::Internal)?;
    let latest_revision = revisions.first().cloned();
    let latest_revision_chunks = match latest_revision.as_ref() {
        Some(revision) => state
            .arango_document_store
            .list_chunks_by_revision(revision.revision_id)
            .await
            .map_err(|_| ApiError::Internal)?,
        None => Vec::new(),
    };
    let latest_revision_typed_facts = match latest_revision.as_ref() {
        Some(revision) => {
            state
                .canonical_services
                .knowledge
                .list_typed_technical_facts(&state, revision.revision_id)
                .await?
        }
        None => Vec::new(),
    };
    let latest_revision_evidence = match latest_revision.as_ref() {
        Some(revision) => state
            .arango_graph_store
            .list_evidence_by_revision(revision.revision_id)
            .await
            .map_err(|_| ApiError::Internal)?,
        None => Vec::new(),
    };
    Ok(Json(KnowledgeDocumentDetailResponse {
        document,
        revisions,
        latest_revision,
        latest_revision_chunks,
        latest_revision_typed_facts: latest_revision_typed_facts.clone(),
        technical_fact_summary: summarize_typed_technical_facts(&latest_revision_typed_facts),
        graph_evidence_summary: summarize_graph_evidence(&latest_revision_evidence),
    }))
}

async fn get_graph_topology(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(library_id): Path<Uuid>,
) -> Result<Json<KnowledgeGraphTopologyResponse>, ApiError> {
    let _ = load_library_and_authorize(&auth, &state, library_id, POLICY_KNOWLEDGE_READ).await?;
    let entities = state
        .arango_graph_store
        .list_entities_by_library(library_id)
        .await
        .map_err(|_| ApiError::Internal)?;
    let relations =
        state.arango_graph_store.list_relation_topology_by_library(library_id).await.map_err(
            |error| {
                tracing::error!(%library_id, ?error, "failed to list knowledge relation topology");
                ApiError::Internal
            },
        )?;
    let document_links = match timeout(
        Duration::from_secs(3),
        state.arango_graph_store.list_document_graph_links_by_library(library_id),
    )
    .await
    {
        Ok(Ok(rows)) => rows,
        Ok(Err(error)) => {
            tracing::warn!(
                %library_id,
                ?error,
                "failed to list knowledge document graph links; continuing without document nodes",
            );
            Vec::new()
        }
        Err(_) => {
            tracing::warn!(
                %library_id,
                "timed out while listing knowledge document graph links; continuing without document nodes",
            );
            Vec::new()
        }
    };
    let linked_document_ids =
        document_links.iter().map(|row| row.document_id).collect::<HashSet<_>>();
    let documents = state
        .arango_document_store
        .list_documents_by_ids(&linked_document_ids.into_iter().collect::<Vec<_>>())
        .await
        .map_err(|_| ApiError::Internal)?;

    Ok(Json(KnowledgeGraphTopologyResponse { documents, entities, relations, document_links }))
}

async fn get_context_bundle(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(bundle_id): Path<Uuid>,
) -> Result<Json<KnowledgeContextBundleDetailResponse>, ApiError> {
    let bundle_set = state
        .arango_context_store
        .get_bundle_reference_set(bundle_id)
        .await
        .map_err(|_| ApiError::Internal)?
        .ok_or_else(|| ApiError::context_bundle_not_found(bundle_id))?;
    let _ = load_library_and_authorize(
        &auth,
        &state,
        bundle_set.bundle.library_id,
        POLICY_KNOWLEDGE_READ,
    )
    .await?;
    let traces = state
        .arango_context_store
        .list_traces_by_bundle(bundle_set.bundle.bundle_id)
        .await
        .map_err(|_| ApiError::Internal)?;
    Ok(Json(KnowledgeContextBundleDetailResponse {
        bundle: bundle_set.bundle,
        traces,
        chunk_references: bundle_set.chunk_references,
        entity_references: bundle_set.entity_references,
        relation_references: bundle_set.relation_references,
        evidence_references: bundle_set.evidence_references,
    }))
}

async fn search_documents(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(library_id): Path<Uuid>,
    Query(query): Query<KnowledgeDocumentSearchQuery>,
) -> Result<Json<KnowledgeDocumentSearchResponse>, ApiError> {
    search_documents_impl(
        auth,
        state,
        library_id,
        query.query,
        query.limit,
        query.chunk_hit_limit_per_document,
        query.evidence_sample_limit,
    )
    .await
}

async fn search_documents_by_library_query(
    auth: AuthContext,
    State(state): State<AppState>,
    Query(query): Query<KnowledgeDocumentSearchRequest>,
) -> Result<Json<KnowledgeDocumentSearchResponse>, ApiError> {
    search_documents_impl(
        auth,
        state,
        query.library_id,
        query.query,
        query.limit,
        query.chunk_hit_limit_per_document,
        query.evidence_sample_limit,
    )
    .await
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
    let evidence_sample_limit =
        evidence_sample_limit.unwrap_or(DEFAULT_EVIDENCE_SAMPLE_LIMIT).max(1);
    let lexical_chunk_hits = state
        .arango_search_store
        .search_chunks(library_id, &query_text, limit)
        .await
        .map_err(|_| ApiError::Internal)?;
    let lexical_entity_hits =
        search_entities_by_library(&state, library_id, &query_text, limit).await?;
    let lexical_relation_hits =
        search_relations_by_library(&state, library_id, &query_text, limit).await?;

    let hybrid_context = resolve_hybrid_search_context(&state, library_id, &query_text).await?;
    let vector_candidate_limit = limit.saturating_mul(2).max(1);
    let vector_chunk_hits = if let Some(context) = hybrid_context.as_ref() {
        match state
            .arango_search_store
            .search_chunk_vectors_by_similarity(
                library_id,
                &context.model_catalog_id.to_string(),
                context.freshness_generation,
                &context.query_vector,
                vector_candidate_limit,
                Some(16),
            )
            .await
        {
            Ok(rows) => rows,
            Err(error) => {
                warn!(
                    library_id = %library_id,
                    model_catalog_id = %context.model_catalog_id,
                    freshness_generation = context.freshness_generation,
                    error = ?error,
                    "hybrid knowledge chunk vector search failed; falling back to lexical-only hits",
                );
                Vec::new()
            }
        }
    } else {
        Vec::new()
    };
    let vector_entity_hits = if let Some(context) = hybrid_context.as_ref() {
        match state
            .arango_search_store
            .search_entity_vectors_by_similarity(
                library_id,
                &context.model_catalog_id.to_string(),
                context.freshness_generation,
                &context.query_vector,
                vector_candidate_limit,
                Some(16),
            )
            .await
        {
            Ok(rows) => rows,
            Err(error) => {
                warn!(
                    library_id = %library_id,
                    model_catalog_id = %context.model_catalog_id,
                    freshness_generation = context.freshness_generation,
                    error = ?error,
                    "hybrid knowledge entity vector search failed; returning lexical entity hits only",
                );
                Vec::new()
            }
        }
    } else {
        Vec::new()
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

    let revision_ids: Vec<Uuid> = chunk_map
        .values()
        .map(|chunk| chunk.revision_id)
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    let revisions = load_revisions_by_ids(&state, &revision_ids).await?;
    let revision_map: HashMap<Uuid, KnowledgeRevisionRow> =
        revisions.into_iter().map(|revision| (revision.revision_id, revision)).collect();

    let document_ids: Vec<Uuid> = chunk_map
        .values()
        .map(|chunk| chunk.document_id)
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    let documents = load_documents_by_ids(&state, &document_ids).await?;
    let document_map: HashMap<Uuid, KnowledgeDocumentRow> =
        documents.into_iter().map(|document| (document.document_id, document)).collect();

    let mut accumulators: HashMap<Uuid, KnowledgeDocumentAccumulator> = HashMap::new();
    for (rank, hit) in lexical_chunk_hits.iter().enumerate() {
        let chunk = chunk_map
            .get(&hit.chunk_id)
            .ok_or_else(|| ApiError::resource_not_found("knowledge_chunk", hit.chunk_id))?;
        let revision = revision_map
            .get(&chunk.revision_id)
            .ok_or_else(|| ApiError::resource_not_found("knowledge_revision", chunk.revision_id))?;
        let document = document_map
            .get(&chunk.document_id)
            .ok_or_else(|| ApiError::resource_not_found("knowledge_document", chunk.document_id))?;
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
        let chunk = chunk_map
            .get(&hit.chunk_id)
            .ok_or_else(|| ApiError::resource_not_found("knowledge_chunk", hit.chunk_id))?;
        let revision = revision_map
            .get(&chunk.revision_id)
            .ok_or_else(|| ApiError::resource_not_found("knowledge_revision", chunk.revision_id))?;
        let document = document_map
            .get(&chunk.document_id)
            .ok_or_else(|| ApiError::resource_not_found("knowledge_document", chunk.document_id))?;
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
                right
                    .score
                    .partial_cmp(&left.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
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
        for chunk_id in candidate_chunk_ids {
            if !seen_evidence_chunks.insert(chunk_id) {
                continue;
            }
            let evidence_rows = state
                .arango_graph_store
                .list_evidence_by_chunk(chunk_id)
                .await
                .map_err(|_| ApiError::Internal)?;
            for evidence in evidence_rows {
                if evidence.document_id != accumulator.document.document_id {
                    continue;
                }
                if accumulator.evidence_ids.insert(evidence.evidence_id) {
                    accumulator.evidence_samples.push(evidence);
                }
                if accumulator.evidence_samples.len() >= evidence_sample_limit {
                    break;
                }
            }
            if accumulator.evidence_samples.len() >= evidence_sample_limit {
                break;
            }
        }

        let lexical_rank = accumulator.lexical_rank.unwrap_or(usize::MAX / 2);
        let vector_rank = accumulator.vector_rank.unwrap_or(usize::MAX / 2);
        let provenance_bonus = (accumulator.evidence_samples.len() as f64) / 1000.0;
        accumulator.score = (1.0 / (60.0 + lexical_rank as f64))
            + (1.0 / (60.0 + vector_rank as f64))
            + provenance_bonus;
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
    let mut response_hits = Vec::with_capacity(document_hits.len());
    for accumulator in document_hits {
        let technical_fact_samples = state
            .canonical_services
            .knowledge
            .list_typed_technical_facts(&state, accumulator.revision.revision_id)
            .await?;
        response_hits.push(KnowledgeSearchDocumentHit {
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
        });
    }

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
        .arango_document_store
        .list_library_generations(library_id)
        .await
        .map_err(|_| ApiError::Internal)?;
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
            api_key_override: Some(binding.api_key),
            base_url_override: binding.provider_base_url.clone(),
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
    let mut rows = Vec::with_capacity(revision_ids.len());
    for revision_id in revision_ids {
        let revision = state
            .arango_document_store
            .get_revision(*revision_id)
            .await
            .map_err(|_| ApiError::Internal)?
            .ok_or_else(|| ApiError::resource_not_found("knowledge_revision", revision_id))?;
        rows.push(revision);
    }
    Ok(rows)
}

async fn load_documents_by_ids(
    state: &AppState,
    document_ids: &[Uuid],
) -> Result<Vec<KnowledgeDocumentRow>, ApiError> {
    if document_ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut rows = Vec::with_capacity(document_ids.len());
    for document_id in document_ids {
        let document = state
            .arango_document_store
            .get_document(*document_id)
            .await
            .map_err(|_| ApiError::Internal)?
            .ok_or_else(|| ApiError::resource_not_found("knowledge_document", document_id))?;
        rows.push(document);
    }
    Ok(rows)
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
        .map_err(|_| ApiError::Internal)
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
        .map_err(|_| ApiError::Internal)
}

async fn list_library_generations(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(library_id): Path<Uuid>,
) -> Result<Json<Vec<crate::domains::knowledge::KnowledgeLibraryGeneration>>, ApiError> {
    let _ = load_library_and_authorize(&auth, &state, library_id, POLICY_KNOWLEDGE_READ).await?;
    let generations =
        state.canonical_services.knowledge.list_library_generations(&state, library_id).await?;
    Ok(Json(generations))
}

async fn get_entity(
    auth: AuthContext,
    State(state): State<AppState>,
    Path((library_id, entity_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<KnowledgeEntityDetailResponse>, ApiError> {
    let _ = load_library_and_authorize(&auth, &state, library_id, POLICY_KNOWLEDGE_READ).await?;
    let entity = state
        .arango_graph_store
        .get_entity_by_id(entity_id)
        .await
        .map_err(|_| ApiError::Internal)?
        .ok_or_else(|| ApiError::resource_not_found("knowledge_entity", entity_id))?;
    if entity.library_id != library_id {
        return Err(ApiError::resource_not_found("knowledge_entity", entity_id));
    }

    let mention_edges = list_entity_mention_edges(&state, entity_id).await?;
    let mention_chunk_ids: Vec<Uuid> = mention_edges.iter().map(|edge| edge.chunk_id).collect();
    let mentioned_chunks = load_chunks_by_ids(&state, &mention_chunk_ids).await?;
    let supporting_evidence_edges = list_entity_evidence_support_edges(&state, entity_id).await?;
    let supporting_evidence_ids: Vec<Uuid> =
        supporting_evidence_edges.iter().map(|edge| edge.evidence_id).collect();
    let supporting_evidence = load_evidence_by_ids(&state, &supporting_evidence_ids).await?;
    let supporting_typed_facts =
        load_typed_technical_facts_for_evidence(&state, &supporting_evidence).await?;
    let graph_evidence_summary =
        summarize_graph_evidence_from_support(&supporting_evidence, supporting_typed_facts.len());

    Ok(Json(KnowledgeEntityDetailResponse {
        entity,
        mention_edges,
        mentioned_chunks,
        supporting_evidence_edges,
        supporting_evidence,
        supporting_typed_facts: supporting_typed_facts.clone(),
        graph_evidence_summary,
    }))
}

async fn get_relation(
    auth: AuthContext,
    State(state): State<AppState>,
    Path((library_id, relation_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<KnowledgeRelationDetailResponse>, ApiError> {
    let _ = load_library_and_authorize(&auth, &state, library_id, POLICY_KNOWLEDGE_READ).await?;
    let relation = state
        .arango_graph_store
        .get_relation_topology_by_id(relation_id)
        .await
        .map_err(|error| {
            tracing::error!(%library_id, %relation_id, ?error, "failed to load knowledge relation topology");
            ApiError::Internal
        })?
        .ok_or_else(|| ApiError::resource_not_found("knowledge_relation", relation_id))?;
    if relation.relation.library_id != library_id {
        return Err(ApiError::resource_not_found("knowledge_relation", relation_id));
    }

    let supporting_evidence_edges =
        list_relation_evidence_support_edges(&state, relation_id)
            .await
            .map_err(|error| {
                tracing::error!(%library_id, %relation_id, ?error, "failed to load relation evidence support edges");
                error
            })?;
    let supporting_evidence_ids: Vec<Uuid> =
        supporting_evidence_edges.iter().map(|edge| edge.evidence_id).collect();
    let supporting_evidence =
        load_evidence_by_ids(&state, &supporting_evidence_ids).await.map_err(|error| {
            tracing::error!(
                %library_id,
                %relation_id,
                evidence_count = supporting_evidence_ids.len(),
                ?error,
                "failed to load relation supporting evidence",
            );
            error
        })?;
    let supporting_typed_facts =
        load_typed_technical_facts_for_evidence(&state, &supporting_evidence).await?;
    let graph_evidence_summary =
        summarize_graph_evidence_from_support(&supporting_evidence, supporting_typed_facts.len());

    Ok(Json(KnowledgeRelationDetailResponse {
        relation,
        supporting_evidence_edges,
        supporting_evidence,
        supporting_typed_facts: supporting_typed_facts.clone(),
        graph_evidence_summary,
    }))
}

async fn load_chunks_by_ids(
    state: &AppState,
    chunk_ids: &[Uuid],
) -> Result<Vec<KnowledgeChunkRow>, ApiError> {
    if chunk_ids.is_empty() {
        return Ok(Vec::new());
    }
    let cursor = state
        .arango_document_store
        .client()
        .query_json(
            "FOR chunk IN @@collection
             FILTER chunk.chunk_id IN @chunk_ids
             SORT chunk.chunk_id ASC
             RETURN chunk",
            serde_json::json!({
                "@collection": KNOWLEDGE_CHUNK_COLLECTION,
                "chunk_ids": chunk_ids,
            }),
        )
        .await
        .map_err(|_| ApiError::Internal)?;
    decode_many_results(cursor).map_err(|_| ApiError::Internal)
}

async fn load_evidence_by_ids(
    state: &AppState,
    evidence_ids: &[Uuid],
) -> Result<Vec<KnowledgeEvidenceRow>, ApiError> {
    if evidence_ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut evidence_rows = Vec::new();
    for evidence_id in evidence_ids {
        if let Some(evidence) = state
            .arango_graph_store
            .get_evidence_by_id(*evidence_id)
            .await
            .map_err(|_| ApiError::Internal)?
        {
            evidence_rows.push(evidence);
        }
    }
    Ok(evidence_rows)
}

async fn load_typed_technical_facts_for_evidence(
    state: &AppState,
    evidence_rows: &[KnowledgeEvidenceRow],
) -> Result<Vec<TypedTechnicalFact>, ApiError> {
    let fact_ids = evidence_rows
        .iter()
        .filter_map(|evidence| evidence.fact_id)
        .collect::<HashSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    state.canonical_services.knowledge.list_typed_technical_facts_by_ids(state, &fact_ids).await
}

fn summarize_typed_technical_facts(
    typed_facts: &[TypedTechnicalFact],
) -> KnowledgeTechnicalFactProvenanceSummary {
    let mut fact_kind_counts = BTreeMap::<String, usize>::new();
    let mut conflict_group_ids = HashSet::<String>::new();
    let mut support_block_ids = HashSet::<Uuid>::new();
    let mut support_chunk_ids = HashSet::<Uuid>::new();
    for fact in typed_facts {
        *fact_kind_counts.entry(fact.fact_kind.as_str().to_string()).or_default() += 1;
        if let Some(conflict_group_id) = fact.conflict_group_id.as_ref() {
            conflict_group_ids.insert(conflict_group_id.clone());
        }
        support_block_ids.extend(fact.support_block_ids.iter().copied());
        support_chunk_ids.extend(fact.support_chunk_ids.iter().copied());
    }
    KnowledgeTechnicalFactProvenanceSummary {
        typed_fact_count: typed_facts.len(),
        fact_kind_counts,
        conflict_group_count: conflict_group_ids.len(),
        support_block_count: support_block_ids.len(),
        support_chunk_count: support_chunk_ids.len(),
    }
}

fn summarize_graph_evidence(
    evidence_rows: &[KnowledgeEvidenceRow],
) -> KnowledgeGraphEvidenceSummary {
    KnowledgeGraphEvidenceSummary {
        evidence_count: evidence_rows.len(),
        chunk_backed_count: evidence_rows
            .iter()
            .filter(|evidence| evidence.chunk_id.is_some())
            .count(),
        block_backed_count: evidence_rows
            .iter()
            .filter(|evidence| evidence.block_id.is_some())
            .count(),
        fact_backed_count: evidence_rows
            .iter()
            .filter(|evidence| evidence.fact_id.is_some())
            .count(),
    }
}

fn summarize_graph_evidence_from_support(
    evidence_rows: &[KnowledgeEvidenceRow],
    typed_fact_count: usize,
) -> KnowledgeGraphEvidenceSummary {
    let mut summary = summarize_graph_evidence(evidence_rows);
    summary.fact_backed_count = summary.fact_backed_count.max(typed_fact_count);
    summary
}

async fn list_entity_mention_edges(
    state: &AppState,
    entity_id: Uuid,
) -> Result<Vec<KnowledgeEntityMentionEdgeRow>, ApiError> {
    let cursor = state
        .arango_graph_store
        .client()
        .query_json(
            "FOR edge IN @@collection
             LET entity = DOCUMENT(edge._to)
             LET chunk = DOCUMENT(edge._from)
             LET resolved_rank = edge.rank != null ? edge.rank : edge.payload.rank
             LET resolved_score = edge.score != null ? edge.score : edge.payload.score
             LET resolved_inclusion_reason = edge.inclusionReason != null ? edge.inclusionReason : edge.payload.inclusionReason
             FILTER entity != null
               AND chunk != null
               AND entity.entity_id == @entity_id
             SORT resolved_rank ASC, edge.created_at ASC, edge._key ASC
             RETURN {
                key: edge._key,
                entityId: entity.entity_id,
                chunkId: chunk.chunk_id,
                rank: resolved_rank,
                score: resolved_score,
                inclusionReason: resolved_inclusion_reason,
                createdAt: edge.created_at
             }",
            serde_json::json!({
                "@collection": KNOWLEDGE_CHUNK_MENTIONS_ENTITY_EDGE,
                "entity_id": entity_id,
            }),
        )
        .await
        .map_err(|_| ApiError::Internal)?;
    decode_many_results(cursor).map_err(|_| ApiError::Internal)
}

async fn list_entity_evidence_support_edges(
    state: &AppState,
    entity_id: Uuid,
) -> Result<Vec<KnowledgeEvidenceSupportEntityEdgeRow>, ApiError> {
    let cursor = state
        .arango_graph_store
        .client()
        .query_json(
            "FOR edge IN @@collection
             LET evidence = DOCUMENT(edge._from)
             LET entity = DOCUMENT(edge._to)
             LET resolved_rank = edge.rank != null ? edge.rank : edge.payload.rank
             LET resolved_score = edge.score != null ? edge.score : edge.payload.score
             LET resolved_inclusion_reason = edge.inclusionReason != null ? edge.inclusionReason : edge.payload.inclusionReason
             FILTER evidence != null
               AND entity != null
               AND entity.entity_id == @entity_id
             SORT resolved_rank ASC, edge.created_at ASC, edge._key ASC
             RETURN {
                key: edge._key,
                evidenceId: evidence.evidence_id,
                entityId: entity.entity_id,
                rank: resolved_rank,
                score: resolved_score,
                inclusionReason: resolved_inclusion_reason,
                createdAt: edge.created_at
             }",
            serde_json::json!({
                "@collection": KNOWLEDGE_EVIDENCE_SUPPORTS_ENTITY_EDGE,
                "entity_id": entity_id,
            }),
        )
        .await
        .map_err(|_| ApiError::Internal)?;
    decode_many_results(cursor).map_err(|_| ApiError::Internal)
}

async fn list_relation_evidence_support_edges(
    state: &AppState,
    relation_id: Uuid,
) -> Result<Vec<KnowledgeEvidenceSupportRelationEdgeRow>, ApiError> {
    let cursor = state
        .arango_graph_store
        .client()
        .query_json(
            "FOR edge IN @@collection
             LET evidence = DOCUMENT(edge._from)
             LET relation = DOCUMENT(edge._to)
             LET resolved_rank = edge.rank != null ? edge.rank : edge.payload.rank
             LET resolved_score = edge.score != null ? edge.score : edge.payload.score
             LET resolved_inclusion_reason = edge.inclusionReason != null ? edge.inclusionReason : edge.payload.inclusionReason
             FILTER evidence != null
               AND relation != null
               AND relation.relation_id == @relation_id
             SORT resolved_rank ASC, edge.created_at ASC, edge._key ASC
             RETURN {
                key: edge._key,
                evidenceId: evidence.evidence_id,
                relationId: relation.relation_id,
                rank: resolved_rank,
                score: resolved_score,
                inclusionReason: resolved_inclusion_reason,
                createdAt: edge.created_at
             }",
            serde_json::json!({
                "@collection": KNOWLEDGE_EVIDENCE_SUPPORTS_RELATION_EDGE,
                "relation_id": relation_id,
            }),
        )
        .await
        .map_err(|_| ApiError::Internal)?;
    decode_many_results(cursor).map_err(|_| ApiError::Internal)
}

fn decode_many_results<T>(cursor: serde_json::Value) -> Result<Vec<T>, ApiError>
where
    T: for<'de> serde::Deserialize<'de>,
{
    let result = cursor.get("result").cloned().ok_or(ApiError::Internal)?;
    serde_json::from_value(result).map_err(|_| ApiError::Internal)
}
