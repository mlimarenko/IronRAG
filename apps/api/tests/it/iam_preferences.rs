use crate::{http_response_support::response_json, temp_postgres_database::TempPostgresDatabase};

use anyhow::{Context, Result};
use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
};
use base64::Engine as _;
use chrono::Utc;
use serde_json::{Value, json};
use sqlx::{PgPool, postgres::PgPoolOptions};
use tower::ServiceExt;
use uuid::Uuid;

use ironrag_backend::{
    app::{config::Settings, state::AppState},
    infra::{persistence::Persistence, repositories::iam_repository},
    interfaces::http::{auth::hash_token, router},
    shared::auth_tokens::{
        build_session_cookie_value, hash_session_secret, mint_plaintext_api_token,
        mint_plaintext_session_secret,
    },
};

struct PreferencesFixture {
    state: AppState,
    temp_database: TempPostgresDatabase,
}

impl PreferencesFixture {
    async fn create() -> Result<Self> {
        let mut settings =
            Settings::from_env().context("failed to load settings for iam preferences test")?;
        let temp_database =
            TempPostgresDatabase::create(&settings.database_url, "iam_preferences").await?;
        settings.database_url = temp_database.database_url.clone();
        // AppState construction requires an encryption keyring even though this
        // test never stores a credential.
        settings.credential_master_key =
            Some(base64::engine::general_purpose::STANDARD.encode([37_u8; 32]));

        let postgres = PgPoolOptions::new()
            .max_connections(4)
            .connect(&settings.database_url)
            .await
            .context("failed to connect iam preferences postgres")?;
        sqlx::migrate!("./migrations")
            .run(&postgres)
            .await
            .context("failed to apply iam preferences migrations")?;

        let state = build_test_state(settings, postgres)?;
        Ok(Self { state, temp_database })
    }

    fn app(&self) -> Router {
        Router::new().nest("/v1", router()).with_state(self.state.clone())
    }

    const fn pool(&self) -> &PgPool {
        &self.state.persistence.postgres
    }

    async fn cleanup(self) -> Result<()> {
        self.state.persistence.postgres.close().await;
        self.temp_database.drop().await
    }

    /// Creates a real user and returns its browser session cookie.
    async fn sign_in(&self, label: &str) -> Result<(Uuid, String)> {
        let suffix = Uuid::now_v7().simple();
        let user = iam_repository::create_user(
            self.pool(),
            &format!("{label}-{suffix}"),
            &format!("{label}-{suffix}@example.invalid"),
            "Preferences Test",
            &hash_token(&format!("password-{suffix}")),
            iam_repository::SystemRole::Viewer,
        )
        .await
        .with_context(|| format!("failed to create user for {label}"))?;
        let secret = mint_plaintext_session_secret();
        let session = iam_repository::create_session(
            self.pool(),
            user.principal_id,
            &hash_session_secret(&secret),
            Utc::now() + chrono::Duration::hours(1),
        )
        .await
        .with_context(|| format!("failed to create session for {label}"))?;
        Ok((
            user.principal_id,
            format!(
                "{}={}",
                self.state.ui_session_cookie.name,
                build_session_cookie_value(session.id, &secret)
            ),
        ))
    }

    async fn send(
        &self,
        method: &str,
        path: &str,
        credential: Option<(header::HeaderName, String)>,
        payload: Option<Value>,
    ) -> Result<(StatusCode, Value)> {
        let mut builder = Request::builder().method(method).uri(path);
        if let Some((name, value)) = credential {
            builder = builder.header(name, value);
        }
        let body = match payload {
            Some(payload) => {
                builder = builder.header(header::CONTENT_TYPE, "application/json");
                Body::from(payload.to_string())
            }
            None => Body::empty(),
        };
        let response = self
            .app()
            .oneshot(builder.body(body).context("failed to build iam preferences request")?)
            .await
            .with_context(|| format!("{method} {path} failed"))?;
        let status = response.status();
        if status.is_client_error()
            && response.headers().get(header::CONTENT_TYPE).is_some_and(|value| {
                value.to_str().is_ok_and(|value| value.starts_with("text/plain"))
            })
        {
            // Axum's JSON extractor rejects malformed bodies with plain text.
            return Ok((status, Value::Null));
        }
        Ok((status, response_json(response).await?))
    }

    fn cookie(session_cookie: &str) -> (header::HeaderName, String) {
        (header::COOKIE, session_cookie.to_string())
    }
}

fn build_test_state(settings: Settings, postgres: PgPool) -> Result<AppState> {
    let redis = redis::Client::open(settings.redis_url.clone())
        .context("failed to create redis client for iam preferences state")?;
    let persistence = Persistence::for_tests(postgres, redis);
    AppState::from_dependencies(settings, persistence)
}

