//! IRG-182 service-level coverage for the decision-catalog contract.
//!
//! The unit gate lives in `services::ai_catalog_service::tests::decision_eligibility`;
//! these cases drive the real service write paths (`create_binding`,
//! `update_binding`, `create_model_catalog`, `update_model_catalog`) against a
//! freshly migrated and seeded database, because that is where a create-time
//! state override and the provider/model purpose contract actually meet
//! PostgreSQL. Both decision purposes are activatable — `query_rerank` since
//! IRG-187, `graph_entity_type` since IRG-185; a decision provider's models may
//! never allow a generative purpose; a chat provider stays bindable on a
//! decision purpose.
//!
//! IRG-153 also exercises binding JSONB save/read, server-owned model reasoning
//! projection and the real gateway payload against a local synthetic provider.

use anyhow::{Context, Result};
use rust_decimal::Decimal;
use sqlx::{PgPool, postgres::PgPoolOptions};
use uuid::Uuid;

use ironrag_backend::{
    app::{config::Settings, seed_clean_v1_ai_catalog, state::AppState},
    domains::ai::{AiBindingPurpose, AiScopeKind, ModelPricing, ProviderCatalogEntry},
    infra::{persistence::Persistence, repositories::ai_repository},
    integrations::llm::{LlmGateway, UnifiedGateway, build_text_chat_request},
    interfaces::http::router_support::ApiError,
    services::ai_catalog_service::{
        CreateAiBindingCommand, CreateModelCatalogCommand, CreateProviderCatalogCommand,
        UpdateAiBindingCommand, UpdateModelCatalogCommand,
    },
};

use crate::temp_postgres_database::TempPostgresDatabase;

struct DecisionCatalogFixture {
    state: AppState,
    temp_database: TempPostgresDatabase,
}

fn assert_reasoning_independent_embedding_profile(
    resolved: &ironrag_backend::services::ai_catalog_service::ResolvedRuntimeBinding,
) {
    let mut no_reasoning = resolved.clone();
    let parameters = no_reasoning.extra_parameters_json.as_object_mut().expect("object");
    parameters.remove("reasoningMode");
    parameters.remove("_providerModelReasoning");
    assert_eq!(
        resolved.embedding_execution_profile_key(),
        no_reasoning.embedding_execution_profile_key(),
        "chat reasoning metadata must not change the embedding fingerprint"
    );
}

fn assert_selected_reasoning_payload(
    body: &serde_json::Value,
    wire: &str,
    extra: &serde_json::Value,
) {
    let (parameter, value, absent) = if wire == "thinking_disabled" {
        ("thinking", serde_json::json!({"type": "disabled"}), "reasoning_effort")
    } else {
        ("reasoning_effort", serde_json::json!("none"), "thinking")
    };
    assert_eq!(body[parameter], value);
    assert!(body.get(absent).is_none());
    assert_eq!(body["vendor_extension"], extra["vendor_extension"]);
    assert!(body.get("reasoningMode").is_none());
    assert!(body.as_object().expect("body").keys().all(|key| !key.starts_with("_provider")));
}

