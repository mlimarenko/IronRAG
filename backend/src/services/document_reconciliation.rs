use anyhow::{Context, bail};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    app::state::AppState,
    infra::repositories::{
        self, DocumentMutationWorkflowRow, DocumentRevisionRow, IngestionJobRow,
        RuntimeIngestionRunRow,
    },
    services::runtime_ingestion::{
        RuntimeUploadFileInput, persist_extracted_content_from_plan,
        provider_profile_snapshot_json, queue_prepared_runtime_attempt,
        resolve_effective_provider_profile, validate_runtime_extraction_plan,
    },
    services::{graph_projection::mark_graph_snapshot_stale, graph_rebuild::rebuild_library_graph},
    shared::file_extract::build_runtime_file_extraction_plan,
};

#[derive(Debug, Clone, Default)]
pub struct DocumentReconciliationService;

#[derive(Debug, Clone)]
pub struct CreateRevisionRequest {
    pub document_id: Uuid,
    pub revision_kind: String,
    pub parent_revision_id: Option<Uuid>,
    pub source_file_name: String,
    pub mime_type: Option<String>,
    pub file_size_bytes: Option<i64>,
    pub appended_text_excerpt: Option<String>,
    pub content_hash: Option<String>,
}

#[derive(Debug, Clone)]
pub struct QueuedDocumentMutation {
    pub runtime_run: RuntimeIngestionRunRow,
    pub ingestion_job: IngestionJobRow,
    pub target_revision: DocumentRevisionRow,
    pub mutation_workflow: DocumentMutationWorkflowRow,
}

#[derive(Debug, Clone)]
pub struct AppendDocumentRequest {
    pub runtime_run: RuntimeIngestionRunRow,
    pub requested_by: Option<String>,
    pub trigger_kind: String,
    pub parent_job_id: Option<Uuid>,
    pub appended_text: String,
}

#[derive(Debug, Clone)]
pub struct ReplaceDocumentRequest {
    pub runtime_run: RuntimeIngestionRunRow,
    pub requested_by: Option<String>,
    pub trigger_kind: String,
    pub parent_job_id: Option<Uuid>,
    pub file: RuntimeUploadFileInput,
}

#[derive(Debug, Clone)]
pub struct DeleteDocumentRequest {
    pub runtime_run: RuntimeIngestionRunRow,
    pub requested_by: Option<String>,
}

#[derive(Debug, Clone)]
pub struct DeletedDocumentMutation {
    pub runtime_run: RuntimeIngestionRunRow,
    pub mutation_workflow: DocumentMutationWorkflowRow,
}

const DELETE_RECONCILIATION_ACTIVITY_MESSAGE: &str =
    "waiting for graph reconciliation after delete mutation";

pub async fn create_document_revision(
    state: &AppState,
    request: CreateRevisionRequest,
) -> anyhow::Result<DocumentRevisionRow> {
    let next_revision_no =
        repositories::next_document_revision_no(&state.persistence.postgres, request.document_id)
            .await
            .context("failed to compute next document revision number")?;
    repositories::create_document_revision(
        &state.persistence.postgres,
        request.document_id,
        next_revision_no,
        &request.revision_kind,
        request.parent_revision_id,
        &request.source_file_name,
        request.mime_type.as_deref(),
        request.file_size_bytes,
        request.appended_text_excerpt.as_deref(),
        request.content_hash.as_deref(),
    )
    .await
    .context("failed to create document revision")
}

pub async fn create_document_mutation_workflow(
    state: &AppState,
    document_id: Uuid,
    target_revision_id: Option<Uuid>,
    mutation_kind: &str,
    stale_guard_revision_no: Option<i32>,
    requested_by: Option<&str>,
) -> anyhow::Result<DocumentMutationWorkflowRow> {
    repositories::create_document_mutation_workflow(
        &state.persistence.postgres,
        document_id,
        target_revision_id,
        mutation_kind,
        stale_guard_revision_no,
        requested_by,
    )
    .await
    .context("failed to create document mutation workflow")
}

