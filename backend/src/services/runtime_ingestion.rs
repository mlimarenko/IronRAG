use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context, bail};
use chrono::Utc;
use serde_json::json;
use tokio::{task::JoinHandle, time};
use tracing::warn;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::{
        ai::AiBindingPurpose,
        provider_profiles::{EffectiveProviderProfile, ProviderModelSelection},
        runtime_ingestion::RuntimeDocumentActivityStatus,
    },
    infra::repositories::{
        self, IngestionExecutionPayload, RuntimeExtractedContentRow, RuntimeGraphEdgeRow,
        RuntimeGraphNodeRow, RuntimeIngestionRunRow,
    },
    integrations::llm::{EmbeddingBatchRequest, EmbeddingBatchResponse},
    services::search_service::ChunkEmbeddingWrite,
    shared::file_extract::{UploadFileKind, extraction_quality_from_source_map},
};

const EMBEDDING_BATCH_SIZE: usize = 16;

#[derive(Debug, Clone)]
struct PersistedExtractedContentInput {
    extraction_kind: String,
    content_text: Option<String>,
    page_count: Option<i32>,
    char_count: Option<i32>,
    extraction_warnings_json: serde_json::Value,
    source_map_json: serde_json::Value,
    provider_kind: Option<String>,
    model_name: Option<String>,
    extraction_version: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct RuntimeStageUsageSummary {
    pub call_count: usize,
    pub provider_kind: Option<String>,
    pub model_name: Option<String>,
    pub prompt_tokens: Option<i32>,
    pub completion_tokens: Option<i32>,
    pub total_tokens: Option<i32>,
    prompt_token_sum: i64,
    completion_token_sum: i64,
    total_token_sum: i64,
    saw_prompt_tokens: bool,
    saw_completion_tokens: bool,
    saw_total_tokens: bool,
}

#[derive(Debug, Clone)]
pub struct JobLeaseHeartbeat {
    job_id: Uuid,
    worker_id: String,
    runtime_ingestion_run_id: Option<Uuid>,
    lease_duration: chrono::Duration,
    min_interval: Duration,
    last_renewed_at: Instant,
}

#[derive(Debug)]
pub struct JobLeaseKeepAlive {
    handle: JoinHandle<()>,
}

impl RuntimeStageUsageSummary {
    #[must_use]
    pub fn with_model(provider_kind: &str, model_name: &str) -> Self {
        Self {
            provider_kind: Some(provider_kind.to_string()),
            model_name: Some(model_name.to_string()),
            ..Self::default()
        }
    }

    pub fn absorb_usage_json(&mut self, usage_json: &serde_json::Value) {
        self.call_count += 1;
        if let Some(prompt_tokens) =
            usage_json.get("prompt_tokens").and_then(serde_json::Value::as_i64)
        {
            self.prompt_token_sum += prompt_tokens;
            self.saw_prompt_tokens = true;
        }
        if let Some(completion_tokens) =
            usage_json.get("completion_tokens").and_then(serde_json::Value::as_i64)
        {
            self.completion_token_sum += completion_tokens;
            self.saw_completion_tokens = true;
        }
        if let Some(total_tokens) =
            usage_json.get("total_tokens").and_then(serde_json::Value::as_i64)
        {
            self.total_token_sum += total_tokens;
            self.saw_total_tokens = true;
        }
    }

    #[must_use]
    pub fn prompt_tokens(&self) -> Option<i32> {
        self.finalized_clone().prompt_tokens
    }

    #[must_use]
    pub fn completion_tokens(&self) -> Option<i32> {
        self.finalized_clone().completion_tokens
    }

    #[must_use]
    pub fn total_tokens(&self) -> Option<i32> {
        self.finalized_clone().total_tokens
    }

    #[must_use]
    pub fn has_token_usage(&self) -> bool {
        self.total_tokens().is_some()
            || self.prompt_tokens().is_some()
            || self.completion_tokens().is_some()
    }

    #[must_use]
    pub fn into_usage_json(mut self) -> serde_json::Value {
        self.finalize();
        json!({
            "aggregation": "sum",
            "call_count": self.call_count,
            "provider_kind": self.provider_kind,
            "model_name": self.model_name,
            "prompt_tokens": self.prompt_tokens,
            "completion_tokens": self.completion_tokens,
            "total_tokens": self.total_tokens,
        })
    }

    fn finalized_clone(&self) -> Self {
        let mut clone = self.clone();
        clone.finalize();
        clone
    }

