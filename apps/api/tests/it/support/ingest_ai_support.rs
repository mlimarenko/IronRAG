use std::{sync::Arc, time::Duration};

use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use serde_json::json;
use uuid::Uuid;

use ironrag_backend::{
    app::state::AppState,
    infra::repositories::ai_repository,
    integrations::llm::{
        ChatRequest, ChatResponse, EmbeddingBatchRequest, EmbeddingBatchResponse, EmbeddingRequest,
        EmbeddingResponse, LlmGateway, VisionRequest, VisionResponse,
    },
    shared::secret_encryption::SecretPurpose,
};

pub(super) struct IngestAiFixtureConfig<'a> {
    pub(super) account_label: &'a str,
    pub(super) account_kind: &'static str,
    pub(super) credential_ref: &'a str,
    pub(super) fixture_name: &'static str,
    pub(super) graph_output: &'static str,
    pub(super) invalid_dimensions_subject: Option<&'static str>,
    pub(super) missing_catalog_subject: &'static str,
    /// Optional artificial provider latency. Cancellation tests need the
    /// inline pipeline to stay in flight long enough to drop the caller
    /// future at a known point; other fixtures leave this `None`.
    pub(super) io_delay: Option<Duration>,
    /// Optional injected panic, naming the gateway call that must panic
    /// (`"generate"` or `"embed_many"`). Used to prove a panic in a detached
    /// ingest is finalized instead of leaving the attempt leased.
    pub(super) panic_on: Option<&'static str>,
}

#[derive(Clone)]
struct IngestAiGateway {
    embedding_dimensions: usize,
    fixture_name: &'static str,
    graph_output: &'static str,
    io_delay: Option<Duration>,
    panic_on: Option<&'static str>,
}

impl IngestAiGateway {
    async fn apply_delay(&self) {
        if let Some(delay) = self.io_delay {
            tokio::time::sleep(delay).await;
        }
    }

    fn embedding(&self) -> Vec<f32> {
        let mut embedding = vec![0.0; self.embedding_dimensions];
        if let Some(first) = embedding.first_mut() {
            *first = 1.0;
        }
        embedding
    }
}

#[async_trait]
impl LlmGateway for IngestAiGateway {
    async fn generate(&self, mut request: ChatRequest) -> Result<ChatResponse> {
        self.apply_delay().await;
        if self.panic_on == Some("generate") {
            panic!("IRG-199 test: injected panic in the extract_graph provider call");
        }
        Ok(ChatResponse {
            provider_kind: std::mem::take(&mut request.provider_kind),
            model_name: std::mem::take(&mut request.model_name),
            output_text: self.graph_output.to_string(),
            usage_json: json!({}),
        })
    }

    async fn embed(&self, mut request: EmbeddingRequest) -> Result<EmbeddingResponse> {
        self.apply_delay().await;
        Ok(EmbeddingResponse {
            provider_kind: std::mem::take(&mut request.provider_kind),
            model_name: std::mem::take(&mut request.model_name),
            dimensions: self.embedding_dimensions,
            embedding: self.embedding(),
            usage_json: json!({}),
        })
    }

    async fn embed_many(
        &self,
        mut request: EmbeddingBatchRequest,
    ) -> Result<EmbeddingBatchResponse> {
        self.apply_delay().await;
        if self.panic_on == Some("embed_many") {
            panic!("IRG-199 test: injected panic in the embed_chunk provider call");
        }
        let embeddings = request.inputs.iter().map(|_| self.embedding()).collect();
        Ok(EmbeddingBatchResponse {
            provider_kind: std::mem::take(&mut request.provider_kind),
            model_name: std::mem::take(&mut request.model_name),
            dimensions: self.embedding_dimensions,
            embeddings,
            usage_json: json!({}),
        })
    }

    async fn vision_extract(&self, request: VisionRequest) -> Result<VisionResponse> {
        Err(anyhow!(
            "vision extraction is not used by the {} fixture: {}",
            self.fixture_name,
            request.provider_kind,
        ))
    }
}

pub(super) async fn configure_ingest_ai_fixture(
    state: &mut AppState,
    workspace_id: Uuid,
    library_id: Uuid,
    config: IngestAiFixtureConfig<'_>,
) -> Result<()> {
    let provider = ai_repository::list_provider_catalog(&state.persistence.postgres)
        .await
        .with_context(|| format!("failed to list {} provider catalog", config.fixture_name))?
        .into_iter()
        .find(|row| row.provider_kind == "openai")
        .with_context(|| {
            format!("seeded OpenAI provider is missing for {}", config.missing_catalog_subject)
        })?;
    let models = ai_repository::list_model_catalog(&state.persistence.postgres, Some(provider.id))
        .await
        .with_context(|| format!("failed to list {} model catalog", config.fixture_name))?;
    let embedding_model = models
        .iter()
        .find(|row| row.allowed_binding_purposes.iter().any(|purpose| purpose == "embed_chunk"))
        .with_context(|| {
            format!("seeded embedding model is missing for {}", config.missing_catalog_subject)
        })?;
    let graph_model = models
        .iter()
        .find(|row| row.allowed_binding_purposes.iter().any(|purpose| purpose == "extract_graph"))
        .with_context(|| {
            format!("seeded graph model is missing for {}", config.missing_catalog_subject)
        })?;
    let embedding_dimensions = embedding_model.metadata_json["dimensions"]
        .as_u64()
        .and_then(|value| usize::try_from(value).ok())
        .filter(|value| *value > 0)
        .with_context(|| {
            config.invalid_dimensions_subject.map_or_else(
                || "seeded embedding model has no valid dimensions".to_string(),
                |subject| format!("seeded embedding model has no valid dimensions for {subject}"),
            )
        })?;

    let account_id = Uuid::now_v7();
    let encrypted_api_key = state.credential_cipher.encrypt(
        SecretPurpose::AiAccountApiKey,
        account_id,
        config.credential_ref,
    )?;
    let account = ai_repository::create_account(
        &state.persistence.postgres,
        ai_repository::NewAiAccount {
            account_id,
            scope_kind: "workspace",
            workspace_id: Some(workspace_id),
            library_id: None,
            provider_catalog_id: provider.id,
            label: config.account_label,
            base_url: None,
            created_by_principal_id: None,
        },
        Some(&encrypted_api_key),
    )
    .await
    .with_context(|| {
        format!("failed to create {} {} account", config.fixture_name, config.account_kind)
    })?;

    for (purpose, model) in [("embed_chunk", embedding_model), ("extract_graph", graph_model)] {
        ai_repository::create_binding(
            &state.persistence.postgres,
            ai_repository::CreateAiBindingInput {
                scope_kind: "library",
                workspace_id: Some(workspace_id),
                library_id: Some(library_id),
                binding_purpose: purpose,
                account_id: account.id,
                provider_catalog_id: provider.id,
                model_catalog_id: model.id,
                system_prompt: None,
                temperature: None,
                top_p: None,
                max_output_tokens_override: None,
                extra_parameters_json: json!({}),
                binding_state: "active",
                updated_by_principal_id: None,
            },
        )
        .await
        .with_context(|| format!("failed to create {} {purpose} binding", config.fixture_name))?;
    }

    state.llm_gateway = Arc::new(IngestAiGateway {
        embedding_dimensions,
        fixture_name: config.fixture_name,
        graph_output: config.graph_output,
        io_delay: config.io_delay,
        panic_on: config.panic_on,
    });
    Ok(())
}
