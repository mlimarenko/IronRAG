//! Embedded Docling extraction adapter.
//!
//! Docling is installed into the backend image and executed locally. This
//! module owns the process boundary and converts Docling Markdown output into
//! the shared [`ExtractionOutput`] shape consumed by the ingest pipeline.

mod output;
mod runtime;

use std::{
    ffi::OsStr,
    future::Future,
    path::{Path, PathBuf},
    process::{ExitStatus, Stdio},
    sync::{Arc, LazyLock},
    time::Duration,
};

#[cfg(target_family = "unix")]
use std::os::unix::process::ExitStatusExt;

use serde::Deserialize;
use tempfile::TempDir;
use thiserror::Error;
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, BufReader},
    process::Command,
    sync::{OwnedSemaphorePermit, Semaphore},
    time::timeout,
};

use crate::shared::{
    admission::PermitWaitError,
    extraction::{ExtractionOutput, ExtractionSourceMetadata, build_text_layout_from_content},
    telemetry,
};

use self::{
    output::build_output,
    runtime::{
        acquire_docling_permit, page_batch_size, process_status_is_memory_kill,
        resolve_docling_max_concurrency, run_docling, run_docling_batch, run_docling_page_batches,
        run_docling_page_count, write_input_file,
    },
};

/// Identity of the bytes handed to the Docling extractor: the caller's file
/// name, declared MIME type, and the resolved source format. Grouped so the
/// input identity travels as one value instead of three positional arguments.
#[derive(Clone, Copy)]
pub struct DoclingSource<'a> {
    pub file_name: Option<&'a str>,
    pub mime_type: Option<&'a str>,
    pub source_format: &'a str,
}

pub(crate) use self::runtime::default_docling_acquire_timeout_secs;

const DEFAULT_EXTRACT_BIN: &str = "ironrag-docling-extract";
const DEFAULT_TIMEOUT_SECS: u64 = 900;
const DEFAULT_PAGE_BATCH_SIZE: u32 = 10;
const DOCLING_AUTO_MAX_CONCURRENCY: usize = 4;
const DOCLING_AUTO_RESERVED_MEMORY_MIB: u64 = 2048;
const DOCLING_AUTO_MEMORY_PER_PROCESS_MIB: u64 = 2560;
const DOCLING_PROCESS_SAFETY_MARGIN_MIB: u64 = 256;
const STDERR_PREVIEW_LIMIT: usize = 4_000;

/// Heavy-pipeline jobs that may queue per Docling process slot. The ingest
/// worker sizes heavy-revision parallelism with this one constant and the
/// Docling acquire bound waits long enough to outlast a fully queued pipeline,
/// so the two cannot drift apart.
pub(crate) const DOCLING_WAITERS_PER_PROCESS: usize = 2;

static DOCLING_MAX_CONCURRENCY: LazyLock<usize> = LazyLock::new(|| {
    let concurrency = resolve_docling_max_concurrency();
    tracing::info!(concurrency, "docling concurrency configured");
    concurrency
});

static DOCLING_CONCURRENCY: LazyLock<Arc<Semaphore>> = LazyLock::new(|| {
    let concurrency = *DOCLING_MAX_CONCURRENCY;
    Arc::new(Semaphore::new(concurrency))
});

#[derive(Debug, Error)]
pub enum DoclingExtractionError {
    #[error("failed to create docling workspace: {0}")]
    TempDir(std::io::Error),

    #[error("failed to write docling input: {0}")]
    WriteInput(std::io::Error),

    #[error("docling extractor is unavailable: {0}")]
    Spawn(std::io::Error),

    #[error("docling extraction timed out after {0}s")]
    Timeout(u64),

    #[error("docling extractor failed with status {status}: {stderr}")]
    ProcessFailed { status: ExitStatus, stderr: String },

    #[error(
        "docling extraction needs at least {required_mib} MiB plus {safety_margin_mib} MiB safety margin, but the worker cgroup has only {available_mib} MiB available (limit {memory_limit_mib} MiB, current RSS {current_rss_mib} MiB); raise IRONRAG_WORKER_MEMORY_LIMIT and recreate the worker container"
    )]
    InsufficientMemory {
        memory_limit_mib: u64,
        current_rss_mib: u64,
        available_mib: u64,
        required_mib: u64,
        safety_margin_mib: u64,
    },

    #[error("docling extractor returned invalid utf-8: {0}")]
    InvalidUtf8(std::string::FromUtf8Error),

    #[error("docling extractor returned invalid json: {0}")]
    InvalidJson(serde_json::Error),

    #[error("docling extracted no text")]
    EmptyOutput,

    #[error("docling concurrency limiter is closed")]
    LimiterClosed,

    #[error("docling concurrency permit wait timed out after {timeout_ms} ms")]
    AcquireTimeout { timeout_ms: u64 },

    #[error("docling page extraction failed for page {page}: {source}")]
    PageExtractionFailed {
        page: u32,
        #[source]
        source: Box<Self>,
    },

    #[error("docling paginated merge failed: {0}")]
    PaginatedMergeFailed(String),

    #[error("docling pdf page count is unavailable")]
    PdfPageCountUnavailable,
}

