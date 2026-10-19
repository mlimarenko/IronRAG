use std::{collections::HashMap, future::Future, sync::Arc};

use anyhow::Context as _;
use chrono::Utc;
use futures::{StreamExt, stream};
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    infra::{
        knowledge_plane::{IngestAttemptMutationOutcome, IngestRevisionWriteAuthority},
        knowledge_rows::KnowledgeRevisionRow,
        repositories::ingest_repository,
    },
    integrations::docling,
    shared::extraction::{
        ExtractionOutput, ExtractionSourceMetadata, build_text_layout_from_content,
        document_summary::{DocumentSummaryBlock, build_document_summary_from_blocks},
        file_extract::{
            ExtractionFailureClass, build_inline_text_extraction_plan, docling_failure_class,
        },
    },
};

use super::super::service::{INGEST_STAGE_EXTRACT_CONTENT, RecordStageUnitProgressCommand};
use super::errors::ContentExtractionError;

const DEFAULT_PDF_EXTRACT_STREAM_WINDOW_PAGES: u32 = 40;

pub(super) struct ExtractedContent {
    pub(super) extraction_plan: crate::shared::extraction::file_extract::FileExtractionPlan,
    pub(super) stage_details: serde_json::Value,
    pub(super) provider_kind: Option<String>,
    pub(super) model_name: Option<String>,
    pub(super) usage_json: serde_json::Value,
}

fn persisted_revision_file_name(revision: &KnowledgeRevisionRow) -> String {
    let source_name = revision
        .source_uri
        .as_deref()
        .and_then(|value| value.split_once("://").map(|(_, rest)| rest).or(Some(value)))
        .and_then(|value| value.rsplit('/').next())
        .map(str::trim)
        .filter(|value| !value.is_empty() && *value != "inline")
        .map(str::to_string);
    source_name
        .or_else(|| {
            revision
                .title
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        })
        .unwrap_or_else(|| format!("revision-{}", revision.revision_id))
}

pub(super) async fn resolve_ingest_content(
    state: &AppState,
    job: &ingest_repository::IngestJobRow,
    authority: IngestRevisionWriteAuthority,
    revision: &KnowledgeRevisionRow,
    cancellation_token: &CancellationToken,
) -> Result<IngestAttemptMutationOutcome<ExtractedContent>, ContentExtractionError> {
    ensure_extraction_not_cancelled(cancellation_token, revision.revision_id)?;
    let storage_ref =
        match revision.storage_ref.as_deref().map(str::trim).filter(|value| !value.is_empty()) {
            Some(storage_ref) => storage_ref.to_string(),
            None => Box::pin(await_with_extraction_cancellation(
                cancellation_token,
                revision.revision_id,
                state
                    .application_services
                    .content
                    .resolve_revision_storage_key(state, revision.revision_id),
            ))
            .await?
            .map_err(|_| {
                ContentExtractionError::missing_stored_source(job.id, revision.revision_id)
            })?
            .unwrap_or_default(),
        };
    if !storage_ref.is_empty() {
        let stored_bytes = await_with_extraction_cancellation(
            cancellation_token,
            revision.revision_id,
            state.content_storage.read_revision_source(&storage_ref),
        )
        .await?
        .map_err(|error| ContentExtractionError::stored_source_read(&storage_ref, error))?;
        let file_name = persisted_revision_file_name(revision);
        if is_pdf_revision(revision, &file_name) {
            return resolve_resumable_pdf_extract_content(
                state,
                authority,
                revision,
                PdfExtractSource {
                    file_name: &file_name,
                    storage_ref: &storage_ref,
                    stored_bytes: &stored_bytes,
                },
                cancellation_token,
            )
            .await;
        }
        let plan = await_with_extraction_cancellation(
            cancellation_token,
            revision.revision_id,
            state.application_services.content.build_runtime_extraction_plan(
                state,
                revision.library_id,
                &file_name,
                Some(revision.mime_type.as_str()),
                &stored_bytes,
            ),
        )
        .await?
        .map_err(|rejection| ContentExtractionError::from_upload_rejection(&rejection))?;
        // Move `plan` into the result rather than cloning: for a large PDF
        // with hundreds of images the payload can be 150+ MB, and cloning
        // multiplies peak RSS by the library job parallelism limit.
        let content_char_count = plan.normalized_text.as_deref().unwrap_or("").chars().count();
        let stage_details = serde_json::json!({
            "contentLength": content_char_count,
            "fileKind": plan.file_kind.as_str(),
            "recognition": plan.source_map.get("recognition").cloned().unwrap_or_else(|| serde_json::json!({})),
            "warningCount": plan.extraction_warnings.len(),
            "lineCount": plan.source_format_metadata.line_count,
            "pageCount": plan.source_format_metadata.page_count,
            "normalizationProfile": plan.normalization_profile,
            "source": "content_storage",
            "storageRef": storage_ref,
        });
        return Ok(IngestAttemptMutationOutcome::Applied(ExtractedContent {
            provider_kind: plan.provider_kind.clone(),
            model_name: plan.model_name.clone(),
            usage_json: plan.usage_json.clone(),
            extraction_plan: plan,
            stage_details,
        }));
    }

    if let Some(text) = revision
        .normalized_text
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
    {
        let extraction_plan = build_inline_text_extraction_plan(&text);
        return Ok(IngestAttemptMutationOutcome::Applied(ExtractedContent {
            provider_kind: extraction_plan.provider_kind.clone(),
            model_name: extraction_plan.model_name.clone(),
            usage_json: extraction_plan.usage_json.clone(),
            extraction_plan,
            stage_details: serde_json::json!({
                "contentLength": text.chars().count(),
                "source": "knowledge_revision",
            }),
        }));
    }

    Err(ContentExtractionError::missing_stored_source(job.id, revision.revision_id))
}

