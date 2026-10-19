use std::time::Instant;

use anyhow::Context;
use sha2::{Digest, Sha256};
use tracing::warn;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    infra::knowledge_plane::{IngestAttemptMutationOutcome, IngestRevisionWriteAuthority},
    infra::repositories::{content_document_repository, content_revision_repository},
    services::content::service::preparation_fence_text,
    services::ingest::service::{
        INGEST_STAGE_CHUNK_CONTENT, INGEST_STAGE_EXTRACT_CONTENT,
        INGEST_STAGE_EXTRACT_TECHNICAL_FACTS, INGEST_STAGE_PREPARE_STRUCTURE,
        RecordStageEventCommand,
    },
    shared::extraction::file_extract::FileExtractionPlan,
};

use super::{
    attempt::IngestStageContext,
    errors::{ContentExtractionError, JobLeaseLost, map_stage_error},
    extraction::{
        ExtractedContent, resolve_ingest_content,
        sync_resumable_pdf_extract_stage_progress_from_units,
    },
    limits::acquire_heavy_revision_pipeline_slot,
};

pub(super) struct PreparedContent {
    pub(super) revision: crate::infra::knowledge_rows::KnowledgeRevisionRow,
}

pub(super) fn vision_billing_usage_items(usage_json: &serde_json::Value) -> Vec<serde_json::Value> {
    usage_json
        .get("embedded_picture_ocr_usage")
        .and_then(serde_json::Value::as_array)
        .filter(|items| !items.is_empty())
        .map_or_else(|| vec![usage_json.clone()], Clone::clone)
}

pub(super) fn extract_artifact_text_and_checksum(plan: &FileExtractionPlan) -> (String, String) {
    let text = preparation_fence_text(plan);
    let checksum = hex::encode(Sha256::digest(text.as_bytes()));
    (text, checksum)
}

async fn load_or_self_heal_knowledge_revision(
    state: &AppState,
    authority: IngestRevisionWriteAuthority,
    revision_row: &content_revision_repository::ContentRevisionRow,
) -> anyhow::Result<IngestAttemptMutationOutcome<crate::infra::knowledge_rows::KnowledgeRevisionRow>>
{
    let revision_id = revision_row.id;
    let document = content_document_repository::get_document_by_id(
        &state.persistence.postgres,
        revision_row.document_id,
    )
    .await
    .context("failed to load document for self-heal")?
    .with_context(|| format!("document {} not found in Postgres", revision_row.document_id))?;
    let document_projection = crate::infra::knowledge_rows::KnowledgeDocumentRow {
        document_id: document.id,
        workspace_id: document.workspace_id,
        library_id: document.library_id,
        external_key: document.external_key.clone(),
        file_name: Some(document.external_key),
        title: None,
        source_uri: None,
        document_hint: None,
        document_state: document.document_state,
        active_revision_id: None,
        readable_revision_id: None,
        latest_revision_no: None,
        parent_document_id: document.parent_document_id,
        document_role: document.document_role,
        created_at: document.created_at,
        updated_at: document.created_at,
        deleted_at: document.deleted_at,
    };
    let revision_projection = crate::infra::knowledge_rows::KnowledgeRevisionRow {
        revision_id,
        workspace_id: revision_row.workspace_id,
        library_id: revision_row.library_id,
        document_id: revision_row.document_id,
        revision_number: i64::from(revision_row.revision_number),
        revision_state: "accepted".to_string(),
        revision_kind: revision_row.content_source_kind.clone(),
        storage_ref: revision_row.storage_key.clone(),
        source_uri: revision_row.source_uri.clone(),
        document_hint: revision_row.document_hint.clone(),
        mime_type: revision_row.mime_type.clone(),
        checksum: revision_row.checksum.clone(),
        title: revision_row.title.clone(),
        byte_size: revision_row.byte_size,
        normalized_text: None,
        text_checksum: None,
        image_checksum: None,
        text_state: "accepted".to_string(),
        vector_state: "accepted".to_string(),
        graph_state: "accepted".to_string(),
        text_readable_at: None,
        vector_ready_at: None,
        graph_ready_at: None,
        superseded_by_revision_id: None,
        created_at: revision_row.created_at,
    };
    tracing::debug!(%revision_id, "ensuring attempt-fenced knowledge projection");
    state
        .document_store
        .ensure_ingest_revision_projection_fenced(
            authority,
            &document_projection,
            &revision_projection,
        )
        .await
        .with_context(|| format!("failed to ensure knowledge revision {revision_id} in store"))
}

