//! Browser OIDC endpoints. Every rejected callback redirects to the sign-in
//! screen with the validated same-origin return path and one of two verdicts
//! (`ssoError=denied|failed`), and intentionally omits raw provider error
//! values, codes, tokens, subjects, and claims from both responses and logs.

use axum::{
    extract::{Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use percent_encoding::percent_decode_str;
use serde::Deserialize;
use tracing::warn;

use crate::{
    app::state::AppState,
    interfaces::http::{
        router_support::{ApiError, RequestId},
        ui_support::{
            build_cleared_oidc_attempt_cookie, build_oidc_attempt_cookie,
            session_cookie_secure_for_request,
        },
    },
    services::iam::{
        audit::{AppendAuditEventCommand, AppendAuditEventSubjectCommand},
        oidc::{
            OIDC_ATTEMPT_TTL_SECONDS, OIDC_REDIS_OPERATION_TIMEOUT, OidcFailure, OidcLoginAttempt,
            attempt_redis_key, begin_authorization, exchange_code_for_identity,
            external_identity_login, issuer_provider_identifier, new_attempt_cookie_binding,
        },
        service::AuthenticateOidcSessionCommand,
    },
};

use super::session::{
    SessionLoginMethod, record_session_login_audit, session_cookie_response_headers,
};
use super::source_key::{OptionalConnectInfo, extract_login_source_key};

const OIDC_ATTEMPT_COOKIE_NAME: &str = "ironrag_oidc_attempt";
const MAX_RETURN_PATH_BYTES: usize = 2_048;
const MAX_STATE_BYTES: usize = 1_024;
/// The SPA sign-in route a rejected callback lands on. Redirecting to the
/// return path itself would open a protected route first, and the SPA would
/// bounce to sign-in without the verdict.
const LOGIN_PATH: &str = "/login";
const SSO_ERROR_QUERY: &str = "ssoError";
const RETURN_TO_QUERY: &str = "returnTo";

/// The two verdicts the browser is told. `Denied` is an identity the provider
/// vouched for but this deployment does not admit; `Failed` is everything
/// else. Both are opaque on purpose: nothing about the provider, the group,
/// or the person travels in the address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SsoError {
    Denied,
    Failed,
}

impl SsoError {
    fn query_value(self) -> &'static str {
        match self {
            Self::Denied => "denied",
            Self::Failed => "failed",
        }
    }
}

/// Every way a callback is turned away. The variant names the log line; the
/// verdict is derived here so the two never drift apart. No variant carries
/// provider data.
#[derive(Debug, Clone, Copy)]
enum CallbackRejection {
    /// No configured provider, malformed state, no attempt cookie, callback
    /// admission, an unknown, replayed, or foreign-browser attempt, or
    /// unreachable attempt storage.
    State,
    /// The attempt was consumed but the provider sent no authorization code.
    MissingCode,
    /// Code exchange, token verification, userinfo, or claim admission failed.
    Identity(OidcFailure),
    /// The verified identity could not be provisioned or signed in locally.
    Provision,
    /// The session exists but its cookie could not be issued.
    Session,
}

impl CallbackRejection {
    fn stage(self) -> &'static str {
        match self {
            Self::State => "oidc_callback_state",
            Self::MissingCode => "oidc_callback_code",
            Self::Identity(_) => "oidc_callback_identity",
            Self::Provision => "oidc_callback_provision",
            Self::Session => "oidc_callback_session",
        }
    }

    fn sso_error(self) -> SsoError {
        match self {
            Self::Identity(OidcFailure::Claims) => SsoError::Denied,
            _ => SsoError::Failed,
        }
    }
}

#[cfg(test)]
static OIDC_ATTEMPT_STORE_CALLS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);
#[cfg(test)]
static OIDC_ATTEMPT_CONSUME_CALLS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// The `returnTo` path is deliberately a path, not an arbitrary redirect URL.
#[derive(Deserialize, utoipa::IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub(crate) struct OidcLoginQuery {
    pub return_to: Option<String>,
}

