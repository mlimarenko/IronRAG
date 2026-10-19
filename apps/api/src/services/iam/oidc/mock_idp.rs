//! In-process OIDC identity provider for tests.
//!
//! It speaks discovery, JWKS, token and userinfo over plain HTTP on a
//! loopback port, which [`super::validate_discovered_endpoint`] admits only
//! under `cfg(test)` or the `test-support` feature. Both the protocol unit
//! tests and the HTTP integration suite drive it, so a scenario exists once.

use std::sync::{
    Arc, Mutex, OnceLock,
    atomic::{AtomicUsize, Ordering},
};

use axum::{
    Json, Router,
    extract::{Form, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use chrono::{Duration as ChronoDuration, Utc};
use openidconnect::{
    AccessToken, Audience, IssuerUrl, JsonWebKeyId, Nonce, PrivateSigningKey, StandardClaims,
    SubjectIdentifier,
    core::{CoreIdToken, CoreIdTokenClaims, CoreJwsSigningAlgorithm, CoreRsaPrivateSigningKey},
};
use rsa::{
    RsaPrivateKey,
    pkcs1::{EncodeRsaPrivateKey, LineEnding},
    rand_core::OsRng,
};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use tokio::{net::TcpListener, sync::oneshot, task::JoinHandle};

use crate::app::config::OidcSettings;

pub const MOCK_CLIENT_ID: &str = "mock-ironrag-client";
pub const MOCK_CLIENT_SECRET: &str = "mock-client-secret";
const MOCK_BASIC_CREDENTIALS: &str = "mock-ironrag-client:mock-client-secret";
const MOCK_ACCESS_TOKEN: &str = "mock-access-token";
pub const MOCK_AUTHORIZATION_CODE: &str = "approved-authorization-code";
pub const MOCK_SUBJECT: &str = "oidc-subject-42";
pub const MOCK_EMAIL: &str = "sso.user@example.test";
pub const MOCK_REQUIRED_GROUP: &str = "ironrag-users";

#[derive(Clone, Copy, Debug, Default)]
pub struct MockScenario {
    pub discovery: DiscoveryScenario,
    pub id_token: IdTokenScenario,
    pub userinfo: UserInfoScenario,
}

#[derive(Clone, Copy, Debug, Default)]
pub enum DiscoveryScenario {
    #[default]
    Valid,
    Redirect,
}

#[derive(Clone, Copy, Debug, Default)]
pub enum IdTokenScenario {
    #[default]
    Valid,
    InvalidIssuer,
    InvalidSignature,
    InvalidAudience,
    Expired,
    InvalidNonce,
}

#[derive(Clone, Copy, Debug, Default)]
pub enum UserInfoScenario {
    #[default]
    Valid,
    SubjectMismatch,
    UnverifiedEmail,
    BlankEmail,
    MissingGroup,
    WrongGroup,
    MalformedGroup,
    ScalarGroup,
}

#[derive(Clone, Debug, Default)]
struct AttemptExpectation {
    code: String,
    pkce_verifier: String,
    nonce: String,
}

#[derive(Clone, Debug, Default)]
pub struct CapturedTokenRequest {
    pub code: String,
    pub pkce_verifier: String,
    pub client_authenticated: bool,
}

/// How many times each provider endpoint was called.
#[derive(Default)]
pub struct RequestMetrics {
    discovery: AtomicUsize,
    jwks: AtomicUsize,
    token: AtomicUsize,
    userinfo: AtomicUsize,
    redirected_discovery_target: AtomicUsize,
}

impl RequestMetrics {
    pub fn discovery(&self) -> usize {
        self.discovery.load(Ordering::Relaxed)
    }

    pub fn jwks(&self) -> usize {
        self.jwks.load(Ordering::Relaxed)
    }

    pub fn token(&self) -> usize {
        self.token.load(Ordering::Relaxed)
    }

    pub fn userinfo(&self) -> usize {
        self.userinfo.load(Ordering::Relaxed)
    }

    pub fn redirected_discovery_target(&self) -> usize {
        self.redirected_discovery_target.load(Ordering::Relaxed)
    }
}

struct MockOidcState {
    origin: String,
    scenario: MockScenario,
    signing_key: Arc<CoreRsaPrivateSigningKey>,
    verification_jwk: Value,
    expected_attempt: Mutex<AttemptExpectation>,
    captured_token_request: Mutex<CapturedTokenRequest>,
    metrics: RequestMetrics,
}

impl MockOidcState {
    fn new(
        origin: String,
        scenario: MockScenario,
        signing_key: Arc<CoreRsaPrivateSigningKey>,
    ) -> anyhow::Result<Self> {
        let verification_jwk = serde_json::to_value(signing_key.as_verification_key())?;

        Ok(Self {
            origin,
            scenario,
            signing_key,
            verification_jwk,
            expected_attempt: Mutex::default(),
            captured_token_request: Mutex::default(),
            metrics: RequestMetrics::default(),
        })
    }
}

pub struct MockOidcProvider {
    origin: String,
    state: Arc<MockOidcState>,
    shutdown: Option<oneshot::Sender<()>>,
    _task: JoinHandle<()>,
}

impl MockOidcProvider {
    pub async fn start(scenario: MockScenario) -> anyhow::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let origin = format!("http://{}", listener.local_addr()?);
        let signing_key = mock_signing_key()?;
        let state = Arc::new(MockOidcState::new(origin.clone(), scenario, signing_key)?);
        let app = Router::new()
            .route("/.well-known/openid-configuration", get(discovery))
            .route("/redirected-discovery", get(redirected_discovery_target))
            .route("/jwks", get(jwks))
            .route("/token", post(token))
            .route("/userinfo", get(userinfo))
            .with_state(Arc::clone(&state));
        let (shutdown, shutdown_receiver) = oneshot::channel();
        let task = tokio::spawn(async move {
            let _result = axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _received_shutdown = shutdown_receiver.await;
                })
                .await;
        });

        Ok(Self { origin, state, shutdown: Some(shutdown), _task: task })
    }

    /// The issuer URL this provider answers on.
    pub fn origin(&self) -> &str {
        &self.origin
    }

    pub fn settings(&self) -> OidcSettings {
        OidcSettings {
            issuer_url: self.origin.clone(),
            client_id: MOCK_CLIENT_ID.to_string(),
            client_secret: MOCK_CLIENT_SECRET.to_string(),
            redirect_url: "http://frontend.example.test/v1/iam/oidc/callback".to_string(),
            required_group: MOCK_REQUIRED_GROUP.to_string(),
            bootstrap_admin_subject: None,
        }
    }

    /// Accept the token exchange for [`MOCK_AUTHORIZATION_CODE`] against the
    /// PKCE verifier and nonce the relying party stored for its attempt.
    pub fn expect_exchange(&self, pkce_verifier: &str, nonce: &str) -> anyhow::Result<()> {
        let mut expected_attempt = self
            .state
            .expected_attempt
            .lock()
            .map_err(|_| anyhow::anyhow!("mock OIDC attempt lock poisoned"))?;
        *expected_attempt = AttemptExpectation {
            code: MOCK_AUTHORIZATION_CODE.to_string(),
            pkce_verifier: pkce_verifier.to_string(),
            nonce: nonce.to_string(),
        };
        Ok(())
    }

    pub fn captured_token_request(&self) -> anyhow::Result<CapturedTokenRequest> {
        self.state
            .captured_token_request
            .lock()
            .map_err(|_| anyhow::anyhow!("mock OIDC capture lock poisoned"))
            .map(|captured| captured.clone())
    }

    pub fn metrics(&self) -> &RequestMetrics {
        &self.state.metrics
    }
}

