use std::{
    collections::{BTreeSet, HashMap},
    sync::{Arc, Mutex as StdMutex, Weak},
};

use anyhow::{Context, Result as AnyhowResult, anyhow};
use chrono::Utc;
use futures::stream::{self, StreamExt};
use sha2::{Digest, Sha256};
use sqlx::{Connection, PgConnection};
use tokio::sync::{
    Mutex as AsyncMutex, OwnedMutexGuard, OwnedRwLockReadGuard, OwnedRwLockWriteGuard, RwLock,
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::{
    error::QueryServiceError,
    vector_dimensions::{
        EmbeddingDimensionState, EmbeddingProfileIndexState, ensure_active_embedding_profile_key,
        inspect_library_embedding_profile_indexed_uncached,
        invalidate_library_embedding_profile_inventory, invalidate_library_vector_index_dimensions,
        library_vector_index_dimension_state_for_binding,
        library_vector_index_dimensions_for_binding, validate_embedding_batch_dimensions,
        validate_embedding_vector_dimensions,
    },
};
use crate::{
    app::state::AppState,
    domains::ai::AiBindingPurpose,
    domains::query_ir::QueryIR,
    infra::{
        knowledge_plane::{
            CanonicalIngestVectorWriteFence, CanonicalVectorWriteFence,
            VECTOR_REBUILD_STAGING_PROFILE_PREFIX,
        },
        knowledge_rows::{
            KNOWLEDGE_CHUNK_VECTOR_KIND, KNOWLEDGE_ENTITY_VECTOR_KIND, KnowledgeChunkRow,
            KnowledgeChunkSearchRow, KnowledgeChunkVectorRow, KnowledgeEntityRow,
            KnowledgeEntitySearchRow, KnowledgeEntityVectorRow, KnowledgeRelationSearchRow,
            KnowledgeTechnicalFactSearchRow,
        },
        repositories::{ai_repository, catalog_repository, content_repository},
    },
    integrations::llm::{EmbeddingBatchRequest, EmbeddingBatchResponse},
    services::{
        ai_catalog_service::{EmbeddingDimensions, ResolvedRuntimeBinding},
        ingest::{
            cancellation::{StageError, anyhow_is_cancelled, ensure_not_cancelled},
            service::{INGEST_STAGE_EMBED_CHUNK, RecordStageUnitProgressCommand},
        },
    },
};

/// Per-batch size used for chunk embedding requests. Keeps each call below
/// the typical 8k-token provider soft cap even when chunks run long and
/// reduces the blast radius of one bad chunk failing the whole revision.
const CHUNK_EMBEDDING_BATCH_SIZE: usize = 16;
const CHUNK_VECTOR_REUSE_SOURCE_BATCH_SIZE: usize = 128;
const CHUNK_REBUILD_FETCH_PAGE_SIZE: usize = 2_000;
const FACT_FETCH_MULTIPLIER: usize = 2;
const FACT_FETCH_MIN: usize = 6;
const VECTOR_PLANE_REBUILD_ADVISORY_LOCK_KEY: &str = "query.vector_plane.rebuild.library";
type ChunkEmbeddingBatch = Vec<usize>;

struct RevisionChunkEmbeddingBatchWrite<'a> {
    embedding_profile_key: &'a str,
    freshness_generation: i64,
    expected_dimensions: Option<EmbeddingDimensions>,
    expected_source_truth_version: i64,
    ingest_attempt: CanonicalIngestVectorWriteFence,
}

#[derive(Debug, Default)]
struct RevisionVectorCoverage {
    chunk_ids: BTreeSet<Uuid>,
}

fn take_dimension_learning_batch(
    observed_dimensions: Option<EmbeddingDimensions>,
    batches: &mut Vec<ChunkEmbeddingBatch>,
) -> Option<ChunkEmbeddingBatch> {
    if observed_dimensions.is_some() || batches.is_empty() {
        return None;
    }
    Some(batches.remove(0))
}

struct RevisionChunkEmbeddingRun<'a> {
    service: &'a SearchService,
    state: &'a AppState,
    library_id: Uuid,
    revision_id: Uuid,
    attempt_id: Uuid,
    source_truth_version: i64,
    cancellation_token: &'a CancellationToken,
    binding: &'a ResolvedRuntimeBinding,
    embedding_profile_key: &'a str,
    freshness_generation: i64,
    ingest_attempt: CanonicalIngestVectorWriteFence,
    total_chunk_count: usize,
    reused_chunk_count: usize,
}