/// Provider errors are intentionally not bound into this type. Axum ignores
/// those unknown callback query parameters, keeping them out of our data flow.
#[derive(Deserialize, utoipa::IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub(crate) struct OidcCallbackQuery {
    pub state: Option<String>,
    pub code: Option<String>,
}

#[utoipa::path(
    get,
    path = "/v1/iam/oidc/login",
    tag = "iam",
    operation_id = "startIamOidcLogin",
    security(),
    params(OidcLoginQuery),
    responses(
        (status = 303, description = "A short-lived OIDC attempt cookie is set and the browser is redirected to the configured identity provider."),
        (status = 404, description = "OIDC sign-in is not configured for this deployment"),
        (status = 503, description = "OIDC login admission, provider discovery, or OIDC attempt storage is temporarily unavailable"),
    ),
)]
#[tracing::instrument(level = "info", name = "http.oidc_login", skip_all)]
pub(crate) async fn oidc_login(
    State(state): State<AppState>,
    request_headers: HeaderMap,
    OptionalConnectInfo(peer_addr): OptionalConnectInfo,
    Query(query): Query<OidcLoginQuery>,
) -> Result<Response, ApiError> {
    let oidc_settings = state
        .oidc
        .as_deref()
        .ok_or_else(|| ApiError::NotFound("OIDC sign-in is not configured".to_string()))?;
    let return_path = validate_return_path(query.return_to.as_deref())?;
    // Admission comes before provider discovery. Rejecting here is both
    // fail-closed for a Redis outage and intentionally avoids any outbound
    // IdP work, Redis attempt write, or browser cookie on a flood.
    let source_key = extract_login_source_key(
        &request_headers,
        peer_addr,
        state.settings.login_rate_limit_trusted_proxy_hops,
    );
    let admission_permit = state
        .oidc_login_admission
        .try_admit(&oidc_settings.issuer_url, &source_key)
        .await
        .map_err(|_| {
            ApiError::service_unavailable(
                "OIDC sign-in is temporarily unavailable",
                "oidc_login_admission_rejected",
            )
        })?;
    let authorization = begin_authorization(oidc_settings).await.map_err(|_| {
        // Discovery failures are operational, unlike an invalid callback. The
        // browser is still on our origin here, so surface only a generic status.
        ApiError::service_unavailable("OIDC sign-in is temporarily unavailable", "oidc_unavailable")
    })?;
    let cookie_binding = new_attempt_cookie_binding();
    let attempt = OidcLoginAttempt {
        cookie_binding: cookie_binding.clone(),
        pkce_verifier: authorization.pkce_verifier,
        nonce: authorization.nonce,
        return_path,
    };
    store_attempt(&state.persistence.redis, &authorization.state, &attempt).await.map_err(
        |_| {
            ApiError::service_unavailable(
                "OIDC sign-in is temporarily unavailable",
                "oidc_attempt_storage_unavailable",
            )
        },
    )?;
    // The local permit spans discovery and the Redis attempt write. It does
    // not need to cover pure response construction after durable state exists.
    drop(admission_permit);

    let secure =
        session_cookie_secure_for_request(state.ui_session_cookie.secure, &request_headers);
    let mut response_headers = HeaderMap::new();
    response_headers.insert(
        header::SET_COOKIE,
        build_oidc_attempt_cookie(
            OIDC_ATTEMPT_COOKIE_NAME,
            &cookie_binding,
            OIDC_ATTEMPT_TTL_SECONDS,
            secure,
        )
        .parse()
        .map_err(|error| ApiError::internal_with_log(error, "failed to set OIDC attempt cookie"))?,
    );
    redirect_response(&authorization.authorization_url, response_headers)
}

