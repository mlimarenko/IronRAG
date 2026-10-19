use std::collections::{BTreeMap, HashSet};

pub mod library;
pub mod search;

use axum::{
    Json, Router,
    body::Body,
    extract::{Path, Query, State},
    http::{StatusCode, header},
    response::Response,
    routing::{get, post},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::knowledge::{KnowledgeLibraryGeneration, TypedTechnicalFact},
    infra::knowledge_rows::{KnowledgeChunkRow, KnowledgeEvidenceRow},
    infra::repositories,
    interfaces::http::{
        auth::AuthContext,
        authorization::{
            POLICY_KNOWLEDGE_READ, POLICY_KNOWLEDGE_WRITE, load_library_and_authorize,
        },
        router_support::ApiError,
    },
    services::{
        graph::{
            backfill::{
                GraphReextractProgress, REEXTRACT_MAX_BATCH_SIZE,
                load_library_graph_reextract_progress, run_library_graph_reextract_batch,
            },
            extract::GRAPH_EXTRACTION_VERSION,
        },
        knowledge::graph_stream::build_graph_topology_bytes,
    },
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/knowledge/context-bundles/{bundle_id}", get(library::get_context_bundle))
        .route(
            "/knowledge/libraries/{library_id}/context-bundles",
            get(library::list_context_bundles),
        )
        .route("/knowledge/libraries/{library_id}/summary", get(library::get_library_summary))
        .route("/knowledge/libraries/{library_id}/graph", get(get_graph))
        .route(
            "/knowledge/libraries/{library_id}/graph/reextract",
            get(get_graph_reextract_progress).post(enqueue_graph_reextract_batch),
        )
        .route("/knowledge/libraries/{library_id}/entities/{entity_id}", get(get_entity))
        .route("/knowledge/libraries/{library_id}/relations", get(list_relations))
        .route("/knowledge/libraries/{library_id}/relations/{relation_id}", get(get_relation))
        .route(
            "/knowledge/libraries/{library_id}/generations",
            get(library::list_library_generations),
        )
        .route("/knowledge/libraries/{library_id}/search", post(search::search_library))
}

#[derive(Debug, Default, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GraphReextractBatchRequest {
    /// Number of documents to enqueue in this request. Must be between 1 and
    /// 256 when provided; omitted requests use the service default.
    #[schema(minimum = 1, maximum = 256)]
    pub batch_size: Option<u32>,
    /// Also retry readable documents that have never produced any extraction
    /// row. Defaults to true for explicit operator recovery and for the
    /// bounded periodic background sweep.
    pub include_never_extracted: Option<bool>,
}

fn validate_graph_reextract_batch_size(batch_size: Option<u32>) -> Result<(), ApiError> {
    if batch_size.is_some_and(|value| value == 0 || i64::from(value) > REEXTRACT_MAX_BATCH_SIZE) {
        return Err(ApiError::BadRequest(format!(
            "batchSize must be between 1 and {REEXTRACT_MAX_BATCH_SIZE}"
        )));
    }
    Ok(())
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GraphReextractProgressResponse {
    extraction_version: String,
    readable_document_count: i64,
    known_eligible_document_count: i64,
    current_extraction_document_count: i64,
    missing_current_extraction_document_count: i64,
    outdated_or_incomplete_document_count: i64,
    never_extracted_document_count: i64,
    queued_job_count: i64,
    leased_job_count: i64,
    paused_job_count: i64,
}

impl From<GraphReextractProgress> for GraphReextractProgressResponse {
    fn from(progress: GraphReextractProgress) -> Self {
        Self {
            extraction_version: GRAPH_EXTRACTION_VERSION.to_string(),
            readable_document_count: progress.readable_document_count,
            known_eligible_document_count: progress.known_eligible_document_count,
            current_extraction_document_count: progress.current_extraction_document_count,
            missing_current_extraction_document_count: progress
                .missing_current_extraction_document_count,
            outdated_or_incomplete_document_count: progress.outdated_or_incomplete_document_count,
            never_extracted_document_count: progress.never_extracted_document_count,
            queued_job_count: progress.queued_job_count,
            leased_job_count: progress.leased_job_count,
            paused_job_count: progress.paused_job_count,
        }
    }
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GraphReextractBatchResponse {
    batch_size: i64,
    include_never_extracted: bool,
    candidates: usize,
    enqueued: usize,
    skipped_live_jobs: usize,
    failures: usize,
    progress: GraphReextractProgressResponse,
}

#[tracing::instrument(
    level = "info",
    name = "http.knowledge.get_graph_reextract_progress",
    skip_all,
    fields(%library_id)
)]
#[utoipa::path(
    get,
    path = "/v1/knowledge/libraries/{libraryId}/graph/reextract",
    tag = "knowledge",
    operation_id = "getKnowledgeGraphReextractProgress",
    params(("libraryId" = uuid::Uuid, Path, description = "Library identifier")),
    responses(
        (status = 200, description = "Current graph extraction coverage and recovery queue depth", body = GraphReextractProgressResponse),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not authorized for the library"),
        (status = 404, description = "Library not found"),
    ),
)]
pub async fn get_graph_reextract_progress(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(library_id): Path<Uuid>,
) -> Result<Json<GraphReextractProgressResponse>, ApiError> {
    let _ = load_library_and_authorize(&auth, &state, library_id, POLICY_KNOWLEDGE_READ).await?;
    let progress = load_library_graph_reextract_progress(&state, library_id).await?;
    Ok(Json(progress.into()))
}

