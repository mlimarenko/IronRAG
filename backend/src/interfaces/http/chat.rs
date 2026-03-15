use axum::{
    Json, Router,
    extract::{Path, Query, State},
};
use serde::{Deserialize, Serialize};
use tracing::{error, info};
use uuid::Uuid;

use crate::{
    app::state::AppState,
    infra::repositories,
    interfaces::http::{
        auth::AuthContext,
        authorization::{POLICY_QUERY_READ, load_project_and_authorize},
        router_support::ApiError,
    },
};

#[derive(Deserialize)]
pub struct ChatSessionsQuery {
    pub project_id: Uuid,
}

#[derive(Serialize)]
pub struct ChatSessionSummary {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub project_id: Uuid,
    pub title: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Serialize)]
pub struct ChatMessageItem {
    pub id: Uuid,
    pub session_id: Uuid,
    pub project_id: Uuid,
    pub role: String,
    pub content: String,
    pub retrieval_run_id: Option<Uuid>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/chat/sessions", axum::routing::get(list_chat_sessions))
        .route("/chat/sessions/{id}/messages", axum::routing::get(list_chat_messages))
}

async fn list_chat_sessions(
    auth: AuthContext,
    State(state): State<AppState>,
    Query(query): Query<ChatSessionsQuery>,
) -> Result<Json<Vec<ChatSessionSummary>>, ApiError> {
    auth.require_any_scope(POLICY_QUERY_READ)?;
    let project =
        load_project_and_authorize(&auth, &state, query.project_id, POLICY_QUERY_READ).await?;
    let items =
        repositories::list_chat_sessions_by_project(&state.persistence.postgres, query.project_id)
            .await
            .map_err(|error| {
                error!(
                    auth_token_id = %auth.token_id,
                    workspace_id = %project.workspace_id,
                    project_id = %query.project_id,
                    ?error,
                    "failed to list chat sessions",
                );
                ApiError::Internal
            })?
            .into_iter()
            .map(|row| ChatSessionSummary {
                id: row.id,
                workspace_id: row.workspace_id,
                project_id: row.project_id,
                title: row.title,
                created_at: row.created_at,
                updated_at: row.updated_at,
            })
            .collect::<Vec<_>>();

    info!(
        auth_token_id = %auth.token_id,
        workspace_id = %project.workspace_id,
        project_id = %query.project_id,
        session_count = items.len(),
        "listed chat sessions",
    );

    Ok(Json(items))
}

async fn list_chat_messages(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<ChatMessageItem>>, ApiError> {
    auth.require_any_scope(POLICY_QUERY_READ)?;
    let session = repositories::get_chat_session_by_id(&state.persistence.postgres, id)
        .await
        .map_err(|error| {
            error!(auth_token_id = %auth.token_id, session_id = %id, ?error, "failed to load chat session");
            ApiError::Internal
        })?
        .ok_or_else(|| ApiError::NotFound(format!("chat_session {id} not found")))?;
    let project =
        load_project_and_authorize(&auth, &state, session.project_id, POLICY_QUERY_READ).await?;
    let items = repositories::list_chat_messages_by_session(&state.persistence.postgres, id)
        .await
        .map_err(|error| {
            error!(
                auth_token_id = %auth.token_id,
                workspace_id = %project.workspace_id,
                session_id = %id,
                ?error,
                "failed to list chat messages",
            );
            ApiError::Internal
        })?
        .into_iter()
        .map(|row| ChatMessageItem {
            id: row.id,
            session_id: row.session_id,
            project_id: row.project_id,
            role: row.role,
            content: row.content,
            retrieval_run_id: row.retrieval_run_id,
            created_at: row.created_at,
        })
        .collect::<Vec<_>>();

    Ok(Json(items))
}