impl RevisionChunkEmbeddingRun<'_> {
    async fn resolve_request_result(
        &self,
        cleanup_owned_vector_ids: &BTreeSet<Uuid>,
        result: AnyhowResult<(Vec<usize>, EmbeddingBatchResponse)>,
    ) -> std::result::Result<(Vec<usize>, EmbeddingBatchResponse), QueryServiceError> {
        match result {
            Ok(result) => Ok(result),
            Err(error) if anyhow_is_cancelled(&error) => {
                fail_embed_chunks_cancelled(self.revision_id)
            }
            Err(error) => {
                fail_embed_chunks_by_typed_policy(
                    self.state,
                    self.revision_id,
                    self.source_truth_version,
                    self.ingest_attempt,
                    cleanup_owned_vector_ids,
                    error,
                )
                .await
            }
        }
    }

    async fn persist_batch(
        &self,
        chunks: &[&KnowledgeChunkRow],
        batch: &[usize],
        response: &EmbeddingBatchResponse,
        expected_dimensions: Option<EmbeddingDimensions>,
        cleanup_owned_vector_ids: &mut BTreeSet<Uuid>,
    ) -> std::result::Result<(usize, EmbeddingDimensions), QueryServiceError> {
        let result = self
            .service
            .persist_revision_chunk_embedding_batch(
                self.state,
                self.library_id,
                chunks,
                batch,
                response,
                RevisionChunkEmbeddingBatchWrite {
                    embedding_profile_key: self.embedding_profile_key,
                    freshness_generation: self.freshness_generation,
                    expected_dimensions,
                    expected_source_truth_version: self.source_truth_version,
                    ingest_attempt: self.ingest_attempt,
                },
                cleanup_owned_vector_ids,
                self.cancellation_token,
            )
            .await;
        match result {
            Ok(result) => Ok(result),
            Err(error) => {
                fail_embed_chunks_by_typed_policy(
                    self.state,
                    self.revision_id,
                    self.source_truth_version,
                    self.ingest_attempt,
                    cleanup_owned_vector_ids,
                    error,
                )
                .await
            }
        }
    }

    async fn cleanup_error<T>(
        &self,
        cleanup_owned_vector_ids: &BTreeSet<Uuid>,
        result: AnyhowResult<T>,
    ) -> std::result::Result<T, QueryServiceError> {
        match result {
            Ok(value) => Ok(value),
            Err(error) => {
                fail_embed_chunks_after_cleanup(
                    self.state,
                    self.revision_id,
                    self.source_truth_version,
                    self.ingest_attempt,
                    cleanup_owned_vector_ids,
                    error,
                )
                .await
            }
        }
    }

    async fn sync_progress(&self, chunks_embedded: usize) {
        sync_embed_chunk_stage_progress(
            self.state,
            self.attempt_id,
            chunks_embedded.saturating_add(self.reused_chunk_count),
            self.total_chunk_count,
        )
        .await;
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ChunkEmbeddingWrite {
    pub chunk_id: Uuid,
    /// Exact execution-profile provenance for the supplied vector. A model id
    /// is insufficient because endpoint, API style, runtime path, and request
    /// parameters may produce a different vector space at the same dimension.
    pub embedding_profile_key: String,
    pub embedding_vector: Vec<f32>,
}

/// Outcome of an ingest-time chunk-embed call for a single revision.
/// Feeds the `embed_chunk` stage event (chunk count, elapsed, billing).
#[derive(Debug, Clone, Default)]
pub struct EmbedChunksStageOutcome {
    /// Exact profile used for every embedded/reused vector in this revision.
    /// Readiness callers must re-resolve and compare it immediately before
    /// promoting the revision.
    pub embedding_profile_key: Option<String>,
    pub chunks_embedded: usize,
    pub chunks_reused: usize,
    pub usage_json: Option<serde_json::Value>,
    pub provider_kind: Option<String>,
    pub model_name: Option<String>,
    pub prompt_tokens: Option<i32>,
    pub completion_tokens: Option<i32>,
    pub total_tokens: Option<i32>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VectorPlaneRebuildOutcome {
    pub previous_dimensions: Option<u64>,
    /// `None` is the healthy empty-library result: no provider request or
    /// synthetic vector lane was needed.
    pub target_dimensions: Option<u64>,
    pub indexes_recreated: bool,
    pub libraries_rebuilt: usize,
    pub chunk_embeddings_rebuilt: usize,
    pub graph_node_embeddings_rebuilt: usize,
}

struct ChunkEmbeddingRebuildOutcome {
    rebuilt: usize,
    touched_revision_ids: BTreeSet<Uuid>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GraphNodeEmbeddingWrite {
    pub node_id: Uuid,
    pub embedding_profile_key: String,
    pub embedding_vector: Vec<f32>,
}

#[derive(Debug, Clone)]
pub struct QueryEvidenceSearchResult {
    pub chunk_hits: Vec<KnowledgeChunkSearchRow>,
    pub technical_fact_hits: Vec<KnowledgeTechnicalFactSearchRow>,
    pub entity_hits: Vec<KnowledgeEntitySearchRow>,
    pub relation_hits: Vec<KnowledgeRelationSearchRow>,
    pub exact_literal_bias: bool,
}

#[derive(Clone)]
pub struct SearchService {
    vector_plane_locks: Arc<StdMutex<HashMap<Uuid, Weak<RwLock<()>>>>>,
    vector_rebuild_locks: Arc<StdMutex<HashMap<Uuid, Weak<AsyncMutex<()>>>>>,
}

pub(crate) struct VectorPlaneReadGuard {
    _local: OwnedRwLockReadGuard<()>,
}

pub(crate) struct VectorPlaneWriteGuard {
    _local: OwnedRwLockWriteGuard<()>,
}

struct VectorPlaneRebuildGuard {
    _local: OwnedMutexGuard<()>,
    _connection: PgConnection,
}

#[derive(Default)]
struct EmbeddingUsageTotals {
    prompt_tokens: i64,
    completion_tokens: i64,
    total_tokens: i64,
    saw_prompt: bool,
    saw_completion: bool,
    saw_total: bool,
}

impl EmbeddingUsageTotals {
    fn record(&mut self, usage_json: &serde_json::Value) {
        if let Some(value) = usage_json.get("prompt_tokens").and_then(serde_json::Value::as_i64) {
            self.prompt_tokens = self.prompt_tokens.saturating_add(value);
            self.saw_prompt = true;
        }
        if let Some(value) = usage_json.get("completion_tokens").and_then(serde_json::Value::as_i64)
        {
            self.completion_tokens = self.completion_tokens.saturating_add(value);
            self.saw_completion = true;
        }
        if let Some(value) = usage_json.get("total_tokens").and_then(serde_json::Value::as_i64) {
            self.total_tokens = self.total_tokens.saturating_add(value);
            self.saw_total = true;
        }
    }
}

impl Default for SearchService {
    fn default() -> Self {
        Self::new()
    }
}

async fn sync_embed_chunk_stage_progress(
    state: &AppState,
    attempt_id: Uuid,
    completed_units: usize,
    total_units: usize,
) {
    if total_units == 0 {
        return;
    }
    let command = RecordStageUnitProgressCommand {
        attempt_id,
        stage_name: INGEST_STAGE_EMBED_CHUNK.to_string(),
        completed_units: u32::try_from(completed_units).unwrap_or(u32::MAX),
        total_units: u32::try_from(total_units).unwrap_or(u32::MAX),
        details_json: serde_json::json!({}),
    };
    if let Err(error) =
        state.canonical_services.ingest.record_stage_unit_progress(state, command).await
    {
        tracing::warn!(
            attempt_id = %attempt_id,
            ?error,
            "failed to sync embed_chunk stage progress"
        );
    }
}

async fn request_revision_chunk_embedding_batch(
    state: &AppState,
    revision_id: Uuid,
    chunks: &[&KnowledgeChunkRow],
    batch: Vec<usize>,
    binding: &ResolvedRuntimeBinding,
    cancellation_token: &CancellationToken,
) -> AnyhowResult<(Vec<usize>, EmbeddingBatchResponse)> {
    ensure_not_cancelled(cancellation_token)?;
    let inputs = batch.iter().map(|index| chunks[*index].normalized_text.clone()).collect();
    let first_offset = batch.first().copied().unwrap_or_default();
    let request = EmbeddingBatchRequest {
        provider_kind: binding.provider_kind.clone(),
        model_name: binding.model_name.clone(),
        inputs,
        api_key_override: binding.api_key.clone(),
        base_url_override: binding.provider_base_url.clone(),
        extra_parameters_json: binding.extra_parameters_json.clone(),
    };
    let response = tokio::select! {
        _ = cancellation_token.cancelled() => {
            return Err(anyhow::Error::new(StageError::Cancelled));
        }
        result = state.llm_gateway.embed_many(request) => result.with_context(|| {
            format!(
                "failed to embed chunk batch for revision {revision_id} starting at offset {first_offset}"
            )
        })?,
    };
    ensure_not_cancelled(cancellation_token)?;
    Ok((batch, response))
}

async fn resolve_reused_revision_chunk_ids(
    service: &SearchService,
    state: &AppState,
    library_id: Uuid,
    revision_id: Uuid,
    chunks: &[KnowledgeChunkRow],
    binding: &ResolvedRuntimeBinding,
    embedding_profile_key: &str,
    freshness_generation: i64,
    source_truth_version: i64,
    ingest_attempt: CanonicalIngestVectorWriteFence,
    cleanup_owned_vector_ids: &mut BTreeSet<Uuid>,
) -> std::result::Result<(EmbeddingDimensionState, BTreeSet<Uuid>), QueryServiceError> {
    let _vector_guard = service.vector_plane_read_guard(state, library_id).await?;
    let dimension_state =
        library_vector_index_dimension_state_for_binding(state, library_id, binding).await?;
    let EmbeddingDimensionState::Known(expected_dimensions) = dimension_state else {
        return Ok((dimension_state, BTreeSet::new()));
    };
    let coverage = load_current_revision_chunk_vector_ids(
        state,
        revision_id,
        chunks,
        embedding_profile_key,
        freshness_generation,
        expected_dimensions.get(),
    )
    .await;
    let mut reused_chunk_ids = match coverage {
        Ok(coverage) => coverage.chunk_ids,
        Err(error) => {
            return fail_embed_chunks_after_cleanup(
                state,
                revision_id,
                source_truth_version,
                ingest_attempt,
                cleanup_owned_vector_ids,
                error,
            )
            .await;
        }
    };
    if !reused_chunk_ids.is_empty() {
        tracing::info!(
            revision_id = %revision_id,
            reused = reused_chunk_ids.len(),
            total_chunks = chunks.len(),
            "embed_chunk resume: reusing current revision chunk vectors",
        );
    }
    let chunks_missing_current_vectors = chunks
        .iter()
        .filter(|chunk| !reused_chunk_ids.contains(&chunk.chunk_id))
        .cloned()
        .collect::<Vec<_>>();
    let parent_reuse = reuse_chunk_vectors_from_parent_revision(
        state,
        revision_id,
        &chunks_missing_current_vectors,
        embedding_profile_key,
        freshness_generation,
        expected_dimensions.get(),
        source_truth_version,
        ingest_attempt,
        cleanup_owned_vector_ids,
    )
    .await;
    match parent_reuse {
        Ok(parent_reused_chunk_ids) => reused_chunk_ids.extend(parent_reused_chunk_ids),
        Err(error) => {
            return fail_embed_chunks_after_cleanup(
                state,
                revision_id,
                source_truth_version,
                ingest_attempt,
                cleanup_owned_vector_ids,
                error,
            )
            .await;
        }
    }
    Ok((dimension_state, reused_chunk_ids))
}

fn revision_chunk_embedding_batches(chunk_count: usize) -> Vec<ChunkEmbeddingBatch> {
    (0..chunk_count)
        .step_by(CHUNK_EMBEDDING_BATCH_SIZE)
        .map(|start| {
            let end = start.saturating_add(CHUNK_EMBEDDING_BATCH_SIZE).min(chunk_count);
            (start..end).collect()
        })
        .collect()
}

async fn learn_revision_embedding_dimensions(
    run: &RevisionChunkEmbeddingRun<'_>,
    chunks: &[&KnowledgeChunkRow],
    batches: &mut Vec<ChunkEmbeddingBatch>,
    observed_dimensions: Option<EmbeddingDimensions>,
    usage: &mut EmbeddingUsageTotals,
    cleanup_owned_vector_ids: &mut BTreeSet<Uuid>,
) -> std::result::Result<(EmbeddingDimensions, usize), QueryServiceError> {
    if let Some(dimensions) = observed_dimensions {
        return Ok((dimensions, 0));
    }
    let first_batch = take_dimension_learning_batch(observed_dimensions, batches).ok_or_else(|| {
        QueryServiceError::StateConflict {
            message: format!(
                "library {} has chunks but no real embedding batch was available to establish its vector dimension",
                run.library_id
            ),
        }
    })?;
    let request = request_revision_chunk_embedding_batch(
        run.state,
        run.revision_id,
        chunks,
        first_batch,
        run.binding,
        run.cancellation_token,
    )
    .await;
    let (first_batch, response) =
        run.resolve_request_result(cleanup_owned_vector_ids, request).await?;
    let (persisted, learned_dimensions) =
        run.persist_batch(chunks, &first_batch, &response, None, cleanup_owned_vector_ids).await?;
    usage.record(&response.usage_json);
    run.sync_progress(persisted).await;
    fail_embed_chunks_if_cancelled(run.revision_id, run.cancellation_token)?;
    Ok((learned_dimensions, persisted))
}

async fn persist_revision_embedding_batches(
    run: &RevisionChunkEmbeddingRun<'_>,
    chunks: &[&KnowledgeChunkRow],
    batches: Vec<ChunkEmbeddingBatch>,
    expected_dimensions: EmbeddingDimensions,
    parallelism: usize,
    initial_chunks_embedded: usize,
    usage: &mut EmbeddingUsageTotals,
    cleanup_owned_vector_ids: &mut BTreeSet<Uuid>,
) -> std::result::Result<usize, QueryServiceError> {
    let batch_responses = stream::iter(batches.into_iter().map(|batch| {
        request_revision_chunk_embedding_batch(
            run.state,
            run.revision_id,
            chunks,
            batch,
            run.binding,
            run.cancellation_token,
        )
    }))
    .buffer_unordered(parallelism)
    .collect::<Vec<_>>()
    .await;
    let mut chunks_embedded = initial_chunks_embedded;
    for batch_result in batch_responses {
        fail_embed_chunks_if_cancelled(run.revision_id, run.cancellation_token)?;
        let (batch, response) =
            run.resolve_request_result(cleanup_owned_vector_ids, batch_result).await?;
        let (persisted, _) = run
            .persist_batch(
                chunks,
                &batch,
                &response,
                Some(expected_dimensions),
                cleanup_owned_vector_ids,
            )
            .await?;
        usage.record(&response.usage_json);
        chunks_embedded = chunks_embedded.saturating_add(persisted);
        if persisted > 0 {
            run.sync_progress(chunks_embedded).await;
            fail_embed_chunks_if_cancelled(run.revision_id, run.cancellation_token)?;
        }
    }
    Ok(chunks_embedded)
}

async fn verify_revision_embedding_coverage(
    run: &RevisionChunkEmbeddingRun<'_>,
    chunks_embedded: usize,
    cleanup_owned_vector_ids: &BTreeSet<Uuid>,
) -> std::result::Result<(), QueryServiceError> {
    fail_embed_chunks_if_cancelled(run.revision_id, run.cancellation_token)?;
    if !embed_coverage_is_complete(chunks_embedded, run.reused_chunk_count, run.total_chunk_count) {
        return run
            .cleanup_error(
                cleanup_owned_vector_ids,
                Err(anyhow!(
                    "embedding coverage mismatch for revision {}: {} chunks, {} embedded, {} reused",
                    run.revision_id,
                    run.total_chunk_count,
                    chunks_embedded,
                    run.reused_chunk_count,
                )),
            )
            .await;
    }
    let persisted_vector_count = run
        .cleanup_error(
            cleanup_owned_vector_ids,
            run.state
                .search_store
                .count_chunk_vectors_by_revision(
                    run.revision_id,
                    run.embedding_profile_key,
                    KNOWLEDGE_CHUNK_VECTOR_KIND,
                    run.freshness_generation,
                )
                .await
                .with_context(|| {
                    format!(
                        "failed to verify persisted chunk-vector coverage for revision {}",
                        run.revision_id
                    )
                }),
        )
        .await?;
    ensure_not_cancelled(run.cancellation_token)?;
    if persisted_vector_count == run.total_chunk_count {
        return Ok(());
    }
    run.cleanup_error(
        cleanup_owned_vector_ids,
        Err(anyhow!(
            "persisted chunk-vector coverage mismatch for revision {}: {} chunks, {} current vectors",
            run.revision_id,
            run.total_chunk_count,
            persisted_vector_count,
        )),
    )
    .await
}

fn embedding_usage_token_counts(
    usage: &EmbeddingUsageTotals,
) -> (Option<i32>, Option<i32>, Option<i32>) {
    let prompt_tokens =
        usage.saw_prompt.then(|| i32::try_from(usage.prompt_tokens).unwrap_or(i32::MAX));
    let completion_tokens =
        usage.saw_completion.then(|| i32::try_from(usage.completion_tokens).unwrap_or(i32::MAX));
    let total_tokens = if usage.saw_total {
        Some(i32::try_from(usage.total_tokens).unwrap_or(i32::MAX))
    } else if usage.saw_prompt || usage.saw_completion {
        Some(
            i32::try_from(usage.prompt_tokens.saturating_add(usage.completion_tokens))
                .unwrap_or(i32::MAX),
        )
    } else {
        None
    };
    (prompt_tokens, completion_tokens, total_tokens)
}

async fn acquire_vector_rebuild_advisory_lock(
    database_url: &str,
    library_id: Uuid,
) -> AnyhowResult<PgConnection> {
    let mut connection = PgConnection::connect(database_url)
        .await
        .context("connect dedicated vector rebuild lock session")?;
    sqlx::query("select pg_advisory_lock(hashtextextended($1::text, 0))")
        .bind(format!("{VECTOR_PLANE_REBUILD_ADVISORY_LOCK_KEY}:{library_id}"))
        .execute(&mut connection)
        .await
        .context("acquire vector rebuild advisory lock")?;
    Ok(connection)
}

impl SearchService {
    #[must_use]
    pub fn new() -> Self {
        Self {
            vector_plane_locks: Arc::new(StdMutex::new(HashMap::new())),
            vector_rebuild_locks: Arc::new(StdMutex::new(HashMap::new())),
        }
    }

    fn vector_plane_lock(&self, library_id: Uuid) -> Arc<RwLock<()>> {
        let mut locks =
            self.vector_plane_locks.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        locks.retain(|_, lock| lock.strong_count() > 0);
        if let Some(lock) = locks.get(&library_id).and_then(Weak::upgrade) {
            return lock;
        }
        let lock = Arc::new(RwLock::new(()));
        locks.insert(library_id, Arc::downgrade(&lock));
        lock
    }

    fn vector_rebuild_lock(&self, library_id: Uuid) -> Arc<AsyncMutex<()>> {
        let mut locks =
            self.vector_rebuild_locks.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        locks.retain(|_, lock| lock.strong_count() > 0);
        if let Some(lock) = locks.get(&library_id).and_then(Weak::upgrade) {
            return lock;
        }
        let lock = Arc::new(AsyncMutex::new(()));
        locks.insert(library_id, Arc::downgrade(&lock));
        lock
    }

    pub(crate) async fn vector_plane_read_guard(
        &self,
        _state: &AppState,
        library_id: Uuid,
    ) -> AnyhowResult<VectorPlaneReadGuard> {
        let local = self.vector_plane_lock(library_id).read_owned().await;
        Ok(VectorPlaneReadGuard { _local: local })
    }

    pub(crate) async fn vector_plane_write_guard(
        &self,
        _state: &AppState,
        library_id: Uuid,
    ) -> AnyhowResult<VectorPlaneWriteGuard> {
        let local = self.vector_plane_lock(library_id).write_owned().await;
        Ok(VectorPlaneWriteGuard { _local: local })
    }

    async fn vector_rebuild_guard(
        &self,
        state: &AppState,
        library_id: Uuid,
    ) -> AnyhowResult<VectorPlaneRebuildGuard> {
        // Acquire the process-local lock before any network resource. The
        // advisory lock lives on a dedicated session rather than a pooled idle
        // transaction: rebuild work may freely use a one-connection app pool,
        // while dropping this guard closes the session and releases the lock.
        let local = self.vector_rebuild_lock(library_id).lock_owned().await;
        let connection =
            acquire_vector_rebuild_advisory_lock(&state.settings.database_url, library_id).await?;
        Ok(VectorPlaneRebuildGuard { _local: local, _connection: connection })
    }

    async fn promote_staged_vector_rebuild_under_write_guard(
        &self,
        state: &AppState,
        library_id: Uuid,
        dimensions: u64,
        canonical_embedding_model_key: &str,
        staging_embedding_model_key: &str,
        expected_source_truth_version: i64,
        expected_chunk_count: Option<u64>,
        expected_entity_count: Option<u64>,
    ) -> std::result::Result<(), QueryServiceError> {
        let _promotion_guard = self
            .vector_plane_write_guard(state, library_id)
            .await
            .context("failed to acquire the vector promotion write guard")?;
        state
            .search_store
            .promote_staged_vector_rebuild(
                library_id,
                dimensions,
                canonical_embedding_model_key,
                staging_embedding_model_key,
                expected_source_truth_version,
                expected_chunk_count,
                expected_entity_count,
            )
            .await
            .context("failed to atomically promote staged vector lanes")?;
        Ok(())
    }

    async fn purge_empty_vector_plane_under_write_guard(
        &self,
        state: &AppState,
        library_id: Uuid,
        expected_source_truth_version: i64,
    ) -> std::result::Result<u64, QueryServiceError> {
        let _purge_guard = self
            .vector_plane_write_guard(state, library_id)
            .await
            .context("failed to acquire empty vector-plane purge guard")?;
        let deleted = state
            .search_store
            .purge_empty_library_vector_plane(library_id, expected_source_truth_version)
            .await
            .context("failed to atomically purge empty library vector plane")?;
        invalidate_library_vector_index_dimensions(library_id);
        invalidate_library_embedding_profile_inventory(library_id);
        Ok(deleted)
    }

    pub async fn search_query_evidence(
        &self,
        state: &AppState,
        library_id: Uuid,
        query: &str,
        query_ir: &QueryIR,
        limit: usize,
    ) -> std::result::Result<QueryEvidenceSearchResult, QueryServiceError> {
        let normalized_limit = limit.max(1);
        // Bias fact retrieval for exact-literal technical asks (known URLs /
        // paths / ports / config keys). Signal comes straight from the
        // compiled IR — `QueryAct::RetrieveValue` with at least one literal
        // constraint — instead of re-scanning the raw query for
        // hand-maintained marker strings.
        let exact_literal_bias = query_ir.is_exact_literal_technical();
        let fact_limit = if exact_literal_bias {
            normalized_limit.saturating_mul(FACT_FETCH_MULTIPLIER).max(FACT_FETCH_MIN)
        } else {
            normalized_limit
        };
        let (temporal_start, temporal_end) = query_ir.resolved_temporal_bounds();
        let chunk_hits = state
            .search_store
            .search_chunks(library_id, query, normalized_limit, temporal_start, temporal_end)
            .await
            .context("failed to search canonical knowledge chunks")?;
        let technical_fact_hits = state
            .search_store
            .search_technical_facts(library_id, query, fact_limit)
            .await
            .context("failed to search canonical technical facts")?;
        let entity_hits = state
            .search_store
            .search_entities(library_id, query, normalized_limit)
            .await
            .context("failed to search canonical entities")?;
        let relation_hits = state
            .search_store
            .search_relations(library_id, query, normalized_limit)
            .await
            .context("failed to search canonical relations")?;
        Ok(QueryEvidenceSearchResult {
            chunk_hits,
            technical_fact_hits,
            entity_hits,
            relation_hits,
            exact_literal_bias,
        })
    }

    pub async fn resolve_embedding_model_catalog_id(
        &self,
        state: &AppState,
        provider_kind: &str,
        model_name: &str,
    ) -> std::result::Result<Uuid, QueryServiceError> {
        resolve_embedding_model_catalog_id(state, provider_kind, model_name)
            .await
            .map_err(Into::into)
    }

    pub async fn persist_chunk_embeddings(
        &self,
        state: &AppState,
        writes: &[ChunkEmbeddingWrite],
    ) -> std::result::Result<usize, QueryServiceError> {
        let mut written = 0usize;
        for write in writes {
            let chunk = load_knowledge_chunk(state, write.chunk_id).await?;
            let _vector_guard = self.vector_plane_read_guard(state, chunk.library_id).await?;
            // Capture before resolving the binding. Every source or effective
            // AI-profile mutation advances this value; the storage transaction
            // compares it while holding the same lock as manifest+vector I/O.
            let source_truth_version =
                load_vector_write_source_fence(state, chunk.library_id).await?;
            let binding = resolve_active_embedding_binding(state, chunk.library_id).await?;
            let active_profile_key = binding.embedding_execution_profile_key();
            let expected_dimensions =
                library_vector_index_dimensions_for_binding(state, chunk.library_id, &binding)
                    .await?;
            validate_embedding_profile_write(
                chunk.library_id,
                &write.embedding_profile_key,
                &active_profile_key,
            )?;
            let freshness_generation =
                resolve_chunk_vector_generation(state, &chunk).await.with_context(|| {
                    format!("failed to resolve vector generation for chunk {}", write.chunk_id)
                })?;
            let vector = write.embedding_vector.clone();
            let row = KnowledgeChunkVectorRow {
                vector_id: Uuid::now_v7(),
                workspace_id: chunk.workspace_id,
                library_id: chunk.library_id,
                chunk_id: chunk.chunk_id,
                revision_id: chunk.revision_id,
                embedding_model_key: write.embedding_profile_key.clone(),
                vector_kind: KNOWLEDGE_CHUNK_VECTOR_KIND.to_string(),
                dimensions: validate_embedding_vector_dimensions(
                    expected_dimensions,
                    &vector,
                    format!("chunk {}", write.chunk_id),
                )
                .with_context(|| {
                    format!("failed to resolve chunk embedding dimensions for {}", write.chunk_id)
                })?,
                vector,
                freshness_generation,
                created_at: Utc::now(),
                occurred_at: chunk.occurred_at,
                occurred_until: chunk.occurred_until,
            };
            state
                .search_store
                .upsert_chunk_vectors_bulk_fenced(
                    std::slice::from_ref(&row),
                    &CanonicalVectorWriteFence {
                        expected_source_truth_version: source_truth_version,
                        embedding_profile_key: active_profile_key,
                        ingest_attempt: None,
                        advance_source_truth_version: true,
                    },
                )
                .await
                .with_context(|| {
                    format!("failed to atomically persist chunk vector for {}", write.chunk_id)
                })?;
            invalidate_library_vector_index_dimensions(chunk.library_id);
            written += 1;
        }
        Ok(written)
    }

    pub async fn persist_graph_node_embeddings(
        &self,
        state: &AppState,
        writes: &[GraphNodeEmbeddingWrite],
    ) -> std::result::Result<usize, QueryServiceError> {
        let mut written = 0usize;
        for write in writes {
            let entity = state
                .graph_store
                .get_entity_by_id(write.node_id)
                .await
                .with_context(|| {
                    format!("failed to load knowledge entity {}", write.node_id)
                })?
                .ok_or_else(|| {
                    anyhow!(
                        "graph node {} is not a canonical knowledge entity; relation or projection node vectors are not supported by the search store",
                        write.node_id
                    )
            })?;
            let _vector_guard = self.vector_plane_read_guard(state, entity.library_id).await?;
            let source_truth_version =
                load_vector_write_source_fence(state, entity.library_id).await?;
            let binding = resolve_active_embedding_binding(state, entity.library_id).await?;
            let active_profile_key = binding.embedding_execution_profile_key();
            let expected_dimensions =
                library_vector_index_dimensions_for_binding(state, entity.library_id, &binding)
                    .await?;
            validate_embedding_profile_write(
                entity.library_id,
                &write.embedding_profile_key,
                &active_profile_key,
            )?;
            let vector = write.embedding_vector.clone();
            let row = KnowledgeEntityVectorRow {
                vector_id: Uuid::now_v7(),
                workspace_id: entity.workspace_id,
                library_id: entity.library_id,
                entity_id: entity.entity_id,
                embedding_model_key: write.embedding_profile_key.clone(),
                vector_kind: KNOWLEDGE_ENTITY_VECTOR_KIND.to_string(),
                dimensions: validate_embedding_vector_dimensions(
                    expected_dimensions,
                    &vector,
                    format!("entity {}", write.node_id),
                )
                .with_context(|| {
                    format!("failed to resolve entity embedding dimensions for {}", write.node_id)
                })?,
                vector,
                freshness_generation: entity.freshness_generation,
                created_at: Utc::now(),
            };
            state
                .search_store
                .upsert_entity_vectors_bulk_fenced(
                    std::slice::from_ref(&row),
                    &CanonicalVectorWriteFence {
                        expected_source_truth_version: source_truth_version,
                        embedding_profile_key: active_profile_key,
                        ingest_attempt: None,
                        advance_source_truth_version: true,
                    },
                )
                .await
                .with_context(|| {
                    format!(
                        "failed to atomically persist canonical entity vector for {}",
                        write.node_id
                    )
                })?;
            invalidate_library_vector_index_dimensions(entity.library_id);
            written += 1;
        }
        Ok(written)
    }

    /// Rebuild the vector plane for a single library against its active
    /// `EmbedChunk` binding dimension. Per-library now: dropping a library's
    /// rows from any wrong-dim shard, ensuring the target-dim shard exists,
    /// then re-embedding chunks and entities into it.
    ///
    /// Other libraries' material is untouched — different libraries on
    /// different embed bindings (and therefore different dims) coexist in
    /// separate per-dim shards.
    pub async fn rebuild_vector_plane_for_library(
        &self,
        state: &AppState,
        library_id: Uuid,
    ) -> std::result::Result<VectorPlaneRebuildOutcome, QueryServiceError> {
        // Rebuild serialization is intentionally separate from the data-plane
        // read/write lock. Provider calls populate an opaque staging profile,
        // so queries keep reading the previous canonical profile until the
        // short atomic promotion below.
        let _rebuild_guard = self.vector_rebuild_guard(state, library_id).await?;
        discard_abandoned_vector_rebuilds(state, library_id).await?;
        let source_truth_version = load_vector_write_source_fence(state, library_id).await?;
        let active_chunk_count = state
            .document_store
            .count_active_chunks_by_library(library_id)
            .await
            .context("failed to count active chunks before vector-plane rebuild")?;
        if active_chunk_count == 0 {
            self.purge_empty_vector_plane_under_write_guard(
                state,
                library_id,
                source_truth_version,
            )
            .await?;
            return Ok(VectorPlaneRebuildOutcome {
                previous_dimensions: None,
                target_dimensions: None,
                indexes_recreated: false,
                libraries_rebuilt: 1,
                chunk_embeddings_rebuilt: 0,
                graph_node_embeddings_rebuilt: 0,
            });
        }
        let embedding_binding = resolve_active_embedding_binding(state, library_id).await?;
        let dimension_state =
            library_vector_index_dimension_state_for_binding(state, library_id, &embedding_binding)
                .await?;
        let canonical_embedding_model_key = embedding_binding.embedding_execution_profile_key();
        let staging_embedding_model_key =
            vector_rebuild_staging_profile_key(&canonical_embedding_model_key, Uuid::now_v7());
        let previous_dimensions = None;

        let mut outcome = VectorPlaneRebuildOutcome {
            previous_dimensions,
            target_dimensions: None,
            indexes_recreated: false,
            libraries_rebuilt: 0,
            chunk_embeddings_rebuilt: 0,
            graph_node_embeddings_rebuilt: 0,
        };
        let mut prepared_dimensions = None;
        let staged_result = async {
            let (chunk_rebuild, target_dimensions) = self
                .rebuild_chunk_embeddings_with_dimension_state(
                    state,
                    library_id,
                    dimension_state,
                    &embedding_binding,
                    &canonical_embedding_model_key,
                    &staging_embedding_model_key,
                    &mut prepared_dimensions,
                )
                .await?;
            let entity_rebuilt = self
                .rebuild_graph_node_embeddings_with_expected_dimensions(
                    state,
                    library_id,
                    target_dimensions.get(),
                    &embedding_binding,
                    &staging_embedding_model_key,
                    &mut prepared_dimensions,
                )
                .await?;
            ensure_embedding_profile_stable_after_rebuild(
                state,
                library_id,
                &canonical_embedding_model_key,
            )
            .await?;
            ensure_rebuilt_chunk_embedding_inventory(
                state,
                library_id,
                &staging_embedding_model_key,
                chunk_rebuild.rebuilt,
            )
            .await?;
            let expected_chunk_count = u64::try_from(chunk_rebuild.rebuilt)
                .context("rebuilt chunk count overflowed u64")?;
            let expected_entity_count =
                u64::try_from(entity_rebuilt).context("rebuilt entity count overflowed u64")?;
            Ok::<_, QueryServiceError>({
                (
                    chunk_rebuild,
                    entity_rebuilt,
                    expected_chunk_count,
                    expected_entity_count,
                    target_dimensions,
                )
            })
        }
        .await;
        let (
            chunk_rebuild,
            entity_rebuilt,
            expected_chunk_count,
            expected_entity_count,
            target_dimensions,
        ) = match staged_result {
            Ok(result) => result,
            Err(error) => {
                return Err(discard_staged_vector_rebuild_if_prepared(
                    state,
                    library_id,
                    prepared_dimensions,
                    &staging_embedding_model_key,
                    error,
                )
                .await);
            }
        };
        if let Err(error) = self
            .promote_staged_vector_rebuild_under_write_guard(
                state,
                library_id,
                target_dimensions.get(),
                &canonical_embedding_model_key,
                &staging_embedding_model_key,
                source_truth_version,
                Some(expected_chunk_count),
                Some(expected_entity_count),
            )
            .await
        {
            return Err(discard_staged_vector_rebuild_if_prepared(
                state,
                library_id,
                prepared_dimensions,
                &staging_embedding_model_key,
                error,
            )
            .await);
        }
        invalidate_library_vector_index_dimensions(library_id);
        ensure_rebuilt_chunk_embedding_inventory(
            state,
            library_id,
            &canonical_embedding_model_key,
            chunk_rebuild.rebuilt,
        )
        .await?;
        outcome.chunk_embeddings_rebuilt = chunk_rebuild.rebuilt;
        outcome.graph_node_embeddings_rebuilt = entity_rebuilt;
        outcome.target_dimensions = Some(target_dimensions.get());
        mark_revisions_vector_ready(state, &chunk_rebuild.touched_revision_ids)
            .await
            .context("failed to mark rebuilt revisions as vector-ready")?;
        invalidate_library_embedding_profile_inventory(library_id);
        outcome.libraries_rebuilt = 1;
        Ok(outcome)
    }

    async fn persist_revision_chunk_embedding_batch(
        &self,
        state: &AppState,
        library_id: Uuid,
        chunks: &[&KnowledgeChunkRow],
        batch: &[usize],
        batch_response: &EmbeddingBatchResponse,
        write: RevisionChunkEmbeddingBatchWrite<'_>,
        cleanup_owned_vector_ids: &mut BTreeSet<Uuid>,
        cancellation_token: &CancellationToken,
    ) -> AnyhowResult<(usize, EmbeddingDimensions)> {
        let RevisionChunkEmbeddingBatchWrite {
            embedding_profile_key,
            freshness_generation,
            expected_dimensions,
            expected_source_truth_version,
            ingest_attempt,
        } = write;
        let dimensions = validate_embedding_batch_dimensions(
            batch.len(),
            batch_response.dimensions,
            &batch_response.embeddings,
            expected_dimensions,
            "revision chunk batch",
        )?;
        let _vector_guard = self.vector_plane_read_guard(state, library_id).await?;
        ensure_active_embedding_profile_key(state, library_id, embedding_profile_key).await?;
        ensure_not_cancelled(cancellation_token)?;

        let mut rows = Vec::with_capacity(batch.len());
        for (chunk_index, vector) in batch.iter().zip(batch_response.embeddings.iter()) {
            let chunk = chunks[*chunk_index];
            rows.push(KnowledgeChunkVectorRow {
                vector_id: Uuid::now_v7(),
                workspace_id: chunk.workspace_id,
                library_id: chunk.library_id,
                chunk_id: chunk.chunk_id,
                revision_id: chunk.revision_id,
                embedding_model_key: embedding_profile_key.to_string(),
                vector_kind: KNOWLEDGE_CHUNK_VECTOR_KIND.to_string(),
                dimensions: validate_embedding_vector_dimensions(
                    dimensions.get(),
                    vector,
                    format!("chunk {}", chunk.chunk_id),
                )?,
                vector: vector.clone(),
                freshness_generation,
                created_at: Utc::now(),
                occurred_at: chunk.occurred_at,
                occurred_until: chunk.occurred_until,
            });
        }
        state
            .search_store
            .upsert_chunk_vectors_bulk_fenced(
                &rows,
                &CanonicalVectorWriteFence {
                    expected_source_truth_version,
                    embedding_profile_key: embedding_profile_key.to_string(),
                    ingest_attempt: Some(ingest_attempt),
                    advance_source_truth_version: false,
                },
            )
            .await
            .context("failed to source/profile-fenced bulk-persist chunk vectors")?;
        cleanup_owned_vector_ids.extend(rows.iter().map(|row| row.vector_id));
        // `Unobserved` is cached as healthy dimension state. Evict it as soon
        // as the first exact-profile manifest claim and its real vectors are
        // durable, so query/entity paths cannot falsely fail closed.
        invalidate_library_vector_index_dimensions(library_id);
        ensure_not_cancelled(cancellation_token)?;
        Ok((rows.len(), dimensions))
    }

    /// Embeds every chunk of a single revision using the library's active
    /// EmbedChunk binding, persists the vectors into per-dim vector relations,
    /// and returns per-stage usage
    /// for billing + stage-event reporting.
    ///
    /// Called inline from the ingest worker and inline-mutation pipelines
    /// so a newly readable revision gets queryable vectors before graph
    /// extraction runs. The revision's `vector_state` / library's
    /// `active_vector_generation` only flip to "ready" when this returns
    /// a matching chunks_embedded count — no silent "pretend ready"
    /// divergence between revision metadata and actual vector inventory.
    pub async fn embed_chunks_for_revision(
        &self,
        state: &AppState,
        library_id: Uuid,
        revision_id: Uuid,
        attempt_id: Uuid,
        vector_write_source_truth_version: i64,
        cancellation_token: &CancellationToken,
    ) -> std::result::Result<EmbedChunksStageOutcome, QueryServiceError> {
        ensure_not_cancelled(cancellation_token)?;
        let revision = state
            .document_store
            .get_revision(revision_id)
            .await
            .with_context(|| format!("failed to load revision {revision_id}"))?
            .ok_or_else(|| anyhow!("knowledge revision {revision_id} not found"))?;
        let chunks = state
            .document_store
            .list_chunks_by_revision(revision_id)
            .await
            .with_context(|| format!("failed to list chunks for revision {revision_id}"))?;
        ensure_not_cancelled(cancellation_token)?;
        if chunks.is_empty() {
            return Ok(EmbedChunksStageOutcome::default());
        }

        // The caller captures this snapshot before entering the stage so the
        // same fence can authorize success or failure publication even when a
        // provider call returns an error.
        if vector_write_source_truth_version <= 0 {
            return Err(QueryServiceError::StateConflict {
                message: "embed_chunk requires a positive source-truth fence".to_string(),
            });
        }
        let ingest_attempt = CanonicalIngestVectorWriteFence { attempt_id, revision_id };
        let mut cleanup_owned_vector_ids = BTreeSet::new();
        let binding = state
            .canonical_services
            .ai_catalog
            .resolve_active_runtime_binding(state, library_id, AiBindingPurpose::EmbedChunk)
            .await?
            .ok_or_else(|| {
                anyhow!("active embedding binding is not configured for library {library_id}")
            })?;
        ensure_not_cancelled(cancellation_token)?;
        let embedding_model_key = binding.embedding_execution_profile_key();
        let parallelism = state.settings.ingestion_embedding_parallelism.max(1);
        let freshness_generation = revision.revision_number;
        let (dimension_state, reused_chunk_ids) = resolve_reused_revision_chunk_ids(
            self,
            state,
            library_id,
            revision_id,
            &chunks,
            &binding,
            &embedding_model_key,
            freshness_generation,
            vector_write_source_truth_version,
            ingest_attempt,
            &mut cleanup_owned_vector_ids,
        )
        .await?;
        ensure_not_cancelled(cancellation_token)?;
        // Resume path: only the chunks not already covered by a persisted /
        // reusable vector are embedded on this attempt. After a transient failure
        // that preserved partial vectors, the prior batches land in
        // `reused_chunk_ids` and the retry pays only for the missing remainder.
        let chunk_ids = chunks.iter().map(|chunk| chunk.chunk_id).collect::<Vec<_>>();
        let missing_chunk_ids: BTreeSet<Uuid> =
            chunk_ids_missing_vectors(&chunk_ids, &reused_chunk_ids).into_iter().collect();
        let chunks_to_embed: Vec<&KnowledgeChunkRow> =
            chunks.iter().filter(|chunk| missing_chunk_ids.contains(&chunk.chunk_id)).collect();
        sync_embed_chunk_stage_progress(state, attempt_id, reused_chunk_ids.len(), chunks.len())
            .await;

        let provider_kind_owned = binding.provider_kind.clone();
        let model_name_owned = binding.model_name.clone();

        let mut batches = revision_chunk_embedding_batches(chunks_to_embed.len());
        let chunks_ref = &chunks_to_embed;
        let observed_dimensions = match dimension_state {
            EmbeddingDimensionState::Known(dimensions) => Some(dimensions),
            EmbeddingDimensionState::Unobserved => None,
        };
        let mut usage = EmbeddingUsageTotals::default();
        let run = RevisionChunkEmbeddingRun {
            service: self,
            state,
            library_id,
            revision_id,
            attempt_id,
            source_truth_version: vector_write_source_truth_version,
            cancellation_token,
            binding: &binding,
            embedding_profile_key: &embedding_model_key,
            freshness_generation,
            ingest_attempt,
            total_chunk_count: chunks.len(),
            reused_chunk_count: reused_chunk_ids.len(),
        };
        let (expected_dimensions, learned_chunk_count) = learn_revision_embedding_dimensions(
            &run,
            chunks_ref,
            &mut batches,
            observed_dimensions,
            &mut usage,
            &mut cleanup_owned_vector_ids,
        )
        .await?;
        let chunks_embedded = persist_revision_embedding_batches(
            &run,
            chunks_ref,
            batches,
            expected_dimensions,
            parallelism,
            learned_chunk_count,
            &mut usage,
            &mut cleanup_owned_vector_ids,
        )
        .await?;
        verify_revision_embedding_coverage(&run, chunks_embedded, &cleanup_owned_vector_ids)
            .await?;

        let (prompt_tokens, completion_tokens, total_tokens) = embedding_usage_token_counts(&usage);

        let usage_json = serde_json::json!({
            "provider_kind": provider_kind_owned,
            "model_name": model_name_owned,
            "prompt_tokens": prompt_tokens,
            "completion_tokens": completion_tokens,
            "total_tokens": total_tokens,
            "chunks_embedded": chunks_embedded,
            "chunks_reused": reused_chunk_ids.len(),
        });

        Ok(EmbedChunksStageOutcome {
            embedding_profile_key: Some(embedding_model_key),
            chunks_embedded,
            chunks_reused: reused_chunk_ids.len(),
            usage_json: (chunks_embedded > 0).then_some(usage_json),
            provider_kind: Some(provider_kind_owned),
            model_name: Some(model_name_owned),
            prompt_tokens,
            completion_tokens,
            total_tokens,
        })
    }

    pub async fn rebuild_chunk_embeddings(
        &self,
        state: &AppState,
        library_id: Uuid,
    ) -> std::result::Result<usize, QueryServiceError> {
        let _rebuild_guard = self.vector_rebuild_guard(state, library_id).await?;
        discard_abandoned_vector_rebuilds(state, library_id).await?;
        let source_truth_version = load_vector_write_source_fence(state, library_id).await?;
        if state
            .document_store
            .count_active_chunks_by_library(library_id)
            .await
            .context("failed to count active chunks before chunk-vector rebuild")?
            == 0
        {
            self.purge_empty_vector_plane_under_write_guard(
                state,
                library_id,
                source_truth_version,
            )
            .await?;
            return Ok(0);
        }
        let embedding_binding = resolve_active_embedding_binding(state, library_id).await?;
        let dimension_state =
            library_vector_index_dimension_state_for_binding(state, library_id, &embedding_binding)
                .await?;
        let canonical_embedding_model_key = embedding_binding.embedding_execution_profile_key();
        let staging_embedding_model_key =
            vector_rebuild_staging_profile_key(&canonical_embedding_model_key, Uuid::now_v7());
        let mut prepared_dimensions = None;
        let staged_result = async {
            let (rebuild, target_dimensions) = self
                .rebuild_chunk_embeddings_with_dimension_state(
                    state,
                    library_id,
                    dimension_state,
                    &embedding_binding,
                    &canonical_embedding_model_key,
                    &staging_embedding_model_key,
                    &mut prepared_dimensions,
                )
                .await?;
            ensure_embedding_profile_stable_after_rebuild(
                state,
                library_id,
                &canonical_embedding_model_key,
            )
            .await?;
            ensure_rebuilt_chunk_embedding_inventory(
                state,
                library_id,
                &staging_embedding_model_key,
                rebuild.rebuilt,
            )
            .await?;
            let expected_chunk_count =
                u64::try_from(rebuild.rebuilt).context("rebuilt chunk count overflowed u64")?;
            Ok::<_, QueryServiceError>((rebuild, expected_chunk_count, target_dimensions))
        }
        .await;
        let (rebuild, expected_chunk_count, target_dimensions) = match staged_result {
            Ok(result) => result,
            Err(error) => {
                return Err(discard_staged_vector_rebuild_if_prepared(
                    state,
                    library_id,
                    prepared_dimensions,
                    &staging_embedding_model_key,
                    error,
                )
                .await);
            }
        };
        if let Err(error) = self
            .promote_staged_vector_rebuild_under_write_guard(
                state,
                library_id,
                target_dimensions.get(),
                &canonical_embedding_model_key,
                &staging_embedding_model_key,
                source_truth_version,
                Some(expected_chunk_count),
                None,
            )
            .await
        {
            return Err(discard_staged_vector_rebuild_if_prepared(
                state,
                library_id,
                prepared_dimensions,
                &staging_embedding_model_key,
                error,
            )
            .await);
        }
        invalidate_library_vector_index_dimensions(library_id);
        ensure_rebuilt_chunk_embedding_inventory(
            state,
            library_id,
            &canonical_embedding_model_key,
            rebuild.rebuilt,
        )
        .await?;
        mark_revisions_vector_ready(state, &rebuild.touched_revision_ids)
            .await
            .context("failed to mark rebuilt revisions as vector-ready")?;
        invalidate_library_embedding_profile_inventory(library_id);
        Ok(rebuild.rebuilt)
    }

    async fn rebuild_chunk_embeddings_with_dimension_state(
        &self,
        state: &AppState,
        library_id: Uuid,
        dimension_state: EmbeddingDimensionState,
        embedding_binding: &ResolvedRuntimeBinding,
        canonical_embedding_model_key: &str,
        staging_embedding_model_key: &str,
        prepared_dimensions: &mut Option<u64>,
    ) -> std::result::Result<(ChunkEmbeddingRebuildOutcome, EmbeddingDimensions), QueryServiceError>
    {
        let mut dimensions = match dimension_state {
            EmbeddingDimensionState::Known(dimensions) => Some(dimensions),
            EmbeddingDimensionState::Unobserved => None,
        };
        if let Some(dimensions) = dimensions {
            state
                .search_store
                .prepare_chunk_vector_rebuild_lane(
                    library_id,
                    dimensions.get(),
                    staging_embedding_model_key,
                )
                .await
                .context("failed to prepare chunk vector manifest lane for rebuild")?;
            *prepared_dimensions = Some(dimensions.get());
        }
        let rebuild_result = async {
            let mut revision_number_cache = HashMap::<Uuid, i64>::new();
            let mut touched_revision_ids = BTreeSet::new();
            let mut rebuilt = 0usize;
            let mut after = None;

            loop {
                let chunks = state
                    .document_store
                    .list_active_chunks_by_library_page(
                        library_id,
                        after,
                        CHUNK_REBUILD_FETCH_PAGE_SIZE,
                    )
                    .await
                    .with_context(|| {
                        format!(
                            "failed to stream knowledge chunks for library {library_id} rebuild"
                        )
                    })?;
                let Some(next_after) = chunk_rebuild_page_cursor(&chunks) else {
                    if rebuilt == 0 {
                        return Err(QueryServiceError::StateConflict {
                            message: format!(
                                "active chunk inventory changed while rebuilding library {library_id}; retry the rebuild"
                            ),
                        });
                    }
                    break;
                };
                let page_is_complete = chunks.len() < CHUNK_REBUILD_FETCH_PAGE_SIZE;
                let mut page_offset = 0usize;
                if dimensions.is_none() {
                    let first_batch_len = chunks.len().min(CHUNK_EMBEDDING_BATCH_SIZE);
                    let first_batch = &chunks[..first_batch_len];
                    let freshness = resolve_chunk_freshness_generations(
                        state,
                        first_batch,
                        &mut revision_number_cache,
                    )
                    .await?;
                    let (learned_dimensions, rows) = embed_rebuild_chunk_batch_rows(
                        state,
                        first_batch,
                        &freshness,
                        embedding_binding,
                        staging_embedding_model_key,
                        None,
                    )
                    .await?;
                    // The provider call above is the first ordinary rebuild
                    // batch. Fence the binding before its learned dimension is
                    // allowed to create durable staging metadata.
                    ensure_active_embedding_profile_key(
                        state,
                        library_id,
                        canonical_embedding_model_key,
                    )
                    .await?;
                    state
                        .search_store
                        .prepare_chunk_vector_rebuild_lane(
                            library_id,
                            learned_dimensions.get(),
                            staging_embedding_model_key,
                        )
                        .await
                        .context(
                            "failed to prepare learned chunk vector manifest lane for rebuild",
                        )?;
                    *prepared_dimensions = Some(learned_dimensions.get());
                    state
                        .search_store
                        .upsert_chunk_vectors_bulk_deferred_manifest(&rows)
                        .await
                        .with_context(|| {
                            format!(
                                "failed to persist {} first-batch rebuilt chunk vectors",
                                rows.len()
                            )
                        })?;
                    rebuilt = rebuilt.checked_add(rows.len()).ok_or_else(|| {
                        anyhow!("rebuilt chunk count overflowed for library {library_id}")
                    })?;
                    touched_revision_ids
                        .extend(first_batch.iter().map(|chunk| chunk.revision_id));
                    dimensions = Some(learned_dimensions);
                    page_offset = first_batch_len;
                }

                if page_offset < chunks.len() {
                    let page_dimensions = dimensions.ok_or_else(|| {
                        QueryServiceError::StateConflict {
                            message: format!(
                                "library {library_id} has remaining chunks but no established vector dimension"
                            ),
                        }
                    })?;
                    let (page_rebuilt, page_touched) = rebuild_chunk_embedding_page(
                        state,
                        &chunks[page_offset..],
                        page_dimensions,
                        embedding_binding,
                        staging_embedding_model_key,
                        &mut revision_number_cache,
                    )
                    .await?;
                    rebuilt = rebuilt.checked_add(page_rebuilt).ok_or_else(|| {
                        anyhow!("rebuilt chunk count overflowed for library {library_id}")
                    })?;
                    touched_revision_ids.extend(page_touched);
                }
                if page_is_complete {
                    break;
                }
                after = Some(next_after);
            }
            let dimensions = dimensions.ok_or_else(|| QueryServiceError::StateConflict {
                message: format!(
                    "library {library_id} rebuild finished without observing a chunk vector dimension"
                ),
            })?;
            Ok((rebuilt, touched_revision_ids, dimensions))
        }
        .await;
        let Some(prepared_dimensions) = *prepared_dimensions else {
            return rebuild_result.map(|(rebuilt, touched_revision_ids, dimensions)| {
                (ChunkEmbeddingRebuildOutcome { rebuilt, touched_revision_ids }, dimensions)
            });
        };
        let reconciliation_result = state
            .search_store
            .reconcile_chunk_vector_manifest_count(
                library_id,
                prepared_dimensions,
                staging_embedding_model_key,
            )
            .await
            .context("failed to reconcile chunk vector manifest after rebuild")
            .map_err(QueryServiceError::from);
        let (rebuilt, touched_revision_ids, dimensions) =
            finish_deferred_manifest_rebuild(rebuild_result, reconciliation_result)?;
        Ok((ChunkEmbeddingRebuildOutcome { rebuilt, touched_revision_ids }, dimensions))
    }

    pub async fn rebuild_graph_node_embeddings(
        &self,
        state: &AppState,
        library_id: Uuid,
    ) -> std::result::Result<usize, QueryServiceError> {
        let _rebuild_guard = self.vector_rebuild_guard(state, library_id).await?;
        discard_abandoned_vector_rebuilds(state, library_id).await?;
        let source_truth_version = load_vector_write_source_fence(state, library_id).await?;
        if state
            .document_store
            .count_active_chunks_by_library(library_id)
            .await
            .context("failed to count active chunks before entity-vector rebuild")?
            == 0
        {
            self.purge_empty_vector_plane_under_write_guard(
                state,
                library_id,
                source_truth_version,
            )
            .await?;
            return Ok(0);
        }
        let embedding_binding = resolve_active_embedding_binding(state, library_id).await?;
        let expected_dimensions =
            library_vector_index_dimensions_for_binding(state, library_id, &embedding_binding)
                .await?;
        let canonical_embedding_model_key = embedding_binding.embedding_execution_profile_key();
        let staging_embedding_model_key =
            vector_rebuild_staging_profile_key(&canonical_embedding_model_key, Uuid::now_v7());
        let mut prepared_dimensions = None;
        let staged_result = async {
            let rebuilt = self
                .rebuild_graph_node_embeddings_with_expected_dimensions(
                    state,
                    library_id,
                    expected_dimensions,
                    &embedding_binding,
                    &staging_embedding_model_key,
                    &mut prepared_dimensions,
                )
                .await?;
            ensure_embedding_profile_stable_after_rebuild(
                state,
                library_id,
                &canonical_embedding_model_key,
            )
            .await?;
            let expected_entity_count =
                u64::try_from(rebuilt).context("rebuilt entity count overflowed u64")?;
            Ok::<_, QueryServiceError>((rebuilt, expected_entity_count))
        }
        .await;
        let (rebuilt, expected_entity_count) = match staged_result {
            Ok(result) => result,
            Err(error) => {
                return Err(discard_staged_vector_rebuild_if_prepared(
                    state,
                    library_id,
                    prepared_dimensions,
                    &staging_embedding_model_key,
                    error,
                )
                .await);
            }
        };
        if let Err(error) = self
            .promote_staged_vector_rebuild_under_write_guard(
                state,
                library_id,
                expected_dimensions,
                &canonical_embedding_model_key,
                &staging_embedding_model_key,
                source_truth_version,
                None,
                Some(expected_entity_count),
            )
            .await
        {
            return Err(discard_staged_vector_rebuild_if_prepared(
                state,
                library_id,
                prepared_dimensions,
                &staging_embedding_model_key,
                error,
            )
            .await);
        }
        invalidate_library_vector_index_dimensions(library_id);
        invalidate_library_embedding_profile_inventory(library_id);
        Ok(rebuilt)
    }

    async fn rebuild_graph_node_embeddings_with_expected_dimensions(
        &self,
        state: &AppState,
        library_id: Uuid,
        expected_dimensions: u64,
        embedding_binding: &ResolvedRuntimeBinding,
        staging_embedding_model_key: &str,
        prepared_dimensions: &mut Option<u64>,
    ) -> std::result::Result<usize, QueryServiceError> {
        state
            .search_store
            .prepare_entity_vector_rebuild_lane(
                library_id,
                expected_dimensions,
                staging_embedding_model_key,
            )
            .await
            .context("failed to prepare entity vector manifest lane for rebuild")?;
        *prepared_dimensions = Some(expected_dimensions);
        let rebuild_result =
            async {
                let entities =
                    state.graph_store.list_entities_by_library(library_id).await.context(
                        "failed to load knowledge entities for canonical vector rebuild",
                    )?;
                let mut rebuilt = 0usize;
                for entity_batch in entities.chunks(64) {
                    let batch_response = state
                        .llm_gateway
                        .embed_many(EmbeddingBatchRequest {
                            provider_kind: embedding_binding.provider_kind.clone(),
                            model_name: embedding_binding.model_name.clone(),
                            inputs: entity_batch.iter().map(build_entity_embedding_input).collect(),
                            api_key_override: embedding_binding.api_key.clone(),
                            base_url_override: embedding_binding.provider_base_url.clone(),
                            extra_parameters_json: embedding_binding.extra_parameters_json.clone(),
                        })
                        .await
                        .context("failed to rebuild entity vectors")?;
                    let dimensions = validate_embedding_batch_dimensions(
                        entity_batch.len(),
                        batch_response.dimensions,
                        &batch_response.embeddings,
                        Some(EmbeddingDimensions::try_from(expected_dimensions).map_err(
                            |error| QueryServiceError::Internal(anyhow::Error::new(error)),
                        )?),
                        "entity rebuild batch",
                    )?;

                    let rows = entity_batch
                        .iter()
                        .zip(batch_response.embeddings.iter())
                        .map(|(entity, embedding)| {
                            Ok(KnowledgeEntityVectorRow {
                                vector_id: Uuid::now_v7(),
                                workspace_id: entity.workspace_id,
                                library_id: entity.library_id,
                                entity_id: entity.entity_id,
                                embedding_model_key: staging_embedding_model_key.to_string(),
                                vector_kind: KNOWLEDGE_ENTITY_VECTOR_KIND.to_string(),
                                dimensions: validate_embedding_vector_dimensions(
                                    dimensions.get(),
                                    embedding.as_slice(),
                                    format!("rebuilt entity {}", entity.entity_id),
                                )?,
                                vector: embedding.clone(),
                                freshness_generation: entity.freshness_generation,
                                created_at: Utc::now(),
                            })
                        })
                        .collect::<AnyhowResult<Vec<_>>>()?;
                    state
                        .search_store
                        .upsert_entity_vectors_bulk_deferred_manifest(rows.as_slice())
                        .await
                        .with_context(|| {
                            format!("failed to bulk-persist {} rebuilt entity vectors", rows.len())
                        })?;
                    rebuilt = rebuilt.checked_add(rows.len()).ok_or_else(|| {
                        anyhow!("rebuilt entity count overflowed for library {library_id}")
                    })?;
                }
                Ok(rebuilt)
            }
            .await;
        let reconciliation_result = state
            .search_store
            .reconcile_entity_vector_manifest_count(
                library_id,
                expected_dimensions,
                staging_embedding_model_key,
            )
            .await
            .context("failed to reconcile entity vector manifest after rebuild")
            .map_err(QueryServiceError::from);
        finish_deferred_manifest_rebuild(rebuild_result, reconciliation_result)
    }
}

fn chunk_rebuild_page_cursor(chunks: &[KnowledgeChunkRow]) -> Option<(i32, Uuid)> {
    chunks.last().map(|chunk| (chunk.chunk_index, chunk.chunk_id))
}

async fn rebuild_chunk_embedding_page(
    state: &AppState,
    chunks: &[KnowledgeChunkRow],
    expected_dimensions: EmbeddingDimensions,
    embedding_binding: &ResolvedRuntimeBinding,
    embedding_model_key: &str,
    revision_number_cache: &mut HashMap<Uuid, i64>,
) -> std::result::Result<(usize, BTreeSet<Uuid>), QueryServiceError> {
    let freshness_per_chunk =
        resolve_chunk_freshness_generations(state, chunks, revision_number_cache).await?;

    let parallelism = state.settings.ingestion_embedding_parallelism.max(1);
    let batch_results = stream::iter(
        chunks
            .chunks(CHUNK_EMBEDDING_BATCH_SIZE)
            .zip(freshness_per_chunk.chunks(CHUNK_EMBEDDING_BATCH_SIZE))
            .map(|(chunk_batch, freshness_batch)| {
                let binding = embedding_binding.clone();
                async move {
                    let (_, rows) = embed_rebuild_chunk_batch_rows(
                        state,
                        chunk_batch,
                        freshness_batch,
                        &binding,
                        embedding_model_key,
                        Some(expected_dimensions),
                    )
                    .await?;
                    Ok::<_, anyhow::Error>(rows)
                }
            }),
    )
    .buffer_unordered(parallelism)
    .collect::<Vec<_>>()
    .await;

    let mut rows = Vec::with_capacity(chunks.len());
    for batch_result in batch_results {
        rows.extend(batch_result?);
    }
    state
        .search_store
        .upsert_chunk_vectors_bulk_deferred_manifest(rows.as_slice())
        .await
        .with_context(|| format!("failed to bulk-persist {} rebuilt chunk vectors", rows.len()))?;
    let touched_revision_ids = chunks.iter().map(|chunk| chunk.revision_id).collect();
    Ok((rows.len(), touched_revision_ids))
}

async fn resolve_chunk_freshness_generations(
    state: &AppState,
    chunks: &[KnowledgeChunkRow],
    revision_number_cache: &mut HashMap<Uuid, i64>,
) -> AnyhowResult<Vec<i64>> {
    let mut freshness = Vec::with_capacity(chunks.len());
    for chunk in chunks {
        if let Some(generation) = chunk.vector_generation.or(chunk.text_generation) {
            freshness.push(generation);
            continue;
        }
        let revision_number = match revision_number_cache.get(&chunk.revision_id).copied() {
            Some(revision_number) => revision_number,
            None => {
                let revision = state
                    .document_store
                    .get_revision(chunk.revision_id)
                    .await
                    .with_context(|| {
                        format!(
                            "failed to load revision {} for chunk generation",
                            chunk.revision_id
                        )
                    })?
                    .ok_or_else(|| anyhow!("knowledge revision {} not found", chunk.revision_id))?;
                revision_number_cache.insert(chunk.revision_id, revision.revision_number);
                revision.revision_number
            }
        };
        freshness.push(revision_number);
    }
    Ok(freshness)
}

async fn embed_rebuild_chunk_batch_rows(
    state: &AppState,
    chunks: &[KnowledgeChunkRow],
    freshness: &[i64],
    embedding_binding: &ResolvedRuntimeBinding,
    embedding_model_key: &str,
    expected_dimensions: Option<EmbeddingDimensions>,
) -> AnyhowResult<(EmbeddingDimensions, Vec<KnowledgeChunkVectorRow>)> {
    anyhow::ensure!(
        chunks.len() == freshness.len(),
        "chunk rebuild freshness batch length mismatch"
    );
    let response = state
        .llm_gateway
        .embed_many(EmbeddingBatchRequest {
            provider_kind: embedding_binding.provider_kind.clone(),
            model_name: embedding_binding.model_name.clone(),
            inputs: chunks.iter().map(|chunk| chunk.normalized_text.clone()).collect(),
            api_key_override: embedding_binding.api_key.clone(),
            base_url_override: embedding_binding.provider_base_url.clone(),
            extra_parameters_json: embedding_binding.extra_parameters_json.clone(),
        })
        .await
        .with_context(|| {
            let first_chunk_id = chunks.first().map(|chunk| chunk.chunk_id);
            format!("failed to rebuild chunk embeddings for batch starting at {first_chunk_id:?}")
        })?;
    let dimensions = validate_embedding_batch_dimensions(
        chunks.len(),
        response.dimensions,
        &response.embeddings,
        expected_dimensions,
        "chunk rebuild batch",
    )?;
    let rows = chunks
        .iter()
        .zip(freshness)
        .zip(response.embeddings.iter())
        .map(|((chunk, freshness_generation), embedding)| {
            Ok(KnowledgeChunkVectorRow {
                vector_id: Uuid::now_v7(),
                workspace_id: chunk.workspace_id,
                library_id: chunk.library_id,
                chunk_id: chunk.chunk_id,
                revision_id: chunk.revision_id,
                embedding_model_key: embedding_model_key.to_string(),
                vector_kind: KNOWLEDGE_CHUNK_VECTOR_KIND.to_string(),
                dimensions: validate_embedding_vector_dimensions(
                    dimensions.get(),
                    embedding,
                    format!("rebuilt chunk {}", chunk.chunk_id),
                )?,
                vector: embedding.clone(),
                freshness_generation: *freshness_generation,
                created_at: Utc::now(),
                occurred_at: chunk.occurred_at,
                occurred_until: chunk.occurred_until,
            })
        })
        .collect::<AnyhowResult<Vec<_>>>()?;
    Ok((dimensions, rows))
}

fn finish_deferred_manifest_rebuild<T>(
    rebuild_result: std::result::Result<T, QueryServiceError>,
    reconciliation_result: std::result::Result<(), QueryServiceError>,
) -> std::result::Result<T, QueryServiceError> {
    match (rebuild_result, reconciliation_result) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(rebuild_error), Ok(())) => Err(rebuild_error),
        (Ok(_), Err(reconciliation_error)) => Err(reconciliation_error),
        (Err(rebuild_error), Err(reconciliation_error)) => {
            Err(QueryServiceError::Internal(anyhow!(
                "vector rebuild failed before manifest reconciliation: {rebuild_error}; manifest reconciliation also failed: {reconciliation_error}"
            )))
        }
    }
}

async fn discard_staged_vector_rebuild_if_prepared(
    state: &AppState,
    library_id: Uuid,
    dimensions: Option<u64>,
    staging_embedding_model_key: &str,
    primary_error: QueryServiceError,
) -> QueryServiceError {
    let Some(dimensions) = dimensions else {
        return primary_error;
    };
    match state
        .search_store
        .discard_staged_vector_rebuild(library_id, dimensions, staging_embedding_model_key)
        .await
    {
        Ok(()) => primary_error,
        Err(cleanup_error) => QueryServiceError::Internal(anyhow!(
            "staged vector rebuild failed: {primary_error}; staging cleanup also failed: {cleanup_error:#}"
        )),
    }
}

async fn discard_abandoned_vector_rebuilds(
    state: &AppState,
    library_id: Uuid,
) -> std::result::Result<(), QueryServiceError> {
    let discarded = state
        .search_store
        .discard_abandoned_staged_vector_rebuilds(library_id)
        .await
        .context("failed to recover abandoned staged vector rebuilds")?;
    if discarded > 0 {
        tracing::warn!(
            library_id = %library_id,
            discarded,
            "removed durable staging residue before vector rebuild",
        );
    }
    Ok(())
}

async fn resolve_active_embedding_binding(
    state: &AppState,
    library_id: Uuid,
) -> std::result::Result<ResolvedRuntimeBinding, QueryServiceError> {
    state
        .canonical_services
        .ai_catalog
        .resolve_active_runtime_binding(state, library_id, AiBindingPurpose::EmbedChunk)
        .await?
        .ok_or_else(|| QueryServiceError::BindingNotConfigured {
            message: format!(
                "active embed_chunk binding is not configured for library {library_id}"
            ),
        })
}

async fn load_vector_write_source_fence(
    state: &AppState,
    library_id: Uuid,
) -> std::result::Result<i64, QueryServiceError> {
    // Capture without holding a transaction across provider I/O. Canonical
    // writes compare this under the per-library DB serializer; staged rebuilds
    // compare and advance it in their short promotion/purge transaction.
    catalog_repository::get_library_source_truth_version(&state.persistence.postgres, library_id)
        .await
        .with_context(|| {
            format!(
                "failed to capture source/profile fence before vector work for library {library_id}"
            )
        })
        .map_err(QueryServiceError::from)
}

fn vector_rebuild_staging_profile_key(
    canonical_embedding_profile_key: &str,
    rebuild_id: Uuid,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"ironrag.vector-rebuild-staging.v1");
    hasher.update(canonical_embedding_profile_key.as_bytes());
    hasher.update(rebuild_id.as_bytes());
    format!("{VECTOR_REBUILD_STAGING_PROFILE_PREFIX}{}", hex::encode(hasher.finalize()))
}

