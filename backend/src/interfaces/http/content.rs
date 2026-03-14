use axum::{Json, Router, extract::State};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};
use uuid::Uuid;

use crate::{
    app::state::AppState,
    infra::repositories,
    interfaces::http::{
        auth::AuthContext,
        authorization::{POLICY_DOCUMENTS_WRITE, POLICY_QUERY_READ, load_project_and_authorize},
        content_support::embed_project_chunks_with_usage,
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
    pub ingestion_job_id: Uuid,
    pub status: String,
    pub stage: String,
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
    let project =
        load_project_and_authorize(&auth, &state, payload.project_id, POLICY_DOCUMENTS_WRITE)
            .await?;
    if payload.external_key.trim().is_empty() {
        warn!(
            workspace_id = %project.workspace_id,
            project_id = %payload.project_id,
            source_id = ?payload.source_id,
            "rejecting text ingestion request with empty external_key",
        );
        return Err(ApiError::BadRequest("external_key must not be empty".into()));
    }
    if payload.text.trim().is_empty() {
        warn!(
            workspace_id = %project.workspace_id,
            project_id = %payload.project_id,
            source_id = ?payload.source_id,
            external_key = %payload.external_key.trim(),
            "rejecting text ingestion request with empty text payload",
        );
        return Err(ApiError::BadRequest("text must not be empty".into()));
    }

    let external_key = payload.external_key.trim().to_string();
    let text_len = payload.text.len();
    let idempotency_key = format!("ingest-text:{}:{}", payload.project_id, external_key);
    info!(
        workspace_id = %project.workspace_id,
        project_id = %payload.project_id,
        source_id = ?payload.source_id,
        external_key = %external_key,
        text_len,
        "accepted text ingestion request",
    );
    let job = repositories::create_ingestion_job(
        &state.persistence.postgres,
        payload.project_id,
        payload.source_id,
        "text_ingest",
        None,
        None,
        Some(&idempotency_key),
        serde_json::json!({
            "project_id": payload.project_id,
            "source_id": payload.source_id,
            "external_key": external_key,
            "title": payload.title,
            "mime_type": "text/plain",
            "text": payload.text,
            "ingest_mode": "text_chunking_v1",
            "extra_metadata": {},
        }),
    )
    .await
    .map_err(|error| match error {
        sqlx::Error::Database(database_error)
            if database_error.constraint() == Some("idx_ingestion_job_idempotency_key") =>
        {
            warn!(
                workspace_id = %project.workspace_id,
                project_id = %payload.project_id,
                source_id = ?payload.source_id,
                external_key = %external_key,
                "duplicate text ingestion request",
            );
            ApiError::Conflict("an ingestion job already exists for this idempotency key".into())
        }
        _ => ApiError::Internal,
    })?;

    info!(
        workspace_id = %project.workspace_id,
        project_id = %payload.project_id,
        source_id = ?payload.source_id,
        ingestion_job_id = %job.id,
        status = %job.status,
        stage = %job.stage,
        external_key = %external_key,
        text_len,
        "created ingestion job for text request",
    );

    Ok(Json(IngestTextResponse { ingestion_job_id: job.id, status: job.status, stage: job.stage }))
}

async fn search_chunks(
    auth: AuthContext,
    State(state): State<AppState>,
    Json(payload): Json<SearchChunksRequest>,
) -> Result<Json<Vec<ChunkResult>>, ApiError> {
    let project =
        load_project_and_authorize(&auth, &state, payload.project_id, POLICY_QUERY_READ).await?;
    if payload.query_text.trim().is_empty() {
        warn!(
            workspace_id = %project.workspace_id,
            project_id = %payload.project_id,
            top_k = payload.top_k.unwrap_or(5),
            "rejecting chunk search request with empty query_text",
        );
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
    let project =
        load_project_and_authorize(&auth, &state, payload.project_id, POLICY_DOCUMENTS_WRITE)
            .await?;

    let (provider_kind, model_name) = match payload.embedding_model_profile_id {
        Some(model_profile_id) => {
            let profile = repositories::get_model_profile_by_id(
                &state.persistence.postgres,
                model_profile_id,
            )
            .await
            .map_err(|_| ApiError::Internal)?
            .ok_or_else(|| ApiError::NotFound("embedding model_profile not found".into()))?;
            if profile.workspace_id != project.workspace_id {
                return Err(ApiError::Unauthorized);
            }
            let provider = repositories::get_provider_account_by_id(
                &state.persistence.postgres,
                profile.provider_account_id,
            )
            .await
            .map_err(|_| ApiError::Internal)?
            .ok_or_else(|| ApiError::NotFound("embedding provider_account not found".into()))?;
            if provider.workspace_id != project.workspace_id {
                return Err(ApiError::Unauthorized);
            }
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
