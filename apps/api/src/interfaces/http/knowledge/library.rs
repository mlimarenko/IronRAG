use axum::{
    Json,
    extract::{Path, State},
};
use serde::Serialize;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::knowledge::{KnowledgeLibraryGeneration, TypedTechnicalFact},
    infra::arangodb::{
        context_store::{
            KnowledgeBundleChunkReferenceRow, KnowledgeBundleEntityReferenceRow,
            KnowledgeBundleEvidenceReferenceRow, KnowledgeBundleRelationReferenceRow,
            KnowledgeContextBundleRow, KnowledgeRetrievalTraceRow,
        },
        document_store::{KnowledgeChunkRow, KnowledgeDocumentRow, KnowledgeRevisionRow},
    },
    interfaces::http::{
        auth::AuthContext,
        authorization::{POLICY_KNOWLEDGE_READ, load_library_and_authorize},
        router_support::ApiError,
    },
};

use super::{
    KnowledgeGraphEvidenceSummary, KnowledgeLibrarySummaryResponse,
    KnowledgeTechnicalFactProvenanceSummary, summarize_graph_evidence,
    summarize_typed_technical_facts,
};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct KnowledgeContextBundleDetailResponse {
    bundle: KnowledgeContextBundleRow,
    traces: Vec<KnowledgeRetrievalTraceRow>,
    chunk_references: Vec<KnowledgeBundleChunkReferenceRow>,
    entity_references: Vec<KnowledgeBundleEntityReferenceRow>,
    relation_references: Vec<KnowledgeBundleRelationReferenceRow>,
    evidence_references: Vec<KnowledgeBundleEvidenceReferenceRow>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct KnowledgeDocumentDetailResponse {
    document: KnowledgeDocumentRow,
    revisions: Vec<KnowledgeRevisionRow>,
    latest_revision: Option<KnowledgeRevisionRow>,
    latest_revision_chunks: Vec<KnowledgeChunkRow>,
    latest_revision_typed_facts: Vec<TypedTechnicalFact>,
    technical_fact_summary: KnowledgeTechnicalFactProvenanceSummary,
    graph_evidence_summary: KnowledgeGraphEvidenceSummary,
}

pub(super) async fn list_context_bundles(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(library_id): Path<Uuid>,
) -> Result<Json<Vec<KnowledgeContextBundleRow>>, ApiError> {
    let _ = load_library_and_authorize(&auth, &state, library_id, POLICY_KNOWLEDGE_READ).await?;
    let bundles = state
        .arango_context_store
        .list_bundles_by_library(library_id)
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
    Ok(Json(bundles))
}

pub(super) async fn list_documents(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(library_id): Path<Uuid>,
) -> Result<Json<Vec<KnowledgeDocumentRow>>, ApiError> {
    let library =
        load_library_and_authorize(&auth, &state, library_id, POLICY_KNOWLEDGE_READ).await?;
    let documents = state
        .arango_document_store
        .list_documents_by_library(library.workspace_id, library.id, false)
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
    Ok(Json(documents))
}

pub(super) async fn get_library_summary(
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

pub(super) async fn get_document(
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
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        .ok_or_else(|| ApiError::resource_not_found("knowledge_document", document_id))?;
    if document.library_id != library.id {
        return Err(ApiError::resource_not_found("knowledge_document", document_id));
    }
    let revisions = state
        .arango_document_store
        .list_revisions_by_document(document_id)
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
    let latest_revision = revisions.first().cloned();
    let latest_revision_chunks = match latest_revision.as_ref() {
        Some(revision) => state
            .arango_document_store
            .list_chunks_by_revision(revision.revision_id)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?,
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
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?,
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

pub(super) async fn get_context_bundle(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(bundle_id): Path<Uuid>,
) -> Result<Json<KnowledgeContextBundleDetailResponse>, ApiError> {
    let bundle_set = state
        .arango_context_store
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
        .arango_context_store
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

pub(super) async fn list_library_generations(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(library_id): Path<Uuid>,
) -> Result<Json<Vec<KnowledgeLibraryGeneration>>, ApiError> {
    let _ = load_library_and_authorize(&auth, &state, library_id, POLICY_KNOWLEDGE_READ).await?;
    let generations =
        state.canonical_services.knowledge.list_library_generations(&state, library_id).await?;
    Ok(Json(generations))
}