    fn finalize(&mut self) {
        self.prompt_tokens = self
            .saw_prompt_tokens
            .then(|| i32::try_from(self.prompt_token_sum).unwrap_or(i32::MAX));
        self.completion_tokens = self
            .saw_completion_tokens
            .then(|| i32::try_from(self.completion_token_sum).unwrap_or(i32::MAX));
        let total_tokens = if self.saw_total_tokens {
            Some(i32::try_from(self.total_token_sum).unwrap_or(i32::MAX))
        } else if self.saw_prompt_tokens || self.saw_completion_tokens {
            Some(
                i32::try_from(self.prompt_token_sum.saturating_add(self.completion_token_sum))
                    .unwrap_or(i32::MAX),
            )
        } else {
            None
        };
        self.total_tokens = total_tokens;
    }
}

impl JobLeaseHeartbeat {
    #[must_use]
    pub fn new(
        job_id: Uuid,
        worker_id: impl Into<String>,
        runtime_ingestion_run_id: Option<Uuid>,
        lease_duration: chrono::Duration,
        min_interval: Duration,
    ) -> Self {
        Self {
            job_id,
            worker_id: worker_id.into(),
            runtime_ingestion_run_id,
            lease_duration,
            min_interval,
            last_renewed_at: Instant::now(),
        }
    }

    pub async fn maybe_renew(&mut self, state: &AppState) -> anyhow::Result<()> {
        if self.last_renewed_at.elapsed() >= self.min_interval {
            self.force_renew(state).await?;
        }
        Ok(())
    }

    pub async fn force_renew(&mut self, state: &AppState) -> anyhow::Result<()> {
        let renewed = repositories::renew_ingestion_job_lease(
            &state.persistence.postgres,
            self.job_id,
            &self.worker_id,
            self.lease_duration,
            i64::try_from(state.pipeline_hardening.heartbeat_write_min_interval_seconds)
                .unwrap_or(i64::MAX),
        )
        .await
        .with_context(|| format!("failed to renew ingestion job lease {}", self.job_id))?;
        match renewed {
            repositories::LeaseRenewalOutcome::Renewed => {
                self.last_renewed_at = Instant::now();
            }
            repositories::LeaseRenewalOutcome::Busy => {}
            repositories::LeaseRenewalOutcome::NotOwned => {
                bail!(
                    "worker {} no longer owns ingestion job {} lease",
                    self.worker_id,
                    self.job_id
                );
            }
        }
        Ok(())
    }