pub async fn queue_append_document_mutation(
    state: &AppState,
    request: AppendDocumentRequest,
) -> anyhow::Result<QueuedDocumentMutation> {
    let appended_text = request.appended_text.trim();
    if appended_text.is_empty() {
        bail!("append payload must not be empty");
    }

    let (document, active_revision, existing_extracted_content) =
        load_mutable_document_context(state, &request.runtime_run).await?;
    let existing_extracted_content = existing_extracted_content.with_context(|| {
        format!("runtime ingestion run {} has no extracted content", request.runtime_run.id)
    })?;
    let existing_text = existing_extracted_content
        .content_text
        .clone()
        .filter(|value| !value.trim().is_empty())
        .with_context(|| {
            format!(
                "runtime ingestion run {} has no extracted text available for append",
                request.runtime_run.id
            )
        })?;
    let combined_text = if existing_text.trim().is_empty() {
        appended_text.to_string()
    } else {
        format!("{}\n\n{}", existing_text.trim_end(), appended_text)
    };
    let combined_hash = sha256_hex(&combined_text);
    let target_revision = create_document_revision(
        state,
        CreateRevisionRequest {
            document_id: document.id,
            revision_kind: "append".to_string(),
            parent_revision_id: Some(active_revision.id),
            source_file_name: request.runtime_run.file_name.clone(),
            mime_type: request.runtime_run.mime_type.clone(),
            file_size_bytes: request.runtime_run.file_size_bytes,
            appended_text_excerpt: Some(truncate_excerpt(appended_text)),
            content_hash: Some(combined_hash.clone()),
        },
    )
    .await?;
    let mutation_workflow = create_document_mutation_workflow(
        state,
        document.id,
        Some(target_revision.id),
        "update_append",
        Some(active_revision.revision_no),
        request.requested_by.as_deref(),
    )
    .await?;
    repositories::update_document_current_revision(
        &state.persistence.postgres,
        document.id,
        document.current_revision_id,
        "reconciling",
        Some("update_append"),
        Some("accepted"),
    )
    .await
    .context("failed to mark document append mutation as accepted")?;

    let provider_profile =
        resolve_effective_provider_profile(state, request.runtime_run.project_id).await?;
    let runtime_run = repositories::prepare_runtime_ingestion_run_for_attempt(
        &state.persistence.postgres,
        request.runtime_run.id,
        Some(target_revision.id),
        provider_profile_snapshot_json(&provider_profile),
        "update_append",
        &request.runtime_run.file_name,
        &request.runtime_run.file_type,
        request.runtime_run.mime_type.as_deref(),
        request.runtime_run.file_size_bytes,
    )
    .await
    .context("failed to prepare runtime ingestion run for append mutation")?;
    let extracted_content = repositories::upsert_runtime_extracted_content(
        &state.persistence.postgres,
        runtime_run.id,
        Some(document.id),
        "append_text",
        Some(&combined_text),
        existing_extracted_content.page_count,
        i32::try_from(combined_text.chars().count()).ok(),
        existing_extracted_content.extraction_warnings_json.clone(),
        serde_json::json!({
            "mutation_kind": "update_append",
            "base_source_map": existing_extracted_content.source_map_json,
            "appended_char_count": appended_text.chars().count(),
        }),
        existing_extracted_content.provider_kind.as_deref(),
        existing_extracted_content.model_name.as_deref(),
        existing_extracted_content.extraction_version.as_deref(),
    )
    .await
    .context("failed to persist appended extracted content")?;
    let ingestion_job = queue_prepared_runtime_attempt(
        state,
        &runtime_run,
        &extracted_content,
        document.source_id,
        request.requested_by.as_deref(),
        &request.trigger_kind,
        request.parent_job_id,
        Some(mutation_workflow.id),
        Some(active_revision.revision_no),
        Some("update_append"),
    )
    .await?;

    Ok(QueuedDocumentMutation { runtime_run, ingestion_job, target_revision, mutation_workflow })
}

