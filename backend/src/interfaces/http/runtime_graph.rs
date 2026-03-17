use std::collections::{BTreeSet, HashMap};

use axum::{
    Json, Router,
    extract::{Path, Query, State},
};
use serde::Deserialize;
use serde::Serialize;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    infra::{
        graph_store::{GraphProjectionData, GraphProjectionEdgeWrite, GraphProjectionNodeWrite},
        repositories,
    },
    interfaces::http::{
        auth::AuthContext,
        authorization::POLICY_GRAPH_READ,
        router_support::{ApiError, ApiWarningBody, partial_convergence_warning},
        runtime_support::load_library_and_authorize,
    },
};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeGraphSurfaceNodeResponse {
    pub id: String,
    pub label: String,
    pub node_type: String,
    pub secondary_label: Option<String>,
    pub support_count: i32,
    pub filtered_artifact: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeGraphSurfaceEdgeResponse {
    pub id: String,
    pub source: String,
    pub target: String,
    pub relation_type: String,
    pub support_count: i32,
    pub filtered_artifact: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeGraphLegendItemResponse {
    pub key: String,
    pub label: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeGraphSearchHitResponse {
    pub id: String,
    pub label: String,
    pub node_type: String,
    pub secondary_label: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeGraphRelatedEdgeResponse {
    pub id: String,
    pub relation_type: String,
    pub other_node_id: String,
    pub other_node_label: String,
    pub support_count: i32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeGraphEvidenceResponse {
    pub id: String,
    pub document_id: Option<String>,
    pub document_label: Option<String>,
    pub chunk_id: Option<String>,
    pub page_ref: Option<String>,
    pub evidence_text: String,
    pub confidence_score: Option<f64>,
    pub created_at: String,
    pub active_provenance_only: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeGraphSurfaceResponse {
    pub library_id: String,
    pub graph_status: String,
    pub convergence_status: String,
    pub projection_version: i64,
    pub node_count: usize,
    pub relation_count: usize,
    pub filtered_artifact_count: usize,
    pub last_built_at: Option<String>,
    pub warning: Option<String>,
    pub warnings: Vec<ApiWarningBody>,
    pub nodes: Vec<RuntimeGraphSurfaceNodeResponse>,
    pub edges: Vec<RuntimeGraphSurfaceEdgeResponse>,
    pub legend: Vec<RuntimeGraphLegendItemResponse>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeGraphNodeDetailResponse {
    pub id: String,
    pub label: String,
    pub node_type: String,
    pub summary: String,
    pub properties: Vec<(String, String)>,
    pub related_documents: Vec<RuntimeGraphSearchHitResponse>,
    pub connected_nodes: Vec<RuntimeGraphSearchHitResponse>,
    pub related_edges: Vec<RuntimeGraphRelatedEdgeResponse>,
    pub evidence: Vec<RuntimeGraphEvidenceResponse>,
    pub relation_count: usize,
    pub reconciliation_status: Option<String>,
    pub convergence_status: String,
    pub pending_update_count: usize,
    pub pending_delete_count: usize,
    pub active_provenance_only: bool,
    pub filtered_artifact_count: usize,
    pub warning: Option<String>,
    pub warnings: Vec<ApiWarningBody>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeGraphDiagnosticsResponse {
    pub library_id: String,
    pub graph_status: String,
    pub reconciliation_status: String,
    pub convergence_status: String,
    pub projection_version: i64,
    pub node_count: usize,
    pub edge_count: usize,
    pub projection_freshness: String,
    pub rebuild_backlog_count: usize,
    pub ready_no_graph_count: usize,
    pub pending_update_count: usize,
    pub pending_delete_count: usize,
    pub filtered_artifact_count: usize,
    pub filtered_empty_relation_count: usize,
    pub filtered_degenerate_loop_count: usize,
    pub provenance_coverage_percent: Option<f64>,
    pub last_built_at: Option<String>,
    pub last_error_message: Option<String>,
    pub last_mutation_warning: Option<String>,
    pub active_provenance_only: bool,
    pub blockers: Vec<String>,
    pub warning: Option<String>,
    pub warnings: Vec<ApiWarningBody>,
    pub graph_backend: String,
}

#[derive(Debug, Clone)]
struct GraphReconciliationState {
    reconciliation_status: String,
    pending_update_count: usize,
    pending_delete_count: usize,
    last_mutation_warning: Option<String>,
    active_provenance_only: bool,
}

#[derive(Debug, Default, Deserialize)]
struct RuntimeGraphSurfaceQuery {
    include_filtered: Option<bool>,
}

pub fn router() -> Router<crate::app::state::AppState> {
    Router::new()
        .route(
            "/runtime/libraries/{library_id}/graph/surface",
            axum::routing::get(get_graph_surface),
        )
        .route(
            "/runtime/libraries/{library_id}/graph/nodes/{node_id}",
            axum::routing::get(get_graph_node_detail),
        )
        .route(
            "/runtime/libraries/{library_id}/graph/diagnostics",
            axum::routing::get(get_graph_diagnostics),
        )
}

async fn get_graph_surface(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(library_id): Path<Uuid>,
    Query(query): Query<RuntimeGraphSurfaceQuery>,
) -> Result<Json<RuntimeGraphSurfaceResponse>, ApiError> {
    let _ = load_library_and_authorize(&auth, &state, library_id, POLICY_GRAPH_READ).await?;
    Ok(Json(
        load_runtime_graph_surface(&state, library_id, query.include_filtered.unwrap_or(false))
            .await?,
    ))
}

async fn get_graph_node_detail(
    auth: AuthContext,
    State(state): State<AppState>,
    Path((library_id, node_id)): Path<(Uuid, Uuid)>,
    Query(query): Query<RuntimeGraphSurfaceQuery>,
) -> Result<Json<RuntimeGraphNodeDetailResponse>, ApiError> {
    let _ = load_library_and_authorize(&auth, &state, library_id, POLICY_GRAPH_READ).await?;
    Ok(Json(
        load_runtime_graph_node_detail(
            &state,
            library_id,
            node_id,
            query.include_filtered.unwrap_or(false),
        )
        .await?,
    ))
}

async fn get_graph_diagnostics(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(library_id): Path<Uuid>,
) -> Result<Json<RuntimeGraphDiagnosticsResponse>, ApiError> {
    let _ = load_library_and_authorize(&auth, &state, library_id, POLICY_GRAPH_READ).await?;
    Ok(Json(load_runtime_graph_diagnostics(&state, library_id).await?))
}

pub(crate) async fn load_runtime_graph_surface(
    state: &AppState,
    library_id: Uuid,
    include_filtered: bool,
) -> Result<RuntimeGraphSurfaceResponse, ApiError> {
    let (snapshot, projection) = load_snapshot_and_projection(state, library_id).await?;
    let filtered_projection = filter_projection_for_surface(state, &projection);
    let active_projection = if include_filtered { &projection } else { &filtered_projection };
    let filtered_node_ids = filtered_projection
        .nodes
        .iter()
        .map(|node| node.node_id)
        .collect::<std::collections::HashSet<_>>();
    let filtered_edge_ids = filtered_projection
        .edges
        .iter()
        .map(|edge| edge.edge_id)
        .collect::<std::collections::HashSet<_>>();
    let counters = repositories::load_runtime_graph_convergence_counters(
        &state.persistence.postgres,
        library_id,
    )
    .await
    .map_err(|_| ApiError::Internal)?;
    let convergence_status =
        graph_convergence_status(&snapshot.graph_status, &counters).to_string();
    let nodes = active_projection
        .nodes
        .iter()
        .map(|node| RuntimeGraphSurfaceNodeResponse {
            id: node.node_id.to_string(),
            label: node.label.clone(),
            node_type: node.node_type.clone(),
            secondary_label: surface_secondary_label(node),
            support_count: node.support_count,
            filtered_artifact: !filtered_node_ids.contains(&node.node_id),
        })
        .collect::<Vec<_>>();
    let edges = active_projection
        .edges
        .iter()
        .map(|edge| RuntimeGraphSurfaceEdgeResponse {
            id: edge.edge_id.to_string(),
            source: edge.from_node_id.to_string(),
            target: edge.to_node_id.to_string(),
            relation_type: edge.relation_type.clone(),
            support_count: edge.support_count,
            filtered_artifact: !filtered_edge_ids.contains(&edge.edge_id),
        })
        .collect::<Vec<_>>();

    Ok(RuntimeGraphSurfaceResponse {
        library_id: library_id.to_string(),
        graph_status: snapshot.graph_status.clone(),
        convergence_status: convergence_status.clone(),
        projection_version: snapshot.projection_version,
        node_count: nodes.len(),
        relation_count: edges.len(),
        filtered_artifact_count: usize::try_from(counters.filtered_artifact_count)
            .unwrap_or_default(),
        last_built_at: snapshot.last_built_at.map(|value| value.to_rfc3339()),
        warning: surface_warning(&snapshot.graph_status, &filtered_projection)
            .or_else(|| convergence_warning(state, &convergence_status, &counters)),
        warnings: graph_warnings(state, &convergence_status, &counters),
        legend: build_legend(active_projection),
        nodes,
        edges,
    })
}

pub(crate) async fn load_runtime_graph_node_detail(
    state: &AppState,
    library_id: Uuid,
    node_id: Uuid,
    include_filtered: bool,
) -> Result<RuntimeGraphNodeDetailResponse, ApiError> {
    let (snapshot, projection) = load_snapshot_and_projection(state, library_id).await?;
    let filtered_projection = filter_projection_for_surface(state, &projection);
    let active_projection = if include_filtered { &projection } else { &filtered_projection };
    let counters = repositories::load_runtime_graph_convergence_counters(
        &state.persistence.postgres,
        library_id,
    )
    .await
    .map_err(|_| ApiError::Internal)?;
    let convergence_status =
        graph_convergence_status(&snapshot.graph_status, &counters).to_string();
    let reconciliation =
        graph_reconciliation_state_from_counters(&snapshot.graph_status, &counters);
    let Some(node) = active_projection.nodes.iter().find(|node| node.node_id == node_id).cloned()
    else {
        return Err(ApiError::NotFound(format!("graph node {node_id} not found")));
    };

    let node_index =
        active_projection.nodes.iter().map(|item| (item.node_id, item)).collect::<HashMap<_, _>>();
    let related_edges = active_projection
        .edges
        .iter()
        .filter(|edge| edge.from_node_id == node_id || edge.to_node_id == node_id)
        .cloned()
        .collect::<Vec<_>>();
    let connected_nodes = related_edges
        .iter()
        .filter_map(|edge| {
            let other_id =
                if edge.from_node_id == node_id { edge.to_node_id } else { edge.from_node_id };
            node_index.get(&other_id).map(|other| RuntimeGraphSearchHitResponse {
                id: other.node_id.to_string(),
                label: other.label.clone(),
                node_type: other.node_type.clone(),
                secondary_label: Some(edge.relation_type.clone()),
            })
        })
        .collect::<Vec<_>>();

    let evidence_rows = repositories::list_runtime_graph_evidence_by_target(
        &state.persistence.postgres,
        library_id,
        "node",
        node_id,
    )
    .await
    .map_err(|_| ApiError::Internal)?;
    let related_document_ids =
        evidence_rows.iter().filter_map(|row| row.document_id).collect::<BTreeSet<_>>();
    let related_documents = load_document_hits(state, &related_document_ids).await?;
    let document_label_index = related_documents
        .iter()
        .map(|item| (Uuid::parse_str(&item.id).ok(), item.label.clone()))
        .filter_map(|(id, label)| id.map(|value| (value, label)))
        .collect::<HashMap<_, _>>();
    let evidence = evidence_rows
        .into_iter()
        .filter(|row| match row.document_id {
            Some(document_id) => document_label_index.contains_key(&document_id),
            None => true,
        })
        .map(|row| RuntimeGraphEvidenceResponse {
            id: row.id.to_string(),
            document_id: row.document_id.map(|value| value.to_string()),
            document_label: row
                .document_id
                .and_then(|value| document_label_index.get(&value).cloned()),
            chunk_id: row.chunk_id.map(|value| value.to_string()),
            page_ref: row.page_ref,
            evidence_text: row.evidence_text,
            confidence_score: row.confidence_score,
            created_at: row.created_at.to_rfc3339(),
            active_provenance_only: true,
        })
        .collect::<Vec<_>>();

    Ok(RuntimeGraphNodeDetailResponse {
        id: node.node_id.to_string(),
        label: node.label.clone(),
        node_type: node.node_type.clone(),
        summary: node.summary.clone().unwrap_or_else(|| default_node_summary(&node)),
        properties: build_node_properties(&node, snapshot.projection_version),
        related_documents,
        connected_nodes,
        related_edges: related_edges
            .iter()
            .filter_map(|edge| {
                let other_id =
                    if edge.from_node_id == node_id { edge.to_node_id } else { edge.from_node_id };
                node_index.get(&other_id).map(|other| RuntimeGraphRelatedEdgeResponse {
                    id: edge.edge_id.to_string(),
                    relation_type: edge.relation_type.clone(),
                    other_node_id: other.node_id.to_string(),
                    other_node_label: other.label.clone(),
                    support_count: edge.support_count,
                })
            })
            .collect(),
        evidence,
        relation_count: related_edges.len(),
        reconciliation_status: Some(reconciliation.reconciliation_status.clone()),
        convergence_status: convergence_status.clone(),
        pending_update_count: reconciliation.pending_update_count,
        pending_delete_count: reconciliation.pending_delete_count,
        active_provenance_only: reconciliation.active_provenance_only,
        filtered_artifact_count: usize::try_from(counters.filtered_artifact_count)
            .unwrap_or_default(),
        warning: reconciliation.last_mutation_warning.clone().or_else(|| {
            surface_warning(&snapshot.graph_status, &filtered_projection)
                .or_else(|| convergence_warning(state, &convergence_status, &counters))
        }),
        warnings: graph_warnings(state, &convergence_status, &counters),
    })
}

pub(crate) async fn load_runtime_graph_diagnostics(
    state: &AppState,
    library_id: Uuid,
) -> Result<RuntimeGraphDiagnosticsResponse, ApiError> {
    let (snapshot, projection) = load_snapshot_and_projection(state, library_id).await?;
    let filtered_projection = filter_projection_for_surface(state, &projection);
    let counters = repositories::load_runtime_graph_convergence_counters(
        &state.persistence.postgres,
        library_id,
    )
    .await
    .map_err(|_| ApiError::Internal)?;
    let convergence_status =
        graph_convergence_status(&snapshot.graph_status, &counters).to_string();
    let reconciliation =
        graph_reconciliation_state_from_counters(&snapshot.graph_status, &counters);
    let rebuild_backlog_count =
        usize::try_from(counters.queued_document_count + counters.processing_document_count)
            .unwrap_or_default();
    let ready_no_graph_count = usize::try_from(counters.ready_no_graph_count).unwrap_or_default();
    let projection_freshness =
        projection_freshness(&snapshot.graph_status, rebuild_backlog_count, ready_no_graph_count);
    let mut blockers = diagnostics_blockers(
        &snapshot.graph_status,
        &filtered_projection,
        rebuild_backlog_count,
        ready_no_graph_count,
    );
    let pending_update_count = usize::try_from(counters.pending_update_count).unwrap_or_default();
    let pending_delete_count = usize::try_from(counters.pending_delete_count).unwrap_or_default();
    if pending_update_count > 0 {
        blockers.push(format!(
            "{} document update mutation(s) are still reconciling.",
            pending_update_count
        ));
    }
    if pending_delete_count > 0 {
        blockers.push(format!(
            "{} document delete mutation(s) are still reconciling.",
            pending_delete_count
        ));
    }
    Ok(RuntimeGraphDiagnosticsResponse {
        library_id: library_id.to_string(),
        graph_status: snapshot.graph_status.clone(),
        reconciliation_status: reconciliation.reconciliation_status,
        convergence_status: convergence_status.clone(),
        projection_version: snapshot.projection_version,
        node_count: filtered_projection.nodes.len(),
        edge_count: filtered_projection.edges.len(),
        projection_freshness,
        rebuild_backlog_count,
        ready_no_graph_count,
        pending_update_count,
        pending_delete_count,
        filtered_artifact_count: usize::try_from(counters.filtered_artifact_count)
            .unwrap_or_default(),
        filtered_empty_relation_count: usize::try_from(counters.filtered_empty_relation_count)
            .unwrap_or_default(),
        filtered_degenerate_loop_count: usize::try_from(counters.filtered_degenerate_loop_count)
            .unwrap_or_default(),
        provenance_coverage_percent: snapshot.provenance_coverage_percent,
        last_built_at: snapshot.last_built_at.map(|value| value.to_rfc3339()),
        last_error_message: snapshot.last_error_message.clone(),
        last_mutation_warning: reconciliation.last_mutation_warning.clone(),
        active_provenance_only: reconciliation.active_provenance_only,
        blockers,
        warning: reconciliation.last_mutation_warning.or_else(|| {
            surface_warning(&snapshot.graph_status, &filtered_projection)
                .or_else(|| convergence_warning(state, &convergence_status, &counters))
        }),
        warnings: graph_warnings(state, &convergence_status, &counters),
        graph_backend: state.graph_store.backend_name().to_string(),
    })
}

async fn load_canonical_projection(
    state: &AppState,
    library_id: Uuid,
    projection_version: i64,
) -> Result<GraphProjectionData, ApiError> {
    let nodes = repositories::list_admitted_runtime_graph_nodes_by_projection(
        &state.persistence.postgres,
        library_id,
        projection_version,
    )
    .await
    .map_err(|_| ApiError::Internal)?;
    let edges = repositories::list_admitted_runtime_graph_edges_by_projection(
        &state.persistence.postgres,
        library_id,
        projection_version,
    )
    .await
    .map_err(|_| ApiError::Internal)?;

    Ok(GraphProjectionData {
        nodes: nodes
            .into_iter()
            .map(|node| GraphProjectionNodeWrite {
                node_id: node.id,
                canonical_key: node.canonical_key,
                label: node.label,
                node_type: node.node_type,
                support_count: node.support_count,
                summary: node.summary,
                aliases: serde_json::from_value(node.aliases_json).unwrap_or_default(),
                metadata_json: node.metadata_json,
            })
            .collect(),
        edges: edges
            .into_iter()
            .map(|edge| GraphProjectionEdgeWrite {
                edge_id: edge.id,
                from_node_id: edge.from_node_id,
                to_node_id: edge.to_node_id,
                relation_type: edge.relation_type,
                canonical_key: edge.canonical_key,
                support_count: edge.support_count,
                summary: edge.summary,
                weight: edge.weight,
                metadata_json: edge.metadata_json,
            })
            .collect(),
    })
}

fn projection_contains_document_nodes(projection: &GraphProjectionData) -> bool {
    projection.nodes.iter().any(|node| node.node_type == "document")
}

async fn load_snapshot_and_projection(
    state: &AppState,
    library_id: Uuid,
) -> Result<(repositories::RuntimeGraphSnapshotRow, GraphProjectionData), ApiError> {
    let mut snapshot =
        repositories::get_runtime_graph_snapshot(&state.persistence.postgres, library_id)
            .await
            .map_err(|_| ApiError::Internal)?
            .unwrap_or_else(|| repositories::RuntimeGraphSnapshotRow {
                project_id: library_id,
                graph_status: "empty".to_string(),
                projection_version: 1,
                node_count: 0,
                edge_count: 0,
                provenance_coverage_percent: Some(0.0),
                last_built_at: None,
                last_error_message: None,
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
            });
    let projection =
        if snapshot.graph_status == "empty" && snapshot.node_count == 0 && snapshot.edge_count == 0
        {
            GraphProjectionData::default()
        } else {
            state
                .graph_store
                .load_library_projection(library_id, snapshot.projection_version)
                .await
                .map_err(|_| ApiError::Internal)?
        };

    let latest_canonical_version = repositories::get_latest_runtime_graph_projection_version(
        &state.persistence.postgres,
        library_id,
    )
    .await
    .map_err(|_| ApiError::Internal)?
    .filter(|version| *version > snapshot.projection_version);

    if let Some(latest_canonical_version) = latest_canonical_version {
        let canonical_projection =
            load_canonical_projection(state, library_id, latest_canonical_version).await?;
        if projection.nodes.is_empty()
            || (!projection_contains_document_nodes(&projection)
                && projection_contains_document_nodes(&canonical_projection))
        {
            snapshot.projection_version = latest_canonical_version;
            snapshot.node_count =
                i32::try_from(canonical_projection.nodes.len()).unwrap_or(i32::MAX);
            snapshot.edge_count =
                i32::try_from(canonical_projection.edges.len()).unwrap_or(i32::MAX);
            if snapshot.graph_status == "ready" {
                snapshot.graph_status = "stale".to_string();
            }
            return Ok((snapshot, canonical_projection));
        }
    }

    Ok((snapshot, projection))
}

async fn load_document_hits(
    state: &AppState,
    document_ids: &BTreeSet<Uuid>,
) -> Result<Vec<RuntimeGraphSearchHitResponse>, ApiError> {
    let mut hits = Vec::new();
    for document_id in document_ids {
        let Some(document) =
            repositories::get_document_by_id(&state.persistence.postgres, *document_id)
                .await
                .map_err(|_| ApiError::Internal)?
        else {
            continue;
        };
        if document.deleted_at.is_some() {
            continue;
        }
        hits.push(RuntimeGraphSearchHitResponse {
            id: document.id.to_string(),
            label: document.title.unwrap_or(document.external_key),
            node_type: "document".to_string(),
            secondary_label: document.mime_type,
        });
    }
    Ok(hits)
}

fn graph_reconciliation_state_from_counters(
    graph_status: &str,
    counters: &repositories::RuntimeGraphConvergenceCountersRow,
) -> GraphReconciliationState {
    let pending_update_count = usize::try_from(counters.pending_update_count).unwrap_or_default();
    let pending_delete_count = usize::try_from(counters.pending_delete_count).unwrap_or_default();
    GraphReconciliationState {
        reconciliation_status: graph_reconciliation_status(
            graph_status,
            pending_update_count,
            pending_delete_count,
        )
        .to_string(),
        pending_update_count,
        pending_delete_count,
        last_mutation_warning: graph_last_mutation_warning(
            counters.latest_failed_mutation_kind.as_deref(),
            pending_update_count,
            pending_delete_count,
        ),
        active_provenance_only: true,
    }
}

fn build_legend(projection: &GraphProjectionData) -> Vec<RuntimeGraphLegendItemResponse> {
    let mut legend = vec![
        RuntimeGraphLegendItemResponse {
            key: "document".to_string(),
            label: "Document".to_string(),
        },
        RuntimeGraphLegendItemResponse { key: "entity".to_string(), label: "Entity".to_string() },
    ];
    if projection.nodes.iter().any(|node| node.node_type == "topic") {
        legend.push(RuntimeGraphLegendItemResponse {
            key: "topic".to_string(),
            label: "Topic".to_string(),
        });
    }
    legend.push(RuntimeGraphLegendItemResponse {
        key: "relation".to_string(),
        label: "Relation".to_string(),
    });
    legend
}

fn surface_secondary_label(node: &GraphProjectionNodeWrite) -> Option<String> {
    match node.node_type.as_str() {
        "document" => node
            .metadata_json
            .get("mime_type")
            .and_then(serde_json::Value::as_str)
            .map(std::string::ToString::to_string),
        "topic" => Some("Topic".to_string()),
        "entity" => Some(format!("{} evidence", node.support_count)),
        _ => None,
    }
}

fn default_node_summary(node: &GraphProjectionNodeWrite) -> String {
    match node.node_type.as_str() {
        "document" => "Source document in the active library.".to_string(),
        "topic" => "Topic node extracted from processed documents.".to_string(),
        _ => "Entity node extracted from processed documents.".to_string(),
    }
}

fn build_node_properties(
    node: &GraphProjectionNodeWrite,
    projection_version: i64,
) -> Vec<(String, String)> {
    let mut properties = vec![
        ("Type".to_string(), node.node_type.clone()),
        ("Support".to_string(), node.support_count.to_string()),
        ("Projection".to_string(), projection_version.to_string()),
        ("Canonical key".to_string(), node.canonical_key.clone()),
    ];
    let aliases = node
        .aliases
        .iter()
        .filter(|alias| alias.trim() != node.label.trim())
        .cloned()
        .collect::<Vec<_>>();
    if !aliases.is_empty() {
        properties.push(("Aliases".to_string(), aliases.join(", ")));
    }
    properties
}

fn surface_warning(graph_status: &str, projection: &GraphProjectionData) -> Option<String> {
    match graph_status {
        "failed" => Some(
            "The latest graph projection failed. Check diagnostics before relying on answers."
                .to_string(),
        ),
        "building" => Some("Graph projection is currently rebuilding.".to_string()),
        "stale" => Some(
            "Graph coverage is stale after a recent delete or reprocess. The visible graph only reflects surviving evidence."
                .to_string(),
        ),
        "empty" if projection.nodes.is_empty() => {
            Some("No graph evidence has been extracted for this library yet.".to_string())
        }
        "ready" if projection.nodes.is_empty() => Some(
            "The graph snapshot is marked ready, but the projection is still empty.".to_string(),
        ),
        _ => None,
    }
}

fn filter_projection_for_surface(
    state: &AppState,
    projection: &GraphProjectionData,
) -> GraphProjectionData {
    state.bulk_ingest_hardening_services.graph_quality_guard.filter_projection(projection)
}

fn graph_convergence_status(
    graph_status: &str,
    counters: &repositories::RuntimeGraphConvergenceCountersRow,
) -> &'static str {
    if matches!(graph_status, "failed" | "stale") {
        return "degraded";
    }
    if counters.queued_document_count > 0
        || counters.processing_document_count > 0
        || counters.ready_no_graph_count > 0
        || counters.pending_update_count > 0
        || counters.pending_delete_count > 0
        || matches!(graph_status, "building" | "empty" | "partial")
    {
        return "partial";
    }
    "current"
}

fn convergence_warning(
    state: &AppState,
    convergence_status: &str,
    counters: &repositories::RuntimeGraphConvergenceCountersRow,
) -> Option<String> {
    if convergence_status != "partial" {
        return None;
    }
    let backlog = counters.queued_document_count
        + counters.processing_document_count
        + counters.ready_no_graph_count
        + counters.pending_update_count
        + counters.pending_delete_count;
    let threshold =
        i64::try_from(state.bulk_ingest_hardening.graph_convergence_warning_backlog_threshold)
            .unwrap_or(1);
    if backlog < threshold {
        return None;
    }
    Some(format!(
        "Graph coverage is still converging while {backlog} document or mutation task(s) remain in backlog."
    ))
}

fn graph_warnings(
    state: &AppState,
    convergence_status: &str,
    counters: &repositories::RuntimeGraphConvergenceCountersRow,
) -> Vec<ApiWarningBody> {
    match convergence_warning(state, convergence_status, counters) {
        Some(message) => vec![partial_convergence_warning(message)],
        None => Vec::new(),
    }
}

fn projection_freshness(
    graph_status: &str,
    rebuild_backlog_count: usize,
    ready_no_graph_count: usize,
) -> String {
    if graph_status == "failed" {
        return "failed".to_string();
    }
    if graph_status == "building" {
        return "building".to_string();
    }
    if graph_status == "stale" {
        return "stale".to_string();
    }
    if graph_status == "empty" {
        return "empty".to_string();
    }
    if rebuild_backlog_count > 0 || ready_no_graph_count > 0 {
        return "lagging".to_string();
    }
    "current".to_string()
}

fn graph_reconciliation_status(
    graph_status: &str,
    pending_update_count: usize,
    pending_delete_count: usize,
) -> &'static str {
    if graph_status == "failed" {
        return "failed";
    }
    if graph_status == "stale" {
        return "stale";
    }
    if pending_update_count > 0 && pending_delete_count > 0 {
        return "mixed";
    }
    if pending_delete_count > 0 {
        return "deleting";
    }
    if pending_update_count > 0 || graph_status == "building" {
        return "updating";
    }
    "current"
}

fn graph_last_mutation_warning(
    latest_failed_mutation_kind: Option<&str>,
    pending_update_count: usize,
    pending_delete_count: usize,
) -> Option<String> {
    if pending_delete_count > 0 {
        return Some(format!(
            "Delete reconciliation is still removing provenance from {pending_delete_count} document(s)."
        ));
    }
    if pending_update_count > 0 {
        return Some(format!(
            "Revision reconciliation is still updating graph truth for {pending_update_count} document(s)."
        ));
    }

    latest_failed_mutation_kind.map(|mutation_kind| match mutation_kind {
        "delete" => {
            "The latest document delete failed. Some stale evidence may still require review."
                .to_string()
        }
        "update_append" | "update_replace" => {
            "The latest document update failed. Active graph truth still reflects the previous revision."
                .to_string()
        }
        _ => "The latest document mutation failed. Review lifecycle status before relying on graph answers."
            .to_string(),
    })
}

fn diagnostics_blockers(
    graph_status: &str,
    projection: &GraphProjectionData,
    rebuild_backlog_count: usize,
    ready_no_graph_count: usize,
) -> Vec<String> {
    let mut blockers = Vec::new();
    if graph_status == "failed" {
        blockers.push("The last graph projection failed.".to_string());
    }
    if graph_status == "stale" {
        blockers.push(
            "The graph is temporarily stale because a document was deleted or reprocessed."
                .to_string(),
        );
    }
    if rebuild_backlog_count > 0 {
        blockers.push(format!(
            "{rebuild_backlog_count} document(s) are still queued or processing, so graph coverage can change again soon."
        ));
    }
    if ready_no_graph_count > 0 {
        blockers.push(format!(
            "{ready_no_graph_count} processed document(s) still have no graph evidence."
        ));
    }
    if projection.nodes.is_empty() {
        blockers.push("The active library does not have projected graph nodes yet.".to_string());
    }
    if !projection.nodes.is_empty() && projection.edges.is_empty() {
        blockers.push(
            "Projected graph nodes exist, but relationship coverage is still empty.".to_string(),
        );
    }
    blockers
}