    #[must_use]
    pub fn spawn_keep_alive(&self, state: Arc<AppState>) -> JobLeaseKeepAlive {
        let job_id = self.job_id;
        let worker_id = self.worker_id.clone();
        let runtime_ingestion_run_id = self.runtime_ingestion_run_id;
        let lease_duration = self.lease_duration;
        let tick_interval = self.min_interval;
        let handle = tokio::spawn(async move {
            let mut ticker = time::interval(tick_interval);
            ticker.tick().await;
            loop {
                ticker.tick().await;
                match repositories::renew_ingestion_job_lease(
                    &state.persistence.postgres,
                    job_id,
                    &worker_id,
                    lease_duration,
                    i64::try_from(state.pipeline_hardening.heartbeat_write_min_interval_seconds)
                        .unwrap_or(i64::MAX),
                )
                .await
                {
                    Ok(repositories::LeaseRenewalOutcome::Renewed) => {}
                    Ok(repositories::LeaseRenewalOutcome::Busy) => {
                        continue;
                    }
                    Ok(repositories::LeaseRenewalOutcome::NotOwned) => {
                        warn!(
                            %worker_id,
                            job_id = %job_id,
                            "stopping background lease keep-alive because the worker no longer owns the job",
                        );
                        break;
                    }
                    Err(error) => {
                        warn!(
                            %worker_id,
                            job_id = %job_id,
                            ?error,
                            "background lease keep-alive failed",
                        );
                        continue;
                    }
                }
                if let Some(runtime_ingestion_run_id) = runtime_ingestion_run_id {
                    if let Err(error) =
                        repositories::update_runtime_ingestion_run_heartbeat_with_interval(
                            &state.persistence.postgres,
                            runtime_ingestion_run_id,
                            Utc::now(),
                            activity_status_label(RuntimeDocumentActivityStatus::Active),
                            i64::try_from(
                                state.pipeline_hardening.heartbeat_write_min_interval_seconds,
                            )
                            .unwrap_or(i64::MAX),
                        )
                        .await
                    {
                        warn!(
                            %worker_id,
                            job_id = %job_id,
                            runtime_ingestion_run_id = %runtime_ingestion_run_id,
                            ?error,
                            "background runtime heartbeat update failed",
                        );
                    }
                }
            }
        });
        JobLeaseKeepAlive { handle }
    }
}

impl Drop for JobLeaseKeepAlive {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

#[must_use]
pub fn provider_profile_from_snapshot_json(
    snapshot_json: &serde_json::Value,
) -> Option<EffectiveProviderProfile> {
    serde_json::from_value(snapshot_json.clone()).ok()
}

fn binding_purpose_label(binding_purpose: AiBindingPurpose) -> &'static str {
    match binding_purpose {
        AiBindingPurpose::ExtractText => "extract_text",
        AiBindingPurpose::ExtractGraph => "extract_graph",
        AiBindingPurpose::EmbedChunk => "embed_chunk",
        AiBindingPurpose::QueryRetrieve => "query_retrieve",
        AiBindingPurpose::QueryAnswer => "query_answer",
        AiBindingPurpose::Vision => "vision",
    }
}

async fn resolve_library_binding_selection(
    state: &AppState,
    library_id: Uuid,
    binding_purpose: AiBindingPurpose,
) -> anyhow::Result<ProviderModelSelection> {
    let binding_label = binding_purpose_label(binding_purpose);
    let binding = state
        .canonical_services
        .ai_catalog
        .resolve_active_runtime_binding(state, library_id, binding_purpose)
        .await
        .with_context(|| format!("failed to resolve active {binding_label} binding"))?
        .with_context(|| {
            format!("active {binding_label} binding is not configured for library {library_id}")
        })?;
    let provider_kind = binding.provider_kind.parse().map_err(|error: String| {
        anyhow::anyhow!("invalid provider kind for {binding_label}: {error}")
    })?;

    Ok(ProviderModelSelection { provider_kind, model_name: binding.model_name })
}

pub async fn resolve_effective_provider_profile(
    state: &AppState,
    library_id: Uuid,
) -> anyhow::Result<EffectiveProviderProfile> {
    Ok(EffectiveProviderProfile {
        indexing: resolve_library_binding_selection(
            state,
            library_id,
            AiBindingPurpose::ExtractGraph,
        )
        .await?,
        embedding: resolve_library_binding_selection(
            state,
            library_id,
            AiBindingPurpose::EmbedChunk,
        )
        .await?,
        answer: resolve_library_binding_selection(state, library_id, AiBindingPurpose::QueryAnswer)
            .await?,
        vision: resolve_library_binding_selection(state, library_id, AiBindingPurpose::Vision)
            .await?,
    })
}

pub fn resolve_runtime_run_provider_profile(
    runtime_run: &RuntimeIngestionRunRow,
) -> anyhow::Result<EffectiveProviderProfile> {
    provider_profile_from_snapshot_json(&runtime_run.provider_profile_snapshot_json).with_context(
        || {
            format!(
                "runtime ingestion run {} is missing canonical provider profile snapshot",
                runtime_run.id
            )
        },
    )
}

pub async fn persist_extracted_content_from_payload(
    state: &AppState,
    ingestion_run_id: Uuid,
    document_id: Option<Uuid>,
    payload: &IngestionExecutionPayload,
) -> anyhow::Result<RuntimeExtractedContentRow> {
    let persisted = persisted_extracted_content_from_payload(payload);
    repositories::upsert_runtime_extracted_content(
        &state.persistence.postgres,
        ingestion_run_id,
        document_id,
        &persisted.extraction_kind,
        persisted.content_text.as_deref(),
        persisted.page_count,
        persisted.char_count,
        persisted.extraction_warnings_json,
        persisted.source_map_json,
        persisted.provider_kind.as_deref(),
        persisted.model_name.as_deref(),
        persisted.extraction_version.as_deref(),
    )
    .await
    .context("failed to persist runtime extracted content from payload")
}

fn persisted_extracted_content_from_payload(
    payload: &IngestionExecutionPayload,
) -> PersistedExtractedContentInput {
    persisted_extracted_content(
        payload.file_kind.as_deref().and_then(UploadFileKind::from_str),
        payload
            .extraction_kind
            .as_deref()
            .unwrap_or_else(|| payload.file_kind.as_deref().unwrap_or("unknown")),
        payload.text.clone(),
        payload.page_count.and_then(|value| i32::try_from(value).ok()),
        payload.extraction_warnings.clone(),
        payload.source_map.clone(),
        payload.extraction_provider_kind.clone(),
        payload.extraction_model_name.clone(),
        payload.extraction_version.clone(),
    )
}

#[cfg(test)]
fn persisted_extracted_content_from_plan(
    plan: &crate::shared::file_extract::FileExtractionPlan,
) -> PersistedExtractedContentInput {
    persisted_extracted_content(
        Some(plan.file_kind),
        &plan.extraction_kind,
        plan.extracted_text.clone(),
        plan.page_count.and_then(|value| i32::try_from(value).ok()),
        plan.extraction_warnings.clone(),
        plan.source_map.clone(),
        plan.provider_kind.clone(),
        plan.model_name.clone(),
        plan.extraction_version.clone(),
    )
}

fn persisted_extracted_content(
    file_kind: Option<UploadFileKind>,
    extraction_kind: &str,
    content_text: Option<String>,
    page_count: Option<i32>,
    warnings: Vec<String>,
    source_map: serde_json::Value,
    provider_kind: Option<String>,
    model_name: Option<String>,
    extraction_version: Option<String>,
) -> PersistedExtractedContentInput {
    let source_map_json = normalize_persisted_extraction_source_map(
        file_kind,
        extraction_kind,
        warnings.len(),
        source_map,
    );
    let content_text = content_text.and_then(|value| (!value.trim().is_empty()).then_some(value));
    let char_count =
        content_text.as_ref().and_then(|value| i32::try_from(value.chars().count()).ok());

    PersistedExtractedContentInput {
        extraction_kind: extraction_kind.to_string(),
        content_text,
        page_count,
        char_count,
        extraction_warnings_json: serde_json::to_value(&warnings).unwrap_or_else(|_| json!([])),
        source_map_json,
        provider_kind,
        model_name,
        extraction_version,
    }
}

fn normalize_persisted_extraction_source_map(
    file_kind: Option<UploadFileKind>,
    extraction_kind: &str,
    warning_count: usize,
    source_map: serde_json::Value,
) -> serde_json::Value {
    let quality = extraction_quality_from_source_map(&source_map, extraction_kind, warning_count);
    let mut source_map = match source_map {
        serde_json::Value::Object(map) => map,
        _ => serde_json::Map::new(),
    };
    let ocr_source = quality
        .ocr_source
        .as_deref()
        .or_else(|| matches!(file_kind, Some(UploadFileKind::Image)).then_some("vision_llm"));
    source_map.insert(
        "content_quality".to_string(),
        json!({
            "normalization_status": quality.normalization_status.as_str(),
            "ocr_source": ocr_source,
            "warning_count": quality.warning_count,
        }),
    );
    serde_json::Value::Object(source_map)
}

pub async fn upsert_runtime_document_chunk_contribution_summary(
    state: &AppState,
    document_id: Uuid,
    revision_id: Option<Uuid>,
    runtime_ingestion_run_id: Uuid,
    attempt_no: i32,
    chunk_count: usize,
) -> anyhow::Result<()> {
    repositories::upsert_runtime_document_chunk_count(
        &state.persistence.postgres,
        document_id,
        revision_id,
        Some(runtime_ingestion_run_id),
        attempt_no,
        Some(i32::try_from(chunk_count).unwrap_or(i32::MAX)),
    )
    .await
    .context("failed to upsert runtime document chunk contribution summary")?;
    Ok(())
}

pub async fn upsert_runtime_document_graph_contribution_summary(
    state: &AppState,
    project_id: Uuid,
    document_id: Uuid,
    revision_id: Option<Uuid>,
    runtime_ingestion_run_id: Uuid,
    attempt_no: i32,
) -> anyhow::Result<()> {
    let graph_counts = match revision_id {
        Some(revision_id) => repositories::count_runtime_graph_contributions_by_document_revision(
            &state.persistence.postgres,
            project_id,
            document_id,
            revision_id,
        )
        .await
        .context("failed to count revision-scoped graph contributions")?,
        None => repositories::count_runtime_graph_contributions_by_document(
            &state.persistence.postgres,
            project_id,
            document_id,
        )
        .await
        .context("failed to count document graph contributions")?,
    };
    let filtered_artifact_count =
        repositories::count_runtime_graph_filtered_artifacts_by_ingestion_run(
            &state.persistence.postgres,
            project_id,
            runtime_ingestion_run_id,
            revision_id,
        )
        .await
        .context("failed to count filtered graph artifacts for ingestion run")?;
    repositories::upsert_runtime_document_graph_contribution_counts(
        &state.persistence.postgres,
        document_id,
        revision_id,
        Some(runtime_ingestion_run_id),
        attempt_no,
        i32::try_from(graph_counts.node_count).unwrap_or(i32::MAX),
        i32::try_from(graph_counts.edge_count).unwrap_or(i32::MAX),
        i32::try_from(filtered_artifact_count).unwrap_or(i32::MAX),
        i32::try_from(filtered_artifact_count).unwrap_or(i32::MAX),
    )
    .await
    .context("failed to upsert runtime document graph contribution summary")?;
    Ok(())
}

fn activity_status_label(status: RuntimeDocumentActivityStatus) -> &'static str {
    match status {
        RuntimeDocumentActivityStatus::Queued => "queued",
        RuntimeDocumentActivityStatus::Active => "active",
        RuntimeDocumentActivityStatus::Blocked => "blocked",
        RuntimeDocumentActivityStatus::Retrying => "retrying",
        RuntimeDocumentActivityStatus::Stalled => "stalled",
        RuntimeDocumentActivityStatus::Ready => "ready",
        RuntimeDocumentActivityStatus::Failed => "failed",
    }
}