async fn ensure_embedding_profile_stable_after_rebuild(
    state: &AppState,
    library_id: Uuid,
    rebuilt_profile_key: &str,
) -> std::result::Result<(), QueryServiceError> {
    let active_profile_key = resolve_active_embedding_binding(state, library_id)
        .await?
        .embedding_execution_profile_key();
    if active_profile_key == rebuilt_profile_key {
        return Ok(());
    }
    Err(QueryServiceError::StateConflict {
        message: format!(
            "active embedding execution profile changed while rebuilding library {library_id}; rerun `ironrag-maintenance rebuild vector-plane --source-library {library_id}`"
        ),
    })
}

async fn ensure_rebuilt_chunk_embedding_inventory(
    state: &AppState,
    library_id: Uuid,
    embedding_profile_key: &str,
    rebuilt: usize,
) -> std::result::Result<(), QueryServiceError> {
    let expected = state
        .document_store
        .count_active_chunks_by_library(library_id)
        .await
        .context("failed to count active chunks after vector rebuild")?;
    let rebuilt = u64::try_from(rebuilt).map_err(|_| QueryServiceError::StateConflict {
        message: format!(
            "rebuilt chunk count overflowed for library {library_id}; rerun `ironrag-maintenance rebuild vector-plane --source-library {library_id}`"
        ),
    })?;
    validate_rebuilt_chunk_count(library_id, expected, rebuilt)?;
    let index_state = inspect_library_embedding_profile_indexed_uncached(
        state,
        library_id,
        embedding_profile_key,
    )
    .await?;
    match (expected, index_state) {
        (0, EmbeddingProfileIndexState::Empty)
        | (1.., EmbeddingProfileIndexState::Ready { .. }) => Ok(()),
        _ => Err(QueryServiceError::StateConflict {
            message: format!(
                "rebuilt vector inventory state does not match the active chunk inventory for library {library_id}; rerun `ironrag-maintenance rebuild vector-plane --source-library {library_id}`"
            ),
        }),
    }
}

