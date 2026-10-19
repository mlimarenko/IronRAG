//! PostgreSQL integration coverage for the chunk-level graph extraction
//! record repository (`infra::repositories::runtime_graph_repository::extraction`).
//!
//! Run with:
//! `cargo test -p ironrag-backend --test it -- --ignored runtime_graph_extraction_repository::`

use crate::temp_postgres_database;

use anyhow::{Context, Result};
use chrono::Utc;
use sqlx::PgPool;
use uuid::Uuid;

use ironrag_backend::{
    domains::agent_runtime::{RuntimeExecutionOwnerKind, RuntimeLifecycleState, RuntimeTaskKind},
    infra::repositories::{
        catalog_repository,
        runtime_graph_repository::{
            self, CreateRuntimeGraphExtractionRecordInput, UpdateRuntimeGraphExtractionRecordInput,
        },
        runtime_repository::{self, NewRuntimeExecution},
    },
};
use temp_postgres_database::TempPostgresDatabase;

struct Fixture {
    database: TempPostgresDatabase,
    pool: PgPool,
    workspace_id: Uuid,
    library_id: Uuid,
    document_id: Uuid,
    runtime_execution_id: Uuid,
}

impl Fixture {
    async fn create() -> Result<Self> {
        let database_url = std::env::var("IRONRAG_DATABASE_URL")
            .context("IRONRAG_DATABASE_URL must be configured")?;
        let database =
            TempPostgresDatabase::create(&database_url, "runtime_graph_extraction_repository")
                .await?;
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(4)
            .connect(&database.database_url)
            .await
            .context("failed to connect runtime graph extraction repository postgres")?;
        sqlx::migrate!("./migrations")
            .run(&pool)
            .await
            .context("failed to apply runtime graph extraction repository migrations")?;

        let suffix = Uuid::now_v7().simple().to_string();
        let workspace = catalog_repository::create_workspace(
            &pool,
            &format!("runtime-graph-extraction-workspace-{suffix}"),
            "Runtime Graph Extraction Workspace",
            None,
        )
        .await
        .context("failed to create runtime graph extraction workspace")?;
        let library = catalog_repository::create_library(
            &pool,
            workspace.id,
            &format!("runtime-graph-extraction-library-{suffix}"),
            "Runtime Graph Extraction Library",
            None,
            None,
        )
        .await
        .context("failed to create runtime graph extraction library")?;

        let document_id = sqlx::query_scalar::<_, Uuid>(
            "insert into content_document (
                workspace_id, library_id, external_key, document_state, created_at
             ) values ($1, $2, $3, 'active', now())
             returning id",
        )
        .bind(workspace.id)
        .bind(library.id)
        .bind(format!("runtime-graph-extraction-doc-{suffix}"))
        .fetch_one(&pool)
        .await
        .context("failed to insert content_document")?;

        let runtime_execution_id = Uuid::now_v7();
        runtime_repository::create_runtime_execution(
            &pool,
            &NewRuntimeExecution {
                id: runtime_execution_id,
                owner_kind: RuntimeExecutionOwnerKind::GraphExtractionAttempt.as_str(),
                owner_id: Uuid::now_v7(),
                task_kind: RuntimeTaskKind::GraphExtract.as_str(),
                surface_kind: "worker",
                contract_name: "graph_extract",
                contract_version: "runtime-graph-extraction-test",
                lifecycle_state: RuntimeLifecycleState::Running.as_str(),
                active_stage: None,
                turn_budget: 1,
                turn_count: 0,
                parallel_action_limit: 1,
                failure_code: None,
                failure_summary_redacted: None,
                parent_execution_id: None,
            },
        )
        .await
        .context("failed to insert runtime execution for graph extraction fixture")?;