#[utoipa::path(
    get,
    path = "/v1/iam/oidc/callback",
    tag = "iam",
    operation_id = "completeIamOidcLogin",
    security(),
    params(OidcCallbackQuery),
    responses(
        (status = 303, description = "The OIDC attempt is consumed and the browser is redirected to a validated same-origin return path. A rejected callback redirects to /login?returnTo=<path>&ssoError=denied|failed; provider failures and identity claims are not exposed."),
    ),
)]
#[tracing::instrument(level = "info", name = "http.oidc_callback", skip_all)]
pub(crate) async fn oidc_callback(
    State(state): State<AppState>,
    request_headers: HeaderMap,
    request_id: Option<axum::Extension<RequestId>>,
    Query(query): Query<OidcCallbackQuery>,
) -> Result<Response, ApiError> {
    let secure =
        session_cookie_secure_for_request(state.ui_session_cookie.secure, &request_headers);
    // Until an attempt is consumed, no return path is known and the cookie
    // must stay: clearing it for unrelated cross-site callbacks would let an
    // attacker interrupt an in-flight flow without knowing its state or
    // cookie binding. Redis unavailability is fail-closed the same way. Unlike
    // the start endpoint, callback failures are always redirected so raw IdP
    // data cannot become a browser-visible error response.
    let rejected_before_consumption = || callback_rejected(CallbackRejection::State, "/", None);
    let Some(oidc_settings) = state.oidc.as_deref() else {
        return rejected_before_consumption();
    };
    let Some(state_parameter) = query.state.as_deref().filter(is_valid_callback_state) else {
        return rejected_before_consumption();
    };
    let Some(cookie_binding) = read_cookie(&request_headers, OIDC_ATTEMPT_COOKIE_NAME) else {
        return rejected_before_consumption();
    };
    // A state was admitted at login start, but a coordinated wave of valid
    // callbacks can still otherwise fan out into token/userinfo work. This is
    // intentionally no-queue and does not debit the start bucket again.
    let Ok(callback_admission_permit) = state.oidc_login_admission.try_admit_callback() else {
        return rejected_before_consumption();
    };
    let Ok(Some(attempt)) =
        consume_attempt(&state.persistence.redis, state_parameter, &cookie_binding).await
    else {
        return rejected_before_consumption();
    };
    // Only a callback that atomically proved both state and browser cookie
    // ownership may clear the attempt cookie.
    let clear_attempt_cookie = cleared_attempt_cookie_header(secure)?;
    let return_path =
        validate_return_path(Some(&attempt.return_path)).unwrap_or_else(|_| "/".to_string());
    let rejected = |rejection: CallbackRejection| {
        callback_rejected(rejection, &return_path, Some(clear_attempt_cookie.clone()))
    };
    let Some(code) = query.code.as_deref() else {
        return rejected(CallbackRejection::MissingCode);
    };
    let identity = match exchange_code_for_identity(
        oidc_settings,
        code,
        &attempt.pkce_verifier,
        &attempt.nonce,
    )
    .await
    {
        Ok(identity) => identity,
        Err(failure) => return rejected(CallbackRejection::Identity(failure)),
    };
    let provider_identifier = issuer_provider_identifier(&oidc_settings.issuer_url);
    let login = external_identity_login(&provider_identifier, &identity.subject);
    let bootstrap_admin = oidc_settings
        .bootstrap_admin_subject
        .as_deref()
        .is_some_and(|subject| subject == identity.subject);
    let oidc_session = match state
        .application_services
        .iam
        .authenticate_oidc_session(
            &state,
            AuthenticateOidcSessionCommand {
                auth_provider_kind: provider_identifier,
                external_subject: identity.subject,
                login,
                email: identity.email,
                display_name: identity.display_name,
                bootstrap_admin,
            },
        )
        .await
    {
        Ok(session) => session,
        Err(_) => return rejected(CallbackRejection::Provision),
    };

    let audit_request_id = request_id.map(|value| value.0.0);
    if oidc_session.identity_created {
        record_oidc_provisioning_audit(
            &state,
            audit_request_id.clone(),
            oidc_session.session.principal_id,
        );
    }
    let session = oidc_session.session;
    let mut response_headers = match session_cookie_response_headers(
        &state,
        &request_headers,
        session.session_id,
        &session.session_secret,
    ) {
        Ok(headers) => headers,
        Err(_) => return rejected(CallbackRejection::Session),
    };
    record_session_login_audit(
        &state,
        audit_request_id,
        session.principal_id,
        session.session_id,
        SessionLoginMethod::Oidc,
    );
    response_headers.append(header::SET_COOKIE, clear_attempt_cookie);
    drop(callback_admission_permit);
    redirect_response(&return_path, response_headers)
}