#[derive(Debug, Error)]
pub enum DoclingBatchStreamError {
    #[error(transparent)]
    Extraction(#[from] DoclingExtractionError),

    #[error("docling page batch handler failed: {0}")]
    Batch(anyhow::Error),
}

impl DoclingExtractionError {
    #[must_use]
    pub fn memory_failure_code(&self) -> Option<&'static str> {
        match self {
            Self::InsufficientMemory { .. } => Some("docling_insufficient_memory"),
            Self::ProcessFailed { status, .. } if process_status_is_memory_kill(status) => {
                Some("docling_process_oom")
            }
            Self::PageExtractionFailed { source, .. } => source.memory_failure_code(),
            _ => None,
        }
    }
}

impl From<PermitWaitError> for DoclingExtractionError {
    fn from(error: PermitWaitError) -> Self {
        match error {
            PermitWaitError::AcquireTimeout { timeout_ms, .. } => {
                Self::AcquireTimeout { timeout_ms }
            }
            PermitWaitError::LimiterClosed { .. } => Self::LimiterClosed,
        }
    }
}

impl DoclingBatchStreamError {
    #[must_use]
    pub fn memory_failure_code(&self) -> Option<&'static str> {
        match self {
            Self::Extraction(error) => error.memory_failure_code(),
            Self::Batch(_) => None,
        }
    }
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
struct DoclingExtractionPayload {
    markdown: String,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    picture_ocr_text: Vec<String>,
    #[serde(default)]
    page_count: Option<u32>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    input_format: Option<String>,
    #[serde(default)]
    docling_version: Option<String>,
    #[serde(default)]
    warnings: Vec<String>,
    #[serde(default)]
    timings: serde_json::Value,
    /// Embedded picture items extracted from the source. Each entry
    /// carries the cropped picture bytes (base64-encoded PNG) so the
    /// caller can route them through the active document-understanding binding when the
    /// library policy chooses provider-backed OCR instead of local CPU OCR.
    /// The `index` matches the placeholder ordinal in `markdown`.
    #[serde(default)]
    pictures: Vec<DoclingExtractionPicture>,
}

/// Lightweight page-count response from `--page-count`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DoclingPageCountPayload {
    #[serde(default)]
    page_count: Option<u32>,
}

/// Batch response from `--pages START-END`. Contains per-page payloads
/// produced in a single Python process (`RapidOCR` loaded once).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DoclingBatchPayload {
    #[serde(default)]
    page_range: Option<String>,
    pages: Vec<DoclingExtractionPayload>,
}