fn validate_rebuilt_chunk_count(
    library_id: Uuid,
    expected: u64,
    rebuilt: u64,
) -> std::result::Result<(), QueryServiceError> {
    if expected == rebuilt {
        return Ok(());
    }
    Err(QueryServiceError::StateConflict {
        message: format!(
            "active chunk inventory changed while rebuilding library {library_id} (expected {expected}, rebuilt {rebuilt}); rerun `ironrag-maintenance rebuild vector-plane --source-library {library_id}`"
        ),
    })
}

fn validate_embedding_profile_write(
    library_id: Uuid,
    supplied_profile_key: &str,
    active_profile_key: &str,
) -> std::result::Result<(), QueryServiceError> {
    if supplied_profile_key == active_profile_key {
        return Ok(());
    }
    Err(QueryServiceError::StateConflict {
        message: format!(
            "refusing to persist a vector for library {library_id}: supplied embedding execution-profile provenance does not match the active binding"
        ),
    })
}

async fn resolve_embedding_model_catalog_id(
    state: &AppState,
    provider_kind: &str,
    model_name: &str,
) -> AnyhowResult<Uuid> {
    let provider = ai_repository::list_provider_catalog(&state.persistence.postgres)
        .await
        .context("failed to list provider catalog while resolving embedding model")?
        .into_iter()
        .find(|row| row.provider_kind == provider_kind)
        .ok_or_else(|| anyhow!("provider catalog entry {provider_kind} not found"))?;
    ai_repository::list_model_catalog(&state.persistence.postgres, Some(provider.id))
        .await
        .context("failed to list model catalog while resolving embedding model")?
        .into_iter()
        .find(|row| row.model_name == model_name)
        .map(|row| row.id)
        .ok_or_else(|| anyhow!("model catalog entry {provider_kind}/{model_name} not found"))
}