fn record_oidc_provisioning_audit(
    state: &AppState,
    request_id: Option<String>,
    principal_id: uuid::Uuid,
) {
    let audit_state = state.clone();
    tokio::spawn(async move {
        if let Err(error) = audit_state
            .application_services
            .audit
            .append_event(&audit_state, oidc_provisioning_audit_command(request_id, principal_id))
            .await
        {
            warn!(
                principal_id = %principal_id,
                ?error,
                "failed to append OIDC provisioning audit event",
            );
        }
    });
}

fn oidc_provisioning_audit_command(
    request_id: Option<String>,
    principal_id: uuid::Uuid,
) -> AppendAuditEventCommand {
    AppendAuditEventCommand {
        actor_principal_id: Some(principal_id),
        surface_kind: "rest".to_string(),
        action_kind: "iam.oidc.provision".to_string(),
        request_id,
        trace_id: None,
        result_kind: "succeeded".to_string(),
        redacted_message: Some("OIDC identity provisioned".to_string()),
        internal_message: Some(format!("principal {principal_id} provisioned via oidc")),
        subjects: vec![AppendAuditEventSubjectCommand {
            subject_kind: "principal".to_string(),
            subject_id: principal_id,
            workspace_id: None,
            library_id: None,
            document_id: None,
        }],
    }
}

async fn store_attempt(
    redis: &redis::Client,
    state: &str,
    attempt: &OidcLoginAttempt,
) -> Result<(), ()> {
    #[cfg(test)]
    OIDC_ATTEMPT_STORE_CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let payload = serde_json::to_string(attempt).map_err(|_| ())?;
    let mut connection = tokio::time::timeout(
        OIDC_REDIS_OPERATION_TIMEOUT,
        redis.get_multiplexed_async_connection(),
    )
    .await
    .map_err(|_| ())?
    .map_err(|_| ())?;
    let stored: Option<String> = tokio::time::timeout(
        OIDC_REDIS_OPERATION_TIMEOUT,
        redis::cmd("SET")
            .arg(attempt_redis_key(state))
            .arg(payload)
            .arg("NX")
            .arg("EX")
            .arg(OIDC_ATTEMPT_TTL_SECONDS)
            .query_async(&mut connection),
    )
    .await
    .map_err(|_| ())?
    .map_err(|_| ())?;
    (stored.as_deref() == Some("OK")).then_some(()).ok_or(())
}

/// Atomically verifies cookie binding and consumes the one-use attempt. A
/// mismatched/no cookie returns nil *without deleting* the attempt, so a
/// third party that sees URL state cannot turn it into a login CSRF or DoS.
async fn consume_attempt(
    redis: &redis::Client,
    state: &str,
    cookie_binding: &str,
) -> Result<Option<OidcLoginAttempt>, ()> {
    #[cfg(test)]
    OIDC_ATTEMPT_CONSUME_CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    const CONSUME_IF_BOUND: &str = r#"
local raw = redis.call('GET', KEYS[1])
if not raw then
  return nil
end
local attempt = cjson.decode(raw)
if attempt.cookie_binding ~= ARGV[1] then
  return nil
end
redis.call('DEL', KEYS[1])
return raw
"#;
    let mut connection = tokio::time::timeout(
        OIDC_REDIS_OPERATION_TIMEOUT,
        redis.get_multiplexed_async_connection(),
    )
    .await
    .map_err(|_| ())?
    .map_err(|_| ())?;
    let payload: Option<String> = tokio::time::timeout(
        OIDC_REDIS_OPERATION_TIMEOUT,
        redis::cmd("EVAL")
            .arg(CONSUME_IF_BOUND)
            .arg(1)
            .arg(attempt_redis_key(state))
            .arg(cookie_binding)
            .query_async(&mut connection),
    )
    .await
    .map_err(|_| ())?
    .map_err(|_| ())?;
    payload.map(|value| serde_json::from_str(&value).map_err(|_| ())).transpose()
}

