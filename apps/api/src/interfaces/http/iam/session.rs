use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode, header},
    response::IntoResponse,
};
use tracing::warn;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    interfaces::http::{
        auth::{AuthContext, build_session_cookie_value},
        router_support::{ApiError, RequestId},
        ui_support::{
            build_cleared_session_cookie, build_session_cookie, session_cookie_secure_for_request,
        },
    },
    interfaces::shell::{build_shell_bootstrap, parse_ui_locale, to_bootstrap_contract},
    services::iam::{
        audit::{AppendAuditEventCommand, AppendAuditEventSubjectCommand},
        service::{AuthenticateSessionCommand, BootstrapSetupAiCommand, BootstrapSetupCommand},
    },
};

use super::{
    load_contract_me,
    types::{BootstrapSetupRequest, LoginSessionRequest, SessionResponse, SessionUserResponse},
};

#[utoipa::path(
    get,
    path = "/v1/iam/session/resolve",
    tag = "iam",
    operation_id = "resolveIamSession",
    security(),
    responses(
        (status = 200, description = "Composite session view (mode + session + me + shell bootstrap + bootstrap status). Always 200 — the canonical UI shell calls this on every navigation.", body = ironrag_contracts::auth::SessionResolveResponse),
    ),
)]
#[tracing::instrument(level = "info", name = "http.resolve_session", skip_all)]
pub async fn resolve_session(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<ironrag_contracts::auth::SessionResolveResponse>, ApiError> {
    let bootstrap_status_outcome =
        state.canonical_services.iam.get_bootstrap_status(&state).await?;
    let bootstrap_status = to_bootstrap_contract(&bootstrap_status_outcome);
    let locale = parse_ui_locale(&state.ui_runtime.default_locale);

    match crate::interfaces::http::auth::resolve_optional_auth_context_from_headers(&state, &headers)
        .await
    {
        Ok(Some(auth)) if auth.token_kind.is_session() => {
            let session = load_contract_session(&state, &auth).await?;
            let me = load_contract_me(&state, &auth).await?;
            let shell_bootstrap = build_shell_bootstrap(&state, &auth).await?;

            Ok(Json(ironrag_contracts::auth::SessionResolveResponse {
                mode: ironrag_contracts::auth::SessionMode::Authenticated,
                locale,
                session: Some(session),
                me: Some(me),
                shell_bootstrap: Some(shell_bootstrap),
                bootstrap_status,
                message: None,
            }))
        }
        Ok(Some(_)) => Ok(Json(ironrag_contracts::auth::SessionResolveResponse {
            mode: session_mode_from_bootstrap(&bootstrap_status),
            locale,
            session: None,
            me: None,
            shell_bootstrap: None,
            bootstrap_status,
            message: Some(
                "Browser shell requires the canonical UI session cookie and does not accept bearer tokens."
                    .to_string(),
            ),
        })),
        Ok(None) | Err(ApiError::Unauthorized) => {
            Ok(Json(ironrag_contracts::auth::SessionResolveResponse {
                mode: session_mode_from_bootstrap(&bootstrap_status),
                locale,
                session: None,
                me: None,
                shell_bootstrap: None,
                bootstrap_status,
                message: None,
            }))
        }
        Err(error) => {
            warn!(?error, "failed to resolve optional auth context for session restore");
            Ok(Json(ironrag_contracts::auth::SessionResolveResponse {
                mode: session_mode_from_bootstrap(&bootstrap_status),
                locale,
                session: None,
                me: None,
                shell_bootstrap: None,
                bootstrap_status,
                message: Some(
                    "Session restore could not validate the current browser credentials.".to_string(),
                ),
            }))
        }
    }
}

#[utoipa::path(
    get,
    path = "/v1/iam/bootstrap/status",
    tag = "iam",
    operation_id = "getBootstrapStatus",
    security(),
    responses(
        (status = 200, description = "Bootstrap status indicating whether first-run admin setup is required", body = ironrag_contracts::auth::BootstrapStatus),
    ),
)]
#[tracing::instrument(level = "info", name = "http.get_bootstrap_status", skip_all)]
pub async fn get_bootstrap_status(
    State(state): State<AppState>,
) -> Result<Json<ironrag_contracts::auth::BootstrapStatus>, ApiError> {
    let outcome = state.canonical_services.iam.get_bootstrap_status(&state).await?;
    Ok(Json(to_bootstrap_contract(&outcome)))
}