fn build_entity_embedding_input(entity: &KnowledgeEntityRow) -> String {
    format!(
        "entity_type: {}\ncanonical_label: {}\naliases: {}\nsummary: {}",
        entity.entity_type,
        entity.canonical_label,
        entity.aliases.join(", "),
        entity.summary.clone().unwrap_or_default(),
    )
}

async fn fail_embed_chunks_after_cleanup<T>(
    state: &AppState,
    revision_id: Uuid,
    expected_source_truth_version: i64,
    ingest_attempt: CanonicalIngestVectorWriteFence,
    cleanup_owned_vector_ids: &BTreeSet<Uuid>,
    error: anyhow::Error,
) -> std::result::Result<T, QueryServiceError> {
    if cleanup_owned_vector_ids.is_empty() {
        return Err(error.into());
    }
    let revision = match state.document_store.get_revision(revision_id).await {
        Ok(Some(revision)) => revision,
        Ok(None) => {
            return Err(error
                .context(format!(
                    "failed to fence partial-vector cleanup: revision {revision_id} disappeared"
                ))
                .into());
        }
        Err(fence_error) => {
            return Err(error
                .context(format!(
                    "failed to load revision {revision_id} before partial-vector cleanup: {fence_error:#}"
                ))
                .into());
        }
    };
    let _vector_guard = match state
        .canonical_services
        .search
        .vector_plane_write_guard(state, revision.library_id)
        .await
    {
        Ok(guard) => guard,
        Err(fence_error) => {
            return Err(error
                .context(format!(
                    "failed to acquire the vector cleanup guard for revision {revision_id}: {fence_error:#}"
                ))
                .into());
        }
    };
    let vector_ids = cleanup_owned_vector_ids.iter().copied().collect::<Vec<_>>();
    match state
        .search_store
        .delete_attempt_owned_chunk_vectors_by_ids_fenced(
            revision.library_id,
            &vector_ids,
            expected_source_truth_version,
            ingest_attempt,
        )
        .await
    {
        Ok(Some(outcome)) if outcome.deleted > 0 => {
            invalidate_library_embedding_profile_inventory(revision.library_id);
            tracing::warn!(
                revision_id = %revision_id,
                deleted = outcome.deleted,
                "removed only attempt-owned chunk vectors after failed embed_chunk stage",
            );
        }
        Ok(None) => {
            tracing::warn!(
                revision_id = %revision_id,
                attempt_id = %ingest_attempt.attempt_id,
                "preserved partial chunk vectors because ingest authority moved to a retry",
            );
        }
        Ok(Some(_)) => {}
        Err(cleanup_error) => {
            return Err(error.context(format!(
                "failed to remove partial chunk vectors for revision {revision_id}: {cleanup_error:#}"
            )).into());
        }
    }
    Err(error.into())
}

