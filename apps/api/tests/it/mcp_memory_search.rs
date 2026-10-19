use crate::{iam_token_support, mcp_tool_call_support, temp_postgres_database};

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use anyhow::{Context, anyhow, ensure};
use async_trait::async_trait;
use axum::Router;
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use sqlx::postgres::PgPoolOptions;
use tokio::sync::Barrier;
use uuid::Uuid;

use temp_postgres_database::TempPostgresDatabase;

use ironrag_backend::infra::knowledge_plane::VectorSimilaritySearchInput;
use ironrag_backend::{
    app::{config::Settings, seed_clean_v1_ai_catalog, state::AppState},
    domains::ai::AiBindingPurpose,
    infra::{
        knowledge_rows::{
            KNOWLEDGE_CHUNK_VECTOR_KIND, KnowledgeChunkVectorRow, KnowledgeStructuredBlockRow,
            KnowledgeStructuredRevisionRow,
        },
        repositories::{
            ai_repository, catalog_repository, content_chunk_repository,
            content_document_head_repository, content_document_repository,
            content_revision_repository,
        },
    },
    integrations::llm::{
        ChatRequest, ChatResponse, EmbeddingBatchRequest, EmbeddingBatchResponse, EmbeddingRequest,
        EmbeddingResponse, LlmGateway, VisionRequest, VisionResponse,
    },
    interfaces::http::router,
    services::knowledge::service::{
        CreateKnowledgeDocumentCommand, CreateKnowledgeRevisionCommand,
        PromoteKnowledgeDocumentCommand,
    },
    shared::secret_encryption::SecretPurpose,
};

const TEST_EMBEDDING_DIMENSIONS: usize = 3;

#[derive(Clone)]
struct EmbeddingPause {
    reached: Arc<Barrier>,
    release: Arc<Barrier>,
}

impl EmbeddingPause {
    fn new() -> Self {
        Self { reached: Arc::new(Barrier::new(2)), release: Arc::new(Barrier::new(2)) }
    }
}

struct ArmedEmbeddingPause {
    calls_before_pause: usize,
    pause: EmbeddingPause,
}

#[derive(Clone, Default)]
struct McpSearchGateway {
    next_embedding_pause: Arc<Mutex<Option<ArmedEmbeddingPause>>>,
}

impl McpSearchGateway {
    fn embedding() -> Vec<f32> {
        vec![1.0, 0.0, 0.0]
    }

    fn arm_embedding_pause_after(
        &self,
        calls_before_pause: usize,
    ) -> anyhow::Result<EmbeddingPause> {
        let pause = EmbeddingPause::new();
        let mut slot = self
            .next_embedding_pause
            .lock()
            .map_err(|_| anyhow!("MCP embedding pause mutex was poisoned"))?;
        ensure!(slot.is_none(), "an MCP embedding pause is already armed");
        *slot = Some(ArmedEmbeddingPause { calls_before_pause, pause: pause.clone() });
        Ok(pause)
    }

    async fn wait_for_armed_pause(&self) -> anyhow::Result<()> {
        let pause = {
            let mut slot = self
                .next_embedding_pause
                .lock()
                .map_err(|_| anyhow!("MCP embedding pause mutex was poisoned"))?;
            match slot.as_mut() {
                Some(armed) if armed.calls_before_pause > 0 => {
                    armed.calls_before_pause -= 1;
                    None
                }
                Some(_) => slot.take().map(|armed| armed.pause),
                None => None,
            }
        };
        if let Some(pause) = pause {
            pause.reached.wait().await;
            pause.release.wait().await;
        }
        Ok(())
    }
}

#[async_trait]
impl LlmGateway for McpSearchGateway {
    async fn generate(&self, request: ChatRequest) -> anyhow::Result<ChatResponse> {
        Err(anyhow!("generate is not used in the MCP search fixture: {}", request.provider_kind,))
    }

    async fn embed(&self, mut request: EmbeddingRequest) -> anyhow::Result<EmbeddingResponse> {
        self.wait_for_armed_pause().await?;
        Ok(EmbeddingResponse {
            provider_kind: std::mem::take(&mut request.provider_kind),
            model_name: std::mem::take(&mut request.model_name),
            dimensions: TEST_EMBEDDING_DIMENSIONS,
            embedding: Self::embedding(),
            usage_json: json!({}),
        })
    }

    async fn embed_many(
        &self,
        mut request: EmbeddingBatchRequest,
    ) -> anyhow::Result<EmbeddingBatchResponse> {
        self.wait_for_armed_pause().await?;
        let embeddings = request.inputs.iter().map(|_| Self::embedding()).collect();
        Ok(EmbeddingBatchResponse {
            provider_kind: std::mem::take(&mut request.provider_kind),
            model_name: std::mem::take(&mut request.model_name),
            dimensions: TEST_EMBEDDING_DIMENSIONS,
            embeddings,
            usage_json: json!({}),
        })
    }

    async fn vision_extract(&self, request: VisionRequest) -> anyhow::Result<VisionResponse> {
        Err(anyhow!(
            "vision extraction is not used in the MCP search fixture: {}",
            request.provider_kind,
        ))
    }
}

/// Identifies the content-plane chunk projection `project_search_chunks` writes.
struct SearchChunkProjection<'a> {
    library_id: Uuid,
    document_id: Uuid,
    revision_id: Uuid,
    external_key: &'a str,
    matching_chunks: &'a [&'a str],
    now: DateTime<Utc>,
}

struct McpSearchFixture {
    state: AppState,
    gateway: McpSearchGateway,
    temp_database: TempPostgresDatabase,
    workspace_id: Uuid,
    primary_library_id: Uuid,
    primary_library_ref: String,
    secondary_library_id: Uuid,
    secondary_library_ref: String,
    foreign_library_ref: String,
    embedding_profile_keys: HashMap<Uuid, String>,
}

impl McpSearchFixture {
    async fn create(mut settings: Settings) -> anyhow::Result<Self> {
        settings.credential_master_key =
            Some("KioqKioqKioqKioqKioqKioqKioqKioqKioqKioqKio=".to_string());
        // This suite used to run straight against the configured database and
        // never migrated it, so it only worked when an earlier suite had
        // already created the schema — an ordering dependency the Makefile
        // never promised.
        let temp_database =
            TempPostgresDatabase::create(&settings.database_url, "mcp_memory_search").await?;
        settings.database_url = temp_database.database_url.clone();
        let postgres = PgPoolOptions::new()
            .max_connections(4)
            .connect(&settings.database_url)
            .await
            .context("failed to connect mcp_memory_search postgres")?;
        sqlx::migrate!("./migrations")
            .run(&postgres)
            .await
            .context("failed to apply mcp_memory_search migrations")?;
        seed_clean_v1_ai_catalog(&postgres)
            .await
            .context("failed to seed mcp_memory_search AI catalog")?;
        postgres.close().await;
        let mut state = AppState::new(settings).await?;
        let gateway = McpSearchGateway::default();
        state.llm_gateway = Arc::new(gateway.clone());
        let suffix = Uuid::now_v7().simple().to_string();

        let workspace = catalog_repository::create_workspace(
            &state.persistence.postgres,
            &format!("mcp-search-{suffix}"),
            "MCP Search Test",
            None,
        )
        .await
        .context("failed to create mcp search workspace")?;
        let primary_library = catalog_repository::create_library(
            &state.persistence.postgres,
            workspace.id,
            &format!("mcp-search-primary-{suffix}"),
            "Primary Search Library",
            Some("primary mcp search test library"),
            None,
        )
        .await
        .context("failed to create primary search library")?;
        let secondary_library = catalog_repository::create_library(
            &state.persistence.postgres,
            workspace.id,
            &format!("mcp-search-secondary-{suffix}"),
            "Secondary Search Library",
            Some("secondary mcp search test library"),
            None,
        )
        .await
        .context("failed to create secondary search library")?;

        let foreign_workspace = catalog_repository::create_workspace(
            &state.persistence.postgres,
            &format!("mcp-search-foreign-{suffix}"),
            "MCP Search Foreign Test",
            None,
        )
        .await
        .context("failed to create foreign search workspace")?;
        let foreign_library = catalog_repository::create_library(
            &state.persistence.postgres,
            foreign_workspace.id,
            &format!("mcp-search-foreign-library-{suffix}"),
            "Foreign Search Library",
            Some("foreign mcp search test library"),
            None,
        )
        .await
        .context("failed to create foreign search library")?;

        let mut embedding_profile_keys = HashMap::new();
        for library_id in [primary_library.id, secondary_library.id, foreign_library.id] {
            embedding_profile_keys
                .insert(library_id, Self::bind_embedding_model(&state, library_id).await?);
        }

        Ok(Self {
            state,
            gateway,
            temp_database,
            workspace_id: workspace.id,
            primary_library_id: primary_library.id,
            primary_library_ref: format!("{}/{}", workspace.slug, primary_library.slug),
            secondary_library_id: secondary_library.id,
            secondary_library_ref: format!("{}/{}", workspace.slug, secondary_library.slug),
            foreign_library_ref: format!("{}/{}", foreign_workspace.slug, foreign_library.slug),
            embedding_profile_keys,
        })
    }