async fn record_extract_content_failure(
    state: &AppState,
    authority: IngestRevisionWriteAuthority,
    error: &ContentExtractionError,
    elapsed_ms: i64,
) -> IngestAttemptMutationOutcome<()> {
    let extract_outcome = mark_extract_content_failed(state, authority).await;
    if matches!(extract_outcome, IngestAttemptMutationOutcome::AuthorityLost) {
        return IngestAttemptMutationOutcome::AuthorityLost;
    }
    if let Err(event_error) = state
        .application_services
        .ingest
        .record_stage_event(
            state,
            RecordStageEventCommand {
                attempt_id: authority.attempt_id,
                stage_name: INGEST_STAGE_EXTRACT_CONTENT.to_string(),
                stage_state: "failed".to_string(),
                message: Some(error.to_string()),
                details_json: serde_json::json!({ "failureCode": error.failure_code }),
                provider_kind: None,
                model_name: None,
                prompt_tokens: None,
                completion_tokens: None,
                total_tokens: None,
                cached_tokens: None,
                estimated_cost: None,
                currency_code: None,
                elapsed_ms: Some(elapsed_ms),
            },
        )
        .await
    {
        tracing::warn!(attempt_id = %authority.attempt_id, ?event_error, "failed to record extract_content stage failure event");
    }
    IngestAttemptMutationOutcome::Applied(())
}

pub(super) async fn prepare_ingest_content(
    context: &IngestStageContext<'_>,
) -> anyhow::Result<PreparedContent> {
    let IngestStageContext { state, job, attempt_id, revision_id, cancellation, .. } = *context;
    let authority = IngestRevisionWriteAuthority {
        workspace_id: job.workspace_id,
        library_id: job.library_id,
        attempt_id,
        revision_id,
    };
    // --- Stage: extract_content -----------------------------------------------
    cancellation.check(job.id)?;
    state
        .application_services
        .ingest
        .record_stage_event(
            state,
            RecordStageEventCommand::for_stage(
                attempt_id,
                INGEST_STAGE_EXTRACT_CONTENT,
                "started",
                None,
                serde_json::json!({}),
                None,
            ),
        )
        .await
        .context("failed to record extract_content started stage event")?;

    let extract_content_start = Instant::now();

    // Read revision metadata from Postgres — the authoritative source. If
    // the knowledge-plane row has not been materialized yet, populate it here so
    // the pipeline has a consistent view.
    let revision_row =
        content_revision_repository::get_revision_by_id(&state.persistence.postgres, revision_id)
            .await
            .context("failed to load knowledge revision from postgres")?
            .with_context(|| format!("knowledge revision {revision_id} not found in postgres"))?;
    sync_extract_progress_before_admission(context, authority).await?;
    let heavy_revision_pipeline_permit = acquire_revision_admission(context, &revision_row).await?;
    let heavy_revision_pipeline_limited = heavy_revision_pipeline_permit.is_some();

    let revision =
        match load_or_self_heal_knowledge_revision(state, authority, &revision_row).await? {
            IngestAttemptMutationOutcome::Applied(revision) => revision,
            IngestAttemptMutationOutcome::AuthorityLost => {
                return Err(anyhow::Error::new(JobLeaseLost { job_id: job.id }));
            }
        };

    let mut extracted_content =
        resolve_revision_content(context, authority, &revision, extract_content_start).await?;
    let (normalized_text, text_checksum) =
        extract_artifact_text_and_checksum(&extracted_content.extraction_plan);

    let extract_outcome = state
        .application_services
        .knowledge
        .set_ingest_revision_extract_state(
            state,
            authority,
            crate::services::knowledge::service::IngestRevisionExtractContent {
                extract_state: "ready",
                normalized_text: Some(&normalized_text),
                text_checksum: Some(&text_checksum),
                image_checksum: extracted_content.extraction_plan.image_checksum.as_deref(),
            },
        )
        .await
        .context("failed to persist extracted content")?;
    if matches!(extract_outcome, IngestAttemptMutationOutcome::AuthorityLost) {
        return Err(anyhow::Error::new(JobLeaseLost { job_id: job.id }));
    }

    let extract_content_elapsed_ms = Some(extract_content_start.elapsed().as_millis() as i64);

    capture_extraction_billing(context, &extracted_content).await;

    state
        .application_services
        .ingest
        .record_stage_event(
            state,
            RecordStageEventCommand {
                attempt_id,
                stage_name: INGEST_STAGE_EXTRACT_CONTENT.to_string(),
                stage_state: "completed".to_string(),
                message: Some("content extracted".to_string()),
                details_json: extracted_content.stage_details.take(),
                provider_kind: extracted_content.provider_kind.clone(),
                model_name: extracted_content.model_name.clone(),
                prompt_tokens: extracted_content
                    .usage_json
                    .get("prompt_tokens")
                    .and_then(serde_json::Value::as_i64)
                    .map(|v| v as i32),
                completion_tokens: extracted_content
                    .usage_json
                    .get("completion_tokens")
                    .and_then(serde_json::Value::as_i64)
                    .map(|v| v as i32),
                total_tokens: extracted_content
                    .usage_json
                    .get("total_tokens")
                    .and_then(serde_json::Value::as_i64)
                    .map(|v| v as i32),
                cached_tokens: None,
                estimated_cost: None,
                currency_code: None,
                elapsed_ms: extract_content_elapsed_ms,
            },
        )
        .await
        .context("failed to record extract_content stage event")?;

    prepare_revision_structure(context, authority, &revision, &extracted_content.extraction_plan)
        .await?;
    drop(extracted_content.extraction_plan);
    if heavy_revision_pipeline_limited {
        drop(heavy_revision_pipeline_permit);
        log_revision_admission_released(revision_id);
    }
    Ok(PreparedContent { revision })
}

