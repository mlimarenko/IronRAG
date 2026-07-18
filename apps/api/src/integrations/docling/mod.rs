//! Embedded Docling extraction adapter.
//!
//! Docling is installed into the backend image and executed locally. This
//! module owns the process boundary and converts Docling Markdown output into
//! the canonical [`ExtractionOutput`] shape consumed by the ingest pipeline.

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

use base64::Engine as _;
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
    extraction::{ExtractionOutput, ExtractionSourceMetadata, build_text_layout_from_content},
    telemetry,
};

const DEFAULT_EXTRACT_BIN: &str = "ironrag-docling-extract";
const DEFAULT_TIMEOUT_SECS: u64 = 900;
const DEFAULT_PAGE_BATCH_SIZE: u32 = 10;
const DOCLING_AUTO_MAX_CONCURRENCY: usize = 4;
const DOCLING_AUTO_RESERVED_MEMORY_MIB: u64 = 2048;
const DOCLING_AUTO_MEMORY_PER_PROCESS_MIB: u64 = 2560;
const DOCLING_PROCESS_SAFETY_MARGIN_MIB: u64 = 256;
const STDERR_PREVIEW_LIMIT: usize = 4_000;

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
    file_name: Option<&str>,
    mime_type: Option<&str>,
    source_format: &str,
    file_bytes: Vec<u8>,
) -> Result<ExtractionOutput, DoclingExtractionError> {
    let _permit = acquire_docling_permit().await?;
    let temp_dir = tempfile::tempdir().map_err(DoclingExtractionError::TempDir)?;
    let input_path = write_input_file(&temp_dir, file_name, mime_type, source_format, &file_bytes)?;

    if source_format == "pdf" {
        return extract_document_paginated(&input_path, file_name, mime_type, source_format).await;
    }

    let payload = run_docling(&input_path, &[]).await?;
    build_output(payload, file_name, mime_type, source_format)
}

pub async fn extract_pdf_page_count(
    file_name: Option<&str>,
    mime_type: Option<&str>,
    source_format: &str,
    file_bytes: &[u8],
) -> Result<Option<u32>, DoclingExtractionError> {
    let temp_dir = tempfile::tempdir().map_err(DoclingExtractionError::TempDir)?;
    let input_path = write_input_file(&temp_dir, file_name, mime_type, source_format, file_bytes)?;
    run_docling_page_count(&input_path).await
}