    /// Search now proves the exact vector inventory of a library before it
    /// answers, and refuses when the library has no active `embed_chunk`
    /// binding. The fixture predates that rule, so every search returned a
    /// conflict instead of results.
    async fn bind_embedding_model(state: &AppState, library_id: Uuid) -> anyhow::Result<String> {
        let provider_catalog = ai_repository::list_provider_catalog(&state.persistence.postgres)
            .await
            .context("failed to list provider catalog for mcp search test")?
            .into_iter()
            .find(|row| row.provider_kind == "openai")
            .context("expected seeded openai provider catalog row")?;
        let model_catalog = ai_repository::list_model_catalog(
            &state.persistence.postgres,
            Some(provider_catalog.id),
        )
        .await
        .context("failed to list model catalog for mcp search test")?
        .into_iter()
        .find(|row| row.allowed_binding_purposes.iter().any(|purpose| purpose == "embed_chunk"))
        .context("expected seeded embedding model catalog row")?;
        let library =
            catalog_repository::get_library_by_id(&state.persistence.postgres, library_id)
                .await
                .context("failed to load library for mcp search binding")?
                .context("library disappeared before its embedding binding was created")?;
        let account_id = Uuid::now_v7();
        let encrypted_api_key = state.credential_cipher.encrypt(
            SecretPurpose::AiAccountApiKey,
            account_id,
            "secret://mcp-search/provider",
        )?;
        let account = ai_repository::create_account(
            &state.persistence.postgres,
            ai_repository::NewAiAccount {
                account_id,
                scope_kind: "workspace",
                workspace_id: Some(library.workspace_id),
                library_id: None,
                provider_catalog_id: provider_catalog.id,
                label: &format!("mcp-search-credential-{account_id}"),
                base_url: None,
                created_by_principal_id: None,
            },
            Some(&encrypted_api_key),
        )
        .await
        .context("failed to create mcp search provider credential")?;
        ai_repository::create_binding(
            &state.persistence.postgres,
            ai_repository::CreateAiBindingInput {
                scope_kind: "library",
                workspace_id: Some(library.workspace_id),
                library_id: Some(library_id),
                binding_purpose: "embed_chunk",
                account_id: account.id,
                provider_catalog_id: provider_catalog.id,
                model_catalog_id: model_catalog.id,
                system_prompt: None,
                temperature: None,
                top_p: None,
                max_output_tokens_override: None,
                // Keep the vector fixture small while exercising the same
                // declared-dimension and exact-profile contract as production.
                extra_parameters_json: json!({"dimensions": TEST_EMBEDDING_DIMENSIONS}),
                binding_state: "active",
                updated_by_principal_id: None,
            },
        )
        .await
        .context("failed to create mcp search embedding binding")?;
        let binding = state
            .application_services
            .ai_catalog
            .resolve_active_runtime_binding(state, library_id, AiBindingPurpose::EmbedChunk)
            .await
            .context("failed to resolve mcp search embedding binding")?
            .context("mcp search embedding binding was not resolved")?;
        let dimensions = binding
            .effective_embedding_dimensions
            .context("mcp search embedding binding omitted its declared dimensions")?
            .get();
        ensure!(
            dimensions == TEST_EMBEDDING_DIMENSIONS as u64,
            "mcp search binding resolved {dimensions} dimensions instead of {TEST_EMBEDDING_DIMENSIONS}",
        );
        Ok(binding.embedding_execution_profile_key())
    }

    async fn cleanup(self) -> anyhow::Result<()> {
        self.state.persistence.postgres.close().await;
        self.temp_database.drop().await
    }

    fn app(&self) -> Router {
        Router::new().nest("/v1", router()).with_state(self.state.clone())
    }

    async fn bearer_token(&self, scopes: &[&str], label: &str) -> anyhow::Result<String> {
        iam_token_support::mint_api_token(
            &self.state.persistence.postgres,
            Some(self.workspace_id),
            "workspace",
            label,
            scopes,
        )
        .await
        .map(|token| token.plaintext)
        .with_context(|| format!("failed to create mcp search token for {label}"))
    }

    async fn mcp_tool_call(
        &self,
        token: &str,
        tool_name: &str,
        arguments: Value,
    ) -> anyhow::Result<Value> {
        mcp_tool_call_support::call_tool(
            self.app(),
            "/v1/mcp",
            token,
            "search-test",
            tool_name,
            arguments,
        )
        .await
    }

