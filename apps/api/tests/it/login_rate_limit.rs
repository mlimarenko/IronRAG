//! Login rate-limit integration tests (IRG-80). Ignored by default: they need
//! a live PostgreSQL and Redis, same as every other suite in this crate. They
//! also need the `test-support` feature: the only way to observe "did
//! Argon2id actually run" without relying on flaky response timing is the
//! process-global `PASSWORD_VERIFICATION_ATTEMPTS` counter that
//! `services::iam::service` exposes behind that feature.
//!
//! Each test drives the real HTTP router (`Router::oneshot`, in-process, no
//! socket) against `POST /v1/iam/sessions` and asserts on the *count* of real
//! `verify_password` invocations before and after the point where the rate
//! limit should engage -- not on how long a request took.

use crate::temp_postgres_database;

use temp_postgres_database::TempPostgresDatabase;

use anyhow::{Context, Result};
use argon2::{
    Argon2,
    password_hash::{PasswordHasher, SaltString, rand_core::OsRng},
};
use axum::{
    Router,
    body::Body,
    http::{HeaderValue, Request, StatusCode, header},
};
use serde_json::{Value, json};
use sqlx::{PgPool, postgres::PgPoolOptions};
use tower::ServiceExt;
use uuid::Uuid;

use ironrag_backend::{
    app::{config::Settings, state::AppState},
    infra::{
        persistence::Persistence,
        repositories::iam_repository::{self, SystemRole},
    },
    interfaces::http::router,
    services::iam::service::password_verification_attempts,
};

const TEST_PASSWORD: &str = "correct-horse-battery-staple-9182";
const WRONG_PASSWORD: &str = "this-is-definitely-not-the-password";

fn test_credential_master_key() -> String {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    STANDARD.encode([73_u8; 32])
}

fn argon2_hash(password: &str) -> Result<String> {
    Argon2::default()
        .hash_password(password.as_bytes(), &SaltString::generate(&mut OsRng))
        .map(|hash| hash.to_string())
        .map_err(|error| anyhow::anyhow!("failed to hash test password: {error}"))
}

/// Rate-limit settings tuned to small, deterministic values so a test can
/// exhaust a bucket in a handful of requests instead of relying on the
/// production defaults.
struct RateLimitTuning {
    per_login_max_attempts: u32,
    per_source_max_attempts: u32,
    trusted_proxy_hops: u32,
}

struct LoginRateLimitFixture {
    state: AppState,
    temp_database: TempPostgresDatabase,
}

impl LoginRateLimitFixture {
    async fn create(tuning: RateLimitTuning) -> Result<Self> {
        let mut settings =
            Settings::from_env().context("failed to load settings for login rate limit test")?;
        let temp_database =
            TempPostgresDatabase::create(&settings.database_url, "login_rate_limit").await?;
        settings.database_url = temp_database.database_url.clone();
        settings.credential_master_key = Some(test_credential_master_key());
        settings.login_rate_limit_per_login_max_attempts = tuning.per_login_max_attempts;
        settings.login_rate_limit_per_login_window_seconds = 120;
        settings.login_rate_limit_per_source_max_attempts = tuning.per_source_max_attempts;
        settings.login_rate_limit_per_source_window_seconds = 120;
        settings.login_rate_limit_max_concurrent_verifications = 8;
        settings.login_rate_limit_admission_queue = 32;
        settings.login_rate_limit_admission_timeout_ms = 2_000;
        settings.login_rate_limit_registry_max_entries = 1_000;
        settings.login_rate_limit_registry_idle_ttl_seconds = 600;
        settings.login_rate_limit_trusted_proxy_hops = tuning.trusted_proxy_hops;

        let postgres = PgPoolOptions::new()
            .max_connections(4)
            .connect(&settings.database_url)
            .await
            .context("failed to connect login rate limit test postgres")?;
        sqlx::migrate!("./migrations")
            .run(&postgres)
            .await
            .context("failed to apply login rate limit migrations")?;

        let redis = redis::Client::open(settings.redis_url.clone())
            .context("failed to create redis client for login rate limit test state")?;
        let persistence = Persistence::for_tests(postgres, redis);
        let state = AppState::from_dependencies(settings, persistence)
            .context("failed to build login rate limit test app state")?;

        Ok(Self { state, temp_database })
    }

    const fn pool(&self) -> &PgPool {
        &self.state.persistence.postgres
    }

    fn app(&self) -> Router {
        Router::new().nest("/v1", router()).with_state(self.state.clone())
    }