        Ok(Self {
            database,
            pool,
            workspace_id: workspace.id,
            library_id: library.id,
            document_id,
            runtime_execution_id,
        })
    }

    /// Inserts a `content_revision` + `content_chunk` pair scoped to the
    /// fixture's document/library, returning the chunk id. Only the
    /// semantic-cache-key lookup test needs the chunk join target.
    async fn create_chunk(&self, text_checksum: &str) -> Result<Uuid> {
        self.create_chunk_in_revision(text_checksum, 1, 128).await
    }

    /// Inserts revision number `revision_number` of `byte_size` bytes plus one
    /// chunk carrying `text_checksum`. Two calls with the same checksum model a
    /// parent and a longer append revision whose unchanged chunk text must
    /// reuse the earlier ready extraction (IRG-197).
    async fn create_chunk_in_revision(
        &self,
        text_checksum: &str,
        revision_number: i32,
        byte_size: i64,
    ) -> Result<Uuid> {
        let revision_id = sqlx::query_scalar::<_, Uuid>(
            "insert into content_revision (
                document_id, workspace_id, library_id, revision_number, content_source_kind,
                checksum, mime_type, byte_size, title, created_at
             ) values ($1, $2, $3, $4, 'upload', $5, 'text/plain', $6, 'Fixture revision', now())
             returning id",
        )
        .bind(self.document_id)
        .bind(self.workspace_id)
        .bind(self.library_id)
        .bind(revision_number)
        .bind(format!("sha256:revision-{}", Uuid::now_v7().simple()))
        .bind(byte_size)
        .fetch_one(&self.pool)
        .await
        .context("failed to insert content_revision")?;

        sqlx::query_scalar::<_, Uuid>(
            "insert into content_chunk (
                revision_id, chunk_index, start_offset, end_offset, token_count,
                normalized_text, text_checksum
             ) values ($1, 0, 0, 32, 6, 'fixture chunk text', $2)
             returning id",
        )
        .bind(revision_id)
        .bind(text_checksum)
        .fetch_one(&self.pool)
        .await
        .context("failed to insert content_chunk")
    }

    fn new_record_input(
        &self,
        chunk_id: Uuid,
        status: &str,
    ) -> CreateRuntimeGraphExtractionRecordInput {
        CreateRuntimeGraphExtractionRecordInput {
            id: Uuid::now_v7(),
            runtime_execution_id: self.runtime_execution_id,
            library_id: self.library_id,
            document_id: self.document_id,
            chunk_id,
            provider_kind: "openai".to_string(),
            model_name: "gpt-5.4-nano".to_string(),
            extraction_version: "v1".to_string(),
            prompt_hash: "sha256:prompt-a".to_string(),
            status: status.to_string(),
            raw_output_json: serde_json::json!({
                "entities": [],
                "extraction_contract_hash": "sha256:contract-a",
            }),
            normalized_output_json: serde_json::json!({"entities": [], "relations": []}),
            glean_pass_count: 0,
            error_message: None,
        }
    }

    /// A ready record whose stable cache identity is `extraction_contract_hash`.
    /// `prompt_hash` is deliberately irrelevant to the lookup: it is the
    /// audit-only full rendered prompt hash (IRG-197).
    fn new_ready_record_input(
        &self,
        chunk_id: Uuid,
        extraction_contract_hash: &str,
    ) -> CreateRuntimeGraphExtractionRecordInput {
        let mut input = self.new_record_input(chunk_id, "ready");
        input.raw_output_json = serde_json::json!({
            "entities": [],
            "extraction_contract_hash": extraction_contract_hash,
        });
        input
    }

    /// A ready record written before IRG-197: its `raw_output_json` carries no
    /// stable cache identity, only the audit `prompt_hash` column.
    fn new_legacy_ready_record_input(
        &self,
        chunk_id: Uuid,
    ) -> CreateRuntimeGraphExtractionRecordInput {
        let mut input = self.new_record_input(chunk_id, "ready");
        input.raw_output_json = serde_json::json!({"entities": []});
        input
    }

    async fn cleanup(self) -> Result<()> {
        self.pool.close().await;
        self.database.drop().await
    }
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn create_and_fetch_round_trips_every_field() -> Result<()> {
    let fixture = Fixture::create().await?;
    let chunk_id = Uuid::now_v7();
    let input = fixture.new_record_input(chunk_id, "processing");

    let created =
        runtime_graph_repository::create_runtime_graph_extraction_record(&fixture.pool, &input)
            .await
            .context("failed to create runtime graph extraction record")?;

    assert_eq!(created.id, input.id);
    assert_eq!(created.runtime_execution_id, fixture.runtime_execution_id);
    assert_eq!(created.library_id, fixture.library_id);
    assert_eq!(created.document_id, fixture.document_id);
    assert_eq!(created.chunk_id, chunk_id);
    assert_eq!(created.provider_kind, "openai");
    assert_eq!(created.model_name, "gpt-5.4-nano");
    assert_eq!(created.extraction_version, "v1");
    assert_eq!(created.prompt_hash, "sha256:prompt-a");
    assert_eq!(created.status, "processing");
    assert_eq!(created.glean_pass_count, 0);
    assert!(created.error_message.is_none());
    assert!(created.created_at <= Utc::now());

    let fetched = runtime_graph_repository::get_runtime_graph_extraction_record_by_id(
        &fixture.pool,
        created.id,
    )
    .await
    .context("failed to fetch runtime graph extraction record by id")?
    .context("expected the created record to be fetchable by id")?;
    assert_eq!(fetched.id, created.id);
    assert_eq!(fetched.status, "processing");

    let missing = runtime_graph_repository::get_runtime_graph_extraction_record_by_id(
        &fixture.pool,
        Uuid::now_v7(),
    )
    .await
    .context("failed to query a nonexistent runtime graph extraction record")?;
    assert!(missing.is_none());

    fixture.cleanup().await
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn update_replaces_output_status_and_glean_count() -> Result<()> {
    let fixture = Fixture::create().await?;
    let created = runtime_graph_repository::create_runtime_graph_extraction_record(
        &fixture.pool,
        &fixture.new_record_input(Uuid::now_v7(), "processing"),
    )
    .await?;

    let update = UpdateRuntimeGraphExtractionRecordInput {
        provider_kind: "openai".to_string(),
        model_name: "gpt-5.4-nano".to_string(),
        prompt_hash: "sha256:prompt-a".to_string(),
        status: "ready".to_string(),
        raw_output_json: serde_json::json!({"entities": [{"label": "Alpha"}]}),
        normalized_output_json: serde_json::json!({
            "entities": [{"label": "Alpha"}],
            "relations": [],
        }),
        glean_pass_count: 1,
        error_message: None,
    };
    let updated = runtime_graph_repository::update_runtime_graph_extraction_record_safe(
        &fixture.pool,
        created.id,
        &update,
    )
    .await
    .context("failed to update runtime graph extraction record")?
    .context("expected the update to return the updated row")?;

    assert_eq!(updated.status, "ready");
    assert_eq!(updated.glean_pass_count, 1);
    assert_eq!(updated.normalized_output_json["entities"][0]["label"], "Alpha");
    assert!(updated.error_message.is_none());

    let missing_update = runtime_graph_repository::update_runtime_graph_extraction_record_safe(
        &fixture.pool,
        Uuid::now_v7(),
        &update,
    )
    .await
    .context("update on a nonexistent id should not error")?;
    assert!(missing_update.is_none());

    fixture.cleanup().await
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn cancel_processing_only_touches_processing_rows_for_the_target_document() -> Result<()> {
    let fixture = Fixture::create().await?;
    let processing_one = runtime_graph_repository::create_runtime_graph_extraction_record(
        &fixture.pool,
        &fixture.new_record_input(Uuid::now_v7(), "processing"),
    )
    .await?;
    let processing_two = runtime_graph_repository::create_runtime_graph_extraction_record(
        &fixture.pool,
        &fixture.new_record_input(Uuid::now_v7(), "processing"),
    )
    .await?;
    let already_ready = runtime_graph_repository::create_runtime_graph_extraction_record(
        &fixture.pool,
        &fixture.new_record_input(Uuid::now_v7(), "ready"),
    )
    .await?;

    let canceled_count =
        runtime_graph_repository::cancel_processing_runtime_graph_extractions_for_document(
            &fixture.pool,
            fixture.library_id,
            fixture.document_id,
            "revision restarted from a ready checkpoint",
        )
        .await
        .context("failed to cancel processing runtime graph extraction records")?;
    assert_eq!(canceled_count, 2);

    for id in [processing_one.id, processing_two.id] {
        let row =
            runtime_graph_repository::get_runtime_graph_extraction_record_by_id(&fixture.pool, id)
                .await?
                .context("expected the canceled row to still exist")?;
        assert_eq!(row.status, "canceled");
        assert_eq!(
            row.error_message.as_deref(),
            Some("revision restarted from a ready checkpoint"),
        );
    }

    let untouched = runtime_graph_repository::get_runtime_graph_extraction_record_by_id(
        &fixture.pool,
        already_ready.id,
    )
    .await?
    .context("expected the already-ready row to still exist")?;
    assert_eq!(untouched.status, "ready");

    // Idempotent: a second cancel over the same document finds nothing left to cancel.
    let second_pass =
        runtime_graph_repository::cancel_processing_runtime_graph_extractions_for_document(
            &fixture.pool,
            fixture.library_id,
            fixture.document_id,
            "revision restarted from a ready checkpoint",
        )
        .await?;
    assert_eq!(second_pass, 0);

    fixture.cleanup().await
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn list_by_document_scopes_to_the_document_and_extraction_version() -> Result<()> {
    let fixture = Fixture::create().await?;
    let mut current_version_input = fixture.new_record_input(Uuid::now_v7(), "ready");
    current_version_input.extraction_version = "v1".to_string();
    let current_version = runtime_graph_repository::create_runtime_graph_extraction_record(
        &fixture.pool,
        &current_version_input,
    )
    .await?;

    let mut stale_version_input = fixture.new_record_input(Uuid::now_v7(), "ready");
    stale_version_input.extraction_version = "v0-stale".to_string();
    runtime_graph_repository::create_runtime_graph_extraction_record(
        &fixture.pool,
        &stale_version_input,
    )
    .await?;

    let records = runtime_graph_repository::list_runtime_graph_extraction_records_by_document(
        &fixture.pool,
        fixture.document_id,
        "v1",
    )
    .await
    .context("failed to list runtime graph extraction records by document")?;
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].id, current_version.id);

    let unmatched_document =
        runtime_graph_repository::list_runtime_graph_extraction_records_by_document(
            &fixture.pool,
            Uuid::now_v7(),
            "v1",
        )
        .await?;
    assert!(unmatched_document.is_empty());

    fixture.cleanup().await
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn list_by_library_scopes_to_the_library_and_extraction_version() -> Result<()> {
    let fixture = Fixture::create().await?;
    let mut first_input = fixture.new_record_input(Uuid::now_v7(), "ready");
    first_input.extraction_version = "v1".to_string();
    let first = runtime_graph_repository::create_runtime_graph_extraction_record(
        &fixture.pool,
        &first_input,
    )
    .await?;

    let mut second_input = fixture.new_record_input(Uuid::now_v7(), "processing");
    second_input.extraction_version = "v1".to_string();
    let second = runtime_graph_repository::create_runtime_graph_extraction_record(
        &fixture.pool,
        &second_input,
    )
    .await?;

    let records = runtime_graph_repository::list_runtime_graph_extraction_records_by_library(
        &fixture.pool,
        fixture.library_id,
        "v1",
    )
    .await
    .context("failed to list runtime graph extraction records by library")?;
    assert_eq!(records.iter().map(|row| row.id).collect::<Vec<_>>(), vec![first.id, second.id]);

    let unmatched_version =
        runtime_graph_repository::list_runtime_graph_extraction_records_by_library(
            &fixture.pool,
            fixture.library_id,
            "v-does-not-exist",
        )
        .await?;
    assert!(unmatched_version.is_empty());

    fixture.cleanup().await
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn semantic_cache_lookup_requires_the_exact_extraction_contract_hash() -> Result<()> {
    let fixture = Fixture::create().await?;
    let text_checksum = format!("sha256:chunk-{}", Uuid::now_v7().simple());
    let chunk_id = fixture.create_chunk(&text_checksum).await?;

    // Not ready yet: must be ignored by the cache lookup.
    runtime_graph_repository::create_runtime_graph_extraction_record(
        &fixture.pool,
        &fixture.new_record_input(chunk_id, "processing"),
    )
    .await?;

    // Both ready rows carry the same stable contract identity but different
    // audit-only prompt hashes. The cache must no longer key on prompt_hash.
    let mut older_ready_input = fixture.new_ready_record_input(chunk_id, "sha256:contract-current");
    older_ready_input.prompt_hash = "sha256:prompt-old".to_string();
    let older_ready = runtime_graph_repository::create_runtime_graph_extraction_record(
        &fixture.pool,
        &older_ready_input,
    )
    .await?;

    let mut current_contract_input =
        fixture.new_ready_record_input(chunk_id, "sha256:contract-current");
    current_contract_input.prompt_hash = "sha256:prompt-current".to_string();
    let current_contract = runtime_graph_repository::create_runtime_graph_extraction_record(
        &fixture.pool,
        &current_contract_input,
    )
    .await?;

    let hit =
        runtime_graph_repository::find_ready_runtime_graph_extraction_record_by_semantic_cache_key(
            &fixture.pool,
            runtime_graph_repository::RuntimeGraphExtractionSemanticCacheKey {
                library_id: fixture.library_id,
                text_checksum: &text_checksum,
                extraction_version: "v1",
                provider_kind: "openai",
                model_name: "gpt-5.4-nano",
                extraction_contract_hash: "sha256:contract-current",
            },
        )
        .await
        .context("failed to look up the semantic cache key")?
        .context("expected a ready cache hit")?;
    assert_eq!(hit.id, current_contract.id);
    assert!(
        hit.id == current_contract.id || hit.id == older_ready.id,
        "either row is the same contract identity"
    );
    // The returned row is the caller's own chunk, so the materialization plan
    // keys its `reused_chunk_ids` on a chunk the provider stage would otherwise
    // pay for: this is the persistence half of the zero-provider-work boundary.
    assert_eq!(hit.chunk_id, chunk_id);

    // A contract identity nobody has ready must invalidate, never fall back.
    let drifted =
        runtime_graph_repository::find_ready_runtime_graph_extraction_record_by_semantic_cache_key(
            &fixture.pool,
            runtime_graph_repository::RuntimeGraphExtractionSemanticCacheKey {
                library_id: fixture.library_id,
                text_checksum: &text_checksum,
                extraction_version: "v1",
                provider_kind: "openai",
                model_name: "gpt-5.4-nano",
                extraction_contract_hash: "sha256:contract-never-seen",
            },
        )
        .await?;
    assert!(drifted.is_none(), "contract drift must invalidate the cache entry");

    // A different extraction version must never hit this chunk's cache entries.
    let wrong_version =
        runtime_graph_repository::find_ready_runtime_graph_extraction_record_by_semantic_cache_key(
            &fixture.pool,
            runtime_graph_repository::RuntimeGraphExtractionSemanticCacheKey {
                library_id: fixture.library_id,
                text_checksum: &text_checksum,
                extraction_version: "v2",
                provider_kind: "openai",
                model_name: "gpt-5.4-nano",
                extraction_contract_hash: "sha256:contract-current",
            },
        )
        .await?;
    assert!(wrong_version.is_none());

    // A different provider must never hit this chunk's cache entries.
    let wrong_provider =
        runtime_graph_repository::find_ready_runtime_graph_extraction_record_by_semantic_cache_key(
            &fixture.pool,
            runtime_graph_repository::RuntimeGraphExtractionSemanticCacheKey {
                library_id: fixture.library_id,
                text_checksum: &text_checksum,
                extraction_version: "v1",
                provider_kind: "azure",
                model_name: "gpt-5.4-nano",
                extraction_contract_hash: "sha256:contract-current",
            },
        )
        .await?;
    assert!(wrong_provider.is_none());

    // A different model must never hit this chunk's cache entries.
    let wrong_model =
        runtime_graph_repository::find_ready_runtime_graph_extraction_record_by_semantic_cache_key(
            &fixture.pool,
            runtime_graph_repository::RuntimeGraphExtractionSemanticCacheKey {
                library_id: fixture.library_id,
                text_checksum: &text_checksum,
                extraction_version: "v1",
                provider_kind: "openai",
                model_name: "gpt-5.6-luna",
                extraction_contract_hash: "sha256:contract-current",
            },
        )
        .await?;
    assert!(wrong_model.is_none());

    fixture.cleanup().await
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn semantic_cache_reuses_an_unchanged_chunk_across_revisions() -> Result<()> {
    let fixture = Fixture::create().await?;
    let text_checksum = format!("sha256:chunk-{}", Uuid::now_v7().simple());

    // Parent revision: extracted and ready under the stable contract.
    let parent_chunk = fixture.create_chunk_in_revision(&text_checksum, 1, 128).await?;
    let parent_ready = runtime_graph_repository::create_runtime_graph_extraction_record(
        &fixture.pool,
        &fixture.new_ready_record_input(parent_chunk, "sha256:contract-stable"),
    )
    .await?;

    // A longer append revision of the same document carries the identical chunk
    // text. Its ordinal and revision id differ, yet the ready extraction from the
    // parent must answer the lookup.
    let child_chunk = fixture.create_chunk_in_revision(&text_checksum, 2, 4096).await?;
    assert_ne!(parent_chunk, child_chunk);

    let hit =
        runtime_graph_repository::find_ready_runtime_graph_extraction_record_by_semantic_cache_key(
            &fixture.pool,
            runtime_graph_repository::RuntimeGraphExtractionSemanticCacheKey {
                library_id: fixture.library_id,
                text_checksum: &text_checksum,
                extraction_version: "v1",
                provider_kind: "openai",
                model_name: "gpt-5.4-nano",
                extraction_contract_hash: "sha256:contract-stable",
            },
        )
        .await?
        .context("the identical chunk text in a new revision must reuse the parent extraction")?;
    assert_eq!(hit.id, parent_ready.id);
    assert_eq!(hit.chunk_id, parent_chunk);

    // The next contract version must not reuse the parent output.
    let next_contract =
        runtime_graph_repository::find_ready_runtime_graph_extraction_record_by_semantic_cache_key(
            &fixture.pool,
            runtime_graph_repository::RuntimeGraphExtractionSemanticCacheKey {
                library_id: fixture.library_id,
                text_checksum: &text_checksum,
                extraction_version: "v1",
                provider_kind: "openai",
                model_name: "gpt-5.4-nano",
                extraction_contract_hash: "sha256:contract-next",
            },
        )
        .await?;
    assert!(next_contract.is_none());

    fixture.cleanup().await
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn semantic_cache_lookup_ignores_a_legacy_row_without_the_contract_identity() -> Result<()> {
    let fixture = Fixture::create().await?;
    let text_checksum = format!("sha256:chunk-{}", Uuid::now_v7().simple());
    let chunk_id = fixture.create_chunk(&text_checksum).await?;

    // Ready, but written before the stable identity existed: its raw output has
    // no extraction_contract_hash. The lookup must miss cleanly — no error, and
    // no fallback to a row whose contract cannot be established.
    runtime_graph_repository::create_runtime_graph_extraction_record(
        &fixture.pool,
        &fixture.new_legacy_ready_record_input(chunk_id),
    )
    .await?;

    let hit =
        runtime_graph_repository::find_ready_runtime_graph_extraction_record_by_semantic_cache_key(
            &fixture.pool,
            runtime_graph_repository::RuntimeGraphExtractionSemanticCacheKey {
                library_id: fixture.library_id,
                text_checksum: &text_checksum,
                extraction_version: "v1",
                provider_kind: "openai",
                model_name: "gpt-5.4-nano",
                extraction_contract_hash: "sha256:contract-stable",
            },
        )
        .await
        .context("a legacy row must not turn the cache lookup into an error")?;
    assert!(hit.is_none(), "a legacy row without the contract identity must not be reused");

    fixture.cleanup().await
}