    async fn create_document_state(
        &self,
        library_id: Uuid,
        external_key: &str,
        status: &str,
        extracted_text: Option<&str>,
        matching_chunks: &[&str],
    ) -> anyhow::Result<(Uuid, Uuid)> {
        let document = content_document_repository::create_document(
            &self.state.persistence.postgres,
            &content_document_repository::NewContentDocument {
                workspace_id: self.workspace_id,
                library_id,
                external_key,
                document_state: "active",
                created_by_principal_id: None,
                parent_external_key: None,
                parent_document_id: None,
                document_role: "primary",
            },
        )
        .await
        .with_context(|| format!("failed to create search document {external_key}"))?;
        let revision = crate::content_repository::create_imported_fixture_revision(
            &self.state.persistence.postgres,
            &content_revision_repository::NewContentRevision {
                admission_identity: None,
                document_id: document.id,
                workspace_id: self.workspace_id,
                library_id,
                revision_number: 1,
                parent_revision_id: None,
                content_source_kind: "upload",
                checksum: &format!("sha256:{}", "5".repeat(64)),
                mime_type: "text/plain",
                byte_size: 512,
                title: Some(external_key),
                language_code: None,
                source_uri: Some(&format!("{external_key}.txt")),
                document_hint: None,
                storage_key: None,
                created_by_principal_id: None,
            },
            &format!("{external_key}.txt"),
        )
        .await
        .with_context(|| format!("failed to create search revision for {external_key}"))?;
        crate::content_lifecycle_support::seed_extracted_revision_head(
            &self.state.persistence.postgres,
            document.id,
            revision.id,
            &format!("{external_key}.txt"),
            extracted_text,
        )
        .await?;

        // The search tools resolve documents through the knowledge plane. This
        // fixture only wrote content-plane rows and discarded the three
        // arguments below, so nothing it created was ever findable.
        self.state
            .application_services
            .knowledge
            .create_document_shell(
                &self.state,
                CreateKnowledgeDocumentCommand {
                    document_id: document.id,
                    workspace_id: self.workspace_id,
                    library_id,
                    external_key: external_key.to_string(),
                    file_name: None,
                    title: Some(external_key.to_string()),
                    document_state: "active".to_string(),
                },
            )
            .await
            .with_context(|| format!("failed to create knowledge shell for {external_key}"))?;
        let now = Utc::now();
        let graph_state = if status == "failed" { "failed" } else { "accepted" };
        self.state
            .application_services
            .knowledge
            .write_revision(
                &self.state,
                CreateKnowledgeRevisionCommand {
                    revision_id: revision.id,
                    workspace_id: self.workspace_id,
                    library_id,
                    document_id: document.id,
                    revision_number: 1,
                    revision_state: "active".to_string(),
                    revision_kind: "upload".to_string(),
                    storage_ref: None,
                    source_uri: Some(format!("{external_key}.txt")),
                    document_hint: None,
                    mime_type: "text/plain".to_string(),
                    checksum: revision.checksum.clone(),
                    byte_size: 512,
                    title: Some(external_key.to_string()),
                    normalized_text: extracted_text.map(ToString::to_string),
                    text_checksum: extracted_text.map(|_| "mcp-search-text".to_string()),
                    text_state: match (extracted_text, status) {
                        (Some(_), _) => "text_readable",
                        (None, "failed") => "failed",
                        (None, _) => "extracting_text",
                    }
                    .to_string(),
                    // This helper creates the complete exact-profile vector
                    // inventory synchronously below, so readiness must agree
                    // with that persisted source of truth.
                    vector_state: "ready".to_string(),
                    graph_state: graph_state.to_string(),
                    text_readable_at: extracted_text.map(|_| now),
                    vector_ready_at: Some(now),
                    graph_ready_at: None,
                    superseded_by_revision_id: None,
                },
            )
            .await
            .with_context(|| format!("failed to write knowledge revision for {external_key}"))?;
        if let Some(text) = extracted_text {
            let span_end = i32::try_from(text.chars().count()).unwrap_or(i32::MAX);
            self.state
                .document_store
                .replace_structured_revision_artifact(
                    &KnowledgeStructuredRevisionRow {
                        revision_id: revision.id,
                        workspace_id: self.workspace_id,
                        library_id,
                        document_id: document.id,
                        preparation_state: "prepared".to_string(),
                        normalization_profile: "mcp-search-test-v1".to_string(),
                        source_format: "text".to_string(),
                        language_code: None,
                        block_count: 1,
                        chunk_count: i32::try_from(matching_chunks.len())
                            .context("MCP search fixture chunk count exceeds i32")?,
                        typed_fact_count: 0,
                        outline_json: json!([]),
                        prepared_at: now,
                        updated_at: now,
                    },
                    &[KnowledgeStructuredBlockRow {
                        block_id: Uuid::now_v7(),
                        workspace_id: self.workspace_id,
                        library_id,
                        document_id: document.id,
                        revision_id: revision.id,
                        ordinal: 0,
                        block_kind: "paragraph".to_string(),
                        text: text.to_string(),
                        normalized_text: text.to_string(),
                        heading_trail: Vec::new(),
                        section_path: Vec::new(),
                        page_number: None,
                        span_start: Some(0),
                        span_end: Some(span_end),
                        parent_block_id: None,
                        table_coordinates_json: None,
                        code_language: None,
                        created_at: now,
                        updated_at: now,
                    }],
                )
                .await
                .with_context(|| {
                    format!("failed to persist prepared text block for {external_key}")
                })?;
        }
        self.project_search_chunks(SearchChunkProjection {
            library_id,
            document_id: document.id,
            revision_id: revision.id,
            external_key,
            matching_chunks,
            now,
        })
        .await?;

        crate::content_lifecycle_support::promote_fixture_document(
            &self.state,
            document.id,
            extracted_text.map(|_| revision.id),
        )
        .await?;
        Ok((document.id, revision.id))
    }

    /// Projects the content-plane chunks into the knowledge plane and persists
    /// the exact-profile vectors search reads.
    async fn project_search_chunks(
        &self,
        projection: SearchChunkProjection<'_>,
    ) -> anyhow::Result<()> {
        let SearchChunkProjection {
            library_id,
            document_id,
            revision_id,
            external_key,
            matching_chunks,
            now,
        } = projection;
        // `create_chunk` writes only the content plane. Search reads the
        // knowledge plane, so chunks have to be projected the way the ingest
        // pipeline projects them or nothing is ever findable.
        let mut start_offset = 0i32;
        let mut projections = Vec::with_capacity(matching_chunks.len());
        for (ordinal, chunk) in matching_chunks.iter().enumerate() {
            let chunk_length = i32::try_from(chunk.chars().count()).unwrap_or(i32::MAX);
            let end_offset = start_offset.saturating_add(chunk_length);
            projections.push(content_chunk_repository::NewContentChunkProjection {
                workspace_id: self.workspace_id,
                library_id,
                document_id,
                revision_id,
                chunk_index: i32::try_from(ordinal).unwrap_or(i32::MAX),
                start_offset,
                end_offset,
                token_count: Some(
                    i32::try_from(chunk.split_whitespace().count()).unwrap_or(i32::MAX),
                ),
                chunk_kind: Some("paragraph".to_string()),
                content_text: (*chunk).to_string(),
                normalized_text: (*chunk).to_string(),
                text_checksum: "mcp-search-chunk".to_string(),
                support_block_ids: Vec::new(),
                section_path: Vec::new(),
                heading_trail: Vec::new(),
                literal_digest: None,
                chunk_state: "ready".to_string(),
                text_generation: Some(1),
                vector_generation: Some(1),
                quality_score: Some(1.0),
                window_text: Some((*chunk).to_string()),
                occurred_at: None,
                occurred_until: None,
            });
            start_offset = end_offset;
        }
        let created_chunks = if projections.is_empty() {
            Vec::new()
        } else {
            content_chunk_repository::replace_chunks_with_projection(
                &self.state.persistence.postgres,
                revision_id,
                &projections,
            )
            .await
            .with_context(|| format!("failed to project chunks for {external_key}"))?
        };
        if !created_chunks.is_empty() {
            let embedding_profile_key = self
                .embedding_profile_keys
                .get(&library_id)
                .with_context(|| format!("embedding profile missing for library {library_id}"))?;
            let vectors = created_chunks
                .iter()
                .map(|chunk| KnowledgeChunkVectorRow {
                    vector_id: Uuid::now_v7(),
                    workspace_id: self.workspace_id,
                    library_id,
                    chunk_id: chunk.id,
                    revision_id,
                    embedding_model_key: embedding_profile_key.clone(),
                    vector_kind: KNOWLEDGE_CHUNK_VECTOR_KIND.to_string(),
                    dimensions: TEST_EMBEDDING_DIMENSIONS as i32,
                    vector: McpSearchGateway::embedding(),
                    freshness_generation: 1,
                    created_at: now,
                    occurred_at: chunk.occurred_at,
                    occurred_until: chunk.occurred_until,
                })
                .collect::<Vec<_>>();
            self.state.search_store.upsert_chunk_vectors_bulk(&vectors).await.with_context(
                || format!("failed to persist exact-profile vectors for {external_key}"),
            )?;
        }
        Ok(())
    }