fn build_search_chunk_embedding_writes(
    chunks: &[repositories::ChunkRow],
    batch_response: &EmbeddingBatchResponse,
    model_catalog_id: Uuid,
) -> Vec<ChunkEmbeddingWrite> {
    chunks
        .iter()
        .zip(batch_response.embeddings.iter())
        .map(|(chunk, embedding)| ChunkEmbeddingWrite {
            chunk_id: chunk.id,
            model_catalog_id,
            embedding_vector: embedding.clone(),
            active: true,
        })
        .collect()
}

fn build_runtime_graph_node_vector_target_inputs(
    nodes: &[&RuntimeGraphNodeRow],
    batch_response: &EmbeddingBatchResponse,
) -> Vec<repositories::RuntimeVectorTargetUpsertInput> {
    nodes
        .iter()
        .zip(batch_response.embeddings.iter())
        .map(|(node, embedding)| repositories::RuntimeVectorTargetUpsertInput {
            project_id: node.project_id,
            target_kind: "entity".to_string(),
            target_id: node.id,
            provider_kind: batch_response.provider_kind.clone(),
            model_name: batch_response.model_name.clone(),
            dimensions: i32::try_from(embedding.len()).ok(),
            embedding_json: serde_json::to_value(embedding).unwrap_or_else(|_| json!([])),
        })
        .collect()
}