pub async fn extract_pdf_page_ranges_streamed<F, Fut>(
    file_name: Option<&str>,
    mime_type: Option<&str>,
    source_format: &str,
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
    let input_path = write_input_file(&temp_dir, file_name, mime_type, source_format, file_bytes)?;
    run_docling_page_batches(
        &input_path,
        file_name,
        mime_type,
        source_format,
        start_page,
        end_page,
        batch_size,
        on_batch,
    )
    .await
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
    file_name: Option<&str>,
    mime_type: Option<&str>,
    source_format: &str,
) -> Result<ExtractionOutput, DoclingExtractionError> {
    let page_count = run_docling_page_count(input_path)
        .await?
        .filter(|value| *value > 0)
        .ok_or(DoclingExtractionError::PdfPageCountUnavailable)?;

    let batch_size = page_batch_size();
    let batch_count = page_count.div_ceil(batch_size);
    tracing::info!(
        page_count,
        batch_size,
        batch_count,
        "docling: starting batched page-at-a-time extraction"
    );

    let mut merged = PaginatedDoclingPayload::default();
    let mut total_seconds = 0.0_f64;

    for batch_idx in 0..batch_count {
        let start = batch_idx * batch_size + 1;
        let end = ((batch_idx + 1) * batch_size).min(page_count);
        let (payloads, batch_elapsed) = extract_docling_page_batch(input_path, start, end).await?;

        for payload in payloads {
            merged.append(payload);
        }

        total_seconds += batch_elapsed;

        if batch_count > 1 {
            tracing::info!(
                batch = batch_idx + 1,
                batch_count,
                pages = format!("{start}-{end}"),
                "docling: batch complete"
            );
        }
    }

    tracing::info!(page_count, total_seconds, "docling: page-at-a-time extraction complete");

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

    build_output(merged_payload, file_name, mime_type, source_format)
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

async fn acquire_docling_permit() -> Result<OwnedSemaphorePermit, DoclingExtractionError> {
    DOCLING_CONCURRENCY
        .clone()
        .acquire_owned()
        .await
        .map_err(|_| DoclingExtractionError::LimiterClosed)
}

fn write_input_file(
    temp_dir: &TempDir,
    file_name: Option<&str>,
    mime_type: Option<&str>,
    source_format: &str,
    file_bytes: &[u8],
) -> Result<PathBuf, DoclingExtractionError> {
    let file_name = normalized_input_file_name(file_name, mime_type, source_format);
    let input_path = temp_dir.path().join(file_name);
    std::fs::write(&input_path, file_bytes).map_err(DoclingExtractionError::WriteInput)?;
    Ok(input_path)
}

async fn run_docling(
    input_path: &Path,
    extra_args: &[&str],
) -> Result<DoclingExtractionPayload, DoclingExtractionError> {
    ensure_docling_process_memory_headroom()?;

    // Log current process RSS so operators can correlate OOM kills with
    // pre-extraction baseline memory usage.
    if let Some(rss_mb) = current_rss_mb() {
        if rss_mb > 3000 {
            tracing::warn!(rss_mb, "docling: worker RSS is high before extraction — OOM likely");
        } else {
            tracing::info!(rss_mb, "docling: worker RSS before extraction");
        }
    }

    let output = run_docling_raw(input_path, extra_args).await?;
    serde_json::from_slice(&output.stdout).map_err(DoclingExtractionError::InvalidJson)
}

/// Queries the page count of a PDF via `--page-count`. Returns `None`
/// when the extractor reports that page counting is unsupported.
async fn run_docling_page_count(input_path: &Path) -> Result<Option<u32>, DoclingExtractionError> {
    ensure_docling_process_memory_headroom()?;
    let output = run_docling_raw(input_path, &["--page-count"]).await?;
    let payload: DoclingPageCountPayload =
        serde_json::from_slice(&output.stdout).map_err(DoclingExtractionError::InvalidJson)?;
    Ok(payload.page_count)
}

/// Calls `--pages START-END` to extract a range of pages in a single
/// Python process, reusing the loaded `RapidOCR` model across pages.
async fn run_docling_batch(
    input_path: &Path,
    start_page: u32,
    end_page: u32,
) -> Result<DoclingBatchPayload, DoclingExtractionError> {
    ensure_docling_process_memory_headroom()?;
    let range = format!("{start_page}-{end_page}");
    let output = run_docling_raw(input_path, &["--pages", &range]).await?;
    serde_json::from_slice(&output.stdout).map_err(DoclingExtractionError::InvalidJson)
}

async fn run_docling_page_batches<F, Fut>(
    input_path: &Path,
    file_name: Option<&str>,
    mime_type: Option<&str>,
    source_format: &str,
    start_page: u32,
    end_page: u32,
    batch_size: u32,
    mut on_batch: F,
) -> Result<(), DoclingBatchStreamError>
where
    F: FnMut(DoclingPageRangeExtraction) -> Fut,
    Fut: Future<Output = anyhow::Result<()>>,
{
    ensure_docling_process_memory_headroom()?;

    let timeout_secs = docling_timeout_secs();
    let range = format!("{start_page}-{end_page}");
    let mut command = Command::new(docling_extract_bin());
    command
        .arg("--page-batches")
        .arg(batch_size.to_string())
        .arg(range)
        .arg(input_path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let mut child = command.spawn().map_err(DoclingExtractionError::Spawn)?;
    let stdout = child.stdout.take().ok_or_else(|| {
        DoclingExtractionError::Spawn(std::io::Error::other("docling batch stdout was not piped"))
    })?;
    let stderr_task = child.stderr.take().map(|mut stderr| {
        tokio::spawn(async move {
            let mut bytes = Vec::new();
            let _ = stderr.read_to_end(&mut bytes).await;
            bytes
        })
    });

    let mut lines = BufReader::new(stdout).lines();
    while let Some(line) = timeout(Duration::from_secs(timeout_secs), lines.next_line())
        .await
        .map_err(|_| {
            let _ = child.start_kill();
            DoclingExtractionError::Timeout(timeout_secs)
        })?
        .map_err(DoclingExtractionError::Spawn)?
    {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut payload: DoclingBatchPayload =
            serde_json::from_str(line).map_err(DoclingExtractionError::InvalidJson)?;
        let (range_start, range_end) =
            parse_page_range(payload.page_range.as_deref()).map_err(|message| {
                DoclingExtractionError::PaginatedMergeFailed(format!(
                    "docling streamed batch has invalid pageRange: {message}"
                ))
            })?;
        let merged_payload = merge_batch_payload(&mut payload, range_start, range_end);
        let elapsed_ms = merged_payload
            .timings
            .get("totalSeconds")
            .and_then(serde_json::Value::as_f64)
            .map(|seconds| (seconds * 1000.0).round() as i64)
            .unwrap_or_default();
        let output = build_output(merged_payload, file_name, mime_type, source_format)?;
        on_batch(DoclingPageRangeExtraction {
            start_page: range_start,
            end_page: range_end,
            elapsed_ms,
            output,
        })
        .await
        .map_err(DoclingBatchStreamError::Batch)?;
    }

    let status = child.wait().await.map_err(DoclingExtractionError::Spawn)?;
    let stderr_bytes = match stderr_task {
        Some(task) => task.await.unwrap_or_default(),
        None => Vec::new(),
    };
    if !status.success() {
        let stderr =
            String::from_utf8(stderr_bytes).map_err(DoclingExtractionError::InvalidUtf8)?;
        return Err(DoclingExtractionError::ProcessFailed {
            status,
            stderr: truncate_for_error(&stderr),
        }
        .into());
    }

    Ok(())
}

/// Low-level docling invocation returning raw output for custom parsing.
async fn run_docling_raw(
    input_path: &Path,
    extra_args: &[&str],
) -> Result<std::process::Output, DoclingExtractionError> {
    let timeout_secs = docling_timeout_secs();
    let mut command = Command::new(docling_extract_bin());
    for arg in extra_args {
        command.arg(arg);
    }
    command.arg(input_path).kill_on_drop(true);

    let output = timeout(Duration::from_secs(timeout_secs), command.output())
        .await
        .map_err(|_| DoclingExtractionError::Timeout(timeout_secs))?
        .map_err(DoclingExtractionError::Spawn)?;

    if !output.status.success() {
        let stderr =
            String::from_utf8(output.stderr).map_err(DoclingExtractionError::InvalidUtf8)?;
        return Err(DoclingExtractionError::ProcessFailed {
            status: output.status,
            stderr: truncate_for_error(&stderr),
        });
    }

    Ok(output)
}

fn parse_page_range(raw: Option<&str>) -> Result<(u32, u32), String> {
    let raw = raw.ok_or_else(|| "missing pageRange".to_string())?;
    let (start, end) =
        raw.split_once('-').ok_or_else(|| format!("expected START-END, got {raw}"))?;
    let start_page = start.parse::<u32>().map_err(|_| format!("invalid start page {start}"))?;
    let end_page = end.parse::<u32>().map_err(|_| format!("invalid end page {end}"))?;
    if start_page == 0 || end_page < start_page {
        return Err(format!("invalid page range {raw}"));
    }
    Ok((start_page, end_page))
}

fn page_batch_size() -> u32 {
    std::env::var("IRONRAG_DOCLING_PAGE_BATCH_SIZE")
        .ok()
        .and_then(|raw| raw.parse::<u32>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_PAGE_BATCH_SIZE)
}

/// Returns the current process RSS in megabytes, or `None` if the platform
/// does not support `/proc/self/statm` (non-Linux).
fn current_rss_mb() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
        let pages: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
        // statm reports pages; typical page size is 4 KiB.
        Some(pages * 4 / 1024)
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

fn ensure_docling_process_memory_headroom() -> Result<(), DoclingExtractionError> {
    let Some(memory_limit_bytes) = telemetry::detect_container_memory_limit_bytes() else {
        return Ok(());
    };
    docling_process_memory_headroom_for_limits(memory_limit_bytes, current_rss_mb()).map(|_| ())
}

fn docling_process_memory_headroom_for_limits(
    memory_limit_bytes: u64,
    current_rss_mib: Option<u64>,
) -> Result<u64, DoclingExtractionError> {
    let memory_limit_mib = memory_limit_bytes / (1024 * 1024);
    let current_rss_mib = current_rss_mib.unwrap_or_default();
    let available_mib = memory_limit_mib.saturating_sub(current_rss_mib);
    let required_with_margin =
        DOCLING_AUTO_MEMORY_PER_PROCESS_MIB.saturating_add(DOCLING_PROCESS_SAFETY_MARGIN_MIB);
    if available_mib < required_with_margin {
        return Err(DoclingExtractionError::InsufficientMemory {
            memory_limit_mib,
            current_rss_mib,
            available_mib,
            required_mib: DOCLING_AUTO_MEMORY_PER_PROCESS_MIB,
            safety_margin_mib: DOCLING_PROCESS_SAFETY_MARGIN_MIB,
        });
    }
    Ok(available_mib)
}

fn process_status_is_memory_kill(status: &ExitStatus) -> bool {
    #[cfg(target_family = "unix")]
    {
        status.signal() == Some(9)
    }
    #[cfg(not(target_family = "unix"))]
    {
        let _ = status;
        false
    }
}

fn build_output(
    payload: DoclingExtractionPayload,
    file_name: Option<&str>,
    mime_type: Option<&str>,
    source_format: &str,
) -> Result<ExtractionOutput, DoclingExtractionError> {
    let has_picture_payload = !payload.pictures.is_empty();
    let has_picture_ocr_text =
        payload.picture_ocr_text.iter().any(|snippet| !snippet.trim().is_empty());
    let content = select_docling_content(payload.markdown, payload.text);
    if content.trim().is_empty() && !has_picture_payload && !has_picture_ocr_text {
        return Err(DoclingExtractionError::EmptyOutput);
    }

    let layout = build_text_layout_from_content(content.trim());
    let line_count = i32::try_from(layout.structure_hints.lines.len()).unwrap_or(i32::MAX);
    let input_format = payload.input_format.unwrap_or_else(|| source_format.to_string());
    let page_count = payload.page_count;

    let extracted_images = payload
        .pictures
        .into_iter()
        .filter_map(|picture| {
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(picture.content_base64.as_bytes())
                .ok()?;
            let width = picture.size_px.first().copied().unwrap_or_default();
            let height = picture.size_px.get(1).copied().unwrap_or_default();
            Some(crate::shared::extraction::ExtractedImage {
                page: 0,
                image_bytes: bytes,
                mime_type: picture.mime.unwrap_or_else(|| "image/png".to_string()),
                width,
                height,
            })
        })
        .collect();

    Ok(ExtractionOutput {
        extraction_kind: "docling_markdown".to_string(),
        content_text: layout.content_text,
        page_count,
        warnings: payload.warnings,
        source_metadata: ExtractionSourceMetadata {
            source_format: input_format.clone(),
            page_count,
            line_count,
        },
        structure_hints: layout.structure_hints,
        source_map: serde_json::json!({
            "adapter": "docling",
            "input_file_name": file_name,
            "mime_type": mime_type,
            "source_format": source_format,
            "docling_input_format": input_format,
            "docling_status": payload.status,
            "docling_version": payload.docling_version,
            "docling_picture_ocr_text": payload.picture_ocr_text,
            "timings": payload.timings,
        }),
        provider_kind: None,
        model_name: None,
        usage_json: serde_json::json!({}),
        extracted_images,
    })
}

fn select_docling_content(markdown: String, text: Option<String>) -> String {
    let text = text.unwrap_or_default();
    let markdown_body = markdown.replace("<!-- image -->", "");
    if markdown_body.trim().is_empty() || markdown.trim().is_empty() { text } else { markdown }
}

fn docling_extract_bin() -> String {
    std::env::var("IRONRAG_DOCLING_EXTRACT_BIN").unwrap_or_else(|_| DEFAULT_EXTRACT_BIN.to_string())
}

fn docling_timeout_secs() -> u64 {
    std::env::var("IRONRAG_DOCLING_TIMEOUT_SECS")
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_TIMEOUT_SECS)
}

fn resolve_docling_max_concurrency() -> usize {
    let raw = std::env::var("IRONRAG_DOCLING_MAX_CONCURRENCY").ok();
    match raw.as_deref().map(str::trim) {
        Some(value) if value.eq_ignore_ascii_case("auto") || value == "0" || value.is_empty() => {
            auto_docling_max_concurrency()
        }
        Some(value) => {
            if let Some(value) = value.parse::<usize>().ok().filter(|value| *value > 0) {
                value
            } else {
                tracing::warn!(
                    raw = value,
                    "invalid IRONRAG_DOCLING_MAX_CONCURRENCY; using automatic docling concurrency"
                );
                auto_docling_max_concurrency()
            }
        }
        None => auto_docling_max_concurrency(),
    }
}

fn auto_docling_max_concurrency() -> usize {
    let cpu_parallelism = telemetry::detect_container_cpu_parallelism().unwrap_or(1);
    let memory_limit_bytes = telemetry::detect_container_memory_limit_bytes();
    let concurrency = auto_docling_max_concurrency_for_limits(cpu_parallelism, memory_limit_bytes);
    let memory_limit_mib = memory_limit_bytes.map(|bytes| bytes / (1024 * 1024));
    let soft_limit_mib = memory_limit_mib.map(|mib| mib.saturating_mul(9) / 10);
    let docling_budget_mib =
        soft_limit_mib.map(|mib| mib.saturating_sub(DOCLING_AUTO_RESERVED_MEMORY_MIB));
    tracing::info!(
        cpu_parallelism,
        ?memory_limit_mib,
        ?soft_limit_mib,
        ?docling_budget_mib,
        reserved_mib = DOCLING_AUTO_RESERVED_MEMORY_MIB,
        per_process_mib = DOCLING_AUTO_MEMORY_PER_PROCESS_MIB,
        max_concurrency = DOCLING_AUTO_MAX_CONCURRENCY,
        concurrency,
        "docling auto concurrency resolved"
    );
    if docling_budget_mib.is_some_and(|budget| budget < DOCLING_AUTO_MEMORY_PER_PROCESS_MIB) {
        tracing::warn!(
            ?memory_limit_mib,
            ?soft_limit_mib,
            ?docling_budget_mib,
            required_mib = DOCLING_AUTO_MEMORY_PER_PROCESS_MIB,
            "docling auto concurrency has only enough memory budget for the mandatory single process"
        );
    }
    concurrency
}

fn auto_docling_max_concurrency_for_limits(
    cpu_parallelism: usize,
    memory_limit_bytes: Option<u64>,
) -> usize {
    // One Docling process is internally CPU-heavy (Torch/RapidOCR); reserve
    // roughly half of the worker CPU quota for the Rust pipeline, embeddings,
    // health checks, and query traffic while extraction is running.
    let cpu_bound = cpu_parallelism.max(1).saturating_div(2).max(1);
    let memory_bound =
        memory_limit_bytes.map(|bytes| bytes / (1024 * 1024)).map_or(1, |memory_mib| {
            let soft_limit_mib = memory_mib.saturating_mul(9) / 10;
            soft_limit_mib
                .saturating_sub(DOCLING_AUTO_RESERVED_MEMORY_MIB)
                .checked_div(DOCLING_AUTO_MEMORY_PER_PROCESS_MIB)
                .unwrap_or(0) as usize
        });

    cpu_bound.min(memory_bound.max(1)).clamp(1, DOCLING_AUTO_MAX_CONCURRENCY)
}

fn normalized_input_file_name(
    file_name: Option<&str>,
    mime_type: Option<&str>,
    source_format: &str,
) -> String {
    let extension = file_extension_for_docling(file_name, mime_type, source_format);
    let base = file_name
        .and_then(|value| Path::new(value).file_name())
        .and_then(OsStr::to_str)
        .map(|value| {
            value
                .chars()
                .map(|ch| if ch == '/' || ch == '\\' || ch == '\0' { '_' } else { ch })
                .collect::<String>()
        })
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "upload".to_string());

    if Path::new(&base).extension().is_some() { base } else { format!("{base}.{extension}") }
}

