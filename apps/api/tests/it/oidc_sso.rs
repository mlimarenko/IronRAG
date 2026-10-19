//! End-to-end persistence and HTTP assertions for the OIDC browser boundary.
//!
//! These cases intentionally use a temporary PostgreSQL database and the
//! shared Redis service. The protocol exchange itself is unit-tested at the
//! cryptographic boundary; this suite proves the durable single-use state,
//! identity serialization, bootstrap reconciliation, and ordinary IronRAG
//! session/token behavior that follow a verified identity.

use crate::{http_response_support, temp_postgres_database};

use anyhow::{Context, Result, ensure};
use argon2::{
    Argon2,
    password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString, rand_core::OsRng},
};
use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use sqlx::{PgPool, postgres::PgPoolOptions};
use tokio::{net::TcpListener, task::JoinHandle};
use tower::ServiceExt;
use uuid::Uuid;

use ironrag_backend::{
    app::{
        bootstrap::ensure_bootstrap_admin, config::Settings, seed_clean_v1_ai_catalog,
        state::AppState,
    },
    infra::{
        persistence::Persistence,
        repositories::{catalog_repository, iam_repository},
    },
    interfaces::http::{auth::build_session_cookie_value, router, router_support::ApiError},
    services::iam::{
        oidc::mock_idp::{
            MOCK_AUTHORIZATION_CODE, MOCK_EMAIL, MOCK_SUBJECT, MockOidcProvider, MockScenario,
            UserInfoScenario,
        },
        service::{AuthenticateOidcSessionCommand, AuthenticateSessionCommand},
    },
};
use temp_postgres_database::TempPostgresDatabase;

const TEST_CREDENTIAL_MASTER_KEY: &str = "KioqKioqKioqKioqKioqKioqKioqKioqKioqKioqKio=";
const TEST_OIDC_ISSUER: &str = "https://issuer.example.invalid/realms/ironrag";
const TEST_OIDC_CLIENT_ID: &str = "ironrag-integration";
const TEST_OIDC_CLIENT_SECRET: &str = "integration-client-secret";
const TEST_OIDC_REDIRECT: &str = "http://127.0.0.1:19000/v1/iam/oidc/callback";
const TEST_OIDC_REQUIRED_GROUP: &str = "ironrag-users";
const OIDC_LOGIN_CLUSTER_BURST: u64 = 8;
const OIDC_LOGIN_SOURCE_BURST: u64 = 2;
const OIDC_LOGIN_TOKEN_SCALE: u64 = 1_000_000;
const OIDC_LOGIN_BUCKET_TTL_SECONDS: u64 = 5;

struct OidcFixture {
    state: AppState,
    temp_database: TempPostgresDatabase,
}

impl OidcFixture {
    async fn create(suite_name: &str, settings: Settings) -> Result<Self> {
        let temp_database =
            TempPostgresDatabase::create(&settings.database_url, suite_name).await?;
        let postgres = PgPoolOptions::new()
            .max_connections(8)
            .connect(&temp_database.database_url)
            .await
            .context("failed to connect temporary OIDC PostgreSQL database")?;
        sqlx::migrate!("./migrations")
            .run(&postgres)
            .await
            .context("failed to apply OIDC test migrations")?;
        seed_clean_v1_ai_catalog(&postgres).await.context("failed to seed OIDC test AI catalog")?;
        let state = build_test_state(settings, postgres)?;
        Ok(Self { state, temp_database })
    }

    async fn create_oidc(suite_name: &str) -> Result<Self> {
        let mut settings = base_settings()?;
        configure_oidc(&mut settings);
        Self::create(suite_name, settings).await
    }

    /// A deployment whose login admission reads the trusted `X-Forwarded-For`
    /// hop, so a case can drive distinct per-source buckets.
    async fn create_oidc_with_trusted_hop_and_issuer(
        suite_name: &str,
        issuer_url: String,
    ) -> Result<Self> {
        let mut settings = base_settings()?;
        configure_oidc(&mut settings);
        settings.oidc_issuer_url = Some(issuer_url);
        settings.login_rate_limit_trusted_proxy_hops = 1;
        Self::create(suite_name, settings).await
    }

    /// A deployment whose identity provider is the in-process mock.
    async fn create_oidc_with_provider(
        suite_name: &str,
        provider: &MockOidcProvider,
    ) -> Result<Self> {
        let mut fixture = Self::create_oidc(suite_name).await?;
        fixture.state = fixture.state.with_oidc_settings_for_tests(provider.settings());
        Ok(fixture)
    }

    /// One callback request, as the browser sends it after the provider redirect.
    async fn callback(
        &self,
        state: &str,
        code: Option<&str>,
        cookie_binding: &str,
    ) -> Result<axum::response::Response> {
        let uri = match code {
            Some(code) => format!("/v1/iam/oidc/callback?state={state}&code={code}"),
            None => format!("/v1/iam/oidc/callback?state={state}"),
        };
        self.app()
            .oneshot(
                Request::builder()
                    .uri(uri)
                    .header(header::COOKIE, format!("ironrag_oidc_attempt={cookie_binding}"))
                    .body(Body::empty())
                    .context("failed to build OIDC callback")?,
            )
            .await
            .context("OIDC callback failed")
    }

    fn app(&self) -> Router {
        Router::new().nest("/v1", router()).with_state(self.state.clone())
    }

    async fn redis_set_attempt(
        &self,
        state: &str,
        cookie_binding: &str,
        return_path: &str,
    ) -> Result<()> {
        let payload = json!({
            "cookie_binding": cookie_binding,
            "pkce_verifier": "test-pkce-verifier",
            "nonce": "test-nonce",
            "return_path": return_path,
        })
        .to_string();
        let mut connection = self
            .state
            .persistence
            .redis
            .get_multiplexed_async_connection()
            .await
            .context("failed to connect OIDC test Redis")?;
        let _: String = redis::cmd("SET")
            .arg(oidc_attempt_key(state))
            .arg(payload)
            .arg("EX")
            .arg(300)
            .query_async(&mut connection)
            .await
            .context("failed to seed OIDC Redis attempt")?;
        Ok(())
    }

    async fn redis_attempt_exists(&self, state: &str) -> Result<bool> {
        let mut connection = self
            .state
            .persistence
            .redis
            .get_multiplexed_async_connection()
            .await
            .context("failed to connect OIDC test Redis")?;
        let value: Option<String> = redis::cmd("GET")
            .arg(oidc_attempt_key(state))
            .query_async(&mut connection)
            .await
            .context("failed to read OIDC Redis attempt")?;
        Ok(value.is_some())
    }

    async fn cleanup(self) -> Result<()> {
        self.state.persistence.postgres.close().await;
        self.temp_database.drop().await
    }
}

