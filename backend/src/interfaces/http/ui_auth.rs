use argon2::{
    Argon2,
    password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString, rand_core::OsRng},
};
use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode, header},
    response::IntoResponse,
};
use chrono::{Duration, Utc};
use serde::{Deserialize, Serialize};
use tracing::{error, info};
use uuid::Uuid;

use crate::{
    app::state::AppState,
    infra::{repositories, ui_queries},
    interfaces::http::{
        router_support::ApiError,
        ui_support::{
            UiSessionContext, build_cleared_session_cookie, build_session_cookie, parse_locale,
        },
    },
};

#[derive(Deserialize)]
pub struct LoginRequest {
    pub login: String,
    pub password: String,
    pub remember_me: Option<bool>,
    pub locale: Option<String>,
}

#[derive(Serialize)]
pub struct SessionUserResponse {
    pub id: Uuid,
    pub email: String,
    pub display_name: String,
    pub role_label: String,
    pub initials: String,
}

#[derive(Serialize)]
pub struct SessionResponse {
    pub session_id: Uuid,
    pub locale: String,
    pub active_workspace_id: Option<Uuid>,
    pub active_library_id: Option<Uuid>,
    pub expires_at: chrono::DateTime<chrono::Utc>,
    pub user: SessionUserResponse,
}

fn initials_from_name(display_name: &str) -> String {
    let initials = display_name
        .split_whitespace()
        .filter_map(|part| part.chars().next())
        .take(2)
        .collect::<String>();
    if initials.is_empty() { "RR".to_string() } else { initials.to_uppercase() }
}

fn hash_password(password: &str) -> Result<String, ApiError> {
    Argon2::default()
        .hash_password(password.as_bytes(), &SaltString::generate(&mut OsRng))
        .map(|hash| hash.to_string())
        .map_err(|_| ApiError::Internal)
}

fn verify_password(password: &str, password_hash: &str) -> Result<(), ApiError> {
    let parsed_hash = PasswordHash::new(password_hash).map_err(|_| ApiError::Internal)?;
    Argon2::default()
        .verify_password(password.as_bytes(), &parsed_hash)
        .map_err(|_| ApiError::Unauthorized)
}

async fn ensure_default_workspace_and_library(
    state: &AppState,
    user_id: Uuid,
    role_label: &str,
) -> Result<(repositories::WorkspaceRow, repositories::ProjectRow), ApiError> {
    if !state.settings.destructive_fresh_bootstrap_settings().allow_legacy_startup_side_effects {
        return Err(ApiError::forbidden(
            "legacy default workspace bootstrap is disabled; use canonical catalog bootstrap flows",
        ));
    }

    let workspace = repositories::find_or_create_default_workspace(&state.persistence.postgres)
        .await
        .map_err(|_| ApiError::Internal)?;

    repositories::ensure_workspace_member(
        &state.persistence.postgres,
        workspace.id,
        user_id,
        role_label,
    )
    .await
    .map_err(|_| ApiError::Internal)?;

    let project =
        repositories::find_or_create_default_project(&state.persistence.postgres, workspace.id)
            .await
            .map_err(|_| ApiError::Internal)?;

    repositories::ensure_project_access_grant(
        &state.persistence.postgres,
        project.id,
        user_id,
        "write",
    )
    .await
    .map_err(|_| ApiError::Internal)?;

    Ok((workspace, project))
}

pub async fn ensure_bootstrap_admin(state: &AppState) -> Result<(), ApiError> {
    let Some(bootstrap_admin) = state.ui_bootstrap_admin.clone() else {
        return Ok(());
    };

    let user = if let Some(existing) =
        repositories::get_ui_user_by_login(&state.persistence.postgres, &bootstrap_admin.login)
            .await
            .map_err(|_| ApiError::Internal)?
            .or(repositories::get_ui_user_by_email(
                &state.persistence.postgres,
                &bootstrap_admin.email,
            )
            .await
            .map_err(|_| ApiError::Internal)?)
    {
        existing
    } else {
        let should_create = if state.settings.has_explicit_ui_bootstrap_admin() {
            true
        } else if state.settings.environment.eq_ignore_ascii_case("local") {
            true
        } else {
            repositories::count_ui_users(&state.persistence.postgres)
                .await
                .map_err(|_| ApiError::Internal)?
                == 0
        };

        if !should_create {
            return Ok(());
        }

        let password_hash = hash_password(&bootstrap_admin.password)?;
        let created = repositories::create_ui_user(
            &state.persistence.postgres,
            &bootstrap_admin.login,
            &bootstrap_admin.email,
            &bootstrap_admin.display_name,
            "Owner",
            &password_hash,
            &state.ui_runtime.default_locale,
        )
        .await
        .map_err(|_| ApiError::Internal)?;
        info!(
            login = %created.login,
            email = %created.email,
            user_id = %created.id,
            explicit = state.settings.has_explicit_ui_bootstrap_admin(),
            "created bootstrap ui admin",
        );
        created
    };

    ensure_default_workspace_and_library(state, user.id, &user.role_label).await?;
    Ok(())
}