fn file_extension_for_docling(
    file_name: Option<&str>,
    mime_type: Option<&str>,
    source_format: &str,
) -> &'static str {
    if let Some(ext) = file_name
        .and_then(|value| Path::new(value).extension())
        .and_then(OsStr::to_str)
        .map(str::to_ascii_lowercase)
    {
        return match ext.as_str() {
            "pdf" => "pdf",
            "docx" => "docx",
            "pptx" => "pptx",
            "png" => "png",
            "jpg" | "jpeg" => "jpg",
            "tif" | "tiff" => "tiff",
            "bmp" => "bmp",
            "webp" => "webp",
            _ => extension_from_source_format(source_format),
        };
    }

    match mime_type.map(str::to_ascii_lowercase).as_deref() {
        Some("application/pdf") => "pdf",
        Some("application/vnd.openxmlformats-officedocument.wordprocessingml.document") => "docx",
        Some("application/vnd.openxmlformats-officedocument.presentationml.presentation") => "pptx",
        Some("image/jpeg") => "jpg",
        Some("image/png") => "png",
        Some("image/tiff") => "tiff",
        Some("image/bmp") => "bmp",
        Some("image/webp") => "webp",
        _ => extension_from_source_format(source_format),
    }
}

fn extension_from_source_format(source_format: &str) -> &'static str {
    match source_format {
        "pdf" => "pdf",
        "docx" => "docx",
        "pptx" => "pptx",
        "image" | "png" => "png",
        "jpeg" | "jpg" => "jpg",
        "tiff" => "tiff",
        "bmp" => "bmp",
        "webp" => "webp",
        _ => "bin",
    }
}