// Synthetic catalog evidence tests our contract, not vendor/model acceptance.
async fn store_reasoning_fixture(
    fixture: &DecisionCatalogFixture,
    provider_id: Uuid,
    purpose: AiBindingPurpose,
    wire: &str,
) -> Result<ironrag_backend::domains::ai::AiBinding> {
    let state = &fixture.state;
    let ai = &state.application_services.ai_catalog;
    let model = ai
        .create_model_catalog(
            state,
            model_command(provider_id, format!("synthetic-{wire}"), vec![purpose]),
        )
        .await?;
    let account_id = create_active_account(fixture.pool(), provider_id, wire).await?;
    let confirmation = serde_json::json!({"confirmed": {
        "mode": "none", "wire": wire, "evidence": "synthetic isolated catalog fixture"
    }});
    let extra = serde_json::json!({
        "reasoningMode": {"version": 1, "mode": "none"},
        "vendor_extension": {"keep": [1, "foreign"]},
        "_providerModelReasoning": {"modelName": "forged-model", "capability": "unconfirmed"},
        "_providerProfile": {"requestPolicy": {"reasoningCapability": confirmation}}
    });
    let mut command = binding_command(purpose, account_id, model.id, None);
    command.extra_parameters_json = extra.clone();
    let refusal = expect_bad_request(
        ai.create_binding(state, command.clone())
            .await
            .expect_err("binding JSON cannot self-grant an unconfirmed pair"),
    );
    assert!(refusal.contains("not confirmed"), "{refusal}");

    ai.update_model_catalog(
        state,
        UpdateModelCatalogCommand {
            model_id: model.id,
            lifecycle_state: "active".to_string(),
            allowed_binding_purposes: vec![purpose],
            context_window: None,
            max_output_tokens: None,
            metadata_json: Some(serde_json::json!({"reasoningCapability": confirmation})),
            pricing: zero_pricing(),
        },
    )
    .await?;
    for raw in [
        serde_json::json!({"reasoning_effort": "high"}),
        serde_json::json!({"thinking": {"type": "enabled"}}),
    ] {
        let mut conflicting = command.clone();
        conflicting
            .extra_parameters_json
            .as_object_mut()
            .expect("object")
            .extend(raw.as_object().expect("object").clone());
        let refusal = expect_bad_request(
            ai.create_binding(state, conflicting)
                .await
                .expect_err("either conflicting caller switch must be a 4xx"),
        );
        assert!(refusal.contains("raw `"), "{refusal}");
    }
    ai.create_binding(state, command).await.map_err(Into::into)
}

async fn reasoning_roundtrip_case(
    fixture: &DecisionCatalogFixture,
    provider_id: Uuid,
    gateway: &UnifiedGateway,
    bodies: &mut tokio::sync::mpsc::Receiver<serde_json::Value>,
    purpose: AiBindingPurpose,
    wire: &str,
) -> Result<()> {
    let state = &fixture.state;
    let ai = &state.application_services.ai_catalog;
    let stored = store_reasoning_fixture(fixture, provider_id, purpose, wire).await?;
    let extra = stored.extra_parameters_json.clone();
    let confirmation = serde_json::json!({"confirmed": {
        "mode": "none", "wire": wire, "evidence": "synthetic isolated catalog fixture"
    }});
    let read = ai_repository::get_binding_by_id(fixture.pool(), stored.id)
        .await?
        .context("saved binding must be readable from PostgreSQL")?;
    assert_eq!(read.extra_parameters_json, extra, "JSONB roundtrip preserves foreign JSON");
    let resolved = ai.resolve_runtime_binding_by_id(state, stored.id).await?;
    assert_eq!(
        resolved.extra_parameters_json["_providerModelReasoning"]["capability"],
        confirmation
    );
    assert!(
        resolved.extra_parameters_json["_providerProfile"]["runtime"].is_object(),
        "server profile must replace the forged client profile"
    );
    assert_reasoning_independent_embedding_profile(&resolved);
    let body = capture_reasoning_payload(gateway, bodies, &resolved).await?;
    assert_selected_reasoning_payload(&body, wire, &extra);

    // Updating to unset retains the existing raw behaviour, distinct from typed none.
    let unset = serde_json::json!({"reasoning_effort": "high", "vendor_extension": extra["vendor_extension"]});
    ai.update_binding(
        state,
        UpdateAiBindingCommand {
            binding_id: stored.id,
            account_id: stored.account_id,
            model_catalog_id: stored.model_catalog_id,
            system_prompt: None,
            temperature: None,
            top_p: None,
            max_output_tokens_override: None,
            extra_parameters_json: unset,
            binding_state: "active".to_string(),
            updated_by_principal_id: None,
        },
    )
    .await?;
    let resolved = ai.resolve_runtime_binding_by_id(state, stored.id).await?;
    assert!(resolved.extra_parameters_json.get("reasoningMode").is_none());
    let body = capture_reasoning_payload(gateway, bodies, &resolved).await?;
    assert_eq!(body["reasoning_effort"], "high");
    assert!(body.get("thinking").is_none());
    Ok(())
}

async fn capture_reasoning_payload(
    gateway: &UnifiedGateway,
    bodies: &mut tokio::sync::mpsc::Receiver<serde_json::Value>,
    resolved: &ironrag_backend::services::ai_catalog_service::ResolvedRuntimeBinding,
) -> Result<serde_json::Value> {
    gateway
        .generate(build_text_chat_request(resolved.chat_request_seed(), "fixture".to_string()))
        .await?;
    tokio::time::timeout(std::time::Duration::from_secs(5), bodies.recv())
        .await?
        .context("mock provider must receive the real payload")
}