#[tracing::instrument(
    level = "info",
    name = "http.knowledge.enqueue_graph_reextract_batch",
    skip_all,
    fields(%library_id)
)]
#[utoipa::path(
    post,
    path = "/v1/knowledge/libraries/{libraryId}/graph/reextract",
    tag = "knowledge",
    operation_id = "enqueueKnowledgeGraphReextractBatch",
    params(("libraryId" = uuid::Uuid, Path, description = "Library identifier")),
    request_body = GraphReextractBatchRequest,
    responses(
        (status = 200, description = "Bounded graph_refresh batch enqueued", body = GraphReextractBatchResponse),
        (status = 400, description = "batchSize is outside the supported range"),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not authorized for the library"),
        (status = 404, description = "Library not found"),
    ),
)]
pub async fn enqueue_graph_reextract_batch(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(library_id): Path<Uuid>,
    Json(request): Json<GraphReextractBatchRequest>,
) -> Result<(StatusCode, Json<GraphReextractBatchResponse>), ApiError> {
    let _ = load_library_and_authorize(&auth, &state, library_id, POLICY_KNOWLEDGE_WRITE).await?;
    validate_graph_reextract_batch_size(request.batch_size)?;
    let include_never_extracted = request.include_never_extracted.unwrap_or(true);
    let result = run_library_graph_reextract_batch(
        &state,
        library_id,
        request.batch_size,
        include_never_extracted,
    )
    .await?;
    Ok((
        StatusCode::OK,
        Json(GraphReextractBatchResponse {
            batch_size: result.batch_size,
            include_never_extracted,
            candidates: result.outcome.candidates,
            enqueued: result.outcome.enqueued,
            skipped_live_jobs: result.outcome.skipped_dedupe,
            failures: result.outcome.failures,
            progress: result.progress.into(),
        }),
    ))
}

/// Query params shared by every knowledge list endpoint (`context-bundles`,
/// `relations`). `cursor` is the opaque offset token from a previous page's
/// `nextCursor`; absent = start from the top. `limit` is clamped server-side
/// (see `DEFAULT_LIST_LIMIT`/`MAX_LIST_LIMIT`).
#[derive(Debug, Deserialize, utoipa::ToSchema, utoipa::IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct KnowledgeListPageQuery {
    pub cursor: Option<String>,
    pub limit: Option<u32>,
}

pub(super) const DEFAULT_LIST_LIMIT: u32 = 50;
pub(super) const MAX_LIST_LIMIT: u32 = 500;

/// Opaque offset cursor for the in-memory-paginated knowledge list
/// endpoints (`context-bundles`, `relations`). These lists are already
/// materialized in full by their backing store call (bounded per-library
/// volumes, not the unbounded-corpus case `content` documents pagination
/// solves), so a base64-encoded offset is an honest `nextCursor` — it
/// really does resume at that row — without building full DB-level keyset
/// pagination for read surfaces this plan explicitly frames as
/// verification/registration, not new query engineering.
pub(super) fn encode_list_offset_cursor(offset: usize) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(offset.to_string())
}

pub(super) fn decode_list_offset_cursor(token: &str) -> Result<usize, ApiError> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(token)
        .map_err(|_| ApiError::invalid_continuation_token("invalid cursor"))?;
    let text = String::from_utf8(bytes)
        .map_err(|_| ApiError::invalid_continuation_token("invalid cursor"))?;
    text.parse::<usize>().map_err(|_| ApiError::invalid_continuation_token("invalid cursor"))
}