fn ensure_extraction_not_cancelled(
    cancellation_token: &CancellationToken,
    revision_id: Uuid,
) -> Result<(), ContentExtractionError> {
    if cancellation_token.is_cancelled() {
        Err(ContentExtractionError::extraction_failed(
            "extract_canceled",
            format!("extraction canceled for revision {revision_id}"),
        ))
    } else {
        Ok(())
    }
}

pub(super) async fn await_with_extraction_cancellation<T, F>(
    cancellation_token: &CancellationToken,
    revision_id: Uuid,
    future: F,
) -> Result<T, ContentExtractionError>
where
    F: Future<Output = T>,
{
    tokio::select! {
        biased;
        () = cancellation_token.cancelled() => {
            Err(ContentExtractionError::extraction_failed(
                "extract_canceled",
                format!("extraction canceled for revision {revision_id}"),
            ))
        }
        output = future => Ok(output),
    }
}

fn is_pdf_revision(revision: &KnowledgeRevisionRow, file_name: &str) -> bool {
    let mime_type = revision
        .mime_type
        .split(';')
        .next()
        .map(str::trim)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let extension = file_name.rsplit_once('.').map(|(_, extension)| extension.to_ascii_lowercase());
    mime_type == "application/pdf" || matches!(extension.as_deref(), Some("pdf"))
}

#[derive(Clone)]
pub(super) struct PdfExtractUnit {
    pub(super) unit_ordinal: i32,
    pub(super) start_page: u32,
    pub(super) end_page: u32,
    pub(super) range_start: i32,
    pub(super) range_end: i32,
}

#[derive(Clone, Copy)]
pub(super) struct PdfExtractRun {
    pub(super) start_page: u32,
    pub(super) end_page: u32,
}

pub(super) struct PdfExtractUnitMergeFragment {
    pub(super) content_text: String,
    pub(super) warnings: Vec<String>,
    pub(super) provider_kind: Option<String>,
    pub(super) model_name: Option<String>,
    pub(super) usage_json: serde_json::Value,
}

impl PdfExtractUnitMergeFragment {
    pub(super) fn from_output(mut output: ExtractionOutput) -> Self {
        Self {
            content_text: std::mem::take(&mut output.content_text),
            warnings: std::mem::take(&mut output.warnings),
            provider_kind: output.provider_kind.take(),
            model_name: output.model_name.take(),
            usage_json: output.usage_json,
        }
    }
}

pub(super) fn contiguous_pdf_extract_unit_runs(
    units: &[PdfExtractUnit],
    max_pages_per_run: u32,
) -> Vec<PdfExtractRun> {
    let Some(first) = units.first() else {
        return Vec::new();
    };
    let max_pages_per_run = max_pages_per_run.max(1);
    let mut runs = Vec::new();
    let mut run_start = first.start_page;
    let mut run_end = first.end_page;

    for unit in units.iter().skip(1) {
        let would_stay_contiguous = unit.start_page == run_end.saturating_add(1);
        let would_stay_bounded =
            unit.end_page.saturating_sub(run_start).saturating_add(1) <= max_pages_per_run;
        if would_stay_contiguous && would_stay_bounded {
            run_end = unit.end_page;
            continue;
        }
        runs.push(PdfExtractRun { start_page: run_start, end_page: run_end });
        run_start = unit.start_page;
        run_end = unit.end_page;
    }

    runs.push(PdfExtractRun { start_page: run_start, end_page: run_end });
    runs
}