#[tokio::test]
#[ignore = "requires local postgres and redis services; uses an isolated disposable database"]
async fn preferences_are_account_scoped_and_require_a_browser_session() -> Result<()> {
    let fixture = PreferencesFixture::create().await?;

    let result = async {
        let (principal_a, cookie_a) = fixture.sign_in("preferences-a").await?;
        let (principal_b, cookie_b) = fixture.sign_in("preferences-b").await?;

        let (status, body) = fixture
            .send("GET", "/v1/iam/preferences", Some(PreferencesFixture::cookie(&cookie_a)), None)
            .await?;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, json!({"telemetryConsent": null}));

        let (status, body) = fixture
            .send(
                "PUT",
                "/v1/iam/preferences",
                Some(PreferencesFixture::cookie(&cookie_a)),
                Some(json!({"expectedPrincipalId": principal_a, "telemetryConsent": "granted"})),
            )
            .await?;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, json!({"telemetryConsent": "granted"}));

        let (status, body) = fixture
            .send("GET", "/v1/iam/preferences", Some(PreferencesFixture::cookie(&cookie_b)), None)
            .await?;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, json!({"telemetryConsent": null}), "one principal must not read another");

        let (status, _) = fixture
            .send(
                "POST",
                "/v1/iam/preferences/seed",
                Some(PreferencesFixture::cookie(&cookie_b)),
                Some(json!({"expectedPrincipalId": principal_a, "telemetryConsent": "granted"})),
            )
            .await?;
        assert_eq!(status, StatusCode::CONFLICT);
        let (status, body) = fixture
            .send("GET", "/v1/iam/preferences", Some(PreferencesFixture::cookie(&cookie_b)), None)
            .await?;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            body,
            json!({"telemetryConsent": null}),
            "mismatched seed must not create a row"
        );

        let (status, body) = fixture
            .send(
                "PUT",
                "/v1/iam/preferences",
                Some(PreferencesFixture::cookie(&cookie_b)),
                Some(json!({"expectedPrincipalId": principal_b, "telemetryConsent": "denied"})),
            )
            .await?;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, json!({"telemetryConsent": "denied"}));

        let (status, body) = fixture
            .send("GET", "/v1/iam/preferences", Some(PreferencesFixture::cookie(&cookie_a)), None)
            .await?;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            body,
            json!({"telemetryConsent": "granted"}),
            "one principal must not write another"
        );

        // Seeding is one-way: an existing account choice is never overwritten.
        let (status, body) = fixture
            .send(
                "POST",
                "/v1/iam/preferences/seed",
                Some(PreferencesFixture::cookie(&cookie_a)),
                Some(json!({"expectedPrincipalId": principal_a, "telemetryConsent": "denied"})),
            )
            .await?;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, json!({"telemetryConsent": "granted"}));
        let (status, body) = fixture
            .send(
                "POST",
                "/v1/iam/preferences/seed",
                Some(PreferencesFixture::cookie(&cookie_b)),
                Some(json!({"expectedPrincipalId": principal_b, "telemetryConsent": "granted"})),
            )
            .await?;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, json!({"telemetryConsent": "denied"}));

        for (method, path) in [("PUT", "/v1/iam/preferences"), ("POST", "/v1/iam/preferences/seed")]
        {
            let (status, _) = fixture
                .send(
                    method,
                    path,
                    Some(PreferencesFixture::cookie(&cookie_b)),
                    Some(
                        json!({"expectedPrincipalId": principal_a, "telemetryConsent": "granted"}),
                    ),
                )
                .await?;
            assert_eq!(status, StatusCode::CONFLICT, "stale account must not mutate B");
            for payload in [
                json!({"telemetryConsent": "granted"}),
                json!({"expectedPrincipalId": "invalid", "telemetryConsent": "granted"}),
            ] {
                let (status, _) = fixture
                    .send(method, path, Some(PreferencesFixture::cookie(&cookie_b)), Some(payload))
                    .await?;
                assert!(status.is_client_error(), "missing or invalid principal must be rejected");
            }
            let (status, _) = fixture
                .send(
                    method,
                    path,
                    None,
                    Some(
                        json!({"expectedPrincipalId": principal_b, "telemetryConsent": "granted"}),
                    ),
                )
                .await?;
            assert_eq!(status, StatusCode::UNAUTHORIZED);
        }
        let (status, body) = fixture
            .send("GET", "/v1/iam/preferences", Some(PreferencesFixture::cookie(&cookie_b)), None)
            .await?;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, json!({"telemetryConsent": "denied"}));

        // A different session for the same principal is still a valid precondition.
        let secret = mint_plaintext_session_secret();
        let session = iam_repository::create_session(
            fixture.pool(),
            principal_b,
            &hash_session_secret(&secret),
            Utc::now() + chrono::Duration::hours(1),
        )
        .await?;
        let second_cookie = format!(
            "{}={}",
            fixture.state.ui_session_cookie.name,
            build_session_cookie_value(session.id, &secret)
        );
        let (status, body) = fixture
            .send(
                "PUT",
                "/v1/iam/preferences",
                Some(PreferencesFixture::cookie(&second_cookie)),
                Some(json!({"expectedPrincipalId": principal_b, "telemetryConsent": "denied"})),
            )
            .await?;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, json!({"telemetryConsent": "denied"}));

        let (status, _) = fixture.send("GET", "/v1/iam/preferences", None, None).await?;
        assert_eq!(status, StatusCode::UNAUTHORIZED);

        let plaintext = mint_plaintext_api_token();
        let token = iam_repository::create_api_token(
            fixture.pool(),
            None,
            "preferences-bearer",
            "pref",
            None,
            None,
        )
        .await
        .context("failed to create bearer token principal")?;
        iam_repository::create_api_token_secret(
            fixture.pool(),
            token.principal_id,
            &hash_token(&plaintext),
        )
        .await
        .context("failed to create bearer token secret")?;
        let (status, _) = fixture
            .send(
                "GET",
                "/v1/iam/preferences",
                Some((header::AUTHORIZATION, format!("Bearer {plaintext}"))),
                None,
            )
            .await?;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "a bearer token is not a browser session");
        for (method, path) in [("PUT", "/v1/iam/preferences"), ("POST", "/v1/iam/preferences/seed")]
        {
            let (status, _) = fixture
                .send(
                    method,
                    path,
                    Some((header::AUTHORIZATION, format!("Bearer {plaintext}"))),
                    Some(
                        json!({"expectedPrincipalId": principal_b, "telemetryConsent": "granted"}),
                    ),
                )
                .await?;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "mutations require a browser session");
        }
        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}