#[tokio::test]
#[ignore = "requires isolated postgres service; provider HTTP is local and synthetic"]
async fn selected_reasoning_binding_roundtrips_to_real_gateway_payload() -> Result<()> {
    let fixture = DecisionCatalogFixture::create().await?;
    // Private-provider loopback URLs target the Docker host, not this test's
    // container. Use this process's routed interface; UDP connect sends no packet.
    let route_probe = std::net::UdpSocket::bind("0.0.0.0:0")?;
    route_probe.connect("192.0.2.1:9")?;
    let host = route_probe.local_addr()?.ip();
    anyhow::ensure!(!host.is_loopback() && !host.is_unspecified(), "fixture needs a routed IP");
    let listener = tokio::net::TcpListener::bind((host, 0)).await?;
    let base_url = format!("http://{}", listener.local_addr()?);
    assert_eq!(
        ironrag_backend::shared::provider_base_url::resolve_runtime_provider_base_url(
            true, &base_url
        ),
        base_url,
        "provider transport must not redirect the in-process fixture to a Docker host"
    );
    let (sender, mut bodies) = tokio::sync::mpsc::channel(4);
    let discovery_calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let discovery_counter = discovery_calls.clone();
    let app = axum::Router::new().route("/models", axum::routing::get(move || {
        let counter = discovery_counter.clone();
        async move {
            counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            axum::Json(serde_json::json!({"data": [{"id": "synthetic-reasoning_effort_none"}]}))
        }
    })).route("/chat/completions", axum::routing::post(
        move |axum::Json(body): axum::Json<serde_json::Value>| {
            let sender = sender.clone();
            async move {
                sender.send(body).await.expect("payload receiver stays alive");
                axum::Json(serde_json::json!({"choices": [{"message": {"role": "assistant", "content": "ok"}}]}))
            }
        }
    ));
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let result = async {
        let state = &fixture.state;
        let ai = &state.application_services.ai_catalog;
        let template = ai
            .list_provider_catalog(state)
            .await?
            .into_iter()
            .find(|provider| provider.provider_kind == "ollama")
            .context("seed must contain the credential-free OpenAI-compatible provider")?;
        let mut profile = template.profile;
        profile.runtime.chat_path = "/chat/completions".to_string();
        profile.base_url.require_https = false;
        profile.base_url.allow_private_network = true;
        profile.credentials.api_key_required = false;
        profile.model_discovery = serde_json::from_value(serde_json::json!({
            "mode": "shared", "paths": [{"capabilityKind": "chat", "path": "/models"}]
        }))?;
        let provider = ai
            .create_provider_catalog(
                state,
                CreateProviderCatalogCommand {
                    provider_kind: "synthetic-reasoning-fixture".to_string(),
                    display_name: "Synthetic reasoning fixture".to_string(),
                    api_style: "openai_compatible".to_string(),
                    lifecycle_state: "active".to_string(),
                    default_base_url: Some(base_url),
                    capability_flags_json: serde_json::to_value(profile)?,
                },
            )
            .await?;
        let gateway = UnifiedGateway::from_settings(&state.settings)?;
        for (purpose, wire) in [
            (AiBindingPurpose::QueryAnswer, "reasoning_effort_none"),
            (AiBindingPurpose::QueryCompile, "thinking_disabled"),
        ] {
            reasoning_roundtrip_case(&fixture, provider.id, &gateway, &mut bodies, purpose, wire)
                .await?;
        }
        let account_id = create_active_account(fixture.pool(), provider.id, "discovery").await?;
        let discovered = ai.list_available_provider_models(state, account_id).await?;
        assert!(discovered.iter().any(|entry| entry.discovered.model_name == "synthetic-reasoning_effort_none"));
        let calls_before = discovery_calls.load(std::sync::atomic::Ordering::SeqCst);
        assert!(calls_before >= 1, "explicit discovery must reach the synthetic HTTP endpoint");
        let before: serde_json::Value = sqlx::query_scalar(
            "select jsonb_agg(to_jsonb(m) order by id) from ai_model_catalog m where provider_catalog_id=$1",
        ).bind(provider.id).fetch_one(fixture.pool()).await?;
        let listed = ai.list_resolved_model_catalog(state, Some(provider.id), None, None, Some(account_id)).await?;
        assert!(!listed.is_empty());
        for row in listed {
            let resolved = ai.get_resolved_model_catalog(state, row.model.id).await?;
            assert!(resolved.available_account_ids.contains(&account_id));
        }
        let after: serde_json::Value = sqlx::query_scalar(
            "select jsonb_agg(to_jsonb(m) order by id) from ai_model_catalog m where provider_catalog_id=$1",
        ).bind(provider.id).fetch_one(fixture.pool()).await?;
        assert_eq!(after, before, "resolved reads must not rewrite the persisted model catalog");
        assert_eq!(discovery_calls.load(std::sync::atomic::Ordering::SeqCst), calls_before,
            "resolved get/list must not perform extra HTTP discovery");
        Ok(())
    }
    .await;
    server.abort();
    fixture.cleanup().await?;
    result
}