fn pdf_extract_stream_window_pages(batch_size: u32) -> u32 {
    std::env::var("IRONRAG_DOCLING_PAGE_STREAM_WINDOW_PAGES")
        .ok()
        .and_then(|raw| raw.parse::<u32>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_PDF_EXTRACT_STREAM_WINDOW_PAGES)
        .max(batch_size.max(1))
}

type PdfExtractUnitOutput = (i32, PdfExtractUnitMergeFragment);
type PdfExtractUnitPartition = (Vec<PdfExtractUnitOutput>, Vec<PdfExtractUnit>, i32);

fn collect_pdf_extract_units(
    completed_rows: &HashMap<i32, ingest_repository::ContentRevisionIngestUnitRow>,
    page_count: u32,
    batch_size: u32,
) -> Result<PdfExtractUnitPartition, ContentExtractionError> {
    let mut output_units = Vec::new();
    let mut missing_units = Vec::new();
    let mut reused_unit_count = 0_i32;
    for batch_idx in 0..page_count.div_ceil(batch_size) {
        let unit_ordinal = i32::try_from(batch_idx).unwrap_or(i32::MAX) + 1;
        let start_page = batch_idx * batch_size + 1;
        let end_page = ((batch_idx + 1) * batch_size).min(page_count);
        let range_start = i32::try_from(start_page).unwrap_or(i32::MAX);
        let range_end = i32::try_from(end_page).unwrap_or(i32::MAX);
        if let Some(fragment) = completed_rows
            .get(&unit_ordinal)
            .filter(|row| row.range_start == range_start && row.range_end == range_end)
            .and_then(verified_merge_fragment_from_unit)
        {
            output_units.push((unit_ordinal, fragment));
            reused_unit_count += 1;
            continue;
        }
        missing_units.push(PdfExtractUnit {
            unit_ordinal,
            start_page,
            end_page,
            range_start,
            range_end,
        });
    }
    Ok((output_units, missing_units, reused_unit_count))
}

struct PdfExtractSource<'a> {
    file_name: &'a str,
    storage_ref: &'a str,
    stored_bytes: &'a [u8],
}

struct MissingPdfUnits<'a> {
    page_count: u32,
    batch_size: u32,
    stream_window_pages: u32,
    units: &'a [PdfExtractUnit],
}

struct PdfExtractRunInput<'a> {
    state: AppState,
    authority: IngestRevisionWriteAuthority,
    file_name: String,
    mime_type: String,
    stored_bytes: &'a [u8],
    page_count: u32,
    batch_size: u32,
    run: PdfExtractRun,
    expected_units: Arc<HashMap<u32, PdfExtractUnit>>,
    shared_output_units: Arc<tokio::sync::Mutex<Vec<PdfExtractUnitOutput>>>,
    cancellation_token: CancellationToken,
}

async fn extract_missing_pdf_units(
    state: &AppState,
    authority: IngestRevisionWriteAuthority,
    revision: &KnowledgeRevisionRow,
    source: &PdfExtractSource<'_>,
    missing: MissingPdfUnits<'_>,
    cancellation_token: &CancellationToken,
) -> Result<IngestAttemptMutationOutcome<(Vec<PdfExtractUnitOutput>, usize)>, ContentExtractionError>
{
    let PdfExtractSource { file_name, stored_bytes, .. } = *source;
    let MissingPdfUnits { page_count, batch_size, stream_window_pages, units: missing_units } =
        missing;
    let shared_output_units = Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let expected_units: Arc<HashMap<u32, PdfExtractUnit>> =
        Arc::new(missing_units.iter().cloned().map(|unit| (unit.start_page, unit)).collect());
    let runs = contiguous_pdf_extract_unit_runs(missing_units, stream_window_pages);
    let run_parallelism = runs.len().max(1).min(docling::configured_max_concurrency());
    let mut run_results = stream::iter(runs.into_iter().map(|run| {
        extract_pdf_run_streamed(PdfExtractRunInput {
            state: state.clone(),
            authority,
            file_name: file_name.to_string(),
            mime_type: revision.mime_type.clone(),
            stored_bytes,
            page_count,
            batch_size,
            run,
            expected_units: Arc::clone(&expected_units),
            shared_output_units: Arc::clone(&shared_output_units),
            cancellation_token: cancellation_token.clone(),
        })
    }))
    .buffer_unordered(run_parallelism);

    while let Some(result) = run_results.next().await {
        match result? {
            IngestAttemptMutationOutcome::Applied(()) => {}
            IngestAttemptMutationOutcome::AuthorityLost => {
                drop(run_results);
                return Ok(IngestAttemptMutationOutcome::AuthorityLost);
            }
        }
    }
    drop(run_results);
    let output_units = Arc::try_unwrap(shared_output_units)
        .map_err(|_| {
            ContentExtractionError::extraction_failed(
                "extract_unit_collect_failed",
                format!("failed to collect extract units for revision {}", revision.revision_id),
            )
        })?
        .into_inner();
    Ok(IngestAttemptMutationOutcome::Applied((output_units, run_parallelism)))
}