impl Drop for MockOidcProvider {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _send_result = shutdown.send(());
        }
    }
}

#[derive(Deserialize)]
struct TokenForm {
    code: String,
    code_verifier: String,
    grant_type: String,
}

async fn discovery(State(state): State<Arc<MockOidcState>>) -> Response {
    state.metrics.discovery.fetch_add(1, Ordering::Relaxed);
    if matches!(state.scenario.discovery, DiscoveryScenario::Redirect) {
        return (StatusCode::TEMPORARY_REDIRECT, [(header::LOCATION, "/redirected-discovery")])
            .into_response();
    }

    Json(json!({
        "issuer": state.origin,
        "authorization_endpoint": format!("{}/authorize", state.origin),
        "token_endpoint": format!("{}/token", state.origin),
        "userinfo_endpoint": format!("{}/userinfo", state.origin),
        "jwks_uri": format!("{}/jwks", state.origin),
        "response_types_supported": ["code"],
        "subject_types_supported": ["public"],
        "id_token_signing_alg_values_supported": ["RS256"],
        "token_endpoint_auth_methods_supported": ["client_secret_basic"]
    }))
    .into_response()
}

async fn redirected_discovery_target(State(state): State<Arc<MockOidcState>>) -> Response {
    state.metrics.redirected_discovery_target.fetch_add(1, Ordering::Relaxed);
    StatusCode::NO_CONTENT.into_response()
}

