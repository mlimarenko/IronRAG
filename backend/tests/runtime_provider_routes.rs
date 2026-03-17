use anyhow::Context;
use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

use rustrag_backend::{
    app::{config::Settings, state::AppState},
    infra::repositories,
    integrations::provider_catalog::{
        ROLE_ANSWER, ROLE_EMBEDDING, ROLE_INDEXING, ROLE_VISION, supported_provider_catalog,
    },
    interfaces::http::{auth::hash_token, router},
};

struct RuntimeProviderFixture {
    state: AppState,
    workspace_id: Uuid,
    library_id: Uuid,
}

impl RuntimeProviderFixture {
    async fn create(mut settings: Settings) -> anyhow::Result<Self> {
        settings.runtime_live_validation_enabled = true;
        let state = AppState::new(settings).await?;
        let suffix = Uuid::now_v7().simple().to_string();
        let workspace = repositories::create_workspace(
            &state.persistence.postgres,
            &format!("runtime-provider-test-{suffix}"),
            "Runtime Provider Test",
        )
        .await
        .context("failed to create runtime provider test workspace")?;
        let library = repositories::create_project(
            &state.persistence.postgres,
            workspace.id,
            &format!("runtime-provider-library-{suffix}"),
            "Runtime Provider Library",
            Some("runtime provider route test fixture"),
        )
        .await
        .context("failed to create runtime provider test library")?;

        Ok(Self { state, workspace_id: workspace.id, library_id: library.id })
    }

    async fn cleanup(&self) -> anyhow::Result<()> {
        sqlx::query("delete from workspace where id = $1")
            .bind(self.workspace_id)
            .execute(&self.state.persistence.postgres)
            .await
            .context("failed to delete runtime provider test workspace")?;
        Ok(())
    }

    fn app(&self) -> Router {
        Router::new().nest("/v1", router()).with_state(self.state.clone())
    }

    async fn bearer_token(&self, scopes: &[&str], label: &str) -> anyhow::Result<String> {
        let plaintext = format!("test-{}-{}", label, Uuid::now_v7());
        repositories::create_api_token(
            &self.state.persistence.postgres,
            Some(self.workspace_id),
            "workspace",
            label,
            &hash_token(&plaintext),
            Some("test-token"),
            json!(scopes),
            None,
        )
        .await
        .with_context(|| format!("failed to create api token for {label}"))?;
        Ok(plaintext)
    }
}

async fn response_json(response: axum::response::Response) -> anyhow::Result<Value> {
    let bytes =
        response.into_body().collect().await.context("failed to collect response body")?.to_bytes();
    if bytes.is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_slice(&bytes).context("failed to decode response json")
}

fn select_profile_payload(state: &AppState) -> Value {
    let catalog = supported_provider_catalog(&state.settings, &state.runtime_provider_defaults);
    let openai = catalog
        .into_iter()
        .find(|entry| entry.provider_kind.as_str() == "openai" && entry.is_configured)
        .expect("openai must be configured for runtime provider tests");

    let pick = |role: &str| {
        openai
            .available_models
            .get(role)
            .and_then(|models| models.last())
            .cloned()
            .or_else(|| openai.default_models.get(role).cloned())
            .expect("role model available")
    };

    json!({
        "indexingProviderKind": "openai",
        "indexingModelName": pick(ROLE_INDEXING),
        "embeddingProviderKind": "openai",
        "embeddingModelName": pick(ROLE_EMBEDDING),
        "answerProviderKind": "openai",
        "answerModelName": pick(ROLE_ANSWER),
        "visionProviderKind": "openai",
        "visionModelName": pick(ROLE_VISION),
    })
}

async fn provider_validation_log_count(pool: &PgPool, library_id: Uuid) -> anyhow::Result<i64> {
    let count = sqlx::query_scalar::<_, i64>(
        "select count(*) from runtime_provider_validation_log where project_id = $1",
    )
    .bind(library_id)
    .fetch_one(pool)
    .await
    .context("failed to count provider validation logs")?;
    Ok(count)
}