async fn create_session_response(
    state: &AppState,
    user: repositories::UiUserRow,
    locale: String,
    remember_me: bool,
) -> Result<(HeaderMap, SessionResponse), ApiError> {
    let (workspace, project) =
        ensure_default_workspace_and_library(state, user.id, &user.role_label).await?;
    let ttl_hours = if remember_me { state.ui_session_cookie.ttl_hours } else { 24 };
    let expires_at = Utc::now() + Duration::hours(i64::try_from(ttl_hours).unwrap_or(24));
    let session = repositories::create_ui_session(
        &state.persistence.postgres,
        user.id,
        Some(workspace.id),
        Some(project.id),
        &locale,
        expires_at,
    )
    .await
    .map_err(|_| ApiError::Internal)?;

    let mut headers = HeaderMap::new();
    let cookie_value =
        build_session_cookie(state.ui_session_cookie.name, &session.id.to_string(), ttl_hours);
    let header_value = cookie_value.parse().map_err(|_| ApiError::Internal)?;
    headers.insert(header::SET_COOKIE, header_value);

    Ok((
        headers,
        SessionResponse {
            session_id: session.id,
            locale,
            active_workspace_id: session.active_workspace_id,
            active_library_id: session.active_project_id,
            expires_at: session.expires_at,
            user: SessionUserResponse {
                id: user.id,
                email: user.email,
                display_name: user.display_name.clone(),
                role_label: user.role_label,
                initials: initials_from_name(&user.display_name),
            },
        },
    ))
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/ui/auth/login", axum::routing::post(login))
        .route("/ui/auth/session", axum::routing::get(get_session))
        .route("/ui/auth/logout", axum::routing::post(logout))
}

async fn login(
    State(state): State<AppState>,
    Json(payload): Json<LoginRequest>,
) -> Result<(HeaderMap, Json<SessionResponse>), ApiError> {
    let login = payload.login.trim().to_lowercase();
    if login.is_empty() || payload.password.trim().is_empty() {
        return Err(ApiError::BadRequest("login and password are required".into()));
    }
    let locale = parse_locale(
        payload.locale.as_deref().unwrap_or(&state.ui_runtime.default_locale),
        &state.ui_runtime.default_locale,
    );
    let user = repositories::get_ui_user_by_login(&state.persistence.postgres, &login)
        .await
        .map_err(|_| ApiError::Internal)?
        .ok_or(ApiError::Unauthorized)?;
    verify_password(payload.password.trim(), &user.password_hash)?;
    let (headers, response) =
        create_session_response(&state, user.clone(), locale, payload.remember_me.unwrap_or(false))
            .await?;
    info!(login = %login, user_id = %user.id, "created ui session from credentials");
    Ok((headers, Json(response)))
}

async fn get_session(
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Result<impl IntoResponse, ApiError> {
    let session_cookie =
        headers.get(header::COOKIE).and_then(|value| value.to_str().ok()).and_then(|value| {
            value.split(';').find_map(|pair| {
                let mut parts = pair.trim().splitn(2, '=');
                match (parts.next(), parts.next()) {
                    (Some(cookie_name), Some(cookie_value))
                        if cookie_name == state.ui_session_cookie.name =>
                    {
                        Some(cookie_value.to_string())
                    }
                    _ => None,
                }
            })
        });
    let Some(session_cookie) = session_cookie else {
        return Ok(StatusCode::NO_CONTENT.into_response());
    };
    let Ok(session_id) = session_cookie.parse::<Uuid>() else {
        return Ok(StatusCode::NO_CONTENT.into_response());
    };
    let Some(session) = repositories::get_ui_session_by_id(&state.persistence.postgres, session_id)
        .await
        .map_err(|_| ApiError::Internal)?
    else {
        return Ok(StatusCode::NO_CONTENT.into_response());
    };
    if session.expires_at < Utc::now() {
        let _ = repositories::revoke_ui_session(&state.persistence.postgres, session.id).await;
        return Ok(StatusCode::NO_CONTENT.into_response());
    }
    let Some(user) = repositories::get_ui_user_by_id(&state.persistence.postgres, session.user_id)
        .await
        .map_err(|_| ApiError::Internal)?
    else {
        return Ok(StatusCode::NO_CONTENT.into_response());
    };
    let response = ui_queries::map_ui_session(&session, &user);
    Ok(Json(SessionResponse {
        session_id: response.id,
        locale: response.locale,
        active_workspace_id: response.active_workspace_id,
        active_library_id: response.active_library_id,
        expires_at: response.expires_at,
        user: SessionUserResponse {
            id: response.user.id,
            email: response.user.email,
            display_name: response.user.display_name.clone(),
            role_label: response.user.role_label,
            initials: response.user.initials,
        },
    })
    .into_response())
}

async fn logout(
    ui_session: UiSessionContext,
    State(state): State<AppState>,
) -> Result<(HeaderMap, Json<serde_json::Value>), ApiError> {
    repositories::revoke_ui_session(&state.persistence.postgres, ui_session.session_id)
        .await
        .map_err(|error| {
            error!(session_id = %ui_session.session_id, ?error, "failed to revoke ui session");
            ApiError::Internal
        })?;
    let mut headers = HeaderMap::new();
    let cookie_value = build_cleared_session_cookie(state.ui_session_cookie.name);
    let header_value = cookie_value.parse().map_err(|_| ApiError::Internal)?;
    headers.insert(header::SET_COOKIE, header_value);
    Ok((headers, Json(serde_json::json!({ "ok": true }))))
}
