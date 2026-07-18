use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::patch,
};
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::query::QueryConversation,
    infra::repositories::query_repository::QueryConversationRow,
    interfaces::http::{
        auth::AuthContext,
        authorization::{POLICY_LIBRARY_WRITE, POLICY_QUERY_RUN, load_query_session_and_authorize},
        router_support::ApiError,
    },
    services::{
        iam::audit::AppendAuditEventCommand,
        query::service::{DeleteConversationCommand, RenameConversationCommand},
    },
};

/// Builds the durable assistant-session mutation routes.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/query/sessions/{session_id}", patch(rename_session).delete(delete_session))
}

#[utoipa::path(
    patch,
    path = "/v1/query/sessions/{sessionId}",
    tag = "query",
    operation_id = "renameQuerySession",
    summary = "Rename an assistant session.",
    description = "Persists a bounded, non-empty display title for one UI assistant session. The caller must have query access to the session scope and must either own the session or hold library-management permission.",
    params(("sessionId" = uuid::Uuid, Path, description = "Assistant session identifier")),
    request_body(content = ironrag_contracts::assistant::RenameAssistantSessionRequest, description = "New durable session title."),
    responses(
        (status = 200, description = "Renamed assistant session", body = QueryConversation),
        (status = 400, description = "Title is empty or exceeds the contract limit"),
        (status = 401, description = "Caller is not authenticated or lacks scope access"),
        (status = 403, description = "Caller may not mutate this session"),
        (status = 404, description = "UI session not found"),
    ),
)]
#[tracing::instrument(
    level = "info",
    name = "http.query.rename_session",
    skip_all,
    fields(session_id = %session_id)
)]
/// Persists a caller-authorized assistant-session title.
///
/// # Errors
/// Returns an API error when validation, authorization, or persistence fails.
pub async fn rename_session(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(session_id): Path<Uuid>,
    Json(payload): Json<ironrag_contracts::assistant::RenameAssistantSessionRequest>,
) -> Result<Json<QueryConversation>, ApiError> {
    let session =
        load_query_session_and_authorize(&auth, &state, session_id, POLICY_QUERY_RUN).await?;
    let allow_manage_all = authorize_session_mutation(&auth, &session)?;
    let renamed = state
        .canonical_services
        .query
        .rename_conversation(
            &state,
            RenameConversationCommand {
                conversation_id: session.id,
                actor_principal_id: auth.principal_id,
                allow_manage_all,
                title: payload.title,
            },
        )
        .await?;
    append_session_mutation_audit(
        &state,
        &auth,
        &session,
        "query.session.rename",
        "query session renamed",
    )
    .await;
    Ok(Json(renamed))
}

