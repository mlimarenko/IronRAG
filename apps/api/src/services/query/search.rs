use std::{
    collections::{BTreeSet, HashMap},
    sync::Arc,
};

use anyhow::{Context, Result as AnyhowResult, anyhow, bail};
use chrono::Utc;
use futures::stream::{self, StreamExt};
use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256};
use sqlx::{Postgres, Transaction};
use tokio::sync::{OwnedRwLockReadGuard, OwnedRwLockWriteGuard, RwLock};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::ai::AiBindingPurpose,
    domains::query_ir::QueryIR,
    infra::arangodb::{
        collections::{
            KNOWLEDGE_CHUNK_COLLECTION, KNOWLEDGE_CHUNK_VECTOR_COLLECTION,
            KNOWLEDGE_ENTITY_VECTOR_COLLECTION, chunk_vector_collection_for_dim,
            entity_vector_collection_for_dim,
        },
        document_store::KnowledgeChunkRow,
        graph_store::KnowledgeEntityRow,
        search_store::{
            KNOWLEDGE_CHUNK_VECTOR_KIND, KNOWLEDGE_ENTITY_VECTOR_KIND, KnowledgeChunkSearchRow,
            KnowledgeChunkVectorRow, KnowledgeEntitySearchRow, KnowledgeEntityVectorRow,
            KnowledgeRelationSearchRow, KnowledgeTechnicalFactSearchRow,
        },
    },
    infra::repositories::{ai_repository, content_repository},
    integrations::llm::{EmbeddingBatchRequest, EmbeddingRequest},
    services::{
        ai_catalog_service::ResolvedRuntimeBinding,
        ingest::{
            cancellation::{StageError, ensure_not_cancelled},
            service::{INGEST_STAGE_EMBED_CHUNK, RecordStageUnitProgressCommand},
        },
    },
};

use super::{
    error::QueryServiceError,
    vector_dimensions::{
        invalidate_library_vector_index_dimensions, library_vector_index_dimensions,
        validate_embedding_vector_dimensions,
    },
};

/// Per-batch size used for chunk embedding requests. Keeps each call below
/// the typical 8k-token provider soft cap even when chunks run long and
/// reduces the blast radius of one bad chunk failing the whole revision.
const CHUNK_EMBEDDING_BATCH_SIZE: usize = 16;
const CHUNK_VECTOR_REUSE_SOURCE_BATCH_SIZE: usize = 128;
const FACT_FETCH_MULTIPLIER: usize = 2;
const FACT_FETCH_MIN: usize = 6;
const VECTOR_PLANE_ADVISORY_LOCK_KEY: &str = "query.vector_plane";

#[derive(Debug, Clone, PartialEq)]
pub struct ChunkEmbeddingWrite {
    pub chunk_id: Uuid,
    pub model_catalog_id: Uuid,
    pub embedding_vector: Vec<f32>,
    pub active: bool,
}