pub async fn queue_replace_document_mutation(
    state: &AppState,
    request: ReplaceDocumentRequest,
) -> anyhow::Result<QueuedDocumentMutation> {
    let (document, active_revision, _) =
        load_mutable_document_context(state, &request.runtime_run).await?;
    let provider_profile =
        resolve_effective_provider_profile(state, request.runtime_run.project_id).await?;
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
    let replacement_text = extraction_plan
        .extracted_text
        .clone()
        .filter(|value| !value.trim().is_empty())
        .with_context(|| {
            format!("replacement file {} has no extracted text", request.file.file_name)
        })?;
    let target_revision = create_document_revision(
        state,
        CreateRevisionRequest {
            document_id: document.id,
            revision_kind: "replace".to_string(),
            parent_revision_id: Some(active_revision.id),
            source_file_name: request.file.file_name.clone(),
            mime_type: request.file.mime_type.clone(),
            file_size_bytes: i64::try_from(request.file.file_bytes.len()).ok(),
            appended_text_excerpt: None,
            content_hash: Some(sha256_hex(&replacement_text)),
        },
    )
    .await?;
    let mutation_workflow = create_document_mutation_workflow(
        state,
        document.id,
        Some(target_revision.id),
        "update_replace",
        Some(active_revision.revision_no),
        request.requested_by.as_deref(),
    )
    .await?;
    repositories::update_document_current_revision(
        &state.persistence.postgres,
        document.id,
        document.current_revision_id,
        "reconciling",
        Some("update_replace"),
        Some("accepted"),
    )
    .await
    .context("failed to mark document replace mutation as accepted")?;

    let runtime_run = repositories::prepare_runtime_ingestion_run_for_attempt(
        &state.persistence.postgres,
        request.runtime_run.id,
        Some(target_revision.id),
        provider_profile_snapshot_json(&provider_profile),
        "update_replace",
        &request.file.file_name,
        extraction_plan.file_kind.as_str(),
        request.file.mime_type.as_deref(),
        i64::try_from(request.file.file_bytes.len()).ok(),
    )
    .await
    .context("failed to prepare runtime ingestion run for replace mutation")?;
    let extracted_content = persist_extracted_content_from_plan(
        state,
        runtime_run.id,
        Some(document.id),
        &extraction_plan,
    )
    .await?;
    let ingestion_job = queue_prepared_runtime_attempt(
        state,
        &runtime_run,
        &extracted_content,
        document.source_id,
        request.requested_by.as_deref(),
        &request.trigger_kind,
        request.parent_job_id,
        Some(mutation_workflow.id),
        Some(active_revision.revision_no),
        Some("update_replace"),
    )
    .await?;

    Ok(QueuedDocumentMutation { runtime_run, ingestion_job, target_revision, mutation_workflow })
}

pub async fn delete_document_and_reconcile(
    state: &AppState,
    request: DeleteDocumentRequest,
) -> anyhow::Result<DeletedDocumentMutation> {
    let (document, active_revision, _) =
        load_mutable_document_context(state, &request.runtime_run).await?;
    let mutation_workflow = create_document_mutation_workflow(
        state,
        document.id,
        None,
        "delete",
        Some(active_revision.revision_no),
        request.requested_by.as_deref(),
    )
    .await?;
    repositories::update_document_current_revision(
        &state.persistence.postgres,
        document.id,
        document.current_revision_id,
        "deleting",
        Some("delete"),
        Some("reconciling"),
    )
    .await
    .context("failed to mark logical document as deleting")?;
    if should_publish_delete_reconciliation_activity(&request.runtime_run) {
        repositories::update_runtime_ingestion_run_stage_activity(
            &state.persistence.postgres,
            request.runtime_run.id,
            &request.runtime_run.current_stage,
            request.runtime_run.progress_percent,
            "blocked",
            chrono::Utc::now(),
            Some(DELETE_RECONCILIATION_ACTIVITY_MESSAGE),
        )
        .await
        .context("failed to publish delete reconciliation activity on runtime run")?;
    }
    repositories::delete_ingestion_jobs_by_runtime_ingestion_run_id(
        &state.persistence.postgres,
        request.runtime_run.id,
    )
    .await
    .context("failed to delete queued ingestion jobs for tombstoned runtime run")?;
    repositories::delete_runtime_query_references_by_document_revision(
        &state.persistence.postgres,
        document.project_id,
        document.id,
        active_revision.id,
    )
    .await
    .context("failed to delete query references for deleted document revision")?;
    repositories::deactivate_runtime_graph_evidence_by_document_revision(
        &state.persistence.postgres,
        document.project_id,
        document.id,
        active_revision.id,
        Some(mutation_workflow.id),
    )
    .await
    .context("failed to deactivate graph evidence for deleted document revision")?;
    let chunk_ids = repositories::list_chunks_by_document(&state.persistence.postgres, document.id)
        .await
        .context("failed to list chunks for deleted document")?
        .into_iter()
        .map(|chunk| chunk.id)
        .collect::<Vec<_>>();
    repositories::delete_chunks_by_ids(&state.persistence.postgres, &chunk_ids)
        .await
        .context("failed to delete chunks for tombstoned document")?;

    let snapshot =
        repositories::get_runtime_graph_snapshot(&state.persistence.postgres, document.project_id)
            .await
            .context("failed to load graph snapshot before delete reconciliation")?;
    let projection_version =
        snapshot.as_ref().map(|row| row.projection_version).filter(|value| *value > 0).unwrap_or(1);
    let node_count = snapshot
        .as_ref()
        .map(|row| usize::try_from(row.node_count).unwrap_or_default())
        .unwrap_or_default();
    let edge_count = snapshot
        .as_ref()
        .map(|row| usize::try_from(row.edge_count).unwrap_or_default())
        .unwrap_or_default();
    let _ = mark_graph_snapshot_stale(
        state,
        document.project_id,
        projection_version,
        node_count,
        edge_count,
        Some("Graph rebuild pending after document delete."),
    )
    .await;

    repositories::update_document_revision_status(
        &state.persistence.postgres,
        active_revision.id,
        "deleted",
    )
    .await
    .context("failed to mark deleted revision as deleted")?;
    repositories::tombstone_document_by_id(
        &state.persistence.postgres,
        document.id,
        "deleted",
        Some("delete"),
        Some("reconciling"),
    )
    .await
    .context("failed to tombstone logical document")?;

    if let Err(error) = rebuild_library_graph(state, document.project_id).await {
        let _ = repositories::update_document_mutation_workflow_status(
            &state.persistence.postgres,
            mutation_workflow.id,
            "failed",
            Some(&error.to_string()),
        )
        .await;
        return Err(error).context("failed to rebuild graph after document delete");
    }

    repositories::update_document_mutation_workflow_status(
        &state.persistence.postgres,
        mutation_workflow.id,
        "completed",
        None,
    )
    .await
    .context("failed to mark delete mutation as completed")?;
    repositories::tombstone_document_by_id(
        &state.persistence.postgres,
        document.id,
        "deleted",
        Some("delete"),
        Some("completed"),
    )
    .await
    .context("failed to finalize tombstoned logical document")?;

    Ok(DeletedDocumentMutation { runtime_run: request.runtime_run, mutation_workflow })
}