fn base_settings() -> Result<Settings> {
    let mut settings = Settings::from_env().context("failed to load OIDC test settings")?;
    settings.service_role = "api".to_string();
    settings.destructive_fresh_bootstrap_required = true;
    settings.credential_master_key = Some(TEST_CREDENTIAL_MASTER_KEY.to_string());
    settings.ui_bootstrap_admin_login = None;
    settings.ui_bootstrap_admin_email = None;
    settings.ui_bootstrap_admin_name = None;
    settings.ui_bootstrap_admin_password = None;
    settings.ui_bootstrap_admin_api_token = None;
    settings.oidc_issuer_url = None;
    settings.oidc_client_id = None;
    settings.oidc_client_secret = None;
    settings.oidc_redirect_url = None;
    settings.oidc_required_group = None;
    settings.oidc_bootstrap_admin_subject = None;
    Ok(settings)
}

fn configure_oidc(settings: &mut Settings) {
    settings.frontend_origin = "http://127.0.0.1:19000".to_string();
    settings.oidc_issuer_url = Some(TEST_OIDC_ISSUER.to_string());
    settings.oidc_client_id = Some(TEST_OIDC_CLIENT_ID.to_string());
    settings.oidc_client_secret = Some(TEST_OIDC_CLIENT_SECRET.to_string());
    settings.oidc_redirect_url = Some(TEST_OIDC_REDIRECT.to_string());
    settings.oidc_required_group = Some(TEST_OIDC_REQUIRED_GROUP.to_string());
    settings.oidc_bootstrap_admin_subject = Some("sso-bootstrap-subject".to_string());
}

fn build_test_state(settings: Settings, postgres: PgPool) -> Result<AppState> {
    let redis = redis::Client::open(settings.redis_url.clone())
        .context("failed to create OIDC test Redis client")?;
    AppState::from_dependencies(settings, Persistence::for_tests(postgres, redis))
}

fn oidc_attempt_key(state: &str) -> String {
    format!("ironrag:oidc:attempt:{}", hex::encode(Sha256::digest(state.as_bytes())))
}

fn oidc_login_admission_key(issuer_url: &str) -> String {
    format!(
        "ironrag:oidc:login-admission:v1:{{{}}}",
        hex::encode(Sha256::digest(issuer_url.as_bytes()))
    )
}

/// Mirror of `oidc_login_admission_source_redis_key`: the source identity is
/// hashed into the bucket name and the issuer hash is the `{...}` hash tag.
fn oidc_login_source_admission_key(issuer_url: &str, source_key: &str) -> String {
    format!(
        "ironrag:oidc:login-admission-source:v1:{}:{{{}}}",
        hex::encode(Sha256::digest(source_key.as_bytes())),
        hex::encode(Sha256::digest(issuer_url.as_bytes()))
    )
}

/// A deliberately non-TLS loopback endpoint. It makes a valid HTTPS issuer
/// fail at discovery immediately after the admission path has accepted it,
/// without calling a real identity provider or creating an OIDC attempt.
struct RejectedTlsDiscoveryProbe {
    issuer_url: String,
    task: JoinHandle<()>,
}

impl RejectedTlsDiscoveryProbe {
    async fn start() -> Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .context("failed to bind rejecting OIDC TLS probe")?;
        let issuer_url = format!(
            "https://{}/oidc-admission-{}",
            listener.local_addr().context("failed to read rejecting OIDC TLS probe address")?,
            Uuid::now_v7()
        );
        let task = tokio::spawn(async move {
            while let Ok((stream, _address)) = listener.accept().await {
                drop(stream);
            }
        });

        Ok(Self { issuer_url, task })
    }
}