/// Decide cleanup from the preserved typed error, never from its display text.
async fn fail_embed_chunks_by_typed_policy<T>(
    state: &AppState,
    revision_id: Uuid,
    expected_source_truth_version: i64,
    ingest_attempt: CanonicalIngestVectorWriteFence,
    cleanup_owned_vector_ids: &BTreeSet<Uuid>,
    error: anyhow::Error,
) -> std::result::Result<T, QueryServiceError> {
    let error = QueryServiceError::from(error);
    if error.preserves_partial_vectors() {
        return fail_embed_chunks_preserving_partial(revision_id, error);
    }
    fail_embed_chunks_after_cleanup(
        state,
        revision_id,
        expected_source_truth_version,
        ingest_attempt,
        cleanup_owned_vector_ids,
        anyhow::Error::new(error),
    )
    .await
}

/// Preserve completed batches only for an explicitly typed provider failure.
/// Readiness remains count-gated, so a partial revision cannot become ready.
fn fail_embed_chunks_preserving_partial<T>(
    revision_id: Uuid,
    error: QueryServiceError,
) -> std::result::Result<T, QueryServiceError> {
    debug_assert!(error.preserves_partial_vectors());
    tracing::warn!(
        revision_id = %revision_id,
        error = %error,
        "embed_chunk stage failed transiently; preserving persisted chunk vectors for retry",
    );
    Err(error)
}