fn build_runtime_graph_edge_vector_target_inputs(
    edges: &[RuntimeGraphEdgeRow],
    batch_response: &EmbeddingBatchResponse,
) -> Vec<repositories::RuntimeVectorTargetUpsertInput> {
    edges
        .iter()
        .zip(batch_response.embeddings.iter())
        .map(|(edge, embedding)| repositories::RuntimeVectorTargetUpsertInput {
            project_id: edge.project_id,
            target_kind: "relation".to_string(),
            target_id: edge.id,
            provider_kind: batch_response.provider_kind.clone(),
            model_name: batch_response.model_name.clone(),
            dimensions: i32::try_from(embedding.len()).ok(),
            embedding_json: serde_json::to_value(embedding).unwrap_or_else(|_| json!([])),
        })
        .collect()
}

pub async fn embed_runtime_chunks(
    state: &AppState,
    provider_profile: &EffectiveProviderProfile,
    chunks: &[repositories::ChunkRow],
    mut lease_heartbeat: Option<&mut JobLeaseHeartbeat>,
) -> anyhow::Result<RuntimeStageUsageSummary> {
    let Some(first_chunk) = chunks.first() else {
        return Ok(RuntimeStageUsageSummary::with_model(
            provider_profile.embedding.provider_kind.as_str(),
            &provider_profile.embedding.model_name,
        ));
    };
    let embedding_binding = state
        .canonical_services
        .ai_catalog
        .resolve_active_runtime_binding(state, first_chunk.project_id, AiBindingPurpose::EmbedChunk)
        .await?
        .ok_or_else(|| {
            anyhow::anyhow!(
                "active embedding binding is not configured for library {}",
                first_chunk.project_id
            )
        })?;
    let model_catalog_id = embedding_binding.model_catalog_id;
    let mut usage = RuntimeStageUsageSummary::with_model(
        &embedding_binding.provider_kind,
        &embedding_binding.model_name,
    );
    for chunk_batch in chunks.chunks(EMBEDDING_BATCH_SIZE) {
        if let Some(lease_heartbeat) = lease_heartbeat.as_deref_mut() {
            lease_heartbeat.maybe_renew(state).await?;
        }
        let batch_response = state
            .llm_gateway
            .embed_many(EmbeddingBatchRequest {
                provider_kind: embedding_binding.provider_kind.clone(),
                model_name: embedding_binding.model_name.clone(),
                inputs: chunk_batch.iter().map(|chunk| chunk.content.clone()).collect::<Vec<_>>(),
                api_key_override: Some(embedding_binding.api_key.clone()),
                base_url_override: embedding_binding.provider_base_url.clone(),
            })
            .await
            .with_context(|| {
                format!(
                    "failed to embed chunk batch starting with {}",
                    chunk_batch.first().map(|chunk| chunk.id).unwrap_or_default()
                )
            })?;

        if batch_response.embeddings.len() != chunk_batch.len() {
            bail!(
                "embedding batch returned {} vectors for {} chunks",
                batch_response.embeddings.len(),
                chunk_batch.len(),
            );
        }

        state
            .canonical_services
            .search
            .persist_chunk_embeddings(
                state,
                &build_search_chunk_embedding_writes(
                    chunk_batch,
                    &batch_response,
                    model_catalog_id,
                ),
            )
            .await
            .with_context(|| {
                format!(
                    "failed to persist canonical chunk embeddings for batch starting with {}",
                    chunk_batch.first().map(|chunk| chunk.id).unwrap_or_default()
                )
            })?;

        usage.absorb_usage_json(&batch_response.usage_json);
    }

    Ok(usage)
}
pub async fn embed_runtime_graph_nodes(
    state: &AppState,
    provider_profile: &EffectiveProviderProfile,
    nodes: &[RuntimeGraphNodeRow],
    mut lease_heartbeat: Option<&mut JobLeaseHeartbeat>,
) -> anyhow::Result<RuntimeStageUsageSummary> {
    let nodes_to_embed =
        nodes.iter().filter(|node| node.node_type != "document").collect::<Vec<_>>();
    let Some(first_node) = nodes_to_embed.first() else {
        return Ok(RuntimeStageUsageSummary::with_model(
            provider_profile.embedding.provider_kind.as_str(),
            &provider_profile.embedding.model_name,
        ));
    };
    let embedding_binding = state
        .canonical_services
        .ai_catalog
        .resolve_active_runtime_binding(state, first_node.project_id, AiBindingPurpose::EmbedChunk)
        .await?
        .ok_or_else(|| {
            anyhow::anyhow!(
                "active embedding binding is not configured for library {}",
                first_node.project_id
            )
        })?;
    let mut usage = RuntimeStageUsageSummary::with_model(
        &embedding_binding.provider_kind,
        &embedding_binding.model_name,
    );
    for node_batch in nodes_to_embed.chunks(EMBEDDING_BATCH_SIZE) {
        if let Some(lease_heartbeat) = lease_heartbeat.as_deref_mut() {
            lease_heartbeat.maybe_renew(state).await?;
        }
        let batch_response = state
            .llm_gateway
            .embed_many(EmbeddingBatchRequest {
                provider_kind: embedding_binding.provider_kind.clone(),
                model_name: embedding_binding.model_name.clone(),
                inputs: node_batch
                    .iter()
                    .map(|node| build_graph_node_embedding_input(node))
                    .collect::<Vec<_>>(),
                api_key_override: Some(embedding_binding.api_key.clone()),
                base_url_override: embedding_binding.provider_base_url.clone(),
            })
            .await
            .with_context(|| {
                format!(
                    "failed to embed graph node batch starting with {}",
                    node_batch.first().map(|node| node.id).unwrap_or_default()
                )
            })?;

        if batch_response.embeddings.len() != node_batch.len() {
            bail!(
                "embedding batch returned {} vectors for {} graph nodes",
                batch_response.embeddings.len(),
                node_batch.len(),
            );
        }

        repositories::upsert_runtime_vector_targets(
            &state.persistence.postgres,
            &build_runtime_graph_node_vector_target_inputs(node_batch, &batch_response),
        )
        .await
        .with_context(|| {
            format!(
                "failed to persist graph node embedding batch starting with {}",
                node_batch.first().map(|node| node.id).unwrap_or_default()
            )
        })?;
        usage.absorb_usage_json(&batch_response.usage_json);
    }

    Ok(usage)
}

