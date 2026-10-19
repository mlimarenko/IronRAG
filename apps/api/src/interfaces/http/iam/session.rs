use axum::{
    Json,
    extract::{Query, State},
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
    interfaces::shell::{
        ShellSelection, build_shell_bootstrap, parse_ui_locale, to_bootstrap_contract,
    },
    services::iam::{
        audit::{AppendAuditEventCommand, AppendAuditEventSubjectCommand},
        service::{
            AuthenticateSessionCommand, AuthenticateSessionOutcome, BootstrapSetupAiCommand,
            BootstrapSetupCommand,
        },
    },
};

use super::{
    load_contract_me,
    source_key::{OptionalConnectInfo, extract_login_source_key},
    types::{
        BootstrapSetupRequest, LoginSessionRequest, SessionContextQuery, SessionResponse,
        SessionUserResponse,
    },
};

/// Authentication mechanism recorded for a successful browser session.
///
/// The value is intentionally limited to product-owned vocabulary; no IdP
/// subject, authorization code, error text, or token becomes audit data.
#[derive(Clone, Copy)]
pub(super) enum SessionLoginMethod {
    Password,
    Oidc,
}

impl SessionLoginMethod {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Password => "password",
            Self::Oidc => "oidc",
        }
    }
}

/// Builds the response headers that hand the browser a freshly issued UI
/// session cookie; shared by bootstrap setup and login, which mint sessions
/// through different services but publish them identically.
pub(super) fn session_cookie_response_headers(
    state: &AppState,
    request_headers: &HeaderMap,
    session_id: Uuid,
    session_secret: &str,
) -> Result<HeaderMap, ApiError> {
    let cookie = build_session_cookie(
        state.ui_session_cookie.name,
        &build_session_cookie_value(session_id, session_secret),
        state.ui_session_cookie.lifetime.absolute_ttl_hours,
        session_cookie_secure_for_request(state.ui_session_cookie.secure, request_headers),
    );
    let mut response_headers = HeaderMap::new();
    response_headers.insert(
        header::SET_COOKIE,
        cookie.parse().map_err(|error| ApiError::internal_with_log(error, "internal"))?,
    );
    Ok(response_headers)
}

/// Appends the common successful-session audit event without delaying the
/// browser redirect/response. Both password and OIDC authentication create
/// the same durable session class and must therefore share this evidence.
pub(super) fn record_session_login_audit(
    state: &AppState,
    request_id: Option<String>,
    principal_id: Uuid,
    session_id: Uuid,
    method: SessionLoginMethod,
) {
    let audit_state = state.clone();
    tokio::spawn(async move {
        if let Err(error) = audit_state
            .application_services
            .audit
            .append_event(
                &audit_state,
                session_login_audit_command(request_id, principal_id, session_id, method),
            )
            .await
        {
            warn!(
                principal_id = %principal_id,
                session_id = %session_id,
                ?error,
                "failed to append iam session login audit event",
            );
        }
    });
}

fn session_login_audit_command(
    request_id: Option<String>,
    principal_id: Uuid,
    session_id: Uuid,
    method: SessionLoginMethod,
) -> AppendAuditEventCommand {
    let (redacted_message, internal_message) = match method {
        // Preserve the established password-login audit payload. OIDC adds a
        // bounded product-owned mechanism label without recording IdP data.
        SessionLoginMethod::Password => (
            "session login succeeded".to_string(),
            format!("principal {principal_id} created session {session_id}"),
        ),
        SessionLoginMethod::Oidc => {
            let method = method.as_str();
            (
                format!("session login succeeded via {method}"),
                format!("principal {principal_id} created session {session_id} via {method}"),
            )
        }
    };
    AppendAuditEventCommand {
        actor_principal_id: Some(principal_id),
        surface_kind: "rest".to_string(),
        action_kind: "iam.session.login".to_string(),
        request_id,
        trace_id: None,
        result_kind: "succeeded".to_string(),
        redacted_message: Some(redacted_message),
        internal_message: Some(internal_message),
        subjects: vec![AppendAuditEventSubjectCommand {
            subject_kind: "session".to_string(),
            subject_id: session_id,
            workspace_id: None,
            library_id: None,
            document_id: None,
        }],
    }
}