#[utoipa::path(
    delete,
    path = "/v1/query/sessions/{sessionId}",
    tag = "query",
    operation_id = "deleteQuerySession",
    summary = "Delete an assistant session.",
    description = "Deletes one owned UI assistant session and its query-owned turns, executions, caches, and context snapshots. Active executions and sessions retained as provenance for an external replay are rejected with a conflict; independent audit and runtime records remain available under their retention policies.",
    params(("sessionId" = uuid::Uuid, Path, description = "Assistant session identifier")),
    responses(
        (status = 204, description = "Assistant session deleted"),
        (status = 401, description = "Caller is not authenticated or lacks scope access"),
        (status = 403, description = "Caller may not mutate this session"),
        (status = 404, description = "UI session not found"),
        (status = 409, description = "Session has active work or retained replay provenance"),
    ),
)]
#[tracing::instrument(
    level = "info",
    name = "http.query.delete_session",
    skip_all,
    fields(session_id = %session_id)
)]
/// Deletes a caller-authorized assistant session after lifecycle checks.
///
/// # Errors
/// Returns an API error when authorization fails or retention blocks deletion.
pub async fn delete_session(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(session_id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    let session =
        load_query_session_and_authorize(&auth, &state, session_id, POLICY_QUERY_RUN).await?;
    let allow_manage_all = authorize_session_mutation(&auth, &session)?;
    state
        .canonical_services
        .query
        .delete_conversation(
            &state,
            DeleteConversationCommand {
                conversation_id: session.id,
                actor_principal_id: auth.principal_id,
                allow_manage_all,
            },
        )
        .await?;
    append_session_mutation_audit(
        &state,
        &auth,
        &session,
        "query.session.delete",
        "query session deleted",
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

fn authorize_session_mutation(
    auth: &AuthContext,
    session: &QueryConversationRow,
) -> Result<bool, ApiError> {
    auth.require_write_capability()?;
    let allow_manage_all = auth.is_system_admin
        || auth.has_library_permission(
            session.workspace_id,
            session.library_id,
            POLICY_LIBRARY_WRITE,
        );
    if session.created_by_principal_id == Some(auth.principal_id) || allow_manage_all {
        return Ok(allow_manage_all);
    }
    Err(ApiError::forbidden("query session belongs to another principal"))
}

async fn append_session_mutation_audit(
    state: &AppState,
    auth: &AuthContext,
    session: &QueryConversationRow,
    action_kind: &str,
    redacted_message: &str,
) {
    if let Err(error) = state
        .canonical_services
        .audit
        .append_event(
            state,
            AppendAuditEventCommand {
                actor_principal_id: Some(auth.principal_id),
                surface_kind: "ui".to_string(),
                action_kind: action_kind.to_string(),
                request_id: None,
                trace_id: None,
                result_kind: "succeeded".to_string(),
                redacted_message: Some(redacted_message.to_string()),
                internal_message: None,
                subjects: vec![state.canonical_services.audit.query_session_subject(
                    session.id,
                    session.workspace_id,
                    session.library_id,
                )],
            },
        )
        .await
    {
        tracing::warn!(stage = "audit", error = %error, "audit append failed");
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use chrono::Utc;

    use crate::{
        domains::{iam::PrincipalKind, query::QueryConversationState},
        infra::repositories::{iam_repository::SystemRole, query_repository::QueryConversationRow},
        interfaces::http::{
            auth::{AuthContext, AuthGrant, AuthTokenKind},
            authorization::PERMISSION_LIBRARY_WRITE,
            router_support::ApiError,
        },
    };

    use super::*;

    fn session(owner: Uuid, workspace_id: Uuid, library_id: Uuid) -> QueryConversationRow {
        QueryConversationRow {
            id: Uuid::now_v7(),
            workspace_id,
            library_id,
            created_by_principal_id: Some(owner),
            title: Some("Session".to_string()),
            conversation_state: QueryConversationState::Active,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn auth(principal_id: Uuid, role: SystemRole) -> AuthContext {
        AuthContext {
            token_id: Uuid::now_v7(),
            principal_id,
            parent_principal_id: None,
            workspace_id: None,
            token_kind: AuthTokenKind::Principal(PrincipalKind::User),
            scopes: Vec::new(),
            grants: Vec::new(),
            workspace_memberships: Vec::new(),
            visible_workspace_ids: BTreeSet::new(),
            is_system_admin: false,
            system_role: Some(role),
        }
    }

    #[test]
    fn owner_with_write_capability_may_mutate_without_manager_override() {
        let owner = Uuid::now_v7();
        let session = session(owner, Uuid::now_v7(), Uuid::now_v7());

        let outcome = authorize_session_mutation(&auth(owner, SystemRole::Operator), &session);

        assert!(outcome.is_ok());
        assert_eq!(outcome.ok(), Some(false));
    }

    #[test]
    fn viewer_cannot_mutate_even_an_owned_session() {
        let owner = Uuid::now_v7();
        let session = session(owner, Uuid::now_v7(), Uuid::now_v7());

        assert!(matches!(
            authorize_session_mutation(&auth(owner, SystemRole::Viewer), &session),
            Err(ApiError::Unauthorized),
        ));
    }

    #[test]
    fn scoped_library_manager_may_mutate_another_principals_session() {
        let workspace_id = Uuid::now_v7();
        let library_id = Uuid::now_v7();
        let mut manager = auth(Uuid::now_v7(), SystemRole::Operator);
        manager.grants.push(AuthGrant {
            id: Uuid::now_v7(),
            resource_kind: "library".to_string(),
            resource_id: library_id,
            permission_kind: PERMISSION_LIBRARY_WRITE.to_string(),
            workspace_id: Some(workspace_id),
            library_id: Some(library_id),
            document_id: None,
        });

        let outcome = authorize_session_mutation(
            &manager,
            &session(Uuid::now_v7(), workspace_id, library_id),
        );

        assert!(outcome.is_ok());
        assert_eq!(outcome.ok(), Some(true));
    }

    #[test]
    fn non_owner_without_management_permission_is_forbidden() {
        let caller = auth(Uuid::now_v7(), SystemRole::Operator);
        let session = session(Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());

        assert!(matches!(
            authorize_session_mutation(&caller, &session),
            Err(ApiError::Forbidden(_)),
        ));
    }
}