async fn prepare_revision_structure(
    context: &IngestStageContext<'_>,
    authority: IngestRevisionWriteAuthority,
    revision: &crate::infra::knowledge_rows::KnowledgeRevisionRow,
    extraction_plan: &FileExtractionPlan,
) -> anyhow::Result<()> {
    let IngestStageContext { state, job, attempt_id, revision_id, cancellation, .. } = *context;
    // --- Stage: prepare_structure / chunk_content / extract_technical_facts ---
    cancellation.check(job.id)?;
    state
        .application_services
        .ingest
        .record_stage_event(
            state,
            RecordStageEventCommand::for_stage(
                attempt_id,
                INGEST_STAGE_PREPARE_STRUCTURE,
                "started",
                Some("building structured revision from normalized text".to_string()),
                serde_json::json!({
                    "libraryId": revision.library_id,
                    "revisionId": revision_id,
                }),
                None,
            ),
        )
        .await
        .context("failed to record prepare_structure start stage event")?;

    let prepare_structure_start = Instant::now();
    let preparation = match state
        .application_services
        .content
        .prepare_and_persist_revision_structure(
            state,
            authority,
            extraction_plan,
            cancellation.token,
        )
        .await
    {
        Ok(IngestAttemptMutationOutcome::Applied(preparation)) => preparation,
        Ok(IngestAttemptMutationOutcome::AuthorityLost) => {
            return Err(anyhow::Error::new(JobLeaseLost { job_id: job.id }));
        }
        Err(error) => {
            let mapped_error = map_stage_error(
                error.into(),
                cancellation.user_requested,
                cancellation.operator_pause_requested,
                cancellation.lease_lost_requested,
                job.id,
                "failed to prepare and persist structured revision",
            );
            let failure_message = format!("{mapped_error:#}");
            let elapsed_ms = Some(prepare_structure_start.elapsed().as_millis() as i64);
            state
                .application_services
                .ingest
                .record_stage_event(
                    state,
                    RecordStageEventCommand {
                        attempt_id,
                        stage_name: INGEST_STAGE_PREPARE_STRUCTURE.to_string(),
                        stage_state: "failed".to_string(),
                        message: Some("structured revision preparation failed".to_string()),
                        details_json: serde_json::json!({
                            "revisionId": revision_id,
                            "error": failure_message,
                        }),
                        provider_kind: None,
                        model_name: None,
                        prompt_tokens: None,
                        completion_tokens: None,
                        total_tokens: None,
                        cached_tokens: None,
                        estimated_cost: None,
                        currency_code: None,
                        elapsed_ms,
                    },
                )
                .await
                .context("failed to record prepare_structure failure stage event")?;
            return Err(mapped_error);
        }
    };

    let prepare_structure_elapsed_ms = Some(preparation.prepare_structure_elapsed_ms);
    let chunk_content_elapsed_ms = Some(preparation.chunk_content_elapsed_ms);
    let extract_technical_facts_elapsed_ms = Some(preparation.extract_technical_facts_elapsed_ms);

    state
        .application_services
        .ingest
        .record_stage_event(
            state,
            RecordStageEventCommand {
                attempt_id,
                stage_name: INGEST_STAGE_PREPARE_STRUCTURE.to_string(),
                stage_state: "completed".to_string(),
                message: Some("structured revision prepared".to_string()),
                details_json: serde_json::json!({
                    "revisionId": revision_id,
                    "normalizationProfile": preparation.normalization_profile,
                    "blockCount": preparation.prepared_revision.block_count,
                    "chunkCount": preparation.chunk_count,
                }),
                provider_kind: None,
                model_name: None,
                prompt_tokens: None,
                completion_tokens: None,
                total_tokens: None,
                cached_tokens: None,
                estimated_cost: None,
                currency_code: None,
                elapsed_ms: prepare_structure_elapsed_ms,
            },
        )
        .await
        .context("failed to record prepare_structure stage event")?;
    state
        .application_services
        .ingest
        .record_stage_event(
            state,
            RecordStageEventCommand::for_stage(
                attempt_id,
                INGEST_STAGE_CHUNK_CONTENT,
                "started",
                None,
                serde_json::json!({}),
                None,
            ),
        )
        .await
        .context("failed to record chunk_content started stage event")?;
    state
        .application_services
        .ingest
        .record_stage_event(
            state,
            RecordStageEventCommand {
                attempt_id,
                stage_name: INGEST_STAGE_CHUNK_CONTENT.to_string(),
                stage_state: "completed".to_string(),
                message: Some("content chunks persisted".to_string()),
                details_json: serde_json::json!({
                    "chunkCount": preparation.chunk_count,
                }),
                provider_kind: None,
                model_name: None,
                prompt_tokens: None,
                completion_tokens: None,
                total_tokens: None,
                cached_tokens: None,
                estimated_cost: None,
                currency_code: None,
                elapsed_ms: chunk_content_elapsed_ms,
            },
        )
        .await
        .context("failed to record chunk_content stage event")?;
    state
        .application_services
        .ingest
        .record_stage_event(
            state,
            RecordStageEventCommand::for_stage(
                attempt_id,
                INGEST_STAGE_EXTRACT_TECHNICAL_FACTS,
                "started",
                None,
                serde_json::json!({}),
                None,
            ),
        )
        .await
        .context("failed to record extract_technical_facts started stage event")?;
    state
        .application_services
        .ingest
        .record_stage_event(
            state,
            RecordStageEventCommand {
                attempt_id,
                stage_name: INGEST_STAGE_EXTRACT_TECHNICAL_FACTS.to_string(),
                stage_state: "completed".to_string(),
                message: Some("technical facts extracted from structured revision".to_string()),
                details_json: serde_json::json!({
                    "technicalFactCount": preparation.technical_fact_count,
                    "technicalConflictCount": preparation.technical_conflict_count,
                }),
                provider_kind: None,
                model_name: None,
                prompt_tokens: None,
                completion_tokens: None,
                total_tokens: None,
                cached_tokens: None,
                estimated_cost: None,
                currency_code: None,
                elapsed_ms: extract_technical_facts_elapsed_ms,
            },
        )
        .await
        .context("failed to record extract_technical_facts stage event")?;
    Ok(())
}