fn truncate_for_error(value: &str) -> String {
    let mut output = value.chars().take(STDERR_PREVIEW_LIMIT).collect::<String>();
    if value.chars().count() > STDERR_PREVIEW_LIMIT {
        output.push_str("...");
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_extraction_output_from_docling_markdown() {
        let payload = DoclingExtractionPayload {
            markdown: "# Operations Report\n\n| Region | Amount |\n|---|---:|\n| West | 42 |"
                .to_string(),
            text: None,
            picture_ocr_text: Vec::new(),
            page_count: Some(2),
            status: Some("success".to_string()),
            input_format: Some("pdf".to_string()),
            docling_version: Some("2.91.0".to_string()),
            warnings: Vec::new(),
            timings: serde_json::json!({"total": 1.5}),
            pictures: Vec::new(),
        };

        let output =
            build_output(payload, Some("operations-report.pdf"), Some("application/pdf"), "pdf")
                .expect("docling output");

        assert_eq!(output.extraction_kind, "docling_markdown");
        assert_eq!(output.page_count, Some(2));
        assert_eq!(output.source_metadata.source_format, "pdf");
        assert!(output.content_text.contains("| West | 42 |"));
        assert!(output.structure_hints.lines.iter().any(|line| {
            line.signals.contains(&crate::shared::extraction::ExtractionLineSignal::TableRow)
        }));
        assert_eq!(output.source_map["adapter"], "docling");
    }

    #[test]
    fn builds_extraction_output_from_docling_text_when_markdown_is_only_image_placeholder() {
        let payload = DoclingExtractionPayload {
            markdown: "<!-- image -->".to_string(),
            text: Some("Formats PDF / DOCX / PPTX / PNG / JPG".to_string()),
            picture_ocr_text: Vec::new(),
            page_count: Some(1),
            status: Some("success".to_string()),
            input_format: Some("pdf".to_string()),
            docling_version: Some("2.91.0".to_string()),
            warnings: Vec::new(),
            timings: serde_json::json!({"total": 1.5}),
            pictures: Vec::new(),
        };

        let output = build_output(payload, Some("formats.pdf"), Some("application/pdf"), "pdf")
            .expect("docling output");

        assert_eq!(output.content_text, "Formats PDF / DOCX / PPTX / PNG / JPG");
    }

    #[test]
    fn rejects_docling_output_when_markdown_is_only_image_placeholder_without_text() {
        let payload = DoclingExtractionPayload {
            markdown: "<!-- image -->".to_string(),
            text: None,
            picture_ocr_text: Vec::new(),
            page_count: Some(1),
            status: Some("success".to_string()),
            input_format: Some("pdf".to_string()),
            docling_version: Some("2.91.0".to_string()),
            warnings: Vec::new(),
            timings: serde_json::json!({"total": 1.5}),
            pictures: Vec::new(),
        };

        let error = build_output(payload, Some("empty.pdf"), Some("application/pdf"), "pdf")
            .expect_err("placeholder-only output should be rejected");

        assert!(matches!(error, DoclingExtractionError::EmptyOutput));
    }

    #[test]
    fn normalized_input_file_name_adds_extension_when_missing() {
        assert_eq!(
            normalized_input_file_name(Some("upload"), Some("application/pdf"), "pdf"),
            "upload.pdf"
        );
        assert_eq!(
            normalized_input_file_name(Some("../unsafe/report"), None, "docx"),
            "report.docx"
        );
    }

    #[test]
    fn auto_docling_concurrency_uses_cpu_and_memory_bounds() {
        assert_eq!(auto_docling_max_concurrency_for_limits(4, Some(8 * 1024 * 1024 * 1024)), 2);
        assert_eq!(auto_docling_max_concurrency_for_limits(8, Some(16 * 1024 * 1024 * 1024)), 4);
        assert_eq!(auto_docling_max_concurrency_for_limits(8, Some(4 * 1024 * 1024 * 1024)), 1);
        assert_eq!(auto_docling_max_concurrency_for_limits(8, None), 1);
    }

    #[test]
    fn docling_process_memory_gate_rejects_limits_below_one_process_floor() {
        let error = docling_process_memory_headroom_for_limits(768 * 1024 * 1024, Some(64))
            .expect_err("tiny worker cap cannot safely run a docling process");

        assert_eq!(error.memory_failure_code(), Some("docling_insufficient_memory"));
    }

    #[cfg(target_family = "unix")]
    #[test]
    fn docling_process_memory_failure_uses_typed_sigkill_status() {
        let error = DoclingExtractionError::ProcessFailed {
            status: std::process::ExitStatus::from_raw(9),
            stderr: String::new(),
        };

        assert_eq!(error.memory_failure_code(), Some("docling_process_oom"));
    }

    #[cfg(target_family = "unix")]
    #[test]
    fn docling_process_memory_failure_ignores_stderr_prose() {
        let error = DoclingExtractionError::ProcessFailed {
            status: std::process::ExitStatus::from_raw(9 << 8),
            stderr: "SIGKILL likely out of memory".to_string(),
        };

        assert_eq!(error.memory_failure_code(), None);
    }

    #[test]
    fn docling_process_memory_failure_ignores_nested_error_prose() {
        let error = DoclingExtractionError::PageExtractionFailed {
            page: 1,
            source: Box::new(DoclingExtractionError::PaginatedMergeFailed(
                "SIGKILL likely out of memory".to_string(),
            )),
        };

        assert_eq!(error.memory_failure_code(), None);
    }

    #[test]
    fn docling_process_memory_gate_allows_single_process_when_headroom_exists() {
        let available =
            docling_process_memory_headroom_for_limits(4 * 1024 * 1024 * 1024, Some(256))
                .expect("4 GiB worker cap has enough headroom for one docling process");

        assert!(available >= DOCLING_AUTO_MEMORY_PER_PROCESS_MIB);
    }
}