fn should_publish_delete_reconciliation_activity(runtime_run: &RuntimeIngestionRunRow) -> bool {
    runtime_run.activity_status != "blocked"
        || runtime_run.latest_error_message.as_deref()
            != Some(DELETE_RECONCILIATION_ACTIVITY_MESSAGE)
}

async fn load_mutable_document_context(
    state: &AppState,
    runtime_run: &RuntimeIngestionRunRow,
) -> anyhow::Result<(
    repositories::DocumentRow,
    DocumentRevisionRow,
    Option<repositories::RuntimeExtractedContentRow>,
)> {
    if matches!(runtime_run.status.as_str(), "queued" | "processing") {
        bail!("document mutation conflict: document is still processing");
    }
    let document_id = runtime_run.document_id.with_context(|| {
        format!("runtime ingestion run {} has no logical document yet", runtime_run.id)
    })?;
    let document = repositories::get_document_by_id(&state.persistence.postgres, document_id)
        .await
        .context("failed to load logical document for mutation")?
        .with_context(|| format!("logical document {document_id} not found"))?;
    if document.deleted_at.is_some() {
        bail!("document mutation conflict: logical document has been deleted");
    }
    if repositories::get_active_document_mutation_workflow_by_document_id(
        &state.persistence.postgres,
        document_id,
    )
    .await
    .context("failed to check active document mutation workflow")?
    .is_some()
    {
        bail!("document mutation conflict: another mutation is already active");
    }
    let active_revision_id = document
        .current_revision_id
        .with_context(|| format!("logical document {document_id} has no active revision"))?;
    let active_revision =
        repositories::get_document_revision_by_id(&state.persistence.postgres, active_revision_id)
            .await
            .context("failed to load active document revision")?
            .with_context(|| format!("active document revision {active_revision_id} not found"))?;
    let extracted_content = repositories::get_runtime_extracted_content_by_run(
        &state.persistence.postgres,
        runtime_run.id,
    )
    .await
    .context("failed to load runtime extracted content")?;

    Ok((document, active_revision, extracted_content))
}

fn truncate_excerpt(value: &str) -> String {
    const LIMIT: usize = 280;
    let trimmed = value.trim();
    if trimmed.chars().count() <= LIMIT {
        return trimmed.to_string();
    }
    trimmed.chars().take(LIMIT).collect()
}

fn sha256_hex(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    hex::encode(hasher.finalize())
}
