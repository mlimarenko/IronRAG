use std::future::Future;

use chrono::Utc;
use cookie::{Cookie, SameSite};
use uuid::Uuid;

use axum::{
    extract::FromRequestParts,
    http::{HeaderMap, StatusCode, header, request::Parts},
};

use crate::{
    app::state::AppState, infra::repositories, interfaces::http::router_support::ApiError,
};

#[derive(Clone, Debug)]
pub struct UiSessionContext {
    pub session_id: Uuid,
    pub user_id: Uuid,
    pub email: String,
    pub display_name: String,
    pub role_label: String,
    pub locale: String,
    pub active_workspace_id: Option<Uuid>,
    pub active_project_id: Option<Uuid>,
}

#[derive(Clone, Debug)]
pub struct UiActiveContext {
    pub workspace: repositories::WorkspaceRow,
    pub project: repositories::ProjectRow,
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

pub fn build_session_cookie(cookie_name: &str, value: &str, max_age_hours: u64) -> String {
    Cookie::build((cookie_name, value.to_string()))
        .path("/")
        .http_only(true)
        .same_site(SameSite::Lax)
        .max_age(cookie::time::Duration::hours(i64::try_from(max_age_hours).unwrap_or(720)))
        .build()
        .to_string()
}

pub fn build_cleared_session_cookie(cookie_name: &str) -> String {
    Cookie::build((cookie_name, String::new()))
        .path("/")
        .http_only(true)
        .same_site(SameSite::Lax)
        .max_age(cookie::time::Duration::seconds(0))
        .build()
        .to_string()
}

impl FromRequestParts<AppState> for UiSessionContext {
    type Rejection = ApiError;

    fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> impl Future<Output = Result<Self, Self::Rejection>> + Send {
        let headers = parts.headers.clone();
        let postgres = state.persistence.postgres.clone();

        async move {
            let session_cookie = read_cookie(&headers, state.ui_session_cookie.name)
                .ok_or(ApiError::Unauthorized)?;
            let session_id = session_cookie.parse::<Uuid>().map_err(|_| ApiError::Unauthorized)?;
            let session = repositories::get_ui_session_by_id(&postgres, session_id)
                .await
                .map_err(|_| ApiError::Internal)?
                .ok_or(ApiError::Unauthorized)?;
            if session.expires_at < Utc::now() {
                let _ = repositories::revoke_ui_session(&postgres, session.id).await;
                return Err(ApiError::Unauthorized);
            }
            let user = repositories::get_ui_user_by_id(&postgres, session.user_id)
                .await
                .map_err(|_| ApiError::Internal)?
                .ok_or(ApiError::Unauthorized)?;

            Ok(Self {
                session_id: session.id,
                user_id: user.id,
                email: user.email,
                display_name: user.display_name,
                role_label: user.role_label,
                locale: session.locale,
                active_workspace_id: session.active_workspace_id,
                active_project_id: session.active_project_id,
            })
        }
    }
}

pub fn parse_locale(raw: &str, fallback: &str) -> String {
    match raw.trim().to_lowercase().as_str() {
        "en" => "en".to_string(),
        "ru" => "ru".to_string(),
        _ => fallback.to_string(),
    }
}

pub fn require_admin_role(role_label: &str) -> Result<(), ApiError> {
    match role_label.to_lowercase().as_str() {
        "owner" | "admin" => Ok(()),
        _ => Err(ApiError::Unauthorized),
    }
}

pub fn unauthorized_response() -> (StatusCode, &'static str) {
    (StatusCode::UNAUTHORIZED, "unauthorized")
}

pub async fn load_active_ui_context(
    state: &AppState,
    ui_session: &UiSessionContext,
) -> Result<UiActiveContext, ApiError> {
    let workspace_id = ui_session
        .active_workspace_id
        .ok_or_else(|| ApiError::Conflict("ui session has no active workspace".into()))?;
    let project_id = ui_session
        .active_project_id
        .ok_or_else(|| ApiError::Conflict("ui session has no active library".into()))?;

    let workspace =
        repositories::list_workspaces_for_ui_user(&state.persistence.postgres, ui_session.user_id)
            .await
            .map_err(|_| ApiError::Internal)?
            .into_iter()
            .find(|item| item.id == workspace_id)
            .ok_or(ApiError::Unauthorized)?;

    let project = repositories::list_projects_for_ui_user(
        &state.persistence.postgres,
        ui_session.user_id,
        workspace.id,
    )
    .await
    .map_err(|_| ApiError::Internal)?
    .into_iter()
    .find(|item| item.id == project_id)
    .ok_or(ApiError::Unauthorized)?;

    Ok(UiActiveContext { workspace, project })
}