async fn mark_extract_content_failed(
    state: &AppState,
    authority: IngestRevisionWriteAuthority,
) -> IngestAttemptMutationOutcome<()> {
    match state
        .application_services
        .knowledge
        .set_ingest_revision_extract_state(
            state,
            authority,
            crate::services::knowledge::service::IngestRevisionExtractContent {
                extract_state: "failed",
                normalized_text: None,
                text_checksum: None,
                image_checksum: None,
            },
        )
        .await
    {
        Ok(IngestAttemptMutationOutcome::Applied(_)) => IngestAttemptMutationOutcome::Applied(()),
        Ok(IngestAttemptMutationOutcome::AuthorityLost) => {
            IngestAttemptMutationOutcome::AuthorityLost
        }
        Err(update_error) => {
            tracing::warn!(revision_id = %authority.revision_id, ?update_error, "failed to set revision extract state to failed");
            IngestAttemptMutationOutcome::Applied(())
        }
    }
}

async fn sync_extract_progress_before_admission(
    context: &IngestStageContext<'_>,
    authority: IngestRevisionWriteAuthority,
) -> anyhow::Result<()> {
    match sync_resumable_pdf_extract_stage_progress_from_units(
        context.state,
        authority,
        context.cancellation.token,
    )
    .await
    {
        Ok(IngestAttemptMutationOutcome::Applied(())) => Ok(()),
        Ok(IngestAttemptMutationOutcome::AuthorityLost) => {
            Err(anyhow::Error::new(JobLeaseLost { job_id: context.job.id }))
        }
        Err(error) => {
            context.cancellation.check(context.job.id)?;
            warn!(attempt_id = %context.attempt_id, revision_id = %context.revision_id, ?error,
                "failed to sync resumable extract_content progress before heavy revision wait");
            Ok(())
        }
    }
}