#[utoipa::path(
    post,
    path = "/v1/iam/bootstrap/setup",
    tag = "iam",
    operation_id = "postBootstrapSetup",
    security(),
    request_body = BootstrapSetupRequest,
    responses(
        (status = 200, description = "Bootstrap admin configured; sets the canonical UI session cookie", body = SessionResponse),
        (status = 409, description = "Bootstrap already completed"),
    ),
)]
#[tracing::instrument(level = "info", name = "http.setup_bootstrap_admin", skip_all)]
pub async fn setup_bootstrap_admin(
    State(state): State<AppState>,
    request_headers: HeaderMap,
    request_id: Option<axum::Extension<RequestId>>,
    Json(payload): Json<BootstrapSetupRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let request_id = request_id.map_or_else(|| Uuid::now_v7().to_string(), |value| value.0.0);
    let outcome = state
        .canonical_services
        .iam
        .setup_bootstrap_admin(
            &state,
            BootstrapSetupCommand {
                login: payload.login,
                display_name: payload.display_name,
                password: payload.password,
                ai_setup: payload.ai_setup.map(|ai_setup| BootstrapSetupAiCommand {
                    provider_kind: ai_setup.provider_kind,
                    api_key: ai_setup.api_key,
                    base_url: ai_setup.base_url,
                }),
                ttl_hours: state.ui_session_cookie.ttl_hours,
                request_id: request_id.clone(),
            },
        )
        .await?;

    let mut response_headers = HeaderMap::new();
    let cookie = build_session_cookie(
        state.ui_session_cookie.name,
        &build_session_cookie_value(outcome.session_id, &outcome.session_secret),
        state.ui_session_cookie.ttl_hours,
        session_cookie_secure_for_request(state.ui_session_cookie.secure, &request_headers),
    );
    response_headers.insert(
        header::SET_COOKIE,
        cookie.parse().map_err(|error| ApiError::internal_with_log(error, "internal"))?,
    );

    if let Err(error) = state
        .canonical_services
        .audit
        .append_event(
            &state,
            AppendAuditEventCommand {
                actor_principal_id: Some(outcome.principal_id),
                surface_kind: "rest".to_string(),
                action_kind: "iam.bootstrap.setup".to_string(),
                request_id: Some(request_id),
                trace_id: None,
                result_kind: "succeeded".to_string(),
                redacted_message: Some("bootstrap setup succeeded".to_string()),
                internal_message: Some(format!(
                    "principal {} configured bootstrap session {}",
                    outcome.principal_id, outcome.session_id
                )),
                subjects: vec![
                    AppendAuditEventSubjectCommand {
                        subject_kind: "principal".to_string(),
                        subject_id: outcome.principal_id,
                        workspace_id: None,
                        library_id: None,
                        document_id: None,
                    },
                    AppendAuditEventSubjectCommand {
                        subject_kind: "session".to_string(),
                        subject_id: outcome.session_id,
                        workspace_id: None,
                        library_id: None,
                        document_id: None,
                    },
                ],
            },
        )
        .await
    {
        warn!(stage = "audit", error = %error, "audit append failed");
    }

    Ok((
        response_headers,
        Json(SessionResponse {
            session_id: outcome.session_id,
            expires_at: outcome.expires_at,
            user: SessionUserResponse {
                principal_id: outcome.principal_id,
                login: outcome.login,
                email: outcome.email,
                display_name: outcome.display_name,
            },
        }),
    ))
}