#[utoipa::path(
    get,
    path = "/v1/iam/context",
    tag = "iam",
    operation_id = "getIamContext",
    security(),
    params(SessionContextQuery),
    responses(
        (status = 200, description = "Composite session view (mode + session + me + shell bootstrap + bootstrap status). Always 200 — the UI shell calls this on every navigation.", body = ironrag_contracts::auth::SessionResolveResponse),
    ),
)]
#[tracing::instrument(level = "info", name = "http.resolve_session", skip_all)]
pub async fn resolve_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<SessionContextQuery>,
) -> Result<Json<ironrag_contracts::auth::SessionResolveResponse>, ApiError> {
    let bootstrap_status_outcome =
        state.application_services.iam.get_bootstrap_status(&state).await?;
    let bootstrap_status = to_bootstrap_contract(&bootstrap_status_outcome);
    let locale = parse_ui_locale(&state.ui_runtime.default_locale);

    match crate::interfaces::http::auth::resolve_optional_auth_context_from_headers(
        &state, &headers,
    )
    .await
    {
        Ok(Some(auth)) if auth.token_kind.is_session() => {
            let session = load_contract_session(&state, &auth).await?;
            let me = load_contract_me(&state, &auth).await?;
            let shell_bootstrap = build_shell_bootstrap(
                &state,
                &auth,
                ShellSelection {
                    active_workspace_id: query.active_workspace_id,
                    active_library_id: query.active_library_id,
                },
            )
            .await?;

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
                "Browser shell requires the UI session cookie and does not accept bearer tokens."
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
                    "Session restore could not validate the current browser credentials."
                        .to_string(),
                ),
            }))
        }
    }
}

#[utoipa::path(
    get,
    path = "/v1/iam/bootstrap",
    tag = "iam",
    operation_id = "getIamBootstrap",
    security(),
    responses(
        (status = 200, description = "Bootstrap status indicating whether first-run admin setup is required", body = ironrag_contracts::auth::BootstrapStatus),
    ),
)]
#[tracing::instrument(level = "info", name = "http.get_bootstrap_status", skip_all)]
pub async fn get_bootstrap_status(
    State(state): State<AppState>,
) -> Result<Json<ironrag_contracts::auth::BootstrapStatus>, ApiError> {
    let outcome = state.application_services.iam.get_bootstrap_status(&state).await?;
    Ok(Json(to_bootstrap_contract(&outcome)))
}

/// The `201` browser-session response shared by every path that creates a
/// durable session (bootstrap setup, password login, OIDC login resolution):
/// they all resolve to the same [`AuthenticateSessionOutcome`] shape, and the
/// browser must see one identical representation regardless of entry point.
fn created_session_response(
    response_headers: HeaderMap,
    mut outcome: AuthenticateSessionOutcome,
) -> (StatusCode, HeaderMap, Json<SessionResponse>) {
    (
        StatusCode::CREATED,
        response_headers,
        Json(SessionResponse {
            session_id: outcome.session_id,
            expires_at: outcome.expires_at,
            user: SessionUserResponse {
                principal_id: outcome.principal_id,
                login: std::mem::take(&mut outcome.login),
                email: std::mem::take(&mut outcome.email),
                display_name: std::mem::take(&mut outcome.display_name),
            },
        }),
    )
}

#[utoipa::path(
    post,
    path = "/v1/iam/bootstrap",
    tag = "iam",
    operation_id = "createIamBootstrap",
    security(),
    request_body = BootstrapSetupRequest,
    responses(
        (status = 201, description = "Bootstrap admin configured; sets the UI session cookie", body = SessionResponse),
        (status = 409, description = "Bootstrap already completed"),
    ),
)]
#[tracing::instrument(level = "info", name = "http.setup_bootstrap_admin", skip_all)]
pub async fn setup_bootstrap_admin(
    State(state): State<AppState>,
    request_headers: HeaderMap,
    request_id: Option<axum::Extension<RequestId>>,
    Json(mut payload): Json<BootstrapSetupRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let request_id = request_id.map_or_else(|| Uuid::now_v7().to_string(), |value| value.0.0);
    let outcome = state
        .application_services
        .iam
        .setup_bootstrap_admin(
            &state,
            BootstrapSetupCommand {
                login: std::mem::take(&mut payload.login),
                email: std::mem::take(&mut payload.email),
                display_name: payload.display_name.take(),
                password: std::mem::take(&mut payload.password),
                ai_setup: payload.ai_setup.take().map(|mut ai_setup| BootstrapSetupAiCommand {
                    provider_kind: std::mem::take(&mut ai_setup.provider_kind),
                    api_key: ai_setup.api_key.take(),
                    base_url: ai_setup.base_url.take(),
                }),
                request_id: request_id.clone(),
            },
        )
        .await?;

    let response_headers = session_cookie_response_headers(
        &state,
        &request_headers,
        outcome.session_id,
        &outcome.session_secret,
    )?;

    if let Err(error) = state
        .application_services
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

    Ok(created_session_response(response_headers, outcome))
}