pub async fn embed_runtime_graph_edges(
    state: &AppState,
    provider_profile: &EffectiveProviderProfile,
    nodes: &[RuntimeGraphNodeRow],
    edges: &[RuntimeGraphEdgeRow],
    mut lease_heartbeat: Option<&mut JobLeaseHeartbeat>,
) -> anyhow::Result<RuntimeStageUsageSummary> {
    let node_index = nodes.iter().map(|node| (node.id, node)).collect::<HashMap<_, _>>();
    let Some(first_edge) = edges.first() else {
        return Ok(RuntimeStageUsageSummary::with_model(
            provider_profile.embedding.provider_kind.as_str(),
            &provider_profile.embedding.model_name,
        ));
    };
    let embedding_binding = state
        .canonical_services
        .ai_catalog
        .resolve_active_runtime_binding(state, first_edge.project_id, AiBindingPurpose::EmbedChunk)
        .await?
        .ok_or_else(|| {
            anyhow::anyhow!(
                "active embedding binding is not configured for library {}",
                first_edge.project_id
            )
        })?;
    let mut usage = RuntimeStageUsageSummary::with_model(
        &embedding_binding.provider_kind,
        &embedding_binding.model_name,
    );
    for edge_batch in edges.chunks(EMBEDDING_BATCH_SIZE) {
        if let Some(lease_heartbeat) = lease_heartbeat.as_deref_mut() {
            lease_heartbeat.maybe_renew(state).await?;
        }
        let batch_response = state
            .llm_gateway
            .embed_many(EmbeddingBatchRequest {
                provider_kind: embedding_binding.provider_kind.clone(),
                model_name: embedding_binding.model_name.clone(),
                inputs: edge_batch
                    .iter()
                    .map(|edge| build_graph_edge_embedding_input(edge, &node_index))
                    .collect::<Vec<_>>(),
                api_key_override: Some(embedding_binding.api_key.clone()),
                base_url_override: embedding_binding.provider_base_url.clone(),
            })
            .await
            .with_context(|| {
                format!(
                    "failed to embed graph edge batch starting with {}",
                    edge_batch.first().map(|edge| edge.id).unwrap_or_default()
                )
            })?;

        if batch_response.embeddings.len() != edge_batch.len() {
            bail!(
                "embedding batch returned {} vectors for {} graph edges",
                batch_response.embeddings.len(),
                edge_batch.len(),
            );
        }

        repositories::upsert_runtime_vector_targets(
            &state.persistence.postgres,
            &build_runtime_graph_edge_vector_target_inputs(edge_batch, &batch_response),
        )
        .await
        .with_context(|| {
            format!(
                "failed to persist graph edge embedding batch starting with {}",
                edge_batch.first().map(|edge| edge.id).unwrap_or_default()
            )
        })?;
        usage.absorb_usage_json(&batch_response.usage_json);
    }

    Ok(usage)
}