pub struct DoclingPageRangeExtraction {
    pub start_page: u32,
    pub end_page: u32,
    pub elapsed_ms: i64,
    pub output: ExtractionOutput,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DoclingExtractionPicture {
    pub index: usize,
    #[serde(default)]
    pub mime: Option<String>,
    pub content_base64: String,
    #[serde(default)]
    pub size_px: Vec<u32>,
}

/// Extracts document text with the local Docling runtime.
///
/// # Errors
///
/// Returns [`DoclingExtractionError`] when the embedded extractor is missing,
/// fails, times out, or returns empty/invalid output.
pub async fn extract_document(
    source: DoclingSource<'_>,
    file_bytes: Vec<u8>,
) -> Result<ExtractionOutput, DoclingExtractionError> {
    let _permit = acquire_docling_permit().await?;
    let temp_dir = tempfile::tempdir().map_err(DoclingExtractionError::TempDir)?;
    let input_path = write_input_file(&temp_dir, source, &file_bytes)?;

    if source.source_format == "pdf" {
        return extract_document_paginated(&input_path, source).await;
    }

    let payload = run_docling(&input_path, &[]).await?;
    build_output(payload, source.file_name, source.mime_type, source.source_format)
}

pub async fn extract_pdf_page_count(
    source: DoclingSource<'_>,
    file_bytes: &[u8],
) -> Result<Option<u32>, DoclingExtractionError> {
    let temp_dir = tempfile::tempdir().map_err(DoclingExtractionError::TempDir)?;
    let input_path = write_input_file(&temp_dir, source, file_bytes)?;
    run_docling_page_count(&input_path).await
}

pub async fn extract_pdf_page_ranges_streamed<F, Fut>(
    source: DoclingSource<'_>,
    file_bytes: &[u8],
    start_page: u32,
    end_page: u32,
    batch_size: u32,
    on_batch: F,
) -> Result<(), DoclingBatchStreamError>
where
    F: FnMut(DoclingPageRangeExtraction) -> Fut,
    Fut: Future<Output = anyhow::Result<()>>,
{
    let _permit = acquire_docling_permit().await?;
    let temp_dir = tempfile::tempdir().map_err(DoclingExtractionError::TempDir)?;
    let input_path = write_input_file(&temp_dir, source, file_bytes)?;
    run_docling_page_batches(&input_path, source, start_page, end_page, batch_size, on_batch).await
}

#[must_use]
pub fn configured_page_batch_size() -> u32 {
    page_batch_size()
}

#[must_use]
pub fn configured_max_concurrency() -> usize {
    *DOCLING_MAX_CONCURRENCY
}

/// Extracts a PDF through the page-range Docling path, merging results into
/// a single output for callers that do not own durable ingest-unit state.
async fn extract_document_paginated(
    input_path: &Path,
    source: DoclingSource<'_>,
) -> Result<ExtractionOutput, DoclingExtractionError> {
    let page_count = run_docling_page_count(input_path)
        .await?
        .filter(|value| *value > 0)
        .ok_or(DoclingExtractionError::PdfPageCountUnavailable)?;

    let batch_size = page_batch_size();
    let batch_count = page_count.div_ceil(batch_size);
    log_paginated_extraction_start(page_count, batch_size, batch_count);

    let (merged, total_seconds) =
        extract_paginated_payloads(input_path, page_count, batch_size, batch_count).await?;

    log_paginated_extraction_complete(page_count, total_seconds);

    let merged_payload = DoclingExtractionPayload {
        markdown: merged.markdown,
        text: Some(merged.text),
        picture_ocr_text: merged.picture_ocr_text,
        page_count: Some(page_count),
        status: merged.status,
        input_format: merged.input_format,
        docling_version: None,
        warnings: merged.warnings,
        timings: serde_json::json!({"totalSeconds": total_seconds, "paginated": true, "pageCount": page_count}),
        pictures: merged.pictures,
    };

    build_output(merged_payload, source.file_name, source.mime_type, source.source_format)
}

fn log_paginated_extraction_start(page_count: u32, batch_size: u32, batch_count: u32) {
    tracing::info!(
        page_count,
        batch_size,
        batch_count,
        "docling: starting batched page-at-a-time extraction"
    );
}

fn log_paginated_extraction_complete(page_count: u32, total_seconds: f64) {
    tracing::info!(page_count, total_seconds, "docling: page-at-a-time extraction complete");
}

/// Extracts every page batch in order, merging each into one payload and
/// summing the per-batch elapsed seconds.
async fn extract_paginated_payloads(
    input_path: &Path,
    page_count: u32,
    batch_size: u32,
    batch_count: u32,
) -> Result<(PaginatedDoclingPayload, f64), DoclingExtractionError> {
    let mut merged = PaginatedDoclingPayload::default();
    let mut total_seconds = 0.0_f64;

    for batch_idx in 0..batch_count {
        let (start, end) = page_batch_range(batch_idx, batch_size, page_count);
        let (payloads, batch_elapsed) = extract_docling_page_batch(input_path, start, end).await?;

        for payload in payloads {
            merged.append(payload);
        }

        total_seconds += batch_elapsed;

        if batch_count > 1 {
            log_paginated_batch_complete(batch_idx, batch_count, start, end);
        }
    }

    Ok((merged, total_seconds))
}

fn log_paginated_batch_complete(batch_idx: u32, batch_count: u32, start: u32, end: u32) {
    tracing::info!(
        batch = batch_idx + 1,
        batch_count,
        pages = format!("{start}-{end}"),
        "docling: batch complete"
    );
}

/// Inclusive 1-based page range covered by batch `batch_index`.
fn page_batch_range(batch_index: u32, batch_size: u32, page_count: u32) -> (u32, u32) {
    let start = batch_index * batch_size + 1;
    let end = ((batch_index + 1) * batch_size).min(page_count);
    (start, end)
}

#[derive(Default)]
struct PaginatedDoclingPayload {
    markdown: String,
    text: String,
    pictures: Vec<DoclingExtractionPicture>,
    picture_ocr_text: Vec<String>,
    warnings: Vec<String>,
    status: Option<String>,
    input_format: Option<String>,
}

impl PaginatedDoclingPayload {
    fn append(&mut self, payload: DoclingExtractionPayload) {
        if !self.markdown.is_empty() {
            self.markdown.push_str("\n\n");
            self.text.push_str("\n\n");
        }
        self.markdown.push_str(&payload.markdown);
        if let Some(text) = payload.text {
            self.text.push_str(&text);
        }
        self.picture_ocr_text.extend(payload.picture_ocr_text);

        let picture_index_offset = self.pictures.len();
        self.pictures.extend(payload.pictures.into_iter().map(|mut picture| {
            picture.index += picture_index_offset;
            picture
        }));

        self.warnings.extend(payload.warnings);
        self.status = payload.status.or(self.status.clone());
        self.input_format = payload.input_format.or(self.input_format.clone());
    }
}

fn extract_docling_batch_elapsed(payload: &DoclingExtractionPayload) -> f64 {
    payload.timings.get("totalSeconds").and_then(serde_json::Value::as_f64).unwrap_or(0.0)
}

async fn extract_docling_page_batch(
    input_path: &Path,
    start_page: u32,
    end_page: u32,
) -> Result<(Vec<DoclingExtractionPayload>, f64), DoclingExtractionError> {
    if start_page == end_page {
        let page_arg = start_page.to_string();
        let payload = run_docling(input_path, &["--page", &page_arg]).await.map_err(|error| {
            DoclingExtractionError::PageExtractionFailed {
                page: start_page,
                source: Box::new(error),
            }
        })?;
        let elapsed = extract_docling_batch_elapsed(&payload);
        return Ok((vec![payload], elapsed));
    }

    let mut batch = run_docling_batch(input_path, start_page, end_page).await.map_err(|error| {
        DoclingExtractionError::PageExtractionFailed { page: start_page, source: Box::new(error) }
    })?;
    let elapsed = batch.pages.first().map(extract_docling_batch_elapsed).unwrap_or(0.0);
    Ok((vec![merge_batch_payload(&mut batch, start_page, end_page)], elapsed))
}

fn merge_batch_payload(
    batch: &mut DoclingBatchPayload,
    start_page: u32,
    end_page: u32,
) -> DoclingExtractionPayload {
    let mut merged_markdown = String::new();
    let mut merged_text = String::new();
    let mut merged_pictures: Vec<DoclingExtractionPicture> = Vec::new();
    let mut merged_picture_ocr_text: Vec<String> = Vec::new();
    let mut all_warnings: Vec<String> = Vec::new();
    let mut total_seconds = 0.0_f64;
    let mut status: Option<String> = None;
    let mut input_format: Option<String> = None;
    let mut picture_index_offset: usize = 0;

    for payload in batch.pages.drain(..) {
        if !merged_markdown.is_empty() {
            merged_markdown.push_str("\n\n");
            merged_text.push_str("\n\n");
        }
        merged_markdown.push_str(&payload.markdown);
        if let Some(ref text) = payload.text {
            merged_text.push_str(text);
        }
        merged_picture_ocr_text.extend(payload.picture_ocr_text);

        for mut picture in payload.pictures {
            picture.index += picture_index_offset;
            merged_pictures.push(picture);
        }
        picture_index_offset = merged_pictures.len();

        all_warnings.extend(payload.warnings);
        total_seconds +=
            payload.timings.get("totalSeconds").and_then(serde_json::Value::as_f64).unwrap_or(0.0);
        status = payload.status.or(status);
        input_format = payload.input_format.or(input_format);
    }

    let page_count = end_page.saturating_sub(start_page).saturating_add(1);
    DoclingExtractionPayload {
        markdown: merged_markdown,
        text: Some(merged_text),
        picture_ocr_text: merged_picture_ocr_text,
        page_count: Some(page_count),
        status,
        input_format,
        docling_version: None,
        warnings: all_warnings,
        timings: serde_json::json!({
            "totalSeconds": total_seconds,
            "paginated": true,
            "pageRange": format!("{start_page}-{end_page}"),
        }),
        pictures: merged_pictures,
    }
}

#[cfg(test)]
mod tests;