async fn resolve_resumable_pdf_extract_content(
    state: &AppState,
    authority: IngestRevisionWriteAuthority,
    revision: &KnowledgeRevisionRow,
    source: PdfExtractSource<'_>,
    cancellation_token: &CancellationToken,
) -> Result<IngestAttemptMutationOutcome<ExtractedContent>, ContentExtractionError> {
    let PdfExtractSource { file_name, storage_ref, stored_bytes } = source;
    let file_size_bytes = u64::try_from(stored_bytes.len()).unwrap_or(u64::MAX);
    let page_count = await_with_extraction_cancellation(
        cancellation_token,
        revision.revision_id,
        docling::extract_pdf_page_count(
            docling::DoclingSource {
                file_name: Some(file_name),
                mime_type: Some(revision.mime_type.as_str()),
                source_format: "pdf",
            },
            stored_bytes,
        ),
    )
    .await?
    .map_err(|error| {
        pdf_docling_failure(
            &error,
            "pdf_page_count_failed",
            format!("failed to read pdf page count for revision {}: {error}", revision.revision_id),
        )
    })?
    .filter(|value| *value > 0)
    .ok_or_else(|| {
        ContentExtractionError::extraction_failed(
            "pdf_page_count_unavailable",
            format!("failed to read pdf page count for revision {}", revision.revision_id),
        )
    })?;

    let batch_size = docling::configured_page_batch_size();
    let existing_rows = ingest_repository::list_content_revision_ingest_units_fenced(
        &state.persistence.postgres,
        authority,
        INGEST_STAGE_EXTRACT_CONTENT,
    )
    .await
    .map_err(|error| {
        ContentExtractionError::extraction_failed(
            "extract_unit_read_failed",
            format!("failed to load extract units for revision {}: {error}", revision.revision_id),
        )
    })?;
    let existing_rows = match existing_rows {
        IngestAttemptMutationOutcome::Applied(rows) => rows,
        IngestAttemptMutationOutcome::AuthorityLost => {
            return Ok(IngestAttemptMutationOutcome::AuthorityLost);
        }
    };
    let completed_rows: HashMap<i32, ingest_repository::ContentRevisionIngestUnitRow> =
        existing_rows
            .into_iter()
            .filter(|row| row.unit_state == "completed" && row.unit_kind == "pdf_page_range")
            .map(|row| (row.unit_ordinal, row))
            .collect();
    let batch_count = page_count.div_ceil(batch_size);
    let (mut output_units, missing_units, reused_unit_count) =
        collect_pdf_extract_units(&completed_rows, page_count, batch_size)?;
    let progress_outcome =
        sync_pdf_extract_stage_progress(state, authority, page_count, cancellation_token)
            .await
            .map_err(|error| {
                ContentExtractionError::extraction_failed(
                    "extract_progress_sync_failed",
                    format!(
                        "failed to sync extract progress for revision {}: {error}",
                        revision.revision_id
                    ),
                )
            })?;
    if matches!(progress_outcome, IngestAttemptMutationOutcome::AuthorityLost) {
        return Ok(IngestAttemptMutationOutcome::AuthorityLost);
    }

    let extracted_unit_count = i32::try_from(missing_units.len()).unwrap_or(i32::MAX);
    let stream_window_pages = pdf_extract_stream_window_pages(batch_size);
    let run_parallelism = if missing_units.is_empty() {
        0
    } else {
        let extraction_outcome = extract_missing_pdf_units(
            state,
            authority,
            revision,
            &source,
            MissingPdfUnits { page_count, batch_size, stream_window_pages, units: &missing_units },
            cancellation_token,
        )
        .await?;
        match extraction_outcome {
            IngestAttemptMutationOutcome::Applied((extracted_outputs, parallelism)) => {
                output_units.extend(extracted_outputs);
                parallelism
            }
            IngestAttemptMutationOutcome::AuthorityLost => {
                return Ok(IngestAttemptMutationOutcome::AuthorityLost);
            }
        }
    };

    output_units.sort_by_key(|(unit_ordinal, _)| *unit_ordinal);
    let outputs = output_units.into_iter().map(|(_, output)| output).collect();
    let merged_output = merge_pdf_unit_outputs(outputs, page_count, file_name, &revision.mime_type);
    let plan = await_with_extraction_cancellation(
        cancellation_token,
        revision.revision_id,
        state.application_services.content.build_runtime_pdf_docling_extraction_plan_from_output(
            state,
            crate::services::content::service::DoclingPdfExtractionPlanInput {
                library_id: revision.library_id,
                file_name,
                mime_type: Some(revision.mime_type.as_str()),
                file_size_bytes,
                output: merged_output,
            },
        ),
    )
    .await?
    .map_err(|rejection| ContentExtractionError::from_upload_rejection(&rejection))?;

    let content_char_count = plan.normalized_text.as_deref().unwrap_or("").chars().count();
    let stage_details = serde_json::json!({
        "contentLength": content_char_count,
        "fileKind": plan.file_kind.as_str(),
        "recognition": plan.source_map.get("recognition").cloned().unwrap_or_else(|| serde_json::json!({})),
        "warningCount": plan.extraction_warnings.len(),
        "lineCount": plan.source_format_metadata.line_count,
        "pageCount": plan.source_format_metadata.page_count,
        "normalizationProfile": plan.normalization_profile,
        "source": "content_storage",
        "storageRef": storage_ref,
        "pageBatchSize": batch_size,
        "pageStreamWindowPages": stream_window_pages,
        "pageStreamWindowParallelism": run_parallelism,
        "extractUnitCount": batch_count,
        "reusedExtractUnitCount": reused_unit_count,
        "newExtractUnitCount": extracted_unit_count,
    });
    Ok(IngestAttemptMutationOutcome::Applied(ExtractedContent {
        provider_kind: plan.provider_kind.clone(),
        model_name: plan.model_name.clone(),
        usage_json: plan.usage_json.clone(),
        extraction_plan: plan,
        stage_details,
    }))
}