    async fn publish_replacement_revision_without_generation_advance(
        &self,
        document_id: Uuid,
        parent_revision_id: Uuid,
        external_key: &str,
        normalized_text: &str,
    ) -> anyhow::Result<Uuid> {
        let revision = crate::content_repository::create_imported_fixture_revision(
            &self.state.persistence.postgres,
            &content_revision_repository::NewContentRevision {
                admission_identity: None,
                document_id,
                workspace_id: self.workspace_id,
                library_id: self.primary_library_id,
                revision_number: 2,
                parent_revision_id: Some(parent_revision_id),
                content_source_kind: "upload",
                checksum: &format!("sha256:{}", "6".repeat(64)),
                mime_type: "text/plain",
                byte_size: 512,
                title: Some(external_key),
                language_code: None,
                source_uri: Some(&format!("{external_key}.txt")),
                document_hint: None,
                storage_key: None,
                created_by_principal_id: None,
            },
            &format!("{external_key}.txt"),
        )
        .await
        .context("failed to create replacement content revision")?;
        let now = Utc::now();
        self.state
            .application_services
            .knowledge
            .write_revision(
                &self.state,
                CreateKnowledgeRevisionCommand {
                    revision_id: revision.id,
                    workspace_id: self.workspace_id,
                    library_id: self.primary_library_id,
                    document_id,
                    revision_number: 2,
                    revision_state: "active".to_string(),
                    revision_kind: "upload".to_string(),
                    storage_ref: None,
                    source_uri: Some(format!("{external_key}.txt")),
                    document_hint: None,
                    mime_type: "text/plain".to_string(),
                    checksum: revision.checksum.clone(),
                    byte_size: 512,
                    title: Some(external_key.to_string()),
                    normalized_text: Some(normalized_text.to_string()),
                    text_checksum: Some("mcp-search-replacement-text".to_string()),
                    text_state: "text_readable".to_string(),
                    vector_state: "ready".to_string(),
                    graph_state: "accepted".to_string(),
                    text_readable_at: Some(now),
                    vector_ready_at: Some(now),
                    graph_ready_at: None,
                    superseded_by_revision_id: None,
                },
            )
            .await
            .context("failed to create replacement knowledge revision")?;
        let span_end = i32::try_from(normalized_text.chars().count())
            .context("replacement structured text length exceeds i32")?;
        self.state
            .document_store
            .replace_structured_revision_artifact(
                &KnowledgeStructuredRevisionRow {
                    revision_id: revision.id,
                    workspace_id: self.workspace_id,
                    library_id: self.primary_library_id,
                    document_id,
                    preparation_state: "prepared".to_string(),
                    normalization_profile: "mcp-search-test-v1".to_string(),
                    source_format: "text".to_string(),
                    language_code: None,
                    block_count: 1,
                    chunk_count: 0,
                    typed_fact_count: 0,
                    outline_json: json!([]),
                    prepared_at: now,
                    updated_at: now,
                },
                &[KnowledgeStructuredBlockRow {
                    block_id: Uuid::now_v7(),
                    workspace_id: self.workspace_id,
                    library_id: self.primary_library_id,
                    document_id,
                    revision_id: revision.id,
                    ordinal: 0,
                    block_kind: "paragraph".to_string(),
                    text: normalized_text.to_string(),
                    normalized_text: normalized_text.to_string(),
                    heading_trail: Vec::new(),
                    section_path: Vec::new(),
                    page_number: None,
                    span_start: Some(0),
                    span_end: Some(span_end),
                    parent_block_id: None,
                    table_coordinates_json: None,
                    code_language: None,
                    created_at: now,
                    updated_at: now,
                }],
            )
            .await
            .context("failed to persist replacement structured artifact")?;
        content_document_head_repository::upsert_document_head_without_generation_with_executor(
            &self.state.persistence.postgres,
            &content_document_head_repository::NewContentDocumentHead {
                document_id,
                active_revision_id: Some(revision.id),
                readable_revision_id: Some(revision.id),
                publishing_mutation_item_id: None,
                latest_mutation_id: None,
                latest_successful_attempt_id: None,
            },
        )
        .await
        .context("failed to publish replacement content head")?;
        self.state
            .application_services
            .knowledge
            .promote_document(&self.state, PromoteKnowledgeDocumentCommand { document_id })
            .await
            .context("failed to publish replacement knowledge head")?;
        Ok(revision.id)
    }
}

