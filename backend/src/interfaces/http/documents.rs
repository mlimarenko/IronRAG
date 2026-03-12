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

#[derive(Serialize)]
pub struct DocumentSummary {
    pub id: Uuid,
    pub project_id: Uuid,
    pub source_id: Option<Uuid>,
    pub external_key: String,
    pub title: Option<String>,
    pub mime_type: Option<String>,
    pub checksum: Option<String>,
}

#[derive(Deserialize)]
pub struct ProjectScopedQuery {
    pub project_id: Option<Uuid>,
}

#[derive(Deserialize)]
pub struct CreateDocumentRequest {
    pub project_id: Uuid,
    pub source_id: Option<Uuid>,
    pub external_key: String,
    pub title: Option<String>,
    pub mime_type: Option<String>,
    pub checksum: Option<String>,
}

pub fn router() -> Router<crate::app::state::AppState> {
    Router::new().route("/documents", axum::routing::get(list_documents).post(create_document))
}

async fn list_documents(
    State(state): State<AppState>,
    Query(query): Query<ProjectScopedQuery>,
) -> Result<Json<Vec<DocumentSummary>>, ApiError> {
    let items = repositories::list_documents(&state.persistence.postgres, query.project_id)
        .await
        .map_err(|_| ApiError::Internal)?
        .into_iter()
        .map(|row| DocumentSummary {
            id: row.id,
            project_id: row.project_id,
            source_id: row.source_id,
            external_key: row.external_key,
            title: row.title,
            mime_type: row.mime_type,
            checksum: row.checksum,
        })
        .collect();

    Ok(Json(items))
}

async fn create_document(
    auth: AuthContext,
    State(state): State<AppState>,
    Json(payload): Json<CreateDocumentRequest>,
) -> Result<Json<DocumentSummary>, ApiError> {
    auth.require_any_scope(&["documents:write", "workspace:admin"])?;
    if payload.external_key.trim().is_empty() {
        return Err(ApiError::BadRequest("external_key must not be empty".into()));
    }

    let row = repositories::create_document(
        &state.persistence.postgres,
        payload.project_id,
        payload.source_id,
        &payload.external_key,
        payload.title.as_deref(),
        payload.mime_type.as_deref(),
        payload.checksum.as_deref(),
    )
    .await
    .map_err(|_| ApiError::Internal)?;

    Ok(Json(DocumentSummary {
        id: row.id,
        project_id: row.project_id,
        source_id: row.source_id,
        external_key: row.external_key,
        title: row.title,
        mime_type: row.mime_type,
        checksum: row.checksum,
    }))
}