#[utoipa::path(
    post,
    path = "/v1/iam/session/login",
    tag = "iam",
    operation_id = "loginIamSession",
    security(),
    request_body = LoginSessionRequest,
    responses(
        (status = 200, description = "Login succeeded; sets the canonical UI session cookie", body = SessionResponse),
        (status = 401, description = "Invalid credentials"),
    ),
)]
#[tracing::instrument(level = "info", name = "http.login_session", skip_all)]
pub async fn login_session(
    State(state): State<AppState>,
    request_headers: HeaderMap,
    request_id: Option<axum::Extension<RequestId>>,
    Json(payload): Json<LoginSessionRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let ttl_hours =
        if payload.remember_me.unwrap_or(false) { state.ui_session_cookie.ttl_hours } else { 24 };
    let outcome = state
        .canonical_services
        .iam
        .authenticate_session(
            &state,
            AuthenticateSessionCommand {
                login: payload.login,
                password: payload.password,
                ttl_hours,
            },
        )
        .await?;

    let mut response_headers = HeaderMap::new();
    let cookie = build_session_cookie(
        state.ui_session_cookie.name,
        &build_session_cookie_value(outcome.session_id, &outcome.session_secret),
        ttl_hours,
        session_cookie_secure_for_request(state.ui_session_cookie.secure, &request_headers),
    );
    response_headers.insert(
        header::SET_COOKIE,
        cookie.parse().map_err(|error| ApiError::internal_with_log(error, "internal"))?,
    );

    let audit_state = state.clone();
    let audit_request_id = request_id.map(|value| value.0.0);
    let audit_principal_id = outcome.principal_id;
    let audit_session_id = outcome.session_id;
    tokio::spawn(async move {
        if let Err(error) = audit_state
            .canonical_services
            .audit
            .append_event(
                &audit_state,
                AppendAuditEventCommand {
                    actor_principal_id: Some(audit_principal_id),
                    surface_kind: "rest".to_string(),
                    action_kind: "iam.session.login".to_string(),
                    request_id: audit_request_id,
                    trace_id: None,
                    result_kind: "succeeded".to_string(),
                    redacted_message: Some("session login succeeded".to_string()),
                    internal_message: Some(format!(
                        "principal {} created session {}",
                        audit_principal_id, audit_session_id
                    )),
                    subjects: vec![AppendAuditEventSubjectCommand {
                        subject_kind: "session".to_string(),
                        subject_id: audit_session_id,
                        workspace_id: None,
                        library_id: None,
                        document_id: None,
                    }],
                },
            )
            .await
        {
            warn!(
                principal_id = %audit_principal_id,
                session_id = %audit_session_id,
                ?error,
                "failed to append iam session login audit event",
            );
        }
    });

    Ok((
        response_headers,
        Json(SessionResponse {
            session_id: outcome.session_id,
            expires_at: outcome.expires_at,
            user: SessionUserResponse {
                principal_id: outcome.principal_id,
                login: outcome.login,
                email: outcome.email,
                display_name: outcome.display_name,
            },
        }),
    ))
}

#[utoipa::path(
    get,
    path = "/v1/iam/session",
    tag = "iam",
    operation_id = "getIamSession",
    responses(
        (status = 200, description = "Current authenticated session", body = SessionResponse),
        (status = 401, description = "Caller is not authenticated"),
    ),
)]
#[tracing::instrument(level = "info", name = "http.get_session", skip_all)]
pub async fn get_session(
    auth: AuthContext,
    State(state): State<AppState>,
) -> Result<Json<SessionResponse>, ApiError> {
    let session = load_contract_session(&state, &auth).await?;
    Ok(Json(map_contract_session_response(session)))
}