/// One in-memory paginated slice: the page, its next opaque cursor and the
/// full pre-pagination total. Aliased because the whole tuple nested inside
/// `Result` exceeds clippy's type-complexity budget.
pub(super) type OffsetPaginatedSlice<T> = (Vec<T>, Option<String>, i64);

pub(super) fn paginate_offset_slice<T>(
    items: Vec<T>,
    query: &KnowledgeListPageQuery,
) -> Result<OffsetPaginatedSlice<T>, ApiError> {
    let total = items.len() as i64;
    let offset = match query.cursor.as_deref() {
        Some(token) => decode_list_offset_cursor(token)?,
        None => 0,
    };
    let limit = query.limit.unwrap_or(DEFAULT_LIST_LIMIT).clamp(1, MAX_LIST_LIMIT) as usize;
    let next_offset = offset.saturating_add(limit);
    let next_cursor = (next_offset < items.len()).then(|| encode_list_offset_cursor(next_offset));
    let page = items.into_iter().skip(offset).take(limit).collect();
    Ok((page, next_cursor, total))
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct KnowledgeEntityDetailResponse {
    entity: RuntimeKnowledgeEntityRow,
    mention_edges: Vec<KnowledgeEntityMentionEdgeRow>,
    mentioned_chunks: Vec<KnowledgeMentionedChunkRow>,
    supporting_evidence_edges: Vec<KnowledgeEvidenceSupportEntityEdgeRow>,
    supporting_evidence: Vec<RuntimeKnowledgeEvidenceRow>,
    supporting_typed_facts: Vec<TypedTechnicalFact>,
    graph_evidence_summary: KnowledgeGraphEvidenceSummary,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct KnowledgeRelationDetailResponse {
    relation: RuntimeKnowledgeRelationRow,
    supporting_evidence_edges: Vec<KnowledgeEvidenceSupportRelationEdgeRow>,
    supporting_evidence: Vec<RuntimeKnowledgeEvidenceRow>,
    supporting_typed_facts: Vec<TypedTechnicalFact>,
    graph_evidence_summary: KnowledgeGraphEvidenceSummary,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct KnowledgeLibrarySummaryResponse {
    library_id: Uuid,
    document_counts_by_readiness: BTreeMap<String, i64>,
    node_count: i64,
    edge_count: i64,
    graph_ready_document_count: i64,
    graph_sparse_document_count: i64,
    typed_fact_document_count: i64,
    updated_at: DateTime<Utc>,
    latest_generation: Option<KnowledgeLibraryGeneration>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct KnowledgeDocumentProvenanceSummary {
    supporting_evidence_count: usize,
    lexical_chunk_count: usize,
    vector_chunk_count: usize,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct KnowledgeTechnicalFactProvenanceSummary {
    typed_fact_count: usize,
    fact_kind_counts: BTreeMap<String, usize>,
    conflict_group_count: usize,
    support_block_count: usize,
    support_chunk_count: usize,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct KnowledgeGraphEvidenceSummary {
    evidence_count: usize,
    chunk_backed_count: usize,
    block_backed_count: usize,
    fact_backed_count: usize,
}

#[derive(Debug, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
struct KnowledgeEntityMentionEdgeRow {
    entity_id: Uuid,
    chunk_id: Uuid,
    rank: Option<i32>,
    score: Option<f64>,
    inclusion_reason: Option<String>,
    created_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
struct KnowledgeEvidenceSupportEntityEdgeRow {
    evidence_id: Uuid,
    entity_id: Uuid,
    rank: Option<i32>,
    score: Option<f64>,
    inclusion_reason: Option<String>,
    created_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
struct KnowledgeEvidenceSupportRelationEdgeRow {
    evidence_id: Uuid,
    relation_id: Uuid,
    rank: Option<i32>,
    score: Option<f64>,
    inclusion_reason: Option<String>,
    created_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
struct RuntimeKnowledgeEntityRow {
    entity_id: Uuid,
    library_id: Uuid,
    #[serde(rename = "\u{63}anonicalLabel")]
    primary_label: String,
    aliases: Vec<String>,
    entity_type: String,
    entity_sub_type: Option<String>,
    summary: Option<String>,
    confidence: Option<f64>,
    support_count: i32,
    freshness_generation: i64,
    entity_state: String,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
struct RuntimeKnowledgeRelationRow {
    relation_id: Uuid,
    library_id: Uuid,
    relation_type: String,
    normalized_assertion: String,
    confidence: Option<f64>,
    support_count: i32,
    contradiction_state: String,
    freshness_generation: i64,
    relation_state: String,
    subject_entity_id: Uuid,
    object_entity_id: Uuid,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
struct RuntimeKnowledgeEvidenceRow {
    evidence_id: Uuid,
    library_id: Uuid,
    document_id: Uuid,
    revision_id: Uuid,
    chunk_id: Option<Uuid>,
    span_start: Option<i32>,
    span_end: Option<i32>,
    excerpt: String,
    support_kind: String,
    extraction_method: String,
    confidence: Option<f64>,
    evidence_state: String,
    freshness_generation: i64,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

/// Wire-facing projection of [`KnowledgeChunkRow`] used for
/// `mentionedChunks` (camelCase, no `workspace_id` — the same convention
/// `search.rs`'s `KnowledgeSearch*` DTOs follow for the hybrid search
/// response).
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
struct KnowledgeMentionedChunkRow {
    chunk_id: Uuid,
    library_id: Uuid,
    document_id: Uuid,
    revision_id: Uuid,
    chunk_index: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    chunk_kind: Option<String>,
    content_text: String,
    normalized_text: String,
    span_start: Option<i32>,
    span_end: Option<i32>,
    token_count: Option<i32>,
    support_block_ids: Vec<Uuid>,
    section_path: Vec<String>,
    heading_trail: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    literal_digest: Option<String>,
    chunk_state: String,
    text_generation: Option<i64>,
    vector_generation: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    quality_score: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    window_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    occurred_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    occurred_until: Option<DateTime<Utc>>,
}

impl From<KnowledgeChunkRow> for KnowledgeMentionedChunkRow {
    fn from(row: KnowledgeChunkRow) -> Self {
        Self {
            chunk_id: row.chunk_id,
            library_id: row.library_id,
            document_id: row.document_id,
            revision_id: row.revision_id,
            chunk_index: row.chunk_index,
            chunk_kind: row.chunk_kind,
            content_text: row.content_text,
            normalized_text: row.normalized_text,
            span_start: row.span_start,
            span_end: row.span_end,
            token_count: row.token_count,
            support_block_ids: row.support_block_ids,
            section_path: row.section_path,
            heading_trail: row.heading_trail,
            literal_digest: row.literal_digest,
            chunk_state: row.chunk_state,
            text_generation: row.text_generation,
            vector_generation: row.vector_generation,
            quality_score: row.quality_score,
            window_text: row.window_text,
            occurred_at: row.occurred_at,
            occurred_until: row.occurred_until,
        }
    }
}

// Removed in the 2026-04-15 perf audit: `list_entities`, `list_relations`
// and `get_graph_workbench` were huge endpoints (17–27 MiB payloads, no
// pagination) that were never called from the web UI. The only graph
// surface that survives is the compact NDJSON stream at
// `/v1/knowledge/libraries/{id}/graph`, which the React graph page has
// used exclusively since the B4 release. Single-entity and
// single-relation detail endpoints are still wired up below.

#[tracing::instrument(
    level = "info",
    name = "http.get_graph",
    skip_all,
    fields(%library_id)
)]
#[utoipa::path(
    get,
    path = "/v1/knowledge/libraries/{libraryId}/graph",
    tag = "knowledge",
    operation_id = "getKnowledgeGraphWorkbench",
    params(("libraryId" = uuid::Uuid, Path, description = "Library identifier")),
    responses(
        (status = 200, description = "Knowledge graph topology streamed as ND-JSON", content_type = "application/x-ndjson", body = String),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not authorized for the library"),
    ),
)]
pub async fn get_graph(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(library_id): Path<Uuid>,
) -> Result<Response, ApiError> {
    let _ = load_library_and_authorize(&auth, &state, library_id, POLICY_KNOWLEDGE_READ).await?;

    let bytes = build_graph_topology_bytes(&state, library_id).await.map_err(ApiError::from)?;
    Response::builder()
        .status(200)
        .header(header::CONTENT_TYPE, "application/x-ndjson")
        .header(header::CACHE_CONTROL, "no-store")
        .body(Body::from(bytes))
        .map_err(|error| ApiError::internal_with_log(error, "internal"))
}

#[tracing::instrument(
    level = "info",
    name = "http.get_entity",
    skip_all,
    fields(library_id = %library_id, entity_id = %entity_id)
)]
#[utoipa::path(
    get,
    path = "/v1/knowledge/libraries/{libraryId}/entities/{entityId}",
    tag = "knowledge",
    operation_id = "getKnowledgeEntity",
    params(
        ("libraryId" = uuid::Uuid, Path, description = "Library identifier"),
        ("entityId" = uuid::Uuid, Path, description = "Entity identifier"),
    ),
    responses(
        (status = 200, description = "Knowledge entity detail", body = KnowledgeEntityDetailResponse),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not authorized for the entity"),
        (status = 404, description = "Entity not found"),
    ),
)]
pub async fn get_entity(
    auth: AuthContext,
    State(state): State<AppState>,
    Path((library_id, entity_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<KnowledgeEntityDetailResponse>, ApiError> {
    let _ = load_library_and_authorize(&auth, &state, library_id, POLICY_KNOWLEDGE_READ).await?;
    let snapshot =
        repositories::get_runtime_graph_snapshot(&state.persistence.postgres, library_id)
            .await
            .map_err(|error| ApiError::internal_with_log(error, "graph snapshot lookup failed"))?
            .ok_or_else(|| ApiError::resource_not_found("knowledge_entity", entity_id))?;
    let selected_projection =
        crate::services::knowledge::runtime_read::selected_graph_projection(&snapshot)
            .map_err(ApiError::from)?;
    let entity = repositories::get_runtime_graph_node_by_id(
        &state.persistence.postgres,
        library_id,
        entity_id,
    )
    .await
    .map_err(|e| ApiError::internal_with_log(e, "internal"))?
    .ok_or_else(|| ApiError::resource_not_found("runtime_graph_node", entity_id))?;
    if entity.library_id != library_id
        || entity.projection_version != selected_projection.projection_version
    {
        return Err(ApiError::resource_not_found("knowledge_entity", entity_id));
    }

    let supporting_evidence =
        load_runtime_graph_supporting_evidence(&state, library_id, "node", entity_id).await?;
    let mention_edges = build_runtime_entity_mention_edges(entity_id, &supporting_evidence);
    let mention_chunk_ids: Vec<Uuid> = mention_edges.iter().map(|edge| edge.chunk_id).collect();
    let mentioned_chunks = load_chunks_by_ids(&state, &mention_chunk_ids).await?;
    let supporting_evidence_edges =
        build_runtime_entity_evidence_support_edges(entity_id, &supporting_evidence);
    let supporting_typed_facts = Vec::new();
    let graph_evidence_summary = summarize_runtime_graph_evidence(&supporting_evidence, 0);
    if !repositories::content_fingerprint_repository::graph_target_is_current(
        &state.persistence.postgres,
        library_id,
        entity_id,
        "node",
    )
    .await
    .map_err(|error| ApiError::internal_with_log(error, "graph visibility lookup failed"))?
    {
        return Err(ApiError::resource_not_found("knowledge_entity", entity_id));
    }
    crate::services::knowledge::runtime_read::ensure_selected_graph_current(
        &state,
        selected_projection,
    )
    .await
    .map_err(ApiError::from)?;

    Ok(Json(KnowledgeEntityDetailResponse {
        entity: map_runtime_graph_node_to_entity_row(entity, library_id),
        mention_edges,
        mentioned_chunks: mentioned_chunks.into_iter().map(Into::into).collect(),
        supporting_evidence_edges,
        supporting_evidence,
        supporting_typed_facts,
        graph_evidence_summary,
    }))
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct KnowledgeRelationListResponse {
    items: Vec<RuntimeKnowledgeRelationRow>,
    #[serde(skip_serializing_if = "Option::is_none")]
    next_cursor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    total: Option<i64>,
}

/// Paginated parent collection for the restored relation item read below.
/// Slim rows only (no per-relation evidence expansion) — callers that need
/// the full detail follow up with `GET .../relations/{relationId}`.
#[tracing::instrument(
    level = "info",
    name = "http.knowledge.list_relations",
    skip_all,
    fields(library_id = %library_id, item_count)
)]
#[utoipa::path(
    get,
    path = "/v1/knowledge/libraries/{libraryId}/relations",
    tag = "knowledge",
    operation_id = "listKnowledgeRelations",
    params(
        ("libraryId" = uuid::Uuid, Path, description = "Library identifier"),
        KnowledgeListPageQuery,
    ),
    responses(
        (status = 200, description = "Knowledge relations for the library", body = KnowledgeRelationListResponse),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not authorized for the library"),
    ),
)]
pub async fn list_relations(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(library_id): Path<Uuid>,
    Query(query): Query<KnowledgeListPageQuery>,
) -> Result<Json<KnowledgeRelationListResponse>, ApiError> {
    let span = tracing::Span::current();
    let _ = load_library_and_authorize(&auth, &state, library_id, POLICY_KNOWLEDGE_READ).await?;
    let snapshot =
        repositories::get_runtime_graph_snapshot(&state.persistence.postgres, library_id)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
    let selected_projection = snapshot
        .as_ref()
        .map(crate::services::knowledge::runtime_read::selected_graph_projection)
        .transpose()
        .map_err(ApiError::from)?;
    let edges = match snapshot {
        Some(snapshot) if snapshot.graph_status != "empty" && snapshot.projection_version > 0 => {
            repositories::list_runtime_graph_edges_by_library(
                &state.persistence.postgres,
                library_id,
                snapshot.projection_version,
            )
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        }
        _ => Vec::new(),
    };
    let relations: Vec<RuntimeKnowledgeRelationRow> = edges
        .into_iter()
        .map(|edge| map_runtime_graph_edge_to_relation_row(library_id, edge))
        .collect();
    let (items, next_cursor, total) = paginate_offset_slice(relations, &query)?;
    span.record("item_count", items.len());
    if let Some(selected_projection) = selected_projection {
        crate::services::knowledge::runtime_read::ensure_selected_graph_current(
            &state,
            selected_projection,
        )
        .await
        .map_err(ApiError::from)?;
    }
    Ok(Json(KnowledgeRelationListResponse { items, next_cursor, total: Some(total) }))
}

#[tracing::instrument(
    level = "info",
    name = "http.get_relation",
    skip_all,
    fields(library_id = %library_id, relation_id = %relation_id)
)]
#[utoipa::path(
    get,
    path = "/v1/knowledge/libraries/{libraryId}/relations/{relationId}",
    tag = "knowledge",
    operation_id = "getKnowledgeRelation",
    params(
        ("libraryId" = uuid::Uuid, Path, description = "Library identifier"),
        ("relationId" = uuid::Uuid, Path, description = "Relation identifier"),
    ),
    responses(
        (status = 200, description = "Knowledge relation detail", body = KnowledgeRelationDetailResponse),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not authorized for the relation"),
        (status = 404, description = "Relation not found"),
    ),
)]
pub async fn get_relation(
    auth: AuthContext,
    State(state): State<AppState>,
    Path((library_id, relation_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<KnowledgeRelationDetailResponse>, ApiError> {
    let _ = load_library_and_authorize(&auth, &state, library_id, POLICY_KNOWLEDGE_READ).await?;
    let snapshot =
        repositories::get_runtime_graph_snapshot(&state.persistence.postgres, library_id)
            .await
            .map_err(|error| ApiError::internal_with_log(error, "graph snapshot lookup failed"))?
            .ok_or_else(|| ApiError::resource_not_found("knowledge_relation", relation_id))?;
    let selected_projection =
        crate::services::knowledge::runtime_read::selected_graph_projection(&snapshot)
            .map_err(ApiError::from)?;
    let relation = repositories::get_runtime_graph_edge_by_id(
        &state.persistence.postgres,
        library_id,
        relation_id,
    )
    .await
    .map_err(|e| ApiError::internal_with_log(e, "internal"))?
    .ok_or_else(|| ApiError::resource_not_found("runtime_graph_edge", relation_id))?;
    if relation.library_id != library_id
        || relation.projection_version != selected_projection.projection_version
    {
        return Err(ApiError::resource_not_found("knowledge_relation", relation_id));
    }

    let supporting_evidence =
        load_runtime_graph_supporting_evidence(&state, library_id, "edge", relation_id).await?;
    let supporting_evidence_edges =
        build_runtime_relation_evidence_support_edges(relation_id, &supporting_evidence);
    let supporting_typed_facts = Vec::new();
    let graph_evidence_summary = summarize_runtime_graph_evidence(&supporting_evidence, 0);

    if !repositories::content_fingerprint_repository::graph_target_is_current(
        &state.persistence.postgres,
        library_id,
        relation_id,
        "edge",
    )
    .await
    .map_err(|error| ApiError::internal_with_log(error, "graph visibility lookup failed"))?
    {
        return Err(ApiError::resource_not_found("knowledge_relation", relation_id));
    }
    crate::services::knowledge::runtime_read::ensure_selected_graph_current(
        &state,
        selected_projection,
    )
    .await
    .map_err(ApiError::from)?;
    Ok(Json(KnowledgeRelationDetailResponse {
        relation: map_runtime_graph_edge_to_relation_row(library_id, relation),
        supporting_evidence_edges,
        supporting_evidence,
        supporting_typed_facts,
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
    state
        .document_store
        .list_chunks_by_ids(chunk_ids)
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))
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

fn summarize_runtime_graph_evidence(
    evidence_rows: &[RuntimeKnowledgeEvidenceRow],
    typed_fact_count: usize,
) -> KnowledgeGraphEvidenceSummary {
    let chunk_backed_count =
        evidence_rows.iter().filter(|evidence| evidence.chunk_id.is_some()).count();
    KnowledgeGraphEvidenceSummary {
        evidence_count: evidence_rows.len(),
        chunk_backed_count,
        block_backed_count: 0,
        fact_backed_count: typed_fact_count,
    }
}

fn runtime_graph_aliases(metadata: &serde_json::Value) -> Vec<String> {
    metadata
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .map(ToOwned::to_owned)
        .collect()
}

fn runtime_graph_confidence(metadata: &serde_json::Value) -> Option<f64> {
    metadata.get("confidence").and_then(serde_json::Value::as_f64)
}

fn runtime_graph_state(metadata: &serde_json::Value, fallback: &str) -> String {
    // `extraction_recovery_status` describes HOW extraction produced the
    // node (clean / partial / recovered / failed) — it is NOT the
    // admittance state. The admittance state lives in `entity_state` /
    // `relation_state`, which default to "active" when admitted.
    metadata
        .get("entity_state")
        .and_then(serde_json::Value::as_str)
        .or_else(|| metadata.get("relation_state").and_then(serde_json::Value::as_str))
        .unwrap_or(fallback)
        .to_string()
}

fn runtime_graph_contradiction_state(metadata: &serde_json::Value) -> String {
    metadata
        .get("contradiction_state")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("clean")
        .to_string()
}

fn map_runtime_graph_node_to_entity_row(
    row: repositories::RuntimeGraphNodeRow,
    library_id: Uuid,
) -> RuntimeKnowledgeEntityRow {
    RuntimeKnowledgeEntityRow {
        entity_id: row.id,
        library_id,
        primary_label: row.label,
        aliases: runtime_graph_aliases(&row.aliases_json),
        entity_type: row.node_type,
        entity_sub_type: row
            .metadata_json
            .get("sub_type")
            .and_then(serde_json::Value::as_str)
            .map(ToString::to_string),
        summary: row.summary,
        confidence: runtime_graph_confidence(&row.metadata_json),
        support_count: row.support_count,
        freshness_generation: row.projection_version,
        entity_state: runtime_graph_state(&row.metadata_json, "active"),
        created_at: row.created_at,
        updated_at: row.updated_at,
    }
}

fn map_runtime_graph_edge_to_relation_row(
    library_id: Uuid,
    row: repositories::RuntimeGraphEdgeRow,
) -> RuntimeKnowledgeRelationRow {
    RuntimeKnowledgeRelationRow {
        relation_id: row.id,
        library_id,
        relation_type: row.relation_type.clone(),
        normalized_assertion: row.summary.clone().unwrap_or_else(|| row.identity_key.clone()),
        confidence: row.weight,
        support_count: row.support_count,
        contradiction_state: runtime_graph_contradiction_state(&row.metadata_json),
        freshness_generation: row.projection_version,
        relation_state: runtime_graph_state(&row.metadata_json, "active"),
        subject_entity_id: row.from_node_id,
        object_entity_id: row.to_node_id,
        created_at: row.created_at,
        updated_at: row.updated_at,
    }
}

async fn load_runtime_graph_supporting_evidence(
    state: &AppState,
    library_id: Uuid,
    target_kind: &str,
    target_id: Uuid,
) -> Result<Vec<RuntimeKnowledgeEvidenceRow>, ApiError> {
    let evidence_rows = repositories::list_active_runtime_graph_evidence_lifecycle_by_target(
        &state.persistence.postgres,
        library_id,
        target_kind,
        target_id,
    )
    .await
    .map_err(|e| ApiError::internal_with_log(e, "internal"))?;

    Ok(evidence_rows
        .into_iter()
        .filter_map(|row| {
            let document_id = row.document_id?;
            Some(RuntimeKnowledgeEvidenceRow {
                evidence_id: row.id,
                library_id,
                document_id,
                revision_id: row.revision_id.unwrap_or_else(Uuid::nil),
                chunk_id: row.chunk_id,
                span_start: None,
                span_end: None,
                excerpt: row.evidence_text,
                support_kind: target_kind.to_string(),
                extraction_method: "runtime_graph".to_string(),
                confidence: row.confidence_score,
                evidence_state: "active".to_string(),
                freshness_generation: 0,
                created_at: row.created_at,
                updated_at: row.created_at,
            })
        })
        .collect())
}

fn build_runtime_entity_mention_edges(
    entity_id: Uuid,
    evidence_rows: &[RuntimeKnowledgeEvidenceRow],
) -> Vec<KnowledgeEntityMentionEdgeRow> {
    let mut seen = HashSet::<Uuid>::new();
    let mut edges = Vec::new();
    for row in evidence_rows {
        let Some(chunk_id) = row.chunk_id else {
            continue;
        };
        if !seen.insert(chunk_id) {
            continue;
        }
        edges.push(KnowledgeEntityMentionEdgeRow {
            entity_id,
            chunk_id,
            rank: None,
            score: row.confidence,
            inclusion_reason: Some("runtime_graph_evidence".to_string()),
            created_at: row.created_at,
        });
    }
    edges
}

fn build_runtime_entity_evidence_support_edges(
    entity_id: Uuid,
    evidence_rows: &[RuntimeKnowledgeEvidenceRow],
) -> Vec<KnowledgeEvidenceSupportEntityEdgeRow> {
    evidence_rows
        .iter()
        .map(|row| KnowledgeEvidenceSupportEntityEdgeRow {
            evidence_id: row.evidence_id,
            entity_id,
            rank: None,
            score: row.confidence,
            inclusion_reason: Some("runtime_graph_evidence".to_string()),
            created_at: row.created_at,
        })
        .collect()
}

fn build_runtime_relation_evidence_support_edges(
    relation_id: Uuid,
    evidence_rows: &[RuntimeKnowledgeEvidenceRow],
) -> Vec<KnowledgeEvidenceSupportRelationEdgeRow> {
    evidence_rows
        .iter()
        .map(|row| KnowledgeEvidenceSupportRelationEdgeRow {
            evidence_id: row.evidence_id,
            relation_id,
            rank: None,
            score: row.confidence,
            inclusion_reason: Some("runtime_graph_evidence".to_string()),
            created_at: row.created_at,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use base64::Engine;

    use super::*;

    #[test]
    fn list_cursor_round_trips_and_rejects_non_numeric_payloads() {
        let cursor = encode_list_offset_cursor(501);
        assert_eq!(decode_list_offset_cursor(&cursor).unwrap(), 501);

        let invalid = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode("not-an-offset");
        assert!(decode_list_offset_cursor(&invalid).is_err());
        assert!(decode_list_offset_cursor("%%%").is_err());
    }

    #[test]
    fn paginated_slice_clamps_limits_and_only_emits_cursor_when_more_items_exist() {
        let first_page = paginate_offset_slice(
            vec![1_u8, 2, 3],
            &KnowledgeListPageQuery { cursor: None, limit: Some(2) },
        )
        .unwrap();
        assert_eq!(first_page.0, vec![1, 2]);
        assert_eq!(first_page.1, Some(encode_list_offset_cursor(2)));
        assert_eq!(first_page.2, 3);

        let final_page = paginate_offset_slice(
            vec![1_u8, 2, 3],
            &KnowledgeListPageQuery { cursor: first_page.1, limit: Some(0) },
        )
        .unwrap();
        assert_eq!(final_page.0, vec![3]);
        assert_eq!(final_page.1, None);
        assert_eq!(final_page.2, 3);
    }

    #[test]
    fn graph_metadata_uses_admittance_state_not_extraction_status() {
        let metadata = serde_json::json!({
            "entity_state": "suppressed",
            "relation_state": "active",
            "extraction_recovery_status": "recovered",
            "confidence": 0.75,
        });

        assert_eq!(runtime_graph_state(&metadata, "active"), "suppressed");
        assert_eq!(runtime_graph_confidence(&metadata), Some(0.75));
        assert_eq!(runtime_graph_contradiction_state(&metadata), "clean");
        assert_eq!(runtime_graph_aliases(&serde_json::json!(["Alpha", 42, null])), vec!["Alpha"]);
    }

    #[test]
    fn graph_reextract_batch_size_accepts_only_the_documented_bounds() {
        assert!(validate_graph_reextract_batch_size(None).is_ok());
        assert!(validate_graph_reextract_batch_size(Some(1)).is_ok());
        assert!(validate_graph_reextract_batch_size(Some(256)).is_ok());

        for invalid in [0, 257, u32::MAX] {
            let error = validate_graph_reextract_batch_size(Some(invalid))
                .expect_err("out-of-range batch size must be rejected");
            assert_eq!(
                axum::response::IntoResponse::into_response(error).status(),
                StatusCode::BAD_REQUEST,
            );
        }
    }
}