impl DecisionCatalogFixture {
    async fn create() -> Result<Self> {
        let mut settings =
            Settings::from_env().context("failed to load settings for ai_catalog_service test")?;
        let temp_database =
            TempPostgresDatabase::create(&settings.database_url, "ai_catalog_service").await?;
        settings.database_url = temp_database.database_url.clone();
        let postgres = PgPoolOptions::new()
            .max_connections(4)
            .connect(&settings.database_url)
            .await
            .context("failed to connect ai_catalog_service postgres")?;
        sqlx::migrate!("./migrations")
            .run(&postgres)
            .await
            .context("failed to apply migrations for ai_catalog_service test")?;
        seed_clean_v1_ai_catalog(&postgres)
            .await
            .context("failed to seed ai_catalog_service catalog")?;

        let redis = redis::Client::open(settings.redis_url.clone())
            .context("failed to create redis client for ai_catalog_service test")?;
        let persistence = Persistence::for_tests(postgres, redis);
        settings.credential_master_key =
            Some("KioqKioqKioqKioqKioqKioqKioqKioqKioqKioqKio=".to_string());
        let state = AppState::from_dependencies(settings, persistence)?;
        Ok(Self { state, temp_database })
    }

    fn pool(&self) -> &PgPool {
        &self.state.persistence.postgres
    }

    async fn cleanup(self) -> Result<()> {
        self.state.persistence.postgres.close().await;
        self.temp_database.drop().await
    }
}

fn zero_pricing() -> ModelPricing {
    ModelPricing {
        currency_code: "USD".to_string(),
        input_price_per_1m: Decimal::ZERO,
        cached_input_price_per_1m: Decimal::ZERO,
        cache_write_input_price_per_1m: Decimal::ZERO,
        output_price_per_1m: Decimal::ZERO,
        long_context: None,
    }
}

fn expect_bad_request(error: ApiError) -> String {
    match error {
        ApiError::BadRequest(message) => message,
        other => panic!("the catalog write gate must reject with BadRequest, got {other:?}"),
    }
}

fn binding_command(
    purpose: AiBindingPurpose,
    account_id: Uuid,
    model_catalog_id: Uuid,
    binding_state: Option<&str>,
) -> CreateAiBindingCommand {
    CreateAiBindingCommand {
        scope_kind: AiScopeKind::Instance,
        workspace_id: None,
        library_id: None,
        binding_purpose: purpose,
        account_id,
        model_catalog_id,
        system_prompt: None,
        temperature: None,
        top_p: None,
        max_output_tokens_override: None,
        extra_parameters_json: serde_json::json!({}),
        binding_state: binding_state.map(str::to_string),
        updated_by_principal_id: None,
    }
}

fn model_command(
    provider_catalog_id: Uuid,
    model_name: String,
    purposes: Vec<AiBindingPurpose>,
) -> CreateModelCatalogCommand {
    CreateModelCatalogCommand {
        provider_catalog_id,
        model_name,
        lifecycle_state: "active".to_string(),
        allowed_binding_purposes: purposes,
        context_window: None,
        max_output_tokens: None,
        metadata_json: serde_json::json!({}),
        pricing: zero_pricing(),
    }
}

struct CatalogTargets {
    decision_provider: ProviderCatalogEntry,
    decision_model_id: Uuid,
    decision_account_id: Uuid,
    generative_model_id: Uuid,
    generative_account_id: Uuid,
}

