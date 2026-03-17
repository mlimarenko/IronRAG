use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context, bail};
use chrono::Utc;
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
            RuntimeDocumentActivityStatus, RuntimeIngestionStage, RuntimeIngestionStatus,
        },
    },
    infra::repositories::{
        self, IngestionExecutionPayload, IngestionJobRow, RuntimeExtractedContentRow,
        RuntimeGraphEdgeRow, RuntimeGraphNodeRow, RuntimeIngestionRunRow,
        RuntimeProviderProfileRow,
    },
    infra::vector_search,
    integrations::llm::EmbeddingBatchRequest,
    services::{
        document_reconciliation::{DeleteDocumentRequest, delete_document_and_reconcile},
        graph_projection::mark_graph_snapshot_stale,
        graph_rebuild::rebuild_library_graph,
        ingest_activity::IngestActivityService,
    },
    shared::file_extract::{
        FileExtractionPlan, UploadFileKind, build_runtime_file_extraction_plan,
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
        )
        .await
        .with_context(|| format!("failed to renew ingestion job lease {}", self.job_id))?;
        if !renewed {
            bail!("worker {} no longer owns ingestion job {} lease", self.worker_id, self.job_id);
        }
        self.last_renewed_at = Instant::now();
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
                )
                .await
                {
                    Ok(true) => {}
                    Ok(false) => {
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
                    if let Err(error) = repositories::update_runtime_ingestion_run_heartbeat(
                        &state.persistence.postgres,
                        runtime_ingestion_run_id,
                        Utc::now(),
                        activity_status_label(RuntimeDocumentActivityStatus::Active),
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
    repositories::upsert_runtime_extracted_content(
        &state.persistence.postgres,
        ingestion_run_id,
        document_id,
        &extraction_plan.extraction_kind,
        extraction_plan.extracted_text.as_deref(),
        extraction_plan.page_count.and_then(|value| i32::try_from(value).ok()),
        extraction_plan
            .extracted_text
            .as_ref()
            .and_then(|value| i32::try_from(value.chars().count()).ok()),
        serde_json::to_value(&extraction_plan.extraction_warnings).unwrap_or_else(|_| json!([])),
        extraction_plan.source_map.clone(),
        extraction_plan.provider_kind.as_deref(),
        extraction_plan.model_name.as_deref(),
        extraction_plan.extraction_version.as_deref(),
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
    repositories::upsert_runtime_extracted_content(
        &state.persistence.postgres,
        ingestion_run_id,
        document_id,
        payload
            .extraction_kind
            .as_deref()
            .unwrap_or_else(|| payload.file_kind.as_deref().unwrap_or("unknown")),
        payload.text.as_deref(),
        payload.page_count.and_then(|value| i32::try_from(value).ok()),
        payload.text.as_ref().and_then(|value| i32::try_from(value.chars().count()).ok()),
        serde_json::to_value(&payload.extraction_warnings).unwrap_or_else(|_| json!([])),
        payload.source_map.clone(),
        payload.extraction_provider_kind.as_deref(),
        payload.extraction_model_name.as_deref(),
        payload.extraction_version.as_deref(),
    )
    .await
    .context("failed to persist runtime extracted content from payload")
}

pub fn validate_runtime_extraction_plan(
    file_name: &str,
    extraction_plan: &FileExtractionPlan,
) -> anyhow::Result<()> {
    if extraction_plan.file_kind == UploadFileKind::TextLike
        && extraction_plan.extracted_text.as_deref().is_some_and(|text| text.trim().is_empty())
    {
        bail!("uploaded file {file_name} is empty");
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
    repositories::create_ingestion_job(
        &state.persistence.postgres,
        runtime_run.project_id,
        source_id,
        trigger_kind,
        requested_by,
        parent_job_id,
        None,
        payload_json,
    )
    .await
    .context("failed to create runtime ingestion job")
}

pub async fn queue_new_runtime_upload(
    state: &AppState,
    request: QueueRuntimeUploadRequest,
) -> anyhow::Result<RuntimeQueuedUpload> {
    let provider_profile = resolve_effective_provider_profile(state, request.project_id).await?;
    let extraction_plan = build_runtime_file_extraction_plan(
        state.llm_gateway.as_ref(),
        &provider_profile.vision,
        Some(&request.file.file_name),
        request.file.mime_type.as_deref(),
        request.file.file_bytes.clone(),
    )
    .await
    .with_context(|| format!("failed to extract {}", request.file.file_name))?;
    validate_runtime_extraction_plan(&request.file.file_name, &extraction_plan)?;
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
        build_runtime_payload_json(&request, &runtime_run, &extraction_plan),
    )
    .await
    .context("failed to create runtime ingestion job")?;

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

    Ok((runtime_run, ingestion_job))
}

pub async fn delete_runtime_run_and_rebuild(
    state: &AppState,
    runtime_run: &RuntimeIngestionRunRow,
    requested_by: Option<&str>,
) -> anyhow::Result<Option<repositories::DocumentMutationWorkflowRow>> {
    if runtime_run.document_id.is_none() {
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

    Ok((runtime_run, ingestion_job))
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

        for (chunk, embedding) in chunk_batch.iter().zip(batch_response.embeddings.iter()) {
            repositories::upsert_chunk_embedding(
                &state.persistence.postgres,
                chunk.id,
                chunk.project_id,
                &batch_response.provider_kind,
                &batch_response.model_name,
                i32::try_from(embedding.len()).unwrap_or(i32::MAX),
                serde_json::to_value(embedding).unwrap_or_else(|_| json!([])),
            )
            .await
            .with_context(|| format!("failed to persist chunk embedding {}", chunk.id))?;

            if embedding.len() == 1536 {
                vector_search::set_chunk_embedding_vector(
                    &state.persistence.postgres,
                    chunk.id,
                    embedding,
                )
                .await
                .with_context(|| {
                    format!("failed to write pgvector embedding for chunk {}", chunk.id)
                })?;
            }
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

        for (node, embedding) in node_batch.iter().zip(batch_response.embeddings.iter()) {
            repositories::upsert_runtime_vector_target(
                &state.persistence.postgres,
                node.project_id,
                "entity",
                node.id,
                &batch_response.provider_kind,
                &batch_response.model_name,
                i32::try_from(embedding.len()).ok(),
                serde_json::to_value(embedding).unwrap_or_else(|_| json!([])),
            )
            .await
            .with_context(|| format!("failed to persist graph node embedding {}", node.id))?;
        }
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

        for (edge, embedding) in edge_batch.iter().zip(batch_response.embeddings.iter()) {
            repositories::upsert_runtime_vector_target(
                &state.persistence.postgres,
                edge.project_id,
                "relation",
                edge.id,
                &batch_response.provider_kind,
                &batch_response.model_name,
                i32::try_from(embedding.len()).ok(),
                serde_json::to_value(embedding).unwrap_or_else(|_| json!([])),
            )
            .await
            .with_context(|| format!("failed to persist graph edge embedding {}", edge.id))?;
        }
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
}