#[derive(Debug)]
enum PdfExtractStreamControl {
    Cancelled,
    AuthorityLost,
}

impl std::fmt::Display for PdfExtractStreamControl {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cancelled => formatter.write_str("pdf extraction canceled"),
            Self::AuthorityLost => formatter.write_str("pdf extraction authority lost"),
        }
    }
}

impl std::error::Error for PdfExtractStreamControl {}

async fn extract_pdf_run_streamed(
    input: PdfExtractRunInput<'_>,
) -> Result<IngestAttemptMutationOutcome<()>, ContentExtractionError> {
    let PdfExtractRunInput {
        state,
        authority,
        file_name,
        mime_type,
        stored_bytes,
        page_count,
        batch_size,
        run,
        expected_units,
        shared_output_units,
        cancellation_token,
    } = input;
    let revision_id = authority.revision_id;
    let library_id = authority.library_id;
    let callback_cancellation = cancellation_token.clone();
    let stream_result = await_with_extraction_cancellation(
        &cancellation_token,
        revision_id,
        Box::pin(docling::extract_pdf_page_ranges_streamed(
        docling::DoclingSource {
            file_name: Some(&file_name),
            mime_type: Some(mime_type.as_str()),
            source_format: "pdf",
        },
        stored_bytes,
        run.start_page,
        run.end_page,
        batch_size,
        move |batch| {
            let shared_output_units = Arc::clone(&shared_output_units);
            let expected_units = Arc::clone(&expected_units);
            let state = state.clone();
            let cancellation_token = callback_cancellation.clone();
            async move {
                if cancellation_token.is_cancelled() {
                    return Err(anyhow::Error::new(PdfExtractStreamControl::Cancelled));
                }
                let unit = expected_units
                    .get(&batch.start_page)
                    .filter(|unit| unit.end_page == batch.end_page)
                    .cloned()
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "docling emitted unexpected page range {}-{} for revision {}",
                            batch.start_page,
                            batch.end_page,
                            revision_id
                        )
                    })?;
                let mut output = batch.output;
                tokio::select! {
                    biased;
                    () = cancellation_token.cancelled() => {
                        return Err(anyhow::Error::new(PdfExtractStreamControl::Cancelled));
                    }
                    result = state.application_services.content.augment_runtime_docling_output(
                        &state,
                        library_id,
                        &mut output,
                    ) => result,
                }
                    .map_err(|error| {
                        anyhow::anyhow!(
                            "failed to augment docling output for revision {revision_id}: {error}"
                        )
                    })?;
                let content_checksum = sha256_text(&output.content_text);
                let artifact = ingest_repository::ContentRevisionIngestUnitCompletedArtifact {
                    stage_name: INGEST_STAGE_EXTRACT_CONTENT.to_string(),
                    unit_ordinal: unit.unit_ordinal,
                    unit_kind: "pdf_page_range".to_string(),
                    range_start: unit.range_start,
                    range_end: unit.range_end,
                    content_text: Some(output.content_text.clone()),
                    structure_hints_json: Some(serialize_json(
                        &output.structure_hints,
                        "extraction structure hints",
                    )?),
                    source_metadata_json: Some(serialize_json(
                        &output.source_metadata,
                        "extraction source metadata",
                    )?),
                    source_map_json: Some(output.source_map.clone()),
                    warnings_json: serialize_json(&output.warnings, "extraction warnings")?,
                    usage_json: output.usage_json.clone(),
                    provider_kind: output.provider_kind.clone(),
                    model_name: output.model_name.clone(),
                    content_checksum: Some(content_checksum),
                    details_json: serde_json::json!({
                        "pageCount": page_count,
                        "pageBatchSize": batch_size,
                        "streamed": true,
                    }),
                    elapsed_ms: Some(batch.elapsed_ms),
                    started_at: Some(Utc::now()),
                };
                let persisted = tokio::select! {
                    biased;
                    () = cancellation_token.cancelled() => {
                        return Err(anyhow::Error::new(PdfExtractStreamControl::Cancelled));
                    }
                    result = ingest_repository::upsert_content_revision_ingest_unit_completed_fenced(
                        &state.persistence.postgres,
                        authority,
                        &artifact,
                    ) => result,
                }
                .map_err(|error| {
                    anyhow::anyhow!(
                        "failed to persist extract unit {} for revision {}: {error}",
                        unit.unit_ordinal,
                        revision_id
                    )
                })?;
                if matches!(persisted, IngestAttemptMutationOutcome::AuthorityLost) {
                    return Err(anyhow::Error::new(PdfExtractStreamControl::AuthorityLost));
                }
                if cancellation_token.is_cancelled() {
                    return Err(anyhow::Error::new(PdfExtractStreamControl::Cancelled));
                }
                tokio::select! {
                    biased;
                    () = cancellation_token.cancelled() => {
                        return Err(anyhow::Error::new(PdfExtractStreamControl::Cancelled));
                    }
                    mut output_units = shared_output_units.lock() => {
                        output_units.push((
                            unit.unit_ordinal,
                            PdfExtractUnitMergeFragment::from_output(output),
                        ));
                    }
                }
                let progress = sync_pdf_extract_stage_progress(
                    &state,
                    authority,
                    page_count,
                    &cancellation_token,
                )
                .await?;
                if matches!(progress, IngestAttemptMutationOutcome::AuthorityLost) {
                    return Err(anyhow::Error::new(PdfExtractStreamControl::AuthorityLost));
                }
                Ok(())
            }
        },
    )),
    )
    .await?;
    match stream_result {
        Ok(()) => Ok(IngestAttemptMutationOutcome::Applied(())),
        Err(docling::DoclingBatchStreamError::Batch(error))
            if matches!(
                error.downcast_ref::<PdfExtractStreamControl>(),
                Some(PdfExtractStreamControl::AuthorityLost)
            ) =>
        {
            Ok(IngestAttemptMutationOutcome::AuthorityLost)
        }
        Err(docling::DoclingBatchStreamError::Batch(error))
            if matches!(
                error.downcast_ref::<PdfExtractStreamControl>(),
                Some(PdfExtractStreamControl::Cancelled)
            ) =>
        {
            Err(ContentExtractionError::extraction_failed(
                "extract_canceled",
                format!("extraction canceled for revision {revision_id}"),
            ))
        }
        Err(error) => Err({
            let message = format!(
                "failed to extract pdf pages {}-{} for revision {}: {error}",
                run.start_page, run.end_page, revision_id
            );
            match error {
                docling::DoclingBatchStreamError::Extraction(source) => {
                    pdf_docling_failure(&source, "pdf_page_range_extract_failed", message)
                }
                docling::DoclingBatchStreamError::Batch(_) => {
                    ContentExtractionError::extraction_failed(
                        "pdf_page_range_extract_failed",
                        message,
                    )
                }
            }
        }),
    }
}

