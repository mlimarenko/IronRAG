use std::{collections::HashMap, sync::Arc};

use chrono::Utc;
use futures::{StreamExt, stream};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    app::state::AppState,
    infra::{knowledge_rows::KnowledgeRevisionRow, repositories::ingest_repository},
    integrations::docling,
    services::ingest::worker::{CanonicalExtractContentError, CanonicalExtractedContent},
    shared::extraction::{
        ExtractionOutput, ExtractionSourceMetadata, build_text_layout_from_content,
        document_summary::{DocumentSummaryBlock, build_document_summary_from_blocks},
        file_extract::build_inline_text_extraction_plan,
    },
};

use super::super::service::{INGEST_STAGE_EXTRACT_CONTENT, RecordStageUnitProgressCommand};

const DEFAULT_PDF_EXTRACT_STREAM_WINDOW_PAGES: u32 = 40;

fn canonical_revision_file_name(revision: &KnowledgeRevisionRow) -> String {
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

pub(super) async fn resolve_canonical_extract_content(
    state: &AppState,
    job: &ingest_repository::IngestJobRow,
    attempt_id: Uuid,
    revision: &KnowledgeRevisionRow,
) -> Result<CanonicalExtractedContent, CanonicalExtractContentError> {
    let storage_ref =
        match revision.storage_ref.as_deref().map(str::trim).filter(|value| !value.is_empty()) {
            Some(storage_ref) => storage_ref.to_string(),
            None => state
                .canonical_services
                .content
                .resolve_revision_storage_key(state, revision.revision_id)
                .await
                .map_err(|_| {
                    CanonicalExtractContentError::missing_stored_source(
                        job.id,
                        revision.revision_id,
                    )
                })?
                .unwrap_or_default(),
        };
    if !storage_ref.is_empty() {
        let stored_bytes =
            state.content_storage.read_revision_source(&storage_ref).await.map_err(|error| {
                CanonicalExtractContentError::stored_source_read(&storage_ref, error)
            })?;
        let file_name = canonical_revision_file_name(revision);
        if is_pdf_revision(revision, &file_name) {
            return resolve_resumable_pdf_extract_content(
                state,
                attempt_id,
                revision,
                &file_name,
                &storage_ref,
                &stored_bytes,
            )
            .await;
        }
        let plan = state
            .canonical_services
            .content
            .build_runtime_extraction_plan(
                state,
                revision.library_id,
                &file_name,
                Some(revision.mime_type.as_str()),
                &stored_bytes,
            )
            .await
            .map_err(|rejection| CanonicalExtractContentError::extraction_rejected(&rejection))?;
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
        return Ok(CanonicalExtractedContent {
            provider_kind: plan.provider_kind.clone(),
            model_name: plan.model_name.clone(),
            usage_json: plan.usage_json.clone(),
            extraction_plan: plan,
            stage_details,
        });
    }

    if let Some(text) = revision
        .normalized_text
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
    {
        let extraction_plan = build_inline_text_extraction_plan(&text);
        return Ok(CanonicalExtractedContent {
            provider_kind: extraction_plan.provider_kind.clone(),
            model_name: extraction_plan.model_name.clone(),
            usage_json: extraction_plan.usage_json.clone(),
            extraction_plan,
            stage_details: serde_json::json!({
                "contentLength": text.chars().count(),
                "source": "knowledge_revision",
            }),
        });
    }

    Err(CanonicalExtractContentError::missing_stored_source(job.id, revision.revision_id))
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
struct PdfExtractUnit {
    unit_ordinal: i32,
    start_page: u32,
    end_page: u32,
    range_start: i32,
    range_end: i32,
}

#[derive(Clone, Copy)]
struct PdfExtractRun {
    start_page: u32,
    end_page: u32,
}

struct PdfExtractUnitMergeFragment {
    content_text: String,
    warnings: Vec<String>,
    provider_kind: Option<String>,
    model_name: Option<String>,
    usage_json: serde_json::Value,
}

impl PdfExtractUnitMergeFragment {
    fn from_output(mut output: ExtractionOutput) -> Self {
        Self {
            content_text: std::mem::take(&mut output.content_text),
            warnings: std::mem::take(&mut output.warnings),
            provider_kind: output.provider_kind.take(),
            model_name: output.model_name.take(),
            usage_json: output.usage_json,
        }
    }
}

fn contiguous_pdf_extract_unit_runs(
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

async fn resolve_resumable_pdf_extract_content(
    state: &AppState,
    attempt_id: Uuid,
    revision: &KnowledgeRevisionRow,
    file_name: &str,
    storage_ref: &str,
    stored_bytes: &[u8],
) -> Result<CanonicalExtractedContent, CanonicalExtractContentError> {
    let file_size_bytes = u64::try_from(stored_bytes.len()).unwrap_or(u64::MAX);
    let page_count = docling::extract_pdf_page_count(
        Some(file_name),
        Some(revision.mime_type.as_str()),
        "pdf",
        stored_bytes,
    )
    .await
    .map_err(|error| {
        let message =
            format!("failed to read pdf page count for revision {}: {error}", revision.revision_id);
        if let Some(failure_code) = error.memory_failure_code() {
            CanonicalExtractContentError::extraction_failed_terminal(failure_code, message)
        } else {
            CanonicalExtractContentError::extraction_failed("pdf_page_count_failed", message)
        }
    })?
    .filter(|value| *value > 0)
    .ok_or_else(|| {
        CanonicalExtractContentError::extraction_failed(
            "pdf_page_count_unavailable",
            format!("failed to read pdf page count for revision {}", revision.revision_id),
        )
    })?;

    let batch_size = docling::configured_page_batch_size();
    let existing_rows = ingest_repository::list_content_revision_ingest_units(
        &state.persistence.postgres,
        revision.revision_id,
        INGEST_STAGE_EXTRACT_CONTENT,
    )
    .await
    .map_err(|error| {
        CanonicalExtractContentError::extraction_failed(
            "extract_unit_read_failed",
            format!("failed to load extract units for revision {}: {error}", revision.revision_id),
        )
    })?;
    let completed_rows: HashMap<i32, ingest_repository::ContentRevisionIngestUnitRow> =
        existing_rows
            .into_iter()
            .filter(|row| row.unit_state == "completed" && row.unit_kind == "pdf_page_range")
            .map(|row| (row.unit_ordinal, row))
            .collect();
    let mut output_units: Vec<(i32, PdfExtractUnitMergeFragment)> = Vec::new();
    let mut missing_units: Vec<PdfExtractUnit> = Vec::new();
    let mut reused_unit_count = 0_i32;
    let batch_count = page_count.div_ceil(batch_size);

    for batch_idx in 0..batch_count {
        let unit_ordinal = i32::try_from(batch_idx).unwrap_or(i32::MAX) + 1;
        let start_page = batch_idx * batch_size + 1;
        let end_page = ((batch_idx + 1) * batch_size).min(page_count);
        let range_start = i32::try_from(start_page).unwrap_or(i32::MAX);
        let range_end = i32::try_from(end_page).unwrap_or(i32::MAX);

        if let Some(row) = completed_rows
            .get(&unit_ordinal)
            .filter(|row| row.range_start == range_start && row.range_end == range_end)
        {
            output_units.push((unit_ordinal, merge_fragment_from_unit(row)?));
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
    sync_pdf_extract_stage_progress(state, attempt_id, revision.revision_id, page_count)
        .await
        .map_err(|error| {
            CanonicalExtractContentError::extraction_failed(
                "extract_progress_sync_failed",
                format!(
                    "failed to sync extract progress for revision {}: {error}",
                    revision.revision_id
                ),
            )
        })?;

    let extracted_unit_count = i32::try_from(missing_units.len()).unwrap_or(i32::MAX);
    let stream_window_pages = pdf_extract_stream_window_pages(batch_size);
    let mut run_parallelism = 0_usize;
    if !missing_units.is_empty() {
        let shared_output_units = Arc::new(tokio::sync::Mutex::new(output_units));
        let expected_units: Arc<HashMap<u32, PdfExtractUnit>> =
            Arc::new(missing_units.iter().cloned().map(|unit| (unit.start_page, unit)).collect());
        let revision_id = revision.revision_id;
        let library_id = revision.library_id;
        let mime_type = revision.mime_type.clone();
        let file_name = file_name.to_string();

        let runs = contiguous_pdf_extract_unit_runs(&missing_units, stream_window_pages);
        let run_count = runs.len();
        run_parallelism = run_count.max(1).min(docling::configured_max_concurrency());
        let mut run_results = stream::iter(runs.into_iter().map(|run| {
            extract_pdf_run_streamed(
                state.clone(),
                attempt_id,
                revision_id,
                library_id,
                file_name.clone(),
                mime_type.clone(),
                stored_bytes,
                page_count,
                batch_size,
                run,
                Arc::clone(&expected_units),
                Arc::clone(&shared_output_units),
            )
        }))
        .buffer_unordered(run_parallelism);

        let mut first_error = None;
        while let Some(result) = run_results.next().await {
            if let Err(error) = result {
                first_error.get_or_insert(error);
            }
        }
        drop(run_results);
        if let Some(error) = first_error {
            return Err(error);
        }

        output_units = Arc::try_unwrap(shared_output_units)
            .map_err(|_| {
                CanonicalExtractContentError::extraction_failed(
                    "extract_unit_collect_failed",
                    format!(
                        "failed to collect extract units for revision {}",
                        revision.revision_id
                    ),
                )
            })?
            .into_inner();
    }

    output_units.sort_by_key(|(unit_ordinal, _)| *unit_ordinal);
    let outputs = output_units.into_iter().map(|(_, output)| output).collect();
    let merged_output = merge_pdf_unit_outputs(outputs, page_count, file_name, &revision.mime_type);
    let plan = state
        .canonical_services
        .content
        .build_runtime_pdf_docling_extraction_plan_from_output(
            state,
            revision.library_id,
            file_name,
            Some(revision.mime_type.as_str()),
            file_size_bytes,
            merged_output,
        )
        .await
        .map_err(|rejection| CanonicalExtractContentError::extraction_rejected(&rejection))?;

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
    Ok(CanonicalExtractedContent {
        provider_kind: plan.provider_kind.clone(),
        model_name: plan.model_name.clone(),
        usage_json: plan.usage_json.clone(),
        extraction_plan: plan,
        stage_details,
    })
}

async fn extract_pdf_run_streamed(
    state: AppState,
    attempt_id: Uuid,
    revision_id: Uuid,
    library_id: Uuid,
    file_name: String,
    mime_type: String,
    stored_bytes: &[u8],
    page_count: u32,
    batch_size: u32,
    run: PdfExtractRun,
    expected_units: Arc<HashMap<u32, PdfExtractUnit>>,
    shared_output_units: Arc<tokio::sync::Mutex<Vec<(i32, PdfExtractUnitMergeFragment)>>>,
) -> Result<(), CanonicalExtractContentError> {
    docling::extract_pdf_page_ranges_streamed(
        Some(&file_name),
        Some(mime_type.as_str()),
        "pdf",
        stored_bytes,
        run.start_page,
        run.end_page,
        batch_size,
        move |batch| {
            let shared_output_units = Arc::clone(&shared_output_units);
            let expected_units = Arc::clone(&expected_units);
            let state = state.clone();
            async move {
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
                state
                    .canonical_services
                    .content
                    .augment_runtime_docling_output(&state, library_id, &mut output)
                    .await
                    .map_err(|error| {
                        anyhow::anyhow!(
                            "failed to augment docling output for revision {}: {error}",
                            revision_id
                        )
                    })?;
                let content_checksum = sha256_text(&output.content_text);
                ingest_repository::upsert_content_revision_ingest_unit_completed(
                    &state.persistence.postgres,
                    &ingest_repository::UpsertContentRevisionIngestUnitCompleted {
                        revision_id,
                        stage_name: INGEST_STAGE_EXTRACT_CONTENT.to_string(),
                        unit_ordinal: unit.unit_ordinal,
                        unit_kind: "pdf_page_range".to_string(),
                        range_start: unit.range_start,
                        range_end: unit.range_end,
                        content_text: Some(output.content_text.clone()),
                        structure_hints_json: Some(json_or_null(&output.structure_hints)),
                        source_metadata_json: Some(json_or_null(&output.source_metadata)),
                        source_map_json: Some(output.source_map.clone()),
                        warnings_json: json_or_null(&output.warnings),
                        usage_json: output.usage_json.clone(),
                        provider_kind: output.provider_kind.clone(),
                        model_name: output.model_name.clone(),
                        content_checksum: Some(content_checksum),
                        details_json: serde_json::json!({
                            "pageCount": page_count,
                            "pageBatchSize": batch_size,
                            "streamed": true,
                        }),
                        attempt_id: Some(attempt_id),
                        elapsed_ms: Some(batch.elapsed_ms),
                        started_at: Some(Utc::now()),
                    },
                )
                .await
                .map_err(|error| {
                    anyhow::anyhow!(
                        "failed to persist extract unit {} for revision {}: {error}",
                        unit.unit_ordinal,
                        revision_id
                    )
                })?;
                shared_output_units
                    .lock()
                    .await
                    .push((unit.unit_ordinal, PdfExtractUnitMergeFragment::from_output(output)));
                sync_pdf_extract_stage_progress(&state, attempt_id, revision_id, page_count)
                    .await?;
                Ok(())
            }
        },
    )
    .await
    .map_err(|error| {
        let message = format!(
            "failed to extract pdf pages {}-{} for revision {}: {error}",
            run.start_page, run.end_page, revision_id
        );
        if let Some(failure_code) = error.memory_failure_code() {
            CanonicalExtractContentError::extraction_failed_terminal(failure_code, message)
        } else {
            CanonicalExtractContentError::extraction_failed(
                "pdf_page_range_extract_failed",
                message,
            )
        }
    })
}

async fn sync_pdf_extract_stage_progress(
    state: &AppState,
    attempt_id: Uuid,
    revision_id: Uuid,
    page_count: u32,
) -> anyhow::Result<()> {
    let completed_page_count = ingest_repository::sum_completed_content_revision_ingest_unit_pages(
        &state.persistence.postgres,
        revision_id,
        INGEST_STAGE_EXTRACT_CONTENT,
    )
    .await?
    .clamp(0, i64::from(page_count)) as u32;
    state
        .canonical_services
        .ingest
        .record_stage_unit_progress(
            state,
            RecordStageUnitProgressCommand {
                attempt_id,
                stage_name: INGEST_STAGE_EXTRACT_CONTENT.to_string(),
                completed_units: completed_page_count,
                total_units: page_count,
                details_json: serde_json::json!({ "fileKind": "pdf" }),
            },
        )
        .await?;
    Ok(())
}

pub(super) async fn sync_resumable_pdf_extract_stage_progress_from_units(
    state: &AppState,
    attempt_id: Uuid,
    revision_id: Uuid,
) -> anyhow::Result<()> {
    let progress = ingest_repository::get_content_revision_ingest_unit_page_progress(
        &state.persistence.postgres,
        revision_id,
        INGEST_STAGE_EXTRACT_CONTENT,
    )
    .await?;
    let Some(total_page_count) = progress
        .total_page_count
        .and_then(|value| u32::try_from(value).ok())
        .filter(|value| *value > 0)
    else {
        return Ok(());
    };
    let completed_page_count =
        progress.completed_page_count.clamp(0, i64::from(total_page_count)) as u32;
    state
        .canonical_services
        .ingest
        .record_stage_unit_progress(
            state,
            RecordStageUnitProgressCommand {
                attempt_id,
                stage_name: INGEST_STAGE_EXTRACT_CONTENT.to_string(),
                completed_units: completed_page_count,
                total_units: total_page_count,
                details_json: serde_json::json!({ "fileKind": "pdf" }),
            },
        )
        .await?;
    Ok(())
}

fn merge_fragment_from_unit(
    row: &ingest_repository::ContentRevisionIngestUnitRow,
) -> Result<PdfExtractUnitMergeFragment, CanonicalExtractContentError> {
    let content_text = row.content_text.clone().ok_or_else(|| {
        CanonicalExtractContentError::extraction_failed(
            "extract_unit_missing_content",
            format!(
                "completed extract unit {} for revision {} has no content",
                row.unit_ordinal, row.revision_id
            ),
        )
    })?;
    let warnings = serde_json::from_value(row.warnings_json.clone()).unwrap_or_default();
    Ok(PdfExtractUnitMergeFragment {
        content_text,
        warnings,
        provider_kind: row.provider_kind.clone(),
        model_name: row.model_name.clone(),
        usage_json: row.usage_json.clone(),
    })
}

fn merge_pdf_unit_outputs(
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

fn json_or_null<T: serde::Serialize>(value: &T) -> serde_json::Value {
    serde_json::to_value(value).unwrap_or(serde_json::Value::Null)
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
        .unwrap_or_default();

    if blocks.is_empty() {
        return Ok(String::new());
    }

    Ok(build_document_summary_from_blocks(
        blocks
            .iter()
            .map(|block| DocumentSummaryBlock { block_kind: &block.block_kind, text: &block.text }),
    ))
}

#[cfg(test)]
mod tests {
    use super::{
        PdfExtractUnit, PdfExtractUnitMergeFragment, contiguous_pdf_extract_unit_runs,
        merge_pdf_unit_outputs,
    };
    use crate::shared::extraction::{
        ExtractedImage, ExtractionOutput, ExtractionSourceMetadata, ExtractionStructureHints,
    };

    fn unit(unit_ordinal: i32, start_page: u32, end_page: u32) -> PdfExtractUnit {
        PdfExtractUnit {
            unit_ordinal,
            start_page,
            end_page,
            range_start: i32::try_from(start_page).unwrap(),
            range_end: i32::try_from(end_page).unwrap(),
        }
    }

    #[test]
    fn splits_contiguous_pdf_extract_runs_by_memory_window() {
        let units = vec![
            unit(1, 1, 10),
            unit(2, 11, 20),
            unit(3, 21, 30),
            unit(4, 31, 40),
            unit(5, 41, 50),
        ];

        let runs = contiguous_pdf_extract_unit_runs(&units, 25);

        assert_eq!(runs.len(), 3);
        assert_eq!((runs[0].start_page, runs[0].end_page), (1, 20));
        assert_eq!((runs[1].start_page, runs[1].end_page), (21, 40));
        assert_eq!((runs[2].start_page, runs[2].end_page), (41, 50));
    }

    #[test]
    fn keeps_pdf_extract_run_boundaries_at_existing_gaps() {
        let units = vec![unit(1, 1, 10), unit(3, 31, 40), unit(4, 41, 50)];

        let runs = contiguous_pdf_extract_unit_runs(&units, 100);

        assert_eq!(runs.len(), 2);
        assert_eq!((runs[0].start_page, runs[0].end_page), (1, 10));
        assert_eq!((runs[1].start_page, runs[1].end_page), (31, 50));
    }

    #[test]
    fn pdf_extract_merge_fragment_drops_heavy_extraction_state() {
        let fragment = PdfExtractUnitMergeFragment::from_output(ExtractionOutput {
            extraction_kind: "docling_markdown".to_string(),
            content_text: "Alpha section".to_string(),
            page_count: Some(1),
            warnings: vec!["partial table".to_string()],
            source_metadata: ExtractionSourceMetadata {
                source_format: "pdf".to_string(),
                page_count: Some(1),
                line_count: 1,
            },
            structure_hints: ExtractionStructureHints::default(),
            source_map: serde_json::json!({"heavy": true}),
            provider_kind: Some("provider".to_string()),
            model_name: Some("model".to_string()),
            usage_json: serde_json::json!({"tokens": 42}),
            extracted_images: vec![ExtractedImage {
                page: 1,
                image_bytes: vec![7_u8; 1024 * 1024],
                mime_type: "image/png".to_string(),
                width: 128,
                height: 128,
            }],
        });

        assert_eq!(fragment.content_text, "Alpha section");
        assert_eq!(fragment.warnings, vec!["partial table"]);
        assert_eq!(fragment.provider_kind.as_deref(), Some("provider"));
        assert_eq!(fragment.model_name.as_deref(), Some("model"));
        assert_eq!(fragment.usage_json, serde_json::json!({"tokens": 42}));
    }

    #[test]
    fn merge_pdf_unit_outputs_rebuilds_final_layout_from_fragments() {
        let output = merge_pdf_unit_outputs(
            vec![
                PdfExtractUnitMergeFragment {
                    content_text: "Alpha section".to_string(),
                    warnings: vec!["first warning".to_string()],
                    provider_kind: Some("provider".to_string()),
                    model_name: Some("model".to_string()),
                    usage_json: serde_json::json!({"tokens": 7}),
                },
                PdfExtractUnitMergeFragment {
                    content_text: "Beta section".to_string(),
                    warnings: vec!["second warning".to_string()],
                    provider_kind: None,
                    model_name: None,
                    usage_json: serde_json::json!({}),
                },
            ],
            2,
            "input.pdf",
            "application/pdf",
        );

        assert_eq!(output.content_text, "Alpha section\n\nBeta section");
        assert_eq!(output.page_count, Some(2));
        assert_eq!(output.warnings, vec!["first warning", "second warning"]);
        assert_eq!(output.provider_kind.as_deref(), Some("provider"));
        assert_eq!(output.model_name.as_deref(), Some("model"));
        assert_eq!(output.usage_json, serde_json::json!({"tokens": 7}));
        assert!(output.extracted_images.is_empty());
        assert_eq!(output.structure_hints.lines.len(), 3);
    }

    #[test]
    fn merge_pdf_unit_outputs_preserves_all_embedded_picture_usage() {
        let output = merge_pdf_unit_outputs(
            vec![
                PdfExtractUnitMergeFragment {
                    content_text: "Alpha section".to_string(),
                    warnings: Vec::new(),
                    provider_kind: Some("provider".to_string()),
                    model_name: Some("model".to_string()),
                    usage_json: serde_json::json!({
                        "embedded_picture_ocr_usage": [
                            {"prompt_tokens": 10, "completion_tokens": 2, "total_tokens": 12},
                            {"prompt_tokens": 20, "completion_tokens": 4, "total_tokens": 24}
                        ],
                    }),
                },
                PdfExtractUnitMergeFragment {
                    content_text: "Beta section".to_string(),
                    warnings: Vec::new(),
                    provider_kind: Some("provider".to_string()),
                    model_name: Some("model".to_string()),
                    usage_json: serde_json::json!({
                        "embedded_picture_ocr_usage": [
                            {"prompt_tokens": 30, "completion_tokens": 6, "total_tokens": 36}
                        ],
                    }),
                },
            ],
            2,
            "input.pdf",
            "application/pdf",
        );

        assert_eq!(output.usage_json["embedded_picture_ocr_call_count"], serde_json::json!(3));
        assert_eq!(output.usage_json["prompt_tokens"], serde_json::json!(60));
        assert_eq!(output.usage_json["completion_tokens"], serde_json::json!(12));
        assert_eq!(output.usage_json["total_tokens"], serde_json::json!(72));
        assert_eq!(
            output.usage_json["embedded_picture_ocr_usage"].as_array().map(Vec::len),
            Some(3)
        );
    }
}
