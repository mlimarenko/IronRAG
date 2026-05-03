use std::collections::{BTreeSet, HashMap};

use anyhow::{Context, Result, anyhow, bail};
use chrono::Utc;
use futures::stream::{self, StreamExt};
use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::ai::AiBindingPurpose,
    domains::query_ir::QueryIR,
    infra::arangodb::{
        collections::KNOWLEDGE_CHUNK_COLLECTION,
        document_store::KnowledgeChunkRow,
        graph_store::KnowledgeEntityRow,
        search_store::{
            KnowledgeChunkSearchRow, KnowledgeChunkVectorRow, KnowledgeEntitySearchRow,
            KnowledgeEntityVectorRow, KnowledgeRelationSearchRow, KnowledgeTechnicalFactSearchRow,
        },
    },
    infra::repositories::{ai_repository, content_repository},
    integrations::llm::EmbeddingBatchRequest,
    services::knowledge::service::RefreshKnowledgeLibraryGenerationCommand,
};

/// Per-batch size used for chunk embedding requests. Keeps each call below
/// the typical 8k-token provider soft cap even when chunks run long and
/// reduces the blast radius of one bad chunk failing the whole revision.
const CHUNK_EMBEDDING_BATCH_SIZE: usize = 16;
const CHUNK_VECTOR_REUSE_SOURCE_BATCH_SIZE: usize = 128;

const VECTOR_KIND_CHUNK: &str = "chunk_embedding";
const VECTOR_KIND_ENTITY: &str = "entity_embedding";
const FACT_FETCH_MULTIPLIER: usize = 2;
const FACT_FETCH_MIN: usize = 6;

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

#[derive(Clone, Default)]
pub struct SearchService;

impl SearchService {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    pub async fn search_query_evidence(
        &self,
        state: &AppState,
        library_id: Uuid,
        query: &str,
        query_ir: &QueryIR,
        limit: usize,
    ) -> Result<QueryEvidenceSearchResult> {
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
    ) -> Result<Uuid> {
        resolve_embedding_model_catalog_id(state, provider_kind, model_name).await
    }

