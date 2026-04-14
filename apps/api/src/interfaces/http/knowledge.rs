use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

mod library;
mod search;

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    routing::get,
};
use chrono::{DateTime, Utc};
use ironrag_contracts::{
    diagnostics::{MessageLevel, OperatorWarning},
    graph::{
        GraphConvergenceStatus, GraphDocumentReference, GraphEdge, GraphEvidence, GraphFilterState,
        GraphGenerationSummary, GraphNode, GraphNodeDetail, GraphNodeType, GraphReadinessSummary,
        GraphRelatedNode, GraphStatus, GraphSurface, GraphWorkbenchSurface,
    },
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::knowledge::{KnowledgeLibraryGeneration, TypedTechnicalFact},
    infra::arangodb::{
        collections::KNOWLEDGE_CHUNK_COLLECTION,
        document_store::{KnowledgeChunkRow, KnowledgeDocumentRow},
        graph_store::KnowledgeEvidenceRow,
    },
    infra::repositories,
    interfaces::http::{
        auth::AuthContext,
        authorization::{POLICY_KNOWLEDGE_READ, load_library_and_authorize},
        router_support::ApiError,
    },
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/knowledge/context-bundles/{bundle_id}", get(library::get_context_bundle))
        .route(
            "/knowledge/libraries/{library_id}/context-bundles",
            get(library::list_context_bundles),
        )
        .route("/knowledge/libraries/{library_id}/documents", get(library::list_documents))
        .route(
            "/knowledge/libraries/{library_id}/documents/{document_id}",
            get(library::get_document),
        )
        .route("/knowledge/libraries/{library_id}/summary", get(library::get_library_summary))
        .route("/knowledge/libraries/{library_id}/graph-workbench", get(get_graph_workbench))
        .route("/knowledge/libraries/{library_id}/graph-topology", get(get_graph_topology))
        .route("/knowledge/libraries/{library_id}/entities", get(list_entities))
        .route("/knowledge/libraries/{library_id}/entities/{entity_id}", get(get_entity))
        .route("/knowledge/libraries/{library_id}/relations", get(list_relations))
        .route("/knowledge/libraries/{library_id}/relations/{relation_id}", get(get_relation))
        .route(
            "/knowledge/libraries/{library_id}/generations",
            get(library::list_library_generations),
        )
        .route("/knowledge/libraries/{library_id}/search/documents", get(search::search_documents))
        .route("/search/documents", get(search::search_documents_by_library_query))
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct GraphWorkbenchQuery {
    #[serde(default)]
    search_query: Option<String>,
    #[serde(default)]
    focus_document_id: Option<Uuid>,
    #[serde(default)]
    include_filtered_artifacts: bool,
    #[serde(default)]
    selected_node_id: Option<Uuid>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct KnowledgeEntityDetailResponse {
    entity: RuntimeKnowledgeEntityRow,
    mention_edges: Vec<KnowledgeEntityMentionEdgeRow>,
    mentioned_chunks: Vec<KnowledgeChunkRow>,
    supporting_evidence_edges: Vec<KnowledgeEvidenceSupportEntityEdgeRow>,
    supporting_evidence: Vec<RuntimeKnowledgeEvidenceRow>,
    supporting_typed_facts: Vec<TypedTechnicalFact>,
    graph_evidence_summary: KnowledgeGraphEvidenceSummary,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct KnowledgeRelationDetailResponse {
    relation: RuntimeKnowledgeRelationRow,
    supporting_evidence_edges: Vec<KnowledgeEvidenceSupportRelationEdgeRow>,
    supporting_evidence: Vec<RuntimeKnowledgeEvidenceRow>,
    supporting_typed_facts: Vec<TypedTechnicalFact>,
    graph_evidence_summary: KnowledgeGraphEvidenceSummary,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct KnowledgeGraphTopologyResponse {
    documents: Vec<KnowledgeDocumentRow>,
    entities: Vec<RuntimeKnowledgeEntityRow>,
    relations: Vec<RuntimeKnowledgeRelationRow>,
    document_links: Vec<RuntimeKnowledgeDocumentLinkRow>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct KnowledgeLibrarySummaryResponse {
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeKnowledgeEntityRow {
    key: String,
    entity_id: Uuid,
    workspace_id: Uuid,
    library_id: Uuid,
    canonical_label: String,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeKnowledgeRelationRow {
    key: String,
    relation_id: Uuid,
    workspace_id: Uuid,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeKnowledgeEvidenceRow {
    key: String,
    evidence_id: Uuid,
    workspace_id: Uuid,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeKnowledgeDocumentLinkRow {
    document_id: Uuid,
    target_node_id: Uuid,
    target_node_type: String,
    relation_type: String,
    support_count: i64,
}

async fn list_entities(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(library_id): Path<Uuid>,
) -> Result<Json<Vec<RuntimeKnowledgeEntityRow>>, ApiError> {
    let _ = load_library_and_authorize(&auth, &state, library_id, POLICY_KNOWLEDGE_READ).await?;
    let entities = load_runtime_graph_topology_rows(&state, library_id).await?.entities;
    Ok(Json(entities))
}

async fn list_relations(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(library_id): Path<Uuid>,
) -> Result<Json<Vec<RuntimeKnowledgeRelationRow>>, ApiError> {
    let _ = load_library_and_authorize(&auth, &state, library_id, POLICY_KNOWLEDGE_READ).await?;
    let relations = load_runtime_graph_topology_rows(&state, library_id).await?.relations;
    Ok(Json(relations))
}

async fn get_graph_workbench(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(library_id): Path<Uuid>,
    Query(query): Query<GraphWorkbenchQuery>,
) -> Result<Json<GraphWorkbenchSurface>, ApiError> {
    let _ = load_library_and_authorize(&auth, &state, library_id, POLICY_KNOWLEDGE_READ).await?;
    let GraphWorkbenchQuery {
        search_query,
        focus_document_id,
        include_filtered_artifacts,
        selected_node_id,
    } = query;
    let summary =
        state.canonical_services.knowledge.get_library_summary(&state, library_id).await?;
    let topology = load_runtime_graph_topology_rows(&state, library_id).await?;
    let snapshot =
        repositories::get_runtime_graph_snapshot(&state.persistence.postgres, library_id)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
    let nodes = topology
        .entities
        .iter()
        .filter(|entity| include_filtered_artifacts || entity.entity_state == "active")
        .cloned()
        .map(map_runtime_entity_to_graph_node)
        .collect::<Vec<_>>();
    let edges = topology
        .relations
        .iter()
        .filter(|relation| include_filtered_artifacts || relation.relation_state == "active")
        .cloned()
        .map(map_runtime_relation_to_graph_edge)
        .collect::<Vec<_>>();
    let selected_node_id =
        selected_node_id.filter(|node_id| nodes.iter().any(|node| node.id == *node_id));
    let selected_node = match selected_node_id {
        Some(node_id) => {
            build_graph_node_detail(
                &state,
                library_id,
                node_id,
                &topology.entities,
                &topology.relations,
            )
            .await?
        }
        None => None,
    };
    let diagnostics = build_graph_diagnostics(&summary, snapshot.as_ref());

    Ok(Json(GraphWorkbenchSurface {
        graph: build_graph_surface(
            &summary,
            snapshot.as_ref(),
            nodes,
            edges,
            diagnostics.first().map(|warning| warning.detail.clone()),
        ),
        filters: GraphFilterState { search_query, focus_document_id, include_filtered_artifacts },
        selected_node_id,
        selected_node,
        diagnostics,
    }))
}

async fn get_graph_topology(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(library_id): Path<Uuid>,
) -> Result<Json<KnowledgeGraphTopologyResponse>, ApiError> {
    let _ = load_library_and_authorize(&auth, &state, library_id, POLICY_KNOWLEDGE_READ).await?;
    let topology = load_runtime_graph_topology_rows(&state, library_id).await?;
    let documents = state
        .arango_document_store
        .list_documents_by_ids(&topology.document_ids)
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;

    Ok(Json(KnowledgeGraphTopologyResponse {
        documents,
        entities: topology.entities,
        relations: topology.relations,
        document_links: topology.document_links,
    }))
}

async fn get_entity(
    auth: AuthContext,
    State(state): State<AppState>,
    Path((library_id, entity_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<KnowledgeEntityDetailResponse>, ApiError> {
    let library =
        load_library_and_authorize(&auth, &state, library_id, POLICY_KNOWLEDGE_READ).await?;
    let entity = repositories::get_runtime_graph_node_by_id(
        &state.persistence.postgres,
        library_id,
        entity_id,
    )
    .await
    .map_err(|e| ApiError::internal_with_log(e, "internal"))?
    .ok_or_else(|| ApiError::resource_not_found("runtime_graph_node", entity_id))?;
    if entity.library_id != library_id {
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

    Ok(Json(KnowledgeEntityDetailResponse {
        entity: map_runtime_graph_node_to_entity_row(entity, library.workspace_id, library_id),
        mention_edges,
        mentioned_chunks,
        supporting_evidence_edges,
        supporting_evidence,
        supporting_typed_facts,
        graph_evidence_summary,
    }))
}

async fn get_relation(
    auth: AuthContext,
    State(state): State<AppState>,
    Path((library_id, relation_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<KnowledgeRelationDetailResponse>, ApiError> {
    let library =
        load_library_and_authorize(&auth, &state, library_id, POLICY_KNOWLEDGE_READ).await?;
    let relation = repositories::get_runtime_graph_edge_by_id(
        &state.persistence.postgres,
        library_id,
        relation_id,
    )
    .await
    .map_err(|e| ApiError::internal_with_log(e, "internal"))?
    .ok_or_else(|| ApiError::resource_not_found("runtime_graph_edge", relation_id))?;
    if relation.library_id != library_id {
        return Err(ApiError::resource_not_found("knowledge_relation", relation_id));
    }

    let supporting_evidence =
        load_runtime_graph_supporting_evidence(&state, library_id, "edge", relation_id).await?;
    let supporting_evidence_edges =
        build_runtime_relation_evidence_support_edges(relation_id, &supporting_evidence);
    let supporting_typed_facts = Vec::new();
    let graph_evidence_summary = summarize_runtime_graph_evidence(&supporting_evidence, 0);

    Ok(Json(KnowledgeRelationDetailResponse {
        relation: map_runtime_graph_edge_to_relation_row(
            library.workspace_id,
            library_id,
            relation,
        ),
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
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
    decode_many_results(cursor).map_err(|e| ApiError::internal_with_log(e, "internal"))
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

#[derive(Debug)]
struct RuntimeGraphTopologyRows {
    document_ids: Vec<Uuid>,
    entities: Vec<RuntimeKnowledgeEntityRow>,
    relations: Vec<RuntimeKnowledgeRelationRow>,
    document_links: Vec<RuntimeKnowledgeDocumentLinkRow>,
}

fn build_graph_surface(
    summary: &crate::domains::knowledge::KnowledgeLibrarySummary,
    snapshot: Option<&repositories::RuntimeGraphSnapshotRow>,
    nodes: Vec<GraphNode>,
    edges: Vec<GraphEdge>,
    warning: Option<String>,
) -> GraphSurface {
    let graph_status = graph_workbench_status(summary, snapshot);
    GraphSurface {
        library_id: summary.library_id,
        status: graph_status,
        convergence_status: graph_workbench_convergence_status(graph_status, snapshot),
        warning,
        node_count: saturating_usize_to_i32(nodes.len()),
        relation_count: saturating_usize_to_i32(edges.len()),
        edge_count: saturating_usize_to_i32(edges.len()),
        graph_ready_document_count: saturating_i64_to_i32(summary.graph_ready_document_count),
        graph_sparse_document_count: saturating_i64_to_i32(summary.graph_sparse_document_count),
        typed_fact_document_count: saturating_i64_to_i32(summary.typed_fact_document_count),
        updated_at: snapshot.and_then(|row| row.last_built_at).or(Some(summary.updated_at)),
        nodes,
        edges,
        readiness_summary: Some(GraphReadinessSummary {
            library_id: summary.library_id,
            document_counts_by_readiness: summary
                .document_counts_by_readiness
                .iter()
                .map(|(key, value)| (key.clone(), *value))
                .collect(),
            graph_ready_document_count: summary.graph_ready_document_count,
            graph_sparse_document_count: summary.graph_sparse_document_count,
            typed_fact_document_count: summary.typed_fact_document_count,
            latest_generation: summary.latest_generation.as_ref().map(|generation| {
                GraphGenerationSummary {
                    generation_id: Some(generation.id),
                    active_graph_generation: 1,
                    degraded_state: snapshot.map(|row| row.graph_status.clone()),
                    updated_at: generation.completed_at.or(Some(generation.created_at)),
                }
            }),
            updated_at: Some(summary.updated_at),
        }),
    }
}

fn build_graph_diagnostics(
    summary: &crate::domains::knowledge::KnowledgeLibrarySummary,
    snapshot: Option<&repositories::RuntimeGraphSnapshotRow>,
) -> Vec<OperatorWarning> {
    let total_documents = summary.document_counts_by_readiness.values().copied().sum::<i64>();
    let mut diagnostics = Vec::new();

    if let Some(snapshot) = snapshot
        && let Some(detail) =
            snapshot.last_error_message.as_ref().filter(|detail| !detail.is_empty())
    {
        diagnostics.push(OperatorWarning {
            code: "runtime_graph_snapshot_error".to_string(),
            level: MessageLevel::Error,
            title: "Runtime graph snapshot error".to_string(),
            detail: detail.clone(),
        });
    }

    match graph_workbench_status(summary, snapshot) {
        GraphStatus::Empty | GraphStatus::Ready => {}
        GraphStatus::Building => diagnostics.push(OperatorWarning {
            code: "runtime_graph_building".to_string(),
            level: MessageLevel::Info,
            title: "Runtime graph is building".to_string(),
            detail: if total_documents > 0 {
                format!(
                    "The active library has {total_documents} readable documents, but the runtime graph has not converged yet."
                )
            } else {
                "The active library has not produced any graph rows yet.".to_string()
            },
        }),
        GraphStatus::Rebuilding => diagnostics.push(OperatorWarning {
            code: "runtime_graph_rebuilding".to_string(),
            level: MessageLevel::Warning,
            title: "Runtime graph is rebuilding".to_string(),
            detail: "The active library graph is refreshing after recent extraction work.".to_string(),
        }),
        GraphStatus::Partial => diagnostics.push(OperatorWarning {
            code: "runtime_graph_partial".to_string(),
            level: MessageLevel::Warning,
            title: "Graph coverage remains partial".to_string(),
            detail: format!(
                "{} readable documents remain graph-sparse for this library.",
                summary.graph_sparse_document_count
            ),
        }),
        GraphStatus::Failed => diagnostics.push(OperatorWarning {
            code: "runtime_graph_failed".to_string(),
            level: MessageLevel::Error,
            title: "Runtime graph failed".to_string(),
            detail: "The active library graph projection is currently failed.".to_string(),
        }),
        GraphStatus::Stale => diagnostics.push(OperatorWarning {
            code: "runtime_graph_stale".to_string(),
            level: MessageLevel::Warning,
            title: "Runtime graph is stale".to_string(),
            detail: "The active library graph projection needs a refresh.".to_string(),
        }),
    }

    diagnostics
}

fn graph_workbench_status(
    summary: &crate::domains::knowledge::KnowledgeLibrarySummary,
    snapshot: Option<&repositories::RuntimeGraphSnapshotRow>,
) -> GraphStatus {
    let readable_without_graph_count =
        summary.document_counts_by_readiness.get("readable").copied().unwrap_or(0);

    if let Some(snapshot) = snapshot {
        return match snapshot.graph_status.as_str() {
            "empty" => GraphStatus::Empty,
            "building" => {
                if summary.graph_ready_document_count > 0 || summary.graph_sparse_document_count > 0
                {
                    GraphStatus::Rebuilding
                } else {
                    GraphStatus::Building
                }
            }
            "rebuilding" => GraphStatus::Rebuilding,
            "ready" => {
                if summary.graph_sparse_document_count > 0 || readable_without_graph_count > 0 {
                    GraphStatus::Partial
                } else {
                    GraphStatus::Ready
                }
            }
            "partial" => GraphStatus::Partial,
            "failed" => GraphStatus::Failed,
            "stale" => GraphStatus::Stale,
            _ => graph_workbench_status_from_summary(summary),
        };
    }

    graph_workbench_status_from_summary(summary)
}

fn graph_workbench_status_from_summary(
    summary: &crate::domains::knowledge::KnowledgeLibrarySummary,
) -> GraphStatus {
    let total_documents = summary.document_counts_by_readiness.values().copied().sum::<i64>();
    let readable_without_graph_count =
        summary.document_counts_by_readiness.get("readable").copied().unwrap_or(0);
    if total_documents == 0 {
        GraphStatus::Empty
    } else if summary.graph_ready_document_count > 0
        && summary.graph_sparse_document_count == 0
        && readable_without_graph_count == 0
    {
        GraphStatus::Ready
    } else if summary.graph_ready_document_count > 0
        || summary.graph_sparse_document_count > 0
        || readable_without_graph_count > 0
    {
        GraphStatus::Partial
    } else {
        GraphStatus::Building
    }
}

fn graph_workbench_convergence_status(
    status: GraphStatus,
    snapshot: Option<&repositories::RuntimeGraphSnapshotRow>,
) -> Option<GraphConvergenceStatus> {
    match status {
        GraphStatus::Ready => {
            let coverage = snapshot.and_then(|row| row.provenance_coverage_percent);
            if coverage.is_some_and(|value| value < 100.0) {
                Some(GraphConvergenceStatus::Partial)
            } else {
                Some(GraphConvergenceStatus::Current)
            }
        }
        GraphStatus::Partial | GraphStatus::Building | GraphStatus::Rebuilding => {
            Some(GraphConvergenceStatus::Partial)
        }
        GraphStatus::Failed | GraphStatus::Stale => Some(GraphConvergenceStatus::Degraded),
        GraphStatus::Empty => None,
    }
}

fn map_runtime_entity_to_graph_node(row: RuntimeKnowledgeEntityRow) -> GraphNode {
    GraphNode {
        id: row.entity_id,
        canonical_key: row.key,
        label: row.canonical_label,
        node_type: map_graph_node_type(&row.entity_type),
        secondary_label: Some(row.entity_type),
        support_count: row.support_count,
        summary: row.summary,
        filtered_artifact: row.entity_state != "active",
    }
}

fn map_runtime_relation_to_graph_edge(row: RuntimeKnowledgeRelationRow) -> GraphEdge {
    GraphEdge {
        id: row.relation_id,
        canonical_key: row.key,
        source: row.subject_entity_id,
        target: row.object_entity_id,
        relation_type: row.relation_type,
        support_count: row.support_count,
        filtered_artifact: row.relation_state != "active",
    }
}

async fn build_graph_node_detail(
    state: &AppState,
    library_id: Uuid,
    node_id: Uuid,
    entities: &[RuntimeKnowledgeEntityRow],
    relations: &[RuntimeKnowledgeRelationRow],
) -> Result<Option<GraphNodeDetail>, ApiError> {
    let Some(entity) = entities.iter().find(|entity| entity.entity_id == node_id).cloned() else {
        return Ok(None);
    };
    let evidence =
        load_runtime_graph_supporting_evidence(state, library_id, "node", node_id).await?;
    let related_nodes = relations
        .iter()
        .filter_map(|relation| {
            if relation.subject_entity_id == node_id {
                entities
                    .iter()
                    .find(|candidate| candidate.entity_id == relation.object_entity_id)
                    .map(|candidate| GraphRelatedNode {
                        id: candidate.entity_id,
                        label: candidate.canonical_label.clone(),
                        relation_type: relation.relation_type.clone(),
                    })
            } else if relation.object_entity_id == node_id {
                entities
                    .iter()
                    .find(|candidate| candidate.entity_id == relation.subject_entity_id)
                    .map(|candidate| GraphRelatedNode {
                        id: candidate.entity_id,
                        label: candidate.canonical_label.clone(),
                        relation_type: relation.relation_type.clone(),
                    })
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    let supporting_document_ids = evidence
        .iter()
        .map(|row| row.document_id)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let supporting_documents = if supporting_document_ids.is_empty() {
        Vec::new()
    } else {
        let documents = state
            .arango_document_store
            .list_documents_by_ids(&supporting_document_ids)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        let document_labels = documents
            .into_iter()
            .map(|document| (document.document_id, document.title.unwrap_or(document.external_key)))
            .collect::<HashMap<_, _>>();
        supporting_document_ids
            .into_iter()
            .map(|document_id| GraphDocumentReference {
                document_id,
                document_label: document_labels.get(&document_id).cloned(),
            })
            .collect()
    };
    let document_labels = supporting_documents
        .iter()
        .map(|document| (document.document_id, document.document_label.clone()))
        .collect::<HashMap<_, _>>();

    Ok(Some(GraphNodeDetail {
        id: entity.entity_id,
        label: entity.canonical_label.clone(),
        node_type: map_graph_node_type(&entity.entity_type),
        summary: entity
            .summary
            .unwrap_or_else(|| humanize_graph_entity_state(&entity.entity_state)),
        properties: vec![
            ("key".to_string(), entity.key),
            ("entity_type".to_string(), entity.entity_type),
            ("support_count".to_string(), entity.support_count.to_string()),
            ("freshness_generation".to_string(), entity.freshness_generation.to_string()),
            ("entity_state".to_string(), entity.entity_state.clone()),
        ],
        related_nodes,
        supporting_documents,
        evidence: evidence
            .into_iter()
            .map(|row| GraphEvidence {
                id: row.key,
                document_id: Some(row.document_id),
                document_label: document_labels.get(&row.document_id).cloned().flatten(),
                chunk_id: row.chunk_id,
                excerpt: row.excerpt,
                support_kind: Some(row.support_kind),
                extraction_method: Some(row.extraction_method),
                confidence: row.confidence,
                created_at: Some(row.created_at),
            })
            .collect(),
        warning: (entity.entity_state != "active")
            .then(|| format!("Selected node is currently {}.", entity.entity_state)),
    }))
}

fn map_graph_node_type(value: &str) -> GraphNodeType {
    match value.to_ascii_lowercase().as_str() {
        "person" => GraphNodeType::Person,
        "organization" => GraphNodeType::Organization,
        "location" => GraphNodeType::Location,
        "event" => GraphNodeType::Event,
        "artifact" => GraphNodeType::Artifact,
        "natural" => GraphNodeType::Natural,
        "process" => GraphNodeType::Process,
        "concept" => GraphNodeType::Concept,
        "attribute" => GraphNodeType::Attribute,
        // Backward compatibility
        "topic" => GraphNodeType::Concept,
        "technology" => GraphNodeType::Artifact,
        "api" => GraphNodeType::Artifact,
        "code_symbol" => GraphNodeType::Artifact,
        "natural_kind" => GraphNodeType::Natural,
        "metric" => GraphNodeType::Attribute,
        "regulation" => GraphNodeType::Artifact,
        _ => GraphNodeType::Entity,
    }
}

fn humanize_graph_entity_state(value: &str) -> String {
    format!("Node is currently {}.", value.replace('_', " "))
}

fn saturating_i64_to_i32(value: i64) -> i32 {
    i32::try_from(value).unwrap_or(i32::MAX)
}

fn saturating_usize_to_i32(value: usize) -> i32 {
    i32::try_from(value).unwrap_or(i32::MAX)
}

async fn load_runtime_graph_topology_rows(
    state: &AppState,
    library_id: Uuid,
) -> Result<RuntimeGraphTopologyRows, ApiError> {
    let library = state
        .canonical_services
        .catalog
        .get_library(state, library_id)
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
    let workspace_id = library.workspace_id;
    let Some(snapshot) =
        repositories::get_runtime_graph_snapshot(&state.persistence.postgres, library_id)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
    else {
        return Ok(RuntimeGraphTopologyRows {
            document_ids: Vec::new(),
            entities: Vec::new(),
            relations: Vec::new(),
            document_links: Vec::new(),
        });
    };

    if snapshot.graph_status == "empty" || snapshot.projection_version <= 0 {
        return Ok(RuntimeGraphTopologyRows {
            document_ids: Vec::new(),
            entities: Vec::new(),
            relations: Vec::new(),
            document_links: Vec::new(),
        });
    }

    let projection_version = snapshot.projection_version;
    let node_rows = repositories::list_admitted_runtime_graph_nodes_by_library(
        &state.persistence.postgres,
        library_id,
        projection_version,
    )
    .await
    .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
    let edge_rows = repositories::list_admitted_runtime_graph_edges_by_library(
        &state.persistence.postgres,
        library_id,
        projection_version,
    )
    .await
    .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
    let document_link_rows = repositories::list_runtime_graph_document_links_by_library(
        &state.persistence.postgres,
        library_id,
        projection_version,
    )
    .await
    .map_err(|e| ApiError::internal_with_log(e, "internal"))?;

    let document_node_ids = node_rows
        .iter()
        .filter(|row| row.node_type == "document")
        .map(|row| row.id)
        .collect::<HashSet<_>>();
    let document_ids = node_rows
        .iter()
        .filter(|row| row.node_type == "document")
        .filter_map(runtime_graph_document_id)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();

    let entities = node_rows
        .into_iter()
        .filter(|row| row.node_type != "document")
        .map(|row| map_runtime_graph_node_to_entity_row(row, workspace_id, library_id))
        .collect::<Vec<_>>();
    let mut relations = Vec::with_capacity(edge_rows.len());
    for row in edge_rows {
        if document_node_ids.contains(&row.from_node_id)
            || document_node_ids.contains(&row.to_node_id)
        {
            continue;
        }
        relations.push(map_runtime_graph_edge_to_relation_row(workspace_id, library_id, row));
    }
    let document_links = document_link_rows
        .into_iter()
        .map(|row| RuntimeKnowledgeDocumentLinkRow {
            document_id: row.document_id,
            target_node_id: row.target_node_id,
            target_node_type: row.target_node_type,
            relation_type: row.relation_type,
            support_count: row.support_count,
        })
        .collect();

    Ok(RuntimeGraphTopologyRows { document_ids, entities, relations, document_links })
}

fn runtime_graph_document_id(row: &repositories::RuntimeGraphNodeRow) -> Option<Uuid> {
    row.metadata_json
        .get("document_id")
        .and_then(serde_json::Value::as_str)
        .and_then(|value| Uuid::parse_str(value).ok())
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
    // admittance state. Previously this function returned the recovery
    // value here, and the workbench filter then dropped every node whose
    // recovery state wasn't the literal string "active", hiding ~99% of
    // the graph on dashboards and the graph viewer. The admittance state
    // lives in `entity_state` / `relation_state`, which default to
    // "active" when the node is admitted into the projection.
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
    workspace_id: Uuid,
    library_id: Uuid,
) -> RuntimeKnowledgeEntityRow {
    RuntimeKnowledgeEntityRow {
        key: row.canonical_key.clone(),
        entity_id: row.id,
        workspace_id,
        library_id,
        canonical_label: row.label,
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
    workspace_id: Uuid,
    library_id: Uuid,
    row: repositories::RuntimeGraphEdgeRow,
) -> RuntimeKnowledgeRelationRow {
    RuntimeKnowledgeRelationRow {
        key: row.canonical_key.clone(),
        relation_id: row.id,
        workspace_id,
        library_id,
        relation_type: row.relation_type.clone(),
        normalized_assertion: row.summary.clone().unwrap_or_else(|| row.canonical_key.clone()),
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
    let workspace_id = state
        .canonical_services
        .catalog
        .get_library(state, library_id)
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        .workspace_id;
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
                key: row.id.to_string(),
                evidence_id: row.id,
                workspace_id,
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
            key: format!("{}:{chunk_id}", row.evidence_id),
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
            key: row.key.clone(),
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
            key: row.key.clone(),
            evidence_id: row.evidence_id,
            relation_id,
            rank: None,
            score: row.confidence,
            inclusion_reason: Some("runtime_graph_evidence".to_string()),
            created_at: row.created_at,
        })
        .collect()
}

fn decode_many_results<T>(cursor: serde_json::Value) -> Result<Vec<T>, ApiError>
where
    T: for<'de> serde::Deserialize<'de>,
{
    let result = cursor.get("result").cloned().ok_or(ApiError::Internal)?;
    serde_json::from_value(result).map_err(|e| ApiError::internal_with_log(e, "internal"))
}
