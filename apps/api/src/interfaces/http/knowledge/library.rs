use axum::{
    Json,
    extract::{Path, Query, State},
};
use serde::Serialize;
use uuid::Uuid;

use super::{KnowledgeLibrarySummaryResponse, KnowledgeListPageQuery, paginate_offset_slice};
use crate::{
    app::state::AppState,
    domains::knowledge::KnowledgeLibraryGeneration,
    infra::knowledge_rows::{
        KnowledgeBundleChunkReferenceRow, KnowledgeBundleEntityReferenceRow,
        KnowledgeBundleEvidenceReferenceRow, KnowledgeBundleRelationReferenceRow,
        KnowledgeContextBundleRow, KnowledgeRetrievalTraceRow,
    },
    interfaces::http::{
        auth::AuthContext,
        authorization::{POLICY_KNOWLEDGE_READ, load_library_and_authorize},
        router_support::ApiError,
    },
};

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct KnowledgeContextBundleDetailResponse {
    bundle: KnowledgeContextBundleRow,
    traces: Vec<KnowledgeRetrievalTraceRow>,
    chunk_references: Vec<KnowledgeBundleChunkReferenceRow>,
    entity_references: Vec<KnowledgeBundleEntityReferenceRow>,
    relation_references: Vec<KnowledgeBundleRelationReferenceRow>,
    evidence_references: Vec<KnowledgeBundleEvidenceReferenceRow>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct KnowledgeContextBundleListResponse {
    items: Vec<KnowledgeContextBundleRow>,
    #[serde(skip_serializing_if = "Option::is_none")]
    next_cursor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    total: Option<i64>,
}

#[utoipa::path(
    get,
    path = "/v1/knowledge/libraries/{libraryId}/context-bundles",
    tag = "knowledge",
    operation_id = "listKnowledgeContextBundles",
    params(
        ("libraryId" = uuid::Uuid, Path, description = "Library identifier"),
        KnowledgeListPageQuery,
    ),
    responses(
        (status = 200, description = "Context bundles for the library", body = KnowledgeContextBundleListResponse),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not authorized for the library"),
    ),
)]
#[tracing::instrument(
    level = "info",
    name = "http.knowledge.list_context_bundles",
    skip_all,
    fields(library_id = %library_id, item_count)
)]
pub async fn list_context_bundles(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(library_id): Path<Uuid>,
    Query(query): Query<KnowledgeListPageQuery>,
) -> Result<Json<KnowledgeContextBundleListResponse>, ApiError> {
    let span = tracing::Span::current();
    let _ = load_library_and_authorize(&auth, &state, library_id, POLICY_KNOWLEDGE_READ).await?;
    let bundles = state
        .context_store
        .list_bundles_by_library(library_id)
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
    let (items, next_cursor, total) = paginate_offset_slice(bundles, &query)?;
    span.record("item_count", items.len());
    Ok(Json(KnowledgeContextBundleListResponse { items, next_cursor, total: Some(total) }))
}

#[tracing::instrument(
    level = "info",
    name = "http.knowledge.get_library_summary",
    skip_all,
    fields(library_id = %library_id)
)]
#[utoipa::path(
    get,
    path = "/v1/knowledge/libraries/{libraryId}/summary",
    tag = "knowledge",
    operation_id = "getKnowledgeLibrarySummary",
    params(("libraryId" = uuid::Uuid, Path, description = "Library identifier")),
    responses(
        (status = 200, description = "Knowledge library summary", body = KnowledgeLibrarySummaryResponse),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not authorized for the library"),
    ),
)]
pub async fn get_library_summary(
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
        node_count: summary.node_count,
        edge_count: summary.edge_count,
        graph_ready_document_count: summary.graph_ready_document_count,
        graph_sparse_document_count: summary.graph_sparse_document_count,
        typed_fact_document_count: summary.typed_fact_document_count,
        updated_at: summary.updated_at,
        latest_generation: summary.latest_generation,
    }))
}

#[tracing::instrument(
    level = "info",
    name = "http.get_context_bundle",
    skip_all,
    fields(bundle_id = %bundle_id)
)]
#[utoipa::path(
    get,
    path = "/v1/knowledge/context-bundles/{bundleId}",
    tag = "knowledge",
    operation_id = "getKnowledgeContextBundle",
    params(("bundleId" = uuid::Uuid, Path, description = "Context bundle identifier")),
    responses(
        (status = 200, description = "Context bundle detail", body = KnowledgeContextBundleDetailResponse),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not authorized for the bundle"),
        (status = 404, description = "Bundle not found"),
    ),
)]
pub async fn get_context_bundle(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(bundle_id): Path<Uuid>,
) -> Result<Json<KnowledgeContextBundleDetailResponse>, ApiError> {
    let bundle_set = state
        .context_store
        .get_bundle_reference_set(bundle_id)
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        .ok_or_else(|| ApiError::context_bundle_not_found(bundle_id))?;
    let _ = load_library_and_authorize(
        &auth,
        &state,
        bundle_set.bundle.library_id,
        POLICY_KNOWLEDGE_READ,
    )
    .await?;
    let traces = state
        .context_store
        .list_traces_by_bundle(bundle_set.bundle.bundle_id)
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
    Ok(Json(KnowledgeContextBundleDetailResponse {
        bundle: bundle_set.bundle,
        traces,
        chunk_references: bundle_set.chunk_references,
        entity_references: bundle_set.entity_references,
        relation_references: bundle_set.relation_references,
        evidence_references: bundle_set.evidence_references,
    }))
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct KnowledgeLibraryGenerationListResponse {
    items: Vec<KnowledgeLibraryGeneration>,
    #[serde(skip_serializing_if = "Option::is_none")]
    next_cursor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    total: Option<i64>,
}

/// Paginated history of graph-generation/build runs for the library,
/// distinct from the single `latestGeneration` field on `/summary`.
///
/// The derivation this wraps (`derive_library_generation_rows`) currently
/// synthesizes at most one row — the library's *current* readable
/// generation state — because there is no build-history table with a
/// writer in the ingest/graph pipeline yet. This endpoint is the correct,
/// honest read surface for that state today; `nextCursor` is always
/// `null` because there is never more than one page. Real multi-row build
/// history (timings, degraded passes over time) requires a schema change
/// plus a pipeline write-path and is out of scope for this read-surface
/// restoration.
#[tracing::instrument(
    level = "info",
    name = "http.list_library_generations",
    skip_all,
    fields(library_id = %library_id, item_count)
)]
#[utoipa::path(
    get,
    path = "/v1/knowledge/libraries/{libraryId}/generations",
    tag = "knowledge",
    operation_id = "listKnowledgeLibraryGenerations",
    params(("libraryId" = uuid::Uuid, Path, description = "Library identifier")),
    responses(
        (status = 200, description = "Knowledge generation history for the library", body = KnowledgeLibraryGenerationListResponse),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not authorized for the library"),
        (status = 404, description = "Library not found"),
    ),
)]
pub async fn list_library_generations(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(library_id): Path<Uuid>,
) -> Result<Json<KnowledgeLibraryGenerationListResponse>, ApiError> {
    let span = tracing::Span::current();
    let _ = load_library_and_authorize(&auth, &state, library_id, POLICY_KNOWLEDGE_READ).await?;
    let generations =
        state.canonical_services.knowledge.list_library_generations(&state, library_id).await?;
    span.record("item_count", generations.len());
    let total = generations.len() as i64;
    Ok(Json(KnowledgeLibraryGenerationListResponse {
        items: generations,
        next_cursor: None,
        total: Some(total),
    }))
}