#[utoipa::path(
    post,
    path = "/v1/iam/sessions",
    tag = "iam",
    operation_id = "createIamSession",
    security(),
    request_body = LoginSessionRequest,
    responses(
        (status = 200, description = "Login succeeded; sets the UI session cookie", body = SessionResponse),
        (status = 401, description = "Invalid credentials"),
        (status = 429, description = "Rate limited by login, by source, or by concurrent verification admission; retry after the given `Retry-After` interval"),
    ),
)]
#[tracing::instrument(level = "info", name = "http.login_session", skip_all)]
pub(crate) async fn login_session(
    State(state): State<AppState>,
    request_headers: HeaderMap,
    request_id: Option<axum::Extension<RequestId>>,
    OptionalConnectInfo(peer_addr): OptionalConnectInfo,
    Json(mut payload): Json<LoginSessionRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let source_key = extract_login_source_key(
        &request_headers,
        peer_addr,
        state.settings.login_rate_limit_trusted_proxy_hops,
    );
    let outcome = state
        .application_services
        .iam
        .authenticate_session(
            &state,
            AuthenticateSessionCommand {
                login: std::mem::take(&mut payload.login),
                password: std::mem::take(&mut payload.password),
                source_key,
            },
        )
        .await?;

    let response_headers = session_cookie_response_headers(
        &state,
        &request_headers,
        outcome.session_id,
        &outcome.session_secret,
    )?;

    record_session_login_audit(
        &state,
        request_id.map(|value| value.0.0),
        outcome.principal_id,
        outcome.session_id,
        SessionLoginMethod::Password,
    );

    Ok(created_session_response(response_headers, outcome))
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
    delete,
    path = "/v1/iam/session",
    tag = "iam",
    operation_id = "deleteIamSession",
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

    state.application_services.iam.revoke_session(&state, auth.token_id).await?;

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
        .application_services
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

const fn session_mode_from_bootstrap(
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
            "failed to load authenticated session",
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
            "failed to load authenticated session user",
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oidc_session_login_audit_uses_only_safe_product_metadata() {
        let principal_id = Uuid::from_u128(41);
        let session_id = Uuid::from_u128(42);
        let command = session_login_audit_command(
            Some("request-41".to_string()),
            principal_id,
            session_id,
            SessionLoginMethod::Oidc,
        );

        assert_eq!(command.action_kind, "iam.session.login");
        assert_eq!(command.request_id.as_deref(), Some("request-41"));
        assert_eq!(command.redacted_message.as_deref(), Some("session login succeeded via oidc"));
        assert_eq!(
            command.internal_message.as_deref(),
            Some(
                "principal 00000000-0000-0000-0000-000000000029 created session 00000000-0000-0000-0000-00000000002a via oidc"
            )
        );
        assert_eq!(command.subjects.len(), 1);
        assert_eq!(command.subjects[0].subject_kind, "session");
        assert_eq!(command.subjects[0].subject_id, session_id);
    }

    #[test]
    fn password_session_login_audit_keeps_its_established_payload() {
        let principal_id = Uuid::from_u128(51);
        let session_id = Uuid::from_u128(52);
        let command = session_login_audit_command(
            Some("request-51".to_string()),
            principal_id,
            session_id,
            SessionLoginMethod::Password,
        );

        assert_eq!(command.redacted_message.as_deref(), Some("session login succeeded"));
        assert_eq!(
            command.internal_message.as_deref(),
            Some(
                "principal 00000000-0000-0000-0000-000000000033 created session 00000000-0000-0000-0000-000000000034"
            )
        );
    }
}