/// Outcome of an ingest-time chunk-embed call for a single revision.
/// Feeds the `embed_chunk` stage event (chunk count, elapsed, billing).
#[derive(Debug, Clone, Default)]
pub struct EmbedChunksStageOutcome {
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
    pub target_dimensions: u64,
    pub indexes_recreated: bool,
    pub libraries_rebuilt: usize,
    pub chunk_embeddings_rebuilt: usize,
    pub graph_node_embeddings_rebuilt: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GraphNodeEmbeddingWrite {
    pub node_id: Uuid,
    pub model_catalog_id: Uuid,
    pub embedding_vector: Vec<f32>,
    pub active: bool,
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
    vector_plane_lock: Arc<RwLock<()>>,
}

pub(crate) struct VectorPlaneReadGuard {
    _local: OwnedRwLockReadGuard<()>,
    _transaction: Transaction<'static, Postgres>,
}

struct VectorPlaneWriteGuard {
    _local: OwnedRwLockWriteGuard<()>,
    _transaction: Transaction<'static, Postgres>,
}

impl Default for SearchService {
    fn default() -> Self {
        Self::new()
    }
}

async fn sync_embed_chunk_stage_progress(
    state: &AppState,
    attempt_id: Option<Uuid>,
    completed_units: usize,
    total_units: usize,
) {
    let Some(attempt_id) = attempt_id else {
        return;
    };
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

impl SearchService {
    #[must_use]
    pub fn new() -> Self {
        Self { vector_plane_lock: Arc::new(RwLock::new(())) }
    }

    pub(crate) async fn vector_plane_read_guard(
        &self,
        state: &AppState,
    ) -> AnyhowResult<VectorPlaneReadGuard> {
        let mut transaction = state.persistence.postgres.begin().await?;
        sqlx::query("select pg_advisory_xact_lock_shared(hashtextextended($1::text, 0))")
            .bind(VECTOR_PLANE_ADVISORY_LOCK_KEY)
            .execute(&mut *transaction)
            .await?;
        let local = Arc::clone(&self.vector_plane_lock).read_owned().await;
        Ok(VectorPlaneReadGuard { _local: local, _transaction: transaction })
    }

    async fn vector_plane_write_guard(
        &self,
        state: &AppState,
    ) -> AnyhowResult<VectorPlaneWriteGuard> {
        let mut transaction = state.persistence.postgres.begin().await?;
        sqlx::query("select pg_advisory_xact_lock(hashtextextended($1::text, 0))")
            .bind(VECTOR_PLANE_ADVISORY_LOCK_KEY)
            .execute(&mut *transaction)
            .await?;
        let local = Arc::clone(&self.vector_plane_lock).write_owned().await;
        Ok(VectorPlaneWriteGuard { _local: local, _transaction: transaction })
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
            .arango_search_store
            .search_chunks(library_id, query, normalized_limit, temporal_start, temporal_end)
            .await
            .context("failed to search canonical knowledge chunks")?;
        let technical_fact_hits = state
            .arango_search_store
            .search_technical_facts(library_id, query, fact_limit)
            .await
            .context("failed to search canonical technical facts")?;
        let entity_hits = state
            .arango_search_store
            .search_entities(library_id, query, normalized_limit)
            .await
            .context("failed to search canonical entities")?;
        let relation_hits = state
            .arango_search_store
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

    #[must_use]
    pub fn select_current_chunk_vector<'a>(
        &self,
        rows: &'a [KnowledgeChunkVectorRow],
    ) -> Option<&'a KnowledgeChunkVectorRow> {
        rows.iter().max_by(|left, right| {
            left.freshness_generation
                .cmp(&right.freshness_generation)
                .then_with(|| left.created_at.cmp(&right.created_at))
                .then_with(|| left.vector_id.cmp(&right.vector_id))
        })
    }

    #[must_use]
    pub fn select_current_entity_vector<'a>(
        &self,
        rows: &'a [KnowledgeEntityVectorRow],
    ) -> Option<&'a KnowledgeEntityVectorRow> {
        rows.iter().max_by(|left, right| {
            left.freshness_generation
                .cmp(&right.freshness_generation)
                .then_with(|| left.created_at.cmp(&right.created_at))
                .then_with(|| left.vector_id.cmp(&right.vector_id))
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
        let _vector_guard = self.vector_plane_read_guard(state).await?;
        let mut dimensions_by_library: HashMap<Uuid, u64> = HashMap::new();
        let mut written = 0usize;
        for write in writes {
            let chunk = load_knowledge_chunk(state, write.chunk_id).await?;
            let expected_dimensions = match dimensions_by_library.get(&chunk.library_id).copied() {
                Some(dimensions) => dimensions,
                None => {
                    let dimensions =
                        library_vector_index_dimensions(state, chunk.library_id).await?;
                    dimensions_by_library.insert(chunk.library_id, dimensions);
                    dimensions
                }
            };
            let freshness_generation =
                resolve_chunk_vector_generation(state, &chunk).await.with_context(|| {
                    format!("failed to resolve vector generation for chunk {}", write.chunk_id)
                })?;
            let vector = write.embedding_vector.clone();
            let row = KnowledgeChunkVectorRow {
                key: build_chunk_vector_key(
                    write.chunk_id,
                    write.model_catalog_id,
                    freshness_generation,
                ),
                arango_id: None,
                arango_rev: None,
                vector_id: Uuid::now_v7(),
                workspace_id: chunk.workspace_id,
                library_id: chunk.library_id,
                chunk_id: chunk.chunk_id,
                revision_id: chunk.revision_id,
                embedding_model_key: write.model_catalog_id.to_string(),
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
            let _ =
                state.arango_search_store.upsert_chunk_vector(&row).await.with_context(|| {
                    format!("failed to persist chunk vector for {}", write.chunk_id)
                })?;
            if write.active {
                self.activate_chunk_embedding_index(state, write.chunk_id, write.model_catalog_id)
                    .await?;
            }
            written += 1;
        }
        Ok(written)
    }

    pub async fn persist_graph_node_embeddings(
        &self,
        state: &AppState,
        writes: &[GraphNodeEmbeddingWrite],
    ) -> std::result::Result<usize, QueryServiceError> {
        let _vector_guard = self.vector_plane_read_guard(state).await?;
        let mut dimensions_by_library: HashMap<Uuid, u64> = HashMap::new();
        let mut written = 0usize;
        for write in writes {
            let entity = state
                .arango_graph_store
                .get_entity_by_id(write.node_id)
                .await
                .with_context(|| {
                    format!("failed to load knowledge entity {}", write.node_id)
                })?
                .ok_or_else(|| {
                    anyhow!(
                        "graph node {} is not a canonical knowledge entity; relation or projection node vectors are not supported by the Arango search store",
                        write.node_id
                    )
                })?;
            let expected_dimensions = match dimensions_by_library.get(&entity.library_id).copied() {
                Some(dimensions) => dimensions,
                None => {
                    let dimensions =
                        library_vector_index_dimensions(state, entity.library_id).await?;
                    dimensions_by_library.insert(entity.library_id, dimensions);
                    dimensions
                }
            };
            let vector = write.embedding_vector.clone();
            let row = KnowledgeEntityVectorRow {
                key: build_entity_vector_key(
                    entity.entity_id,
                    write.model_catalog_id,
                    entity.freshness_generation,
                ),
                arango_id: None,
                arango_rev: None,
                vector_id: Uuid::now_v7(),
                workspace_id: entity.workspace_id,
                library_id: entity.library_id,
                entity_id: entity.entity_id,
                embedding_model_key: write.model_catalog_id.to_string(),
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
            let _ =
                state.arango_search_store.upsert_entity_vector(&row).await.with_context(|| {
                    format!("failed to persist canonical entity vector for {}", write.node_id)
                })?;
            if write.active {
                self.activate_graph_node_embedding_index(
                    state,
                    write.node_id,
                    write.model_catalog_id,
                )
                .await?;
            }
            written += 1;
        }
        Ok(written)
    }

    pub async fn activate_chunk_embedding_index(
        &self,
        state: &AppState,
        chunk_id: Uuid,
        model_catalog_id: Uuid,
    ) -> std::result::Result<(), QueryServiceError> {
        let embedding_model_key = model_catalog_id.to_string();
        let rows = state
            .arango_search_store
            .list_chunk_vectors_by_chunk(chunk_id)
            .await
            .with_context(|| format!("failed to load chunk vectors for {}", chunk_id))?;
        let has_model = rows.iter().any(|row| row.embedding_model_key == embedding_model_key);
        if !has_model {
            return Err(QueryServiceError::NotFound {
                message: format!(
                    "chunk {chunk_id} has no canonical vector for model {model_catalog_id}"
                ),
            });
        }
        Ok(())
    }

    pub async fn activate_graph_node_embedding_index(
        &self,
        state: &AppState,
        node_id: Uuid,
        model_catalog_id: Uuid,
    ) -> std::result::Result<(), QueryServiceError> {
        let embedding_model_key = model_catalog_id.to_string();
        let rows = state
            .arango_search_store
            .list_entity_vectors_by_entity(node_id)
            .await
            .with_context(|| format!("failed to load entity vectors for {}", node_id))?;
        let has_model = rows.iter().any(|row| row.embedding_model_key == embedding_model_key);
        if !has_model {
            return Err(QueryServiceError::NotFound {
                message: format!(
                    "entity {node_id} has no canonical vector for model {model_catalog_id}"
                ),
            });
        }
        Ok(())
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
        let _vector_guard = self.vector_plane_write_guard(state).await?;
        invalidate_library_vector_index_dimensions(library_id);
        let target_dimensions =
            crate::services::query::vector_dimensions::library_vector_index_dimensions(
                state, library_id,
            )
            .await?;
        let previous_dimensions = None;

        // Make sure the target-dim shards exist (and their ANN indexes).
        state
            .arango_search_store
            .ensure_chunk_vector_shard(target_dimensions)
            .await
            .context("failed to ensure chunk vector shard for library rebuild")?;
        state
            .arango_search_store
            .ensure_entity_vector_shard(target_dimensions)
            .await
            .context("failed to ensure entity vector shard for library rebuild")?;

        // Drop this library's rows from every per-dim shard whose dim
        // does not match the new target. Includes the legacy single-dim
        // collection — its rows are pre-migration material that must
        // not survive a binding change. The target-dim shard itself is
        // also cleared so the subsequent re-embed writes a clean set.
        let target_chunk_collection = chunk_vector_collection_for_dim(target_dimensions);
        let target_entity_collection = entity_vector_collection_for_dim(target_dimensions);

        let mut chunk_targets = vec![KNOWLEDGE_CHUNK_VECTOR_COLLECTION.to_string()];
        chunk_targets.extend(state.arango_client.list_per_dim_chunk_vector_collections().await?);
        for collection in &chunk_targets {
            if collection == &target_chunk_collection {
                continue;
            }
            let _ = state
                .arango_client
                .query_json(
                    "FOR row IN @@collection
                     FILTER row.library_id == @library_id
                     REMOVE row IN @@collection
                     RETURN OLD._key",
                    serde_json::json!({
                        "@collection": collection,
                        "library_id": library_id,
                    }),
                )
                .await
                .with_context(|| {
                    format!("failed to drop library {library_id} chunk vectors from {collection}")
                })?;
        }

        let mut entity_targets = vec![KNOWLEDGE_ENTITY_VECTOR_COLLECTION.to_string()];
        entity_targets.extend(state.arango_client.list_per_dim_entity_vector_collections().await?);
        for collection in &entity_targets {
            if collection == &target_entity_collection {
                continue;
            }
            let _ = state
                .arango_client
                .query_json(
                    "FOR row IN @@collection
                     FILTER row.library_id == @library_id
                     REMOVE row IN @@collection
                     RETURN OLD._key",
                    serde_json::json!({
                        "@collection": collection,
                        "library_id": library_id,
                    }),
                )
                .await
                .with_context(|| {
                    format!("failed to drop library {library_id} entity vectors from {collection}")
                })?;
        }

        let mut outcome = VectorPlaneRebuildOutcome {
            previous_dimensions,
            target_dimensions,
            indexes_recreated: false,
            libraries_rebuilt: 0,
            chunk_embeddings_rebuilt: 0,
            graph_node_embeddings_rebuilt: 0,
        };
        outcome.chunk_embeddings_rebuilt += self
            .rebuild_chunk_embeddings_with_expected_dimensions(state, library_id, target_dimensions)
            .await?;
        outcome.graph_node_embeddings_rebuilt += self
            .rebuild_graph_node_embeddings_with_expected_dimensions(
                state,
                library_id,
                target_dimensions,
            )
            .await?;
        outcome.libraries_rebuilt = 1;
        Ok(outcome)
    }

    #[allow(dead_code)]
    async fn probe_library_vector_dimensions(
        &self,
        state: &AppState,
        library_id: Uuid,
        dimension_cache: &mut HashMap<String, u64>,
    ) -> std::result::Result<u64, QueryServiceError> {
        let embed_binding = state
            .canonical_services
            .ai_catalog
            .resolve_active_runtime_binding(state, library_id, AiBindingPurpose::EmbedChunk)
            .await?
            .ok_or_else(|| {
                anyhow!("active embedding binding is not configured for library {library_id}")
            })?;
        let retrieve_binding = state
            .canonical_services
            .ai_catalog
            .resolve_active_runtime_binding(state, library_id, AiBindingPurpose::QueryRetrieve)
            .await?
            .ok_or_else(|| {
                anyhow!("active query retrieval binding is not configured for library {library_id}")
            })?;
        let embed_dimensions =
            probe_binding_vector_dimensions(state, &embed_binding, dimension_cache).await?;
        let retrieve_dimensions =
            probe_binding_vector_dimensions(state, &retrieve_binding, dimension_cache).await?;
        if embed_dimensions != retrieve_dimensions {
            return Err(anyhow!(
                "library {library_id} vector bindings disagree: embed_chunk produces {embed_dimensions} dimensions, query_retrieve produces {retrieve_dimensions}"
            )
            .into());
        }
        Ok(embed_dimensions)
    }

    /// Embeds every chunk of a single revision using the library's active
    /// EmbedChunk binding, persists the vectors into Arango's
    /// `knowledge_chunk_vector` collection, and returns per-stage usage
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
        attempt_id: Option<Uuid>,
        cancellation_token: &CancellationToken,
    ) -> std::result::Result<EmbedChunksStageOutcome, QueryServiceError> {
        ensure_not_cancelled(cancellation_token)?;
        let revision = state
            .arango_document_store
            .get_revision(revision_id)
            .await
            .with_context(|| format!("failed to load revision {revision_id}"))?
            .ok_or_else(|| anyhow!("knowledge revision {revision_id} not found"))?;
        let chunks = state
            .arango_document_store
            .list_chunks_by_revision(revision_id)
            .await
            .with_context(|| format!("failed to list chunks for revision {revision_id}"))?;
        ensure_not_cancelled(cancellation_token)?;
        if chunks.is_empty() {
            return Ok(EmbedChunksStageOutcome::default());
        }

        let binding = state
            .canonical_services
            .ai_catalog
            .resolve_active_runtime_binding(state, library_id, AiBindingPurpose::EmbedChunk)
            .await?
            .ok_or_else(|| {
                anyhow!("active embedding binding is not configured for library {library_id}")
            })?;
        ensure_not_cancelled(cancellation_token)?;
        let model_catalog_id = binding.model_catalog_id;
        let embedding_model_key = model_catalog_id.to_string();
        let parallelism = state.settings.ingestion_embedding_parallelism.max(1);
        let freshness_generation = revision.revision_number;
        let reused_chunk_ids = {
            let _vector_guard = self.vector_plane_read_guard(state).await?;
            let expected_dimensions = library_vector_index_dimensions(state, library_id).await?;
            let mut reused_chunk_ids = match load_current_revision_chunk_vector_ids(
                state,
                revision_id,
                chunks.as_slice(),
                &embedding_model_key,
                freshness_generation,
                expected_dimensions,
            )
            .await
            {
                Ok(reused_chunk_ids) => reused_chunk_ids,
                Err(error) => {
                    return fail_embed_chunks_after_cleanup(state, revision_id, error).await;
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
            match reuse_chunk_vectors_from_parent_revision(
                state,
                revision_id,
                chunks_missing_current_vectors.as_slice(),
                model_catalog_id,
                &embedding_model_key,
                freshness_generation,
                expected_dimensions,
            )
            .await
            {
                Ok(parent_reused_chunk_ids) => {
                    reused_chunk_ids.extend(parent_reused_chunk_ids);
                    reused_chunk_ids
                }
                Err(error) => {
                    return fail_embed_chunks_after_cleanup(state, revision_id, error).await;
                }
            }
        };
        ensure_not_cancelled(cancellation_token)?;
        let chunks_to_embed: Vec<&KnowledgeChunkRow> =
            chunks.iter().filter(|chunk| !reused_chunk_ids.contains(&chunk.chunk_id)).collect();
        sync_embed_chunk_stage_progress(state, attempt_id, reused_chunk_ids.len(), chunks.len())
            .await;

        let provider_kind_owned = binding.provider_kind.clone();
        let model_name_owned = binding.model_name.clone();
        let api_key_owned = binding.api_key.clone();
        let base_url_owned = binding.provider_base_url.clone();
        let extra_parameters_json_owned = binding.extra_parameters_json.clone();

        type ChunkBatch = Vec<usize>;
        let mut batches: Vec<ChunkBatch> = Vec::new();
        let mut current: ChunkBatch = Vec::with_capacity(CHUNK_EMBEDDING_BATCH_SIZE);
        for index in 0..chunks_to_embed.len() {
            current.push(index);
            if current.len() == CHUNK_EMBEDDING_BATCH_SIZE {
                batches.push(std::mem::replace(
                    &mut current,
                    Vec::with_capacity(CHUNK_EMBEDDING_BATCH_SIZE),
                ));
            }
        }
        if !current.is_empty() {
            batches.push(current);
        }

        let chunks_ref = &chunks_to_embed;
        let batch_responses = stream::iter(batches.into_iter().map(|batch| {
            let provider_kind = provider_kind_owned.clone();
            let model_name = model_name_owned.clone();
            let api_key = api_key_owned.clone();
            let base_url = base_url_owned.clone();
            let extra_parameters_json = extra_parameters_json_owned.clone();
            let cancellation_token = cancellation_token.clone();
            async move {
                ensure_not_cancelled(&cancellation_token)?;
                let inputs: Vec<String> =
                    batch.iter().map(|index| chunks_ref[*index].normalized_text.clone()).collect();
                let first_offset = batch.first().copied().unwrap_or_default();
                let request = EmbeddingBatchRequest {
                        provider_kind,
                        model_name,
                        inputs,
                        api_key_override: api_key,
                        base_url_override: base_url,
                        extra_parameters_json,
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
                ensure_not_cancelled(&cancellation_token)?;
                anyhow::Ok((batch, response))
            }
        }))
        .buffer_unordered(parallelism)
        .collect::<Vec<_>>()
        .await;

        let mut prompt_token_sum: i64 = 0;
        let mut completion_token_sum: i64 = 0;
        let mut total_token_sum: i64 = 0;
        let mut saw_prompt = false;
        let mut saw_completion = false;
        let mut saw_total = false;
        let mut chunks_embedded = 0usize;

        for batch_result in batch_responses {
            fail_embed_chunks_if_cancelled(state, revision_id, cancellation_token).await?;
            let (batch, batch_response) = match batch_result {
                Ok(batch_response) => batch_response,
                Err(error) => {
                    return fail_embed_chunks_after_cleanup(state, revision_id, error).await;
                }
            };
            if batch_response.embeddings.len() != batch.len() {
                return fail_embed_chunks_after_cleanup(
                    state,
                    revision_id,
                    anyhow!(
                        "embedding batch returned {} vectors for {} chunks",
                        batch_response.embeddings.len(),
                        batch.len(),
                    ),
                )
                .await;
            }
            if let Some(prompt) =
                batch_response.usage_json.get("prompt_tokens").and_then(|v| v.as_i64())
            {
                prompt_token_sum += prompt;
                saw_prompt = true;
            }
            if let Some(completion) =
                batch_response.usage_json.get("completion_tokens").and_then(|v| v.as_i64())
            {
                completion_token_sum += completion;
                saw_completion = true;
            }
            if let Some(total) =
                batch_response.usage_json.get("total_tokens").and_then(|v| v.as_i64())
            {
                total_token_sum += total;
                saw_total = true;
            }

            let persist_result = async {
                let _vector_guard = self.vector_plane_read_guard(state).await?;
                let expected_dimensions =
                    library_vector_index_dimensions(state, library_id).await?;
                let mut batch_rows: Vec<KnowledgeChunkVectorRow> = Vec::with_capacity(batch.len());
                for (chunk_index, vector) in batch.iter().zip(batch_response.embeddings.iter()) {
                    ensure_not_cancelled(cancellation_token)?;
                    let chunk = chunks_ref[*chunk_index];
                    let dimensions = validate_embedding_vector_dimensions(
                        expected_dimensions,
                        vector.as_slice(),
                        format!("chunk {}", chunk.chunk_id),
                    )
                    .with_context(|| {
                        format!(
                            "failed to resolve chunk embedding dimensions for {}",
                            chunk.chunk_id
                        )
                    })?;
                    batch_rows.push(KnowledgeChunkVectorRow {
                        key: build_chunk_vector_key(
                            chunk.chunk_id,
                            model_catalog_id,
                            freshness_generation,
                        ),
                        arango_id: None,
                        arango_rev: None,
                        vector_id: Uuid::now_v7(),
                        workspace_id: chunk.workspace_id,
                        library_id: chunk.library_id,
                        chunk_id: chunk.chunk_id,
                        revision_id: chunk.revision_id,
                        embedding_model_key: embedding_model_key.clone(),
                        vector_kind: KNOWLEDGE_CHUNK_VECTOR_KIND.to_string(),
                        dimensions,
                        vector: vector.clone(),
                        freshness_generation,
                        created_at: Utc::now(),
                        occurred_at: chunk.occurred_at,
                        occurred_until: chunk.occurred_until,
                    });
                }
                if !batch_rows.is_empty() {
                    state
                        .arango_search_store
                        .upsert_chunk_vectors_bulk(&batch_rows)
                        .await
                        .context("failed to bulk-persist chunk vectors")?;
                    ensure_not_cancelled(cancellation_token)?;
                }
                anyhow::Ok(batch_rows.len())
            }
            .await;
            let persisted_count = match persist_result {
                Ok(persisted_count) => persisted_count,
                Err(error) => {
                    return fail_embed_chunks_after_cleanup(state, revision_id, error).await;
                }
            };
            if persisted_count > 0 {
                chunks_embedded += persisted_count;
                sync_embed_chunk_stage_progress(
                    state,
                    attempt_id,
                    chunks_embedded.saturating_add(reused_chunk_ids.len()),
                    chunks.len(),
                )
                .await;
                fail_embed_chunks_if_cancelled(state, revision_id, cancellation_token).await?;
            }
        }

        fail_embed_chunks_if_cancelled(state, revision_id, cancellation_token).await?;
        let covered_chunk_count = chunks_embedded.saturating_add(reused_chunk_ids.len());
        if covered_chunk_count != chunks.len() {
            return fail_embed_chunks_after_cleanup(
                state,
                revision_id,
                anyhow!(
                "embedding coverage mismatch for revision {revision_id}: {} chunks, {} embedded, {} reused",
                chunks.len(),
                chunks_embedded,
                reused_chunk_ids.len(),
                ),
            )
            .await;
        }
        let persisted_vector_count = match state
            .arango_search_store
            .count_chunk_vectors_by_revision(
                revision_id,
                &embedding_model_key,
                KNOWLEDGE_CHUNK_VECTOR_KIND,
                freshness_generation,
            )
            .await
            .with_context(|| {
                format!(
                    "failed to verify persisted chunk-vector coverage for revision {revision_id}"
                )
            }) {
            Ok(persisted_vector_count) => persisted_vector_count,
            Err(error) => {
                return fail_embed_chunks_after_cleanup(state, revision_id, error).await;
            }
        };
        ensure_not_cancelled(cancellation_token)?;
        if persisted_vector_count != chunks.len() {
            return fail_embed_chunks_after_cleanup(
                state,
                revision_id,
                anyhow!(
                "persisted chunk-vector coverage mismatch for revision {revision_id}: {} chunks, {} current vectors",
                chunks.len(),
                persisted_vector_count,
                ),
            )
            .await;
        }

        let prompt_tokens = saw_prompt.then(|| i32::try_from(prompt_token_sum).unwrap_or(i32::MAX));
        let completion_tokens =
            saw_completion.then(|| i32::try_from(completion_token_sum).unwrap_or(i32::MAX));
        let total_tokens = if saw_total {
            Some(i32::try_from(total_token_sum).unwrap_or(i32::MAX))
        } else if saw_prompt || saw_completion {
            Some(
                i32::try_from(prompt_token_sum.saturating_add(completion_token_sum))
                    .unwrap_or(i32::MAX),
            )
        } else {
            None
        };

        let usage_json = serde_json::json!({
            "provider_kind": binding.provider_kind,
            "model_name": binding.model_name,
            "prompt_tokens": prompt_tokens,
            "completion_tokens": completion_tokens,
            "total_tokens": total_tokens,
            "chunks_embedded": chunks_embedded,
            "chunks_reused": reused_chunk_ids.len(),
        });

        Ok(EmbedChunksStageOutcome {
            chunks_embedded,
            chunks_reused: reused_chunk_ids.len(),
            usage_json: (chunks_embedded > 0).then_some(usage_json),
            provider_kind: Some(binding.provider_kind),
            model_name: Some(binding.model_name),
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
        let _vector_guard = self.vector_plane_write_guard(state).await?;
        let expected_dimensions = library_vector_index_dimensions(state, library_id).await?;
        self.rebuild_chunk_embeddings_with_expected_dimensions(
            state,
            library_id,
            expected_dimensions,
        )
        .await
    }

    async fn rebuild_chunk_embeddings_with_expected_dimensions(
        &self,
        state: &AppState,
        library_id: Uuid,
        expected_dimensions: u64,
    ) -> std::result::Result<usize, QueryServiceError> {
        let embedding_binding = state
            .canonical_services
            .ai_catalog
            .resolve_active_runtime_binding(state, library_id, AiBindingPurpose::EmbedChunk)
            .await?
            .ok_or_else(|| {
                anyhow!("active embedding binding is not configured for library {}", library_id)
            })?;
        let model_catalog_id = embedding_binding.model_catalog_id;
        let embedding_model_key = model_catalog_id.to_string();
        state
            .arango_search_store
            .delete_chunk_vectors_by_library(library_id)
            .await
            .context("failed to clear stale chunk vectors before rebuild")?;
        let chunks = list_knowledge_chunks_by_library(state, library_id)
            .await
            .context("failed to load knowledge chunks for chunk embedding rebuild")?;
        if chunks.is_empty() {
            return Ok(0);
        }

        // Resolve per-chunk freshness generation up-front so the parallel
        // embed path doesn't stack N+1 `get_revision` reads inside each
        // batch. Falls back to `chunk.text_generation`, then to the
        // revision number fetched once per distinct revision.
        let mut revision_number_cache: std::collections::HashMap<Uuid, i64> =
            std::collections::HashMap::new();
        let mut freshness_per_chunk: Vec<i64> = Vec::with_capacity(chunks.len());
        for chunk in &chunks {
            if let Some(generation) = chunk.vector_generation.or(chunk.text_generation) {
                freshness_per_chunk.push(generation);
                continue;
            }
            let cached = revision_number_cache.get(&chunk.revision_id).copied();
            let rn = if let Some(rn) = cached {
                rn
            } else {
                let revision = state
                    .arango_document_store
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
            };
            freshness_per_chunk.push(rn);
        }

        let parallelism = state.settings.ingestion_embedding_parallelism.max(1);
        let provider_kind_owned = embedding_binding.provider_kind.clone();
        let model_name_owned = embedding_binding.model_name.clone();
        let api_key_owned = embedding_binding.api_key.clone();
        let base_url_owned = embedding_binding.provider_base_url.clone();
        let extra_parameters_json_owned = embedding_binding.extra_parameters_json.clone();

        type ChunkBatch = Vec<usize>;
        let mut batches: Vec<ChunkBatch> = Vec::new();
        let mut current: ChunkBatch = Vec::with_capacity(CHUNK_EMBEDDING_BATCH_SIZE);
        for index in 0..chunks.len() {
            current.push(index);
            if current.len() == CHUNK_EMBEDDING_BATCH_SIZE {
                batches.push(std::mem::replace(
                    &mut current,
                    Vec::with_capacity(CHUNK_EMBEDDING_BATCH_SIZE),
                ));
            }
        }
        if !current.is_empty() {
            batches.push(current);
        }

        // Each task embeds one batch AND persists its vectors before
        // returning — so the outer `collect` sees already-upserted
        // batches and only has to fold per-batch counters. Previous
        // version collected all embed futures first and only upserted
        // in a serial second pass, so no vectors landed in Arango
        // during the embedding phase — a 15+ min window where queries
        // saw zero hits even though the backfill was "working".
        let chunks_ref = &chunks;
        let freshness_ref = &freshness_per_chunk;
        let embedding_model_key_ref = &embedding_model_key;
        let search_store = &state.arango_search_store;
        let batch_results = stream::iter(batches.into_iter().map(|batch| {
            let provider_kind = provider_kind_owned.clone();
            let model_name = model_name_owned.clone();
            let api_key = api_key_owned.clone();
            let base_url = base_url_owned.clone();
            let extra_parameters_json = extra_parameters_json_owned.clone();
            async move {
                let inputs: Vec<String> =
                    batch.iter().map(|idx| chunks_ref[*idx].normalized_text.clone()).collect();
                let first_offset = batch.first().copied().unwrap_or_default();
                let response = state
                    .llm_gateway
                    .embed_many(EmbeddingBatchRequest {
                        provider_kind,
                        model_name,
                        inputs,
                        api_key_override: api_key,
                        base_url_override: base_url,
                        extra_parameters_json,
                    })
                    .await
                    .with_context(|| {
                        format!(
                            "failed to rebuild chunk embeddings (batch starting at offset {first_offset})"
                        )
                    })?;
                if response.embeddings.len() != batch.len() {
                    bail!(
                        "embedding batch returned {} vectors for {} chunks",
                        response.embeddings.len(),
                        batch.len(),
                    );
                }

                let mut local_touched = Vec::new();
                for (index, embedding) in batch.iter().zip(response.embeddings.iter()) {
                    let chunk = &chunks_ref[*index];
                    let freshness_generation = freshness_ref[*index];
                    let row = KnowledgeChunkVectorRow {
                        key: build_chunk_vector_key(
                            chunk.chunk_id,
                            model_catalog_id,
                            freshness_generation,
                        ),
                        arango_id: None,
                        arango_rev: None,
                        vector_id: Uuid::now_v7(),
                        workspace_id: chunk.workspace_id,
                        library_id: chunk.library_id,
                        chunk_id: chunk.chunk_id,
                        revision_id: chunk.revision_id,
                        embedding_model_key: embedding_model_key_ref.clone(),
                        vector_kind: KNOWLEDGE_CHUNK_VECTOR_KIND.to_string(),
                        dimensions: validate_embedding_vector_dimensions(
                            expected_dimensions,
                            embedding.as_slice(),
                            format!("rebuilt chunk {}", chunk.chunk_id),
                        )
                        .with_context(
                            || {
                                format!(
                                    "failed to resolve rebuilt chunk vector dimensions for {}",
                                    chunk.chunk_id
                                )
                            },
                        )?,
                        vector: embedding.clone(),
                        freshness_generation,
                        created_at: Utc::now(),
                        occurred_at: chunk.occurred_at,
                        occurred_until: chunk.occurred_until,
                    };
                    search_store.upsert_chunk_vector(&row).await.with_context(|| {
                        format!("failed to persist rebuilt chunk vector for {}", chunk.chunk_id)
                    })?;
                    local_touched.push(chunk.revision_id);
                }
                anyhow::Ok((batch.len(), local_touched))
            }
        }))
        .buffer_unordered(parallelism)
        .collect::<Vec<_>>()
        .await;

        let mut touched_revision_ids = BTreeSet::new();
        let mut rebuilt = 0usize;
        for batch_result in batch_results {
            let (count, local_touched) = batch_result?;
            rebuilt += count;
            for revision_id in local_touched {
                touched_revision_ids.insert(revision_id);
            }
        }

        mark_revisions_vector_ready(state, &touched_revision_ids)
            .await
            .context("failed to mark rebuilt revisions as vector-ready")?;

        Ok(rebuilt)
    }

    pub async fn rebuild_graph_node_embeddings(
        &self,
        state: &AppState,
        library_id: Uuid,
    ) -> std::result::Result<usize, QueryServiceError> {
        let _vector_guard = self.vector_plane_write_guard(state).await?;
        let expected_dimensions = library_vector_index_dimensions(state, library_id).await?;
        self.rebuild_graph_node_embeddings_with_expected_dimensions(
            state,
            library_id,
            expected_dimensions,
        )
        .await
    }

    async fn rebuild_graph_node_embeddings_with_expected_dimensions(
        &self,
        state: &AppState,
        library_id: Uuid,
        expected_dimensions: u64,
    ) -> std::result::Result<usize, QueryServiceError> {
        let embedding_binding = state
            .canonical_services
            .ai_catalog
            .resolve_active_runtime_binding(state, library_id, AiBindingPurpose::EmbedChunk)
            .await?
            .ok_or_else(|| {
                anyhow!("active embedding binding is not configured for library {}", library_id)
            })?;
        let model_catalog_id = embedding_binding.model_catalog_id;
        state
            .arango_search_store
            .delete_entity_vectors_by_library(library_id)
            .await
            .context("failed to clear stale entity vectors before rebuild")?;
        let entities = state
            .arango_graph_store
            .list_entities_by_library(library_id)
            .await
            .context("failed to load knowledge entities for canonical vector rebuild")?;
        if entities.is_empty() {
            return Ok(0);
        }

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
            if batch_response.embeddings.len() != entity_batch.len() {
                return Err(QueryServiceError::ProviderUnavailable {
                    message: format!(
                        "embedding batch returned {} vectors for {} entities",
                        batch_response.embeddings.len(),
                        entity_batch.len()
                    ),
                });
            }

            for (entity, embedding) in entity_batch.iter().zip(batch_response.embeddings.iter()) {
                let row = KnowledgeEntityVectorRow {
                    key: build_entity_vector_key(
                        entity.entity_id,
                        model_catalog_id,
                        entity.freshness_generation,
                    ),
                    arango_id: None,
                    arango_rev: None,
                    vector_id: Uuid::now_v7(),
                    workspace_id: entity.workspace_id,
                    library_id: entity.library_id,
                    entity_id: entity.entity_id,
                    embedding_model_key: model_catalog_id.to_string(),
                    vector_kind: KNOWLEDGE_ENTITY_VECTOR_KIND.to_string(),
                    dimensions: validate_embedding_vector_dimensions(
                        expected_dimensions,
                        embedding.as_slice(),
                        format!("rebuilt entity {}", entity.entity_id),
                    )
                    .with_context(|| {
                        format!(
                            "failed to resolve rebuilt entity vector dimensions for {}",
                            entity.entity_id
                        )
                    })?,
                    vector: embedding.clone(),
                    freshness_generation: entity.freshness_generation,
                    created_at: Utc::now(),
                };
                let _ = state.arango_search_store.upsert_entity_vector(&row).await.with_context(
                    || format!("failed to persist rebuilt entity vector for {}", entity.entity_id),
                )?;
                self.activate_graph_node_embedding_index(state, entity.entity_id, model_catalog_id)
                    .await?;
                rebuilt += 1;
            }
        }

        Ok(rebuilt)
    }
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

#[allow(dead_code)]
async fn probe_binding_vector_dimensions(
    state: &AppState,
    binding: &ResolvedRuntimeBinding,
    dimension_cache: &mut HashMap<String, u64>,
) -> AnyhowResult<u64> {
    let fingerprint = runtime_binding_vector_fingerprint(binding);
    if let Some(dimensions) = dimension_cache.get(&fingerprint).copied() {
        return Ok(dimensions);
    }
    let response = state
        .llm_gateway
        .embed(EmbeddingRequest {
            provider_kind: binding.provider_kind.clone(),
            model_name: binding.model_name.clone(),
            input: "vector dimension probe".to_string(),
            api_key_override: binding.api_key.clone(),
            base_url_override: binding.provider_base_url.clone(),
            extra_parameters_json: binding.extra_parameters_json.clone(),
        })
        .await
        .with_context(|| {
            format!(
                "failed to probe vector dimensions for {}/{}",
                binding.provider_kind, binding.model_name
            )
        })?;
    let dimensions = u64::try_from(response.embedding.len())
        .context("embedding vector dimension overflowed u64")?;
    let reported_dimensions = u64::try_from(response.dimensions)
        .context("reported embedding dimension overflowed u64")?;
    anyhow::ensure!(
        dimensions == reported_dimensions,
        "embedding response for {}/{} reported {reported_dimensions} dimensions but returned {dimensions} values",
        binding.provider_kind,
        binding.model_name
    );
    validate_embedding_vector_dimensions(
        dimensions,
        &response.embedding,
        format!("{}/{} dimension probe", binding.provider_kind, binding.model_name),
    )?;
    dimension_cache.insert(fingerprint, dimensions);
    Ok(dimensions)
}

#[allow(dead_code)]
fn runtime_binding_vector_fingerprint(binding: &ResolvedRuntimeBinding) -> String {
    let extra_parameters =
        serde_json::to_string(&binding.extra_parameters_json).unwrap_or_else(|_| "{}".to_string());
    format!(
        "{}:{}:{:?}:{}:{}:{}",
        binding.credential_id,
        binding.provider_kind,
        binding.provider_base_url,
        binding.model_catalog_id,
        binding.model_name,
        extra_parameters,
    )
}

fn build_chunk_vector_key(
    chunk_id: Uuid,
    model_catalog_id: Uuid,
    freshness_generation: i64,
) -> String {
    format!("{chunk_id}:{model_catalog_id}:{freshness_generation}")
}

async fn fail_embed_chunks_after_cleanup<T>(
    state: &AppState,
    revision_id: Uuid,
    error: anyhow::Error,
) -> std::result::Result<T, QueryServiceError> {
    match state.arango_search_store.delete_chunk_vectors_by_revision(revision_id).await {
        Ok(deleted) if deleted > 0 => {
            tracing::warn!(
                revision_id = %revision_id,
                deleted,
                "removed partial chunk vectors after failed embed_chunk stage",
            );
        }
        Ok(_) => {}
        Err(cleanup_error) => {
            return Err(error.context(format!(
                "failed to remove partial chunk vectors for revision {revision_id}: {cleanup_error:#}"
            )).into());
        }
    }
    Err(error.into())
}

async fn fail_embed_chunks_if_cancelled(
    state: &AppState,
    revision_id: Uuid,
    cancellation_token: &CancellationToken,
) -> std::result::Result<(), QueryServiceError> {
    if cancellation_token.is_cancelled() {
        fail_embed_chunks_after_cleanup(
            state,
            revision_id,
            anyhow::Error::new(StageError::Cancelled),
        )
        .await
    } else {
        Ok(())
    }
}

fn build_entity_vector_key(
    entity_id: Uuid,
    model_catalog_id: Uuid,
    freshness_generation: i64,
) -> String {
    format!("{entity_id}:{model_catalog_id}:{freshness_generation}")
}

async fn load_current_revision_chunk_vector_ids(
    state: &AppState,
    revision_id: Uuid,
    chunks: &[KnowledgeChunkRow],
    embedding_model_key: &str,
    freshness_generation: i64,
    expected_dimensions: u64,
) -> AnyhowResult<BTreeSet<Uuid>> {
    let mut reused_chunk_ids = BTreeSet::new();
    for chunk_batch in chunks.chunks(CHUNK_VECTOR_REUSE_SOURCE_BATCH_SIZE) {
        let chunk_ids = chunk_batch.iter().map(|chunk| chunk.chunk_id).collect::<Vec<_>>();
        let vectors = state
            .arango_search_store
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
                reused_chunk_ids.insert(vector.chunk_id);
            }
        }
    }
    Ok(reused_chunk_ids)
}

async fn reuse_chunk_vectors_from_parent_revision(
    state: &AppState,
    revision_id: Uuid,
    new_chunks: &[KnowledgeChunkRow],
    model_catalog_id: Uuid,
    embedding_model_key: &str,
    freshness_generation: i64,
    expected_dimensions: u64,
) -> AnyhowResult<BTreeSet<Uuid>> {
    if new_chunks.is_empty() {
        return Ok(BTreeSet::new());
    }
    let Some(revision_row) =
        content_repository::get_revision_by_id(&state.persistence.postgres, revision_id)
            .await
            .with_context(|| {
                format!("failed to load content revision {revision_id} for vector reuse")
            })?
    else {
        return Ok(BTreeSet::new());
    };
    let Some(parent_revision_id) = revision_row.parent_revision_id else {
        return Ok(BTreeSet::new());
    };

    let mut new_chunks_by_checksum: HashMap<String, Vec<&KnowledgeChunkRow>> = HashMap::new();
    for chunk in new_chunks {
        let Some(checksum) = chunk_text_reuse_key(chunk) else {
            continue;
        };
        new_chunks_by_checksum.entry(checksum).or_default().push(chunk);
    }
    if new_chunks_by_checksum.is_empty() {
        return Ok(BTreeSet::new());
    }

    let parent_chunks = state
        .arango_document_store
        .list_chunks_by_revision(parent_revision_id)
        .await
        .with_context(|| {
            format!("failed to list parent chunks for vector reuse from {parent_revision_id}")
        })?;
    if parent_chunks.is_empty() {
        return Ok(BTreeSet::new());
    }

    let mut parent_chunk_by_id: HashMap<Uuid, &KnowledgeChunkRow> = HashMap::new();
    let mut parent_ids_by_checksum: HashMap<String, Uuid> = HashMap::new();
    for parent_chunk in &parent_chunks {
        let Some(checksum) = chunk_text_reuse_key(parent_chunk) else {
            continue;
        };
        if !new_chunks_by_checksum.contains_key(&checksum) {
            continue;
        }
        parent_ids_by_checksum.entry(checksum).or_insert(parent_chunk.chunk_id);
        parent_chunk_by_id.entry(parent_chunk.chunk_id).or_insert(parent_chunk);
    }
    if parent_ids_by_checksum.is_empty() {
        return Ok(BTreeSet::new());
    }

    let mut reused_chunk_ids = BTreeSet::new();
    let parent_ids: Vec<Uuid> = parent_ids_by_checksum.values().copied().collect();
    for parent_batch in parent_ids.chunks(CHUNK_VECTOR_REUSE_SOURCE_BATCH_SIZE) {
        let vectors = state
            .arango_search_store
            .list_chunk_vectors_by_chunks(
                parent_batch,
                embedding_model_key,
                KNOWLEDGE_CHUNK_VECTOR_KIND,
            )
            .await
            .with_context(|| {
                format!("failed to load parent chunk vectors for revision {parent_revision_id}")
            })?;
        let mut current_vector_by_parent: HashMap<Uuid, KnowledgeChunkVectorRow> = HashMap::new();
        for vector in vectors {
            match current_vector_by_parent.get(&vector.chunk_id) {
                Some(existing) if !chunk_vector_is_newer(&vector, existing) => {}
                _ => {
                    current_vector_by_parent.insert(vector.chunk_id, vector);
                }
            }
        }

        let mut rows: Vec<KnowledgeChunkVectorRow> = Vec::with_capacity(CHUNK_EMBEDDING_BATCH_SIZE);
        for (parent_chunk_id, parent_vector) in current_vector_by_parent {
            let Ok(parent_dimensions) = validate_embedding_vector_dimensions(
                expected_dimensions,
                &parent_vector.vector,
                format!("parent chunk vector {}", parent_vector.chunk_id),
            ) else {
                continue;
            };
            let Some(parent_chunk) = parent_chunk_by_id.get(&parent_chunk_id) else {
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
                    key: build_chunk_vector_key(
                        new_chunk.chunk_id,
                        model_catalog_id,
                        freshness_generation,
                    ),
                    arango_id: None,
                    arango_rev: None,
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
                    state
                        .arango_search_store
                        .upsert_chunk_vectors_bulk(rows.as_slice())
                        .await
                        .context("failed to bulk-persist reused chunk vectors")?;
                    rows.clear();
                }
            }
        }
        if !rows.is_empty() {
            state
                .arango_search_store
                .upsert_chunk_vectors_bulk(rows.as_slice())
                .await
                .context("failed to bulk-persist reused chunk vectors")?;
        }
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
    let cursor = state
        .arango_document_store
        .client()
        .query_json(
            "FOR chunk IN @@collection
             FILTER chunk.chunk_id == @chunk_id
             LIMIT 1
             RETURN chunk",
            serde_json::json!({
                "@collection": KNOWLEDGE_CHUNK_COLLECTION,
                "chunk_id": chunk_id,
            }),
        )
        .await
        .with_context(|| format!("failed to load knowledge chunk {}", chunk_id))?;
    decode_optional_single_result(cursor)?
        .ok_or_else(|| anyhow!("knowledge chunk {} not found", chunk_id))
}

async fn list_knowledge_chunks_by_library(
    state: &AppState,
    library_id: Uuid,
) -> AnyhowResult<Vec<KnowledgeChunkRow>> {
    let cursor = state
        .arango_document_store
        .client()
        .query_json(
            "FOR chunk IN @@collection
             FILTER chunk.library_id == @library_id
             SORT chunk.revision_id ASC, chunk.chunk_index ASC, chunk.chunk_id ASC
             RETURN chunk",
            serde_json::json!({
                "@collection": KNOWLEDGE_CHUNK_COLLECTION,
                "library_id": library_id,
            }),
        )
        .await
        .with_context(|| format!("failed to list knowledge chunks for library {}", library_id))?;
    decode_many_results(cursor)
}

#[allow(dead_code)]
async fn library_has_vector_material(state: &AppState, library_id: Uuid) -> AnyhowResult<bool> {
    Ok(collection_has_library_row(state, KNOWLEDGE_CHUNK_VECTOR_COLLECTION, library_id).await?
        || collection_has_library_row(state, KNOWLEDGE_ENTITY_VECTOR_COLLECTION, library_id)
            .await?)
}

#[allow(dead_code)]
async fn collection_has_library_row(
    state: &AppState,
    collection: &str,
    library_id: Uuid,
) -> AnyhowResult<bool> {
    let cursor = state
        .arango_document_store
        .client()
        .query_json(
            "FOR row IN @@collection
             FILTER row.library_id == @library_id
             LIMIT 1
             RETURN 1",
            serde_json::json!({
                "@collection": collection,
                "library_id": library_id,
            }),
        )
        .await
        .with_context(|| {
            format!("failed to inspect whether {collection} has rows for library {library_id}")
        })?;
    let rows: Vec<i32> = decode_many_results(cursor)?;
    Ok(!rows.is_empty())
}

async fn resolve_chunk_vector_generation(
    state: &AppState,
    chunk: &KnowledgeChunkRow,
) -> AnyhowResult<i64> {
    if let Some(generation) = chunk.vector_generation.or(chunk.text_generation) {
        return Ok(generation);
    }

    let revision = state
        .arango_document_store
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
            .arango_document_store
            .get_revision(*revision_id)
            .await
            .with_context(|| format!("failed to load revision {}", revision_id))?
            .ok_or_else(|| anyhow!("knowledge revision {} not found", revision_id))?;
        let updated = state
            .arango_document_store
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

fn decode_optional_single_result<T>(cursor: serde_json::Value) -> AnyhowResult<Option<T>>
where
    T: DeserializeOwned,
{
    let result = cursor
        .get("result")
        .cloned()
        .ok_or_else(|| anyhow!("ArangoDB cursor response is missing result"))?;
    let mut rows: Vec<T> =
        serde_json::from_value(result).context("failed to decode ArangoDB result rows")?;
    Ok((!rows.is_empty()).then(|| rows.remove(0)))
}

fn decode_many_results<T>(cursor: serde_json::Value) -> AnyhowResult<Vec<T>>
where
    T: DeserializeOwned,
{
    let result = cursor
        .get("result")
        .cloned()
        .ok_or_else(|| anyhow!("ArangoDB cursor response is missing result"))?;
    serde_json::from_value(result).context("failed to decode ArangoDB result rows")
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, Utc};

    #[test]
    fn current_chunk_vector_selection_prefers_latest_generation() {
        let chunk_id = Uuid::now_v7();
        let model_catalog_id = Uuid::now_v7();
        let old = KnowledgeChunkVectorRow {
            key: "old".to_string(),
            arango_id: None,
            arango_rev: None,
            vector_id: Uuid::now_v7(),
            workspace_id: Uuid::now_v7(),
            library_id: Uuid::now_v7(),
            chunk_id,
            revision_id: Uuid::now_v7(),
            embedding_model_key: model_catalog_id.to_string(),
            vector_kind: "chunk_embedding".to_string(),
            dimensions: 3,
            vector: vec![1.0, 2.0, 3.0],
            freshness_generation: 1,
            created_at: Utc::now() - Duration::minutes(1),
            occurred_at: None,
            occurred_until: None,
        };
        let new = KnowledgeChunkVectorRow {
            key: "new".to_string(),
            freshness_generation: 2,
            created_at: Utc::now(),
            ..old.clone()
        };

        let candidates = [old, new.clone()];
        let selected =
            SearchService::new().select_current_chunk_vector(&candidates).expect("chunk vector");
        assert_eq!(selected.freshness_generation, new.freshness_generation);
    }

    #[test]
    fn current_entity_vector_selection_prefers_latest_generation() {
        let entity_id = Uuid::now_v7();
        let model_catalog_id = Uuid::now_v7();
        let old = KnowledgeEntityVectorRow {
            key: "old".to_string(),
            arango_id: None,
            arango_rev: None,
            vector_id: Uuid::now_v7(),
            workspace_id: Uuid::now_v7(),
            library_id: Uuid::now_v7(),
            entity_id,
            embedding_model_key: model_catalog_id.to_string(),
            vector_kind: "entity_embedding".to_string(),
            dimensions: 3,
            vector: vec![1.0, 2.0, 3.0],
            freshness_generation: 1,
            created_at: Utc::now() - Duration::minutes(1),
        };
        let new = KnowledgeEntityVectorRow {
            key: "new".to_string(),
            freshness_generation: 2,
            created_at: Utc::now(),
            ..old.clone()
        };

        let candidates = [old, new.clone()];
        let selected =
            SearchService::new().select_current_entity_vector(&candidates).expect("entity vector");
        assert_eq!(selected.freshness_generation, new.freshness_generation);
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
            key: Uuid::now_v7().to_string(),
            arango_id: None,
            arango_rev: None,
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
}