/// Cancellation and lease loss deliberately preserve rows already committed by
/// this attempt. The retry/count gate owns recovery; destructive cleanup here
/// could race a replacement attempt after the cancellation signal was raised.
fn fail_embed_chunks_cancelled<T>(revision_id: Uuid) -> std::result::Result<T, QueryServiceError> {
    tracing::warn!(
        revision_id = %revision_id,
        "embed_chunk stage cancelled; preserving attempt-owned vectors for retry or GC",
    );
    Err(QueryServiceError::Cancelled)
}

/// Chunk ids that still need a vector on this attempt: every chunk whose id is
/// not already covered by a persisted/reusable vector. Drives the embed-only-the-
/// remainder resume path so a retry after a transient failure re-embeds just the
/// missing chunks instead of the whole revision.
fn chunk_ids_missing_vectors(chunk_ids: &[Uuid], already_covered: &BTreeSet<Uuid>) -> Vec<Uuid> {
    chunk_ids.iter().copied().filter(|id| !already_covered.contains(id)).collect()
}

/// Count-gated readiness: a revision is only fully embedded when the chunks
/// embedded on this attempt plus the chunks reused from prior attempts/parents
/// exactly cover every chunk. A half-embedded revision (e.g. after a transient
/// failure that preserved partial vectors) fails this gate and never flips to
/// `vector_state = ready`.
fn embed_coverage_is_complete(chunks_embedded: usize, chunks_reused: usize, total: usize) -> bool {
    chunks_embedded.saturating_add(chunks_reused) == total
}

fn fail_embed_chunks_if_cancelled(
    revision_id: Uuid,
    cancellation_token: &CancellationToken,
) -> std::result::Result<(), QueryServiceError> {
    if cancellation_token.is_cancelled() {
        fail_embed_chunks_cancelled(revision_id)
    } else {
        Ok(())
    }
}

async fn load_current_revision_chunk_vector_ids(
    state: &AppState,
    revision_id: Uuid,
    chunks: &[KnowledgeChunkRow],
    embedding_model_key: &str,
    freshness_generation: i64,
    expected_dimensions: u64,
) -> AnyhowResult<RevisionVectorCoverage> {
    let mut coverage = RevisionVectorCoverage::default();
    for chunk_batch in chunks.chunks(CHUNK_VECTOR_REUSE_SOURCE_BATCH_SIZE) {
        let chunk_ids = chunk_batch.iter().map(|chunk| chunk.chunk_id).collect::<Vec<_>>();
        let vectors = state
            .search_store
            .list_chunk_vectors_by_chunks(
                &chunk_ids,
                embedding_model_key,
                KNOWLEDGE_CHUNK_VECTOR_KIND,
            )
            .await
            .with_context(|| {
                format!("failed to load current chunk vectors for revision {revision_id}")
            })?;
        for vector in vectors {
            if vector.revision_id == revision_id
                && vector.freshness_generation == freshness_generation
                && vector.vector_kind == KNOWLEDGE_CHUNK_VECTOR_KIND
                && validate_embedding_vector_dimensions(
                    expected_dimensions,
                    &vector.vector,
                    format!("current chunk vector {}", vector.chunk_id),
                )
                .is_ok()
            {
                coverage.chunk_ids.insert(vector.chunk_id);
            }
        }
    }
    Ok(coverage)
}

async fn reuse_chunk_vectors_from_parent_revision(
    state: &AppState,
    revision_id: Uuid,
    new_chunks: &[KnowledgeChunkRow],
    embedding_model_key: &str,
    freshness_generation: i64,
    expected_dimensions: u64,
    expected_source_truth_version: i64,
    ingest_attempt: CanonicalIngestVectorWriteFence,
    cleanup_owned_vector_ids: &mut BTreeSet<Uuid>,
) -> AnyhowResult<BTreeSet<Uuid>> {
    if new_chunks.is_empty() {
        return Ok(BTreeSet::new());
    }
    let Some(parent_revision_id) = load_parent_revision_id(state, revision_id).await? else {
        return Ok(BTreeSet::new());
    };
    let new_chunks_by_checksum = index_chunks_by_checksum(new_chunks);
    if new_chunks_by_checksum.is_empty() {
        return Ok(BTreeSet::new());
    }
    let parent_chunks =
        state.document_store.list_chunks_by_revision(parent_revision_id).await.with_context(
            || format!("failed to list parent chunks for vector reuse from {parent_revision_id}"),
        )?;
    let (parent_chunk_by_id, parent_ids_by_checksum) =
        index_matching_parent_chunks(&parent_chunks, &new_chunks_by_checksum);
    if parent_ids_by_checksum.is_empty() {
        return Ok(BTreeSet::new());
    }

    let mut reused_chunk_ids = BTreeSet::new();
    for parent_batch in parent_ids_by_checksum
        .values()
        .copied()
        .collect::<Vec<_>>()
        .chunks(CHUNK_VECTOR_REUSE_SOURCE_BATCH_SIZE)
    {
        let vectors =
            load_parent_vectors(state, parent_revision_id, parent_batch, embedding_model_key)
                .await?;
        let current_vectors = newest_parent_vectors(vectors);
        persist_parent_vector_matches(
            state,
            &current_vectors,
            &parent_chunk_by_id,
            &new_chunks_by_checksum,
            &mut reused_chunk_ids,
            embedding_model_key,
            freshness_generation,
            expected_dimensions,
            expected_source_truth_version,
            ingest_attempt,
            cleanup_owned_vector_ids,
        )
        .await?;
    }

    if !reused_chunk_ids.is_empty() {
        tracing::info!(
            revision_id = %revision_id,
            parent_revision_id = %parent_revision_id,
            reused = reused_chunk_ids.len(),
            total_chunks = new_chunks.len(),
            "diff-aware ingest: reusing chunk vectors for unchanged chunks",
        );
    }
    Ok(reused_chunk_ids)
}

async fn load_parent_revision_id(
    state: &AppState,
    revision_id: Uuid,
) -> AnyhowResult<Option<Uuid>> {
    Ok(content_repository::get_revision_by_id(&state.persistence.postgres, revision_id)
        .await
        .with_context(|| format!("failed to load content revision {revision_id} for vector reuse"))?
        .and_then(|revision| revision.parent_revision_id))
}

fn index_chunks_by_checksum(
    chunks: &[KnowledgeChunkRow],
) -> HashMap<String, Vec<&KnowledgeChunkRow>> {
    chunks.iter().filter_map(|chunk| chunk_text_reuse_key(chunk).map(|key| (key, chunk))).fold(
        HashMap::new(),
        |mut index, (key, chunk)| {
            index.entry(key).or_default().push(chunk);
            index
        },
    )
}

fn index_matching_parent_chunks<'a>(
    parent_chunks: &'a [KnowledgeChunkRow],
    new_chunks_by_checksum: &HashMap<String, Vec<&KnowledgeChunkRow>>,
) -> (HashMap<Uuid, &'a KnowledgeChunkRow>, HashMap<String, Uuid>) {
    parent_chunks
        .iter()
        .filter_map(|chunk| {
            let checksum = chunk_text_reuse_key(chunk)?;
            new_chunks_by_checksum.contains_key(&checksum).then_some((checksum, chunk))
        })
        .fold(
            (HashMap::new(), HashMap::new()),
            |(mut by_id, mut by_checksum), (checksum, chunk)| {
                by_checksum.entry(checksum).or_insert(chunk.chunk_id);
                by_id.entry(chunk.chunk_id).or_insert(chunk);
                (by_id, by_checksum)
            },
        )
}

async fn load_parent_vectors(
    state: &AppState,
    parent_revision_id: Uuid,
    parent_batch: &[Uuid],
    embedding_model_key: &str,
) -> AnyhowResult<Vec<KnowledgeChunkVectorRow>> {
    state
        .search_store
        .list_chunk_vectors_by_chunks(
            parent_batch,
            embedding_model_key,
            KNOWLEDGE_CHUNK_VECTOR_KIND,
        )
        .await
        .with_context(|| {
            format!("failed to load parent chunk vectors for revision {parent_revision_id}")
        })
}

fn newest_parent_vectors(
    vectors: Vec<KnowledgeChunkVectorRow>,
) -> HashMap<Uuid, KnowledgeChunkVectorRow> {
    vectors.into_iter().fold(HashMap::new(), |mut current, vector| {
        match current.get(&vector.chunk_id) {
            Some(existing) if !chunk_vector_is_newer(&vector, existing) => {}
            _ => {
                current.insert(vector.chunk_id, vector);
            }
        }
        current
    })
}

async fn persist_parent_vector_matches(
    state: &AppState,
    current_vectors: &HashMap<Uuid, KnowledgeChunkVectorRow>,
    parent_chunk_by_id: &HashMap<Uuid, &KnowledgeChunkRow>,
    new_chunks_by_checksum: &HashMap<String, Vec<&KnowledgeChunkRow>>,
    reused_chunk_ids: &mut BTreeSet<Uuid>,
    embedding_model_key: &str,
    freshness_generation: i64,
    expected_dimensions: u64,
    expected_source_truth_version: i64,
    ingest_attempt: CanonicalIngestVectorWriteFence,
    cleanup_owned_vector_ids: &mut BTreeSet<Uuid>,
) -> AnyhowResult<()> {
    let mut rows = Vec::with_capacity(CHUNK_EMBEDDING_BATCH_SIZE);
    for (parent_chunk_id, parent_vector) in current_vectors {
        let Ok(parent_dimensions) = validate_embedding_vector_dimensions(
            expected_dimensions,
            &parent_vector.vector,
            format!("parent chunk vector {}", parent_vector.chunk_id),
        ) else {
            continue;
        };
        let Some(parent_chunk) = parent_chunk_by_id.get(parent_chunk_id) else {
            continue;
        };
        let Some(parent_checksum) = chunk_text_reuse_key(parent_chunk) else {
            continue;
        };
        let Some(new_matches) = new_chunks_by_checksum.get(&parent_checksum) else {
            continue;
        };
        for new_chunk in new_matches {
            if reused_chunk_ids.contains(&new_chunk.chunk_id) {
                continue;
            }
            rows.push(KnowledgeChunkVectorRow {
                vector_id: Uuid::now_v7(),
                workspace_id: new_chunk.workspace_id,
                library_id: new_chunk.library_id,
                chunk_id: new_chunk.chunk_id,
                revision_id: new_chunk.revision_id,
                embedding_model_key: embedding_model_key.to_string(),
                vector_kind: KNOWLEDGE_CHUNK_VECTOR_KIND.to_string(),
                dimensions: parent_dimensions,
                vector: parent_vector.vector.clone(),
                freshness_generation,
                created_at: Utc::now(),
                occurred_at: new_chunk.occurred_at,
                occurred_until: new_chunk.occurred_until,
            });
            reused_chunk_ids.insert(new_chunk.chunk_id);
            if rows.len() == CHUNK_EMBEDDING_BATCH_SIZE {
                persist_reused_vector_rows(
                    state,
                    &rows,
                    expected_source_truth_version,
                    embedding_model_key,
                    ingest_attempt,
                    cleanup_owned_vector_ids,
                )
                .await?;
                rows.clear();
            }
        }
    }
    if !rows.is_empty() {
        persist_reused_vector_rows(
            state,
            &rows,
            expected_source_truth_version,
            embedding_model_key,
            ingest_attempt,
            cleanup_owned_vector_ids,
        )
        .await?;
    }
    Ok(())
}

async fn persist_reused_vector_rows(
    state: &AppState,
    rows: &[KnowledgeChunkVectorRow],
    expected_source_truth_version: i64,
    embedding_model_key: &str,
    ingest_attempt: CanonicalIngestVectorWriteFence,
    cleanup_owned_vector_ids: &mut BTreeSet<Uuid>,
) -> AnyhowResult<()> {
    state
        .search_store
        .upsert_chunk_vectors_bulk_fenced(
            rows,
            &CanonicalVectorWriteFence {
                expected_source_truth_version,
                embedding_profile_key: embedding_model_key.to_string(),
                ingest_attempt: Some(ingest_attempt),
                advance_source_truth_version: false,
            },
        )
        .await
        .context("failed to source/profile-fenced persist reused chunk vectors")?;
    cleanup_owned_vector_ids.extend(rows.iter().map(|row| row.vector_id));
    Ok(())
}

fn chunk_vector_is_newer(
    candidate: &KnowledgeChunkVectorRow,
    existing: &KnowledgeChunkVectorRow,
) -> bool {
    candidate
        .freshness_generation
        .cmp(&existing.freshness_generation)
        .then_with(|| candidate.created_at.cmp(&existing.created_at))
        .then_with(|| candidate.vector_id.cmp(&existing.vector_id))
        .is_gt()
}

fn chunk_text_reuse_key(chunk: &KnowledgeChunkRow) -> Option<String> {
    (!chunk.normalized_text.trim().is_empty()).then(|| {
        let mut hasher = Sha256::new();
        hasher.update(chunk.normalized_text.as_bytes());
        hex::encode(hasher.finalize())
    })
}

async fn load_knowledge_chunk(state: &AppState, chunk_id: Uuid) -> AnyhowResult<KnowledgeChunkRow> {
    state
        .document_store
        .list_chunks_by_ids(&[chunk_id])
        .await
        .with_context(|| format!("failed to load knowledge chunk {}", chunk_id))?
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("knowledge chunk {} not found", chunk_id))
}