impl Drop for RejectedTlsDiscoveryProbe {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn oidc_redis_time_micros(client: &redis::Client) -> Result<i64> {
    let mut connection = client
        .get_multiplexed_async_connection()
        .await
        .context("failed to connect OIDC admission test Redis")?;
    let parts: Vec<i64> = redis::cmd("TIME")
        .query_async(&mut connection)
        .await
        .context("failed to read OIDC admission test Redis TIME")?;
    ensure!(parts.len() == 2, "OIDC admission Redis TIME returned an unexpected response");
    parts[0]
        .checked_mul(1_000_000)
        .and_then(|seconds| seconds.checked_add(parts[1]))
        .context("OIDC admission Redis TIME overflowed microseconds")
}

async fn seed_oidc_login_admission_bucket(
    client: &redis::Client,
    key: &str,
    tokens: impl redis::ToRedisArgs,
    updated_at_micros: i64,
) -> Result<()> {
    let mut connection = client
        .get_multiplexed_async_connection()
        .await
        .context("failed to connect OIDC admission test Redis")?;
    let _: i64 = redis::cmd("HSET")
        .arg(key)
        .arg("tokens")
        .arg(tokens)
        .arg("updated_at_micros")
        .arg(updated_at_micros)
        .query_async(&mut connection)
        .await
        .context("failed to seed OIDC login admission bucket")?;
    let _: i64 = redis::cmd("EXPIRE")
        .arg(key)
        .arg(OIDC_LOGIN_BUCKET_TTL_SECONDS)
        .query_async(&mut connection)
        .await
        .context("failed to set OIDC login admission bucket expiry")?;
    Ok(())
}

async fn oidc_login_admission_bucket_updated_at_micros(
    client: &redis::Client,
    key: &str,
) -> Result<i64> {
    let mut connection = client
        .get_multiplexed_async_connection()
        .await
        .context("failed to connect OIDC admission test Redis")?;
    let value: Option<i64> = redis::cmd("HGET")
        .arg(key)
        .arg("updated_at_micros")
        .query_async(&mut connection)
        .await
        .context("failed to read OIDC login admission timestamp")?;
    value.context("OIDC login admission bucket lost its timestamp")
}

async fn oidc_login_source_bucket_ttl_seconds(client: &redis::Client, key: &str) -> Result<i64> {
    let mut connection = client
        .get_multiplexed_async_connection()
        .await
        .context("failed to connect OIDC admission test Redis")?;
    redis::cmd("TTL")
        .arg(key)
        .query_async(&mut connection)
        .await
        .context("failed to read the OIDC source bucket TTL")
}

async fn oidc_login_admission_bucket_tokens(client: &redis::Client, key: &str) -> Result<i64> {
    oidc_login_admission_bucket_tokens_optional(client, key)
        .await?
        .context("OIDC login admission bucket lost its token count")
}

async fn oidc_login_admission_bucket_tokens_optional(
    client: &redis::Client,
    key: &str,
) -> Result<Option<i64>> {
    let mut connection = client
        .get_multiplexed_async_connection()
        .await
        .context("failed to connect OIDC admission test Redis")?;
    redis::cmd("HGET")
        .arg(key)
        .arg("tokens")
        .query_async(&mut connection)
        .await
        .context("failed to read OIDC login admission token count")
}

async fn delete_oidc_login_admission_bucket(client: &redis::Client, key: &str) -> Result<()> {
    let mut connection = client
        .get_multiplexed_async_connection()
        .await
        .context("failed to connect OIDC admission test Redis")?;
    let _: i64 = redis::cmd("DEL")
        .arg(key)
        .query_async(&mut connection)
        .await
        .context("failed to remove the exact OIDC login admission test key")?;
    Ok(())
}

/// One admission request whose source key is the single trusted hop.
async fn oidc_login_response_from(
    fixture: &OidcFixture,
    source: &str,
) -> Result<axum::response::Response> {
    fixture
        .app()
        .oneshot(
            Request::builder()
                .uri("/v1/iam/oidc/login?returnTo=/libraries")
                .header("x-forwarded-for", source)
                .body(Body::empty())
                .context("failed to build OIDC admission login request")?,
        )
        .await
        .context("OIDC admission login request failed")
}

/// The stable error code of a failed OIDC login response. `oidc_unavailable`
/// means admission let the request through and the deliberately non-TLS
/// discovery probe failed afterwards; `oidc_login_admission_rejected` means
/// admission itself refused it.
async fn oidc_login_error_code(response: axum::response::Response) -> Result<String> {
    let body = response_json(response).await?;
    body["code"].as_str().map(str::to_string).context("OIDC login error carried no code")
}

async fn ensure_oidc_login_error(
    response: axum::response::Response,
    expected_code: &str,
    message: &str,
) -> Result<()> {
    ensure!(response.status() == StatusCode::SERVICE_UNAVAILABLE, "{message}: status changed");
    ensure!(
        response.headers().get(header::SET_COOKIE).is_none(),
        "{message}: response unexpectedly set a browser cookie"
    );
    let body = response_json(response).await?;
    ensure!(body["code"] == Value::String(expected_code.to_string()), "{message}: {body}");
    Ok(())
}

fn oidc_command(
    provider: &str,
    subject: &str,
    login: &str,
    email: &str,
    bootstrap_admin: bool,
) -> AuthenticateOidcSessionCommand {
    AuthenticateOidcSessionCommand {
        auth_provider_kind: provider.to_string(),
        external_subject: subject.to_string(),
        login: login.to_string(),
        email: email.to_string(),
        display_name: email.to_string(),
        bootstrap_admin,
    }
}

fn password_hash(password: &str) -> Result<String> {
    Argon2::default()
        .hash_password(password.as_bytes(), &SaltString::generate(&mut OsRng))
        .map(|hash| hash.to_string())
        .context("failed to generate integration password hash")
}

fn password_hash_matches(password: &str, encoded_hash: &str) -> Result<bool> {
    let parsed = PasswordHash::new(encoded_hash).context("invalid integration password hash")?;
    Ok(Argon2::default().verify_password(password.as_bytes(), &parsed).is_ok())
}

async fn response_json(response: axum::response::Response) -> Result<Value> {
    http_response_support::response_json(response).await
}

fn redirect_location(response: &axum::response::Response) -> Result<&str> {
    ensure!(response.status() == StatusCode::SEE_OTHER, "OIDC callback did not redirect");
    response
        .headers()
        .get(header::LOCATION)
        .and_then(|value| value.to_str().ok())
        .context("OIDC callback redirect has no readable location")
}

/// A rejected callback lands on the sign-in screen with the page the person
/// was heading for and one of two verdicts, and carries nothing the identity
/// provider or the person said.
fn ensure_rejected_to_login(
    response: &axum::response::Response,
    return_path: &str,
    sso_error: &str,
    message: &str,
) -> Result<()> {
    let location = redirect_location(response)?;
    let expected = format!(
        "/login?{}",
        url::form_urlencoded::Serializer::new(String::new())
            .append_pair("returnTo", return_path)
            .append_pair("ssoError", sso_error)
            .finish()
    );
    ensure!(location == expected, "{message}: redirected to {location}, expected {expected}");
    Ok(())
}

fn ensure_location_reveals_nothing(location: &str, secrets: &[&str]) -> Result<()> {
    for secret in secrets {
        ensure!(!location.contains(secret), "OIDC callback redirect exposed {secret:?}");
    }
    Ok(())
}

#[tokio::test]
#[ignore = "requires local postgres and redis services"]
async fn callback_state_is_cookie_bound_single_use_and_sso_is_advertised() -> Result<()> {
    let fixture = OidcFixture::create_oidc("oidc_callback_state").await?;
    let result = async {
        let context_response = fixture
            .app()
            .oneshot(
                Request::builder()
                    .uri("/v1/iam/context")
                    .body(Body::empty())
                    .context("failed to build SSO availability request")?,
            )
            .await
            .context("SSO availability request failed")?;
        ensure!(context_response.status() == StatusCode::OK, "SSO availability request failed");
        let context = response_json(context_response).await?;
        ensure!(
            context["bootstrapStatus"]["ssoAvailable"] == Value::Bool(true),
            "SSO not advertised in the canonical bootstrap contract"
        );

        let state = format!("state-{}", Uuid::now_v7());
        let binding = format!("binding-{}", Uuid::now_v7());
        fixture.redis_set_attempt(&state, &binding, "/libraries?tab=all").await?;

        let unrelated_response = fixture
            .app()
            .oneshot(
                Request::builder()
                    .uri(format!("/v1/iam/oidc/callback?state=unrelated-{state}"))
                    .header(header::COOKIE, format!("ironrag_oidc_attempt={binding}"))
                    .body(Body::empty())
                    .context("failed to build unrelated OIDC callback")?,
            )
            .await
            .context("unrelated OIDC callback failed")?;
        ensure!(
            unrelated_response.status() == StatusCode::SEE_OTHER,
            "unrelated callback leaked an error"
        );
        ensure!(
            unrelated_response.headers().get(header::SET_COOKIE).is_none(),
            "unrelated callback cleared an active OIDC attempt cookie"
        );
        ensure!(
            fixture.redis_attempt_exists(&state).await?,
            "unrelated callback consumed a valid OIDC attempt"
        );

        let csrf_response = fixture
            .app()
            .oneshot(
                Request::builder()
                    .uri(format!("/v1/iam/oidc/callback?state={state}"))
                    .header(header::COOKIE, "ironrag_oidc_attempt=wrong-browser")
                    .body(Body::empty())
                    .context("failed to build OIDC CSRF callback")?,
            )
            .await
            .context("OIDC CSRF callback failed")?;
        ensure_rejected_to_login(&csrf_response, "/", "failed", "CSRF callback")?;
        ensure_location_reveals_nothing(redirect_location(&csrf_response)?, &[&state])?;
        ensure!(
            csrf_response.headers().get(header::SET_COOKIE).is_none(),
            "cookie mismatch cleared an active OIDC attempt cookie"
        );
        ensure!(
            fixture.redis_attempt_exists(&state).await?,
            "cookie mismatch consumed a valid OIDC attempt"
        );

        let consumed_response = fixture
            .app()
            .oneshot(
                Request::builder()
                    .uri(format!("/v1/iam/oidc/callback?state={state}"))
                    .header(header::COOKIE, format!("ironrag_oidc_attempt={binding}"))
                    .body(Body::empty())
                    .context("failed to build OIDC consume callback")?,
            )
            .await
            .context("OIDC consume callback failed")?;
        // A consumed attempt without an authorization code cannot complete;
        // the person is told so where they can retry, with their page kept.
        ensure_rejected_to_login(
            &consumed_response,
            "/libraries?tab=all",
            "failed",
            "OIDC callback without a code",
        )?;
        ensure!(
            !fixture.redis_attempt_exists(&state).await?,
            "accepted OIDC attempt was not consumed"
        );
        ensure!(
            consumed_response.headers().get(header::SET_COOKIE).is_some(),
            "accepted OIDC callback did not clear its consumed attempt cookie"
        );

        let replay_response = fixture
            .app()
            .oneshot(
                Request::builder()
                    .uri(format!("/v1/iam/oidc/callback?state={state}"))
                    .header(header::COOKIE, format!("ironrag_oidc_attempt={binding}"))
                    .body(Body::empty())
                    .context("failed to build OIDC replay callback")?,
            )
            .await
            .context("OIDC replay callback failed")?;
        ensure_rejected_to_login(&replay_response, "/", "failed", "replayed OIDC callback")?;
        ensure!(
            replay_response.headers().get(header::SET_COOKIE).is_none(),
            "replayed OIDC callback cleared a non-matching attempt cookie"
        );
        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres and redis services"]
async fn login_admission_bucket_enforces_burst_refill_malformed_state_and_clock_rollback()
-> Result<()> {
    let discovery_probe = RejectedTlsDiscoveryProbe::start().await?;
    let fixture = OidcFixture::create_oidc_with_trusted_hop_and_issuer(
        "oidc_login_admission",
        discovery_probe.issuer_url.clone(),
    )
    .await?;
    let redis = fixture.state.persistence.redis.clone();
    let bucket_key = oidc_login_admission_key(&discovery_probe.issuer_url);

    let result = async {
        delete_oidc_login_admission_bucket(&redis, &bucket_key).await?;

        // Seed the timestamp ahead of Redis TIME. Correct Lua keeps that
        // effective timestamp after each debit; writing the lower observed
        // time here would let a rollback refill the bucket twice.
        //
        // Each request comes from its own source: the per-source axis is a
        // separate case's subject, and one shared source would reject these
        // requests before they reach the issuer-wide bucket at all.
        let future_timestamp = oidc_redis_time_micros(&redis)
            .await?
            .checked_add(60_000_000)
            .context("OIDC admission future timestamp overflowed")?;
        seed_oidc_login_admission_bucket(
            &redis,
            &bucket_key,
            OIDC_LOGIN_CLUSTER_BURST * OIDC_LOGIN_TOKEN_SCALE,
            future_timestamp,
        )
        .await?;
        for index in 0..OIDC_LOGIN_CLUSTER_BURST {
            ensure_oidc_login_error(
                oidc_login_response_from(&fixture, &format!("198.51.100.{index}")).await?,
                "oidc_unavailable",
                "configured OIDC burst was rejected before discovery",
            )
            .await?;
        }
        ensure!(
            oidc_login_admission_bucket_updated_at_micros(&redis, &bucket_key).await?
                == future_timestamp,
            "OIDC admission rollback wrote a timestamp below its effective time"
        );
        ensure_oidc_login_error(
            oidc_login_response_from(&fixture, "198.51.100.200").await?,
            "oidc_login_admission_rejected",
            "OIDC admission bucket accepted more than its configured burst",
        )
        .await?;

        // Redis TIME, rather than a caller clock, refills a deliberately
        // empty bucket. The request reaches discovery and therefore reports
        // only the generic OIDC availability failure from the local probe.
        let past_timestamp = oidc_redis_time_micros(&redis)
            .await?
            .checked_sub(1_000_000)
            .context("OIDC admission past timestamp underflowed")?;
        seed_oidc_login_admission_bucket(&redis, &bucket_key, 0_u64, past_timestamp).await?;
        ensure_oidc_login_error(
            oidc_login_response_from(&fixture, "198.51.100.201").await?,
            "oidc_unavailable",
            "OIDC admission bucket did not refill from Redis TIME",
        )
        .await?;

        let now = oidc_redis_time_micros(&redis).await?;
        seed_oidc_login_admission_bucket(&redis, &bucket_key, "malformed", now).await?;
        ensure_oidc_login_error(
            oidc_login_response_from(&fixture, "198.51.100.202").await?,
            "oidc_login_admission_rejected",
            "malformed OIDC admission bucket did not fail closed",
        )
        .await?;
        Ok(())
    }
    .await;

    let redis_cleanup = delete_oidc_login_admission_bucket(&redis, &bucket_key).await;
    let fixture_cleanup = fixture.cleanup().await;
    redis_cleanup?;
    fixture_cleanup?;
    result
}

/// IRG-91: one anonymous origin must not be able to drain the issuer-wide
/// sign-in budget, because doing so turns its own flood into an outage for
/// every other origin of the deployment.
#[tokio::test]
#[ignore = "requires local postgres and redis services"]
async fn login_admission_source_axis_keeps_one_origin_from_draining_the_shared_bucket() -> Result<()>
{
    let discovery_probe = RejectedTlsDiscoveryProbe::start().await?;
    let fixture = OidcFixture::create_oidc_with_trusted_hop_and_issuer(
        "oidc_login_source_axis",
        discovery_probe.issuer_url.clone(),
    )
    .await?;
    let redis = fixture.state.persistence.redis.clone();
    let issuer_url = discovery_probe.issuer_url.clone();
    let cluster_key = oidc_login_admission_key(&issuer_url);
    let flooding_source = "198.51.100.10";
    let source_key = oidc_login_source_admission_key(&issuer_url, flooding_source);

    let result = async {
        delete_oidc_login_admission_bucket(&redis, &cluster_key).await?;
        delete_oidc_login_admission_bucket(&redis, &source_key).await?;

        // Freeze both clocks ahead of Redis TIME: no refill can grant the
        // flood extra admissions while the loop runs, so the exact counts
        // below cannot race the one-second source refill.
        let future_timestamp = oidc_redis_time_micros(&redis)
            .await?
            .checked_add(60_000_000)
            .context("OIDC source-axis future timestamp overflowed")?;
        seed_oidc_login_admission_bucket(
            &redis,
            &cluster_key,
            OIDC_LOGIN_CLUSTER_BURST * OIDC_LOGIN_TOKEN_SCALE,
            future_timestamp,
        )
        .await?;
        seed_oidc_login_admission_bucket(
            &redis,
            &source_key,
            OIDC_LOGIN_SOURCE_BURST * OIDC_LOGIN_TOKEN_SCALE,
            future_timestamp,
        )
        .await?;

        // One origin floods the whole issuer-wide burst. Only its own budget
        // may be spent: the shared bucket stays intact for everyone else.
        let mut admitted = 0_usize;
        let mut rejected = 0_usize;
        for _ in 0..OIDC_LOGIN_CLUSTER_BURST {
            let code =
                oidc_login_error_code(oidc_login_response_from(&fixture, flooding_source).await?)
                    .await?;
            match code.as_str() {
                "oidc_unavailable" => admitted += 1,
                "oidc_login_admission_rejected" => rejected += 1,
                other => anyhow::bail!("unexpected OIDC login outcome: {other}"),
            }
        }
        ensure!(
            admitted == usize::try_from(OIDC_LOGIN_SOURCE_BURST).unwrap_or(usize::MAX),
            "one origin spent {admitted} starts of its own {OIDC_LOGIN_SOURCE_BURST}-token budget"
        );
        ensure!(
            rejected
                == usize::try_from(OIDC_LOGIN_CLUSTER_BURST - OIDC_LOGIN_SOURCE_BURST)
                    .unwrap_or(usize::MAX),
            "one origin was rejected {rejected} times instead of after its own burst"
        );

        // A different origin still signs in while the flood is still going:
        // the whole point of the per-source axis.
        ensure_oidc_login_error(
            oidc_login_response_from(&fixture, "198.51.100.20").await?,
            "oidc_unavailable",
            "a second origin was locked out by one flooding origin",
        )
        .await?;

        // The aggregate axis still fails closed on its own, and a rejection
        // there must not spend the source budget of the caller it refuses.
        // The empty cluster bucket keeps the frozen future timestamp so the
        // one-second cluster refill cannot admit this request.
        seed_oidc_login_admission_bucket(&redis, &cluster_key, 0_u64, future_timestamp).await?;
        let fresh_source = "198.51.100.30";
        let fresh_key = oidc_login_source_admission_key(&issuer_url, fresh_source);
        delete_oidc_login_admission_bucket(&redis, &fresh_key).await?;
        ensure_oidc_login_error(
            oidc_login_response_from(&fixture, fresh_source).await?,
            "oidc_login_admission_rejected",
            "an exhausted issuer-wide bucket did not reject",
        )
        .await?;
        let remaining = oidc_login_admission_bucket_tokens_optional(&redis, &fresh_key).await?;
        ensure!(
            remaining.is_none()
                || remaining
                    == Some(
                        i64::try_from(OIDC_LOGIN_SOURCE_BURST * OIDC_LOGIN_TOKEN_SCALE)
                            .unwrap_or(i64::MAX)
                    ),
            "a cluster-axis rejection spent the source budget of the refused caller"
        );
        delete_oidc_login_admission_bucket(&redis, &fresh_key).await?;

        // The flood spent exactly its own burst: the bucket is visibly empty.
        ensure!(
            oidc_login_admission_bucket_tokens(&redis, &source_key).await? == 0,
            "the flooding origin's own bucket was not charged for its admitted starts"
        );

        // A source bucket refills from Redis TIME like the cluster one, and a
        // charged bucket carries its expiry. The issuer-wide bucket goes back
        // to full first so this step tests the source axis alone.
        seed_oidc_login_admission_bucket(
            &redis,
            &cluster_key,
            OIDC_LOGIN_CLUSTER_BURST * OIDC_LOGIN_TOKEN_SCALE,
            future_timestamp,
        )
        .await?;
        let past_timestamp = oidc_redis_time_micros(&redis)
            .await?
            .checked_sub(1_000_000)
            .context("OIDC source-axis past timestamp underflowed")?;
        seed_oidc_login_admission_bucket(&redis, &source_key, 0_u64, past_timestamp).await?;
        ensure_oidc_login_error(
            oidc_login_response_from(&fixture, flooding_source).await?,
            "oidc_unavailable",
            "the source bucket did not refill from Redis TIME",
        )
        .await?;
        ensure!(
            oidc_login_source_bucket_ttl_seconds(&redis, &source_key).await? > 0,
            "a charged source bucket has no expiry"
        );

        // Asymmetric clocks: a source bucket that ran empty carries its own
        // timestamp and must not inherit refill from the cluster bucket's
        // future one. With one shared clock this request would be admitted.
        let now = oidc_redis_time_micros(&redis).await?;
        let asymmetric_future = now
            .checked_add(10_000_000)
            .context("OIDC asymmetric future timestamp overflowed")?;
        seed_oidc_login_admission_bucket(
            &redis,
            &cluster_key,
            OIDC_LOGIN_CLUSTER_BURST * OIDC_LOGIN_TOKEN_SCALE,
            asymmetric_future,
        )
        .await?;
        seed_oidc_login_admission_bucket(&redis, &source_key, 0_u64, now).await?;
        ensure_oidc_login_error(
            oidc_login_response_from(&fixture, flooding_source).await?,
            "oidc_login_admission_rejected",
            "a future cluster timestamp refilled an empty source bucket",
        )
        .await?;

        // Concurrent admissions from distinct sources spend the cluster
        // bucket exactly once per admitted request: the script is atomic, so
        // a burst cannot double-spend its tokens.
        let concurrent_tokens = 4_u64;
        seed_oidc_login_admission_bucket(
            &redis,
            &cluster_key,
            concurrent_tokens * OIDC_LOGIN_TOKEN_SCALE,
            future_timestamp,
        )
        .await?;
        let mut admissions = tokio::task::JoinSet::new();
        for index in 0..8_u16 {
            let app = fixture.app();
            let source = format!("198.51.100.{}", 40 + index);
            admissions.spawn(async move {
                let response = app
                    .oneshot(
                        Request::builder()
                            .uri("/v1/iam/oidc/login?returnTo=/libraries")
                            .header("x-forwarded-for", source)
                            .body(Body::empty())
                            .expect("concurrent OIDC admission request builds"),
                    )
                    .await
                    .expect("concurrent OIDC admission request runs");
                oidc_login_error_code(response).await
            });
        }
        let mut concurrent_admitted = 0_u64;
        let mut concurrent_rejected = 0_u64;
        while let Some(code) = admissions.join_next().await {
            match code.expect("concurrent admission task joins")?.as_str() {
                "oidc_unavailable" => concurrent_admitted += 1,
                "oidc_login_admission_rejected" => concurrent_rejected += 1,
                other => anyhow::bail!("unexpected concurrent OIDC login outcome: {other}"),
            }
        }
        ensure!(
            concurrent_admitted == concurrent_tokens
                && concurrent_rejected == 8 - concurrent_tokens,
            "concurrent admissions spent {concurrent_admitted} of {concurrent_tokens} tokens              ({concurrent_rejected} rejected)"
        );

        // A malformed source bucket fails the whole admission closed.
        seed_oidc_login_admission_bucket(&redis, &source_key, "malformed", future_timestamp)
            .await?;
        ensure_oidc_login_error(
            oidc_login_response_from(&fixture, flooding_source).await?,
            "oidc_login_admission_rejected",
            "malformed source bucket did not fail closed",
        )
        .await?;
        Ok(())
    }
    .await;

    let cluster_cleanup = delete_oidc_login_admission_bucket(&redis, &cluster_key).await;
    let source_cleanup = delete_oidc_login_admission_bucket(&redis, &source_key).await;
    let fixture_cleanup = fixture.cleanup().await;
    cluster_cleanup?;
    source_cleanup?;
    fixture_cleanup?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres and redis services"]
async fn jit_provisioning_serializes_concurrent_same_subject_and_keeps_identity_stable()
-> Result<()> {
    let fixture = OidcFixture::create_oidc("oidc_jit_concurrency").await?;
    let result = async {
        let provider = format!("oidc:concurrency:{}", Uuid::now_v7());
        let subject = format!("subject-{}", Uuid::now_v7());
        let login = format!("oidc-concurrent-{}", Uuid::now_v7().simple());
        let email = format!("{login}@example.invalid");
        let first_pool = fixture.state.persistence.postgres.clone();
        let second_pool = fixture.state.persistence.postgres.clone();
        let first_provider = provider.clone();
        let second_provider = provider.clone();
        let first_subject = subject.clone();
        let second_subject = subject.clone();
        let first_login = login.clone();
        let second_login = login.clone();
        let first_email = email.clone();
        let second_email = email.clone();

        let first = tokio::spawn(async move {
            iam_repository::provision_oidc_user(&first_pool, &iam_repository::NewOidcUser {
    auth_provider_kind: &first_provider,
    external_subject: &first_subject,
    login: &first_login,
    email: &first_email,
    display_name: "Concurrent OIDC User",
    password_hash: "oidc-only-sentinel",
    role: iam_repository::SystemRole::Viewer,
})
            .await
        });
        let second = tokio::spawn(async move {
            iam_repository::provision_oidc_user(&second_pool, &iam_repository::NewOidcUser {
    auth_provider_kind: &second_provider,
    external_subject: &second_subject,
    login: &second_login,
    email: &second_email,
    display_name: "Concurrent OIDC User",
    password_hash: "oidc-only-sentinel",
    role: iam_repository::SystemRole::Viewer,
})
            .await
        });
        let first = first.await.context("first OIDC JIT task panicked")??;
        let second = second.await.context("second OIDC JIT task panicked")??;
        let (first_id, first_created) = provisioned_identity(first)?;
        let (second_id, second_created) = provisioned_identity(second)?;
        ensure!(first_id == second_id, "same OIDC subject created multiple principals");
        ensure!(first_created != second_created, "same OIDC subject was not serialized");

        let durable_count = sqlx::query_scalar::<_, i64>(
            "select count(*)::bigint from iam_user where auth_provider_kind = $1 and external_subject = $2",
        )
        .bind(&provider)
        .bind(&subject)
        .fetch_one(&fixture.state.persistence.postgres)
        .await
        .context("failed to count concurrent OIDC identities")?;
        ensure!(durable_count == 1, "same OIDC subject created {durable_count} rows");

        let stable = iam_repository::provision_oidc_user(&fixture.state.persistence.postgres, &iam_repository::NewOidcUser {
    auth_provider_kind: &provider,
    external_subject: &subject,
    login: &login,
    email: "mutated-email@example.invalid",
    display_name: "Changed Display Name",
    password_hash: "oidc-only-sentinel",
    role: iam_repository::SystemRole::Admin,
})
        .await
        .context("failed to re-resolve stable OIDC identity")?;
        let (stable_id, stable_created) = provisioned_identity(stable)?;
        ensure!(stable_id == first_id && !stable_created, "OIDC identity was re-provisioned");
        let stable_user = iam_repository::get_user_by_external_identity(
            &fixture.state.persistence.postgres,
            &provider,
            &subject,
        )
        .await
        .context("failed to reload stable OIDC identity")?
        .context("stable OIDC identity disappeared")?;
        ensure!(stable_user.email == email, "OIDC identity followed mutable email data");
        ensure!(stable_user.role == "viewer", "OIDC role changed on repeat provisioning");
        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres and redis services"]
async fn jit_provisioning_rejects_email_and_login_collisions_without_linking() -> Result<()> {
    let fixture = OidcFixture::create_oidc("oidc_jit_collisions").await?;
    let result = async {
        let local_user = iam_repository::create_user(
            &fixture.state.persistence.postgres,
            "existing-local-login",
            "existing-local@example.invalid",
            "Existing local user",
            "synthetic-local-password-hash",
            iam_repository::SystemRole::Admin,
        )
        .await
        .context("failed to create local collision fixture")?;

        let email_collision = iam_repository::provision_oidc_user(
            &fixture.state.persistence.postgres,
            &iam_repository::NewOidcUser {
                auth_provider_kind: "oidc:email-collision",
                external_subject: "email-collision-subject",
                login: "fresh-oidc-login",
                email: &local_user.email,
                display_name: "OIDC user",
                password_hash: "oidc-only-sentinel",
                role: iam_repository::SystemRole::Viewer,
            },
        )
        .await
        .context("failed to evaluate OIDC email collision")?;
        ensure!(
            matches!(email_collision, iam_repository::OidcProvisionUserOutcome::IdentityCollision),
            "matching local email was linked to an OIDC identity"
        );

        let login_collision = iam_repository::provision_oidc_user(
            &fixture.state.persistence.postgres,
            &iam_repository::NewOidcUser {
                auth_provider_kind: "oidc:login-collision",
                external_subject: "login-collision-subject",
                login: &local_user.login,
                email: "fresh-oidc@example.invalid",
                display_name: "OIDC user",
                password_hash: "oidc-only-sentinel",
                role: iam_repository::SystemRole::Viewer,
            },
        )
        .await
        .context("failed to evaluate OIDC login collision")?;
        ensure!(
            matches!(login_collision, iam_repository::OidcProvisionUserOutcome::IdentityCollision),
            "matching local login was linked to an OIDC identity"
        );

        for (provider, subject) in [
            ("oidc:email-collision", "email-collision-subject"),
            ("oidc:login-collision", "login-collision-subject"),
        ] {
            ensure!(
                iam_repository::get_user_by_external_identity(
                    &fixture.state.persistence.postgres,
                    provider,
                    subject,
                )
                .await
                .context("failed to inspect rejected OIDC identity")?
                .is_none(),
                "rejected OIDC identity was persisted"
            );
        }
        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres and redis services"]
async fn oidc_only_password_login_and_disabled_identity_are_rejected() -> Result<()> {
    let fixture = OidcFixture::create_oidc("oidc_disabled_identity").await?;
    let result = async {
        let provider = "oidc:disabled-identity";
        let subject = "disabled-subject";
        let login = "oidc-disabled-user";
        let authenticated = fixture
            .state
            .application_services
            .iam
            .authenticate_oidc_session(
                &fixture.state,
                oidc_command(provider, subject, login, "disabled@example.invalid", false),
            )
            .await
            .context("failed to provision OIDC identity before disable")?;

        let password_login = fixture
            .state
            .application_services
            .iam
            .authenticate_session(
                &fixture.state,
                AuthenticateSessionCommand {
                    login: login.to_string(),
                    password: "not-an-oidc-password".to_string(),
                    source_key: "203.0.113.7".to_string(),
                },
            )
            .await;
        ensure!(
            matches!(password_login, Err(ApiError::Unauthorized)),
            "password login accepted an OIDC-only user"
        );

        let disabled = iam_repository::disable_principal(
            &fixture.state.persistence.postgres,
            authenticated.session.principal_id,
        )
        .await
        .context("failed to disable OIDC principal")?;
        ensure!(disabled.is_some(), "OIDC principal was not disabled");
        let repeated = fixture
            .state
            .application_services
            .iam
            .authenticate_oidc_session(
                &fixture.state,
                oidc_command(provider, subject, login, "disabled@example.invalid", false),
            )
            .await;
        ensure!(
            matches!(repeated, Err(ApiError::Unauthorized)),
            "disabled OIDC user received a new session"
        );
        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres and redis services"]
async fn bootstrap_password_reconciles_only_matching_local_identity() -> Result<()> {
    let mut settings = base_settings()?;
    settings.service_role = "startup".to_string();
    settings.ui_bootstrap_admin_login = Some("break-glass-admin".to_string());
    settings.ui_bootstrap_admin_email = Some("break-glass-admin@example.invalid".to_string());
    settings.ui_bootstrap_admin_name = Some("Break Glass Admin".to_string());
    settings.ui_bootstrap_admin_password = Some("new-bootstrap-password".to_string());
    let fixture = OidcFixture::create("oidc_bootstrap_reconcile", settings).await?;
    let result = async {
        let old_hash = password_hash("old-bootstrap-password")?;
        let local_user = iam_repository::create_user(
            &fixture.state.persistence.postgres,
            "break-glass-admin",
            "break-glass-admin@example.invalid",
            "Break Glass Admin",
            &old_hash,
            iam_repository::SystemRole::Admin,
        )
        .await
        .context("failed to create existing bootstrap user")?;

        ensure_bootstrap_admin(&fixture.state)
            .await
            .context("failed to reconcile bootstrap password")?;
        let reconciled = iam_repository::get_user_by_principal_id(
            &fixture.state.persistence.postgres,
            local_user.principal_id,
        )
        .await
        .context("failed to reload reconciled bootstrap user")?
        .context("reconciled bootstrap user disappeared")?;
        ensure!(
            password_hash_matches("new-bootstrap-password", &reconciled.password_hash)?,
            "configured bootstrap password was not reconciled"
        );
        ensure!(
            !password_hash_matches("old-bootstrap-password", &reconciled.password_hash)?,
            "stale bootstrap password still authenticates"
        );
        let reconciled_hash = reconciled.password_hash.clone();
        ensure_bootstrap_admin(&fixture.state)
            .await
            .context("failed to repeat bootstrap reconciliation")?;
        let unchanged = iam_repository::get_user_by_principal_id(
            &fixture.state.persistence.postgres,
            local_user.principal_id,
        )
        .await
        .context("failed to reload repeated bootstrap user")?
        .context("repeated bootstrap user disappeared")?;
        ensure!(
            unchanged.password_hash == reconciled_hash,
            "matching bootstrap password was unnecessarily rehashed"
        );

        let oidc_user = iam_repository::provision_oidc_user(
            &fixture.state.persistence.postgres,
            &iam_repository::NewOidcUser {
                auth_provider_kind: "oidc:bootstrap-collision",
                external_subject: "bootstrap-collision-subject",
                login: "oidc-bootstrap-user",
                email: "oidc-bootstrap@example.invalid",
                display_name: "OIDC bootstrap collision",
                password_hash: "oidc-only-sentinel",
                role: iam_repository::SystemRole::Viewer,
            },
        )
        .await
        .context("failed to create OIDC bootstrap collision fixture")?;
        let (oidc_principal_id, _) = provisioned_identity(oidc_user)?;
        let oidc_before = iam_repository::get_user_by_principal_id(
            &fixture.state.persistence.postgres,
            oidc_principal_id,
        )
        .await
        .context("failed to read OIDC bootstrap collision fixture")?
        .context("OIDC bootstrap collision fixture disappeared")?;

        let mut conflicting_settings = base_settings()?;
        conflicting_settings.service_role = "startup".to_string();
        conflicting_settings.ui_bootstrap_admin_login =
            Some("different-break-glass-login".to_string());
        conflicting_settings.ui_bootstrap_admin_email =
            Some("oidc-bootstrap@example.invalid".to_string());
        conflicting_settings.ui_bootstrap_admin_password = Some("replacement-password".to_string());
        let conflicting_state =
            build_test_state(conflicting_settings, fixture.state.persistence.postgres.clone())?;
        let conflict = ensure_bootstrap_admin(&conflicting_state).await;
        drop(conflicting_state);
        ensure!(
            matches!(conflict, Err(ApiError::Conflict(_))),
            "bootstrap configuration claimed an OIDC identity"
        );
        let oidc_after = iam_repository::get_user_by_principal_id(
            &fixture.state.persistence.postgres,
            oidc_principal_id,
        )
        .await
        .context("failed to reload OIDC bootstrap collision fixture")?
        .context("OIDC bootstrap collision fixture disappeared after conflict")?;
        ensure!(
            oidc_after.password_hash == oidc_before.password_hash
                && oidc_after.role == oidc_before.role
                && oidc_after.auth_provider_kind == "oidc:bootstrap-collision",
            "bootstrap reconciliation mutated an OIDC identity"
        );
        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres and redis services"]
async fn configured_sso_admin_mints_bearer_token_with_issuer_and_revocation() -> Result<()> {
    let fixture = OidcFixture::create_oidc("oidc_sso_admin_tokens").await?;
    let result = async {
        let provider = "oidc:sso-admin";
        let subject = "sso-bootstrap-subject";
        let oidc_session = fixture
            .state
            .application_services
            .iam
            .authenticate_oidc_session(
                &fixture.state,
                oidc_command(
                    provider,
                    subject,
                    "oidc-sso-admin",
                    "sso-admin@example.invalid",
                    true,
                ),
            )
            .await
            .context("failed to provision configured SSO administrator")?;
        ensure!(
            oidc_session.identity_created,
            "initial configured SSO identity was not marked as provisioned"
        );
        let session = oidc_session.session;
        let sso_admin = iam_repository::get_user_by_principal_id(
            &fixture.state.persistence.postgres,
            session.principal_id,
        )
        .await
        .context("failed to load configured SSO administrator")?
        .context("configured SSO administrator disappeared")?;
        ensure!(sso_admin.role == "admin", "configured SSO subject did not receive admin role");

        let repeat = fixture
            .state
            .application_services
            .iam
            .authenticate_oidc_session(
                &fixture.state,
                oidc_command(
                    provider,
                    subject,
                    "oidc-sso-admin",
                    "changed-sso-email@example.invalid",
                    false,
                ),
            )
            .await
            .context("failed to repeat configured SSO administrator login")?;
        ensure!(
            repeat.session.principal_id == session.principal_id && !repeat.identity_created,
            "configured SSO identity changed between logins"
        );
        let viewer = fixture
            .state
            .application_services
            .iam
            .authenticate_oidc_session(
                &fixture.state,
                oidc_command(
                    provider,
                    "ordinary-sso-subject",
                    "oidc-sso-viewer",
                    "sso-viewer@example.invalid",
                    false,
                ),
            )
            .await
            .context("failed to provision ordinary SSO user")?;
        let viewer_user = iam_repository::get_user_by_principal_id(
            &fixture.state.persistence.postgres,
            viewer.session.principal_id,
        )
        .await
        .context("failed to load ordinary SSO user")?
        .context("ordinary SSO user disappeared")?;
        ensure!(viewer_user.role == "viewer", "ordinary SSO user received elevated role");

        let workspace = catalog_repository::create_workspace(
            &fixture.state.persistence.postgres,
            &format!("oidc-token-workspace-{}", Uuid::now_v7().simple()),
            "OIDC Token Workspace",
            None,
        )
        .await
        .context("failed to create OIDC token workspace")?;
        let session_cookie = format!(
            "{}={}",
            fixture.state.ui_session_cookie.name,
            build_session_cookie_value(session.session_id, &session.session_secret)
        );
        let mint_response = fixture
            .app()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/iam/tokens")
                    .header(header::COOKIE, &session_cookie)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "workspaceId": workspace.id,
                            "label": "sso-admin-issued-token",
                            "permissionKinds": ["document_read"],
                        })
                        .to_string(),
                    ))
                    .context("failed to build SSO token mint request")?,
            )
            .await
            .context("SSO token mint request failed")?;
        ensure!(mint_response.status() == StatusCode::OK, "SSO admin could not mint an API token");
        let minted = response_json(mint_response).await?;
        let token = minted["token"]
            .as_str()
            .map(str::to_string)
            .context("mint response did not include plaintext token")?;
        let token_principal_id = minted["apiToken"]["principalId"]
            .as_str()
            .context("mint response did not include token principal id")?
            .parse::<Uuid>()
            .context("mint response included invalid token principal id")?;
        ensure!(
            minted["apiToken"]["issuer"]["principalId"]
                == Value::String(session.principal_id.to_string()),
            "minted token lost its SSO issuer"
        );

        let bearer_response = fixture
            .app()
            .oneshot(
                Request::builder()
                    .uri("/v1/iam/me")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .context("failed to build SSO bearer request")?,
            )
            .await
            .context("SSO bearer request failed")?;
        ensure!(bearer_response.status() == StatusCode::OK, "minted SSO token was not accepted");
        let bearer = response_json(bearer_response).await?;
        ensure!(
            bearer["principal"]["principalKind"] == Value::String("api_token".to_string()),
            "bearer request did not resolve the minted token principal"
        );

        let revoke_response = fixture
            .app()
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/v1/iam/tokens/{token_principal_id}"))
                    .header(header::COOKIE, &session_cookie)
                    .body(Body::empty())
                    .context("failed to build SSO token revoke request")?,
            )
            .await
            .context("SSO token revoke request failed")?;
        ensure!(
            revoke_response.status() == StatusCode::NO_CONTENT,
            "SSO admin could not revoke token"
        );
        let revoked_bearer_response = fixture
            .app()
            .oneshot(
                Request::builder()
                    .uri("/v1/iam/me")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .context("failed to build revoked SSO bearer request")?,
            )
            .await
            .context("revoked SSO bearer request failed")?;
        ensure!(
            revoked_bearer_response.status() == StatusCode::UNAUTHORIZED,
            "revoked SSO-issued token remained usable"
        );
        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres and redis services"]
async fn callback_tells_the_browser_denied_from_failed_without_identity_details() -> Result<()> {
    let provider = MockOidcProvider::start(MockScenario {
        userinfo: UserInfoScenario::MissingGroup,
        ..MockScenario::default()
    })
    .await?;
    let fixture =
        OidcFixture::create_oidc_with_provider("oidc_callback_verdict", &provider).await?;
    let result = async {
        let return_path = "/graph?layout=force";
        let pkce_verifier = "test-pkce-verifier";
        let nonce = "test-nonce";
        provider.expect_exchange(pkce_verifier, nonce)?;

        // The provider refuses the code: the exchange failed, nothing is known
        // about the person, and the browser is told only that much.
        let state = format!("state-{}", Uuid::now_v7());
        let binding = format!("binding-{}", Uuid::now_v7());
        fixture.redis_set_attempt(&state, &binding, return_path).await?;
        let failed_response = fixture.callback(&state, Some("unexpected-code"), &binding).await?;
        ensure_rejected_to_login(&failed_response, return_path, "failed", "refused code")?;
        ensure!(
            failed_response.headers().get(header::SET_COOKIE).is_some(),
            "refused code left the consumed attempt cookie in place"
        );
        ensure_location_reveals_nothing(
            redirect_location(&failed_response)?,
            &[&state, "unexpected-code", MOCK_SUBJECT, MOCK_EMAIL],
        )?;
        ensure!(provider.metrics().userinfo() == 0, "refused code still reached userinfo");

        // The provider vouches for the person, but they are not in the required
        // group: that is a denial, distinct from a failure, and still nameless.
        let state = format!("state-{}", Uuid::now_v7());
        let binding = format!("binding-{}", Uuid::now_v7());
        fixture.redis_set_attempt(&state, &binding, return_path).await?;
        let denied_response =
            fixture.callback(&state, Some(MOCK_AUTHORIZATION_CODE), &binding).await?;
        ensure_rejected_to_login(&denied_response, return_path, "denied", "missing group")?;
        ensure!(
            denied_response.headers().get(header::SET_COOKIE).is_some(),
            "denied callback left the consumed attempt cookie in place"
        );
        ensure_location_reveals_nothing(
            redirect_location(&denied_response)?,
            &[&state, MOCK_AUTHORIZATION_CODE, MOCK_SUBJECT, MOCK_EMAIL, "ironrag-users"],
        )?;
        ensure!(provider.metrics().userinfo() == 1, "denied callback skipped userinfo");
        ensure!(!fixture.redis_attempt_exists(&state).await?, "denied attempt was not consumed");
        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

fn provisioned_identity(outcome: iam_repository::OidcProvisionUserOutcome) -> Result<(Uuid, bool)> {
    match outcome {
        iam_repository::OidcProvisionUserOutcome::Provisioned { user, created } => {
            Ok((user.principal_id, created))
        }
        iam_repository::OidcProvisionUserOutcome::IdentityCollision => {
            anyhow::bail!("OIDC provisioning unexpectedly collided")
        }
    }
}