fn validate_return_path(value: Option<&str>) -> Result<String, ApiError> {
    let value = value.unwrap_or("/");
    if value.is_empty()
        || value.len() > MAX_RETURN_PATH_BYTES
        || !value.starts_with('/')
        || value.starts_with("//")
        || value.contains('\\')
        || value.chars().any(char::is_control)
    {
        return Err(ApiError::BadRequest("returnTo must be a same-origin path".to_string()));
    }
    let decoded = percent_decode_str(value)
        .decode_utf8()
        .map_err(|_| ApiError::BadRequest("returnTo must be a same-origin path".to_string()))?;
    if decoded.starts_with("//") || decoded.contains('\\') || decoded.chars().any(char::is_control)
    {
        return Err(ApiError::BadRequest("returnTo must be a same-origin path".to_string()));
    }
    Ok(value.to_string())
}

fn is_valid_callback_state(value: &&str) -> bool {
    !value.is_empty() && value.len() <= MAX_STATE_BYTES && !value.chars().any(char::is_control)
}

fn read_cookie(headers: &HeaderMap, expected_name: &str) -> Option<String> {
    headers.get_all(header::COOKIE).iter().find_map(|header_value| {
        let raw = header_value.to_str().ok()?;
        raw.split(';').find_map(|item| {
            let mut pieces = item.trim().splitn(2, '=');
            match (pieces.next(), pieces.next()) {
                (Some(name), Some(value)) if name == expected_name && !value.is_empty() => {
                    Some(value.to_string())
                }
                _ => None,
            }
        })
    })
}

fn cleared_attempt_cookie_header(secure: bool) -> Result<HeaderValue, ApiError> {
    build_cleared_oidc_attempt_cookie(OIDC_ATTEMPT_COOKIE_NAME, secure)
        .parse()
        .map_err(|error| ApiError::internal_with_log(error, "failed to clear OIDC attempt cookie"))
}

/// The one exit for a rejected callback: a stage-only log line and a
/// redirect to the sign-in screen carrying the return path and the verdict.
fn callback_rejected(
    rejection: CallbackRejection,
    return_path: &str,
    cleared_attempt_cookie: Option<HeaderValue>,
) -> Result<Response, ApiError> {
    let sso_error = rejection.sso_error();
    warn!(
        stage = rejection.stage(),
        failure = ?rejection,
        sso_error = sso_error.query_value(),
        "OIDC callback rejected"
    );
    let mut response_headers = HeaderMap::new();
    if let Some(cleared_attempt_cookie) = cleared_attempt_cookie {
        response_headers.insert(header::SET_COOKIE, cleared_attempt_cookie);
    }
    redirect_response(&login_redirect_location(return_path, sso_error), response_headers)
}

/// `/login?returnTo=<path>&ssoError=<verdict>`. The return path is the same
/// validated value a successful callback would have landed on; the SPA reads
/// it back for a retry.
fn login_redirect_location(return_path: &str, sso_error: SsoError) -> String {
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair(RETURN_TO_QUERY, return_path)
        .append_pair(SSO_ERROR_QUERY, sso_error.query_value())
        .finish();
    format!("{LOGIN_PATH}?{query}")
}