async fn seed_targets(fixture: &DecisionCatalogFixture) -> Result<CatalogTargets> {
    let ai = &fixture.state.application_services.ai_catalog;
    let state = &fixture.state;
    let providers = ai.list_provider_catalog(state).await.context("list provider catalog")?;
    let decision_provider = providers
        .iter()
        .find(|provider| provider.capabilities.decision.is_supported())
        .context("the seed must ship a TypeSafe decision provider")?
        .clone();
    let decision_model = ai
        .list_model_catalog(state, Some(decision_provider.id))
        .await
        .context("list decision provider models")?
        .into_iter()
        .find(|model| {
            model.allowed_binding_purposes.contains(&AiBindingPurpose::QueryRerank)
                && model.allowed_binding_purposes.contains(&AiBindingPurpose::GraphEntityType)
        })
        .context("the seed must ship a decision model allowing both decision purposes")?;

    let generative_provider = providers
        .iter()
        .find(|provider| {
            provider.capabilities.chat.is_supported()
                && !provider.capabilities.decision.is_supported()
        })
        .context("the seed must ship a generative chat provider")?
        .clone();
    let generative_model = ai
        .create_model_catalog(
            state,
            model_command(
                generative_provider.id,
                format!("chat-decision-{}", Uuid::now_v7().simple()),
                vec![AiBindingPurpose::GraphEntityType],
            ),
        )
        .await
        .context("a chat provider must accept a decision-purpose model")?;

    Ok(CatalogTargets {
        decision_account_id: create_active_account(
            fixture.pool(),
            decision_provider.id,
            "decision",
        )
        .await?,
        decision_model_id: decision_model.id,
        decision_provider,
        generative_model_id: generative_model.id,
        generative_account_id: create_active_account(
            fixture.pool(),
            generative_provider.id,
            "generative",
        )
        .await?,
    })
}

