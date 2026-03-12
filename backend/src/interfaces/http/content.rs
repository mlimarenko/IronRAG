use axum::{Json, Router, extract::State};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    app::state::AppState,
    infra::repositories,
    interfaces::http::{
        auth::AuthContext,
        content_support::{TextIngestRequest, embed_project_chunks_with_usage, ingest_plain_text},
        router_support::ApiError,
    },
};

#[derive(Deserialize)]
pub struct IngestTextRequest {
    pub project_id: Uuid,
    pub source_id: Option<Uuid>,
    pub external_key: String,
    pub title: Option<String>,
    pub text: String,
}

#[derive(Serialize)]
pub struct IngestTextResponse {
    pub document_id: Uuid,
    pub chunk_count: usize,
}

#[derive(Deserialize)]
pub struct SearchChunksRequest {
    pub project_id: Uuid,
    pub query_text: String,
    pub top_k: Option<i32>,
}

#[derive(Serialize)]
pub struct ChunkResult {
    pub id: Uuid,
    pub document_id: Uuid,
    pub ordinal: i32,
    pub content: String,
}

#[derive(Deserialize)]
pub struct EmbedProjectChunksRequest {
    pub project_id: Uuid,
    pub provider_kind: String,
    pub model_name: String,
    pub embedding_model_profile_id: Option<Uuid>,
    pub limit: Option<i64>,
}

#[derive(Serialize)]
pub struct EmbedProjectChunksResponse {
    pub project_id: Uuid,
    pub embedded_chunks: usize,
    pub provider_kind: String,
    pub model_name: String,
}

pub fn router() -> Router<crate::app::state::AppState> {
    Router::new()
        .route("/content/ingest-text", axum::routing::post(ingest_text))
        .route("/content/search-chunks", axum::routing::post(search_chunks))
        .route("/content/embed-project", axum::routing::post(embed_project_chunks))
}

async fn ingest_text(
    auth: AuthContext,
    State(state): State<AppState>,
    Json(payload): Json<IngestTextRequest>,
) -> Result<Json<IngestTextResponse>, ApiError> {
    auth.require_any_scope(&["documents:write", "workspace:admin"])?;
    if payload.external_key.trim().is_empty() {
        return Err(ApiError::BadRequest("external_key must not be empty".into()));
    }
    if payload.text.trim().is_empty() {
        return Err(ApiError::BadRequest("text must not be empty".into()));
    }

    let (document_id, chunk_count) = ingest_plain_text(
        &state,
        TextIngestRequest {
            project_id: payload.project_id,
            source_id: payload.source_id,
            external_key: &payload.external_key,
            title: payload.title.as_deref(),
            mime_type: Some("text/plain"),
            text: &payload.text,
            ingest_mode: "text_chunking_v1",
            extra_metadata: serde_json::json!({}),
        },
    )
    .await?;

    Ok(Json(IngestTextResponse { document_id, chunk_count }))
}

async fn search_chunks(
    auth: AuthContext,
    State(state): State<AppState>,
    Json(payload): Json<SearchChunksRequest>,
) -> Result<Json<Vec<ChunkResult>>, ApiError> {
    auth.require_any_scope(&["documents:read", "query:run", "workspace:admin"])?;
    if payload.query_text.trim().is_empty() {
        return Err(ApiError::BadRequest("query_text must not be empty".into()));
    }

    let items = repositories::search_chunks_by_project(
        &state.persistence.postgres,
        payload.project_id,
        &payload.query_text,
        payload.top_k.unwrap_or(5),
    )
    .await
    .map_err(|_| ApiError::Internal)?
    .into_iter()
    .map(|row| ChunkResult {
        id: row.id,
        document_id: row.document_id,
        ordinal: row.ordinal,
        content: row.content,
    })
    .collect();

    Ok(Json(items))
}

async fn embed_project_chunks(
    auth: AuthContext,
    State(state): State<AppState>,
    Json(payload): Json<EmbedProjectChunksRequest>,
) -> Result<Json<EmbedProjectChunksResponse>, ApiError> {
    auth.require_any_scope(&["documents:write", "workspace:admin"])?;

    let (provider_kind, model_name) = match payload.embedding_model_profile_id {
        Some(model_profile_id) => {
            let profile = repositories::get_model_profile_by_id(
                &state.persistence.postgres,
                model_profile_id,
            )
            .await
            .map_err(|_| ApiError::Internal)?
            .ok_or_else(|| ApiError::NotFound("embedding model_profile not found".into()))?;
            let provider = repositories::get_provider_account_by_id(
                &state.persistence.postgres,
                profile.provider_account_id,
            )
            .await
            .map_err(|_| ApiError::Internal)?
            .ok_or_else(|| ApiError::NotFound("embedding provider_account not found".into()))?;
            (provider.provider_kind, profile.model_name)
        }
        None => (payload.provider_kind.clone(), payload.model_name.clone()),
    };

    let embedded = embed_project_chunks_with_usage(
        &state,
        payload.project_id,
        provider_kind.clone(),
        model_name.clone(),
        payload.embedding_model_profile_id,
        payload.limit.unwrap_or(100),
    )
    .await?;

    Ok(Json(EmbedProjectChunksResponse {
        project_id: payload.project_id,
        embedded_chunks: embedded,
        provider_kind,
        model_name,
    }))
}
