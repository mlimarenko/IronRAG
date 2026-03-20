use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context, bail};
use chrono::Utc;
use rust_decimal::Decimal;
use serde_json::json;
use sha2::{Digest, Sha256};
use tokio::{task::JoinHandle, time};
use tracing::warn;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::{
        provider_profiles::{EffectiveProviderProfile, ProviderModelSelection},
        runtime_ingestion::{
            RuntimeAccountingTruthStatus, RuntimeCollectionGraphThroughputSummary,
            RuntimeCollectionResidualReason, RuntimeDocumentActivityStatus,
            RuntimeDocumentGraphThroughputSummary, RuntimeGraphProgressCadence,
            RuntimeIngestionStage, RuntimeIngestionStatus,
        },
    },
    infra::repositories::{
        self, IngestionExecutionPayload, IngestionJobRow, RuntimeExtractedContentRow,
        RuntimeGraphEdgeRow, RuntimeGraphExtractionResumeRollupRow, RuntimeGraphNodeRow,
        RuntimeGraphProgressCheckpointRow, RuntimeIngestionRunRow, RuntimeProviderProfileRow,
    },
    infra::vector_search,
    integrations::llm::{EmbeddingBatchRequest, EmbeddingBatchResponse},
    services::{
        document_accounting,
        document_reconciliation::{
            DeleteDocumentRequest, delete_document_and_reconcile,
            refresh_graph_summaries_after_reconciliation,
        },
        graph_projection::mark_graph_snapshot_stale,
        graph_rebuild::rebuild_library_graph,
        graph_summary::GraphSummaryRefreshRequest,
        ingest_activity::IngestActivityService,
    },
    shared::file_extract::{
        FileExtractionPlan, UploadAdmissionError, UploadFileKind,
        build_runtime_file_extraction_plan, extraction_quality_from_source_map,
    },
};

const EMBEDDING_BATCH_SIZE: usize = 16;

#[derive(Debug, Clone)]
pub struct RuntimeUploadFileInput {
    pub source_id: Option<Uuid>,
    pub file_name: String,
    pub mime_type: Option<String>,
    pub file_bytes: Vec<u8>,
    pub title: Option<String>,
}

#[derive(Debug, Clone)]
pub struct QueueRuntimeUploadRequest {
    pub project_id: Uuid,
    pub upload_batch_id: Option<Uuid>,
    pub requested_by: Option<String>,
    pub trigger_kind: String,
    pub parent_job_id: Option<Uuid>,
    pub idempotency_key: Option<String>,
    pub file: RuntimeUploadFileInput,
}

#[derive(Debug, Clone)]
pub struct RuntimeQueuedUpload {
    pub runtime_run: RuntimeIngestionRunRow,
    pub ingestion_job: IngestionJobRow,
    pub extracted_content: RuntimeExtractedContentRow,
}

#[derive(Debug, Clone)]
pub struct RuntimeIngestionContext {
    pub library_id: Uuid,
    pub provider_profile: EffectiveProviderProfile,
    pub current_stage: RuntimeIngestionStage,
}

#[derive(Debug, Clone)]
pub struct RuntimeDocumentActivityView {
    pub activity_status: String,
    pub last_activity_at: Option<chrono::DateTime<Utc>>,
    pub stalled_reason: Option<String>,
}

#[derive(Debug, Clone)]
struct InitialDocumentSnapshot {
    document: repositories::DocumentRow,
    revision: repositories::DocumentRevisionRow,
}

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

impl RuntimeIngestionContext {
    #[must_use]
    pub fn new(library_id: Uuid, provider_profile: EffectiveProviderProfile) -> Self {
        Self { library_id, provider_profile, current_stage: RuntimeIngestionStage::Accepted }
    }
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