async fn resolve_chunk_vector_generation(
    state: &AppState,
    chunk: &KnowledgeChunkRow,
) -> AnyhowResult<i64> {
    if let Some(generation) = chunk.vector_generation.or(chunk.text_generation) {
        return Ok(generation);
    }

    let revision = state
        .document_store
        .get_revision(chunk.revision_id)
        .await
        .with_context(|| {
            format!(
                "failed to load revision {} while resolving chunk generation",
                chunk.revision_id
            )
        })?
        .ok_or_else(|| anyhow!("knowledge revision {} not found", chunk.revision_id))?;
    Ok(revision.revision_number)
}

async fn mark_revisions_vector_ready(
    state: &AppState,
    revision_ids: &BTreeSet<Uuid>,
) -> AnyhowResult<()> {
    for revision_id in revision_ids {
        let revision = state
            .document_store
            .get_revision(*revision_id)
            .await
            .with_context(|| format!("failed to load revision {}", revision_id))?
            .ok_or_else(|| anyhow!("knowledge revision {} not found", revision_id))?;
        let updated = state
            .document_store
            .update_revision_readiness(
                revision.revision_id,
                &revision.text_state,
                "ready",
                &revision.graph_state,
                revision.text_readable_at,
                Some(Utc::now()),
                revision.graph_ready_at,
                revision.superseded_by_revision_id,
            )
            .await
            .with_context(|| format!("failed to update vector readiness for {}", revision_id))?;
        if updated.is_none() {
            return Err(anyhow!(
                "knowledge revision {} disappeared during vector readiness update",
                revision_id
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::config::Settings;
    use sqlx::postgres::PgPoolOptions;
    use std::time::Duration;
    use tokio::time::timeout;

    #[test]
    fn low_level_vector_write_requires_active_execution_profile_provenance() {
        let library_id = Uuid::from_u128(42);
        assert!(
            validate_embedding_profile_write(
                library_id,
                "embedding-profile:v1:active",
                "embedding-profile:v1:active",
            )
            .is_ok()
        );
        assert!(matches!(
            validate_embedding_profile_write(
                library_id,
                "embedding-profile:v1:stale",
                "embedding-profile:v1:active",
            ),
            Err(QueryServiceError::StateConflict { .. })
        ));
    }

    #[test]
    fn vector_rebuild_count_must_match_the_active_chunk_inventory() {
        let library_id = Uuid::from_u128(43);

        assert!(validate_rebuilt_chunk_count(library_id, 0, 0).is_ok());
        assert!(validate_rebuilt_chunk_count(library_id, 12_345, 12_345).is_ok());
        let error = validate_rebuilt_chunk_count(library_id, 12_345, 10_000)
            .expect_err("a truncated rebuild must fail closed");
        assert!(matches!(error, QueryServiceError::StateConflict { .. }));
        assert!(error.to_string().contains("rebuild vector-plane --source-library"));
    }

    #[test]
    fn revision_embedding_batches_preserve_order_and_batch_limit() {
        let batches = revision_chunk_embedding_batches(CHUNK_EMBEDDING_BATCH_SIZE * 2 + 1);

        assert_eq!(batches.len(), 3);
        assert_eq!(batches[0], (0..CHUNK_EMBEDDING_BATCH_SIZE).collect::<Vec<_>>());
        assert_eq!(
            batches[1],
            (CHUNK_EMBEDDING_BATCH_SIZE..CHUNK_EMBEDDING_BATCH_SIZE * 2).collect::<Vec<_>>()
        );
        assert_eq!(batches[2], vec![CHUNK_EMBEDDING_BATCH_SIZE * 2]);
        assert!(revision_chunk_embedding_batches(0).is_empty());
    }

    #[test]
    fn embedding_usage_counts_preserve_provider_totals_and_fallback() {
        let explicit = EmbeddingUsageTotals {
            prompt_tokens: 5,
            completion_tokens: 7,
            total_tokens: 20,
            saw_prompt: true,
            saw_completion: true,
            saw_total: true,
        };
        assert_eq!(embedding_usage_token_counts(&explicit), (Some(5), Some(7), Some(20)));

        let derived = EmbeddingUsageTotals { saw_total: false, ..explicit };
        assert_eq!(embedding_usage_token_counts(&derived), (Some(5), Some(7), Some(12)));
        assert_eq!(
            embedding_usage_token_counts(&EmbeddingUsageTotals::default()),
            (None, None, None)
        );
    }

    #[test]
    fn unobserved_dimension_consumes_the_first_real_batch_exactly_once() {
        let mut batches = vec![vec![0, 1], vec![2, 3], vec![4]];
        let first = take_dimension_learning_batch(None, &mut batches).unwrap();
        assert_eq!(first, vec![0, 1]);
        assert_eq!(batches, vec![vec![2, 3], vec![4]]);

        let known = EmbeddingDimensions::try_from(3).unwrap();
        let unchanged = batches.clone();
        assert!(take_dimension_learning_batch(Some(known), &mut batches).is_none());
        assert_eq!(batches, unchanged);
    }

    #[test]
    fn cancellation_preserves_committed_attempt_vectors_without_cleanup() {
        let cancellation = CancellationToken::new();
        cancellation.cancel();

        let error = fail_embed_chunks_if_cancelled(Uuid::from_u128(49), &cancellation)
            .expect_err("cancelled embedding must stop");
        assert!(matches!(error, QueryServiceError::Cancelled));
        assert!(
            !error.preserves_partial_vectors(),
            "cancellation preservation is a lifecycle race invariant, not provider retry policy",
        );
    }

    #[tokio::test]
    async fn remote_rebuild_serialization_does_not_block_same_library_readers() {
        let service = SearchService::new();
        let library_id = Uuid::from_u128(44);
        let _rebuild = service.vector_rebuild_lock(library_id).lock_owned().await;

        let read_guard =
            timeout(Duration::from_secs(1), service.vector_plane_lock(library_id).read_owned())
                .await
                .expect("the rebuild lock must not exclude data-plane readers");
        drop(read_guard);
    }

    #[tokio::test]
    async fn library_write_lock_does_not_block_another_library() {
        let service = SearchService::new();
        let first_library_id = Uuid::from_u128(45);
        let second_library_id = Uuid::from_u128(46);
        let _first_write = service.vector_plane_lock(first_library_id).write_owned().await;

        let second_read = timeout(
            Duration::from_secs(1),
            service.vector_plane_lock(second_library_id).read_owned(),
        )
        .await
        .expect("one library's promotion must not block another library");
        drop(second_read);
    }

    #[tokio::test]
    #[ignore = "requires local postgres service"]
    async fn rebuild_session_lock_leaves_a_one_connection_app_pool_available() {
        let settings = Settings::from_env().expect("load postgres settings");
        let pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(&settings.database_url)
            .await
            .expect("connect one-slot postgres pool");
        let library_id = Uuid::now_v7();
        let rebuild_lock = acquire_vector_rebuild_advisory_lock(&settings.database_url, library_id)
            .await
            .expect("acquire dedicated rebuild session lock");

        let value = timeout(
            Duration::from_secs(1),
            sqlx::query_scalar::<_, i32>("select 1").fetch_one(&pool),
        )
        .await
        .expect("dedicated rebuild session must not consume the app pool")
        .expect("query through one-slot app pool");
        assert_eq!(value, 1);

        drop(rebuild_lock);
        pool.close().await;
    }

    #[test]
    fn staging_profile_keys_are_opaque_unique_and_not_canonical_aliases() {
        let canonical = "embedding-profile:v1:canonical";
        let first = vector_rebuild_staging_profile_key(canonical, Uuid::from_u128(47));
        let second = vector_rebuild_staging_profile_key(canonical, Uuid::from_u128(48));

        assert_ne!(first, canonical);
        assert_ne!(second, canonical);
        assert_ne!(first, second);
        assert!(first.starts_with(VECTOR_REBUILD_STAGING_PROFILE_PREFIX));
        assert_eq!(first.len(), VECTOR_REBUILD_STAGING_PROFILE_PREFIX.len() + 64);
    }

    #[test]
    fn rebuild_keyset_cursor_streams_more_than_ten_thousand_chunks_with_duplicate_indexes() {
        let chunks = (0..10_005usize)
            .map(|ordinal| {
                let mut chunk = make_chunk_for_reuse("neutral fixture");
                chunk.chunk_index = i32::try_from(ordinal / 2).unwrap_or(i32::MAX);
                chunk.chunk_id = Uuid::from_u128((ordinal + 1) as u128);
                chunk
            })
            .collect::<Vec<_>>();
        let mut after = None;
        let mut visited = Vec::new();

        loop {
            let page = chunks
                .iter()
                .filter(|chunk| {
                    after.is_none_or(|cursor| (chunk.chunk_index, chunk.chunk_id) > cursor)
                })
                .take(CHUNK_REBUILD_FETCH_PAGE_SIZE)
                .cloned()
                .collect::<Vec<_>>();
            let Some(next_after) = chunk_rebuild_page_cursor(&page) else {
                break;
            };
            visited.extend(page.iter().map(|chunk| chunk.chunk_id));
            if page.len() < CHUNK_REBUILD_FETCH_PAGE_SIZE {
                break;
            }
            after = Some(next_after);
        }

        assert_eq!(visited.len(), chunks.len());
        assert_eq!(visited.iter().copied().collect::<BTreeSet<_>>().len(), chunks.len());
    }

    #[test]
    fn deferred_rebuild_returns_primary_error_after_successful_reconciliation() {
        let result = finish_deferred_manifest_rebuild::<()>(
            Err(QueryServiceError::StateConflict { message: "primary".to_string() }),
            Ok(()),
        );

        assert!(matches!(result, Err(QueryServiceError::StateConflict { .. })));
    }

    #[test]
    fn deferred_rebuild_fails_closed_when_reconciliation_also_fails() {
        let result = finish_deferred_manifest_rebuild::<()>(
            Err(QueryServiceError::StateConflict { message: "primary".to_string() }),
            Err(QueryServiceError::StateConflict { message: "reconcile".to_string() }),
        );

        assert!(matches!(result, Err(QueryServiceError::Internal(_))));
    }

    #[test]
    fn chunk_text_reuse_key_is_content_addressed() {
        let left = make_chunk_for_reuse("alpha\nbeta");
        let right = make_chunk_for_reuse("alpha\nbeta");
        let changed = make_chunk_for_reuse("alpha\ngamma");
        let blank = make_chunk_for_reuse("   ");

        assert_eq!(chunk_text_reuse_key(&left), chunk_text_reuse_key(&right));
        assert_ne!(chunk_text_reuse_key(&left), chunk_text_reuse_key(&changed));
        assert_eq!(chunk_text_reuse_key(&blank), None);
    }

    fn make_chunk_for_reuse(normalized_text: &str) -> KnowledgeChunkRow {
        KnowledgeChunkRow {
            chunk_id: Uuid::now_v7(),
            workspace_id: Uuid::now_v7(),
            library_id: Uuid::now_v7(),
            document_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            chunk_index: 0,
            chunk_kind: Some("text".to_string()),
            content_text: normalized_text.to_string(),
            normalized_text: normalized_text.to_string(),
            span_start: Some(0),
            span_end: Some(i32::try_from(normalized_text.len()).unwrap_or(i32::MAX)),
            token_count: None,
            support_block_ids: Vec::new(),
            section_path: Vec::new(),
            heading_trail: Vec::new(),
            literal_digest: None,
            chunk_state: "ready".to_string(),
            text_generation: Some(1),
            vector_generation: Some(1),
            quality_score: None,
            window_text: None,
            raptor_level: None,
            occurred_at: None,
            occurred_until: None,
        }
    }

    // BUG B (c): after a transient failure that preserved the already-persisted
    // batches, the retry embeds ONLY the missing remainder — chunks whose
    // vectors survived are skipped.
    #[test]
    fn retry_embeds_only_chunks_missing_vectors() {
        let chunk_a = Uuid::now_v7();
        let chunk_b = Uuid::now_v7();
        let chunk_c = Uuid::now_v7();
        let all = [chunk_a, chunk_b, chunk_c];

        // Fresh revision: nothing persisted yet, so every chunk is embedded.
        let none_covered = BTreeSet::new();
        assert_eq!(chunk_ids_missing_vectors(&all, &none_covered), vec![chunk_a, chunk_b, chunk_c]);

        // Retry after a blip persisted A and B: only C remains.
        let mut partially_covered = BTreeSet::new();
        partially_covered.insert(chunk_a);
        partially_covered.insert(chunk_b);
        assert_eq!(chunk_ids_missing_vectors(&all, &partially_covered), vec![chunk_c]);

        // Final retry after the remainder also persisted: nothing left to embed.
        let fully_covered: BTreeSet<Uuid> = all.iter().copied().collect();
        assert!(chunk_ids_missing_vectors(&all, &fully_covered).is_empty());
    }

    // BUG B (d): readiness stays gated on FULL coverage. A revision is only
    // complete when embedded + reused == total; a half-embedded revision
    // (transient failure preserved partial vectors) never passes the gate.
    #[test]
    fn embed_readiness_is_gated_on_full_coverage() {
        // Fully covered by a single clean run.
        assert!(embed_coverage_is_complete(3, 0, 3));
        // Fully covered by resume: 1 reused from a prior attempt + 2 embedded now.
        assert!(embed_coverage_is_complete(2, 1, 3));
        // Half-embedded: a transient failure left only 2 of 3 covered.
        assert!(!embed_coverage_is_complete(2, 0, 3));
        // Still short after a partial resume.
        assert!(!embed_coverage_is_complete(1, 1, 3));
    }
}