async fn create_active_account(
    pool: &PgPool,
    provider_catalog_id: Uuid,
    label: &str,
) -> Result<Uuid> {
    let account_id = Uuid::now_v7();
    ai_repository::create_account(
        pool,
        ai_repository::NewAiAccount {
            account_id,
            scope_kind: "instance",
            workspace_id: None,
            library_id: None,
            provider_catalog_id,
            label: &format!("IRG-182 {label} account"),
            base_url: None,
            created_by_principal_id: None,
        },
        None,
    )
    .await
    .context("create active provider account")?;
    Ok(account_id)
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn decision_binding_is_created_disabled_and_query_rerank_can_activate() -> Result<()> {
    let fixture = DecisionCatalogFixture::create().await?;
    let result = async {
        let targets = seed_targets(&fixture).await?;
        let ai = &fixture.state.application_services.ai_catalog;
        let mut stored_query_rerank_id = None;
        let mut stored_graph_entity_type_id = None;
        for purpose in [AiBindingPurpose::QueryRerank, AiBindingPurpose::GraphEntityType] {
            let stored = ai
                .create_binding(
                    &fixture.state,
                    binding_command(
                        purpose,
                        targets.decision_account_id,
                        targets.decision_model_id,
                        Some("disabled"),
                    ),
                )
                .await
                .with_context(|| format!("store a disabled {} binding", purpose.as_str()))?;
            assert_eq!(
                stored.binding_state, "disabled",
                "the create-time state must be persisted, not hardcoded active"
            );
            match purpose {
                AiBindingPurpose::QueryRerank => stored_query_rerank_id = Some(stored.id),
                AiBindingPurpose::GraphEntityType => stored_graph_entity_type_id = Some(stored.id),
                _ => {}
            }
        }

        // IRG-187 shipped the query_rerank runtime, so activating the stored
        // binding must now succeed rather than fail closed.
        let activated = ai
            .update_binding(
                &fixture.state,
                UpdateAiBindingCommand {
                    binding_id: stored_query_rerank_id.expect("query_rerank was stored"),
                    account_id: targets.decision_account_id,
                    model_catalog_id: targets.decision_model_id,
                    system_prompt: None,
                    temperature: None,
                    top_p: None,
                    max_output_tokens_override: None,
                    extra_parameters_json: serde_json::json!({}),
                    binding_state: "active".to_string(),
                    updated_by_principal_id: None,
                },
            )
            .await
            .context("query_rerank ships with IRG-187 and must activate")?;
        assert_eq!(activated.binding_state, "active");

        // IRG-185 shipped the graph entity typing runtime, so activating the
        // stored graph_entity_type binding must now succeed too.
        let activated_graph = ai
            .update_binding(
                &fixture.state,
                UpdateAiBindingCommand {
                    binding_id: stored_graph_entity_type_id.expect("graph_entity_type was stored"),
                    account_id: targets.decision_account_id,
                    model_catalog_id: targets.decision_model_id,
                    system_prompt: None,
                    temperature: None,
                    top_p: None,
                    max_output_tokens_override: None,
                    extra_parameters_json: serde_json::json!({}),
                    binding_state: "active".to_string(),
                    updated_by_principal_id: None,
                },
            )
            .await
            .context("graph_entity_type ships with IRG-185 and must activate")?;
        assert_eq!(activated_graph.binding_state, "active");
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn decision_provider_refuses_generative_purposes_even_disabled() -> Result<()> {
    let fixture = DecisionCatalogFixture::create().await?;
    let result = async {
        let targets = seed_targets(&fixture).await?;
        let ai = &fixture.state.application_services.ai_catalog;
        for purpose in [AiBindingPurpose::ExtractText, AiBindingPurpose::QueryAnswer] {
            let error = ai
                .create_binding(
                    &fixture.state,
                    binding_command(
                        purpose,
                        targets.decision_account_id,
                        targets.decision_model_id,
                        Some("disabled"),
                    ),
                )
                .await
                .expect_err("a decision-only provider must refuse a generative purpose");
            let message = expect_bad_request(error);
            assert!(
                message.contains(purpose.as_str()),
                "the refusal must name the purpose {}, got: {message}",
                purpose.as_str()
            );
        }
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn chat_provider_stays_bindable_on_a_decision_purpose() -> Result<()> {
    let fixture = DecisionCatalogFixture::create().await?;
    let result = async {
        let targets = seed_targets(&fixture).await?;
        let binding = fixture
            .state
            .application_services
            .ai_catalog
            .create_binding(
                &fixture.state,
                binding_command(
                    AiBindingPurpose::GraphEntityType,
                    targets.generative_account_id,
                    targets.generative_model_id,
                    None,
                ),
            )
            .await
            .context("a chat model allowed for graph_entity_type must stay bindable")?;
        assert_eq!(binding.binding_state, "active");
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn decision_provider_model_catalog_enforces_decision_only_purposes() -> Result<()> {
    let fixture = DecisionCatalogFixture::create().await?;
    let result = async {
        let targets = seed_targets(&fixture).await?;
        let ai = &fixture.state.application_services.ai_catalog;
        for purpose in [AiBindingPurpose::ExtractText, AiBindingPurpose::Agent] {
            let error = ai
                .create_model_catalog(
                    &fixture.state,
                    model_command(
                        targets.decision_provider.id,
                        format!("decision-generative-{}", Uuid::now_v7().simple()),
                        vec![purpose],
                    ),
                )
                .await
                .expect_err("a decision provider must not gain a generative-purpose model");
            assert!(expect_bad_request(error).contains(purpose.as_str()));
        }

        let decision_model = ai
            .create_model_catalog(
                &fixture.state,
                model_command(
                    targets.decision_provider.id,
                    format!("decision-only-{}", Uuid::now_v7().simple()),
                    vec![AiBindingPurpose::QueryRerank, AiBindingPurpose::GraphEntityType],
                ),
            )
            .await
            .context("a decision provider must accept decision-purpose models")?;

        let update_error = ai
            .update_model_catalog(
                &fixture.state,
                UpdateModelCatalogCommand {
                    model_id: decision_model.id,
                    lifecycle_state: "active".to_string(),
                    allowed_binding_purposes: vec![AiBindingPurpose::QueryAnswer],
                    context_window: None,
                    max_output_tokens: None,
                    metadata_json: None,
                    pricing: zero_pricing(),
                },
            )
            .await
            .expect_err("an update must not add a generative purpose to a decision model");
        assert!(expect_bad_request(update_error).contains("query_answer"));

        let updated = ai
            .update_model_catalog(
                &fixture.state,
                UpdateModelCatalogCommand {
                    model_id: decision_model.id,
                    lifecycle_state: "active".to_string(),
                    allowed_binding_purposes: vec![AiBindingPurpose::GraphEntityType],
                    context_window: None,
                    max_output_tokens: None,
                    metadata_json: None,
                    pricing: zero_pricing(),
                },
            )
            .await
            .context("a decision-purpose update must succeed")?;
        assert_eq!(updated.allowed_binding_purposes, [AiBindingPurpose::GraphEntityType]);
        let reloaded = ai_repository::get_model_catalog_by_id(fixture.pool(), decision_model.id)
            .await?
            .context("updated model persisted")?;
        for (id, provider, name) in [
            (updated.id, updated.provider_catalog_id, updated.model_name.as_str()),
            (reloaded.id, reloaded.provider_catalog_id, reloaded.model_name.as_str()),
        ] {
            assert_eq!(id, decision_model.id);
            assert_eq!(provider, decision_model.provider_catalog_id);
            assert_eq!(name, decision_model.model_name);
        }
        let account_id = Uuid::now_v7();
        let wrong_cipher =
            ironrag_backend::shared::secret_encryption::CredentialCipher::from_base64(
                "R0dHR0dHR0dHR0dHR0dHR0dHR0dHR0dHR0dHR0dHR0c=",
            )?;
        let encrypted = wrong_cipher.encrypt(
            ironrag_backend::shared::secret_encryption::SecretPurpose::AiAccountApiKey,
            account_id,
            "synthetic-test-only",
        )?;
        ai_repository::create_account(
            fixture.pool(),
            ai_repository::NewAiAccount {
                account_id,
                scope_kind: "instance",
                workspace_id: None,
                library_id: None,
                provider_catalog_id: decision_model.provider_catalog_id,
                label: "wrong-key control",
                base_url: None,
                created_by_principal_id: None,
            },
            Some(&encrypted),
        )
        .await?;
        assert!(
            matches!(
                ai.get_account(&fixture.state, account_id).await,
                Err(ApiError::ServiceUnavailable { kind: "credential_decryption_unavailable", .. })
            ),
            "full account reads must actually decrypt the wrong-key control"
        );
        let resolved = ai.get_resolved_model_catalog(&fixture.state, decision_model.id).await?;
        assert!(resolved.available_account_ids.contains(&account_id));
        let listed = ai
            .list_resolved_model_catalog(
                &fixture.state,
                Some(decision_model.provider_catalog_id),
                None,
                None,
                Some(account_id),
            )
            .await?;
        assert!(listed.iter().any(|row| row.model.id == decision_model.id
            && row.available_account_ids.contains(&account_id)));
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn decision_binding_update_activates_graph_entity_type() -> Result<()> {
    let fixture = DecisionCatalogFixture::create().await?;
    let result = async {
        let targets = seed_targets(&fixture).await?;
        let ai = &fixture.state.application_services.ai_catalog;
        let stored = ai
            .create_binding(
                &fixture.state,
                binding_command(
                    AiBindingPurpose::GraphEntityType,
                    targets.decision_account_id,
                    targets.decision_model_id,
                    Some("disabled"),
                ),
            )
            .await
            .context("store a disabled graph_entity_type binding")?;

        let activated = ai
            .update_binding(
                &fixture.state,
                UpdateAiBindingCommand {
                    binding_id: stored.id,
                    account_id: targets.decision_account_id,
                    model_catalog_id: targets.decision_model_id,
                    system_prompt: None,
                    temperature: None,
                    top_p: None,
                    max_output_tokens_override: None,
                    extra_parameters_json: serde_json::json!({}),
                    binding_state: "active".to_string(),
                    updated_by_principal_id: None,
                },
            )
            .await
            .context("graph_entity_type ships with IRG-185 and must activate")?;
        assert_eq!(activated.binding_state, "active");

        let disabled_again = ai
            .update_binding(
                &fixture.state,
                UpdateAiBindingCommand {
                    binding_id: stored.id,
                    account_id: targets.decision_account_id,
                    model_catalog_id: targets.decision_model_id,
                    system_prompt: None,
                    temperature: None,
                    top_p: None,
                    max_output_tokens_override: None,
                    extra_parameters_json: serde_json::json!({}),
                    binding_state: "disabled".to_string(),
                    updated_by_principal_id: None,
                },
            )
            .await
            .context("re-saving the binding disabled must succeed")?;
        assert_eq!(disabled_again.binding_state, "disabled");
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}
