use axum::{
    Json, Router,
    extract::{Path, State},
    routing::{get, post},
};
use serde::Serialize;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::{
        graph::{GraphEdge, GraphNode},
        search::{SearchHit, SearchRequest},
    },
    infra::repositories::{content_repository, extract_repository, graph_repository},
    interfaces::http::{
        auth::AuthContext,
        authorization::{POLICY_LIBRARY_READ, load_library_and_authorize},
        router_support::ApiError,
    },
    services::graph_service::GraphService,
};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphProjectionDetailResponse {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub source_attempt_id: Option<Uuid>,
    pub projection_state: String,
    pub started_at: chrono::DateTime<chrono::Utc>,
    pub completed_at: Option<chrono::DateTime<chrono::Utc>>,
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/graph/libraries/{library_id}/projection", get(get_active_projection))
        .route("/graph/libraries/{library_id}/nodes", get(list_graph_nodes))
        .route("/graph/libraries/{library_id}/edges", get(list_graph_edges))
        .route("/search/documents", post(search_documents))
}

async fn get_active_projection(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(library_id): Path<Uuid>,
) -> Result<Json<GraphProjectionDetailResponse>, ApiError> {
    let library =
        load_library_and_authorize(&auth, &state, library_id, POLICY_LIBRARY_READ).await?;
    let projection = resolve_active_projection(&state, library.workspace_id, library.id).await?;
    Ok(Json(map_projection_row(projection)))
}

async fn list_graph_nodes(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(library_id): Path<Uuid>,
) -> Result<Json<Vec<GraphNode>>, ApiError> {
    let library =
        load_library_and_authorize(&auth, &state, library_id, POLICY_LIBRARY_READ).await?;
    let projection = resolve_active_projection(&state, library.workspace_id, library.id).await?;
    let rows = graph_repository::list_graph_nodes_by_projection(
        &state.persistence.postgres,
        projection.id,
    )
    .await
    .map_err(|_| ApiError::Internal)?;
    Ok(Json(rows.into_iter().map(map_node_row).collect()))
}

async fn list_graph_edges(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(library_id): Path<Uuid>,
) -> Result<Json<Vec<GraphEdge>>, ApiError> {
    let library =
        load_library_and_authorize(&auth, &state, library_id, POLICY_LIBRARY_READ).await?;
    let projection = resolve_active_projection(&state, library.workspace_id, library.id).await?;
    let rows = graph_repository::list_graph_edges_by_projection(
        &state.persistence.postgres,
        projection.id,
    )
    .await
    .map_err(|_| ApiError::Internal)?;
    Ok(Json(rows.into_iter().map(map_edge_row).collect()))
}

async fn search_documents(
    auth: AuthContext,
    State(state): State<AppState>,
    Json(payload): Json<SearchRequest>,
) -> Result<Json<Vec<SearchHit>>, ApiError> {
    let library =
        load_library_and_authorize(&auth, &state, payload.library_id, POLICY_LIBRARY_READ).await?;
    let query = payload.query_text.trim();
    if query.is_empty() {
        return Err(ApiError::BadRequest("queryText must not be empty".to_string()));
    }

    let query_lower = query.to_ascii_lowercase();
    let limit = payload.limit.clamp(1, 100);
    let documents =
        content_repository::list_documents_by_library(&state.persistence.postgres, library.id)
            .await
            .map_err(|_| ApiError::Internal)?;
    let mut hits = Vec::new();

    for document in documents {
        if document.document_state == "deleted" {
            continue;
        }
        let Some(head) =
            content_repository::get_document_head(&state.persistence.postgres, document.id)
                .await
                .map_err(|_| ApiError::Internal)?
        else {
            continue;
        };
        let Some(readable_revision_id) = head.readable_revision_id else {
            continue;
        };

        let preview = if let Some(extracted) =
            extract_repository::get_extract_content_by_revision_id(
                &state.persistence.postgres,
                readable_revision_id,
            )
            .await
            .map_err(|_| ApiError::Internal)?
        {
            extracted
                .normalized_text
                .as_deref()
                .and_then(|text| preview_for_query(text, &query_lower))
        } else {
            None
        };

        let preview = match preview {
            Some(preview) => Some(preview),
            None => {
                let chunks = content_repository::list_chunks_by_revision(
                    &state.persistence.postgres,
                    readable_revision_id,
                )
                .await
                .map_err(|_| ApiError::Internal)?;
                chunks
                    .into_iter()
                    .find_map(|chunk| preview_for_query(&chunk.normalized_text, &query_lower))
            }
        };

        if let Some(preview) = preview {
            let score = score_preview(&preview, &query_lower);
            hits.push(SearchHit { subject_id: document.id, score, preview: Some(preview) });
        }
    }

    hits.sort_by(|left, right| right.score.total_cmp(&left.score));
    hits.truncate(limit);
    Ok(Json(hits))
}

async fn resolve_active_projection(
    state: &AppState,
    workspace_id: Uuid,
    library_id: Uuid,
) -> Result<graph_repository::GraphProjectionRow, ApiError> {
    let rows = graph_repository::list_graph_projections_by_library(
        &state.persistence.postgres,
        workspace_id,
        library_id,
    )
    .await
    .map_err(|_| ApiError::Internal)?;
    GraphService::new()
        .select_active_projection(&rows)
        .cloned()
        .ok_or_else(|| ApiError::resource_not_found("graph_projection", library_id))
}

fn map_projection_row(row: graph_repository::GraphProjectionRow) -> GraphProjectionDetailResponse {
    GraphProjectionDetailResponse {
        id: row.id,
        workspace_id: row.workspace_id,
        library_id: row.library_id,
        source_attempt_id: row.source_attempt_id,
        projection_state: row.projection_state,
        started_at: row.started_at,
        completed_at: row.completed_at,
    }
}

fn map_node_row(row: graph_repository::GraphNodeRow) -> GraphNode {
    GraphNode {
        id: row.id,
        projection_id: row.projection_id,
        canonical_key: row.canonical_key,
        node_kind: row.node_kind,
        display_label: row.display_label,
        support_count: row.support_count,
    }
}

fn map_edge_row(row: graph_repository::GraphEdgeRow) -> GraphEdge {
    GraphEdge {
        id: row.id,
        projection_id: row.projection_id,
        canonical_key: row.canonical_key,
        edge_kind: row.edge_kind,
        from_node_id: row.from_node_id,
        to_node_id: row.to_node_id,
        support_count: row.support_count,
    }
}

fn preview_for_query(text: &str, query_lower: &str) -> Option<String> {
    let text_lower = text.to_ascii_lowercase();
    let position = text_lower.find(query_lower)?;
    let start = position.saturating_sub(80);
    let end = (position + query_lower.len() + 160).min(text.len());
    Some(text[start..end].trim().to_string())
}

fn score_preview(preview: &str, query_lower: &str) -> f32 {
    let preview_lower = preview.to_ascii_lowercase();
    preview_lower.find(query_lower).map(|position| 1.0f32 / (1.0 + position as f32)).unwrap_or(0.0)
}
