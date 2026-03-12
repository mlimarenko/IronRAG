use axum::{
    Json, Router,
    extract::{Query, State},
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    app::state::AppState,
    infra::repositories,
    interfaces::http::{auth::AuthContext, router_support::ApiError},
};

#[derive(Deserialize)]
pub struct ChunksQuery {
    pub project_id: Option<Uuid>,
    pub document_id: Option<Uuid>,
    pub limit: Option<i64>,
}

#[derive(Serialize)]
pub struct ChunkSummary {
    pub id: Uuid,
    pub document_id: Uuid,
    pub project_id: Uuid,
    pub ordinal: i32,
    pub content: String,
    pub token_count: Option<i32>,
}

pub fn router() -> Router<crate::app::state::AppState> {
    Router::new().route("/chunks", axum::routing::get(list_chunks))
}

async fn list_chunks(
    auth: AuthContext,
    State(state): State<AppState>,
    Query(query): Query<ChunksQuery>,
) -> Result<Json<Vec<ChunkSummary>>, ApiError> {
    auth.require_any_scope(&["documents:read", "workspace:admin"])?;

    let items = if let Some(document_id) = query.document_id {
        repositories::list_chunks_by_document(&state.persistence.postgres, document_id)
            .await
            .map_err(|_| ApiError::Internal)?
    } else if let Some(project_id) = query.project_id {
        repositories::list_chunks_by_project(
            &state.persistence.postgres,
            project_id,
            query.limit.unwrap_or(100),
        )
        .await
        .map_err(|_| ApiError::Internal)?
    } else {
        return Err(ApiError::BadRequest("either project_id or document_id is required".into()));
    };

    Ok(Json(
        items
            .into_iter()
            .map(|row| ChunkSummary {
                id: row.id,
                document_id: row.document_id,
                project_id: row.project_id,
                ordinal: row.ordinal,
                content: row.content,
                token_count: row.token_count,
            })
            .collect(),
    ))
}