pub(super) fn pdf_docling_failure(
    error: &docling::DoclingExtractionError,
    failure_code: &str,
    message: String,
) -> ContentExtractionError {
    let failure_code = error.memory_failure_code().unwrap_or(failure_code);
    // `docling_failure_class` yields only retryable and non-retryable classes
    // and never a content verdict, so the only decision left is whether the
    // worker may requeue. Any non-retryable docling fault is terminal without
    // claiming the source bytes are unsupported.
    if matches!(docling_failure_class(error), ExtractionFailureClass::OperationalRetryable) {
        ContentExtractionError::extraction_failed(failure_code, message)
    } else {
        ContentExtractionError::extraction_failed_terminal(failure_code, message)
    }
}

async fn sync_pdf_extract_stage_progress(
    state: &AppState,
    authority: IngestRevisionWriteAuthority,
    page_count: u32,
    cancellation_token: &CancellationToken,
) -> anyhow::Result<IngestAttemptMutationOutcome<()>> {
    if cancellation_token.is_cancelled() {
        return Err(anyhow::Error::new(PdfExtractStreamControl::Cancelled));
    }
    let completed_page_count =
        ingest_repository::sum_completed_content_revision_ingest_unit_pages_fenced(
            &state.persistence.postgres,
            authority,
            INGEST_STAGE_EXTRACT_CONTENT,
        )
        .await?;
    let completed_page_count = match completed_page_count {
        IngestAttemptMutationOutcome::Applied(value) => {
            value.clamp(0, i64::from(page_count)) as u32
        }
        IngestAttemptMutationOutcome::AuthorityLost => {
            return Ok(IngestAttemptMutationOutcome::AuthorityLost);
        }
    };
    if cancellation_token.is_cancelled() {
        return Err(anyhow::Error::new(PdfExtractStreamControl::Cancelled));
    }
    state
        .application_services
        .ingest
        .record_stage_unit_progress(
            state,
            RecordStageUnitProgressCommand {
                attempt_id: authority.attempt_id,
                stage_name: INGEST_STAGE_EXTRACT_CONTENT.to_string(),
                completed_units: completed_page_count,
                total_units: page_count,
                details_json: serde_json::json!({ "fileKind": "pdf" }),
            },
        )
        .await?;
    Ok(IngestAttemptMutationOutcome::Applied(()))
}