async fn acquire_revision_admission(
    context: &IngestStageContext<'_>,
    revision_row: &content_revision_repository::ContentRevisionRow,
) -> anyhow::Result<Option<tokio::sync::OwnedSemaphorePermit>> {
    // Keep biased cancellation ahead of bounded admission, including when both are ready.
    let permit = tokio::select! {
        biased;
        () = context.cancellation.token.cancelled() => return Err(context.cancellation.error(context.job.id)),
        permit = acquire_heavy_revision_pipeline_slot(revision_row) => permit?,
    };
    if permit.is_some() {
        log_revision_admission_acquired(context.revision_id, revision_row.byte_size);
    }
    Ok(permit)
}

fn log_revision_admission_acquired(revision_id: Uuid, byte_size: i64) {
    tracing::info!(%revision_id, byte_size, "heavy revision pipeline slot acquired");
}

fn log_revision_admission_released(revision_id: Uuid) {
    tracing::info!(%revision_id, "heavy revision pipeline slot released before provider-bound stages");
}

async fn resolve_revision_content(
    context: &IngestStageContext<'_>,
    authority: IngestRevisionWriteAuthority,
    revision: &crate::infra::knowledge_rows::KnowledgeRevisionRow,
    started_at: Instant,
) -> anyhow::Result<ExtractedContent> {
    match Box::pin(resolve_ingest_content(
        context.state,
        context.job,
        authority,
        revision,
        context.cancellation.token,
    ))
    .await
    {
        Ok(IngestAttemptMutationOutcome::Applied(content)) => Ok(content),
        Ok(IngestAttemptMutationOutcome::AuthorityLost) => {
            Err(anyhow::Error::new(JobLeaseLost { job_id: context.job.id }))
        }
        Err(error) => {
            context.cancellation.check(context.job.id)?;
            let failure_outcome = record_extract_content_failure(
                context.state,
                authority,
                &error,
                started_at.elapsed().as_millis() as i64,
            )
            .await;
            if matches!(failure_outcome, IngestAttemptMutationOutcome::AuthorityLost) {
                return Err(anyhow::Error::new(JobLeaseLost { job_id: context.job.id }));
            }
            Err(anyhow::Error::new(error))
        }
    }
}

async fn capture_extraction_billing(
    context: &IngestStageContext<'_>,
    extracted_content: &ExtractedContent,
) {
    let Some(provider_kind) = extracted_content.provider_kind.clone() else {
        return;
    };
    let model_name = extracted_content.model_name.clone().unwrap_or_default();
    for usage_json in vision_billing_usage_items(&extracted_content.usage_json) {
        if let Err(e) = context
            .state
            .application_services
            .billing
            .capture_ingest_attempt(
                context.state,
                crate::services::ops::billing::CaptureIngestAttemptBillingCommand {
                    workspace_id: context.job.workspace_id,
                    library_id: context.job.library_id,
                    attempt_id: context.attempt_id,
                    binding_id: None,
                    provider_kind: provider_kind.clone(),
                    model_name: model_name.clone(),
                    call_kind: "vision_extract".to_string(),
                    usage_json,
                },
            )
            .await
        {
            warn!(worker_id = %context.worker_id, job_id = %context.job.id, ?e, "document-understanding billing capture failed");
        }
    }
}