#[tokio::test]
#[ignore = "requires local postgres and redis services"]
async fn authorized_search_spans_multiple_libraries_and_returns_stable_scope_ids()
-> anyhow::Result<()> {
    let settings = Settings::from_env().context("failed to load settings for mcp search test")?;
    let fixture = Box::pin(McpSearchFixture::create(settings)).await?;

    let result = async {
        let token = fixture.bearer_token(&["documents:read"], "search-multi").await?;
        let (primary_document_id, primary_revision_id) = fixture
            .create_document_state(
                fixture.primary_library_id,
                "primary-beacon",
                "ready",
                Some("beacon-signal alpha beacon-signal beta beacon-signal gamma"),
                &["beacon-signal alpha", "beacon-signal beta", "beacon-signal gamma"],
            )
            .await?;
        let (secondary_document_id, secondary_revision_id) = fixture
            .create_document_state(
                fixture.secondary_library_id,
                "secondary-beacon",
                "ready",
                Some("beacon-signal delta beacon-signal epsilon"),
                &["beacon-signal delta", "beacon-signal epsilon"],
            )
            .await?;

        let response = fixture
            .mcp_tool_call(
                &token,
                "search_documents",
                json!({
                    "query": "beacon-signal",
                    "libraries": [fixture.primary_library_ref, fixture.secondary_library_ref],
                    "limit": 5
                }),
            )
            .await?;
        assert_eq!(
            response["result"]["isError"],
            json!(false),
            "search_documents returned an MCP tool error: {response}",
        );
        assert_eq!(
            response["result"]["structuredContent"]["libraries"],
            json!([fixture.primary_library_ref, fixture.secondary_library_ref])
        );

        let hits = response["result"]["structuredContent"]["hits"]
            .as_array()
            .context("search hits missing")?;
        assert_eq!(hits.len(), 2);

        // Global hit order is score/readability based and UUID-tied, not the
        // caller's library-list order. Assert each scoped result by identity.
        let primary_hit = hits
            .iter()
            .find(|hit| hit["documentId"] == json!(primary_document_id))
            .context("primary library search hit missing")?;
        assert_eq!(primary_hit["libraryId"], json!(fixture.primary_library_id));
        assert_eq!(primary_hit["workspaceId"], json!(fixture.workspace_id));
        assert_eq!(primary_hit["latestRevisionId"], json!(primary_revision_id));
        assert_eq!(primary_hit["readabilityState"], json!("readable"));
        assert!(
            primary_hit["excerpt"]
                .as_str()
                .is_some_and(|excerpt| excerpt.contains("beacon-signal"))
        );

        let secondary_hit = hits
            .iter()
            .find(|hit| hit["documentId"] == json!(secondary_document_id))
            .context("secondary library search hit missing")?;
        assert_eq!(secondary_hit["libraryId"], json!(fixture.secondary_library_id));
        assert_eq!(secondary_hit["workspaceId"], json!(fixture.workspace_id));
        assert_eq!(secondary_hit["latestRevisionId"], json!(secondary_revision_id));
        assert_eq!(secondary_hit["readabilityState"], json!("readable"));
        assert!(
            secondary_hit["excerpt"]
                .as_str()
                .is_some_and(|excerpt| excerpt.contains("beacon-signal"))
        );
        assert!(primary_hit["score"].as_f64().is_some_and(|score| score > 0.0));
        assert!(secondary_hit["score"].as_f64().is_some_and(|score| score > 0.0));

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres and redis services"]
async fn search_revalidates_primary_hits_after_all_later_library_awaits() -> anyhow::Result<()> {
    let settings = Settings::from_env()
        .context("failed to load settings for MCP revision-race search test")?;
    let fixture = Box::pin(McpSearchFixture::create(settings)).await?;

    let result = async {
        let token = fixture.bearer_token(&["documents:read"], "search-revision-race").await?;
        let (document_id, retired_revision_id) = fixture
            .create_document_state(
                fixture.primary_library_id,
                "revision-race-fixture",
                "ready",
                Some("retired-generation-anchor belongs only to the first revision"),
                &["retired-generation-anchor belongs only to the first revision"],
            )
            .await?;
        let (secondary_document_id, secondary_revision_id) = fixture
            .create_document_state(
                fixture.secondary_library_id,
                "revision-race-control",
                "ready",
                Some("retired-generation-anchor remains current in the control library"),
                &["retired-generation-anchor remains current in the control library"],
            )
            .await?;
        // The first embedding belongs to the primary library. Pausing the
        // second call proves the primary hit has already passed its per-library
        // authoritative validation before its readable head advances.
        let pause = fixture.gateway.arm_embedding_pause_after(1)?;
        let app = fixture.app();
        let search_token = token.clone();
        let primary_library_ref = fixture.primary_library_ref.clone();
        let secondary_library_ref = fixture.secondary_library_ref.clone();
        let mut search_task = tokio::spawn(async move {
            mcp_tool_call_support::call_tool(
                app,
                "/v1/mcp",
                &search_token,
                "search-revision-race",
                "search_documents",
                json!({
                    "query": "retired-generation-anchor",
                    "libraries": [primary_library_ref, secondary_library_ref],
                    "limit": 3,
                    "includeReferences": true
                }),
            )
            .await
        });

        tokio::time::timeout(std::time::Duration::from_secs(5), pause.reached.wait())
            .await
            .context("MCP search never reached the secondary library's paused embedding lane")?;
        let replacement_result = fixture
            .publish_replacement_revision_without_generation_advance(
                document_id,
                retired_revision_id,
                "revision-race-fixture",
                "current-generation-anchor belongs only to the replacement revision",
            )
            .await;
        pause.release.wait().await;
        let raced_search_result =
            tokio::time::timeout(std::time::Duration::from_secs(5), &mut search_task)
                .await
                .context("paused MCP search did not complete after release")?;
        let current_revision_id = replacement_result?;

        let content_head = content_document_head_repository::get_document_head(
            &fixture.state.persistence.postgres,
            document_id,
        )
        .await?
        .context("replacement content head disappeared")?;
        assert_eq!(content_head.readable_revision_id, Some(current_revision_id));
        let knowledge_document = fixture
            .state
            .document_store
            .get_document(document_id)
            .await?
            .context("replacement knowledge document disappeared")?;
        assert_eq!(knowledge_document.readable_revision_id, Some(current_revision_id));

        let raced_response = raced_search_result??;
        assert_eq!(
            raced_response["result"]["isError"],
            json!(false),
            "raced search returned an MCP tool error: {raced_response}",
        );
        let raced_hits = raced_response["result"]["structuredContent"]["hits"]
            .as_array()
            .context("cross-library revision-race hits missing")?;
        assert!(
            raced_hits.iter().all(|hit| hit["documentId"] != json!(document_id)),
            "the primary hit passed local validation but leaked through after its readable head advanced: {raced_hits:?}",
        );
        let secondary_hit = raced_hits
            .iter()
            .find(|hit| hit["documentId"] == json!(secondary_document_id))
            .context("the unchanged secondary control hit was lost")?;
        assert_eq!(secondary_hit["latestRevisionId"], json!(secondary_revision_id));

        let current_response = fixture
            .mcp_tool_call(
                &token,
                "search_documents",
                json!({
                    "query": "current-generation-anchor",
                    "libraries": [fixture.primary_library_ref.clone()],
                    "limit": 3
                }),
            )
            .await?;
        let current_hit = current_response["result"]["structuredContent"]["hits"]
            .as_array()
            .context("current revision search hits missing")?
            .iter()
            .find(|hit| hit["documentId"] == json!(document_id))
            .context("replacement revision was not searchable")?;
        assert_eq!(current_hit["latestRevisionId"], json!(current_revision_id));

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres and redis services"]
async fn vector_search_rescues_a_hidden_document_only_below_the_exact_scan_bound()
-> anyhow::Result<()> {
    let settings = Settings::from_env()
        .context("failed to load settings for MCP dense-document vector search test")?;
    let fixture = Box::pin(McpSearchFixture::create(settings)).await?;

    let result = async {
        let token = fixture.bearer_token(&["documents:read"], "search-vector-dense").await?;
        let dominant_chunks =
            (0..13).map(|ordinal| format!("neutral fragment {ordinal:02}")).collect::<Vec<_>>();
        let dominant_chunk_refs = dominant_chunks.iter().map(String::as_str).collect::<Vec<_>>();
        let (dominant_document_id, dominant_revision_id) = fixture
            .create_document_state(
                fixture.primary_library_id,
                "vector-dense-a",
                "ready",
                Some("a fragmented neutral vector fixture"),
                &dominant_chunk_refs,
            )
            .await?;
        let (rescued_document_id, rescued_revision_id) = fixture
            .create_document_state(
                fixture.primary_library_id,
                "vector-dense-b",
                "ready",
                Some("a second neutral vector fixture"),
                &["one secondary neutral fragment"],
            )
            .await?;
        let dominant_projected_chunks =
            fixture.state.document_store.list_chunks_by_revision(dominant_revision_id).await?;
        assert_eq!(dominant_projected_chunks.len(), 13);
        let rescued_projected_chunks =
            fixture.state.document_store.list_chunks_by_revision(rescued_revision_id).await?;
        assert_eq!(rescued_projected_chunks.len(), 1);
        let mut rescued_vectors = fixture
            .state
            .search_store
            .list_chunk_vectors_by_chunk(rescued_projected_chunks[0].chunk_id)
            .await?;
        assert_eq!(rescued_vectors.len(), 1);
        rescued_vectors[0].vector = vec![0.8, 0.6, 0.0];
        fixture.state.search_store.upsert_chunk_vector(&rescued_vectors[0]).await?;

        // limit=2 produces six MCP document candidates and therefore twelve
        // primary ANN chunk candidates. All thirteen A vectors score 1.0,
        // while B scores 0.8, so the old global chunk top-k returned only A.
        let response = fixture
            .mcp_tool_call(
                &token,
                "search_documents",
                json!({
                    "query": "semantic-only-anchor",
                    "libraries": [fixture.primary_library_ref.clone()],
                    "limit": 2,
                    "includeReferences": true
                }),
            )
            .await?;
        assert_eq!(
            response["result"]["isError"],
            json!(false),
            "document-dense vector search returned an MCP tool error: {response}",
        );
        let hits = response["result"]["structuredContent"]["hits"]
            .as_array()
            .context("document-dense vector search hits missing")?;
        assert_eq!(hits.len(), 2);
        for expected_document_id in [dominant_document_id, rescued_document_id] {
            let hit = hits
                .iter()
                .find(|hit| hit["documentId"] == json!(expected_document_id))
                .with_context(|| {
                    format!("vector hit missing for document {expected_document_id}")
                })?;
            let references =
                hit["chunkReferences"].as_array().context("vector chunk references missing")?;
            assert_eq!(references.len(), 1);
            assert_eq!(references[0]["inclusionReason"], json!("vector_chunk"));
        }

        let embedding_profile_key = fixture
            .embedding_profile_keys
            .get(&fixture.primary_library_id)
            .context("primary library embedding profile disappeared")?;
        let manifest_update = sqlx::query(
            "update knowledge_vector_relation_manifest
             set row_count = $1
             where library_id = $2
               and dim = $3
               and vector_kind = $4
               and embedding_model_key = $5",
        )
        // Runtime configuration is clamped to at most 100_000 rows. Setting
        // the manifest one row above that hard ceiling deterministically
        // exercises the executor's bounded branch without process-global env
        // mutation that could race another integration test.
        .bind(100_001_i64)
        .bind(fixture.primary_library_id)
        .bind(TEST_EMBEDDING_DIMENSIONS as i32)
        .bind(KNOWLEDGE_CHUNK_VECTOR_KIND)
        .bind(embedding_profile_key)
        .execute(&fixture.state.persistence.postgres)
        .await
        .context("failed to place dense-vector manifest above the exact rescue bound")?;
        ensure!(
            manifest_update.rows_affected() == 1,
            "dense-vector manifest update affected {} rows instead of one",
            manifest_update.rows_affected(),
        );

        let bounded_rows = fixture
            .state
            .search_store
            .search_chunk_vectors_by_document_similarity(
                VectorSimilaritySearchInput {
                    dim: TEST_EMBEDDING_DIMENSIONS as u64,
                    library_id: fixture.primary_library_id,
                    embedding_model_key: embedding_profile_key,
                    query_vector: &McpSearchGateway::embedding(),
                    n_probe: None,
                },
                6,
                None,
                None,
            )
            .await
            .context("bounded document-dense vector search failed")?;
        let dominant_chunk_ids =
            dominant_projected_chunks.iter().map(|chunk| chunk.chunk_id).collect::<Vec<_>>();
        assert_eq!(
            bounded_rows.len(),
            1,
            "an over-bound manifest must return only the ANN-represented document"
        );
        assert!(dominant_chunk_ids.contains(&bounded_rows[0].chunk_id));
        assert!(
            bounded_rows.iter().all(|row| row.chunk_id != rescued_projected_chunks[0].chunk_id),
            "the over-bound executor ran the forbidden exact unseen-document rescue",
        );

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres and redis services"]
async fn empty_search_results_return_explicit_no_match_payload() -> anyhow::Result<()> {
    let settings = Settings::from_env().context("failed to load settings for empty search test")?;
    let fixture = Box::pin(McpSearchFixture::create(settings)).await?;

    let result = async {
        let token = fixture.bearer_token(&["documents:read"], "search-empty").await?;

        let response = fixture
            .mcp_tool_call(
                &token,
                "search_documents",
                json!({
                    "query": "string-that-does-not-exist",
                    "limit": 3
                }),
            )
            .await?;
        assert_eq!(
            response["result"]["isError"],
            json!(false),
            "search_documents returned an MCP tool error: {response}",
        );
        assert_eq!(
            response["result"]["structuredContent"]["query"],
            json!("string-that-does-not-exist")
        );
        assert_eq!(response["result"]["structuredContent"]["hits"], json!([]));
        let mut actual_libraries = response["result"]["structuredContent"]["libraries"]
            .as_array()
            .context("search library scope missing")?
            .iter()
            .filter_map(Value::as_str)
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        let mut expected_libraries =
            vec![fixture.primary_library_ref.clone(), fixture.secondary_library_ref.clone()];
        actual_libraries.sort();
        expected_libraries.sort();
        assert_eq!(actual_libraries, expected_libraries);

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres and redis services"]
async fn empty_and_failed_documents_surface_honest_readability_metadata_in_hits()
-> anyhow::Result<()> {
    let settings = Settings::from_env().context("failed to load settings for search state test")?;
    let fixture = Box::pin(McpSearchFixture::create(settings)).await?;

    let result = async {
        let token = fixture.bearer_token(&["documents:read"], "search-states").await?;
        let (processing_document_id, _) = fixture
            .create_document_state(
                fixture.primary_library_id,
                "processing-beacon",
                "processing",
                None,
                &[],
            )
            .await?;
        let (failed_document_id, _) = fixture
            .create_document_state(fixture.primary_library_id, "failed-beacon", "failed", None, &[])
            .await?;

        let response = fixture
            .mcp_tool_call(
                &token,
                "search_documents",
                json!({
                    "query": "beacon",
                    "libraries": [fixture.primary_library_ref],
                    "limit": 5,
                    "includeUnreadable": true
                }),
            )
            .await?;
        assert_eq!(
            response["result"]["isError"],
            json!(false),
            "search_documents returned an MCP tool error: {response}",
        );

        let hits = response["result"]["structuredContent"]["hits"]
            .as_array()
            .context("stateful search hits missing")?;
        assert_eq!(hits.len(), 2);

        let processing_hit = hits
            .iter()
            .find(|hit| hit["documentId"] == json!(processing_document_id))
            .context("processing search hit missing")?;
        assert_eq!(processing_hit["readabilityState"], json!("unavailable"));
        assert_eq!(processing_hit["readinessKind"], json!("empty"));
        assert_eq!(processing_hit["graphCoverageKind"], json!("empty"));
        assert!(
            processing_hit["excerpt"].as_str().is_some_and(|excerpt| excerpt.contains("beacon"))
        );
        assert!(processing_hit["excerptStartOffset"].as_u64().is_some());
        assert!(processing_hit["excerptEndOffset"].as_u64().is_some());

        let failed_hit = hits
            .iter()
            .find(|hit| hit["documentId"] == json!(failed_document_id))
            .context("failed search hit missing")?;
        assert_eq!(failed_hit["readabilityState"], json!("failed"));
        assert_eq!(failed_hit["readinessKind"], json!("failed"));
        assert_eq!(failed_hit["graphCoverageKind"], json!("failed"));
        assert!(failed_hit["excerpt"].as_str().is_some_and(|excerpt| excerpt.contains("beacon")));
        assert!(failed_hit["excerptStartOffset"].as_u64().is_some());
        assert!(failed_hit["excerptEndOffset"].as_u64().is_some());

        for hit in [processing_hit, failed_hit] {
            assert!(hit["latestRevisionId"].is_null());
            assert!(hit["suggestedStartOffset"].is_null());
            let decoded: ironrag_backend::mcp_types::McpDocumentHit =
                serde_json::from_value(hit.clone())?;
            assert!(decoded.chunk_references.is_empty(), "unpublished chunks leaked");
            assert!(decoded.technical_fact_references.is_empty(), "unpublished facts leaked");
            assert!(decoded.entity_references.is_empty(), "unpublished entities leaked");
            assert!(decoded.relation_references.is_empty(), "unpublished relations leaked");
            assert!(decoded.evidence_references.is_empty(), "unpublished evidence leaked");
        }
        assert_eq!(failed_hit["statusReason"], json!("latest readable revision extraction failed"));

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres and redis services"]
async fn processing_documents_with_extracted_text_surface_as_readable_graph_sparse_hits()
-> anyhow::Result<()> {
    let settings = Settings::from_env().context("failed to load settings for search state test")?;
    let fixture = Box::pin(McpSearchFixture::create(settings)).await?;

    let result = async {
        let token = fixture.bearer_token(&["documents:read"], "search-early-readable").await?;
        let (document_id, revision_id) = fixture
            .create_document_state(
                fixture.primary_library_id,
                "processing-readable-beacon",
                "processing",
                Some(
                    "status-signal extracted memory is already readable before graph extraction finishes",
                ),
                &["status-signal extracted memory is already readable"],
            )
            .await?;

        let response = fixture
            .mcp_tool_call(
                &token,
                "search_documents",
                json!({
                    "query": "status-signal",
                    "libraries": [fixture.primary_library_ref.clone()],
                    "limit": 5
                }),
            )
            .await?;
        assert_eq!(
            response["result"]["isError"],
            json!(false),
            "search_documents returned an MCP tool error: {response}",
        );

        let hit = response["result"]["structuredContent"]["hits"]
            .as_array()
            .context("search hits missing")?
            .iter()
            .find(|item| item["documentId"] == json!(document_id))
            .context("processing readable hit missing")?;
        assert_eq!(hit["latestRevisionId"], json!(revision_id));
        assert_eq!(hit["readabilityState"], json!("readable"));
        assert_eq!(hit["readinessKind"], json!("graph_sparse"));
        assert_eq!(hit["graphCoverageKind"], json!("graph_sparse"));
        assert_eq!(
            hit["statusReason"],
            json!("document text is readable, but graph coverage is still sparse for this revision"),
        );
        assert!(hit["excerpt"].as_str().is_some_and(|excerpt| excerpt.contains("status-signal")));
        assert!(hit["excerptStartOffset"].as_i64().is_some());
        assert!(hit["excerptEndOffset"].as_i64().is_some());

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres and redis services"]
async fn extracted_text_matches_are_searchable_without_chunk_rows_and_after_graph_failure()
-> anyhow::Result<()> {
    let settings =
        Settings::from_env().context("failed to load settings for direct text search test")?;
    let fixture = Box::pin(McpSearchFixture::create(settings)).await?;

    let result = async {
        let token = fixture.bearer_token(&["documents:read"], "search-direct-text").await?;
        // More matching blocks than the lexical candidate window (limit 2 ->
        // six candidates) proves candidate selection is document-dense. A
        // block-level LIMIT would spend the entire window on this revision
        // and starve the later matching document.
        let ready_block_texts = [
            "Résumé search-direct-anchor search-direct-anchor",
            "search-direct-anchor search-direct-anchor fragment-01",
            "search-direct-anchor search-direct-anchor fragment-02",
            "search-direct-anchor search-direct-anchor fragment-03",
            "search-direct-anchor search-direct-anchor fragment-04",
            "search-direct-anchor search-direct-anchor fragment-05",
            "search-direct-anchor search-direct-anchor fragment-06",
            "search-direct-anchor search-direct-anchor fragment-07",
        ];
        let ready_text = ready_block_texts.join("\n\n");
        let (ready_document_id, ready_revision_id) = fixture
            .create_document_state(
                fixture.primary_library_id,
                "direct-readable-beacon",
                "ready",
                Some(&ready_text),
                &[],
            )
            .await?;

        let now = Utc::now();
        let mut next_span_start = 0i32;
        let ready_blocks = ready_block_texts
            .iter()
            .enumerate()
            .map(|(ordinal, text)| {
                let span_start = next_span_start;
                let text_length = i32::try_from(text.chars().count())
                    .context("ready structured block text length exceeds i32")?;
                let span_end = span_start
                    .checked_add(text_length)
                    .context("ready structured block span exceeds i32")?;
                next_span_start = span_end
                    .checked_add(2)
                    .context("ready structured block separator span exceeds i32")?;
                Ok(KnowledgeStructuredBlockRow {
                    block_id: Uuid::now_v7(),
                    workspace_id: fixture.workspace_id,
                    library_id: fixture.primary_library_id,
                    document_id: ready_document_id,
                    revision_id: ready_revision_id,
                    ordinal: i32::try_from(ordinal)
                        .context("ready structured block ordinal exceeds i32")?,
                    block_kind: "paragraph".to_string(),
                    text: (*text).to_string(),
                    normalized_text: (*text).to_string(),
                    heading_trail: Vec::new(),
                    section_path: Vec::new(),
                    page_number: None,
                    span_start: Some(span_start),
                    span_end: Some(span_end),
                    parent_block_id: None,
                    table_coordinates_json: None,
                    code_language: None,
                    created_at: now,
                    updated_at: now,
                })
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        fixture
            .state
            .document_store
            .replace_structured_revision_artifact(
                &KnowledgeStructuredRevisionRow {
                    revision_id: ready_revision_id,
                    workspace_id: fixture.workspace_id,
                    library_id: fixture.primary_library_id,
                    document_id: ready_document_id,
                    preparation_state: "prepared".to_string(),
                    normalization_profile: "mcp-search-test-v1".to_string(),
                    source_format: "text".to_string(),
                    language_code: None,
                    block_count: i32::try_from(ready_blocks.len())
                        .context("ready structured block count exceeds i32")?,
                    chunk_count: 0,
                    typed_fact_count: 0,
                    outline_json: json!([]),
                    prepared_at: now,
                    updated_at: now,
                },
                &ready_blocks,
            )
            .await
            .context("failed to persist fragmented ready structured revision")?;

        assert!(
            fixture
                .state
                .document_store
                .list_chunks_by_revision(ready_revision_id)
                .await?
                .is_empty(),
            "ready direct-text fixture unexpectedly contains chunk rows",
        );
        let (failed_document_id, failed_revision_id) = fixture
            .create_document_state(
                fixture.primary_library_id,
                "failed-readable-beacon",
                "failed",
                Some("search-direct-anchor also survives a later graph projection failure"),
                &[],
            )
            .await?;
        assert!(
            fixture
                .state
                .document_store
                .list_chunks_by_revision(failed_revision_id)
                .await?
                .is_empty(),
            "failed direct-text fixture unexpectedly contains chunk rows",
        );

        let response = fixture
            .mcp_tool_call(
                &token,
                "search_documents",
                json!({
                    "query": "search-direct-anchor",
                    "libraries": [fixture.primary_library_ref.clone()],
                    "limit": 2
                }),
            )
            .await?;
        assert_eq!(
            response["result"]["isError"],
            json!(false),
            "search_documents returned an MCP tool error: {response}",
        );

        let hits = response["result"]["structuredContent"]["hits"]
            .as_array()
            .context("direct text search hits missing")?;
        assert_eq!(hits.len(), 2);

        let ready_hit = hits
            .iter()
            .find(|hit| hit["documentId"] == json!(ready_document_id))
            .context("ready direct-text hit missing")?;
        assert_eq!(ready_hit["latestRevisionId"], json!(ready_revision_id));
        assert_eq!(ready_hit["readabilityState"], json!("readable"));
        assert_eq!(ready_hit["readinessKind"], json!("graph_sparse"));
        assert_eq!(ready_hit["graphCoverageKind"], json!("graph_sparse"));
        assert_eq!(
            ready_hit["statusReason"],
            json!("document text is readable, but graph coverage is still sparse for this revision"),
        );
        assert!(
            ready_hit["excerpt"]
                .as_str()
                .is_some_and(|excerpt| excerpt.contains("search-direct-anchor"))
        );

        let failed_hit = hits
            .iter()
            .find(|hit| hit["documentId"] == json!(failed_document_id))
            .context("failed direct-text hit missing")?;
        assert_eq!(failed_hit["latestRevisionId"], json!(failed_revision_id));
        assert_eq!(failed_hit["readabilityState"], json!("readable"));
        assert_eq!(failed_hit["readinessKind"], json!("graph_sparse"));
        assert_eq!(failed_hit["graphCoverageKind"], json!("failed"));
        assert_eq!(
            failed_hit["statusReason"],
            json!(
                "document text is readable, but normalized technical facts and graph evidence are not available yet"
            ),
        );
        assert!(
            failed_hit["excerpt"]
                .as_str()
                .is_some_and(|excerpt| excerpt.contains("search-direct-anchor"))
        );

        let diacritic_response = fixture
            .mcp_tool_call(
                &token,
                "search_documents",
                json!({
                    "query": "resume",
                    "libraries": [fixture.primary_library_ref.clone()],
                    "limit": 5
                }),
            )
            .await?;
        assert_eq!(
            diacritic_response["result"]["isError"],
            json!(false),
            "diacritic search returned an MCP tool error: {diacritic_response}",
        );
        let diacritic_hit = diacritic_response["result"]["structuredContent"]["hits"]
            .as_array()
            .context("diacritic search hits missing")?
            .iter()
            .find(|hit| hit["documentId"] == json!(ready_document_id))
            .context("accent-folded direct-text hit missing")?;
        assert!(
            diacritic_hit["excerpt"]
                .as_str()
                .is_some_and(|excerpt| excerpt.contains("Résumé")),
            "accent-folded search did not preserve the source glyphs: {diacritic_hit}",
        );

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres and redis services"]
async fn inaccessible_library_filters_reject_search_instead_of_returning_partial_results()
-> anyhow::Result<()> {
    let settings = Settings::from_env().context("failed to load settings for search auth test")?;
    let fixture = Box::pin(McpSearchFixture::create(settings)).await?;

    let result = async {
        let token = fixture.bearer_token(&["documents:read"], "search-authz").await?;

        let response = fixture
            .mcp_tool_call(
                &token,
                "search_documents",
                json!({
                    "query": "beacon-signal",
                    "libraries": [
                        fixture.primary_library_ref.clone(),
                        fixture.foreign_library_ref.clone(),
                    ],
                    "limit": 5
                }),
            )
            .await?;
        assert_eq!(response["result"]["isError"], json!(true));
        assert_eq!(response["result"]["structuredContent"]["errorKind"], json!("unauthorized"));
        assert_eq!(response["result"]["structuredContent"]["message"], json!("unauthorized"));

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres and redis services"]
async fn search_documents_preserves_lexical_hits_with_complete_vector_inventory()
-> anyhow::Result<()> {
    let settings =
        Settings::from_env().context("failed to load settings for mcp lexical search test")?;
    let fixture = Box::pin(McpSearchFixture::create(settings)).await?;

    let result = async {
        let token = fixture.bearer_token(&["documents:read"], "search-lexical").await?;
        let _ = fixture
            .create_document_state(
                fixture.primary_library_id,
                "lexical-beacon",
                "ready",
                Some("lexical-anchor remains searchable through the lexical retrieval lane"),
                &["lexical-anchor remains searchable through the lexical retrieval lane"],
            )
            .await?;

        let response = fixture
            .mcp_tool_call(
                &token,
                "search_documents",
                json!({
                    "query": "lexical-anchor",
                    "libraries": [fixture.primary_library_ref.clone()],
                    "limit": 5,
                    "includeReferences": true
                }),
            )
            .await?;
        // A successful call proves the exact-profile inventory gate accepted
        // the fixture. The returned reference proves the lexical lane still
        // contributed even though the vector lane was healthy and available.
        assert_eq!(
            response["result"]["isError"],
            json!(false),
            "search_documents returned an MCP tool error: {response}",
        );
        let hits = response["result"]["structuredContent"]["hits"]
            .as_array()
            .context("lexical search hits missing")?;
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0]["readabilityState"], json!("readable"));
        assert!(
            hits[0]["excerpt"].as_str().is_some_and(|excerpt| excerpt.contains("lexical-anchor"))
        );
        let chunk_references =
            hits[0]["chunkReferences"].as_array().context("lexical chunk references missing")?;
        assert_eq!(chunk_references.len(), 1);
        assert_eq!(chunk_references[0]["inclusionReason"], json!("lexical_chunk"));

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres and redis"]
async fn search_documents_returns_structured_content_with_stable_envelope() -> anyhow::Result<()> {
    let settings = Settings::from_env().context("failed to load settings for mcp search test")?;
    let fixture = Box::pin(McpSearchFixture::create(settings)).await?;

    let result = async {
        let token = fixture.bearer_token(&["documents:read"], "search-envelope").await?;
        let _ = fixture
            .create_document_state(
                fixture.primary_library_id,
                "envelope-beacon",
                "ready",
                Some("envelope-anchor text is searchable"),
                &["envelope-anchor text"],
            )
            .await?;

        let response = fixture
            .mcp_tool_call(
                &token,
                "search_documents",
                json!({
                    "query": "envelope-anchor",
                    "libraries": [fixture.primary_library_ref.clone()],
                    "limit": 3
                }),
            )
            .await?;

        let result_value = response.get("result").context("missing result field")?;
        assert_eq!(
            result_value["isError"],
            json!(false),
            "search_documents returned an MCP tool error: {response}",
        );

        let has_structured_content =
            result_value.get("structuredContent").is_some_and(|value| !value.is_null());
        let has_content = result_value.get("content").is_some_and(|value| !value.is_null());
        assert!(has_structured_content || has_content);

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres and redis services"]
async fn a_document_deleted_only_in_the_knowledge_plane_drops_out_instead_of_failing_the_search()
-> anyhow::Result<()> {
    let settings =
        Settings::from_env().context("failed to load settings for plane divergence test")?;
    let fixture = Box::pin(McpSearchFixture::create(settings)).await?;

    let result = async {
        let token = fixture.bearer_token(&["documents:read"], "search-plane-divergence").await?;
        let (healthy_document_id, _) = fixture
            .create_document_state(
                fixture.primary_library_id,
                "divergence-beacon-healthy",
                "readable",
                Some("Healthy divergence-beacon source."),
                &[],
            )
            .await?;
        let (stale_document_id, _) = fixture
            .create_document_state(
                fixture.primary_library_id,
                "divergence-beacon-stale",
                "readable",
                Some("Stale divergence-beacon source."),
                &[],
            )
            .await?;

        // The metadata lane admits documents on the content-plane state while
        // readiness is derived from the knowledge-plane row. Nothing the API
        // offers produces that pair, so it is written directly: the content row
        // stays active and only the knowledge row is retired.
        let retired = sqlx::query(
            "update knowledge_document \
             set document_state = 'deleted', deleted_at = now() \
             where document_id = $1",
        )
        .bind(stale_document_id)
        .execute(&fixture.state.persistence.postgres)
        .await
        .context("failed to retire the knowledge-plane row")?;
        ensure!(
            retired.rows_affected() == 1,
            "expected to retire exactly one knowledge row, retired {}",
            retired.rows_affected()
        );

        let response = fixture
            .mcp_tool_call(
                &token,
                "search_documents",
                json!({
                    "query": "divergence-beacon",
                    "libraries": [fixture.primary_library_ref],
                    "limit": 5,
                    "includeUnreadable": true
                }),
            )
            .await?;
        assert_eq!(
            response["result"]["isError"],
            json!(false),
            "one stale document turned the whole search into an error: {response}",
        );

        let hits = response["result"]["structuredContent"]["hits"]
            .as_array()
            .context("plane divergence search hits missing")?;
        // Asserting the healthy hit survives is what stops an empty result from
        // passing this test for the wrong reason.
        assert!(
            hits.iter().any(|hit| hit["documentId"] == json!(healthy_document_id)),
            "the healthy document disappeared along with the stale one: {response}",
        );
        assert!(
            !hits.iter().any(|hit| hit["documentId"] == json!(stale_document_id)),
            "a document deleted in the knowledge plane was still returned: {response}",
        );

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}