pub(super) async fn sync_resumable_pdf_extract_stage_progress_from_units(
    state: &AppState,
    authority: IngestRevisionWriteAuthority,
    cancellation_token: &CancellationToken,
) -> anyhow::Result<IngestAttemptMutationOutcome<()>> {
    if cancellation_token.is_cancelled() {
        return Err(anyhow::Error::new(PdfExtractStreamControl::Cancelled));
    }
    let progress = ingest_repository::get_content_revision_ingest_unit_page_progress_fenced(
        &state.persistence.postgres,
        authority,
        INGEST_STAGE_EXTRACT_CONTENT,
    )
    .await?;
    let progress = match progress {
        IngestAttemptMutationOutcome::Applied(progress) => progress,
        IngestAttemptMutationOutcome::AuthorityLost => {
            return Ok(IngestAttemptMutationOutcome::AuthorityLost);
        }
    };
    let Some(total_page_count) = progress
        .total_page_count
        .and_then(|value| u32::try_from(value).ok())
        .filter(|value| *value > 0)
    else {
        return Ok(IngestAttemptMutationOutcome::Applied(()));
    };
    let completed_page_count =
        progress.completed_page_count.clamp(0, i64::from(total_page_count)) as u32;
    if cancellation_token.is_cancelled() {
        return Err(anyhow::Error::new(PdfExtractStreamControl::Cancelled));
    }
    state
        .application_services
        .ingest
        .record_stage_unit_progress(
            state,
            RecordStageUnitProgressCommand {
                attempt_id: authority.attempt_id,
                stage_name: INGEST_STAGE_EXTRACT_CONTENT.to_string(),
                completed_units: completed_page_count,
                total_units: total_page_count,
                details_json: serde_json::json!({ "fileKind": "pdf" }),
            },
        )
        .await?;
    Ok(IngestAttemptMutationOutcome::Applied(()))
}