    async fn create_user(&self, login: &str) -> Result<()> {
        let password_hash = argon2_hash(TEST_PASSWORD)?;
        iam_repository::create_user(
            self.pool(),
            login,
            &format!("{login}@login-rate-limit.example.test"),
            login,
            &password_hash,
            SystemRole::Viewer,
        )
        .await
        .with_context(|| format!("failed to create login rate limit test user {login}"))?;
        Ok(())
    }

    /// Posts one login attempt and returns (status, retry-after header text,
    /// decoded JSON body).
    async fn login(
        &self,
        login: &str,
        password: &str,
        source_ip: &str,
    ) -> Result<(StatusCode, Option<String>, Value)> {
        let request = Request::builder()
            .method("POST")
            .uri("/v1/iam/sessions")
            .header(header::CONTENT_TYPE, "application/json")
            .header(
                header::HeaderName::from_static("x-forwarded-for"),
                HeaderValue::from_str(source_ip).context("invalid test source ip header value")?,
            )
            .body(Body::from(json!({ "login": login, "password": password }).to_string()))
            .context("failed to build login rate limit POST request")?;
        let response =
            self.app().oneshot(request).await.context("login rate limit POST request failed")?;
        let status = response.status();
        let retry_after = response
            .headers()
            .get(header::RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .map(ToString::to_string);
        let bytes = axum::body::to_bytes(response.into_body(), 16 * 1024)
            .await
            .context("failed to collect login rate limit response body")?;
        let body: Value = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes)
                .context("failed to decode login rate limit response json")?
        };
        Ok((status, retry_after, body))
    }

    async fn cleanup(self) -> Result<()> {
        self.state.persistence.postgres.close().await;
        self.temp_database.drop().await
    }
}

