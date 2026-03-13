use axum::{
    Json, Router,
    extract::{FromRequestParts, Path, Query, State},
    http::{StatusCode, header, request::Parts},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    app::state::AppState, infra::repositories, interfaces::http::router_support::ApiError,
};

#[derive(Clone, Debug)]
pub struct AuthContext {
    pub token_id: Uuid,
    pub workspace_id: Option<Uuid>,
    pub token_kind: String,
    pub scopes: Vec<String>,
}

impl AuthContext {
    /// Validates that the token has at least one accepted scope.
    ///
    /// # Errors
    /// Returns [`ApiError::Unauthorized`] when the token lacks all accepted scopes.
    pub fn require_any_scope(&self, accepted: &[&str]) -> Result<(), ApiError> {
        if self.token_kind == "instance_admin" {
            return Ok(());
        }

        if self.scopes.iter().any(|scope| accepted.iter().any(|wanted| scope == wanted)) {
            return Ok(());
        }

        Err(ApiError::Unauthorized)
    }

    /// Ensures the caller is allowed to access a specific workspace.
    ///
    /// Instance administrators may access any workspace. Workspace-scoped tokens must
    /// match the target workspace id exactly.
    ///
    /// # Errors
    /// Returns [`ApiError::Unauthorized`] when the caller does not belong to the target workspace.
    pub fn require_workspace_access(&self, workspace_id: Uuid) -> Result<(), ApiError> {
        if self.token_kind == "instance_admin" {
            return Ok(());
        }

        match self.workspace_id {
            Some(token_workspace_id) if token_workspace_id == workspace_id => Ok(()),
            _ => Err(ApiError::Unauthorized),
        }
    }
}

#[derive(Serialize)]
pub struct TokenCreateResponse {
    pub id: Uuid,
    pub workspace_id: Option<Uuid>,
    pub token_kind: String,
    pub label: String,
    pub token: String,
    pub scopes: Vec<String>,
}

#[derive(Serialize)]
pub struct TokenSummary {
    pub id: Uuid,
    pub workspace_id: Option<Uuid>,
    pub token_kind: String,
    pub label: String,
    pub status: String,
    pub scopes: Vec<String>,
    pub last_used_at: Option<chrono::DateTime<chrono::Utc>>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Deserialize)]
pub struct CreateTokenRequest {
    pub workspace_id: Option<Uuid>,
    pub token_kind: String,
    pub label: String,
    pub scopes: Vec<String>,
}

#[derive(Deserialize)]
pub struct ListTokensQuery {
    pub workspace_id: Option<Uuid>,
}

pub fn router() -> Router<crate::app::state::AppState> {
    Router::new()
        .route("/auth/tokens", axum::routing::post(create_token).get(list_tokens))
        .route("/auth/tokens/{id}", axum::routing::get(get_token))
}

#[must_use]
pub fn hash_token(raw: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(raw.as_bytes());
    hex::encode(hasher.finalize())
}

#[must_use]
pub fn mint_plaintext_token() -> String {
    format!("rtrg_{}", Uuid::now_v7().simple())
}

/// Creates a new API token and returns the plaintext token once.
///
/// # Errors
/// Returns [`ApiError::BadRequest`] for invalid payloads and [`ApiError::Internal`]
/// when token persistence or scope serialization fails.
pub async fn create_token(
    State(state): State<AppState>,
    Json(payload): Json<CreateTokenRequest>,
) -> Result<Json<TokenCreateResponse>, ApiError> {
    if payload.token_kind.trim().is_empty() || payload.label.trim().is_empty() {
        return Err(ApiError::BadRequest("token_kind and label must not be empty".into()));
    }

    let plaintext = mint_plaintext_token();
    let token_hash = hash_token(&plaintext);
    let scope_json = serde_json::to_value(&payload.scopes).map_err(|_| ApiError::Internal)?;

    let row = repositories::create_api_token(
        &state.persistence.postgres,
        payload.workspace_id,
        &payload.token_kind,
        &payload.label,
        &token_hash,
        scope_json,
    )
    .await
    .map_err(|_| ApiError::Internal)?;

    Ok(Json(TokenCreateResponse {
        id: row.id,
        workspace_id: row.workspace_id,
        token_kind: row.token_kind,
        label: row.label,
        token: plaintext,
        scopes: payload.scopes,
    }))
}