fn verified_merge_fragment_from_unit(
    row: &ingest_repository::ContentRevisionIngestUnitRow,
) -> Option<PdfExtractUnitMergeFragment> {
    let content_text = row.content_text.clone()?;
    let expected_checksum = sha256_text(&content_text);
    if row.content_checksum.as_deref()? != expected_checksum.as_str() {
        return None;
    }
    let warnings = serde_json::from_value(row.warnings_json.clone()).unwrap_or_default();
    Some(PdfExtractUnitMergeFragment {
        content_text,
        warnings,
        provider_kind: row.provider_kind.clone(),
        model_name: row.model_name.clone(),
        usage_json: row.usage_json.clone(),
    })
}

pub(super) fn merge_pdf_unit_outputs(
    outputs: Vec<PdfExtractUnitMergeFragment>,
    page_count: u32,
    file_name: &str,
    mime_type: &str,
) -> ExtractionOutput {
    let mut content = String::new();
    let mut warnings = Vec::new();
    let mut usage_items = Vec::new();
    let mut provider_kind = None;
    let mut model_name = None;

    for output in outputs {
        if !content.is_empty() {
            content.push_str("\n\n");
        }
        content.push_str(output.content_text.trim());
        warnings.extend(output.warnings);
        if provider_kind.is_none() {
            provider_kind = output.provider_kind;
        }
        if model_name.is_none() {
            model_name = output.model_name;
        }
        if output.usage_json != serde_json::json!({}) {
            usage_items.push(output.usage_json);
        }
    }
    let usage_json = merge_pdf_unit_usage_json(usage_items);

    let layout = build_text_layout_from_content(content.trim());
    let line_count = i32::try_from(layout.structure_hints.lines.len()).unwrap_or(i32::MAX);
    ExtractionOutput {
        extraction_kind: "docling_markdown".to_string(),
        content_text: layout.content_text,
        page_count: Some(page_count),
        warnings,
        source_metadata: ExtractionSourceMetadata {
            source_format: "pdf".to_string(),
            page_count: Some(page_count),
            line_count,
        },
        structure_hints: layout.structure_hints,
        source_map: serde_json::json!({
            "adapter": "docling",
            "input_file_name": file_name,
            "mime_type": mime_type,
            "source_format": "pdf",
            "timings": {
                "resumableUnits": true,
            },
        }),
        provider_kind,
        model_name,
        usage_json,
        extracted_images: Vec::new(),
    }
}

fn merge_pdf_unit_usage_json(usages: Vec<serde_json::Value>) -> serde_json::Value {
    let mut embedded_picture_usages = Vec::new();
    let mut standalone_usages = Vec::new();

    for usage in usages {
        match usage.get("embedded_picture_ocr_usage").and_then(serde_json::Value::as_array) {
            Some(items) if !items.is_empty() => {
                embedded_picture_usages.extend(items.iter().cloned());
            }
            _ => standalone_usages.push(usage),
        }
    }

    if !embedded_picture_usages.is_empty() {
        embedded_picture_usages.extend(standalone_usages);
        return crate::shared::extraction::file_extract::aggregate_vision_picture_usage_json(
            embedded_picture_usages,
        );
    }

    standalone_usages.into_iter().next().unwrap_or_else(|| serde_json::json!({}))
}

fn serialize_json<T: serde::Serialize>(
    value: &T,
    description: &'static str,
) -> anyhow::Result<serde_json::Value> {
    serde_json::to_value(value).with_context(|| format!("failed to serialize {description}"))
}

fn sha256_text(value: &str) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(value.as_bytes())))
}

pub(super) async fn generate_document_summary_from_blocks(
    state: &AppState,
    revision_id: Uuid,
) -> anyhow::Result<String> {
    let blocks = state
        .document_store
        .list_structured_blocks_by_revision(revision_id)
        .await
        .context("failed to load structured blocks for document summary generation")?;

    if blocks.is_empty() {
        return Ok(String::new());
    }

    Ok(build_document_summary_from_blocks(
        blocks
            .iter()
            .map(|block| DocumentSummaryBlock { block_kind: &block.block_kind, text: &block.text }),
    ))
}
