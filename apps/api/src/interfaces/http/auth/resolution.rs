use std::collections::BTreeSet;

use axum::{
    extract::FromRequestParts,
    http::{HeaderMap, header, request::Parts},
};
use chrono::Utc;
use tracing::warn;
use uuid::Uuid;

use super::context::{AuthContext, AuthGrant, AuthTokenKind, AuthWorkspaceMembership};
use super::tokens::{hash_session_secret, hash_token, parse_session_cookie_value};
use crate::{
    app::state::AppState,
    domains::iam::{PrincipalKind, SessionLifetimeSnapshot, SessionLifetimeState},
    infra::repositories::iam_repository::{self, SystemRole},
    interfaces::http::router_support::ApiError,
    shared::auth_tokens,
};

const AUTH_ACTIVITY_REFRESH_INTERVAL_SECS: i64 = 30;

/// Resolves the optional auth context represented by the incoming request headers.
///
/// # Errors
///
/// Returns an [`ApiError`] when the headers contain an invalid or unverifiable token.
pub async fn resolve_optional_auth_context_from_headers(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<Option<AuthContext>, ApiError> {
    let auth_header = headers.get(header::AUTHORIZATION).cloned();

    if let Some(auth_header) = auth_header {
        let header_value = auth_header.to_str().map_err(|_| ApiError::Unauthorized)?.to_owned();
        let token = header_value.strip_prefix("Bearer ").ok_or(ApiError::Unauthorized)?;

        if !auth_tokens::is_well_formed_api_token(token) {
            return Err(ApiError::Unauthorized);
        }
        let token_hash = hash_token(token);
        let token_row = iam_repository::find_active_api_token_by_secret_hash(
            &state.persistence.postgres,
            &token_hash,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        .ok_or(ApiError::Unauthorized)?;

        let now = Utc::now();
        if auth_activity_refresh_due(token_row.last_used_at, now) {
            let postgres = state.persistence.postgres.clone();
            let stale_before = now - chrono::Duration::seconds(AUTH_ACTIVITY_REFRESH_INTERVAL_SECS);
            let principal_id = token_row.principal_id;
            tokio::spawn(async move {
                if let Err(error) =
                    iam_repository::touch_api_token_if_stale(&postgres, principal_id, stale_before)
                        .await
                {
                    warn!(
                        %principal_id,
                        ?error,
                        "failed to refresh iam api token last_used_at",
                    );
                }
            });
        }

        let context = build_auth_context_for_principal(
            state,
            token_row.principal_id,
            token_row.principal_id,
            AuthTokenKind::Principal(parse_principal_kind(&token_row.principal_kind)?),
            token_row.workspace_id,
            token_row.parent_principal_id,
        )
        .await?;
        return Ok(Some(context));
    }

    let Some(cookie_value) = read_cookie(headers, state.ui_session_cookie.name) else {
        return Ok(None);
    };

    let (session_id, session_secret) =
        parse_session_cookie_value(&cookie_value).ok_or(ApiError::Unauthorized)?;
    let session_row = iam_repository::get_session_by_id(&state.persistence.postgres, session_id)
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        .ok_or(ApiError::Unauthorized)?;
    let now = Utc::now();
    let lifetime = state.ui_session_cookie.lifetime.evaluate(
        SessionLifetimeSnapshot {
            expires_at: session_row.expires_at,
            last_seen_at: session_row.last_seen_at,
            revoked_at: session_row.revoked_at,
        },
        now,
    );
    if !lifetime.is_active() {
        if matches!(
            lifetime,
            SessionLifetimeState::IdleExpired | SessionLifetimeState::AbsoluteExpired
        ) {
            tracing::debug!(
                session_id = %session_row.id,
                reason = ?lifetime,
                "rejected an expired ui session",
            );
        }
        return Err(ApiError::Unauthorized);
    }
    if session_row.session_secret_hash != hash_session_secret(&session_secret) {
        return Err(ApiError::Unauthorized);
    }

    // Only a session that just authenticated may slide its idle window
    // forward; the expiry decision above deliberately runs first.
    if auth_activity_refresh_due(Some(session_row.last_seen_at), now) {
        let postgres = state.persistence.postgres.clone();
        let stale_before = now - chrono::Duration::seconds(AUTH_ACTIVITY_REFRESH_INTERVAL_SECS);
        tokio::spawn(async move {
            if let Err(error) =
                iam_repository::touch_session_if_stale(&postgres, session_id, stale_before).await
            {
                warn!(
                    %session_id,
                    ?error,
                    "failed to refresh iam session last_seen_at",
                );
            }
        });
    }

    let context = build_auth_context_for_principal(
        state,
        session_row.principal_id,
        session_row.id,
        AuthTokenKind::Session,
        None,
        None,
    )
    .await?;
    Ok(Some(context))
}

fn auth_activity_refresh_due(
    last_seen_at: Option<chrono::DateTime<chrono::Utc>>,
    now: chrono::DateTime<chrono::Utc>,
) -> bool {
    match last_seen_at {
        Some(last_seen_at) => {
            now.signed_duration_since(last_seen_at).num_seconds()
                >= AUTH_ACTIVITY_REFRESH_INTERVAL_SECS
        }
        None => true,
    }
}

fn read_cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    headers.get(header::COOKIE).and_then(|value| value.to_str().ok()).and_then(|value| {
        value.split(';').find_map(|pair| {
            let mut parts = pair.trim().splitn(2, '=');
            match (parts.next(), parts.next()) {
                (Some(cookie_name), Some(cookie_value)) if cookie_name == name => {
                    Some(cookie_value.to_string())
                }
                _ => None,
            }
        })
    })
}

async fn build_auth_context_for_principal(
    state: &AppState,
    principal_id: Uuid,
    token_id: Uuid,
    token_kind: AuthTokenKind,
    workspace_id: Option<Uuid>,
    parent_principal_id: Option<Uuid>,
) -> Result<AuthContext, ApiError> {
    let mut grants = iam_repository::list_resolved_grants_by_principal(
        &state.persistence.postgres,
        principal_id,
    )
    .await
    .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
    let mut memberships = iam_repository::list_workspace_memberships_by_principal(
        &state.persistence.postgres,
        principal_id,
    )
    .await
    .map_err(|e| ApiError::internal_with_log(e, "internal"))?;

    if let Some(token_workspace_id) = workspace_id {
        grants.retain(|grant| grant.workspace_id == Some(token_workspace_id));
        memberships.retain(|membership| membership.workspace_id == token_workspace_id);
    }

    // System roles belong exclusively to human user principals. API tokens do
    // not inherit a parent user role and therefore cannot administer IAM.
    let system_role =
        iam_repository::get_user_by_principal_id(&state.persistence.postgres, principal_id)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
            .map(|user| user.system_role());

    let is_system_admin = system_role == Some(SystemRole::Admin);
    let scopes = collect_permission_kinds(&grants);
    let workspace_memberships = memberships
        .into_iter()
        .filter(|membership| membership.membership_state == "active")
        .map(|membership| AuthWorkspaceMembership {
            workspace_id: membership.workspace_id,
            membership_state: membership.membership_state,
        })
        .collect::<Vec<_>>();
    let auth_grants = grants
        .into_iter()
        .map(|grant| AuthGrant {
            id: grant.id,
            resource_kind: grant.resource_kind,
            resource_id: grant.resource_id,
            permission_kind: grant.permission_kind,
            workspace_id: grant.workspace_id,
            library_id: grant.library_id,
            document_id: grant.document_id,
        })
        .collect::<Vec<_>>();
    let mut visible_workspace_ids = workspace_memberships
        .iter()
        .map(|membership| membership.workspace_id)
        .collect::<BTreeSet<_>>();
    visible_workspace_ids.extend(
        auth_grants
            .iter()
            .filter(|grant| grant.resource_kind == "workspace")
            .filter_map(|grant| grant.workspace_id),
    );

    Ok(AuthContext {
        token_id,
        principal_id,
        parent_principal_id,
        workspace_id,
        token_kind,
        scopes,
        grants: auth_grants,
        workspace_memberships,
        visible_workspace_ids,
        is_system_admin,
        system_role,
    })
}

fn parse_principal_kind(value: &str) -> Result<PrincipalKind, ApiError> {
    match value {
        "user" => Ok(PrincipalKind::User),
        "api_token" => Ok(PrincipalKind::ApiToken),
        "worker" => Ok(PrincipalKind::Worker),
        "bootstrap" => Ok(PrincipalKind::Bootstrap),
        _ => Err(ApiError::Unauthorized),
    }
}

fn collect_permission_kinds(grants: &[iam_repository::ResolvedIamGrantScopeRow]) -> Vec<String> {
    let mut permissions = BTreeSet::new();
    for grant in grants {
        permissions.insert(grant.permission_kind.clone());
    }
    permissions.into_iter().collect()
}

impl FromRequestParts<AppState> for AuthContext {
    type Rejection = ApiError;

    fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> impl std::future::Future<Output = Result<Self, Self::Rejection>> + Send {
        let request_auth = parts
            .extensions
            .get::<crate::interfaces::http::middleware::auth::RequestAuth>()
            .cloned();
        let headers = parts.headers.clone();
        let state = state.clone();

        async move {
            if let Some(request_auth) = request_auth {
                return request_auth.required_context();
            }

            resolve_optional_auth_context_from_headers(&state, &headers)
                .await?
                .ok_or(ApiError::Unauthorized)
        }
    }
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};

    use super::{AUTH_ACTIVITY_REFRESH_INTERVAL_SECS, auth_activity_refresh_due};

    #[test]
    fn auth_activity_refresh_is_skipped_inside_refresh_window() {
        let now = Utc.timestamp_opt(1_700_000_000, 0).single().expect("valid timestamp");
        let last_seen_at = now - chrono::Duration::seconds(29);

        assert!(!auth_activity_refresh_due(Some(last_seen_at), now));
    }

    #[test]
    fn auth_activity_refresh_is_due_at_threshold() {
        let now = Utc.timestamp_opt(1_700_000_000, 0).single().expect("valid timestamp");
        let last_seen_at = now - chrono::Duration::seconds(AUTH_ACTIVITY_REFRESH_INTERVAL_SECS);

        assert!(auth_activity_refresh_due(Some(last_seen_at), now));
    }

    #[test]
    fn auth_activity_refresh_is_due_when_timestamp_missing() {
        let now = Utc.timestamp_opt(1_700_000_000, 0).single().expect("valid timestamp");

        assert!(auth_activity_refresh_due(None, now));
    }
}