    pub async fn persist_chunk_embeddings(
        &self,
        state: &AppState,
        writes: &[ChunkEmbeddingWrite],
    ) -> Result<usize> {
        let mut written = 0usize;
        for write in writes {
            let chunk = load_knowledge_chunk(state, write.chunk_id).await?;
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
                vector_kind: VECTOR_KIND_CHUNK.to_string(),
                dimensions: embedding_dimensions(&vector).with_context(|| {
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
    ) -> Result<usize> {
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
                vector_kind: VECTOR_KIND_ENTITY.to_string(),
                dimensions: embedding_dimensions(&vector).with_context(|| {
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
    ) -> Result<()> {
        let embedding_model_key = model_catalog_id.to_string();
        let rows = state
            .arango_search_store
            .list_chunk_vectors_by_chunk(chunk_id)
            .await
            .with_context(|| format!("failed to load chunk vectors for {}", chunk_id))?;
        let has_model = rows.iter().any(|row| row.embedding_model_key == embedding_model_key);
        if !has_model {
            return Err(anyhow!(
                "chunk {} has no canonical vector for model {}",
                chunk_id,
                model_catalog_id
            ));
        }
        Ok(())
    }

    pub async fn activate_graph_node_embedding_index(
        &self,
        state: &AppState,
        node_id: Uuid,
        model_catalog_id: Uuid,
    ) -> Result<()> {
        let embedding_model_key = model_catalog_id.to_string();
        let rows = state
            .arango_search_store
            .list_entity_vectors_by_entity(node_id)
            .await
            .with_context(|| format!("failed to load entity vectors for {}", node_id))?;
        let has_model = rows.iter().any(|row| row.embedding_model_key == embedding_model_key);
        if !has_model {
            return Err(anyhow!(
                "entity {} has no canonical vector for model {}",
                node_id,
                model_catalog_id
            ));
        }
        Ok(())
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
    ) -> Result<EmbedChunksStageOutcome> {
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
        let model_catalog_id = binding.model_catalog_id;
        let embedding_model_key = model_catalog_id.to_string();
        let parallelism = state.settings.ingestion_embedding_parallelism.max(1);
        let freshness_generation = revision.revision_number;
        let reused_chunk_ids = reuse_chunk_vectors_from_parent_revision(
            state,
            revision_id,
            chunks.as_slice(),
            model_catalog_id,
            &embedding_model_key,
            freshness_generation,
        )
        .await?;
        let chunks_to_embed: Vec<&KnowledgeChunkRow> =
            chunks.iter().filter(|chunk| !reused_chunk_ids.contains(&chunk.chunk_id)).collect();

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
            async move {
                let inputs: Vec<String> =
                    batch.iter().map(|index| chunks_ref[*index].normalized_text.clone()).collect();
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
                            "failed to embed chunk batch for revision {revision_id} starting at offset {first_offset}"
                        )
                    })?;
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
            let (batch, batch_response) = batch_result?;
            if batch_response.embeddings.len() != batch.len() {
                bail!(
                    "embedding batch returned {} vectors for {} chunks",
                    batch_response.embeddings.len(),
                    batch.len(),
                );
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

            let mut batch_rows: Vec<KnowledgeChunkVectorRow> = Vec::with_capacity(batch.len());
            for (chunk_index, vector) in batch.iter().zip(batch_response.embeddings.iter()) {
                let chunk = chunks_ref[*chunk_index];
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
                    vector_kind: VECTOR_KIND_CHUNK.to_string(),
                    dimensions: embedding_dimensions(vector.as_slice()).with_context(|| {
                        format!(
                            "failed to resolve chunk embedding dimensions for {}",
                            chunk.chunk_id
                        )
                    })?,
                    vector: vector.clone(),
                    freshness_generation,
                    created_at: Utc::now(),
                    occurred_at: chunk.occurred_at,
                    occurred_until: chunk.occurred_until,
                });
            }
            // Collapse N sequential `upsert_chunk_vector` AQLs into one
            // bulk FOR/UPSERT round-trip per embedding batch.
            if !batch_rows.is_empty() {
                state
                    .arango_search_store
                    .upsert_chunk_vectors_bulk(&batch_rows)
                    .await
                    .context("failed to bulk-persist chunk vectors")?;
                chunks_embedded += batch_rows.len();
            }
        }

        let covered_chunk_count = chunks_embedded.saturating_add(reused_chunk_ids.len());
        if covered_chunk_count != chunks.len() {
            bail!(
                "embedding coverage mismatch for revision {revision_id}: {} chunks, {} embedded, {} reused",
                chunks.len(),
                chunks_embedded,
                reused_chunk_ids.len(),
            );
        }
        let persisted_vector_count = state
            .arango_search_store
            .count_chunk_vectors_by_revision(
                revision_id,
                &embedding_model_key,
                VECTOR_KIND_CHUNK,
                freshness_generation,
            )
            .await
            .with_context(|| {
                format!(
                    "failed to verify persisted chunk-vector coverage for revision {revision_id}"
                )
            })?;
        if persisted_vector_count != chunks.len() {
            bail!(
                "persisted chunk-vector coverage mismatch for revision {revision_id}: {} chunks, {} current vectors",
                chunks.len(),
                persisted_vector_count,
            );
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
    ) -> Result<usize> {
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
                let mut local_max_gen: Option<i64> = None;
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
                        vector_kind: VECTOR_KIND_CHUNK.to_string(),
                        dimensions: embedding_dimensions(embedding.as_slice()).with_context(
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
                    local_max_gen = Some(local_max_gen.map_or(freshness_generation, |current| {
                        current.max(freshness_generation)
                    }));
                }
                anyhow::Ok((batch.len(), local_touched, local_max_gen))
            }
        }))
        .buffer_unordered(parallelism)
        .collect::<Vec<_>>()
        .await;

        let mut touched_revision_ids = BTreeSet::new();
        let mut max_vector_generation = None::<i64>;
        let mut rebuilt = 0usize;
        for batch_result in batch_results {
            let (count, local_touched, local_max_gen) = batch_result?;
            rebuilt += count;
            for revision_id in local_touched {
                touched_revision_ids.insert(revision_id);
            }
            if let Some(gen_value) = local_max_gen {
                max_vector_generation =
                    Some(max_vector_generation.map_or(gen_value, |current| current.max(gen_value)));
            }
        }

        mark_revisions_vector_ready(state, &touched_revision_ids)
            .await
            .context("failed to mark rebuilt revisions as vector-ready")?;
        if let Some(vector_generation) = max_vector_generation {
            refresh_library_vector_generation_if_present(
                state,
                library_id,
                chunks[0].workspace_id,
                vector_generation,
            )
            .await
            .context("failed to refresh library vector generation after chunk rebuild")?;
        }

        Ok(rebuilt)
    }

    pub async fn rebuild_graph_node_embeddings(
        &self,
        state: &AppState,
        library_id: Uuid,
    ) -> Result<usize> {
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

        let mut max_vector_generation = None::<i64>;
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
                return Err(anyhow!(
                    "embedding batch returned {} vectors for {} entities",
                    batch_response.embeddings.len(),
                    entity_batch.len()
                ));
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
                    vector_kind: VECTOR_KIND_ENTITY.to_string(),
                    dimensions: embedding_dimensions(embedding.as_slice()).with_context(|| {
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
                max_vector_generation =
                    Some(max_vector_generation.map_or(entity.freshness_generation, |current| {
                        current.max(entity.freshness_generation)
                    }));
                rebuilt += 1;
            }
        }

        if let Some(vector_generation) = max_vector_generation {
            refresh_library_vector_generation_if_present(
                state,
                library_id,
                entities[0].workspace_id,
                vector_generation,
            )
            .await
            .context("failed to refresh library vector generation after entity rebuild")?;
        }

        Ok(rebuilt)
    }
}