fn build_graph_node_embedding_input(node: &RuntimeGraphNodeRow) -> String {
    let aliases =
        serde_json::from_value::<Vec<String>>(node.aliases_json.clone()).unwrap_or_default();
    let alias_text = aliases
        .into_iter()
        .filter(|alias| alias.trim() != node.label.trim())
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "node_type: {}\nlabel: {}\naliases: {}\nsummary: {}\nmetadata: {}",
        node.node_type,
        node.label,
        alias_text,
        node.summary.clone().unwrap_or_default(),
        node.metadata_json,
    )
}

fn build_graph_edge_embedding_input(
    edge: &RuntimeGraphEdgeRow,
    node_index: &HashMap<Uuid, &RuntimeGraphNodeRow>,
) -> String {
    let from_label =
        node_index.get(&edge.from_node_id).map_or("unknown", |node| node.label.as_str());
    let to_label = node_index.get(&edge.to_node_id).map_or("unknown", |node| node.label.as_str());
    format!(
        "relation_type: {}\nsource: {}\ntarget: {}\nsummary: {}\nmetadata: {}",
        edge.relation_type,
        from_label,
        to_label,
        edge.summary.clone().unwrap_or_default(),
        edge.metadata_json,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domains::provider_profiles::SupportedProviderKind;
    use crate::shared::file_extract::FileExtractionPlan;

    #[test]
    fn restores_effective_profile_from_snapshot_json() {
        let profile = EffectiveProviderProfile {
            indexing: ProviderModelSelection {
                provider_kind: SupportedProviderKind::OpenAi,
                model_name: "gpt-5-mini".to_string(),
            },
            embedding: ProviderModelSelection {
                provider_kind: SupportedProviderKind::DeepSeek,
                model_name: "embedding-1".to_string(),
            },
            answer: ProviderModelSelection {
                provider_kind: SupportedProviderKind::OpenAi,
                model_name: "gpt-5.4".to_string(),
            },
            vision: ProviderModelSelection {
                provider_kind: SupportedProviderKind::OpenAi,
                model_name: "gpt-5-mini".to_string(),
            },
        };

        let snapshot_json = serde_json::to_value(&profile).expect("serialize snapshot profile");
        let restored =
            provider_profile_from_snapshot_json(&snapshot_json).expect("restore snapshot profile");

        assert_eq!(restored, profile);
    }

    #[test]
    fn stage_usage_summary_exposes_finalized_tokens_without_consuming() {
        let mut usage = RuntimeStageUsageSummary::with_model("openai", "text-embedding-3-small");
        usage.absorb_usage_json(&json!({
            "prompt_tokens": 120,
        }));
        usage.absorb_usage_json(&json!({
            "prompt_tokens": 30,
            "completion_tokens": 5,
        }));

        assert_eq!(usage.prompt_tokens(), Some(150));
        assert_eq!(usage.completion_tokens(), Some(5));
        assert_eq!(usage.total_tokens(), Some(155));
        assert!(usage.has_token_usage());
    }

    #[test]
    fn persisted_plan_keeps_normalized_text_separate_from_warnings() {
        let plan = FileExtractionPlan {
            file_kind: UploadFileKind::Image,
            adapter_status: "ready".to_string(),
            extracted_text: Some("Acme Corp\nBudget 2026".to_string()),
            extraction_error: None,
            extraction_kind: "vision_image".to_string(),
            page_count: Some(1),
            extraction_warnings: vec!["Low contrast OCR".to_string()],
            source_map: json!({
                "mime_type": "image/png",
                "content_quality": {
                    "normalization_status": "normalized",
                    "ocr_source": "vision_llm",
                    "warning_count": 1,
                },
            }),
            provider_kind: Some("openai".to_string()),
            model_name: Some("gpt-5-mini".to_string()),
            extraction_version: Some("runtime_extraction_v1".to_string()),
            ingest_mode: "runtime_upload".to_string(),
        };

        let persisted = persisted_extracted_content_from_plan(&plan);

        assert_eq!(persisted.content_text.as_deref(), Some("Acme Corp\nBudget 2026"));
        assert_eq!(persisted.extraction_warnings_json, json!(["Low contrast OCR"]));
        assert_eq!(
            persisted.source_map_json["content_quality"]["normalization_status"],
            json!("normalized")
        );
        assert_eq!(persisted.source_map_json["content_quality"]["warning_count"], json!(1));
    }

    #[test]
    fn chunk_embedding_writes_preserve_all_dimensions() {
        let model_catalog_id = Uuid::now_v7();
        let project_id = Uuid::now_v7();
        let document_id = Uuid::now_v7();
        let chunks = vec![
            repositories::ChunkRow {
                id: Uuid::now_v7(),
                document_id,
                project_id,
                ordinal: 0,
                content: "alpha".to_string(),
                token_count: Some(1),
                metadata_json: json!({}),
                created_at: Utc::now(),
            },
            repositories::ChunkRow {
                id: Uuid::now_v7(),
                document_id,
                project_id,
                ordinal: 1,
                content: "beta".to_string(),
                token_count: Some(1),
                metadata_json: json!({}),
                created_at: Utc::now(),
            },
        ];
        let batch_response = EmbeddingBatchResponse {
            provider_kind: "openai".to_string(),
            model_name: "text-embedding-3-small".to_string(),
            dimensions: 3,
            embeddings: vec![vec![0.0; 1536], vec![0.1, 0.2, 0.3]],
            usage_json: json!({}),
        };

        let writes =
            build_search_chunk_embedding_writes(&chunks, &batch_response, model_catalog_id);

        assert_eq!(writes.len(), 2);
        assert_eq!(writes[0].chunk_id, chunks[0].id);
        assert_eq!(writes[0].model_catalog_id, model_catalog_id);
        assert_eq!(writes[0].embedding_vector.len(), 1536);
        assert_eq!(writes[1].embedding_vector, vec![0.1, 0.2, 0.3]);
    }

    #[test]
    fn graph_target_batches_keep_target_identity() {
        let project_id = Uuid::now_v7();
        let nodes = vec![RuntimeGraphNodeRow {
            id: Uuid::now_v7(),
            project_id,
            canonical_key: "entity::acme-corp".to_string(),
            label: "Acme Corp".to_string(),
            node_type: "entity".to_string(),
            aliases_json: json!([]),
            summary: Some("Budget owner".to_string()),
            metadata_json: json!({}),
            support_count: 1,
            projection_version: 1,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }];
        let batch_response = EmbeddingBatchResponse {
            provider_kind: "openai".to_string(),
            model_name: "text-embedding-3-small".to_string(),
            dimensions: 1536,
            embeddings: vec![vec![0.2; 1536]],
            usage_json: json!({}),
        };

        let node_refs = nodes.iter().collect::<Vec<_>>();
        let target_rows =
            build_runtime_graph_node_vector_target_inputs(node_refs.as_slice(), &batch_response);

        assert_eq!(target_rows.len(), 1);
        assert_eq!(target_rows[0].target_kind, "entity");
        assert_eq!(target_rows[0].target_id, nodes[0].id);
        assert_eq!(target_rows[0].dimensions, Some(1536));
    }
}