async fn jwks(State(state): State<Arc<MockOidcState>>) -> Json<Value> {
    state.metrics.jwks.fetch_add(1, Ordering::Relaxed);
    Json(json!({ "keys": [state.verification_jwk] }))
}

async fn token(
    State(state): State<Arc<MockOidcState>>,
    headers: HeaderMap,
    Form(form): Form<TokenForm>,
) -> Response {
    state.metrics.token.fetch_add(1, Ordering::Relaxed);
    let client_authenticated = has_expected_client_credentials(&headers);
    let expected_attempt = match state.expected_attempt.lock() {
        Ok(expected_attempt) => expected_attempt.clone(),
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let mut captured = match state.captured_token_request.lock() {
        Ok(captured) => captured,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    *captured = CapturedTokenRequest {
        code: form.code.clone(),
        pkce_verifier: form.code_verifier.clone(),
        client_authenticated,
    };
    drop(captured);

    if form.grant_type != "authorization_code"
        || form.code != expected_attempt.code
        || form.code_verifier != expected_attempt.pkce_verifier
        || !client_authenticated
    {
        return (StatusCode::BAD_REQUEST, Json(json!({ "error": "invalid_request" })))
            .into_response();
    }

    let id_token = match make_id_token(&state, &expected_attempt) {
        Ok(id_token) => id_token,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    Json(json!({
        "access_token": MOCK_ACCESS_TOKEN,
        "token_type": "Bearer",
        "expires_in": 300,
        "id_token": id_token,
    }))
    .into_response()
}

async fn userinfo(State(state): State<Arc<MockOidcState>>, headers: HeaderMap) -> Response {
    state.metrics.userinfo.fetch_add(1, Ordering::Relaxed);
    let has_expected_access_token = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value == format!("Bearer {MOCK_ACCESS_TOKEN}"));
    if !has_expected_access_token {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    Json(userinfo_claims(state.scenario.userinfo)).into_response()
}

fn make_signing_key() -> anyhow::Result<CoreRsaPrivateSigningKey> {
    let mut rng = OsRng;
    let private_key = RsaPrivateKey::new(&mut rng, 2_048)?;
    let private_key_pem = private_key.to_pkcs1_pem(LineEnding::LF)?;
    CoreRsaPrivateSigningKey::from_pem(
        private_key_pem.as_str(),
        Some(JsonWebKeyId::new("mock-oidc-rs256-key".to_string())),
    )
    .map_err(anyhow::Error::msg)
}

fn mock_signing_key() -> anyhow::Result<Arc<CoreRsaPrivateSigningKey>> {
    static MOCK_SIGNING_KEY: OnceLock<Result<Arc<CoreRsaPrivateSigningKey>, String>> =
        OnceLock::new();

    MOCK_SIGNING_KEY
        .get_or_init(|| make_signing_key().map(Arc::new).map_err(|error| error.to_string()))
        .as_ref()
        .map(Arc::clone)
        .map_err(|error| anyhow::anyhow!("mock OIDC signing key setup failed: {error}"))
}

fn make_id_token(
    state: &MockOidcState,
    expected_attempt: &AttemptExpectation,
) -> anyhow::Result<String> {
    let now = Utc::now();
    let issuer = match state.scenario.id_token {
        IdTokenScenario::InvalidIssuer => format!("{}/untrusted", state.origin),
        _ => state.origin.clone(),
    };
    let audience = match state.scenario.id_token {
        IdTokenScenario::InvalidAudience => "another-client".to_string(),
        _ => MOCK_CLIENT_ID.to_string(),
    };
    let expires_at = match state.scenario.id_token {
        IdTokenScenario::Expired => now - ChronoDuration::minutes(5),
        _ => now + ChronoDuration::minutes(5),
    };
    let nonce = match state.scenario.id_token {
        IdTokenScenario::InvalidNonce => "unexpected-nonce".to_string(),
        _ => expected_attempt.nonce.clone(),
    };
    let claims = CoreIdTokenClaims::new(
        IssuerUrl::new(issuer)?,
        vec![Audience::new(audience)],
        expires_at,
        now,
        StandardClaims::new(SubjectIdentifier::new(MOCK_SUBJECT.to_string())),
        Default::default(),
    )
    .set_nonce(Some(Nonce::new(nonce)));
    let id_token = CoreIdToken::new(
        claims,
        state.signing_key.as_ref(),
        CoreJwsSigningAlgorithm::RsaSsaPkcs1V15Sha256,
        Some(&AccessToken::new(MOCK_ACCESS_TOKEN.to_string())),
        None,
    )?
    .to_string();

    if matches!(state.scenario.id_token, IdTokenScenario::InvalidSignature) {
        corrupt_signature(&id_token)
    } else {
        Ok(id_token)
    }
}

fn corrupt_signature(token: &str) -> anyhow::Result<String> {
    let (unsigned, signature) = token
        .rsplit_once('.')
        .ok_or_else(|| anyhow::anyhow!("mock ID token has no signature segment"))?;
    let (first, rest) = signature
        .split_at_checked(1)
        .ok_or_else(|| anyhow::anyhow!("mock ID token has an empty signature segment"))?;
    let replacement = if first == "A" { "B" } else { "A" };

    Ok(format!("{unsigned}.{replacement}{rest}"))
}

fn has_expected_client_credentials(headers: &HeaderMap) -> bool {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Basic "))
        .and_then(|value| STANDARD.decode(value).ok())
        .and_then(|value| String::from_utf8(value).ok())
        .is_some_and(|value| value == MOCK_BASIC_CREDENTIALS)
}

fn userinfo_claims(scenario: UserInfoScenario) -> Value {
    let subject = match scenario {
        UserInfoScenario::SubjectMismatch => "different-subject",
        _ => MOCK_SUBJECT,
    };
    let email = match scenario {
        UserInfoScenario::BlankEmail => "",
        _ => MOCK_EMAIL,
    };
    let email_verified = !matches!(scenario, UserInfoScenario::UnverifiedEmail);
    let mut claims = Map::from_iter([
        ("sub".to_string(), Value::String(subject.to_string())),
        ("email".to_string(), Value::String(email.to_string())),
        ("email_verified".to_string(), Value::Bool(email_verified)),
    ]);
    match scenario {
        UserInfoScenario::MissingGroup => {}
        UserInfoScenario::WrongGroup => {
            claims.insert("groups".to_string(), json!(["different-group"]));
        }
        UserInfoScenario::MalformedGroup => {
            claims.insert("groups".to_string(), json!({ "name": MOCK_REQUIRED_GROUP }));
        }
        UserInfoScenario::ScalarGroup => {
            claims.insert("groups".to_string(), Value::String(MOCK_REQUIRED_GROUP.to_string()));
        }
        _ => {
            claims.insert("groups".to_string(), json!([MOCK_REQUIRED_GROUP]));
        }
    }

    Value::Object(claims)
}