#[tokio::test]
#[ignore = "requires local postgres and redis services, and --features test-support"]
async fn login_axis_rate_limit_rejects_before_argon2_once_the_per_login_bucket_is_exhausted()
-> Result<()> {
    const PER_LOGIN_MAX_ATTEMPTS: u32 = 3;
    let fixture = LoginRateLimitFixture::create(RateLimitTuning {
        per_login_max_attempts: PER_LOGIN_MAX_ATTEMPTS,
        per_source_max_attempts: 50,
        trusted_proxy_hops: 1,
    })
    .await?;

    let result = async {
        let login = format!("login-axis-user-{}", Uuid::now_v7().simple());
        fixture.create_user(&login).await?;
        let source_ip = "203.0.113.10";

        // The first PER_LOGIN_MAX_ATTEMPTS wrong-password attempts are each
        // admitted: real Argon2id runs and rejects on a genuine password
        // mismatch, so every one of them is a real 401 with the counter
        // advancing by exactly one call.
        for attempt in 1..=PER_LOGIN_MAX_ATTEMPTS {
            let before = password_verification_attempts();
            let (status, retry_after, _body) =
                fixture.login(&login, WRONG_PASSWORD, source_ip).await?;
            let after = password_verification_attempts();
            assert_eq!(status, StatusCode::UNAUTHORIZED, "attempt {attempt} must reach Argon2id");
            assert_eq!(
                after - before,
                1,
                "attempt {attempt} must invoke verify_password exactly once"
            );
            assert!(retry_after.is_none(), "an admitted attempt must not carry Retry-After");
        }

        // The next attempt exceeds the login-axis bucket. It must be
        // rejected with 429 and a Retry-After hint, and -- the actual
        // vulnerability this closes -- verify_password must not run again.
        let before = password_verification_attempts();
        let (status, retry_after, body) = fixture.login(&login, WRONG_PASSWORD, source_ip).await?;
        let after = password_verification_attempts();
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "the login-axis bucket is exhausted");
        assert_eq!(
            after, before,
            "a rate-limited attempt must not invoke verify_password / Argon2id at all"
        );
        let retry_after = retry_after.context("429 response must carry a Retry-After header")?;
        assert!(
            retry_after.parse::<u64>().is_ok_and(|seconds| seconds > 0),
            "Retry-After must be a positive integer number of seconds, got {retry_after:?}"
        );
        assert_eq!(body["code"], json!("login_rate_limited"));

        // A second, distinct login from the same source is unaffected: the
        // login axis is keyed per normalized login, not shared across logins
        // hitting the same source.
        let other_login = format!("login-axis-other-{}", Uuid::now_v7().simple());
        fixture.create_user(&other_login).await?;
        let before = password_verification_attempts();
        let (status, _retry_after, _body) =
            fixture.login(&other_login, TEST_PASSWORD, source_ip).await?;
        let after = password_verification_attempts();
        assert_eq!(
            status,
            StatusCode::CREATED,
            "a different login must not share the exhausted bucket"
        );
        assert_eq!(after - before, 1);

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres and redis services, and --features test-support"]
async fn source_axis_rate_limit_rejects_distinct_nonexistent_logins_before_argon2() -> Result<()> {
    const PER_SOURCE_MAX_ATTEMPTS: u32 = 3;
    let fixture = LoginRateLimitFixture::create(RateLimitTuning {
        per_login_max_attempts: 50,
        per_source_max_attempts: PER_SOURCE_MAX_ATTEMPTS,
        trusted_proxy_hops: 1,
    })
    .await?;

    let result = async {
        let source_ip = "203.0.113.20";
        let run_suffix = Uuid::now_v7().simple().to_string();

        // Every attempt below uses a *different* nonexistent login, so the
        // per-login axis (capacity 50, and never the same key twice) never
        // engages. Every one of them still runs the dummy-hash branch of
        // authenticate_session, so the source axis is the only thing that
        // can observe and bound this traffic.
        for attempt in 1..=PER_SOURCE_MAX_ATTEMPTS {
            let login = format!("source-axis-ghost-{run_suffix}-{attempt}");
            let before = password_verification_attempts();
            let (status, retry_after, _body) =
                fixture.login(&login, WRONG_PASSWORD, source_ip).await?;
            let after = password_verification_attempts();
            assert_eq!(
                status,
                StatusCode::UNAUTHORIZED,
                "attempt {attempt} (nonexistent login) must still reach the dummy Argon2id hash"
            );
            assert_eq!(
                after - before,
                1,
                "attempt {attempt} must invoke verify_password exactly once"
            );
            assert!(retry_after.is_none());
        }

        let final_login = format!("source-axis-ghost-{run_suffix}-final");
        let before = password_verification_attempts();
        let (status, retry_after, body) =
            fixture.login(&final_login, WRONG_PASSWORD, source_ip).await?;
        let after = password_verification_attempts();
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "the source-axis bucket is exhausted");
        assert_eq!(
            after, before,
            "a source-rate-limited attempt must not invoke verify_password / Argon2id at all"
        );
        let retry_after = retry_after.context("429 response must carry a Retry-After header")?;
        assert!(retry_after.parse::<u64>().is_ok_and(|seconds| seconds > 0));
        assert_eq!(body["code"], json!("login_source_rate_limited"));

        // A different source is completely unaffected.
        let other_login = format!("source-axis-ghost-{run_suffix}-other-source");
        let before = password_verification_attempts();
        let (status, _retry_after, _body) =
            fixture.login(&other_login, WRONG_PASSWORD, "203.0.113.21").await?;
        let after = password_verification_attempts();
        assert_eq!(status, StatusCode::UNAUTHORIZED, "a distinct source has its own bucket");
        assert_eq!(after - before, 1);

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres and redis services, and --features test-support"]
async fn source_axis_rate_limit_ignores_client_controlled_leading_forwarded_hops() -> Result<()> {
    const PER_SOURCE_MAX_ATTEMPTS: u32 = 3;
    let fixture = LoginRateLimitFixture::create(RateLimitTuning {
        per_login_max_attempts: 50,
        per_source_max_attempts: PER_SOURCE_MAX_ATTEMPTS,
        trusted_proxy_hops: 1,
    })
    .await?;

    let result = async {
        let real_client = "203.0.113.30";
        let run_suffix = Uuid::now_v7().simple().to_string();

        // One trusted proxy appends the real client as the LAST entry. The
        // caller forges a fresh leading entry on every attempt; if that entry
        // were honoured, every attempt would land in a brand-new bucket and
        // the source axis would never engage.
        for attempt in 1..=PER_SOURCE_MAX_ATTEMPTS {
            let login = format!("forged-hop-ghost-{run_suffix}-{attempt}");
            let forged = format!("198.51.100.{attempt}, {real_client}");
            let (status, _retry_after, _body) =
                fixture.login(&login, WRONG_PASSWORD, &forged).await?;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "attempt {attempt} is within capacity");
        }

        let final_login = format!("forged-hop-ghost-{run_suffix}-final");
        let forged = format!("198.51.100.250, {real_client}");
        let before = password_verification_attempts();
        let (status, retry_after, body) =
            fixture.login(&final_login, WRONG_PASSWORD, &forged).await?;
        let after = password_verification_attempts();
        assert_eq!(
            status,
            StatusCode::TOO_MANY_REQUESTS,
            "a rotating forged X-Forwarded-For prefix must not escape the source bucket"
        );
        assert_eq!(after, before, "the rejected attempt must not reach Argon2id");
        assert!(
            retry_after.is_some_and(|value| value.parse::<u64>().is_ok_and(|seconds| seconds > 0))
        );
        assert_eq!(body["code"], json!("login_source_rate_limited"));

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}