    pub fn merge(&mut self, other: &Self) {
        self.call_count += other.call_count;
        self.prompt_token_sum += other.prompt_token_sum;
        self.completion_token_sum += other.completion_token_sum;
        self.total_token_sum += other.total_token_sum;
        self.saw_prompt_tokens |= other.saw_prompt_tokens;
        self.saw_completion_tokens |= other.saw_completion_tokens;
        self.saw_total_tokens |= other.saw_total_tokens;
        if self.provider_kind.is_none() {
            self.provider_kind = other.provider_kind.clone();
        }
        if self.model_name.is_none() {
            self.model_name = other.model_name.clone();
        }
        self.finalize();
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
pub fn provider_profile_snapshot_json(profile: &EffectiveProviderProfile) -> serde_json::Value {
    serde_json::to_value(profile).unwrap_or_else(|_| json!({}))
}

#[must_use]
pub fn provider_profile_from_snapshot_json(
    snapshot_json: &serde_json::Value,
) -> Option<EffectiveProviderProfile> {
    serde_json::from_value(snapshot_json.clone()).ok()
}

fn sha256_hex(value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    format!("{:x}", hasher.finalize())
}

#[must_use]
pub fn map_runtime_provider_profile_row(
    row: &RuntimeProviderProfileRow,
) -> EffectiveProviderProfile {
    EffectiveProviderProfile {
        indexing: ProviderModelSelection {
            provider_kind: row.indexing_provider_kind.parse().unwrap_or_default(),
            model_name: row.indexing_model_name.clone(),
        },
        embedding: ProviderModelSelection {
            provider_kind: row.embedding_provider_kind.parse().unwrap_or_default(),
            model_name: row.embedding_model_name.clone(),
        },
        answer: ProviderModelSelection {
            provider_kind: row.answer_provider_kind.parse().unwrap_or_default(),
            model_name: row.answer_model_name.clone(),
        },
        vision: ProviderModelSelection {
            provider_kind: row.vision_provider_kind.parse().unwrap_or_default(),
            model_name: row.vision_model_name.clone(),
        },
    }
}

async fn ensure_runtime_provider_profile(
    state: &AppState,
    library_id: Uuid,
) -> anyhow::Result<RuntimeProviderProfileRow> {
    if let Some(existing) =
        repositories::get_runtime_provider_profile(&state.persistence.postgres, library_id)
            .await
            .context("failed to query runtime provider profile")?
    {
        return Ok(existing);
    }

    let defaults = &state.runtime_provider_defaults;
    repositories::upsert_runtime_provider_profile(
        &state.persistence.postgres,
        library_id,
        defaults.indexing.provider_kind.as_str(),
        &defaults.indexing.model_name,
        defaults.embedding.provider_kind.as_str(),
        &defaults.embedding.model_name,
        defaults.answer.provider_kind.as_str(),
        &defaults.answer.model_name,
        defaults.vision.provider_kind.as_str(),
        &defaults.vision.model_name,
    )
    .await
    .context("failed to upsert default runtime provider profile")
}

pub async fn resolve_effective_provider_profile(
    state: &AppState,
    library_id: Uuid,
) -> anyhow::Result<EffectiveProviderProfile> {
    let row = ensure_runtime_provider_profile(state, library_id).await?;
    Ok(map_runtime_provider_profile_row(&row))
}

#[must_use]
pub fn resolve_runtime_run_provider_profile(
    state: &AppState,
    runtime_run: &RuntimeIngestionRunRow,
) -> EffectiveProviderProfile {
    provider_profile_from_snapshot_json(&runtime_run.provider_profile_snapshot_json)
        .unwrap_or_else(|| state.effective_provider_profile())
}

#[must_use]
pub fn build_runtime_ingestion_context(
    state: &AppState,
    library_id: Uuid,
) -> RuntimeIngestionContext {
    RuntimeIngestionContext::new(library_id, state.effective_provider_profile())
}

#[must_use]
pub fn classify_runtime_document_activity_with_service(
    ingest_activity: &IngestActivityService,
    runtime_run: &RuntimeIngestionRunRow,
) -> RuntimeDocumentActivityView {
    let now = Utc::now();
    let run_status = parse_runtime_ingestion_status(&runtime_run.status);
    let derived_status = ingest_activity.derive_status(
        run_status.clone(),
        runtime_run.started_at,
        runtime_run.last_activity_at,
        runtime_run.latest_error_message.as_deref(),
        now,
    );
    RuntimeDocumentActivityView {
        activity_status: activity_status_label(derived_status).to_string(),
        last_activity_at: runtime_run.last_activity_at,
        stalled_reason: ingest_activity.stalled_reason(
            run_status,
            runtime_run.started_at,
            runtime_run.last_activity_at,
            runtime_run.latest_error_message.as_deref(),
            now,
        ),
    }
}

#[must_use]
pub fn classify_runtime_document_activity(
    state: &AppState,
    runtime_run: &RuntimeIngestionRunRow,
) -> RuntimeDocumentActivityView {
    classify_runtime_document_activity_with_service(
        &state.bulk_ingest_hardening_services.ingest_activity,
        runtime_run,
    )
}

pub async fn persist_extracted_content_from_plan(
    state: &AppState,
    ingestion_run_id: Uuid,
    document_id: Option<Uuid>,
    extraction_plan: &FileExtractionPlan,
) -> anyhow::Result<RuntimeExtractedContentRow> {
    let persisted = persisted_extracted_content_from_plan(extraction_plan);
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
    .context("failed to persist runtime extracted content")
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

fn persisted_extracted_content_from_plan(
    extraction_plan: &FileExtractionPlan,
) -> PersistedExtractedContentInput {
    persisted_extracted_content(
        Some(extraction_plan.file_kind),
        &extraction_plan.extraction_kind,
        extraction_plan.extracted_text.clone(),
        extraction_plan.page_count.and_then(|value| i32::try_from(value).ok()),
        extraction_plan.extraction_warnings.clone(),
        extraction_plan.source_map.clone(),
        extraction_plan.provider_kind.clone(),
        extraction_plan.model_name.clone(),
        extraction_plan.extraction_version.clone(),
    )
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

pub fn validate_runtime_extraction_plan(
    file_name: &str,
    mime_type: Option<&str>,
    file_size_bytes: u64,
    extraction_plan: &FileExtractionPlan,
) -> Result<(), UploadAdmissionError> {
    if extraction_plan.file_kind == UploadFileKind::TextLike
        && extraction_plan.extracted_text.as_deref().is_some_and(|text| text.trim().is_empty())
    {
        return Err(UploadAdmissionError::from_file_extract_error(
            file_name,
            mime_type,
            file_size_bytes,
            crate::shared::file_extract::FileExtractError::ExtractionFailed {
                file_kind: UploadFileKind::TextLike,
                message: format!("uploaded file {file_name} is empty"),
            },
        ));
    }

    Ok(())
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

fn build_runtime_payload_json(
    request: &QueueRuntimeUploadRequest,
    runtime_run: &RuntimeIngestionRunRow,
    extraction_plan: &FileExtractionPlan,
) -> serde_json::Value {
    let file_name = request.file.file_name.clone();
    let mime_type = request.file.mime_type.clone();
    let title = request.file.title.clone().unwrap_or_else(|| runtime_run.file_name.clone());
    let file_size_bytes = u64::try_from(request.file.file_bytes.len()).unwrap_or(u64::MAX);
    json!({
        "project_id": request.project_id,
        "runtime_ingestion_run_id": runtime_run.id,
        "upload_batch_id": request.upload_batch_id,
        "logical_document_id": runtime_run.document_id,
        "target_revision_id": runtime_run.revision_id,
        "document_mutation_workflow_id": null,
        "stale_guard_revision_no": null,
        "attempt_kind": runtime_run.attempt_kind,
        "provider_profile_snapshot": runtime_run.provider_profile_snapshot_json,
        "mutation_kind": null,
        "source_id": request.file.source_id,
        "external_key": file_name,
        "title": title,
        "mime_type": mime_type,
        "text": extraction_plan.extracted_text.clone(),
        "file_kind": extraction_plan.file_kind.as_str(),
        "file_size_bytes": file_size_bytes,
        "adapter_status": extraction_plan.adapter_status.clone(),
        "extraction_error": extraction_plan.extraction_error.clone(),
        "extraction_kind": extraction_plan.extraction_kind.clone(),
        "page_count": extraction_plan.page_count,
        "extraction_warnings": extraction_plan.extraction_warnings.clone(),
        "source_map": extraction_plan.source_map.clone(),
        "extraction_provider_kind": extraction_plan.provider_kind.clone(),
        "extraction_model_name": extraction_plan.model_name.clone(),
        "extraction_version": extraction_plan.extraction_version.clone(),
        "ingest_mode": extraction_plan.ingest_mode.clone(),
        "extra_metadata": {
            "file_name": runtime_run.file_name.clone(),
            "original_file_name": runtime_run.file_name.clone(),
            "file_kind": extraction_plan.file_kind.as_str(),
            "file_size_bytes": file_size_bytes,
        },
    })
}

async fn create_initial_document_snapshot(
    state: &AppState,
    request: &QueueRuntimeUploadRequest,
    extraction_plan: &FileExtractionPlan,
) -> anyhow::Result<InitialDocumentSnapshot> {
    let checksum = extraction_plan
        .extracted_text
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .map(sha256_hex);
    let file_size_bytes = i64::try_from(request.file.file_bytes.len()).ok();
    let document = repositories::create_document(
        &state.persistence.postgres,
        request.project_id,
        request.file.source_id,
        &request.file.file_name,
        request.file.title.as_deref(),
        request.file.mime_type.as_deref(),
        checksum.as_deref(),
    )
    .await
    .context("failed to create logical document snapshot for runtime upload")?;
    let revision = repositories::create_document_revision(
        &state.persistence.postgres,
        document.id,
        1,
        "initial_upload",
        None,
        &request.file.file_name,
        request.file.mime_type.as_deref(),
        file_size_bytes,
        None,
        checksum.as_deref(),
    )
    .await
    .context("failed to create initial pending document revision for runtime upload")?;
    repositories::update_document_current_revision(
        &state.persistence.postgres,
        document.id,
        None,
        "queued",
        None,
        None,
    )
    .await
    .context("failed to mark logical document as queued for initial upload")?;

    Ok(InitialDocumentSnapshot { document, revision })
}

fn build_requeued_payload_json(
    runtime_run: &RuntimeIngestionRunRow,
    extracted_content: &RuntimeExtractedContentRow,
    source_id: Option<Uuid>,
    document_mutation_workflow_id: Option<Uuid>,
    stale_guard_revision_no: Option<i32>,
    mutation_kind: Option<&str>,
) -> anyhow::Result<serde_json::Value> {
    let extraction_warnings: Vec<String> =
        serde_json::from_value(extracted_content.extraction_warnings_json.clone())
            .unwrap_or_default();
    let file_size_bytes = runtime_run.file_size_bytes.and_then(|value| u64::try_from(value).ok());
    let text = extracted_content.content_text.clone();
    if text.as_deref().is_none_or(|value| value.trim().is_empty()) {
        bail!("reprocess payload is missing extracted text");
    }

    Ok(json!({
        "project_id": runtime_run.project_id,
        "runtime_ingestion_run_id": runtime_run.id,
        "upload_batch_id": runtime_run.upload_batch_id,
        "logical_document_id": runtime_run.document_id,
        "target_revision_id": runtime_run.revision_id,
        "document_mutation_workflow_id": document_mutation_workflow_id,
        "stale_guard_revision_no": stale_guard_revision_no,
        "attempt_kind": runtime_run.attempt_kind,
        "provider_profile_snapshot": runtime_run.provider_profile_snapshot_json,
        "mutation_kind": mutation_kind,
        "source_id": source_id,
        "external_key": runtime_run.file_name.clone(),
        "title": runtime_run.file_name.clone(),
        "mime_type": runtime_run.mime_type.clone(),
        "text": text,
        "file_kind": runtime_run.file_type.clone(),
        "file_size_bytes": file_size_bytes,
        "adapter_status": "ready",
        "extraction_error": null,
        "extraction_kind": extracted_content.extraction_kind.clone(),
        "page_count": extracted_content.page_count.and_then(|value| u32::try_from(value).ok()),
        "extraction_warnings": extraction_warnings,
        "source_map": extracted_content.source_map_json.clone(),
        "extraction_provider_kind": extracted_content.provider_kind.clone(),
        "extraction_model_name": extracted_content.model_name.clone(),
        "extraction_version": extracted_content.extraction_version.clone(),
        "ingest_mode": "runtime_requeue",
        "extra_metadata": {
            "file_name": runtime_run.file_name.clone(),
            "original_file_name": runtime_run.file_name.clone(),
            "file_kind": runtime_run.file_type.clone(),
            "file_size_bytes": file_size_bytes,
        },
    }))
}

pub async fn queue_prepared_runtime_attempt(
    state: &AppState,
    runtime_run: &RuntimeIngestionRunRow,
    extracted_content: &RuntimeExtractedContentRow,
    source_id: Option<Uuid>,
    requested_by: Option<&str>,
    trigger_kind: &str,
    parent_job_id: Option<Uuid>,
    document_mutation_workflow_id: Option<Uuid>,
    stale_guard_revision_no: Option<i32>,
    mutation_kind: Option<&str>,
) -> anyhow::Result<IngestionJobRow> {
    let accepted_at = Utc::now();
    repositories::append_runtime_stage_event(
        &state.persistence.postgres,
        runtime_run.id,
        runtime_run.current_attempt_no,
        "accepted",
        "completed",
        Some("queued for processing"),
        json!({
            "operation": trigger_kind,
            "file_name": runtime_run.file_name,
            "target_revision_id": runtime_run.revision_id,
            "attempt_kind": runtime_run.attempt_kind,
            "provider_profile_snapshot": runtime_run.provider_profile_snapshot_json,
            "mutation_kind": mutation_kind,
            "started_at": accepted_at,
            "finished_at": accepted_at,
            "elapsed_ms": 0,
        }),
    )
    .await
    .context("failed to append runtime queued stage event")?;
    repositories::update_runtime_ingestion_run_queued_stage(
        &state.persistence.postgres,
        runtime_run.id,
        "accepted",
        None,
        activity_status_label(RuntimeDocumentActivityStatus::Queued),
        None,
    )
    .await
    .context("failed to stamp queued runtime run activity")?;
    let payload_json = build_requeued_payload_json(
        runtime_run,
        extracted_content,
        source_id,
        document_mutation_workflow_id,
        stale_guard_revision_no,
        mutation_kind,
    )?;
    let job = repositories::create_ingestion_job(
        &state.persistence.postgres,
        runtime_run.project_id,
        source_id,
        trigger_kind,
        requested_by,
        parent_job_id,
        None,
        Some(runtime_run.current_attempt_no),
        payload_json,
    )
    .await
    .context("failed to create runtime ingestion job")?;
    persist_library_queue_isolation_waiting_reason(state, runtime_run.project_id).await?;
    Ok(job)
}

pub async fn queue_new_runtime_upload(
    state: &AppState,
    request: QueueRuntimeUploadRequest,
) -> anyhow::Result<RuntimeQueuedUpload> {
    let file_size_bytes = u64::try_from(request.file.file_bytes.len()).unwrap_or(u64::MAX);
    let provider_profile = resolve_effective_provider_profile(state, request.project_id).await?;
    let extraction_plan = build_runtime_file_extraction_plan(
        state.llm_gateway.as_ref(),
        &provider_profile.vision,
        Some(&request.file.file_name),
        request.file.mime_type.as_deref(),
        request.file.file_bytes.clone(),
    )
    .await
    .map_err(|error| {
        anyhow::Error::new(UploadAdmissionError::from_file_extract_error(
            &request.file.file_name,
            request.file.mime_type.as_deref(),
            file_size_bytes,
            error,
        ))
    })?;
    validate_runtime_extraction_plan(
        &request.file.file_name,
        request.file.mime_type.as_deref(),
        file_size_bytes,
        &extraction_plan,
    )
    .map_err(anyhow::Error::new)?;
    let initial_document =
        create_initial_document_snapshot(state, &request, &extraction_plan).await?;

    let track_id = format!("run_{}", Uuid::now_v7().simple());
    let runtime_run = repositories::create_runtime_ingestion_run(
        &state.persistence.postgres,
        request.project_id,
        Some(initial_document.document.id),
        Some(initial_document.revision.id),
        request.upload_batch_id,
        &track_id,
        &request.file.file_name,
        extraction_plan.file_kind.as_str(),
        request.file.mime_type.as_deref(),
        i64::try_from(request.file.file_bytes.len()).ok(),
        match RuntimeIngestionStatus::Queued {
            RuntimeIngestionStatus::Queued => "queued",
            _ => unreachable!(),
        },
        match RuntimeIngestionStage::Accepted {
            RuntimeIngestionStage::Accepted => "accepted",
            _ => unreachable!(),
        },
        "initial_upload",
        provider_profile_snapshot_json(&provider_profile),
    )
    .await
    .context("failed to create runtime ingestion run")?;
    let extracted_content = persist_extracted_content_from_plan(
        state,
        runtime_run.id,
        Some(initial_document.document.id),
        &extraction_plan,
    )
    .await?;
    let accepted_at = Utc::now();
    repositories::append_runtime_stage_event(
        &state.persistence.postgres,
        runtime_run.id,
        runtime_run.current_attempt_no,
        "accepted",
        "completed",
        Some("queued for processing"),
        json!({
            "file_kind": extraction_plan.file_kind.as_str(),
            "file_name": runtime_run.file_name,
            "target_revision_id": runtime_run.revision_id,
            "attempt_kind": runtime_run.attempt_kind,
            "provider_profile_snapshot": runtime_run.provider_profile_snapshot_json,
            "started_at": accepted_at,
            "finished_at": accepted_at,
            "elapsed_ms": 0,
        }),
    )
    .await
    .context("failed to append accepted runtime stage event")?;
    repositories::update_runtime_ingestion_run_queued_stage(
        &state.persistence.postgres,
        runtime_run.id,
        "accepted",
        None,
        activity_status_label(RuntimeDocumentActivityStatus::Queued),
        None,
    )
    .await
    .context("failed to stamp accepted runtime run activity")?;
    let ingestion_job = repositories::create_ingestion_job(
        &state.persistence.postgres,
        request.project_id,
        request.file.source_id,
        &request.trigger_kind,
        request.requested_by.as_deref(),
        request.parent_job_id,
        request.idempotency_key.as_deref(),
        None,
        build_runtime_payload_json(&request, &runtime_run, &extraction_plan),
    )
    .await
    .context("failed to create runtime ingestion job")?;
    persist_library_queue_isolation_waiting_reason(state, request.project_id).await?;

    Ok(RuntimeQueuedUpload { runtime_run, ingestion_job, extracted_content })
}

pub async fn requeue_runtime_run(
    state: &AppState,
    runtime_run: &RuntimeIngestionRunRow,
    requested_by: Option<&str>,
    trigger_kind: &str,
    parent_job_id: Option<Uuid>,
) -> anyhow::Result<(RuntimeIngestionRunRow, IngestionJobRow)> {
    let provider_profile =
        resolve_effective_provider_profile(state, runtime_run.project_id).await?;
    let extracted_content = repositories::get_runtime_extracted_content_by_run(
        &state.persistence.postgres,
        runtime_run.id,
    )
    .await
    .context("failed to load runtime extracted content")?
    .with_context(|| {
        format!("runtime ingestion run {} has no extracted content", runtime_run.id)
    })?;
    let source_id = match runtime_run.document_id {
        Some(document_id) => {
            let document =
                repositories::get_document_by_id(&state.persistence.postgres, document_id)
                    .await
                    .context("failed to load runtime document for requeue")?;
            if document.as_ref().is_some_and(|row| row.deleted_at.is_some()) {
                bail!("runtime document has been deleted and cannot be requeued");
            }
            document.and_then(|row| row.source_id)
        }
        None => None,
    };
    let runtime_run = repositories::requeue_runtime_ingestion_run(
        &state.persistence.postgres,
        runtime_run.id,
        provider_profile_snapshot_json(&provider_profile),
    )
    .await
    .context("failed to reset runtime ingestion run for requeue")?;
    let ingestion_job = queue_prepared_runtime_attempt(
        state,
        &runtime_run,
        &extracted_content,
        source_id,
        requested_by,
        trigger_kind,
        parent_job_id,
        None,
        None,
        None,
    )
    .await?;
    persist_library_queue_isolation_waiting_reason(state, runtime_run.project_id).await?;

    Ok((runtime_run, ingestion_job))
}

pub async fn delete_runtime_run_and_rebuild(
    state: &AppState,
    runtime_run: &RuntimeIngestionRunRow,
    requested_by: Option<&str>,
) -> anyhow::Result<Option<repositories::DocumentMutationWorkflowRow>> {
    if runtime_run.document_id.is_none() {
        let project_id = runtime_run.project_id;
        repositories::delete_ingestion_jobs_by_runtime_ingestion_run_id(
            &state.persistence.postgres,
            runtime_run.id,
        )
        .await
        .context("failed to delete queued ingestion jobs for runtime run")?;
        repositories::delete_runtime_ingestion_run_by_id(
            &state.persistence.postgres,
            runtime_run.id,
        )
        .await
        .context("failed to delete runtime ingestion run without logical document")?;
        refresh_library_queue_isolation_snapshot(state, project_id).await?;
        return Ok(None);
    }

    let deleted = delete_document_and_reconcile(
        state,
        DeleteDocumentRequest {
            runtime_run: runtime_run.clone(),
            requested_by: requested_by.map(str::to_string),
        },
    )
    .await?;
    Ok(Some(deleted.mutation_workflow))
}

pub async fn reprocess_runtime_run_and_rebuild(
    state: &AppState,
    runtime_run: &RuntimeIngestionRunRow,
    requested_by: Option<&str>,
    trigger_kind: &str,
) -> anyhow::Result<(RuntimeIngestionRunRow, IngestionJobRow)> {
    let snapshot = repositories::get_runtime_graph_snapshot(
        &state.persistence.postgres,
        runtime_run.project_id,
    )
    .await
    .context("failed to load graph snapshot before reprocess")?;
    let previous_projection_version =
        snapshot.as_ref().map(|row| row.projection_version).filter(|value| *value > 0).unwrap_or(1);
    let previous_node_count = snapshot
        .as_ref()
        .map(|row| usize::try_from(row.node_count).unwrap_or_default())
        .unwrap_or_default();
    let previous_edge_count = snapshot
        .as_ref()
        .map(|row| usize::try_from(row.edge_count).unwrap_or_default())
        .unwrap_or_default();

    if let Some(document_id) = runtime_run.document_id {
        repositories::delete_runtime_query_references_by_document(
            &state.persistence.postgres,
            runtime_run.project_id,
            document_id,
        )
        .await
        .context("failed to clean persisted query references before reprocess")?;
        repositories::deactivate_runtime_graph_evidence_by_document(
            &state.persistence.postgres,
            runtime_run.project_id,
            document_id,
        )
        .await
        .context("failed to deactivate graph evidence before reprocess")?;
        repositories::recalculate_runtime_graph_support_counts(
            &state.persistence.postgres,
            runtime_run.project_id,
            previous_projection_version,
        )
        .await
        .context("failed to recalculate graph support counts before reprocess rebuild")?;
    }

    let (runtime_run, ingestion_job) =
        requeue_runtime_run(state, runtime_run, requested_by, trigger_kind, None).await?;

    let _ = mark_graph_snapshot_stale(
        state,
        runtime_run.project_id,
        previous_projection_version,
        previous_node_count,
        previous_edge_count,
        Some("Graph rebuild pending after document reprocess."),
    )
    .await;

    if let Some(document_id) = runtime_run.document_id {
        repositories::delete_document_by_id(&state.persistence.postgres, document_id)
            .await
            .context("failed to delete document before reprocess rebuild")?;
    }

    let rebuilt = rebuild_library_graph(state, runtime_run.project_id)
        .await
        .context("failed to rebuild graph after document reprocess")?;
    let _ = mark_graph_snapshot_stale(
        state,
        runtime_run.project_id,
        rebuilt.projection_version,
        rebuilt.node_count,
        rebuilt.edge_count,
        Some("Graph coverage is rebuilding while the reprocessed document runs again."),
    )
    .await;
    let _ = refresh_graph_summaries_after_reconciliation(
        state,
        runtime_run.project_id,
        GraphSummaryRefreshRequest::broad(),
    )
    .await
    .context("failed to invalidate graph summaries after reprocess rebuild")?;

    Ok((runtime_run, ingestion_job))
}

pub async fn refresh_library_queue_isolation_snapshot(
    state: &AppState,
    project_id: Uuid,
) -> anyhow::Result<()> {
    let snapshot = build_library_queue_isolation_snapshot_input(state, project_id).await?;
    repositories::upsert_runtime_library_queue_slice(&state.persistence.postgres, &snapshot)
        .await
        .context("failed to persist queue-isolation snapshot")?;
    refresh_library_collection_settlement_snapshots(state, project_id).await?;
    refresh_library_warning_snapshots(state, project_id).await
}

pub async fn persist_library_queue_isolation_waiting_reason(
    state: &AppState,
    project_id: Uuid,
) -> anyhow::Result<()> {
    let snapshot = build_library_queue_isolation_snapshot_input(state, project_id).await?;
    repositories::persist_runtime_library_queue_waiting_reason(
        &state.persistence.postgres,
        &snapshot,
    )
    .await
    .context("failed to persist queue-isolation waiting reason")?;
    refresh_library_collection_settlement_snapshots(state, project_id).await?;
    refresh_library_warning_snapshots(state, project_id).await
}

pub async fn release_library_queue_isolation_slot(
    state: &AppState,
    project_id: Uuid,
) -> anyhow::Result<()> {
    let snapshot = build_library_queue_isolation_snapshot_input(state, project_id).await?;
    repositories::release_runtime_library_queue_slot(&state.persistence.postgres, &snapshot)
        .await
        .context("failed to release queue-isolation slot")?;
    refresh_library_collection_settlement_snapshots(state, project_id).await?;
    refresh_library_warning_snapshots(state, project_id).await
}

#[derive(Debug, Clone)]
struct RuntimeCollectionAccountingSnapshot {
    live_total_estimated_cost: Option<Decimal>,
    settled_total_estimated_cost: Option<Decimal>,
    missing_total_estimated_cost: Option<Decimal>,
    currency: Option<String>,
    prompt_tokens: i64,
    completion_tokens: i64,
    total_tokens: i64,
    priced_stage_count: i32,
    unpriced_stage_count: i32,
    in_flight_stage_count: i32,
    missing_stage_count: i32,
    accounting_status: RuntimeAccountingTruthStatus,
    display_accounting_status: String,
}

#[derive(Debug, Clone, Default)]
struct RuntimeCollectionScopeAccountingSnapshot {
    document_count: i64,
    live_estimated_cost: Option<Decimal>,
    settled_estimated_cost: Option<Decimal>,
    missing_estimated_cost: Option<Decimal>,
    currency: Option<String>,
    prompt_tokens: i64,
    completion_tokens: i64,
    total_tokens: i64,
    accounting_status: String,
}

pub async fn refresh_library_warning_snapshots(
    state: &AppState,
    project_id: Uuid,
) -> anyhow::Result<()> {
    let queue_slice =
        repositories::load_runtime_library_queue_slice(&state.persistence.postgres, project_id)
            .await
            .context("failed to load queue-isolation slice for warning refresh")?;
    let settlement_row = repositories::load_runtime_collection_settlement_snapshot(
        &state.persistence.postgres,
        project_id,
    )
    .await
    .context("failed to load collection settlement snapshot for warning refresh")?;
    let format_rollups = repositories::list_runtime_collection_settlement_rollups(
        &state.persistence.postgres,
        project_id,
        "format",
    )
    .await
    .context("failed to load persisted collection format rollups for warning refresh")?;
    let settlement_row =
        settlement_row.context("collection settlement snapshot missing during warning refresh")?;
    let settlement = map_runtime_collection_settlement_summary(&settlement_row);
    let queue_isolation = state.pipeline_hardening_services.queue_isolation.summarize(
        usize::try_from(queue_slice.queued_count).unwrap_or(usize::MAX),
        usize::try_from(queue_slice.processing_count).unwrap_or(usize::MAX),
        usize::try_from(queue_slice.workspace_processing_count).unwrap_or(usize::MAX),
        usize::try_from(queue_slice.global_processing_count).unwrap_or(usize::MAX),
        queue_slice.last_claimed_at,
        queue_slice.last_progress_at,
        repositories::parse_runtime_queue_waiting_reason(queue_slice.waiting_reason.as_deref()),
    );
    let degraded_extraction_count = format_rollups
        .iter()
        .map(|row| usize::try_from(row.ready_no_graph_count).unwrap_or(usize::MAX))
        .sum();
    let warnings = state.pipeline_hardening_services.operator_warning.build_collection_warnings(
        Some(&queue_isolation),
        &settlement,
        usize::try_from(settlement_row.failed_count).unwrap_or(usize::MAX),
        settlement_row.missing_stage_count,
        0,
        degraded_extraction_count,
    );
    let warning_rows =
        repositories::build_runtime_collection_warning_rows(project_id, &warnings, Utc::now());
    repositories::replace_runtime_collection_warning_snapshots(
        &state.persistence.postgres,
        project_id,
        &warning_rows,
    )
    .await
    .context("failed to persist collection warning snapshots")
}

pub async fn refresh_library_collection_settlement_snapshots(
    state: &AppState,
    project_id: Uuid,
) -> anyhow::Result<()> {
    let progress_rollup = repositories::load_runtime_collection_progress_rollup(
        &state.persistence.postgres,
        project_id,
    )
    .await
    .context("failed to load collection progress rollup for settlement refresh")?;
    let stage_rollups = repositories::list_runtime_collection_stage_rollups(
        &state.persistence.postgres,
        project_id,
    )
    .await
    .context("failed to load collection stage rollups for settlement refresh")?;
    let format_rollups = repositories::list_runtime_collection_format_rollups(
        &state.persistence.postgres,
        project_id,
    )
    .await
    .context("failed to load collection format rollups for settlement refresh")?;
    let accounting_rows = repositories::list_runtime_collection_resolved_stage_accounting(
        &state.persistence.postgres,
        project_id,
    )
    .await
    .context("failed to load collection accounting rows for settlement refresh")?;
    let existing_snapshot = repositories::load_runtime_collection_settlement_snapshot(
        &state.persistence.postgres,
        project_id,
    )
    .await
    .context("failed to load previous collection settlement snapshot")?;
    let existing_terminal_outcome = repositories::load_runtime_collection_terminal_projection(
        &state.persistence.postgres,
        project_id,
    )
    .await
    .context("failed to load previous collection terminal outcome")?;
    let graph_health = repositories::load_runtime_graph_diagnostics_snapshot(
        &state.persistence.postgres,
        project_id,
    )
    .await
    .context("failed to load graph diagnostics snapshot for terminal settlement")?;
    let project = repositories::get_project_by_id(&state.persistence.postgres, project_id)
        .await
        .context("failed to load project for terminal settlement refresh")?
        .context("project missing during terminal settlement refresh")?;
    let failed_runs = repositories::list_runtime_ingestion_runs_by_project(
        &state.persistence.postgres,
        project_id,
    )
    .await
    .context("failed to load runtime runs for terminal settlement refresh")?
    .into_iter()
    .filter(|row| row.status == "failed")
    .collect::<Vec<_>>();

    let accounting = summarize_runtime_collection_accounting(&accounting_rows);
    let pending_graph_count = format_rollups
        .iter()
        .map(|row| usize::try_from(row.ready_no_graph_count).unwrap_or(usize::MAX))
        .sum();
    let residual_reason = derive_collection_residual_reason(
        graph_health.as_ref(),
        &failed_runs,
        accounting.missing_stage_count,
        existing_terminal_outcome.as_ref().and_then(|row| {
            repositories::parse_runtime_collection_residual_reason(row.residual_reason.as_deref())
        }),
    );
    let terminal_transition_at = terminal_transition_at(
        existing_terminal_outcome.as_ref(),
        usize::try_from(progress_rollup.queue_backlog_count).unwrap_or(usize::MAX),
        usize::try_from(progress_rollup.processing_backlog_count).unwrap_or(usize::MAX),
        pending_graph_count,
        usize::try_from(progress_rollup.failed_count).unwrap_or(usize::MAX),
        accounting.missing_stage_count,
        residual_reason.as_ref(),
    );
    let terminal_outcome = state.resolve_settle_blockers_services.terminal_settlement.summarize(
        usize::try_from(progress_rollup.queue_backlog_count).unwrap_or(usize::MAX),
        usize::try_from(progress_rollup.processing_backlog_count).unwrap_or(usize::MAX),
        pending_graph_count,
        usize::try_from(progress_rollup.failed_count).unwrap_or(usize::MAX),
        accounting.missing_stage_count,
        residual_reason,
        accounting.live_total_estimated_cost,
        accounting.settled_total_estimated_cost,
        accounting.missing_total_estimated_cost,
        accounting.currency.clone(),
        existing_terminal_outcome.as_ref().and_then(|row| row.settled_at),
        terminal_transition_at,
    );
    let settlement = state.pipeline_hardening_services.collection_settlement.summarize(
        &terminal_outcome,
        accounting.live_total_estimated_cost,
        accounting.settled_total_estimated_cost,
        accounting.missing_total_estimated_cost,
        accounting.currency.clone(),
        accounting.in_flight_stage_count,
        accounting.missing_stage_count,
        accounting.accounting_status.clone(),
        existing_snapshot.as_ref().and_then(|row| row.settled_at),
    );
    let computed_at = Utc::now();
    let snapshot_row = repositories::RuntimeCollectionSettlementRow {
        project_id,
        progress_state: repositories::runtime_collection_progress_state_key(
            &settlement.progress_state,
        )
        .to_string(),
        terminal_state: repositories::runtime_collection_terminal_state_key(
            &terminal_outcome.terminal_state,
        )
        .to_string(),
        terminal_transition_at: terminal_outcome.last_transition_at,
        residual_reason: terminal_outcome
            .residual_reason
            .as_ref()
            .map(repositories::runtime_collection_residual_reason_key)
            .map(str::to_string),
        document_count: progress_rollup.accepted_count.max(0),
        accepted_count: progress_rollup.accepted_count.max(0),
        content_extracted_count: progress_rollup.content_extracted_count.max(0),
        chunked_count: progress_rollup.chunked_count.max(0),
        embedded_count: progress_rollup.embedded_count.max(0),
        graph_active_count: progress_rollup.extracting_graph_count.max(0),
        graph_ready_count: progress_rollup.graph_ready_count.max(0),
        pending_graph_count: i64::try_from(pending_graph_count).unwrap_or(i64::MAX),
        ready_count: progress_rollup.ready_count.max(0),
        failed_count: progress_rollup.failed_count.max(0),
        queue_backlog_count: progress_rollup.queue_backlog_count.max(0),
        processing_backlog_count: progress_rollup.processing_backlog_count.max(0),
        live_total_estimated_cost: accounting.live_total_estimated_cost,
        settled_total_estimated_cost: accounting.settled_total_estimated_cost,
        missing_total_estimated_cost: accounting.missing_total_estimated_cost,
        currency: accounting.currency.clone(),
        prompt_tokens: accounting.prompt_tokens,
        completion_tokens: accounting.completion_tokens,
        total_tokens: accounting.total_tokens,
        priced_stage_count: accounting.priced_stage_count,
        unpriced_stage_count: accounting.unpriced_stage_count,
        in_flight_stage_count: accounting.in_flight_stage_count,
        missing_stage_count: accounting.missing_stage_count,
        accounting_status: accounting.display_accounting_status.clone(),
        is_fully_settled: settlement.is_fully_settled,
        settled_at: settlement.settled_at,
        computed_at,
    };
    let terminal_outcome_row = repositories::RuntimeCollectionTerminalOutcomeRow {
        project_id,
        workspace_id: project.workspace_id,
        terminal_state: repositories::runtime_collection_terminal_state_key(
            &terminal_outcome.terminal_state,
        )
        .to_string(),
        residual_reason: terminal_outcome
            .residual_reason
            .as_ref()
            .map(repositories::runtime_collection_residual_reason_key)
            .map(str::to_string),
        queued_count: i64::try_from(terminal_outcome.queued_count).unwrap_or(i64::MAX),
        processing_count: i64::try_from(terminal_outcome.processing_count).unwrap_or(i64::MAX),
        pending_graph_count: i64::try_from(terminal_outcome.pending_graph_count)
            .unwrap_or(i64::MAX),
        failed_document_count: i64::try_from(terminal_outcome.failed_document_count)
            .unwrap_or(i64::MAX),
        live_total_estimated_cost: terminal_outcome.live_total_estimated_cost,
        settled_total_estimated_cost: terminal_outcome.settled_total_estimated_cost,
        missing_total_estimated_cost: terminal_outcome.missing_total_estimated_cost,
        currency: terminal_outcome.currency.clone(),
        settled_at: terminal_outcome.settled_at,
        last_transition_at: terminal_outcome.last_transition_at,
    };
    repositories::upsert_runtime_collection_settlement_snapshot(
        &state.persistence.postgres,
        &snapshot_row,
    )
    .await
    .context("failed to persist collection settlement snapshot")?;
    repositories::upsert_runtime_collection_terminal_outcome(
        &state.persistence.postgres,
        &terminal_outcome_row,
    )
    .await
    .context("failed to persist collection terminal outcome")?;

    let stage_inputs = build_stage_settlement_rollup_inputs(&stage_rollups, &accounting_rows);
    repositories::replace_runtime_collection_settlement_rollups(
        &state.persistence.postgres,
        project_id,
        "stage",
        &stage_inputs,
    )
    .await
    .context("failed to persist stage settlement rollups")?;

    let format_inputs = build_format_settlement_rollup_inputs(&format_rollups, &accounting_rows);
    repositories::replace_runtime_collection_settlement_rollups(
        &state.persistence.postgres,
        project_id,
        "format",
        &format_inputs,
    )
    .await
    .context("failed to persist format settlement rollups")
}

fn summarize_runtime_collection_accounting(
    rows: &[repositories::RuntimeCollectionResolvedStageAccountingRow],
) -> RuntimeCollectionAccountingSnapshot {
    let settled_total_estimated_cost = sum_decimal_values(
        rows.iter()
            .filter(|row| row.accounting_scope == "stage_rollup")
            .filter_map(|row| row.estimated_cost),
    );
    let live_total_estimated_cost = sum_decimal_values(
        rows.iter()
            .filter(|row| row.accounting_scope == "provider_call")
            .filter_map(|row| row.estimated_cost),
    );
    let missing_stage_count =
        i32::try_from(rows.iter().filter(|row| row.accounting_scope == "missing").count())
            .unwrap_or(i32::MAX);
    let in_flight_stage_count =
        i32::try_from(rows.iter().filter(|row| row.accounting_scope == "provider_call").count())
            .unwrap_or(i32::MAX);
    let priced_stage_count = i32::try_from(
        rows.iter()
            .filter(|row| row.accounting_scope == "stage_rollup" && row.pricing_status == "priced")
            .count(),
    )
    .unwrap_or(i32::MAX);
    let unpriced_stage_count = i32::try_from(
        rows.iter()
            .filter(|row| row.accounting_scope == "stage_rollup" && row.pricing_status != "priced")
            .count(),
    )
    .unwrap_or(i32::MAX);
    let accounting_status = document_accounting::classify_accounting_truth_status(
        priced_stage_count,
        unpriced_stage_count,
        in_flight_stage_count,
        missing_stage_count,
    );
    let display_accounting_status =
        document_accounting::accounting_truth_status_key(&accounting_status).to_string();
    RuntimeCollectionAccountingSnapshot {
        live_total_estimated_cost,
        settled_total_estimated_cost,
        missing_total_estimated_cost: (missing_stage_count > 0).then_some(Decimal::ZERO),
        currency: rows.iter().find_map(|row| row.currency.clone()),
        prompt_tokens: rows.iter().map(|row| row.prompt_tokens).sum(),
        completion_tokens: rows.iter().map(|row| row.completion_tokens).sum(),
        total_tokens: rows.iter().map(|row| row.total_tokens).sum(),
        priced_stage_count,
        unpriced_stage_count,
        in_flight_stage_count,
        missing_stage_count,
        accounting_status,
        display_accounting_status,
    }
}

fn map_runtime_collection_settlement_summary(
    row: &repositories::RuntimeCollectionSettlementRow,
) -> crate::domains::runtime_ingestion::RuntimeCollectionSettlementSummary {
    crate::domains::runtime_ingestion::RuntimeCollectionSettlementSummary {
        progress_state: repositories::parse_runtime_collection_progress_state(Some(
            row.progress_state.as_str(),
        )),
        live_total_estimated_cost: row.live_total_estimated_cost,
        settled_total_estimated_cost: row.settled_total_estimated_cost,
        missing_total_estimated_cost: row.missing_total_estimated_cost,
        currency: row.currency.clone(),
        is_fully_settled: row.is_fully_settled,
        settled_at: row.settled_at,
    }
}

fn derive_collection_residual_reason(
    graph_health: Option<&repositories::RuntimeGraphDiagnosticsSnapshotRow>,
    failed_runs: &[repositories::RuntimeIngestionRunRow],
    missing_stage_count: i32,
    existing_reason: Option<RuntimeCollectionResidualReason>,
) -> Option<RuntimeCollectionResidualReason> {
    if let Some(graph_health) = graph_health {
        match graph_health.last_projection_failure_kind.as_deref() {
            Some("projection_contention") if graph_health.failed_projection_count > 0 => {
                return Some(RuntimeCollectionResidualReason::ProjectionContention);
            }
            Some("graph_persistence_integrity") if graph_health.failed_projection_count > 0 => {
                return Some(RuntimeCollectionResidualReason::GraphPersistenceIntegrity);
            }
            Some("diagnostics_unavailable") | _ if !graph_health.is_runtime_readable => {
                return Some(RuntimeCollectionResidualReason::DiagnosticsUnavailable);
            }
            _ => {}
        }
    }
    for run in failed_runs {
        if let Some(reason) =
            classify_failed_run_residual_reason(run.latest_error_message.as_deref())
        {
            return Some(reason);
        }
    }
    if !failed_runs.is_empty() {
        if let Some(reason) = existing_reason {
            return Some(reason);
        }
        return Some(RuntimeCollectionResidualReason::ProviderFailure);
    }
    if missing_stage_count > 0 {
        return Some(RuntimeCollectionResidualReason::SettlementRefreshFailed);
    }
    None
}

fn classify_failed_run_residual_reason(
    latest_error_message: Option<&str>,
) -> Option<RuntimeCollectionResidualReason> {
    let normalized = latest_error_message?.to_ascii_lowercase();
    if normalized.contains("upload_limit_exceeded")
        || normalized.contains("upload limit exceeded")
        || normalized.contains("exceeded the size limit")
    {
        return Some(RuntimeCollectionResidualReason::UploadLimitExceeded);
    }
    if normalized.contains("projection contention")
        || normalized.contains("deadlock")
        || normalized.contains("lock timeout")
    {
        return Some(RuntimeCollectionResidualReason::ProjectionContention);
    }
    if normalized.contains("graph persistence integrity")
        || normalized.contains("foreign key violation")
        || normalized.contains("runtime_graph_edge")
    {
        return Some(RuntimeCollectionResidualReason::GraphPersistenceIntegrity);
    }
    if normalized.contains("settlement refresh failed")
        || normalized.contains("failed to persist collection settlement")
        || normalized.contains("failed to persist collection terminal outcome")
    {
        return Some(RuntimeCollectionResidualReason::SettlementRefreshFailed);
    }
    if normalized.contains("provider failure")
        || normalized.contains("upstream timeout")
        || normalized.contains("upstream rejection")
        || normalized.contains("invalid model output")
        || normalized.contains("invalid_request")
        || normalized.contains("invalid request")
    {
        return Some(RuntimeCollectionResidualReason::ProviderFailure);
    }
    None
}

fn terminal_transition_at(
    existing: Option<&repositories::RuntimeCollectionTerminalOutcomeRow>,
    queued_count: usize,
    processing_count: usize,
    pending_graph_count: usize,
    failed_document_count: usize,
    missing_stage_count: i32,
    residual_reason: Option<&RuntimeCollectionResidualReason>,
) -> Option<chrono::DateTime<Utc>> {
    let terminal_state = if queued_count > 0 || processing_count > 0 || pending_graph_count > 0 {
        "live_in_flight"
    } else if residual_reason.is_some() || failed_document_count > 0 || missing_stage_count > 0 {
        "failed_with_residual_work"
    } else {
        "fully_settled"
    };
    let residual_reason = residual_reason
        .map(repositories::runtime_collection_residual_reason_key)
        .map(str::to_string);
    match existing {
        Some(existing)
            if existing.terminal_state == terminal_state
                && existing.residual_reason == residual_reason =>
        {
            Some(existing.last_transition_at)
        }
        _ => None,
    }
}

fn sum_decimal_values<I>(values: I) -> Option<Decimal>
where
    I: Iterator<Item = Decimal>,
{
    let mut saw_value = false;
    let total = values.fold(Decimal::ZERO, |acc, value| {
        saw_value = true;
        acc + value
    });
    saw_value.then_some(total)
}

fn summarize_runtime_collection_scope_accounting(
    rows: &[repositories::RuntimeCollectionResolvedStageAccountingRow],
) -> RuntimeCollectionScopeAccountingSnapshot {
    let document_count =
        i64::try_from(rows.iter().map(|row| row.ingestion_run_id).collect::<BTreeSet<_>>().len())
            .unwrap_or(i64::MAX);
    let priced_stage_count = i32::try_from(
        rows.iter()
            .filter(|row| row.accounting_scope == "stage_rollup" && row.pricing_status == "priced")
            .count(),
    )
    .unwrap_or(i32::MAX);
    let unpriced_stage_count = i32::try_from(
        rows.iter()
            .filter(|row| row.accounting_scope == "stage_rollup" && row.pricing_status != "priced")
            .count(),
    )
    .unwrap_or(i32::MAX);
    let in_flight_stage_count =
        i32::try_from(rows.iter().filter(|row| row.accounting_scope == "provider_call").count())
            .unwrap_or(i32::MAX);
    let missing_stage_count =
        i32::try_from(rows.iter().filter(|row| row.accounting_scope == "missing").count())
            .unwrap_or(i32::MAX);

    RuntimeCollectionScopeAccountingSnapshot {
        document_count,
        live_estimated_cost: sum_decimal_values(
            rows.iter()
                .filter(|row| row.accounting_scope == "provider_call")
                .filter_map(|row| row.estimated_cost),
        ),
        settled_estimated_cost: sum_decimal_values(
            rows.iter()
                .filter(|row| row.accounting_scope == "stage_rollup")
                .filter_map(|row| row.estimated_cost),
        ),
        missing_estimated_cost: (missing_stage_count > 0).then_some(Decimal::ZERO),
        currency: rows.iter().find_map(|row| row.currency.clone()),
        prompt_tokens: rows.iter().map(|row| row.prompt_tokens).sum(),
        completion_tokens: rows.iter().map(|row| row.completion_tokens).sum(),
        total_tokens: rows.iter().map(|row| row.total_tokens).sum(),
        accounting_status: document_accounting::accounting_truth_status_key(
            &document_accounting::classify_accounting_truth_status(
                priced_stage_count,
                unpriced_stage_count,
                in_flight_stage_count,
                missing_stage_count,
            ),
        )
        .to_string(),
    }
}

fn build_stage_settlement_rollup_inputs(
    stage_rollups: &[repositories::RuntimeCollectionStageRollupRow],
    accounting_rows: &[repositories::RuntimeCollectionResolvedStageAccountingRow],
) -> Vec<repositories::RuntimeCollectionSettlementRollupInput> {
    let mut rows_by_stage =
        BTreeMap::<String, Vec<repositories::RuntimeCollectionResolvedStageAccountingRow>>::new();
    for row in accounting_rows {
        rows_by_stage.entry(row.stage.clone()).or_default().push(row.clone());
    }

    let mut rows = stage_rollups
        .iter()
        .map(|rollup| {
            let stage_rows = rows_by_stage.remove(&rollup.stage).unwrap_or_default();
            let summary = summarize_runtime_collection_scope_accounting(&stage_rows);
            repositories::RuntimeCollectionSettlementRollupInput {
                scope_kind: "stage".to_string(),
                scope_key: rollup.stage.clone(),
                queued_count: 0,
                processing_count: rollup.active_count.max(0),
                completed_count: rollup.completed_count.max(0),
                failed_count: rollup.failed_count.max(0),
                document_count: summary.document_count.max(0),
                ready_count: 0,
                ready_no_graph_count: 0,
                content_extracted_count: 0,
                chunked_count: 0,
                embedded_count: 0,
                graph_active_count: 0,
                graph_ready_count: 0,
                live_estimated_cost: summary.live_estimated_cost,
                settled_estimated_cost: summary.settled_estimated_cost,
                missing_estimated_cost: summary.missing_estimated_cost,
                currency: summary.currency.clone(),
                avg_elapsed_ms: rollup.avg_elapsed_ms,
                max_elapsed_ms: rollup.max_elapsed_ms,
                bottleneck_stage: None,
                bottleneck_avg_elapsed_ms: None,
                bottleneck_max_elapsed_ms: None,
                prompt_tokens: summary.prompt_tokens,
                completion_tokens: summary.completion_tokens,
                total_tokens: summary.total_tokens,
                accounting_status: summary.accounting_status,
                bottleneck_rank: None,
                is_primary_bottleneck: false,
            }
        })
        .collect::<Vec<_>>();

    apply_rollup_bottleneck_ranks(&mut rows, |row| {
        (row.avg_elapsed_ms, row.max_elapsed_ms, row.scope_key.clone())
    });
    rows
}

fn build_format_settlement_rollup_inputs(
    format_rollups: &[repositories::RuntimeCollectionFormatRollupRow],
    accounting_rows: &[repositories::RuntimeCollectionResolvedStageAccountingRow],
) -> Vec<repositories::RuntimeCollectionSettlementRollupInput> {
    let mut rows_by_format =
        BTreeMap::<String, Vec<repositories::RuntimeCollectionResolvedStageAccountingRow>>::new();
    for row in accounting_rows {
        rows_by_format.entry(row.file_type.clone()).or_default().push(row.clone());
    }

    let mut rows = format_rollups
        .iter()
        .map(|rollup| {
            let format_rows = rows_by_format.remove(&rollup.file_type).unwrap_or_default();
            let summary = summarize_runtime_collection_scope_accounting(&format_rows);
            repositories::RuntimeCollectionSettlementRollupInput {
                scope_kind: "format".to_string(),
                scope_key: rollup.file_type.clone(),
                queued_count: rollup.queued_count.max(0),
                processing_count: rollup.processing_count.max(0),
                completed_count: (rollup.ready_count + rollup.ready_no_graph_count).max(0),
                failed_count: rollup.failed_count.max(0),
                document_count: rollup.document_count.max(0),
                ready_count: rollup.ready_count.max(0),
                ready_no_graph_count: rollup.ready_no_graph_count.max(0),
                content_extracted_count: rollup.content_extracted_count.max(0),
                chunked_count: rollup.chunked_count.max(0),
                embedded_count: rollup.embedded_count.max(0),
                graph_active_count: rollup.extracting_graph_count.max(0),
                graph_ready_count: rollup.graph_ready_count.max(0),
                live_estimated_cost: summary.live_estimated_cost,
                settled_estimated_cost: summary.settled_estimated_cost,
                missing_estimated_cost: summary.missing_estimated_cost,
                currency: summary.currency.clone(),
                avg_elapsed_ms: rollup.avg_total_elapsed_ms,
                max_elapsed_ms: rollup.max_total_elapsed_ms,
                bottleneck_stage: rollup.bottleneck_stage.clone(),
                bottleneck_avg_elapsed_ms: rollup.bottleneck_avg_elapsed_ms,
                bottleneck_max_elapsed_ms: rollup.bottleneck_max_elapsed_ms,
                prompt_tokens: summary.prompt_tokens,
                completion_tokens: summary.completion_tokens,
                total_tokens: summary.total_tokens,
                accounting_status: summary.accounting_status,
                bottleneck_rank: None,
                is_primary_bottleneck: false,
            }
        })
        .collect::<Vec<_>>();

    apply_rollup_bottleneck_ranks(&mut rows, |row| {
        (
            row.bottleneck_avg_elapsed_ms.or(row.avg_elapsed_ms),
            row.bottleneck_max_elapsed_ms.or(row.max_elapsed_ms),
            row.scope_key.clone(),
        )
    });
    rows
}

fn apply_rollup_bottleneck_ranks<F>(
    rows: &mut [repositories::RuntimeCollectionSettlementRollupInput],
    metrics: F,
) where
    F: Fn(
        &repositories::RuntimeCollectionSettlementRollupInput,
    ) -> (Option<i64>, Option<i64>, String),
{
    let mut ranked = rows
        .iter()
        .enumerate()
        .map(|(index, row)| {
            let (avg_elapsed_ms, max_elapsed_ms, scope_key) = metrics(row);
            (index, avg_elapsed_ms, max_elapsed_ms, scope_key)
        })
        .collect::<Vec<_>>();
    ranked.sort_by(|left, right| {
        right.1.cmp(&left.1).then_with(|| right.2.cmp(&left.2)).then_with(|| left.3.cmp(&right.3))
    });
    let primary_rank_available = ranked
        .first()
        .map(|(_, avg_elapsed_ms, max_elapsed_ms, _)| {
            avg_elapsed_ms.is_some() || max_elapsed_ms.is_some()
        })
        .unwrap_or(false);
    for (rank, (index, _, _, _)) in ranked.into_iter().enumerate() {
        rows[index].bottleneck_rank = Some(i32::try_from(rank + 1).unwrap_or(i32::MAX));
        rows[index].is_primary_bottleneck = primary_rank_available && rank == 0;
    }
}

async fn build_library_queue_isolation_snapshot_input(
    state: &AppState,
    project_id: Uuid,
) -> anyhow::Result<repositories::RuntimeLibraryQueueSliceSnapshotInput> {
    let queue_slice =
        repositories::load_runtime_library_queue_slice(&state.persistence.postgres, project_id)
            .await
            .context("failed to load queue-isolation slice for snapshot refresh")?;
    let summary = state.pipeline_hardening_services.queue_isolation.summarize(
        usize::try_from(queue_slice.queued_count).unwrap_or(usize::MAX),
        usize::try_from(queue_slice.processing_count).unwrap_or(usize::MAX),
        usize::try_from(queue_slice.workspace_processing_count).unwrap_or(usize::MAX),
        usize::try_from(queue_slice.global_processing_count).unwrap_or(usize::MAX),
        queue_slice.last_claimed_at,
        queue_slice.last_progress_at,
        repositories::parse_runtime_queue_waiting_reason(queue_slice.waiting_reason.as_deref()),
    );
    Ok(repositories::RuntimeLibraryQueueSliceSnapshotInput {
        project_id,
        workspace_id: queue_slice.workspace_id,
        queued_count: queue_slice.queued_count,
        processing_count: queue_slice.processing_count,
        workspace_processing_count: queue_slice.workspace_processing_count,
        global_processing_count: queue_slice.global_processing_count,
        isolated_capacity_count: i64::try_from(summary.isolated_capacity_count).unwrap_or(i64::MAX),
        available_capacity_count: i64::try_from(summary.available_capacity_count)
            .unwrap_or(i64::MAX),
        waiting_reason: Some(
            repositories::runtime_queue_waiting_reason_key(&summary.waiting_reason).to_string(),
        ),
        last_claimed_at: summary.last_claimed_at,
        last_progress_at: summary.last_progress_at,
    })
}

const GRAPH_PROGRESS_FAST_POLL_MS: i64 = 2_000;
const GRAPH_PROGRESS_WATCH_POLL_MS: i64 = 4_000;
const GRAPH_PROGRESS_CALM_POLL_MS: i64 = 8_000;

#[must_use]
pub fn rank_runtime_graph_progress_bottlenecks(
    checkpoints: &[RuntimeGraphProgressCheckpointRow],
) -> HashMap<(Uuid, i32), usize> {
    let mut sorted = checkpoints.to_vec();
    sorted.sort_by(|left, right| {
        right
            .avg_chunk_elapsed_ms
            .unwrap_or_default()
            .cmp(&left.avg_chunk_elapsed_ms.unwrap_or_default())
            .then_with(|| right.total_chunks.cmp(&left.total_chunks))
            .then_with(|| left.ingestion_run_id.cmp(&right.ingestion_run_id))
    });
    sorted
        .into_iter()
        .enumerate()
        .map(|(index, row)| ((row.ingestion_run_id, row.attempt_no), index + 1))
        .collect()
}

#[must_use]
pub fn build_runtime_document_graph_throughput_summary(
    checkpoint: Option<&RuntimeGraphProgressCheckpointRow>,
    resume_rollup: Option<&RuntimeGraphExtractionResumeRollupRow>,
    bottleneck_rank: Option<usize>,
) -> Option<RuntimeDocumentGraphThroughputSummary> {
    let checkpoint = checkpoint?;
    let (cadence, recommended_poll_interval_ms) =
        graph_progress_cadence(checkpoint.pressure_kind.as_deref(), checkpoint.computed_at);
    let resumed_chunk_count = resume_rollup
        .map(|row| usize::try_from(row.resumed_chunk_count.max(0)).unwrap_or(usize::MAX))
        .unwrap_or(0);
    let resume_hit_count = resume_rollup
        .map(|row| usize::try_from(row.resume_hit_count.max(0)).unwrap_or(usize::MAX))
        .unwrap_or(0);
    let replayed_chunk_count = resume_rollup
        .map(|row| usize::try_from(row.replayed_chunk_count.max(0)).unwrap_or(usize::MAX))
        .unwrap_or(0);
    let max_downgrade_level = resume_rollup
        .map(|row| usize::try_from(row.max_downgrade_level.max(0)).unwrap_or(usize::MAX))
        .unwrap_or(0);
    let duplicate_work_ratio = (checkpoint.total_chunks > 0)
        .then(|| replayed_chunk_count as f64 / checkpoint.total_chunks.max(1) as f64);
    Some(RuntimeDocumentGraphThroughputSummary {
        processed_chunks: usize::try_from(checkpoint.processed_chunks).unwrap_or(usize::MAX),
        total_chunks: usize::try_from(checkpoint.total_chunks).unwrap_or(usize::MAX),
        progress_percent: checkpoint.progress_percent,
        provider_call_count: usize::try_from(checkpoint.provider_call_count).unwrap_or(usize::MAX),
        resumed_chunk_count,
        resume_hit_count,
        replayed_chunk_count,
        duplicate_work_ratio,
        max_downgrade_level,
        avg_call_elapsed_ms: checkpoint.avg_call_elapsed_ms,
        avg_chunk_elapsed_ms: checkpoint.avg_chunk_elapsed_ms,
        avg_chars_per_second: checkpoint.avg_chars_per_second,
        avg_tokens_per_second: checkpoint.avg_tokens_per_second,
        last_provider_call_at: checkpoint.last_provider_call_at,
        last_checkpoint_at: checkpoint.computed_at,
        last_checkpoint_elapsed_ms: checkpoint_age_ms(checkpoint.computed_at),
        next_checkpoint_eta_ms: checkpoint.next_checkpoint_eta_ms,
        pressure_kind: checkpoint.pressure_kind.clone(),
        cadence,
        recommended_poll_interval_ms,
        bottleneck_rank,
    })
}

#[must_use]
pub fn build_runtime_collection_graph_throughput_summary(
    checkpoints: &[RuntimeGraphProgressCheckpointRow],
    resume_rollups: &[RuntimeGraphExtractionResumeRollupRow],
) -> Option<RuntimeCollectionGraphThroughputSummary> {
    if checkpoints.is_empty() {
        return None;
    }

    let tracked_document_count = checkpoints.len();
    let active_document_count = checkpoints
        .iter()
        .filter(|checkpoint| checkpoint.processed_chunks < checkpoint.total_chunks)
        .count();
    let processed_chunks =
        checkpoints.iter().map(|checkpoint| checkpoint.processed_chunks.max(0)).sum::<i64>();
    let total_chunks =
        checkpoints.iter().map(|checkpoint| checkpoint.total_chunks.max(0)).sum::<i64>();
    let provider_call_count =
        checkpoints.iter().map(|checkpoint| checkpoint.provider_call_count.max(0)).sum::<i64>();
    let resumed_chunk_count =
        resume_rollups.iter().map(|rollup| rollup.resumed_chunk_count.max(0)).sum::<i64>();
    let resume_hit_count =
        resume_rollups.iter().map(|rollup| rollup.resume_hit_count.max(0)).sum::<i64>();
    let replayed_chunk_count =
        resume_rollups.iter().map(|rollup| rollup.replayed_chunk_count.max(0)).sum::<i64>();
    let max_downgrade_level = resume_rollups
        .iter()
        .map(|rollup| rollup.max_downgrade_level.max(0))
        .max()
        .unwrap_or_default();
    let total_call_elapsed_ms = checkpoints.iter().fold(0i64, |accumulator, checkpoint| {
        accumulator.saturating_add(
            checkpoint
                .avg_call_elapsed_ms
                .unwrap_or_default()
                .max(0)
                .saturating_mul(checkpoint.provider_call_count.max(0)),
        )
    });
    let total_chunk_elapsed_ms = checkpoints.iter().fold(0i64, |accumulator, checkpoint| {
        accumulator.saturating_add(
            checkpoint
                .avg_chunk_elapsed_ms
                .unwrap_or_default()
                .max(0)
                .saturating_mul(checkpoint.processed_chunks.max(0)),
        )
    });
    let chars_per_second_samples =
        checkpoints.iter().filter(|checkpoint| checkpoint.avg_chars_per_second.is_some()).count();
    let tokens_per_second_samples =
        checkpoints.iter().filter(|checkpoint| checkpoint.avg_tokens_per_second.is_some()).count();
    let last_checkpoint_at =
        checkpoints.iter().map(|checkpoint| checkpoint.computed_at).max().unwrap_or_else(Utc::now);
    let pressure_kind = strongest_graph_pressure_kind(
        checkpoints.iter().filter_map(|checkpoint| checkpoint.pressure_kind.as_deref()),
    );
    let (cadence, recommended_poll_interval_ms) =
        graph_progress_cadence(pressure_kind, last_checkpoint_at);

    Some(RuntimeCollectionGraphThroughputSummary {
        tracked_document_count,
        active_document_count,
        processed_chunks: usize::try_from(processed_chunks).unwrap_or(usize::MAX),
        total_chunks: usize::try_from(total_chunks).unwrap_or(usize::MAX),
        progress_percent: percent_from_counts(processed_chunks, total_chunks),
        provider_call_count: usize::try_from(provider_call_count).unwrap_or(usize::MAX),
        resumed_chunk_count: usize::try_from(resumed_chunk_count).unwrap_or(usize::MAX),
        resume_hit_count: usize::try_from(resume_hit_count).unwrap_or(usize::MAX),
        replayed_chunk_count: usize::try_from(replayed_chunk_count).unwrap_or(usize::MAX),
        duplicate_work_ratio: (total_chunks > 0)
            .then_some(replayed_chunk_count as f64 / total_chunks.max(1) as f64),
        max_downgrade_level: usize::try_from(max_downgrade_level).unwrap_or(usize::MAX),
        avg_call_elapsed_ms: (provider_call_count > 0)
            .then_some(total_call_elapsed_ms / provider_call_count.max(1)),
        avg_chunk_elapsed_ms: (processed_chunks > 0)
            .then_some(total_chunk_elapsed_ms / processed_chunks.max(1)),
        avg_chars_per_second: (chars_per_second_samples > 0).then(|| {
            checkpoints.iter().filter_map(|checkpoint| checkpoint.avg_chars_per_second).sum::<f64>()
                / chars_per_second_samples as f64
        }),
        avg_tokens_per_second: (tokens_per_second_samples > 0).then(|| {
            checkpoints
                .iter()
                .filter_map(|checkpoint| checkpoint.avg_tokens_per_second)
                .sum::<f64>()
                / tokens_per_second_samples as f64
        }),
        last_provider_call_at: checkpoints
            .iter()
            .filter_map(|checkpoint| checkpoint.last_provider_call_at)
            .max(),
        last_checkpoint_at,
        last_checkpoint_elapsed_ms: checkpoint_age_ms(last_checkpoint_at),
        next_checkpoint_eta_ms: checkpoints
            .iter()
            .filter_map(|checkpoint| checkpoint.next_checkpoint_eta_ms)
            .max(),
        pressure_kind: pressure_kind.map(str::to_string),
        cadence,
        recommended_poll_interval_ms,
        bottleneck_rank: Some(1),
    })
}

fn graph_progress_cadence(
    pressure_kind: Option<&str>,
    last_checkpoint_at: chrono::DateTime<Utc>,
) -> (RuntimeGraphProgressCadence, i64) {
    let last_checkpoint_elapsed_ms = checkpoint_age_ms(last_checkpoint_at);
    match (graph_pressure_severity(pressure_kind), last_checkpoint_elapsed_ms) {
        (2, _) | (_, 15_000..) => (RuntimeGraphProgressCadence::Fast, GRAPH_PROGRESS_FAST_POLL_MS),
        (1, _) | (_, 6_000..) => (RuntimeGraphProgressCadence::Watch, GRAPH_PROGRESS_WATCH_POLL_MS),
        _ => (RuntimeGraphProgressCadence::Calm, GRAPH_PROGRESS_CALM_POLL_MS),
    }
}

fn strongest_graph_pressure_kind<'a>(values: impl Iterator<Item = &'a str>) -> Option<&'a str> {
    values.max_by_key(|value| graph_pressure_severity(Some(*value)))
}

fn graph_pressure_severity(value: Option<&str>) -> i32 {
    match value {
        Some("high") => 2,
        Some("elevated") => 1,
        Some("steady") => 0,
        _ => -1,
    }
}

fn checkpoint_age_ms(value: chrono::DateTime<Utc>) -> i64 {
    Utc::now().signed_duration_since(value).num_milliseconds().max(0)
}

fn percent_from_counts(processed: i64, total: i64) -> Option<i32> {
    if processed <= 0 || total <= 0 {
        return None;
    }
    Some(((processed as f64 / total as f64) * 100.0).round() as i32)
}

fn parse_runtime_ingestion_status(status: &str) -> RuntimeIngestionStatus {
    match status {
        "ready" => RuntimeIngestionStatus::Ready,
        "ready_no_graph" => RuntimeIngestionStatus::ReadyNoGraph,
        "failed" => RuntimeIngestionStatus::Failed,
        "processing" => RuntimeIngestionStatus::Processing,
        _ => RuntimeIngestionStatus::Queued,
    }
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

fn build_chunk_embedding_upsert_inputs(
    chunks: &[repositories::ChunkRow],
    batch_response: &EmbeddingBatchResponse,
) -> Vec<repositories::ChunkEmbeddingUpsertInput> {
    chunks
        .iter()
        .zip(batch_response.embeddings.iter())
        .map(|(chunk, embedding)| repositories::ChunkEmbeddingUpsertInput {
            chunk_id: chunk.id,
            project_id: chunk.project_id,
            provider_kind: batch_response.provider_kind.clone(),
            model_name: batch_response.model_name.clone(),
            dimensions: i32::try_from(embedding.len()).unwrap_or(i32::MAX),
            embedding_json: serde_json::to_value(embedding).unwrap_or_else(|_| json!([])),
        })
        .collect()
}

fn build_chunk_embedding_vector_write_inputs(
    chunks: &[repositories::ChunkRow],
    embeddings: &[Vec<f32>],
) -> Vec<vector_search::ChunkEmbeddingVectorWriteInput> {
    chunks
        .iter()
        .zip(embeddings.iter())
        .filter_map(|(chunk, embedding)| {
            (embedding.len() == 1536).then(|| vector_search::ChunkEmbeddingVectorWriteInput {
                chunk_id: chunk.id,
                embedding: embedding.clone(),
            })
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
    let mut usage = RuntimeStageUsageSummary::with_model(
        provider_profile.embedding.provider_kind.as_str(),
        &provider_profile.embedding.model_name,
    );
    for chunk_batch in chunks.chunks(EMBEDDING_BATCH_SIZE) {
        if let Some(lease_heartbeat) = lease_heartbeat.as_deref_mut() {
            lease_heartbeat.maybe_renew(state).await?;
        }
        let batch_response = state
            .llm_gateway
            .embed_many(EmbeddingBatchRequest {
                provider_kind: provider_profile.embedding.provider_kind.as_str().to_string(),
                model_name: provider_profile.embedding.model_name.clone(),
                inputs: chunk_batch.iter().map(|chunk| chunk.content.clone()).collect::<Vec<_>>(),
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

        repositories::upsert_chunk_embeddings(
            &state.persistence.postgres,
            &build_chunk_embedding_upsert_inputs(chunk_batch, &batch_response),
        )
        .await
        .with_context(|| {
            format!(
                "failed to persist chunk embedding batch starting with {}",
                chunk_batch.first().map(|chunk| chunk.id).unwrap_or_default()
            )
        })?;

        let vector_rows =
            build_chunk_embedding_vector_write_inputs(chunk_batch, &batch_response.embeddings);
        if !vector_rows.is_empty() {
            vector_search::set_chunk_embedding_vectors(&state.persistence.postgres, &vector_rows)
                .await
                .with_context(|| {
                    format!(
                        "failed to write pgvector chunk batch starting with {}",
                        chunk_batch.first().map(|chunk| chunk.id).unwrap_or_default()
                    )
                })?;
        }

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
    let mut usage = RuntimeStageUsageSummary::with_model(
        provider_profile.embedding.provider_kind.as_str(),
        &provider_profile.embedding.model_name,
    );
    for node_batch in nodes_to_embed.chunks(EMBEDDING_BATCH_SIZE) {
        if let Some(lease_heartbeat) = lease_heartbeat.as_deref_mut() {
            lease_heartbeat.maybe_renew(state).await?;
        }
        let batch_response = state
            .llm_gateway
            .embed_many(EmbeddingBatchRequest {
                provider_kind: provider_profile.embedding.provider_kind.as_str().to_string(),
                model_name: provider_profile.embedding.model_name.clone(),
                inputs: node_batch
                    .iter()
                    .map(|node| build_graph_node_embedding_input(node))
                    .collect::<Vec<_>>(),
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
    let mut usage = RuntimeStageUsageSummary::with_model(
        provider_profile.embedding.provider_kind.as_str(),
        &provider_profile.embedding.model_name,
    );
    for edge_batch in edges.chunks(EMBEDDING_BATCH_SIZE) {
        if let Some(lease_heartbeat) = lease_heartbeat.as_deref_mut() {
            lease_heartbeat.maybe_renew(state).await?;
        }
        let batch_response = state
            .llm_gateway
            .embed_many(EmbeddingBatchRequest {
                provider_kind: provider_profile.embedding.provider_kind.as_str().to_string(),
                model_name: provider_profile.embedding.model_name.clone(),
                inputs: edge_batch
                    .iter()
                    .map(|edge| build_graph_edge_embedding_input(edge, &node_index))
                    .collect::<Vec<_>>(),
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

    #[test]
    fn maps_provider_profile_row_into_effective_profile() {
        let row = RuntimeProviderProfileRow {
            project_id: Uuid::nil(),
            indexing_provider_kind: "openai".to_string(),
            indexing_model_name: "gpt-5-mini".to_string(),
            embedding_provider_kind: "deepseek".to_string(),
            embedding_model_name: "embedding-v1".to_string(),
            answer_provider_kind: "openai".to_string(),
            answer_model_name: "gpt-5.4".to_string(),
            vision_provider_kind: "openai".to_string(),
            vision_model_name: "gpt-5-mini".to_string(),
            last_validated_at: None,
            last_validation_status: None,
            last_validation_error: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };

        let profile = map_runtime_provider_profile_row(&row);

        assert_eq!(profile.indexing.provider_kind, SupportedProviderKind::OpenAi);
        assert_eq!(profile.embedding.provider_kind, SupportedProviderKind::DeepSeek);
        assert_eq!(profile.answer.model_name, "gpt-5.4");
    }

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

        let restored =
            provider_profile_from_snapshot_json(&provider_profile_snapshot_json(&profile))
                .expect("restore snapshot profile");

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
    fn chunk_vector_batch_only_keeps_pgvector_compatible_dimensions() {
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

        let vector_rows = build_chunk_embedding_vector_write_inputs(
            &chunks,
            &[vec![0.0; 1536], vec![0.1, 0.2, 0.3]],
        );

        assert_eq!(vector_rows.len(), 1);
        assert_eq!(vector_rows[0].chunk_id, chunks[0].id);
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

    #[test]
    fn graph_progress_bottlenecks_rank_slowest_checkpoint_first() {
        let slow_run_id = Uuid::now_v7();
        let fast_run_id = Uuid::now_v7();
        let checkpoints = vec![
            repositories::RuntimeGraphProgressCheckpointRow {
                ingestion_run_id: fast_run_id,
                attempt_no: 1,
                processed_chunks: 10,
                total_chunks: 40,
                progress_percent: Some(25),
                provider_call_count: 10,
                avg_call_elapsed_ms: Some(1_500),
                avg_chunk_elapsed_ms: Some(2_500),
                avg_chars_per_second: Some(1200.0),
                avg_tokens_per_second: Some(420.0),
                last_provider_call_at: Some(Utc::now()),
                next_checkpoint_eta_ms: Some(8_000),
                pressure_kind: Some("steady".to_string()),
                provider_failure_class: None,
                request_shape_key: None,
                request_size_bytes: None,
                upstream_status: None,
                retry_outcome: None,
                computed_at: Utc::now(),
            },
            repositories::RuntimeGraphProgressCheckpointRow {
                ingestion_run_id: slow_run_id,
                attempt_no: 1,
                processed_chunks: 8,
                total_chunks: 40,
                progress_percent: Some(20),
                provider_call_count: 8,
                avg_call_elapsed_ms: Some(3_500),
                avg_chunk_elapsed_ms: Some(12_000),
                avg_chars_per_second: Some(420.0),
                avg_tokens_per_second: Some(150.0),
                last_provider_call_at: Some(Utc::now()),
                next_checkpoint_eta_ms: Some(20_000),
                pressure_kind: Some("high".to_string()),
                provider_failure_class: None,
                request_shape_key: None,
                request_size_bytes: None,
                upstream_status: None,
                retry_outcome: None,
                computed_at: Utc::now(),
            },
        ];

        let ranks = rank_runtime_graph_progress_bottlenecks(&checkpoints);

        assert_eq!(ranks.get(&(slow_run_id, 1)), Some(&1usize));
        assert_eq!(ranks.get(&(fast_run_id, 1)), Some(&2usize));
    }

    #[test]
    fn document_graph_progress_summary_exposes_poll_cadence() {
        let checkpoint = repositories::RuntimeGraphProgressCheckpointRow {
            ingestion_run_id: Uuid::now_v7(),
            attempt_no: 2,
            processed_chunks: 12,
            total_chunks: 40,
            progress_percent: Some(30),
            provider_call_count: 12,
            avg_call_elapsed_ms: Some(2_800),
            avg_chunk_elapsed_ms: Some(10_500),
            avg_chars_per_second: Some(640.0),
            avg_tokens_per_second: Some(210.0),
            last_provider_call_at: Some(Utc::now()),
            next_checkpoint_eta_ms: Some(15_000),
            pressure_kind: Some("high".to_string()),
            provider_failure_class: None,
            request_shape_key: None,
            request_size_bytes: None,
            upstream_status: None,
            retry_outcome: None,
            computed_at: Utc::now(),
        };

        let resume_rollup = repositories::RuntimeGraphExtractionResumeRollupRow {
            ingestion_run_id: checkpoint.ingestion_run_id,
            chunk_count: 12,
            ready_chunk_count: 10,
            failed_chunk_count: 1,
            replayed_chunk_count: 4,
            resume_hit_count: 2,
            resumed_chunk_count: 2,
            max_downgrade_level: 1,
        };

        let summary = build_runtime_document_graph_throughput_summary(
            Some(&checkpoint),
            Some(&resume_rollup),
            Some(1),
        )
        .expect("summary should exist");

        assert_eq!(summary.cadence, RuntimeGraphProgressCadence::Fast);
        assert_eq!(summary.recommended_poll_interval_ms, GRAPH_PROGRESS_FAST_POLL_MS);
        assert_eq!(summary.bottleneck_rank, Some(1));
        assert_eq!(summary.resumed_chunk_count, 2);
        assert_eq!(summary.replayed_chunk_count, 4);
        assert_eq!(summary.max_downgrade_level, 1);
    }

    #[test]
    fn collection_graph_progress_summary_aggregates_checkpoint_truth() {
        let now = Utc::now();
        let checkpoints = vec![
            repositories::RuntimeGraphProgressCheckpointRow {
                ingestion_run_id: Uuid::now_v7(),
                attempt_no: 1,
                processed_chunks: 10,
                total_chunks: 20,
                progress_percent: Some(50),
                provider_call_count: 10,
                avg_call_elapsed_ms: Some(2_000),
                avg_chunk_elapsed_ms: Some(4_000),
                avg_chars_per_second: Some(1_000.0),
                avg_tokens_per_second: Some(350.0),
                last_provider_call_at: Some(now),
                next_checkpoint_eta_ms: Some(6_000),
                pressure_kind: Some("elevated".to_string()),
                provider_failure_class: None,
                request_shape_key: None,
                request_size_bytes: None,
                upstream_status: None,
                retry_outcome: None,
                computed_at: now,
            },
            repositories::RuntimeGraphProgressCheckpointRow {
                ingestion_run_id: Uuid::now_v7(),
                attempt_no: 1,
                processed_chunks: 5,
                total_chunks: 20,
                progress_percent: Some(25),
                provider_call_count: 5,
                avg_call_elapsed_ms: Some(1_000),
                avg_chunk_elapsed_ms: Some(2_000),
                avg_chars_per_second: Some(1_200.0),
                avg_tokens_per_second: Some(420.0),
                last_provider_call_at: Some(now),
                next_checkpoint_eta_ms: Some(8_000),
                pressure_kind: Some("steady".to_string()),
                provider_failure_class: None,
                request_shape_key: None,
                request_size_bytes: None,
                upstream_status: None,
                retry_outcome: None,
                computed_at: now,
            },
        ];

        let resume_rollups = vec![
            repositories::RuntimeGraphExtractionResumeRollupRow {
                ingestion_run_id: checkpoints[0].ingestion_run_id,
                chunk_count: 10,
                ready_chunk_count: 8,
                failed_chunk_count: 1,
                replayed_chunk_count: 3,
                resume_hit_count: 1,
                resumed_chunk_count: 1,
                max_downgrade_level: 1,
            },
            repositories::RuntimeGraphExtractionResumeRollupRow {
                ingestion_run_id: checkpoints[1].ingestion_run_id,
                chunk_count: 5,
                ready_chunk_count: 5,
                failed_chunk_count: 0,
                replayed_chunk_count: 1,
                resume_hit_count: 1,
                resumed_chunk_count: 1,
                max_downgrade_level: 0,
            },
        ];

        let summary =
            build_runtime_collection_graph_throughput_summary(&checkpoints, &resume_rollups)
                .expect("summary should exist");

        assert_eq!(summary.tracked_document_count, 2);
        assert_eq!(summary.active_document_count, 2);
        assert_eq!(summary.processed_chunks, 15);
        assert_eq!(summary.total_chunks, 40);
        assert_eq!(summary.provider_call_count, 15);
        assert_eq!(summary.resumed_chunk_count, 2);
        assert_eq!(summary.replayed_chunk_count, 4);
        assert_eq!(summary.progress_percent, Some(38));
        assert_eq!(summary.pressure_kind.as_deref(), Some("elevated"));
    }

    #[test]
    fn terminal_transition_is_preserved_when_terminal_truth_does_not_change() {
        let previous_at = Utc::now();
        let existing = repositories::RuntimeCollectionTerminalOutcomeRow {
            project_id: Uuid::now_v7(),
            workspace_id: Uuid::now_v7(),
            terminal_state: "failed_with_residual_work".to_string(),
            residual_reason: Some("provider_failure".to_string()),
            queued_count: 0,
            processing_count: 0,
            pending_graph_count: 0,
            failed_document_count: 1,
            live_total_estimated_cost: None,
            settled_total_estimated_cost: None,
            missing_total_estimated_cost: None,
            currency: None,
            settled_at: None,
            last_transition_at: previous_at,
        };

        let transition_at = terminal_transition_at(
            Some(&existing),
            0,
            0,
            0,
            1,
            0,
            Some(&RuntimeCollectionResidualReason::ProviderFailure),
        );

        assert_eq!(transition_at, Some(previous_at));
    }
}