#[tokio::test]
#[ignore = "requires local postgres, redis, and neo4j services"]
async fn runtime_provider_profile_update_persists_via_route() -> anyhow::Result<()> {
    let settings =
        Settings::from_env().context("failed to load settings for runtime provider test")?;
    let fixture = RuntimeProviderFixture::create(settings).await?;

    let result = async {
        let token = fixture.bearer_token(&["providers:admin"], "provider-profile-update").await?;
        let payload = select_profile_payload(&fixture.state);

        let response = fixture
            .app()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri(format!("/v1/runtime/libraries/{}/provider-profile", fixture.library_id))
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(payload.to_string()))
                    .expect("build provider update request"),
            )
            .await
            .context("provider profile update route failed")?;

        assert_eq!(response.status(), StatusCode::OK);
        let body = response_json(response).await?;
        assert_eq!(body["libraryId"], fixture.library_id.to_string());
        assert_eq!(body["indexingProviderKind"], payload["indexingProviderKind"]);
        assert_eq!(body["indexingModelName"], payload["indexingModelName"]);
        assert_eq!(body["embeddingModelName"], payload["embeddingModelName"]);
        assert_eq!(body["answerModelName"], payload["answerModelName"]);
        assert_eq!(body["visionModelName"], payload["visionModelName"]);

        let persisted = repositories::get_runtime_provider_profile(
            &fixture.state.persistence.postgres,
            fixture.library_id,
        )
        .await
        .context("failed to reload runtime provider profile")?
        .context("missing persisted runtime provider profile")?;

        assert_eq!(persisted.indexing_provider_kind, payload["indexingProviderKind"]);
        assert_eq!(persisted.indexing_model_name, payload["indexingModelName"]);
        assert_eq!(persisted.embedding_model_name, payload["embeddingModelName"]);
        assert_eq!(persisted.answer_model_name, payload["answerModelName"]);
        assert_eq!(persisted.vision_model_name, payload["visionModelName"]);

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres, redis, and neo4j services"]
async fn runtime_provider_validation_route_records_failed_check_without_external_call()
-> anyhow::Result<()> {
    let settings = Settings::from_env()
        .context("failed to load settings for runtime provider validation test")?;
    let fixture = RuntimeProviderFixture::create(settings).await?;

    let result = async {
        let token = fixture.bearer_token(&["providers:admin"], "provider-validation").await?;
        let before =
            provider_validation_log_count(&fixture.state.persistence.postgres, fixture.library_id)
                .await?;

        let response = fixture
            .app()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/runtime/providers/validate")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "libraryId": fixture.library_id,
                            "providerKind": "openai",
                            "modelName": "not-a-real-openai-model",
                            "capability": "chat",
                        })
                        .to_string(),
                    ))
                    .expect("build provider validation request"),
            )
            .await
            .context("provider validation route failed")?;

        assert_eq!(response.status(), StatusCode::OK);
        let body = response_json(response).await?;
        assert_eq!(body["providerKind"], "openai");
        assert_eq!(body["capability"], "chat");
        assert_eq!(body["status"], "failed");
        let error = body["error"].as_str().unwrap_or_default();
        assert!(error.contains("not-a-real-openai-model"));

        let after =
            provider_validation_log_count(&fixture.state.persistence.postgres, fixture.library_id)
                .await?;
        assert_eq!(after, before + 1);

        let profile = repositories::get_runtime_provider_profile(
            &fixture.state.persistence.postgres,
            fixture.library_id,
        )
        .await
        .context("failed to reload provider profile after validation")?
        .context("missing provider profile after validation")?;
        assert_eq!(profile.last_validation_status.as_deref(), Some("failed"));
        assert!(
            profile
                .last_validation_error
                .as_deref()
                .is_some_and(|value| value.contains("not-a-real-openai-model"))
        );

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres, redis, and neo4j services"]
async fn runtime_routes_enforce_scopes_for_provider_graph_and_query_paths() -> anyhow::Result<()> {
    let settings =
        Settings::from_env().context("failed to load settings for runtime scope test")?;
    let fixture = RuntimeProviderFixture::create(settings).await?;

    let result = async {
        let providers_token = fixture.bearer_token(&["providers:admin"], "providers-scope").await?;
        let graph_token = fixture.bearer_token(&["graph:read"], "graph-scope").await?;

        let provider_response = fixture
            .app()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/v1/runtime/libraries/{}/provider-profile", fixture.library_id))
                    .header(header::AUTHORIZATION, format!("Bearer {providers_token}"))
                    .body(Body::empty())
                    .expect("build provider profile request"),
            )
            .await
            .context("provider profile route failed")?;
        assert_eq!(provider_response.status(), StatusCode::OK);

        let forbidden_provider_response = fixture
            .app()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/v1/runtime/libraries/{}/provider-profile", fixture.library_id))
                    .header(header::AUTHORIZATION, format!("Bearer {graph_token}"))
                    .body(Body::empty())
                    .expect("build unauthorized provider profile request"),
            )
            .await
            .context("unauthorized provider profile route failed")?;
        assert_eq!(forbidden_provider_response.status(), StatusCode::UNAUTHORIZED);

        let graph_response = fixture
            .app()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/v1/runtime/libraries/{}/graph/surface", fixture.library_id))
                    .header(header::AUTHORIZATION, format!("Bearer {graph_token}"))
                    .body(Body::empty())
                    .expect("build graph surface request"),
            )
            .await
            .context("graph surface route failed")?;
        assert_eq!(graph_response.status(), StatusCode::OK);

        let query_response = fixture
            .app()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/v1/runtime/libraries/{}/queries/answer", fixture.library_id))
                    .header(header::AUTHORIZATION, format!("Bearer {graph_token}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "question": "What is in this graph?",
                            "mode": "hybrid",
                        })
                        .to_string(),
                    ))
                    .expect("build unauthorized query request"),
            )
            .await
            .context("query route failed")?;
        assert_eq!(query_response.status(), StatusCode::UNAUTHORIZED);

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}
