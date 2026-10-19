use crate::infra::knowledge_plane::StagedVectorRebuildPromotion;

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
            IngestVectorWriteFence, LexicalQueryOrigin, VECTOR_REBUILD_STAGING_PROFILE_PREFIX,
            VectorWriteFence,
        },
        knowledge_rows::{
            KNOWLEDGE_CHUNK_VECTOR_KIND, KNOWLEDGE_ENTITY_VECTOR_KIND, KnowledgeChunkRow,
            KnowledgeChunkSearchRow, KnowledgeChunkVectorRow, KnowledgeEntityRow,
            KnowledgeEntitySearchRow, KnowledgeEntityVectorRow, KnowledgeRelationSearchRow,
            KnowledgeTechnicalFactSearchRow,
        },
        repositories::{ai_repository, catalog_repository, content_revision_repository},
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

mod chunk_rebuild;
mod embedding;
mod evidence;
mod failures;
mod graph_rebuild;
mod persistence;
mod rebuild;
mod reuse;
#[cfg(test)]
mod tests;

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

pub use self::embedding::EmbedRevisionChunksInput;

/// Process-local read/write locks guarding a library's vector plane.
type VectorPlaneLockRegistry = Arc<StdMutex<HashMap<Uuid, Weak<RwLock<()>>>>>;
/// Process-local mutex guarding a library's staged vector rebuild.
type VectorRebuildLockRegistry = Arc<StdMutex<HashMap<Uuid, Weak<AsyncMutex<()>>>>>;

#[derive(Clone)]
pub struct SearchService {
    vector_plane_locks: VectorPlaneLockRegistry,
    vector_rebuild_locks: VectorRebuildLockRegistry,
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
        promotion: StagedVectorRebuildPromotion<'_>,
    ) -> std::result::Result<(), QueryServiceError> {
        let _promotion_guard = self
            .vector_plane_write_guard(state, promotion.library_id)
            .await
            .context("failed to acquire the vector promotion write guard")?;
        state
            .search_store
            .promote_staged_vector_rebuild(promotion)
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
}