fn redirect_response(
    location: &str,
    mut response_headers: HeaderMap,
) -> Result<Response, ApiError> {
    response_headers.insert(
        header::LOCATION,
        HeaderValue::from_str(location).map_err(|error| {
            ApiError::internal_with_log(error, "invalid OIDC redirect location")
        })?,
    );
    Ok((StatusCode::SEE_OTHER, response_headers).into_response())
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use axum::{Json, Router, routing::get};
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use tokio::{net::TcpListener, task::JoinHandle};

    use crate::{
        app::config::{OidcSettings, Settings},
        infra::persistence::Persistence,
        services::iam::oidc::OidcLoginAdmission,
    };

    use super::*;

    fn oidc_attempt_store_call_count() -> usize {
        OIDC_ATTEMPT_STORE_CALLS.load(std::sync::atomic::Ordering::Relaxed)
    }

    fn oidc_attempt_consume_call_count() -> usize {
        OIDC_ATTEMPT_CONSUME_CALLS.load(std::sync::atomic::Ordering::Relaxed)
    }

    #[derive(Clone)]
    struct CountingDiscoveryState {
        issuer_url: String,
        requests: Arc<AtomicUsize>,
    }

    struct CountingDiscoveryProvider {
        issuer_url: String,
        requests: Arc<AtomicUsize>,
        task: JoinHandle<()>,
    }

    impl Drop for CountingDiscoveryProvider {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    async fn start_counting_discovery_provider() -> CountingDiscoveryProvider {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("test-only OIDC discovery listener should bind");
        let issuer_url = format!(
            "http://{}",
            listener
                .local_addr()
                .expect("test-only OIDC discovery listener should expose its address")
        );
        let requests = Arc::new(AtomicUsize::new(0));
        let state = CountingDiscoveryState {
            issuer_url: issuer_url.clone(),
            requests: Arc::clone(&requests),
        };
        let app = Router::new()
            .route("/.well-known/openid-configuration", get(counting_discovery))
            .with_state(state);
        let task = tokio::spawn(async move {
            let _result = axum::serve(listener, app).await;
        });

        CountingDiscoveryProvider { issuer_url, requests, task }
    }

    async fn counting_discovery(
        State(state): State<CountingDiscoveryState>,
    ) -> Json<serde_json::Value> {
        state.requests.fetch_add(1, Ordering::Relaxed);
        let issuer_url = state.issuer_url;
        let authorization_endpoint = format!("{issuer_url}/authorize");
        let token_endpoint = format!("{issuer_url}/token");
        let userinfo_endpoint = format!("{issuer_url}/userinfo");
        let jwks_uri = format!("{issuer_url}/jwks");
        Json(serde_json::json!({
            "issuer": issuer_url,
            "authorization_endpoint": authorization_endpoint,
            "token_endpoint": token_endpoint,
            "userinfo_endpoint": userinfo_endpoint,
            "jwks_uri": jwks_uri,
            "response_types_supported": ["code"],
            "subject_types_supported": ["public"],
            "id_token_signing_alg_values_supported": ["RS256"],
            "token_endpoint_auth_methods_supported": ["client_secret_basic"],
        }))
    }

    struct CountingRedisProbe {
        url: String,
        connections: Arc<AtomicUsize>,
        task: JoinHandle<()>,
    }

    impl Drop for CountingRedisProbe {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    async fn start_counting_redis_probe() -> CountingRedisProbe {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("test-only Redis probe listener should bind");
        let url = format!(
            "redis://{}/",
            listener
                .local_addr()
                .expect("test-only Redis probe listener should expose its address")
        );
        let connections = Arc::new(AtomicUsize::new(0));
        let probe_connections = Arc::clone(&connections);
        let task = tokio::spawn(async move {
            while let Ok((stream, _address)) = listener.accept().await {
                probe_connections.fetch_add(1, Ordering::Relaxed);
                drop(stream);
            }
        });

        CountingRedisProbe { url, connections, task }
    }

    fn oidc_login_test_state(redis_url: &str, oidc_settings: OidcSettings) -> AppState {
        let mut settings = Settings::from_env().expect("default settings should load");
        settings.discard_credential_master_key();
        settings.credential_master_key = Some(STANDARD.encode([71_u8; 32]));
        settings.credential_master_key_id = None;
        settings.credential_previous_master_keys = None;
        settings.service_role = "api".to_string();
        settings.ui_bootstrap_admin_login = None;
        settings.ui_bootstrap_admin_email = None;
        settings.ui_bootstrap_admin_name = None;
        settings.ui_bootstrap_admin_password = None;
        settings.ui_bootstrap_admin_api_token = None;
        settings.frontend_origin = "https://graph.example.test".to_string();
        settings.oidc_issuer_url = None;
        settings.oidc_client_id = None;
        settings.oidc_client_secret = None;
        settings.oidc_redirect_url = None;
        settings.oidc_required_group = None;
        settings.oidc_bootstrap_admin_subject = None;

        let postgres = sqlx::PgPool::connect_lazy("postgres://127.0.0.1/ironrag")
            .expect("test-only lazy Postgres pool should parse");
        let redis = redis::Client::open(redis_url).expect("test-only Redis probe URL should parse");
        let mut state =
            AppState::from_dependencies(settings, Persistence::for_tests(postgres, redis))
                .expect("OIDC test state should prepare without connecting dependencies");
        state.oidc = Some(Arc::new(oidc_settings));
        state.oidc_login_admission =
            OidcLoginAdmission::with_local_capacity_for_tests(state.persistence.redis.clone(), 1);
        state
    }

    fn counting_provider_settings(provider: &CountingDiscoveryProvider) -> OidcSettings {
        OidcSettings {
            issuer_url: provider.issuer_url.clone(),
            client_id: "oidc-admission-test-client".to_string(),
            client_secret: "oidc-admission-test-secret".to_string(),
            redirect_url: "https://graph.example.test/v1/iam/oidc/callback".to_string(),
            required_group: "ironrag-users".to_string(),
            bootstrap_admin_subject: None,
        }
    }

    #[test]
    fn return_path_rejects_cross_origin_and_encoded_authority_forms() {
        for value in [
            "https://attacker.example/",
            "//attacker.example/",
            "/%2f%2fattacker.example/",
            "/\\attacker.example/",
            "/\r\nLocation: https://attacker.example/",
        ] {
            assert!(validate_return_path(Some(value)).is_err(), "accepted {value:?}");
        }
        assert_eq!(
            validate_return_path(Some("/libraries?tab=all")).ok().as_deref(),
            Some("/libraries?tab=all")
        );
    }

    #[test]
    fn rejected_callback_lands_on_sign_in_with_return_path_and_verdict() {
        assert_eq!(
            login_redirect_location("/libraries?tab=all", SsoError::Denied),
            "/login?returnTo=%2Flibraries%3Ftab%3Dall&ssoError=denied"
        );
        assert_eq!(
            login_redirect_location("/", SsoError::Failed),
            "/login?returnTo=%2F&ssoError=failed"
        );
    }

    #[test]
    fn only_a_claims_rejection_is_a_denial() {
        // Claims is the one stage where the provider vouched for the person
        // and this deployment said no; every other rejection is opaque.
        assert_eq!(CallbackRejection::Identity(OidcFailure::Claims).sso_error(), SsoError::Denied);
        for failure in [
            OidcFailure::Discovery,
            OidcFailure::TokenExchange,
            OidcFailure::IdTokenVerification,
            OidcFailure::UserInfo,
        ] {
            assert_eq!(
                CallbackRejection::Identity(failure).sso_error(),
                SsoError::Failed,
                "{failure:?}"
            );
        }
        for rejection in [
            CallbackRejection::State,
            CallbackRejection::MissingCode,
            CallbackRejection::Provision,
            CallbackRejection::Session,
        ] {
            assert_eq!(rejection.sso_error(), SsoError::Failed, "{rejection:?}");
        }
    }

    #[test]
    fn rejected_callback_clears_the_attempt_cookie_only_after_consumption() {
        let cleared = cleared_attempt_cookie_header(true).expect("test cookie should parse");
        let after = callback_rejected(
            CallbackRejection::Identity(OidcFailure::Claims),
            "/graph?layout=force",
            Some(cleared),
        )
        .expect("rejection should redirect");
        assert_eq!(after.status(), StatusCode::SEE_OTHER);
        assert_eq!(
            after.headers().get(header::LOCATION).and_then(|value| value.to_str().ok()),
            Some("/login?returnTo=%2Fgraph%3Flayout%3Dforce&ssoError=denied")
        );
        assert!(after.headers().get(header::SET_COOKIE).is_some());

        let before = callback_rejected(CallbackRejection::State, "/", None)
            .expect("rejection should redirect");
        assert_eq!(
            before.headers().get(header::LOCATION).and_then(|value| value.to_str().ok()),
            Some("/login?returnTo=%2F&ssoError=failed")
        );
        assert!(before.headers().get(header::SET_COOKIE).is_none());
    }

    #[test]
    fn callback_state_requires_bounded_non_control_value() {
        assert!(is_valid_callback_state(&"state"));
        assert!(!is_valid_callback_state(&""));
        assert!(!is_valid_callback_state(&"bad\nstate"));
    }

    #[test]
    fn cookie_reader_selects_the_named_attempt_cookie() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            "unrelated=x; ironrag_oidc_attempt=attempt-binding; session=y".parse().unwrap(),
        );

        assert_eq!(
            read_cookie(&headers, OIDC_ATTEMPT_COOKIE_NAME).as_deref(),
            Some("attempt-binding")
        );
    }

    #[test]
    fn provisioning_audit_does_not_include_identity_claims_or_credentials() {
        let principal_id = uuid::Uuid::from_u128(77);
        let command = oidc_provisioning_audit_command(Some("request-77".to_string()), principal_id);

        assert_eq!(command.action_kind, "iam.oidc.provision");
        assert_eq!(command.request_id.as_deref(), Some("request-77"));
        assert_eq!(command.redacted_message.as_deref(), Some("OIDC identity provisioned"));
        assert_eq!(
            command.internal_message.as_deref(),
            Some("principal 00000000-0000-0000-0000-00000000004d provisioned via oidc")
        );
        assert_eq!(command.subjects.len(), 1);
        assert_eq!(command.subjects[0].subject_kind, "principal");
        assert_eq!(command.subjects[0].subject_id, principal_id);
    }

    #[tokio::test]
    async fn saturated_oidc_login_rejects_before_discovery_or_attempt_storage() {
        let provider = start_counting_discovery_provider().await;
        let redis_probe = start_counting_redis_probe().await;
        let state = oidc_login_test_state(&redis_probe.url, counting_provider_settings(&provider));
        let stores_before = oidc_attempt_store_call_count();
        let _held_permit = state
            .oidc_login_admission
            .try_acquire_local_for_tests()
            .expect("test capacity should allow the saturation permit");

        let error = oidc_login(
            State(state),
            HeaderMap::new(),
            OptionalConnectInfo(None),
            Query(OidcLoginQuery { return_to: Some("/libraries".to_string()) }),
        )
        .await
        .expect_err("a saturated login guard must reject before discovery");

        assert_eq!(error.kind(), "oidc_login_admission_rejected");
        let response = axum::response::IntoResponse::into_response(error);
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert!(response.headers().get(header::SET_COOKIE).is_none());
        assert_eq!(provider.requests.load(Ordering::Relaxed), 0);
        assert_eq!(redis_probe.connections.load(Ordering::Relaxed), 0);
        assert_eq!(oidc_attempt_store_call_count(), stores_before);
    }

    #[tokio::test]
    async fn saturated_oidc_callback_rejects_before_state_or_identity_exchange() {
        let provider = start_counting_discovery_provider().await;
        let redis_probe = start_counting_redis_probe().await;
        let state = oidc_login_test_state(&redis_probe.url, counting_provider_settings(&provider));
        let consumes_before = oidc_attempt_consume_call_count();
        let _held_permit = state
            .oidc_login_admission
            .try_acquire_callback_for_tests()
            .expect("test capacity should allow the saturation permit");
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            "ironrag_oidc_attempt=browser-bound-attempt".parse().expect("test cookie should parse"),
        );

        let response = oidc_callback(
            State(state),
            headers,
            None,
            Query(OidcCallbackQuery {
                state: Some("opaque-state".to_string()),
                code: Some("authorization-code".to_string()),
            }),
        )
        .await
        .expect("a saturated callback should safely redirect");

        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_eq!(
            response.headers().get(header::LOCATION).and_then(|value| value.to_str().ok()),
            Some("/login?returnTo=%2F&ssoError=failed")
        );
        assert!(response.headers().get(header::SET_COOKIE).is_none());
        assert_eq!(provider.requests.load(Ordering::Relaxed), 0);
        assert_eq!(redis_probe.connections.load(Ordering::Relaxed), 0);
        assert_eq!(oidc_attempt_consume_call_count(), consumes_before);
    }
}