async fn resolve_embedding_model_catalog_id(
    state: &AppState,
    provider_kind: &str,
    model_name: &str,
) -> Result<Uuid> {
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

fn build_chunk_vector_key(
    chunk_id: Uuid,
    model_catalog_id: Uuid,
    freshness_generation: i64,
) -> String {
    format!("{chunk_id}:{model_catalog_id}:{freshness_generation}")
}

fn build_entity_vector_key(
    entity_id: Uuid,
    model_catalog_id: Uuid,
    freshness_generation: i64,
) -> String {
    format!("{entity_id}:{model_catalog_id}:{freshness_generation}")
}

fn embedding_dimensions(vector: &[f32]) -> Result<i32> {
    if vector.is_empty() {
        return Err(anyhow!("embedding vector must not be empty"));
    }
    i32::try_from(vector.len()).context("embedding vector dimension overflowed i32")
}

async fn reuse_chunk_vectors_from_parent_revision(
    state: &AppState,
    revision_id: Uuid,
    new_chunks: &[KnowledgeChunkRow],
    model_catalog_id: Uuid,
    embedding_model_key: &str,
    freshness_generation: i64,
) -> Result<BTreeSet<Uuid>> {
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
        let Some(checksum) = chunk_text_reuse_key(chunk) else { continue };
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
        let Some(checksum) = chunk_text_reuse_key(parent_chunk) else { continue };
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
            .list_chunk_vectors_by_chunks(parent_batch, embedding_model_key, VECTOR_KIND_CHUNK)
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
                    vector_kind: VECTOR_KIND_CHUNK.to_string(),
                    dimensions: parent_vector.dimensions,
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

async fn load_knowledge_chunk(state: &AppState, chunk_id: Uuid) -> Result<KnowledgeChunkRow> {
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
) -> Result<Vec<KnowledgeChunkRow>> {
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

async fn resolve_chunk_vector_generation(
    state: &AppState,
    chunk: &KnowledgeChunkRow,
) -> Result<i64> {
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
) -> Result<()> {
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

async fn refresh_library_vector_generation_if_present(
    state: &AppState,
    library_id: Uuid,
    workspace_id: Uuid,
    vector_generation: i64,
) -> Result<()> {
    let Some(existing) = state
        .canonical_services
        .knowledge
        .derive_library_generation_rows(state, library_id)
        .await
        .with_context(|| format!("failed to derive library generations for {}", library_id))?
        .into_iter()
        .next()
    else {
        return Ok(());
    };

    state
        .canonical_services
        .knowledge
        .refresh_library_generation(
            state,
            RefreshKnowledgeLibraryGenerationCommand {
                generation_id: existing.generation_id,
                workspace_id,
                library_id,
                active_text_generation: existing.active_text_generation,
                active_vector_generation: existing.active_vector_generation.max(vector_generation),
                active_graph_generation: existing.active_graph_generation,
                degraded_state: existing.degraded_state,
            },
        )
        .await
        .map_err(|error| {
            anyhow!("failed to refresh vector generation for library {}: {:?}", library_id, error)
        })?;
    Ok(())
}

fn decode_optional_single_result<T>(cursor: serde_json::Value) -> Result<Option<T>>
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

fn decode_many_results<T>(cursor: serde_json::Value) -> Result<Vec<T>>
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