#[utoipa::path(
    post,
    path = "/v1/iam/session/logout",
    tag = "iam",
    operation_id = "logoutIamSession",
    responses(
        (status = 200, description = "Session revoked; clears the session cookie"),
        (status = 401, description = "Caller is not authenticated"),
    ),
)]
#[tracing::instrument(level = "info", name = "http.logout_session", skip_all)]
pub async fn logout_session(
    auth: AuthContext,
    State(state): State<AppState>,
    request_headers: HeaderMap,
    request_id: Option<axum::Extension<RequestId>>,
) -> Result<impl IntoResponse, ApiError> {
    auth.require_session_token()?;

    state.canonical_services.iam.revoke_session(&state, auth.token_id).await?;

    let mut response_headers = HeaderMap::new();
    response_headers.insert(
        header::SET_COOKIE,
        build_cleared_session_cookie(
            state.ui_session_cookie.name,
            session_cookie_secure_for_request(state.ui_session_cookie.secure, &request_headers),
        )
        .parse()
        .map_err(|error| ApiError::internal_with_log(error, "internal"))?,
    );

    if let Err(error) = state
        .canonical_services
        .audit
        .append_event(
            &state,
            AppendAuditEventCommand {
                actor_principal_id: Some(auth.principal_id),
                surface_kind: "rest".to_string(),
                action_kind: "iam.session.logout".to_string(),
                request_id: request_id.map(|value| value.0.0),
                trace_id: None,
                result_kind: "succeeded".to_string(),
                redacted_message: Some("session logout succeeded".to_string()),
                internal_message: Some(format!(
                    "principal {} revoked session {}",
                    auth.principal_id, auth.token_id
                )),
                subjects: vec![AppendAuditEventSubjectCommand {
                    subject_kind: "session".to_string(),
                    subject_id: auth.token_id,
                    workspace_id: None,
                    library_id: None,
                    document_id: None,
                }],
            },
        )
        .await
    {
        warn!(stage = "audit", error = %error, "audit append failed");
    }

    Ok((response_headers, StatusCode::NO_CONTENT))
}

fn session_mode_from_bootstrap(
    bootstrap_status: &ironrag_contracts::auth::BootstrapStatus,
) -> ironrag_contracts::auth::SessionMode {
    if bootstrap_status.setup_required {
        ironrag_contracts::auth::SessionMode::BootstrapRequired
    } else {
        ironrag_contracts::auth::SessionMode::Guest
    }
}

async fn load_contract_session(
    state: &AppState,
    auth: &AuthContext,
) -> Result<ironrag_contracts::auth::AuthenticatedSession, ApiError> {
    auth.require_session_token()?;

    let session_row = crate::infra::repositories::iam_repository::get_session_by_id(
        &state.persistence.postgres,
        auth.token_id,
    )
    .await
    .map_err(|error| {
        tracing::error!(
            auth_principal_id = %auth.principal_id,
            session_id = %auth.token_id,
            ?error,
            "failed to load canonical session",
        );
        ApiError::Internal
    })?
    .ok_or_else(|| ApiError::resource_not_found("session", auth.token_id))?;
    let user_row = crate::infra::repositories::iam_repository::get_user_by_principal_id(
        &state.persistence.postgres,
        auth.principal_id,
    )
    .await
    .map_err(|error| {
        tracing::error!(
            auth_principal_id = %auth.principal_id,
            ?error,
            "failed to load canonical session user",
        );
        ApiError::Internal
    })?
    .ok_or(ApiError::Unauthorized)?;

    Ok(ironrag_contracts::auth::AuthenticatedSession {
        session_id: session_row.id,
        expires_at: session_row.expires_at,
        user: ironrag_contracts::auth::SessionUser {
            principal_id: user_row.principal_id,
            login: user_row.login,
            email: user_row.email,
            display_name: user_row.display_name,
        },
    })
}

fn map_contract_session_response(
    session: ironrag_contracts::auth::AuthenticatedSession,
) -> SessionResponse {
    SessionResponse {
        session_id: session.session_id,
        expires_at: session.expires_at,
        user: SessionUserResponse {
            principal_id: session.user.principal_id,
            login: session.user.login,
            email: session.user.email,
            display_name: session.user.display_name,
        },
    }
}