/// Lists API tokens visible to a workspace administrator.
///
/// # Errors
/// Returns [`ApiError::Unauthorized`] when the caller lacks admin scope and
/// [`ApiError::Internal`] when token loading fails.
pub async fn list_tokens(
    auth: AuthContext,
    State(state): State<AppState>,
    Query(query): Query<ListTokensQuery>,
) -> Result<Json<Vec<TokenSummary>>, ApiError> {
    auth.require_any_scope(&["workspace:admin"])?;

    let items = repositories::list_api_tokens(&state.persistence.postgres, query.workspace_id)
        .await
        .map_err(|_| ApiError::Internal)?
        .into_iter()
        .map(map_token_summary)
        .collect();

    Ok(Json(items))
}

/// Returns a single API token summary by id.
///
/// # Errors
/// Returns [`ApiError::Unauthorized`] when the caller lacks admin scope,
/// [`ApiError::NotFound`] when the token does not exist, and [`ApiError::Internal`]
/// when token loading fails.
pub async fn get_token(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<TokenSummary>, ApiError> {
    auth.require_any_scope(&["workspace:admin"])?;

    let row = repositories::get_api_token_by_id(&state.persistence.postgres, id)
        .await
        .map_err(|_| ApiError::Internal)?
        .ok_or_else(|| ApiError::NotFound(format!("api_token {id} not found")))?;

    Ok(Json(map_token_summary(row)))
}

fn map_token_summary(row: repositories::ApiTokenRow) -> TokenSummary {
    let scopes = serde_json::from_value(row.scope_json).unwrap_or_default();

    TokenSummary {
        id: row.id,
        workspace_id: row.workspace_id,
        token_kind: row.token_kind,
        label: row.label,
        status: row.status,
        scopes,
        last_used_at: row.last_used_at,
        created_at: row.created_at,
        updated_at: row.updated_at,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace_token(workspace_id: Option<Uuid>) -> AuthContext {
        AuthContext {
            token_id: Uuid::now_v7(),
            workspace_id,
            token_kind: "workspace_token".into(),
            scopes: vec!["workspace:admin".into()],
        }
    }

    #[test]
    fn workspace_access_allows_matching_workspace() {
        let workspace_id = Uuid::now_v7();
        let auth = workspace_token(Some(workspace_id));

        assert!(auth.require_workspace_access(workspace_id).is_ok());
    }

    #[test]
    fn workspace_access_rejects_mismatched_workspace() {
        let auth = workspace_token(Some(Uuid::now_v7()));

        assert!(matches!(
            auth.require_workspace_access(Uuid::now_v7()),
            Err(ApiError::Unauthorized)
        ));
    }

    #[test]
    fn instance_admin_can_access_any_workspace() {
        let auth = AuthContext {
            token_id: Uuid::now_v7(),
            workspace_id: None,
            token_kind: "instance_admin".into(),
            scopes: Vec::new(),
        };

        assert!(auth.require_workspace_access(Uuid::now_v7()).is_ok());
    }
}

impl FromRequestParts<AppState> for AuthContext {
    type Rejection = (StatusCode, &'static str);

    fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> impl std::future::Future<Output = Result<Self, Self::Rejection>> + Send {
        let auth_header = parts.headers.get(header::AUTHORIZATION).cloned();
        let state = state.clone();

        async move {
            let header_value = auth_header
                .ok_or((StatusCode::UNAUTHORIZED, "missing authorization header"))?
                .to_str()
                .map_err(|_| (StatusCode::UNAUTHORIZED, "invalid authorization header"))?
                .to_owned();

            let token = header_value
                .strip_prefix("Bearer ")
                .ok_or((StatusCode::UNAUTHORIZED, "expected bearer token"))?;

            let token_hash = hash_token(token);
            let row =
                repositories::find_api_token_by_hash(&state.persistence.postgres, &token_hash)
                    .await
                    .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "token lookup failed"))?
                    .ok_or((StatusCode::UNAUTHORIZED, "invalid token"))?;

            repositories::touch_api_token_last_used(&state.persistence.postgres, row.id)
                .await
                .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "token touch failed"))?;

            let scopes: Vec<String> = serde_json::from_value(row.scope_json)
                .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "invalid token scopes"))?;

            Ok(Self {
                token_id: row.id,
                workspace_id: row.workspace_id,
                token_kind: row.token_kind,
                scopes,
            })
        }
    }
}
